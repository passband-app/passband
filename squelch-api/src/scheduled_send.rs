//! Scheduled send: the `/client/scheduled` door and the worker that fires it.
//! See docs/SCHEDULED-SEND.md; §3 is the schedule handler, §4 the worker, §5
//! the rest of the door.
//!
//! HUMAN DOOR ONLY, like drafts: the outbox is the user's own finished mail
//! waiting to go, and no `/mcp` surface reads `scheduled_sends` or the files
//! it claims. Every response here is `no-store`, errors included (a listing
//! and a cancel both carry the user's own words).
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
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use squelch_core::store::{ATTACHMENT_MISSING, CancelOutcome, ParentContext, ScheduledSend};
use squelch_core::types::GroupMode;

use crate::error::ApiError;
use crate::gmail_write::{GmailWriteClient, SentRef};
use crate::group_send;
use crate::handlers::{
    Fire, OutboundAttachmentView, SendBody, SendGuard, SendOutcome, audit_action,
    composition_kinds, dedup_ids, echo_sent, forward_original_kinds, guard_verdict,
    load_send_attachments, mail_attachment, no_store, perform_send, read_forward_original,
    resolve_target, send_target, store_call, validate_send, write_client,
};
use crate::state::ApiState;

/// Audit action for scheduling, and for every refusal on the way to it.
const AUDIT_SCHEDULE: &str = "schedule";
const AUDIT_CANCEL: &str = "schedule.cancel";
const AUDIT_NOW: &str = "schedule.now";

/// How far ahead a mail may be scheduled. Past a year it is far more likely a
/// typo in the palette (2027 for 2026) than an intent, and a row that sits for
/// longer than the write credential's life is a failure waiting to happen.
const MAX_SCHEDULE_AHEAD: chrono::Duration = chrono::Duration::days(365);

/// The worker's claim lease. A send that has not settled inside it is
/// presumed to have died mid-send, and the next pass asks Gmail by Message-ID
/// before anything is retried.
pub(crate) const SCHEDULED_LEASE: chrono::Duration = chrono::Duration::seconds(120);

/// Claims a row gets before it is `failed` and waits for the user. The claim
/// counts, so a row fails on its third unsuccessful attempt.
pub(crate) const SCHEDULED_MAX_ATTEMPTS: u32 = 3;

/// `2^attempts` minutes after a failed attempt: 2, then 4 (the third failure
/// is terminal, so 8 is never waited out).
fn retry_backoff(attempts: u32) -> chrono::Duration {
    chrono::Duration::minutes(1i64 << attempts.min(16))
}

/// Every response on this door carries `Cache-Control: no-store`, the error
/// bodies too: one spelling, applied once per handler, so no arm can forget.
fn no_store_response(result: Result<Response, ApiError>) -> Response {
    let mut resp = match result {
        Ok(r) => r,
        Err(e) => e.into_response(),
    };
    for (name, value) in no_store().iter() {
        resp.headers_mut().insert(name.clone(), value.clone());
    }
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
/// `201 {"id", "send_at"}`. Refusals are the send route's own, audited under
/// `schedule` with the same details.
pub async fn schedule_send(
    State(state): State<ApiState>,
    Json(req): Json<ScheduleBody>,
) -> Response {
    no_store_response(schedule(&state, req).await)
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
    // addressed; `group_id` stays in the request for attribution at fire time.
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

    // A reply to a message that is not there (another account's, sealed away,
    // never stored) would fail at fire time three times over and land as
    // "didn't send" hours later. Asked now, it is the same 404 a send gets.
    if let Some(parent) = body.reply_to_message_id
        && let Err(e) = resolve_target(state, parent).await
    {
        audit_action(state, AUDIT_SCHEDULE, target, "failed:target").await;
        return Err(e);
    }

    // 4. The files, exactly as a send reads them: every id resolves (from the
    // draft being consumed, or unclaimed), 25 MB in total.
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

    // 7-8. Mint the Message-ID and freeze the request: `confirm` forced true
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

    // 9. ONE transaction: the row, the files moved onto it, the draft gone.
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
        Err(e) => {
            audit_action(state, AUDIT_SCHEDULE, target, "failed:store").await;
            return Err(e);
        }
    };

    // 10. Ids only.
    audit_action(state, AUDIT_SCHEDULE, target, &format!("ok:{}", row.id)).await;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": row.id, "send_at": row.send_at.to_rfc3339() })),
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
pub async fn list_scheduled_sends(State(state): State<ApiState>) -> Response {
    no_store_response(
        async {
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
        .await,
    )
}

// --- DELETE /client/scheduled/{id} -------------------------------------------

/// `DELETE /client/scheduled/{id}` — pull a mail back out of the outbox.
///
/// `200 {"status": "cancelled", "id", "request", "attachments"}`: `request` is
/// the frozen send body as a JSON object (the composer reopens from it), and
/// `attachments` the files it held, now unclaimed, so the tray can be redrawn
/// and the composer's next autosave re-claims them. No draft is created here.
///
/// `409` for a row already in Gmail's hands; `404` for an unknown id, another
/// account's, or one already sent or cancelled. Audited, ids only.
pub async fn cancel_scheduled_send(State(state): State<ApiState>, Path(id): Path<i64>) -> Response {
    no_store_response(
        async {
            let outcome = store_call(&state, move |store, account_id| {
                store.cancel_scheduled_send(account_id, id, Utc::now())
            })
            .await?;
            let target = Some(id.to_string());
            match outcome {
                CancelOutcome::Cancelled(row) => {
                    audit_action(&state, AUDIT_CANCEL, target, "ok").await;
                    let request = serde_json::from_str::<serde_json::Value>(&row.request)
                        .unwrap_or(serde_json::Value::Null);
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
                    Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "this mail is already being sent and can no longer be cancelled",
                    ))
                }
                CancelOutcome::NotFound => {
                    audit_action(&state, AUDIT_CANCEL, target, "failed:not_found").await;
                    Err(ApiError::not_found())
                }
            }
        }
        .await,
    )
}

// --- POST /client/scheduled/{id}/now -----------------------------------------

/// `POST /client/scheduled/{id}/now` — send a `scheduled` or `failed` row on
/// the next tick. `202 {"status": "queued", "id"}`: queued, not sent; the
/// listing shows it go. A `failed` row gets a fresh retry budget (the store's
/// rule). `404` for anything else. Audited, ids only.
pub async fn send_scheduled_now(State(state): State<ApiState>, Path(id): Path<i64>) -> Response {
    no_store_response(
        async {
            let queued = store_call(&state, move |store, account_id| {
                store.send_scheduled_now(account_id, id, Utc::now())
            })
            .await?;
            let target = Some(id.to_string());
            if !queued {
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
        .await,
    )
}

// --- the worker ---------------------------------------------------------------

/// What one [`run_scheduled_send_pass`] did, as counts (ids go to the log).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScheduledPassReport {
    /// Lapsed `sending` rows Gmail already had: settled `sent`, not re-sent.
    pub recovered_sent: usize,
    /// Lapsed `sending` rows Gmail did not have: back to `scheduled`.
    pub requeued: usize,
    pub sent: usize,
    /// Failed attempts put back with a backoff.
    pub retried: usize,
    /// Rows that went terminal (`failed`) this pass.
    pub failed: usize,
    /// Settled rows past their 7 days, dropped.
    pub swept: usize,
}

impl ScheduledPassReport {
    /// Whether the pass moved anything worth a log line.
    pub fn is_idle(&self) -> bool {
        *self == Self::default()
    }
}

/// One pass of the outbox (docs/SCHEDULED-SEND.md §4): RECOVER lapsed leases,
/// CLAIM and send every due row, SWEEP settled ones. The daemon's timer calls
/// it every tick; the tick is the clock, so a restart costs punctuality and
/// never a mail.
///
/// Exported for the daemon because it needs the WRITE credential, which the
/// readonly-bound sync loop must never touch. Never fails: every error is
/// logged (ids only) and left for the next pass, which is what makes firing
/// it unconditionally safe.
///
/// EXACTLY ONCE: a row is sent only from `sending` under a lease, and a row
/// whose lease lapsed is answered by a Message-ID search BEFORE it can be
/// claimed again. A search that fails leaves it `sending`: "could not ask" is
/// never "did not go out".
pub async fn run_scheduled_send_pass(state: &ApiState) -> ScheduledPassReport {
    let mut report = ScheduledPassReport::default();
    recover(state, &mut report).await;

    loop {
        let claimed = store_call(state, |store, account_id| {
            store.claim_due_scheduled_send(account_id, Utc::now(), SCHEDULED_LEASE)
        })
        .await;
        match claimed {
            Ok(Some(row)) => fire(state, row, &mut report).await,
            Ok(None) => break,
            Err(_) => {
                eprintln!("squelch-api: scheduled send claim failed; retrying next pass");
                break;
            }
        }
    }

    match store_call(state, |store, account_id| {
        store.sweep_scheduled_sends(account_id, Utc::now())
    })
    .await
    {
        Ok(n) => report.swept = n,
        Err(_) => eprintln!("squelch-api: scheduled send sweep failed"),
    }
    report
}

/// Step 1: every `sending` row whose lease lapsed (a worker that died
/// mid-send) is asked of Gmail by its minted Message-ID.
async fn recover(state: &ApiState, report: &mut ScheduledPassReport) {
    let expired = match store_call(state, |store, account_id| {
        store.expired_sending_scheduled_sends(account_id, Utc::now())
    })
    .await
    {
        Ok(rows) => rows,
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
        // `sending` until a credential exists to search with.
        eprintln!(
            "squelch-api: {} scheduled send(s) await recovery; no write credential",
            expired.len()
        );
        return;
    };
    for row in expired {
        match client.find_by_rfc822_msgid(&row.message_id_header).await {
            Ok(Some(gmail_id)) => {
                if settle_recovered(state, &client, &row, gmail_id).await {
                    report.recovered_sent += 1;
                }
            }
            Ok(None) => {
                let id = row.id;
                match store_call(state, move |store, account_id| {
                    store.requeue_scheduled_send(account_id, id, Utc::now())
                })
                .await
                {
                    Ok(true) => {
                        eprintln!("squelch-api: scheduled send {id} never reached Gmail; requeued");
                        report.requeued += 1;
                    }
                    Ok(false) => {}
                    Err(_) => eprintln!("squelch-api: scheduled send {id} requeue failed"),
                }
            }
            Err(_) => eprintln!(
                "squelch-api: scheduled send {} recovery search failed; still sending",
                row.id
            ),
        }
    }
}

/// A lapsed row Gmail already has: echo it (best effort, so the Sent list and
/// the row both point at the local copy), drop its files, settle it `sent`.
/// NO SECOND SEND. `true` when the row moved.
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
    let echo_message_id = echo_sent(state, client, target.clone(), &sent).await;
    let thread_id = match echo_message_id {
        Some(local) => resolve_target(state, local).await.ok().map(|m| m.thread_id),
        None => None,
    };
    let id = row.id;
    let ids: Vec<i64> = row.attachments.iter().map(|a| a.id).collect();
    discard_scheduled_files(state, id, ids).await;
    let settled = store_call(state, move |store, account_id| {
        store.settle_scheduled_sent(
            account_id,
            id,
            echo_message_id,
            thread_id.as_deref(),
            Utc::now(),
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

/// Steps 3-4 for one claimed row: send it, then settle.
async fn fire(state: &ApiState, row: ScheduledSend, report: &mut ScheduledPassReport) {
    let id = row.id;
    let Ok(body) = serde_json::from_str::<SendBody>(&row.request) else {
        // Nothing to retry: the same bytes parse the same way next time.
        eprintln!("squelch-api: scheduled send {id} has an unreadable request; failed");
        settle_failed(state, id, "failed:request", report).await;
        return;
    };

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
            // The row's own files are claimed by it and never swept while it
            // lives, so a missing one is not going to come back: terminal,
            // rather than three sends refused the same way.
            Ok(_) => {
                eprintln!("squelch-api: scheduled send {id} lost an attached file; failed");
                settle_failed(state, id, "failed:attachment_missing", report).await;
                return;
            }
            Err(_) => {
                settle_retry(state, &row, "failed:store", report).await;
                return;
            }
        }
    };

    let fired = Fire {
        id,
        message_id: &row.message_id_header,
    };
    match perform_send(state, &body, attachments, SendGuard::Settled, Some(fired)).await {
        Ok(outcome) => {
            let (echo_message_id, thread_id) = match outcome {
                SendOutcome::Sent {
                    echo_message_id,
                    thread_id,
                } => (echo_message_id, thread_id),
                // Scheduling refuses a fan-out, so a row answering this was
                // stored by something else. It went, so it is settled sent.
                SendOutcome::Sending { .. } => {
                    eprintln!("squelch-api: scheduled send {id} fanned out; settled sent");
                    (None, None)
                }
            };
            // `finish_send` consumed only unclaimed files (it knows no
            // scheduled id); this row's own go here.
            discard_scheduled_files(state, id, ids).await;
            match store_call(state, move |store, account_id| {
                store.settle_scheduled_sent(
                    account_id,
                    id,
                    echo_message_id,
                    thread_id.as_deref(),
                    Utc::now(),
                )
            })
            .await
            {
                Ok(true) => report.sent += 1,
                Ok(false) => eprintln!("squelch-api: scheduled send {id} sent; row had moved"),
                Err(_) => eprintln!("squelch-api: scheduled send {id} sent but not settled"),
            }
        }
        // `perform_send` already wrote the refusal to the ledger.
        Err(failure) => settle_retry(state, &row, failure.reason, report).await,
    }
}

/// A failed attempt: back to `scheduled` after the backoff while attempts
/// remain, else `failed` for the user.
async fn settle_retry(
    state: &ApiState,
    row: &ScheduledSend,
    reason: &'static str,
    report: &mut ScheduledPassReport,
) {
    if row.attempts >= SCHEDULED_MAX_ATTEMPTS {
        eprintln!(
            "squelch-api: scheduled send {} failed ({reason}) on attempt {}; giving up",
            row.id, row.attempts
        );
        settle_failed(state, row.id, reason, report).await;
        return;
    }
    let id = row.id;
    let next = Utc::now() + retry_backoff(row.attempts);
    match store_call(state, move |store, account_id| {
        store.settle_scheduled_retry(account_id, id, next, reason, Utc::now())
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

async fn settle_failed(
    state: &ApiState,
    id: i64,
    reason: &'static str,
    report: &mut ScheduledPassReport,
) {
    match store_call(state, move |store, account_id| {
        store.settle_scheduled_failed(account_id, id, reason, Utc::now())
    })
    .await
    {
        Ok(true) => report.failed += 1,
        Ok(false) => {}
        Err(_) => eprintln!("squelch-api: scheduled send {id} failure not recorded"),
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
        assert!(parse_send_at(Some(&fmt(now + chrono::Duration::days(366))), now).is_err());
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
            created_at: now,
            updated_at: now,
            attachments: Vec::new(),
        };
        let v = ScheduledSendView::new(row, ParentContext::default());
        assert_eq!((v.to.as_str(), v.subject.as_str()), ("", ""));
        assert_eq!(v.state, "failed");
    }
}
