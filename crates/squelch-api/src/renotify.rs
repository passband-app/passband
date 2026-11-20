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
//! QUICK CLEAR IS ANOTHER OUTCOME OF THE SAME PASS (docs/RENOTIFY.md, "Quick
//! clear"): same triggers, same gate, same budget, same tap route. One
//! arrival sends at most one banner: due-soon mail first, then a quick clear
//! that met its floors, then plain "still waiting" mail. It goes out as a
//! `quick_clear` frame, recorded in `renotify_batches` with `kind =
//! 'quick_clear'`.
//!
//! OFF IS AS IF IT DID NOT EXIST. With the `renotify` feature off, no session
//! is written, no pass runs, and both routes 404.

use std::sync::atomic::{AtomicUsize, Ordering};
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
use squelch_core::quick_clear;
use squelch_core::renotify::{self, Banner, Habits, Hold, Moment, Policy, Trigger};
use tokio::sync::{Notify, broadcast, watch};

use crate::error::ApiError;
use crate::handlers::blocking;
use crate::state::ApiState;

/// Frames buffered toward slow Macs. A banner is about now: a receiver that
/// lags simply misses one.
const FRAME_BUFFER: usize = 8;
/// How far back the habits look.
const HABIT_DAYS: i64 = 30;
/// How far back quick clear's calibration looks: long enough for a
/// multiplier to settle, short enough to follow an owner's habits.
const CALIBRATION_DAYS: i64 = 60;
/// Busy intervals read around now, each way.
const BUSY_HORIZON_HOURS: i64 = 12;

/// One banner on its way to the Mac streams: the SSE event name
/// (`renotify` or `quick_clear`) and the encoded payload.
#[derive(Debug, Clone)]
pub(crate) struct BannerFrame {
    pub(crate) event: &'static str,
    pub(crate) payload: Arc<str>,
}

/// What every clone of the state shares about re-notify.
#[derive(Clone)]
pub(crate) struct RenotifyShared {
    pub(crate) config: Arc<RenotifyConfig>,
    /// Banners, as encoded frames, to every Mac stream listening.
    pub(crate) frames: broadcast::Sender<BannerFrame>,
    /// Poked by a Mac stream that opened after a long enough absence.
    pub(crate) arrival: Arc<Notify>,
    /// The session that poke was for: the worker checks it is still open
    /// once the settle time has passed.
    pub(crate) arrived: Arc<Mutex<Option<i64>>>,
    /// Open Mac streams that can show a quick clear (`quick_clear=1`).
    pub(crate) quick_listeners: Arc<AtomicUsize>,
    /// Until when a held quick-clear plan stands: the backlog is not read
    /// again before then ([`quick_clear::HELD_RECHECK_MINS`]). This daemon
    /// is one account's, so one slot is the per-account cache.
    pub(crate) quick_held_until: Arc<Mutex<Option<DateTime<Utc>>>>,
}

impl RenotifyShared {
    pub(crate) fn new(config: RenotifyConfig) -> Self {
        Self {
            config: Arc::new(config),
            frames: broadcast::channel(FRAME_BUFFER).0,
            arrival: Arc::new(Notify::new()),
            arrived: Arc::new(Mutex::new(None)),
            quick_listeners: Arc::new(AtomicUsize::new(0)),
            quick_held_until: Arc::new(Mutex::new(None)),
        }
    }
}

impl Default for RenotifyShared {
    fn default() -> Self {
        Self::new(RenotifyConfig::default())
    }
}

/// One open Mac stream that can show a quick clear, counted while it lives.
pub(crate) struct QuickClearListener(Arc<AtomicUsize>);

impl QuickClearListener {
    pub(crate) fn start(state: &ApiState) -> Self {
        let counter = state.renotify.quick_listeners.clone();
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter)
    }
}

impl Drop for QuickClearListener {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn quick_listening(state: &ApiState) -> bool {
    state.renotify.quick_listeners.load(Ordering::SeqCst) > 0
}

/// A held quick-clear plan still stands at `now`.
fn quick_held(state: &ApiState, now: DateTime<Utc>) -> bool {
    state
        .renotify
        .quick_held_until
        .lock()
        .ok()
        .and_then(|until| *until)
        .is_some_and(|until| now < until)
}

fn hold_quick(state: &ApiState, until: Option<DateTime<Utc>>) {
    if let Ok(mut slot) = state.renotify.quick_held_until.lock() {
        *slot = until;
    }
}

/// The frame's `at`: when the banner was decided, moved on by the time the
/// pass took. `now` is when the pass began; the backlog read sits between
/// that and the send, and a Mac drops a frame more than a minute old.
fn sent_at(now: DateTime<Utc>, began: std::time::Instant) -> DateTime<Utc> {
    now + chrono::Duration::from_std(began.elapsed()).unwrap_or_default()
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
    /// A quick-clear banner went out: its id and its queue, in order.
    QuickClear { batch: i64, message_ids: Vec<i64> },
}

/// Encode a banner as the `renotify` frame's payload. `at` is when it was
/// decided (Unix seconds): like `open_draft`, a frame a sleeping Mac receives
/// late is the app's to drop.
fn frame_payload(
    batch: i64,
    trigger: Trigger,
    at: DateTime<Utc>,
    picks: &[renotify::Candidate],
) -> Option<BannerFrame> {
    let v = serde_json::json!({
        "renotify": batch,
        "trigger": trigger.as_str(),
        "at": at.timestamp(),
        "items": picks,
    });
    serde_json::to_string(&v).ok().map(|payload| BannerFrame {
        event: "renotify",
        payload: Arc::from(payload),
    })
}

/// Encode a quick clear as the `quick_clear` frame's payload: the batch id,
/// the trigger, when it was decided, the numbers the app words the banner
/// from, and the queue in order (smart-clear rows first, with basis
/// `smart_clear`, for the app to turn into one press). Like `renotify`, it
/// names no account: the stream it arrives on is the account's.
fn quick_clear_payload(
    batch: i64,
    trigger: Trigger,
    at: DateTime<Utc>,
    quick: &quick_clear::QuickClear,
) -> Option<BannerFrame> {
    let v = serde_json::json!({
        "quick_clear": batch,
        "trigger": trigger.as_str(),
        "at": at.timestamp(),
        "budget_secs": quick.budget_secs,
        "est_secs": quick.est_secs,
        "fit_count": quick.fit_count,
        "open_count": quick.open_count,
        "smart_clear_count": quick.smart_clear_count,
        "items": quick.items,
    });
    serde_json::to_string(&v).ok().map(|payload| BannerFrame {
        event: "quick_clear",
        payload: Arc::from(payload),
    })
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
    let began = std::time::Instant::now();
    let policy = Policy::from_config(&state.renotify.config);
    let store = state.store.clone();
    let account_id = state.account_id;
    let due_policy = policy.clone();
    let (zone, busy, candidates, fired_today, quick_fired_today, outcomes, last_quick_clear) =
        blocking(state, move || {
            let zone = store.owner_zone(account_id)?.zone;
            let horizon = chrono::Duration::hours(BUSY_HORIZON_HOURS);
            let busy = store.calendar_busy(account_id, now - horizon, now + horizon)?;
            // Unopened mail still waiting, then For-your-eyes mail coming due.
            // The planner keeps one entry per message.
            let mut candidates = store.renotify_candidates(account_id)?;
            candidates.extend(store.renotify_due_candidates(account_id, &due_policy, zone, now)?);
            let day_start = renotify::local_day_start(zone, now);
            let fired_today = store.renotify_fired_since(account_id, day_start)?;
            let quick_fired_today = store.quick_clear_fired_since(account_id, day_start)?;
            let outcomes =
                store.renotify_outcomes(account_id, now - chrono::Duration::days(HABIT_DAYS))?;
            let last_quick_clear = store.last_quick_clear_at(account_id)?;
            Ok((
                zone,
                busy,
                candidates,
                fired_today,
                quick_fired_today,
                outcomes,
                last_quick_clear,
            ))
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
        last_quick_clear,
        quick_fired_today,
        quick_listener: quick_listening(state),
    };
    let stage = match renotify::stage(&policy, &moment, &candidates) {
        Ok(stage) => stage,
        Err(hold) => return Ok(PassOutcome::Held(hold)),
    };
    // The backlog is read only when a quick clear could win: not when
    // due-soon mail will, not when habits, the gap or the cap rule it out,
    // and not again within the hour after a plan held.
    let mut quick_hold = None;
    let quick = if stage.wants_quick_clear() && !quick_held(state, now) {
        let store = state.store.clone();
        let inventory = squelch_core::store::agent_triage::AgentListQuery {
            since: Some(crate::triage::default_feed_since(now)),
            include_done: false,
            // Smart clear's own inventory (`triage::smart_clear`): the whole
            // month, not the Reading board, so the count matches the press.
            reading_board: false,
        };
        let limit = crate::triage::feed_limit(None);
        let (backlog, samples) = blocking(state, move || {
            let backlog = store.quick_clear_backlog(account_id, now, &inventory, limit)?;
            let samples = store.quick_clear_calibration(
                account_id,
                now - chrono::Duration::days(CALIBRATION_DAYS),
            )?;
            Ok((backlog, samples))
        })
        .await?;
        match quick_clear::plan(&backlog, quick_clear::multiplier(&samples)) {
            Ok(quick) => {
                hold_quick(state, None);
                Some(quick)
            }
            Err(hold) => {
                hold_quick(
                    state,
                    Some(now + chrono::Duration::minutes(quick_clear::HELD_RECHECK_MINS)),
                );
                quick_hold = Some(hold);
                eprintln!(
                    "squelchd: renotify {} quick clear held: {}",
                    trigger.as_str(),
                    hold.as_str()
                );
                None
            }
        }
    } else {
        None
    };
    let picks = match renotify::finish(stage, quick) {
        Ok(Banner::Renotify(picks)) => picks,
        Ok(Banner::QuickClear(quick)) => {
            return fire_quick_clear(state, trigger, now, began, quick).await;
        }
        Err(hold) => {
            // Nothing else to send, and a quick clear was tried: say why it
            // held rather than "nothing due".
            let hold = match (hold, quick_hold) {
                (Hold::NothingDue, Some(quick)) => Hold::Quick(quick),
                (hold, _) => hold,
            };
            return Ok(PassOutcome::Held(hold));
        }
    };
    // Counted again at the last moment: the Mac may have gone while the
    // store was being read, and a banner nobody received is not a banner.
    let listeners = state.renotify.frames.receiver_count();
    if listeners == 0 {
        return Ok(PassOutcome::NoListener);
    }
    let message_ids: Vec<i64> = picks.iter().map(|c| c.message_id).collect();
    let store = state.store.clone();
    let ids: Vec<(i64, Option<DateTime<Utc>>)> =
        picks.iter().map(|c| (c.message_id, c.due_at)).collect();
    let batch = blocking(state, move || {
        store.record_renotify(account_id, trigger, now, listeners, &ids)
    })
    .await?;
    if let Some(payload) = frame_payload(batch, trigger, sent_at(now, began), &picks) {
        let _ = state.renotify.frames.send(payload);
    }
    Ok(PassOutcome::Fired { batch, message_ids })
}

/// Record a quick-clear banner and send it, if a Mac is still listening.
async fn fire_quick_clear(
    state: &ApiState,
    trigger: Trigger,
    now: DateTime<Utc>,
    began: std::time::Instant,
    quick: quick_clear::QuickClear,
) -> Result<PassOutcome, ApiError> {
    // Only the Macs that can show it count: one that cannot would drop it.
    let listeners = state.renotify.quick_listeners.load(Ordering::SeqCst);
    if listeners == 0 || state.renotify.frames.receiver_count() == 0 {
        return Ok(PassOutcome::NoListener);
    }
    let message_ids: Vec<i64> = quick.items.iter().map(|p| p.message_id).collect();
    let store = state.store.clone();
    let account_id = state.account_id;
    let items = quick.items.clone();
    let batch = blocking(state, move || {
        store.record_quick_clear(account_id, trigger, now, listeners, &items)
    })
    .await?;
    if let Some(frame) = quick_clear_payload(batch, trigger, sent_at(now, began), &quick) {
        let _ = state.renotify.frames.send(frame);
    }
    Ok(PassOutcome::QuickClear { batch, message_ids })
}

fn log(trigger: Trigger, outcome: &Result<PassOutcome, ApiError>) {
    // Ids and counts only: never a sender or a subject.
    match outcome {
        Ok(PassOutcome::Fired { batch, message_ids }) => eprintln!(
            "squelchd: renotify {} fired batch {batch} ({} messages)",
            trigger.as_str(),
            message_ids.len()
        ),
        Ok(PassOutcome::QuickClear { batch, message_ids }) => eprintln!(
            "squelchd: renotify {} fired quick clear {batch} ({} items)",
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

/// Close the sessions a previous process left open. Call ONCE at startup,
/// BEFORE the router serves: run any later, a Mac that reconnected in the
/// meantime has its live session stamped closed, and its next blip would read
/// as hours away and fire an arrival nobody made.
pub fn close_stale_renotify_sessions(state: &ApiState) {
    if let Err(error) = state
        .store
        .close_stale_client_sessions(state.account_id, Utc::now())
    {
        eprintln!("squelchd: renotify: closing stale sessions failed: {error}");
    }
}

/// THE WORKER. Waits on three things: a Mac arriving, the meeting-end tick,
/// and shutdown. Stale sessions are closed before it starts
/// ([`close_stale_renotify_sessions`]).
pub async fn renotify_worker(
    state: ApiState,
    mut shutdown: watch::Receiver<bool>,
    timing: RenotifyTiming,
) {
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
            // A REPLACEMENT CONNECTION COUNTS AS STILL HERE. A Wi-Fi handoff
            // during the settle closes the session that woke this and opens
            // another that reports no gap (so no second wake), and the owner
            // who just sat down is still sitting there. Whether a Mac is
            // listening right now is the question; the session id was only
            // ever a proxy for it.
            let still_here = still_here || state.renotify.frames.receiver_count() > 0;
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

/// `GET /client/renotify/stats` — the last 30 days of banners, by trigger, by
/// kind and by hour of the owner's day, which triggers the owner's own
/// responses have paused, and quick clear's calibration: predicted against
/// actual seconds per basis over the last 60 days, and the multiplier the
/// next plan will use.
pub async fn renotify_stats(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    if !enabled(&state) {
        return Err(ApiError::not_found());
    }
    let store = state.store.clone();
    let account_id = state.account_id;
    let now = Utc::now();
    let (zone, outcomes, samples) = blocking(&state, move || {
        let zone = store.owner_zone(account_id)?.zone;
        let outcomes =
            store.renotify_outcomes(account_id, now - chrono::Duration::days(HABIT_DAYS))?;
        let samples = store
            .quick_clear_calibration(account_id, now - chrono::Duration::days(CALIBRATION_DAYS))?;
        Ok((zone, outcomes, samples))
    })
    .await?;
    Ok(Json(serde_json::json!({
        "days": HABIT_DAYS,
        "zone": zone.name(),
        "summary": renotify::summarize(&outcomes, zone),
        "quick_clear": {
            "days": CALIBRATION_DAYS,
            "calibration": quick_clear::calibrate(&samples),
        },
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_names_its_batch_and_items_and_nothing_hidden() {
        let at = Utc::now();
        let c = renotify::Candidate {
            why: renotify::Why::Unhandled,
            event_id: Some(9),
            message_id: 4,
            thread_id: "t4".into(),
            kind: squelch_core::types::EventKind::Deadline,
            importance: 80,
            sender: "alice@example.com".into(),
            one_line: "Invoice due Friday".into(),
            deadline: Some("2026-10-30T17:00:00Z".into()),
            due_at: None,
            notified_at: at,
            renotified: 1,
            last_renotified_at: Some(at),
        };
        let frame = frame_payload(12, Trigger::MeetingEnd, at, &[c]).unwrap();
        assert_eq!(frame.event, "renotify");
        let v: serde_json::Value = serde_json::from_str(&frame.payload).unwrap();
        assert_eq!(v["renotify"], 12);
        assert_eq!(v["trigger"], "meeting_end");
        assert_eq!(v["items"][0]["message_id"], 4);
        assert_eq!(v["items"][0]["kind"], "deadline");
        assert_eq!(v["items"][0]["why"], "unhandled");
        assert!(
            v["items"][0].get("due_at").is_none(),
            "only on due-soon items"
        );
        // Bookkeeping stays in the daemon.
        assert!(v["items"][0].get("renotified").is_none());
        assert!(v["items"][0].get("notified_at").is_none());
        // Never mistakable for an event: no top-level `id`.
        assert!(v.get("id").is_none());
    }

    #[test]
    fn the_quick_clear_frame_is_the_contract_shape() {
        use squelch_core::quick_clear::{Basis, ItemKind, Planned, QuickClear};
        let at = Utc::now();
        let quick = QuickClear {
            budget_secs: 300,
            est_secs: 284,
            fit_count: 2,
            open_count: 3,
            smart_clear_count: 1,
            items: vec![
                Planned {
                    message_id: 3,
                    thread_id: "t3".into(),
                    est_secs: 0,
                    raw_secs: 0,
                    basis: Basis::SmartClear,
                    kind: ItemKind::SmartClear,
                    sender: "deals@shop.example".into(),
                    one_line: "Sale ended".into(),
                },
                Planned {
                    message_id: 4,
                    thread_id: "t4".into(),
                    est_secs: 8,
                    raw_secs: 8,
                    basis: Basis::Heuristic,
                    kind: ItemKind::Receipt,
                    sender: "alice@example.com".into(),
                    one_line: "Your order shipped".into(),
                },
            ],
        };
        let frame = quick_clear_payload(12, Trigger::ClientOnline, at, &quick).unwrap();
        assert_eq!(frame.event, "quick_clear");
        let v: serde_json::Value = serde_json::from_str(&frame.payload).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "quick_clear": 12, "trigger": "client_online", "at": at.timestamp(),
                "budget_secs": 300, "est_secs": 284, "fit_count": 2, "open_count": 3,
                "smart_clear_count": 1,
                "items": [
                    {"message_id": 3, "thread_id": "t3", "est_secs": 0,
                     "basis": "smart_clear", "kind": "smart_clear",
                     "sender": "deals@shop.example", "one_line": "Sale ended"},
                    {"message_id": 4, "thread_id": "t4", "est_secs": 8,
                     "basis": "heuristic", "kind": "receipt",
                     "sender": "alice@example.com", "one_line": "Your order shipped"}
                ]
            })
        );
    }
}
