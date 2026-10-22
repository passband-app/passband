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
    /// The owner asked for ANOTHER line (clicked the headline): skip the cache
    /// and write a new one. Still counted against the daily cap.
    pub fresh: Option<bool>,
    /// The line on screen when they clicked, so the new one is different.
    pub previous: Option<String>,
}

/// GET /client/v2/fye/headline: one sentence of GOOD NEWS about the owner's
/// mail for the sitrep's hero, never a problem or a chore. Always 200; `headline` is null when there
/// is no model, the band is empty, the day's cap is spent, or the model had
/// nothing to say. Cached by board state, so asking again about the same band
/// spends nothing. See [`squelch_core::triage::fye_headline`].
pub async fn fye_headline(
    State(state): State<ApiState>,
    Query(query): Query<HeadlineQuery>,
) -> Result<impl IntoResponse, ApiError> {
    use squelch_core::triage::fye_headline::{self as fh, GoodNews, HeadlineItem};
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
    // The app sends its own local day. Anything that does not gets the
    // owner's day (#526), not UTC's, which is tomorrow by an American evening.
    let today = match query.today {
        Some(today) => today,
        None => store_call(&state, |store, account| store.owner_zone(account))
            .await
            .map(|zone| squelch_core::owner_clock::OwnerClock::new(Utc::now(), zone))
            .unwrap_or_else(|_| {
                squelch_core::owner_clock::OwnerClock::new(
                    Utc::now(),
                    squelch_core::owner_clock::OwnerZone::UTC,
                )
            })
            .local_now()
            .format("%Y-%m-%d (%A)")
            .to_string(),
    }
    .chars()
    .take(40)
    .collect::<String>();
    let ranking = state.triage_config.ranking.clone();
    let policy = state.shipment_policy;
    let now = Utc::now();
    let read = store_call(&state, move |store, account| {
        let fye = store.agent_fye(account, usize::MAX, &ranking, now)?;
        let on_board = fye.len();
        let items: Vec<HeadlineItem> = fye
            .iter()
            .filter_map(HeadlineItem::from_list_item)
            .take(fh::ITEM_LIMIT)
            .collect();
        // GOOD NEWS the daemon owns. Packages only while they are pleasant to
        // hear about: moving, or landed in the last two days. An exception
        // is a problem, and problems are not this line's to tell.
        let recent = now - chrono::Duration::days(2);
        let shipments: Vec<(String, String)> = store
            .list_shipments(account, true, policy)?
            .into_iter()
            .filter(|s| !s.item_name.trim().is_empty())
            .filter(|s| match s.status.as_str() {
                "shipped" | "out_for_delivery" => true,
                "delivered" => s.delivered_at.is_some_and(|d| d >= recent),
                _ => false,
            })
            .take(5)
            .map(|s| (s.item_name, s.status.replace('_', " ")))
            .collect();
        // Best effort: a count that fails to read is a sentence the model
        // simply does not get to write.
        let arrived_last_day = store
            .count_inbound_since(account, now - chrono::Duration::days(1))
            .ok();
        let good = GoodNews {
            shipments,
            arrived_last_day,
            on_board,
        };
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
        Ok((items, good, spent))
    })
    .await?;
    let (items, good, spent) = read;
    if items.is_empty() {
        return Ok(none());
    }
    // The board as the model would see it. The date is part of it: "due
    // tomorrow" stops being true at midnight even if no row changed.
    let key = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        today.hash(&mut h);
        items.hash(&mut h);
        good.shipments.hash(&mut h);
        good.on_board.hash(&mut h);
        // Bucketed: every newsletter that lands would otherwise be a new
        // board, and "most of the day's mail was filed" does not change with
        // one more.
        good.arrived_last_day.map(|n| n / 25).hash(&mut h);
        h.finish()
    };
    let cached = state
        .fye_headline_cache
        .lock()
        .ok()
        .and_then(|c| c.clone())
        .filter(|(k, _)| *k == key);
    let fresh = query.fresh == Some(true);
    let previous = query
        .previous
        .filter(|p| !p.trim().is_empty())
        .map(|p| p.chars().take(200).collect::<String>());
    if let Some((_, headline)) = cached.filter(|_| !fresh) {
        return Ok((
            [(header::CACHE_CONTROL, "no-store")],
            Json(json!({ "headline": headline })),
        ));
    }
    // RESERVE before calling, not after. The ledger only learns of calls the
    // provider reported usage for, so a run of timeouts would never count
    // against it, and two cache misses at once (the Mac and the phone, or a
    // click racing a poll) would both read the same total. The in-memory
    // tally counts every ATTEMPT, atomically; the ledger keeps the cap across
    // restarts. Whichever is higher is what has been spent.
    let day = now.format("%Y-%m-%d").to_string();
    let reserved = state.fye_headline_attempts.lock().is_ok_and(|mut t| {
        if t.0 != day {
            *t = (day.clone(), 0);
        }
        if u64::from(t.1).max(spent) >= u64::from(fh::DAILY_CAP) {
            return false;
        }
        t.1 += 1;
        true
    });
    if !reserved {
        return Ok(none());
    }
    let result = fh::write_headline(&client, &today, &items, &good, previous.as_deref()).await;
    let headline = result.headline.clone();
    // Only an ANSWER is remembered. A timeout or an error is not "nothing
    // good to say", and caching it as one would hide the line until the
    // board changed. A fresh ask that came back empty leaves the cache alone
    // too: the line the owner is looking at is still a good one.
    if result.settled
        && (!fresh || headline.is_some())
        && let Ok(mut c) = state.fye_headline_cache.lock()
    {
        *c = Some((key, headline.clone()));
    }
    let usage = result.usage;
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
    // `settled: false` tells the client this was a failure, not a verdict,
    // so it may ask again later.
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "headline": headline, "settled": result.settled })),
    ))
}

pub async fn message(
    State(state): State<ApiState>,
    Path(message_id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let view = store_call(&state, move |store, account| {
        let message = store.human_read_message(account, message_id)?;
        let mut thread =
            serde_json::to_value(store.thread_view_with_html(account, &message.thread_id)?)
                .map_err(|error| squelch_core::CoreError::Other(error.into()))?;
        thread["cache_allowed"] = store
            .human_cache_allowed(account, &message.thread_id)?
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
            account_id: account, message_id, kind: "triage".into(), trigger: "inspect".into(),
            ..Default::default()
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
