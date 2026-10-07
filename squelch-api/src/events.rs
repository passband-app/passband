//! Notification delivery on the human door: `GET /client/events` (SSE) and
//! `GET /client/events/{id}`, the iOS NSE's by-id fetch after an opaque push.
//! A pure READER of core's `events` table: no delivered/cursor flag is written
//! here, every client carries its own `after=<id>`.
//!
//! SEALED MAIL DOES APPEAR HERE, and this module is the path it appears on
//! (docs/NOTIFY.md §11.6). The fast lane's sealed ping is an ordinary `events`
//! row with `sealed_kind` set, whose `one_line` is derived FROM THE KIND ALONE
//! ("Login code arrived") and whose `sender` is the from-address `/client/sealed`
//! already serves: no subject, no body, no code, so there is nothing here to
//! gate and no query may start gating on `sealed_kind`. What the field is for is
//! ROUTING, and it is the client's job, not this module's: a frame carrying
//! `sealed_kind` is NOT a thread arrival and must not be treated as one — the tap
//! belongs on the auth reveal flow, because `thread_guard_and_subject` 404s a
//! sealed thread. See docs/NOTIFY.md §11.6 for the full client contract.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json,
    extract::{Path, Query, State},
    response::{
        IntoResponse, Sse,
        sse::{Event as SseEvent, KeepAlive},
    },
};
use serde::Deserialize;
use squelch_core::store::{SqliteStore, Store};
use squelch_core::types::{AccountId, Event};
use tokio::sync::{broadcast, mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;

use crate::error::ApiError;
use crate::handlers::blocking;
use crate::state::ApiState;

/// Rows one replay read pulls at a time. Bounds how long the store mutex is
/// held per read: a client resuming after a week offline must not lock out the
/// sync engine with one giant SELECT. The replay loops until drained.
const REPLAY_BATCH: usize = 100;

/// Frames buffered toward the client before the pump task blocks. Backpressure
/// is deliberate: a stalled client slows its own pump instead of growing an
/// unbounded queue in the daemon.
const CHANNEL_BUFFER: usize = 64;

/// Comment-ping interval, so proxies and NATs do not reap an idle connection —
/// a notification feed can legitimately be silent for hours.
const KEEPALIVE: Duration = Duration::from_secs(15);

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    /// The client's own cursor: send everything with `id > after`, then follow.
    after: Option<i64>,
    /// `1` from a Mac app that understands [`open_draft_frame`] and will act
    /// on it. OPT-IN, because the frame is useless to anything else and the
    /// daemon counts the connections that asked as "a Mac is listening": an
    /// older app, or the phone, must not make that count true.
    ///
    /// A STRING, compared to `"1"`: a typed number here would make any other
    /// spelling (`opens=true`) a 400 for the whole notification feed, and an
    /// opt-in nobody understood should cost only the opt-in.
    #[serde(default)]
    opens: Option<String>,
}

/// Where a connection starts reading. WITH `after`: exactly there (exclusive);
/// `after=0` legitimately means "replay the whole log" and a negative clamps to
/// it rather than 400ing. WITHOUT `after`: the newest id, i.e. LIVE-ONLY — a
/// cursorless fresh install must not be handed the backlog as a notification
/// storm for mail the user long since dealt with.
fn start_cursor(after: Option<i64>, latest: i64) -> i64 {
    match after {
        Some(a) => a.max(0),
        None => latest,
    }
}

/// The wake channel for one connection: the state's notifier, else the store's,
/// else a private channel plus its sender as an ANCHOR. The anchor matters — a
/// channel whose sender was dropped reports `Closed` immediately and would end
/// the stream on connect. With no notifier nothing can append events either, so
/// parking is honest; replay and keep-alive still work.
fn subscribe(state: &ApiState) -> (broadcast::Receiver<i64>, Option<broadcast::Sender<i64>>) {
    if let Some(tx) = state.event_notifier() {
        return (tx.subscribe(), None);
    }
    if let Some(tx) = state.store.event_notifier() {
        return (tx.subscribe(), None);
    }
    let (tx, rx) = broadcast::channel(1);
    (rx, Some(tx))
}

/// Encode one event as an SSE frame. WIRE CONTRACT: the SSE `id:` field IS the
/// event id, so `Last-Event-ID` and our `after=` are the same number.
fn frame(ev: &Event) -> Option<SseEvent> {
    // `id` panics on embedded newlines; an integer has none.
    SseEvent::default().id(ev.id.to_string()).json_data(ev).ok()
}

/// "Bring this draft up" (the agent door's `open_draft`), as an SSE frame.
///
/// NO `id:`. The id field is the notification cursor (see [`frame`]) and this
/// is not a row in that log; a frame without one inherits the connection's
/// last id on the client, which moves nothing. The payload is one key no
/// [`Event`] has, so a reader that does not know it fails to decode an event
/// and carries on.
///
/// `at` IS WHEN THE REQUEST WAS MADE (Unix seconds). A connection to a Mac
/// that went to sleep stays open on this side for minutes, and the frame is
/// then delivered when it wakes: the app drops a request that is not fresh,
/// so nobody opens their laptop to a window thrown at them by something an
/// agent did an hour ago.
fn open_draft_frame(draft_id: i64) -> SseEvent {
    let at = chrono::Utc::now().timestamp();
    SseEvent::default()
        .event("open_draft")
        .data(format!("{{\"open_draft\":{draft_id},\"at\":{at}}}"))
}

/// Send every event past `cursor`, oldest first, in bounded batches. `None`
/// means the connection is over (client hung up, or a store read failed — a
/// failed read ends the stream rather than retrying, since the client reconnects
/// with its own cursor).
async fn pump(
    store: &Arc<SqliteStore>,
    account_id: AccountId,
    mut cursor: i64,
    tx: &mpsc::Sender<Result<SseEvent, Infallible>>,
) -> Option<i64> {
    loop {
        let s = store.clone();
        let batch =
            tokio::task::spawn_blocking(move || s.events_after(account_id, cursor, REPLAY_BATCH))
                .await
                .ok()?
                .ok()?;
        let drained = batch.len() < REPLAY_BATCH;
        for ev in batch {
            // Advance FIRST: an unencodable event must not stall the cursor.
            cursor = ev.id;
            let s = store.clone();
            let event_id = ev.id;
            if !tokio::task::spawn_blocking(move || {
                s.notification_delivery_allowed(account_id, event_id)
            })
            .await
            .ok()?
            .ok()?
            {
                continue;
            }
            match frame(&ev) {
                Some(f) => tx.send(Ok(f)).await.ok()?,
                None => continue,
            }
        }
        if drained {
            return Some(cursor);
        }
    }
}

/// Drive one connection until the client leaves or the notifier closes.
/// `_anchor` keeps a fallback channel alive for its duration; see [`subscribe`].
async fn run(
    store: Arc<SqliteStore>,
    account_id: AccountId,
    start: i64,
    mut wake: broadcast::Receiver<i64>,
    _anchor: Option<broadcast::Sender<i64>>,
    mut opens: Option<broadcast::Receiver<i64>>,
    tx: mpsc::Sender<Result<SseEvent, Infallible>>,
) {
    // Replay first — a no-op for a live-only connection.
    let mut cursor = match pump(&store, account_id, start, &tx).await {
        Some(c) => c,
        None => return,
    };

    loop {
        tokio::select! {
            recv = wake.recv() => match recv {
                // A new id and a lagged wake are handled identically: the
                // payload is only a hint, the table is the truth, so re-reading
                // from OUR cursor brings back everything missed.
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {
                    cursor = match pump(&store, account_id, cursor, &tx).await {
                        Some(c) => c,
                        None => return,
                    };
                }
                // The store (and with it the process) is going away.
                Err(broadcast::error::RecvError::Closed) => return,
            },
            // A draft to show, for a connection that asked to be told. A
            // lagged receiver simply missed some: there is no table to
            // re-read, and a stale "show this" is worse than a dropped one.
            Some(open) = next_open(&mut opens) => {
                if let Some(draft_id) = open
                    && tx.send(Ok(open_draft_frame(draft_id))).await.is_err()
                {
                    return;
                }
            }
            // The client hung up. A send would notice too, but a send needs a
            // wake and a quiet mailbox may never deliver one.
            _ = tx.closed() => return,
        }
    }
}

/// The next open-draft request on a connection that subscribed: `Some(id)`,
/// or `None` for a lag (nothing to send). Pends forever on a connection that
/// did not subscribe, and stops listening if the channel closes.
async fn next_open(opens: &mut Option<broadcast::Receiver<i64>>) -> Option<Option<i64>> {
    let Some(rx) = opens else {
        return std::future::pending().await;
    };
    match rx.recv().await {
        Ok(id) => Some(Some(id)),
        Err(broadcast::error::RecvError::Lagged(_)) => Some(None),
        Err(broadcast::error::RecvError::Closed) => {
            *opens = None;
            Some(None)
        }
    }
}

/// Resolve when the daemon starts shutting down, or never if nothing wired a
/// signal in. A dropped sender counts as shutdown.
async fn shutting_down(shutdown: Option<watch::Receiver<bool>>) {
    match shutdown {
        Some(mut rx) => {
            let _ = rx.wait_for(|stopping| *stopping).await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// `GET /client/events` — the live notification feed, `text/event-stream`.
/// `?after=<id>` replays past that cursor then follows; without it there is NO
/// replay (see [`start_cursor`]). Nothing about the connection is persisted.
pub async fn events_stream(
    State(state): State<ApiState>,
    Query(q): Query<EventsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    // SUBSCRIBE BEFORE READING THE CURSOR: anything appended in between lands in
    // the broadcast queue and is re-read past our cursor, so the replay/live
    // seam cannot drop an event.
    let (wake, anchor) = subscribe(&state);

    let store = state.store.clone();
    let account_id = state.account_id;
    let latest = if q.after.is_none() {
        let store = store.clone();
        blocking(move || store.latest_event_id(account_id)).await?
    } else {
        // An explicit replay cursor never uses the latest id. Avoid joining
        // the store queue just to read a value start_cursor will discard.
        0
    };
    let start = start_cursor(q.after, latest);

    // Subscribed for the life of the connection, and dropped with it: the
    // receiver count IS the number of Macs listening.
    let opens = (q.opens.as_deref() == Some("1")).then(|| state.draft_opens.subscribe());

    let (tx, rx) = mpsc::channel::<Result<SseEvent, Infallible>>(CHANNEL_BUFFER);
    let shutdown = state.shutdown.clone();
    tokio::spawn(async move {
        // One shutdown gate over BOTH replay and follow: dropping the pump ends
        // the response body, which is what lets axum's graceful shutdown finish.
        // A lost in-flight frame comes back on the client's next reconnect.
        tokio::select! {
            _ = shutting_down(shutdown) => {}
            _ = run(store, account_id, start, wake, anchor, opens, tx) => {}
        }
    });

    Ok(Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::new().interval(KEEPALIVE)))
}

/// `GET /client/events/{id}` — one event as JSON, scoped to the account. An
/// unknown id and another account's id are the SAME 404: an event id is a
/// guessable integer, so "no such event" and "not yours" must be indistinguishable.
pub async fn get_event(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let store = state.store.clone();
    let account_id = state.account_id;
    let event = blocking(move || store.event_by_id(account_id, id)).await?;
    event.map(Json).ok_or_else(ApiError::not_found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_cursor_starts_live_only() {
        // A fresh install joins at the head and is told about none of the 42.
        assert_eq!(start_cursor(None, 42), 42);
        assert_eq!(start_cursor(None, 0), 0);
    }

    #[test]
    fn a_cursor_is_honoured_verbatim() {
        assert_eq!(start_cursor(Some(7), 42), 7);
        // Explicit zero means "I have never seen anything; replay the log".
        assert_eq!(start_cursor(Some(0), 42), 0);
        // Past the head is not an error: the next real event still has a bigger id.
        assert_eq!(start_cursor(Some(99), 42), 99);
        // Garbage clamps to the harmless reading instead of 400ing.
        assert_eq!(start_cursor(Some(-5), 42), 0);
    }
}
