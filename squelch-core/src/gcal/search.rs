//! Calendar search (docs/CALENDAR.md §10.1): real events and ghosts that
//! match a search box query, in time order. One function, [`search`], serves
//! all three doors: the human search box, the in-app assistant and Agent MCP.
//! What differs by door is the [`Audience`], which decides whether anyone's
//! address is matched, and the view each door serves.
//!
//! No index. The store returns every row containing at least one term (a
//! substring superset), and the exact word test runs here.
use super::{SyncedEvent, match_ghosts};
use crate::owner_clock::OwnerClock;
use crate::store::{SqliteStore, parse_search_query};
use crate::triage::ghost::EventTime;
use crate::types::{AccountId, GhostEvent, GhostUserState};
use crate::{Result, owner_clock};
use chrono::{DateTime, FixedOffset, NaiveDate, Offset, Utc};
use serde::Serialize;

/// Hits one search returns at most.
pub const MAX_LIMIT: usize = 50;

/// Words too common to ask of an event. Dropped unless the query is nothing
/// else, so "dinner with sam at nopa" asks for dinner, sam and nopa.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "at", "for", "in", "of", "on", "or", "the", "to", "with", "my", "me",
];

/// Who is asking. The agent door never matches or sees an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// The owner's own search box.
    Owner,
    /// The in-app assistant and Agent MCP.
    Agent,
}

/// A query as calendar search reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarQuery {
    terms: Vec<String>,
    after: Option<DateTime<Utc>>,
    before: Option<DateTime<Utc>>,
}

impl CalendarQuery {
    /// None when no event can answer: a mail-only operator (`from:`, `to:`,
    /// `in:sent`, `from:me`), or no term left once operators are lifted.
    pub fn parse(raw: &str) -> Option<Self> {
        let (text, filter) = parse_search_query(raw.trim());
        if filter.from.is_some() || filter.to.is_some() || filter.from_me {
            return None;
        }
        let all: Vec<String> = words(&text).filter(|w| w.chars().count() >= 2).collect();
        let kept: Vec<String> = all
            .iter()
            .filter(|w| !STOPWORDS.contains(&w.as_str()))
            .cloned()
            .collect();
        let mut terms = if kept.is_empty() { all } else { kept };
        terms.dedup();
        if terms.is_empty() {
            return None;
        }
        Some(Self {
            terms,
            after: filter.after,
            before: filter.before,
        })
    }

    pub fn terms(&self) -> &[String] {
        &self.terms
    }

    /// What the store's prefilter looks for: each term's shortest form, the
    /// prefix every word the term can match contains. None when a term is not
    /// ASCII, because SQLite lowercases ASCII only and the prefilter would
    /// then miss "Über" for "über"; the caller reads every row instead.
    pub fn needles(&self) -> Option<Vec<String>> {
        if !self.terms.iter().all(|t| t.is_ascii()) {
            return None;
        }
        Some(
            self.terms
                .iter()
                .map(|t| forms(t).min_by_key(|f| f.len()).unwrap_or(t).to_owned())
                .collect(),
        )
    }

    /// How many terms some word of `text` answers.
    pub fn matched(&self, text: &[&str]) -> usize {
        let words: Vec<String> = text.iter().flat_map(|t| words(t)).collect();
        self.terms
            .iter()
            .filter(|term| words.iter().any(|w| term_matches(term, w)))
            .count()
    }

    /// Whether a hit with `matched` terms is a hit, given the best any
    /// candidate scored: every term, or (no event has every term, and there
    /// are two or more) all but one.
    fn keeps(&self, matched: usize, best: usize) -> bool {
        let all = self.terms.len();
        matched == all || (best < all && all >= 2 && matched + 1 == all)
    }

    /// `after:` (inclusive) and `before:` (exclusive) on the START.
    fn in_bounds(&self, start: DateTime<Utc>) -> bool {
        self.after.is_none_or(|a| start >= a) && self.before.is_none_or(|b| start < b)
    }
}

/// Lowercase alphanumeric runs.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
}

/// A word and its forms with a plural `s` or `es` taken off, none shorter
/// than two characters.
fn forms(word: &str) -> impl Iterator<Item = &str> {
    let s = word.strip_suffix('s').filter(|_| !word.ends_with("ss"));
    let es = word.strip_suffix("es");
    std::iter::once(word)
        .chain(s)
        .chain(es)
        .filter(|f| f.chars().count() >= 2)
}

/// A term answers a word that starts with it, or that agrees with it once a
/// plural is off ("dinners" answers "Dinner").
fn term_matches(term: &str, word: &str) -> bool {
    word.starts_with(term) || forms(term).any(|t| forms(word).any(|w| w == t))
}

/// One hit: a real event or a ghost.
#[derive(Debug, Clone)]
pub enum CalendarHit {
    Event(SyncedEvent),
    Ghost(GhostEvent),
}

impl CalendarHit {
    fn start(&self) -> &EventTime {
        match self {
            Self::Event(e) => &e.row.start,
            Self::Ghost(g) => &g.start,
        }
    }

    fn end(&self) -> Option<EventTime> {
        match self {
            Self::Event(e) => e.row.end,
            Self::Ghost(g) => g.effective_end().map(|e| e.end),
        }
    }
}

/// What a search found, ordered, after the limit.
#[derive(Debug, Clone, Default)]
pub struct CalendarHits {
    pub hits: Vec<CalendarHit>,
    /// Hits before the limit.
    pub total: usize,
}

/// A time as an instant, a date at the owner's midnight.
fn instant(time: &EventTime, offset: &FixedOffset) -> DateTime<Utc> {
    match time {
        EventTime::At(at) => at.with_timezone(&Utc),
        EventTime::Date(day) => day
            .and_time(chrono::NaiveTime::MIN)
            .and_local_timezone(*offset)
            .single()
            .map(|at| at.with_timezone(&Utc))
            .unwrap_or_else(|| day.and_time(chrono::NaiveTime::MIN).and_utc()),
    }
}

/// Whether a hit is still ahead or under way: its end (or start) is not yet
/// past. A date is past once the owner's day is after it.
fn upcoming(
    start: &EventTime,
    end: Option<&EventTime>,
    now: DateTime<Utc>,
    today: NaiveDate,
) -> bool {
    match end.unwrap_or(start) {
        EventTime::At(at) => at.with_timezone(&Utc) >= now,
        EventTime::Date(day) => *day >= today,
    }
}

/// Upcoming hits first, soonest first, then past hits, most recent first.
pub fn order(hits: &mut [CalendarHit], now: DateTime<Utc>, today: NaiveDate, offset: &FixedOffset) {
    hits.sort_by_cached_key(|hit| {
        let at = instant(hit.start(), offset);
        let ahead = upcoming(hit.start(), hit.end().as_ref(), now, today);
        let key = if ahead {
            at.timestamp()
        } else {
            -at.timestamp()
        };
        (!ahead, key)
    });
}

/// Run a calendar search. `events` says whether real events are read (the
/// caller's grant check: a grant taken back hides what was synced under it).
/// Ghosts the user dismissed, and ghosts the timeline would show as a real
/// event, are left out. For [`Audience::Agent`] a ghost is kept only with
/// sources whose threads the agent door may open, and only when its supplier
/// is one of them.
pub fn search(
    store: &SqliteStore,
    account: AccountId,
    query: &CalendarQuery,
    audience: Audience,
    events: bool,
    now: DateTime<Utc>,
    limit: usize,
) -> Result<CalendarHits> {
    let zone = store
        .owner_zone(account)
        .unwrap_or(owner_clock::OwnerZone::UTC);
    let clock = OwnerClock::new(now, zone);
    let today = clock.today();
    let offset = clock.local_now().offset().fix();
    let needles = query.needles();
    let agent = audience == Audience::Agent;

    let event_rows: Vec<SyncedEvent> = if events {
        match &needles {
            Some(needles) => store.calendar_search_candidates(account, needles, !agent)?,
            // Non-ASCII: every row is a candidate (an empty needle is in
            // every text).
            None => store.calendar_search_candidates(account, &[String::new()], !agent)?,
        }
    } else {
        Vec::new()
    };
    let scored_events: Vec<(usize, SyncedEvent)> = event_rows
        .into_iter()
        // A declined event is not on the owner's time; every client hides
        // it, so serving it would make `total` count rows nobody draws.
        .filter(|e| e.row.self_response != Some(super::SelfResponse::Declined))
        .filter(|e| query.in_bounds(instant(&e.row.start, &offset)))
        .map(|e| {
            let mut text: Vec<&str> = vec![
                e.row.summary.as_deref().unwrap_or(""),
                e.row.location.as_deref().unwrap_or(""),
                e.row.description.as_deref().unwrap_or(""),
                e.calendar_name.as_deref().unwrap_or(""),
            ];
            for g in &e.row.guests {
                text.push(g.name.as_deref().unwrap_or(""));
                if !agent {
                    text.push(&g.email);
                }
            }
            if !agent {
                text.push(e.row.organizer_email.as_deref().unwrap_or(""));
            }
            (query.matched(&text), e)
        })
        .collect();

    let far = chrono::Duration::days(365 * 20);
    let from = (now - far).fixed_offset();
    let to = (now + far).fixed_offset();
    let ghost_rows = store.ghost_events_containing(account, from, to, needles.as_deref())?;
    let mut scored_ghosts: Vec<(usize, GhostEvent)> = Vec::new();
    for mut ghost in ghost_rows {
        if ghost.user_state == Some(GhostUserState::Dismissed) {
            continue;
        }
        if !query.in_bounds(instant(&ghost.start, &offset)) {
            continue;
        }
        let matched = query.matched(&[&ghost.title, ghost.location.as_deref().unwrap_or("")]);
        if matched == 0 {
            continue;
        }
        if agent {
            let mut kept = Vec::with_capacity(ghost.sources.len());
            for source in std::mem::take(&mut ghost.sources) {
                if store.external_thread_allowed(account, &source.thread_id)? {
                    kept.push(source);
                }
            }
            if !kept
                .iter()
                .any(|s| Some(s.message_id) == ghost.supplier_message_id)
            {
                continue;
            }
            ghost.sources = kept;
        }
        scored_ghosts.push((matched, ghost));
    }

    let best = scored_events
        .iter()
        .map(|(m, _)| *m)
        .chain(scored_ghosts.iter().map(|(m, _)| *m))
        .max()
        .unwrap_or(0);
    let kept_events: Vec<SyncedEvent> = scored_events
        .into_iter()
        .filter(|(m, _)| query.keeps(*m, best))
        .map(|(_, e)| e)
        .collect();
    let kept_ghosts: Vec<GhostEvent> = scored_ghosts
        .into_iter()
        .filter(|(m, _)| query.keeps(*m, best))
        .map(|(_, g)| g)
        .collect();

    // A ghost the timeline would draw as a real event is that event: drop it
    // when it matches one of the account's events at its start, whether or
    // not that event matched the query.
    let mut hits: Vec<CalendarHit> = Vec::new();
    for ghost in kept_ghosts {
        let twin = events && {
            let pad = chrono::Duration::days(2);
            let at = instant(&ghost.start, &offset).fixed_offset();
            let (near, _) =
                store.list_calendar_events_capped(account, at - pad, at + pad, now, 200)?;
            match_ghosts([(ghost.title.as_str(), &ghost.start)], &near)[0].is_some()
        };
        if !twin {
            hits.push(CalendarHit::Ghost(ghost));
        }
    }
    hits.extend(kept_events.into_iter().map(CalendarHit::Event));
    order(&mut hits, now, today, &offset);
    let total = hits.len();
    hits.truncate(limit.clamp(1, MAX_LIMIT));
    Ok(CalendarHits { hits, total })
}

/// Description characters the agent door serves at most.
pub const AGENT_DESCRIPTION_CHARS: usize = 1000;

/// One hit as the agent door serves it (in-app assistant and Agent MCP).
/// SERIALIZED SHAPE IS A WIRE CONTRACT. NO ADDRESS OF ANYONE: guests by name,
/// a guest with no name only counted, the organizer by name or as the owner.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentCalendarHit {
    /// `event` (on a real calendar) or `ghost` (guessed from mail).
    pub kind: &'static str,
    pub title: Option<String>,
    pub start: String,
    pub end: Option<String>,
    /// The end is an estimate, not a stated fact (ghosts only).
    pub end_estimated: bool,
    pub all_day: bool,
    pub start_timezone: Option<String>,
    pub location: Option<String>,
    /// The calendar's name (events only).
    pub calendar: Option<String>,
    /// `confirmed` or `tentative` for an event; for a ghost what the mail
    /// says and the owner's word (`live`, `cancelled`, `going`).
    pub status: String,
    /// The owner's RSVP to an event: `accepted`, `declined`, `tentative`,
    /// `needs_action`, or null.
    pub your_response: Option<&'static str>,
    pub organizer: Option<String>,
    pub organizer_is_you: bool,
    pub guests: Vec<String>,
    pub guest_count: usize,
    pub description: Option<String>,
    /// The mail a ghost came from, for get_thread.
    pub thread_id: Option<String>,
    /// How likely a ghost is to happen: `likely`, `maybe`, `unlikely`.
    pub likelihood: Option<String>,
}

impl AgentCalendarHit {
    pub fn from_hit(hit: &CalendarHit, now: DateTime<FixedOffset>) -> Self {
        match hit {
            CalendarHit::Event(e) => {
                let row = &e.row;
                let organizer_guest = row.guests.iter().find(|g| g.organizer);
                let organizer_is_you =
                    row.organizer_self || organizer_guest.is_some_and(|g| g.is_self);
                let organizer = (!organizer_is_you)
                    .then(|| organizer_guest.and_then(|g| g.name.clone()))
                    .flatten()
                    .filter(|n| !n.trim().is_empty());
                let guests: Vec<String> = row
                    .guests
                    .iter()
                    .filter(|g| !g.is_self)
                    .filter_map(|g| g.name.clone().filter(|n| !n.trim().is_empty()))
                    .collect();
                let guest_count = row.guests.iter().filter(|g| !g.is_self).count();
                Self {
                    kind: "event",
                    title: row.summary.clone(),
                    start: row.start.to_string(),
                    end: row.end.map(|e| e.to_string()),
                    end_estimated: false,
                    all_day: row.all_day(),
                    start_timezone: row.start_timezone.clone(),
                    location: row.location.clone(),
                    calendar: e.calendar_name.clone(),
                    status: if row.tentative {
                        "tentative"
                    } else {
                        "confirmed"
                    }
                    .into(),
                    your_response: row.self_response.map(|r| r.as_str()),
                    organizer,
                    organizer_is_you,
                    guests,
                    guest_count,
                    description: row
                        .description
                        .as_deref()
                        .map(|d| d.chars().take(AGENT_DESCRIPTION_CHARS).collect()),
                    thread_id: None,
                    likelihood: None,
                }
            }
            CalendarHit::Ghost(g) => {
                let end = g.effective_end();
                let supplier = g
                    .sources
                    .iter()
                    .find(|s| Some(s.message_id) == g.supplier_message_id)
                    .or(g.sources.first());
                Self {
                    kind: "ghost",
                    title: Some(g.title.clone()),
                    start: g.start.to_string(),
                    end: end.as_ref().map(|e| e.end.to_string()),
                    end_estimated: end.as_ref().is_some_and(|e| e.estimated),
                    all_day: g.all_day(),
                    start_timezone: g.start_timezone.clone(),
                    location: g.location.clone(),
                    calendar: None,
                    status: serde_json::to_value(g.state(now))
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_default(),
                    your_response: None,
                    organizer: None,
                    organizer_is_you: false,
                    guests: Vec::new(),
                    guest_count: 0,
                    description: None,
                    thread_id: supplier.map(|s| s.thread_id.clone()),
                    likelihood: g.served_likelihood().and_then(|l| {
                        serde_json::to_value(l.band)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_owned))
                    }),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(raw: &str) -> CalendarQuery {
        CalendarQuery::parse(raw).expect("a query")
    }

    #[test]
    fn mail_operators_and_empty_queries_ask_nothing_of_the_calendar() {
        for raw in [
            "",
            "   ",
            "from:sam dinner",
            "to:dan lunch",
            "in:sent trip",
            "from:me x",
            "a",
        ] {
            assert_eq!(CalendarQuery::parse(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn stopwords_drop_unless_they_are_all_there_is() {
        assert_eq!(
            q("dinner with sam at Nopa").terms(),
            ["dinner", "sam", "nopa"]
        );
        assert_eq!(q("to the").terms(), ["to", "the"]);
    }

    #[test]
    fn a_term_matches_word_prefixes_and_plurals_both_ways() {
        assert_eq!(q("din").matched(&["Dinner w/ Sam"]), 1);
        assert_eq!(q("dinners").matched(&["Dinner w/ Sam"]), 1);
        assert_eq!(q("classes").matched(&["Pottery class"]), 1);
        assert_eq!(q("class").matched(&["Pottery classes"]), 1);
        // Inside a word is not a match: "art" is not in "party".
        assert_eq!(q("art").matched(&["Birthday party"]), 0);
        assert_eq!(q("über").matched(&["Fahrt mit ÜBER"]), 1);
    }

    #[test]
    fn every_needle_is_a_prefix_of_what_its_term_matches() {
        let query = q("dinners classes SFO");
        assert_eq!(query.needles().unwrap(), ["dinner", "class", "sfo"]);
        assert_eq!(q("über").needles(), None);
    }

    #[test]
    fn all_terms_or_all_but_one_when_nothing_has_them_all() {
        let query = q("dinner sam nopa");
        assert!(query.keeps(3, 3));
        assert!(!query.keeps(2, 3));
        assert!(query.keeps(2, 2));
        assert!(!query.keeps(1, 2));
        let one = q("dinner");
        assert!(!one.keeps(0, 0));
    }

    #[test]
    fn dates_bound_the_start() {
        let query = q("trip after:2026-11-01 before:2026-11-08");
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        assert!(query.in_bounds(at("2026-11-01T00:00:00Z")));
        assert!(!query.in_bounds(at("2026-11-08T00:00:00Z")));
        assert!(!query.in_bounds(at("2026-10-31T23:59:59Z")));
    }

    #[test]
    fn upcoming_soonest_first_then_past_most_recent_first() {
        use crate::gcal::EventRow;
        let event = |start: &str| {
            CalendarHit::Event(SyncedEvent {
                calendar_id: "c".into(),
                calendar_name: None,
                calendar_color: None,
                calendar_access: None,
                calendar_primary: true,
                row: EventRow {
                    event_id: start.into(),
                    ical_uid: None,
                    tentative: false,
                    summary: None,
                    location: None,
                    start: EventTime::parse(start).unwrap(),
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
                },
            })
        };
        let now = DateTime::parse_from_rfc3339("2026-10-28T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let today = now.date_naive();
        let utc = FixedOffset::east_opt(0).unwrap();
        let mut hits = vec![
            event("2026-09-01T10:00:00Z"),
            event("2026-11-20T10:00:00Z"),
            event("2026-10-28"),
            event("2026-10-20T10:00:00Z"),
            event("2026-11-02T10:00:00Z"),
        ];
        order(&mut hits, now, today, &utc);
        let ids: Vec<&str> = hits
            .iter()
            .map(|h| match h {
                CalendarHit::Event(e) => e.row.event_id.as_str(),
                CalendarHit::Ghost(_) => unreachable!(),
            })
            .collect();
        assert_eq!(
            ids,
            [
                "2026-10-28",
                "2026-11-02T10:00:00Z",
                "2026-11-20T10:00:00Z",
                "2026-10-20T10:00:00Z",
                "2026-09-01T10:00:00Z",
            ]
        );
    }
}
