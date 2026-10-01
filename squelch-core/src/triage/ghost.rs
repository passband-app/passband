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
    EventChange, EventCommitment, EventLikelihood, EventRecordKind, RecordProposal,
};
use super::order_link::plain;
use super::text::{truncate_chars, truncate_trimmed};
use chrono::{DateTime, Duration, FixedOffset, NaiveDate};
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
    /// RFC3339 or `YYYY-MM-DD`, the two shapes a `SupportedTime` may hold
    /// (the same test decision validation applies to other times). Anything
    /// else is None.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if let Ok(at) = DateTime::parse_from_rfc3339(raw) {
            return Some(Self::At(at));
        }
        NaiveDate::parse_from_str(raw, "%Y-%m-%d")
            .ok()
            .map(Self::Date)
    }
    pub fn has_clock(&self) -> bool {
        matches!(self, Self::At(_))
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
    let start_time = start.as_ref().and_then(|t| EventTime::parse(&t.value));
    let start_timezone = start
        .as_ref()
        .filter(|_| start_time.is_some())
        .and_then(|t| t.timezone.as_deref())
        .and_then(iana_zone);
    // An end with no start to be after is meaningless, so it goes too.
    let end = end
        .as_ref()
        .and_then(|t| EventTime::parse(&t.value))
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
    records
        .iter()
        .filter_map(launder)
        .take(EVENTS_PER_MESSAGE)
        .collect()
}

/// Who a record speaks for: the sender's [`organisation_domain`], or the full
/// lowercased address where the domain is shared by strangers (freemail, a
/// shared tenant). None when the message is not AUTHENTICATED (`auth_pass` is
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
/// `gmx.de`: see `receipt_match::is_freemail_domain`) or a SHARED TENANT host
/// (`googlegroups.com`), where the domain says nothing about who sent the mail
/// and only the full address identifies a sender. None too when there is no
/// registrable domain at all (an empty or one-label domain, or a domain that
/// is itself a public suffix). Takes an address or a bare domain. Two
/// addresses are the same organisation only when this is Some and equal for
/// both.
///
/// Shared by everything that asks "same sender organisation?" on
/// authenticated mail (the ghost scope here, the invitation binding in the
/// correspondent-timezone code), so they cannot drift apart.
pub(crate) fn organisation_domain(address: &str) -> Option<String> {
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

/// A ref written as text, as the key two mails worded differently agree on.
///
/// Split into tokens on anything not alphanumeric. Label words go wherever
/// they stand (a brand before the label stays out of the key too, as the
/// digit rule below leaves it behind), and so does a position in a venue
/// ("Seat 12A", "gate4") with its number. Then:
///
/// - With a token carrying a digit, the key is the digit-bearing tokens
///   joined: "OpenTable confirmation 123456789" and "123456789" agree. A key
///   that short (under 5) takes the 1 to 3 letter token just before its
///   first digit-bearing token, so "UA 123" and "Flight UA123" are both
///   `ua123`.
/// - With none, only a single all-capitals token that is not a status word
///   is a record locator ("QXJZKM"); anything else made of letters is prose.
///
/// Dropped as identifying nothing: an all-digit key under 6 digits (an order
/// line, a year) and an 8-digit key that is a valid `YYYYMMDD` date.
fn text_ref(raw: &str) -> Option<String> {
    let has_digit = |t: &str| t.chars().any(char::is_numeric);
    let mut tokens: Vec<&str> = Vec::new();
    let mut after_position = false;
    for token in raw
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
    {
        let lower = token.to_lowercase();
        if LABEL_WORDS.contains(&lower.as_str()) {
            continue;
        }
        if POSITION_WORDS.contains(&lower.as_str()) {
            after_position = true;
            continue;
        }
        if std::mem::take(&mut after_position) && has_digit(token) {
            continue;
        }
        let glued_position = POSITION_WORDS.iter().any(|w| {
            lower
                .strip_prefix(w)
                .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        });
        if !glued_position {
            tokens.push(token);
        }
    }
    let key = match tokens.iter().position(|t| has_digit(t)) {
        Some(first) => {
            let mut key: String = tokens
                .iter()
                .filter(|t| has_digit(t))
                .map(|t| t.to_lowercase())
                .collect();
            if key.chars().count() < *REF_LEN.start()
                && let Some(before) = first.checked_sub(1).map(|i| tokens[i])
                && (1..=3).contains(&before.chars().count())
                && before.chars().all(char::is_alphabetic)
            {
                key = format!("{}{key}", before.to_lowercase());
            }
            key
        }
        None => {
            let [only] = tokens.as_slice() else {
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

/// A URL ref as lowercase host and path, kept only when the path itself
/// names one event. The query and fragment are gone (that is where tokens
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
    names_one.then(|| format!("{host}{path}").to_lowercase())
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
/// missing it kept its query, passcode and all. A bare `host/path` must start
/// the token and needs an alphabetic top-level label, so "St./Elm" and
/// "2.5/3h" stay text.
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
    let tld_ok = core
        .split_once('/')
        .and_then(|(host, _)| host.rsplit('.').next())
        .is_some_and(|tld| tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()));
    (tld_ok && looks_like_url(core)).then_some(core)
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
    // "lu.ma/abc": a dotted host then a path. An iCalendar UID has an '@'.
    !raw.contains(char::is_whitespace)
        && !raw.contains('@')
        && raw
            .split_once('/')
            .is_some_and(|(host, _)| host.contains('.') && !host.starts_with('.'))
}

/// Host without `www.`, and path without its trailing slash. Query, fragment
/// and credentials in the authority are never returned.
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
    let path = url.path().trim_end_matches('/').to_string();
    Some((host, path))
}

fn clean_text(raw: &str) -> Option<String> {
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
fn launder_location(raw: &str) -> Option<String> {
    let text = map_urls(&plain(raw), |url| {
        let (host, path) = url_host_path(url)?;
        if path.split('/').any(credential_shaped) {
            return Some(host);
        }
        Some(format!("{host}{path}"))
    });
    clean_text(&text)
}

/// The zone a start claims, as an IANA PLACE zone, or None: a valid name with
/// a `/` that is not under `Etc/` ([`crate::timezone::place_zone`], the
/// correspondent-timezone code's own test). "Eastern Standard Time", "PDT",
/// "EST" and "UTC" are not places: an abbreviation names several, and a fixed
/// offset is wrong for half the year wherever it is read.
fn iana_zone(raw: &str) -> Option<String> {
    let zone = plain(raw);
    if zone.chars().count() > TIMEZONE_CAP || !zone.contains('/') {
        return None;
    }
    crate::timezone::place_zone(&zone).map(|tz| tz.name().to_string())
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
        assert_eq!(
            identity_keys(&ghost, Some("eventbrite.com")),
            vec![
                "ref:eventbrite.com:eventbrite.com/e/12345",
                "ref:eventbrite.com:lu.ma/abcd7"
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
    /// mails wording one booking number differently agree.
    #[test]
    fn a_ref_keys_on_its_digit_bearing_tokens_wherever_the_labels_stand() {
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

    /// F8: a short number takes the 1 to 3 letter carrier code before it, so
    /// the flight is one key however it is printed; a longer word does not.
    #[test]
    fn a_short_number_takes_the_carrier_code_before_it() {
        for raw in ["UA 123", "Flight UA123", "UA123", "flight ua 123"] {
            assert_eq!(normalize_ref(raw).as_deref(), Some("ua123"), "{raw:?}");
        }
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
            Some("abc1234")
        );
        // "United" is no carrier code, and "123" alone is a fragment.
        assert_eq!(normalize_ref("United 123"), None);
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
}
