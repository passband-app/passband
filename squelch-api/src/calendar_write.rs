//! The user's own writes to their Google Calendar: create, edit and delete
//! one event, a direct action on the calendar page (docs/CALENDAR.md §8,
//! "Creating and editing events").
//!
//! - `POST   /client/calendar/events` creates, 201 with the served event
//!   (200 when the id it names was already created: see below).
//! - `PATCH  /client/calendar/events/{calendar_id}/{id}` edits, 200 with it.
//! - `DELETE /client/calendar/events/{calendar_id}/{id}?notify=` deletes, 204.
//!
//! THE ORDER IS THE CONTRACT: check everything (the grant, the body, the
//! calendar, the event and the edit guard), then Google, then the local row,
//! then the sync kick. Google's refusal (or silence) leaves every local row
//! exactly as it was; Google's answer, not the request, is what gets stored,
//! read by [`squelch_core::gcal::read_written_event`] like any synced event.
//! Writes go through the same token resolver the sync uses
//! ([`CalendarCredentials`]).
//!
//! GUESTS, AND THE ONE WAY ANYONE IS EMAILED (docs/CALENDAR.md §8, "Guests,
//! invited by Google"). A create or an edit may name `guests`; Google, not
//! Passband, writes and sends the invitation, the update and the
//! cancellation, and ONLY when the request says `notify: true` (`?notify=true`
//! on a delete). Absent or false sends `sendUpdates=none`; nothing else can
//! turn it on ([`write::notify`], [`Notify`]), and no route but these three
//! reaches it. The guards, each refused before Google hears anything:
//!
//! - guests only on the user's OWN calendar ([`StoredCalendar::can_invite`]);
//! - an existing event's list only when it is `guest_editable`
//!   ([`SyncedEvent::guest_editable`]: editable, organized by the user, and
//!   NOT truncated);
//! - at most 50 plain addresses, no duplicates, never the owner;
//! - `notify: true` on an edit or a delete (an update or a cancellation
//!   mailed under the calendar's name) only on a calendar that may invite,
//!   the same rule ([`refuse_notify_unless_own`]).
//!
//! A guest edit sends Google the FULL attendee list, because `events.patch`
//! replaces the array. It is built from Google's own list read in the same
//! request ([`CalendarApi::event_attendees`]), so a guest who stays keeps the
//! answer and every other field Google holds, as of now and not as of the
//! last sync; if that read comes back truncated the edit is refused. An edit
//! that does not name `guests` sends no `attendees` key at all.
//!
//! EVERY ATTEMPT IS AUDITED, in the shape mail actions use: one row per
//! request under `calendar_event_create` / `_edit` / `_delete`, target
//! `calendar:<calendar_id>/<event_id>` (`calendar:<calendar_id>` for a create
//! Google has not answered, nothing before the calendar is known to be one
//! being read), detail `ok` or why not, then, once the request is understood,
//! ` guests=<count> notified=<true|false>`: how many guests the event has
//! after the write (before it, for a delete or a refusal; `<count>+` when
//! Google holds more than are stored) and whether Google was asked to mail
//! them. Never a title, a place, a description or a GUEST'S address. The
//! target does carry the calendar id, and a calendar id can be an address:
//! the primary calendar is stored as `primary`, but a personal calendar
//! someone shared with the user is named by its owner's address
//! (`calendar:colleague@example.com/<event>`). Audit rows stay on the human
//! door, as SECURITY.md section 9 says. The `calendar:` namespace is the
//! credential rows' trick (`token:<id>`): the audit listing joins a numeric
//! target to a message, and a calendar id can start with digits.
//!
//! THE AUDIT ROW SURVIVES A DISCONNECT. Each handler's work (the checks, the
//! Google call and the audit write) runs in a task of its own that the
//! handler awaits ([`detached`]). If the app goes away mid-request the
//! handler's future is dropped, but the task is not: Google's answer is
//! still stored and the row still written, so a write that may have mailed
//! guests is never left unaudited.
//!
//! WHEN GOOGLE'S ANSWER IS LOST. A refusal from Google (any HTTP error) is a
//! 502, [`GOOGLE_REFUSED`], code `google_refused`: nothing changed. A write
//! that went out and got no usable answer (a timeout or a lost connection
//! after sending, a 2xx body that does not read: [`CallError::Uncertain`]) is
//! a 504, [`GOOGLE_UNCERTAIN`], code `uncertain`, and kicks a sync: Google
//! may have made the change, so the client refreshes before offering to try
//! again, and a "save and tell guests" is not sent twice on a guess. So is a
//! write Google ACCEPTED whose answer could not be read or stored here: it
//! happened, and only a 504 makes the app hold the pane rather than offer
//! the draft back for editing.
//!
//! EVERY ERROR BODY CARRIES A `code` from a closed list ([`code`]), which the
//! app keys on: a code that says nothing reached Google lets the pane be
//! edited again, and anything else (no code, an unknown one) holds it.
//!
//! ONE DEADLINE PER WRITE. Each Google call has its own 30 second timeout,
//! and a write can make three (a token refresh, the guest list's reread, the
//! write), more than the app waits. So the whole of a write's work runs under
//! [`WRITE_DEADLINE`], well inside the app's 45 seconds: past it the answer is
//! the same 504 and the same kick, since the write may have gone out. The
//! task keeps no Google call running past it.
//!
//! A CREATE SENT AGAIN FINDS THE FIRST. The app names the new event's `id`
//! itself (Google allows a client-chosen id: lowercase `a`-`v` and digits, 5
//! to 1024 characters, [`write::event_id`]), one per pane, and after an
//! uncertain answer can only resend the same body. Google "cannot guarantee
//! that ID collisions will be detected at event creation time", so the daemon
//! does not rely on its 409: a create naming an id READS it first
//! ([`CalendarApi::get_event`]). An event already there is answered 200 as it
//! stands, with no insert and so no second invitation (audited `ok:existing
//! notified=false`), but ONLY when it is the event this create would make
//! ([`write::is_same_create`]: title, place, notes, times, all-day and
//! guests, on the calendar read; not `notify`, which Google's event does not
//! record, and which a resend from the app never changes). The same id with anything else is a 409, [`ID_IN_USE`],
//! with no write. An insert Google answers 409 (`duplicate`) is read and
//! judged the same way. One already made and since deleted is a 409,
//! [`ALREADY_DELETED`]. A second create with the same id while the first is
//! still running is a 409, [`ALREADY_CREATING`], before Google hears it
//! ([`InFlight`]). What this does NOT close: a read that lags an insert just
//! made, followed by an insert Google fails to flag, can still make two; and
//! the read is per calendar. A create with no `id` (an older app) lets Google
//! choose, as before.
//!
//! A DELETE GOOGLE ANSWERS 410 found the event already gone, and removes the
//! local row (204). A 404 says the same only while the calendar is still the
//! user's: Google answers 404 too for a calendar the user can no longer
//! reach, or an event they can no longer see. So on a calendar other than
//! the primary one a 404 is checked against the calendar list
//! ([`CalendarApi::calendar_listed`]): still listed with an `owner` or
//! `writer` role is done (204); anything else is a 410, [`CALENDAR_GONE`],
//! the row is left for the kicked sync, and the app does not say "deleted".
//! A list read that fails is a 502 and deletes nothing.
//!
//! Error bodies are fixed strings that never echo the request, and logs carry
//! the action and an error kind only (a calendar id is usually an address).
//!
//! HUMAN DOOR ONLY: nothing on `/mcp` or `/client/agent/*` reaches any of it,
//! and the in-app assistant has no tool for it.

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::Value;
use squelch_core::credentials::CalendarCredentials;
use squelch_core::gcal::client::{API_BASE, CalendarApi, CallError, Notify};
use squelch_core::gcal::write::{self, Invalid};
use squelch_core::gcal::{EventRow, StoredCalendar, SyncedEvent, read_written_event};

use crate::error::ApiError;
use crate::handlers::{audit_action, calendar_event_view, no_store, store_call};
use crate::state::ApiState;

const CREATE: &str = "calendar_event_create";
const EDIT: &str = "calendar_event_edit";
const DELETE: &str = "calendar_event_delete";

/// The longest id taken from a path: Google's own ids are far shorter, and
/// anything longer cannot name a stored event (the sync refuses to store
/// one).
const PATH_ID_CAP: usize = 1024;

pub(crate) const NOT_GRANTED: &str = "Google Calendar is not connected";
pub(crate) const UNKNOWN_CALENDAR: &str = "calendar_id must name a calendar Passband reads";
pub(crate) const READ_ONLY_CALENDAR: &str = "that calendar is read-only";
pub(crate) const NOT_EDITABLE: &str = "this event cannot be changed from Passband";
pub(crate) const GOOGLE_REFUSED: &str = "Google Calendar did not accept the change";
pub(crate) const GOOGLE_UNCERTAIN: &str =
    "Google Calendar may have made this change; refresh before trying again";
pub(crate) const CANNOT_NOTIFY: &str = "guests can only be told about changes on your own calendar";
pub(crate) const UNREADABLE_ANSWER: &str =
    "Google Calendar saved the change, but its answer could not be read";
pub(crate) const UNSTORED_ANSWER: &str =
    "Google Calendar saved the change, but Passband could not store it";
pub(crate) const CANNOT_INVITE: &str = "guests can only be added on your own calendar";
pub(crate) const GUESTS_READ_ONLY: &str = "this event's guest list cannot be changed from Passband";
pub(crate) const CALENDAR_GONE: &str = "that calendar is no longer available to you";
pub(crate) const ALREADY_DELETED: &str =
    "that event was already created and has since been deleted";
pub(crate) const ID_IN_USE: &str = "that event id is already used for a different event";
pub(crate) const ALREADY_CREATING: &str = "that event is already being created";

/// EVERY ERROR BODY NAMES ITS KIND, `{"error", "code"}`: a closed list the
/// app reads (docs/CALENDAR.md §8, "What a refusal says"). The app keys on
/// the code, never the status alone, and treats anything not on the list,
/// or no code at all, as a write that may have happened.
///
/// NOTHING HAPPENED: refused before any write reached Google, or refused by
/// Google itself.
/// - [`code::INVALID`] 400: the body, a guest address, the delete's query.
/// - [`code::UNKNOWN_CALENDAR`], [`code::READ_ONLY`],
///   [`code::CANNOT_INVITE`], [`code::GUESTS_READ_ONLY`],
///   [`code::CANNOT_NOTIFY`] 400.
/// - [`code::NOT_GRANTED`], [`code::NOT_EDITABLE`] 403.
/// - [`code::NOT_FOUND`] 404: no such stored event.
/// - [`code::ID_IN_USE`], [`code::ALREADY_CREATING`],
///   [`code::ALREADY_DELETED`] 409.
/// - [`code::CALENDAR_GONE`] 410: Google answered a delete 404 and the
///   calendar is no longer the user's to write.
/// - [`code::GOOGLE_REFUSED`] 502: Google answered the write (or a read or
///   a token refresh before it) with an error.
///
/// MAY HAVE HAPPENED:
/// - [`code::UNCERTAIN`] 504: the answer was lost or the deadline passed
///   after the write may have gone out, or Google ACCEPTED the write and
///   its answer could not be read or stored.
/// - [`code::INTERNAL`] 500: anything else.
pub(crate) mod code {
    pub(crate) const INVALID: &str = "invalid";
    pub(crate) const UNKNOWN_CALENDAR: &str = "unknown_calendar";
    pub(crate) const READ_ONLY: &str = "read_only";
    pub(crate) const CANNOT_INVITE: &str = "cannot_invite";
    pub(crate) const GUESTS_READ_ONLY: &str = "guests_read_only";
    pub(crate) const CANNOT_NOTIFY: &str = "cannot_notify";
    pub(crate) const NOT_GRANTED: &str = "not_granted";
    pub(crate) const NOT_EDITABLE: &str = "not_editable";
    pub(crate) const NOT_FOUND: &str = "not_found";
    pub(crate) const ID_IN_USE: &str = "id_in_use";
    pub(crate) const ALREADY_CREATING: &str = "already_creating";
    pub(crate) const ALREADY_DELETED: &str = "already_deleted";
    pub(crate) const CALENDAR_GONE: &str = "calendar_gone";
    pub(crate) const GOOGLE_REFUSED: &str = "google_refused";
    pub(crate) const UNCERTAIN: &str = "uncertain";
    pub(crate) const INTERNAL: &str = "internal";
}

/// The most one calendar write may take in all ([`within`]): below the app's
/// own 45 second wait, so the app hears the 504 rather than giving up first.
pub(crate) const WRITE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(35);

/// A refused request: its audit detail and its answer.
struct Refusal {
    detail: &'static str,
    error: ApiError,
}

impl Refusal {
    fn new(
        detail: &'static str,
        status: StatusCode,
        code: &'static str,
        message: &'static str,
    ) -> Self {
        Self {
            detail,
            error: ApiError::new(status, message).with_code(code),
        }
    }
    fn body(Invalid(message): Invalid) -> Self {
        Self::new(
            "rejected:body",
            StatusCode::BAD_REQUEST,
            code::INVALID,
            message,
        )
    }
    fn not_granted() -> Self {
        Self::new(
            "rejected:not_granted",
            StatusCode::FORBIDDEN,
            code::NOT_GRANTED,
            NOT_GRANTED,
        )
    }
    /// A READ before the write failed (the guest list's fresh read): nothing
    /// was asked to change, so every failure is a plain refusal.
    fn google(action: &str, error: CallError) -> Self {
        eprintln!("squelch-api: {action}: Google refused the write ({error})");
        Self::new(
            "failed:google",
            StatusCode::BAD_GATEWAY,
            code::GOOGLE_REFUSED,
            GOOGLE_REFUSED,
        )
    }
    /// The WRITE itself failed. An answer Google gave is a refusal (502); no
    /// usable answer after sending ([`CallError::Uncertain`]) means the change
    /// may have happened: 504 with its own message, and a sync is kicked so
    /// the client's refresh shows what Google holds.
    fn written(state: &ApiState, action: &str, error: CallError) -> Self {
        if error != CallError::Uncertain {
            return Self::google(action, error);
        }
        eprintln!("squelch-api: {action}: Google's answer to the write was lost ({error})");
        state.calendar_refresh.kick_now(state.account_id);
        Self::new(
            "failed:uncertain",
            StatusCode::GATEWAY_TIMEOUT,
            code::UNCERTAIN,
            GOOGLE_UNCERTAIN,
        )
    }
    /// Google ACCEPTED the write, and its answer could not be read
    /// (`failed:answer`) or stored (`failed:store`): the change happened, so
    /// it is answered as one that may have, never as a refusal. The caller
    /// has kicked the sync that brings the event in.
    fn accepted_unseen(detail: &'static str, message: &'static str) -> Self {
        Self::new(
            detail,
            StatusCode::GATEWAY_TIMEOUT,
            code::UNCERTAIN,
            message,
        )
    }
    fn internal(detail: &'static str) -> Self {
        Self::new(
            detail,
            StatusCode::INTERNAL_SERVER_ERROR,
            code::INTERNAL,
            "internal error",
        )
    }
}

/// What the audit row says about guests: how many the event has (other than
/// the user), and whether Google was asked to mail them. A COUNT, never an
/// address.
#[derive(Debug, Clone, Copy)]
struct GuestNote {
    guests: usize,
    /// Google holds more guests than are stored: the count is a floor.
    more: bool,
    notify: Notify,
}

impl GuestNote {
    fn of(row: &EventRow, notify: Notify) -> Self {
        Self {
            guests: row.other_guests(),
            more: row.guests_truncated,
            notify,
        }
    }
}

/// Write the audit row for `outcome` and answer it. `note` is None until
/// the request has been understood that far. `ok` is a success's detail.
async fn finish<T>(
    state: &ApiState,
    action: &'static str,
    target: Option<String>,
    note: Option<GuestNote>,
    ok: &'static str,
    outcome: Result<T, Refusal>,
) -> Result<T, ApiError> {
    let (detail, result) = match outcome {
        Ok(value) => (ok, Ok(value)),
        Err(refusal) => (refusal.detail, Err(refusal.error)),
    };
    let detail = match note {
        Some(note) => format!(
            "{detail} guests={}{} notified={}",
            note.guests,
            if note.more { "+" } else { "" },
            note.notify.tells_guests()
        ),
        None => detail.to_string(),
    };
    audit_action(state, action, target, &detail).await;
    result
}

fn target(calendar_id: &str, event_id: Option<&str>) -> String {
    match event_id {
        Some(event_id) => format!("calendar:{calendar_id}/{event_id}"),
        None => format!("calendar:{calendar_id}"),
    }
}

/// The resolver, when calendar is granted.
async fn granted(state: &ApiState) -> Result<Arc<CalendarCredentials>, Refusal> {
    match state.calendar() {
        Some(creds) if creds.granted().await => Ok(creds.clone()),
        _ => Err(Refusal::not_granted()),
    }
}

/// A token for the write and the client to send it with. A grant that turns
/// out to be gone is 403, a token that cannot be refreshed is Google's
/// failure.
async fn google(
    state: &ApiState,
    creds: &CalendarCredentials,
    action: &str,
) -> Result<(String, CalendarApi), Refusal> {
    let token = match creds.calendar_token(state.account_id).await {
        Ok(Some(token)) => token.access_token,
        Ok(None) => return Err(Refusal::not_granted()),
        Err(_) => {
            eprintln!("squelch-api: {action}: no usable calendar token");
            return Err(Refusal::new(
                "failed:token",
                StatusCode::BAD_GATEWAY,
                code::GOOGLE_REFUSED,
                GOOGLE_REFUSED,
            ));
        }
    };
    let base = state.calendar_api_base.as_deref().unwrap_or(API_BASE);
    let api = CalendarApi::new(base).map_err(|_| Refusal::internal("failed:client"))?;
    Ok((token, api))
}

fn json_body(body: &Bytes) -> Result<Value, Refusal> {
    write::body(body).map_err(Refusal::body)
}

/// The calendar a write names, as stored.
async fn calendar(
    state: &ApiState,
    calendar_id: String,
) -> Result<Option<StoredCalendar>, Refusal> {
    store_call(state, move |store, account| {
        Ok(store
            .calendar_calendars(account)?
            .into_iter()
            .find(|c| c.selected && c.calendar_id == calendar_id))
    })
    .await
    .map_err(|_| Refusal::internal("failed:store"))
}

/// The stored event a path names, with its calendar.
async fn stored_event(
    state: &ApiState,
    calendar_id: &str,
    event_id: &str,
) -> Result<(StoredCalendar, SyncedEvent), Refusal> {
    let missing = || {
        Refusal::new(
            "failed:target",
            StatusCode::NOT_FOUND,
            code::NOT_FOUND,
            "not found",
        )
    };
    if calendar_id.chars().count() > PATH_ID_CAP || event_id.chars().count() > PATH_ID_CAP {
        return Err(missing());
    }
    let cal = calendar(state, calendar_id.to_string())
        .await?
        .ok_or_else(missing)?;
    let (c, e) = (calendar_id.to_string(), event_id.to_string());
    let event = store_call(state, move |store, account| {
        store.calendar_event(account, &c, &e)
    })
    .await
    .map_err(|_| Refusal::internal("failed:store"))?
    .ok_or_else(missing)?;
    Ok((cal, event))
}

/// THE INVITE GUARD, at the door: `guests` may be named only on a calendar
/// that may invite, and never include its owner (the mailbox's address, or
/// the calendar's own id on a secondary calendar).
fn may_carry_guests(
    calendar: &StoredCalendar,
    guests: &[String],
    account_email: &str,
) -> Result<(), Refusal> {
    if !calendar.can_invite() {
        return Err(Refusal::new(
            "rejected:cannot_invite",
            StatusCode::BAD_REQUEST,
            code::CANNOT_INVITE,
            CANNOT_INVITE,
        ));
    }
    write::check_not_owner(guests, &[account_email, &calendar.calendar_id]).map_err(Refusal::body)
}

/// THE NOTIFY GUARD: `notify: true` on an edit or a delete makes Google mail
/// every guest an update or a cancellation under the calendar's name, which
/// is what an invitation is. So, like guests ([`may_carry_guests`]), it is
/// refused on a calendar that may not invite: one the user only writes to.
fn refuse_notify_unless_own(calendar: &StoredCalendar, notify: Notify) -> Result<(), Refusal> {
    if notify.tells_guests() && !calendar.can_invite() {
        return Err(Refusal::new(
            "rejected:cannot_notify",
            StatusCode::BAD_REQUEST,
            code::CANNOT_NOTIFY,
            CANNOT_NOTIFY,
        ));
    }
    Ok(())
}

/// THE WRITE'S DEADLINE: `work` (everything from the grant check to the
/// stored answer) gets [`ApiState::calendar_write_deadline`] in all. Past it
/// the work is dropped, Google calls and all, and the answer is the 504 a
/// lost answer gets, with its sync kick: a write already sent may have
/// happened.
async fn within<T>(
    state: &ApiState,
    action: &str,
    work: impl std::future::Future<Output = Result<T, Refusal>>,
) -> Result<T, Refusal> {
    match tokio::time::timeout(state.calendar_write_deadline, work).await {
        Ok(outcome) => outcome,
        Err(_) => {
            eprintln!("squelch-api: {action}: the write ran past its deadline");
            Err(Refusal::written(state, action, CallError::Uncertain))
        }
    }
}

/// Run a handler's work in a task of its own and wait for it. Dropping the
/// request (the client went away) drops this future but NOT the task, so a
/// Google call already sent still has its answer stored and its audit row
/// written. A task that panicked is a 500, as it would have been inline.
async fn detached<T: Send + 'static>(
    work: impl std::future::Future<Output = Result<T, ApiError>> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::spawn(work).await.unwrap_or_else(|_| {
        Err(
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
                .with_code(code::INTERNAL),
        )
    })
}

/// One create naming its own id, held in
/// [`ApiState::calendar_creates_in_flight`] for as long as this value lives.
/// Dropped on every way out of the work, an answer, a refusal, the deadline
/// dropping the work, or a panic unwinding it, so a key is never left
/// behind.
pub(crate) struct InFlight {
    set: Arc<std::sync::Mutex<std::collections::HashSet<(i64, String, String)>>>,
    key: (i64, String, String),
}

impl InFlight {
    /// Take the key, or None when a create holding it is still running.
    fn take(state: &ApiState, calendar_id: &str, id: &str) -> Option<Self> {
        let key = (state.account_id, calendar_id.to_string(), id.to_string());
        let set = state.calendar_creates_in_flight.clone();
        let fresh = set
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.clone());
        fresh.then_some(Self { set, key })
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.set
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.key);
    }
}

fn guests_read_only() -> Refusal {
    Refusal::new(
        "rejected:guests_read_only",
        StatusCode::BAD_REQUEST,
        code::GUESTS_READ_ONLY,
        GUESTS_READ_ONLY,
    )
}

/// Google said yes: store its answer under the calendar, kick the sync, and
/// serve the event (and hand back the row that was stored). An answer that
/// cannot be read is stored nowhere (the kicked sync brings the event in),
/// and one that cannot be stored is not served: both are the 504
/// [`code::UNCERTAIN`], never a refusal, since the change DID happen at
/// Google.
async fn keep_answer(
    state: &ApiState,
    calendar: &StoredCalendar,
    answer: &Value,
    account_email: &str,
) -> Result<(EventRow, Value), Refusal> {
    let Some(row) = read_written_event(answer, calendar.time_zone.as_deref()) else {
        state.calendar_refresh.kick_now(state.account_id);
        return Err(Refusal::accepted_unseen("failed:answer", UNREADABLE_ANSWER));
    };
    let stored = put(state, &calendar.calendar_id, &row).await;
    state.calendar_refresh.kick_now(state.account_id);
    stored?;
    let event = SyncedEvent {
        calendar_id: calendar.calendar_id.clone(),
        calendar_name: calendar.summary.clone(),
        calendar_color: calendar.color.clone(),
        calendar_access: calendar.access_role,
        calendar_primary: calendar.is_primary,
        row,
    };
    let view = calendar_event_view(&event, account_email);
    Ok((event.row, view))
}

async fn put(state: &ApiState, calendar_id: &str, row: &EventRow) -> Result<(), Refusal> {
    let (cal, row) = (calendar_id.to_string(), row.clone());
    store_call(state, move |store, account| {
        store.calendar_put_event(account, &cal, &row)
    })
    .await
    .map(|_| ())
    .map_err(|_| {
        eprintln!("squelch-api: calendar write: Google accepted it; storing the answer failed");
        Refusal::accepted_unseen("failed:store", UNSTORED_ANSWER)
    })
}

// --- POST /client/calendar/events --------------------------------------------

/// `POST /client/calendar/events` with `{"calendar_id", "title", "start",
/// "end", "all_day", "location"?, "description"?, "guests"?, "notify"?}`.
/// See the module doc. With `notify: true` Google mails each guest an
/// invitation; without it the guests are on the event and nobody is told.
pub async fn create_event(
    State(state): State<ApiState>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let (status, view) = detached(create(state, body)).await?;
    Ok((status, no_store(), Json(view)))
}

async fn create(state: ApiState, body: Bytes) -> Result<(StatusCode, Value), ApiError> {
    let mut audit_target = None;
    let mut note = None;
    let mut ok = "ok";
    let outcome = within(&state, CREATE, async {
        let creds = granted(&state).await?;
        let draft = write::parse_create(&json_body(&body)?).map_err(Refusal::body)?;
        note = Some(GuestNote {
            guests: draft.guests.len(),
            more: false,
            notify: draft.notify,
        });
        let cal = calendar(&state, draft.calendar_id.clone())
            .await?
            .ok_or_else(|| {
                Refusal::new(
                    "rejected:calendar",
                    StatusCode::BAD_REQUEST,
                    code::UNKNOWN_CALENDAR,
                    UNKNOWN_CALENDAR,
                )
            })?;
        audit_target = Some(target(&cal.calendar_id, None));
        if !cal.writable() {
            return Err(Refusal::new(
                "rejected:read_only",
                StatusCode::BAD_REQUEST,
                code::READ_ONLY,
                READ_ONLY_CALENDAR,
            ));
        }
        if !draft.guests.is_empty() {
            may_carry_guests(&cal, &draft.guests, creds.account_email())?;
        }
        let google_body = write::insert_body(&draft).map_err(Refusal::body)?;
        // One create per id at a time: a second send while the first is
        // still out is refused before Google hears it, not raced.
        let _in_flight = match &draft.id {
            None => None,
            Some(id) => Some(InFlight::take(&state, &cal.calendar_id, id).ok_or_else(|| {
                Refusal::new(
                    "rejected:in_flight",
                    StatusCode::CONFLICT,
                    code::ALREADY_CREATING,
                    ALREADY_CREATING,
                )
            })?),
        };
        let (token, api) = google(&state, &creds, CREATE).await?;
        // A create naming its own id may have been sent before: read it, and
        // an event already there is the answer. Nothing is inserted, nobody
        // is invited again.
        let existing = match &draft.id {
            None => None,
            Some(id) => match api.get_event(&token, &cal.calendar_id, id).await {
                Ok(event) => Some(event),
                Err(CallError::NotFound) => None,
                Err(e) => return Err(Refusal::google(CREATE, e)),
            },
        };
        let answer = match existing {
            Some(event) => event,
            None => match api
                .insert_event(&token, &cal.calendar_id, &google_body, draft.notify)
                .await
            {
                Ok(answer) => {
                    let (row, view) =
                        keep_answer(&state, &cal, &answer, creds.account_email()).await?;
                    audit_target = Some(target(&cal.calendar_id, Some(&row.event_id)));
                    return Ok((StatusCode::CREATED, view));
                }
                // Google noticed the id is taken: the first send made it.
                Err(CallError::Duplicate) if draft.id.is_some() => {
                    let id = draft.id.as_deref().unwrap_or_default();
                    api.get_event(&token, &cal.calendar_id, id)
                        .await
                        .map_err(|_| Refusal::written(&state, CREATE, CallError::Uncertain))?
                }
                Err(e) => return Err(Refusal::written(&state, CREATE, e)),
            },
        };
        // An event already under the id. This send writes nothing and tells
        // nobody, whatever it answers.
        audit_target = Some(target(&cal.calendar_id, draft.id.as_deref()));
        note = note.map(|n| GuestNote {
            notify: Notify::Nobody,
            ..n
        });
        if answer.get("status").and_then(Value::as_str) == Some("cancelled") {
            return Err(Refusal::new(
                "rejected:already_deleted",
                StatusCode::CONFLICT,
                code::ALREADY_DELETED,
                ALREADY_DELETED,
            ));
        }
        // It is the answer only when it is the event THIS create would make.
        // The same id with another title, time or guest list (an edited
        // draft, or someone else's event) is refused, never answered with
        // what is there: that would drop the edit, or claim an invitation
        // nobody sent.
        let zone = cal.time_zone.as_deref();
        let ours = read_written_event(&answer, zone)
            .is_some_and(|found| write::is_same_create(&draft, &found, zone));
        if !ours {
            return Err(Refusal::new(
                "rejected:id_in_use",
                StatusCode::CONFLICT,
                code::ID_IN_USE,
                ID_IN_USE,
            ));
        }
        let (row, view) = keep_answer(&state, &cal, &answer, creds.account_email()).await?;
        audit_target = Some(target(&cal.calendar_id, Some(&row.event_id)));
        note = Some(GuestNote::of(&row, Notify::Nobody));
        ok = "ok:existing";
        Ok((StatusCode::OK, view))
    })
    .await;
    finish(&state, CREATE, audit_target, note, ok, outcome).await
}

// --- PATCH /client/calendar/events/{calendar_id}/{id} ------------------------

/// `PATCH /client/calendar/events/{calendar_id}/{id}` with any subset of the
/// create's fields. Absent fields are left alone; `location` and
/// `description` take `null` to clear. `guests`, when present, is the full
/// new guest list; absent, Google's list is not touched (no `attendees` key
/// is sent). With `notify: true` Google mails the event's guests about the
/// change.
pub async fn edit_event(
    State(state): State<ApiState>,
    Path((calendar_id, event_id)): Path<(String, String)>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let view = detached(edit(state, calendar_id, event_id, body)).await?;
    Ok((no_store(), Json(view)))
}

async fn edit(
    state: ApiState,
    calendar_id: String,
    event_id: String,
    body: Bytes,
) -> Result<Value, ApiError> {
    let mut audit_target = None;
    let mut note = None;
    let outcome = within(&state, EDIT, async {
        let creds = granted(&state).await?;
        let edit = write::parse_edit(&json_body(&body)?, &calendar_id).map_err(Refusal::body)?;
        let (cal, stored) = stored_event(&state, &calendar_id, &event_id).await?;
        audit_target = Some(target(&calendar_id, Some(&event_id)));
        note = Some(GuestNote::of(&stored.row, edit.notify));
        if !stored.editable(creds.account_email()) {
            return Err(Refusal::new(
                "rejected:not_editable",
                StatusCode::FORBIDDEN,
                code::NOT_EDITABLE,
                NOT_EDITABLE,
            ));
        }
        if let Some(guests) = &edit.guests {
            // THE GUEST GUARDS, on what is stored: the calendar may invite,
            // and the event's list is the user's to change and is WHOLE.
            may_carry_guests(&cal, guests, creds.account_email())?;
            if !stored.guest_editable(creds.account_email()) {
                return Err(guests_read_only());
            }
        }
        refuse_notify_unless_own(&cal, edit.notify)?;
        let times = edit.times(&stored.row).map_err(Refusal::body)?;
        let (token, api) = google(&state, &creds, EDIT).await?;
        let attendees = match &edit.guests {
            // No `guests` in the request: no `attendees` in the patch.
            None => None,
            Some(guests) => {
                let (current, omitted) = api
                    .event_attendees(&token, &calendar_id, &event_id)
                    .await
                    .map_err(|e| Refusal::google(EDIT, e))?;
                // The same guard on what Google holds NOW: a list it cut
                // short must never be sent back as the whole one.
                if omitted {
                    return Err(guests_read_only());
                }
                // Nor one holding a guest it names no address for.
                Some(write::merge_attendees(&current, guests).ok_or_else(guests_read_only)?)
            }
        };
        let patch = write::patch_body(&edit, times.as_ref(), attendees).map_err(Refusal::body)?;
        let answer = api
            .patch_event(&token, &calendar_id, &event_id, &patch, edit.notify)
            .await
            .map_err(|e| Refusal::written(&state, EDIT, e))?;
        let (row, view) = keep_answer(&state, &cal, &answer, creds.account_email()).await?;
        note = Some(GuestNote::of(&row, edit.notify));
        Ok(view)
    })
    .await;
    finish(&state, EDIT, audit_target, note, "ok", outcome).await
}

// --- DELETE /client/calendar/events/{calendar_id}/{id} -----------------------

/// `notify` from a delete's query string: one `notify`, exactly `true` or
/// `false`, or none. Two of them is refused, not resolved.
fn delete_notify(query: &[(String, String)]) -> Result<Notify, Refusal> {
    let mut values = query
        .iter()
        .filter(|(key, _)| key == "notify")
        .map(|(_, value)| value.as_str());
    let first = values.next();
    if values.next().is_some() {
        return Err(Refusal::body(write::BAD_NOTIFY));
    }
    write::notify_query(first).map_err(Refusal::body)
}

/// `DELETE /client/calendar/events/{calendar_id}/{id}?notify=true|false`:
/// 204. An event Google says is already deleted (410) is deleted here too,
/// and so is one it does not know (404) while its calendar is still the
/// user's (410 [`CALENDAR_GONE`] otherwise; see the module doc). With `notify=true` Google mails the event's guests a
/// cancellation; absent or `false`, nobody is told.
pub async fn delete_event(
    State(state): State<ApiState>,
    Path((calendar_id, event_id)): Path<(String, String)>,
    Query(query): Query<Vec<(String, String)>>,
) -> Result<impl IntoResponse, ApiError> {
    detached(delete(state, calendar_id, event_id, query)).await?;
    Ok((StatusCode::NO_CONTENT, no_store()))
}

async fn delete(
    state: ApiState,
    calendar_id: String,
    event_id: String,
    query: Vec<(String, String)>,
) -> Result<(), ApiError> {
    let mut audit_target = None;
    let mut note = None;
    let outcome = within(&state, DELETE, async {
        let creds = granted(&state).await?;
        let notify = delete_notify(&query)?;
        let (cal, stored) = stored_event(&state, &calendar_id, &event_id).await?;
        audit_target = Some(target(&calendar_id, Some(&event_id)));
        note = Some(GuestNote::of(&stored.row, notify));
        if !stored.editable(creds.account_email()) {
            return Err(Refusal::new(
                "rejected:not_editable",
                StatusCode::FORBIDDEN,
                code::NOT_EDITABLE,
                NOT_EDITABLE,
            ));
        }
        refuse_notify_unless_own(&cal, notify)?;
        let (token, api) = google(&state, &creds, DELETE).await?;
        match api
            .delete_event(&token, &calendar_id, &event_id, notify)
            .await
        {
            // Gone (410) at Google: already deleted there.
            Ok(()) | Err(CallError::Gone) => {}
            // Unknown (404): already deleted, OR the calendar is no longer
            // the user's. The primary calendar always is; any other is asked
            // after.
            Err(CallError::NotFound) if !cal.is_primary => {
                match api.calendar_listed(&token, &calendar_id).await {
                    Ok(true) => {}
                    Ok(false) => {
                        state.calendar_refresh.kick_now(state.account_id);
                        return Err(Refusal::new(
                            "failed:calendar_gone",
                            StatusCode::GONE,
                            code::CALENDAR_GONE,
                            CALENDAR_GONE,
                        ));
                    }
                    Err(e) => return Err(Refusal::google(DELETE, e)),
                }
            }
            Err(CallError::NotFound) => {}
            Err(e) => return Err(Refusal::written(&state, DELETE, e)),
        }
        let (c, e) = (calendar_id.clone(), event_id.clone());
        let removed = store_call(&state, move |store, account| {
            store.calendar_remove_event(account, &c, &e)
        })
        .await;
        state.calendar_refresh.kick_now(state.account_id);
        // Google deleted it: a store that failed now is not a refusal.
        removed.map_err(|_| Refusal::accepted_unseen("failed:store", UNSTORED_ANSWER))?;
        Ok(())
    })
    .await;
    finish(&state, DELETE, audit_target, note, "ok", outcome).await
}
