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
use std::sync::Arc;
use std::time::Duration;

use crate::error::ApiError;
use crate::group_send;
use crate::handlers::{
    Fire, MAX_FORWARD_RAW_BYTES, OutboundAttachmentView, SendBody, SendGuard, SendOutcome,
    audit_action, composition_kinds, dedup_ids, echo_sent, forward_original_kinds, guard_verdict,
    load_send_attachments, mail_attachment, perform_send, read_forward_original, resolve_target,
    send_target, store_call, validate_send, write_client,
};
use crate::state::ApiState;
use crate::write::gmail::SCAN_SLACK;
use crate::write::{MailWriter, SentRef, WriteError, send_budget};

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
/// by the write client's 60 s timeout: the pre-fire lookup, the parent's
/// headers (or a forward's original), the 5 s echo, and the store. A send that
/// has not settled inside its lease is presumed to have died mid-send, and
/// recovery asks Gmail by Message-ID before anything is retried.
///
/// THE PRE-FIRE LOOKUP CAN OUTRUN THIS: after a `rfc822msgid:` miss it is a
/// paged Sent scan ([`MailWriter::find_scheduled_send`]), at worst
/// 1 + 4 + 100 requests, which a lease sized for three calls does not cover.
/// That is still safe, because a lapse during the lookup can hand nobody the
/// row mid-send: the pass loop is sequential (recovery runs before the claims,
/// never beside them, and a pass does not start until the last one returned),
/// so recovery never touches a row its own worker is firing, and the lookup
/// posts nothing. What it must not do is leave the upload too little lease: a
/// post after the lease's end is outside the window a later lookup scans, so
/// a lookup that ate the lease is could-not-ask and sends nothing ([`fire`]).
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

/// Claims one pass makes before it yields to the next round, so a backlog (a
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

/// The `Message-ID` a scheduled send goes out with, minted NOW. Its 48-hex
/// local part is also the row's outbox token (`X-Passband-Outbox`, see
/// [`crate::write::mime::outbox_token`]): together they are what answers "did
/// it go out?" after a crash mid-send (the worker's recovery asks Gmail before
/// any retry). 24 bytes of OS entropy as hex, so the id is plain `atext` and
/// neither search needs quoting. The host is DOTTED (`passband.app`): a
/// dotless right-hand side is a plausible reason Gmail rewrote the id in the
/// live finding, and an id that survives is found by the cheap search and
/// skips the scan. Rows minted as `@passband` keep working: the token is the
/// local part whatever the host.
fn mint_message_id() -> Result<String, ApiError> {
    let token = squelch_core::tracking::mint_token()
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error"))?;
    let bytes = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error"))?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("<{hex}@passband.app>"))
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
    // timezone mistake, and firing it at once would send something
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
    // addressed; `group_id` stays in the request for attribution at fire time,
    // and the mode it was addressed under is FROZEN beside it (`group_mode`),
    // so the fire records the list the mail was addressed with even if the
    // group's mode changes before it goes. Whatever a client sent in
    // `group_mode` is overwritten here: only this route may stamp it.
    body.group_mode = None;
    if let Some(group_id) = body.group_id {
        match group_send::resolve(state, group_id).await {
            Ok(audience) if audience.mode != GroupMode::Individual => {
                body.group_mode = Some(audience.mode);
            }
            Ok(_) => {
                audit_action(state, AUDIT_SCHEDULE, target, "rejected:group_fanout").await;
                return Err(ApiError::bad_request(
                    "a one-message-per-person group cannot be scheduled yet; send it now",
                ));
            }
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
            &*client,
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

    // 10. The worker may be asleep until a later row: this one is sooner.
    state.scheduled_wake.notify_one();
    // 11. Ids only. `send_at` through serde, as the listing spells it (`Z`).
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
) -> Result<Arc<dyn MailWriter>, ApiError> {
    match write_client(state).await {
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
    /// The copy lists as frozen, `""` when the request had none, the same
    /// no-branching shape as `to`. `""` here does NOT say "nobody": a reply
    /// that left `cc` absent derives the parent's copy list at fire time.
    cc: String,
    bcc: String,
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
            cc: req.as_ref().and_then(|r| r.cc.clone()).unwrap_or_default(),
            bcc: req.as_ref().and_then(|r| r.bcc.clone()).unwrap_or_default(),
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

/// `POST /client/scheduled/{id}/now` — send a `scheduled` or `failed` row
/// now: the worker is woken for it. `202 {"status": "queued", "id"}`: queued,
/// not sent; the
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
    // Due now: the worker is asleep until a row that was due later.
    state.scheduled_wake.notify_one();
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
/// worker ([`scheduled_send_worker`]) runs it on its maintenance clock and
/// the claim half when a row comes due; the store is the clock, so a restart
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
    claim_and_fire_due(state, &mut report).await;
    sweep(state, &mut report).await;
    report
}

/// Step 4: drop rows past their keep, and the staged files nobody claims.
async fn sweep(state: &ApiState, report: &mut ScheduledPassReport) {
    let now = clock(state);
    match store_call(state, move |store, account_id| {
        store.sweep_scheduled_sends(account_id, now)
    })
    .await
    {
        Ok(n) => report.swept = n,
        Err(_) => eprintln!("squelch-api: scheduled send sweep failed"),
    }
}

/// The CLAIM half of a pass (contract §4 step 2): due rows, at most
/// [`MAX_PER_PASS`], each claimed under its lease and fired. The worker runs
/// this alone between its maintenance passes, when a row has come due.
/// Returns why it stopped, which is what the worker's next wait keys on.
async fn claim_and_fire_due(state: &ApiState, report: &mut ScheduledPassReport) -> ClaimOutcome {
    let mut claims = 0;
    while claims < MAX_PER_PASS {
        if stopping(state) {
            return ClaimOutcome::Stopping;
        }
        let now = clock(state);
        let claimed = store_call(state, move |store, account_id| {
            store.claim_due_scheduled_send_with(account_id, now, lease_for)
        })
        .await;
        match claimed {
            Ok(Some(row)) => {
                claims += 1;
                fire(state, row, report).await;
            }
            Ok(None) => return ClaimOutcome::Drained,
            Err(_) => {
                eprintln!("squelch-api: scheduled send claim failed; retrying next pass");
                return ClaimOutcome::Failed;
            }
        }
    }
    ClaimOutcome::Capped
}

/// Why [`claim_and_fire_due`] stopped claiming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimOutcome {
    /// The claim answered "nothing due": the outbox is drained to `now`.
    Drained,
    /// [`MAX_PER_PASS`] rows claimed; more may be due right now.
    Capped,
    /// The claim itself failed (a store error): nothing is known.
    Failed,
    /// The daemon is stopping; the worker returns on its next look.
    Stopping,
}

/// The scheduled-send worker's clocks: its first look after startup, how
/// often it runs the whole pass (recovery and the sweep) over an outbox
/// with nothing due, and the least it waits after a round whose claim
/// failed or whose due read could not be made.
#[derive(Debug, Clone, Default)]
pub struct ScheduledWorkerTiming {
    /// The first look, a few seconds in: past the bind and the first sync
    /// burst.
    pub first_look: Duration,
    /// How long after a maintenance pass ENDS the next one runs, when nothing
    /// is due in between. Measured from the end, so a recovery deferral the
    /// pass stamped ([`RECOVERY_RETRY`] out from inside it) has come by the
    /// next pass rather than a moment after it. Recovery has a five-minute
    /// grace of its own ([`RECOVERY_GRACE`]) and the sweep drops week-old
    /// rows, so a minute between passes loses nothing; a lapsed lease is
    /// recovered within a minute of its grace where the tick took ten
    /// seconds.
    pub maintenance: Duration,
    /// The least the worker waits when it cannot tell what is due (the claim
    /// failed, the due read failed): the old tick, so such a row is never
    /// later than it was and the worker never spins on a store error.
    pub floor: Duration,
    /// TEST HOOK: counted up once per round, so a test can pin how often an
    /// idle outbox is looked at. `None` in the daemon.
    pub round_counter: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
}

impl ScheduledWorkerTiming {
    /// The daemon's clocks: a first look at five seconds, maintenance every
    /// minute, a ten-second floor.
    pub fn daemon() -> Self {
        Self {
            first_look: Duration::from_secs(5),
            maintenance: Duration::from_secs(60),
            floor: Duration::from_secs(10),
            round_counter: None,
        }
    }
}

/// THE SCHEDULED-SEND WORKER (docs/SCHEDULED-SEND.md §4), owning its own
/// clock. It used to be a ten-second tick in the daemon: six wakeups a minute,
/// each a recovery read, a `BEGIN IMMEDIATE` claim and a sweep transaction,
/// against an outbox that is empty almost all of the time. Now a round runs
/// the claim half when a row is due and the whole pass
/// [`ScheduledWorkerTiming::maintenance`] after the last one ended, then
/// reads when the next row is due (`next_scheduled_send_at`, a walk of the
/// due index that stops at the first row) and sleeps until then, or until
/// maintenance, or until a handler pokes `ApiState::scheduled_wake` for a
/// row just scheduled or pulled forward. A due send fires at its time rather
/// than up to a tick late; an idle outbox costs one pass a minute.
///
/// Honours `shutdown` between rounds, as the pass does between rows: a send
/// in hand settles and nothing new is claimed.
pub async fn scheduled_send_worker(
    state: ApiState,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    timing: ScheduledWorkerTiming,
) {
    let mut wait = timing.first_look;
    let mut maintenance_ended: Option<std::time::Instant> = None;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = state.scheduled_wake.notified() => {}
            res = shutdown.changed() => {
                if res.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
        if *shutdown.borrow() {
            return;
        }
        let maintenance_due = maintenance_ended.is_none_or(|at| at.elapsed() >= timing.maintenance);
        let mut report = ScheduledPassReport::default();
        let outcome = if maintenance_due {
            recover(&state, &mut report).await;
            let outcome = claim_and_fire_due(&state, &mut report).await;
            sweep(&state, &mut report).await;
            maintenance_ended = Some(std::time::Instant::now());
            outcome
        } else {
            claim_and_fire_due(&state, &mut report).await
        };
        if let Some(counter) = &timing.round_counter {
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if !report.is_idle() {
            // Counts only; the pass logged its own ids.
            eprintln!("squelch-api: scheduled-send pass {report:?}");
        }
        let next_due = match store_call(&state, |store, account_id| {
            store.next_scheduled_send_at(account_id)
        })
        .await
        {
            Ok(Some(at)) => NextDue::At(at),
            Ok(None) => NextDue::Nothing,
            Err(_) => {
                eprintln!("squelch-api: scheduled send due read failed");
                NextDue::Unknown
            }
        };
        let until_maintenance = timing
            .maintenance
            .saturating_sub(maintenance_ended.map_or(Duration::ZERO, |at| at.elapsed()));
        wait = scheduled_send_wait(
            next_due,
            clock(&state),
            outcome,
            until_maintenance,
            timing.floor,
        );
    }
}

/// What the due read said after a round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NextDue {
    /// Nothing scheduled with attempts left.
    Nothing,
    /// The earliest `send_at` with attempts left.
    At(DateTime<Utc>),
    /// The read failed: nothing is known.
    Unknown,
}

/// HOW LONG THE WORKER SLEEPS after a round: until the next row is due, or
/// until maintenance, whichever is sooner; nothing at all when a row is
/// already due and the round's claim left it there without failing (the
/// round was capped, or the row came due between the claim's clock and this
/// one), so it is claimed at once; and `floor` when the round could not
/// tell what is due (the claim failed, the due read failed), so a store
/// error is retried on the old tick and never spun on. Pure, so the ladder
/// is testable without a clock.
fn scheduled_send_wait(
    next_due: NextDue,
    now: DateTime<Utc>,
    outcome: ClaimOutcome,
    until_maintenance: Duration,
    floor: Duration,
) -> Duration {
    let floor = floor.min(until_maintenance);
    match next_due {
        NextDue::Unknown => floor,
        NextDue::Nothing => until_maintenance,
        NextDue::At(at) => {
            let due = (at - now).to_std().unwrap_or(Duration::ZERO);
            if !due.is_zero() {
                due.min(until_maintenance)
            } else if outcome == ClaimOutcome::Failed {
                floor
            } else {
                Duration::ZERO
            }
        }
    }
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

/// Whether the claim on `row` has too little lease left at `now` to post
/// inside it: less than the parent's headers (one 60 s call) plus the upload's
/// own budget. A lease-less row has nothing to outrun.
fn lease_too_short_to_post(row: &ScheduledSend, now: DateTime<Utc>) -> bool {
    let needed = lease_for(row) - SCHEDULED_LEASE_BASE + chrono::Duration::seconds(60);
    row.lease_until.is_some_and(|end| end - now < needed)
}

/// Who is asking Gmail about a row, which decides where the attempt being
/// asked about ENDED ([`scan_window`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Asking {
    /// The pre-fire lookup of a claimed `maybe_sent` row: the attempt asked
    /// about is the LAST one, which ended at `last_attempt_at`. The row's
    /// `lease_until` is the claim just taken, not that attempt.
    PreFire,
    /// Recovery of a lapsed `sending` row: the attempt asked about is the one
    /// whose lease lapsed, so it ended at `lease_until`. `last_attempt_at`,
    /// when set, is an EARLIER attempt.
    Recovery,
}

/// The `(after, before)` instants the Sent scan of
/// [`MailWriter::find_scheduled_send`] searches between, [`SCAN_SLACK`]
/// included on both sides.
///
/// FLOOR: the earliest of `send_at`, the last attempt's end and the lease's
/// end, less one whole lease. `send_at` alone will not do, because a retry or
/// a requeue moves it PAST the attempt being asked about.
///
/// CEILING: the attempt's END (see [`Asking`]; with neither on the row,
/// `send_at` plus one lease). An attempt cannot have posted after its own
/// end, and without a ceiling a send-now of a row that failed a day ago would
/// scan a day of Sent mail, hit the page cap on any busy mailbox, and be
/// deferred into `failed:recovery` on every try: the row could never go out.
/// Bounded both ways, the window is about two hours plus one lease however old
/// the row is.
fn scan_window(row: &ScheduledSend, asking: Asking) -> (DateTime<Utc>, DateTime<Utc>) {
    let lease = lease_for(row);
    let earliest = [Some(row.send_at), row.last_attempt_at, row.lease_until]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(row.send_at);
    let ended = match asking {
        Asking::PreFire => row.last_attempt_at.or(row.lease_until),
        Asking::Recovery => row.lease_until.or(row.last_attempt_at),
    }
    .unwrap_or(row.send_at + lease);
    (earliest - lease - SCAN_SLACK, ended + SCAN_SLACK)
}

/// Step 1: every `sending` row whose lease lapsed at least [`RECOVERY_GRACE`]
/// ago (a worker that died mid-send) is asked of Gmail by its minted
/// Message-ID and, failing that, its `X-Passband-Outbox` token (§4). At most [`MAX_PER_PASS`] rows a pass, like the claims: a
/// backlog of lapsed rows (a long outage) is asked about over passes, not in
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
    let Ok(client) = write_client(state).await else {
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
        match client
            .find_scheduled_send(
                &row.message_id_header,
                scan_window(&row, Asking::Recovery),
                &|| stopping(state),
            )
            .await
        {
            Ok(Some(gmail_id)) => {
                if settle_recovered(state, &*client, &row, gmail_id).await {
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
    client: &dyn MailWriter,
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
    // YET): ask by Message-ID and outbox token before sending again. Found: settled, no send.
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
        let client = match write_client(state).await {
            Ok(c) => c,
            Err(_) => {
                // Nothing could be sent without it either: the ordinary
                // retryable failure a fire without a credential is. Audited
                // only when the settle moved the row (see `fail_unsent`).
                if settle_retry(state, &row, "rejected:no_write_credential", false, report).await {
                    audit_fire(state, &row, "rejected:no_write_credential").await;
                }
                return;
            }
        };
        match client
            .find_scheduled_send(
                &row.message_id_header,
                scan_window(&row, Asking::PreFire),
                &|| stopping(state),
            )
            .await
        {
            Ok(Some(gmail_id)) => {
                if settle_recovered(state, &*client, &row, gmail_id).await {
                    report.recovered_sent += 1;
                }
                return;
            }
            // A LOOKUP THAT ATE THE LEASE posts nothing: the scan can outrun
            // a lease sized for three calls (see [`SCHEDULED_LEASE_BASE`]),
            // and a post after the lease's end is one a later lookup's
            // window ([`scan_window`], which ends at the lease) cannot see.
            // Left: the parent's headers (one call) and the upload's own
            // budget. Short of that, it is could-not-ask, and nothing is sent.
            Ok(None) if lease_too_short_to_post(&row, clock(state)) => {
                let overran = WriteError::Transport("the lookup outran the lease".into());
                search_failed(state, &row, &overran, report).await;
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
                if settle_retry(state, &row, "failed:store", false, report).await {
                    audit_fire(state, &row, "failed:store").await;
                }
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
        Err(failure) => {
            settle_retry(state, &row, failure.reason, failure.maybe_sent, report).await;
        }
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
        if settle_retry(state, row, reason, true, report).await {
            audit_fire(state, row, reason).await;
        }
        return;
    }
    let now = clock(state);
    if row
        .search_deferred_since
        .is_some_and(|since| now - since >= RECOVERY_GIVE_UP)
    {
        eprintln!("squelch-api: scheduled send {id} could never be searched for; failed");
        // This claim sent nothing: the last attempt's end stays what it was.
        // Never NULL here: only a `maybe_sent` row is searched for, and every
        // write that sets that flag stamps `last_attempt_at` with it (a
        // failed or requeued claim), so the `None` = now fallback of
        // `settle_failed` cannot fire on this path.
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
/// before a send-now sends it again.
///
/// `true` when the row moved. A caller that audits the attempt itself (the
/// worker's own refusals; `perform_send` audits its own) does so only then,
/// the `fail_unsent` rule: a worker that outlived its claim decided nothing.
async fn settle_retry(
    state: &ApiState,
    row: &ScheduledSend,
    reason: &'static str,
    maybe_sent: bool,
    report: &mut ScheduledPassReport,
) -> bool {
    if row.attempts >= SCHEDULED_MAX_ATTEMPTS {
        eprintln!(
            "squelch-api: scheduled send {} failed ({reason}) on attempt {}; giving up",
            row.id, row.attempts
        );
        return settle_failed(state, row, reason, maybe_sent, None, report).await;
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
            true
        }
        Ok(false) => false,
        Err(_) => {
            eprintln!("squelch-api: scheduled send {id} retry not recorded");
            false
        }
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

    /// IDLE COST OF THE SCHEDULED-SEND WORKER, ignored by default: sixty
    /// passes over an empty outbox on a FILE-backed store, which is what ten
    /// minutes of the daemon's ten-second tick cost before this change,
    /// printing the wall time per pass. Run the test binary under
    /// `strace -f -c -e trace=fcntl,pwrite64,fsync,fdatasync` to count the
    /// locks and writes those passes asked the kernel for;
    /// `SQUELCH_BENCH_SKIP=1` runs the setup alone so the two counts can be
    /// subtracted.
    ///
    /// ```text
    /// cargo test -p squelch-api --lib scheduled_send::tests::idle_pass_cost -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore]
    async fn idle_pass_cost() {
        let dir = std::env::temp_dir().join(format!("squelch-sched-idle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = std::sync::Arc::new(
            squelch_core::store::SqliteStore::open(dir.join("store.sqlite3")).unwrap(),
        );
        let acct = store.ensure_account("me@example.com").unwrap();
        let state = ApiState::new(store, acct, "probe-token");
        if std::env::var_os("SQUELCH_BENCH_SKIP").is_some() {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        const PASSES: u32 = 60;
        let start = std::time::Instant::now();
        for _ in 0..PASSES {
            let report = run_scheduled_send_pass(&state).await;
            assert!(report.is_idle());
        }
        let elapsed = start.elapsed();
        eprintln!(
            "BENCH scheduled_idle: {PASSES} passes in {elapsed:?} ({:?} per pass)",
            elapsed / PASSES
        );
        // What the same ten idle minutes cost the worker now: a maintenance
        // round a minute, each the pass plus the next-due read.
        const ROUNDS: u32 = 10;
        let start = std::time::Instant::now();
        for _ in 0..ROUNDS {
            let report = run_scheduled_send_pass(&state).await;
            assert!(report.is_idle());
            let next = store_call(&state, |store, account_id| {
                store.next_scheduled_send_at(account_id)
            })
            .await
            .unwrap();
            assert!(next.is_none());
        }
        eprintln!(
            "BENCH scheduled_idle_rounds: {ROUNDS} maintenance rounds in {:?}",
            start.elapsed()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE WORKER'S SLEEP, as a table: to the next row, to maintenance,
    /// whichever is sooner; nothing when a row is already due and the claim
    /// did not fail (capped, or the row came due since the claim looked);
    /// the floor when the round could not tell what is due.
    #[test]
    fn the_worker_sleeps_to_the_next_send_or_maintenance_and_never_spins() {
        use ClaimOutcome::*;
        let s = Duration::from_secs;
        let now = Utc::now();
        let at = |secs: i64| NextDue::At(now + chrono::Duration::seconds(secs));
        for (next_due, outcome, until_maintenance, expected, why) in [
            (
                NextDue::Nothing,
                Drained,
                s(45),
                s(45),
                "nothing scheduled: maintenance",
            ),
            (
                at(30),
                Drained,
                s(45),
                s(30),
                "a row ahead: until it is due",
            ),
            (
                at(90),
                Drained,
                s(45),
                s(45),
                "a row past maintenance: maintenance first",
            ),
            (
                at(-5),
                Capped,
                s(45),
                s(0),
                "capped with more due: again at once",
            ),
            (
                at(0),
                Drained,
                s(45),
                s(0),
                "due since the claim looked: at once",
            ),
            (
                at(-5),
                Failed,
                s(45),
                s(10),
                "due and the claim failed: the floor",
            ),
            (at(-5), Failed, s(4), s(4), "unless maintenance is sooner"),
            (
                NextDue::Unknown,
                Drained,
                s(45),
                s(10),
                "due read failed: the floor",
            ),
            (
                NextDue::Unknown,
                Drained,
                s(3),
                s(3),
                "or maintenance, if sooner",
            ),
        ] {
            assert_eq!(
                scheduled_send_wait(next_due, now, outcome, until_maintenance, s(10)),
                expected,
                "{why}"
            );
        }
    }

    /// A state over an in-memory store, with nothing wired in: no write
    /// credential, so a claimed row settles as a retry, which is the
    /// observable a claim leaves (`attempts` counts it).
    fn idle_state() -> (
        ApiState,
        std::sync::Arc<squelch_core::store::SqliteStore>,
        i64,
    ) {
        let store =
            std::sync::Arc::new(squelch_core::store::SqliteStore::open_in_memory().unwrap());
        let acct = store.ensure_account("me@example.com").unwrap();
        (ApiState::new(store.clone(), acct, "tok"), store, acct)
    }

    fn schedule_row(
        store: &squelch_core::store::SqliteStore,
        acct: i64,
        send_at: DateTime<Utc>,
    ) -> i64 {
        let request =
            json!({"to": "alice@example.com", "subject": "s", "body": "b", "confirm": true});
        store
            .schedule_send(
                acct,
                &request.to_string(),
                send_at,
                &format!("<{}@squelch.test>", send_at.timestamp_nanos_opt().unwrap()),
                &[],
                None,
                Utc::now(),
            )
            .unwrap()
            .id
    }

    /// THE WORKER CLAIMS A ROW AT ITS TIME, not at the next tick: a row due
    /// in three seconds is untouched after one and a half and claimed by
    /// four and a half (the old ten-second tick could be nine seconds late).
    /// And A POKE WAKES IT: with nothing due for minutes, a row scheduled a
    /// second out and poked for is claimed within two and a half.
    #[tokio::test]
    async fn the_worker_claims_a_send_at_its_time_and_wakes_on_a_poke() {
        let (state, store, acct) = idle_state();
        let (stop, shutdown) = tokio::sync::watch::channel(false);
        let timing = ScheduledWorkerTiming {
            first_look: Duration::ZERO,
            maintenance: Duration::from_secs(3600),
            floor: Duration::from_secs(10),
            round_counter: None,
        };
        let worker = tokio::spawn(scheduled_send_worker(state.clone(), shutdown, timing));
        let attempts = |id: i64| {
            store
                .scheduled_send_by_id(acct, id)
                .unwrap()
                .unwrap()
                .attempts
        };

        // Let the first look pass over an empty outbox, then schedule.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let first = schedule_row(&store, acct, Utc::now() + chrono::Duration::seconds(3));
        state.scheduled_wake.notify_one();
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(attempts(first), 0, "not due yet, not claimed");
        tokio::time::sleep(Duration::from_millis(3000)).await;
        assert_eq!(attempts(first), 1, "claimed at its time");
        assert_eq!(
            store
                .scheduled_send_by_id(acct, first)
                .unwrap()
                .unwrap()
                .state,
            ScheduledSendState::Scheduled,
            "and, with no write credential, put back for a retry minutes out"
        );

        // The worker is asleep until that retry: a poke brings it to the
        // sooner row.
        let second = schedule_row(&store, acct, Utc::now() + chrono::Duration::seconds(1));
        state.scheduled_wake.notify_one();
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert_eq!(attempts(second), 1, "woken for the sooner row");

        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .expect("the worker stops on shutdown")
            .unwrap();
    }

    /// AN IDLE OUTBOX IS LOOKED AT ONCE PER MAINTENANCE: with maintenance at
    /// two seconds, five seconds of nothing scheduled are about three rounds
    /// (0, 2 and 4 s), where a one-second floor spun on would be five or
    /// more and the old ten-second tick none at all.
    #[tokio::test]
    async fn an_idle_outbox_wakes_the_worker_only_for_maintenance() {
        let (state, _store, _acct) = idle_state();
        let (stop, shutdown) = tokio::sync::watch::channel(false);
        let rounds = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let timing = ScheduledWorkerTiming {
            first_look: Duration::ZERO,
            maintenance: Duration::from_secs(2),
            floor: Duration::from_secs(1),
            round_counter: Some(rounds.clone()),
        };
        let worker = tokio::spawn(scheduled_send_worker(state.clone(), shutdown, timing));
        tokio::time::sleep(Duration::from_millis(5000)).await;
        let rounds = rounds.load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            (2..=4).contains(&rounds),
            "rounds at 0, 2 and 4 s, give or take a loaded runner: {rounds}"
        );
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .expect("the worker stops on shutdown")
            .unwrap();
    }

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
        // A DOTTED host, and the token is the whole local part.
        assert!(a.starts_with('<') && a.ends_with("@passband.app>"));
        let local = &a[1..a.len() - "@passband.app>".len()];
        assert_eq!(local.len(), 48);
        assert!(local.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(crate::write::mime::outbox_token(&a), local);
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
        assert_eq!(
            (
                v.to.as_str(),
                v.cc.as_str(),
                v.bcc.as_str(),
                v.subject.as_str()
            ),
            ("", "", "", "")
        );
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

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000 + secs, 0).unwrap()
    }

    #[test]
    fn a_requeued_rows_window_is_its_last_attempt_not_its_new_send_at() {
        // Failed a day ago, then sent now: `send_at` is today, the attempt
        // asked about ended a day earlier. The claim's lease is today too.
        let mut row = row_with(r#"{"to":"a@x","body":"hi"}"#, &[]);
        row.maybe_sent = true;
        row.last_attempt_at = Some(at(0));
        row.send_at = at(86_400);
        let lease = lease_for(&row);
        for claimed in [None, Some(at(86_400) + lease)] {
            row.lease_until = claimed;
            let (after, before) = scan_window(&row, Asking::PreFire);
            assert_eq!(after, at(0) - lease - SCAN_SLACK);
            // The ceiling is that attempt's end, NOT today: a day of Sent
            // mail is what made the old unbounded scan hit its cap.
            assert_eq!(before, at(0) + SCAN_SLACK);
        }
    }

    #[test]
    fn a_recovered_rows_window_ends_where_its_lease_lapsed() {
        let mut row = row_with(r#"{"to":"a@x","body":"hi"}"#, &[]);
        row.state = ScheduledSendState::Sending;
        row.send_at = at(0);
        let lease = lease_for(&row);
        row.lease_until = Some(at(0) + lease);
        row.last_attempt_at = None;
        let (after, before) = scan_window(&row, Asking::Recovery);
        assert_eq!(after, at(0) - lease - SCAN_SLACK);
        assert_eq!(before, at(0) + lease + SCAN_SLACK);
        // An EARLIER attempt's end on the row does not pull the ceiling back
        // before the lapsed attempt; it only widens the floor.
        row.last_attempt_at = Some(at(-600));
        let (after, before) = scan_window(&row, Asking::Recovery);
        assert_eq!(after, at(-600) - lease - SCAN_SLACK);
        assert_eq!(before, at(0) + lease + SCAN_SLACK);
    }

    #[test]
    fn a_lookup_that_ate_the_lease_leaves_nothing_to_post_in() {
        let mut row = row_with(r#"{"to":"a@x","body":"hi"}"#, &[]);
        let lease = lease_for(&row);
        let upload = lease - SCHEDULED_LEASE_BASE + chrono::Duration::seconds(60);
        row.lease_until = Some(at(0) + lease);
        // A lookup of a few seconds leaves plenty.
        assert!(!lease_too_short_to_post(&row, at(5)));
        // The last instant that still fits the parent's headers and the upload.
        assert!(!lease_too_short_to_post(&row, at(0) + lease - upload));
        assert!(lease_too_short_to_post(&row, at(1) + lease - upload));
        // A lapsed lease.
        assert!(lease_too_short_to_post(&row, at(0) + lease + lease));
        row.lease_until = None;
        assert!(!lease_too_short_to_post(&row, at(86_400)));
    }

    #[test]
    fn a_row_with_only_a_send_at_scans_one_lease_either_side_of_it() {
        let mut row = row_with(r#"{"to":"a@x","body":"hi"}"#, &[]);
        row.send_at = at(0);
        let lease = lease_for(&row);
        for asking in [Asking::PreFire, Asking::Recovery] {
            assert_eq!(
                scan_window(&row, asking),
                (at(0) - lease - SCAN_SLACK, at(0) + lease + SCAN_SLACK)
            );
        }
    }
}
