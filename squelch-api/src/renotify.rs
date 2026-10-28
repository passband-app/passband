//! Smart re-notify on the human door (issue #625, docs/RENOTIFY.md): the
//! worker that decides when, the frame that carries a banner to the Mac, and
//! the two routes the app reports back on.
//!
//! WHEN. Two triggers. A Mac event stream (`/client/events?opens=1`) opening
//! after the Mac was away at least `renotify.min_offline_mins` is the owner
//! sitting down: the stream pokes [`RenotifyShared::arrival`] and the worker,
//! once the connection has stayed up `online_settle_secs`, runs a pass. And
//! once a minute the worker reads the owner's busy intervals and runs a pass
//! a few minutes after a meeting ends. What a pass may fire is
//! [`squelch_core::renotify::plan`]'s to decide, never this module's.
//!
//! HOW IT ARRIVES. A `renotify` frame on the Mac's event stream, like
//! `open_draft`: NO `id:`, so it moves no cursor, and a payload key no
//! [`Event`](squelch_core::types::Event) has, so an app that does not know it
//! fails to decode an event and carries on. It is NOT a second `events` row:
//! that log is one row per message, ever, and the phone's push path reads it.
//! A banner is only ever sent to a Mac that is listening, and only recorded
//! when it was.
//!
//! OFF IS AS IF IT DID NOT EXIST. With the `renotify` feature off, no session
//! is written, no pass runs, and both routes 404.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use squelch_core::config::RenotifyConfig;
use squelch_core::features;
use squelch_core::renotify::{self, Habits, Hold, Moment, Policy, Trigger};
use tokio::sync::{Notify, broadcast, watch};

use crate::error::ApiError;
use crate::handlers::blocking;
use crate::state::ApiState;

/// Frames buffered toward slow Macs. A banner is about now: a receiver that
/// lags simply misses one.
const FRAME_BUFFER: usize = 8;
/// How far back the habits look.
const HABIT_DAYS: i64 = 30;
/// Busy intervals read around now, each way.
const BUSY_HORIZON_HOURS: i64 = 12;

/// What every clone of the state shares about re-notify.
#[derive(Clone)]
pub(crate) struct RenotifyShared {
    pub(crate) config: Arc<RenotifyConfig>,
    /// Banners, as encoded frame payloads, to every Mac stream listening.
    pub(crate) frames: broadcast::Sender<Arc<str>>,
    /// Poked by a Mac stream that opened after a long enough absence.
    pub(crate) arrival: Arc<Notify>,
    /// The session that poke was for: the worker checks it is still open
    /// once the settle time has passed.
    pub(crate) arrived: Arc<Mutex<Option<i64>>>,
}

impl RenotifyShared {
    pub(crate) fn new(config: RenotifyConfig) -> Self {
        Self {
            config: Arc::new(config),
            frames: broadcast::channel(FRAME_BUFFER).0,
            arrival: Arc::new(Notify::new()),
            arrived: Arc::new(Mutex::new(None)),
        }
    }
}

impl Default for RenotifyShared {
    fn default() -> Self {
        Self::new(RenotifyConfig::default())
    }
}

fn enabled(state: &ApiState) -> bool {
    state.features().enabled(features::RENOTIFY)
}

/// One open Mac event stream, recorded in `client_sessions` for as long as
/// the stream lives. Dropping it closes the row.
pub(crate) struct MacSession {
    state: ApiState,
    id: i64,
}

impl MacSession {
    /// Record a Mac stream opening, and wake the worker if the Mac had been
    /// away long enough to count as the owner arriving. `None` (nothing
    /// recorded) when the feature is off or the store could not answer: a
    /// session is a signal, and the notification feed must never fail for
    /// want of one.
    pub(crate) async fn open(state: &ApiState, platform: &'static str) -> Option<Self> {
        if !enabled(state) {
            return None;
        }
        let store = state.store.clone();
        let account_id = state.account_id;
        let now = Utc::now();
        let (id, away) = blocking(state, move || {
            store.open_client_session(account_id, platform, now)
        })
        .await
        .ok()?;
        let min_offline = Policy::from_config(&state.renotify.config).min_offline;
        if away.is_some_and(|away| away >= min_offline) {
            if let Ok(mut arrived) = state.renotify.arrived.lock() {
                *arrived = Some(id);
            }
            state.renotify.arrival.notify_one();
        }
        Some(Self {
            state: state.clone(),
            id,
        })
    }
}

impl Drop for MacSession {
    fn drop(&mut self) {
        let store = self.state.store.clone();
        let (account_id, id) = (self.state.account_id, self.id);
        // Best effort, off the runtime: a row a crash leaves open is closed at
        // the next start (`close_stale_client_sessions`).
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn_blocking(move || {
                let _ = store.close_client_session(account_id, id, Utc::now());
            });
        }
    }
}

/// The worker's clock. [`RenotifyTiming::daemon`] in production; tests shrink
/// it.
#[derive(Debug, Clone, Copy)]
pub struct RenotifyTiming {
    /// How often the meeting-end check runs.
    pub tick: Duration,
}

impl RenotifyTiming {
    pub fn daemon() -> Self {
        Self {
            tick: Duration::from_secs(60),
        }
    }
}

/// What one pass did, for the log and for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PassOutcome {
    /// The feature is off for this mailbox.
    Off,
    /// No Mac is listening, so there is nobody to tell.
    NoListener,
    /// The planner held, and why.
    Held(Hold),
    /// A banner went out: its id and the messages it named.
    Fired { batch: i64, message_ids: Vec<i64> },
}

/// Encode a banner as the `renotify` frame's payload. `at` is when it was
/// decided (Unix seconds): like `open_draft`, a frame a sleeping Mac receives
/// late is the app's to drop.
fn frame_payload(
    batch: i64,
    trigger: Trigger,
    at: DateTime<Utc>,
    picks: &[renotify::Candidate],
) -> Option<Arc<str>> {
    let v = serde_json::json!({
        "renotify": batch,
        "trigger": trigger.as_str(),
        "at": at.timestamp(),
        "items": picks,
    });
    serde_json::to_string(&v).ok().map(Arc::from)
}

/// ONE PASS: ask the planner about this moment and, if it picks anything and
/// a Mac is listening, record the banner and send it.
pub async fn run_renotify_pass(
    state: &ApiState,
    trigger: Trigger,
    now: DateTime<Utc>,
) -> Result<PassOutcome, ApiError> {
    if !enabled(state) {
        return Ok(PassOutcome::Off);
    }
    if state.renotify.frames.receiver_count() == 0 {
        return Ok(PassOutcome::NoListener);
    }
    let policy = Policy::from_config(&state.renotify.config);
    let store = state.store.clone();
    let account_id = state.account_id;
    let (zone, busy, candidates, fired_today, outcomes) = blocking(state, move || {
        let zone = store.owner_zone(account_id)?.zone;
        let horizon = chrono::Duration::hours(BUSY_HORIZON_HOURS);
        let busy = store.calendar_busy(account_id, now - horizon, now + horizon)?;
        let candidates = store.renotify_candidates(account_id)?;
        let fired_today =
            store.renotify_fired_since(account_id, renotify::local_day_start(zone, now))?;
        let outcomes =
            store.renotify_outcomes(account_id, now - chrono::Duration::days(HABIT_DAYS))?;
        Ok((zone, busy, candidates, fired_today, outcomes))
    })
    .await?;
    let habits = Habits::learn(&outcomes);
    let moment = Moment {
        now,
        trigger,
        zone,
        busy: &busy,
        fired_today,
        habits: &habits,
    };
    let picks = match renotify::plan(&policy, &moment, &candidates) {
        Ok(picks) => picks,
        Err(hold) => return Ok(PassOutcome::Held(hold)),
    };
    // Counted again at the last moment: the Mac may have gone while the
    // store was being read, and a banner nobody received is not a banner.
    let listeners = state.renotify.frames.receiver_count();
    if listeners == 0 {
        return Ok(PassOutcome::NoListener);
    }
    let message_ids: Vec<i64> = picks.iter().map(|c| c.message_id).collect();
    let store = state.store.clone();
    let ids = message_ids.clone();
    let batch = blocking(state, move || {
        store.record_renotify(account_id, trigger, now, listeners, &ids)
    })
    .await?;
    if let Some(payload) = frame_payload(batch, trigger, now, &picks) {
        let _ = state.renotify.frames.send(payload);
    }
    Ok(PassOutcome::Fired { batch, message_ids })
}

fn log(trigger: Trigger, outcome: &Result<PassOutcome, ApiError>) {
    // Ids and counts only: never a sender or a subject.
    match outcome {
        Ok(PassOutcome::Fired { batch, message_ids }) => eprintln!(
            "squelchd: renotify {} fired batch {batch} ({} messages)",
            trigger.as_str(),
            message_ids.len()
        ),
        Ok(PassOutcome::Held(hold)) => eprintln!(
            "squelchd: renotify {} held: {}",
            trigger.as_str(),
            hold.as_str()
        ),
        Ok(_) => {}
        Err(_) => eprintln!("squelchd: renotify {} pass failed", trigger.as_str()),
    }
}

/// THE WORKER. Closes sessions a previous process left open, then waits on
/// three things: a Mac arriving, the meeting-end tick, and shutdown.
pub async fn renotify_worker(
    state: ApiState,
    mut shutdown: watch::Receiver<bool>,
    timing: RenotifyTiming,
) {
    {
        let store = state.store.clone();
        let account_id = state.account_id;
        let _ = blocking(&state, move || {
            store
                .close_stale_client_sessions(account_id, Utc::now())
                .map(|_| ())
        })
        .await;
    }
    let policy = Policy::from_config(&state.renotify.config);
    let settle = Duration::from_secs(state.renotify.config.online_settle_secs);
    let mut last_meeting_end: Option<DateTime<Utc>> = None;
    loop {
        let arrived = tokio::select! {
            _ = tokio::time::sleep(timing.tick) => false,
            _ = state.renotify.arrival.notified() => true,
            res = shutdown.changed() => {
                if res.is_err() || *shutdown.borrow() {
                    return;
                }
                continue;
            }
        };
        if *shutdown.borrow() {
            return;
        }
        if arrived {
            tokio::select! {
                _ = tokio::time::sleep(settle) => {}
                _ = shutdown.changed() => return,
            }
            let session = state
                .renotify
                .arrived
                .lock()
                .ok()
                .and_then(|mut a| a.take());
            let Some(session) = session else { continue };
            let store = state.store.clone();
            let account_id = state.account_id;
            let still_here = blocking(&state, move || {
                store.client_session_open(account_id, session)
            })
            .await
            .unwrap_or(false);
            if still_here {
                let outcome = run_renotify_pass(&state, Trigger::ClientOnline, Utc::now()).await;
                log(Trigger::ClientOnline, &outcome);
            }
            continue;
        }
        if !enabled(&state) || state.renotify.frames.receiver_count() == 0 {
            continue;
        }
        let now = Utc::now();
        let store = state.store.clone();
        let account_id = state.account_id;
        let busy = blocking(&state, move || {
            let horizon = chrono::Duration::hours(BUSY_HORIZON_HOURS);
            store.calendar_busy(account_id, now - horizon, now + horizon)
        })
        .await;
        let Ok(busy) = busy else { continue };
        if let Some(end) = renotify::meeting_end_due(&policy, &busy, now, last_meeting_end) {
            last_meeting_end = Some(end);
            let outcome = run_renotify_pass(&state, Trigger::MeetingEnd, now).await;
            log(Trigger::MeetingEnd, &outcome);
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct OpenedBody {
    /// The message the tap opened, when the banner named several.
    #[serde(default)]
    message_id: Option<i64>,
}

/// `POST /client/renotify/{id}/opened` — the app reports a tap on a banner.
/// 204, or 404 for a banner this mailbox does not have (or with the feature
/// off). Idempotent: the first tap is the one kept.
pub async fn renotify_opened(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
    body: Option<Json<OpenedBody>>,
) -> Result<impl IntoResponse, ApiError> {
    if !enabled(&state) {
        return Err(ApiError::not_found());
    }
    let message_id = body.and_then(|Json(b)| b.message_id);
    let store = state.store.clone();
    let account_id = state.account_id;
    let found = blocking(&state, move || {
        store.mark_renotify_opened(account_id, id, message_id, Utc::now())
    })
    .await?;
    if found {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found())
    }
}

/// `GET /client/renotify/stats` — the last 30 days of banners, by trigger and
/// by hour of the owner's day, and which triggers the owner's own responses
/// have paused.
pub async fn renotify_stats(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    if !enabled(&state) {
        return Err(ApiError::not_found());
    }
    let store = state.store.clone();
    let account_id = state.account_id;
    let now = Utc::now();
    let (zone, outcomes) = blocking(&state, move || {
        let zone = store.owner_zone(account_id)?.zone;
        let outcomes =
            store.renotify_outcomes(account_id, now - chrono::Duration::days(HABIT_DAYS))?;
        Ok((zone, outcomes))
    })
    .await?;
    Ok(Json(serde_json::json!({
        "days": HABIT_DAYS,
        "zone": zone.name(),
        "summary": renotify::summarize(&outcomes, zone),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_names_its_batch_and_items_and_nothing_hidden() {
        let at = Utc::now();
        let c = renotify::Candidate {
            event_id: 9,
            message_id: 4,
            thread_id: "t4".into(),
            kind: squelch_core::types::EventKind::Deadline,
            importance: 80,
            sender: "alice@example.com".into(),
            one_line: "Invoice due Friday".into(),
            deadline: Some("2026-10-30T17:00:00Z".into()),
            notified_at: at,
            renotified: 1,
            last_renotified_at: Some(at),
        };
        let raw = frame_payload(12, Trigger::MeetingEnd, at, &[c]).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["renotify"], 12);
        assert_eq!(v["trigger"], "meeting_end");
        assert_eq!(v["items"][0]["message_id"], 4);
        assert_eq!(v["items"][0]["kind"], "deadline");
        // Bookkeeping stays in the daemon.
        assert!(v["items"][0].get("renotified").is_none());
        assert!(v["items"][0].get("notified_at").is_none());
        // Never mistakable for an event: no top-level `id`.
        assert!(v.get("id").is_none());
    }
}
