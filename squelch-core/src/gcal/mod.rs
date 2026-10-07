//! Google Calendar (docs/CALENDAR.md §8, "Calendar sync" and "Creating and
//! editing events").
//!
//! Four parts: [`client`] calls the Calendar API, [`sync`] decides when to
//! read, [`write`] checks a user's create or edit (and its guests) before
//! anything is sent, and
//! this module turns what Google sends into rows that are safe to store
//! ([`read_event`], [`read_calendar_entry`]) and holds the timeline's display
//! match between a ghost and a real event ([`ghost_matches`]).
//!
//! THE ONLY WRITES are the user's own create, edit and delete on the human
//! door, one event at a time. Nothing here writes to a calendar on its own,
//! and Google's answer to a write goes through [`read_event`] like any synced
//! item, so one event shape exists.
//!
//! GUESTS. An event's attendees are read (at most [`MAX_GUESTS`], and marked
//! truncated past it), stored and served on the human door, and the user may
//! set the guest list of an event they organize on their own calendar
//! ([`SyncedEvent::guest_editable`]). Google sends the invitation, update and
//! cancellation mail itself, and only when the write asks for it
//! ([`client::Notify`]): nothing here composes or sends mail.
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
//! capped. Google is asked for less in the first place: a `fields` mask names
//! only what [`read_event`] reads, and `maxAttendees` bounds the guest list
//! at [`MAX_GUESTS`] ([`client`]).

pub mod client;
pub mod sync;
pub mod write;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::triage::ghost::{self, EventTime};

/// The synced window, around now: a month back, 400 days ahead (the widest
/// range `/client/timeline` serves).
pub const WINDOW_BEHIND_DAYS: i64 = 30;
pub const WINDOW_AHEAD_DAYS: i64 = 400;

/// Calendars read in one pass at most, the primary one first.
pub const MAX_CALENDARS: usize = 25;

/// Events stored per calendar at most. `events.list` has no defined order
/// without `orderBy` (which a sync token refuses), so a calendar past it keeps
/// the events NEAREST the present, by start: the farthest are dropped, which
/// in a window of a month back and 400 days ahead means far-future events go
/// first. The calendar is then marked truncated until a full read fits.
pub const MAX_EVENTS_PER_CALENDAR: i64 = 5000;

/// Guests read, stored and served per event at most. It is also the
/// `maxAttendees` every events request carries: past it Google returns "only
/// the participant" and marks the event `attendeesOmitted`, which is stored
/// as the event's truncated mark and makes its guest list read-only.
pub const MAX_GUESTS: usize = 100;

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

/// The user's effective role on a calendar: `calendarList`'s `accessRole`,
/// "the effective access role that the authenticated user has on the
/// calendar".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessRole {
    FreeBusyReader,
    Reader,
    WriterWithoutPrivateAccess,
    Writer,
    Owner,
}

impl AccessRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FreeBusyReader => "freeBusyReader",
            Self::Reader => "reader",
            Self::WriterWithoutPrivateAccess => "writerWithoutPrivateAccess",
            Self::Writer => "writer",
            Self::Owner => "owner",
        }
    }
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "freeBusyReader" => Some(Self::FreeBusyReader),
            "reader" => Some(Self::Reader),
            "writerWithoutPrivateAccess" => Some(Self::WriterWithoutPrivateAccess),
            "writer" => Some(Self::Writer),
            "owner" => Some(Self::Owner),
            _ => None,
        }
    }
    /// Whether the user may create and edit events on the calendar from the
    /// app: `owner` or `writer` only. `writerWithoutPrivateAccess` can write
    /// too, but not to the private events it sees with their details hidden,
    /// and a write the app offers and Google then refuses is worse than one
    /// it does not offer.
    pub fn writable(self) -> bool {
        matches!(self, Self::Owner | Self::Writer)
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
    /// `#rrggbb`, lowercase: Google's `backgroundColor`, or the calendar
    /// palette's colour for its `colorId`.
    pub color: Option<String>,
    pub access_role: Option<AccessRole>,
}

impl CalendarMeta {
    /// The primary calendar when nothing but its alias is known (no list
    /// scope). The user owns their primary calendar; its colour is unknown.
    pub fn primary_only() -> Self {
        Self {
            calendar_id: PRIMARY.to_string(),
            summary: None,
            is_primary: true,
            time_zone: None,
            color: None,
            access_role: Some(AccessRole::Owner),
        }
    }
}

/// A colour as Google writes it, `#0088aa`, normalized to lowercase. Anything
/// else is None: the value is served to a client that draws with it.
pub fn hex_color(raw: &str) -> Option<String> {
    let hex = raw.strip_prefix('#')?;
    (hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| format!("#{}", hex.to_ascii_lowercase()))
}

/// Google's global EVENT palette (`colors.get`, `event`: "A global palette of
/// event colors, mapping from the color ID to its definition"), by id. Held
/// here rather than fetched because `colors.get` does not accept the
/// `calendar.events` scope, which is the one scope a calendar grant is sure to
/// carry.
const EVENT_PALETTE: [&str; 11] = [
    "#a4bdfc", "#7ae7bf", "#dbadff", "#ff887c", "#fbd75b", "#ffb878", "#46d6db", "#e1e1e1",
    "#5484ed", "#51b749", "#dc2127",
];

/// Google's global CALENDAR palette, by id, for a list entry that carries a
/// `colorId` and no `backgroundColor` (which "supersedes the index-based
/// colorId property", so in practice this is a fallback that never runs).
const CALENDAR_PALETTE: [&str; 24] = [
    "#ac725e", "#d06b64", "#f83a22", "#fa573c", "#ff7537", "#ffad46", "#42d692", "#16a765",
    "#7bd148", "#b3dc6c", "#fbe983", "#fad165", "#92e1c0", "#9fe1e7", "#9fc6e7", "#4986e7",
    "#9a9cff", "#b99aff", "#c2c2c2", "#cabdbf", "#cca6ac", "#f691b2", "#cd74e6", "#a47ae2",
];

fn palette(palette: &[&'static str], id: &str) -> Option<&'static str> {
    let n: usize = id.parse().ok()?;
    palette.get(n.checked_sub(1)?).copied()
}

/// An event's own colour override, by its `colorId`.
pub fn event_palette_color(color_id: &str) -> Option<&'static str> {
    palette(&EVENT_PALETTE, color_id)
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
    /// Reads of this calendar that failed in a row, and when a SECONDARY
    /// calendar may be tried again: one broken shared calendar backs off on
    /// its own and never holds back the others.
    pub failures: u32,
    pub retry_at: Option<DateTime<Utc>>,
    /// Events were dropped at [`MAX_EVENTS_PER_CALENDAR`].
    pub truncated: bool,
    pub color: Option<String>,
    pub access_role: Option<AccessRole>,
}

impl StoredCalendar {
    /// A calendar the app may create and edit events on: one being read, on
    /// which the user is `owner` or `writer`.
    pub fn writable(&self) -> bool {
        self.selected && self.access_role.is_some_and(AccessRole::writable)
    }

    /// THE INVITE GUARD, calendar half: whether an event on this calendar
    /// may carry guests set from the app. See [`may_invite`].
    pub fn can_invite(&self) -> bool {
        self.selected && may_invite(self.is_primary, self.access_role)
    }
}

/// THE INVITE GUARD: guests may be set only on the user's OWN calendar, the
/// primary one or one they are `owner` of. A calendar the user merely writes
/// to (`writer`) belongs to someone else, and an invitation from it would go
/// out under that calendar's name on the user's say-so.
pub fn may_invite(is_primary: bool, access: Option<AccessRole>) -> bool {
    is_primary || access == Some(AccessRole::Owner)
}

/// The timeline's `calendar` status, from the stored rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CalendarStatus {
    /// When the calendars the timeline can trust were last read: the oldest
    /// among the primary and every secondary calendar not currently failing.
    pub synced_at: Option<DateTime<Utc>>,
    /// The credential's or the PRIMARY calendar's failure. A secondary
    /// calendar's failure is not an account error and is not served.
    pub error: Option<CalendarErrorKind>,
    /// Some calendar holds fewer events than Google has in the window.
    pub truncated: bool,
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

/// A guest's answer to the invitation: Google's `responseStatus`, whose
/// "Possible values are: `needsAction`, `declined`, `tentative`, `accepted`".
/// `needsAction` (and anything unreadable) is served as `pending`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestResponse {
    Accepted,
    Declined,
    Tentative,
    Pending,
}

impl GuestResponse {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Declined => "declined",
            Self::Tentative => "tentative",
            Self::Pending => "pending",
        }
    }
    pub fn parse(raw: &str) -> Self {
        match raw {
            "accepted" => Self::Accepted,
            "declined" => Self::Declined,
            "tentative" => Self::Tentative,
            _ => Self::Pending,
        }
    }
}

/// One attendee of an event as stored and served: an address, a name, an
/// answer. Other people's data written by other people, so laundered like
/// every other calendar text. What else Google holds about an attendee (a
/// comment, extra guests, optional) is never stored: a guest edit reads it
/// fresh from Google and hands it back untouched ([`write::merge_attendees`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guest {
    pub email: String,
    pub name: Option<String>,
    pub response: GuestResponse,
    /// Google's `attendees[].organizer`.
    pub organizer: bool,
    /// Google's `attendees[].self`: "Whether this entry represents the
    /// calendar on which this copy of the event appears."
    pub is_self: bool,
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
    /// Google sent an organizer that is not this calendar (`organizer.self`
    /// not true) and whose address could not be read (absent, or failing
    /// [`address`]). Somebody organizes the event and it is not known to be
    /// the user, so it is NOT the user's own: distinct from an event that
    /// names no organizer at all.
    pub organizer_unreadable: bool,
    /// Google's `organizer.self`: "Whether the organizer corresponds to the
    /// calendar on which this copy of the event appears."
    pub organizer_self: bool,
    pub self_response: Option<SelfResponse>,
    pub transparent: bool,
    pub html_link: Option<String>,
    pub updated_at: Option<String>,
    /// The event's own colour override: an id into the event palette.
    pub color_id: Option<String>,
    /// Laundered like a location, line breaks kept, at most
    /// [`ghost::DESCRIPTION_CAP`] characters.
    pub description: Option<String>,
    /// The attendees Google sent, in its order, at most [`MAX_GUESTS`].
    pub guests: Vec<Guest>,
    /// Google holds more attendees than `guests` (`attendeesOmitted`, or a
    /// list past the cap). The guest list is then read-only.
    pub guests_truncated: bool,
}

impl EventRow {
    pub fn all_day(&self) -> bool {
        !self.start.has_clock()
    }

    /// The guests other than the user's own entry: who a notification
    /// would reach.
    pub fn other_guests(&self) -> usize {
        self.guests.iter().filter(|g| !g.is_self).count()
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

/// What applying one events page did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageApplied {
    /// The ids upserted from this page (a full read's "seen" set).
    pub stored: Vec<String>,
    /// Events the per-calendar cap dropped while applying it.
    pub dropped: usize,
}

/// One stored event as the timeline serves it, with its calendar's name,
/// colour and the user's role on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncedEvent {
    pub calendar_id: String,
    pub calendar_name: Option<String>,
    pub calendar_color: Option<String>,
    pub calendar_access: Option<AccessRole>,
    pub calendar_primary: bool,
    pub row: EventRow,
}

impl SyncedEvent {
    /// The colour it is drawn in: its own override when it has one, else its
    /// calendar's.
    pub fn color(&self) -> Option<String> {
        self.row
            .color_id
            .as_deref()
            .and_then(event_palette_color)
            .map(str::to_string)
            .or_else(|| self.calendar_color.clone())
    }

    /// THE EDIT GUARD: whether the app offers to edit or delete this event.
    /// The calendar is one the user writes to (`owner` or `writer`), AND the
    /// event is the user's own: Google marks this calendar as its organizer
    /// (`organizer.self`), or it names no organizer at all, or the organizer
    /// is the mailbox's own address. An invitation from someone else, sitting
    /// on the user's own calendar, is theirs to change, not the user's.
    ///
    /// It FAILS CLOSED on doubt: an organizer Google sent but whose address
    /// could not be read ([`EventRow::organizer_unreadable`]) is someone
    /// else, and so is an event whose stored guest list names an organizer
    /// that is not this calendar (`organizer` and not `self`), whatever the
    /// organizer field says.
    pub fn editable(&self, account_email: &str) -> bool {
        let writable = self.calendar_access.is_some_and(AccessRole::writable);
        let organized_elsewhere = self.row.guests.iter().any(|g| g.organizer && !g.is_self);
        let own = self.row.organizer_self
            || (!self.row.organizer_unreadable
                && self
                    .row
                    .organizer_email
                    .as_deref()
                    .is_none_or(|org| org.eq_ignore_ascii_case(account_email.trim())));
        writable && own && !organized_elsewhere
    }

    /// THE GUEST GUARD: whether the app offers to change this event's guest
    /// list. The event is [`Self::editable`]; the list is whole (never
    /// rebuild a guest list from a truncated one: Google replaces the array,
    /// so everyone not in it would be removed); the user ORGANIZES it (Google
    /// says so, or the organizer is the mailbox's own address: an event that
    /// names no organizer is editable but not a thing to invite people to);
    /// and it sits on a calendar that may invite ([`may_invite`]).
    pub fn guest_editable(&self, account_email: &str) -> bool {
        let organizes = self.row.organizer_self
            || self
                .row
                .organizer_email
                .as_deref()
                .is_some_and(|org| org.eq_ignore_ascii_case(account_email.trim()));
        self.editable(account_email)
            && !self.row.guests_truncated
            && organizes
            && may_invite(self.calendar_primary, self.calendar_access)
    }
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
/// over-long one is refused, not cut, since a cut id names something else),
/// and never `.` or `..`, which a URL path reads as "here" and "up one" and
/// so cannot name an event ([`client::CalendarApi`] refuses them too).
fn bounded_id(raw: Option<&str>, cap: usize) -> Option<String> {
    let id = crate::triage::order_link::plain(raw?);
    (!id.is_empty() && id != "." && id != ".." && id.chars().count() <= cap).then_some(id)
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

/// An event's attendees as stored: at most [`MAX_GUESTS`] entries with a
/// usable address, in Google's order, and whether Google holds more than
/// that (`attendeesOmitted`: "Whether attendees may have been omitted from
/// the event's representation", or a list longer than the cap, or an entry
/// that could not be read). The user's own entry is never the one cut.
fn read_guests(item: &Value) -> (Vec<Guest>, bool) {
    let omitted = item.get("attendeesOmitted").and_then(Value::as_bool) == Some(true);
    let Some(entries) = item.get("attendees").and_then(Value::as_array) else {
        return (Vec::new(), omitted);
    };
    let flag = |entry: &Value, key| entry.get(key).and_then(Value::as_bool) == Some(true);
    let mut guests: Vec<Guest> = entries
        .iter()
        .filter_map(|entry| {
            Some(Guest {
                email: address(entry.get("email").and_then(Value::as_str))?,
                name: entry
                    .get("displayName")
                    .and_then(Value::as_str)
                    .and_then(ghost::clean_text),
                response: GuestResponse::parse(
                    entry
                        .get("responseStatus")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                ),
                organizer: flag(entry, "organizer"),
                is_self: flag(entry, "self"),
            })
        })
        .collect();
    let unread = guests.len() < entries.len();
    let over = guests.len() > MAX_GUESTS;
    if over {
        // Keep the user's own entry whatever its position.
        let own = guests.iter().position(|g| g.is_self);
        if let Some(own) = own.filter(|at| *at >= MAX_GUESTS) {
            guests.swap(MAX_GUESTS - 1, own);
        }
        guests.truncate(MAX_GUESTS);
    }
    (guests, omitted || unread || over)
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
    // A date has no zone, whatever Google attaches to it: it sends `timeZone`
    // on the start of a recurring all-day series, and serving it would make
    // the client place a date-only event at a clock time.
    let start_timezone = match (start_zone, start.has_clock()) {
        (_, false) => None,
        (Some(zone), true) => ghost::iana_zone(zone),
        (None, true) => calendar_zone.and_then(ghost::iana_zone),
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
    let (guests, guests_truncated) = read_guests(item);
    // The organizer: an address, or none named, or one Google sent that could
    // not be read (which is somebody, and not known to be the user).
    let organizer = item.get("organizer").filter(|o| !o.is_null());
    let organizer_self = organizer
        .and_then(|o| o.get("self"))
        .and_then(Value::as_bool)
        == Some(true);
    let organizer_email = address(
        organizer
            .and_then(|o| o.get("email"))
            .and_then(Value::as_str),
    );
    let organizer_unreadable = organizer.is_some() && !organizer_self && organizer_email.is_none();
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
        organizer_email,
        organizer_unreadable,
        organizer_self,
        self_response,
        transparent: item.get("transparency").and_then(Value::as_str) == Some("transparent"),
        html_link: google_link(item.get("htmlLink").and_then(Value::as_str)),
        updated_at,
        // Only an id the palette knows: anything else would be stored to
        // mean nothing.
        color_id: item
            .get("colorId")
            .and_then(Value::as_str)
            .filter(|id| event_palette_color(id).is_some())
            .map(str::to_string),
        description: item
            .get("description")
            .and_then(Value::as_str)
            .and_then(ghost::launder_description),
        guests,
        guests_truncated,
    })))
}

/// [`read_event`] for Google's answer to a write the user just made: the same
/// reading with no window, because the user may create an event anywhere in
/// time and the answer is served whether or not the sync will keep it (a full
/// read sweeps one outside the window, as it would any other). A cancelled or
/// unreadable answer is None.
pub fn read_written_event(item: &Value, calendar_zone: Option<&str>) -> Option<EventRow> {
    match read_event(
        item,
        calendar_zone,
        (DateTime::<Utc>::MIN_UTC, DateTime::<Utc>::MAX_UTC),
    )? {
        EventItem::Upsert(row) => Some(*row),
        EventItem::Remove(_) => None,
    }
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
    // `backgroundColor` "supersedes the index-based colorId property"; the
    // calendar palette is the fallback for an entry that carries only the id.
    let color = item
        .get("backgroundColor")
        .and_then(Value::as_str)
        .and_then(hex_color)
        .or_else(|| {
            item.get("colorId")
                .and_then(Value::as_str)
                .and_then(|id| palette(&CALENDAR_PALETTE, id))
                .map(str::to_string)
        });
    Some((
        CalendarMeta {
            calendar_id: if is_primary { PRIMARY.to_string() } else { id },
            summary,
            is_primary,
            time_zone: text(item, "timeZone").and_then(|z| ghost::iana_zone(&z)),
            color,
            access_role: item
                .get("accessRole")
                .and_then(Value::as_str)
                .and_then(AccessRole::parse),
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

    /// A stored row with nothing but its start.
    fn row(start: &str) -> EventRow {
        EventRow {
            event_id: "e".into(),
            ical_uid: None,
            tentative: false,
            summary: None,
            location: None,
            start: at(start),
            end: None,
            start_timezone: None,
            organizer_email: None,
            organizer_unreadable: false,
            organizer_self: false,
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
            calendar_color: None,
            calendar_access: None,
            calendar_primary: true,
            row: EventRow {
                summary: Some(title.into()),
                event_id: format!("{cal}-{title}"),
                ..row(start)
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
        // Google names a zone on a recurring all-day series' start; a date
        // still has none.
        let series = upsert(json!({
            "id": "series_20261005",
            "start": {"date": "2026-10-05", "timeZone": "America/New_York"},
            "end": {"date": "2026-10-06", "timeZone": "America/New_York"}
        }));
        assert_eq!(series.start_timezone, None, "a date has no zone");
    }

    /// With `maxAttendees=1` Google sends only the user's own entry when an
    /// event has more guests: the answer still reads from it.
    #[test]
    fn the_users_answer_reads_from_a_lone_attendee_entry() {
        let row = upsert(json!({
            "id": "e", "start": {"dateTime": "2026-10-05T09:00:00Z"},
            "attendeesOmitted": true,
            "attendees": [{"self": true, "responseStatus": "declined"}]
        }));
        assert_eq!(row.self_response, Some(SelfResponse::Declined));
        // One entry that is someone else's: no answer of the user's.
        let other = upsert(json!({
            "id": "e2", "start": {"dateTime": "2026-10-05T09:00:00Z"},
            "attendees": [{"email": "a@example.com", "responseStatus": "accepted"}]
        }));
        assert_eq!(other.self_response, None);
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

    /// A dot segment is no id: `..` in a URL path climbs out of the event.
    #[test]
    fn an_id_of_dots_or_nothing_is_no_id() {
        for bad in ["", ".", "..", "\u{200B}"] {
            assert_eq!(bounded_id(Some(bad), ID_CAP), None, "{bad:?}");
            assert_eq!(
                read_event(
                    &json!({"id": bad, "start": {"dateTime": "2026-10-05T09:00:00Z"}}),
                    None,
                    window(now())
                ),
                None,
                "{bad:?}"
            );
        }
        for good in ["...", ".a", "a..b", "abc"] {
            assert_eq!(bounded_id(Some(good), ID_CAP).as_deref(), Some(good));
        }
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

    #[test]
    fn an_event_reads_its_colour_organizer_flag_and_description() {
        let row = upsert(json!({
            "id": "e", "start": {"dateTime": "2026-10-05T09:00:00Z"},
            "colorId": "11",
            "organizer": {"email": "team@group.calendar.google.com", "self": true},
            "description": "<p>Agenda</p><p>Join https://meet.example/abc?pwd=1</p>"
        }));
        assert_eq!(row.color_id.as_deref(), Some("11"));
        assert!(row.organizer_self);
        assert_eq!(
            row.description.as_deref(),
            Some("Agenda\n\nJoin meet.example/abc"),
            "a paragraph each, the meeting link without its query"
        );
        // An id the palette does not know, no organizer flag, a long
        // description cut at its cap.
        let other = upsert(json!({
            "id": "e2", "start": {"dateTime": "2026-10-05T09:00:00Z"},
            "colorId": "99",
            "organizer": {"email": "a@example.com"},
            "description": "x".repeat(5000)
        }));
        assert_eq!(other.color_id, None);
        assert!(!other.organizer_self);
        assert_eq!(
            other.description.map(|d| d.chars().count()),
            Some(ghost::DESCRIPTION_CAP)
        );
    }

    #[test]
    fn a_written_event_is_read_wherever_it_falls() {
        let far =
            json!({"id": "far", "start": {"date": "2031-01-01"}, "end": {"date": "2031-01-03"}});
        assert!(matches!(
            read_event(&far, None, window(now())),
            Some(EventItem::Remove(_))
        ));
        let row = read_written_event(&far, None).unwrap();
        assert_eq!(row.end.unwrap().to_string(), "2031-01-02", "inclusive");
        assert_eq!(
            read_written_event(&json!({"id": "c", "status": "cancelled"}), None),
            None
        );
    }

    #[test]
    fn colours_are_googles_hex_or_its_palette() {
        assert_eq!(hex_color("#0088AA").as_deref(), Some("#0088aa"));
        for bad in ["0088aa", "#08a", "#0088aaff", "#00g8aa", "red"] {
            assert_eq!(hex_color(bad), None, "{bad}");
        }
        assert_eq!(event_palette_color("1"), Some("#a4bdfc"));
        assert_eq!(event_palette_color("11"), Some("#dc2127"));
        for bad in ["0", "12", "-1", "x", ""] {
            assert_eq!(event_palette_color(bad), None, "{bad}");
        }
        let (meta, _) = read_calendar_entry(&json!({
            "id": "a", "selected": true, "backgroundColor": "#9FE1E7",
            "colorId": "1", "accessRole": "writer"
        }))
        .unwrap();
        assert_eq!(
            meta.color.as_deref(),
            Some("#9fe1e7"),
            "backgroundColor wins"
        );
        assert_eq!(meta.access_role, Some(AccessRole::Writer));
        let (meta, _) =
            read_calendar_entry(&json!({"id": "b", "colorId": "16", "accessRole": "nope"}))
                .unwrap();
        assert_eq!(
            meta.color.as_deref(),
            Some("#4986e7"),
            "the palette fallback"
        );
        assert_eq!(meta.access_role, None);
    }

    #[test]
    fn only_owner_and_writer_calendars_are_writable() {
        for (raw, writable) in [
            ("owner", true),
            ("writer", true),
            ("writerWithoutPrivateAccess", false),
            ("reader", false),
            ("freeBusyReader", false),
        ] {
            let role = AccessRole::parse(raw).unwrap();
            assert_eq!(role.as_str(), raw);
            assert_eq!(role.writable(), writable, "{raw}");
        }
    }

    #[test]
    fn an_events_guests_are_read_laundered_and_bounded() {
        let start = now() + chrono::Duration::days(1);
        let item = |attendees: Value, omitted: bool| {
            let mut item = json!({
                "id": "e",
                "start": {"dateTime": start.to_rfc3339()},
                "end": {"dateTime": (start + chrono::Duration::hours(1)).to_rfc3339()},
                "attendees": attendees,
            });
            if omitted {
                item["attendeesOmitted"] = json!(true);
            }
            item
        };
        let row = upsert(item(
            json!([
                {"email": "me@example.com", "self": true, "organizer": true,
                 "responseStatus": "tentative"},
                {"email": " Sam@Example.com ", "displayName": "Sam\u{202E} \u{200B}Lee",
                 "responseStatus": "accepted", "comment": "never read"},
                {"email": "pat@example.com", "responseStatus": "declined"},
                {"email": "kim@example.com", "responseStatus": "needsAction"},
                {"email": "odd@example.com", "responseStatus": "somethingNew"},
                {"email": "bare@example.com"},
            ]),
            false,
        ));
        let read: Vec<_> = row
            .guests
            .iter()
            .map(|g| {
                (
                    g.email.as_str(),
                    g.name.as_deref(),
                    g.response.as_str(),
                    g.organizer,
                    g.is_self,
                )
            })
            .collect();
        assert_eq!(
            read,
            [
                ("me@example.com", None, "tentative", true, true),
                ("Sam@Example.com", Some("Sam Lee"), "accepted", false, false),
                ("pat@example.com", None, "declined", false, false),
                ("kim@example.com", None, "pending", false, false),
                ("odd@example.com", None, "pending", false, false),
                ("bare@example.com", None, "pending", false, false),
            ]
        );
        assert!(!row.guests_truncated);
        assert_eq!(row.other_guests(), 5);
        // The user's own answer is still read from their entry.
        assert_eq!(row.self_response, Some(SelfResponse::Tentative));
        // No attendees: none, not truncated.
        let none = upsert(json!({"id": "e", "start": {"dateTime": start.to_rfc3339()}}));
        assert!(none.guests.is_empty() && !none.guests_truncated);
        // Google says it left some out.
        let omitted = upsert(item(
            json!([{"email": "me@example.com", "self": true, "responseStatus": "accepted"}]),
            true,
        ));
        assert_eq!(omitted.guests.len(), 1);
        assert!(omitted.guests_truncated);
        assert_eq!(omitted.other_guests(), 0);
        // An entry that cannot be read (no address, or not one) means the
        // list held here is not the whole list.
        for bad in [
            json!({"displayName": "No address"}),
            json!({"email": "not an address"}),
            json!({"email": "x".repeat(400) + "@example.com"}),
        ] {
            let row = upsert(item(json!([{"email": "a@example.com"}, bad]), false));
            assert_eq!(row.guests.len(), 1);
            assert!(row.guests_truncated);
        }
        // A list past the cap that Google sent anyway: cut to the cap, marked,
        // and the user's own entry kept though it came last.
        let mut crowd: Vec<Value> = (0..MAX_GUESTS + 5)
            .map(|i| json!({"email": format!("g{i}@example.com")}))
            .collect();
        crowd.push(json!({"email": "me@example.com", "self": true,
                          "responseStatus": "accepted"}));
        let row = upsert(item(json!(crowd), false));
        assert_eq!(row.guests.len(), MAX_GUESTS);
        assert!(row.guests_truncated);
        assert!(row.guests.iter().any(|g| g.is_self));
        assert_eq!(row.self_response, Some(SelfResponse::Accepted));
        // Exactly the cap is whole.
        let full: Vec<Value> = (0..MAX_GUESTS)
            .map(|i| json!({"email": format!("g{i}@example.com")}))
            .collect();
        let row = upsert(item(json!(full), false));
        assert_eq!(row.guests.len(), MAX_GUESTS);
        assert!(!row.guests_truncated);
    }

    /// THE GUEST GUARD and THE INVITE GUARD, every arm.
    #[test]
    fn a_guest_list_is_editable_only_on_the_users_own_whole_organized_event() {
        let event = |primary: bool,
                     access: Option<AccessRole>,
                     organizer: Option<&str>,
                     own: bool,
                     truncated: bool| SyncedEvent {
            calendar_id: "c".into(),
            calendar_name: None,
            calendar_color: None,
            calendar_access: access,
            calendar_primary: primary,
            row: EventRow {
                organizer_email: organizer.map(str::to_string),
                organizer_self: own,
                guests_truncated: truncated,
                ..row("2026-10-05T09:00:00Z")
            },
        };
        let me = "Me@Example.com";
        let owner = Some(AccessRole::Owner);
        let writer = Some(AccessRole::Writer);
        assert!(event(true, owner, None, true, false).guest_editable(me));
        assert!(
            event(true, owner, Some("me@example.com"), false, false).guest_editable(me),
            "organized by the mailbox's own address"
        );
        assert!(
            event(false, owner, None, true, false).guest_editable(me),
            "a calendar the user owns that is not the primary one"
        );
        // Truncated: never, whatever else holds.
        assert!(!event(true, owner, None, true, true).guest_editable(me));
        assert!(event(true, owner, None, true, true).editable(me));
        // Not the organizer: an event that names none is editable, and still
        // not one to invite people to.
        let orphan = event(true, owner, None, false, false);
        assert!(orphan.editable(me) && !orphan.guest_editable(me));
        // Someone else's.
        assert!(!event(true, owner, Some("boss@x.com"), false, false).guest_editable(me));
        // A calendar the user writes to but does not own.
        let team = event(false, writer, None, true, false);
        assert!(team.editable(me) && !team.guest_editable(me));
        // A calendar the user cannot write to at all.
        assert!(!event(false, Some(AccessRole::Reader), None, true, false).guest_editable(me));
        assert!(!event(false, None, None, true, false).guest_editable(me));

        assert!(may_invite(true, None), "the primary calendar is the user's");
        assert!(may_invite(true, writer));
        assert!(may_invite(false, owner));
        for role in [
            None,
            writer,
            Some(AccessRole::WriterWithoutPrivateAccess),
            Some(AccessRole::Reader),
            Some(AccessRole::FreeBusyReader),
        ] {
            assert!(!may_invite(false, role), "{role:?}");
        }
    }

    /// THE EDIT GUARD, every arm.
    #[test]
    fn an_event_is_editable_on_a_writable_calendar_when_it_is_the_users_own() {
        let event = |access: Option<AccessRole>, organizer: Option<&str>, own: bool| SyncedEvent {
            calendar_id: PRIMARY.into(),
            calendar_name: None,
            calendar_color: Some("#123456".into()),
            calendar_access: access,
            calendar_primary: true,
            row: EventRow {
                organizer_email: organizer.map(str::to_string),
                organizer_self: own,
                ..row("2026-10-05T09:00:00Z")
            },
        };
        let me = "Me@Example.com";
        let owner = Some(AccessRole::Owner);
        assert!(event(owner, None, false).editable(me), "no organizer");
        assert!(
            event(owner, Some("x@y.com"), true).editable(me),
            "organizer.self"
        );
        assert!(
            event(owner, Some("me@example.COM"), false).editable(me),
            "the mailbox's own address, any case"
        );
        assert!(
            !event(owner, Some("boss@example.com"), false).editable(me),
            "someone else's invitation"
        );
        assert!(event(Some(AccessRole::Writer), None, false).editable(me));
        for access in [
            None,
            Some(AccessRole::Reader),
            Some(AccessRole::FreeBusyReader),
            Some(AccessRole::WriterWithoutPrivateAccess),
        ] {
            assert!(!event(access, None, true).editable(me), "{access:?}");
        }
        // A row from Google: the reviewer's case. An invitation on the
        // user's own primary calendar whose organizer address does not read,
        // and whose guest list names that organizer.
        let synced = |item: Value| SyncedEvent {
            row: upsert(item),
            ..event(owner, None, false)
        };
        let invitation = synced(json!({
            "id": "e", "start": {"dateTime": "2026-10-05T09:00:00Z"},
            "organizer": {"email": "boss at example.com"},
            "attendees": [
                {"email": "boss@example.com", "organizer": true},
                {"email": "me@example.com", "self": true}
            ]
        }));
        assert!(invitation.row.organizer_unreadable);
        assert_eq!(invitation.row.organizer_email, None);
        assert!(
            !invitation.editable(me),
            "an organizer that does not read is somebody else"
        );
        // The unreadable organizer alone is enough, with no guest list.
        let bare = synced(json!({
            "id": "e", "start": {"dateTime": "2026-10-05T09:00:00Z"},
            "organizer": {"displayName": "Boss"}
        }));
        assert!(bare.row.organizer_unreadable && !bare.editable(me));
        // And a guest list naming another organizer is enough on its own,
        // whatever the organizer field says.
        let mut named = event(owner, None, false);
        named.row.guests = invitation.row.guests.clone();
        assert!(!named.editable(me), "a guest marked organizer, not self");
        // No organizer object at all is still the user's own, and so is an
        // organizer this calendar is, address or not.
        assert!(
            synced(json!({"id": "e", "start": {"dateTime": "2026-10-05T09:00:00Z"}})).editable(me)
        );
        let own = synced(json!({
            "id": "e", "start": {"dateTime": "2026-10-05T09:00:00Z"},
            "organizer": {"self": true},
            "attendees": [{"email": "me@example.com", "self": true, "organizer": true}]
        }));
        assert!(!own.row.organizer_unreadable && own.editable(me));

        // Colour: the event's own over the calendar's.
        let mut colored = event(owner, None, true);
        assert_eq!(colored.color().as_deref(), Some("#123456"));
        colored.row.color_id = Some("2".into());
        assert_eq!(colored.color().as_deref(), Some("#7ae7bf"));
    }
}
