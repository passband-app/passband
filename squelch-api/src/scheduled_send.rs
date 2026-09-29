//! Scheduled send: the `/client/scheduled` door and the worker that fires it.
//! See docs/SCHEDULED-SEND.md; §3 is the schedule handler, §4 the worker, §5
//! the rest of the door.
//!
//! HUMAN DOOR ONLY, like drafts: the outbox is the user's own finished mail
//! waiting to go, and no `/mcp` surface reads `scheduled_sends` or the files
//! it claims. Every response on these routes is `no-store`, errors included
//! (a listing and a cancel both carry the user's own words): [`routes`] sets
//! it on the way out, so an extractor's own rejection (a malformed body, a
//! non-numeric id) carries it too.
//!
//! THE SCHEDULE HANDLER DOES EVERYTHING A SEND DOES BEFORE THE GMAIL CALL,
//! because the user is present to answer: validation, the files, the outbound
//! guard (a forward's original is read now), and the write credential. The
//! worker then fires the frozen request through the same `perform_send` a
//! send uses, with the guard SETTLED: nothing re-scans.
//!
//! LOGS AND AUDIT ROWS CARRY IDS, STATES AND REASONS ONLY. The stored
//! `request` is mail content and is never logged or audited.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use squelch_core::store::{
    ATTACHMENT_MISSING, CancelOutcome, ParentContext, SCHEDULED_MAX_ATTEMPTS, ScheduledSend,
    ScheduledSendState, SearchClock, TOO_MANY_SCHEDULED,
};
use squelch_core::types::GroupMode;

use crate::error::ApiError;
use crate::gmail_write::{GmailWriteClient, SentRef, WriteError, send_budget};
use crate::group_send;
use crate::handlers::{
    Fire, MAX_FORWARD_RAW_BYTES, OutboundAttachmentView, SendBody, SendGuard, SendOutcome,
    audit_action, composition_kinds, dedup_ids, echo_sent, forward_original_kinds, guard_verdict,
    load_send_attachments, mail_attachment, perform_send, read_forward_original, resolve_target,
    send_target, store_call, validate_send, write_client,
};
use crate::state::ApiState;

/// Audit action for scheduling, and for every refusal on the way to it.
const AUDIT_SCHEDULE: &str = "schedule";
const AUDIT_CANCEL: &str = "schedule.cancel";
const AUDIT_NOW: &str = "schedule.now";

/// How far ahead a mail may be scheduled: a year, counted as 366 days so
/// "the same date next year" is accepted across a leap day. Past that it is
/// far more likely a typo in the palette (2027 for 2026) than an intent, and a
/// row that sits for longer than the write credential's life is a failure
/// waiting to happen.
const MAX_SCHEDULE_AHEAD: chrono::Duration = chrono::Duration::days(366);

/// The fixed part of the worker's claim lease, before the send's own budget
/// ([`send_budget`], which grows with the message) is added per row. It covers
/// everything a fire spends on Gmail besides the upload itself, each bounded
/// by the write client's 60 s timeout: the pre-fire Message-ID search, the
/// parent's headers (or a forward's original), the 5 s echo, and the store.
/// A send that has not settled inside its lease is presumed to have died
/// mid-send, and recovery asks Gmail by Message-ID before anything is retried.
pub(crate) const SCHEDULED_LEASE_BASE: chrono::Duration = chrono::Duration::seconds(180);

/// How long after a lease lapses recovery waits before asking Gmail, and how
/// far a "not found" pushes the re-fire. Gmail's search index trails a send by
/// seconds to minutes; asking the instant a lease lapsed could read a mail
/// that went out a moment ago as "never sent" and send it twice. The re-fire
/// asks again first (its row is `maybe_sent`), so a slow index gets two looks.
pub(crate) const RECOVERY_GRACE: chrono::Duration = chrono::Duration::minutes(5);

/// When recovery could not ask Gmail (no credential, the search failed), how
/// long before it asks about that row again: one call per row per minute,
/// not one per tick.
const RECOVERY_RETRY: chrono::Duration = chrono::Duration::seconds(60);

/// How long past its lease a `sending` row may wait on a recovery that cannot
/// be asked before it is settled `failed:recovery` for the user. Such a row
/// MAY have been sent; "send now" on it searches before it sends. The same
/// bound holds a re-fire's pre-fire search that keeps failing to be made,
/// counted from its first "could not ask".
pub(crate) const RECOVERY_GIVE_UP: chrono::Duration = chrono::Duration::minutes(30);

/// "Not now" answers that must not spend an attempt (Gmail's budget pacing the
/// mailbox, a pre-fire search that could not be made) come back this far out.
const NOT_NOW_RETRY: chrono::Duration = chrono::Duration::minutes(2);

/// Claims one pass makes before it yields to the next tick, so a backlog (a
/// laptop that slept through a morning of scheduled mail) drains in steps a
/// shutdown can interrupt, not one unbounded burst.
pub(crate) const MAX_PER_PASS: usize = 20;

/// Base64 on the wire, per byte of file: 4/3 for the encoding, and a CRLF
/// after every 76 characters on top (78/76), about 1.368. Rounded up.
const BASE64_WIRE_PERCENT: u64 = 137;

/// The worker's clock: the wall clock, plus the test hook's skew (zero in
/// production). Every time the worker compares or stamps reads this, so a test
/// can step past a grace without sleeping.
fn clock(state: &ApiState) -> DateTime<Utc> {
    Utc::now() + state.worker_clock_skew
}

/// `2^attempts` minutes after a failed attempt: 2, then 4 (the third failure
/// is terminal, so 8 is never waited out).
fn retry_backoff(attempts: u32) -> chrono::Duration {
    chrono::Duration::minutes(1i64 << attempts.min(16))
}

/// The outbox's routes, each response `no-store` whatever produced it: the
/// handler, an extractor's rejection, or an error. One layer rather than one
/// call per handler, so no arm and no rejection can forget it.
pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/client/scheduled",
            get(list_scheduled_sends).post(schedule_send),
        )
        .route("/client/scheduled/{id}", delete(cancel_scheduled_send))
        .route("/client/scheduled/{id}/now", post(send_scheduled_now))
        .layer(axum::middleware::map_response(no_store_layer))
}

async fn no_store_layer(mut resp: Response) -> Response {
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    resp
}

/// The `Message-ID` a scheduled send goes out with, minted NOW: the only thing
/// that answers "did it go out?" after a crash mid-send (the worker's
/// recovery searches Gmail for it before any retry). 24 bytes of OS entropy as
/// hex, so the id is plain `atext` and the `rfc822msgid:` search needs no
/// quoting.
fn mint_message_id() -> Result<String, ApiError> {
    let token = squelch_core::tracking::mint_token()
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error"))?;
    let bytes = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error"))?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("<{hex}@passband>"))
}

// --- POST /client/scheduled --------------------------------------------------

/// A send body plus when to send it. `send_at` is optional in the TYPE only so
/// its absence is this door's own 400 rather than the extractor's 422.
#[derive(Debug, Deserialize)]
pub struct ScheduleBody {
    #[serde(flatten)]
    send: SendBody,
    #[serde(default)]
    send_at: Option<String>,
}

/// `POST /client/scheduled` — queue a finished mail for `send_at`.
/// `201 {"id", "send_at"}`. Refusals are the send route's own, in the send
/// route's order (400, then 422, then 403, then 404), audited under
/// `schedule` with the same details.
pub async fn schedule_send(
    State(state): State<ApiState>,
    Json(req): Json<ScheduleBody>,
) -> Result<Response, ApiError> {
    schedule(&state, req).await
}

async fn schedule(state: &ApiState, req: ScheduleBody) -> Result<Response, ApiError> {
    let ScheduleBody {
        send: mut body,
        send_at,
    } = req;
    let target = send_target(&body);

    // 1. The send route's own ordered refusals.
    if let Err(f) = validate_send(&body) {
        audit_action(state, AUDIT_SCHEDULE, target, f.reason).await;
        return Err(f.into());
    }

    // 2. When. A PAST DATE IS A CLIENT BUG, not a mail to send at once: the
    // palette re-pins a pick that has gone by, so a past `send_at` is a
    // timezone mistake, and firing it on the next tick would send something
    // the user believed was hours away. Same rule as reminders.
    let now = Utc::now();
    let send_at = match parse_send_at(send_at.as_deref(), now) {
        Ok(at) => at,
        Err(e) => {
            audit_action(state, AUDIT_SCHEDULE, target, "rejected:send_at").await;
            return Err(e);
        }
    };

    // 3. An `individual` group fans out one message per member, and one
    // minted Message-ID cannot name N messages, so the crash recovery that
    // makes a scheduled send exactly-once has nothing to search for. Refused
    // for v1. `to`/`bcc` groups are one message the composer already
    // addressed; `group_id` stays in the request for attribution at fire time
    // (and only for that: the fire never re-reads the group's mode).
    if let Some(group_id) = body.group_id {
        match group_send::resolve(state, group_id).await {
            Ok(audience) if audience.mode == GroupMode::Individual => {
                audit_action(state, AUDIT_SCHEDULE, target, "rejected:group_fanout").await;
                return Err(ApiError::bad_request(
                    "a one-message-per-person group cannot be scheduled yet; send it now",
                ));
            }
            Ok(_) => {}
            Err(e) => {
                audit_action(state, AUDIT_SCHEDULE, target, "failed:group").await;
                return Err(e);
            }
        }
    }

    // 4. The files, exactly as a send reads them: every id resolves (from the
    // draft being consumed, or unclaimed), 25 MB in total. THIS is the check
    // a request naming a missing file meets; the store's all-or-nothing move
    // in step 8 is the belt for a file that vanishes in between.
    let attachments = load_send_attachments(
        state,
        AUDIT_SCHEDULE,
        &body.attachment_ids,
        body.draft_id,
        target.clone(),
    )
    .await?;

    // 5. THE GUARD RUNS NOW, once, over everything the mail will carry. A
    // forward's original is read here on the write token (which is why a
    // forward needs the credential before the verdict, and a plain send asks
    // for it after, as the send route does).
    let mut kinds = composition_kinds(&body.body, &attachments);
    let mut have_client = false;
    if let Some(original_id) = body.forward_of_message_id {
        let client = require_write_client(state, target.clone()).await?;
        have_client = true;
        let staged: usize = attachments.iter().map(|a| a.data.len()).sum();
        let (_, original) = read_forward_original(
            state,
            &client,
            AUDIT_SCHEDULE,
            target.clone(),
            original_id,
            staged,
            None,
        )
        .await?;
        for kind in forward_original_kinds(&original) {
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
    }
    // The bytes were only needed for the scan; the store keeps them.
    drop(attachments);
    if let Some(err) = guard_verdict(
        state,
        AUDIT_SCHEDULE,
        &kinds,
        body.override_guard,
        target.clone(),
    )
    .await
    {
        return Err(err);
    }

    // 6. No point queueing a mail that can never go out.
    if !have_client {
        require_write_client(state, target.clone()).await?;
    }

    // 7. A reply's parent, where a send meets it: after the files, the guard
    // and the credential, so a body with two problems gets the send route's
    // answer. A parent that is not stored at all (another account's, never
    // synced, deleted since) would fail at fire time three times over and
    // land as "didn't send" hours later; asked now, it is the same 404 a send
    // gets. UNFILTERED like the send's own lookup: a sealed parent resolves,
    // exactly as it does for a reply sent now.
    if let Some(parent) = body.reply_to_message_id
        && let Err(e) = resolve_target(state, parent).await
    {
        audit_action(state, AUDIT_SCHEDULE, target, "failed:target").await;
        return Err(e);
    }

    // 8. Mint the Message-ID and freeze the request: `confirm` forced true
    // (the user confirmed by scheduling) and `draft_id` stripped (the draft is
    // consumed HERE, and a fired send must not delete whatever draft reuses
    // that id later). `override_guard` persists: the fire path honours it by
    // not asking again.
    let message_id_header = mint_message_id()?;
    let draft_id = body.draft_id.take();
    body.confirm = true;
    let request = serde_json::to_string(&body)
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error"))?;
    let ids = dedup_ids(&body.attachment_ids);

    // 9. ONE transaction: the cap, the row, the files moved onto it, the
    // draft gone.
    let stored = store_call(state, move |store, account_id| {
        store.schedule_send(
            account_id,
            &request,
            send_at,
            &message_id_header,
            &ids,
            draft_id,
            now,
        )
    })
    .await;
    let row = match stored {
        Ok(row) => row,
        Err(e) if e.status == StatusCode::BAD_REQUEST && e.message == ATTACHMENT_MISSING => {
            // A file vanished (swept, deleted, claimed elsewhere) between the
            // read above and the move: the same refusal a send gives.
            audit_action(state, AUDIT_SCHEDULE, target, "rejected:attachment_missing").await;
            return Err(ApiError::bad_request(
                "an attached file is no longer staged; remove it and attach it again",
            ));
        }
        Err(e) if e.status == StatusCode::BAD_REQUEST && e.message == TOO_MANY_SCHEDULED => {
            audit_action(state, AUDIT_SCHEDULE, target, "rejected:too_many_scheduled").await;
            return Err(ApiError::bad_request(
                "too many mails are already scheduled; send or cancel some first",
            ));
        }
        Err(e) => {
            audit_action(state, AUDIT_SCHEDULE, target, "failed:store").await;
            return Err(e);
        }
    };

    // 10. Ids only. `send_at` through serde, as the listing spells it (`Z`).
    audit_action(state, AUDIT_SCHEDULE, target, &format!("ok:{}", row.id)).await;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": row.id, "send_at": row.send_at })),
    )
        .into_response())
}

/// `send_at` as UTC: present, RFC3339, in the future, and at most
/// [`MAX_SCHEDULE_AHEAD`] out. Any offset is accepted: the client owns the
/// user's timezone, the store does not.
fn parse_send_at(raw: Option<&str>, now: DateTime<Utc>) -> Result<DateTime<Utc>, ApiError> {
    let raw = raw.ok_or_else(|| ApiError::bad_request("send_at is required"))?;
    let at = DateTime::parse_from_rfc3339(raw)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|_| ApiError::bad_request("send_at must be an RFC3339 timestamp"))?;
    if at <= now {
        return Err(ApiError::bad_request("send_at must be in the future"));
    }
    if at > now + MAX_SCHEDULE_AHEAD {
        return Err(ApiError::bad_request(
            "send_at must be at most a year from now",
        ));
    }
    Ok(at)
}

/// The write client, or the send route's 403, audited under `schedule`.
async fn require_write_client(
    state: &ApiState,
    target: Option<String>,
) -> Result<GmailWriteClient, ApiError> {
    match write_client(state) {
        Ok(c) => Ok(c),
        Err(e) => {
            audit_action(
                state,
                AUDIT_SCHEDULE,
                target,
                "rejected:no_write_credential",
            )
            .await;
            Err(e)
        }
    }
}

// --- GET /client/scheduled ---------------------------------------------------

/// One outbox row on the wire. Addressing fields are read out of the frozen
/// request; spelled the way the send route and `DraftView` spell them.
#[derive(Debug, Serialize)]
struct ScheduledSendView {
    id: i64,
    send_at: DateTime<Utc>,
    /// `scheduled`, `sending` or `failed` (settled rows are not listed).
    state: &'static str,
    /// Claims so far. A `scheduled` row with `attempts > 0` is waiting out a
    /// retry, and `last_error` says why.
    attempts: u32,
    /// A redacted reason (`failed:gmail`, `rejected:no_write_credential`, ...)
    /// or `null`. Never content.
    last_error: Option<String>,
    /// STICKY: an earlier attempt may have reached Gmail. On a `failed` row
    /// the client says "may have sent, check Sent" whatever `last_error`
    /// reads, and a send-now searches before it sends.
    maybe_sent: bool,
    /// `""` when the request had none (a reply's audience derives at fire
    /// time), so a client need not branch on absence, as with drafts.
    to: String,
    /// `""` when none was typed: a reply's `Re:` derives at fire time, and
    /// `parent_subject` is what the row can show meanwhile.
    subject: String,
    /// The typed words, for the row's snippet line (the picker's `DraftRow`
    /// shows the same). The user's own composition on their own door, like a
    /// draft's body.
    body: String,
    reply_to_message_id: Option<i64>,
    forward_of_message_id: Option<i64>,
    /// The files the row holds, upload order. Always present, `[]` when none.
    attachments: Vec<OutboundAttachmentView>,
    /// Picker context from the replied-to or forwarded message, exactly as a
    /// draft carries it. Always present as keys, `null` for new mail.
    thread_id: Option<String>,
    parent_subject: Option<String>,
    parent_from: Option<String>,
    parent_from_addr: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl ScheduledSendView {
    fn new(row: ScheduledSend, parent: ParentContext) -> Self {
        // A request that no longer parses (the worker fails it as
        // `failed:request`) still lists, with its addressing blank, so the
        // user can see it and cancel it.
        let req = serde_json::from_str::<SendBody>(&row.request).ok();
        let parent_from = parent
            .from_name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .or_else(|| parent.from_addr.clone());
        Self {
            id: row.id,
            send_at: row.send_at,
            state: row.state.as_str(),
            attempts: row.attempts,
            last_error: row.last_error,
            maybe_sent: row.maybe_sent,
            to: req.as_ref().and_then(|r| r.to.clone()).unwrap_or_default(),
            subject: req
                .as_ref()
                .and_then(|r| r.subject.clone())
                .unwrap_or_default(),
            body: req.as_ref().map(|r| r.body.clone()).unwrap_or_default(),
            reply_to_message_id: req.as_ref().and_then(|r| r.reply_to_message_id),
            forward_of_message_id: req.as_ref().and_then(|r| r.forward_of_message_id),
            attachments: row
                .attachments
                .into_iter()
                .map(OutboundAttachmentView::from)
                .collect(),
            thread_id: parent.thread_id,
            parent_subject: parent.subject,
            parent_from,
            parent_from_addr: parent.from_addr,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// `GET /client/scheduled` — the outbox: rows still owed an outcome or the
/// user's attention (`scheduled`, `sending`, `failed`), oldest `send_at`
/// first. No audit rows: a listing reveals nothing the user did not write.
pub async fn list_scheduled_sends(State(state): State<ApiState>) -> Result<Response, ApiError> {
    let rows = store_call(&state, |store, account_id| {
        store.list_scheduled_sends_with_parent(account_id)
    })
    .await?;
    let items: Vec<ScheduledSendView> = rows
        .into_iter()
        .map(|(row, parent)| ScheduledSendView::new(row, parent))
        .collect();
    Ok(Json(items).into_response())
}

// --- DELETE /client/scheduled/{id} -------------------------------------------

/// `DELETE /client/scheduled/{id}` — pull a mail back out of the outbox.
///
/// `200 {"status": "cancelled", "id", "request", "attachments"}`: `request` is
/// the frozen send body as a JSON object (the composer reopens from it), and
/// `attachments` the files it held, now unclaimed, so the tray can be redrawn
/// and the composer's next autosave re-claims them. No draft is created here.
///
/// `request` comes back WITHOUT `override_guard` and `confirm`: both were
/// answers the user gave for the mail as it stood, and a reopened composer is
/// a new question. The client sets `confirm` itself, and a reschedule is
/// judged by the guard again.
///
/// `409` for a row already in Gmail's hands; `404` for an unknown id, another
/// account's, or one already sent or cancelled. Audited, ids only.
pub async fn cancel_scheduled_send(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    let outcome = store_call(&state, move |store, account_id| {
        store.cancel_scheduled_send(account_id, id, Utc::now())
    })
    .await?;
    let target = Some(id.to_string());
    match outcome {
        CancelOutcome::Cancelled(row) => {
            audit_action(&state, AUDIT_CANCEL, target, "ok").await;
            let mut request = serde_json::from_str::<serde_json::Value>(&row.request)
                .unwrap_or(serde_json::Value::Null);
            if let Some(obj) = request.as_object_mut() {
                obj.remove("override_guard");
                obj.remove("confirm");
            }
            let attachments: Vec<OutboundAttachmentView> = row
                .attachments
                .into_iter()
                .map(OutboundAttachmentView::from)
                .collect();
            Ok(Json(json!({
                "status": "cancelled",
                "id": id,
                "request": request,
                "attachments": attachments,
            }))
            .into_response())
        }
        CancelOutcome::Sending => {
            audit_action(&state, AUDIT_CANCEL, target, "rejected:sending").await;
            Err(in_gmails_hands())
        }
        CancelOutcome::NotFound => {
            audit_action(&state, AUDIT_CANCEL, target, "failed:not_found").await;
            Err(ApiError::not_found())
        }
    }
}

/// The `409` for a row the worker holds: cancel and send-now both answer it.
fn in_gmails_hands() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "this mail is already being sent and can no longer be cancelled",
    )
}

// --- POST /client/scheduled/{id}/now -----------------------------------------

/// `POST /client/scheduled/{id}/now` — send a `scheduled` or `failed` row on
/// the next tick. `202 {"status": "queued", "id"}`: queued, not sent; the
/// listing shows it go. A `failed` row gets a fresh retry budget (the store's
/// rule). `409` for a `sending` row (the same answer a cancel gets: it is
/// already going), `404` for anything else. Audited, ids only.
pub async fn send_scheduled_now(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    let queued = store_call(&state, move |store, account_id| {
        store.send_scheduled_now(account_id, id, Utc::now())
    })
    .await?;
    let target = Some(id.to_string());
    if !queued {
        // Why not: read the state, as a cancel does. A row that moved to
        // `sending` in between answers 409 either way, which is the truth.
        let state_now = store_call(&state, move |store, account_id| {
            store.scheduled_send_by_id(account_id, id)
        })
        .await?
        .map(|r| r.state);
        if state_now == Some(ScheduledSendState::Sending) {
            audit_action(&state, AUDIT_NOW, target, "rejected:sending").await;
            return Err(in_gmails_hands());
        }
        audit_action(&state, AUDIT_NOW, target, "failed:not_found").await;
        return Err(ApiError::not_found());
    }
    audit_action(&state, AUDIT_NOW, target, "ok").await;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "status": "queued", "id": id })),
    )
        .into_response())
}

// --- the worker ---------------------------------------------------------------

/// What one [`run_scheduled_send_pass`] did, as counts (ids go to the log).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScheduledPassReport {
    /// Rows Gmail already had (a lapsed lease, or a re-fire of a row whose
    /// earlier attempt may have gone): settled `sent`, not sent again.
    pub recovered_sent: usize,
    /// Lapsed `sending` rows Gmail did not have: back to `scheduled`, to be
    /// asked about once more before they fire.
    pub requeued: usize,
    pub sent: usize,
    /// Failed attempts put back with a backoff.
    pub retried: usize,
    /// "Not now" answers that spent no attempt: Gmail pacing the mailbox, or a
    /// search that could not be made (before a re-fire, or in recovery).
    pub deferred: usize,
    /// Rows that went terminal (`failed`) this pass.
    pub failed: usize,
    /// Rows past their keep (settled a week, failed a month), dropped.
    pub swept: usize,
}

impl ScheduledPassReport {
    /// Whether the pass moved anything worth a log line.
    pub fn is_idle(&self) -> bool {
        *self == Self::default()
    }
}

/// Whether the daemon has asked to stop: the pass finishes the send in hand
/// and claims nothing more.
fn stopping(state: &ApiState) -> bool {
    state.shutdown.as_ref().is_some_and(|rx| *rx.borrow())
}

/// One pass of the outbox (docs/SCHEDULED-SEND.md §4): RECOVER lapsed leases,
/// CLAIM and send due rows (at most [`MAX_PER_PASS`]), SWEEP old ones. The
/// daemon's timer calls it every tick; the tick is the clock, so a restart
/// costs punctuality and never a mail.
///
/// Exported for the daemon because it needs the WRITE credential, which the
/// readonly-bound sync loop must never touch. Never fails: every error is
/// logged (ids only) and left for the next pass, which is what makes firing
/// it unconditionally safe. HONOURS THE SHUTDOWN WATCH the state carries
/// ([`ApiState::with_shutdown`]) between rows: a send in hand settles, and
/// nothing new is claimed.
///
/// EXACTLY ONCE: a row is sent only from `sending` under a lease sized to the
/// send, a row whose lease lapsed is answered by a Message-ID search BEFORE it
/// can be claimed again, and a row whose earlier attempt MAY have reached
/// Gmail is searched for again before it is re-sent. A search that fails
/// sends nothing: "could not ask" is never "did not go out".
pub async fn run_scheduled_send_pass(state: &ApiState) -> ScheduledPassReport {
    let mut report = ScheduledPassReport::default();
    recover(state, &mut report).await;

    let mut claims = 0;
    while claims < MAX_PER_PASS && !stopping(state) {
        let now = clock(state);
        let claimed = store_call(state, move |store, account_id| {
            store.claim_due_scheduled_send_with(account_id, now, lease_for)
        })
        .await;
        match claimed {
            Ok(Some(row)) => {
                claims += 1;
                fire(state, row, &mut report).await;
            }
            Ok(None) => break,
            Err(_) => {
                eprintln!("squelch-api: scheduled send claim failed; retrying next pass");
                break;
            }
        }
    }

    let now = clock(state);
    match store_call(state, move |store, account_id| {
        store.sweep_scheduled_sends(account_id, now)
    })
    .await
    {
        Ok(n) => report.swept = n,
        Err(_) => eprintln!("squelch-api: scheduled send sweep failed"),
    }
    report
}

/// The claim lease for one row: [`SCHEDULED_LEASE_BASE`] plus the send's own
/// Gmail budget for the bytes it will post. The estimate is the files as
/// base64 on the wire ([`BASE64_WIRE_PERCENT`]), the frozen request (the typed words, and then
/// some), and 4 KB of headers; a FORWARD adds the largest original it may
/// carry, since the original's size is not known until it is fetched. Too
/// long only delays a crash's recovery; too short hands a mail still on the
/// wire to it.
pub(crate) fn lease_for(row: &ScheduledSend) -> chrono::Duration {
    let files: u64 = row
        .attachments
        .iter()
        .map(|a| a.size_bytes.max(0) as u64)
        .sum();
    let mut estimate = files * BASE64_WIRE_PERCENT / 100 + row.request.len() as u64 + 4096;
    let is_forward = serde_json::from_str::<SendBody>(&row.request)
        .map(|b| b.forward_of_message_id.is_some())
        .unwrap_or(false);
    if is_forward {
        estimate += MAX_FORWARD_RAW_BYTES as u64 * BASE64_WIRE_PERCENT / 100;
    }
    let budget = chrono::Duration::from_std(send_budget(estimate as usize))
        .unwrap_or(chrono::Duration::hours(1));
    SCHEDULED_LEASE_BASE + budget
}

/// Step 1: every `sending` row whose lease lapsed at least [`RECOVERY_GRACE`]
/// ago (a worker that died mid-send) is asked of Gmail by its minted
/// Message-ID. At most [`MAX_PER_PASS`] rows a pass, like the claims: a
/// backlog of lapsed rows (a long outage) is asked about over ticks, not in
/// one burst of searches a shutdown has to wait out.
async fn recover(state: &ApiState, report: &mut ScheduledPassReport) {
    let now = clock(state);
    let expired = match store_call(state, move |store, account_id| {
        store.expired_sending_scheduled_sends(account_id, now - RECOVERY_GRACE, now)
    })
    .await
    {
        Ok(mut rows) => {
            rows.truncate(MAX_PER_PASS);
            rows
        }
        Err(_) => {
            eprintln!("squelch-api: scheduled send recovery read failed");
            return;
        }
    };
    if expired.is_empty() {
        return;
    }
    let Ok(client) = write_client(state) else {
        // Nothing can be asked, so nothing is decided: the rows stay
        // `sending`, and the listing says why.
        eprintln!(
            "squelch-api: {} scheduled send(s) await recovery; no write credential",
            expired.len()
        );
        for row in &expired {
            cannot_recover(state, row, "rejected:no_write_credential", report).await;
        }
        return;
    };
    for row in expired {
        if stopping(state) {
            return;
        }
        match client.find_by_rfc822_msgid(&row.message_id_header).await {
            Ok(Some(gmail_id)) => {
                if settle_recovered(state, &client, &row, gmail_id).await {
                    report.recovered_sent += 1;
                }
            }
            // The lapsed attempt was the last one it had: a row that keeps
            // dying mid-send (a crash the send itself triggers) stops here
            // rather than looping forever.
            Ok(None) if row.attempts >= SCHEDULED_MAX_ATTEMPTS => {
                eprintln!(
                    "squelch-api: scheduled send {} died mid-send {} times; failed",
                    row.id, row.attempts
                );
                // "Not found" is one look at an index that may trail: the
                // row stays `maybe_sent`, so send-now searches first.
                // Its attempt ended when its lease lapsed, not now.
                fail_unsent(
                    state,
                    &row,
                    "failed:crash_loop",
                    true,
                    row.lease_until,
                    report,
                )
                .await;
            }
            Ok(None) => {
                // NOT SENT AGAIN IN THIS PASS: the requeue lands a grace out,
                // and the re-fire searches once more before it sends (the
                // row is now `maybe_sent`). A mail the index had not caught
                // up with gets a second look.
                let (id, attempts) = (row.id, row.attempts);
                let now = clock(state);
                let at = now + RECOVERY_GRACE;
                match store_call(state, move |store, account_id| {
                    store.requeue_scheduled_send(account_id, id, attempts, at, now)
                })
                .await
                {
                    Ok(true) => {
                        eprintln!("squelch-api: scheduled send {id} not found in Gmail; requeued");
                        report.requeued += 1;
                    }
                    Ok(false) => {}
                    Err(_) => eprintln!("squelch-api: scheduled send {id} requeue failed"),
                }
            }
            Err(_) => {
                eprintln!(
                    "squelch-api: scheduled send {} recovery search failed; still sending",
                    row.id
                );
                cannot_recover(state, &row, "recovery:unreachable", report).await;
            }
        }
    }
}

/// Recovery could not ask Gmail about a lapsed row. Within
/// [`RECOVERY_GIVE_UP`] of its lease the row stays `sending` with the reason
/// shown and is asked about again in a minute; past it, it goes `failed` with
/// `failed:recovery` so the user can cancel it or send it now (which asks
/// Gmail first: the row MAY have been sent).
async fn cannot_recover(
    state: &ApiState,
    row: &ScheduledSend,
    reason: &'static str,
    report: &mut ScheduledPassReport,
) {
    let now = clock(state);
    let lapsed_for = row.lease_until.map(|l| now - l).unwrap_or_default();
    if lapsed_for >= RECOVERY_GIVE_UP {
        eprintln!(
            "squelch-api: scheduled send {} could not be recovered ({reason}); failed",
            row.id
        );
        // Its attempt ended when its lease lapsed, half an hour ago: a
        // send-now straight after this owes no fresh grace.
        fail_unsent(state, row, "failed:recovery", true, row.lease_until, report).await;
        return;
    }
    let (id, attempts) = (row.id, row.attempts);
    match store_call(state, move |store, account_id| {
        store.defer_scheduled_recovery(account_id, id, attempts, now + RECOVERY_RETRY, reason, now)
    })
    .await
    {
        Ok(true) => report.deferred += 1,
        Ok(false) => {}
        Err(_) => eprintln!("squelch-api: scheduled send {id} recovery deferral not recorded"),
    }
}

/// A row Gmail already has: echo it (best effort, so the Sent list and the row
/// both point at the local copy), drop its files, settle it `sent` under the
/// row's claim. NO SECOND SEND. `true` when the row moved.
async fn settle_recovered(
    state: &ApiState,
    client: &GmailWriteClient,
    row: &ScheduledSend,
    gmail_id: String,
) -> bool {
    let target = serde_json::from_str::<SendBody>(&row.request)
        .ok()
        .and_then(|b| send_target(&b));
    let sent = SentRef {
        id: Some(gmail_id),
        thread_id: None,
    };
    let fired = Fire {
        id: row.id,
        message_id: &row.message_id_header,
    };
    let echo_message_id = echo_sent(state, client, target.clone(), &sent, Some(fired)).await;
    let thread_id = match echo_message_id {
        Some(local) => resolve_target(state, local).await.ok().map(|m| m.thread_id),
        None => None,
    };
    let (id, attempts) = (row.id, row.attempts);
    let ids: Vec<i64> = row.attachments.iter().map(|a| a.id).collect();
    discard_scheduled_files(state, id, ids).await;
    let now = clock(state);
    let settled = store_call(state, move |store, account_id| {
        store.settle_scheduled_sent(
            account_id,
            id,
            attempts,
            echo_message_id,
            thread_id.as_deref(),
            now,
        )
    })
    .await;
    match settled {
        Ok(true) => {
            audit_action(state, "send", target, &format!("scheduled:{id}:recovered")).await;
            eprintln!("squelch-api: scheduled send {id} was already sent; recovered");
            true
        }
        Ok(false) => false,
        Err(_) => {
            eprintln!("squelch-api: scheduled send {id} recovered but not settled");
            false
        }
    }
}

/// Steps 3-4 for one claimed row: ask Gmail first if an earlier attempt may
/// have gone, send it, then settle.
async fn fire(state: &ApiState, row: ScheduledSend, report: &mut ScheduledPassReport) {
    let id = row.id;
    let Ok(body) = serde_json::from_str::<SendBody>(&row.request) else {
        // Nothing to retry: the same bytes parse the same way next time.
        eprintln!("squelch-api: scheduled send {id} has an unreadable request; failed");
        fail_unsent(state, &row, "failed:request", false, None, report).await;
        return;
    };

    // THE PRE-FIRE SEARCH. An earlier attempt of this row may be in Gmail (it
    // failed after the post, or its lease lapsed and recovery did not find it
    // YET): ask by Message-ID before sending again. Found: settled, no send.
    // Could not ask: nothing is sent.
    if row.maybe_sent {
        // THE GRACE HOLDS AT FIRE, whoever moved `send_at`: a send-now on a
        // row whose ambiguous attempt ended a moment ago would ask an index
        // that has not caught up yet, read "not found", and send it twice.
        // Not an attempt: the claim is handed back until the grace is out.
        let now = clock(state);
        if let Some(ended) = row.last_attempt_at
            && now < ended + RECOVERY_GRACE
        {
            not_now(
                state,
                &row,
                "recovery:grace",
                ended + RECOVERY_GRACE,
                SearchClock::Keep,
                report,
            )
            .await;
            return;
        }
        let client = match write_client(state) {
            Ok(c) => c,
            Err(_) => {
                // Nothing could be sent without it either: the ordinary
                // retryable failure a fire without a credential is.
                audit_fire(state, &row, "rejected:no_write_credential").await;
                settle_retry(state, &row, "rejected:no_write_credential", false, report).await;
                return;
            }
        };
        match client.find_by_rfc822_msgid(&row.message_id_header).await {
            Ok(Some(gmail_id)) => {
                if settle_recovered(state, &client, &row, gmail_id).await {
                    report.recovered_sent += 1;
                }
                return;
            }
            Ok(None) => {}
            Err(e) => {
                search_failed(state, &row, &e, report).await;
                return;
            }
        }
    }

    let ids = dedup_ids(&body.attachment_ids);
    let attachments = if ids.is_empty() {
        Vec::new()
    } else {
        let wanted = ids.clone();
        match store_call(state, move |store, account_id| {
            store.scheduled_outbound_attachments(account_id, id, &wanted)
        })
        .await
        {
            Ok(rows) if rows.len() == ids.len() => rows.into_iter().map(mail_attachment).collect(),
            // The row's own files are claimed by it and never swept or
            // deleted while it lives, so a missing one is not going to come
            // back: terminal, rather than three sends refused the same way.
            Ok(_) => {
                eprintln!("squelch-api: scheduled send {id} lost an attached file; failed");
                fail_unsent(
                    state,
                    &row,
                    "failed:attachment_missing",
                    false,
                    None,
                    report,
                )
                .await;
                return;
            }
            Err(_) => {
                audit_fire(state, &row, "failed:store").await;
                settle_retry(state, &row, "failed:store", false, report).await;
                return;
            }
        }
    };

    let fired = Fire {
        id,
        message_id: &row.message_id_header,
    };
    match perform_send(state, &body, attachments, SendGuard::Settled, Some(fired)).await {
        Ok(SendOutcome::Sent {
            echo_message_id,
            thread_id,
        }) => {
            // `finish_send` consumed only unclaimed files (it knows no
            // scheduled id); this row's own go here.
            discard_scheduled_files(state, id, ids).await;
            let attempts = row.attempts;
            let now = clock(state);
            match store_call(state, move |store, account_id| {
                store.settle_scheduled_sent(
                    account_id,
                    id,
                    attempts,
                    echo_message_id,
                    thread_id.as_deref(),
                    now,
                )
            })
            .await
            {
                Ok(true) => report.sent += 1,
                Ok(false) => eprintln!("squelch-api: scheduled send {id} sent; row had moved"),
                Err(_) => eprintln!("squelch-api: scheduled send {id} sent but not settled"),
            }
        }
        // UNREACHABLE: a fired send's group is attribution only, so
        // `perform_send` never fans it out. Should that ever change, the row
        // must not read as one mail sent: terminal, for a human to look at.
        Ok(SendOutcome::Sending { .. }) => {
            eprintln!("squelch-api: scheduled send {id} fanned out; failed");
            // Mail DID go (one per member): never re-sent blind.
            fail_unsent(state, &row, "failed:group_fanout", true, None, report).await;
        }
        // Gmail's budget held a call on the way (the send, a forward's
        // original, a reply's parent) and nothing was posted: not this mail's
        // failure, so no attempt is spent. `perform_send` audited it `paced`.
        Err(failure) if failure.paced => {
            let next = clock(state) + NOT_NOW_RETRY;
            not_now(
                state,
                &row,
                failure.reason,
                next,
                SearchClock::Reset,
                report,
            )
            .await;
        }
        // `perform_send` already wrote the refusal to the ledger.
        Err(failure) => settle_retry(state, &row, failure.reason, failure.maybe_sent, report).await,
    }
}

/// A pre-fire search that could not be made. Two kinds of "could not":
///
/// - REFUSED (no credential, which is where a revoked write grant first
///   surfaces since the client fetches no token until it calls; or a 4xx
///   other than 403 and 429): asking again in two minutes gets the same
///   answer, so it is a failed attempt like any other: spent, backed off,
///   `failed` after the third.
/// - UNREACHABLE (transport, a 5xx, a 429, a 403, the budget pacing): "not
///   now",
///   no attempt spent, but BOUNDED: [`RECOVERY_GIVE_UP`] after the first
///   such answer the row goes `failed:recovery` for the user instead of
///   deferring (and refreshing a token) forever.
///
/// A 403 is UNREACHABLE because Gmail answers a per-user rate limit
/// (`userRateLimitExceeded`, `rateLimitExceeded`) with a 403 as often as a
/// permission refusal, and the write client does not surface the body that
/// tells them apart (`is_quota_refusal` reads it for the budget, then it is
/// dropped). A genuine permission 403 costs at most the give-up's wait
/// before the user sees it, and a quota 403 never spends an attempt.
///
/// Either way nothing was sent, and the row stays `maybe_sent`.
async fn search_failed(
    state: &ApiState,
    row: &ScheduledSend,
    e: &WriteError,
    report: &mut ScheduledPassReport,
) {
    let id = row.id;
    let refused = match e {
        WriteError::MissingCredential(_) => Some("rejected:no_write_credential"),
        WriteError::Api { status, .. }
            if (400..500).contains(status) && !matches!(*status, 403 | 429) =>
        {
            Some("failed:search")
        }
        WriteError::Invalid(_) => Some("failed:search"),
        WriteError::Api { .. } | WriteError::Transport(_) | WriteError::Paced { .. } => None,
    };
    if let Some(reason) = refused {
        eprintln!("squelch-api: scheduled send {id} could not be searched for ({reason})");
        audit_fire(state, row, reason).await;
        settle_retry(state, row, reason, true, report).await;
        return;
    }
    let now = clock(state);
    if row
        .search_deferred_since
        .is_some_and(|since| now - since >= RECOVERY_GIVE_UP)
    {
        eprintln!("squelch-api: scheduled send {id} could never be searched for; failed");
        // This claim sent nothing: the last attempt's end stays what it was.
        fail_unsent(
            state,
            row,
            "failed:recovery",
            true,
            row.last_attempt_at,
            report,
        )
        .await;
        return;
    }
    eprintln!("squelch-api: scheduled send {id} could not be searched for; deferred");
    not_now(
        state,
        row,
        "recovery:unreachable",
        now + NOT_NOW_RETRY,
        SearchClock::Start,
        report,
    )
    .await;
}

/// The ledger row for a fire outcome the WORKER decided, without
/// `perform_send` (which audits its own): `send`, `scheduled:<id>:<reason>`,
/// the same shape the send's own rows carry. Ids and reasons only.
async fn audit_fire(state: &ApiState, row: &ScheduledSend, reason: &str) {
    let target = serde_json::from_str::<SendBody>(&row.request)
        .ok()
        .and_then(|b| send_target(&b));
    audit_action(
        state,
        "send",
        target,
        &format!("scheduled:{}:{reason}", row.id),
    )
    .await;
}

/// A terminal outcome only the worker knows of (an unreadable request, a lost
/// file, a crash loop, a recovery never asked): `failed`, then audited, and
/// only when the settle moved the row (a worker that outlived its claim
/// decided nothing, so the ledger must not say it did). `ended` is the last
/// attempt's end for `last_attempt_at`, `None` = now.
async fn fail_unsent(
    state: &ApiState,
    row: &ScheduledSend,
    reason: &'static str,
    maybe_sent: bool,
    ended: Option<DateTime<Utc>>,
    report: &mut ScheduledPassReport,
) {
    if settle_failed(state, row, reason, maybe_sent, ended, report).await {
        audit_fire(state, row, reason).await;
    }
}

/// A failed attempt: back to `scheduled` after the backoff while attempts
/// remain, else `failed` for the user. A failure that MAY HAVE SENT the mail
/// marks the row so the re-fire searches first, and waits at least
/// [`RECOVERY_GRACE`] so the search can see it. THE MARK SURVIVES THE LAST
/// ATTEMPT TOO: a row that goes `failed` on an ambiguous post is searched for
/// before a send-now sends it again. The caller has audited the attempt.
async fn settle_retry(
    state: &ApiState,
    row: &ScheduledSend,
    reason: &'static str,
    maybe_sent: bool,
    report: &mut ScheduledPassReport,
) {
    if row.attempts >= SCHEDULED_MAX_ATTEMPTS {
        eprintln!(
            "squelch-api: scheduled send {} failed ({reason}) on attempt {}; giving up",
            row.id, row.attempts
        );
        settle_failed(state, row, reason, maybe_sent, None, report).await;
        return;
    }
    let (id, attempts) = (row.id, row.attempts);
    let mut wait = retry_backoff(attempts);
    if maybe_sent {
        wait = wait.max(RECOVERY_GRACE);
    }
    let now = clock(state);
    let next = now + wait;
    match store_call(state, move |store, account_id| {
        store.settle_scheduled_retry(account_id, id, attempts, next, reason, maybe_sent, now)
    })
    .await
    {
        Ok(true) => {
            eprintln!("squelch-api: scheduled send {id} failed ({reason}); retrying at {next}");
            report.retried += 1;
        }
        Ok(false) => {}
        Err(_) => eprintln!("squelch-api: scheduled send {id} retry not recorded"),
    }
}

/// "Not now": the claim is handed back without spending an attempt, and the
/// row comes round again at `next`. `search` moves the pre-fire search's
/// give-up clock (see [`search_failed`]).
async fn not_now(
    state: &ApiState,
    row: &ScheduledSend,
    reason: &'static str,
    next: DateTime<Utc>,
    search: SearchClock,
    report: &mut ScheduledPassReport,
) {
    let (id, attempts) = (row.id, row.attempts);
    let now = clock(state);
    match store_call(state, move |store, account_id| {
        store.unclaim_scheduled_send(account_id, id, attempts, next, reason, search, now)
    })
    .await
    {
        Ok(true) => report.deferred += 1,
        Ok(false) => {}
        Err(_) => eprintln!("squelch-api: scheduled send {id} deferral not recorded"),
    }
}

/// `failed` under the row's claim, OR-ing `maybe_sent` into its sticky flag.
/// `ended` stamps `last_attempt_at` (`None` = now). `true` when the row moved.
async fn settle_failed(
    state: &ApiState,
    row: &ScheduledSend,
    reason: &'static str,
    maybe_sent: bool,
    ended: Option<DateTime<Utc>>,
    report: &mut ScheduledPassReport,
) -> bool {
    let (id, attempts) = (row.id, row.attempts);
    let now = clock(state);
    match store_call(state, move |store, account_id| {
        store.settle_scheduled_failed(account_id, id, attempts, reason, maybe_sent, ended, now)
    })
    .await
    {
        Ok(true) => {
            report.failed += 1;
            true
        }
        Ok(false) => false,
        Err(_) => {
            eprintln!("squelch-api: scheduled send {id} failure not recorded");
            false
        }
    }
}

/// Drop the files a sent row carried. Best effort, like the send route's own
/// discard: the mail is away, and the sweep drops them with the row anyway.
async fn discard_scheduled_files(state: &ApiState, id: i64, ids: Vec<i64>) {
    if ids.is_empty() {
        return;
    }
    if store_call(state, move |store, account_id| {
        store.delete_scheduled_outbound_attachments(account_id, id, &ids)
    })
    .await
    .is_err()
    {
        eprintln!("squelch-api: scheduled send {id} files outlived it; discard failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use squelch_core::store::ScheduledSendState;

    #[test]
    fn backoff_is_two_to_the_attempts_in_minutes() {
        assert_eq!(retry_backoff(1), chrono::Duration::minutes(2));
        assert_eq!(retry_backoff(2), chrono::Duration::minutes(4));
        assert_eq!(retry_backoff(3), chrono::Duration::minutes(8));
    }

    #[test]
    fn send_at_must_be_future_and_within_a_year() {
        let now = Utc::now();
        let fmt = |d: DateTime<Utc>| d.to_rfc3339();
        assert!(parse_send_at(None, now).is_err());
        assert!(parse_send_at(Some("tomorrow"), now).is_err());
        assert!(
            parse_send_at(Some(&fmt(now)), now).is_err(),
            "now is not later"
        );
        assert!(parse_send_at(Some(&fmt(now - chrono::Duration::minutes(1))), now).is_err());
        // A YEAR IS 366 DAYS, so "the same date next year" across a leap day
        // is accepted; the exact boundary is in, a second past it is out.
        let edge = now + chrono::Duration::days(366);
        assert_eq!(parse_send_at(Some(&fmt(edge)), now).unwrap(), edge);
        assert!(parse_send_at(Some(&fmt(edge + chrono::Duration::seconds(1))), now).is_err());
        // A literal `Z` is RFC3339 too.
        let z = DateTime::parse_from_rfc3339("2029-12-31T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            parse_send_at(Some("2030-01-01T09:00:00Z"), z).unwrap(),
            z + chrono::Duration::hours(33)
        );
        let at = now + chrono::Duration::hours(3);
        assert_eq!(parse_send_at(Some(&fmt(at)), now).unwrap(), at);
        // Any offset, normalized to UTC.
        let local = "2030-01-01T09:00:00-08:00";
        let then = DateTime::parse_from_rfc3339("2029-12-31T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            parse_send_at(Some(local), then).unwrap().to_rfc3339(),
            "2030-01-01T17:00:00+00:00"
        );
    }

    #[test]
    fn a_minted_message_id_is_a_fresh_plain_atext_id() {
        let a = mint_message_id().unwrap();
        let b = mint_message_id().unwrap();
        assert_ne!(a, b);
        assert!(a.starts_with('<') && a.ends_with("@passband>"));
        let local = &a[1..a.len() - "@passband>".len()];
        assert_eq!(local.len(), 48);
        assert!(local.bytes().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_stored_request_round_trips_into_the_same_body() {
        let full: SendBody = serde_json::from_value(json!({
            "reply_to_message_id": 7,
            "to": "a@x",
            "cc": "",
            "reply_all": true,
            "bcc": "b@x",
            "subject": "s",
            "body": "hi",
            "body_format": "markdown",
            "confirm": true,
            "override_guard": true,
            "include_tracker": false,
            "attachment_ids": [3, 3, 4]
        }))
        .unwrap();
        let stored = serde_json::to_string(&full).unwrap();
        assert_eq!(serde_json::from_str::<SendBody>(&stored).unwrap(), full);
        // A stated EMPTY cc is not an absent one (it clears the derivation),
        // so it must survive the trip.
        assert!(stored.contains("\"cc\":\"\""));

        let bare: SendBody = serde_json::from_value(json!({ "body": "hi" })).unwrap();
        let stored = serde_json::to_string(&bare).unwrap();
        assert_eq!(stored, r#"{"body":"hi"}"#, "defaults are not stored");
        assert_eq!(serde_json::from_str::<SendBody>(&stored).unwrap(), bare);
    }

    #[test]
    fn the_listing_reads_a_corrupt_request_as_blank_not_as_an_error() {
        let now = Utc::now();
        let row = ScheduledSend {
            id: 1,
            account_id: 1,
            request: "not json".into(),
            send_at: now,
            state: ScheduledSendState::Failed,
            lease_until: None,
            attempts: 1,
            last_error: Some("failed:request".into()),
            message_id_header: "<x@passband>".into(),
            sent_message_id: None,
            sent_thread_id: None,
            maybe_sent: false,
            last_attempt_at: None,
            search_deferred_since: None,
            created_at: now,
            updated_at: now,
            attachments: Vec::new(),
        };
        let v = ScheduledSendView::new(row, ParentContext::default());
        assert_eq!((v.to.as_str(), v.subject.as_str()), ("", ""));
        assert_eq!(v.state, "failed");
    }

    fn row_with(request: &str, files: &[i64]) -> ScheduledSend {
        let now = Utc::now();
        ScheduledSend {
            id: 1,
            account_id: 1,
            request: request.into(),
            send_at: now,
            state: ScheduledSendState::Scheduled,
            lease_until: None,
            attempts: 0,
            last_error: None,
            message_id_header: "<x@passband>".into(),
            sent_message_id: None,
            sent_thread_id: None,
            maybe_sent: false,
            last_attempt_at: None,
            search_deferred_since: None,
            created_at: now,
            updated_at: now,
            attachments: files
                .iter()
                .enumerate()
                .map(|(i, size)| squelch_core::store::OutboundAttachmentMeta {
                    id: i as i64 + 1,
                    filename: "f".into(),
                    mime: "application/octet-stream".into(),
                    size_bytes: *size,
                    content_id: format!("c{i}"),
                })
                .collect(),
        }
    }

    #[test]
    fn the_lease_covers_the_send_budget_of_the_bytes_it_will_post() {
        let secs = |d: chrono::Duration| d.num_seconds();
        let bare = lease_for(&row_with(r#"{"to":"a@x","body":"hi"}"#, &[]));
        // Base plus the send's flat minute; a few bytes add nothing.
        assert_eq!(secs(bare), secs(SCHEDULED_LEASE_BASE) + 60);
        // 25 MB of files is ~34 MB of base64 on the wire (76-char lines, not
        // a flat 4/3): over ten minutes more at the send budget's 50 KB/s,
        // the case a flat two minutes got wrong.
        let big = lease_for(&row_with(
            r#"{"to":"a@x","body":"hi"}"#,
            &[15 * 1024 * 1024, 10 * 1024 * 1024],
        ));
        let est = 25 * 1024 * 1024 * 137 / 100 + r#"{"to":"a@x","body":"hi"}"#.len() + 4096;
        assert_eq!(
            secs(big),
            secs(SCHEDULED_LEASE_BASE) + send_budget(est).as_secs() as i64
        );
        assert!(secs(big) > 180 + 60 + 600, "{big}");
        // A forward budgets for the largest original it may carry.
        let fwd = lease_for(&row_with(
            r#"{"forward_of_message_id":7,"to":"a@x","body":""}"#,
            &[],
        ));
        assert!(fwd > bare + chrono::Duration::seconds(500), "{fwd}");
    }
}
