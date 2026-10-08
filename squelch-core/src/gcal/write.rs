//! The user's own writes to Google Calendar (create, edit), checked IN FULL
//! before anything is sent, and the bodies Google is sent.
//!
//! Pure: nothing here calls Google or the store. The human door parses a
//! request with [`parse_create`] or [`parse_edit`], checks the calendar and
//! the event against the store, and only then calls Google, so a bad request
//! never reaches Google and Google's refusal never changes a local row.
//!
//! TIMES. The wire uses the timeline's convention: RFC3339 with an offset for
//! a timed event, `YYYY-MM-DD` for an all-day one, and an all-day `end` that
//! is the INCLUSIVE last day (a one-day event ends the day it starts). Google
//! reads `end` as "The (exclusive) end time of the event", so an all-day end
//! goes out as the day AFTER the last day ([`google_time`]); its answer comes
//! back through [`super::read_event`], which subtracts the day again.
//!
//! TEXT GOES OUT AS TYPED. The user's own words reach Google whole: a title
//! is refused, never cut (trimmed, non-empty, at most [`TITLE_CAP`]
//! characters); a location and a description are trimmed and cut to 200 and
//! 2000 characters. All three lose control and invisible characters (the
//! sync's `plain`; a description keeps its line breaks) and NOTHING ELSE: no
//! query stripping, no meeting-link rewriting, no markup handling, so a Zoom
//! link with its passcode still works in the user's own event. The laundering
//! happens on the way IN: Google's answer goes through [`super::read_event`]
//! like any synced event, so the stored and served copy is cleaned exactly as
//! every other event's is, while Google holds the full text.
//!
//! GUESTS AND WHO IS TOLD. A create or an edit may carry `guests`, a list of
//! plain addresses ([`addr_spec`]: no display names, no quoting, nothing a
//! mail header could be smuggled in), at most [`GUEST_CAP`], no duplicates.
//! Whether the calendar may invite and whether the event's list may be
//! changed are the door's questions ([`super::SyncedEvent::guest_editable`]).
//! `notify` is read by [`notify`] alone: ONLY the JSON value `true` makes
//! Google mail the guests; absent or `false` is silence, and anything else
//! is refused rather than guessed at.

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use super::EventRow;
use super::client::Notify;
use crate::triage::ghost::{self, EventTime};
use crate::triage::order_link::plain;
use crate::triage::text::truncate_trimmed;

/// The most characters a title may have.
pub const TITLE_CAP: usize = 200;
/// The most characters of a location sent to Google.
pub const LOCATION_CAP: usize = 200;
const ID_CAP: usize = 1024;

/// Why a request is refused, as a fixed message that never echoes input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Invalid(pub &'static str);

pub const NOT_AN_OBJECT: Invalid = Invalid("the body must be a JSON object");
pub const BAD_CALENDAR: Invalid = Invalid("calendar_id must name a calendar");
pub const TITLE_EMPTY: Invalid = Invalid("title must not be empty");
pub const TITLE_LONG: Invalid = Invalid("title must be at most 200 characters");
pub const BAD_TIME: Invalid =
    Invalid("start and end must be RFC3339 times with an offset, or YYYY-MM-DD dates");
pub const MIXED: Invalid = Invalid(
    "start and end must both be YYYY-MM-DD dates for an all-day event, or both RFC3339 times for a timed one",
);
pub const END_BEFORE_START: Invalid = Invalid("end must be after start");
pub const BAD_ALL_DAY: Invalid = Invalid("all_day must be true or false");
pub const BAD_TEXT: Invalid = Invalid("location and description must be text or null");
pub const MISSING: Invalid = Invalid("calendar_id, title, start, end and all_day are required");
pub const NOTHING: Invalid = Invalid("nothing to change");
pub const NO_END: Invalid = Invalid("this event has no end: send end with start");
pub const MOVE: Invalid = Invalid("an event cannot be moved to another calendar");
pub const BAD_GUESTS: Invalid = Invalid("guests must be a list of email addresses");
pub const TOO_MANY_GUESTS: Invalid = Invalid("an event takes at most 50 guests");
pub const BAD_GUEST: Invalid = Invalid("each guest must be one plain email address");
pub const DUPLICATE_GUEST: Invalid = Invalid("a guest is listed twice");
pub const GUEST_IS_OWNER: Invalid = Invalid("the calendar's owner cannot be its own guest");
pub const BAD_NOTIFY: Invalid = Invalid("notify must be true or false");
pub const BAD_EVENT_ID: Invalid =
    Invalid("id must be 5 to 1024 characters, each a lowercase letter a to v or a digit");

/// The most guests one request may name.
pub const GUEST_CAP: usize = 50;

/// A field of an edit: left alone, cleared (`null`), or set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change<T> {
    Keep,
    Clear,
    Set(T),
}

/// A start and an end at one precision, the end after the start (an
/// all-day end, the inclusive last day, may equal it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventTimes {
    pub start: EventTime,
    pub end: EventTime,
}

/// A create, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEvent {
    pub calendar_id: String,
    pub title: String,
    pub times: EventTimes,
    pub location: Option<String>,
    pub description: Option<String>,
    /// Checked addresses, as typed. Empty: no guests.
    pub guests: Vec<String>,
    pub notify: Notify,
    /// The event id the client chose for it, or None to let Google choose.
    /// THE CREATE'S IDEMPOTENCY KEY: the app picks one per pane and, after an
    /// uncertain answer, resends only the same body under it, so a create
    /// sent again finds the event the first one made ([`event_id`]). An event
    /// under the id is that answer only when it matches ([`is_same_create`]).
    pub id: Option<String>,
}

/// An edit, its fields checked one by one. The times are checked against the
/// stored event by [`EventEdit::times`], since an edit may send one of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventEdit {
    pub title: Option<String>,
    pub start: Option<EventTime>,
    pub end: Option<EventTime>,
    pub all_day: Option<bool>,
    pub location: Change<String>,
    pub description: Change<String>,
    /// None leaves the guest list alone; Some is the FULL new list (empty
    /// removes every guest).
    pub guests: Option<Vec<String>>,
    pub notify: Notify,
}

/// Check a pair of times: one precision (dates when `all_day`, instants
/// otherwise), and the end after the start. An all-day end is the inclusive
/// last day, so it may be the start itself.
pub fn check_times(start: EventTime, end: EventTime, all_day: bool) -> Result<EventTimes, Invalid> {
    match (all_day, start, end) {
        (true, EventTime::Date(s), EventTime::Date(e)) if e < s => Err(END_BEFORE_START),
        (true, EventTime::Date(_), EventTime::Date(_)) => Ok(EventTimes { start, end }),
        (false, EventTime::At(s), EventTime::At(e)) if e <= s => Err(END_BEFORE_START),
        (false, EventTime::At(_), EventTime::At(_)) => Ok(EventTimes { start, end }),
        _ => Err(MIXED),
    }
}

fn object(body: &Value) -> Result<&Map<String, Value>, Invalid> {
    body.as_object().ok_or(NOT_AN_OBJECT)
}

fn title(value: &Value) -> Result<String, Invalid> {
    let raw = value.as_str().ok_or(TITLE_EMPTY)?.trim();
    if raw.chars().count() > TITLE_CAP {
        return Err(TITLE_LONG);
    }
    // `clean_text` is `plain`, trimmed and capped: no other rewriting.
    ghost::clean_text(raw).ok_or(TITLE_EMPTY)
}

fn time(value: &Value) -> Result<EventTime, Invalid> {
    value.as_str().and_then(EventTime::parse).ok_or(BAD_TIME)
}

fn all_day(value: &Value) -> Result<bool, Invalid> {
    value.as_bool().ok_or(BAD_ALL_DAY)
}

/// A location as sent to Google: control and invisible characters out,
/// trimmed, at most [`LOCATION_CAP`]. Its URLs are left whole (see the module
/// doc: the outgoing text is the user's own).
fn outgoing_location(raw: &str) -> Option<String> {
    let text = truncate_trimmed(&plain(raw), LOCATION_CAP);
    (!text.is_empty()).then_some(text)
}

/// A description as sent to Google: [`outgoing_location`]'s treatment line
/// by line, so its line breaks survive, at most [`ghost::DESCRIPTION_CAP`].
fn outgoing_description(raw: &str) -> Option<String> {
    let lines: Vec<String> = raw.lines().map(plain).collect();
    let text = truncate_trimmed(&lines.join("\n"), ghost::DESCRIPTION_CAP);
    (!text.is_empty()).then_some(text)
}

/// A location or description: `null` clears, text is made sendable (and
/// clears when nothing is left of it).
fn text(
    value: Option<&Value>,
    launder: fn(&str) -> Option<String>,
) -> Result<Change<String>, Invalid> {
    match value {
        None => Ok(Change::Keep),
        Some(Value::Null) => Ok(Change::Clear),
        Some(Value::String(raw)) => Ok(launder(raw).map_or(Change::Clear, Change::Set)),
        Some(_) => Err(BAD_TEXT),
    }
}

/// One guest address: a plain `local@domain` and nothing else. ASCII only;
/// the local part is dot-separated runs of RFC 5322 atom characters (at most
/// 64), the domain two or more dot-separated labels of letters, digits and
/// inner hyphens (at most 253). No display name, angle brackets, quoted
/// local part, comment, address literal, whitespace or control character,
/// so the value can only ever be read as one mailbox. Returned trimmed, as
/// typed.
pub fn addr_spec(raw: &str) -> Option<String> {
    let addr = raw.trim();
    let (local, domain) = addr.split_once('@')?;
    let atom = |c: char| c.is_ascii_alphanumeric() || "!#$%&'*+/=?^_`{|}~-".contains(c);
    let local_ok = !local.is_empty()
        && local.len() <= 64
        && local
            .split('.')
            .all(|run| !run.is_empty() && run.chars().all(atom));
    let label = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    let domain_ok = domain.len() <= 253 && domain.contains('.') && domain.split('.').all(label);
    (local_ok && domain_ok).then(|| addr.to_string())
}

/// A guest list as sent: an array of at most [`GUEST_CAP`] strings, each an
/// [`addr_spec`], no two the same once case is folded.
fn guests(value: &Value) -> Result<Vec<String>, Invalid> {
    let list = value.as_array().ok_or(BAD_GUESTS)?;
    if list.len() > GUEST_CAP {
        return Err(TOO_MANY_GUESTS);
    }
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(list.len());
    for entry in list {
        let addr = entry.as_str().and_then(addr_spec).ok_or(BAD_GUEST)?;
        if !seen.insert(addr.to_ascii_lowercase()) {
            return Err(DUPLICATE_GUEST);
        }
        out.push(addr);
    }
    Ok(out)
}

/// THE NOTIFY DEFAULT. Whether Google mails the guests: ONLY the JSON value
/// `true`. Absent is silence, `false` is silence, and anything else (`null`,
/// `"true"`, `1`) is refused, never read as a yes.
pub fn notify(value: Option<&Value>) -> Result<Notify, Invalid> {
    match value {
        None | Some(Value::Bool(false)) => Ok(Notify::Nobody),
        Some(Value::Bool(true)) => Ok(Notify::Guests),
        Some(_) => Err(BAD_NOTIFY),
    }
}

/// A request body as JSON, refusing one that names `notify` more than once
/// at its top level. `serde_json` keeps the last of duplicate keys, so
/// `{"notify": false, "notify": true}` would quietly read as a yes; like a
/// delete's query ([`notify_query`]'s caller), two of them is refused rather
/// than resolved. Not JSON at all is [`NOT_AN_OBJECT`].
pub fn body(raw: &[u8]) -> Result<Value, Invalid> {
    let value: Value = serde_json::from_slice(raw).map_err(|_| NOT_AN_OBJECT)?;
    if value.is_object() && notify_keys(raw).is_none_or(|count| count > 1) {
        return Err(BAD_NOTIFY);
    }
    Ok(value)
}

/// How many top-level keys of a JSON object are `notify` (after unescaping,
/// so `"notif\u0079"` counts). None when it cannot be read as an object.
fn notify_keys(raw: &[u8]) -> Option<usize> {
    use serde::de::{Deserializer, IgnoredAny, MapAccess, Visitor};
    struct Count;
    impl<'de> Visitor<'de> for Count {
        type Value = usize;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a JSON object")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<usize, A::Error> {
            let mut count = 0;
            while let Some(key) = map.next_key::<String>()? {
                map.next_value::<IgnoredAny>()?;
                count += usize::from(key == "notify");
            }
            Ok(count)
        }
    }
    serde_json::Deserializer::from_slice(raw)
        .deserialize_map(Count)
        .ok()
}

/// [`notify`] for a delete, whose `notify` is a query parameter: exactly
/// `true` tells the guests, absent or exactly `false` tells nobody.
pub fn notify_query(value: Option<&str>) -> Result<Notify, Invalid> {
    match value {
        None | Some("false") => Ok(Notify::Nobody),
        Some("true") => Ok(Notify::Guests),
        Some(_) => Err(BAD_NOTIFY),
    }
}

/// No guest may be one of `owners` (the mailbox's address, the calendar's
/// own id): the organizer is not their own guest, and Google would fold the
/// two entries into one whose answer the app then overwrote.
pub fn check_not_owner(guests: &[String], owners: &[&str]) -> Result<(), Invalid> {
    let owned = guests.iter().any(|guest| {
        owners
            .iter()
            .any(|owner| guest.eq_ignore_ascii_case(owner.trim()))
    });
    if owned { Err(GUEST_IS_OWNER) } else { Ok(()) }
}

/// THE FULL ATTENDEE LIST a guest edit sends, since `events.patch` replaces
/// the array ("Array fields, if specified, overwrite the existing arrays;
/// this discards any previous array elements").
///
/// `current` is Google's own list, read a moment ago, each entry whole.
/// Kept, EXACTLY as Google gave it (its `responseStatus`, comment, optional
/// flag and the rest): the user's own entry and the organizer's, which this
/// door never removes, and every guest still in `wanted`. Dropped: every
/// other entry. Added, bare (`{"email"}`, so Google starts it at
/// `needsAction`): each address in `wanted` that `current` does not hold.
/// Addresses compare with case folded.
///
/// None when `current` holds an entry that is neither the user's nor the
/// organizer's and has no address (Google's `email` is there "if
/// available"): it could not be asked for by address, so it would be
/// removed without the user ever having seen it. The edit is refused.
pub fn merge_attendees(current: &[Value], wanted: &[String]) -> Option<Vec<Value>> {
    let fold = |addr: &str| addr.trim().to_ascii_lowercase();
    let wanted_set: HashSet<String> = wanted.iter().map(|w| fold(w)).collect();
    let flag = |entry: &Value, key| entry.get(key).and_then(Value::as_bool) == Some(true);
    let mut held = HashSet::new();
    let mut out = Vec::new();
    for entry in current {
        let email = entry
            .get("email")
            .and_then(Value::as_str)
            .map(fold)
            .filter(|e| !e.is_empty());
        let own = flag(entry, "self") || flag(entry, "organizer");
        if !own && email.is_none() {
            return None;
        }
        let asked = email.as_ref().is_some_and(|e| wanted_set.contains(e));
        if own || asked {
            if let Some(email) = email {
                held.insert(email);
            }
            out.push(entry.clone());
        }
    }
    for addr in wanted {
        if held.insert(fold(addr)) {
            out.push(json!({ "email": addr }));
        }
    }
    Some(out)
}

/// A calendar id as sent: non-empty text of a bounded length. Whether it
/// names a calendar the user may write to is the store's question.
pub fn calendar_id(value: &Value) -> Result<String, Invalid> {
    let id = value.as_str().map(str::trim).ok_or(BAD_CALENDAR)?;
    (!id.is_empty() && id.chars().count() <= ID_CAP)
        .then(|| id.to_string())
        .ok_or(BAD_CALENDAR)
}

/// A client-chosen event id, as Google takes one: "characters allowed in
/// the ID are those used in base32hex encoding, i.e. lowercase letters a-v
/// and digits 0-9", and "the length of the ID must be between 5 and 1024
/// characters". Anything else is refused here rather than by Google.
pub fn event_id(value: &Value) -> Result<String, Invalid> {
    let id = value.as_str().ok_or(BAD_EVENT_ID)?;
    let ok = (5..=ID_CAP).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='v').contains(&c));
    ok.then(|| id.to_string()).ok_or(BAD_EVENT_ID)
}

/// Read a create: `{"calendar_id", "title", "start", "end", "all_day",
/// "location"?, "description"?, "guests"?, "notify"?, "id"?}`. Unknown keys
/// are ignored.
pub fn parse_create(body: &Value) -> Result<NewEvent, Invalid> {
    let body = object(body)?;
    let need = |key| body.get(key).filter(|v| !v.is_null()).ok_or(MISSING);
    let calendar = calendar_id(need("calendar_id")?)?;
    let title = title(need("title")?)?;
    let all_day = all_day(need("all_day")?)?;
    let times = check_times(time(need("start")?)?, time(need("end")?)?, all_day)?;
    let set = |change| match change {
        Change::Set(text) => Some(text),
        Change::Keep | Change::Clear => None,
    };
    Ok(NewEvent {
        calendar_id: calendar,
        title,
        times,
        location: set(text(body.get("location"), outgoing_location)?),
        description: set(text(body.get("description"), outgoing_description)?),
        guests: match body.get("guests") {
            None | Some(Value::Null) => Vec::new(),
            Some(list) => guests(list)?,
        },
        notify: notify(body.get("notify"))?,
        id: match body.get("id") {
            None | Some(Value::Null) => None,
            Some(id) => Some(event_id(id)?),
        },
    })
}

/// Read an edit: any subset of the create's fields. A `calendar_id`, if sent,
/// must be the event's own (moving an event is not offered); `location` and
/// `description` take `null` to clear; `title`, `start`, `end` and `all_day`
/// cannot be cleared. `guests`, when present, is the full new list (`[]`
/// removes them all; `null` is refused).
pub fn parse_edit(body: &Value, event_calendar: &str) -> Result<EventEdit, Invalid> {
    let body = object(body)?;
    if let Some(cal) = body.get("calendar_id")
        && calendar_id(cal)? != event_calendar
    {
        return Err(MOVE);
    }
    let edit = EventEdit {
        title: body.get("title").map(title).transpose()?,
        start: body.get("start").map(time).transpose()?,
        end: body.get("end").map(time).transpose()?,
        all_day: body.get("all_day").map(all_day).transpose()?,
        location: text(body.get("location"), outgoing_location)?,
        description: text(body.get("description"), outgoing_description)?,
        guests: body.get("guests").map(guests).transpose()?,
        notify: notify(body.get("notify"))?,
    };
    // `notify` alone changes nothing: there is no "tell them again".
    let empty = edit.title.is_none()
        && edit.start.is_none()
        && edit.end.is_none()
        && edit.all_day.is_none()
        && edit.location == Change::Keep
        && edit.description == Change::Keep
        && edit.guests.is_none();
    if empty { Err(NOTHING) } else { Ok(edit) }
}

impl EventEdit {
    /// The times after this edit, checked, or None when it changes none of
    /// them. What it does not send is the stored event's: its start, its
    /// all-day flag, and its end (an all-day event stored with no end is a
    /// one-day event, whose last day is its start).
    pub fn times(&self, stored: &EventRow) -> Result<Option<EventTimes>, Invalid> {
        if self.start.is_none() && self.end.is_none() && self.all_day.is_none() {
            return Ok(None);
        }
        let all_day = self.all_day.unwrap_or_else(|| stored.all_day());
        let start = self.start.unwrap_or(stored.start);
        let end = match (self.end, stored.end) {
            (Some(end), _) | (None, Some(end)) => end,
            (None, None) if stored.all_day() => stored.start,
            (None, None) => return Err(NO_END),
        };
        check_times(start, end, all_day).map(Some)
    }
}

/// One start or end as Google takes it. An all-day END goes out as the day
/// after the inclusive last day, Google's exclusive end. In an edit, the
/// other form is set to `null` ("To delete a field, specify the field and set
/// it to `null`"), so an event switched between all-day and timed does not
/// keep both.
fn google_time(time: &EventTime, is_end: bool, edit: bool) -> Result<Value, Invalid> {
    let (key, value, other) = match time {
        EventTime::At(at) => ("dateTime", at.to_rfc3339(), "date"),
        EventTime::Date(day) => {
            let day = if is_end {
                day.succ_opt().ok_or(BAD_TIME)?
            } else {
                *day
            };
            ("date", day.format("%Y-%m-%d").to_string(), "dateTime")
        }
    };
    let mut out = Map::new();
    out.insert(key.into(), Value::String(value));
    if edit {
        out.insert(other.into(), Value::Null);
    }
    Ok(Value::Object(out))
}

/// The `events.insert` body. Its text is the user's as typed, less control
/// and invisible characters; the stored copy is laundered on the way back in
/// (the module doc's one deliberate asymmetry).
pub fn insert_body(event: &NewEvent) -> Result<Value, Invalid> {
    let mut body = json!({
        "summary": event.title,
        "start": google_time(&event.times.start, false, false)?,
        "end": google_time(&event.times.end, true, false)?,
    });
    if let Some(location) = &event.location {
        body["location"] = json!(location);
    }
    if let Some(description) = &event.description {
        body["description"] = json!(description);
    }
    if let Some(id) = &event.id {
        body["id"] = json!(id);
    }
    if !event.guests.is_empty() {
        let attendees: Vec<Value> = event
            .guests
            .iter()
            .map(|addr| json!({ "email": addr }))
            .collect();
        body["attendees"] = Value::Array(attendees);
    }
    Ok(body)
}

/// Whether `found`, the event Google already holds under a create's own id,
/// is the event THIS create would make: the same title (both laundered the
/// way every stored event is, by reading the create's own insert body back
/// through [`super::read_written_event`]), start, end, all-day flag, and the
/// same guests (case folded, the calendar's own entry and the organizer's
/// left out). Only then may a create naming an id be answered with an event
/// already there; anything else under that id is a different event, and the
/// create is refused rather than answered with it. A guest list Google cut
/// short cannot be shown to match, so it does not. The calendar is the
/// caller's: `found` was read from the calendar the create names.
pub fn is_same_create(draft: &NewEvent, found: &EventRow, calendar_zone: Option<&str>) -> bool {
    let Ok(mut body) = insert_body(draft) else {
        return false;
    };
    body["id"] = json!(found.event_id);
    let Some(want) = super::read_written_event(&body, calendar_zone) else {
        return false;
    };
    let fold = |addr: &str| addr.trim().to_ascii_lowercase();
    let wanted: HashSet<String> = draft.guests.iter().map(|g| fold(g)).collect();
    let held: HashSet<String> = found
        .guests
        .iter()
        .filter(|g| !g.is_self && !g.organizer)
        .map(|g| fold(&g.email))
        .collect();
    !found.guests_truncated
        && want.summary == found.summary
        && want.start == found.start
        && want.end == found.end
        && want.all_day() == found.all_day()
        && wanted == held
}

/// The `events.patch` body: only what the edit changes, its text as typed
/// (see [`insert_body`]). Times, when any
/// changes, go as a pair (`times`, from [`EventEdit::times`]), since a new
/// start can only be checked against an end. `attendees` is the full list
/// from [`merge_attendees`] when the edit changes the guests, and None
/// otherwise: the key is then NOT SENT, which leaves Google's list alone.
pub fn patch_body(
    edit: &EventEdit,
    times: Option<&EventTimes>,
    attendees: Option<Vec<Value>>,
) -> Result<Value, Invalid> {
    let mut body = Map::new();
    if let Some(attendees) = attendees {
        body.insert("attendees".into(), Value::Array(attendees));
    }
    if let Some(title) = &edit.title {
        body.insert("summary".into(), json!(title));
    }
    if let Some(times) = times {
        body.insert("start".into(), google_time(&times.start, false, true)?);
        body.insert("end".into(), google_time(&times.end, true, true)?);
    }
    for (key, change) in [
        ("location", &edit.location),
        ("description", &edit.description),
    ] {
        match change {
            Change::Keep => {}
            Change::Clear => {
                body.insert(key.into(), Value::Null);
            }
            Change::Set(text) => {
                body.insert(key.into(), json!(text));
            }
        }
    }
    Ok(Value::Object(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(raw: &str) -> EventTime {
        EventTime::parse(raw).unwrap()
    }

    fn create(edit: impl FnOnce(&mut Value)) -> Result<NewEvent, Invalid> {
        let mut body = json!({
            "calendar_id": "primary", "title": "  Dinner  ",
            "start": "2026-10-05T19:00:00-04:00", "end": "2026-10-05T21:00:00-04:00",
            "all_day": false,
        });
        edit(&mut body);
        parse_create(&body)
    }

    fn stored(start: &str, end: Option<&str>) -> EventRow {
        EventRow {
            event_id: "e".into(),
            ical_uid: None,
            tentative: false,
            summary: Some("x".into()),
            location: None,
            start: at(start),
            end: end.map(at),
            start_timezone: None,
            organizer_email: None,
            organizer_unreadable: false,
            organizer_self: true,
            self_response: None,
            transparent: false,
            html_link: None,
            updated_at: None,
            color_id: None,
            description: None,
            guests: Vec::new(),
            guests_truncated: false,
        }
    }

    #[test]
    fn a_create_goes_out_as_typed_less_invisible_characters() {
        let event = create(|b| {
            b["location"] = json!(" Zoom:\u{200B} https://zoom.us/j/1?pwd=secret ");
            b["description"] =
                json!("Line one\n\n\n<b>Line</b> two\u{202E}\nhttps://meet.example/x?pwd=1");
        })
        .unwrap();
        assert_eq!(event.title, "Dinner");
        // The passcode, the markup and the blank lines are the user's: kept.
        assert_eq!(
            event.location.as_deref(),
            Some("Zoom: https://zoom.us/j/1?pwd=secret")
        );
        assert_eq!(
            event.description.as_deref(),
            Some("Line one\n\n\n<b>Line</b> two\nhttps://meet.example/x?pwd=1")
        );
        let body = insert_body(&event).unwrap();
        assert_eq!(
            body,
            json!({
                "summary": "Dinner",
                "start": {"dateTime": "2026-10-05T19:00:00-04:00"},
                "end": {"dateTime": "2026-10-05T21:00:00-04:00"},
                "location": "Zoom: https://zoom.us/j/1?pwd=secret",
                "description": "Line one\n\n\n<b>Line</b> two\nhttps://meet.example/x?pwd=1",
            })
        );
        // Cut at the caps, never refused.
        let long = create(|b| {
            b["location"] = json!("l".repeat(300));
            b["description"] = json!("d".repeat(3000));
        })
        .unwrap();
        assert_eq!(long.location.map(|l| l.chars().count()), Some(LOCATION_CAP));
        assert_eq!(
            long.description.map(|d| d.chars().count()),
            Some(ghost::DESCRIPTION_CAP)
        );
        // Nothing left but invisible characters: no location.
        let blank = create(|b| b["location"] = json!("\u{200B} \u{202E}")).unwrap();
        assert_eq!(blank.location, None);
    }

    #[test]
    fn an_all_day_end_goes_out_as_googles_exclusive_day() {
        let one = create(|b| {
            b["all_day"] = json!(true);
            b["start"] = json!("2026-10-05");
            b["end"] = json!("2026-10-05");
        })
        .unwrap();
        let body = insert_body(&one).unwrap();
        assert_eq!(body["start"], json!({"date": "2026-10-05"}));
        assert_eq!(body["end"], json!({"date": "2026-10-06"}), "one day");
        let three = create(|b| {
            b["all_day"] = json!(true);
            b["start"] = json!("2026-10-30");
            b["end"] = json!("2026-11-01");
        })
        .unwrap();
        assert_eq!(
            insert_body(&three).unwrap()["end"],
            json!({"date": "2026-11-02"}),
            "the 30th through the 1st, across a month"
        );
    }

    #[test]
    fn every_bad_create_is_refused_with_a_fixed_message() {
        type Case = (Invalid, Box<dyn FnOnce(&mut Value)>);
        let cases: Vec<Case> = vec![
            (TITLE_EMPTY, Box::new(|b| b["title"] = json!("   "))),
            (
                TITLE_EMPTY,
                Box::new(|b| b["title"] = json!("\u{200B}\u{202E}")),
            ),
            (TITLE_EMPTY, Box::new(|b| b["title"] = json!(7))),
            (
                TITLE_LONG,
                Box::new(|b| b["title"] = json!("a".repeat(201))),
            ),
            (MISSING, Box::new(|b| b["title"] = Value::Null)),
            (
                MISSING,
                Box::new(|b| {
                    b.as_object_mut().unwrap().remove("end");
                }),
            ),
            (
                MISSING,
                Box::new(|b| {
                    b.as_object_mut().unwrap().remove("all_day");
                }),
            ),
            (BAD_ALL_DAY, Box::new(|b| b["all_day"] = json!("yes"))),
            (BAD_CALENDAR, Box::new(|b| b["calendar_id"] = json!(""))),
            (
                BAD_CALENDAR,
                Box::new(|b| b["calendar_id"] = json!("x".repeat(1025))),
            ),
            (BAD_TIME, Box::new(|b| b["start"] = json!("tomorrow"))),
            (
                BAD_TIME,
                Box::new(|b| b["start"] = json!("2026-10-05T19:00:00")),
            ),
            (
                END_BEFORE_START,
                Box::new(|b| b["end"] = json!("2026-10-05T18:00:00-04:00")),
            ),
            (
                END_BEFORE_START,
                Box::new(|b| b["end"] = json!("2026-10-05T23:00:00Z")),
            ),
            (MIXED, Box::new(|b| b["end"] = json!("2026-10-06"))),
            (MIXED, Box::new(|b| b["all_day"] = json!(true))),
            (BAD_TEXT, Box::new(|b| b["location"] = json!(["x"]))),
        ];
        for (want, edit) in cases {
            assert_eq!(create(edit), Err(want));
        }
        assert_eq!(parse_create(&json!([1])), Err(NOT_AN_OBJECT));
        // Exactly 200 characters is a title.
        assert!(create(|b| b["title"] = json!("a".repeat(200))).is_ok());
        // An all-day end before its start, and on it (one day).
        let all_day = |start: &str, end: &str| {
            create(|b| {
                b["all_day"] = json!(true);
                b["start"] = json!(start);
                b["end"] = json!(end);
            })
        };
        assert_eq!(all_day("2026-10-05", "2026-10-04"), Err(END_BEFORE_START));
        assert!(all_day("2026-10-05", "2026-10-05").is_ok());
    }

    #[test]
    fn an_edit_tells_absent_from_null() {
        let edit = parse_edit(&json!({"location": null, "title": "New"}), "primary").unwrap();
        assert_eq!(edit.location, Change::Clear);
        assert_eq!(edit.description, Change::Keep);
        assert_eq!(
            patch_body(&edit, None, None).unwrap(),
            json!({"summary": "New", "location": null})
        );
        assert_eq!(parse_edit(&json!({}), "primary"), Err(NOTHING));
        assert_eq!(
            parse_edit(&json!({"calendar_id": "primary"}), "primary"),
            Err(NOTHING)
        );
        assert_eq!(
            parse_edit(&json!({"calendar_id": "other", "title": "x"}), "primary"),
            Err(MOVE)
        );
        assert_eq!(
            parse_edit(&json!({"title": null}), "primary"),
            Err(TITLE_EMPTY)
        );
        assert_eq!(
            parse_edit(&json!({"start": null}), "primary"),
            Err(BAD_TIME)
        );
    }

    #[test]
    fn an_edits_times_are_checked_against_the_stored_event() {
        let timed = stored(
            "2026-10-05T19:00:00-04:00",
            Some("2026-10-05T20:00:00-04:00"),
        );
        let edit = |body: Value| parse_edit(&body, "primary").unwrap();
        // A new start alone keeps the stored end, and must stay before it.
        let moved = edit(json!({"start": "2026-10-05T19:30:00-04:00"}));
        assert!(moved.times(&timed).unwrap().is_some());
        let past_end = edit(json!({"start": "2026-10-05T20:30:00-04:00"}));
        assert_eq!(past_end.times(&timed), Err(END_BEFORE_START));
        // To all-day without dates is mixed precision.
        assert_eq!(edit(json!({"all_day": true})).times(&timed), Err(MIXED));
        // To all-day with dates: the patch nulls the clock form.
        let to_day = edit(json!({"all_day": true, "start": "2026-10-05", "end": "2026-10-06"}));
        let times = to_day.times(&timed).unwrap();
        assert_eq!(
            patch_body(&to_day, times.as_ref(), None).unwrap(),
            json!({
                "start": {"date": "2026-10-05", "dateTime": null},
                "end": {"date": "2026-10-07", "dateTime": null},
            })
        );
        // A one-day all-day event (no stored end) moved by its start alone:
        // its end is its start, so a later start is refused.
        let one_day = stored("2026-10-05", None);
        assert_eq!(
            edit(json!({"start": "2026-10-06"})).times(&one_day),
            Err(END_BEFORE_START)
        );
        assert!(
            edit(json!({"end": "2026-10-07"}))
                .times(&one_day)
                .unwrap()
                .is_some()
        );
        // A timed event with no end must be sent one.
        let endless = stored("2026-10-05T19:00:00-04:00", None);
        assert_eq!(
            edit(json!({"start": "2026-10-05T18:00:00-04:00"})).times(&endless),
            Err(NO_END)
        );
        // No time field: no times.
        assert_eq!(edit(json!({"title": "x"})).times(&timed), Ok(None));
    }

    #[test]
    fn a_guest_is_one_plain_address_and_nothing_else() {
        for good in [
            "sam@example.com",
            "  Sam.Lee+cal@Mail.Example.co.uk ",
            "o'brien@example.com",
            "a_b-c@x-y.io",
        ] {
            assert_eq!(addr_spec(good).as_deref(), Some(good.trim()), "{good}");
        }
        for bad in [
            "",
            "sam",
            "sam@",
            "@example.com",
            "sam@localhost",
            "sam@example",
            "Sam <sam@example.com>",
            "<sam@example.com>",
            "sam@example.com, pat@example.com",
            "sam@example.com;pat@example.com",
            "sam @example.com",
            "sam@exam ple.com",
            "sam@@example.com",
            "sam@a@example.com",
            ".sam@example.com",
            "sam.@example.com",
            "sa..m@example.com",
            "\"sam\"@example.com",
            "sam(comment)@example.com",
            "sam@[127.0.0.1]",
            "sam@-example.com",
            "sam@example-.com",
            "sam@example..com",
            "sam@example.com.",
            "sam@exa_mple.com",
            "sam@example.com\r\nBcc: x@y.com",
            "sam\u{200B}@example.com",
            "s\u{00E4}m@example.com",
            "sam@ex\u{00E4}mple.com",
            "mailto:sam@example.com",
        ] {
            assert_eq!(addr_spec(bad), None, "{bad:?}");
        }
        let long_local = format!("{}@example.com", "a".repeat(65));
        assert_eq!(addr_spec(&long_local), None);
        let long_domain = format!("a@{}.com", "b.".repeat(130));
        assert_eq!(addr_spec(&long_domain), None);
    }

    #[test]
    fn a_guest_list_is_bounded_and_has_no_duplicates() {
        let with = |guests: Value| create(|b| b["guests"] = guests);
        assert_eq!(
            with(json!(["a@x.com", " B@x.com "])).unwrap().guests,
            ["a@x.com", "B@x.com"],
            "as typed, trimmed"
        );
        assert!(create(|_| {}).unwrap().guests.is_empty(), "absent");
        assert!(with(Value::Null).unwrap().guests.is_empty());
        assert!(with(json!([])).unwrap().guests.is_empty());
        let many = |n: usize| json!((0..n).map(|i| format!("g{i}@x.com")).collect::<Vec<_>>());
        assert_eq!(with(many(50)).unwrap().guests.len(), 50);
        assert_eq!(with(many(51)), Err(TOO_MANY_GUESTS));
        assert_eq!(with(json!("a@x.com")), Err(BAD_GUESTS));
        assert_eq!(with(json!({"email": "a@x.com"})), Err(BAD_GUESTS));
        assert_eq!(with(json!(["a@x.com", 7])), Err(BAD_GUEST));
        assert_eq!(with(json!([{"email": "a@x.com"}])), Err(BAD_GUEST));
        assert_eq!(with(json!(["Sam <a@x.com>"])), Err(BAD_GUEST));
        assert_eq!(with(json!(["a@x.com", "A@X.COM"])), Err(DUPLICATE_GUEST));
        // An edit: absent leaves the list alone, a list replaces it, null is
        // not a list.
        let edit = |body: Value| parse_edit(&body, "primary");
        assert_eq!(edit(json!({"title": "x"})).unwrap().guests, None);
        assert_eq!(
            edit(json!({"guests": []})).unwrap().guests,
            Some(Vec::new())
        );
        assert_eq!(
            edit(json!({"guests": ["a@x.com"]})).unwrap().guests,
            Some(vec!["a@x.com".to_string()])
        );
        assert_eq!(edit(json!({"guests": null})), Err(BAD_GUESTS));
        assert_eq!(edit(json!({"guests": many(51)})), Err(TOO_MANY_GUESTS));
        assert_eq!(
            edit(json!({"guests": ["a@x.com", "a@x.com"]})),
            Err(DUPLICATE_GUEST)
        );
    }

    /// THE NOTIFY DEFAULT, at the parser: only `true` tells anyone.
    #[test]
    fn only_the_value_true_tells_the_guests() {
        assert_eq!(notify(None), Ok(Notify::Nobody));
        assert_eq!(notify(Some(&json!(false))), Ok(Notify::Nobody));
        assert_eq!(notify(Some(&json!(true))), Ok(Notify::Guests));
        for odd in [
            json!(null),
            json!("true"),
            json!(1),
            json!("all"),
            json!([true]),
        ] {
            assert_eq!(notify(Some(&odd)), Err(BAD_NOTIFY), "{odd}");
        }
        assert_eq!(notify_query(None), Ok(Notify::Nobody));
        assert_eq!(notify_query(Some("false")), Ok(Notify::Nobody));
        assert_eq!(notify_query(Some("true")), Ok(Notify::Guests));
        for odd in ["", "1", "TRUE", "True", "yes", "all", "true "] {
            assert_eq!(notify_query(Some(odd)), Err(BAD_NOTIFY), "{odd:?}");
        }
        // Through the parsers.
        assert_eq!(create(|_| {}).unwrap().notify, Notify::Nobody);
        assert_eq!(
            create(|b| b["notify"] = json!(false)).unwrap().notify,
            Notify::Nobody
        );
        assert_eq!(
            create(|b| b["notify"] = json!(true)).unwrap().notify,
            Notify::Guests
        );
        assert_eq!(create(|b| b["notify"] = json!("true")), Err(BAD_NOTIFY));
        let edit = |body: Value| parse_edit(&body, "primary");
        assert_eq!(edit(json!({"title": "x"})).unwrap().notify, Notify::Nobody);
        assert_eq!(
            edit(json!({"title": "x", "notify": true})).unwrap().notify,
            Notify::Guests
        );
        assert_eq!(edit(json!({"title": "x", "notify": 1})), Err(BAD_NOTIFY));
        // Telling the guests is not itself a change.
        assert_eq!(edit(json!({"notify": true})), Err(NOTHING));
    }

    /// A body naming `notify` twice is refused, whichever order, escaped or
    /// not, as the delete's query refuses two; once, or nested, is read.
    #[test]
    fn a_body_with_two_notify_keys_is_refused() {
        for raw in [
            r#"{"title": "x", "notify": false, "notify": true}"#,
            r#"{"notify": true, "title": "x", "notify": false}"#,
            r#"{"notify": true, "notif\u0079": true}"#,
        ] {
            assert_eq!(body(raw.as_bytes()), Err(BAD_NOTIFY), "{raw}");
        }
        for raw in [
            r#"{"title": "x", "notify": true}"#,
            r#"{"title": "x"}"#,
            r#"{"title": "x", "extra": {"notify": 1, "x": {"notify": 2}}}"#,
            r#"[1, 2]"#,
        ] {
            assert!(body(raw.as_bytes()).is_ok(), "{raw}");
        }
        assert_eq!(body(b"not json"), Err(NOT_AN_OBJECT));
    }

    #[test]
    fn the_owner_is_never_a_guest() {
        let guests = vec!["sam@x.com".to_string(), "ME@Example.com".to_string()];
        assert_eq!(
            check_not_owner(&guests, &["me@example.com"]),
            Err(GUEST_IS_OWNER)
        );
        assert_eq!(
            check_not_owner(&guests, &["other@example.com", " sam@X.com "]),
            Err(GUEST_IS_OWNER)
        );
        assert_eq!(check_not_owner(&guests, &["other@example.com"]), Ok(()));
        assert_eq!(check_not_owner(&[], &["me@example.com"]), Ok(()));
    }

    #[test]
    fn a_create_sends_its_guests_bare_and_none_when_it_has_none() {
        let event = create(|b| b["guests"] = json!(["sam@x.com", "Pat@x.com"])).unwrap();
        assert_eq!(
            insert_body(&event).unwrap()["attendees"],
            json!([{"email": "sam@x.com"}, {"email": "Pat@x.com"}])
        );
        let none = create(|b| b["guests"] = json!([])).unwrap();
        assert!(
            !insert_body(&none)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("attendees")
        );
    }

    /// THE RESPONSE-STATUS PRESERVATION: a kept guest goes back exactly as
    /// Google gave it.
    #[test]
    fn a_guest_edit_keeps_what_google_holds_about_everyone_who_stays() {
        let current = vec![
            json!({"email": "me@example.com", "self": true, "organizer": true,
                   "responseStatus": "accepted"}),
            json!({"email": "Sam@x.com", "displayName": "Sam", "responseStatus": "accepted",
                   "comment": "See you there", "additionalGuests": 2, "optional": true}),
            json!({"email": "pat@x.com", "responseStatus": "declined"}),
            json!({"email": "room@resource.calendar.google.com", "resource": true,
                   "responseStatus": "accepted"}),
        ];
        let wanted = vec!["sam@X.com".to_string(), "new@x.com".to_string()];
        let merge =
            |current: &[Value], wanted: &[String]| merge_attendees(current, wanted).unwrap();
        assert_eq!(
            merge(&current, &wanted),
            vec![
                current[0].clone(),
                current[1].clone(),
                json!({"email": "new@x.com"}),
            ],
            "own entry kept unasked, Sam whole, Pat and the room dropped, the new one bare"
        );
        // Removing everyone leaves the user's own entry.
        assert_eq!(merge(&current, &[]), vec![current[0].clone()]);
        // An organizer entry that is not `self` is kept too.
        let organized = vec![json!({"email": "boss@x.com", "organizer": true})];
        assert_eq!(merge(&organized, &[]), organized);
        // An address already held as the user's own entry is not added twice.
        assert_eq!(
            merge(&current[..1], &["ME@example.com".to_string()]),
            vec![current[0].clone()]
        );
        // From nothing: all bare, in the order asked.
        assert_eq!(
            merge(&[], &wanted),
            vec![json!({"email": "sam@X.com"}), json!({"email": "new@x.com"})]
        );
        // A guest with no address cannot be kept by address: no list at all.
        let nameless = vec![
            current[0].clone(),
            json!({"displayName": "Someone", "responseStatus": "accepted"}),
        ];
        assert_eq!(merge_attendees(&nameless, &wanted), None);
        assert_eq!(
            merge_attendees(&[json!({"email": "", "responseStatus": "accepted"})], &[]),
            None
        );
        // The user's own entry needs none.
        assert!(merge_attendees(&[json!({"self": true})], &[]).is_some());
        // In the patch: present when given, the key absent otherwise.
        let edit = parse_edit(&json!({"guests": ["a@x.com"]}), "primary").unwrap();
        assert_eq!(
            patch_body(&edit, None, Some(vec![json!({"email": "a@x.com"})])).unwrap(),
            json!({"attendees": [{"email": "a@x.com"}]})
        );
        let title = parse_edit(&json!({"title": "x"}), "primary").unwrap();
        assert_eq!(
            patch_body(&title, None, None).unwrap(),
            json!({"summary": "x"})
        );
        // Emptying the list sends an empty array, which is not "absent".
        assert_eq!(
            patch_body(&edit, None, Some(Vec::new())).unwrap(),
            json!({"attendees": []})
        );
    }

    /// A CREATE NAMING AN ID IS ANSWERED ONLY BY ITS OWN EVENT (verify3 W1,
    /// W2): the event Google holds under the id must be the one this create
    /// would make, title as laundered, times, all-day and guests.
    #[test]
    fn an_event_under_a_creates_id_is_its_own_only_when_it_matches() {
        let draft = create(|b| {
            b["title"] = json!(" Dinner\u{200B} with  Sam ");
            b["guests"] = json!(["Sam@Example.com"]);
            b["notify"] = json!(true);
            b["id"] = json!("abcdef0123");
        })
        .unwrap();
        // As Google answers it: the times in another offset, the organizer
        // and the calendar's own entry among the attendees, the address
        // folded.
        let google = |edit: &dyn Fn(&mut Value)| {
            let mut event = json!({
                "id": "abcdef0123", "status": "confirmed",
                // What Google stores is what was sent.
                "summary": draft.title,
                "start": {"dateTime": "2026-10-05T23:00:00Z"},
                "end": {"dateTime": "2026-10-06T01:00:00Z"},
                "organizer": {"email": "me@example.com", "self": true},
                "attendees": [
                    {"email": "me@example.com", "self": true, "organizer": true},
                    {"email": "sam@example.com"},
                ],
            });
            edit(&mut event);
            super::super::read_written_event(&event, Some("America/New_York")).unwrap()
        };
        let same = |edit: &dyn Fn(&mut Value)| {
            is_same_create(&draft, &google(edit), Some("America/New_York"))
        };
        assert!(same(&|_| {}), "the event this create makes");
        assert!(!same(&|e| e["summary"] = json!("Lunch")), "another title");
        assert!(
            !same(&|e| e["start"] = json!({"dateTime": "2026-10-05T22:00:00Z"})),
            "another start"
        );
        assert!(
            !same(&|e| e["end"] = json!({"dateTime": "2026-10-06T02:00:00Z"})),
            "another end"
        );
        assert!(
            !same(&|e| {
                e["start"] = json!({"date": "2026-10-05"});
                e["end"] = json!({"date": "2026-10-06"});
            }),
            "all day"
        );
        assert!(
            !same(&|e| e["attendees"][1]["email"] = json!("kim@example.com")),
            "another guest"
        );
        assert!(
            !same(&|e| e["attendees"]
                .as_array_mut()
                .unwrap()
                .push(json!({"email": "kim@example.com"}))),
            "one guest more"
        );
        assert!(
            !same(&|e| {
                e["attendees"].as_array_mut().unwrap().pop();
            }),
            "no guests"
        );
        assert!(
            !same(&|e| e["attendeesOmitted"] = json!(true)),
            "a list Google cut short"
        );
        // An all-day create matches its own all-day event.
        let day = create(|b| {
            b["all_day"] = json!(true);
            b["start"] = json!("2026-10-05");
            b["end"] = json!("2026-10-06");
            b["id"] = json!("abcdef0123");
        })
        .unwrap();
        let held = google(&|e| {
            e["summary"] = json!("Dinner");
            e["start"] = json!({"date": "2026-10-05"});
            e["end"] = json!({"date": "2026-10-07"});
            e.as_object_mut().unwrap().remove("attendees");
        });
        assert!(is_same_create(&day, &held, None));
        let shorter = google(&|e| {
            e["summary"] = json!("Dinner");
            e["start"] = json!({"date": "2026-10-05"});
            e["end"] = json!({"date": "2026-10-06"});
            e.as_object_mut().unwrap().remove("attendees");
        });
        assert!(!is_same_create(&day, &shorter, None), "one day, not two");
    }
}
