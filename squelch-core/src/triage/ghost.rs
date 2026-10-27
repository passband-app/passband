//! Calendar ghosts, the pure half: laundering a triage `event` record, deriving
//! the sender scope and the scoped identity keys that let later mail find a
//! ghost, and resolving an end time. Storage builds on this; nothing here
//! touches a row.
//!
//! The model's words are UNTRUSTED text lifted from email, as in `order_link`.
//! A bad value is dropped or clamped HERE rather than failing decision
//! validation, so a wrong guess about an event can never cost a message its
//! triage.
//!
//! AN ESTIMATE IS NEVER A FACT. `end` on a [`GhostProposal`] is only ever an end
//! the mail stated; the model's guess stays in `estimate`, and
//! [`effective_end`] reports which tier produced the end it returns.
use super::decision::{
    EventChange, EventCommitment, EventLikelihood, EventRecordKind, RecordProposal, SupportedTime,
};
use super::order_link::plain;
use super::text::{truncate_chars, truncate_trimmed};
use chrono::{DateTime, Duration, FixedOffset, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use std::fmt;
use std::ops::RangeInclusive;

/// Cap on the free text the model copies from mail: title and location.
pub const TEXT_CAP: usize = 200;
/// Cap on the model's own explanations, `basis` and `reason`. The timeline
/// shows them as the reason for a guess, so they stay a phrase long.
pub const NOTE_CAP: usize = 80;
/// A URL path segment longer than this that mixes letters and digits is a
/// token (a magic link, a cancellation capability), not a page.
const TOKEN_SEGMENT: usize = 24;
/// Any estimated duration, in minutes: a quarter hour to three days. Three
/// days covers a festival; anything longer is a model error, not an event.
pub const ESTIMATE_MINUTES: RangeInclusive<u32> = 15..=4320;
const TIMEZONE_CAP: usize = 64;
const VENUE_CAP: usize = 120;
/// Identity refs kept per record. A real event has one or two (a booking
/// number, a UID, a page); a record carrying dozens is a digest or an attack,
/// and every ref is one more ghost the record could be matched against.
pub const REFS_PER_RECORD: usize = 5;
/// Event records kept per message: the first this many laundered. A real mail
/// describes a handful (an itinerary's legs); more is a digest or an attack,
/// and each record may mint a ghost.
pub const EVENTS_PER_MESSAGE: usize = 20;
/// Shorter than 5 is a fragment ("1234", "A1B2") that repeats across
/// unrelated events; longer than 200 is a body excerpt, not a reference.
const REF_LEN: RangeInclusive<usize> = 5..=200;
/// An all-digit ref shorter than this is an order line or a year, not a
/// booking number.
const DIGITS_ONLY_MIN: usize = 6;

/// Words a model prints around a reference ("Confirmation #", "OpenTable
/// confirmation", "Flight UA123"). Dropped wherever they stand, so the same
/// booking number worded two ways by two mails is one key.
const LABEL_WORDS: &[&str] = &[
    "confirmation",
    "conf",
    "booking",
    "reservation",
    "order",
    "ref",
    "reference",
    "number",
    "no",
    "nr",
    "code",
    "ticket",
    "record",
    "locator",
    "pnr",
    "itinerary",
    "flight",
    "event",
    "id",
    "uid",
];
/// Words that name a PLACE inside a venue ("Seat 12A", "Gate 4", "Table for
/// 2"). The number after one is shared by every event held there, so it and
/// the number are dropped from a ref.
const POSITION_WORDS: &[&str] = &[
    "seat", "row", "gate", "table", "sec", "section", "terminal", "room", "party", "door", "floor",
    "lot", "zone",
];
/// Capitalised words a model might hand back as a "reference". A record
/// locator is all capitals too, so these are the ones a status line shouts.
const STOP_WORDS: &[&str] = &[
    "confirmed",
    "confirmation",
    "cancelled",
    "canceled",
    "booking",
    "booked",
    "reservation",
    "reserved",
    "ticket",
    "tickets",
    "order",
    "event",
    "invite",
    "invitation",
    "rsvp",
    "none",
    "null",
    "unknown",
    "pending",
];

/// A start or end, at the precision the mail gave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventTime {
    /// A date-only fact. It never gains a clock time, so it never gets an
    /// estimated end.
    Date(NaiveDate),
    At(DateTime<FixedOffset>),
}
impl EventTime {
    /// RFC3339 or `YYYY-MM-DD`: the stored form (see the `Display` impl), and
    /// the same test decision validation applies to other times. Anything else
    /// is None. A time read from a decision goes through
    /// [`EventTime::resolve`] instead, which also takes a zoned local time.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if let Ok(at) = DateTime::parse_from_rfc3339(raw) {
            return Some(Self::At(at));
        }
        NaiveDate::parse_from_str(raw, "%Y-%m-%d")
            .ok()
            .map(Self::Date)
    }
    /// A time as the model writes it into a decision: [`EventTime::parse`]'s
    /// two shapes, or a NAIVE local date-time (`YYYY-MM-DDTHH:MM`, with
    /// optional seconds and fraction) read in `timezone`. The model writes the
    /// naive shape for most event times ("21:30 in Asia/Tokyo"), and reading
    /// only the first two dropped them, leaving a flight with no times at all.
    ///
    /// The zone must be an IANA PLACE zone ([`iana_zone`]'s test): an
    /// abbreviation names several offsets, so a naive time with no usable zone
    /// is None, as before. A zone beside an RFC3339 value is ignored, because
    /// the offset the mail gave is the fact. The two clock changes a year are
    /// settled the same way every time: a local time that happens twice (the
    /// autumn fold) is the earlier instant, and one that never happens (the
    /// spring gap) is read one hour later, which is the wall clock a person
    /// would see at that moment.
    pub fn resolve(value: &str, timezone: Option<&str>) -> Option<Self> {
        if let Some(time) = Self::parse(value) {
            return Some(time);
        }
        let raw = value.trim();
        let local = NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f")
            .or_else(|_| NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M"))
            .ok()?;
        let zone = place_tz(timezone?)?;
        let at = zone.from_local_datetime(&local).earliest().or_else(|| {
            zone.from_local_datetime(&(local + Duration::hours(1)))
                .earliest()
        })?;
        Some(Self::At(at.fixed_offset()))
    }
    pub fn has_clock(&self) -> bool {
        matches!(self, Self::At(_))
    }
    /// The latest instant this can mean. An instant is itself; a date is the
    /// moment that date ends everywhere on Earth, 00:00 of the next day at
    /// UTC-12 (12:00Z), because the mail gave no zone to end it in. None only
    /// at the end of the calendar.
    pub fn latest_instant(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::At(at) => Some(at.with_timezone(&Utc)),
            Self::Date(day) => day
                .succ_opt()?
                .and_hms_opt(12, 0, 0)
                .map(|noon| noon.and_utc()),
        }
    }
    /// Is this strictly after `start`? Mixed precision is never "after": a
    /// date-only start pairs only with a date-only end, because comparing a
    /// day to an instant needs a zone the mail did not give.
    pub fn is_after(&self, start: &EventTime) -> bool {
        match (start, self) {
            (Self::At(s), Self::At(e)) => e > s,
            (Self::Date(s), Self::Date(e)) => e > s,
            _ => false,
        }
    }
}
/// The stored form, which [`EventTime::parse`] reads back: `YYYY-MM-DD` or
/// RFC3339 with the mail's own offset.
impl fmt::Display for EventTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Date(date) => write!(f, "{}", date.format("%Y-%m-%d")),
            Self::At(at) => f.write_str(&at.to_rfc3339()),
        }
    }
}

/// The model's duration guess, laundered: minutes within [`ESTIMATE_MINUTES`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhostEstimate {
    pub minutes: u32,
    pub basis: String,
}

/// One `event` record after laundering. Every field is safe to store.
#[derive(Debug, Clone, PartialEq)]
pub struct GhostProposal {
    pub title: String,
    pub start: Option<EventTime>,
    pub start_timezone: Option<String>,
    /// STATED end only, and only when it is after `start`.
    pub end: Option<EventTime>,
    /// Only when there is no stated end and the start has a clock time.
    pub estimate: Option<GhostEstimate>,
    pub location: Option<String>,
    pub kind: EventRecordKind,
    pub commitment: EventCommitment,
    pub change: EventChange,
    /// Normalized (see [`normalize_ref`]) and deduplicated.
    pub identity_refs: Vec<String>,
    pub likelihood: Option<EventLikelihood>,
}

/// Launder one record. None for anything but an `event`, and for an event with
/// no usable title: a ghost nobody can name is not shown.
/// An end as [`launder`] reads it: in its own zone, or the start's when it
/// names none.
fn resolve_end(end: &SupportedTime, start_zone: Option<&str>) -> Option<EventTime> {
    let zone = end
        .timezone
        .as_deref()
        .filter(|z| !z.trim().is_empty())
        .or(start_zone);
    EventTime::resolve(&end.value, zone)
}

pub fn launder(record: &RecordProposal) -> Option<GhostProposal> {
    let RecordProposal::Event {
        title,
        start,
        location,
        end,
        duration_estimate,
        event_kind,
        commitment,
        change,
        identity_refs,
        likelihood,
        ..
    } = record
    else {
        return None;
    };
    let title = clean_title(title)?;
    let start_zone = start.as_ref().and_then(|t| t.timezone.as_deref());
    let start_time = start
        .as_ref()
        .and_then(|t| EventTime::resolve(&t.value, start_zone));
    let start_timezone = start_zone
        .filter(|_| start_time.is_some())
        .and_then(iana_zone);
    // The end is read in its OWN zone, since a flight lands somewhere else,
    // and in the start's when it names none. A zone it does name that is no
    // place does not fall back: it said where the end is, just unusably.
    // An end with no start to be after is meaningless, so it goes too.
    let end = end
        .as_ref()
        .and_then(|t| resolve_end(t, start_zone))
        .filter(|e| start_time.is_some_and(|s| e.is_after(&s)));
    let estimate = match (start_time, end) {
        (Some(EventTime::At(_)), None) => duration_estimate.as_ref().and_then(|d| {
            // Zero or negative is "no idea", not "a quarter hour": dropping it
            // lets the kind default answer instead.
            let minutes = u32::try_from(d.minutes).ok().filter(|m| *m > 0)?;
            Some(GhostEstimate {
                minutes: minutes.clamp(*ESTIMATE_MINUTES.start(), *ESTIMATE_MINUTES.end()),
                basis: note(&d.basis),
            })
        }),
        _ => None,
    };
    let mut refs: Vec<String> = Vec::new();
    for normalized in identity_refs.iter().filter_map(|r| normalize_ref(r)) {
        if refs.len() == REFS_PER_RECORD {
            break;
        }
        if !refs.contains(&normalized) {
            refs.push(normalized);
        }
    }
    Some(GhostProposal {
        title,
        start: start_time,
        start_timezone,
        end,
        estimate,
        location: location.as_deref().and_then(launder_location),
        kind: *event_kind,
        commitment: *commitment,
        change: *change,
        identity_refs: refs,
        likelihood: likelihood.as_ref().map(|l| EventLikelihood {
            band: l.band,
            reason: note(&l.reason),
        }),
    })
}

/// Every laundered `event` record of a decision, in order, at most
/// [`EVENTS_PER_MESSAGE`]. A record's index in THIS list is its `position`,
/// the second half of its identity in `ghost_event_sources`: storage assigns
/// and reloads records by it, so both sides must count the same way, and they
/// do by both calling this.
pub fn launder_events(records: &[RecordProposal]) -> Vec<GhostProposal> {
    laundered_records(records)
        .into_iter()
        .map(|(_, proposal)| proposal)
        .collect()
}

/// [`launder_events`] with each proposal's raw record beside it, counted the
/// same way (it is the one definition), so a stored `position` finds the
/// record the model wrote as well as what laundering kept of it.
pub fn laundered_records(records: &[RecordProposal]) -> Vec<(&RecordProposal, GhostProposal)> {
    records
        .iter()
        .filter_map(|record| launder(record).map(|proposal| (record, proposal)))
        .take(EVENTS_PER_MESSAGE)
        .collect()
}

/// The longest stretch a record's own duration guess is believed for, when
/// scheduling the past-event re-check: a month, against the calendar's three
/// days ([`ESTIMATE_MINUTES`]), because a ten-day trip drawn as three is a
/// picture, and asked about on day four it is a wasted model call.
const RECORD_ESTIMATE_CAP_MINUTES: i64 = 30 * 24 * 60;

/// The latest instant a RAW event record mentions, for the past-event re-check:
/// the latest of its start, its end and the model's own duration guess from a
/// timed start (raw, up to [`RECORD_ESTIMATE_CAP_MINUTES`]), each at whatever precision the
/// model gave ([`EventTime::latest_instant`]), the end in its own zone or the
/// start's. Read from the record and not the laundered proposal, because
/// laundering drops an end at another precision than the start, and "check-in
/// 15:00, check-out the 16th" must count the 16th.
///
/// With no end after the start, travel ends a day after it starts at the
/// least (a flight lands within a day of leaving), and a RESERVATION with no
/// guess either is None: a stay with no checkout cannot be dated, and the
/// kind's two-hour default would end a hotel on its first night (the
/// re-check then asks a day after check-in, and the model says whether the
/// stay is over). None too when the record has no start that resolves.
pub fn record_latest(record: &RecordProposal) -> Option<DateTime<Utc>> {
    let RecordProposal::Event {
        start,
        end,
        duration_estimate,
        event_kind,
        ..
    } = record
    else {
        return None;
    };
    let start = start.as_ref()?;
    let start_zone = start.timezone.as_deref();
    let start = EventTime::resolve(&start.value, start_zone)?;
    let first = start.latest_instant()?;
    let stated = end
        .as_ref()
        .and_then(|end| resolve_end(end, start_zone))
        .and_then(|end| end.latest_instant())
        .filter(|end| *end > first);
    // A guess is minutes from a clock time, as laundering reads it; a date
    // has none to count from.
    let guessed = duration_estimate
        .as_ref()
        .filter(|d| d.minutes > 0 && start.has_clock())
        .and_then(|d| {
            first.checked_add_signed(Duration::minutes(
                d.minutes.min(RECORD_ESTIMATE_CAP_MINUTES),
            ))
        });
    let last = stated.max(guessed);
    match (stated, event_kind) {
        (None, EventRecordKind::Travel) => last.max(first.checked_add_signed(Duration::hours(24))),
        (None, EventRecordKind::Reservation) => guessed,
        _ => Some(last.unwrap_or(first)),
    }
}

/// Who a record speaks for: the sender's [`organisation_domain`], or the full
/// lowercased address where the domain is shared by strangers (freemail, a
/// consumer ISP, a relay host, a shared tenant). None when the message is not AUTHENTICATED (`auth_pass` is
/// anything but `Some(true)`, unknown included) or the address has no
/// registrable domain: such a record speaks for nobody, so it never matches
/// another ghost and never commits one.
///
/// Authentication here (`messages.auth_pass`) vouches for the From header's
/// DOMAIN, never its local part, which is why a domain anyone can sign up on
/// cannot be a scope by itself.
pub fn sender_scope(from_addr: &str, auth_pass: Option<bool>) -> Option<String> {
    if auth_pass != Some(true) {
        return None;
    }
    let address = from_addr.trim().to_lowercase();
    if !address.contains('@') {
        return None;
    }
    // A shared domain still has to be a real one for its address to count.
    organisation_domain(&address).or_else(|| psl_registrable_domain(&address).map(|_| address))
}

/// Registrable domains where one domain is many unrelated senders, beyond
/// freemail: hosts whose customers mail From the host's own domain. The
/// Public Suffix List (as compiled into `psl` 2.1) does NOT list
/// `onmicrosoft.com`, so every Microsoft 365 tenant's default domain
/// (`contoso.onmicrosoft.com`) has the registrable domain `onmicrosoft.com`
/// and needs this entry; a host the list does carry in its private section
/// (`x.blogspot.com`) is already one registrable domain per customer.
const SHARED_TENANT_DOMAINS: &[&str] = &["onmicrosoft.com", "googlegroups.com"];

/// Is this registrable domain one that strangers share, so that only a full
/// address identifies a sender? The freemail list (`receipt_match`'s, so the
/// bill matcher and the calendar never disagree) or a shared tenant host.
fn is_shared_domain(registrable: &str) -> bool {
    super::receipt_match::is_freemail_domain(registrable)
        || SHARED_TENANT_DOMAINS.contains(&registrable)
}

/// The registrable domain of an address (or a bare domain) under the full
/// Public Suffix List, ICANN and private sections both, lowercased: one label
/// above the longest matching public suffix. None when there is no domain, or
/// the domain is itself a public suffix (`a@com.mx`, `a@onmicrosoft.com`).
/// A TLD the list does not know counts as a one-label suffix.
fn psl_registrable_domain(address: &str) -> Option<String> {
    let domain = match address.rsplit_once('@') {
        Some((_, domain)) => domain,
        None => address,
    };
    let domain = domain.trim().trim_end_matches('.').to_lowercase();
    if domain.is_empty() || domain.split('.').any(str::is_empty) {
        return None;
    }
    psl::domain_str(&domain).map(str::to_string)
}

/// The domain that identifies ONE organisation behind an address: its
/// registrable domain under the full Public Suffix List (private section
/// included, so `hotel.com.mx` and `evil.com.mx` differ, and so do two
/// tenants of `onmicrosoft.com`), lowercased.
///
/// None for an address whose domain is FREEMAIL (`gmail.com`, `hotmail.co.uk`,
/// `gmx.de`, `comcast.net`: see `receipt_match::is_freemail_domain`), whose
/// host is a freemail host under an organisation's domain
/// (`privaterelay.appleid.com`: `receipt_match::is_freemail_host`), or a
/// SHARED TENANT host (`googlegroups.com`), where the domain says nothing
/// about who sent the mail and only the full address identifies a sender.
/// None too when there is no registrable domain at all (an empty or
/// one-label domain, or a domain that is itself a public suffix). Takes an
/// address or a bare domain. Two addresses are the same organisation only
/// when this is Some and equal for both.
///
/// No list of shared domains is ever complete, so only the REF rule leans on
/// this: a ref is something the sender must know. Matching by thread and the
/// invitation binding (`zones::invitation_is_bound`) require the identical
/// address instead.
pub(crate) fn organisation_domain(address: &str) -> Option<String> {
    if super::receipt_match::is_freemail_host(address) {
        return None;
    }
    psl_registrable_domain(address).filter(|d| !is_shared_domain(d))
}

/// The keys a later message can find this record's ghost by:
/// `ref:<scope>:<normalized>` for each identity ref. A ref only means
/// something within the sender that issued it, so a stranger quoting the
/// user's booking number from another domain reaches nothing. An
/// unauthenticated record (no scope) has no keys at all.
pub fn identity_keys(event: &GhostProposal, scope: Option<&str>) -> Vec<String> {
    let Some(scope) = scope else {
        return Vec::new();
    };
    event
        .identity_refs
        .iter()
        .map(|r| format!("ref:{scope}:{r}"))
        .collect()
}

/// One identity ref as a key, or None when it cannot safely identify an event.
///
/// A ref that is or contains a URL is that URL alone (see [`url_ref`]): the
/// label around it identifies nothing, and folding it as text would keep the
/// query's token as alphanumerics. Anything else goes through [`text_ref`].
/// Either way a key shorter than 5 characters is dropped.
///
/// Which refs survive changes only keys, never which RECORDS laundering
/// keeps, so it cannot shift a stored record's position.
pub fn normalize_ref(raw: &str) -> Option<String> {
    let raw = plain(raw);
    let raw = raw.trim();
    let key = if let Some(url) = raw.split(' ').find_map(url_core) {
        url_ref(url)?
    } else {
        text_ref(raw)?
    };
    REF_LEN.contains(&key.chars().count()).then_some(key)
}

/// How a kept token of a text ref meets the kept token before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Join {
    /// Nothing, or exactly one `-`, `/` or `.`: the same code ("ABC-1234-XYZ").
    Tight,
    /// Whitespace only: two words, joined only by the carrier-code allowance.
    Space,
    /// Any other separator, or a dropped clock time or position between.
    Broken,
}

/// A ref written as text, as the key two mails worded differently agree on:
/// ONE code run.
///
/// Tokens are the alphanumeric runs. Label words go wherever they stand
/// (their separators merge, so "UA flight 123" is "UA 123"); a clock time
/// ("8pm", "10:30am", "19:00") and a position in a venue with its number
/// ("Seat 12A", "gate4") go too and break any run across them. A CODE token
/// has a digit or is all capitals of at most 6 letters. A run is a maximal
/// sequence of code tokens joined by `-`, `/`, `.` or nothing, never by
/// whitespace, except that a 1 to 3 letter all-capitals token joins a digit
/// token one space after it ("UA 123", "AB 12345"). Then:
///
/// - The key is the first run that has a digit, joined and lowercased:
///   "ABC-1234-XYZ" and "ABC-1234-QRS" differ, "Booking ABC-1234-XYZ" and
///   "ABC-1234-XYZ" agree, and "OpenTable confirmation 123456789" is
///   `123456789` (the brand is not a code). Only one run, so a count printed
///   after a booking number ("(2 tickets)") is not part of it.
/// - With no digit anywhere, only a single all-capitals token that is not a
///   status word is a record locator ("QXJZKM"); anything else made of
///   letters is prose.
///
/// Dropped as identifying nothing: an all-digit key under 6 digits (an order
/// line, a year) and an 8-digit key that is a valid `YYYYMMDD` date.
fn text_ref(raw: &str) -> Option<String> {
    let has_digit = |t: &str| t.chars().any(char::is_numeric);
    let capitals = |t: &str, most: usize| {
        t.chars().count() <= most && t.chars().all(|c| c.is_alphabetic() && c.is_uppercase())
    };
    let words: Vec<&str> = raw.split_whitespace().collect();
    let mut tokens: Vec<(&str, Join)> = Vec::new();
    // Separator characters since the last kept token, and whether a dropped
    // clock time or position stands between.
    let mut separator = String::new();
    let mut broken = false;
    let mut after_position = false;
    for (w, word) in words.iter().enumerate() {
        if w > 0 {
            separator.push(' ');
        }
        let clock = clock_time(word)
            || (meridiem(word) && w > 0 && hour(words[w - 1]))
            || (hour(word) && words.get(w + 1).is_some_and(|next| meridiem(next)));
        if clock {
            broken = true;
            continue;
        }
        for piece in alphanumeric_pieces(word) {
            let token = match piece {
                Err(punctuation) => {
                    separator.push_str(punctuation);
                    continue;
                }
                Ok(token) => token,
            };
            let lower = token.to_lowercase();
            if LABEL_WORDS.contains(&lower.as_str()) {
                continue;
            }
            if POSITION_WORDS.contains(&lower.as_str()) {
                after_position = true;
                broken = true;
                continue;
            }
            let glued_position = POSITION_WORDS.iter().any(|w| {
                lower
                    .strip_prefix(w)
                    .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
            });
            if (std::mem::take(&mut after_position) && has_digit(token)) || glued_position {
                broken = true;
                continue;
            }
            let join = if broken || tokens.is_empty() {
                Join::Broken
            } else if matches!(separator.as_str(), "" | "-" | "/" | ".") {
                Join::Tight
            } else if separator.chars().all(char::is_whitespace) {
                Join::Space
            } else {
                Join::Broken
            };
            tokens.push((token, join));
            separator.clear();
            broken = false;
        }
    }
    let is_code = |t: &str| has_digit(t) || capitals(t, 6);
    let mut runs: Vec<Vec<&str>> = Vec::new();
    let mut run: Vec<&str> = Vec::new();
    for &(token, join) in &tokens {
        let joins = run.last().is_some_and(|last| {
            is_code(token)
                && match join {
                    Join::Tight => true,
                    Join::Space => capitals(last, 3) && has_digit(token),
                    Join::Broken => false,
                }
        });
        if !joins && !run.is_empty() {
            runs.push(std::mem::take(&mut run));
        }
        if is_code(token) {
            run.push(token);
        }
    }
    if !run.is_empty() {
        runs.push(run);
    }
    let key = match runs.iter().find(|r| r.iter().any(|t| has_digit(t))) {
        Some(run) => run.iter().map(|t| t.to_lowercase()).collect(),
        None => {
            let [(only, _)] = tokens.as_slice() else {
                return None;
            };
            let lower = only.to_lowercase();
            if !only.chars().all(char::is_uppercase) || STOP_WORDS.contains(&lower.as_str()) {
                return None;
            }
            lower
        }
    };
    if key.chars().all(char::is_numeric) {
        let digits = key.chars().count();
        if digits < DIGITS_ONLY_MIN
            || (digits == 8 && NaiveDate::parse_from_str(&key, "%Y%m%d").is_ok())
        {
            return None;
        }
    }
    Some(key)
}

/// A word's alphanumeric runs (`Ok`) and the punctuation between them
/// (`Err`), in order.
fn alphanumeric_pieces(word: &str) -> Vec<Result<&str, &str>> {
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut alphanumeric = None;
    for (i, c) in word.char_indices() {
        let now = c.is_alphanumeric();
        if alphanumeric.is_some_and(|was| was != now) {
            let piece = &word[start..i];
            pieces.push(if now { Err(piece) } else { Ok(piece) });
            start = i;
        }
        alphanumeric = Some(now);
    }
    if let Some(was) = alphanumeric {
        let piece = &word[start..];
        pieces.push(if was { Ok(piece) } else { Err(piece) });
    }
    pieces
}

/// A word without the punctuation prose puts around it, ASCII-lowercased.
fn bare_word(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric())
        .to_ascii_lowercase()
}

/// "am" or "pm", with or without the dots.
fn meridiem(word: &str) -> bool {
    matches!(bare_word(word).as_str(), "am" | "pm" | "a.m" | "p.m")
}

/// An hour of a 12-hour clock: "8", "12", "8:30".
fn hour(word: &str) -> bool {
    twelve_hour(&bare_word(word))
}

fn twelve_hour(text: &str) -> bool {
    let (h, m) = text.split_once(':').unwrap_or((text, "00"));
    let digits = |s: &str, n: std::ops::RangeInclusive<usize>| {
        n.contains(&s.len()) && s.chars().all(|c| c.is_ascii_digit())
    };
    digits(h, 1..=2)
        && digits(m, 2..=2)
        && (1..=12).contains(&h.parse::<u32>().unwrap_or(0))
        && m.parse::<u32>().unwrap_or(60) < 60
}

/// A clock time in one word: "8pm", "10:30am", "8p.m.", "19:00". A time
/// names an hour every event at a venue shares, never one booking.
fn clock_time(word: &str) -> bool {
    let word = bare_word(word);
    if let Some(rest) = ["am", "pm", "a.m", "p.m"]
        .iter()
        .find_map(|m| word.strip_suffix(m))
    {
        return twelve_hour(rest);
    }
    let Some((h, m)) = word.split_once(':') else {
        return false;
    };
    (1..=2).contains(&h.len())
        && m.len() == 2
        && h.parse::<u32>().is_ok_and(|h| h < 24)
        && m.parse::<u32>().is_ok_and(|m| m < 60)
        && word.chars().all(|c| c.is_ascii_digit() || c == ':')
}

/// A URL ref as lowercase host and the path AS WRITTEN (an event id in a path
/// is case-sensitive: `partiful.com/e/AbCdEf1` and `.../aBcDeF1` are two
/// events), kept only when the path itself names one event. The query,
/// fragment and `;` path parameters are gone (that is where tokens
/// live), so a route whose id lived in the query (Google Calendar's
/// `event?eid=`, OpenTable's `book/view?rid=`, OWA's `?itemid=`) is left a
/// generic route every event on that platform shares, and is dropped like a
/// bare site. Kept: a last segment with a digit (`/e/12345`), or a last
/// segment of 6 or more characters under at least one other (`/e/summer-gala`).
/// Dropped too: any credential-shaped segment, whatever else the path says.
/// The record then falls back to thread matching.
fn url_ref(url: &str) -> Option<String> {
    let (host, path) = url_host_path(url)?;
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let last = segments.last()?;
    if segments.iter().any(|s| credential_shaped(s)) {
        return None;
    }
    let names_one = last.chars().any(|c| c.is_ascii_digit())
        || (segments.len() >= 2 && last.chars().count() >= 6);
    names_one.then(|| format!("{host}{path}"))
}

/// A path segment that looks like a secret: one unbroken run longer than
/// [`TOKEN_SEGMENT`] mixing letters and digits, or a UUID. Runs are split on
/// `-` and `_` so a readable slug ("some-event-name-tickets-123456789") is
/// not mistaken for a token, which is also why a UUID, whose runs are short,
/// needs its own test: a cancellation or magic link is often exactly that.
fn credential_shaped(segment: &str) -> bool {
    let token_run = segment.split(['-', '_']).any(|run| {
        run.chars().count() > TOKEN_SEGMENT
            && run.chars().any(|c| c.is_ascii_alphabetic())
            && run.chars().any(|c| c.is_ascii_digit())
    });
    token_run || has_uuid(segment)
}

/// Whether five consecutive `-`-separated runs are hex of lengths 8-4-4-4-12.
fn has_uuid(segment: &str) -> bool {
    let runs: Vec<&str> = segment.split('-').collect();
    runs.windows(5).any(|w| {
        w.iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(run, len)| run.len() == len && run.chars().all(|c| c.is_ascii_hexdigit()))
    })
}

/// The URL in one whitespace-free token, without the punctuation prose wraps
/// around it ("(see https://x/e/1)."), or None when the token is not a URL.
/// A scheme or `www.` is enough, ANYWHERE in the token: a URL glued to its
/// label ("Zoom:https://...", "Meeting<https://...>") is still a URL, and
/// missing it kept its query, passcode and all. A bare host must start the
/// token, needs an alphabetic top-level label (so "St./Elm" and "2.5/3h" stay
/// text), and ends at the first `/`, `?` or `#`, so "zoom.us?pwd=abc" is a
/// URL as much as "zoom.us/j/1?pwd=abc" is.
fn url_core(token: &str) -> Option<&str> {
    const TRAILING: [char; 11] = ['.', ',', ';', ':', '!', '?', ')', '>', ']', '"', '\''];
    // ASCII lowercasing keeps every byte offset, so `at` indexes `token`.
    let lower = token.to_ascii_lowercase();
    let scheme = ["https://", "http://"]
        .iter()
        .filter_map(|s| lower.find(s))
        .min();
    let www = || {
        lower
            .match_indices("www.")
            .map(|(i, _)| i)
            .find(|&i| !lower[..i].ends_with(|c: char| c.is_alphanumeric()))
    };
    if let Some(at) = scheme.or_else(www) {
        let core = token[at..].trim_end_matches(TRAILING);
        return (!core.is_empty()).then_some(core);
    }
    let core = token
        .trim_start_matches(['(', '<', '[', '"', '\''])
        .trim_end_matches(TRAILING);
    let tld_ok = bare_host(core)
        .and_then(|host| host.rsplit('.').next())
        .is_some_and(|tld| tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()));
    (tld_ok && looks_like_url(core)).then_some(core)
}

/// The host of a bare `host/path`, `host?query` or `host#fragment`: what
/// comes before the first `/`, `?` or `#`. None when there is none of them.
fn bare_host(raw: &str) -> Option<&str> {
    raw.find(['/', '?', '#']).map(|at| &raw[..at])
}

/// Every URL in `text` passed through `rewrite` (None removes it), the rest
/// of each token kept. `text` is already [`plain`], so tokens are
/// single-space separated.
fn map_urls(text: &str, mut rewrite: impl FnMut(&str) -> Option<String>) -> String {
    text.split(' ')
        .filter_map(|token| {
            let Some(core) = url_core(token) else {
                return Some(token.to_string());
            };
            // `core` is a slice of `token`, so its offset is exact.
            let at = core.as_ptr() as usize - token.as_ptr() as usize;
            let (before, after) = (&token[..at], &token[at + core.len()..]);
            Some(format!("{before}{}{after}", rewrite(core)?))
        })
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn looks_like_url(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("www.") {
        return true;
    }
    // "lu.ma/abc", "zoom.us?pwd=x": a dotted host then a path, query or
    // fragment. An iCalendar UID has an '@'.
    !raw.contains(char::is_whitespace)
        && !raw.contains('@')
        && bare_host(raw).is_some_and(|host| host.contains('.') && !host.starts_with('.'))
}

/// Host without `www.`, and path without its trailing slash or any `;`
/// parameters on its segments ("/j/1;pwd=x" is "/j/1": a passcode can ride
/// there as well as in the query). Query, fragment and credentials in the
/// authority are never returned.
fn url_host_path(raw: &str) -> Option<(String, String)> {
    let lower = raw.to_ascii_lowercase();
    let with_scheme = if lower.starts_with("http://") || lower.starts_with("https://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    let url = url::Url::parse(&with_scheme).ok()?;
    let host = url.host_str()?.to_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    let path = url
        .path()
        .split('/')
        .map(|segment| segment.split(';').next().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("/");
    Some((host, path.trim_end_matches('/').to_string()))
}

pub(crate) fn clean_text(raw: &str) -> Option<String> {
    let text = truncate_trimmed(&plain(raw), TEXT_CAP);
    (!text.is_empty()).then_some(text)
}

/// A title, cleaned like any text. An empty title drops its record, and which
/// records are kept fixes every later record's stored `position`: changing
/// this rule after ghosts exist shifts positions until each message is
/// re-triaged, so it has to be right before the first release.
fn clean_title(raw: &str) -> Option<String> {
    let text = truncate_trimmed(&plain(raw), TEXT_CAP);
    (!text.is_empty()).then_some(text)
}

/// The model's own explanation (`basis`, `reason`). The timeline shows it as
/// the reason for a guess, so it is capped at [`NOTE_CAP`], and it is dropped
/// outright (the number or band it explains stays) when it carries something
/// mail-shaped: a URL, an address, or a run of 6 or more digits (a booking
/// number, a phone number, a code). The run counts across the separators a
/// number is printed with (space, `-`, `.`, `+`, parentheses), so "call
/// 555-1234" and "(415) 555 1234" are runs too; a letter or any other
/// character ends it, which keeps "doors 7, show 8" a phrase.
fn note(raw: &str) -> String {
    let text = plain(raw);
    let mut run = 0;
    let digits = text.chars().any(|c| {
        if c.is_numeric() {
            run += 1;
        } else if !matches!(c, ' ' | '-' | '.' | '+' | '(' | ')') {
            run = 0;
        }
        run >= 6
    });
    if digits || text.contains('@') || text.split(' ').any(|t| url_core(t).is_some()) {
        return String::new();
    }
    truncate_trimmed(&text, NOTE_CAP)
}

/// A location is display text, except that every URL in it (a meeting link,
/// alone or after a label as in "Zoom: https://...") keeps only host and
/// path: a query string is where a meeting passcode lives. A path with a
/// credential-shaped segment (a join token) is cut back to the host.
pub(crate) fn launder_location(raw: &str) -> Option<String> {
    clean_text(&map_urls(&plain(raw), meeting_link))
}

/// A URL in display text: host and path, never the query, and the host alone
/// when the path carries a credential-shaped segment.
fn meeting_link(url: &str) -> Option<String> {
    let (host, path) = url_host_path(url)?;
    if path.split('/').any(credential_shaped) {
        return Some(host);
    }
    Some(format!("{host}{path}"))
}

/// Cap on a calendar event's description.
pub const DESCRIPTION_CAP: usize = 2000;

/// A calendar event's description, laundered as a location is (control, bidi
/// and invisible characters out, every URL cut to host and path) line by
/// line, so its paragraphs survive: at most one blank line in a row, at most
/// [`DESCRIPTION_CAP`] characters. Google's description "Can contain HTML",
/// and an invitation's usually does: line-breaking tags become line breaks,
/// every other tag goes, and the few entities an editor writes are decoded,
/// so the client is never handed markup to show as text.
pub(crate) fn launder_description(raw: &str) -> Option<String> {
    let text = html_to_text(raw);
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = map_urls(&plain(line), meeting_link);
        if line.is_empty() && lines.last().is_none_or(|l: &String| l.is_empty()) {
            continue;
        }
        lines.push(line);
    }
    let text = truncate_trimmed(&lines.join("\n"), DESCRIPTION_CAP);
    (!text.is_empty()).then_some(text)
}

/// Markup to plain text, for [`launder_description`]. A `<` starts a tag only
/// when a letter, `/` or `!` follows it, so "a < b" is text.
fn html_to_text(raw: &str) -> String {
    const BREAKS: [&str; 7] = ["br", "p", "div", "li", "tr", "h1", "h2"];
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('<') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let tag_like = after
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '/' || c == '!');
        match after.find('>').filter(|_| tag_like) {
            Some(close) => {
                let name: String = after[..close]
                    .trim_start_matches('/')
                    .chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .collect::<String>()
                    .to_ascii_lowercase();
                if BREAKS.contains(&name.as_str()) {
                    out.push('\n');
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// The zone a start claims, as an IANA PLACE zone, or None: a valid name with
/// a `/` that is not under `Etc/` ([`crate::timezone::place_zone`], the
/// correspondent-timezone code's own test). "Eastern Standard Time", "PDT",
/// "EST" and "UTC" are not places: an abbreviation names several, and a fixed
/// offset is wrong for half the year wherever it is read.
pub(crate) fn iana_zone(raw: &str) -> Option<String> {
    place_tz(raw).map(|tz| tz.name().to_string())
}
/// [`iana_zone`]'s test, keeping the zone itself for [`EventTime::resolve`].
fn place_tz(raw: &str) -> Option<Tz> {
    let zone = plain(raw);
    if zone.chars().count() > TIMEZONE_CAP || !zone.contains('/') {
        return None;
    }
    crate::timezone::place_zone(&zone)
}

/// A location reduced to the key duration priors are stored under: the part
/// before the first comma (the venue name, without the street address that
/// one mail prints and the next omits), as lowercase words.
pub fn venue_key(location: &str) -> Option<String> {
    let location = plain(location);
    let venue = location.split(',').next().unwrap_or_default();
    let words: Vec<String> = venue
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let key = truncate_chars(&words.join(" "), VENUE_CAP)
        .trim()
        .to_string();
    (!key.is_empty()).then_some(key)
}

/// The last-resort duration by kind, in minutes. Travel has none: a flight's
/// length is in its mail or nowhere, and a guessed one would block the wrong
/// hours.
pub fn default_minutes(kind: EventRecordKind) -> Option<u32> {
    match kind {
        EventRecordKind::Reservation => Some(120),
        EventRecordKind::Ticket => Some(180),
        EventRecordKind::Invite | EventRecordKind::Proposal | EventRecordKind::Other => Some(60),
        EventRecordKind::Travel => None,
    }
}

/// Which tier produced an end. `Stated` and `User` are facts; the rest are
/// estimates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EndSource {
    Stated,
    User,
    History,
    Model,
    Default,
}
impl EndSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stated => "stated",
            Self::User => "user",
            Self::History => "history",
            Self::Model => "model",
            Self::Default => "default",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveEnd {
    pub end: EventTime,
    /// True for every tier but `Stated` and `User`.
    pub estimated: bool,
    pub source: EndSource,
    /// The duration in minutes. None for a date-only fact, which has no
    /// clock span.
    pub minutes: Option<u32>,
    /// The model's basis, for the `Model` tier only.
    pub basis: Option<String>,
}

/// Resolve the end to draw, strongest first: the stated end, the user's
/// correction, the history prior for this venue and kind, the model's
/// estimate, the kind default. A fact that is not after `start` is skipped.
/// A date-only start never gets an estimate, so with no fact it has no end.
///
/// A kind with no default (travel) has no history tier either: one long
/// flight from "JFK Terminal 4" is no guide to the next one from there.
pub fn effective_end(
    start: &EventTime,
    kind: EventRecordKind,
    stated_end: Option<&EventTime>,
    user_end: Option<&EventTime>,
    history_minutes: Option<u32>,
    model_estimate: Option<&GhostEstimate>,
) -> Option<EffectiveEnd> {
    let fact = |end: &EventTime, source| {
        end.is_after(start).then(|| EffectiveEnd {
            end: *end,
            estimated: false,
            source,
            minutes: match (start, end) {
                (EventTime::At(s), EventTime::At(e)) => u32::try_from((*e - *s).num_minutes()).ok(),
                _ => None,
            },
            basis: None,
        })
    };
    if let Some(found) = stated_end.and_then(|e| fact(e, EndSource::Stated)) {
        return Some(found);
    }
    if let Some(found) = user_end.and_then(|e| fact(e, EndSource::User)) {
        return Some(found);
    }
    let EventTime::At(at) = start else {
        return None;
    };
    let (minutes, source, basis) = history_minutes
        .filter(|_| default_minutes(kind).is_some())
        .map(|m| (m, EndSource::History, None))
        .or_else(|| {
            model_estimate.map(|e| {
                let basis = Some(e.basis.clone()).filter(|b| !b.is_empty());
                (e.minutes, EndSource::Model, basis)
            })
        })
        .or_else(|| default_minutes(kind).map(|m| (m, EndSource::Default, None)))?;
    let minutes = minutes.clamp(*ESTIMATE_MINUTES.start(), *ESTIMATE_MINUTES.end());
    Some(EffectiveEnd {
        end: EventTime::At(*at + Duration::minutes(minutes.into())),
        estimated: true,
        source,
        minutes: Some(minutes),
        basis,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triage::decision::{DurationEstimate, LikelihoodBand, SupportedTime};

    fn time(value: &str) -> Option<SupportedTime> {
        Some(SupportedTime {
            value: value.into(),
            timezone: Some("America/New_York".into()),
            source_message_id: 7,
            interpreted_relative: false,
        })
    }
    fn event(title: &str, refs: &[&str]) -> RecordProposal {
        RecordProposal::Event {
            title: title.into(),
            start: time("2026-10-03T20:00:00-04:00"),
            location: Some("Brooklyn Steel, 319 Frost St".into()),
            end: None,
            duration_estimate: Some(DurationEstimate {
                minutes: 150,
                basis: "doors 7, show 8".into(),
            }),
            event_kind: EventRecordKind::Ticket,
            commitment: EventCommitment::Committed,
            change: EventChange::Created,
            identity_refs: refs.iter().map(|r| r.to_string()).collect(),
            likelihood: None,
            evidence: vec![],
        }
    }
    fn at(raw: &str) -> EventTime {
        EventTime::parse(raw).unwrap()
    }

    #[test]
    fn keys_come_from_refs_with_labels_and_punctuation_folded() {
        let ghost = launder(&event(
            "Show",
            &["Confirmation #ABC-123", "abc123", "Booking ref QXJZKM"],
        ))
        .unwrap();
        // "Confirmation #ABC-123" and "abc123" are one ref.
        assert_eq!(ghost.identity_refs, vec!["abc123", "qxjzkm"]);
        assert_eq!(
            identity_keys(&ghost, Some("venue.com")),
            vec!["ref:venue.com:abc123", "ref:venue.com:qxjzkm"]
        );
    }

    #[test]
    fn url_refs_lose_query_fragment_and_www() {
        let ghost = launder(&event(
            "Show",
            &[
                "https://www.Eventbrite.com/e/12345/?aff=mail&token=secret#tickets",
                "lu.ma/AbCd7",
            ],
        ))
        .unwrap();
        // The host is lowercased; the path keeps its case (round 3, N6).
        assert_eq!(
            identity_keys(&ghost, Some("eventbrite.com")),
            vec![
                "ref:eventbrite.com:eventbrite.com/e/12345",
                "ref:eventbrite.com:lu.ma/AbCd7"
            ]
        );
    }

    /// Identity v2: a ref is scoped to its sender, there is no thread or
    /// title fallback key, and an unauthenticated record has no keys.
    #[test]
    fn keys_are_scoped_and_there_is_no_fallback_key() {
        let ghost = launder(&event("Show", &["ABC123"])).unwrap();
        assert_ne!(
            identity_keys(&ghost, Some("venue.com")),
            identity_keys(&ghost, Some("evil.com")),
            "the same ref from two senders is two keys"
        );
        assert!(identity_keys(&ghost, None).is_empty(), "unauthenticated");
        let refless = launder(&event("Dinner @ Lilia, Friday!", &[])).unwrap();
        assert!(identity_keys(&refless, Some("lilia.com")).is_empty());
        // Junk refs alone are the same as none.
        let junk = launder(&event("Dinner @ Lilia, Friday!", &["Confirmed", "#12"])).unwrap();
        assert!(identity_keys(&junk, Some("lilia.com")).is_empty());
    }

    #[test]
    fn a_record_keeps_at_most_five_refs() {
        let refs = [
            "AAA111", "BBB222", "aaa111", "CCC333", "DDD444", "EEE555", "FFF666",
        ];
        let ghost = launder(&event("Show", &refs)).unwrap();
        assert_eq!(
            ghost.identity_refs,
            vec!["aaa111", "bbb222", "ccc333", "ddd444", "eee555"],
            "duplicates do not use up the cap"
        );
    }

    #[test]
    fn scope_is_the_authenticated_senders_domain_or_freemail_address() {
        assert_eq!(
            sender_scope("Tickets@Mail.Venue.co.uk", Some(true)).as_deref(),
            Some("venue.co.uk")
        );
        assert_eq!(
            sender_scope("events@sub.venue.com", Some(true)),
            sender_scope("box@venue.com", Some(true)),
            "one organisation across its mailboxes"
        );
        // Freemail: the full address, so two strangers never share a scope.
        assert_eq!(
            sender_scope("Alice@Gmail.com", Some(true)).as_deref(),
            Some("alice@gmail.com")
        );
        assert_ne!(
            sender_scope("alice@gmail.com", Some(true)),
            sender_scope("bob@gmail.com", Some(true))
        );
        // Failed, unknown, or no domain: no scope at all.
        assert_eq!(sender_scope("box@venue.com", Some(false)), None);
        assert_eq!(sender_scope("box@venue.com", None), None);
        assert_eq!(sender_scope("nobody", Some(true)), None);
        assert_eq!(sender_scope("venue.com", Some(true)), None, "no address");
        assert_eq!(sender_scope("a@com.mx", Some(true)), None, "a bare suffix");
    }

    /// Round 2 (1.2): the scope's domain comes from the real Public Suffix
    /// List. These are the exact answers `psl` gives, pinned so a crate
    /// update that changes one is noticed.
    #[test]
    fn the_scope_domain_comes_from_the_public_suffix_list() {
        let scope = |from: &str| sender_scope(from, Some(true));
        // `com.mx` is a public suffix: two hotels there are two senders.
        assert_eq!(scope("res@hotel.com.mx").as_deref(), Some("hotel.com.mx"));
        assert_eq!(scope("res@evil.com.mx").as_deref(), Some("evil.com.mx"));
        assert_eq!(
            scope("alerts@news.example.co.uk").as_deref(),
            Some("example.co.uk")
        );
        // An unknown TLD is a one-label suffix (the fixtures' `.example`).
        assert_eq!(
            scope("a@sub.venue.example").as_deref(),
            Some("venue.example")
        );
        // A private-section host is one registrable domain per customer.
        assert_eq!(
            organisation_domain("x.blogspot.com").as_deref(),
            Some("x.blogspot.com")
        );
        // The list does NOT carry `onmicrosoft.com`: every tenant would be
        // one `onmicrosoft.com`, so it is a shared tenant, scoped by address.
        assert_eq!(
            psl_registrable_domain("a@contoso.onmicrosoft.com").as_deref(),
            Some("onmicrosoft.com")
        );
        assert_eq!(
            scope("Ana@Contoso.onmicrosoft.com").as_deref(),
            Some("ana@contoso.onmicrosoft.com")
        );
        assert_ne!(
            scope("a@contoso.onmicrosoft.com"),
            scope("a@fabrikam.onmicrosoft.com")
        );
        assert_eq!(
            scope("club@googlegroups.com").as_deref(),
            Some("club@googlegroups.com")
        );
        assert_ne!(scope("a@googlegroups.com"), scope("b@googlegroups.com"));
    }

    /// Round 2 (1.2): the freemail list is longer, and brands registered
    /// under many suffixes are freemail under all of them.
    #[test]
    fn newly_listed_freemail_providers_scope_by_address() {
        for domain in [
            "mac.com",
            "ymail.com",
            "msn.com",
            "yahoo.co.uk",
            "hotmail.fr",
            "hotmail.co.uk",
            "live.co.uk",
            "gmx.de",
            "gmx.net",
            "web.de",
            "mail.ru",
            "yandex.ru",
            "qq.com",
            "163.com",
            "naver.com",
            "zoho.com",
            "fastmail.com",
            "hey.com",
            "pm.me",
            "tutanota.de",
            "protonmail.ch",
        ] {
            let alice = sender_scope(&format!("alice@{domain}"), Some(true));
            let bob = sender_scope(&format!("bob@{domain}"), Some(true));
            assert_eq!(alice, Some(format!("alice@{domain}")), "{domain}");
            assert_ne!(alice, bob, "{domain}");
            assert_eq!(
                organisation_domain(&format!("alice@{domain}")),
                None,
                "{domain}"
            );
        }
        // An organisation is still its domain, across mailboxes.
        assert_eq!(
            organisation_domain("Events@Mail.Venue.co.uk").as_deref(),
            Some("venue.co.uk")
        );
        assert_eq!(
            organisation_domain("box@venue.co.uk"),
            organisation_domain("events@mail.venue.co.uk")
        );
    }

    /// Round 2 (1.3): a hostile mail cannot mint unbounded ghosts.
    #[test]
    fn a_message_keeps_at_most_twenty_event_records() {
        let titles: Vec<String> = (0..25).map(|i| format!("Event {i}")).collect();
        let mut records: Vec<RecordProposal> = titles.iter().map(|t| event(t, &[])).collect();
        // An empty title is dropped BEFORE the cap counts.
        records.insert(0, event("  ", &[]));
        let kept = launder_events(&records);
        assert_eq!(kept.len(), EVENTS_PER_MESSAGE);
        assert_eq!(kept[0].title, "Event 0");
        assert_eq!(kept[19].title, "Event 19");
    }

    #[test]
    fn positions_count_laundered_events_only() {
        let bill = RecordProposal::Receipt {
            merchant: "x".into(),
            amount: None,
            currency: None,
            evidence: vec![],
        };
        let events = launder_events(&[
            bill.clone(),
            event("   ", &[]),
            event("First", &[]),
            bill,
            event("Second", &[]),
        ]);
        let titles: Vec<&str> = events.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["First", "Second"]);
    }

    #[test]
    fn junk_refs_are_dropped() {
        let long = "9".repeat(201);
        for junk in [
            "",
            "   ",
            "A1",                              // too short
            "#12",                             // too short once folded
            long.as_str(),                     // too long
            "confirmed",                       // a word
            "Confirmed",                       // a word
            "CONFIRMED",                       // a shouted stop word
            "Dinner at Nobu",                  // prose
            "Booking reference",               // only labels
            "https://www.opentable.com/",      // a bare site identifies nothing
            "https://ticketmaster.com?id=999", // still a bare site
        ] {
            assert_eq!(normalize_ref(junk), None, "{junk:?} should drop");
        }
        // Kept: a record locator, a booking number, an iCalendar UID (its
        // digit-bearing part; the domain after `@` is the sender's, and the
        // key is already scoped by sender).
        assert_eq!(normalize_ref("QXJZKM").as_deref(), Some("qxjzkm"));
        assert_eq!(normalize_ref("Order No. 447112").as_deref(), Some("447112"));
        assert_eq!(
            normalize_ref("7kq3abc@google.com").as_deref(),
            Some("7kq3abc")
        );
        assert_eq!(normalize_ref(&"9".repeat(200)).map(|k| k.len()), Some(200));
    }

    #[test]
    fn title_is_trimmed_capped_and_an_empty_one_drops_the_record() {
        let long = format!("  {}  ", "x".repeat(300));
        assert_eq!(launder(&event(&long, &[])).unwrap().title.len(), TEXT_CAP);
        assert_eq!(
            launder(&event("  Show\u{202E}\n time ", &[]))
                .unwrap()
                .title,
            "Show time"
        );
        assert!(launder(&event("   ", &[])).is_none());
        assert!(launder(&event("\u{200F}\t", &[])).is_none());
        let bill = RecordProposal::Receipt {
            merchant: "x".into(),
            amount: None,
            currency: None,
            evidence: vec![],
        };
        assert!(launder(&bill).is_none());
    }

    #[test]
    fn start_and_end_must_parse_and_end_must_follow_start() {
        let with = |start: &str, end: Option<&str>| {
            let mut record = event("Show", &[]);
            if let RecordProposal::Event {
                start: s, end: e, ..
            } = &mut record
            {
                *s = time(start);
                *e = end.and_then(time);
            }
            launder(&record).unwrap()
        };
        let ok = with(
            "2026-10-03T20:00:00-04:00",
            Some("2026-10-03T23:00:00-04:00"),
        );
        assert_eq!(ok.end, Some(at("2026-10-03T23:00:00-04:00")));
        assert_eq!(ok.start_timezone.as_deref(), Some("America/New_York"));
        // A stated end removes the estimate: the guess never sits beside a fact.
        assert_eq!(ok.estimate, None);

        let bad_start = with("Saturday at 8", Some("2026-10-03T23:00:00-04:00"));
        assert_eq!(bad_start.start, None);
        assert_eq!(bad_start.start_timezone, None);
        assert_eq!(bad_start.end, None, "an end with no start is dropped");
        assert_eq!(bad_start.estimate, None);

        assert_eq!(
            with("2026-10-03T20:00:00-04:00", Some("tomorrow")).end,
            None
        );
        let backwards = with(
            "2026-10-03T20:00:00-04:00",
            Some("2026-10-03T19:00:00-04:00"),
        );
        assert_eq!(backwards.end, None);
        assert!(
            backwards.estimate.is_some(),
            "a dropped end lets the estimate stand"
        );
        assert_eq!(
            with(
                "2026-10-03T20:00:00-04:00",
                Some("2026-10-03T20:00:00-04:00")
            )
            .end,
            None,
            "equal is not after"
        );
        assert_eq!(
            with("2026-10-03", Some("2026-10-05")).end,
            Some(EventTime::Date(
                NaiveDate::from_ymd_opt(2026, 10, 5).unwrap()
            ))
        );
        assert_eq!(
            with("2026-10-03", Some("2026-10-03T23:00:00-04:00")).end,
            None,
            "mixed precision"
        );
    }

    #[test]
    fn estimate_is_clamped_and_dropped_for_date_only_starts() {
        let with = |start: &str, minutes: i64, basis: &str| {
            let mut record = event("Show", &[]);
            if let RecordProposal::Event {
                start: s,
                duration_estimate,
                ..
            } = &mut record
            {
                *s = time(start);
                *duration_estimate = Some(DurationEstimate {
                    minutes,
                    basis: basis.into(),
                });
            }
            launder(&record).unwrap().estimate
        };
        let timed = "2026-10-03T20:00:00-04:00";
        assert_eq!(with(timed, 150, " doors 7 ").unwrap().basis, "doors 7");
        assert_eq!(with(timed, 1, "x").unwrap().minutes, 15);
        assert_eq!(with(timed, 100_000, "x").unwrap().minutes, 4320);
        assert_eq!(with(timed, 0, "x"), None);
        assert_eq!(with(timed, -60, "x"), None);
        assert_eq!(
            with(timed, 90, &"b".repeat(500)).unwrap().basis.len(),
            NOTE_CAP
        );
        assert_eq!(with("2026-10-03", 150, "x"), None);
    }

    #[test]
    fn location_and_likelihood_are_laundered() {
        let mut record = event("Show", &[]);
        if let RecordProposal::Event {
            location,
            likelihood,
            ..
        } = &mut record
        {
            *location = Some("https://zoom.us/j/123456?pwd=SECRET".into());
            *likelihood = Some(EventLikelihood {
                band: LikelihoodBand::Likely,
                reason: format!(" {} ", "r".repeat(400)),
            });
        }
        let ghost = launder(&record).unwrap();
        assert_eq!(ghost.location.as_deref(), Some("zoom.us/j/123456"));
        let likelihood = ghost.likelihood.unwrap();
        assert_eq!(likelihood.band, LikelihoodBand::Likely);
        assert_eq!(likelihood.reason.len(), NOTE_CAP);

        if let RecordProposal::Event { location, .. } = &mut record {
            *location = Some("   ".into());
        }
        assert_eq!(launder(&record).unwrap().location, None);
    }

    #[test]
    fn venue_key_is_the_venue_name_folded() {
        assert_eq!(
            venue_key("Brooklyn Steel, 319 Frost St, Brooklyn").as_deref(),
            Some("brooklyn steel")
        );
        assert_eq!(venue_key("BROOKLYN  STEEL"), venue_key("Brooklyn Steel"));
        assert_eq!(venue_key("  , somewhere"), None);
    }

    #[test]
    fn default_minutes_table() {
        assert_eq!(default_minutes(EventRecordKind::Reservation), Some(120));
        assert_eq!(default_minutes(EventRecordKind::Ticket), Some(180));
        assert_eq!(default_minutes(EventRecordKind::Invite), Some(60));
        assert_eq!(default_minutes(EventRecordKind::Proposal), Some(60));
        assert_eq!(default_minutes(EventRecordKind::Other), Some(60));
        assert_eq!(default_minutes(EventRecordKind::Travel), None);
    }

    #[test]
    fn effective_end_takes_the_strongest_tier() {
        let start = at("2026-10-03T20:00:00-04:00");
        let stated = at("2026-10-03T23:30:00-04:00");
        let user = at("2026-10-04T02:00:00-04:00");
        let model = GhostEstimate {
            minutes: 150,
            basis: "doors 7, show 8".into(),
        };
        let kind = EventRecordKind::Ticket;

        let end = effective_end(
            &start,
            kind,
            Some(&stated),
            Some(&user),
            Some(240),
            Some(&model),
        )
        .unwrap();
        assert_eq!(
            (end.end, end.estimated, end.source, end.minutes),
            (stated, false, EndSource::Stated, Some(210))
        );

        let end = effective_end(&start, kind, None, Some(&user), Some(240), Some(&model)).unwrap();
        assert_eq!(
            (end.end, end.estimated, end.source, end.minutes),
            (user, false, EndSource::User, Some(360))
        );

        let end = effective_end(&start, kind, None, None, Some(240), Some(&model)).unwrap();
        assert_eq!(
            (end.end, end.estimated, end.source, end.minutes, end.basis),
            (
                at("2026-10-04T00:00:00-04:00"),
                true,
                EndSource::History,
                Some(240),
                None
            )
        );

        let end = effective_end(&start, kind, None, None, None, Some(&model)).unwrap();
        assert_eq!(
            (end.end, end.estimated, end.source, end.minutes),
            (
                at("2026-10-03T22:30:00-04:00"),
                true,
                EndSource::Model,
                Some(150)
            )
        );
        assert_eq!(end.basis.as_deref(), Some("doors 7, show 8"));

        let end = effective_end(&start, kind, None, None, None, None).unwrap();
        assert_eq!(
            (end.end, end.estimated, end.source, end.minutes),
            (
                at("2026-10-03T23:00:00-04:00"),
                true,
                EndSource::Default,
                Some(180)
            )
        );
        assert_eq!(EndSource::Default.as_str(), "default");

        // Travel has no default: with nothing else, no end.
        assert_eq!(
            effective_end(&start, EventRecordKind::Travel, None, None, None, None),
            None
        );
        // A fact that is not after the start is skipped for the next tier.
        let before = at("2026-10-03T19:00:00-04:00");
        let end = effective_end(&start, kind, Some(&before), Some(&before), None, None).unwrap();
        assert_eq!(end.source, EndSource::Default);
        // A history prior is clamped like any estimate.
        let end = effective_end(&start, kind, None, None, Some(0), None).unwrap();
        assert_eq!(end.minutes, Some(15));
    }

    #[test]
    fn effective_end_never_estimates_a_date_only_start() {
        let day = EventTime::Date(NaiveDate::from_ymd_opt(2026, 10, 3).unwrap());
        let model = GhostEstimate {
            minutes: 150,
            basis: "x".into(),
        };
        for kind in [EventRecordKind::Ticket, EventRecordKind::Other] {
            assert_eq!(
                effective_end(&day, kind, None, None, Some(240), Some(&model)),
                None
            );
        }
        // A stated date-only end is a fact and still applies.
        let next = EventTime::Date(NaiveDate::from_ymd_opt(2026, 10, 5).unwrap());
        let end =
            effective_end(&day, EventRecordKind::Other, Some(&next), None, None, None).unwrap();
        assert_eq!(
            (end.end, end.estimated, end.source, end.minutes),
            (next, false, EndSource::Stated, None)
        );
    }

    #[test]
    fn event_time_round_trips_through_its_stored_form() {
        for raw in [
            "2026-10-03",
            "2026-10-03T20:00:00-04:00",
            "2026-10-03T20:00:00+00:00",
        ] {
            assert_eq!(at(raw).to_string(), raw);
            assert_eq!(EventTime::parse(&at(raw).to_string()), Some(at(raw)));
        }
        assert_eq!(EventTime::parse("2026-10-03T20:00:00"), None, "no offset");
        assert_eq!(EventTime::parse("Oct 3"), None);
    }

    fn zoned(value: &str, zone: Option<&str>) -> Option<SupportedTime> {
        Some(SupportedTime {
            value: value.into(),
            timezone: zone.map(Into::into),
            source_message_id: 7,
            interpreted_relative: false,
        })
    }
    /// A record with these start and end, laundered.
    fn launder_times(start: Option<SupportedTime>, end: Option<SupportedTime>) -> GhostProposal {
        let mut record = event("Flight", &[]);
        if let RecordProposal::Event {
            start: s, end: e, ..
        } = &mut record
        {
            *s = start;
            *e = end;
        }
        launder(&record).unwrap()
    }

    #[test]
    fn a_zoned_local_start_and_end_resolve_in_their_own_zones() {
        // The flight that had no times: departs Tokyo, lands in Los Angeles
        // "earlier" the same day by the wall clock.
        let ghost = launder_times(
            zoned("2026-10-19T21:30", Some("Asia/Tokyo")),
            zoned("2026-10-19T14:45", Some("America/Los_Angeles")),
        );
        assert_eq!(ghost.start, Some(at("2026-10-19T21:30:00+09:00")));
        assert_eq!(ghost.end, Some(at("2026-10-19T14:45:00-07:00")));
        assert_eq!(ghost.start_timezone.as_deref(), Some("Asia/Tokyo"));
        assert_eq!(
            ghost.start.unwrap().to_string(),
            "2026-10-19T21:30:00+09:00",
            "stored as RFC3339 with the zone's offset"
        );
    }

    #[test]
    fn an_end_with_no_zone_is_read_in_the_starts_zone() {
        let ghost = launder_times(
            zoned("2026-10-19T19:00:00", Some("America/New_York")),
            zoned("2026-10-19T22:30:00", None),
        );
        assert_eq!(ghost.end, Some(at("2026-10-19T22:30:00-04:00")));
        let blank = launder_times(
            zoned("2026-10-19T19:00:00", Some("America/New_York")),
            zoned("2026-10-19T22:30:00", Some("  ")),
        );
        assert_eq!(blank.end, Some(at("2026-10-19T22:30:00-04:00")));
        // A zone the end does name, but unusably, does not borrow the start's.
        let junk = launder_times(
            zoned("2026-10-19T19:00:00", Some("America/New_York")),
            zoned("2026-10-19T22:30:00", Some("PDT")),
        );
        assert_eq!(junk.end, None);
    }

    #[test]
    fn resolve_reads_naive_shapes_and_leaves_the_two_old_shapes_alone() {
        let ny = Some("America/New_York");
        for (raw, want) in [
            ("2026-10-19T21:30", "2026-10-19T21:30:00-04:00"),
            ("2026-10-19T21:30:15", "2026-10-19T21:30:15-04:00"),
            (" 2026-10-19T21:30:15.250 ", "2026-10-19T21:30:15.250-04:00"),
        ] {
            assert_eq!(EventTime::resolve(raw, ny), Some(at(want)), "{raw}");
        }
        // RFC3339 keeps the offset the mail gave, whatever zone is beside it.
        assert_eq!(
            EventTime::resolve("2026-10-19T21:30:00+09:00", ny),
            Some(at("2026-10-19T21:30:00+09:00"))
        );
        assert_eq!(
            EventTime::resolve("2026-10-19T21:30:00Z", None),
            Some(at("2026-10-19T21:30:00+00:00"))
        );
        // A date stays a date, zone or not.
        let day = Some(EventTime::Date(
            NaiveDate::from_ymd_opt(2026, 10, 19).unwrap(),
        ));
        assert_eq!(EventTime::resolve("2026-10-19", ny), day);
        assert_eq!(EventTime::resolve("2026-10-19", None), day);
        assert_eq!(EventTime::resolve("tomorrow at 9", ny), None);
        assert_eq!(EventTime::resolve("2026-10-19T25:00", ny), None);
    }

    #[test]
    fn a_naive_time_needs_a_place_zone() {
        for zone in [
            None,
            Some("PDT"),
            Some("EST"),
            Some("UTC"),
            Some("Etc/GMT+5"),
            Some("Eastern Standard Time"),
            Some("America/Nowhere"),
            Some(""),
        ] {
            assert_eq!(
                EventTime::resolve("2026-10-19T21:30", zone),
                None,
                "{zone:?}"
            );
        }
        // And without one the record loses its start, and so its end, as before.
        let ghost = launder_times(
            zoned("2026-10-19T21:30", Some("PDT")),
            zoned("2026-10-19T23:30", Some("America/Los_Angeles")),
        );
        assert_eq!(
            (ghost.start, ghost.end, ghost.start_timezone),
            (None, None, None)
        );
    }

    #[test]
    fn a_time_in_the_dst_fold_is_the_earlier_one_and_in_the_gap_an_hour_later() {
        let ny = Some("America/New_York");
        // 2026-11-01 01:30 happens twice in New York: EDT first, then EST.
        assert_eq!(
            EventTime::resolve("2026-11-01T01:30", ny),
            Some(at("2026-11-01T01:30:00-04:00"))
        );
        // 2026-03-08 02:30 never happens there: the clock jumps 02:00 to 03:00.
        assert_eq!(
            EventTime::resolve("2026-03-08T02:30", ny),
            Some(at("2026-03-08T03:30:00-04:00"))
        );
        // Either side of the gap is untouched.
        assert_eq!(
            EventTime::resolve("2026-03-08T01:59", ny),
            Some(at("2026-03-08T01:59:00-05:00"))
        );
        assert_eq!(
            EventTime::resolve("2026-03-08T03:00", ny),
            Some(at("2026-03-08T03:00:00-04:00"))
        );
    }

    // ---- Laundering hardening (fix pass B2) ----

    /// One record with `edit` applied, laundered.
    fn launder_with(edit: impl FnOnce(&mut RecordProposal)) -> GhostProposal {
        let mut record = event("Show", &[]);
        edit(&mut record);
        launder(&record).unwrap()
    }

    #[test]
    fn a_url_ref_is_kept_when_its_path_names_one_event() {
        for (raw, key) in [
            // A digit in the last segment.
            (
                "https://www.eventbrite.com/e/12345",
                "eventbrite.com/e/12345",
            ),
            ("lu.ma/x7", "lu.ma/x7"),
            // Two segments, the last at least 6 characters.
            (
                "https://partiful.com/e/summergala",
                "partiful.com/e/summergala",
            ),
            ("lu.ma/event/abcdef", "lu.ma/event/abcdef"),
            // Long but letters only: a slug, not a token.
            (
                "example.com/events/abcdefghijklmnopqrstuvwxyzabcd",
                "example.com/events/abcdefghijklmnopqrstuvwxyzabcd",
            ),
            // Mixed, but exactly at the limit.
            (
                "example.com/e/a1b2c3d4e5f6g7h8i9j0k1l2",
                "example.com/e/a1b2c3d4e5f6g7h8i9j0k1l2",
            ),
        ] {
            assert_eq!(normalize_ref(raw).as_deref(), Some(key), "{raw}");
        }
    }

    #[test]
    fn a_url_ref_whose_id_lived_in_the_query_is_dropped() {
        for raw in [
            "https://calendar.google.com/calendar/event?action=VIEW&eid=AAAA111",
            "https://www.opentable.com/book/view?rid=1",
            "https://outlook.office.com/owa/?itemid=AAMkADQ5",
            // A bare site, with or without a query.
            "https://www.opentable.com/",
            "https://ticketmaster.com?id=999",
            // One short segment, or one long one with no digit.
            "lu.ma/event/abcde",
            "lu.ma/abcdef",
        ] {
            assert_eq!(normalize_ref(raw), None, "{raw} should drop");
        }
    }

    #[test]
    fn a_credential_shaped_url_segment_drops_the_ref() {
        for raw in [
            // 25 mixed characters: a magic link.
            "https://example.com/login/aB3dE5fG7hJ9kL1mN3pQ5rS7t",
            // Anywhere in the path, even with an id after it.
            "https://tickets.example/cancel/aB3dE5fG7hJ9kL1mN3pQ5rS7t/123",
            // A UUID is a capability even though its runs are short.
            "https://calendly.com/cancellations/3f2b8c1e-9a4d-4e7b-8c21-0d5f6a7b8c9d",
        ] {
            assert_eq!(normalize_ref(raw), None, "{raw} should drop");
        }
        // A readable slug is not a token, however long: its runs are words.
        assert_eq!(
            normalize_ref("https://www.eventbrite.com/e/some-event-name-tickets-123456789")
                .as_deref(),
            Some("eventbrite.com/e/some-event-name-tickets-123456789")
        );
    }

    #[test]
    fn a_ref_with_a_url_inside_text_is_that_url_without_its_query() {
        assert_eq!(
            normalize_ref("Event page: https://x.example/e/1?token=SECRET").as_deref(),
            Some("x.example/e/1")
        );
        assert_eq!(
            normalize_ref("(see https://x.example/e/1#SECRET).").as_deref(),
            Some("x.example/e/1")
        );
        // The URL inside decides alone: a generic route drops the ref rather
        // than folding the label and the token into one key.
        assert_eq!(
            normalize_ref("Manage: https://calendar.google.com/calendar/event?eid=SECRET"),
            None
        );
    }

    #[test]
    fn a_location_with_a_url_inside_text_loses_the_query() {
        let location = |raw: &str| {
            launder_with(|r| {
                if let RecordProposal::Event { location, .. } = r {
                    *location = Some(raw.into());
                }
            })
            .location
        };
        assert_eq!(
            location("Zoom: https://zoom.us/j/123?pwd=abc").as_deref(),
            Some("Zoom: zoom.us/j/123")
        );
        assert_eq!(
            location("Online (join at https://zoom.us/j/9?pwd=abc).").as_deref(),
            Some("Online (join at zoom.us/j/9).")
        );
        // A join token in the path goes too; the host still says where.
        assert_eq!(
            location(
                "Teams https://teams.example.com/l/meetup-join/19aB3dE5fG7hJ9kL1mN3pQ5rS7t?ctx=1"
            )
            .as_deref(),
            Some("Teams teams.example.com")
        );
        // Prose with a slash is not a URL.
        assert_eq!(
            location("Corner of Main St./Elm Ave").as_deref(),
            Some("Corner of Main St./Elm Ave")
        );
    }

    #[test]
    fn invisible_characters_leave_refs_and_titles() {
        assert_eq!(
            normalize_ref("ABC\u{200B}123\u{FEFF}").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            normalize_ref("QXJ\u{E0041}ZKM\u{2060}").as_deref(),
            Some("qxjzkm")
        );
        // Hidden text never reaches a title the timeline shows...
        assert_eq!(
            launder(&event("Din\u{200B}ner\u{E0041}\u{E0042}", &[]))
                .unwrap()
                .title,
            "Dinner"
        );
        // ...and a title of nothing else is no title: the record is dropped.
        assert!(launder(&event("\u{200B}\u{FEFF}", &[])).is_none());
        let events = launder_events(&[event("\u{200B}", &[]), event("Second", &[])]);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn start_timezone_must_be_an_iana_zone() {
        let zone = |raw: &str| {
            launder_with(|r| {
                if let RecordProposal::Event {
                    start: Some(start), ..
                } = r
                {
                    start.timezone = Some(raw.into());
                }
            })
            .start_timezone
        };
        assert_eq!(
            zone("America/New_York").as_deref(),
            Some("America/New_York")
        );
        assert_eq!(zone(" Europe/London ").as_deref(), Some("Europe/London"));
        for junk in [
            "Eastern Standard Time",
            "PDT",
            "garbage",
            "",
            "America/Nowhere",
        ] {
            assert_eq!(zone(junk), None, "{junk:?} should drop");
        }
    }

    #[test]
    fn basis_and_reason_are_capped_and_dropped_when_mail_shaped() {
        let both = |text: &str| {
            let ghost = launder_with(|r| {
                if let RecordProposal::Event {
                    duration_estimate,
                    likelihood,
                    ..
                } = r
                {
                    *duration_estimate = Some(DurationEstimate {
                        minutes: 150,
                        basis: text.into(),
                    });
                    *likelihood = Some(EventLikelihood {
                        band: LikelihoodBand::Maybe,
                        reason: text.into(),
                    });
                }
            });
            let estimate = ghost.estimate.unwrap();
            let likelihood = ghost.likelihood.unwrap();
            // The number and the band survive whatever happens to the text.
            assert_eq!(estimate.minutes, 150);
            assert_eq!(likelihood.band, LikelihoodBand::Maybe);
            assert_eq!(estimate.basis, likelihood.reason);
            estimate.basis
        };
        assert_eq!(both(" doors 7, show 8 "), "doors 7, show 8");
        assert_eq!(both(&"b".repeat(81)).chars().count(), NOTE_CAP);
        assert_eq!(both("seats 12345, row 2"), "seats 12345, row 2", "5 digits");
        for junk in [
            "see https://venue.example/faq",
            "per venue.example/faq",
            "www.venue.example says so",
            "ask box@venue.example",
            "call 555123 for times",
            "booking 1234567",
        ] {
            assert_eq!(both(junk), "", "{junk:?} should drop");
        }
    }

    // ---- Laundering, round 2 (1.5): the reviewer's table ----

    /// F8: a brand or noun around the label stays out of the key, so two
    /// mails wording one booking number differently agree. (Round 3 renamed
    /// this from `..._keys_on_its_digit_bearing_tokens_...`: the key is now
    /// one code run; every expectation here is unchanged.)
    #[test]
    fn a_ref_keys_on_its_code_run_wherever_the_labels_stand() {
        for (raw, key) in [
            ("OpenTable confirmation 123456789", "123456789"),
            ("123456789", "123456789"),
            ("Eventbrite order #1234567890", "1234567890"),
            ("Airbnb reservation HMABCD1234", "hmabcd1234"),
            ("HMABCD1234", "hmabcd1234"),
            ("Your itinerary number is 5XYZ12", "5xyz12"),
            ("Booking reference: 5XYZ12", "5xyz12"),
            ("Confirmation #: 2CF9K8", "2cf9k8"),
            ("PNR: DL7XKQ", "dl7xkq"),
            ("Record locator K7Q2LM", "k7q2lm"),
            ("ABC123", "abc123"),
            // CJK labels are dropped too: the key is the digit-bearing token.
            ("預約編號 AB12CD", "ab12cd"),
        ] {
            assert_eq!(normalize_ref(raw).as_deref(), Some(key), "{raw:?}");
        }
    }

    /// F8: a number takes the 1 to 3 capital carrier code one space before
    /// it, so the flight is one key however it is printed; a longer word
    /// does not. Round 3 (N6) changed two expectations here: "flight ua 123"
    /// is no longer `ua123` (a lowercase "ua" is not a code, so the run is
    /// "123", a fragment), and "Booking ABC-1234-XYZ" is `abc1234xyz` (the
    /// letters around the digits are part of the code).
    #[test]
    fn a_short_number_takes_the_carrier_code_before_it() {
        for raw in ["UA 123", "Flight UA123", "UA123"] {
            assert_eq!(normalize_ref(raw).as_deref(), Some("ua123"), "{raw:?}");
        }
        assert_eq!(normalize_ref("flight ua 123"), None);
        assert_eq!(normalize_ref("DL 1234").as_deref(), Some("dl1234"));
        // A label BETWEEN the carrier code and the number goes too, and a
        // label after a locator leaves the locator alone.
        assert_eq!(normalize_ref("UA flight 123").as_deref(), Some("ua123"));
        assert_eq!(
            normalize_ref("QXJZKM (confirmation code)").as_deref(),
            Some("qxjzkm")
        );
        assert_eq!(
            normalize_ref("Booking ABC-1234-XYZ").as_deref(),
            Some("abc1234xyz")
        );
        // "United" is no carrier code, and "123" alone is a fragment.
        assert_eq!(normalize_ref("United 123"), None);
    }

    /// Round 3 (N6): the key is ONE code run. The reviewer's collisions
    /// ("ABC-1234-XYZ" and "ABC-1234-QRS") are two keys, and the splits
    /// ("Order #1234567890 (2 tickets)", "Conf #: AB 12345") are one.
    #[test]
    fn a_ref_is_one_code_run() {
        for (raw, key) in [
            ("ABC-1234-XYZ", Some("abc1234xyz")),
            ("ABC-1234-QRS", Some("abc1234qrs")),
            ("QXJ-ZKM-123", Some("qxjzkm123")),
            ("ABC-ZKM-123", Some("abczkm123")),
            ("OpenTable confirmation 123456789", Some("123456789")),
            ("Airbnb reservation HMABCD1234", Some("hmabcd1234")),
            ("Order #1234567890 (2 tickets)", Some("1234567890")),
            ("Order #1234567890", Some("1234567890")),
            ("Conf #: AB 12345", Some("ab12345")),
            ("Conf AB12345", Some("ab12345")),
            ("UA 123", Some("ua123")),
            ("Flight UA123", Some("ua123")),
            ("Table 7 at 8pm", None),
            ("Booking ABC-1234-XYZ", Some("abc1234xyz")),
            // A shouted brand before a number is not a carrier code.
            ("AIRBNB HMABCD1234", Some("hmabcd1234")),
            // "E-ticket" leaves an "E" that is not joined across the space.
            ("E-ticket 0162345678901", Some("0162345678901")),
            ("Ticket number 016-2345678901", Some("0162345678901")),
            // The Google and Outlook UIDs, and the CJK label.
            (
                "7kukuqrfedlm2f9t0vr0aa8b9k@google.com",
                Some("7kukuqrfedlm2f9t0vr0aa8b9k"),
            ),
            (
                "040000008200E00074C5B7101A82E0080000000050C1",
                Some("040000008200e00074c5b7101a82e0080000000050c1"),
            ),
            ("预订号 AB12CD", Some("ab12cd")),
            ("予約番号: 1234567", Some("1234567")),
        ] {
            assert_eq!(normalize_ref(raw).as_deref(), key, "{raw:?}");
        }
    }

    /// Round 3 (N6): a clock time is never a ref or part of one, whichever
    /// way it is written.
    #[test]
    fn clock_times_are_no_ref() {
        for raw in [
            "8pm",
            "Table 7 at 8pm",
            "10:30am",
            "19:00",
            "Dinner at 8 PM",
            "8p.m.",
            "Doors 7:30 p.m.",
        ] {
            assert_eq!(normalize_ref(raw), None, "{raw:?}");
        }
        // A time beside a booking number leaves the number alone.
        assert_eq!(normalize_ref("ABC123 19:00").as_deref(), Some("abc123"));
        assert_eq!(normalize_ref("19:00 ABC-1234").as_deref(), Some("abc1234"));
        // A four-digit number is no time without a colon or am/pm.
        assert_eq!(normalize_ref("Order 1900123").as_deref(), Some("1900123"));
    }

    /// F8 and F9: what is left is too short, a bare small number, a date, or
    /// a place inside a venue.
    #[test]
    fn short_numbers_dates_and_venue_positions_are_no_ref() {
        for raw in [
            "Order 2026",
            "Order #12",
            "4471",
            "12345",
            "2026-10-03",
            "20261003",
            "Seat 12A",
            "seat12a",
            "Gate 4",
            "Row 1",
            "Table for 2",
            "Section 101",
            "Terminal 4",
            "Room 1204",
            "UBER",
            "TBD",
        ] {
            assert_eq!(normalize_ref(raw), None, "{raw:?} should drop");
        }
        // Six digits is a booking number; an 8-digit non-date is too.
        assert_eq!(normalize_ref("Order 447112").as_deref(), Some("447112"));
        assert_eq!(normalize_ref("20261399").as_deref(), Some("20261399"));
        // A booking number survives the seat printed beside it.
        assert_eq!(
            normalize_ref("Order 12345678, Seat 12A").as_deref(),
            Some("12345678")
        );
    }

    /// F7: a URL glued to its label, or wrapped in `<...>` mid-token, is
    /// still a URL, so its query never reaches a key or a location.
    #[test]
    fn a_url_glued_to_a_label_loses_its_query() {
        assert_eq!(
            normalize_ref("Event page:https://x.example/e/1?token=SECRET").as_deref(),
            Some("x.example/e/1")
        );
        assert_eq!(
            normalize_ref("Join<https://x.example/e/1?token=SECRET>").as_deref(),
            Some("x.example/e/1")
        );
        let location = |raw: &str| {
            launder_with(|r| {
                if let RecordProposal::Event { location, .. } = r {
                    *location = Some(raw.into());
                }
            })
            .location
        };
        assert_eq!(
            location("Zoom:https://zoom.us/j/81234567890?pwd=abcDEF123").as_deref(),
            Some("Zoom:zoom.us/j/81234567890")
        );
        assert_eq!(
            location("Join Zoom Meeting<https://zoom.us/j/81234567890?pwd=abcDEF123>").as_deref(),
            Some("Join Zoom Meeting<zoom.us/j/81234567890>")
        );
        assert_eq!(
            location("Room:WWW.x.example/e/1?k=SECRET").as_deref(),
            Some("Room:x.example/e/1")
        );
        // `www.` inside a word is not a URL.
        assert_eq!(location("Awww.cafe").as_deref(), Some("Awww.cafe"));
    }

    /// Round 3 (N6): an event id in a URL path is case-sensitive, so two ids
    /// differing only in case are two keys. The host is still lowercased.
    #[test]
    fn a_url_ref_keeps_the_case_of_its_path() {
        assert_eq!(
            normalize_ref("https://partiful.com/e/AbCdEf1234567").as_deref(),
            Some("partiful.com/e/AbCdEf1234567")
        );
        assert_ne!(
            normalize_ref("https://partiful.com/e/AbCdEf1234567"),
            normalize_ref("https://partiful.com/e/aBcDeF1234567")
        );
        assert_eq!(
            normalize_ref("HTTPS://WWW.Lu.Ma/event/evt-AbCdEf123").as_deref(),
            Some("lu.ma/event/evt-AbCdEf123")
        );
    }

    /// Round 3 (N7): a bare host followed straight by a query or fragment is
    /// a URL too, so the query never survives, with or without a path; and
    /// `;` parameters on a path segment go like a query.
    #[test]
    fn a_bare_host_with_a_query_loses_the_query() {
        let location = |raw: &str| {
            launder_with(|r| {
                if let RecordProposal::Event { location, .. } = r {
                    *location = Some(raw.into());
                }
            })
            .location
        };
        for (raw, laundered) in [
            ("Zoom: zoom.us?pwd=abc", "Zoom: zoom.us"),
            ("zoom.us?pwd=abc", "zoom.us"),
            ("Join: example.com?token=SECRET", "Join: example.com"),
            ("zoom.us#pwd=abc", "zoom.us"),
            ("zoom.us/j/1#pwd=x", "zoom.us/j/1"),
            ("https://zoom.us/j/1;pwd=x", "zoom.us/j/1"),
            ("Room: zoom.us/j/1;pwd=x?a=b", "Room: zoom.us/j/1"),
            // Prose keeps its punctuation.
            ("Is it at the Elm St. venue?", "Is it at the Elm St. venue?"),
            ("Corner of Main St./Elm Ave", "Corner of Main St./Elm Ave"),
        ] {
            assert_eq!(location(raw).as_deref(), Some(laundered), "{raw:?}");
        }
        // As a ref the bare host names no event, so it drops; a path
        // parameter never reaches a key.
        assert_eq!(normalize_ref("Join: example.com?token=SECRET"), None);
        assert_eq!(
            normalize_ref("https://x.example/e/12345;jsessionid=SECRET").as_deref(),
            Some("x.example/e/12345")
        );
    }

    /// Round 3 (N4): consumer ISPs, the mail.com family, duck.com and
    /// Apple's relay give strangers one domain, so each address is its own
    /// scope. The relay is matched by host: `appleid.com`, its registrable
    /// domain, is still Apple's organisation.
    #[test]
    fn isps_relays_and_the_mail_com_family_scope_by_address() {
        for domain in [
            "comcast.net",
            "verizon.net",
            "att.net",
            "sbcglobal.net",
            "btinternet.com",
            "orange.fr",
            "t-online.de",
            "libero.it",
            "optusnet.com.au",
            "aol.com",
            "aim.com",
            "mail.com",
            "email.com",
            "usa.com",
            "post.com",
            "engineer.com",
            "duck.com",
            "privaterelay.appleid.com",
        ] {
            let alice = sender_scope(&format!("alice@{domain}"), Some(true));
            let bob = sender_scope(&format!("bob@{domain}"), Some(true));
            assert_eq!(alice, Some(format!("alice@{domain}")), "{domain}");
            assert_ne!(alice, bob, "{domain}");
        }
        assert_eq!(
            sender_scope("x@PrivateRelay.AppleID.com", Some(true)).as_deref(),
            Some("x@privaterelay.appleid.com")
        );
        assert_eq!(
            sender_scope("no_reply@email.appleid.com", Some(true)).as_deref(),
            Some("appleid.com")
        );
    }

    /// The note filter counts digits across the separators a phone number
    /// is printed with.
    #[test]
    fn a_phone_number_split_by_separators_drops_the_note() {
        let basis = |text: &str| {
            launder_with(|r| {
                if let RecordProposal::Event {
                    duration_estimate, ..
                } = r
                {
                    *duration_estimate = Some(DurationEstimate {
                        minutes: 150,
                        basis: text.into(),
                    });
                }
            })
            .estimate
            .unwrap()
            .basis
        };
        for junk in ["call 555-1234", "(415) 555 1234", "+1 415.555.1234"] {
            assert_eq!(basis(junk), "", "{junk:?} should drop");
        }
        for kept in ["doors 7, show 8", "seats 12345, row 2", "2 - 3 hours"] {
            assert_eq!(basis(kept), kept);
        }
    }

    /// Only a place zone is kept: a fixed abbreviation, UTC and `Etc/` are
    /// dropped.
    #[test]
    fn start_timezone_must_be_a_place() {
        let zone = |raw: &str| {
            launder_with(|r| {
                if let RecordProposal::Event {
                    start: Some(start), ..
                } = r
                {
                    start.timezone = Some(raw.into());
                }
            })
            .start_timezone
        };
        for junk in [
            "EST",
            "UTC",
            "GMT",
            "PDT",
            "EST5EDT",
            "Etc/GMT+5",
            "Etc/UTC",
        ] {
            assert_eq!(zone(junk), None, "{junk:?} should drop");
        }
        assert_eq!(zone("Asia/Kolkata").as_deref(), Some("Asia/Kolkata"));
    }

    /// F12: a travel ghost has no default, and no history tier either.
    #[test]
    fn travel_never_takes_a_history_prior() {
        let start = at("2026-10-03T08:00:00-04:00");
        assert_eq!(
            effective_end(&start, EventRecordKind::Travel, None, None, Some(780), None),
            None
        );
        let model = GhostEstimate {
            minutes: 300,
            basis: String::new(),
        };
        assert_eq!(
            effective_end(
                &start,
                EventRecordKind::Travel,
                None,
                None,
                Some(780),
                Some(&model)
            )
            .map(|e| e.source),
            Some(EndSource::Model)
        );
    }
    /// An event record of `kind` with these raw start and end values.
    fn raw(
        kind: EventRecordKind,
        start: Option<(&str, Option<&str>)>,
        end: Option<(&str, Option<&str>)>,
    ) -> RecordProposal {
        let t = |(value, zone): (&str, Option<&str>)| SupportedTime {
            value: value.into(),
            timezone: zone.map(Into::into),
            source_message_id: 7,
            interpreted_relative: false,
        };
        let mut record = event("Stay", &["H1"]);
        if let RecordProposal::Event {
            start: s,
            end: e,
            event_kind,
            duration_estimate,
            ..
        } = &mut record
        {
            *s = start.map(t);
            *e = end.map(t);
            *event_kind = kind;
            *duration_estimate = None;
        }
        record
    }
    /// [`raw`] with the model's duration guess.
    fn guessed(record: RecordProposal, minutes: i64) -> RecordProposal {
        let mut record = record;
        if let RecordProposal::Event {
            duration_estimate, ..
        } = &mut record
        {
            *duration_estimate = Some(DurationEstimate {
                minutes,
                basis: "a guess".into(),
            });
        }
        record
    }
    fn utc(raw: &str) -> Option<DateTime<Utc>> {
        Some(
            DateTime::parse_from_rfc3339(raw)
                .unwrap()
                .with_timezone(&Utc),
        )
    }
    const NY: Option<&str> = Some("America/New_York");

    #[test]
    fn record_latest_counts_an_end_at_another_precision() {
        // A timed check-in and a date-only checkout: the checkout, as the
        // end of the 16th everywhere.
        let stay = raw(
            EventRecordKind::Reservation,
            Some(("2026-09-14T15:00", NY)),
            Some(("2026-09-16", None)),
        );
        assert_eq!(record_latest(&stay), utc("2026-09-17T12:00:00Z"));
        // And the other way round: a date-only start, a timed end.
        let show = raw(
            EventRecordKind::Ticket,
            Some(("2026-09-14", None)),
            Some(("2026-09-16T23:00", NY)),
        );
        assert_eq!(record_latest(&show), utc("2026-09-17T03:00:00Z"));
    }

    #[test]
    fn record_latest_reads_the_end_in_its_own_zone_or_the_starts() {
        let flight = raw(
            EventRecordKind::Travel,
            Some(("2026-10-19T21:30", Some("Asia/Tokyo"))),
            Some(("2026-10-19T14:45", Some("America/Los_Angeles"))),
        );
        assert_eq!(record_latest(&flight), utc("2026-10-19T21:45:00Z"));
        let train = raw(
            EventRecordKind::Travel,
            Some(("2026-10-19T09:00", NY)),
            Some(("2026-10-19T13:00", None)),
        );
        assert_eq!(record_latest(&train), utc("2026-10-19T17:00:00Z"));
    }

    #[test]
    fn record_latest_ignores_an_end_before_the_start() {
        let ticket = raw(
            EventRecordKind::Ticket,
            Some(("2026-10-03T20:00", NY)),
            Some(("2026-10-03T18:00", NY)),
        );
        assert_eq!(record_latest(&ticket), utc("2026-10-04T00:00:00Z"));
        // Travel with no usable end lands within a day of leaving.
        let flight = raw(
            EventRecordKind::Travel,
            Some(("2026-10-03T20:00", NY)),
            Some(("2026-10-03T18:00", NY)),
        );
        assert_eq!(record_latest(&flight), utc("2026-10-05T00:00:00Z"));
        let open_ended = raw(
            EventRecordKind::Travel,
            Some(("2026-10-03T20:00", NY)),
            None,
        );
        assert_eq!(record_latest(&open_ended), utc("2026-10-05T00:00:00Z"));
    }

    #[test]
    fn record_latest_needs_a_start_that_resolves() {
        let no_start = raw(
            EventRecordKind::Travel,
            None,
            Some(("2026-10-03T20:00", NY)),
        );
        assert_eq!(record_latest(&no_start), None);
        // An abbreviation names several offsets: no start, whatever the end.
        let abbreviated = raw(
            EventRecordKind::Travel,
            Some(("2026-12-20T10:00", Some("PST"))),
            Some(("2026-12-20T18:00", NY)),
        );
        assert_eq!(record_latest(&abbreviated), None);
    }

    #[test]
    fn record_latest_takes_the_raw_guess_up_to_a_month() {
        // Ten days, past the calendar's three-day clamp.
        let trip = guessed(
            raw(
                EventRecordKind::Travel,
                Some(("2026-10-03T15:00", NY)),
                None,
            ),
            14_400,
        );
        assert_eq!(record_latest(&trip), utc("2026-10-13T19:00:00Z"));
        // A short guess on travel still lands no sooner than a day after.
        let hop = guessed(
            raw(
                EventRecordKind::Travel,
                Some(("2026-10-03T15:00", NY)),
                None,
            ),
            90,
        );
        assert_eq!(record_latest(&hop), utc("2026-10-04T19:00:00Z"));
        // A year is believed for a month.
        let year = guessed(
            raw(
                EventRecordKind::Ticket,
                Some(("2026-10-03T15:00", NY)),
                None,
            ),
            525_600,
        );
        assert_eq!(record_latest(&year), utc("2026-11-02T19:00:00Z"));
        // A guess past a stated end wins; zero and less is no guess.
        let show = guessed(
            raw(
                EventRecordKind::Ticket,
                Some(("2026-10-03T15:00", NY)),
                Some(("2026-10-03T16:00", NY)),
            ),
            240,
        );
        assert_eq!(record_latest(&show), utc("2026-10-03T23:00:00Z"));
        let none = guessed(
            raw(
                EventRecordKind::Ticket,
                Some(("2026-10-03T15:00", NY)),
                None,
            ),
            0,
        );
        assert_eq!(record_latest(&none), utc("2026-10-03T19:00:00Z"));
        let negative = guessed(
            raw(
                EventRecordKind::Ticket,
                Some(("2026-10-03T15:00", NY)),
                None,
            ),
            -60,
        );
        assert_eq!(record_latest(&negative), utc("2026-10-03T19:00:00Z"));
    }

    #[test]
    fn record_latest_cannot_date_a_stay_with_no_checkout() {
        let stay = raw(
            EventRecordKind::Reservation,
            Some(("2026-10-03T15:00", NY)),
            None,
        );
        assert_eq!(record_latest(&stay), None);
        let all_day = raw(
            EventRecordKind::Reservation,
            Some(("2026-10-03", None)),
            None,
        );
        assert_eq!(record_latest(&all_day), None);
        // With a guess, or a checkout, it can.
        assert_eq!(
            record_latest(&guessed(stay.clone(), 120)),
            utc("2026-10-03T21:00:00Z")
        );
        let booked = raw(
            EventRecordKind::Reservation,
            Some(("2026-10-03T15:00", NY)),
            Some(("2026-10-05", None)),
        );
        assert_eq!(record_latest(&booked), utc("2026-10-06T12:00:00Z"));
    }

    #[test]
    fn record_latest_counts_no_guess_from_a_date() {
        let day = guessed(
            raw(EventRecordKind::Ticket, Some(("2026-10-03", None)), None),
            600,
        );
        assert_eq!(record_latest(&day), utc("2026-10-04T12:00:00Z"));
        let stay = guessed(
            raw(
                EventRecordKind::Reservation,
                Some(("2026-10-03", None)),
                None,
            ),
            4_320,
        );
        assert_eq!(record_latest(&stay), None);
    }
}
