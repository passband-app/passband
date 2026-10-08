//! Read endpoints for the embedded user agent. These never reuse the human
//! reader's unrestricted projections. Account identity comes from API state.
use crate::{ApiError, ApiState, handlers::store_call};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::header,
    response::IntoResponse,
};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use squelch_core::{
    CoreError,
    store::{
        LeggedHit, SearchSort, Store,
        agent_triage::{AgentListItem, AgentTriageStore},
        archive_surface::{SurfaceWait, await_thread_access, search_with_surfacing},
        parse_search_query,
    },
    triage::decision::{EmailKind, RecordProposal},
};

fn no_store(value: Value) -> impl IntoResponse {
    ([(header::CACHE_CONTROL, "no-store")], Json(value))
}

pub async fn thread(
    State(state): State<ApiState>,
    Path(thread_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    // The checks this open queues are waited on BEFORE the guard is applied,
    // so a thread is not served with a sibling missing and then closed a
    // moment later when that sibling's verdict lands.
    let surfacing = state
        .archive_surfacing
        .clone()
        .capped(SurfaceWait::HTTP_DEADLINE);
    await_thread_access(&state.store, state.account_id, &thread_id, &surfacing)
        .await
        .map_err(ApiError::from)?;
    let view = store_call(&state, move |store, account| {
        store.thread_view(account, &thread_id)
    })
    .await?;
    Ok(no_store(json!(view)))
}

#[derive(Deserialize)]
pub struct FeedQuery {
    #[serde(default = "default_destination")]
    destination: String,
    limit: Option<usize>,
}
fn default_destination() -> String {
    "fye".into()
}

/// Deliberately omit notification advice and executor diagnostics. The agent
/// gets the explanation and attention state needed to help its human.
fn feed_item(item: AgentListItem) -> Value {
    json!({"message_id": item.message_id, "thread_id": item.thread_id,
        "from_addr": item.from_addr, "subject": item.subject,
        "received_at": item.received_at, "score": item.score,
        "decision": {"kinds": item.decision.kinds, "destinations": item.decision.destinations,
            "reading_kind": item.decision.reading_kind,
            "summary": item.decision.summary, "reason": item.decision.reason},
        "attention": item.attention})
}

pub async fn feed(
    State(state): State<ApiState>,
    Query(query): Query<FeedQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let now = Utc::now();
    let ranking = state.triage_config.ranking.clone();
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let items = store_call(&state, move |store, account| {
        match query.destination.as_str() {
            "fye" => store.external_agent_fye(account, limit, &ranking, now),
            "reading" => store.external_agent_reading(account, limit),
            "records" => store.external_agent_records(account, limit),
            _ => Err(CoreError::InvalidInput("unsupported destination".into())),
        }
    })
    .await?;
    Ok(no_store(json!({"version": 2, "ranked_at": now,
        "items": items.into_iter().map(feed_item).collect::<Vec<_>>()})))
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: String,
    limit: Option<u32>,
    mode: Option<String>,
}

pub async fn search(
    State(state): State<ApiState>,
    Query(query): Query<SearchQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let (text, filter) = parse_search_query(query.q.trim());
    let text = text.trim().to_owned();
    if text.is_empty() && filter.is_empty() {
        return Err(ApiError::bad_request("q must not be empty"));
    }
    let limit = query.limit.unwrap_or(30).clamp(1, 100);
    let requested = query.mode.as_deref().unwrap_or("hybrid");
    if !matches!(requested, "keyword" | "semantic" | "hybrid") {
        return Err(ApiError::bad_request("unsupported search mode"));
    }
    let mode = if text.is_empty() || state.store.embedder().is_none() {
        "keyword"
    } else {
        requested
    }
    .to_owned();
    let response_mode = mode.clone();
    let account = state.account_id;
    // ASSESS ON SURFACING: archive mail the KEYWORD leg matched but that has
    // never had an access check gets one now, and the search is run again
    // after a short wait. See `squelch_core::store::archive_surface`.
    let search = move |store: &squelch_core::store::SqliteStore| {
        // Which leg found a row matters here and only here: a vector-only
        // neighbour is never sent for a check.
        let legged = |hits: Vec<_>, keyword: bool| -> Vec<LeggedHit> {
            hits.into_iter()
                .map(|hit| LeggedHit {
                    hit,
                    keyword,
                    vector: !keyword,
                })
                .collect()
        };
        // Access is applied before the public result limit, inside the one
        // recall loop both doors share: a hidden leading window must not hide
        // a readable result farther down the ranking, and the window stops
        // growing at the same ceiling in every mode.
        store.external_recall(account, limit as usize, |window| {
            Ok(match mode.as_str() {
                // No snippet windows: every hit is rehydrated by the loop.
                "hybrid" => store.hybrid_search_legs(
                    account,
                    &text,
                    &filter,
                    SearchSort::default(),
                    false,
                    false,
                    window,
                )?,
                "semantic" => {
                    let (hits, full) = store.semantic_search_hits(
                        account,
                        &text,
                        &filter,
                        SearchSort::default(),
                        false,
                        window,
                    )?;
                    (legged(hits, false), full)
                }
                _ => {
                    let hits = store.search_filtered(
                        account,
                        &text,
                        &filter,
                        SearchSort::default(),
                        false,
                        window as u32,
                        0,
                    )?;
                    let full = hits.len() == window;
                    (legged(hits, true), full)
                }
            })
        })
    };
    let surfacing = state
        .archive_surfacing
        .clone()
        .capped(SurfaceWait::HTTP_DEADLINE);
    let (hits, horizon) = search_with_surfacing(&state.store, account, &surfacing, search)
        .await
        .map_err(ApiError::from)?;
    let mut body = json!({"items": hits, "match_kind": response_mode, "sort": "recent"});
    // How far back this search can see, so the assistant says "search covers
    // mail since X" instead of "that mail does not exist". Both are absent
    // when there is nothing to say (an empty mailbox; the archive turned off).
    // NEITHER DEPENDS ON THE QUERY: nothing here says whether older mail
    // matched, how much, or that any is waiting on a check.
    if let Some(indexed_since) = horizon.indexed_since {
        body["indexed_since"] = json!(indexed_since);
    }
    if let Some(archive) = horizon.archive {
        body["archive"] = json!(archive);
    }
    Ok(no_store(body))
}

pub async fn decision(
    State(state): State<ApiState>,
    Path(message_id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let decision = store_call(&state, move |store, account| {
        store.external_agent_decision(account, message_id)
    })
    .await?;
    Ok(no_store(
        json!({"decision": {"kinds": decision.kinds, "destinations": decision.destinations,
        "reading_kind": decision.reading_kind, "summary": decision.summary, "reason": decision.reason, "attention": decision.attention,
        "rule_exceptions": decision.rule_exceptions}}),
    ))
}

#[derive(Deserialize)]
pub struct RecordsQuery {
    kind: String,
    days: Option<u32>,
    hours: Option<u32>,
    #[serde(default)]
    include_delivered: bool,
}

fn record_matches(record: &RecordProposal, kind: &str, include_delivered: bool) -> bool {
    match (record, kind) {
        (RecordProposal::Bill { .. }, "bills")
        | (RecordProposal::Receipt { .. }, "receipts")
        | (RecordProposal::Event { .. }, "calendar")
        | (RecordProposal::FinancialUpdate { .. }, "banking") => true,
        (RecordProposal::Delivery { status, .. }, "shipments") => {
            include_delivered || status != "delivered"
        }
        _ => false,
    }
}

/// What this route serves for one record. An event is cut back to the fields
/// it had before the calendar work: the rest (identity refs such as booking
/// numbers, the duration guess, commitment, likelihood) is ghost data, and
/// ghosts are served on `/client/timeline` only. Every other record is served
/// whole, as before.
fn record_view(record: &RecordProposal) -> serde_json::Value {
    match record {
        RecordProposal::Event {
            title,
            start,
            location,
            evidence,
            ..
        } => json!({"kind": "event", "title": title, "start": start,
            "location": location, "evidence": evidence}),
        other => json!(other),
    }
}

pub async fn records(
    State(state): State<ApiState>,
    Query(query): Query<RecordsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    if !matches!(
        query.kind.as_str(),
        "bills" | "receipts" | "calendar" | "banking" | "shipments" | "marketing"
    ) {
        return Err(ApiError::bad_request("unsupported record kind"));
    }
    let marketing = query.kind == "marketing";
    let items = store_call(&state, move |store, account| {
        if marketing {
            store.external_agent_reading(account, 500)
        } else {
            store.external_agent_records(account, 500)
        }
    })
    .await?;
    let hours = query
        .hours
        .map(|hours| hours.clamp(1, 2160) as i64)
        .unwrap_or(query.days.unwrap_or(30).clamp(1, 90) as i64 * 24);
    let since = Utc::now() - Duration::hours(hours);
    let mut rows = Vec::new();
    for item in items {
        if DateTime::parse_from_rfc3339(&item.received_at).is_ok_and(|time| time < since) {
            continue;
        }
        if marketing {
            if item.decision.kinds.contains(&EmailKind::Promotional) {
                rows.push(
                    json!({"message_id": item.message_id, "thread_id": item.thread_id,
                    "received_at": item.received_at, "summary": item.decision.summary}),
                );
            }
            continue;
        }
        for record in &item.decision.records {
            if record_matches(record, &query.kind, query.include_delivered) {
                rows.push(
                    json!({"message_id": item.message_id, "thread_id": item.thread_id,
                    "received_at": item.received_at, "record": record_view(record)}),
                );
            }
        }
    }
    rows.truncate(200);
    Ok(no_store(json!({(query.kind): rows})))
}
