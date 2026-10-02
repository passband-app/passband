//! Google Calendar, READ ONLY (docs/CALENDAR.md §8, "Calendar sync"). Nothing
//! here writes to a calendar.
//!
//! Three parts: [`client`] calls the Calendar API, [`sync`] decides when, and
//! this module turns what Google sends into rows that are safe to store
//! ([`read_event`], [`read_calendar_entry`]) and holds the timeline's display
//! match between a ghost and a real event ([`ghost_matches`]).
//!
//! HUMAN DOOR ONLY. Event titles, places, organizers and links are personal
//! data: they are served on `/client/timeline` and nowhere else, never logged,
//! and never put in a triage prompt. A log line names a calendar by its
//! position in the pass, never by its id, because a calendar id is usually an
//! email address.
//!
//! EVERYTHING AN EVENT CARRIES IS BOUNDED. A shared calendar is written by
//! other people, so its text is laundered like a ghost's (control, bidi and
//! invisible characters out, 200 characters at most, a meeting link without
//! its query), and the number of calendars and of events per calendar is
//! capped.

pub mod client;
pub mod sync;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::triage::ghost::{self, EventTime};

/// The synced window, around now: a month back, 400 days ahead (the widest
/// range `/client/timeline` serves).
pub const WINDOW_BEHIND_DAYS: i64 = 30;
pub const WINDOW_AHEAD_DAYS: i64 = 400;

/// Calendars read in one pass at most, the primary one first.
pub const MAX_CALENDARS: usize = 25;

/// Events stored per calendar at most. A new event past it is not stored
/// until room frees up; an event already stored keeps updating.
pub const MAX_EVENTS_PER_CALENDAR: i64 = 5000;

/// The stored id of the user's primary calendar, whatever Google calls it.
/// Google accepts `primary` as an alias on every events call, and without the
/// list scope that alias is all the daemon knows, so the row keeps one id
/// whether or not the list scope is granted later.
pub const PRIMARY: &str = "primary";

const ID_CAP: usize = 1024;
const ADDRESS_CAP: usize = 320;
const LINK_CAP: usize = 2048;
const STAMP_CAP: usize = 64;

/// Why a calendar call failed, as stored and served: a kind, never a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CalendarErrorKind {
    /// Google answered, but not with a usable answer (5xx, a timeout, a body
    /// that does not parse, a calendar too large to page through).
    Unavailable,
    /// Google is rate limiting this user or project.
    Quota,
    /// The token is dead, or lacks the scope (a 401, a 403 that is not a rate
    /// limit, a refresh Google refused).
    Auth,
}

impl CalendarErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Quota => "quota",
            Self::Unavailable => "unavailable",
        }
    }
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "auth" => Some(Self::Auth),
            "quota" => Some(Self::Quota),
            "unavailable" => Some(Self::Unavailable),
            _ => None,
        }
    }
}

/// One calendar to read, as the pass found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarMeta {
    /// [`PRIMARY`] for the primary calendar, Google's id otherwise.
    pub calendar_id: String,
    pub summary: Option<String>,
    pub is_primary: bool,
    pub time_zone: Option<String>,
}

/// A calendar as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCalendar {
    pub calendar_id: String,
    pub summary: Option<String>,
    pub is_primary: bool,
    pub selected: bool,
    pub time_zone: Option<String>,
    pub sync_token: Option<String>,
    pub synced_at: Option<DateTime<Utc>>,
    pub full_synced_at: Option<DateTime<Utc>>,
    pub last_error_kind: Option<CalendarErrorKind>,
}

/// The user's answer to an invitation, from the attendee entry marked `self`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfResponse {
    Accepted,
    Declined,
    Tentative,
    NeedsAction,
}

impl SelfResponse {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Declined => "declined",
            Self::Tentative => "tentative",
            Self::NeedsAction => "needs_action",
        }
    }
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "accepted" => Some(Self::Accepted),
            "declined" => Some(Self::Declined),
            "tentative" => Some(Self::Tentative),
            "needs_action" | "needsAction" => Some(Self::NeedsAction),
            _ => None,
        }
    }
}

/// One event, laundered and ready to store. `status` is `confirmed` or
/// `tentative`: a cancelled event is never a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    pub event_id: String,
    pub ical_uid: Option<String>,
    pub tentative: bool,
    pub summary: Option<String>,
    pub location: Option<String>,
    pub start: EventTime,
    /// The end. A timed end is Google's instant. An ALL-DAY end is the
    /// INCLUSIVE last day, the ghosts' convention (a date-only end runs
    /// through that day), converted from Google's exclusive `end.date`: Oct 5
    /// to Oct 7 for a three-day event, and None for a one-day event, whose
    /// last day is its start (a ghost's date-only end is likewise only ever
    /// after its start). None too when Google gave no end, or one not after
    /// the start at the same precision.
    pub end: Option<EventTime>,
    pub start_timezone: Option<String>,
    pub organizer_email: Option<String>,
    pub self_response: Option<SelfResponse>,
    pub transparent: bool,
    pub html_link: Option<String>,
    pub updated_at: Option<String>,
}

impl EventRow {
    pub fn all_day(&self) -> bool {
        !self.start.has_clock()
    }
}

/// What one item of an events page does to the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventItem {
    Upsert(Box<EventRow>),
    /// Cancelled, outside the window, or with no usable start: whatever is
    /// stored under this id goes.
    Remove(String),
}

/// One stored event as the timeline serves it, with its calendar's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncedEvent {
    pub calendar_id: String,
    pub calendar_name: Option<String>,
    pub row: EventRow,
}

/// The synced window for a pass that starts at `now`.
pub fn window(now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    (
        now - chrono::Duration::days(WINDOW_BEHIND_DAYS),
        now + chrono::Duration::days(WINDOW_AHEAD_DAYS),
    )
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// A bounded identifier: laundered, non-empty, at most `cap` characters (an
/// over-long one is refused, not cut, since a cut id names something else).
fn bounded_id(raw: Option<&str>, cap: usize) -> Option<String> {
    let id = crate::triage::order_link::plain(raw?);
    (!id.is_empty() && id.chars().count() <= cap).then_some(id)
}

/// A start or end: `dateTime` (RFC3339) for a timed event, `date`
/// (`YYYY-MM-DD`) for an all-day one, in the text form ghosts use.
fn event_time(value: Option<&Value>) -> Option<EventTime> {
    let value = value?;
    if let Some(at) = value.get("dateTime").and_then(Value::as_str) {
        return match EventTime::parse(at)? {
            at @ EventTime::At(_) => Some(at),
            EventTime::Date(_) => None,
        };
    }
    match EventTime::parse(value.get("date").and_then(Value::as_str)?)? {
        day @ EventTime::Date(_) => Some(day),
        EventTime::At(_) => None,
    }
}

/// A start or end as one instant for the window test: a date at 00:00Z.
fn instant(time: &EventTime) -> DateTime<Utc> {
    match time {
        EventTime::At(at) => at.with_timezone(&Utc),
        EventTime::Date(day) => day.and_time(chrono::NaiveTime::MIN).and_utc(),
    }
}

/// Only Google's own event pages: `htmlLink` is opened by the client, and a
/// link to anywhere else has no business being one.
fn google_link(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.chars().count() > LINK_CAP {
        return None;
    }
    let url = url::Url::parse(raw).ok()?;
    let host = url.host_str()?;
    let google = host == "google.com" || host.ends_with(".google.com");
    (url.scheme() == "https" && google && url.username().is_empty() && url.password().is_none())
        .then(|| url.to_string())
}

/// An organizer's address, only when it looks like one.
fn address(raw: Option<&str>) -> Option<String> {
    let addr = crate::triage::order_link::plain(raw?);
    (addr.contains('@') && !addr.contains(' ') && addr.chars().count() <= ADDRESS_CAP)
        .then_some(addr)
}

/// Read one item of an `events.list` page. None when the item has no usable
/// id (there is nothing to store or remove it under).
///
/// `calendar_zone` is the calendar's own zone, the start zone of a timed event
/// that names none. The window test is Google's own for `timeMin`/`timeMax`:
/// the event ends after the window opens and starts before it closes. An
/// incremental page is not bounded by the window (Google refuses `timeMin`
/// beside a sync token), so an event that moved out of it is removed here.
pub fn read_event(
    item: &Value,
    calendar_zone: Option<&str>,
    window: (DateTime<Utc>, DateTime<Utc>),
) -> Option<EventItem> {
    let event_id = bounded_id(item.get("id").and_then(Value::as_str), ID_CAP)?;
    let status = item.get("status").and_then(Value::as_str);
    if status == Some("cancelled") {
        return Some(EventItem::Remove(event_id));
    }
    let Some(start) = event_time(item.get("start")) else {
        return Some(EventItem::Remove(event_id));
    };
    // Google's end, exclusive for an all-day event, which is what the window
    // test (Google's own) reads; converted to the inclusive last day below.
    let end = event_time(item.get("end")).filter(|end| end.is_after(&start));
    let last = end.as_ref().map_or_else(|| instant(&start), instant);
    let (open, close) = window;
    // A zero-length event (no end) ends where it starts, so it counts when it
    // starts inside the window.
    let inside =
        instant(&start) < close && (last > open || (end.is_none() && instant(&start) >= open));
    if !inside {
        return Some(EventItem::Remove(event_id));
    }
    let start_zone = item
        .get("start")
        .and_then(|s| s.get("timeZone"))
        .and_then(Value::as_str);
    let start_timezone = match (start_zone, start.has_clock()) {
        (Some(zone), _) => ghost::iana_zone(zone),
        (None, true) => calendar_zone.and_then(ghost::iana_zone),
        (None, false) => None,
    };
    let self_response = item
        .get("attendees")
        .and_then(Value::as_array)
        .and_then(|attendees| {
            attendees
                .iter()
                .find(|a| a.get("self").and_then(Value::as_bool) == Some(true))
        })
        .and_then(|me| me.get("responseStatus").and_then(Value::as_str))
        .and_then(SelfResponse::parse);
    let updated_at = text(item, "updated")
        .filter(|u| u.len() <= STAMP_CAP && chrono::DateTime::parse_from_rfc3339(u).is_ok());
    let end = end.and_then(|end| match end {
        EventTime::Date(day) => day
            .pred_opt()
            .map(EventTime::Date)
            .filter(|last| last.is_after(&start)),
        at => Some(at),
    });
    Some(EventItem::Upsert(Box::new(EventRow {
        event_id,
        ical_uid: bounded_id(item.get("iCalUID").and_then(Value::as_str), ID_CAP),
        tentative: status == Some("tentative"),
        summary: item
            .get("summary")
            .and_then(Value::as_str)
            .and_then(ghost::clean_text),
        location: item
            .get("location")
            .and_then(Value::as_str)
            .and_then(ghost::launder_location),
        start,
        end,
        start_timezone,
        organizer_email: address(
            item.get("organizer")
                .and_then(|o| o.get("email"))
                .and_then(Value::as_str),
        ),
        self_response,
        transparent: item.get("transparency").and_then(Value::as_str) == Some("transparent"),
        html_link: google_link(item.get("htmlLink").and_then(Value::as_str)),
        updated_at,
    })))
}

/// One `calendarList` entry: the calendar and whether to read it. Only a
/// calendar the user shows (`selected`), and not a deleted or hidden one, is
/// read; the primary calendar always is. None for an entry with no usable id.
pub fn read_calendar_entry(item: &Value) -> Option<(CalendarMeta, bool)> {
    let is_primary = item.get("primary").and_then(Value::as_bool) == Some(true);
    let id = bounded_id(item.get("id").and_then(Value::as_str), ID_CAP)?;
    let flag = |key| item.get(key).and_then(Value::as_bool) == Some(true);
    let wanted = is_primary || (flag("selected") && !flag("deleted") && !flag("hidden"));
    // What the user renamed it to, if they did: that is the name they see.
    let summary = ["summaryOverride", "summary"].iter().find_map(|key| {
        item.get(*key)
            .and_then(Value::as_str)
            .and_then(ghost::clean_text)
    });
    Some((
        CalendarMeta {
            calendar_id: if is_primary { PRIMARY.to_string() } else { id },
            summary,
            is_primary,
            time_zone: text(item, "timeZone").and_then(|z| ghost::iana_zone(&z)),
        },
        wanted,
    ))
}

/// A title for comparison: lowercased, whitespace collapsed.
pub fn normalize_title(title: &str) -> String {
    title
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Two starts are one: the same instant, whatever the offsets, or the same
/// date for two all-day starts. A date never matches an instant.
pub fn same_start(a: &EventTime, b: &EventTime) -> bool {
    match (a, b) {
        (EventTime::At(a), EventTime::At(b)) => a.with_timezone(&Utc) == b.with_timezone(&Utc),
        (EventTime::Date(a), EventTime::Date(b)) => a == b,
        _ => false,
    }
}

/// THE DISPLAY MATCH between a ghost and a real event: the same start (see
/// [`same_start`]) and titles that are equal after [`normalize_title`], or one
/// containing the other. An empty title matches nothing (every string
/// contains the empty one).
///
/// Display de-duplication ONLY: it is computed when the timeline is listed
/// and never stored, so it changes no ghost state and cannot go stale.
pub fn ghost_matches(
    ghost_title: &str,
    ghost_start: &EventTime,
    event_title: Option<&str>,
    event_start: &EventTime,
) -> bool {
    if !same_start(ghost_start, event_start) {
        return false;
    }
    let ghost = normalize_title(ghost_title);
    let event = normalize_title(event_title.unwrap_or_default());
    !ghost.is_empty()
        && !event.is_empty()
        && (ghost == event || ghost.contains(&event) || event.contains(&ghost))
}

/// [`ghost_matches`] for a whole listing: for each ghost (its title and
/// start), the index of the first event in `events` (in their served order)
/// that it matches. Events are grouped by start first, so this is linear in
/// the two listings rather than their product.
pub fn match_ghosts<'a>(
    ghosts: impl IntoIterator<Item = (&'a str, &'a EventTime)>,
    events: &[SyncedEvent],
) -> Vec<Option<usize>> {
    #[derive(Hash, PartialEq, Eq)]
    enum Key {
        At(DateTime<Utc>),
        Date(chrono::NaiveDate),
    }
    let key = |t: &EventTime| match t {
        EventTime::At(at) => Key::At(at.with_timezone(&Utc)),
        EventTime::Date(day) => Key::Date(*day),
    };
    let mut by_start: std::collections::HashMap<Key, Vec<usize>> = Default::default();
    for (i, event) in events.iter().enumerate() {
        by_start.entry(key(&event.row.start)).or_default().push(i);
    }
    ghosts
        .into_iter()
        .map(|(title, start)| {
            by_start.get(&key(start))?.iter().copied().find(|&i| {
                ghost_matches(
                    title,
                    start,
                    events[i].row.summary.as_deref(),
                    &events[i].row.start,
                )
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(raw: &str) -> EventTime {
        EventTime::parse(raw).unwrap()
    }

    fn now() -> DateTime<Utc> {
        "2026-10-01T12:00:00Z".parse().unwrap()
    }

    fn upsert(item: Value) -> EventRow {
        match read_event(&item, Some("America/New_York"), window(now())) {
            Some(EventItem::Upsert(row)) => *row,
            other => panic!("not an upsert: {other:?}"),
        }
    }

    #[test]
    fn the_match_rule_takes_the_same_start_and_a_contained_title() {
        let start = at("2026-10-05T19:00:00-04:00");
        // The same instant written in another offset.
        let utc = at("2026-10-05T23:00:00Z");
        assert!(ghost_matches(
            "Dinner at Lilia",
            &start,
            Some("dinner  at LILIA"),
            &utc
        ));
        assert!(ghost_matches(
            "Dinner",
            &start,
            Some("Dinner with Sam"),
            &utc
        ));
        assert!(ghost_matches(
            "Phoebe Bridgers at Brooklyn Steel",
            &start,
            Some("phoebe bridgers"),
            &start
        ));
        let day = at("2026-10-05");
        assert!(ghost_matches("Conference", &day, Some("Conference"), &day));
    }

    #[test]
    fn the_match_rule_refuses_near_misses() {
        let start = at("2026-10-05T19:00:00-04:00");
        // The same title a day apart.
        assert!(!ghost_matches(
            "Dinner",
            &start,
            Some("Dinner"),
            &at("2026-10-06T19:00:00-04:00")
        ));
        // A minute apart.
        assert!(!ghost_matches(
            "Dinner",
            &start,
            Some("Dinner"),
            &at("2026-10-05T19:01:00-04:00")
        ));
        // The same instant, unrelated titles.
        assert!(!ghost_matches("Dinner", &start, Some("Dentist"), &start));
        // All-day against timed on the same date, both ways.
        let day = at("2026-10-05");
        assert!(!ghost_matches("Dinner", &day, Some("Dinner"), &start));
        assert!(!ghost_matches("Dinner", &start, Some("Dinner"), &day));
        // All-day a date apart.
        assert!(!ghost_matches(
            "Trip",
            &day,
            Some("Trip"),
            &at("2026-10-06")
        ));
        // No title never matches, even though "" is in every string.
        assert!(!ghost_matches("Dinner", &start, None, &start));
        assert!(!ghost_matches("Dinner", &start, Some("   "), &start));
        assert!(!ghost_matches("", &start, Some("Dinner"), &start));
    }

    #[test]
    fn a_listing_matches_each_ghost_to_its_first_matching_event() {
        let event = |cal: &str, title: &str, start: &str| SyncedEvent {
            calendar_id: cal.into(),
            calendar_name: None,
            row: EventRow {
                event_id: format!("{cal}-{title}"),
                ical_uid: None,
                tentative: false,
                summary: Some(title.into()),
                location: None,
                start: at(start),
                end: None,
                start_timezone: None,
                organizer_email: None,
                self_response: None,
                transparent: false,
                html_link: None,
                updated_at: None,
            },
        };
        let events = [
            event("primary", "Dentist", "2026-10-05T19:00:00Z"),
            event("primary", "Dinner with Sam", "2026-10-05T19:00:00Z"),
            event("team", "Dinner", "2026-10-05T19:00:00Z"),
            event("primary", "Trip", "2026-10-07"),
        ];
        let (dinner, trip, nothing) = (
            at("2026-10-05T15:00:00-04:00"),
            at("2026-10-07"),
            at("2026-10-06T19:00:00Z"),
        );
        assert_eq!(
            match_ghosts(
                [
                    ("dinner", &dinner),
                    ("Trip", &trip),
                    ("Dinner", &nothing),
                    ("Trip", &dinner)
                ],
                &events
            ),
            [Some(1), Some(3), None, None]
        );
    }

    #[test]
    fn a_timed_event_reads_every_field() {
        let row = upsert(json!({
            "id": "evt1",
            "status": "tentative",
            "iCalUID": "uid1@google.com",
            "summary": "Coffee\u{200B} with\tSam",
            "location": "Zoom: https://zoom.us/j/123?pwd=secret",
            "start": {"dateTime": "2026-10-05T09:00:00-04:00", "timeZone": "America/New_York"},
            "end": {"dateTime": "2026-10-05T09:30:00-04:00"},
            "organizer": {"email": "sam@example.com"},
            "attendees": [
                {"email": "sam@example.com", "responseStatus": "accepted"},
                {"email": "me@example.com", "self": true, "responseStatus": "needsAction"}
            ],
            "transparency": "transparent",
            "htmlLink": "https://www.google.com/calendar/event?eid=abc",
            "updated": "2026-09-30T10:00:00.000Z"
        }));
        assert_eq!(row.event_id, "evt1");
        assert!(row.tentative);
        assert_eq!(row.summary.as_deref(), Some("Coffee with Sam"));
        assert_eq!(row.location.as_deref(), Some("Zoom: zoom.us/j/123"));
        assert_eq!(row.start.to_string(), "2026-10-05T09:00:00-04:00");
        assert_eq!(row.end.unwrap().to_string(), "2026-10-05T09:30:00-04:00");
        assert_eq!(row.start_timezone.as_deref(), Some("America/New_York"));
        assert_eq!(row.organizer_email.as_deref(), Some("sam@example.com"));
        assert_eq!(row.self_response, Some(SelfResponse::NeedsAction));
        assert!(row.transparent);
        assert_eq!(
            row.html_link.as_deref(),
            Some("https://www.google.com/calendar/event?eid=abc")
        );
        assert!(!row.all_day());
    }

    #[test]
    fn an_all_day_end_is_the_inclusive_last_day() {
        // One day: Google's exclusive end is the next day; the last day is
        // the start, so there is no end, as with a one-day ghost.
        let one = upsert(json!({
            "id": "one", "start": {"date": "2026-10-05"}, "end": {"date": "2026-10-06"}
        }));
        assert_eq!(one.end, None);
        // Three days, the 5th through the 7th.
        let three = upsert(json!({
            "id": "three", "start": {"date": "2026-10-05"}, "end": {"date": "2026-10-08"}
        }));
        assert_eq!(three.end.unwrap().to_string(), "2026-10-07");
        // A timed end is untouched.
        let timed = upsert(json!({
            "id": "t", "start": {"dateTime": "2026-10-05T09:00:00Z"},
            "end": {"dateTime": "2026-10-06T00:00:00Z"}
        }));
        assert_eq!(timed.end.unwrap().to_string(), "2026-10-06T00:00:00+00:00");
        // The window still reads Google's exclusive end: a one-day event on
        // the window's first day is inside it.
        let open = window(now()).0.date_naive();
        let first_day = read_event(
            &json!({"id": "edge", "start": {"date": open.to_string()},
                    "end": {"date": open.succ_opt().unwrap().to_string()}}),
            None,
            window(now()),
        );
        assert!(
            matches!(first_day, Some(EventItem::Upsert(_))),
            "{first_day:?}"
        );
    }

    #[test]
    fn an_all_day_event_keeps_its_dates_and_no_zone() {
        let row = upsert(json!({
            "id": "allday",
            "summary": "Offsite",
            "start": {"date": "2026-10-05"},
            "end": {"date": "2026-10-07"}
        }));
        assert!(row.all_day());
        assert_eq!(row.start.to_string(), "2026-10-05");
        assert_eq!(row.end.unwrap().to_string(), "2026-10-06", "inclusive");
        assert_eq!(row.start_timezone, None, "a date has no zone");
        assert_eq!(row.self_response, None, "no attendees: the user's own");
        assert!(!row.transparent);
    }

    #[test]
    fn a_timed_event_without_a_zone_takes_the_calendars() {
        let row = upsert(json!({
            "id": "e", "start": {"dateTime": "2026-10-05T09:00:00Z"}
        }));
        assert_eq!(row.start_timezone.as_deref(), Some("America/New_York"));
        assert_eq!(row.end, None);
    }

    #[test]
    fn cancelled_startless_and_out_of_window_events_are_removals() {
        let w = window(now());
        let remove = |item: Value| read_event(&item, None, w);
        assert_eq!(
            remove(json!({"id": "c", "status": "cancelled"})),
            Some(EventItem::Remove("c".into()))
        );
        assert_eq!(
            remove(json!({"id": "n", "summary": "No start"})),
            Some(EventItem::Remove("n".into()))
        );
        assert_eq!(
            remove(
                json!({"id": "old", "start": {"dateTime": "2026-01-01T09:00:00Z"},
                          "end": {"dateTime": "2026-01-01T10:00:00Z"}})
            ),
            Some(EventItem::Remove("old".into()))
        );
        assert_eq!(
            remove(json!({"id": "far", "start": {"date": "2028-01-01"}})),
            Some(EventItem::Remove("far".into()))
        );
        // No id: nothing to store or remove.
        assert_eq!(remove(json!({"status": "cancelled"})), None);
        assert_eq!(remove(json!({"id": ""})), None);
        assert_eq!(remove(json!({"id": "x".repeat(2000)})), None);
    }

    #[test]
    fn text_is_laundered_and_capped_and_links_are_googles_only() {
        let row = upsert(json!({
            "id": "e",
            "summary": format!("\u{202E}{}", "a".repeat(500)),
            "start": {"dateTime": "2026-10-05T09:00:00Z"},
            "organizer": {"email": "not an address"},
            "htmlLink": "https://evil.example/calendar/event?eid=abc",
            "attendees": [{"self": true, "responseStatus": "something-new"}]
        }));
        assert_eq!(row.summary.as_deref().map(|s| s.chars().count()), Some(200));
        assert!(!row.summary.unwrap().contains('\u{202E}'));
        assert_eq!(row.organizer_email, None);
        assert_eq!(row.html_link, None);
        assert_eq!(row.self_response, None);
        for bad in [
            "http://www.google.com/calendar/event?eid=abc",
            "https://google.com.evil.example/x",
            "https://user:pw@www.google.com/x",
        ] {
            assert_eq!(google_link(Some(bad)), None, "{bad}");
        }
        assert!(google_link(Some("https://calendar.google.com/calendar/event?eid=x")).is_some());
    }

    #[test]
    fn calendar_entries_read_the_primary_and_the_shown_ones() {
        let (primary, wanted) = read_calendar_entry(&json!({
            "id": "me@example.com", "primary": true, "summary": "Me",
            "timeZone": "America/New_York", "selected": false
        }))
        .unwrap();
        assert!(wanted, "the primary is always read");
        assert_eq!(primary.calendar_id, PRIMARY);
        assert_eq!(primary.time_zone.as_deref(), Some("America/New_York"));
        let (shared, wanted) = read_calendar_entry(&json!({
            "id": "team@group.calendar.google.com", "summary": "Team",
            "summaryOverride": "My team", "selected": true
        }))
        .unwrap();
        assert!(wanted);
        assert_eq!(shared.summary.as_deref(), Some("My team"));
        for entry in [
            json!({"id": "a", "selected": false}),
            json!({"id": "b"}),
            json!({"id": "c", "selected": true, "hidden": true}),
            json!({"id": "d", "selected": true, "deleted": true}),
        ] {
            assert!(!read_calendar_entry(&entry).unwrap().1, "{entry}");
        }
    }
}
