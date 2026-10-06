//! The user's own writes to their Google Calendar: create, edit and delete
//! one event, a direct action on the calendar page (docs/CALENDAR.md §8,
//! "Creating and editing events").
//!
//! - `POST   /client/calendar/events` creates, 201 with the served event.
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
//! - at most 50 plain addresses, no duplicates, never the owner.
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
//! them. Never a title, a place, a description or an
//! ADDRESS. The `calendar:` namespace is the credential rows' trick
//! (`token:<id>`): the audit listing joins a numeric target to a message, and
//! a calendar id can start with digits.
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
pub(crate) const UNREADABLE_ANSWER: &str =
    "Google Calendar saved the change, but its answer could not be read";
pub(crate) const CANNOT_INVITE: &str = "guests can only be added on your own calendar";
pub(crate) const GUESTS_READ_ONLY: &str = "this event's guest list cannot be changed from Passband";

/// A refused request: its audit detail and its answer.
struct Refusal {
    detail: &'static str,
    error: ApiError,
}

impl Refusal {
    fn new(detail: &'static str, status: StatusCode, message: &'static str) -> Self {
        Self {
            detail,
            error: ApiError::new(status, message),
        }
    }
    fn body(Invalid(message): Invalid) -> Self {
        Self::new("rejected:body", StatusCode::BAD_REQUEST, message)
    }
    fn not_granted() -> Self {
        Self::new("rejected:not_granted", StatusCode::FORBIDDEN, NOT_GRANTED)
    }
    fn google(action: &str, error: CallError) -> Self {
        eprintln!("squelch-api: {action}: Google refused the write ({error})");
        Self::new("failed:google", StatusCode::BAD_GATEWAY, GOOGLE_REFUSED)
    }
    fn internal(detail: &'static str) -> Self {
        Self::new(detail, StatusCode::INTERNAL_SERVER_ERROR, "internal error")
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
/// the request has been understood that far.
async fn finish<T>(
    state: &ApiState,
    action: &'static str,
    target: Option<String>,
    note: Option<GuestNote>,
    outcome: Result<T, Refusal>,
) -> Result<T, ApiError> {
    let (detail, result) = match outcome {
        Ok(value) => ("ok", Ok(value)),
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
                GOOGLE_REFUSED,
            ));
        }
    };
    let base = state.calendar_api_base.as_deref().unwrap_or(API_BASE);
    let api = CalendarApi::new(base).map_err(|_| Refusal::internal("failed:client"))?;
    Ok((token, api))
}

fn json_body(body: &Bytes) -> Result<Value, Refusal> {
    serde_json::from_slice(body).map_err(|_| Refusal::body(write::NOT_AN_OBJECT))
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
    let missing = || Refusal::new("failed:target", StatusCode::NOT_FOUND, "not found");
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
            CANNOT_INVITE,
        ));
    }
    write::check_not_owner(guests, &[account_email, &calendar.calendar_id]).map_err(Refusal::body)
}

fn guests_read_only() -> Refusal {
    Refusal::new(
        "rejected:guests_read_only",
        StatusCode::BAD_REQUEST,
        GUESTS_READ_ONLY,
    )
}

/// Google said yes: store its answer under the calendar, kick the sync, and
/// serve the event (and hand back the row that was stored). An answer that cannot be read is stored nowhere (the
/// kicked sync brings the event in) and is a 502 with its own message, since
/// the change DID happen at Google.
async fn keep_answer(
    state: &ApiState,
    calendar: &StoredCalendar,
    answer: &Value,
    account_email: &str,
) -> Result<(EventRow, Value), Refusal> {
    let Some(row) = read_written_event(answer, calendar.time_zone.as_deref()) else {
        state.calendar_refresh.kick_now(state.account_id);
        return Err(Refusal::new(
            "failed:answer",
            StatusCode::BAD_GATEWAY,
            UNREADABLE_ANSWER,
        ));
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
        Refusal::internal("failed:store")
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
    let mut audit_target = None;
    let mut note = None;
    let outcome = async {
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
                    UNKNOWN_CALENDAR,
                )
            })?;
        audit_target = Some(target(&cal.calendar_id, None));
        if !cal.writable() {
            return Err(Refusal::new(
                "rejected:read_only",
                StatusCode::BAD_REQUEST,
                READ_ONLY_CALENDAR,
            ));
        }
        if !draft.guests.is_empty() {
            may_carry_guests(&cal, &draft.guests, creds.account_email())?;
        }
        let google_body = write::insert_body(&draft).map_err(Refusal::body)?;
        let (token, api) = google(&state, &creds, CREATE).await?;
        let answer = api
            .insert_event(&token, &cal.calendar_id, &google_body, draft.notify)
            .await
            .map_err(|e| Refusal::google(CREATE, e))?;
        let (row, view) = keep_answer(&state, &cal, &answer, creds.account_email()).await?;
        audit_target = Some(target(&cal.calendar_id, Some(&row.event_id)));
        Ok(view)
    }
    .await;
    let view = finish(&state, CREATE, audit_target, note, outcome).await?;
    Ok((StatusCode::CREATED, no_store(), Json(view)))
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
    let mut audit_target = None;
    let mut note = None;
    let outcome = async {
        let creds = granted(&state).await?;
        let edit = write::parse_edit(&json_body(&body)?, &calendar_id).map_err(Refusal::body)?;
        let (cal, stored) = stored_event(&state, &calendar_id, &event_id).await?;
        audit_target = Some(target(&calendar_id, Some(&event_id)));
        note = Some(GuestNote::of(&stored.row, edit.notify));
        if !stored.editable(creds.account_email()) {
            return Err(Refusal::new(
                "rejected:not_editable",
                StatusCode::FORBIDDEN,
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
            .map_err(|e| Refusal::google(EDIT, e))?;
        let (row, view) = keep_answer(&state, &cal, &answer, creds.account_email()).await?;
        note = Some(GuestNote::of(&row, edit.notify));
        Ok(view)
    }
    .await;
    let view = finish(&state, EDIT, audit_target, note, outcome).await?;
    Ok((no_store(), Json(view)))
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
/// 204. An event Google says is already deleted (410) is deleted here too.
/// With `notify=true` Google mails the event's guests a cancellation;
/// absent or `false`, nobody is told.
pub async fn delete_event(
    State(state): State<ApiState>,
    Path((calendar_id, event_id)): Path<(String, String)>,
    Query(query): Query<Vec<(String, String)>>,
) -> Result<impl IntoResponse, ApiError> {
    let mut audit_target = None;
    let mut note = None;
    let outcome = async {
        let creds = granted(&state).await?;
        let notify = delete_notify(&query)?;
        let (_, stored) = stored_event(&state, &calendar_id, &event_id).await?;
        audit_target = Some(target(&calendar_id, Some(&event_id)));
        note = Some(GuestNote::of(&stored.row, notify));
        if !stored.editable(creds.account_email()) {
            return Err(Refusal::new(
                "rejected:not_editable",
                StatusCode::FORBIDDEN,
                NOT_EDITABLE,
            ));
        }
        let (token, api) = google(&state, &creds, DELETE).await?;
        match api
            .delete_event(&token, &calendar_id, &event_id, notify)
            .await
        {
            Ok(()) | Err(CallError::Gone) => {}
            Err(e) => return Err(Refusal::google(DELETE, e)),
        }
        let (c, e) = (calendar_id.clone(), event_id.clone());
        let removed = store_call(&state, move |store, account| {
            store.calendar_remove_event(account, &c, &e)
        })
        .await;
        state.calendar_refresh.kick_now(state.account_id);
        removed.map_err(|_| Refusal::internal("failed:store"))?;
        Ok(())
    }
    .await;
    finish(&state, DELETE, audit_target, note, outcome).await?;
    Ok((StatusCode::NO_CONTENT, no_store()))
}
