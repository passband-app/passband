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

/// Cap on every free-text field the model writes: title, basis, reason,
/// location.
pub const TEXT_CAP: usize = 200;
/// Any estimated duration, in minutes: a quarter hour to three days. Three
/// days covers a festival; anything longer is a model error, not an event.
pub const ESTIMATE_MINUTES: RangeInclusive<u32> = 15..=4320;
const TIMEZONE_CAP: usize = 64;
const VENUE_CAP: usize = 120;
/// Identity refs kept per record. A real event has one or two (a booking
/// number, a UID, a page); a record carrying dozens is a digest or an attack,
/// and every ref is one more ghost the record could be matched against.
pub const REFS_PER_RECORD: usize = 5;
/// Shorter than 4 is a fragment ("12", "A1") that would merge strangers'
/// events; longer than 200 is a body excerpt, not a reference.
const REF_LEN: RangeInclusive<usize> = 4..=200;

/// Words a model prints in front of a reference ("Confirmation #", "Booking
/// ref"). Stripped from the front so "Confirmation ABC123" and "ABC123" from
/// the modification mail are one key.
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
    "id",
    "uid",
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
    let title = clean_text(title)?;
    let start_time = start.as_ref().and_then(|t| EventTime::parse(&t.value));
    let start_timezone = start
        .as_ref()
        .filter(|_| start_time.is_some())
        .and_then(|t| t.timezone.as_deref())
        .map(|zone| truncate_trimmed(&plain(zone), TIMEZONE_CAP))
        .filter(|zone| !zone.is_empty());
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
                basis: truncate_trimmed(&plain(&d.basis), TEXT_CAP),
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
            reason: truncate_trimmed(&plain(&l.reason), TEXT_CAP),
        }),
    })
}

/// Every laundered `event` record of a decision, in order. A record's index in
/// THIS list is its `position`, the second half of its identity in
/// `ghost_event_sources`: storage assigns and reloads records by it, so both
/// sides must count the same way, and they do by both calling this.
pub fn launder_events(records: &[RecordProposal]) -> Vec<GhostProposal> {
    records.iter().filter_map(launder).collect()
}

/// Who a record speaks for: the sender's registrable domain, or the full
/// address on a freemail domain (two strangers share `gmail.com`). None when
/// the message is not AUTHENTICATED (`auth_pass` is anything but
/// `Some(true)`, unknown included) or the address yields no domain: such a
/// record speaks for nobody, so it never matches another ghost and never
/// commits one.
///
/// Reuses `receipt_match`'s definition of a sender, the one the bill and
/// shipment matchers use, rather than a second domain parser.
pub fn sender_scope(from_addr: &str, auth_pass: Option<bool>) -> Option<String> {
    if auth_pass != Some(true) {
        return None;
    }
    let address = from_addr.trim().to_lowercase();
    let domain = super::receipt_match::registrable_domain(&address)?;
    Some(if super::receipt_match::is_freemail_domain(&domain) {
        address
    } else {
        domain
    })
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
/// A URL becomes lowercase host and path, with the query and fragment (where
/// tokens live) gone, and a bare site with no path is dropped: every event on
/// that platform would share it. Anything else becomes its lowercase
/// alphanumerics with any leading label words removed. Prose is dropped.
pub fn normalize_ref(raw: &str) -> Option<String> {
    let raw = plain(raw);
    let raw = raw.trim();
    let key = if looks_like_url(raw) {
        let (host, path) = url_host_path(raw)?;
        if path.is_empty() {
            return None;
        }
        format!("{host}{path}").to_lowercase()
    } else {
        let mut tokens: Vec<&str> = raw
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .collect();
        while tokens
            .first()
            .is_some_and(|t| LABEL_WORDS.contains(&t.to_lowercase().as_str()))
        {
            tokens.remove(0);
        }
        if tokens.is_empty() || is_bare_word(&tokens) {
            return None;
        }
        tokens.concat().to_lowercase()
    };
    REF_LEN.contains(&key.chars().count()).then_some(key)
}

/// Letters only and not a record locator. A locator ("QXJZKM") is one token
/// printed in capitals; anything else made only of letters is prose.
fn is_bare_word(tokens: &[&str]) -> bool {
    if tokens.iter().any(|t| t.chars().any(char::is_numeric)) {
        return false;
    }
    let [only] = tokens else {
        return true;
    };
    !only.chars().all(char::is_uppercase) || STOP_WORDS.contains(&only.to_lowercase().as_str())
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

/// A location is display text, except that a URL (a meeting link) keeps only
/// host and path: a query string is where a meeting passcode lives.
fn launder_location(raw: &str) -> Option<String> {
    let text = plain(raw);
    if looks_like_url(text.trim()) {
        let (host, path) = url_host_path(text.trim())?;
        return clean_text(&format!("{host}{path}"));
    }
    clean_text(&text)
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
                "lu.ma/AbCd",
            ],
        ))
        .unwrap();
        assert_eq!(
            identity_keys(&ghost, Some("eventbrite.com")),
            vec![
                "ref:eventbrite.com:eventbrite.com/e/12345",
                "ref:eventbrite.com:lu.ma/abcd"
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
        // Kept: a record locator, a number, an iCalendar UID.
        assert_eq!(normalize_ref("QXJZKM").as_deref(), Some("qxjzkm"));
        assert_eq!(normalize_ref("Order No. 4471").as_deref(), Some("4471"));
        assert_eq!(
            normalize_ref("7kq3abc@google.com").as_deref(),
            Some("7kq3abcgooglecom")
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
            TEXT_CAP
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
        assert_eq!(likelihood.reason.len(), TEXT_CAP);

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
}
