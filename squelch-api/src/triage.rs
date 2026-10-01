//! Versioned human projections for agent-owned triage.
use crate::{ApiError, ApiState, handlers::store_call};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::header,
    response::IntoResponse,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use squelch_core::store::{Store, agent_triage::AgentTriageStore};

#[derive(Deserialize)]
pub struct FeedQuery {
    pub destination: String,
    pub limit: Option<usize>,
    pub since: Option<chrono::DateTime<Utc>>,
    pub all_time: Option<bool>,
    pub include_done: Option<bool>,
}

pub async fn feed(
    State(state): State<ApiState>,
    Query(query): Query<FeedQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let limit = query.limit.unwrap_or(200).clamp(1, 1000);
    let ranking = state.triage_config.ranking.clone();
    let now = Utc::now();
    if query.all_time == Some(true) && query.since.is_some() {
        return Err(ApiError::bad_request(
            "since and all_time cannot be combined",
        ));
    }
    let inventory = squelch_core::store::agent_triage::AgentListQuery {
        since: if query.all_time == Some(true) {
            None
        } else {
            Some(query.since.unwrap_or(now - chrono::Duration::days(30)))
        },
        include_done: query.include_done.unwrap_or(false),
    };
    let mut items = store_call(&state, move |store, account| {
        match query.destination.as_str() {
            "fye" => store.agent_fye(account, usize::MAX, &ranking, now),
            "reading" => store.agent_reading_with_query(account, usize::MAX, &inventory),
            "records" => store.agent_records_with_query(account, usize::MAX, &inventory),
            _ => Err(squelch_core::CoreError::InvalidInput(
                "unsupported destination".into(),
            )),
        }
    })
    .await?;
    let total_count = items.len();
    items.truncate(limit);
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"version": 2, "ranked_at": now, "total_count": total_count, "items": items})),
    ))
}

#[derive(Deserialize)]
pub struct HeadlineQuery {
    /// The owner's local date as the client sees it, e.g. "2026-10-01
    /// (Thursday)". The daemon runs in UTC and may be a continent away, so
    /// "due tomorrow" is the client's call. Free text, folded into the
    /// trusted context line and capped there.
    pub today: Option<String>,
}

/// GET /client/v2/fye/headline: one sentence about what the For-your-eyes band
/// is ABOUT, for the sitrep's hero. Always 200; `headline` is null when there
/// is no model, the band is empty, the day's cap is spent, or the model had
/// nothing to say. Cached by board state, so asking again about the same band
/// spends nothing. See [`squelch_core::triage::fye_headline`].
pub async fn fye_headline(
    State(state): State<ApiState>,
    Query(query): Query<HeadlineQuery>,
) -> Result<impl IntoResponse, ApiError> {
    use squelch_core::triage::fye_headline::{self as fh, HeadlineItem};
    use std::hash::{Hash, Hasher};

    let none = || {
        (
            [(header::CACHE_CONTROL, "no-store")],
            Json(json!({ "headline": null })),
        )
    };
    let Some(client) = state.fye_headline.clone() else {
        return Ok(none());
    };
    let today = query
        .today
        .unwrap_or_else(|| Utc::now().format("%Y-%m-%d (%A)").to_string())
        .chars()
        .take(40)
        .collect::<String>();
    let ranking = state.triage_config.ranking.clone();
    let now = Utc::now();
    let read = store_call(&state, move |store, account| {
        let items: Vec<HeadlineItem> = store
            .agent_fye(account, usize::MAX, &ranking, now)?
            .iter()
            .filter_map(HeadlineItem::from_list_item)
            .take(fh::ITEM_LIMIT)
            .collect();
        // The runaway guard, read from the same ledger the calls bill into.
        let day = now.format("%Y-%m-%d").to_string();
        let spent: u64 = store
            .list_usage_by_category(account, 1)?
            .into_iter()
            .filter(|(c, _)| c == squelch_core::metrics::FYE_HEADLINE_USAGE_CATEGORY)
            .flat_map(|(_, days)| days)
            .filter(|d| d.day == day)
            .map(|d| d.calls)
            .sum();
        Ok((items, spent))
    })
    .await?;
    let (items, spent) = read;
    if items.is_empty() {
        return Ok(none());
    }
    // The board as the model would see it. The date is part of it: "due
    // tomorrow" stops being true at midnight even if no row changed.
    let key = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        today.hash(&mut h);
        items.hash(&mut h);
        h.finish()
    };
    let cached = state
        .fye_headline_cache
        .lock()
        .ok()
        .and_then(|c| c.clone())
        .filter(|(k, _)| *k == key);
    if let Some((_, headline)) = cached {
        return Ok((
            [(header::CACHE_CONTROL, "no-store")],
            Json(json!({ "headline": headline })),
        ));
    }
    if spent >= u64::from(fh::DAILY_CAP) {
        return Ok(none());
    }
    let (headline, usage) = fh::write_headline(&client, &today, &items).await;
    if let Ok(mut c) = state.fye_headline_cache.lock() {
        *c = Some((key, headline.clone()));
    }
    if let Some(u) = usage {
        let _ = store_call(&state, move |store, account| {
            let day = Utc::now().format("%Y-%m-%d").to_string();
            if let Err(e) = store.extract_bump_usage(
                account,
                &day,
                squelch_core::metrics::FYE_HEADLINE_USAGE_CATEGORY,
                u.into(),
            ) {
                eprintln!("squelch: fye headline usage ledger bump failed ({e})");
            }
            Ok(())
        })
        .await;
    }
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "headline": headline })),
    ))
}

pub async fn message(
    State(state): State<ApiState>,
    Path(message_id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let view = store_call(&state, move |store, account| {
        let message = store.agent_read_message(account, message_id)?;
        let mut thread =
            serde_json::to_value(store.thread_view_with_html(account, &message.thread_id)?)
                .map_err(|error| squelch_core::CoreError::Other(error.into()))?;
        thread["cache_allowed"] = store
            .external_thread_allowed(account, &message.thread_id)?
            .into();
        Ok(json!({"message_id": message_id, "thread": thread}))
    })
    .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(view)))
}

pub async fn opened(
    State(state): State<ApiState>,
    Path(message_id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    store_call(&state, move |store, account| {
        store.acknowledge_agent_message(account, message_id, Utc::now())
    })
    .await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn capabilities() -> impl IntoResponse {
    Json(
        json!({"triage_version": 2, "server_ranked_fye": true, "reading": true, "pending_message_read": true}),
    )
}

#[derive(Deserialize)]
pub struct Correction {
    pub field: String,
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    #[serde(default)]
    pub add: Option<Vec<String>>,
    #[serde(default)]
    pub remove: Option<Vec<String>>,
}

pub async fn correct(
    State(state): State<ApiState>,
    Path(message_id): Path<i64>,
    Json(correction): Json<Correction>,
) -> Result<impl IntoResponse, ApiError> {
    store_call(&state, move |store, account| {
        if correction.add.is_some() || correction.remove.is_some() {
            if correction.value.is_some() {
                return Err(squelch_core::error::CoreError::InvalidInput(
                    "choose either value or add/remove".into(),
                ));
            }
            store.correct_agent_triage_delta(
                account,
                message_id,
                &correction.field,
                correction.add.as_deref().unwrap_or(&[]),
                correction.remove.as_deref().unwrap_or(&[]),
                Utc::now(),
            )
        } else {
            let value = correction.value.ok_or_else(|| {
                squelch_core::error::CoreError::InvalidInput(
                    "correction requires value or add/remove".into(),
                )
            })?;
            store.correct_agent_triage(account, message_id, &correction.field, &value, Utc::now())
        }
    })
    .await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn decision(
    State(state): State<ApiState>,
    Path(message_id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let view = store_call(&state, move |store, account| {
        let context = store.load_agent_context(&squelch_core::store::agent_triage::AgentJob {
            id: 0, account_id: account, message_id, kind: "triage".into(), trigger: "inspect".into(),
            lease_token: String::new(), attempts: 0, arrival_eligible: false, foreground: false,
        })?;
        let diagnostics = store.agent_diagnostics(account, message_id)?;
        let fast_notification = store.latest_notification_assessment(account, message_id,
            squelch_core::metrics::NotifyLane::Fast)?;
        Ok(json!({"version": 2, "decision": context.previous_decision, "attention": context.attention,
            "corrections": context.corrections, "pending": context.previous_decision.is_none(),
            "diagnostics": diagnostics, "fast_notification": fast_notification}))
    }).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(view)))
}
