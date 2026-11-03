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

/// How many items a feed page holds: 200 unless asked, never more than 1000.
/// Shared with [`smart_clear`], which scans the Reading inventory a 30-day
/// feed would return (not the shorter Reading board).
fn feed_limit(requested: Option<usize>) -> usize {
    requested.unwrap_or(200).clamp(1, 1000)
}

/// Where a feed's window starts when the caller names no `since`.
fn default_feed_since(now: chrono::DateTime<Utc>) -> chrono::DateTime<Utc> {
    now - chrono::Duration::days(30)
}

pub async fn feed(
    State(state): State<ApiState>,
    Query(query): Query<FeedQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let limit = feed_limit(query.limit);
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
            Some(query.since.unwrap_or(default_feed_since(now)))
        },
        include_done: query.include_done.unwrap_or(false),
        // Asked for no dates of its own: the Reading board, not 30 days.
        reading_board: query.all_time.is_none() && query.since.is_none(),
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
        // The rows the line could speak for: "most of it was filed" is not
        // a claim auth or restricted rows on the board can make true.
        let on_board = fye
            .iter()
            .filter(|item| HeadlineItem::speaks_for(item))
            .count();
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

#[derive(Deserialize)]
pub struct AttentionReport {
    /// Milliseconds this message was on screen since the last report.
    #[serde(default)]
    pub dwell_ms: u64,
    /// Links in it followed since the last report. A count: the client never
    /// sends which.
    #[serde(default)]
    pub link_clicks: u32,
}

/// POST /client/v2/messages/{message_id}/attention: add one report of reading
/// attention to a message's totals. See
/// [`AgentTriageStore::record_message_attention`] for the clamps.
///
/// ALWAYS 204 FOR A WELL-FORMED REPORT, and this is where it parts from
/// `/opened` above, which answers 404 for a message it does not know. Two
/// reasons, and the first is a contract with clients already shipped:
///
/// - A client reads 404 from THIS route as "this daemon predates the route"
///   and stops reporting for the rest of the session. A 404 for one unknown
///   id (mail deleted on another device while it was on screen) would silence
///   every report after it.
/// - Mail KNOWN to be kept close is accepted and never written: auth mail and
///   restricted mail by their stored decision or assessment, and legacy
///   sealed rows (`known_sealed_mail_sql!`, not the retired `sensitivity`
///   column alone). Mail not yet assessed IS written; see that macro's
///   sibling for why that is safe.
///   Answering it differently from a missing id would tell the caller which
///   an id was; one status for written, kept close and missing alike says
///   nothing.
///
/// A malformed body still gets the extractor's ordinary 4xx.
///
/// NO AUDIT ROW, as `/opened` writes none. `audit_log` records what the owner
/// DID to their mailbox (sent, unsubscribed, changed a rule); this is
/// telemetry about looking at it, it fires several times per message read,
/// and a log flooded with it would bury the entries the log exists to keep.
pub async fn attention(
    State(state): State<ApiState>,
    Path(message_id): Path<i64>,
    Json(report): Json<AttentionReport>,
) -> Result<impl IntoResponse, ApiError> {
    store_call(&state, move |store, account| {
        store.record_message_attention(account, message_id, report.dwell_ms, report.link_clicks)
    })
    .await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// POST /client/v2/reading/smart_clear: "I trust you to clean this out". Marks
/// done, as one undoable batch, the open Reading mail that has expired or
/// that the owner plainly does not want. What qualifies, and what is never
/// touched, is [`squelch_core::triage::smart_clear`]; this is the order it
/// happens in, which is about the store mutex:
///
/// 1. One locked read finds the rows worth asking the model about.
/// 2. THE MODEL IS ASKED WITH NO LOCK HELD. It can take twenty seconds, and
///    sync shares that mutex.
/// 3. One locked transaction judges every row AGAIN and writes. The owner was
///    using the app during step 2, so nothing step 1 saw is trusted: a model
///    verdict lands only on a row that is still open and still undecided.
///
/// `model` says what became of step 2. Every `skipped_*` value means the
/// sweep ran on stored facts alone, which is a smaller sweep and never an
/// error: `skipped_nothing_to_ask` (no undecided row has a summary),
/// `skipped_unconfigured` (no key), `skipped_cap` (the day's passes are
/// spent), `skipped_failed` (a refusal, a timeout, an error, or an answer that
/// failed its checks and was thrown away whole). With `skipped_failed`,
/// `model_detail` says which: `refused`, `timeout`, `discarded` or `error`;
/// it is `null` otherwise.
///
/// `rows` is the receipt's full list: one entry per message written done, in
/// `message_ids`' order, with its subject, sender, date and reason, read in
/// the transaction that wrote it. `by_reason` is the tally of it.
///
/// ONE AUDIT ROW for the batch, not one per message: the batch id is the
/// target and everything it took can be read back from it.
///
/// ONE PRESS AT A TIME. Each press reads a hundred senders' mail under the
/// store mutex and may hold a model call open for twenty seconds; a client
/// that loops on the button would otherwise queue those back to back and
/// starve sync. A press that arrives while another is running answers 429
/// before it touches the store.
pub async fn smart_clear(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    use squelch_core::metrics::SMART_CLEAR_USAGE_CATEGORY;
    use squelch_core::triage::smart_clear::{self as sd, ClearReason};
    use std::sync::atomic::Ordering;

    /// Clears the in-flight mark however the press ends, an error or a
    /// dropped connection included.
    struct Running(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Running {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    if state.smart_clear_running.swap(true, Ordering::AcqRel) {
        return Err(ApiError::new(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "a smart clear is already running",
        ));
    }
    let _running = Running(state.smart_clear_running.clone());

    let limit = feed_limit(None);
    let now = Utc::now();
    // NOT the Reading board: smart clear cleans the whole month of open
    // Reading mail, including what has aged off the board. Its stale-promo
    // rule only fires on mail a week old, which the board never shows.
    let inventory = squelch_core::store::agent_triage::AgentListQuery {
        since: Some(default_feed_since(now)),
        include_done: false,
        reading_board: false,
    };
    let day = now.format("%Y-%m-%d").to_string();
    let scan_inventory = inventory.clone();
    let scan_day = day.clone();
    let (scan, spent) = store_call(&state, move |store, account| {
        let scan = store.smart_clear_scan(account, now, &scan_inventory, limit)?;
        // The runaway guard, read from the same ledger the passes bill into.
        let spent: u64 = store
            .list_usage_by_category(account, 1)?
            .into_iter()
            .filter(|(c, _)| c == SMART_CLEAR_USAGE_CATEGORY)
            .flat_map(|(_, days)| days)
            .filter(|d| d.day == scan_day)
            .map(|d| d.calls)
            .sum();
        Ok((scan, spent))
    })
    .await?;

    let mut verdicts = std::collections::HashMap::new();
    let mut model_detail: Option<&'static str> = None;
    let model = if scan.asks.is_empty() {
        "skipped_nothing_to_ask"
    } else if let Some(client) = state.smart_clear.clone() {
        // RESERVE before calling, as the headline does and for its reasons:
        // the ledger only learns of calls the provider reported usage for,
        // and two presses at once would both read the same total.
        let reserved = state.smart_clear_attempts.lock().is_ok_and(|mut t| {
            if t.0 != day {
                *t = (day.clone(), 0);
            }
            if u64::from(t.1).max(spent) >= u64::from(sd::DAILY_CAP) {
                return false;
            }
            t.1 += 1;
            true
        });
        if !reserved {
            "skipped_cap"
        } else {
            let pass = sd::judge(&client, now, &scan.asks).await;
            // Billed on every outcome that reported usage, a refusal and a
            // garbled answer included.
            if let Some(u) = pass.usage {
                let bill_day = day.clone();
                let _ = store_call(&state, move |store, account| {
                    if let Err(e) = store.extract_bump_usage(
                        account,
                        &bill_day,
                        SMART_CLEAR_USAGE_CATEGORY,
                        u.into(),
                    ) {
                        eprintln!("squelch: smart clear usage ledger bump failed ({e})");
                    }
                    Ok(())
                })
                .await;
            }
            if pass.answered {
                verdicts = pass.verdicts;
                "ok"
            } else {
                model_detail = pass.failure;
                "skipped_failed"
            }
        }
    } else {
        "skipped_unconfigured"
    };

    // A FRESH CLOCK for the write: the 24-hour floor and every "has passed"
    // are judged as of the moment the rows are taken, not of the press.
    let outcome = store_call(&state, move |store, account| {
        store.smart_clear_apply(account, Utc::now(), &inventory, limit, &scan, &verdicts)
    })
    .await?;

    let mut by_reason = serde_json::Map::new();
    for reason in ClearReason::ALL {
        let n = outcome.cleared.iter().filter(|(_, r)| *r == reason).count();
        by_reason.insert(reason.as_str().into(), json!(n));
    }
    // THE FULL LIST behind the receipt: each message taken, with its subject,
    // its sender and why. Mail text, so it is here and nowhere else: this
    // route is the human door, the response is `no-store`, and none of it is
    // written to the batch or the audit row. Sealed mail is never cleared, so
    // it is never listed.
    let rows: Vec<serde_json::Value> = outcome
        .rows
        .iter()
        .map(|row| {
            json!({
                "message_id": row.message_id,
                "reason": row.reason.as_str(),
                "sender": row.sender,
                "sender_name": row.sender_name,
                "subject": row.subject,
                "received_at": row.received_at,
            })
        })
        .collect();
    if let Some(batch) = outcome.batch_id {
        crate::handlers::audit_action(
            &state,
            "reading.smart_clear",
            Some(batch.to_string()),
            &outcome.cleared.len().to_string(),
        )
        .await;
    }
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({
            "batch_id": outcome.batch_id.map(|id| id.to_string()),
            "cleared": outcome.cleared.len(),
            "senders": outcome.senders,
            "by_reason": by_reason,
            "message_ids": outcome.cleared.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            "rows": rows,
            "model": model,
            "model_detail": model_detail,
        })),
    ))
}

/// POST /client/v2/reading/smart_clear/{batch_id}/undo: give a batch back.
/// Reopens exactly the rows that batch took and that nobody has touched
/// since, each to the status it had. A second undo reopens nothing and says
/// `0`; a batch this account does not have is 404, whether it is unknown or
/// somebody else's.
pub async fn smart_clear_undo(
    State(state): State<ApiState>,
    Path(batch_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    // The id is handed out as a string so the client never does arithmetic on
    // it; one that is not a number names no batch.
    let Ok(batch) = batch_id.parse::<i64>() else {
        return Err(ApiError::not_found());
    };
    let reopened = store_call(&state, move |store, account| {
        store.smart_clear_undo(account, batch, Utc::now())
    })
    .await?
    .ok_or_else(ApiError::not_found)?;
    crate::handlers::audit_action(
        &state,
        "reading.smart_clear_undo",
        Some(batch.to_string()),
        &reopened.to_string(),
    )
    .await;
    Ok(Json(json!({ "reopened": reopened })))
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
