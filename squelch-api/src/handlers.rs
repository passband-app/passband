//! `/client/*` handlers for the human door.
//!
//! Handlers are thin: validate params, call the store via `spawn_blocking`,
//! serialize core types to JSON. Humans can read pending and restricted mail.
//! External agents use the separate guarded routes in `agent.rs`.

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use squelch_core::CoreError;
use squelch_core::config::{CACHE_READ_INPUT_MULT, CACHE_WRITE_INPUT_MULT};
use squelch_core::mail::MailProvider;
use squelch_core::store::{
    ActionMessageRef, ClientPlatform, Draft, DraftKey, FtsQuery, NewAuditEntry,
    OutboundAttachmentMeta, SearchDiagnostics, SearchFilter, SearchSort, SenderDigestRow,
    SitrepBand, SmartBlockMemo, SpamScope, SqliteStore, Store,
};
use squelch_core::sync::{decode_raw_b64url, parse_internal_date};
use squelch_core::triage::llm::Usage;
use squelch_core::triage::rule_infer;
use squelch_core::triage::rule_suggest;
use squelch_core::types::{
    AccountId, AttentionStatus, Disposition, GroupMode, SenderRule, ShredStats, Tier, TriageAxis,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::error::ApiError;
use crate::group_send;
use crate::guard;
use crate::state::ApiState;
use crate::write::gmail::GmailWriteClient;
use crate::write::mime::{
    ForwardParts, ForwardedOriginal, MailAttachment, ReplyParts, addrs_excluding,
    build_forward_rfc822, build_references, build_reply_rfc822, cc_excluding, count_addrs,
    derive_reply_recipients, forward_subject, mark_inline, outbox_token, parse_addr_list,
    parse_forwarded_original, reply_subject,
};
use crate::write::{MailWriter, SentRef, WriteError};

/// Default page size when `limit` is omitted, and the hard ceiling we clamp to.
const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 500;
/// Default `since` window for `/client/updates` when the caller omits it.
const DEFAULT_UPDATES_WINDOW_DAYS: i64 = 30;

/// Actor label written into the audit log for human-door reveals.
const AUDIT_ACTOR: &str = "human";

/// How long the post-send echo may spend on its read-back (see [`echo_sent`]).
const ECHO_FETCH_TIMEOUT_SECS: u64 = 5;

// --- pagination cursor ------------------------------------------------------

/// An opaque token round-tripping a row offset (`off:<n>`, base64url). Not
/// security-sensitive; it is a scroll position. Unpadded base64url, the same
/// alphabet and padding discipline the Gmail write path uses.
mod cursor {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    pub fn encode_offset(offset: u32) -> String {
        URL_SAFE_NO_PAD.encode(format!("off:{offset}"))
    }

    pub fn decode_offset(s: &str) -> Option<u32> {
        let bytes = URL_SAFE_NO_PAD.decode(s).ok()?;
        let text = String::from_utf8(bytes).ok()?;
        text.strip_prefix("off:")?.parse().ok()
    }
}

/// Resolve `(limit, offset)` from the `limit`/`cursor` query params. `limit` is
/// clamped to `[1, MAX_LIMIT]`; a malformed cursor is a 400.
fn paginate(limit: Option<u32>, cursor: Option<&str>) -> Result<(u32, u32), ApiError> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let offset = match cursor {
        Some(c) => cursor::decode_offset(c).ok_or_else(|| ApiError::bad_request("bad cursor"))?,
        None => 0,
    };
    Ok((limit, offset))
}

/// Build the `next_cursor` for a page: `Some` only if the page came back full
/// (so there may be more), pointing at the next offset.
fn next_cursor(returned: usize, limit: u32, offset: u32) -> Option<String> {
    (returned as u32 == limit).then(|| cursor::encode_offset(offset + limit))
}

/// Envelope for paginated list endpoints.
#[derive(Debug, Serialize)]
struct Page<T> {
    items: Vec<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

/// Why [`blocking_on`] could not run its closure: the permit never came (a
/// closed semaphore, which nothing here closes) or the closure panicked.
/// Opaque to every caller, which answers its own uniform failure.
pub(crate) struct Blocked;

/// Run `f` on a blocking thread holding one permit of `slots` for as long
/// as the thread runs. The permit is taken HERE, on the async side, where a
/// caller cut at its ceiling (`limits`), or whose client hung up, gives up
/// its place without a thread having been taken; and it is MOVED INTO the
/// closure, so a call already on a thread keeps its permit until the store
/// answers, whatever became of the caller. Every semaphore in `ApiState`
/// that bounds store work is taken this way, through here: the store-call
/// slots, the device-token lookups' permits, the pairing claims'.
pub(crate) async fn blocking_on<T, F>(
    slots: &std::sync::Arc<tokio::sync::Semaphore>,
    f: F,
) -> Result<T, Blocked>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let permit = slots.clone().acquire_owned().await.map_err(|_| Blocked)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        f()
    })
    .await
    .map_err(|_| Blocked)
}

/// Run a synchronous store closure off the async runtime, on one of the
/// door's [`crate::limits::STORE_CALL_SLOTS`] ([`blocking_on`] for how the
/// slot rides the thread). A panic inside it surfaces as an opaque 500.
pub(crate) async fn blocking<T, F>(state: &ApiState, f: F) -> Result<T, ApiError>
where
    F: FnOnce() -> Result<T, squelch_core::CoreError> + Send + 'static,
    T: Send + 'static,
{
    blocking_on(&state.store_call_slots, f)
        .await
        .map_err(|Blocked| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error"))?
        .map_err(ApiError::from)
}

/// Run a store call for the ACTIVE ACCOUNT off the async runtime: the preamble
/// every handler shares (clone the `Arc`, copy the account id, hop to a blocking
/// thread). The account id is threaded in rather than captured, so no call site
/// can accidentally query a different account.
pub(crate) async fn store_call<T, F>(state: &ApiState, f: F) -> Result<T, ApiError>
where
    F: FnOnce(&SqliteStore, AccountId) -> Result<T, CoreError> + Send + 'static,
    T: Send + 'static,
{
    let store = state.store.clone();
    let account_id = state.account_id;
    blocking(state, move || f(&store, account_id)).await
}

/// [`store_call`] for the common read shape: whatever the store returns IS the
/// JSON response body.
pub(crate) async fn query<T, F>(state: &ApiState, f: F) -> Result<Json<T>, ApiError>
where
    F: FnOnce(&SqliteStore, AccountId) -> Result<T, CoreError> + Send + 'static,
    T: Send + 'static,
{
    Ok(Json(store_call(state, f).await?))
}

// --- GET /client/updates ----------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct UpdatesQuery {
    since: Option<DateTime<Utc>>,
    min_importance: Option<u8>,
    tier: Option<String>,
    /// Attention-lifecycle filter: new|open|done.
    status: Option<String>,
    /// Server-side sitrep bucket: standing|new|open.
    band: Option<String>,
    /// THE REMINDER SCHEDULE: `pending` narrows to mail with a reminder still
    /// owed, soonest-first. Deliberately not a `band` value — every row in it is
    /// `done` (setting a reminder resolves the thread), so it composes with the
    /// other filters rather than sitting alongside them, and it is the one
    /// listing that never stamps the seen-ledger: a schedule the user is
    /// reviewing is not the mail being shown to them.
    reminders: Option<String>,
    /// THE PROVIDER'S SPAM FOLDER: `only` swaps the listing over to the mail
    /// Gmail filed as spam, which every other listing on this route excludes.
    /// Absent means the ordinary mailbox.
    ///
    /// Its own parameter and not a `tier` value, because spam is not a verdict
    /// this system reached — nothing triaged these rows, and they all carry the
    /// neutral tier=noise seed. A `tier=spam` would have made the provider's
    /// call look like one of ours in the one place the difference is the point.
    ///
    /// One accepted value and a 400 for anything else, for the reason
    /// `reminders` gives: a client asking for the spam folder and quietly
    /// getting the inbox back is a failure the user would read as data loss.
    spam: Option<String>,
    limit: Option<u32>,
    cursor: Option<String>,
    /// KEYSET PAGING, the mail list's infinite scroll: rows that arrived
    /// strictly before this `received_at` (and, at the same instant, with a
    /// smaller id than `before_id`; without one, nothing at that instant).
    /// Exclusive with `cursor`, and with `band` / `reminders`, whose listings
    /// are not in arrival order. With no `since`
    /// it reaches past the default window, back to the start of the mailbox.
    before: Option<DateTime<Utc>>,
    before_id: Option<i64>,
    /// READ WITHOUT SURFACING. Default false, so every existing client keeps the
    /// stamping fetch it has always had. `true` is for a reader acting on the
    /// user's behalf (the embedded agent answering "anything important today?"):
    /// it sees the whole page, but shows the human only the few rows it decides
    /// are worth raising. Stamping all of them would report rows as seen that
    /// nobody ever saw, collapsing New into Open and emptying the band the user
    /// reads. Whatever the agent does surface is stamped by the client's own
    /// (non-peek) fetch of those rows.
    #[serde(default)]
    peek: bool,
}

pub async fn get_updates(
    State(state): State<ApiState>,
    Query(q): Query<UpdatesQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let (limit, offset) = paginate(q.limit, q.cursor.as_deref())?;
    let tier_filter = match q.tier.as_deref() {
        None => None,
        Some(s) => Some(Tier::parse(s).ok_or_else(|| {
            ApiError::bad_request("tier must be one of: past_due, deadline, signal, noise")
        })?),
    };
    let status_filter = match q.status.as_deref() {
        None => None,
        Some(s) => Some(
            AttentionStatus::parse(s)
                .ok_or_else(|| ApiError::bad_request("status must be one of: new, open, done"))?,
        ),
    };
    let band = match q.band.as_deref() {
        None => None,
        Some(s) => Some(
            SitrepBand::parse(s)
                .ok_or_else(|| ApiError::bad_request("band must be one of: standing, new, open"))?,
        ),
    };
    // One accepted value, and an unknown one is a 400 rather than a silent
    // full listing: a client asking for the schedule and quietly getting the
    // whole mailbox back is the worst possible failure mode for this parameter.
    let pending_reminders = match q.reminders.as_deref() {
        None => false,
        Some("pending") => true,
        Some(_) => return Err(ApiError::bad_request("reminders must be: pending")),
    };
    let spam = match q.spam.as_deref() {
        None => SpamScope::Exclude,
        Some(s) => {
            SpamScope::parse(s).ok_or_else(|| ApiError::bad_request("spam must be: only"))?
        }
    };
    if q.before.is_some() && q.cursor.is_some() {
        return Err(ApiError::bad_request("before and cursor are exclusive"));
    }
    // An arrival keyset means nothing on a listing ordered by something else.
    if q.before.is_some() && (band.is_some() || pending_reminders) {
        return Err(ApiError::bad_request(
            "before pages the arrival-ordered listing only (no band, no reminders)",
        ));
    }
    // No `before_id`: strictly before the instant, so nothing AT it.
    let before = q.before.map(|at| (at, q.before_id.unwrap_or(i64::MIN)));
    let since = q.since.unwrap_or_else(|| {
        if before.is_some() {
            DateTime::<Utc>::UNIX_EPOCH
        } else {
            Utc::now() - chrono::Duration::days(DEFAULT_UPDATES_WINDOW_DAYS)
        }
    });
    // The store can stop at the page when nothing here narrows it further.
    let store_limit = (tier_filter.is_none()).then_some((offset + limit) as usize);
    let min_importance = q.min_importance;
    // A pending-reminder listing NEVER stamps, whatever `peek` says. Its rows are
    // deferred mail the user is reviewing the schedule of; stamping them surfaced
    // would report as seen exactly the mail they asked not to be shown yet.
    let peek = q.peek || pending_reminders;

    let ranking = state.triage_config.ranking.clone();
    let items = store_call(&state, move |store, account_id| {
        use squelch_core::store::agent_triage::{AgentInventoryQuery, AgentTriageStore};
        let mut all = store.human_agent_updates(
            account_id,
            &AgentInventoryQuery {
                before,
                limit: store_limit,
                since,
                min_importance,
                status: status_filter,
                band,
                pending_reminders,
                spam,
                ranking,
            },
        )?;
        if let Some(t) = tier_filter {
            all.retain(|u| u.update.tier == t);
        }
        let page = all
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .collect::<Vec<_>>();

        // Listing acknowledgement is distinct from opening a message. Pending
        // and restricted mail remain available to the human, including surfacing.
        if !peek {
            let ids: Vec<i64> = page.iter().map(|u| u.update.id).collect();
            store.mark_surfaced(account_id, &ids)?;
        }

        Ok(page)
    })
    .await?;

    // A keyset page's continuation is its last row, not an offset.
    let next = if before.is_some() {
        None
    } else {
        next_cursor(items.len(), limit, offset)
    };
    Ok(Json(Page {
        items,
        next_cursor: next,
    }))
}

// --- POST /client/updates/{message_id}/status -------------------------------

#[derive(Debug, Deserialize)]
pub struct StatusBody {
    /// "done" to dismiss, "open" to reopen. ("new" is accepted for symmetry.)
    status: String,
}

pub async fn set_update_status(
    State(state): State<ApiState>,
    Path(message_id): Path<i64>,
    Json(body): Json<StatusBody>,
) -> Result<impl IntoResponse, ApiError> {
    let status = AttentionStatus::parse(&body.status)
        .ok_or_else(|| ApiError::bad_request("status must be one of: new, open, done"))?;

    let updated = store_call(&state, move |store, account_id| {
        store.set_attention_status(account_id, message_id, status)
    })
    .await?;
    if !updated {
        // Missing or cross-account message.
        return Err(ApiError::not_found());
    }

    // Audited; no body content is recorded.
    audit_action(
        &state,
        "set_status",
        Some(message_id.to_string()),
        status.as_str(),
    )
    .await;

    Ok(Json(
        json!({ "status": status.as_str(), "message_id": message_id }),
    ))
}

// --- POST/DELETE /client/updates/{message_id}/reminder ----------------------

#[derive(Debug, Deserialize)]
pub struct ReminderBody {
    /// When to bring this mail back, RFC3339. Any offset is accepted and
    /// normalized to UTC — the client owns the user's timezone, the store does
    /// not.
    remind_at: String,
}

/// "Remind me about this at T." Also marks the thread done: deferring mail is
/// resolving it until the date arrives, so one round trip both hides it and
/// schedules its return.
pub async fn set_update_reminder(
    State(state): State<ApiState>,
    Path(message_id): Path<i64>,
    Json(body): Json<ReminderBody>,
) -> Result<impl IntoResponse, ApiError> {
    let remind_at = DateTime::parse_from_rfc3339(&body.remind_at)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|_| ApiError::bad_request("remind_at must be an RFC3339 timestamp"))?;
    // A PAST DATE IS A CLIENT BUG, not a reminder that fires immediately: the
    // sweep would fire it on the next tick, so accepting it silently turns a
    // timezone mistake into mail reappearing for no visible reason.
    if remind_at <= Utc::now() {
        return Err(ApiError::bad_request("remind_at must be in the future"));
    }

    let updated = store_call(&state, move |store, account_id| {
        store.set_reminder(account_id, message_id, remind_at)
    })
    .await?;
    if !updated {
        // Missing or cross-account message.
        return Err(ApiError::not_found());
    }

    // Audited; the timestamp is the user's own scheduling decision, not content.
    audit_action(
        &state,
        "set_reminder",
        Some(message_id.to_string()),
        &remind_at.to_rfc3339(),
    )
    .await;

    Ok(Json(
        json!({ "message_id": message_id, "remind_at": remind_at.to_rfc3339() }),
    ))
}

/// Un-schedule a pending reminder. Idempotent: a message with no reminder is a
/// 200, because the caller's intent ("no reminder on this") is already true.
/// Deliberately does NOT reopen the thread — undoing the deferral is what the
/// status route is for.
pub async fn clear_update_reminder(
    State(state): State<ApiState>,
    Path(message_id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let cleared = store_call(&state, move |store, account_id| {
        store.clear_reminder(account_id, message_id)
    })
    .await?;
    if !cleared {
        return Err(ApiError::not_found());
    }

    audit_action(&state, "clear_reminder", Some(message_id.to_string()), "").await;

    Ok(Json(json!({ "message_id": message_id, "cleared": true })))
}

// --- POST /client/refresh ---------------------------------------------------

/// Poke the Gmail sync loop to poll NOW. Fire-and-forget: it does not block on
/// the round trip, and `triggered: false` means no sync loop is wired to this
/// door. A READ-path trigger — no write scope, no mutation, nothing to audit.
pub async fn refresh_now(State(state): State<ApiState>) -> impl IntoResponse {
    let triggered = match &state.refresh {
        Some(notify) => {
            notify.notify_one();
            true
        }
        None => false,
    };
    Json(json!({ "triggered": triggered }))
}

// --- POST /client/spam/refresh -----------------------------------------------

/// ASK THE DAEMON TO GO AND FETCH THE PROVIDER'S SPAM FOLDER.
///
/// The sync loop never walks the SPAM label on its own: spam is the largest and
/// least-read part of a mailbox, and tracking it continuously would spend most
/// of the daemon's fetch budget keeping a page current that nobody has open. So
/// the page asks, and the click pays for it.
///
/// FIRE AND FORGET, like `POST /client/refresh` next door. The fetch is bounded
/// by `sync.spam_max` but still measured in tens of seconds, and holding an HTTP
/// request open for that would trade a slow page for a timed-out one. The client
/// polls `/client/updates?spam=only` as it already does, and reads
/// `stats.spam_synced_at` to tell "still fetching" from "fetched, and the folder
/// is empty" — two states that are the same empty list on the wire.
pub async fn refresh_spam(State(state): State<ApiState>) -> impl IntoResponse {
    let triggered = match &state.spam_refresh {
        Some(notify) => {
            notify.notify_one();
            true
        }
        None => false,
    };
    Json(json!({ "triggered": triggered }))
}

// --- POST /client/retriage (developer tool) ----------------------------------

#[derive(Debug, Deserialize)]
pub struct RetriageBody {
    /// Re-triage just this message. When absent, the trailing-`days` window.
    #[serde(default)]
    message_id: Option<i64>,
    /// Trailing window in days (default 7, clamped 1..=90). Ignored when
    /// `message_id` is set.
    #[serde(default)]
    days: Option<u32>,
}

/// Queue durable investigations for the selected message or recent inbox.
/// Existing decisions and explicit user state remain visible while work runs.
pub async fn retriage(
    State(state): State<ApiState>,
    Json(body): Json<RetriageBody>,
) -> Result<impl IntoResponse, ApiError> {
    let days = body.days.unwrap_or(7).clamp(1, 90);
    let target = body.message_id.map(|id| id.to_string());

    let msg_id = body.message_id;
    let reset = store_call(&state, move |store, account_id| {
        store.retriage_reset(account_id, msg_id, days)
    })
    .await?;

    audit_action(
        &state,
        "retriage",
        target,
        &format!("days={days},reset={reset}"),
    )
    .await;

    // Wake the sync loop so the LLM passes pick the rows up now, not next cycle.
    if let Some(notify) = &state.refresh {
        notify.notify_one();
    }
    Ok(Json(json!({ "reset": reset })))
}

// --- GET /client/retriage (developer tool) -----------------------------------

/// HOW FAR THE LIVE RE-TRIAGE HAS GOT. Read-only, and safe to poll on a short
/// interval — one indexed aggregate, no model call and no mailbox round trip.
///
/// A re-triage takes minutes and the client blocks the app over it, so the
/// counters have to be true or the modal is a liar that cannot be dismissed:
/// `done` is derived from the QUEUE PREDICATES themselves rather than from a
/// marker, which is what keeps an escalated row counted as unfinished until
/// Stage-2 has actually had it.
///
/// `blocked` says why an unfinished run is not moving (Gmail sync failing, a
/// provider cooldown, a spent budget), so the modal can say so instead of
/// sitting on a bar that never fills. Omitted from a door that cannot see sync
/// health, and whenever nothing is holding the run.
pub async fn retriage_progress(
    State(state): State<ApiState>,
) -> Result<impl IntoResponse, ApiError> {
    let mut progress = store_call(&state, move |store, account_id| {
        store.retriage_progress(account_id)
    })
    .await?;
    if let Some(metrics) = state.sync_metrics() {
        let at = |unix: Option<i64>| unix.and_then(|t| DateTime::from_timestamp(t, 0));
        progress.blocked = progress.blocked_by(
            !metrics.sync_failing(),
            at(metrics.sync_next_retry_at()),
            at(metrics.triage_cooldown_until()),
        );
    }
    Ok(Json(progress))
}

// --- GET /client/triage-debug/{message_id} (developer tool) ------------------

/// DEV INSPECTOR: the full triage state of one non-sealed message. Read-only;
/// sealed and unknown are an indistinguishable 404, and no body content is carried.
pub async fn triage_debug(
    State(state): State<ApiState>,
    Path(message_id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let dbg = store_call(&state, move |store, account_id| {
        store.triage_debug(account_id, message_id)
    })
    .await?
    .ok_or(ApiError::not_found())?;
    Ok(Json(dbg))
}

// --- GET /client/thread/{thread_id} -----------------------------------------

/// One message of the served thread: the store's [`ClientMessage`] verbatim,
/// flattened, plus the read-time known-sender bit. Flattened rather than nested
/// so every field a client already decodes keeps its place on the wire.
///
/// Per-message facts ride through the flatten untouched — `is_sent`, the bit the
/// reader right-aligns the user's own bubbles on, among them. THAT one the store
/// computes inside the thread query (stored flag OR `from_addr` == the account's
/// own address; see [`squelch_core::types::ClientMessage`]), which keeps this
/// handler a pure flatten: only `sender_known` is computed here.
#[derive(Debug, Serialize)]
struct ThreadMessageView {
    #[serde(flatten)]
    message: squelch_core::types::ClientMessage,
    /// `true` when this message's `from_addr` is a Sent-derived contact AND the
    /// mail passed email authentication at ingest. The contact half is the SAME
    /// `Store::is_known_contact` signal triage trusts, not a parallel notion.
    /// The reader uses it to decide whether to strip tracking pixels: somebody
    /// the user writes to is allowed to learn the mail was opened.
    ///
    /// The auth half is what stops a stranger from spoofing `From:` into the
    /// bypass — a From header alone is free to write. It comes from the stored
    /// `auth_pass`, so it needs a re-sync to change; the contact half is
    /// computed per request, so a newly-written-to sender takes effect on the
    /// next open with no re-ingest.
    sender_known: bool,
}

/// `ClientThreadView` on the wire, message-for-message, with the extra bit.
#[derive(Debug, Serialize)]
struct ThreadResponse {
    cache_allowed: bool,
    thread_id: String,
    subject: String,
    messages: Vec<ThreadMessageView>,
}

pub async fn get_thread(
    State(state): State<ApiState>,
    Path(thread_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    // Sealed and nonexistent threads are the SAME NotFound. Each message carries
    // its server-sanitized `html`, null for plain-text-only mail.
    //
    // READING A THREAD BUYS NOTHING (docs/TRANSLATION.md): an archive thread's
    // `deferred` translations are served AS `deferred`, with no text, and stay
    // parked. The client prefetches threads through this very GET (search
    // results, list hovers, the sitrep), so promoting here would spend on
    // archive mail nobody opened. The reader that DISPLAYS the thread asks
    // for them over `POST /client/thread/{id}/translate` instead.
    //
    // NO TRANSLATOR, NO PROMISE: without one (translation off, no LLM), a
    // `pending` or `deferred` row would never finish, so it is not served and
    // the reader shows the original alone. A finished translation still is.
    let translate_active = state.translate_active;
    query(&state, move |store, account_id| {
        let mut view = store.thread_view_with_html(account_id, &thread_id)?;
        if !translate_active {
            for message in &mut view.messages {
                if message
                    .translation
                    .as_ref()
                    .is_some_and(|t| t.status == "pending" || t.status == "deferred")
                {
                    message.translation = None;
                }
            }
        }
        // Memoized per distinct sender: a thread is usually two addresses
        // repeated, and each miss is a round trip to `contacts`. The store
        // compares under COLLATE NOCASE, so the memo key is lowercased to match.
        let mut seen: HashMap<String, bool> = HashMap::new();
        let mut messages = Vec::with_capacity(view.messages.len());
        for message in view.messages {
            let known = match seen.get(&message.from_addr.to_ascii_lowercase()) {
                Some(known) => *known,
                None => {
                    let known = store.is_known_contact(account_id, &message.from_addr)?;
                    seen.insert(message.from_addr.to_ascii_lowercase(), known);
                    known
                }
            };
            // AND'd with the ingest-time email-authentication verdict, and
            // gated HERE rather than inside `is_known_contact`: that call also
            // feeds triage's known-contact importance floor at ingest, which
            // must not move. A NULL `auth_pass` — mail never evaluated, or
            // ingested before the column existed — reads as NO bypass, the safe
            // default for a permissive feature, so nothing is backfilled.
            let sender_known = known && message.auth_pass == Some(true);
            messages.push(ThreadMessageView {
                message,
                sender_known,
            });
        }
        Ok(ThreadResponse {
            cache_allowed: store.human_cache_allowed(account_id, &thread_id)?,
            thread_id: view.thread_id,
            subject: view.subject,
            messages,
        })
    })
    .await
}

// --- POST /client/thread/{thread_id}/opened ---------------------------------

/// Record that the user OPENED this thread. See `triage.opened_at`.
///
/// A ROUTE OF ITS OWN, and not a side effect of `GET /client/thread/{id}`,
/// which is where this started and which was wrong in both directions:
///
/// - The client PREFETCHES threads (the sitrep warms every standing item, the
///   inbox warms on hover), so the GET fires for mail nobody has looked at. The
///   updates list solved the same problem with `peek`, and a flag would have
///   fixed this half.
/// - The other half a flag cannot fix: a warmed thread OPENS FROM CACHE and
///   never reaches this daemon at all, so the real opens would be the ones that
///   went unrecorded. That biases the rate DOWN, which is the direction that
///   makes `share_stat` flatter the product, and flattering it with a number is
///   exactly what that whole path exists to avoid.
///
/// So the client says so, once, when a person actually opens something. It is
/// the only party that knows.
///
/// IDEMPOTENT AND CHEAP: first-open-only in SQL, so a reopen writes nothing and
/// the client can fire it without bookkeeping.
pub async fn mark_thread_opened(
    State(state): State<ApiState>,
    Path(thread_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let opened = store_call(&state, move |store, account_id| {
        store.mark_thread_opened(account_id, &thread_id)
    })
    .await?;
    Ok(Json(json!({ "opened": opened })))
}

// --- POST /client/thread/{thread_id}/translate ------------------------------

/// "I am showing this thread: translate it." Promotes the thread's `deferred`
/// (archive-tier) translations to `pending` and wakes the refine lane, so the
/// work starts now rather than at the lane's next interval.
///
/// A ROUTE OF ITS OWN for `mark_thread_opened`'s reason: the GET is also how
/// the client warms threads it has not shown, and archive mail spends nothing
/// unprompted (`messages.is_archive` in schema.sql). Only the reader
/// displaying the thread knows a person is looking at it.
///
/// IDEMPOTENT: only `deferred` rows move, so a second call promotes nothing
/// and pokes nothing. `promoted` says how many moved. Sealed mail may move
/// too and is still never claimed by the pass, so this spends nothing on it.
pub async fn translate_thread(
    State(state): State<ApiState>,
    Path(thread_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    if !state.translate_active {
        // Nothing would ever work a promoted row.
        return Ok(Json(json!({ "promoted": 0 })));
    }
    let poke = state.archive_surfacing.poke.clone();
    let promoted = store_call(&state, move |store, account_id| {
        store.promote_deferred_translations(account_id, &thread_id, Utc::now())
    })
    .await?;
    if promoted > 0
        && let Some(poke) = &poke
    {
        poke.poke();
    }
    Ok(Json(json!({ "promoted": promoted })))
}

// --- GET/POST /client/translate-config ---------------------------------------

/// The "Languages I read" setting: the codes the owner reads, every code they
/// could pick (with its English name), and whether a translator is configured
/// at all, so the card can say so instead of offering a choice that does
/// nothing.
#[derive(Debug, Serialize)]
pub struct TranslateConfigView {
    active: bool,
    read_languages: Vec<String>,
    available: Vec<LanguageChoice>,
}

#[derive(Debug, Serialize)]
pub struct LanguageChoice {
    code: &'static str,
    name: &'static str,
}

fn translate_config_view(active: bool, read_languages: Vec<String>) -> TranslateConfigView {
    TranslateConfigView {
        active,
        read_languages,
        available: squelch_core::translate::Lang::readable_choices()
            .into_iter()
            .map(|l| LanguageChoice {
                code: l.code(),
                name: l.english_name(),
            })
            .collect(),
    }
}

pub async fn get_translate_config(
    State(state): State<ApiState>,
) -> Result<impl IntoResponse, ApiError> {
    let active = state.translate_active;
    let codes = store_call(&state, move |store, account_id| {
        store.read_languages(account_id)
    })
    .await?;
    Ok(Json(translate_config_view(active, codes)))
}

#[derive(Debug, Deserialize)]
pub struct TranslateConfigBody {
    read_languages: Vec<String>,
}

/// Replaces the whole set. Unknown codes and English are dropped, and the
/// response is the set as stored.
pub async fn set_translate_config(
    State(state): State<ApiState>,
    Json(body): Json<TranslateConfigBody>,
) -> Result<impl IntoResponse, ApiError> {
    let active = state.translate_active;
    let codes = store_call(&state, move |store, account_id| {
        store.set_read_languages(account_id, &body.read_languages, Utc::now())
    })
    .await?;
    Ok(Json(translate_config_view(active, codes)))
}

// --- GET /client/attachments/{id} -------------------------------------------

/// Render-safety whitelist for the served `Content-Type`: the stored mime is
/// echoed ONLY for `application/pdf` and non-SVG `image/*`. Everything else
/// serves as `application/octet-stream`, so a hostile attachment can never
/// render inline from our origin. This header discipline IS the security story
/// for the byte endpoint.
fn safe_content_type(mime: &str) -> String {
    // Compare on the bare type, lowercased, ignoring `; charset=...` params.
    let base = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if base == "application/pdf" {
        return base;
    }
    if let Some(sub) = base.strip_prefix("image/")
        && !sub.is_empty()
        // ANY svg/xml-ish image subtype is scriptable: never echo it renderable.
        && !sub.contains("svg")
        && !sub.contains("xml")
    {
        return base;
    }
    "application/octet-stream".to_string()
}

/// Sanitize a filename for `Content-Disposition`: strip path separators,
/// control chars, quotes and non-ASCII, falling back to "attachment".
fn sanitize_attachment_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| c.is_ascii() && !c.is_ascii_control() && *c != '/' && *c != '\\' && *c != '"')
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "attachment".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Serve account-owned attachment bytes to the human. This handler adds safe
/// response headers and reports unstored over-cap attachments with 410.
pub async fn get_attachment(
    State(state): State<ApiState>,
    Path(attachment_id): Path<i64>,
) -> Result<Response, ApiError> {
    // Unknown or cross-account attachment: 404.
    let found = store_call(&state, move |store, account_id| {
        store.attachment_bytes(account_id, attachment_id)
    })
    .await?;
    let (filename, mime, data) = found.ok_or_else(ApiError::not_found)?;
    // Metadata exists but the bytes were never stored (over the ingest cap): 410.
    let bytes =
        data.ok_or_else(|| ApiError::new(StatusCode::GONE, "attachment bytes not stored"))?;
    Ok(attachment_response(
        &filename,
        &mime,
        bytes,
        "private, max-age=3600",
    ))
}

/// The account's own profile photo, as the sync loop last fetched it from
/// Google. 404 when there is none: no profile scope on the grant, or a person
/// who never set a picture.
///
/// Served through [`attachment_response`] like every other byte this door
/// hands out. The type was sniffed from the bytes at fetch time and is one of
/// four image types, but the header discipline costs nothing and means there
/// is one rule for bytes on this door rather than two.
pub async fn get_account_photo(State(state): State<ApiState>) -> Result<Response, ApiError> {
    let photo = store_call(&state, |store, account_id| store.account_photo(account_id))
        .await?
        .ok_or_else(ApiError::not_found)?;
    // REVALIDATE EVERY TIME. The URL is not versioned, so a cache that kept
    // these bytes for a day would hand yesterday's face to a client that had
    // just been told, by the tag on stats, that there is a new one. The tag is
    // the ETag, for any client that wants to ask cheaply.
    let etag = header::HeaderValue::from_str(&format!("\"{}\"", photo.tag)).ok();
    let mut resp = attachment_response(
        "profile-photo",
        &photo.mime,
        photo.bytes,
        "private, no-cache",
    );
    if let Some(etag) = etag {
        resp.headers_mut().insert(header::ETAG, etag);
    }
    Ok(resp)
}

/// One attachment's bytes as a response, under the header discipline that IS
/// the byte door's security story (see [`safe_content_type`]). Shared by the
/// inbound door above and the staged-upload door below: a file the user
/// attached themselves is served with exactly the caution a stranger's gets,
/// because the byte endpoint cannot tell a screenshot from a renamed html.
///
/// `cache` is the door's own policy: the inbound door may let a browser keep
/// a photo for an hour, the staged-upload door is `no-store` like every other
/// read of an unsent composition.
fn attachment_response(
    filename: &str,
    mime: &str,
    bytes: Vec<u8>,
    cache: &'static str,
) -> Response {
    let ctype = safe_content_type(mime);
    let disposition = format!(
        "attachment; filename=\"{}\"",
        sanitize_attachment_filename(filename)
    );

    // Built by hand, not via `Vec<u8>`'s IntoResponse, so we own the
    // Content-Type exactly.
    let mut resp = Response::new(Body::from(bytes));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_str(&ctype)
            .unwrap_or_else(|_| header::HeaderValue::from_static("application/octet-stream")),
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        header::HeaderValue::from_str(&disposition)
            .unwrap_or_else(|_| header::HeaderValue::from_static("attachment")),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static(cache),
    );
    resp
}

// --- /client/compose/attachments --------------------------------------------
//
// FILES ON THEIR WAY OUT. The composer uploads each file the moment it is
// attached, gets an id back, and every later request — the autosave, the send
// — names the file by that id. HUMAN DOOR ONLY, like drafts: what somebody is
// about to send is their own business until it is sent.
//
// Bytes travel as the REQUEST BODY, raw, with the metadata in the query string
// and the `Content-Type` header — not as base64 inside JSON, which would put
// a 25 MB file through a 34 MB JSON parse and a 2 MB default body limit. The
// route carries its own body limit for the same reason.

/// The most one staged file may hold. Gmail refuses a message over 25 MB, and
/// [`MAX_SEND_ATTACHMENT_BYTES`] enforces that total at the send; a single file
/// is capped at the same figure so the upload can refuse a file no send could
/// ever carry rather than store it and refuse later.
pub const MAX_OUTBOUND_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;

/// The most a send's attachments may total, before base64 — Gmail's own
/// message ceiling, and past it the send fails deep in Gmail as an opaque
/// error that blames the wrong side.
const MAX_SEND_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct StageAttachmentQuery {
    filename: String,
    /// The `cid:` token the composer will reference this file by, MINTED BY
    /// THE CLIENT so it can write the reference into the body before the
    /// upload lands. Optional: a client that sends none gets one minted here
    /// and read back off the response. Validated against [`content_id_ok`]
    /// either way — it is written into a MIME header and into html.
    #[serde(default)]
    content_id: Option<String>,
}

/// A `Content-ID` token this daemon will store and later write into a header:
/// the address-ish alphabet, nothing that could close a quote, open a header
/// line or end a `cid:` reference early. Same rule the markdown renderer
/// applies before it lets a `cid:` image through (see `markdown::cid_token`).
pub(crate) fn content_id_ok(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 128
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'@'))
}

/// The filename a staged file is stored — and later sent — under. Path
/// separators and control characters go (the name reaches a
/// `Content-Disposition` header and, on the recipient's side, a disk); everything
/// else stays, unicode included, because the MIME writer speaks RFC 2231 and a
/// `résumé.pdf` should arrive as one. Capped well under the MIME writer's own
/// 80-character truncation so the stored name and the sent name are the same
/// name.
fn clean_upload_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control() && *c != '/' && *c != '\\')
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return "attachment".to_string();
    }
    trimmed.chars().take(80).collect()
}

/// The mime a staged file is stored under: the bare `type/subtype` off the
/// upload's `Content-Type`, lowercased, or `application/octet-stream` when the
/// header is missing or not a media type at all. Parameters are dropped — a
/// `charset` on a text upload would be a claim about bytes this daemon never
/// transcoded, and the MIME writer states its own. A `multipart/*` claim is
/// refused outright: a part declaring itself multipart with no boundary is a
/// malformed message, and the file is served better as a blob.
fn clean_upload_mime(header: Option<&str>) -> String {
    const FALLBACK: &str = "application/octet-stream";
    let Some(raw) = header else {
        return FALLBACK.to_string();
    };
    let base = raw
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let Some((kind, sub)) = base.split_once('/') else {
        return FALLBACK.to_string();
    };
    let token_ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 127
            && s.bytes().all(|b| {
                b.is_ascii_alphanumeric()
                    || matches!(
                        b,
                        b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                    )
            })
    };
    if !token_ok(kind) || !token_ok(sub) || kind == "multipart" {
        return FALLBACK.to_string();
    }
    base
}

/// What the client knows about a staged file: everything but the bytes.
#[derive(Debug, Serialize)]
pub struct OutboundAttachmentView {
    id: i64,
    filename: String,
    mime: String,
    size: i64,
    content_id: String,
}

impl From<OutboundAttachmentMeta> for OutboundAttachmentView {
    fn from(m: OutboundAttachmentMeta) -> Self {
        Self {
            id: m.id,
            filename: m.filename,
            mime: m.mime,
            size: m.size_bytes,
            content_id: m.content_id,
        }
    }
}

/// `POST /client/compose/attachments?filename=…[&content_id=…]` — stage one
/// file for a send. The body is the file. Answers the stored metadata; the
/// `id` is how the draft and the send refer to it from here on.
///
/// A file over [`MAX_OUTBOUND_ATTACHMENT_BYTES`] is a 413 from the body limit
/// on the route, before this runs; the explicit check below is the belt for a
/// caller that arrived some other way.
pub async fn stage_compose_attachment(
    State(state): State<ApiState>,
    Query(q): Query<StageAttachmentQuery>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<impl IntoResponse, ApiError> {
    if body.len() > MAX_OUTBOUND_ATTACHMENT_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "attachment is larger than 25 MB",
        ));
    }
    if body.is_empty() {
        return Err(ApiError::bad_request("attachment is empty"));
    }
    let filename = clean_upload_filename(&q.filename);
    let mut mime = clean_upload_mime(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
    );
    // A `text/*` PART GOES OUT DECLARED `charset="utf-8"` (the MIME writer
    // states the charset of the bytes it is handed, which for a forward's
    // decoded parts is always UTF-8). An upload's bytes are whatever the file
    // held — a Windows-1252 `notes.txt`, a UTF-16 export — and stamping UTF-8
    // on those would render as mojibake at the other end. A text file whose
    // bytes are not UTF-8 is therefore stored as a blob: it arrives intact,
    // and the recipient's client works out the encoding the way it would for
    // any download.
    if mime.starts_with("text/") && std::str::from_utf8(&body).is_err() {
        mime = "application/octet-stream".to_string();
    }
    let content_id = match q.content_id.as_deref().map(str::trim) {
        Some(token) if content_id_ok(token) => token.to_string(),
        Some(_) => return Err(ApiError::bad_request("content_id must be a plain token")),
        None => match squelch_core::tracking::mint_token() {
            Ok(t) => format!("{t}@passband"),
            Err(_) => {
                return Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "could not mint a content id",
                ));
            }
        },
    };
    let data = body.to_vec();
    let meta = store_call(&state, move |store, account_id| {
        store.stage_outbound_attachment(
            account_id,
            &filename,
            &mime,
            &content_id,
            &data,
            Utc::now(),
        )
    })
    .await?;
    Ok((no_store(), Json(OutboundAttachmentView::from(meta))))
}

/// `GET /client/compose/attachments/{id}` — one staged file's bytes, for the
/// composer to draw a thumbnail of a file it did not upload in this session
/// (a restored draft). Same headers as the inbound byte door.
pub async fn get_compose_attachment(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    let found = store_call(&state, move |store, account_id| {
        store.outbound_attachment(account_id, id)
    })
    .await?;
    let att = found.ok_or_else(ApiError::not_found)?;
    Ok(attachment_response(
        &att.meta.filename,
        &att.meta.mime,
        att.data,
        "no-store",
    ))
}

/// `DELETE /client/compose/attachments/{id}` — a file removed from the tray.
/// Another account's id and an unknown id are the same 404, and so is a file a
/// scheduled send holds: only cancelling it releases that file (see
/// `delete_outbound_attachment`).
pub async fn delete_compose_attachment(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let deleted = store_call(&state, move |store, account_id| {
        store.delete_outbound_attachment(account_id, id)
    })
    .await?;
    if deleted {
        Ok((no_store(), Json(json!({ "status": "deleted", "id": id }))))
    } else {
        Err(ApiError::not_found())
    }
}

/// The files a send names, loaded for composing. EVERY id must resolve: a
/// staged file that is gone (swept, deleted, another account's) refuses the
/// whole send rather than going out without it, because the sender reviewed a
/// tray and the mail must carry what the tray showed. Duplicated ids collapse
/// to one file — one row is one part.
///
/// ANOTHER DRAFT'S FILE IS "GONE" TOO: a send may carry only files that are
/// unclaimed or claimed by `draft_id`, the draft it consumes. The claim path
/// already refuses to steal a claimed row; without the same rule here a send
/// naming a foreign id would carry that file AND consume it on success,
/// leaving the other draft pointing at nothing.
///
/// `action` is the audit action a refusal is written under: `send` for a
/// send, `schedule` when the same files are checked at schedule time.
pub(crate) async fn load_send_attachments(
    state: &ApiState,
    action: &'static str,
    ids: &[i64],
    draft_id: Option<i64>,
    target: Option<String>,
) -> Result<Vec<MailAttachment>, ApiError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let wanted = dedup_ids(ids);
    let count = wanted.len();
    // METADATA FIRST. Every file passes the per-file cap on its own, so ten
    // of them could be 250 MB — pulled through the store mutex just to be
    // refused. The sizes say everything the refusals need to know.
    let sizes = {
        let wanted = wanted.clone();
        store_call(state, move |store, account_id| {
            store.outbound_attachment_sizes(account_id, draft_id, &wanted)
        })
        .await?
    };
    if sizes.len() != count {
        audit_action(state, action, target, "rejected:attachment_missing").await;
        return Err(ApiError::bad_request(
            "an attached file is no longer staged; remove it and attach it again",
        ));
    }
    let total: i64 = sizes.iter().map(|(_, size, _)| size).sum();
    if total > MAX_SEND_ATTACHMENT_BYTES as i64 {
        audit_action(state, action, target, "rejected:too_large").await;
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "attachments total more than 25 MB",
        ));
    }
    // TWO PARTS WITH ONE CONTENT-ID would leave the html's reference pointing
    // at whichever the recipient's client picks. The token is client-minted,
    // so a collision is a client bug — refused here, where it costs nothing,
    // rather than shipped as a coin flip.
    let mut cids: Vec<&str> = sizes.iter().map(|(_, _, cid)| cid.as_str()).collect();
    cids.sort_unstable();
    if cids.windows(2).any(|w| w[0] == w[1]) {
        audit_action(state, action, target, "rejected:attachment_cid_clash").await;
        return Err(ApiError::bad_request(
            "two attached files share a content id; remove one and attach it again",
        ));
    }
    let rows = store_call(state, move |store, account_id| {
        store.outbound_attachments(account_id, draft_id, &wanted)
    })
    .await?;
    if rows.len() != count {
        audit_action(state, action, target, "rejected:attachment_missing").await;
        return Err(ApiError::bad_request(
            "an attached file is no longer staged; remove it and attach it again",
        ));
    }
    Ok(rows.into_iter().map(mail_attachment).collect())
}

/// A send's `attachment_ids` in tray order with repeats dropped: one row is
/// one part, however many times the tray named it.
pub(crate) fn dedup_ids(ids: &[i64]) -> Vec<i64> {
    let mut wanted: Vec<i64> = Vec::with_capacity(ids.len());
    for id in ids {
        if !wanted.contains(id) {
            wanted.push(*id);
        }
    }
    wanted
}

/// One staged file as a part of the outgoing message.
pub(crate) fn mail_attachment(a: squelch_core::store::OutboundAttachment) -> MailAttachment {
    MailAttachment {
        filename: a.meta.filename,
        mime: a.meta.mime,
        content_id: Some(a.meta.content_id),
        // Decided later, against the rendered html — see `mark_inline`.
        is_inline: false,
        method: None,
        data: a.data,
    }
}

/// The guard kinds found in the TEXT of a set of attachments. A PEM key
/// pasted into `notes.txt` and dragged onto a mail is the same exfil shape as
/// one pasted into the body — arguably the more natural one — so every
/// `text/*` part is scanned as text, and so is every `message/*` part (an
/// attached email is plain RFC822 text carrying whatever its bodies carried).
/// BINARY PARTS STAY UNSCANNED (a key inside a zip passes): the guard is a
/// seatbelt against the accident, not a DLP boundary, and it is overridable by
/// design.
fn attachment_text_kinds(attachments: &[MailAttachment]) -> Vec<&'static str> {
    let mut kinds = Vec::new();
    for att in attachments {
        let mime = att.mime.to_ascii_lowercase();
        if !(mime.starts_with("text/") || mime.starts_with("message/")) {
            continue;
        }
        let text = String::from_utf8_lossy(&att.data);
        for kind in guard::scan_kinds(&text) {
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
    }
    kinds
}

/// The guard kinds a composition carries in what the sender put in it: the
/// typed body and the text of every staged file, deduped. A forward's
/// original is judged separately ([`forward_original_kinds`]) and unioned in.
pub(crate) fn composition_kinds(body: &str, attachments: &[MailAttachment]) -> Vec<&'static str> {
    let mut kinds = guard::scan_kinds(body);
    for kind in attachment_text_kinds(attachments) {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}

/// The staged files a send CONSUMED, dropped once the mail is away — the same
/// contract as `discard_sent_draft`, and the same best-effort posture: the
/// mail has left, so a failed cleanup is logged and never fails the request.
async fn discard_sent_attachments(state: &ApiState, draft_id: Option<i64>, ids: Vec<i64>) {
    if ids.is_empty() {
        return;
    }
    if store_call(state, move |store, account_id| {
        store.delete_outbound_attachments(account_id, draft_id, &ids)
    })
    .await
    .is_err()
    {
        eprintln!("squelch-api: staged attachments outlived their send; discard failed");
    }
}

// --- GET /client/search -----------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    /// Human search's status grouping; omitted by agent callers. Older apps
    /// also lean on it to bring the owner's sent mail into keyword mode, so it
    /// keeps implying `include_sent`.
    #[serde(default)]
    unfinished_first: bool,
    /// Human search's corpus, independent of order: the owner's own sent mail
    /// alongside inbound, as hybrid/semantic already search. A client that
    /// wants plain ranked order (`unfinished_first` off) sends this to keep
    /// the corpus `unfinished_first=true` gave it. Advertised as
    /// `search_plain_order` on `/client/stats`. It widens nothing beyond the
    /// owner's own mail: sealed, spam and every readable filter apply as for
    /// any other search.
    #[serde(default)]
    include_sent: bool,
    /// The raw query, operators included (`invoice from:jane after:2026-01-01`).
    /// Parsed exactly once, here, then threaded into the store as
    /// `(text, filter)`.
    q: String,
    limit: Option<u32>,
    cursor: Option<String>,
    /// Retrieval mode: keyword|semantic|hybrid. Omitted => hybrid when a vector
    /// index is available (an embedder is attached), else keyword.
    mode: Option<String>,
    /// Result order: recent|best_match. Omitted => recent, which is the ranking
    /// with recency blended in. This is the READER'S standing preference, held
    /// by the client and sent on every search, not something parsed out of `q`.
    sort: Option<String>,
    /// `1`/`true`: match the LAST word of the query as a prefix, because the
    /// reader is still typing it. The panel's debounced as-you-type fetch sends
    /// it; a settled query (the agent's, or a submitted one) does not, and
    /// should not — an agent sending finished words would get `password`
    /// widened to `password*`.
    partial: Option<String>,
}

/// Parse the `partial` flag. Unknown values 400 for the same reason `sort` and
/// `mode` do: a client that sends `partial=yes` and silently gets exact
/// matching has a bug it cannot see from the response.
fn parse_partial(raw: Option<&str>) -> Result<bool, ApiError> {
    match raw {
        None => Ok(false),
        Some("1") | Some("true") => Ok(true),
        Some("0") | Some("false") => Ok(false),
        Some(_) => Err(ApiError::bad_request(
            "partial must be one of: 1, 0, true, false",
        )),
    }
}

/// The three retrieval modes for `/client/search`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchMode {
    Keyword,
    Semantic,
    Hybrid,
}

impl SearchMode {
    fn parse(s: &str) -> Option<SearchMode> {
        match s {
            "keyword" => Some(SearchMode::Keyword),
            "semantic" => Some(SearchMode::Semantic),
            "hybrid" => Some(SearchMode::Hybrid),
            _ => None,
        }
    }

    /// The `match_kind` label echoed on the response.
    fn as_str(self) -> &'static str {
        match self {
            SearchMode::Keyword => "keyword",
            SearchMode::Semantic => "semantic",
            SearchMode::Hybrid => "hybrid",
        }
    }
}

/// Search response envelope: a page of hits plus the mode and order actually
/// run. Both are echoed rather than assumed: a mode can DEGRADE (semantic
/// without a vector index falls back to keyword), and a client that cannot see
/// which order it got cannot tell a bad ranking from a bad request.
#[derive(Debug, Serialize)]
struct SearchPage<T> {
    items: Vec<T>,
    match_kind: &'static str,
    sort: &'static str,
    /// ALWAYS PRESENT, even for a query that looked nothing up (then it is
    /// zeroes and an empty term list). A field that appears only sometimes is a
    /// field every client has to write two code paths for.
    diagnostics: Diagnostics,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

/// WHAT RETRIEVAL MADE OF THE READER'S WORDS, reported beside the hits.
///
/// A search box shows a list and nothing else, so a reader cannot tell "no mail
/// says this" from "no mail says all of this at once" — and those want opposite
/// next moves. These are the facts that separate them, and they are what the
/// deeper-search classifier (docs/SEARCH.md §5) decides on: three or more terms
/// with `strict_hits == 0` is the shape of a question rather than a lookup.
///
/// Every count is account-scoped and blind to sealed and spam mail; see
/// `SqliteStore::search_diagnostics` for why that is a security property and
/// not tidiness. Every count also STOPS at `DIAGNOSTIC_COUNT_CAP`, so a count
/// equal to it means "at least that many": these run on every keystroke of an
/// as-you-type search, and nothing that reads them needs an exact frequency.
#[derive(Debug, Serialize)]
struct Diagnostics {
    /// Messages matching EVERY term.
    strict_hits: u32,
    /// Messages matching AT LEAST ONE term.
    any_hits: u32,
    /// Each term as it was ranked, with how many messages contain it.
    terms: Vec<TermStat>,
}

#[derive(Debug, Serialize)]
struct TermStat {
    text: String,
    df: u32,
}

impl From<SearchDiagnostics> for Diagnostics {
    fn from(d: SearchDiagnostics) -> Self {
        Diagnostics {
            strict_hits: d.strict_hits,
            any_hits: d.any_hits,
            terms: d
                .terms
                .into_iter()
                .map(|t| TermStat {
                    text: t.text,
                    df: t.df,
                })
                .collect(),
        }
    }
}

/// One hit plus WHICH LEG produced it. The hit's own fields are flattened, so
/// this is the same object the client already decodes with one array added
/// beside them: `["keyword"]`, `["vector"]`, or both when both legs agreed.
///
/// IT IS RETRIEVAL PROVENANCE, so it can be EMPTY. A query of operators alone
/// (`from:jane`) runs no MATCH and no KNN — it is a listing, ordered by date,
/// and no leg retrieved anything. Claiming the keyword leg there would tell a
/// client that bm25 ranked these rows and that a term in the diagnostics found
/// them, and both are false.
#[derive(Debug, Serialize)]
struct SearchItem {
    #[serde(flatten)]
    hit: squelch_core::types::SearchHit,
    legs: Vec<&'static str>,
}

/// Recall grows on demand. Remember delivered IDs because expanding RRF's
/// candidate pool can change ranks; an offset alone would repeat or skip mail.
#[derive(Debug, Default, Serialize, Deserialize)]
struct SearchResume {
    seen: Vec<i64>,
    k: usize,
    done: bool,
}

impl SearchResume {
    fn decode(raw: &str) -> Result<Self, ApiError> {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        if raw.len() > 16_384 {
            return Err(ApiError::bad_request("invalid search cursor"));
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(raw)
            .map_err(|_| ApiError::bad_request("invalid search cursor"))?;
        let state: Self = serde_json::from_slice(&bytes)
            .map_err(|_| ApiError::bad_request("invalid search cursor"))?;
        if state.seen.len() > 600 || state.k > 600 {
            return Err(ApiError::bad_request("invalid search cursor"));
        }
        Ok(state)
    }
    fn encode(&self) -> String {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        format!(
            "recall:{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).expect("search cursor"))
        )
    }
}

/// The recall window semantic/hybrid rank before the page is cut out of it.
///
/// With no filter it is exactly the page. With one, the filter is applied
/// POST-HOC to hydrated hits (KNN and RRF cannot carry a `from:`/date
/// predicate), so we over-fetch to leave room for the rows it drops.
/// APPROXIMATION: a heavily-filtered query can still under-fill a page — the
/// matching mail may sit below the window entirely.
///
/// `saturating_add` because the offset comes off the wire (a crafted cursor
/// must not overflow), and BOTH branches cap: past `MAX_RECALL_K` the fused
/// ranking is noise anyway, and an uncapped `k` would hand sqlite-vec a
/// KNN the size of the cursor.
fn recall_k(limit: u32, offset: u32, filter: &SearchFilter) -> usize {
    const MAX_RECALL_K: usize = 600;
    let page = (limit as usize).saturating_add(offset as usize);
    if filter.is_empty() {
        page.min(MAX_RECALL_K)
    } else {
        page.saturating_mul(3).min(MAX_RECALL_K)
    }
}

pub async fn search(
    State(state): State<ApiState>,
    Query(query): Query<SearchQuery>,
) -> Result<impl IntoResponse, ApiError> {
    if query.q.trim().is_empty() {
        return Err(ApiError::bad_request("q must not be empty"));
    }
    // ONE parse for every leg below: operators come out as a `SearchFilter`,
    // what is left is the text to rank on.
    let (term, filter) = squelch_core::store::parse_search_query(query.q.trim());
    let term = term.trim().to_string();
    if term.is_empty() && filter.is_empty() {
        return Err(ApiError::bad_request("q must not be empty"));
    }
    let resume = query
        .cursor
        .as_deref()
        .and_then(|c| c.strip_prefix("recall:"))
        .map(SearchResume::decode)
        .transpose()?;
    let (limit, offset) = paginate(
        query.limit,
        if resume.is_some() {
            None
        } else {
            query.cursor.as_deref()
        },
    )?;

    // Unknown values 400 rather than falling back to the default: a client that
    // sends `sort=newest` and silently gets `recent` has a bug it cannot see.
    let sort = match query.sort.as_deref() {
        Some(s) => SearchSort::parse(s)
            .ok_or_else(|| ApiError::bad_request("sort must be one of: recent, best_match"))?,
        None => SearchSort::default(),
    };

    // An attached embedder means vectors are being written; that gates
    // semantic/hybrid and drives the default mode.
    let have_vectors = state.store.embedder().is_some();

    let mode = match query.mode.as_deref() {
        Some(s) => SearchMode::parse(s).ok_or_else(|| {
            ApiError::bad_request("mode must be one of: keyword, semantic, hybrid")
        })?,
        None => {
            if have_vectors {
                SearchMode::Hybrid
            } else {
                SearchMode::Keyword
            }
        }
    };

    // Semantic/hybrid asked for without a vector index degrade to keyword rather
    // than erroring — but the response reports the kind actually run. A query
    // that is nothing BUT operators has no text to rank, so every mode collapses
    // to the keyword leg's filter-only listing.
    let effective = if term.is_empty() {
        SearchMode::Keyword
    } else {
        match mode {
            SearchMode::Semantic | SearchMode::Hybrid if !have_vectors => SearchMode::Keyword,
            other => other,
        }
    };

    let unfinished_first = query.unfinished_first;
    let include_sent = query.include_sent;
    let grouped_recall = unfinished_first && effective != SearchMode::Keyword;
    // RECALL PAGES BY WHAT IT HAS SERVED, not by offset, for the app: both the
    // grouped order and plain order (`include_sent` alone). The candidate pool
    // grows with the offset, and a fused ranking over a bigger pool is not the
    // same ranking, so an offset into it repeats and skips hits. The cursor
    // carries the served ids and the pool size instead. Callers that send
    // neither flag keep offset paging exactly as before.
    let recall_paging = (unfinished_first || include_sent) && effective != SearchMode::Keyword;
    if resume.is_some() && !recall_paging {
        return Err(ApiError::bad_request(
            "recall cursor requires unfinished_first or include_sent",
        ));
    }
    let k = recall_k(limit, offset, &filter);
    let partial = parse_partial(query.partial.as_deref())?;

    // PUNCTUATION IS NOT AN INVITATION TO LIST THE MAILBOX, in any mode.
    //
    // `q=***` survives every check above: it is non-empty, it parses to no
    // operators, and it leaves a non-empty `term`. The builder then makes
    // nothing of it, which the keyword leg reads as an honest empty result —
    // but the recall legs do not consult the builder at all. They would embed
    // "***" (BGE instruction and all), KNN it, and serve a page of essentially
    // arbitrary mail beside a diagnostics object reporting that nothing was
    // looked up. That self-contradiction is the bug; the empty page is the
    // answer, and it belongs HERE, ahead of the mode dispatch, so all three
    // legs give it.
    //
    // With a filter it is a different question ("*** from:alice" still means
    // alice's mail), and that keeps falling through to the filter-only listing.
    //
    // The same builder answers a second question below: whether anything was
    // RETRIEVED at all, or whether this is a listing wearing a search's clothes.
    let ranks_on_text = !FtsQuery::build(&term, partial).is_empty();
    if filter.is_empty() && !ranks_on_text {
        return Ok(Json(SearchPage::<SearchItem> {
            items: Vec::new(),
            match_kind: effective.as_str(),
            sort: sort.as_str(),
            diagnostics: SearchDiagnostics::default().into(),
            next_cursor: None,
        }));
    }

    // Human retrieval and diagnostics share the corpus policy below. A keyword
    // page holds one lock across both status groups; diagnostics run afterward
    // and may reflect newer mailbox state. Recall uses a bounded candidate pool.
    let queued_at = std::time::Instant::now();
    let (items, window_full, diagnostics, recall_next) = store_call(&state, move |store, account_id| {
        let started = std::time::Instant::now();
        let queue_ms = queued_at.elapsed().as_millis();
        let mut filter = filter;
        filter.include_sent =
            effective != SearchMode::Keyword || unfinished_first || include_sent;
        filter.unfinished_first = unfinished_first;
        let mut resume = resume.unwrap_or_default();
        if recall_paging {
            filter.exclude_ids = resume.seen.clone();
            if grouped_recall && resume.done { filter.done = Some(true); }
        }
        let mut current_k = k.max(resume.k).max(resume.seen.len().saturating_add(limit as usize)).min(600);
        // The strict count worth sharing, and only when it answers the same
        // question the diagnostics ask: no operator predicates. With a `from:`
        // or a date bound the page counted a narrower set, and the two deserve
        // their own counts.
        let page_limit = if recall_paging { limit.min(600 - resume.seen.len() as u32) } else { limit };
        let mut counted_strict = None;
        let (items, window_full): (Vec<SearchItem>, bool) = loop {
            let (items, window_full): (Vec<SearchItem>, bool) = match effective {
                SearchMode::Keyword => {
                    let (hits, strict) = if unfinished_first {
                        (
                            store.search_unfinished_first(
                                account_id, &term, &filter, sort, partial, limit, offset,
                            )?,
                            None,
                        )
                    } else {
                        store.search_filtered_counted(
                            account_id, &term, &filter, sort, partial, limit, offset,
                        )?
                    };
                    if filter.is_empty() {
                        counted_strict = strict;
                    }
                    // A filter-only listing retrieved nothing: no MATCH ran, no
                    // ranking happened, and the rows are simply this account's
                    // newest mail. `legs` is provenance, so it is empty.
                    let legs: Vec<&'static str> = if ranks_on_text {
                        vec!["keyword"]
                    } else {
                        Vec::new()
                    };
                    (
                        hits.into_iter()
                            .map(|hit| SearchItem {
                                hit,
                                legs: legs.clone(),
                            })
                            .collect(),
                        false,
                    )
                }
                SearchMode::Semantic => {
                    let (mut hits, window_full) =
                        store.semantic_search_hits(account_id, &term, &filter, sort, partial, current_k)?;
                    let page: Vec<SearchItem> = hits
                        .drain(..)
                        .skip(offset as usize)
                        .take(page_limit as usize)
                        .map(|hit| SearchItem {
                            hit,
                            legs: vec!["vector"],
                        })
                        .collect();
                    (page, window_full)
                }
                SearchMode::Hybrid => {
                    // The range is counted after filtering, exactly like the page
                    // below. Only rows the client receives need match snippets.
                    let start = offset as usize;
                    let (mut hits, window_full) = store.hybrid_search_legs_ordered(
                        account_id,
                        &term,
                        &filter,
                        sort,
                        partial,
                        start..start.saturating_add(page_limit as usize),
                        current_k,
                    )?;
                    let page: Vec<SearchItem> = hits
                        .drain(..)
                        .skip(offset as usize)
                        .take(page_limit as usize)
                        .map(|h| {
                            let mut legs = Vec::with_capacity(2);
                            if h.keyword {
                                legs.push("keyword");
                            }
                            if h.vector {
                                legs.push("vector");
                            }
                            SearchItem { hit: h.hit, legs }
                        })
                        .collect();
                    (page, window_full)
                }
            };
            // A first page containing enough unfinished hits needs no larger pool.
            // Before crossing into done mail, exhaust the bounded recall window.
            let unfinished = items.iter().filter(|item| !item.hit.is_done).count();
            if recall_paging && window_full && current_k < 600
                && (items.len() < limit as usize
                    || (grouped_recall && !resume.done && unfinished < limit as usize)) {
                current_k = (current_k * 2).max(current_k + limit as usize).min(600);
                continue;
            }
            break (items, window_full);
        };
        let recall_next = if recall_paging && !items.is_empty() {
            resume.k = current_k;
            resume.done |= grouped_recall && items.iter().any(|item| item.hit.is_done);
            resume.seen.extend(items.iter().map(|item| item.hit.id));
            if resume.seen.len() < 600 && (items.len() == limit as usize || window_full) {
                Some(resume.encode())
            } else { None }
        } else { None };
        let retrieval_ms = started.elapsed().as_millis();
        let diagnostics_started = std::time::Instant::now();
        let diagnostics = store.search_diagnostics_with(
            account_id,
            &term,
            partial,
            filter.searches_sent(),
            counted_strict,
        )?;
        if queued_at.elapsed().as_millis() >= 250 {
            eprintln!("squelch: slow search mode={} queue_ms={queue_ms} retrieval_ms={retrieval_ms} diagnostics_ms={} total_ms={} items={}",
                effective.as_str(), diagnostics_started.elapsed().as_millis(),
                queued_at.elapsed().as_millis(), items.len());
        }
        Ok((items, window_full, diagnostics, recall_next))
    })
    .await?;

    // A full page always advances by `limit`. A SHORT page normally means the
    // results are exhausted — except on a filtered recall leg, where the filter
    // may have thinned a FULL candidate window: there the walk continues,
    // advancing by what was actually served (the offset indexes the filtered
    // sequence, and ranking is deterministic). A short page that served
    // NOTHING ends the walk even window-full: the same offset would rebuild
    // the same window and hand the client the same empty page forever.
    let next = if recall_paging {
        recall_next
    } else {
        match next_cursor(items.len(), limit, offset) {
            Some(cursor) => Some(cursor),
            None if window_full && !items.is_empty() => {
                Some(cursor::encode_offset(offset + items.len() as u32))
            }
            None => None,
        }
    };
    Ok(Json(SearchPage {
        items,
        match_kind: effective.as_str(),
        sort: sort.as_str(),
        diagnostics: diagnostics.into(),
        next_cursor: next,
    }))
}

// --- GET /client/shipments --------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ShipmentsQuery {
    /// Include delivered shipments too. Default false => en-route only.
    #[serde(default)]
    include_delivered: bool,
}

pub async fn get_shipments(
    State(state): State<ApiState>,
    Query(q): Query<ShipmentsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    // The shipments table holds no sealed rows: detection never runs on sealed
    // mail, and hand-sealing a message deletes the shipment row it fed
    // (correct_triage), so there is no sealed filtering to apply.
    //
    // The operator's `[carriers]` policy decides the rest: rows that have been
    // silent for `stale_after_days` with no carrier vouching for them, and rows
    // the user cleared. Both are READ-SIDE hides — the rows stay in the store,
    // keep being polled, and come back on their own the moment an update lands.
    let policy = state.shipment_policy;
    query(&state, move |store, account_id| {
        store.list_shipments(account_id, q.include_delivered, policy)
    })
    .await
}

// --- POST /client/shipments/{id}/clear ---------------------------------------

/// "Stop showing me this package." HUMAN DOOR ONLY: writes never go on the agent
/// door, and this is a statement about what the USER wants to see.
///
/// A HIDE, NOT A DELETE, and there is no un-clear endpoint on purpose. The row
/// stays in the store, keeps being polled, and reappears by itself as soon as
/// anything advances its `last_update` past the clear stamp — a carrier poll that
/// moves it, or a new email about it. See [`Store::clear_shipment`].
///
/// Idempotent: clearing an already-cleared row RESTAMPS it (a row revived by an
/// update and cleared again must hide against the later stamp), so a client that
/// double-taps gets the same 200.
///
/// An unknown id is a 404, matching [`set_update_status`]: a path-addressed write
/// against a row this account does not have is not a success with a `false` in it.
pub async fn clear_shipment(
    State(state): State<ApiState>,
    Path(shipment_id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let at = Utc::now();
    // The listing's own policy, so the card cleared is the card the user saw.
    let policy = state.shipment_policy;
    let cleared = store_call(&state, move |store, account_id| {
        store.clear_shipment(account_id, shipment_id, at, policy)
    })
    .await?;
    if !cleared {
        return Err(ApiError::not_found());
    }

    audit_action(
        &state,
        "shipment.clear",
        Some(shipment_id.to_string()),
        "ok",
    )
    .await;

    Ok(Json(
        json!({ "cleared": true, "shipment_id": shipment_id, "cleared_at": at }),
    ))
}

// --- POST /client/shipments/poll --------------------------------------------

/// Poke the carrier poller to run a pass NOW, the shipments-tracker sibling of
/// [`refresh_now`]. Fire-and-forget: it does not wait for the carrier round
/// trips, and the kick does NOT bypass per-carrier cooldowns, budgets or
/// `min_interval` — a user mashing refresh cannot spend a daily cap or earn a
/// 429.
///
/// `kicked: false` with an empty `carriers` is a 200 and a NORMAL answer, not an
/// error: carrier polling is BYOK, so a daemon with no carrier credentials runs
/// no poller at all. The client reads the same two fields either way and can
/// hide the button when the list is empty.
///
/// A READ-path trigger, like `refresh` — no write scope, no mutation, nothing to
/// audit.
pub async fn poll_shipments_now(State(state): State<ApiState>) -> impl IntoResponse {
    match &state.shipment_poll {
        Some(poll) => {
            poll.kick.notify_one();
            Json(json!({ "kicked": true, "carriers": &poll.carriers[..] }))
        }
        None => Json(json!({ "kicked": false, "carriers": [] })),
    }
}

// --- GET /client/receipts ---------------------------------------------------

/// Default look-back window for the receipts list.
const DEFAULT_RECEIPTS_DAYS: u32 = 30;

#[derive(Debug, Deserialize)]
pub struct ReceiptsQuery {
    /// Look-back window in days. Default 30.
    days: Option<u32>,
}

pub async fn get_receipts(
    State(state): State<ApiState>,
    Query(q): Query<ReceiptsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let days = q.days.unwrap_or(DEFAULT_RECEIPTS_DAYS);
    // Newest-first. Structurally sealed-free, like shipments.
    query(&state, move |store, account_id| {
        store.list_receipts(account_id, days)
    })
    .await
}

// --- GET /client/banking -----------------------------------------------------

/// SERIALIZED SHAPE IS A WIRE CONTRACT — the desktop Banking zone decodes
/// exactly [`squelch_core::types::Banking`], newest-received first, with every
/// extracted field nullable and `account_hint` only ever a masked last-4 tail.
/// Sealed mail can never produce a banking row (structural, like receipts).
pub async fn get_banking(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    query(&state, |store, account_id| store.list_banking(account_id)).await
}

// --- GET /client/marketing ---------------------------------------------------

/// Look-back window and clamp bounds. Promos age out fast; a fortnight is
/// already generous for "what is on offer".
const DEFAULT_MARKETING_DAYS: u32 = 14;
const MAX_MARKETING_DAYS: u32 = 90;
const MARKETING_LIMIT: u32 = 200;

#[derive(Debug, Deserialize)]
pub struct MarketingQuery {
    #[serde(default)]
    days: Option<u32>,
}

/// Extracted promotions, newest first. Structurally sealed-free: only an
/// extractor writes this table, and sealed mail never reaches one.
pub async fn get_marketing(
    State(state): State<ApiState>,
    Query(q): Query<MarketingQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let days = q
        .days
        .unwrap_or(DEFAULT_MARKETING_DAYS)
        .clamp(1, MAX_MARKETING_DAYS);
    query(&state, move |store, account_id| {
        store.marketing_offers(account_id, days, MARKETING_LIMIT)
    })
    .await
}

// --- GET /client/calendar ----------------------------------------------------

/// Look-back window in hours of MAIL ARRIVAL, not event start, plus its clamp
/// bounds.
const DEFAULT_CALENDAR_HOURS: u32 = 24;
const MIN_CALENDAR_HOURS: u32 = 1;
const MAX_CALENDAR_HOURS: u32 = 168; // one week

#[derive(Debug, Deserialize)]
pub struct CalendarQuery {
    /// Window in hours over `received_at`. Out-of-range values clamp, not 400.
    hours: Option<u32>,
}

/// SERIALIZED SHAPE IS A WIRE CONTRACT — the desktop sidebar decodes exactly
/// [`squelch_core::types::CalendarUpdate`], newest-received first, with every
/// extracted field nullable and `thread_id` the joined message's thread. Sealed
/// mail can never produce a calendar row (structural, like receipts/banking).
pub async fn get_calendar(
    State(state): State<ApiState>,
    Query(q): Query<CalendarQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let hours = q
        .hours
        .unwrap_or(DEFAULT_CALENDAR_HOURS)
        .clamp(MIN_CALENDAR_HOURS, MAX_CALENDAR_HOURS);
    query(&state, move |store, account_id| {
        store.list_calendar_updates(account_id, hours)
    })
    .await
}

// --- GET /client/timeline, PATCH /client/timeline/ghosts/{id} ---------------

/// The default window around now, and the widest one a caller may ask for.
/// A wider range clamps (its end moves in), like every other out-of-range
/// knob on this door; it is not a 400.
const TIMELINE_DEFAULT_BEHIND: chrono::Duration = chrono::Duration::days(1);
const TIMELINE_DEFAULT_AHEAD: chrono::Duration = chrono::Duration::days(90);
const TIMELINE_MAX_SPAN: chrono::Duration = chrono::Duration::days(400);

#[derive(Debug, Deserialize)]
pub struct TimelineQuery {
    /// RFC3339, with any `+` in the offset percent-encoded (a bare `+` in a
    /// query string decodes to a space). Taken as text so a bad value is this
    /// door's own 400 rather than the extractor's.
    from: Option<String>,
    to: Option<String>,
}

fn timeline_bound(raw: Option<&str>) -> Result<Option<DateTime<chrono::FixedOffset>>, ApiError> {
    raw.map(|raw| {
        DateTime::parse_from_rfc3339(raw.trim())
            .map_err(|_| ApiError::bad_request("from and to must be RFC3339 timestamps"))
    })
    .transpose()
}

/// One ghost as served. SERIALIZED SHAPE IS A WIRE CONTRACT for the timeline
/// client.
///
/// AN ESTIMATE IS NEVER SERVED AS A FACT: `end` is the resolved end and
/// `end_source` / `end_estimated` say which tier produced it, while `estimate`
/// is the model's own guess as stored, served even when a stronger tier wins
/// so the client can say what it was overruled by. A date-only ghost with no
/// stated or corrected end has no end at all (`end`, `end_source` null).
fn ghost_view(
    ghost: &squelch_core::types::GhostEvent,
    now: DateTime<chrono::FixedOffset>,
) -> serde_json::Value {
    let end = ghost.effective_end();
    json!({
        "id": ghost.id,
        "kind": ghost.kind,
        "title": ghost.title,
        "start": ghost.start.to_string(),
        "start_timezone": ghost.start_timezone,
        "all_day": ghost.all_day(),
        "end": end.as_ref().map(|e| e.end.to_string()),
        "end_estimated": end.as_ref().is_some_and(|e| e.estimated),
        "end_source": end.as_ref().map(|e| e.source),
        // Laundering blanks a `basis` or `reason` it will not show; an empty
        // string is served as null so a client has one "no text" to handle.
        "estimate": ghost.estimate.as_ref().map(|e| json!({
            "minutes": e.minutes,
            "basis": (!e.basis.is_empty()).then_some(&e.basis),
        })),
        "location": ghost.location,
        "commitment": ghost.commitment,
        "state": ghost.state(now),
        // What the mail says, apart from the user's word: `state` reads
        // `going` or `dismissed` over a real cancellation.
        "mail_state": ghost.mail_state,
        // False for a ghost made from mail that did not pass authentication.
        "verified": ghost.verified(),
        "likelihood": ghost.served_likelihood().map(|l| json!({
            "band": l.band,
            "reason": (!l.reason.is_empty()).then_some(&l.reason),
        })),
        "sources": ghost.sources.iter().map(|s| json!({
            "message_id": s.message_id,
            "thread_id": s.thread_id,
            "role": s.role,
        })).collect::<Vec<_>>(),
    })
}

/// `GET /client/timeline?from=&to=`: the calendar ghosts and the real Google
/// Calendar events whose start falls in `[from, to)`, each by start, and the
/// calendar sync's status. HUMAN DOOR ONLY: the agent door has no timeline
/// route (docs/CALENDAR.md §10 leaves that open, and until it is decided the
/// answer is no).
///
/// `{ "ghosts": [...], "events": [...], "calendars": [...], "calendar":
/// {granted, synced_at, error, truncated} }`. Events and calendars are served
/// only while calendar is granted. `calendars` is every calendar being read,
/// the primary first: `{id, name, color, primary, writable, can_invite}`,
/// `writable` being whether the app may create events on it (the user is
/// `owner` or `writer`) and `can_invite` whether an event on it may carry
/// guests (the user's own: the primary calendar, or one they are `owner` of).
/// `error` is the credential's or the PRIMARY calendar's (a shared calendar
/// that cannot be read is not an account error). `truncated` is true when
/// the listing hit its cap (it keeps the events nearest now) or a calendar
/// holds fewer events than Google has (its own cap, likewise nearest now).
/// Each ghost
/// carries `matched_event` (`{calendar_id, id}` or null): a real event at the
/// same start whose title matches ([`squelch_core::gcal::ghost_matches`]),
/// computed here for display de-duplication and never stored.
///
/// Defaults: `from` is a day ago and `to` is `from` plus the default window
/// (so with neither, now-1d to now+90d). `state` is judged at the server's
/// clock read in `from`'s offset, so a date-only ghost expires at the end of
/// the CALLER's day when the caller sends its own offset.
pub async fn get_timeline(
    State(state): State<ApiState>,
    Query(q): Query<TimelineQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let now = Utc::now();
    let from = timeline_bound(q.from.as_deref())?
        .unwrap_or_else(|| (now - TIMELINE_DEFAULT_BEHIND).fixed_offset());
    // Checked: `+` panics past chrono's year range, and `from` is the caller's.
    let later = |span| {
        from.checked_add_signed(span)
            .ok_or_else(|| ApiError::bad_request("from is out of range"))
    };
    let to = match timeline_bound(q.to.as_deref())? {
        Some(to) => to,
        None => later(TIMELINE_DEFAULT_BEHIND + TIMELINE_DEFAULT_AHEAD)?,
    };
    let to = to.min(later(TIMELINE_MAX_SPAN)?);
    // Real events are served only while calendar is granted: a grant taken
    // back hides what was synced under it.
    let granted = match state.calendar() {
        Some(calendar) => calendar.granted().await,
        None => false,
    };
    let (ghosts, events, calendars, status) = store_call(&state, move |store, account_id| {
        let ghosts = store.list_ghost_events(account_id, from, to)?;
        if !granted {
            return Ok((ghosts, Vec::new(), Vec::new(), Default::default()));
        }
        let (events, cut) = store.list_calendar_events(account_id, from, to, now)?;
        let mut status: squelch_core::gcal::CalendarStatus = store.calendar_status(account_id)?;
        status.truncated |= cut;
        let calendars: Vec<_> = store
            .calendar_calendars(account_id)?
            .into_iter()
            .filter(|c| c.selected)
            .collect();
        Ok((ghosts, events, calendars, status))
    })
    .await?;
    // The display match, computed here and never stored, so it can never go
    // stale and never changes a ghost.
    let matched = squelch_core::gcal::match_ghosts(
        ghosts.iter().map(|g| (g.title.as_str(), &g.start)),
        &events,
    );
    let now = now.with_timezone(from.offset());
    let ghosts: Vec<_> = ghosts
        .iter()
        .zip(matched)
        .map(|(g, matched)| {
            let mut view = ghost_view(g, now);
            view["matched_event"] = match matched.map(|i| &events[i]) {
                Some(event) => {
                    json!({ "calendar_id": event.calendar_id, "id": event.row.event_id })
                }
                None => serde_json::Value::Null,
            };
            view
        })
        .collect();
    let account_email = state
        .calendar()
        .map(|c| c.account_email().to_string())
        .unwrap_or_default();
    let events: Vec<_> = events
        .iter()
        .map(|e| calendar_event_view(e, &account_email))
        .collect();
    let calendars: Vec<_> = calendars
        .iter()
        .map(|c| {
            json!({
                "id": c.calendar_id,
                "name": c.summary,
                "color": c.color,
                "primary": c.is_primary,
                "writable": c.writable(),
                "can_invite": c.can_invite(),
            })
        })
        .collect();
    Ok((
        no_store(),
        Json(json!({
            "ghosts": ghosts,
            "events": events,
            "calendars": calendars,
            "calendar": {
                "granted": granted,
                "synced_at": status.synced_at.map(|at| at.to_rfc3339()),
                "error": status.error.map(|kind| kind.as_str()),
                "truncated": status.truncated,
            },
        })),
    ))
}

/// One real calendar event as served. SERIALIZED SHAPE IS A WIRE CONTRACT for
/// the timeline client. HUMAN DOOR ONLY: the title, place, organizer and link
/// are the user's personal data and no agent route serves them.
///
/// `start` / `end` are the ghosts' text form and convention: an all-day
/// `end` is the INCLUSIVE last day (Google's exclusive `end.date` minus one),
/// and null for a one-day event, as a one-day ghost has none. `busy` is false
/// when the event is marked free. Declined events are served; the client
/// hides them. `color` is the event's own colour or its calendar's
/// (`#rrggbb`, or null when Google gave neither), `editable` the edit guard
/// ([`squelch_core::gcal::SyncedEvent::editable`]) for `account_email`, and
/// `description` laundered text of at most 2000 characters. `guests` is the
/// event's attendees as Google lists them (`{email, name, response,
/// organizer, self}`, `response` one of `accepted` / `declined` /
/// `tentative` / `pending`), at most 100 and `[]` when it has none;
/// `guests_truncated` says Google holds more than are listed, and
/// `guest_editable` is the guest guard
/// ([`squelch_core::gcal::SyncedEvent::guest_editable`]).
pub(crate) fn calendar_event_view(
    event: &squelch_core::gcal::SyncedEvent,
    account_email: &str,
) -> serde_json::Value {
    let row = &event.row;
    json!({
        "id": row.event_id,
        "calendar_id": event.calendar_id,
        "calendar_name": event.calendar_name,
        "title": row.summary,
        "start": row.start.to_string(),
        "end": row.end.map(|e| e.to_string()),
        "all_day": row.all_day(),
        "start_timezone": row.start_timezone,
        "location": row.location,
        "status": if row.tentative { "tentative" } else { "confirmed" },
        "self_response": row.self_response.map(|r| r.as_str()),
        "busy": !row.transparent,
        "organizer": row.organizer_email,
        "html_link": row.html_link,
        "color": event.color(),
        "editable": event.editable(account_email),
        "description": row.description,
        "guests": row.guests.iter().map(|g| json!({
            "email": g.email,
            "name": g.name,
            "response": g.response.as_str(),
            "organizer": g.organizer,
            "self": g.is_self,
        })).collect::<Vec<_>>(),
        "guests_truncated": row.guests_truncated,
        "guest_editable": event.guest_editable(account_email),
    })
}

/// The least time between two calendar syncs one account may start from the
/// human door.
const TIMELINE_REFRESH_FLOOR: std::time::Duration = std::time::Duration::from_secs(30);

/// `POST /client/timeline/refresh`: start a calendar sync now. The client
/// calls it when the calendar page opens. Always 202: `kicked` says whether a
/// sync was started. At most one per account per 30 seconds, held in memory
/// across requests and clients; inside the floor, or with no sync loop
/// running, nothing is done. A kick also never cuts short the loop's own
/// backoff after Google rate-limited it.
///
/// A READ-path trigger, like the shipments poll: no Gmail write, nothing to
/// audit. HUMAN DOOR ONLY.
pub async fn refresh_timeline(State(state): State<ApiState>) -> impl IntoResponse {
    let refresh = &state.calendar_refresh;
    let kicked = match &refresh.kick {
        Some(kick) => {
            let now = std::time::Instant::now();
            let mut last = refresh.last_kick.lock().unwrap_or_else(|e| e.into_inner());
            let due = last
                .get(&state.account_id)
                .is_none_or(|at| now.duration_since(*at) >= TIMELINE_REFRESH_FLOOR);
            if due {
                last.insert(state.account_id, now);
                kick.notify_one();
            }
            due
        }
        None => false,
    };
    (
        StatusCode::ACCEPTED,
        no_store(),
        Json(json!({ "kicked": kicked })),
    )
}

/// A field that tells ABSENT from an explicit `null`: absent stays `None`,
/// while `null` arrives as `Some(Value::Null)` (plain `Option` would fold it
/// into `None`). Raw values, so a wrong type is this door's own 400.
fn present<'de, D>(deserializer: D) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(Some)
}

/// `PATCH /client/timeline/ghosts/{id}` body. Each field: absent leaves it
/// alone, `null` clears it.
#[derive(Debug, Deserialize)]
pub struct GhostPatchBody {
    /// `"dismissed"`, `"going"` or `null`.
    #[serde(default, deserialize_with = "present")]
    state: Option<serde_json::Value>,
    /// The user's corrected end: RFC3339 for a timed ghost, `YYYY-MM-DD` for a
    /// date-only one, or `null` to fall back to the estimate.
    #[serde(default, deserialize_with = "present")]
    end: Option<serde_json::Value>,
}

/// `PATCH /client/timeline/ghosts/{id}`: the user's word on a ghost. HUMAN DOOR
/// ONLY, like every write. Both fields are user-owned columns the projection
/// never overwrites, so a later mail cannot undo a dismissal or a correction.
///
/// The body is validated before the store is called, and the store applies
/// both fields in ONE transaction, so a refused end (the one check that needs
/// the ghost's start) is a 400 that leaves the state untouched too. An unknown
/// id and another account's are the same 404.
pub async fn patch_ghost(
    State(state): State<ApiState>,
    Path(ghost_id): Path<i64>,
    Json(body): Json<GhostPatchBody>,
) -> Result<impl IntoResponse, ApiError> {
    use serde_json::Value;
    use squelch_core::triage::ghost::EventTime;
    use squelch_core::types::GhostUserState;

    let user_state = match body.state {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(raw)) => Some(Some(GhostUserState::parse(&raw).ok_or_else(|| {
            ApiError::bad_request("state must be \"dismissed\", \"going\" or null")
        })?)),
        Some(_) => {
            return Err(ApiError::bad_request(
                "state must be \"dismissed\", \"going\" or null",
            ));
        }
    };
    let bad_end =
        || ApiError::bad_request("end must be an RFC3339 timestamp, a YYYY-MM-DD date, or null");
    let user_end = match body.end {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(raw)) => Some(Some(EventTime::parse(&raw).ok_or_else(bad_end)?)),
        Some(_) => return Err(bad_end()),
    };
    if user_state.is_none() && user_end.is_none() {
        return Err(ApiError::bad_request(
            "nothing to change: send state, end, or both",
        ));
    }

    let at = Utc::now();
    let found = store_call(&state, move |store, account_id| {
        store.patch_ghost_user(account_id, ghost_id, user_end, user_state, at)
    })
    .await?;
    if !found {
        return Err(ApiError::not_found());
    }
    Ok((no_store(), Json(json!({ "updated": true, "id": ghost_id }))))
}

// --- GET/POST/DELETE /client/rules ------------------------------------------

pub async fn list_rules(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    query(&state, |store, account_id| {
        store.list_sender_rules(account_id)
    })
    .await
}

#[derive(Debug, Deserialize)]
pub struct CreateRuleBody {
    match_pattern: String,
    want: String,
    /// What the rule DOES. Optional: absent (or the literal `"auto"`) means the
    /// daemon infers it from `want`, the plain-English sentence the composer
    /// asks for, so the client never has to make the user pick a taxonomy.
    /// An explicit value is honored verbatim.
    #[serde(default)]
    disposition: Option<String>,
    /// The message the user was acting FROM (the block-sender flows), so a
    /// squelch rule can resolve it: blocking the sender IS that email's
    /// disposition. Optional — the rule editor has no source message.
    #[serde(default)]
    source_message_id: Option<i64>,
    /// INTENT TO MASS-RESOLVE. `true` only from the block-sender flow, where the
    /// user's click means "and clear what they already sent"; every other path
    /// (the rule editor's save, an undo recreating a deleted rule) omits it.
    ///
    /// Separate from the disposition on purpose: "this rule squelches" and "sweep
    /// this sender out of the inbox right now" are different claims, and undoing
    /// a rule delete must be able to make the first without making the second.
    #[serde(default)]
    sweep: bool,
}

/// The literal `disposition` value that asks for inference explicitly, for a
/// client that would rather send a value than omit the key. EXACT MATCH: the real
/// dispositions parse exactly, so this one does too.
const DISPOSITION_AUTO: &str = "auto";

const DISPOSITION_HINT: &str = "disposition must be surface, squelch, or filtered (or omitted, or \"auto\", \
     to infer it from want)";

/// A rule write's disposition, plus WHO decided it. The flag is
/// security-relevant: an inferred verdict is a guess about one sentence, so it
/// may set what the rule does but must not trigger the destructive side effect
/// an explicit squelch does. See [`create_rule`].
struct ResolvedDisposition {
    disposition: Disposition,
    /// `true` when the REQUEST BODY named this disposition; `false` when a model
    /// inferred it from `want`.
    explicit: bool,
    /// What the inference call spent, when one ran. Billed by the caller into
    /// [`RULE_INFER_LEDGER_CATEGORY`]; `None` when no call happened.
    usage: Option<Usage>,
}

/// The usage-ledger category for rule-disposition inference. Its own line, not
/// folded into `stage1`: this is interactive, user-triggered spend, and a call
/// that lands in no category is spend no cap and no report can see.
const RULE_INFER_LEDGER_CATEGORY: &str = "rule_infer";

/// Bill one inference call. Best-effort by design: a ledger write that fails
/// must not fail the save. Callers run it BEFORE the rule write, inside the
/// same store hop (so it costs no extra blocking task): the model call already
/// happened, and a store error on the rule row must not orphan the spend.
fn bill_rule_inference(store: &SqliteStore, account_id: AccountId, usage: Option<Usage>) {
    let Some(u) = usage else { return };
    let day = Utc::now().format("%Y-%m-%d").to_string();
    if let Err(e) = store.extract_bump_usage(account_id, &day, RULE_INFER_LEDGER_CATEGORY, u.into())
    {
        eprintln!("squelch: rule inference usage ledger bump failed ({e})");
    }
}

/// Resolve the disposition for a rule write: an explicit value parses as before,
/// and an absent/`auto` one is inferred from the owner's `want` sentence.
///
/// The classifier is fed the OWNER'S SENTENCE ONLY — never a subject, body, or
/// any other message content — so a hostile email can never author a rule about
/// its own sender. See [`squelch_core::triage::rule_infer`].
///
/// Inference NEVER fails a save: no LLM configured, an API error, or a refusal
/// all resolve to `filtered`, which is the safe answer (a filtered rule defers to
/// Stage-2, which reads the verbatim want text per email).
///
/// INFERENCE IS OPT-IN, EXACTLY: only an ABSENT key or the exact literal `"auto"`
/// asks for it. Every other present value must parse as a real disposition or the
/// write is a 400 — a typo, a stray empty string, or a case variant is a client
/// bug, and quietly inferring instead would hand a model a decision the client
/// believed it had made itself.
async fn resolve_disposition(
    state: &ApiState,
    body: &CreateRuleBody,
) -> Result<ResolvedDisposition, ApiError> {
    let named = match body.disposition.as_deref() {
        None => None,
        Some(DISPOSITION_AUTO) => None,
        Some(raw) => Some(raw),
    };

    if let Some(raw) = named {
        let disposition =
            Disposition::parse(raw).ok_or_else(|| ApiError::bad_request(DISPOSITION_HINT))?;
        return Ok(ResolvedDisposition {
            disposition,
            explicit: true,
            usage: None,
        });
    }

    // Inference needs something to read. An empty want with no disposition is
    // not a rule anyone could act on, so it is a 400 rather than a silent
    // filtered rule the store would reject anyway.
    if body.want.trim().is_empty() {
        return Err(ApiError::bad_request(
            "want must not be empty when disposition is omitted; say in plain words what you \
             do, or do not, want from this sender",
        ));
    }

    let (disposition, usage) = match state.rule_infer() {
        Some(llm) => rule_infer::infer_disposition(&body.want, llm).await,
        // No model configured: filtered is the safe standing answer.
        None => (Disposition::Filtered, None),
    };
    Ok(ResolvedDisposition {
        disposition,
        explicit: false,
        usage,
    })
}

/// SIDE-EFFECT GUARD (security-relevant): may this rule write mark the sender's
/// existing mail resolved?
///
/// TWO CONDITIONS, BOTH REQUIRED.
///
/// 1. An EXPLICIT squelch. An INFERRED squelch is one cheap model's read of one
///    sentence, and the side effect is destructive at scale (every open row from
///    that sender goes to done); a misclassification must never mass-resolve
///    mail. The inferred rule still takes effect on FUTURE mail, which is
///    recoverable by editing the rule.
/// 2. `sweep`, the client's separate statement that this write means "and clear
///    what they already sent". Only the block-sender flow sets it. A rule editor
///    save, or an undo recreating a rule the user just deleted, restores the
///    squelch WITHOUT re-sweeping an inbox the user may have since gone through.
fn may_resolve_sender_mail(resolved: &ResolvedDisposition, sweep: bool) -> bool {
    sweep && resolved.explicit && resolved.disposition == Disposition::Squelch
}

#[derive(Debug, Deserialize)]
pub struct SuggestRuleQuery {
    sender: String,
}

/// GET /client/rules/suggest?sender=addr: the sentence the owner might write
/// about this sender, for the rule card to offer as a placeholder. Always 200;
/// `suggestion` is null when there is no model, too little history, or no
/// clear pattern. It is only ever SHOWN: the owner accepts it with tab and
/// saves it themselves, and its disposition is then inferred from the owner's
/// sentence like any other. See [`squelch_core::triage::rule_suggest`].
pub async fn suggest_rule(
    State(state): State<ApiState>,
    Query(params): Query<SuggestRuleQuery>,
) -> Result<impl IntoResponse, ApiError> {
    // Validated before anything else, so a client bug answers the same 400
    // whether or not this daemon has a model.
    let sender = params.sender.trim().to_lowercase();
    if sender.is_empty() {
        return Err(ApiError::bad_request("sender must not be empty"));
    }
    let none = || (no_store(), Json(serde_json::json!({ "suggestion": null })));
    let Some(client) = state.rule_suggest.clone() else {
        return Ok(none());
    };
    let addr = sender.clone();
    let digest = store_call(&state, move |store, account_id| {
        // The runaway guard: past the day's cap the route answers null
        // without spending. Read from the same ledger the calls bill into.
        if rule_suggest_capped(store, account_id)? {
            return Ok(None);
        }
        let history = store.sender_rule_digest(account_id, &addr, rule_suggest::HISTORY_LIMIT)?;
        let rules = store.list_sender_rules(account_id)?;
        Ok(Some((history, rules)))
    })
    .await?;
    let Some((history, rules)) = digest else {
        return Ok(none());
    };
    let existing = rules_in_the_owners_words(rules, &sender);
    let (suggestion, usage) = rule_suggest::suggest_rule(
        &client,
        &rule_suggest::SuggestInput {
            from_addr: &sender,
            existing_rules: &existing,
            history: &history,
        },
    )
    .await;
    if let Some(u) = usage {
        let _ = store_call(&state, move |store, account_id| {
            bill_rule_suggest(store, account_id, u);
            Ok(())
        })
        .await;
    }
    Ok((
        no_store(),
        Json(serde_json::json!({ "suggestion": suggestion })),
    ))
}

/// The suggester's runaway guard: past the day's cap its routes answer without
/// spending. Read from the same ledger the calls bill into.
fn rule_suggest_capped(store: &SqliteStore, account_id: AccountId) -> Result<bool, CoreError> {
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let spent: u64 = store
        .list_usage_by_category(account_id, 1)?
        .into_iter()
        .filter(|(c, _)| c == squelch_core::metrics::RULE_SUGGEST_USAGE_CATEGORY)
        .flat_map(|(_, days)| days)
        .filter(|d| d.day == today)
        .map(|d| d.calls)
        .sum();
    Ok(spent >= u64::from(rule_suggest::DAILY_CAP))
}

/// Best-effort: a ledger miss must not cost the owner the answer it paid for.
fn bill_rule_suggest(store: &SqliteStore, account_id: AccountId, usage: Usage) {
    let day = Utc::now().format("%Y-%m-%d").to_string();
    if let Err(e) = store.extract_bump_usage(
        account_id,
        &day,
        squelch_core::metrics::RULE_SUGGEST_USAGE_CATEGORY,
        usage.into(),
    ) {
        eprintln!("squelch: rule suggestion usage ledger bump failed ({e})");
    }
}

/// The owner's own sentences from the rules that cover `sender`.
fn rules_in_the_owners_words(rules: Vec<SenderRule>, sender: &str) -> Vec<String> {
    rules
        .into_iter()
        .filter(|r| {
            !r.want_text.trim().is_empty()
                && squelch_core::triage::rules::glob_match(&r.match_pattern, sender)
        })
        .map(|r| r.want_text)
        .collect()
}

#[derive(Debug, Deserialize)]
pub struct SmartBlockQuery {
    sender: String,
}

/// What the first store read of [`unsubscribe_smart_block`] settles.
enum SmartBlockLookup {
    /// Answered already, or not answerable right now (no model, the day's cap).
    Known(Option<rule_suggest::SmartBlock>),
    Ask(Vec<SenderDigestRow>, Vec<SenderRule>),
}

/// GET /client/unsubscribes/smart_block?sender=addr: for a sender who ignored
/// an unsubscribe, a filtered rule that keeps their consequential mail and
/// drops the promotion, as `smart_block: {rule, keeps, drops}`; null when the
/// sender has no such line to draw. 404 when no unsubscribe was ever asked of
/// them.
///
/// ASKED OF A MODEL ONCE. The answer, a no included, is remembered on the
/// unsubscribe row and rides `GET /client/unsubscribes` from then on, so the
/// notice shows the same rule on a relaunch or another device without paying
/// again. Silence (too little mail, a refusal, a timeout) is not remembered.
/// The rule is only ever SHOWN: see [`squelch_core::triage::rule_suggest`].
pub async fn unsubscribe_smart_block(
    State(state): State<ApiState>,
    Query(params): Query<SmartBlockQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let sender = params.sender.trim().to_lowercase();
    if sender.is_empty() {
        return Err(ApiError::bad_request("sender must not be empty"));
    }
    let answer = |block: Option<rule_suggest::SmartBlock>| {
        (no_store(), Json(json!({ "smart_block": block })))
    };
    let client = state.rule_suggest.clone();
    let has_model = client.is_some();
    let addr = sender.clone();
    let lookup = store_call(&state, move |store, account_id| {
        let Some(memo) = store.unsubscribe_smart_block(account_id, &addr)? else {
            return Ok(None);
        };
        if let SmartBlockMemo::Asked(block) = memo {
            return Ok(Some(SmartBlockLookup::Known(block)));
        }
        if !has_model || rule_suggest_capped(store, account_id)? {
            return Ok(Some(SmartBlockLookup::Known(None)));
        }
        let history =
            store.sender_rule_digest(account_id, &addr, rule_suggest::SMART_BLOCK_HISTORY_LIMIT)?;
        Ok(Some(SmartBlockLookup::Ask(
            history,
            store.list_sender_rules(account_id)?,
        )))
    })
    .await?;
    let (history, rules) = match lookup {
        None => return Err(ApiError::not_found()),
        Some(SmartBlockLookup::Known(block)) => return Ok(answer(block)),
        Some(SmartBlockLookup::Ask(history, rules)) => (history, rules),
    };
    let Some(client) = client else {
        return Ok(answer(None));
    };
    let existing = rules_in_the_owners_words(rules, &sender);
    let (answered, usage) = rule_suggest::smart_block(
        &client,
        &rule_suggest::SuggestInput {
            from_addr: &sender,
            existing_rules: &existing,
            history: &history,
        },
    )
    .await;
    if answered.is_some() || usage.is_some() {
        let remembered = answered.clone();
        let _ = store_call(&state, move |store, account_id| {
            if let Some(u) = usage {
                bill_rule_suggest(store, account_id, u);
            }
            // Best-effort: a failed write only means the next ask pays again.
            if let Some(block) = &remembered
                && let Err(e) =
                    store.set_unsubscribe_smart_block(account_id, &sender, block.as_ref())
            {
                eprintln!("squelch: smart block was not remembered ({e})");
            }
            Ok(())
        })
        .await;
    }
    Ok(answer(answered.flatten()))
}

pub async fn create_rule(
    State(state): State<ApiState>,
    Json(body): Json<CreateRuleBody>,
) -> Result<impl IntoResponse, ApiError> {
    if body.match_pattern.trim().is_empty() {
        return Err(ApiError::bad_request("match_pattern must not be empty"));
    }
    let resolved = resolve_disposition(&state, &body).await?;
    let disposition = resolved.disposition;

    let pattern = body.match_pattern.trim().to_string();
    let source_message_id = body.source_message_id;
    let sweep = body.sweep;
    let usage = resolved.usage;
    let id = store_call(&state, move |store, account_id| {
        // Bill BEFORE the rule write: the model call already happened, so the
        // spend must land in the ledger even if set_sender_rule fails. A retry
        // after such a failure re-runs the inference too, so a second entry
        // matches a second real spend.
        bill_rule_inference(store, account_id, usage);
        let id = store.set_sender_rule(account_id, &body.match_pattern, &body.want, disposition)?;
        Ok(id)
    })
    .await?;
    // Best-effort audit: target is the match_pattern, detail the new rule id and
    // the RESOLVED disposition — an inferred verdict has to be as traceable as a
    // named one, since nothing in the request body records it.
    audit_action(
        &state,
        "rule.create",
        Some(pattern.clone()),
        &format!("{id} disposition={}", disposition.as_str()),
    )
    .await;
    // RESOLUTION: a squelch rule marks that sender's open mail done — the user
    // just said "never again" about them, and leaving the mail they already sent
    // in the bands makes the rule look like it did nothing. Squelch only: a
    // surface/filtered rule is tuning, not a verdict.
    //
    // EXPLICIT squelch AND `sweep` — see [`may_resolve_sender_mail`]: an inferred
    // squelch never mass-resolves mail, and neither does a save that did not ask
    // to sweep (the editor, an undo).
    //
    // Driven by the PATTERN, not by source_message_id, so the block prompt in
    // the action layer — which has no message on screen to name — resolves too.
    // Wildcards are left alone: `*@acme.com` is a domain verdict, and resolving
    // every address under a domain from one click is a bigger claim than the
    // click made.
    if may_resolve_sender_mail(&resolved, sweep) {
        if !pattern.contains('*') {
            resolve_sender_done(&state, &pattern).await;
        } else if let Some(mid) = source_message_id {
            resolve_done(&state, mid).await;
        }
    }
    Ok((
        StatusCode::CREATED,
        Json(json!({ "rule_id": id, "disposition": disposition.as_str() })),
    ))
}

pub async fn update_rule(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
    Json(body): Json<CreateRuleBody>,
) -> Result<impl IntoResponse, ApiError> {
    if body.match_pattern.trim().is_empty() {
        return Err(ApiError::bad_request("match_pattern must not be empty"));
    }
    // An edit re-infers from the SUBMITTED want when the client omits the
    // disposition: the sentence is what the rule means, so rewriting it can
    // legitimately change what the rule does.
    let resolved = resolve_disposition(&state, &body).await?;
    let disposition = resolved.disposition;

    let pattern = body.match_pattern.clone();
    let usage = resolved.usage;
    let updated = store_call(&state, move |store, account_id| {
        // Billed first, even when the id is bogus or the write fails: the
        // model call happened and was paid for regardless of what the store
        // does with the row.
        bill_rule_inference(store, account_id, usage);
        let updated = store.update_sender_rule(
            account_id,
            id,
            &body.match_pattern,
            &body.want,
            disposition,
        )?;
        Ok(updated)
    })
    .await?;
    if updated {
        // Only a real edit is recorded; a 404 changed nothing, so no row.
        audit_action(
            &state,
            "rule.update",
            Some(pattern),
            &format!("{id} disposition={}", disposition.as_str()),
        )
        .await;
        Ok(Json(
            json!({ "rule_id": id, "disposition": disposition.as_str() }),
        ))
    } else {
        Err(ApiError::not_found())
    }
}

pub async fn delete_rule(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let deleted = store_call(&state, move |store, account_id| {
        store.delete_sender_rule(account_id, id)
    })
    .await?;
    if deleted {
        // target is the rule id — the pattern is gone post-delete.
        audit_action(&state, "rule.delete", Some(id.to_string()), "ok").await;
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found())
    }
}

// --- GET/POST/PUT/DELETE /client/drafts -------------------------------------
//
// Drafts are LOCAL and human-door only: nothing here is synced to Gmail Drafts
// and no draft ever leaves the machine. Hence NO AUDIT ROWS anywhere in this
// section — the audit log is the ledger of reveals and Gmail writes, and a
// composition that never left is neither.
//
// Keys: a reply draft is one per replied-to message and a forward draft one per
// forwarded message (both via the keyed `PUT /client/drafts`); new-message
// drafts are MANY per account, created by `POST /client/drafts` and edited by id
// through `PUT /client/drafts/{id}`.

/// One draft on the wire. `to_addr` is spelled `to` here: the field name the
/// composer sends and the send endpoint uses.
#[derive(Debug, Serialize)]
struct DraftView {
    id: i64,
    reply_to_message_id: Option<i64>,
    /// The forwarded message, for a forward draft. Always present as a key.
    forward_of_message_id: Option<i64>,
    to: String,
    /// The other two recipient lists, spelled the way the send endpoint spells
    /// them. Always present, `""` when there are none, so a client need not
    /// branch on absence — and an omitted field would be indistinguishable from
    /// a daemon too old to have one, which is exactly the case where a silently
    /// dropped Bcc would matter most.
    cc: String,
    bcc: String,
    subject: String,
    body: String,
    /// The files this draft holds, upload order. Always present — `[]` when
    /// there are none — so a client tells "no files" from a daemon too old to
    /// stage any by the key's absence.
    attachments: Vec<OutboundAttachmentView>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    /// Picker context from the replied-to / forwarded message: its thread,
    /// subject, and sender (`parent_from` is the display name when there is
    /// one, else the address; `parent_from_addr` is always the bare address).
    /// Always present as keys, `null` for plain new mail.
    thread_id: Option<String>,
    parent_subject: Option<String>,
    parent_from: Option<String>,
    parent_from_addr: Option<String>,
    /// An agent wrote this draft through Agent MCP (issue #394). Always
    /// present; the composer marks such a draft so the owner knows whose words
    /// they are about to send.
    from_agent: bool,
}

impl From<Draft> for DraftView {
    fn from(d: Draft) -> Self {
        let parent_from = d
            .parent_from_name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .or_else(|| d.parent_from_addr.clone());
        Self {
            id: d.id,
            reply_to_message_id: d.reply_to_message_id,
            forward_of_message_id: d.forward_of_message_id,
            to: d.to_addr,
            cc: d.cc_addr,
            bcc: d.bcc_addr,
            subject: d.subject,
            body: d.body,
            attachments: d
                .attachments
                .into_iter()
                .map(OutboundAttachmentView::from)
                .collect(),
            created_at: d.created_at,
            updated_at: d.updated_at,
            thread_id: d.thread_id,
            parent_subject: d.parent_subject,
            parent_from,
            parent_from_addr: d.parent_from_addr,
            from_agent: d.from_agent,
        }
    }
}

/// The `Cache-Control: no-store` header pair, one spelling for every response
/// that must not be retained anywhere on the path: a revealed sealed body, and a
/// draft (an unsent composition is the user's own thinking, not something to leave
/// in a proxy or a disk cache).
pub(crate) fn no_store() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    headers
}

/// `GET /client/drafts` query. `forwards` is an OPT-IN that only a client
/// which understands forward drafts sends: an app predating them restores
/// "the first row with no reply key" as its new message, and a forward row
/// matches that, so it would open ⌘N on the forward's subject and note with
/// the original silently gone. Hidden rows are still stored and still
/// consumed by a forward send; they are only kept out of an old app's list.
#[derive(Debug, Default, Deserialize)]
pub struct ListDraftsQuery {
    #[serde(default, deserialize_with = "query_flag")]
    forwards: bool,
}

/// A query-string flag: `1` or `true` is on, anything else (or absent) off.
/// A plain `bool` would 400 on the `1` the client sends.
fn query_flag<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    let raw = String::deserialize(d)?;
    Ok(raw == "1" || raw.eq_ignore_ascii_case("true"))
}

pub async fn list_drafts(
    State(state): State<ApiState>,
    Query(q): Query<ListDraftsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let drafts = store_call(&state, |store, account_id| store.list_drafts(account_id)).await?;
    // Filtered here, not in the store: the hole is a wire-compat one, and the
    // store's own callers (a forward send discarding its draft) see every row.
    let items: Vec<DraftView> = drafts
        .into_iter()
        .filter(|d| q.forwards || d.forward_of_message_id.is_none())
        .map(DraftView::from)
        .collect();
    Ok((no_store(), Json(items)))
}

// --- GET /client/contacts ---------------------------------------------------
//
// Recipient autocomplete over the Sent-derived contacts table. HUMAN DOOR ONLY:
// who the user writes to is exactly the kind of graph the agent door must never
// see, so no `/mcp` surface touches this table.

#[derive(Debug, Deserialize)]
pub struct ContactsQuery {
    q: String,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct ContactView {
    addr: String,
    display_name: Option<String>,
    sent_count: i64,
    last_sent_at: Option<String>,
}

pub async fn get_contacts(
    State(state): State<ApiState>,
    Query(params): Query<ContactsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let q = params.q.trim().to_string();
    if q.is_empty() {
        return Ok((no_store(), Json(Vec::<ContactView>::new())));
    }
    // Suggestion-menu sized by default; capped so no caller turns this into a
    // contacts dump endpoint.
    let limit = params.limit.unwrap_or(8).min(25);
    let hits = store_call(&state, move |store, account_id| {
        store.search_contacts(account_id, &q, limit)
    })
    .await?;
    let items: Vec<ContactView> = hits
        .into_iter()
        .map(|c| ContactView {
            addr: c.addr,
            display_name: c.display_name,
            sent_count: c.sent_count,
            last_sent_at: c.last_sent_at.map(|d| d.to_rfc3339()),
        })
        .collect();
    Ok((no_store(), Json(items)))
}

// --- GET /client/calendar/people --------------------------------------------
//
// The calendar event pane's guest SUGGESTION: a title says "dinner with steven"
// and the pane offers the Steven the user most recently exchanged mail with.
// It offers; it never adds. HUMAN DOOR ONLY, like the contacts it reads: no
// `/mcp` tool, no `/client/agent/*` route and nothing in the assistant relay
// reaches it. No model is involved: the client finds the name in the title and
// the store matches it against people the user has written to.

#[derive(Debug, Deserialize)]
pub struct CalendarPeopleQuery {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Serialize)]
pub struct CalendarPersonView {
    addr: String,
    name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CalendarPeopleView {
    people: Vec<CalendarPersonView>,
}

/// People one answer names at most. A suggestion, not a directory.
const CALENDAR_PEOPLE_LIMIT: u32 = 3;
/// A given name longer than this is not a given name.
const CALENDAR_PEOPLE_NAME_MAX: usize = 64;
/// And one shorter than this is not asked about: one letter matches a large
/// part of anybody's contacts, and every match is a probe of that person's
/// mail under the store's one mutex. The client asks for nothing shorter.
/// Counted in letters and digits of the first word, as the store reads the
/// name (`typed_name_len`): "s." and "(s)" are one letter.
const CALENDAR_PEOPLE_NAME_MIN: usize = 2;

/// `GET /client/calendar/people?name=steven`: up to three people the user has
/// WRITTEN TO whose name starts with the typed words, best first (see
/// `SqliteStore::people_named` for who qualifies and the order). A name
/// shorter than two letters or digits (empty included) is a 400 with a fixed body; one
/// that is too long or an address answers the empty list. Neither says
/// anything of the input.
pub async fn get_calendar_people(
    State(state): State<ApiState>,
    Query(params): Query<CalendarPeopleQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let name = params.name.trim().to_string();
    if squelch_core::store::typed_name_len(&name) < CALENDAR_PEOPLE_NAME_MIN {
        return Err(ApiError::bad_request("name is too short"));
    }
    let people = if name.chars().count() > CALENDAR_PEOPLE_NAME_MAX || name.contains('@') {
        Vec::new()
    } else {
        store_call(&state, move |store, account_id| {
            store.people_named(account_id, &name, CALENDAR_PEOPLE_LIMIT)
        })
        .await?
        .into_iter()
        .map(|p| CalendarPersonView {
            addr: p.addr,
            name: p.display_name,
        })
        .collect()
    };
    Ok((no_store(), Json(CalendarPeopleView { people })))
}

#[derive(Debug, Deserialize)]
pub struct SendersQuery {
    /// OPTIONAL, unlike the search box's `q`: a missing fragment IS the
    /// empty fragment. The Mac client drops empty query values when it builds
    /// a URL, so "the reader has typed from: and nothing else" arrives here
    /// with no `q` at all, and refusing it would blank the menu at exactly the
    /// moment it is supposed to open.
    #[serde(default)]
    q: String,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct SenderView {
    addr: String,
    display_name: Option<String>,
    msg_count: i64,
    last_received_at: String,
}

/// GET /client/senders?q=fragment — the search field's `from:` autocomplete:
/// who has written to this account, ranked for the typed fragment. The twin of
/// `get_contacts` over the other direction of mail; contacts are the people the
/// user writes TO and cannot answer `from:`.
///
/// HUMAN DOOR ONLY, like contacts. UNLIKE contacts, an empty fragment answers:
/// it lists the senders with the most mail, because the reader has just typed
/// `from:` and a menu that stays blank until the next keystroke reads as one
/// that did not open. The response is `no-store`: this is a list of the user's
/// correspondents.
pub async fn get_senders(
    State(state): State<ApiState>,
    Query(params): Query<SendersQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let q = params.q.trim().to_string();
    // Menu-sized by default; capped so no caller turns this into a directory
    // dump endpoint, which the empty-fragment listing would otherwise be.
    let limit = params.limit.unwrap_or(8).min(25);
    let hits = store_call(&state, move |store, account_id| {
        store.search_senders(account_id, &q, limit)
    })
    .await?;
    let items: Vec<SenderView> = hits
        .into_iter()
        .map(|s| SenderView {
            addr: s.addr,
            display_name: s.display_name,
            msg_count: s.msg_count,
            last_received_at: s.last_received_at.to_rfc3339(),
        })
        .collect();
    Ok((no_store(), Json(items)))
}

#[derive(Debug, Serialize)]
pub struct SenderDomainView {
    domain: String,
    sender_count: i64,
    msg_count: i64,
    last_received_at: String,
}

/// GET /client/sender-domains?q=fragment — the rule editor's `*@` suggestions:
/// the sender directory rolled up by host, ranked for a typed fragment of one.
/// `get_senders` answers "who", this answers "everyone at where".
///
/// HUMAN DOOR ONLY, `no-store`, and capped the same way for the same reason.
/// An empty fragment answers here too (the busiest hosts): the owner has typed
/// `*@` and not said which domain yet.
pub async fn get_sender_domains(
    State(state): State<ApiState>,
    Query(params): Query<SendersQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let q = params.q.trim().to_string();
    let limit = params.limit.unwrap_or(8).min(25);
    let hits = store_call(&state, move |store, account_id| {
        store.search_sender_domains(account_id, &q, limit)
    })
    .await?;
    let items: Vec<SenderDomainView> = hits
        .into_iter()
        .map(|d| SenderDomainView {
            domain: d.domain,
            sender_count: d.sender_count,
            msg_count: d.msg_count,
            last_received_at: d.last_received_at.to_rfc3339(),
        })
        .collect();
    Ok((no_store(), Json(items)))
}

// --- GET /client/person ------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct PersonQuery {
    addr: String,
}

#[derive(Debug, Serialize)]
pub struct PersonReceivedView {
    count: i64,
    first_at: String,
    last_at: String,
}

#[derive(Debug, Serialize)]
pub struct PersonSentView {
    count: i64,
    last_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PersonRuleView {
    id: i64,
    match_pattern: String,
    want_text: String,
    disposition: &'static str,
}

#[derive(Debug, Serialize)]
pub struct PersonGroupView {
    id: i64,
    name: String,
}

#[derive(Debug, Serialize)]
pub struct PersonThreadView {
    thread_id: String,
    subject: String,
    last_at: String,
}

/// A person's timezone on the card: the zone, where it came from (`user`,
/// `invite`, `offsets`), how sure the inference is (`high`, `medium`), and the
/// zone's offset from UTC right now, so a client without a zone database can
/// still write "5:00pm ET" without guessing whether daylight saving is on.
#[derive(Debug, Serialize)]
pub struct PersonTimezoneView {
    zone: String,
    source: &'static str,
    confidence: &'static str,
    utc_offset_minutes: i32,
}

fn timezone_view(tz: Option<squelch_core::timezone::InferredZone>) -> Option<PersonTimezoneView> {
    let tz = tz?;
    Some(PersonTimezoneView {
        utc_offset_minutes: tz.utc_offset_minutes(Utc::now())?,
        source: tz.source.as_str(),
        confidence: tz.confidence.as_str(),
        zone: tz.zone,
    })
}

/// The wire shape of the reader's contact card. The optional parts are ABSENT,
/// never null, when there is nothing to say: no mail from them, never written
/// to, no rule covers them.
#[derive(Debug, Serialize)]
pub struct PersonView {
    addr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    is_self: bool,
    known_contact: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    received: Option<PersonReceivedView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sent: Option<PersonSentView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rule: Option<PersonRuleView>,
    groups: Vec<PersonGroupView>,
    recent_threads: Vec<PersonThreadView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timezone: Option<PersonTimezoneView>,
}

/// GET /client/person?addr= — everything the store knows about one address,
/// for the contact card a click on a sender opens in the reader.
///
/// HUMAN DOOR ONLY, like contacts and senders: it answers "how often do I write
/// to this person, and they to me", which is the correspondent graph the agent
/// door is never handed. ONE address per call and no listing mode, so it cannot
/// be walked into a directory dump without already knowing every address.
/// Sealed mail is absent from every count and list on it (see
/// `SqliteStore::person_record`). `no-store`, for the same reason as senders.
/// `timezone` is the correspondent's inferred (or user-set) zone, absent when
/// nothing is known.
pub async fn get_person(
    State(state): State<ApiState>,
    Query(params): Query<PersonQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let addr = params.addr.trim().to_string();
    // An RFC 5321 path tops out at 256 octets; anything longer is not an
    // address a message could have come from.
    if addr.is_empty() || !addr.contains('@') || addr.len() > 320 {
        return Err(ApiError::bad_request("addr must be an email address"));
    }
    let p = store_call(&state, move |store, account_id| {
        store.person_record(account_id, &addr)
    })
    .await?;
    let view = PersonView {
        addr: p.addr,
        display_name: p.display_name,
        is_self: p.is_self,
        known_contact: p.known_contact,
        received: p.received.map(|r| PersonReceivedView {
            count: r.count,
            first_at: r.first_at.to_rfc3339(),
            last_at: r.last_at.to_rfc3339(),
        }),
        sent: p.sent.map(|s| PersonSentView {
            count: s.count,
            last_at: s.last_at.map(|d| d.to_rfc3339()),
        }),
        rule: p.rule.map(|r| PersonRuleView {
            id: r.id,
            match_pattern: r.match_pattern,
            want_text: r.want_text,
            disposition: r.disposition.as_str(),
        }),
        groups: p
            .groups
            .into_iter()
            .map(|g| PersonGroupView {
                id: g.id,
                name: g.name,
            })
            .collect(),
        recent_threads: p
            .recent_threads
            .into_iter()
            .map(|t| PersonThreadView {
                thread_id: t.thread_id,
                subject: t.subject,
                last_at: t.last_at.to_rfc3339(),
            })
            .collect(),
        timezone: timezone_view(p.timezone),
    };
    Ok((no_store(), Json(view)))
}

#[derive(Debug, Deserialize)]
pub struct PersonTimezoneBody {
    addr: String,
    /// An IANA zone name to set, or `null` to clear the override. REQUIRED,
    /// null included: a body that forgot the key must not read as "clear".
    #[serde(deserialize_with = "required_nullable")]
    zone: Option<String>,
}

/// `Option<T>` that must be PRESENT in the body: `null` is `None`, a missing
/// key is a 4xx. Plain `Option` would read an absent key as `None`.
fn required_nullable<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d)
}

/// PUT /client/person/timezone `{"addr": "...", "zone": "America/Chicago"}`:
/// the user's own word on where someone is, which outranks every inferred
/// signal. `"zone": null` clears it and the card goes back to inference.
/// Answers `{"timezone": {...}}` or `{"timezone": null}`: what the card now
/// shows, so the client need not re-read the whole card.
///
/// HUMAN DOOR ONLY, like the card it edits. A local write: nothing reaches
/// Gmail. 400 for an address that is not one, a zone name `chrono-tz` does not
/// know ("Eastern Standard Time", "PDT"), or the account's own address, whose
/// zone is the device's. Not audited, like the user's word on a ghost: it
/// changes a suggestion on this machine and acts on nothing.
pub async fn put_person_timezone(
    State(state): State<ApiState>,
    Json(body): Json<PersonTimezoneBody>,
) -> Result<impl IntoResponse, ApiError> {
    let addr = body.addr.trim().to_string();
    if addr.is_empty() || !addr.contains('@') || addr.len() > 320 {
        return Err(ApiError::bad_request("addr must be an email address"));
    }
    let zone = body.zone.map(|z| z.trim().to_string());
    if zone.as_ref().is_some_and(|z| z.is_empty() || z.len() > 64) {
        return Err(ApiError::bad_request("unknown timezone"));
    }
    let shown = store_call(&state, move |store, account_id| {
        store.set_person_timezone(account_id, &addr, zone.as_deref())
    })
    .await?;
    Ok((
        no_store(),
        Json(json!({ "timezone": timezone_view(shown) })),
    ))
}

#[derive(Debug, Deserialize)]
pub struct DraftBody {
    /// The message being replied to. With `forward_of_message_id` also absent,
    /// the key-less PUT edits the NEWEST new-message draft (created when there
    /// is none): the single-slot behaviour an app predating many drafts expects.
    #[serde(default)]
    reply_to_message_id: Option<i64>,
    /// The message being forwarded: one draft per forwarded message, like
    /// replies. At most one of the two keys may be set.
    #[serde(default)]
    forward_of_message_id: Option<i64>,
    #[serde(flatten)]
    fields: DraftFieldsBody,
}

/// The composition half of a draft body, shared by the keyed `PUT
/// /client/drafts`, `POST /client/drafts`, and `PUT /client/drafts/{id}`.
#[derive(Debug, Deserialize)]
pub struct DraftFieldsBody {
    /// Every text field is optional — a half-composed draft is the normal case,
    /// so a missing (or null) field stores "" rather than 400ing.
    #[serde(default)]
    to: Option<String>,
    /// The other two recipient lists. Absent stores `""` like every other field
    /// here — a half-composed draft is the normal case — and unlike the SEND
    /// route, absence has no second meaning: nothing is ever derived into a
    /// draft.
    #[serde(default)]
    cc: Option<String>,
    #[serde(default)]
    bcc: Option<String>,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    body: Option<String>,
    /// The staged files this draft holds — EXACTLY these, so a file removed
    /// from the tray is released by the next save. ABSENT leaves the claims
    /// alone (a client that predates staging never touches them); `[]` releases
    /// every file the draft held.
    #[serde(default)]
    attachment_ids: Option<Vec<i64>>,
}

/// Write one draft under `key` and apply its attachment claims, all in one
/// store call. `None` only for a [`DraftKey::NewById`] miss.
async fn save_draft(
    state: &ApiState,
    key: DraftKey,
    body: DraftFieldsBody,
) -> Result<Option<Draft>, ApiError> {
    let to = body.to.unwrap_or_default();
    let cc = body.cc.unwrap_or_default();
    let bcc = body.bcc.unwrap_or_default();
    let subject = body.subject.unwrap_or_default();
    let text = body.body.unwrap_or_default();
    let attachment_ids = body.attachment_ids;
    store_call(state, move |store, account_id| {
        let Some(mut draft) = store.upsert_draft(
            account_id,
            key,
            squelch_core::store::DraftFields {
                to_addr: &to,
                cc_addr: &cc,
                bcc_addr: &bcc,
                subject: &subject,
                body: &text,
            },
            Utc::now(),
        )?
        else {
            return Ok(None);
        };
        // Claim under the same store call, then re-read: the row the upsert
        // returned was read before the claim and would list the OLD files.
        if let Some(ids) = attachment_ids {
            store.claim_outbound_attachments(account_id, draft.id, &ids, Utc::now())?;
            if let Some(fresh) = store.draft_by_id(account_id, draft.id)? {
                draft = fresh;
            }
        }
        Ok(Some(draft))
    })
    .await
}

/// `PUT /client/drafts` — save the draft for one KEY: a reply target, a
/// forwarded message, or (neither) the newest new-message draft, for apps that
/// predate many drafts. Idempotent by key: a second PUT edits the same row.
pub async fn put_draft(
    State(state): State<ApiState>,
    Json(body): Json<DraftBody>,
) -> Result<impl IntoResponse, ApiError> {
    let key = match (body.reply_to_message_id, body.forward_of_message_id) {
        (Some(_), Some(_)) => {
            return Err(ApiError::bad_request(
                "a draft is a reply or a forward, not both",
            ));
        }
        (Some(parent), None) => DraftKey::Reply(parent),
        (None, Some(parent)) => DraftKey::Forward(parent),
        (None, None) => DraftKey::NewLatest,
    };
    // Resolve ownership independently from external-agent access. A human may
    // draft a reply to (or a forward of) any of their messages, including
    // authentication mail.
    if let DraftKey::Reply(parent) | DraftKey::Forward(parent) = key {
        resolve_target(&state, parent).await?;
    }
    let draft = save_draft(&state, key, body.fields)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok((no_store(), Json(DraftView::from(draft))))
}

/// `POST /client/drafts` — start a NEW new-message draft. Always inserts; the
/// returned `id` is what later saves address through `PUT /client/drafts/{id}`.
pub async fn create_draft(
    State(state): State<ApiState>,
    Json(body): Json<DraftFieldsBody>,
) -> Result<impl IntoResponse, ApiError> {
    let draft = save_draft(&state, DraftKey::NewCreate, body)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok((no_store(), Json(DraftView::from(draft))))
}

/// `PUT /client/drafts/{id}` — edit one draft by id, whatever its key (the key
/// is left untouched). Another account's id and an unknown id are the same 404.
pub async fn update_draft(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
    Json(body): Json<DraftFieldsBody>,
) -> Result<impl IntoResponse, ApiError> {
    let draft = save_draft(&state, DraftKey::NewById(id), body)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok((no_store(), Json(DraftView::from(draft))))
}

/// `DELETE /client/drafts/{id}` — discard one draft. Another account's id and an
/// unknown id are the same 404.
pub async fn delete_draft(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let deleted = store_call(&state, move |store, account_id| {
        store.delete_draft(account_id, id)
    })
    .await?;
    if deleted {
        Ok((no_store(), Json(json!({ "status": "deleted", "id": id }))))
    } else {
        Err(ApiError::not_found())
    }
}

// --- GET /client/sent --------------------------------------------------------

/// One message the user sent. HUMAN DOOR ONLY: the agent door has no sent
/// surface at all, and every other listing on both doors filters `is_sent = 0`.
/// Metadata only — the body is reached the way any other message's is, through
/// `GET /client/thread/{thread_id}`.
#[derive(Debug, Serialize)]
struct SentItem {
    id: i64,
    thread_id: String,
    /// Display recipients, `""` when the row predates the recipients backfill.
    to: String,
    subject: String,
    snippet: String,
    /// `messages.received_at` verbatim: for sent mail, when it went out.
    sent_at: String,
    /// Recorded read receipts. Always 0 unless the send was tracked.
    opens: i64,
}

#[derive(Debug, Deserialize)]
pub struct SentQuery {
    limit: Option<u32>,
    cursor: Option<String>,
}

/// `GET /client/sent` — the user's own outbox, newest first. A plain read: no
/// surfacing stamp (sent mail is in no attention band) and no audit row, exactly
/// like the other read-only listings.
pub async fn get_sent(
    State(state): State<ApiState>,
    Query(q): Query<SentQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let (limit, offset) = paginate(q.limit, q.cursor.as_deref())?;
    // sent_listing excludes sealed rows in SQL, failing CLOSED on a missing
    // triage row.
    let rows = store_call(&state, move |store, account_id| {
        store.sent_listing(account_id, limit, offset)
    })
    .await?;
    let items: Vec<SentItem> = rows
        .into_iter()
        .map(|m| SentItem {
            id: m.id,
            thread_id: m.thread_id,
            to: m.to,
            subject: m.subject,
            snippet: m.snippet,
            sent_at: m.sent_at,
            opens: m.opens,
        })
        .collect();

    let next = next_cursor(items.len(), limit, offset);
    Ok(Json(Page {
        items,
        next_cursor: next,
    }))
}

// --- POST /client/markdown/preview -------------------------------------------

#[derive(Debug, Deserialize)]
pub struct MarkdownPreviewBody {
    pub source: String,
}

/// The composer/signature preview: the SAME render the send path uses for the
/// HTML half of multipart/alternative, so what the preview shows is what a
/// send of this source would carry. Stateless — renders and returns, stores
/// and audits nothing, because nothing happened.
pub async fn markdown_preview(
    Json(body): Json<MarkdownPreviewBody>,
) -> Result<impl IntoResponse, ApiError> {
    Ok((
        no_store(),
        Json(json!({ "html": crate::markdown::render_email_html(&body.source) })),
    ))
}

// --- GET /client/sealed ------------------------------------------------------

/// Sealed METADATA only. No bodies here, ever.
///
/// `kind` is the one kind a client sees, picked by priority across the
/// decision's kinds. `critical`, `verdict` and `explained_by` are always
/// present (`false` / `null` when there is nothing to say), so a client can
/// tell this daemon from one without verdicts by the key alone.
#[derive(Debug, Serialize)]
struct SealedMeta {
    id: i64,
    thread_id: String,
    sender: String,
    subject: String,
    kind: Option<String>,
    received_at: DateTime<Utc>,
    critical: bool,
    verdict: Option<squelch_core::types::AuthVerdict>,
    explained_by: Option<i64>,
}

pub async fn list_sealed(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    let sealed = store_call(&state, |store, account_id| {
        store.sealed_messages(account_id)
    })
    .await?;
    let items: Vec<SealedMeta> = sealed
        .into_iter()
        .map(|m| SealedMeta {
            id: m.id,
            thread_id: m.thread_id,
            sender: m.from_addr,
            subject: m.subject,
            kind: m.sealed_kind,
            received_at: m.received_at,
            critical: m.critical,
            verdict: m.verdict,
            explained_by: m.explained_by,
        })
        .collect();
    Ok(Json(items))
}

// --- PUT /client/sealed/{message_id}/verdict --------------------------------

/// `{"verdict": "mine" | "not_mine" | null}`. The key is required: a body
/// that forgot it must not read as "clear my ruling".
#[derive(Debug, Deserialize)]
pub struct AuthVerdictBody {
    #[serde(deserialize_with = "required_nullable")]
    verdict: Option<squelch_core::types::AuthVerdict>,
}

/// The owner's ruling on one auth message ("That was me" / "Not me"), kept
/// on the server so every client sees it. `null` clears it. A `not_mine`
/// puts the message in FYE; a `mine` takes a critical change back out.
/// 404 for a message that is not auth mail. A legacy sealed row (sealed by
/// the old pipeline, no agent decision) takes a ruling and serves it back,
/// but it never moves FYE: it has no decision or attention to place.
/// Every write is audited. HUMAN DOOR ONLY.
pub async fn put_sealed_verdict(
    State(state): State<ApiState>,
    Path(message_id): Path<i64>,
    Json(body): Json<AuthVerdictBody>,
) -> Result<impl IntoResponse, ApiError> {
    let verdict = body.verdict;
    store_call(&state, move |store, account_id| {
        store.set_auth_verdict(account_id, message_id, verdict, Utc::now())
    })
    .await?;
    // A ruling moves FYE and the shredder, so it leaves a row like a reveal
    // does: the message id and the ruling, nothing from the mail. The ruling
    // has landed by now, so a failed audit is logged rather than failing a
    // request whose effect a retry would only repeat.
    let entry = NewAuditEntry {
        actor: state.audit_actor.to_string(),
        action: "auth_verdict".to_string(),
        target: Some(message_id.to_string()),
        detail: Some(
            verdict
                .map_or("cleared", |verdict| verdict.as_str())
                .to_string(),
        ),
    };
    if let Err(error) = store_call(&state, move |store, account_id| {
        store.append_audit(account_id, &entry)
    })
    .await
    {
        eprintln!(
            "squelch-api: auth verdict on {message_id} landed but its audit row did not ({error:?})"
        );
    }
    Ok(Json(json!({ "id": message_id, "verdict": verdict })))
}

// --- POST /client/sealed/{message_id}/reveal --------------------------------

/// The revealed sealed body. Marked `Cache-Control: no-store` on the response.
#[derive(Debug, Serialize)]
struct RevealedSealed {
    id: i64,
    thread_id: String,
    sender: String,
    from_name: Option<String>,
    subject: String,
    kind: Option<String>,
    received_at: DateTime<Utc>,
    body: String,
    /// Server-sanitized HTML when the mail had one, through the same single
    /// audited reveal door and `no-store` like the text body.
    html: Option<String>,
}

/// `?source=notification` on a reveal opened by tapping a notification.
/// Absent (every older client) or `interactive` is the owner choosing to
/// reveal on the Auth page or FYE.
#[derive(Debug, Default, Deserialize)]
pub struct RevealQuery {
    #[serde(default)]
    source: Option<String>,
}

pub async fn reveal_sealed(
    State(state): State<ApiState>,
    Path(message_id): Path<i64>,
    Query(query): Query<RevealQuery>,
) -> Result<impl IntoResponse, ApiError> {
    // The source rides on the audit row's detail. The Auth page's pairing
    // reads a reveal as "the owner used this code", which a tap on an
    // unsolicited code's banner is not, so notification reveals never
    // explain an alert (`sealed_rows` skips them).
    let detail = match query.source.as_deref() {
        None | Some("interactive") => None,
        Some("notification") => Some("notification".to_string()),
        Some(_) => {
            return Err(ApiError::bad_request(
                "source must be notification or interactive",
            ));
        }
    };
    let sealed = store_call(&state, move |store, account_id| {
        // Membership FIRST, then the audit, then the body leaves: a reveal
        // row means a real reveal of auth mail. The Auth page's pairing
        // reads these rows as "the owner used this code", so a refused
        // reveal of some other id must leave none (it would read as used if
        // that message were later classified auth). The audit still lands
        // before anything is returned, and a failed audit returns nothing.
        // Only the message id is recorded, so the log never leaks a secret.
        let body = store.sealed_body(account_id, message_id)?;
        store.append_audit(
            account_id,
            &NewAuditEntry {
                actor: AUDIT_ACTOR.to_string(),
                action: "reveal_sealed".to_string(),
                target: Some(message_id.to_string()),
                detail,
            },
        )?;
        Ok(body)
    })
    .await?;

    let payload = RevealedSealed {
        id: sealed.id,
        thread_id: sealed.thread_id,
        sender: sealed.from_addr,
        from_name: sealed.from_name,
        subject: sealed.subject,
        kind: sealed.sealed_kind,
        received_at: sealed.received_at,
        body: sealed.body,
        html: sealed.body_html,
    };

    // Never cache a sealed body anywhere along the path.
    Ok((no_store(), Json(payload)))
}

// --- GET /client/audit -------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    limit: Option<u32>,
}

pub async fn get_audit(
    State(state): State<ApiState>,
    Query(q): Query<AuditQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    query(&state, move |store, account_id| {
        store.list_audit(account_id, limit)
    })
    .await
}

// --- GET /client/stats -------------------------------------------------------

/// Has an iPhone app ever connected to this mailbox? TWO WITNESSES, either one
/// enough:
///
/// * a device token that named itself `ios/...` in `X-Passband-Client`
///   (`client_platform_seen`, which counts revoked rows: a phone that paired
///   and was later unpaired has still been tried), and
/// * an iOS push registration in `devices`, which is the only trace an iOS
///   build from BEFORE that header leaves.
///
/// "Ever", not "now": the question the Mac asks with it is "should we tell this
/// person the iPhone app exists", and somebody who has installed it knows.
fn ios_app_connected(store: &SqliteStore, account_id: AccountId) -> Result<bool, CoreError> {
    if store.client_platform_seen(account_id, ClientPlatform::Ios)? {
        return Ok(true);
    }
    Ok(store
        .list_devices(account_id)?
        .iter()
        .any(|d| d.platform == "ios"))
}

pub async fn get_stats(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    // UTC day key for today's Stage-2 usage row.
    let day = Utc::now().format("%Y-%m-%d").to_string();
    // Band counts run under the same default window as the lists they head.
    let bands_since = Utc::now() - chrono::Duration::days(DEFAULT_UPDATES_WINDOW_DAYS);
    let (stats, usage, unread, email, identity, jobs, parked, ios_connected) =
        store_call(&state, move |store, account_id| {
            let stats = store.stats(account_id, bands_since)?;
            let usage = store.stage2_usage_today(account_id, &day)?;
            let unread = store.inbox_unread(account_id)?;
            let email = store.account_email(account_id)?;
            let identity = store.account_identity(account_id)?;
            let jobs = store.triage_waiting_counts(account_id, Utc::now())?;
            let parked = store.triage_budget_parked(account_id)?;
            let ios_connected = ios_app_connected(store, account_id)?;
            Ok((
                stats,
                usage,
                unread,
                email,
                identity,
                jobs,
                parked,
                ios_connected,
            ))
        })
        .await?;

    // tokens/1e6 * per-MTok price, over input+output plus the prompt-cache
    // split (CACHE_WRITE_INPUT_MULT / CACHE_READ_INPUT_MULT of the input price). Switching the model
    // means updating stage2_price_*_per_mtok or this drifts.
    let est_cost_usd_today = (usage.input_tokens as f64 / 1_000_000.0)
        * state.stage2_price_in_per_mtok
        + (usage.output_tokens as f64 / 1_000_000.0) * state.stage2_price_out_per_mtok
        + (usage.cache_creation_tokens as f64 / 1_000_000.0)
            * state.stage2_price_in_per_mtok
            * CACHE_WRITE_INPUT_MULT
        + (usage.cache_read_tokens as f64 / 1_000_000.0)
            * state.stage2_price_in_per_mtok
            * CACHE_READ_INPUT_MULT;

    let mut body = serde_json::to_value(&stats)
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error"))?;
    body["stage2"] = json!({
        "calls_today": usage.calls,
        "input_tokens_today": usage.input_tokens,
        "output_tokens_today": usage.output_tokens,
        "cache_creation_tokens_today": usage.cache_creation_tokens,
        "cache_read_tokens_today": usage.cache_read_tokens,
        "est_cost_usd_today": est_cost_usd_today,
    });
    // WHICH MAILBOX THIS IS. The client has never had a way to ask: every other
    // door answers about mail, not about the account behind it, so the app knew
    // the daemon's host and nothing about the human. It needs the address to
    // seed a display name on a first run (the local part, until the human says
    // otherwise), and it is the account's own address — the same string the
    // send path already puts in `From` — so no confidence is spent telling the
    // human door about it. ALWAYS PRESENT: absence means an older daemon.
    body["account_email"] = json!(email);
    // WHO THAT IS, as Google's profile last described them: the name to greet
    // and label the account with, and a tag for the photo served at
    // `/client/account/photo`. Each key is OMITTED when unknown (a grant
    // without the profile scope, a nameless account, no picture), so a client
    // reads absence and keeps the initial and the local part it already shows.
    // The tag is the photo's version: a client holding bytes under the same tag
    // has nothing to fetch.
    if let Some(identity) = identity {
        if let Some(name) = identity.name {
            body["account_name"] = json!(name);
        }
        if let Some(given) = identity.given_name {
            body["account_given_name"] = json!(given);
        }
        if let Some(tag) = identity.photo_tag {
            body["account_photo"] = json!(tag);
        }
    }
    // Gmail's own unread counts, as of the sync loop's last successful fetch.
    // The key is OMITTED when the counts were never fetched (older DB, or the
    // fetch has never succeeded) so a client reads absence, not "0 unread" —
    // which is a real answer this daemon has no business inventing.
    //
    // `fetched_at` rides along because a failing fetch keeps serving the last
    // good pair: without the stamp a client cannot tell a live count from one
    // frozen since the scope was revoked.
    if let Some(unread) = unread {
        body["inbox_unread"] = json!({
            "messages": unread.messages,
            "threads": unread.threads,
            "fetched_at": unread.fetched_at.to_rfc3339(),
        });
    }
    // IS THE MAILBOX STILL CONNECTED, and if not, what does this person do
    // about it. The daemon detects a dead refresh token exactly (the refresh
    // exchange fails, `invalid_grant` included) and until now said so only to
    // Prometheus — so the operator could find out and the person staring at an
    // empty mailbox could not.
    //
    // OMITTED ENTIRELY when this door has no metrics handle, so a client reads
    // absence rather than a cheerful "connected" from a process that cannot
    // see. Absence means "no answer", and a client must not render it as good
    // news.
    //
    // `reconnect_url` is the hosted/self-host split, and it is the console SSO
    // origin because that is the same control plane that minted the credential
    // in the first place: hosted tenants have one, self-host has `None` and
    // repairs with `squelchd auth`, which is not a link anything can offer.
    if let Some(metrics) = state.sync_metrics() {
        let failed_since = metrics.gmail_auth_failed_since();
        let mut gmail = json!({ "connected": failed_since.is_none() });
        if let Some(at) = failed_since {
            if let Some(ts) = chrono::DateTime::from_timestamp(at, 0) {
                gmail["disconnected_since"] = json!(ts.to_rfc3339());
            }
            // Only on the broken path: a link to re-consent is noise while the
            // mailbox works, and worse, an invitation to re-consent for no
            // reason.
            // `hint` names this mailbox so the control plane pre-selects it at
            // Google and refuses a consent from any other account.
            if let Some(origin) = state.console_sso_url() {
                gmail["reconnect_url"] =
                    json!(format!("{origin}/reconnect?hint={}", url_component(&email)));
            }
        }
        body["gmail"] = gmail;
    }
    // A CATCH-UP IN FLIGHT, and what it is a denominator of. When Gmail's
    // history cursor expires the daemon re-walks a 30-day window, which is the
    // sync loop's longest single call and blocks triage for the whole of it —
    // so a client that does not know about it renders a working mailbox as a
    // hung one. The re-triage modal is the sharp case: its own progress cannot
    // move until this finishes, and without this field it has no way to say
    // why.
    //
    // OMITTED when no catch-up is running, so absence is the normal state and
    // presence is the explanation.
    if let Some((done, total)) = state.sync_metrics().and_then(|m| m.catchup_progress()) {
        body["catch_up"] = json!({ "done": done, "total": total });
    }
    // IS GMAIL SYNC WORKING, and is triage running. `gmail.connected` answers
    // only "is the credential alive"; a mailbox Gmail is rate-limiting has a
    // perfectly good credential and still syncs nothing, and triage spends
    // nothing until a poll succeeds. Until these two blocks the app said
    // "synced just now" and "You're all clear." over that. OMITTED when this
    // door has no metrics handle, for the same reason `gmail` is.
    if let Some(metrics) = state.sync_metrics() {
        body["sync"] = sync_block(metrics);
        body["triage"] = triage_block(metrics, jobs, parked);
    }
    // Capability flag for the app: whether /client/assistant/messages will
    // relay (hosted, gateway configured) or 404 (self-host, BYOK in the app).
    // Always present so a client reads an answer, not absence.
    body["assistant_relay"] = json!(state.assistant().is_some());
    // The same, for invite sharing: whether `/client/invites` will mint (hosted,
    // a share token installed) or report `can_share: false` (self-host, or a
    // tenant nobody has turned it on for). Always present, so a client reads an
    // answer rather than absence, and an OLD daemon's silence reads as "no".
    body["invite_sharing"] = json!(state.sharing().is_some());
    // The hidden features this mailbox has been let into, as sorted keys. An
    // app shows a hidden feature only when its key is listed, so an OLD
    // daemon's silence and an empty list both read as "nothing extra", which
    // is the safe direction. Keys the daemon itself has never heard of ride
    // through: a feature only the app implements is still a feature.
    body["features"] = json!(state.features().list());
    // Whether the iPhone app has EVER talked to this mailbox. The Mac uses it
    // to decide whether to point somebody at the TestFlight; see
    // [`ios_app_connected`]. Always present, so an old daemon's silence is the
    // only way to read "unknown", and the client treats that as "do not ask".
    body["ios_connected"] = json!(ios_connected);
    // Capability flag for the app: whether this daemon understands
    // `forward_of_message_id` on POST /client/actions/send.
    //
    // IT MUST BE A FLAG BECAUSE THE FIELD FAILS SILENTLY. `SendBody` takes it
    // with `#[serde(default)]`, so a daemon predating forwarding IGNORES the key
    // rather than rejecting it: the request becomes a bare cold send of the
    // user's note, with the original nowhere in it and nothing in the response
    // saying so. The client gates the forward composer on this flag, so that
    // never happens quietly. Always present, so a client reads an answer rather
    // than absence — an old daemon omits the key, which is the `false` case.
    body["forwarding"] = json!(true);
    // And for attachments: whether `attachment_ids` on a send and on a draft
    // mean anything here. Same silent-failure shape as forwarding — an old
    // daemon ignores the key and mails the words without the files — so the
    // client gates the attach affordances on this.
    body["compose_attachments"] = json!(true);
    // And for many drafts: whether POST /client/drafts, PUT /client/drafts/{id}
    // and `forward_of_message_id` on the keyed PUT exist. Same silent-failure
    // reasoning: an old daemon IGNORES `forward_of_message_id` on the keyed PUT
    // (serde default) and would overwrite its single new-message draft with a
    // forward, so the client must not write those shapes without this flag.
    body["drafts_v2"] = json!(true);
    // And for search order: whether `include_sent` on GET /client/search
    // exists, so the app can drop `unfinished_first` (plain ranked order)
    // without losing the owner's sent mail from keyword results. Another
    // silent failure: an old daemon ignores the key and keyword searches lose
    // sent mail, so the client keeps `unfinished_first=true` without the flag.
    body["search_plain_order"] = json!(true);
    // And for scheduled send: whether /client/scheduled (POST, GET, DELETE
    // /{id}, POST /{id}/now) exists. Not a silent failure this time but a
    // LOUD one: an old daemon 404s every one of those routes, so a schedule
    // click would read as a broken app. The client hides every schedule door
    // (the clock, the Sent group) without this flag.
    body["scheduled_send"] = json!(true);
    // And for the calendar timeline: whether GET /client/timeline and PATCH
    // /client/timeline/ghosts/{id} exist. A LOUD failure like scheduled send
    // (an old daemon 404s them), so the client shows no timeline without it.
    //
    // ALWAYS TRUE NOW: the `calendar` hidden feature that once switched this
    // off is retired (`squelch_core::features::RETIRED`). A daemon from
    // before it launched still says `false` for a mailbox without the flag,
    // and the client still reads that as "no Calendar page".
    body["timeline"] = json!(true);
    // IS GOOGLE CALENDAR GRANTED, and if not, how to grant it. Always
    // present (an old daemon's silence is the client's "no calendar"), and
    // human door only: no agent route carries it.
    //
    // `granted` is the token resolver's answer, read off the scopes recorded
    // on the stored token, with no call to Google. `connect_url` is the
    // hosted/self-host split exactly as `gmail.reconnect_url` is: the console
    // origin when this daemon has one, with the mailbox as `hint` so the
    // control plane pre-selects it and refuses any other account. Without
    // one, the command a self-host runs.
    let granted = match state.calendar() {
        Some(calendar) => calendar.granted().await,
        None => false,
    };
    let connect_url = state.console_sso_url().map(|origin| {
        format!(
            "{origin}/reconnect?calendar=1&hint={}",
            url_component(&email)
        )
    });
    let connect_command = connect_url.is_none().then_some("squelchd auth --calendar");
    // `self_connect`: whether `POST /client/calendar/connect` runs the consent
    // here (a self-host with an OAuth client). The client's one Connect button
    // calls it when true and opens `connect_url` otherwise; `connect_command`
    // stays for older builds that still print it.
    body["calendar"] = json!({
        "granted": granted,
        "connect_url": connect_url,
        "connect_command": connect_command,
        "self_connect": crate::calendar_connect::self_connect(&state),
    });
    Ok(Json(body))
}

/// One query-string value, percent-encoded so an address's `+` and `@` reach
/// the control plane as written.
fn url_component(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn rfc3339(unix: i64) -> Option<String> {
    DateTime::from_timestamp(unix, 0).map(|at| at.to_rfc3339())
}

/// `sync{ok, last_success_at?, error?, next_retry_at?, consecutive_failures}`.
/// `ok` is false only after a lifecycle has FAILED and before the next good
/// poll: a first-run backfill or a restart has not polled yet and is not an
/// outage. `error` and `next_retry_at` describe the CURRENT outage, so both
/// are left out while sync is ok: an error from a side call (the spam tab,
/// the unread count) is not a sync failure. `next_retry_at` is also absent
/// while the retry is running.
fn sync_block(metrics: &squelch_core::metrics::SyncMetrics) -> serde_json::Value {
    let ok = !metrics.sync_failing();
    let mut sync = json!({
        "ok": ok,
        "consecutive_failures": metrics.sync_consecutive_failures(),
    });
    if let Some(at) = metrics.sync_last_success().and_then(rfc3339) {
        sync["last_success_at"] = json!(at);
    }
    if !ok {
        if let Some(error) = metrics.sync_last_error() {
            sync["error"] = json!({
                "kind": error.kind.label(),
                "reason": error.reason,
                "at": rfc3339(error.at),
            });
        }
        if let Some(at) = metrics.sync_next_retry_at().and_then(rfc3339) {
            sync["next_retry_at"] = json!(at);
        }
    }
    sync
}

/// `triage{waiting_for_sync, cooldown_until?, queued, leased, budget_parked,
/// budget_resumes_at?}`: whether the triage worker can run, and how much work
/// it has.
fn triage_block(
    metrics: &squelch_core::metrics::SyncMetrics,
    [queued, leased]: [u64; 2],
    (parked, resumes_at): (u64, Option<DateTime<Utc>>),
) -> serde_json::Value {
    let mut triage = json!({
        "waiting_for_sync": metrics.sync_failing(),
        "queued": queued,
        "leased": leased,
        "budget_parked": parked,
    });
    if let Some(at) = metrics.triage_cooldown_until().and_then(rfc3339) {
        triage["cooldown_until"] = json!(at);
    }
    if let Some(at) = resumes_at {
        triage["budget_resumes_at"] = json!(at.to_rfc3339());
    }
    triage
}

// --- GET /client/usage -------------------------------------------------------

/// Default look-back window (rows) for the usage history.
const DEFAULT_USAGE_DAYS: u32 = 30;
const MAX_USAGE_DAYS: u32 = 365;

#[derive(Debug, Deserialize)]
pub struct UsageQuery {
    /// How many recent days of Stage-2 usage to return. Default 30.
    days: Option<u32>,
}

/// One day's row in the usage history response. `input_tokens` is the uncached
/// prompt remainder; the cache columns ride along so clients can show the split.
#[derive(Debug, Serialize)]
struct UsageRow {
    day: String,
    calls: u64,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    /// This day's spend, priced like the window total. Per day so a client can
    /// chart spend over time rather than only sum it.
    est_cost_usd: f64,
}

/// Totals across the returned window, costed from the same per-MTok prices
/// `get_stats` uses (cache writes at 1.25x / reads at 0.1x of the input price).
#[derive(Debug, Serialize)]
struct UsageTotals {
    calls: u64,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    est_cost_usd: f64,
}

/// GET /client/usage — triage usage history + totals + model label. Additive to
/// `/client/stats`, which still carries today's `stage2` rollup.
pub async fn get_usage(
    State(state): State<ApiState>,
    Query(q): Query<UsageQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let days = q
        .days
        .unwrap_or(DEFAULT_USAGE_DAYS)
        .clamp(1, MAX_USAGE_DAYS);
    // EVERY category in the ledger, not a hand-written list: extractors each
    // write their own, so naming them here means every extractor added later
    // spends invisibly. `extract_banking` did exactly that for ten days.
    let by_category = store_call(&state, move |store, account_id| {
        store.list_usage_by_category(account_id, days)
    })
    .await?;

    let s2_rows: Vec<squelch_core::store::Stage2UsageDay> = by_category
        .iter()
        .find(|(c, _)| c == "stage2")
        .map(|(_, rows)| rows.clone())
        .unwrap_or_default();

    // Stage-1 and Stage-2 are SEPARATE categories, each costed from its own
    // per-MTok prices; the client renders whatever categories arrive.
    let rollup = |rows: Vec<squelch_core::store::Stage2UsageDay>,
                  price_in: f64,
                  price_out: f64|
     -> (Vec<UsageRow>, UsageTotals) {
        // tokens/1e6 * per-MTok price, the prompt-cache split at its
        // multipliers — the arithmetic `get_stats` runs for today, applied to
        // each day and then to the window's sums.
        let cost = |input: u64, output: u64, cache_w: u64, cache_r: u64| -> f64 {
            (input as f64 / 1_000_000.0) * price_in
                + (output as f64 / 1_000_000.0) * price_out
                + (cache_w as f64 / 1_000_000.0) * price_in * CACHE_WRITE_INPUT_MULT
                + (cache_r as f64 / 1_000_000.0) * price_in * CACHE_READ_INPUT_MULT
        };
        let (mut in_tok, mut out_tok, mut calls) = (0u64, 0u64, 0u64);
        let (mut cache_w, mut cache_r) = (0u64, 0u64);
        let out_rows: Vec<UsageRow> = rows
            .into_iter()
            .map(|r| {
                calls += r.calls;
                in_tok += r.input_tokens;
                out_tok += r.output_tokens;
                cache_w += r.cache_creation_tokens;
                cache_r += r.cache_read_tokens;
                UsageRow {
                    est_cost_usd: cost(
                        r.input_tokens,
                        r.output_tokens,
                        r.cache_creation_tokens,
                        r.cache_read_tokens,
                    ),
                    day: r.day,
                    calls: r.calls,
                    input_tokens: r.input_tokens,
                    output_tokens: r.output_tokens,
                    cache_creation_tokens: r.cache_creation_tokens,
                    cache_read_tokens: r.cache_read_tokens,
                }
            })
            .collect();
        let est_cost_usd = cost(in_tok, out_tok, cache_w, cache_r);
        (
            out_rows,
            UsageTotals {
                calls,
                input_tokens: in_tok,
                output_tokens: out_tok,
                cache_creation_tokens: cache_w,
                cache_read_tokens: cache_r,
                est_cost_usd,
            },
        )
    };

    // Stage-2 only, for the flat top-level fields older clients read. Every
    // category, Stage-2 included, is rolled up again below for `categories`.
    let (s2_out, s2_totals) = rollup(
        s2_rows,
        state.stage2_price_in_per_mtok,
        state.stage2_price_out_per_mtok,
    );

    // One entry per ledger category, priced by FOUR arms (the fourth, smart
    // done's own model, is marked where it sits). Stage-2 runs the
    // capable model and its prices. `notify` runs the fast lane's own small
    // model and ITS prices (docs/NOTIFY.md §11.5): it is the one category that
    // is neither, and folding it into the arm below would bill a Haiku call at
    // Opus rates and overstate the cheapest pass in the pipeline by ~25x.
    // EVERYTHING ELSE — Stage-1 and every extractor — runs the stage-1 small
    // model and shares its config, so it costs at stage-1 rates.
    //
    // The two STAGES are always present, empty or not: they are permanent parts
    // of the pipeline, and a reader who spent nothing on escalations today is
    // owed "Stage 2: nothing" rather than a section that silently vanishes.
    // Extractors are the opposite — they come and go with what the ledger has
    // actually seen, which is what makes a new one report with no edit here.
    let mut listed: Vec<(String, Vec<squelch_core::store::Stage2UsageDay>)> = by_category.clone();
    for stage in ["stage1", "stage2"] {
        if !listed.iter().any(|(c, _)| c == stage) {
            listed.push((stage.to_string(), Vec::new()));
        }
    }
    listed.sort_by(|a, b| a.0.cmp(&b.0));

    let mut categories = serde_json::Map::new();
    for (name, rows) in &listed {
        // ONE match, so the model label and the prices can never come from
        // different arms: a category priced as Stage-1 but labelled with the
        // notify model would read as a 25x cost regression in the app.
        let (model, price_in, price_out) = if name == "stage2" {
            (
                state.stage2_model.as_ref(),
                state.stage2_price_in_per_mtok,
                state.stage2_price_out_per_mtok,
            )
        } else if name == squelch_core::translate::USAGE_CATEGORY {
            (
                state.translate_model.as_ref(),
                state.translate_price_in_per_mtok,
                state.translate_price_out_per_mtok,
            )
        } else if squelch_core::metrics::runs_notify_model(name) {
            (
                state.notify_model.as_ref(),
                state.notify_price_in_per_mtok,
                state.notify_price_out_per_mtok,
            )
        } else if name == squelch_core::metrics::SMART_CLEAR_USAGE_CATEGORY {
            // THE FOURTH ARM: smart clear's pass runs its own mid-sized model.
            // Left to fall through it would be labelled and billed as Stage-1.
            (
                state.smart_clear_model.as_ref(),
                state.smart_clear_price_in_per_mtok,
                state.smart_clear_price_out_per_mtok,
            )
        } else {
            (
                state.stage1_model.as_ref(),
                state.stage1_price_in_per_mtok,
                state.stage1_price_out_per_mtok,
            )
        };
        let (out_rows, totals) = rollup(rows.clone(), price_in, price_out);
        categories.insert(
            name.clone(),
            json!({ "model": model, "rows": out_rows, "totals": totals }),
        );
    }

    Ok(Json(json!({
        // Top-level fields are Stage-2 (backward-compatible shape).
        "rows": s2_out,
        "totals": s2_totals,
        "provider": state.stage2_provider.as_deref(),
        "model": state.stage2_model.as_ref(),
        "categories": categories,
    })))
}

// --- GET /client/mail-activity ----------------------------------------------

#[derive(Debug, Deserialize)]
pub struct MailActivityQuery {
    /// How many UTC days back from today, today included. Default 30.
    days: Option<u32>,
}

/// GET /client/mail-activity — what the mailbox itself did, per UTC day: mail
/// in and out, and how the incoming split by tier. The usage page draws it
/// beside the model spend so the two read against each other — spend per email,
/// signal share over time — and the days are keyed like the usage ledger's so
/// the two series line up column for column.
///
/// The window is `days` UTC days ENDING TODAY, today included, which is the
/// same shape a reader gets from `?days=` on `/client/usage`; `since`/`until`
/// in the body say exactly which days that was, so the client zero-fills the
/// gaps against the server's calendar rather than its own.
pub async fn get_mail_activity(
    State(state): State<ApiState>,
    Query(q): Query<MailActivityQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let days = q
        .days
        .unwrap_or(DEFAULT_USAGE_DAYS)
        .clamp(1, MAX_USAGE_DAYS);
    let today = Utc::now().date_naive();
    let since_day = today - chrono::Duration::days(i64::from(days) - 1);
    let since = since_day.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc();
    let until = (today + chrono::Duration::days(1))
        .and_hms_opt(0, 0, 0)
        .unwrap_or_default()
        .and_utc();
    let rows = store_call(&state, move |store, account_id| {
        store.mail_activity(account_id, since, until)
    })
    .await?;
    let rows: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "day": r.day,
                "received": r.received,
                "sent": r.sent,
                "sealed": r.sealed,
                "past_due": r.past_due,
                "deadline": r.deadline,
                "signal": r.signal,
                "noise": r.noise,
                "pending": r.pending,
            })
        })
        .collect();
    Ok(Json(json!({
        "days": days,
        "since": since_day.format("%Y-%m-%d").to_string(),
        "until": today.format("%Y-%m-%d").to_string(),
        "rows": rows,
    })))
}

// --- GET/POST /client/triage-config -----------------------------------------
//
// LLM-triage daily budget caps, configurable at runtime without a restart. POST
// persists an app_settings OVERRIDE that the Stage-2 pass re-reads at the start
// of each cycle. Precedence: override > config/env > default.

/// Trailing window (days) for the volume/usage averages on the endpoint.
const TRIAGE_CONFIG_TRAILING_DAYS: i64 = 14;

/// One cap's wire "source": an override row wins, else the config/env layer.
fn cap_source_str(
    has_override: bool,
    config_source: squelch_core::config::CapSource,
) -> &'static str {
    if has_override {
        "override"
    } else {
        config_source.as_str()
    }
}

/// The `/client/triage-config` response body: effective caps, per-cap sources,
/// trailing-14d averages, prices. Shared by GET and POST, which returns this
/// fresh shape after persisting.
async fn triage_config_body(state: &ApiState) -> Result<serde_json::Value, ApiError> {
    let now = Utc::now();
    let since = now - chrono::Duration::days(TRIAGE_CONFIG_TRAILING_DAYS);
    let since_day = since.format("%Y-%m-%d").to_string();

    let (overrides, inbound, usage, s1_usage) = store_call(state, move |store, account_id| {
        let overrides = store.stage2_cap_overrides(account_id)?;
        let inbound = store.count_inbound_since(account_id, since)?;
        let usage = store.stage2_usage_since(account_id, &since_day)?;
        let s1_usage = store.stage1_usage_since(account_id, &since_day)?;
        Ok((overrides, inbound, usage, s1_usage))
    })
    .await?;

    // Effective cap = override if present, else the config/env-layer value.
    let thread = overrides
        .thread_daily_cap
        .unwrap_or(state.stage2_thread_daily_cap);
    let sender = overrides
        .sender_daily_cap
        .unwrap_or(state.stage2_sender_daily_cap);
    let global = overrides
        .global_daily_cap
        .unwrap_or(state.stage2_global_daily_cap);
    let stage1_global = overrides
        .stage1_global_daily_cap
        .unwrap_or(state.stage1_global_daily_cap);

    let src = &state.stage2_cap_sources;
    let days = TRIAGE_CONFIG_TRAILING_DAYS as f64;
    let avg_inbound_per_day = inbound as f64 / days;
    let avg_stage2_calls_per_day = usage.calls as f64 / days;
    // Tokens/call are per-CALL means (null when the ledger has no calls yet).
    let per_call = |u: &squelch_core::store::Stage2Usage| {
        if u.calls == 0 {
            (serde_json::Value::Null, serde_json::Value::Null)
        } else {
            (
                json!(u.input_tokens as f64 / u.calls as f64),
                json!(u.output_tokens as f64 / u.calls as f64),
            )
        }
    };
    let (avg_tokens_in_per_call, avg_tokens_out_per_call) = per_call(&usage);
    let (s1_tokens_in_per_call, s1_tokens_out_per_call) = per_call(&s1_usage);

    Ok(json!({
        "thread_daily_cap": thread,
        "sender_daily_cap": sender,
        "global_daily_cap": global,
        "agent": {
            "budget_unit": "bounded_investigation",
            "daily_run_cap": state.triage_config.agent.daily_run_cap,
            "effective_daily_run_cap": state.triage_config.agent.daily_run_cap,
            "background_daily_run_cap": state.triage_config.agent.background_daily_run_cap,
            "effective_background_daily_run_cap": state.triage_config.agent.effective_background_daily_run_cap(),
            "access_daily_run_cap": state.triage_config.agent.access_daily_run_cap,
            "effective_access_daily_run_cap": state.triage_config.agent.effective_access_daily_run_cap(),
            "reserved_arrival_runs": state.triage_config.agent.reserved_arrival_runs(),
            "max_model_turns": state.triage_config.agent.max_model_turns,
            "legacy_stage_caps_are_active_ceilings": false,
        },
        "sources": {
            "thread_daily_cap": cap_source_str(overrides.thread_daily_cap.is_some(), src.thread_daily_cap),
            "sender_daily_cap": cap_source_str(overrides.sender_daily_cap.is_some(), src.sender_daily_cap),
            "global_daily_cap": cap_source_str(overrides.global_daily_cap.is_some(), src.global_daily_cap),
        },
        "avg_inbound_per_day": avg_inbound_per_day,
        "avg_stage2_calls_per_day": avg_stage2_calls_per_day,
        "avg_tokens_in_per_call": avg_tokens_in_per_call,
        "avg_tokens_out_per_call": avg_tokens_out_per_call,
        "price_in_per_mtok": state.stage2_price_in_per_mtok,
        "price_out_per_mtok": state.stage2_price_out_per_mtok,
        "stage2_model": state.stage2_model.as_ref(),
        // Stage-1 runs on every non-rule email and its only cap is GLOBAL.
        "stage1": {
            "model": state.stage1_model.as_ref(),
            "global_daily_cap": stage1_global,
            "source": cap_source_str(
                overrides.stage1_global_daily_cap.is_some(),
                src.stage1_global_daily_cap,
            ),
            "avg_calls_per_day": s1_usage.calls as f64 / days,
            "avg_tokens_in_per_call": s1_tokens_in_per_call,
            "avg_tokens_out_per_call": s1_tokens_out_per_call,
            "price_in_per_mtok": state.stage1_price_in_per_mtok,
            "price_out_per_mtok": state.stage1_price_out_per_mtok,
        },
    }))
}

pub async fn get_triage_config(
    State(state): State<ApiState>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(triage_config_body(&state).await?))
}

/// POST body: any subset of the caps. Fields are raw JSON values on purpose, so
/// a non-integer fails our own validation with the contractual 400 rather than
/// the extractor's 422.
#[derive(Debug, Deserialize)]
pub struct TriageConfigBody {
    #[serde(default)]
    thread_daily_cap: Option<serde_json::Value>,
    #[serde(default)]
    sender_daily_cap: Option<serde_json::Value>,
    #[serde(default)]
    global_daily_cap: Option<serde_json::Value>,
    /// The Stage-1 GLOBAL daily cap; same validation and 400 as the others.
    #[serde(default)]
    stage1_global_daily_cap: Option<serde_json::Value>,
}

pub async fn set_triage_config(
    State(state): State<ApiState>,
    Json(body): Json<TriageConfigBody>,
) -> Result<impl IntoResponse, ApiError> {
    // ALL-OR-NOTHING: every provided cap is validated before anything is
    // written, so a bad value persists nothing.
    let min = squelch_core::config::STAGE2_CAP_MIN as i64;
    let max = squelch_core::config::STAGE2_CAP_MAX as i64;
    let bad = || ApiError::bad_request(format!("each cap must be an integer in {min}..={max}"));
    let validate = |v: &serde_json::Value| -> Result<u32, ApiError> {
        // `as_i64` is `Some` only for a JSON integer (5.5 / "5" => None).
        let n = v.as_i64().ok_or_else(bad)?;
        if (min..=max).contains(&n) {
            Ok(n as u32)
        } else {
            Err(bad())
        }
    };

    let mut updates: Vec<(&'static str, u32)> = Vec::new();
    if let Some(v) = &body.thread_daily_cap {
        updates.push((
            squelch_core::config::APP_SETTING_THREAD_DAILY_CAP,
            validate(v)?,
        ));
    }
    if let Some(v) = &body.sender_daily_cap {
        updates.push((
            squelch_core::config::APP_SETTING_SENDER_DAILY_CAP,
            validate(v)?,
        ));
    }
    if let Some(v) = &body.global_daily_cap {
        updates.push((
            squelch_core::config::APP_SETTING_GLOBAL_DAILY_CAP,
            validate(v)?,
        ));
    }
    if let Some(v) = &body.stage1_global_daily_cap {
        updates.push((
            squelch_core::config::APP_SETTING_STAGE1_GLOBAL_DAILY_CAP,
            validate(v)?,
        ));
    }

    if !updates.is_empty() {
        let to_write = updates.clone();
        store_call(&state, move |store, account_id| {
            for (key, value) in &to_write {
                store.set_app_setting(account_id, key, &value.to_string())?;
            }
            Ok(())
        })
        .await?;

        // detail names the caps set, e.g. "thread=5,global=300".
        let detail = updates
            .iter()
            .map(|(key, value)| {
                let name = match *key {
                    k if k == squelch_core::config::APP_SETTING_THREAD_DAILY_CAP => "thread",
                    k if k == squelch_core::config::APP_SETTING_SENDER_DAILY_CAP => "sender",
                    k if k == squelch_core::config::APP_SETTING_STAGE1_GLOBAL_DAILY_CAP => {
                        "stage1_global"
                    }
                    _ => "global",
                };
                format!("{name}={value}")
            })
            .collect::<Vec<_>>()
            .join(",");
        audit_action(&state, "triage_config", None, &detail).await;
    }

    // Return the fresh GET shape (effective values after applying).
    Ok(Json(triage_config_body(&state).await?))
}

// --- ACTIONS: the ONLY write capability in the system -----------------------
//
// Non-negotiable gates: every body MUST carry `"confirm": true` (else 400);
// `send` runs the outbound secret guard (422 unless `"override_guard": true`);
// EVERY action, attempted or completed, appends an audit row. Without a write
// credential the action is a 403 — sync/triage/MCP never load the write token.

/// Actor written into the audit log for the human door's write actions. An
/// agent's writes carry `agent` instead (see `ApiState::audit_actor`).
pub(crate) const ACTION_ACTOR: &str = "client-api";

/// The `confirm` contract message returned on a missing/false confirm.
const CONFIRM_HINT: &str = "this action requires an explicit \"confirm\": true in the request body";

/// Append an audit row, best-effort: a failed insert is swallowed so it cannot
/// mask the action's own outcome.
pub(crate) async fn audit_action(
    state: &ApiState,
    action: &'static str,
    target: Option<String>,
    outcome: &str,
) {
    let entry = NewAuditEntry {
        actor: state.audit_actor.to_string(),
        action: action.to_string(),
        target,
        detail: Some(outcome.to_string()),
    };
    let _ = store_call(state, move |store, account_id| {
        store.append_audit(account_id, &entry)
    })
    .await;
}

/// RESOLUTION: mark a triage row `done` after a successful action. Best-effort,
/// so bookkeeping cannot mask the action's success; sealed rows are guarded in
/// the store, so this can never touch sealed mail.
async fn resolve_done(state: &ApiState, message_id: i64) {
    let _ = store_call(state, move |store, account_id| {
        store.set_attention_status(account_id, message_id, AttentionStatus::Done)
    })
    .await;
}

/// RESOLUTION, SENDER-WIDE: the same bookkeeping for an action that is a verdict
/// on a SENDER rather than on one message — unsubscribing, and a squelch rule.
///
/// Resolving only the thread the reader happened to have open leaves the rest of
/// that sender's mail in the bands, which is indistinguishable from the action
/// having failed: unsubscribe from a newsletter with nine emails in the window
/// and eight of them stay. Best-effort like `resolve_done`, for the same reason.
async fn resolve_sender_done(state: &ApiState, sender: &str) {
    let sender = sender.to_string();
    let _ = store_call(state, move |store, account_id| {
        store.resolve_sender(account_id, &sender)
    })
    .await;
}

/// Resolve the account's WRITE-bound mail writer, or 403 with a hint if no
/// write credential is configured.
///
/// THE WRITER IS THE ACCOUNT'S PROVIDER'S (`accounts.provider`, read on the
/// first call and kept in [`ApiState::mail_provider`]). The credential is
/// checked first, so a door with none configured answers exactly as it
/// always has without touching the store. A provider this build does not know
/// is refused, never written to as Gmail: see [`writer_unavailable`].
pub(crate) async fn write_client(state: &ApiState) -> Result<Arc<dyn MailWriter>, ApiError> {
    let Some(creds) = state.write_creds().cloned() else {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "write credential not configured; run `squelchd auth`",
        ));
    };
    let provider = match state.mail_provider.get() {
        Some(provider) => *provider,
        None => {
            // The inner `Result` is the provider read itself, mapped below
            // rather than by the store's generic mapping (which would call an
            // unknown provider the CALLER's 400).
            let read = store_call(state, |store, account_id| {
                Ok(store.account_provider(account_id))
            })
            .await?;
            let provider = read.map_err(writer_unavailable)?;
            *state.mail_provider.get_or_init(|| provider)
        }
    };
    match provider {
        // Over the state's one shared HTTP client (see `ApiState::write_http`);
        // what is built per action is two `Arc` clones and a base URL.
        MailProvider::Gmail => Ok(Arc::new(
            GmailWriteClient::with_http(
                creds,
                state.account_id,
                state
                    .write_api_base()
                    .map(str::to_string)
                    .unwrap_or_else(|| squelch_core::sync::GMAIL_API_BASE.to_string()),
                state.write_http(),
            )
            .with_budget(state.gmail_budget().cloned()),
        )),
    }
}

/// Why the account has no writer although a write credential is configured.
///
/// - A provider this build does not know (a database a newer build set up):
///   501, because this daemon cannot do it at all and an upgrade is the fix.
///   Not the credential's 403, which would send the user to re-authorize, and
///   not 503, which reads as "try again shortly".
/// - Anything else (the door's own account row gone, the store failing): the
///   opaque 500 every other store failure is.
fn writer_unavailable(e: CoreError) -> ApiError {
    match e {
        CoreError::InvalidInput(m) => {
            // The message names the stored value, which this code wrote: safe
            // to log, and what makes the line useful.
            eprintln!("squelch-api: no writer for this account: {m}");
            ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                "this account's mail provider is not supported by this build; upgrade squelchd",
            )
        }
        _ => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    }
}

/// The audit reason for a [`write_client`] refusal, by what refused: the
/// credential (403, the reason every caller has always recorded), a provider
/// this build cannot write to (501), or anything else (the store). Distinct so
/// a store blip is not recorded as "re-authorize".
pub(crate) fn write_refusal(e: &ApiError) -> &'static str {
    match e.status {
        StatusCode::FORBIDDEN => "rejected:no_write_credential",
        StatusCode::NOT_IMPLEMENTED => "rejected:unsupported_provider",
        _ => "failed:store",
    }
}

/// Map a [`WriteError`] to an [`ApiError`], never echoing upstream detail.
fn write_error(e: &WriteError) -> ApiError {
    match e {
        WriteError::MissingCredential(_) => ApiError::new(
            StatusCode::FORBIDDEN,
            "write credential not configured; run `squelchd auth`",
        ),
        WriteError::Invalid(m) => ApiError::bad_request(m.clone()),
        // Nothing reached Gmail: the mailbox is in a quota hold. 503 so the
        // app reads it as "not now" rather than "Gmail rejected this".
        WriteError::Paced { retry_in_secs } => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("Gmail is pacing this mailbox. Try again in {retry_in_secs}s."),
        ),
        WriteError::Api { .. } | WriteError::Transport(_) => {
            ApiError::new(StatusCode::BAD_GATEWAY, "gmail request failed")
        }
    }
}

/// Look up the action target for a local message id. UNFILTERED: a sealed or
/// restricted message resolves like any other (only another account's id or an
/// unknown one fails), so any sealed gate belongs to the caller.
pub(crate) async fn resolve_target(
    state: &ApiState,
    message_id: i64,
) -> Result<ActionMessageRef, ApiError> {
    store_call(state, move |store, account_id| {
        store.action_message_ref(account_id, message_id)
    })
    .await
}

/// Whether a successful action resolves the target's attention row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnSuccess {
    /// RESOLUTION: mark the target row done — the action handled the message.
    ResolveDone,
    /// Leave the attention row where it is (labeling is not handling).
    LeaveOpen,
}

/// The ladder every message-scoped action walks: confirm gate -> the caller's own
/// rejection -> write client -> target lookup (unfiltered, see `resolve_target`)
/// -> the Gmail call.
///
/// AUDIT STRINGS ARE A CONTRACT. Every exit appends a row under `action` with the
/// detail clients assert on — "rejected:confirm", the caller's `reject` detail,
/// "rejected:no_write_credential", "failed:target", "failed:gmail", "ok" — in that
/// order, so no action can quietly stop auditing an early return. `reject` is the
/// action's own pre-flight failure as (audit detail, response error), checked
/// immediately after the confirm gate.
async fn guarded_action<F, Fut>(
    state: &ApiState,
    action: &'static str,
    message_id: i64,
    confirm: bool,
    reject: Option<(&'static str, ApiError)>,
    on_success: OnSuccess,
    call: F,
) -> Result<(), ApiError>
where
    F: FnOnce(Arc<dyn MailWriter>, ActionMessageRef) -> Fut,
    Fut: std::future::Future<Output = Result<(), WriteError>>,
{
    let target = Some(message_id.to_string());

    if !confirm {
        audit_action(state, action, target, "rejected:confirm").await;
        return Err(ApiError::bad_request(CONFIRM_HINT));
    }
    if let Some((detail, err)) = reject {
        audit_action(state, action, target, detail).await;
        return Err(err);
    }

    let client = match write_client(state).await {
        Ok(c) => c,
        Err(e) => {
            audit_action(state, action, target, write_refusal(&e)).await;
            return Err(e);
        }
    };

    let msg = match resolve_target(state, message_id).await {
        Ok(m) => m,
        Err(e) => {
            audit_action(state, action, target, "failed:target").await;
            return Err(e);
        }
    };

    match call(client, msg).await {
        Ok(()) => {
            if on_success == OnSuccess::ResolveDone {
                resolve_done(state, message_id).await;
            }
            audit_action(state, action, target, "ok").await;
            Ok(())
        }
        Err(e) => {
            audit_action(state, action, target, "failed:gmail").await;
            Err(write_error(&e))
        }
    }
}

// --- POST /client/actions/archive -------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ArchiveBody {
    message_id: i64,
    #[serde(default)]
    confirm: bool,
}

pub async fn action_archive(
    State(state): State<ApiState>,
    Json(body): Json<ArchiveBody>,
) -> Result<impl IntoResponse, ApiError> {
    guarded_action(
        &state,
        "archive",
        body.message_id,
        body.confirm,
        None,
        // A successful archive auto-resolves the target row.
        OnSuccess::ResolveDone,
        |client, msg| async move { client.archive(&msg.gmail_msg_id).await },
    )
    .await?;
    Ok(Json(
        json!({ "status": "archived", "message_id": body.message_id }),
    ))
}

// --- POST /client/actions/not_spam ------------------------------------------

#[derive(Debug, Deserialize)]
pub struct NotSpamBody {
    message_id: i64,
    #[serde(default)]
    confirm: bool,
}

/// RESCUE ONE MESSAGE FROM THE PROVIDER'S SPAM FOLDER: remove Gmail's SPAM
/// label, put INBOX back, then clear the local flag and requeue the row for
/// triage.
///
/// GMAIL FIRST, LOCAL SECOND, and the order is the whole design. If the local
/// write went first and the Gmail call then failed, the message would be visible
/// in Passband and still sitting in the spam folder everywhere else — the client
/// would be lying about the state of the mailbox, and the next sync would not
/// correct it (nothing re-reads the SPAM label for a row it already has). With
/// this order a failed Gmail call leaves both sides untouched and the user sees
/// an error, which is the honest outcome. The reverse failure — Gmail moved, the
/// local write lost — self-heals: the message now carries INBOX, so the next
/// history walk ingests it and the upsert's `is_spam = MIN(...)` clears the flag.
///
/// It is the SPAM PAGE'S ONLY WRITE, deliberately. The other direction (marking
/// good mail as spam) is not offered: squelch has its own answer to unwanted
/// mail in sender rules and the noise tier, and teaching users to train Gmail's
/// filter from inside a client that does not read that filter's results would be
/// a button whose effects the product cannot show them.
pub async fn action_not_spam(
    State(state): State<ApiState>,
    Json(body): Json<NotSpamBody>,
) -> Result<impl IntoResponse, ApiError> {
    guarded_action(
        &state,
        "not_spam",
        body.message_id,
        body.confirm,
        None,
        // NOT ResolveDone: this mail is arriving, not being handled. `clear_spam`
        // puts the attention row back to 'new' so it lands in the New band.
        OnSuccess::LeaveOpen,
        |client, msg| async move { client.not_spam(&msg.gmail_msg_id).await },
    )
    .await?;

    // The local half. A `false` here means the row was already un-spammed (a
    // double click, or the sync beat us to it), which is not an error: Gmail
    // agrees with us either way, and the caller asked for a state that now holds.
    let message_id = body.message_id;
    store_call(&state, move |store, account_id| {
        store.clear_spam(account_id, message_id)
    })
    .await?;

    Ok(Json(
        json!({ "status": "not_spam", "message_id": body.message_id }),
    ))
}

// --- POST /client/actions/label ---------------------------------------------

#[derive(Debug, Deserialize)]
pub struct LabelBody {
    message_id: i64,
    #[serde(default)]
    add: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
    #[serde(default)]
    confirm: bool,
}

pub async fn action_label(
    State(state): State<ApiState>,
    Json(body): Json<LabelBody>,
) -> Result<impl IntoResponse, ApiError> {
    let message_id = body.message_id;
    // Checked after the confirm gate, exactly where the hand-written ladder had it.
    let empty = (body.add.is_empty() && body.remove.is_empty()).then(|| {
        (
            "rejected:empty",
            ApiError::bad_request("label requires a non-empty add or remove list"),
        )
    });

    guarded_action(
        &state,
        "label",
        message_id,
        body.confirm,
        empty,
        // Labeling a message is not handling it; the attention row stands.
        OnSuccess::LeaveOpen,
        |client, msg| async move {
            client
                .modify(&msg.gmail_msg_id, &body.add, &body.remove)
                .await
        },
    )
    .await?;
    Ok(Json(
        json!({ "status": "labeled", "message_id": message_id }),
    ))
}

// --- POST /client/actions/send ----------------------------------------------

/// SERIALIZE TOO, because a scheduled send stores this body as its frozen
/// `request` and the worker deserializes it back (docs/SCHEDULED-SEND.md §2):
/// every field this grows is scheduled for free. `None`s, `false`s and an
/// empty file list are skipped so the stored JSON stays small; each skipped
/// value is exactly its `#[serde(default)]`, so the round trip is exact.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct SendBody {
    /// Reply to a stored message (thread + recipient + subject derived from it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reply_to_message_id: Option<i64>,
    /// Forward a stored message: its raw bytes are re-fetched from Gmail and
    /// composed into a NEW conversation (no threading, no derived audience), with
    /// `body` riding along as the user's note above the quoted original. Mutually
    /// exclusive with `reply_to_message_id`, and `to` is required — nobody on the
    /// original is the person a forward is meant for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) forward_of_message_id: Option<i64>,
    /// Explicit recipient (overrides the reply default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) to: Option<String>,
    /// Explicit carbon copies, comma-joined.
    ///
    /// PRESENT — INCLUDING AS `""` — means the caller states the whole copy
    /// list and the parent's derived Cc is not consulted. That distinction is
    /// the whole point: a composer that shows a Cc field has to be able to
    /// EMPTY one, and a client that empties a field it was shown must not have
    /// the daemon quietly put the addresses back. ABSENT keeps the derivation,
    /// which is what every caller written before this field existed sends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cc: Option<String>,
    /// Reply to EVERYONE on the parent: `to` from its Reply-To/From, `cc` from
    /// the rest of its To/Cc minus this account. Requires
    /// `reply_to_message_id` — there is no audience to widen without a parent —
    /// and absent means today's single-recipient reply, so every existing caller
    /// is unchanged. See [`crate::write::mime::derive_reply_recipients`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) reply_all: bool,
    /// BLIND copies, comma-joined, for a send whose audience must not see each
    /// other. Never derived from anything (see [`crate::write::mime::ReplyParts`])
    /// and nothing ever will derive it — no header of a parent records who was
    /// blind-copied on it, which is what blind means, so absent and `""` are the
    /// same thing here, unlike `cc`. Filtered against `to`/`cc` before it
    /// composes so nobody is delivered two copies of one message.
    ///
    /// A send carrying ONLY `bcc` is legitimate — it is the whole shape of a bcc
    /// blast — and the `To` header then falls back to the account's own address
    /// rather than the send being refused. RFC822 wants a `To`, and every mail
    /// client writes the sender there for exactly this case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) bcc: Option<String>,
    /// ADDRESS A SEND GROUP. What it does depends on the group's own mode, which
    /// is a property of the audience rather than of this message:
    ///
    /// * `to` / `bcc` — the composer has ALREADY expanded the membership into
    ///   `to`/`bcc`, so this is attribution only: the send goes out as one
    ///   message and is recorded against the group afterwards.
    /// * `individual` — the daemon fans out, one message per member, and the
    ///   response is a `202` naming the batch rather than a sent message.
    ///
    /// Another account's id and an unknown id are the same 404.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) group_id: Option<i64>,
    /// THE GROUP'S MODE AS IT WAS SCHEDULED, stamped by the schedule route
    /// (never taken from a client: that route overwrites it) and read ONLY
    /// when the worker fires the row, for attribution: the frozen `to`/`bcc`
    /// lists were addressed under this mode, and a group flipped since must
    /// not make the record read the other, possibly empty, field. Ignored on
    /// the live route, which resolves the group's mode as it sends. Absent on
    /// rows scheduled before it existed, which fall back to the group's
    /// current mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) group_mode: Option<GroupMode>,
    /// Explicit subject (overrides the reply-derived subject).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) subject: Option<String>,
    pub(crate) body: String,
    /// `"markdown"` = `body` is markdown source: it goes out verbatim as the
    /// text/plain part with a server-rendered HTML alternative beside it. The
    /// guard scans `body` either way — the HTML is derived from nothing else.
    /// Absent/other = today's single-part plain send, byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) body_format: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) confirm: bool,
    /// Override the outbound secret guard (still audited).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) override_guard: bool,
    /// The local draft this send consumes, discarded once the mail is away. Only
    /// a successful send clears it — every rejection leaves the composition
    /// recoverable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) draft_id: Option<i64>,
    /// Ask for a read-tracking pixel on this send. Absent is `false`, so every
    /// existing caller stays untracked. Honored ONLY when `[tracking] base_url`
    /// is configured; without it the send goes out untracked rather than failing
    /// — the client learns tracking is unavailable from
    /// `GET /client/tracking-config`, not from a refused send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) include_tracker: Option<bool>,
    /// The staged files this send carries (see `/client/compose/attachments`),
    /// in tray order. Every id must still resolve or the send is refused: the
    /// tray the sender reviewed is the contract. Which of them go INLINE is not
    /// stated here — a file is inline exactly when the body references its
    /// `cid:`, decided once against the rendered html (`mark_inline`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) attachment_ids: Vec<i64>,
}

/// ECHO the just-sent message into the local store so the thread view shows the
/// reply without waiting for the next Sent poll: fetch it back `format=raw` on
/// the write token, decode, ingest with `is_sent: true`.
///
/// STRICTLY BEST-EFFORT — the send has already succeeded, so every failure path
/// returns `None` after auditing; none of them may fail the request. Logs carry
/// IDS ONLY, never message content. Returns the local message id on success.
///
/// AUDIT (contract, docs/SECURITY.md §5): action `send.echo`, detail
/// `skipped:no_id` | `skipped:sealed` | `failed:fetch` | `failed:ingest` |
/// `ok:<local id>`, each marked `scheduled:<id>:` when `fire` names the
/// outbox row the mail went out from (see [`fired`]), as its `send` row is.
pub(crate) async fn echo_sent(
    state: &ApiState,
    client: &dyn MailWriter,
    target: Option<String>,
    sent: &SentRef,
    fire: Option<Fire<'_>>,
) -> Option<i64> {
    let echo_audit = |detail: &str| fired(fire, detail);
    let gmail_msg_id = match sent.id.as_deref() {
        Some(id) => id.to_string(),
        // Gmail echoed no id: nothing to fetch back.
        None => {
            audit_action(state, "send.echo", target, &echo_audit("skipped:no_id")).await;
            return None;
        }
    };

    // TIMEOUT BUDGET: the Swift client's POST timeout is 15s and `action_send` has
    // already spent two serial Gmail calls (parent headers, then the send) before
    // getting here. The echo is bookkeeping, so it may not spend what is left —
    // past 5s it gives up and the reply lands on the next backfill instead.
    let fetched = match tokio::time::timeout(
        Duration::from_secs(ECHO_FETCH_TIMEOUT_SECS),
        client.fetch_raw(&gmail_msg_id),
    )
    .await
    {
        Ok(Ok(f)) => f,
        Ok(Err(e)) => {
            eprintln!("squelch-api: echo of sent message {gmail_msg_id} failed to fetch: {e}");
            audit_action(state, "send.echo", target, &echo_audit("failed:fetch")).await;
            return None;
        }
        Err(_elapsed) => {
            eprintln!("squelch-api: echo of sent message {gmail_msg_id} timed out");
            audit_action(state, "send.echo", target, &echo_audit("failed:fetch")).await;
            return None;
        }
    };
    let raw = match decode_raw_b64url(&fetched.raw_b64) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("squelch-api: echo of sent message {gmail_msg_id} failed to decode: {e}");
            audit_action(state, "send.echo", target, &echo_audit("failed:fetch")).await;
            return None;
        }
    };
    // A `format=raw` read that carried no `raw` field decodes to zero bytes, which
    // would ingest as a message with no headers and no body. Nothing to echo.
    if raw.is_empty() {
        eprintln!("squelch-api: echo of sent message {gmail_msg_id} returned no bytes");
        audit_action(state, "send.echo", target, &echo_audit("failed:fetch")).await;
        return None;
    }

    let thread_id = fetched.thread_id.clone().or_else(|| sent.thread_id.clone());
    let internal_date = parse_internal_date(fetched.internal_date.as_deref());
    // Kept back for the log line; the closure takes ownership of the id itself.
    let log_id = gmail_msg_id.clone();
    let ingested = store_call(state, move |store, account_id| {
        let account_addr = store.account_email(account_id)?;
        squelch_core::sync::ingest::ingest_sent(
            store,
            account_id,
            &account_addr,
            &gmail_msg_id,
            thread_id,
            raw,
            internal_date,
            Utc::now(),
        )
    })
    .await;

    match ingested {
        Ok(Some(local_id)) => {
            audit_action(
                state,
                "send.echo",
                target,
                &echo_audit(&format!("ok:{local_id}")),
            )
            .await;
            Some(local_id)
        }
        // The outbound copy tripped seal detection, so core committed nothing: a
        // sealed row in the thread would 404 the whole thread the user is reading.
        // The reply shows up on the next backfill instead.
        Ok(None) => {
            audit_action(state, "send.echo", target, &echo_audit("skipped:sealed")).await;
            None
        }
        Err(_) => {
            eprintln!("squelch-api: echo of sent message {log_id} failed to ingest");
            audit_action(state, "send.echo", target, &echo_audit("failed:ingest")).await;
            None
        }
    }
}

/// Discard the draft a successful send consumed. Best-effort and IDS ONLY in the
/// log line: the mail has already left, so a failed cleanup must not surface as
/// a send failure. A `false` result is a stale id, not an error.
async fn discard_sent_draft(state: &ApiState, draft_id: i64) {
    if store_call(state, move |store, account_id| {
        store.delete_draft(account_id, draft_id)
    })
    .await
    .is_err()
    {
        eprintln!("squelch-api: draft {draft_id} outlived its send; discard failed");
    }
}

/// Mint and persist this send's read-tracking token, returning
/// `(token, pixel_url)`.
///
/// `None` covers every reason a send goes out untracked: the client did not ask,
/// no `[tracking] base_url` is configured, or the tracker row would not persist.
/// THE ROW COMES FIRST BY DESIGN — a pixel whose token is not in
/// `send_trackers` records nothing, so it must never reach the wire.
///
/// AUDIT: action `send.tracker`, detail `minted` | `failed:entropy` |
/// `failed:store`. The token itself is never audited and never logged.
async fn mint_tracker(
    state: &ApiState,
    requested: bool,
    target: Option<String>,
) -> Option<(String, String)> {
    if !requested {
        return None;
    }
    let base = state.tracking_base_url()?;
    let token = match squelch_core::tracking::mint_token() {
        Ok(t) => t,
        Err(_) => {
            audit_action(state, "send.tracker", target, "failed:entropy").await;
            return None;
        }
    };
    let created_at = Utc::now().timestamp();
    let stored = token.clone();
    if store_call(state, move |store, account_id| {
        store.insert_send_tracker(account_id, &stored, None, created_at)
    })
    .await
    .is_err()
    {
        audit_action(state, "send.tracker", target, "failed:store").await;
        return None;
    }
    let pixel_url = format!("{base}/t/{token}");
    audit_action(state, "send.tracker", target, "minted").await;
    Some((token, pixel_url))
}

/// Drop a tracker whose send never reached the wire.
///
/// The mint has to come first (see [`mint_tracker`]), so a send that fails after
/// it leaves a row nothing will ever link to a message: its opens could never be
/// read back, and it would sit in `send_trackers` for the life of the database.
/// BEST-EFFORT — the token is already dead either way.
///
/// AUDIT: action `send.tracker`, detail `discarded` | `failed:discard`.
async fn discard_tracker(state: &ApiState, token: &str, target: Option<String>) {
    let token = token.to_string();
    let detail = match store_call(state, move |store, account_id| {
        store.delete_send_tracker(account_id, &token)
    })
    .await
    {
        Ok(_) => "discarded",
        Err(_) => "failed:discard",
    };
    audit_action(state, "send.tracker", target, detail).await;
}

/// Point a send's tracker at the echoed local copy, which is what makes
/// `GET /client/messages/{id}/opens` able to find it. BEST-EFFORT: the mail is
/// away and the pixel already works, so a failure only leaves opens recorded
/// against a tracker no message names yet.
async fn link_tracker(state: &ApiState, token: &str, message_id: i64) {
    let token = token.to_string();
    if store_call(state, move |store, account_id| {
        store.set_send_tracker_message(account_id, &token, message_id)
    })
    .await
    .is_err()
    {
        eprintln!("squelch-api: sent message {message_id} was not linked to its tracker");
    }
}

/// The outbound guard's verdict on a set of REDACTED match kinds: `Some` is the
/// 422 that stops the send, `None` clears it (writing the `guard_override` row on
/// the way past when there was something to override).
///
/// ONE VERDICT PER SEND. A forward unions its note's kinds with the original's
/// before calling this, so the user is asked once and overrides once — two
/// separate blocks on one composition would read as the first override having
/// been ignored.
///
/// `action` is the audit action both rows are written under: `send`, or
/// `schedule` for the verdict a scheduled send is given when it is queued.
pub(crate) async fn guard_verdict(
    state: &ApiState,
    action: &'static str,
    matches: &[&'static str],
    override_guard: bool,
    target: Option<String>,
) -> Option<ApiError> {
    if matches.is_empty() {
        return None;
    }
    if !override_guard {
        audit_action(state, action, target, "blocked:guard").await;
        return Some(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: format!(
                "outbound guard blocked send; matched (redacted) kinds: {}. \
                 resend with \"override_guard\": true to send anyway",
                matches.join(", ")
            ),
            code: None,
        });
    }
    // Overridden: record that the guard was bypassed (kinds only).
    audit_action(
        state,
        action,
        target,
        &format!("guard_override:{}", matches.join(",")),
    )
    .await;
    None
}

/// Everything that happens AFTER a send's bytes are built: the Gmail call, and
/// the bookkeeping on either side of it.
///
/// ONE copy, shared by the reply/new path and the forward path. The two had
/// already drifted — the reply answered `thread_id.or(sent.thread_id)` and the
/// forward answered `sent.thread_id`, which are the same answer only for as long
/// as a forward's `thread_id` stays `None` — and that is the cheap kind of drift
/// that a second reader has no way to spot as deliberate.
///
/// Parameterized by the four things that genuinely differ:
/// - `thread_id`: `Some` threads a reply onto its parent. A FORWARD PASSES
///   `None`, which is what makes it a new conversation.
/// - `resolve`: the parent message a successful reply marks handled. A forward
///   passes `None` — passing a message on to someone else is not handling it,
///   and the attention row is the only place the user's own outstanding action
///   is remembered.
/// - `outcome`: the audit detail (`ok`, `ok:reply_all:N`, `ok:forward`).
/// - `tracker` / `draft_id`: this send's minted pixel and the draft it consumes.
/// - `fire`: the scheduled send being fired, which marks the failure's ledger
///   row the way [`fired_outcome`] already marked `outcome`.
///
/// THE ORDER IS THE CONTRACT: resolve, discard the draft, audit the outcome,
/// echo, link the tracker, answer. Everything after the audit line is
/// bookkeeping that CANNOT fail the request — the mail has already left. The
/// reply path's bytes, audit rows and response body are what they were before
/// this extraction; the existing reply tests are the proof.
#[allow(clippy::too_many_arguments)]
async fn finish_send(
    state: &ApiState,
    client: &dyn MailWriter,
    raw: &[u8],
    thread_id: Option<String>,
    resolve: Option<i64>,
    outcome: String,
    tracker: Option<(String, String)>,
    draft_id: Option<i64>,
    attachment_ids: &[i64],
    target: Option<String>,
    fire: Option<Fire<'_>>,
) -> Result<SendOutcome, SendFailure> {
    match client.send(raw, thread_id.as_deref()).await {
        Ok(sent) => {
            if let Some(id) = resolve {
                resolve_done(state, id).await;
            }
            if let Some(draft_id) = draft_id {
                discard_sent_draft(state, draft_id).await;
            }
            discard_sent_attachments(state, draft_id, attachment_ids.to_vec()).await;
            audit_action(state, "send", target.clone(), &outcome).await;
            let echo_message_id = echo_sent(state, client, target, &sent, fire).await;
            if let (Some((token, _)), Some(message_id)) = (&tracker, echo_message_id) {
                link_tracker(state, token, message_id).await;
            }
            // A send with no thread of its own — a cold send, or a forward —
            // falls back to the thread Gmail just created, so the client can
            // open what it just sent.
            Ok(SendOutcome::Sent {
                echo_message_id,
                thread_id: thread_id.or(sent.thread_id),
            })
        }
        Err(e) => {
            if let Some((token, _)) = &tracker {
                discard_tracker(state, token, target.clone()).await;
            }
            // The ledger says what the row will: `paced` when the budget held
            // the call (nothing posted), `failed:gmail` for Gmail's answer.
            let failure = SendFailure::after_post("failed:gmail", &e);
            audit_action(state, "send", target, &fired(fire, failure.reason)).await;
            Err(failure)
        }
    }
}

/// What a send that got past every refusal answered, as data rather than as
/// the response body, so a caller outside a request (the scheduled-send
/// worker, docs/SCHEDULED-SEND.md §4) can settle its row from the same values
/// `POST /client/actions/send` puts on the wire. [`SendOutcome::to_json`] is
/// that wire shape, key for key.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SendOutcome {
    /// One message, away. `echo_message_id` is the local copy when the echo
    /// landed; `thread_id` is the parent's, or the one Gmail just created.
    Sent {
        echo_message_id: Option<i64>,
        thread_id: Option<String>,
    },
    /// An `individual` group fan-out, handed to its job (the `202`).
    Sending {
        group_send_id: i64,
        recipients: usize,
    },
}

impl SendOutcome {
    /// The JSON `action_send` answers with. THE BYTES ARE THE CONTRACT: the
    /// Swift client decodes these exact keys, and the send tests pin them.
    pub(crate) fn to_json(&self) -> serde_json::Value {
        match self {
            SendOutcome::Sent {
                echo_message_id,
                thread_id,
            } => json!({
                "status": "sent",
                "echo_message_id": echo_message_id,
                "thread_id": thread_id,
            }),
            SendOutcome::Sending {
                group_send_id,
                recipients,
            } => json!({
                "status": "sending",
                "group_send_id": group_send_id,
                "recipients": recipients,
            }),
        }
    }
}

/// Why a send did not go out: the `ApiError` the request path answers with,
/// UNCHANGED, plus a REDACTED reason for a caller with no response to write
/// it into (a scheduled row's `last_error`).
///
/// `reason` is the audit detail the refusal already wrote (`failed:gmail`,
/// `rejected:no_write_credential`, ...), so the ledger and the row say the same
/// thing. The few refusals that write no audit row (a group that no longer
/// resolves, a store read failing) still get a reason; it is simply not in the
/// ledger. NEVER content, only these fixed strings.
#[derive(Debug)]
pub(crate) struct SendFailure {
    pub(crate) reason: &'static str,
    pub(crate) error: ApiError,
    /// The mail MAY BE IN GMAIL: the failure came back after `messages.send`
    /// was posted (a transport error, or a 5xx), so Gmail may hold the
    /// message and only the answer was lost. `false` for everything refused
    /// before the post (validation, no credential, the parent read, pacing)
    /// and for a 4xx, which Gmail answers without sending. The scheduled-send
    /// worker asks Gmail by Message-ID before it re-fires a row whose failure
    /// said `true` (docs/SCHEDULED-SEND.md §4).
    pub(crate) maybe_sent: bool,
    /// Gmail's budget was pacing the mailbox and nothing was posted: "not
    /// now", not "this mail failed". The worker retries without spending an
    /// attempt.
    pub(crate) paced: bool,
}

impl SendFailure {
    fn new(reason: &'static str, error: ApiError) -> Self {
        Self {
            reason,
            error,
            maybe_sent: false,
            paced: false,
        }
    }

    /// Gmail's budget held a call on the way to the send (the send itself, a
    /// forward's original, a reply's parent): nothing was posted, and the
    /// reason is `paced` wherever it happened, so the ledger, the row and
    /// §4.5's "no attempt spent" all agree.
    fn paced(e: &WriteError) -> Self {
        let mut f = Self::new(PACED, write_error(e));
        f.paced = true;
        f
    }

    /// A failure answered by `messages.send` itself, classified by whether
    /// the post may have landed. A paced send is [`Self::paced`], whatever
    /// `reason` the caller had for a real failure.
    fn after_post(reason: &'static str, e: &WriteError) -> Self {
        if matches!(e, WriteError::Paced { .. }) {
            return Self::paced(e);
        }
        let mut f = Self::new(reason, write_error(e));
        match e {
            // A transport error can come after Gmail took the whole body (a
            // timeout on the answer, a reset, an undecodable 2xx), and a 5xx
            // is Google saying it does not know how it went.
            WriteError::Transport(_) => f.maybe_sent = true,
            WriteError::Api { status, .. } if *status >= 500 => f.maybe_sent = true,
            WriteError::Paced { .. } => {}
            // A 4xx is refused before anything is sent; no credential and bad
            // input never reached the post at all.
            WriteError::Api { .. } | WriteError::MissingCredential(_) | WriteError::Invalid(_) => {}
        }
        f
    }
}

/// The reason (and audit detail) of a send Gmail's budget held: nothing posted.
pub(crate) const PACED: &str = "paced";

/// The request path answers with the error as it always has; the reason was
/// already written to the ledger where there is one.
impl From<SendFailure> for ApiError {
    fn from(f: SendFailure) -> Self {
        f.error
    }
}

/// What [`perform_send`] still owes the outbound guard.
///
/// A plain send is judged BEFORE it gets there (by `action_send`, or at
/// schedule time), so it owes nothing. A FORWARD'S VERDICT WAITS on the
/// original, which is only read inside the send: most of what it sends is
/// somebody else's mail. That is the one case the core itself must judge.
#[derive(Debug)]
pub(crate) enum SendGuard {
    /// Judge a forward once, after its original is fetched, over these kinds
    /// (the note's and the staged files') unioned with the original's. The
    /// request path.
    ForwardOwed(Vec<&'static str>),
    /// Already judged, and `override_guard` already honored where it applied.
    /// NOTHING RE-SCANS: the scheduled-send worker fires a frozen request
    /// whose answer was given when the user was there to give it.
    Settled,
}

/// A scheduled send being FIRED by the worker (docs/SCHEDULED-SEND.md §4): the
/// row it came from and the `Message-ID` minted when it was scheduled. Every
/// fired send carries both that id and its outbox token ([`outbox_token`], the
/// `X-Passband-Outbox` header recovery reads back); an interactive send
/// carries neither.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Fire<'a> {
    pub(crate) id: i64,
    pub(crate) message_id: &'a str,
}

/// The success ledger detail for a send, marked when it was a fired scheduled
/// send: `scheduled:<id>:<outcome>` (`scheduled:7:ok`,
/// `scheduled:7:ok:forward`, `scheduled:7:ok:bcc:3`).
///
/// ONE ROW PER SEND. The worker does not write a second `send` row of its own:
/// the one `finish_send` writes carries the marker, so the ledger neither
/// double-counts a scheduled mail nor loses the reach counts (`bcc:N`,
/// `reply_all:N`) the plain detail records.
fn fired_outcome(fire: Option<Fire<'_>>, outcome: String) -> String {
    match fire {
        Some(f) => format!("scheduled:{}:{outcome}", f.id),
        None => outcome,
    }
}

/// A send's REFUSAL detail, marked the same way when it was a fired scheduled
/// send (`scheduled:7:failed:gmail`), so the ledger says which outbox row a
/// failure belongs to and a plain send's failure reads exactly as before.
fn fired(fire: Option<Fire<'_>>, reason: &str) -> String {
    fired_outcome(fire, reason.to_string())
}

/// AUDIT TARGET for a send: the stored message it is ABOUT — the original for
/// a forward, the parent for a reply, nothing at all for a cold send.
pub(crate) fn send_target(body: &SendBody) -> Option<String> {
    body.forward_of_message_id
        .or(body.reply_to_message_id)
        .map(|id| id.to_string())
}

/// The ordered refusals every send passes before anything is read or spent:
/// confirm, not both reply and forward, a body, a forward's `to`, reply-all's
/// parent, no unresolved `#group`, no group on a reply or forward.
///
/// PURE, so it can run anywhere a `SendBody` is judged: `action_send` now,
/// and scheduling (docs/SCHEDULED-SEND.md §3 step 1), which audits under its
/// own action. The caller writes the audit row from `reason`; THE ORDER IS
/// PART OF THE CONTRACT, because it decides which refusal a body with two
/// problems gets.
pub(crate) fn validate_send(body: &SendBody) -> Result<(), SendFailure> {
    if !body.confirm {
        return Err(SendFailure::new(
            "rejected:confirm",
            ApiError::bad_request(CONFIRM_HINT),
        ));
    }
    // A send is a reply OR a forward, never both: they disagree about threading,
    // about the subject, and about whose conversation the mail joins.
    if body.forward_of_message_id.is_some() && body.reply_to_message_id.is_some() {
        return Err(SendFailure::new(
            "rejected:forward_and_reply",
            ApiError::bad_request("a send is either a reply or a forward, not both"),
        ));
    }
    // AN EMPTY NOTE IS A LEGITIMATE FORWARD — "here, look at this" is most of
    // what forwarding is for, and the original is the body. Every other send
    // still needs something typed in it.
    if body.forward_of_message_id.is_none() && body.body.trim().is_empty() {
        return Err(SendFailure::new(
            "rejected:empty_body",
            ApiError::bad_request("send requires a non-empty body"),
        ));
    }
    // A forward derives NOTHING about its audience from the original: everyone
    // on it is someone the mail already reached. The recipient is the one thing
    // only the user knows.
    if body.forward_of_message_id.is_some()
        && body.to.as_deref().map(str::trim).unwrap_or("").is_empty()
    {
        return Err(SendFailure::new(
            "rejected:no_recipient",
            ApiError::bad_request("forward requires `to`"),
        ));
    }
    // Reply-all has nothing to widen without a parent to read the audience off.
    // Checked here, with the other body validations, so it costs no Gmail call.
    // (This is also what refuses reply_all on a forward: no parent, no widening.)
    if body.reply_all && body.reply_to_message_id.is_none() {
        return Err(SendFailure::new(
            "rejected:reply_all_no_parent",
            ApiError::bad_request("reply_all requires `reply_to_message_id`"),
        ));
    }

    // AN UNRESOLVED GROUP TOKEN NEVER REACHES THE WIRE. Checked before the guard
    // so it costs nothing, and checked at all because the quiet failure is the
    // dangerous one: `parse_addr_list` would drop `#preseed investors` as
    // unemittable and send to everyone else on the line, silently missing the
    // audience the user believed they had addressed.
    for list in [body.to.as_deref(), body.bcc.as_deref()]
        .into_iter()
        .flatten()
    {
        if let Err(e) = group_send::reject_unresolved_tokens(list) {
            return Err(SendFailure::new("rejected:unresolved_group", e));
        }
    }

    // A GROUP IS AN AUDIENCE, and a reply or a forward already has one. Blasting
    // a list into someone else's thread is not a thing this composer can express
    // and not a thing the daemon should invent a meaning for.
    if body.group_id.is_some()
        && (body.reply_to_message_id.is_some() || body.forward_of_message_id.is_some())
    {
        return Err(SendFailure::new(
            "rejected:group_on_reply",
            ApiError::bad_request(
                "a group is addressed by a new message, not by a reply or a forward",
            ),
        ));
    }
    Ok(())
}

/// `POST /client/actions/send`: validate, read the staged files, judge, send.
///
/// The judging stays HERE and out of [`perform_send`], because the scheduled
/// path judges at schedule time and must never be asked again at fire time.
pub async fn action_send(
    State(state): State<ApiState>,
    Json(body): Json<SendBody>,
) -> Result<impl IntoResponse, ApiError> {
    let target = send_target(&body);

    if let Err(f) = validate_send(&body) {
        audit_action(&state, "send", target, f.reason).await;
        return Err(f.into());
    }

    // OUTBOUND GUARD: report only REDACTED kinds, never the matched text. Scanned
    // here, before any Gmail spend. A FORWARD'S VERDICT WAITS — most of what it
    // sends is the original, which has not been read yet — so its kinds are
    // unioned in and judged once, in `forward_send`.
    //
    // ONE VERDICT FOR A FAN-OUT TOO: it is one composition, so it is asked about
    // once here and never again per recipient.
    //
    // THE FILES ARE READ FIRST, so a send naming a file that is gone is refused
    // before the guard spends anything — and so the guard can read them: a
    // text attachment is scanned exactly as a forward's would be.
    let attachments = load_send_attachments(
        &state,
        "send",
        &body.attachment_ids,
        body.draft_id,
        target.clone(),
    )
    .await?;
    let matches = composition_kinds(&body.body, &attachments);
    let owed = if body.forward_of_message_id.is_some() {
        SendGuard::ForwardOwed(matches)
    } else {
        if let Some(err) = guard_verdict(
            &state,
            "send",
            &matches,
            body.override_guard,
            target.clone(),
        )
        .await
        {
            return Err(err);
        }
        SendGuard::Settled
    };

    let outcome = perform_send(&state, &body, attachments, owed, None).await?;
    // 200 for a fan-out too, as it always has been: the Groups page reads
    // `status: "sending"`, not the code.
    Ok(Json(outcome.to_json()))
}

/// Everything a send does once it has been validated and judged: resolve the
/// write client, derive the audience, subject and threading from the parent,
/// mint the tracker, render, build the RFC822, send, and settle
/// ([`finish_send`]). Shared by `action_send` and the scheduled-send worker,
/// which is why it takes no request and writes no response.
///
/// `attachments` are the files [`load_send_attachments`] resolved for
/// `body.attachment_ids`. `owed` is what the outbound guard is still owed (see
/// [`SendGuard`]). `fire` is the scheduled send being fired: its minted
/// `Message-ID` and its `X-Passband-Outbox` token are written onto the
/// outgoing message, and its row id marks the success ledger row (see
/// [`Fire`]); `None` lets Gmail mint the id and writes no token, which is
/// every send made while the user waits.
///
/// Dispatch: a FORWARD goes to [`forward_send`] (unthreaded, reads the
/// original). An `individual` GROUP goes to [`fan_out_send`], which answers
/// [`SendOutcome::Sending`], but NEVER WHEN FIRED: a fired send's group is
/// attribution only (one id cannot name N messages; scheduling refuses a
/// fan-out for v1, and the group may have changed since). Everything else, a
/// reply or a cold send, possibly attributed to a group, is composed here.
///
/// Audit rows, the attachment-scaled Gmail timeout and the echo budget are
/// exactly what `action_send` had before this was pulled out of it.
pub(crate) async fn perform_send(
    state: &ApiState,
    body: &SendBody,
    attachments: Vec<MailAttachment>,
    owed: SendGuard,
    fire: Option<Fire<'_>>,
) -> Result<SendOutcome, SendFailure> {
    let target = send_target(body);
    let message_id = fire.map(|f| f.message_id);

    let client = match write_client(state).await {
        Ok(c) => c,
        Err(e) => {
            let reason = write_refusal(&e);
            audit_action(state, "send", target, &fired(fire, reason)).await;
            return Err(SendFailure::new(reason, e));
        }
    };

    if let Some(original_id) = body.forward_of_message_id {
        return forward_send(
            state,
            body,
            &*client,
            target,
            original_id,
            owed,
            attachments,
            fire,
        )
        .await;
    }

    // A GROUP SEND resolves its audience here, once, after the guard has cleared
    // and the write credential is known good — so a fan-out that is going to be
    // refused is refused before any of it has gone.
    //
    // A FIRED SCHEDULED SEND IS DIFFERENT: its `group_id` is ATTRIBUTION ONLY.
    // The composer addressed its `to`/`bcc` when it was scheduled and those
    // lists are frozen in the request, so the group's CURRENT state decides
    // nothing about the send. A group flipped to `individual` since is still
    // attributed, not fanned out (scheduling refused a fan-out, and one minted
    // Message-ID cannot name N messages); a group deleted since costs only the
    // history entry, never the mail.
    let audience = match (body.group_id, fire) {
        (Some(group_id), None) => Some(
            group_send::resolve(state, group_id)
                .await
                .map_err(|e| SendFailure::new("failed:group", e))?,
        ),
        (Some(group_id), Some(f)) => match group_send::resolve(state, group_id).await {
            Ok(audience) => Some(audience),
            Err(_) => {
                eprintln!(
                    "squelch-api: scheduled send {} names group {group_id}, which no longer \
                     resolves; sending unattributed",
                    f.id
                );
                None
            }
        },
        (None, _) => None,
    };
    if fire.is_none()
        && let Some(audience) = &audience
        && audience.mode == GroupMode::Individual
    {
        return fan_out_send(state, client, body, audience, attachments).await;
    }

    let (parent, thread_id) = match body.reply_to_message_id {
        Some(id) => match resolve_target(state, id).await {
            Ok(m) => {
                let tid = m.thread_id.clone();
                (Some(m), Some(tid))
            }
            Err(e) => {
                audit_action(state, "send", target, &fired(fire, "failed:target")).await;
                return Err(SendFailure::new("failed:target", e));
            }
        },
        None => (None, None),
    };

    // The parent's headers come from Gmail on the WRITE token (gmail.modify
    // grants read), never the read credential. ONE fetch: it carries both the
    // threading ids and the address lists reply-all derives from.
    let parent_hdrs = match &parent {
        Some(p) => match client.parent_headers(&p.gmail_msg_id).await {
            Ok(h) => Some(h),
            // PACED IS "NOT NOW", for a plain reply too: the send right behind
            // this read would be held by the same budget, and degrading to an
            // unthreaded reply with no derived audience because Gmail asked
            // us to wait would be the wrong mail, not a late one. A fired
            // reply hands its attempt back.
            Err(e @ WriteError::Paced { .. }) => {
                audit_action(state, "send", target, &fired(fire, PACED)).await;
                return Err(SendFailure::paced(&e));
            }
            // Threading is best-effort — an unthreaded reply still reaches the
            // right person. REPLY-ALL IS NOT: the audience IS the request, and
            // quietly falling back to reply-to-sender would send to fewer people
            // than the user reviewed. Fail loudly instead.
            Err(_) if body.reply_all => {
                audit_action(state, "send", target, &fired(fire, "failed:gmail")).await;
                return Err(SendFailure::new(
                    "failed:gmail",
                    ApiError::new(
                        StatusCode::BAD_GATEWAY,
                        "could not read the original message's recipients; \
                         reply-all was not sent",
                    ),
                ));
            }
            Err(_) => None,
        },
        None => None,
    };
    let (in_reply_to, references) = match &parent_hdrs {
        Some(h) => {
            let refs = build_references(h.message_id.as_deref(), h.references.as_deref());
            (h.message_id.clone(), refs)
        }
        None => (None, None),
    };

    // The reply's derived audience, bare addresses only (display names
    // dropped). Derived for EVERY reply with readable headers, not just
    // reply-all: a plain reply must honor the parent's Reply-To, and the
    // preview route runs this same derivation — "what was previewed is what is
    // sent" only holds if the send derives too.
    let derived = match &parent_hdrs {
        Some(h) => {
            // Whose address to leave off the copy list. Unreadable (which the
            // account row makes near-impossible) only costs self-exclusion: the
            // user is copied on their own reply rather than the send failing.
            let own = store_call(state, |store, account_id| store.account_email(account_id))
                .await
                .unwrap_or_default();
            Some(derive_reply_recipients(h, &own, body.reply_all))
        }
        None => None,
    };

    // PRECEDENCE: an explicit `to` wins, then the derivation, then the stored
    // From. A derived `cc` rides along on EVERY branch — dropping it because
    // the caller retyped the `to` would send to fewer people than they asked
    // for — always minus whoever the chosen `to` already covers.
    let derived_cc = derived.as_ref().map(|d| d.cc.clone()).unwrap_or_default();
    let bcc_requested = body.bcc.clone().unwrap_or_default();
    let to = match body.to.clone().filter(|s| !s.trim().is_empty()) {
        Some(t) => t,
        None => match (&derived, &parent) {
            // Derivation that yielded nothing addressable falls through to the
            // stored From rather than composing a recipient-less reply.
            (Some(d), _) if !d.to.trim().is_empty() => d.to.clone(),
            (_, Some(p)) => p.from_addr.clone(),
            // A BCC-ONLY SEND is not a send with no recipient: it is the shape
            // of a blind blast, and the `To` it still needs is the sender. Read
            // from the store rather than left blank — `build_reply_rfc822`
            // refuses an empty `To`, and a mail addressed to nobody visible
            // reads to a recipient's client as a header it cannot render.
            (_, None) if !bcc_requested.trim().is_empty() => {
                match store_call(state, |store, account_id| store.account_email(account_id)).await {
                    Ok(own) if !own.trim().is_empty() => own,
                    // The account row is what every other surface reads the
                    // user's own address from, so this is near-impossible. It
                    // still fails the send rather than composing something
                    // half-addressed.
                    _ => {
                        audit_action(state, "send", target, &fired(fire, "rejected:no_recipient"))
                            .await;
                        return Err(SendFailure::new(
                            "rejected:no_recipient",
                            ApiError::bad_request(
                                "a bcc-only send needs this account's own address, \
                                 which could not be read",
                            ),
                        ));
                    }
                }
            }
            (_, None) => {
                audit_action(state, "send", target, &fired(fire, "rejected:no_recipient")).await;
                return Err(SendFailure::new(
                    "rejected:no_recipient",
                    ApiError::bad_request(
                        "send requires `to` (or `reply_to_message_id` to derive it)",
                    ),
                ));
            }
        },
    };
    // A STATED `cc` REPLACES THE DERIVATION, and absent keeps it. That
    // distinction is what lets a composer showing a Cc field EMPTY one: a
    // daemon that put the parent's copies back would make the field a lie.
    // Taken verbatim rather than through `cc_excluding`, which parses to bare
    // addresses and would silently strip the display names the sender typed.
    let cc = match body.cc.clone() {
        Some(stated) => stated,
        None => cc_excluding(&derived_cc, &to),
    };
    // BLIND COPIES LAST, filtered against everyone already VISIBLE on the
    // message. Someone in both `to` and `bcc` would otherwise receive two copies
    // of one mail, and the second would look like a blind copy of a message they
    // are openly on.
    let bcc = addrs_excluding(&bcc_requested, &[&to, &cc]);
    // For the audit line, counted before either moves into the MIME parts.
    // Quote-aware, because a stated `cc` carries display names: the comma in
    // `"Doe, John" <j@x>` is part of a name, and counting it would put a
    // recipient in the ledger who does not exist.
    let copied = count_addrs(&cc);
    let blind = count_addrs(&bcc);

    let subject = body
        .subject
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| match &parent {
            Some(p) => reply_subject(&p.subject),
            None => String::new(),
        });

    // Last thing before composing, so every rejection above costs no token.
    let tracker = mint_tracker(state, body.include_tracker.unwrap_or(false), target.clone()).await;

    // Kept back for the group-send record, which is written after `subject` has
    // moved into the MIME parts.
    let subject_for_record = subject.clone();
    let body_html = match body.body_format.as_deref() {
        Some("markdown") => Some(crate::markdown::render_email_html(&body.body)),
        _ => None,
    };
    // Inline exactly when the html points at it; decided here, once, against
    // the html that actually goes out.
    let mut attachments = attachments;
    mark_inline(&mut attachments, body_html.as_deref());
    let parts = ReplyParts {
        to,
        cc: Some(cc).filter(|s| !s.trim().is_empty()),
        bcc: Some(bcc).filter(|s| !s.trim().is_empty()),
        subject,
        body: body.body.clone(),
        in_reply_to,
        references,
        body_html,
        pixel_url: tracker.as_ref().map(|(_, url)| url.clone()),
        attachments,
        message_id: message_id.map(str::to_string),
        outbox_token: message_id.map(outbox_token),
    };
    let raw = match build_reply_rfc822(&parts) {
        Ok(r) => r,
        Err(e) => {
            if let Some((token, _)) = &tracker {
                discard_tracker(state, token, target.clone()).await;
            }
            audit_action(state, "send", target, &fired(fire, "rejected:compose")).await;
            return Err(SendFailure::new("rejected:compose", write_error(&e)));
        }
    };

    // A send that reaches N people says so in the ledger, with the COUNT and
    // never the addresses. Two shapes do: a reply-all, and a blind copy list —
    // the second matters more, because it is the one whose audience is invisible
    // in the mail itself, so the ledger is the only place it is written down.
    //
    // A STATED `cc` gets no line of its own, deliberately: it is legible in the
    // delivered mail, which is the test for whether the ledger has to carry it.
    let outcome = if blind > 0 {
        format!("ok:bcc:{blind}")
    } else if body.reply_all {
        format!("ok:reply_all:{}", copied + 1)
    } else {
        "ok".to_string()
    };
    let outcome = fired_outcome(fire, outcome);
    let result = finish_send(
        state,
        &*client,
        &raw,
        thread_id,
        // A cold send has no target row to resolve.
        body.reply_to_message_id,
        outcome,
        tracker,
        body.draft_id,
        &body.attachment_ids,
        target,
        fire,
    )
    .await;

    // ATTRIBUTION, once the mail is away. A `to`/`bcc` group went out as one
    // message that the composer addressed itself, so all the daemon adds is the
    // record that this was that group — which is what makes the history entry
    // exact ("12 of 12, sent as bcc") instead of inferred from its recipients.
    if let (
        Some(audience),
        Ok(SendOutcome::Sent {
            echo_message_id, ..
        }),
    ) = (&audience, &result)
    {
        // A fired send whose group has since become `individual` still went
        // as ONE message: record it the way it went (bcc when the frozen
        // request carried blind copies, else to), not the way the group reads
        // now.
        //
        // And WHO it went to is the frozen request's field the group's mode
        // expanded into (`to` for a To group, `bcc` for a Bcc group), the list
        // the mail was actually addressed with, not the group's membership
        // now: a member added since never got this mail, and one removed
        // since did. Only that field: a Bcc group mail with the user's boss
        // typed in `to` does not record the boss as a group recipient.
        //
        // The MODE is the one it was scheduled under (`group_mode`, frozen
        // by the schedule route), not the group's mode now: a Bcc group
        // flipped to To since would otherwise read the frozen `to`, which
        // for that mail is empty.
        let recorded = if fire.is_some() {
            let mode = match body.group_mode.unwrap_or(audience.mode) {
                GroupMode::Individual if bcc_requested.trim().is_empty() => GroupMode::To,
                GroupMode::Individual => GroupMode::Bcc,
                mode => mode,
            };
            group_send::GroupAudience {
                group_id: audience.group_id,
                mode,
                addrs: frozen_addrs(body, mode),
            }
        } else {
            group_send::GroupAudience {
                group_id: audience.group_id,
                mode: audience.mode,
                addrs: audience.addrs.clone(),
            }
        };
        // Nobody in the field the mode names: there is nothing to attribute,
        // and a `group_sends` row of zero recipients would read as a send
        // that reached nobody.
        if recorded.addrs.is_empty() {
            eprintln!(
                "squelch-api: group send {} went out with no recipients in its group's field; \
                 not recorded",
                recorded.group_id
            );
        } else {
            group_send::record_single(state, &recorded, subject_for_record, *echo_message_id).await;
        }
    }
    result
}

/// The addresses a frozen request named in the field `mode` expands a group
/// into (`bcc` for [`GroupMode::Bcc`], else `to`), bare, in order, each once
/// (case-insensitively): the direct recipients a fired group send reached in
/// that field. Non-members the user typed there are included; the other
/// field is not read.
fn frozen_addrs(body: &SendBody, mode: GroupMode) -> Vec<String> {
    let list = match mode {
        GroupMode::Bcc => body.bcc.as_deref(),
        GroupMode::To | GroupMode::Individual => body.to.as_deref(),
    };
    let mut out: Vec<String> = Vec::new();
    for addr in list.map(parse_addr_list).unwrap_or_default() {
        if !out.iter().any(|a| a.eq_ignore_ascii_case(&addr)) {
            out.push(addr);
        }
    }
    out
}

/// A fan-out: one message per member, run as a job because twelve serial sends
/// plus twelve echoes do not fit inside the composer's POST budget.
///
/// Answers `202` with the batch id. The composer closes on it and the Groups
/// page watches `reached` climb on its ordinary history read — the pending
/// recipients ARE the progress bar, so nothing else has to be plumbed.
async fn fan_out_send(
    state: &ApiState,
    client: Arc<dyn MailWriter>,
    body: &SendBody,
    audience: &group_send::GroupAudience,
    attachments: Vec<MailAttachment>,
) -> Result<SendOutcome, SendFailure> {
    let target = Some(audience.group_id.to_string());
    let subject = body
        .subject
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_default();

    // The tracker is minted ONCE for the batch; see `FanOut::pixel_url` for why
    // it is not per recipient.
    let tracker = mint_tracker(state, body.include_tracker.unwrap_or(false), target.clone()).await;

    let plan = group_send::FanOut {
        audience: group_send::GroupAudience {
            group_id: audience.group_id,
            mode: audience.mode,
            addrs: audience.addrs.clone(),
        },
        subject,
        body: body.body.clone(),
        body_html: match body.body_format.as_deref() {
            Some("markdown") => Some(crate::markdown::render_email_html(&body.body)),
            _ => None,
        },
        pixel_url: tracker.as_ref().map(|(_, url)| url.clone()),
        attachments: Vec::new(),
    };
    // One copy of the files for the whole batch, marked once against the one
    // html every member gets.
    let mut plan = plan;
    let mut attachments = attachments;
    mark_inline(&mut attachments, plan.body_html.as_deref());
    plan.attachments = attachments;
    let recipients = plan.audience.addrs.len();

    let group_send_id = match group_send::start(state, client, plan).await {
        Ok(id) => id,
        Err(e) => {
            if let Some((token, _)) = &tracker {
                discard_tracker(state, token, target.clone()).await;
            }
            audit_action(state, "send", target, "failed:fan_out_start").await;
            return Err(SendFailure::new("failed:fan_out_start", e));
        }
    };

    // The composition is away as far as the composer is concerned, so its draft
    // goes with it — the same contract every other successful send has. The
    // files too: the batch holds its own copy of their bytes.
    if let Some(draft_id) = body.draft_id {
        discard_sent_draft(state, draft_id).await;
    }
    discard_sent_attachments(state, body.draft_id, body.attachment_ids.clone()).await;

    Ok(SendOutcome::Sending {
        group_send_id,
        recipients,
    })
}

/// The largest original a forward will carry, in DECODED RFC822 bytes.
///
/// A forward re-encodes everything it reads: the raw is base64-decoded, then
/// base64'd back out into a MIME body, then serialized into a JSON request that
/// reqwest buffers — four to five copies of the original alive at the peak. A
/// 25 MB message (Gmail's own attachment ceiling) therefore costs well over
/// 100 MB of daemon memory and then fails somewhere deep in that pipeline as an
/// opaque 502 pointing at Gmail. Refuse it up front, with a status that says
/// which side the problem is on.
pub(crate) const MAX_FORWARD_RAW_BYTES: usize = 20 * 1024 * 1024;

/// Whether a fetched original is past the forward ceiling. Its own function so
/// the boundary is testable without moving twenty megabytes through a socket.
fn forward_raw_too_large(len: usize) -> bool {
    len > MAX_FORWARD_RAW_BYTES
}

/// Read a forward's ORIGINAL on the write token, exactly as it will be sent:
/// resolve the stored row, fetch its raw bytes, refuse one past the
/// [`MAX_FORWARD_RAW_BYTES`] ceiling (counting `staged`, the sender's own
/// files riding in the same message), and parse it.
///
/// Shared by the forward send and by scheduling a forward, which reads the
/// original NOW so the guard can judge it while the user is there
/// (docs/SCHEDULED-SEND.md §3 step 4). `action` is the audit action every
/// refusal is written under (`send` / `schedule`), and `fire` marks it when a
/// scheduled send is being fired (see [`fired`]).
pub(crate) async fn read_forward_original(
    state: &ApiState,
    client: &dyn MailWriter,
    action: &'static str,
    target: Option<String>,
    original_id: i64,
    staged: usize,
    fire: Option<Fire<'_>>,
) -> Result<(ActionMessageRef, ForwardedOriginal), SendFailure> {
    // Sealed and unknown ids are the same 404 every message-target route returns.
    let original_ref = match resolve_target(state, original_id).await {
        Ok(m) => m,
        Err(e) => {
            audit_action(state, action, target, &fired(fire, "failed:target")).await;
            return Err(SendFailure::new("failed:target", e));
        }
    };

    let raw = match client.fetch_raw(&original_ref.gmail_msg_id).await {
        Ok(f) => decode_raw_b64url(&f.raw_b64).unwrap_or_default(),
        // A DEAD WRITE CREDENTIAL IS NOT A GMAIL OUTAGE. The fetch runs on the
        // write token too, so an expired or revoked one fails HERE, before any
        // send — and collapsing it into the 502 tells the composer to blame
        // Gmail when the fix is `squelchd auth`. Answer it with the same
        // 403 every other write path answers it with, so the client's existing
        // "no write credential" handling fires.
        Err(e @ WriteError::MissingCredential(_)) => {
            audit_action(
                state,
                action,
                target,
                &fired(fire, "rejected:no_write_credential"),
            )
            .await;
            return Err(SendFailure::new(
                "rejected:no_write_credential",
                write_error(&e),
            ));
        }
        // PACING IS NOT A MISSING ORIGINAL. The budget held the read and
        // nothing was asked of Gmail: "not now", which a fired forward hands
        // its attempt back for (docs/SCHEDULED-SEND.md §4 step 5), rather than
        // a `failed:fetch_original` that spends one.
        Err(e @ WriteError::Paced { .. }) => {
            audit_action(state, action, target, &fired(fire, PACED)).await;
            return Err(SendFailure::paced(&e));
        }
        Err(_) => Vec::new(),
    };
    // A forward of a 25 MB message is a memory spike and then an opaque failure
    // (see [`MAX_FORWARD_RAW_BYTES`]). Checked on the DECODED length, before the
    // parse allocates its own copy of every part — and counting the sender's
    // own staged files, which ride in the same message and were each under
    // the cap on their own.
    if forward_raw_too_large(raw.len() + staged) {
        audit_action(state, action, target, &fired(fire, "rejected:too_large")).await;
        return Err(SendFailure::new(
            "rejected:too_large",
            ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "original message is too large to forward",
            ),
        ));
    }
    // Empty bytes, undecodable bytes and unparseable bytes are one outcome:
    // there is no original to forward.
    let Some(original) = (!raw.is_empty())
        .then(|| parse_forwarded_original(&raw))
        .flatten()
    else {
        audit_action(state, action, target, &fired(fire, "failed:fetch_original")).await;
        return Err(SendFailure::new(
            "failed:fetch_original",
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "could not fetch the original message; forward was not sent",
            ),
        ));
    };
    Ok((original_ref, original))
}

/// The outbound guard's kinds over a forward's ORIGINAL, deduped: what the
/// forward sends that the sender did not type. The caller unions them with the
/// note's and the staged files' and asks [`guard_verdict`] once.
pub(crate) fn forward_original_kinds(original: &ForwardedOriginal) -> Vec<&'static str> {
    // BOTH VIEWS OF THE ORIGINAL ARE SCANNED. When a text/plain part exists,
    // mail-parser hands back exactly that part and never converts the html
    // alternative down — so a message whose text half is innocuous and whose html
    // half holds the PEM block would ship with the secret unread. The html view
    // goes through the same html-to-text converter mail-parser uses internally,
    // which is what makes the scan see the words a recipient would see.
    let html_text = original
        .html
        .as_deref()
        .map(mail_parser::decoders::html::html_to_text)
        .unwrap_or_default();

    // AND SO ARE TEXT ATTACHMENTS. A PEM key pasted into `notes.txt` and dragged
    // onto a mail is the same exfil shape as one pasted into the body — arguably
    // the more natural one — and scanning only the bodies let it through while
    // this code called forwarding the classic exfil shape. Every `text/*` part
    // is scanned as text, and so is every `message/*` part — an attached email
    // ("see the thread below") is plain RFC822 text carrying whatever its bodies
    // carried. mail-parser has already transcoded text parts to UTF-8, so the
    // lossy conversion below only ever fires on genuinely broken bytes.
    // BINARY PARTS STAY UNSCANNED (a key inside a zip or an `id_rsa` with no
    // text mime passes): the guard is a seatbelt against the accident, not a DLP
    // boundary, and it is overridable by design.
    let mut kinds: Vec<&'static str> = Vec::new();
    let attachment_text: Vec<String> = original
        .attachments
        .iter()
        .filter(|a| {
            let mime = a.mime.to_ascii_lowercase();
            mime.starts_with("text/") || mime.starts_with("message/")
        })
        .map(|a| String::from_utf8_lossy(&a.data).into_owned())
        .collect();
    for kind in guard::scan_kinds(&original.text)
        .into_iter()
        .chain(guard::scan_kinds(&html_text))
        .chain(attachment_text.iter().flat_map(|t| guard::scan_kinds(t)))
    {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}

/// The FORWARD path of `action_send`, entered once the confirm gate, the body
/// validations and the write credential are behind it.
///
/// Shape: read the original's raw bytes with the write token, guard what they
/// contain, compose, send UNTHREADED. Two invariants earn their own sentences:
///
/// - FORWARDING IS THE CLASSIC EXFIL SHAPE, so the original's own text — its
///   text bodies, an html-to-text view of its html, and the contents of every
///   `text/*` attachment — goes through the same guard the note does, and
///   `override_guard` is the ceremony's second act rather than a formality. What
///   is scanned is what is SENT: the scan runs on the parse of the raw fetch,
///   never on the stored row, because the stored body is a sanitized, flattened
///   memory of the message and the bytes on the wire are the original.
/// - The fetch failing is LOUD (502), like reply-all's audience read: what is
///   forwarded IS the request, so there is nothing to degrade to.
///
/// `owed` is [`SendGuard::ForwardOwed`] on the request path. `Settled` skips
/// the scan outright: that forward was judged before it was queued.
#[allow(clippy::too_many_arguments)]
async fn forward_send(
    state: &ApiState,
    body: &SendBody,
    client: &dyn MailWriter,
    target: Option<String>,
    original_id: i64,
    owed: SendGuard,
    attachments: Vec<MailAttachment>,
    fire: Option<Fire<'_>>,
) -> Result<SendOutcome, SendFailure> {
    // The staged files ride in the same message and were each under the cap
    // on their own, so they count against the original's ceiling.
    let staged: usize = attachments.iter().map(|a| a.data.len()).sum();
    let (original_ref, original) = read_forward_original(
        state,
        client,
        "send",
        target.clone(),
        original_id,
        staged,
        fire,
    )
    .await?;

    // A forward judged before it was queued is not judged again; nothing
    // below is read for any other reason, so it is skipped whole.
    if let SendGuard::ForwardOwed(note_kinds) = owed {
        // ONE verdict over the note AND what it quotes (kinds only, deduped).
        let mut kinds = note_kinds;
        for kind in forward_original_kinds(&original) {
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
        if let Some(err) =
            guard_verdict(state, "send", &kinds, body.override_guard, target.clone()).await
        {
            return Err(SendFailure::new("blocked:guard", err));
        }
    }

    // Validated non-empty before any Gmail spend (see `action_send`).
    let to = body.to.clone().unwrap_or_default();
    // The STORED subject is what the user saw in the reader, so it is what the
    // `Fwd:` goes in front of.
    let subject = body
        .subject
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| forward_subject(&original_ref.subject));

    // Last thing before composing, so every rejection above costs no token.
    let tracker = mint_tracker(state, body.include_tracker.unwrap_or(false), target.clone()).await;

    let note_html = match body.body_format.as_deref() {
        Some("markdown") => Some(crate::markdown::render_email_html(&body.body)),
        _ => None,
    };
    // THE SENDER'S OWN FILES RIDE BESIDE THE ORIGINAL'S, after them, as more
    // parts of the same mixed. Inline is decided against the note's html — the
    // original's own inline parts were flagged by the parser and are not
    // re-judged.
    let mut original = original;
    let mut attachments = attachments;
    mark_inline(&mut attachments, note_html.as_deref());
    original.attachments.extend(attachments);
    let parts = ForwardParts {
        to,
        // Stated or nothing: a forward derives NOTHING about its audience from
        // the original (see the `to` rejection in `action_send`), and that goes
        // double for a copy list.
        cc: body.cc.clone().filter(|s| !s.trim().is_empty()),
        bcc: body.bcc.clone().filter(|s| !s.trim().is_empty()),
        subject,
        note: body.body.clone(),
        note_html,
        pixel_url: tracker.as_ref().map(|(_, url)| url.clone()),
        original,
        message_id: fire.map(|f| f.message_id.to_string()),
        outbox_token: fire.map(|f| outbox_token(f.message_id)),
    };
    let raw = match build_forward_rfc822(&parts) {
        Ok(r) => r,
        Err(e) => {
            if let Some((token, _)) = &tracker {
                discard_tracker(state, token, target.clone()).await;
            }
            audit_action(state, "send", target, &fired(fire, "rejected:compose")).await;
            return Err(SendFailure::new("rejected:compose", write_error(&e)));
        }
    };

    // NO threadId and NO resolve: a forward opens a new conversation rather than
    // filing the user's "look at this" inside the correspondence it came out of,
    // and passing a message on is not handling it. Both are the `None`s below.
    // Same ledger rule as a reply's: counts, never addresses. A forward has no
    // derived audience at all, so every one of these is somebody the sender
    // typed.
    let mut outcome = "ok:forward".to_string();
    let copied = count_addrs(parts.cc.as_deref().unwrap_or(""));
    let blind = count_addrs(parts.bcc.as_deref().unwrap_or(""));
    if copied > 0 {
        outcome.push_str(&format!(":cc:{copied}"));
    }
    if blind > 0 {
        outcome.push_str(&format!(":bcc:{blind}"));
    }
    let outcome = fired_outcome(fire, outcome);
    finish_send(
        state,
        client,
        &raw,
        None,
        None,
        outcome,
        tracker,
        body.draft_id,
        &body.attachment_ids,
        target,
        fire,
    )
    .await
}

// --- GET /client/messages/{id}/reply_recipients ------------------------------

#[derive(Debug, Deserialize)]
pub struct ReplyRecipientsQuery {
    /// `true` = show what reply-all would address. Absent is a plain reply.
    #[serde(default)]
    all: bool,
}

/// What a reply would be addressed to. `cc` is omitted when there is none, so a
/// plain reply's answer has exactly one field.
#[derive(Debug, Serialize)]
struct ReplyRecipientsView {
    to: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    cc: String,
}

/// PREVIEW, not a send: the audience the send path would compose, so the client
/// can show the user who they are about to write to BEFORE the mail leaves.
/// Derived by the same function the send uses, from the same one metadata fetch,
/// so what is previewed is what is sent.
///
/// Sealed and unknown ids are the same 404 every message-target route returns.
/// A missing write credential is the usual 403 and an upstream failure a 502 —
/// both are answers the client can show, not a 500.
pub async fn reply_recipients(
    State(state): State<ApiState>,
    Path(message_id): Path<i64>,
    Query(q): Query<ReplyRecipientsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let msg = resolve_target(&state, message_id).await?;
    let client = write_client(&state).await?;
    let own = store_call(&state, |store, account_id| store.account_email(account_id)).await?;
    let headers = client
        .parent_headers(&msg.gmail_msg_id)
        .await
        .map_err(|e| write_error(&e))?;
    let derived = derive_reply_recipients(&headers, &own, q.all);
    // Headers too broken to yield an address fall back to the stored From, the
    // same recipient a plain reply would have used all along.
    let to = if derived.to.trim().is_empty() {
        msg.from_addr.clone()
    } else {
        derived.to
    };
    Ok(Json(ReplyRecipientsView { to, cc: derived.cc }))
}

// --- UNSUBSCRIBE: human-door-only, no agent-door exposure --------------------
//
// POST /client/unsubscribe picks the sender's http(s) unsubscribe link, header
// first. By default it hands the link to the CLIENT to open. With `auto: true`
// (a client that can show a running job) it starts the daemon's own job instead
// (`crate::unsub_job`, docs/SECURITY.md §7), unless the mail is spam or failed
// authentication. Unknown messages are a 404; no http(s) link is a 422.

/// Upsert the unsubscribe ledger row, resetting the violation clock: a fresh
/// request restarts the 72h grace.
async fn record_unsub(
    state: &ApiState,
    sender: &str,
    method: &'static str,
    source_message_id: i64,
) -> Result<(), ApiError> {
    let sender = sender.to_string();
    store_call(state, move |store, account_id| {
        store.upsert_unsubscribe(
            account_id,
            &sender,
            method,
            Some(source_message_id),
            Utc::now(),
        )
    })
    .await
}

#[derive(Debug, Deserialize)]
pub struct UnsubscribeBody {
    message_id: i64,
    /// The client can show a running job and wants the daemon to do the
    /// unsubscribe itself (`crate::unsub_job`). ABSENT ON EVERY OLDER CLIENT,
    /// which decodes a response with a `url` it opens, so the default stays
    /// exactly that response.
    #[serde(default)]
    auto: bool,
}

pub async fn unsubscribe(
    State(state): State<ApiState>,
    Json(body): Json<UnsubscribeBody>,
) -> Result<impl IntoResponse, ApiError> {
    let message_id = body.message_id;
    let target = Some(message_id.to_string());

    // `None` => missing OR sealed, indistinguishable, both 404.
    let fields = store_call(&state, move |store, account_id| {
        store.message_unsub_fields(account_id, message_id)
    })
    .await?
    .ok_or_else(ApiError::not_found)?;

    let sender = fields.from_addr.trim().to_ascii_lowercase();
    // HEADER FIRST, body second. `List-Unsubscribe` is the sender NAMING its own
    // endpoint; a footer link is us reading the mail and picking one. Plenty of
    // senders ship no header at all, and telling those readers "there is no
    // unsubscribe link" while one sits visible at the bottom of the message is
    // simply wrong.
    let (plan, from_header) =
        match crate::unsubscribe::classify_unsubscribe(fields.list_unsubscribe.as_deref()) {
            crate::unsubscribe::UnsubPlan::None => (
                crate::unsubscribe::classify_unsubscribe_body(fields.body_html.as_deref()),
                false,
            ),
            header => (header, true),
        };

    // THE AUTOMATIC PATH, for a client that asked for it and mail we trust
    // enough to act on. Spam is refused because fetching a spammer's link tells
    // them the address is live; unauthenticated mail because the link's sender
    // may not be who the From line says. Both fall through to the browser
    // response, with the reason, so the person decides with the address bar in
    // front of them.
    let mut refused = if fields.is_spam {
        Some("spam")
    } else {
        match fields.auth_pass {
            Some(true) => None,
            Some(false) => Some("unverified"),
            // Mail from before the verdict was recorded: never checked, which
            // is not the same claim as "failed", and the client says so.
            None => Some("unchecked"),
        }
    };
    if body.auto
        && refused.is_none()
        && let crate::unsubscribe::UnsubPlan::Browser { url } = &plan
    {
        match crate::unsub_job::choose_plan(from_header, fields.list_unsub_one_click, url, &sender)
        {
            Some(job) => {
                // The one string the browser rung may type, settled here where
                // the message is in hand: the address this mail was sent to.
                let to_addrs = fields.to_addrs.clone();
                let email = store_call(&state, move |store, account_id| {
                    let account = store.account_email(account_id)?;
                    Ok(crate::unsub_job::address_to_type(
                        &account,
                        to_addrs.as_deref(),
                    ))
                })
                .await?;
                crate::unsub_job::start(&state, sender.clone(), message_id, job, email).await?;
                audit_action(&state, "unsubscribe", target, &format!("auto:{sender}")).await;
                resolve_sender_done(&state, &sender).await;
                return Ok(Json(
                    json!({ "method": "auto", "sender": sender, "status": "running" }),
                ));
            }
            None => refused = Some("footer_link"),
        }
    }

    match plan {
        crate::unsubscribe::UnsubPlan::None => {
            // No http(s) unsubscribe URL to hand the client.
            Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "message has no http(s) unsubscribe link",
            ))
        }
        crate::unsubscribe::UnsubPlan::Browser { url } => {
            // The browser path: no outbound request is made here. The audit
            // records the sender only, never the URL.
            record_unsub(&state, &sender, "browser", message_id).await?;
            audit_action(&state, "unsubscribe", target, &format!("browser:{sender}")).await;
            // RESOLUTION: unsubscribing IS the disposition of this SENDER —
            // asking them to stop and leaving their mail open would demand a
            // second act, once per thread they already sent.
            resolve_sender_done(&state, &sender).await;
            let mut out = json!({ "method": "browser", "sender": sender, "url": url });
            if body.auto
                && let Some(why) = refused
            {
                out["auto_refused"] = json!(why);
            }
            Ok(Json(out))
        }
    }
}

// --- TRIAGE FEEDBACK (human corrections) ------------------------------------
//
// Applying the fix and recording it are ONE call on purpose: a correction the
// human sees take effect but that never reaches the dataset makes triage look
// better than it is. `to_value` is validated against TriageAxis::allowed, so a
// typo can never write a label the pipeline would not itself produce.

/// How many corrections GET returns by default / at most.
const FEEDBACK_DEFAULT_LIMIT: u32 = 200;
const FEEDBACK_MAX_LIMIT: u32 = 2000;
/// Ceiling on the optional free-text note.
const FEEDBACK_NOTE_MAX: usize = 500;

#[derive(Debug, Deserialize)]
pub struct TriageFeedbackBody {
    message_id: i64,
    /// Which axis was wrong: "tier" | "category".
    dimension: String,
    /// The value it should have had.
    to_value: String,
    #[serde(default)]
    note: Option<String>,
}

pub async fn post_triage_feedback(
    State(state): State<ApiState>,
    Json(body): Json<TriageFeedbackBody>,
) -> Result<impl IntoResponse, ApiError> {
    let Some(axis) = TriageAxis::parse(body.dimension.trim()) else {
        return Err(ApiError::bad_request(
            "dimension must be one of: tier, category, sensitivity",
        ));
    };
    let to_value = body.to_value.trim().to_ascii_lowercase();
    if !axis.allowed().contains(&to_value.as_str()) {
        return Err(ApiError::bad_request(format!(
            "{} must be one of: {}",
            axis.as_str(),
            axis.allowed().join(", ")
        )));
    }
    let note = body
        .note
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty());
    if let Some(n) = &note
        && n.chars().count() > FEEDBACK_NOTE_MAX
    {
        return Err(ApiError::bad_request("note is too long"));
    }

    let message_id = body.message_id;
    let to = to_value.clone();
    let n = note.clone();
    let recorded = store_call(&state, move |store, account_id| {
        store.correct_triage(account_id, message_id, axis, &to, n.as_deref(), Utc::now())
    })
    .await?
    .ok_or_else(ApiError::not_found)?;

    audit_action(
        &state,
        "triage_correction",
        Some(message_id.to_string()),
        &format!(
            "{}:{}->{}",
            axis.as_str(),
            recorded.from_value.as_deref().unwrap_or("none"),
            to_value
        ),
    )
    .await;
    Ok(Json(recorded))
}

#[derive(Debug, Deserialize)]
pub struct FeedbackQuery {
    #[serde(default)]
    limit: Option<u32>,
}

/// The refinement dataset: where triage actually goes wrong.
pub async fn get_triage_feedback(
    State(state): State<ApiState>,
    Query(q): Query<FeedbackQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let limit = q
        .limit
        .unwrap_or(FEEDBACK_DEFAULT_LIMIT)
        .clamp(1, FEEDBACK_MAX_LIMIT);
    query(&state, move |store, account_id| {
        store.list_triage_feedback(account_id, limit)
    })
    .await
}

// --- AUTH-MAIL SHREDDER (retention) -----------------------------------------
//
// Moves auth mail older than the policy window to Gmail's TRASH and records it
// in `shred_log`. Because this deletes a human's mail on a timer, the safety
// posture is non-negotiable: OFF by default; trash only (recoverable for 30
// days, and `gmail.modify` cannot permanently delete); a no-op without the
// opt-in write credential; bounded per pass, oldest first; and the ledger row
// lands only AFTER Gmail confirms, with an audit row for every shred.

const SHRED_ENABLED_KEY: &str = "shred_enabled";
const SHRED_AFTER_DAYS_KEY: &str = "shred_after_days";
/// Default retention window. Matches the Auth page's copy.
const SHRED_DEFAULT_DAYS: i64 = 30;
/// Floor on the window. A shorter one would start trashing codes while they are
/// plausibly still in use, which is a footgun we simply do not offer.
const SHRED_MIN_DAYS: i64 = 7;
const SHRED_MAX_DAYS: i64 = 365;
/// Messages trashed per pass. Bounds the API burst on a first run; the next
/// pass picks up where this one left off (candidates come back oldest-first).
const SHRED_BATCH: u32 = 50;
/// Window for the headline "shredded recently" figure.
const SHRED_RECENT_DAYS: i64 = 30;

/// Interpret the raw `app_settings` values into a policy. Unset or malformed
/// falls back to the SAFE default (off / 30d) rather than erroring: a corrupted
/// knob must never start deleting mail, nor take the Auth page down.
fn parse_shred_policy(enabled: Option<String>, days: Option<String>) -> (bool, i64) {
    let enabled = enabled
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let after_days = days
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|d| (SHRED_MIN_DAYS..=SHRED_MAX_DAYS).contains(d))
        .unwrap_or(SHRED_DEFAULT_DAYS);
    (enabled, after_days)
}

/// Read the account's retention policy off the store.
async fn shred_policy(state: &ApiState) -> Result<(bool, i64), ApiError> {
    store_call(state, |store, account_id| {
        let enabled = store.get_app_setting(account_id, SHRED_ENABLED_KEY)?;
        let days = store.get_app_setting(account_id, SHRED_AFTER_DAYS_KEY)?;
        Ok(parse_shred_policy(enabled, days))
    })
    .await
}

/// Assemble the Auth page's shredder panel state.
async fn shred_stats(state: &ApiState) -> Result<ShredStats, ApiError> {
    let now = Utc::now();
    let recent_since = now - chrono::Duration::days(SHRED_RECENT_DAYS);

    let (enabled, after_days, pending, shredded_recent, shredded_total, last_shredded_at) =
        store_call(state, move |store, account_id| {
            let enabled = store.get_app_setting(account_id, SHRED_ENABLED_KEY)?;
            let days = store.get_app_setting(account_id, SHRED_AFTER_DAYS_KEY)?;
            let (enabled, after_days) = parse_shred_policy(enabled, days);
            let cutoff = now - chrono::Duration::days(after_days);
            let pending = store.shred_pending_count(account_id, cutoff)?;
            let (recent, total, last) = store.shred_counts(account_id, recent_since)?;
            Ok((enabled, after_days, pending, recent, total, last))
        })
        .await?;

    Ok(ShredStats {
        enabled,
        after_days,
        shredded_recent,
        shredded_total,
        last_shredded_at,
        pending,
        // Separate from `enabled` so the UI can tell "off" from "on but unable".
        write_ready: state.write_creds().is_some(),
    })
}

/// Run one bounded retention pass, returning how many messages were trashed.
/// Disabled or credential-less is `Ok(0)`, not an error, so a timer can fire it
/// unconditionally. A per-message Gmail failure is audited and skipped — it
/// neither aborts the pass nor writes a ledger row.
pub async fn run_shred_pass(state: &ApiState) -> Result<u32, ApiError> {
    let (enabled, after_days) = shred_policy(state).await?;
    if !enabled {
        return Ok(0);
    }
    let client = match write_client(state).await {
        Ok(client) => client,
        // No write credential: nothing to do, and nothing to complain about.
        Err(e) if e.status == StatusCode::FORBIDDEN => return Ok(0),
        // Anything else is a failure the timer should see, like every other
        // store error in this pass.
        Err(e) => return Err(e),
    };

    let cutoff = Utc::now() - chrono::Duration::days(after_days);
    let candidates = store_call(state, move |store, account_id| {
        store.shred_candidates(account_id, cutoff, SHRED_BATCH)
    })
    .await?;

    let mut shredded = 0u32;
    for candidate in candidates {
        if client.trash(&candidate.gmail_msg_id).await.is_err() {
            // Skipped with no ledger row, so a later pass retries it naturally.
            audit_action(
                state,
                "shred_failed",
                Some(candidate.message_id.to_string()),
                &candidate.sender,
            )
            .await;
            continue;
        }
        // Ledger AFTER Gmail confirms, so the count can never overstate.
        let c = candidate.clone();
        let recorded = store_call(state, move |store, account_id| {
            store.record_shred(account_id, &c, Utc::now())
        })
        .await;
        if recorded.is_err() {
            // The mail IS trashed and only the bookkeeping failed: say so in the
            // audit trail rather than silently under-counting.
            audit_action(
                state,
                "shred_unrecorded",
                Some(candidate.message_id.to_string()),
                &candidate.sender,
            )
            .await;
            continue;
        }
        audit_action(
            state,
            "shred",
            Some(candidate.message_id.to_string()),
            &format!("trash:{}", candidate.sender),
        )
        .await;
        shredded += 1;
    }
    Ok(shredded)
}

pub async fn get_shredder(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(shred_stats(&state).await?))
}

#[derive(Debug, Deserialize)]
pub struct ShredderBody {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    after_days: Option<i64>,
}

pub async fn set_shredder(
    State(state): State<ApiState>,
    Json(body): Json<ShredderBody>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(days) = body.after_days {
        if !(SHRED_MIN_DAYS..=SHRED_MAX_DAYS).contains(&days) {
            return Err(ApiError::bad_request(format!(
                "after_days must be between {SHRED_MIN_DAYS} and {SHRED_MAX_DAYS}"
            )));
        }
        let v = days.to_string();
        store_call(&state, move |store, account_id| {
            store.set_app_setting(account_id, SHRED_AFTER_DAYS_KEY, &v)
        })
        .await?;
    }
    if let Some(enabled) = body.enabled {
        let v = if enabled { "1" } else { "0" }.to_string();
        store_call(&state, move |store, account_id| {
            store.set_app_setting(account_id, SHRED_ENABLED_KEY, &v)
        })
        .await?;
        // Turning automatic deletion on or off is a policy change worth a row.
        audit_action(
            &state,
            "shred_policy",
            None,
            if enabled { "enabled" } else { "disabled" },
        )
        .await;
    }
    Ok(Json(shred_stats(&state).await?))
}

/// POST /client/shredder/run — run a pass now, without waiting for the timer.
pub async fn run_shredder(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    let shredded = run_shred_pass(&state).await?;
    let stats = shred_stats(&state).await?;
    Ok(Json(json!({ "shredded": shredded, "stats": stats })))
}

// --- GET /client/unsubscribes -----------------------------------------------

pub async fn list_unsubscribes(
    State(state): State<ApiState>,
) -> Result<impl IntoResponse, ApiError> {
    // A `running` row no job in this process owns died with the last one.
    crate::unsub_job::reap(&state).await;
    // Newest requested_at first.
    query(&state, |store, account_id| {
        store.list_unsubscribes(account_id)
    })
    .await
}

// --- POST /client/unsubscribes/opened ---------------------------------------
//
// The user followed a `needs_you` job's link to finish on the sender's site.
// From here the request counts as made: the row arms the violation detector.

#[derive(Debug, Deserialize)]
pub struct OpenedBody {
    sender: String,
}

pub async fn unsubscribe_opened(
    State(state): State<ApiState>,
    Json(body): Json<OpenedBody>,
) -> Result<impl IntoResponse, ApiError> {
    let sender = body.sender.trim().to_ascii_lowercase();
    if sender.is_empty() {
        return Err(ApiError::bad_request("sender must not be empty"));
    }
    let s = sender.clone();
    let updated = store_call(&state, move |store, account_id| {
        store.mark_unsubscribe_opened(account_id, &s, Utc::now())
    })
    .await?;
    if !updated {
        return Err(ApiError::not_found());
    }
    audit_action(
        &state,
        "unsubscribe",
        Some(sender.clone()),
        &format!("opened:{sender}"),
    )
    .await;
    Ok(Json(json!({ "sender": sender, "status": "opened" })))
}

// --- POST /client/unsubscribes/resolution -----------------------------------

#[derive(Debug, Deserialize)]
pub struct ResolutionBody {
    sender: String,
    resolution: String,
}

pub async fn unsubscribe_resolution(
    State(state): State<ApiState>,
    Json(body): Json<ResolutionBody>,
) -> Result<impl IntoResponse, ApiError> {
    let resolution = match body.resolution.as_str() {
        "blocked" | "filtered" | "dismissed" => body.resolution.clone(),
        _ => {
            return Err(ApiError::bad_request(
                "resolution must be one of: blocked, filtered, dismissed",
            ));
        }
    };
    let sender = body.sender.trim().to_ascii_lowercase();
    if sender.is_empty() {
        return Err(ApiError::bad_request("sender must not be empty"));
    }

    let s = sender.clone();
    let r = resolution.clone();
    let updated = store_call(&state, move |store, account_id| {
        store.set_unsubscribe_resolution(account_id, &s, &r)
    })
    .await?;
    if !updated {
        return Err(ApiError::not_found());
    }

    audit_action(
        &state,
        "unsub_resolution",
        Some(sender.clone()),
        &format!("{sender}:{resolution}"),
    )
    .await;
    Ok(Json(json!({ "sender": sender, "resolution": resolution })))
}

#[cfg(test)]
mod upload_policy_tests {
    use super::*;

    #[test]
    fn filenames_are_names_and_bounded() {
        assert_eq!(clean_upload_filename("../../evil.txt"), "....evil.txt");
        assert_eq!(clean_upload_filename("a\\b/c.pdf"), "abc.pdf");
        assert_eq!(clean_upload_filename("  spaced.png  "), "spaced.png");
        assert_eq!(clean_upload_filename("ctl\u{7}chars\n.txt"), "ctlchars.txt");
        assert_eq!(clean_upload_filename(""), "attachment");
        assert_eq!(clean_upload_filename("   "), "attachment");
        assert_eq!(
            clean_upload_filename("résumé.pdf"),
            "résumé.pdf",
            "unicode stays"
        );
        let long = "x".repeat(200) + ".pdf";
        assert_eq!(clean_upload_filename(&long).chars().count(), 80);
    }

    #[test]
    fn mimes_are_bare_media_types_or_a_blob() {
        assert_eq!(clean_upload_mime(Some("image/PNG; charset=x")), "image/png");
        assert_eq!(clean_upload_mime(None), "application/octet-stream");
        assert_eq!(
            clean_upload_mime(Some("nonsense")),
            "application/octet-stream"
        );
        assert_eq!(
            clean_upload_mime(Some("multipart/mixed; boundary=b")),
            "application/octet-stream"
        );
        assert_eq!(
            clean_upload_mime(Some("text/plain\r\nX: y")),
            "application/octet-stream"
        );
        assert_eq!(
            clean_upload_mime(Some("application/vnd.ms-excel")),
            "application/vnd.ms-excel"
        );
    }

    #[test]
    fn content_ids_are_a_bounded_header_safe_alphabet() {
        assert!(content_id_ok("a1b2-c3_d4.e5@passband"));
        assert!(!content_id_ok(""));
        assert!(!content_id_ok("a b"));
        assert!(!content_id_ok("a\"b"));
        assert!(!content_id_ok("a>b"));
        assert!(!content_id_ok("a/b"));
        assert!(content_id_ok(&"a".repeat(128)));
        assert!(!content_id_ok(&"a".repeat(129)));
    }
}

#[cfg(test)]
mod shred_policy_tests {
    use super::*;

    #[test]
    fn unset_policy_is_off_at_the_default_window() {
        // An account that never opted in never deletes mail.
        assert_eq!(parse_shred_policy(None, None), (false, SHRED_DEFAULT_DAYS));
    }

    #[test]
    fn enabled_accepts_the_stored_form_and_a_friendly_one() {
        assert!(parse_shred_policy(Some("1".into()), None).0);
        assert!(parse_shred_policy(Some("true".into()), None).0);
        assert!(parse_shred_policy(Some("TRUE".into()), None).0);
        assert!(!parse_shred_policy(Some("0".into()), None).0);
        // Anything unrecognized reads as OFF, never as on.
        assert!(!parse_shred_policy(Some("yes".into()), None).0);
        assert!(!parse_shred_policy(Some(String::new()), None).0);
    }

    #[test]
    fn window_is_clamped_to_the_supported_range() {
        assert_eq!(parse_shred_policy(None, Some("90".into())).1, 90);
        assert_eq!(parse_shred_policy(None, Some(" 45 ".into())).1, 45);
        // Out of range and garbage fall back to the default rather than
        // deleting on an aggressive window.
        assert_eq!(
            parse_shred_policy(None, Some("1".into())).1,
            SHRED_DEFAULT_DAYS
        );
        assert_eq!(
            parse_shred_policy(None, Some("100000".into())).1,
            SHRED_DEFAULT_DAYS
        );
        assert_eq!(
            parse_shred_policy(None, Some("-30".into())).1,
            SHRED_DEFAULT_DAYS
        );
        assert_eq!(
            parse_shred_policy(None, Some("soon".into())).1,
            SHRED_DEFAULT_DAYS
        );
    }

    #[test]
    fn a_corrupt_knob_never_enables_deletion() {
        for raw in ["", "  ", "off", "null", "2", "-1", "on"] {
            assert!(
                !parse_shred_policy(Some(raw.into()), Some("30".into())).0,
                "{raw:?} must not read as enabled"
            );
        }
    }
}

#[cfg(test)]
mod forward_ceiling_tests {
    use super::*;

    #[test]
    fn the_ceiling_is_twenty_mebibytes_and_it_is_a_ceiling_not_a_floor() {
        // Exactly at the limit still forwards: the number is what we promise to
        // carry, and an off-by-one here silently refuses a legal 20 MiB message.
        assert!(!forward_raw_too_large(0));
        assert!(!forward_raw_too_large(MAX_FORWARD_RAW_BYTES - 1));
        assert!(!forward_raw_too_large(MAX_FORWARD_RAW_BYTES));
        assert!(forward_raw_too_large(MAX_FORWARD_RAW_BYTES + 1));
        assert_eq!(MAX_FORWARD_RAW_BYTES, 20 * 1024 * 1024);
    }
}

#[cfg(test)]
mod cursor_tests {
    use super::*;

    #[test]
    fn offset_tokens_are_unpadded_base64url() {
        // Frozen shape: unpadded base64url of `off:<n>`. Changing the codec
        // would invalidate every cursor a client is holding.
        assert_eq!(cursor::encode_offset(50), "b2ZmOjUw");
        assert_eq!(cursor::encode_offset(2), "b2ZmOjI");
        for off in [0u32, 1, 2, 50, 500, 12345, u32::MAX] {
            assert_eq!(
                cursor::decode_offset(&cursor::encode_offset(off)),
                Some(off)
            );
        }
    }

    #[test]
    fn malformed_tokens_decode_to_none() {
        // Bad alphabet, padding (we emit none), a dangling 1-char chunk, and
        // valid base64 that is not an offset token.
        for bad in ["@@notbase64@@", "b2ZmOjIy=", "b2ZmOjIyM", "", "b", "aGk"] {
            assert_eq!(cursor::decode_offset(bad), None, "{bad:?}");
        }
    }
}

#[cfg(test)]
mod send_split_tests {
    use super::*;

    fn body(v: serde_json::Value) -> SendBody {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn validation_refuses_in_the_documented_order() {
        // Each body carries every problem from its row down, so only the order
        // decides which reason comes back.
        let cases = [
            (
                json!({"body": "", "forward_of_message_id": 1, "reply_to_message_id": 2}),
                "rejected:confirm",
            ),
            (
                json!({"confirm": true, "body": "", "forward_of_message_id": 1, "reply_to_message_id": 2}),
                "rejected:forward_and_reply",
            ),
            (
                json!({"confirm": true, "body": " ", "reply_all": true, "to": "#g"}),
                "rejected:empty_body",
            ),
            (
                json!({"confirm": true, "body": "", "forward_of_message_id": 1, "reply_all": true}),
                "rejected:no_recipient",
            ),
            (
                json!({"confirm": true, "body": "x", "reply_all": true, "to": "#g"}),
                "rejected:reply_all_no_parent",
            ),
            (
                json!({"confirm": true, "body": "x", "bcc": "#investors", "group_id": 1, "reply_to_message_id": 2}),
                "rejected:unresolved_group",
            ),
            (
                json!({"confirm": true, "body": "x", "group_id": 1, "reply_to_message_id": 2}),
                "rejected:group_on_reply",
            ),
        ];
        for (v, want) in cases {
            let got = validate_send(&body(v.clone())).unwrap_err();
            assert_eq!(got.reason, want, "{v}");
            assert_eq!(got.error.status, StatusCode::BAD_REQUEST, "{v}");
        }
        // An uncommented forward is a send.
        assert!(
            validate_send(&body(
                json!({"confirm": true, "body": "", "forward_of_message_id": 1, "to": "a@b.c"})
            ))
            .is_ok()
        );
    }

    #[test]
    fn outcomes_serialize_to_the_wire_shape_action_send_always_had() {
        let sent = SendOutcome::Sent {
            echo_message_id: None,
            thread_id: Some("t1".into()),
        };
        assert_eq!(
            sent.to_json(),
            json!({"status": "sent", "echo_message_id": null, "thread_id": "t1"})
        );
        let sending = SendOutcome::Sending {
            group_send_id: 7,
            recipients: 3,
        };
        assert_eq!(
            sending.to_json(),
            json!({"status": "sending", "group_send_id": 7, "recipients": 3})
        );
    }

    #[test]
    fn a_fired_group_is_attributed_to_the_addresses_the_request_named() {
        // The field the mode expanded into, as it was frozen, bare, each
        // once: who the mail actually went to there, whatever the group says
        // now. The other field is never read.
        let b = body(json!({
            "to": "\"Ann, A.\" <ann@fund.com>, boss@work.com",
            "bcc": "bo@fund.com, cy@fund.com, BO@fund.com",
            "body": "hi"
        }));
        assert_eq!(
            frozen_addrs(&b, GroupMode::To),
            vec!["ann@fund.com", "boss@work.com"]
        );
        assert_eq!(
            frozen_addrs(&b, GroupMode::Bcc),
            vec!["bo@fund.com", "cy@fund.com"]
        );
        assert!(frozen_addrs(&body(json!({ "body": "hi" })), GroupMode::Bcc).is_empty());
    }

    #[test]
    fn a_paced_send_is_paced_not_a_gmail_failure() {
        let f = SendFailure::after_post("failed:gmail", &WriteError::Paced { retry_in_secs: 3 });
        assert_eq!((f.reason, f.paced, f.maybe_sent), (PACED, true, false));
        let f = SendFailure::after_post(
            "failed:gmail",
            &WriteError::Api {
                status: 503,
                message: String::new(),
            },
        );
        assert_eq!(
            (f.reason, f.paced, f.maybe_sent),
            ("failed:gmail", false, true)
        );
    }
}

/// Which writer [`write_client`] hands out: the account's provider's, and
/// never Gmail's by default.
#[cfg(test)]
mod write_client_tests {
    use super::*;
    use squelch_core::credentials::{CredentialStore, OAuthToken};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    struct StubCreds;
    #[async_trait::async_trait]
    impl CredentialStore for StubCreds {
        async fn token(&self, _account: AccountId) -> squelch_core::Result<OAuthToken> {
            Ok(OAuthToken {
                access_token: "WRITE-TOKEN".into(),
                refresh_token: None,
                expires_at: None,
            })
        }
    }

    /// A file-backed store with one account, so a second connection can set
    /// its `accounts.provider` the way a newer build's setup would have.
    struct Fixture {
        dir: std::path::PathBuf,
        store: Arc<SqliteStore>,
        acct: AccountId,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "squelch-write-client-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let store = Arc::new(SqliteStore::open(dir.join("store.sqlite3")).unwrap());
            let acct = store.ensure_account("me@example.com").unwrap();
            Self { dir, store, acct }
        }

        fn set_provider(&self, provider: &str) {
            rusqlite::Connection::open(self.dir.join("store.sqlite3"))
                .unwrap()
                .execute(
                    "UPDATE accounts SET provider = ?1 WHERE id = ?2",
                    rusqlite::params![provider, self.acct],
                )
                .unwrap();
        }

        fn state(&self) -> ApiState {
            ApiState::new(self.store.clone(), self.acct, "probe-token")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A mock API that answers the first request with `{}` and returns it,
    /// or `None` when nothing connected within half a second.
    async fn mock_api() -> (String, tokio::task::JoinHandle<Option<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let accepted =
                tokio::time::timeout(Duration::from_millis(500), listener.accept()).await;
            let Ok(Ok((mut sock, _))) = accepted else {
                return None;
            };
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                  Content-Length: 2\r\nConnection: close\r\n\r\n{}",
            )
            .await
            .unwrap();
            sock.flush().await.unwrap();
            Some(req)
        });
        (format!("http://{addr}"), handle)
    }

    #[tokio::test]
    async fn a_gmail_account_gets_the_gmail_writer() {
        let fx = Fixture::new("gmail");
        let (base, handle) = mock_api().await;
        let state = fx
            .state()
            .with_write_test_harness(Arc::new(StubCreds), base);
        let writer = write_client(&state)
            .await
            .expect("a Gmail account has a writer");
        writer.archive("gm-1").await.unwrap();
        let req = handle.await.unwrap().expect("the archive reached the API");
        // Gmail's archive, exactly: a label modify removing INBOX.
        assert!(req.starts_with("POST /messages/gm-1/modify "), "{req}");
        assert!(req.contains("\"removeLabelIds\":[\"INBOX\"]"), "{req}");
    }

    /// THE PROVIDER IS READ ONCE PER DOOR. After the first writer, a later
    /// action neither pays a store call for it nor can fail on one: a store
    /// hiccup must not fail a write the credential would have allowed.
    #[tokio::test]
    async fn the_provider_is_read_once_and_kept() {
        let fx = Fixture::new("cached");
        let state = fx
            .state()
            .with_write_test_harness(Arc::new(StubCreds), "http://127.0.0.1:9".into());
        assert!(write_client(&state).await.is_ok());
        // What a second read would now see: a provider this build refuses.
        fx.set_provider("imap");
        assert!(
            write_client(&state.clone()).await.is_ok(),
            "the cached read stands, on every clone of the state"
        );
    }

    /// A refusal is audited as what refused, so a store failure is never
    /// recorded as "re-authorize".
    #[test]
    fn a_refusal_is_audited_as_what_refused() {
        let at = |status| ApiError::new(status, "x");
        assert_eq!(
            write_refusal(&at(StatusCode::FORBIDDEN)),
            "rejected:no_write_credential"
        );
        assert_eq!(
            write_refusal(&at(StatusCode::NOT_IMPLEMENTED)),
            "rejected:unsupported_provider"
        );
        assert_eq!(
            write_refusal(&at(StatusCode::INTERNAL_SERVER_ERROR)),
            "failed:store"
        );
    }

    #[tokio::test]
    async fn an_explicit_gmail_row_is_the_same_gmail_writer() {
        let fx = Fixture::new("explicit-gmail");
        fx.set_provider("gmail");
        let (base, handle) = mock_api().await;
        let state = fx
            .state()
            .with_write_test_harness(Arc::new(StubCreds), base);
        write_client(&state)
            .await
            .unwrap()
            .trash("gm-2")
            .await
            .unwrap();
        let req = handle.await.unwrap().expect("the trash reached the API");
        assert!(req.starts_with("POST /messages/gm-2/trash "), "{req}");
    }

    /// A database a newer build wrote (an `imap` account) must not have its
    /// mail written to as Gmail by this one: refused, and nothing is sent.
    #[tokio::test]
    async fn a_provider_this_build_does_not_know_is_refused_not_treated_as_gmail() {
        let fx = Fixture::new("unknown");
        fx.set_provider("imap");
        let (base, handle) = mock_api().await;
        let state = fx
            .state()
            .with_write_test_harness(Arc::new(StubCreds), base);
        let err = match write_client(&state).await {
            Ok(_) => panic!("an unknown provider must not get a writer"),
            Err(e) => e,
        };
        assert_eq!(err.status, StatusCode::NOT_IMPLEMENTED);
        assert_eq!(handle.await.unwrap(), None, "nothing reached the API");
    }

    /// The credential is checked first, so a door without one answers with
    /// the same 403 it always has, whatever the account's provider.
    #[tokio::test]
    async fn no_write_credential_is_still_the_403_whatever_the_provider() {
        let fx = Fixture::new("no-creds");
        fx.set_provider("imap");
        let err = match write_client(&fx.state()).await {
            Ok(_) => panic!("no credential, no writer"),
            Err(e) => e,
        };
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert!(err.message.contains("squelchd auth`"));
    }
}
