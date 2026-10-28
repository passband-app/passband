//! PASSBAND OWNS THE EVENTS MAIL MAKES (docs/CALENDAR.md, "Passband owns
//! Gmail's events"). Gmail puts its own copy of a reservation, a flight or a
//! ticket on the user's calendar (`eventType: fromGmail`), which Google will
//! not let anyone edit, beside the ghost Passband made from the same mail.
//! Two items for one thing, one of them frozen, gets in the way of Passband
//! running the calendar. So Passband takes it over:
//!
//! - ADOPT. A Gmail event the user owns (on their primary calendar, theirs by
//!   every organizer test, no other guests) that matches exactly one live
//!   ghost is replaced: Passband creates its own event from it (same title,
//!   times, colour and free/busy, the card's cleaned place, a line saying
//!   Passband keeps it), then deletes Gmail's. If Google refuses the delete,
//!   Passband's copy is deleted again and the pair is left as Gmail made it.
//! - KEEP CURRENT. When the mail moves the ghost (a new time, a new place) or
//!   cancels it, Passband patches its event to match. A cancellation is
//!   marked, not deleted: the title says "Canceled:" and the event shows as
//!   free, which the user can undo. Only a CHANGE in what the mail says is
//!   written: an edit the user makes to Passband's event stands until the
//!   mail says something new.
//! - NO DUPLICATES. A Gmail event that turns up again for an event Passband
//!   already owns (Gmail re-adding it from a later message) is deleted.
//!
//! ONLY AUTHENTICATED MAIL. A ghost made from mail that did not pass sender
//! authentication ([`GhostEvent::verified`]) adopts nothing and moves nothing.
//!
//! WHAT IS NEVER TOUCHED. An invitation from someone else (an organizer that
//! is not the user, or any other guest), an event on a calendar other than
//! the primary one, a past event, a Gmail event no ghost or more than one
//! ghost matches, and anything the user deleted: once Passband's event for a
//! ghost is gone, that ghost is never adopted again. Every write asks Google
//! to email nobody (`sendUpdates=none`) and is audited.
//!
//! [`plan`] decides, purely, from what the store holds; the sync loop
//! carries out at most [`WRITES_PER_PASS`] steps a pass and [`DAILY_CAP`] a
//! day ([`super::sync::CalendarSync::own_events`]).

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::write::google_time;
use super::{EventRow, SyncedEvent, ghost_matches, normalize_title, same_start};
use crate::triage::ghost::EventTime;
use crate::types::{GhostEvent, GhostMailState, GhostUserState};

/// Google writes one pass carries out at most.
pub const WRITES_PER_PASS: usize = 10;
/// Google writes per account per day at most: the runaway guard.
pub const DAILY_CAP: u32 = 100;
/// The audit log's actor for these writes.
pub const AUDIT_ACTOR: &str = "passband-calendar";
/// The notes on an event Passband owns.
pub const OWNED_NOTE: &str = "Kept up to date by Passband from your mail.";
/// What a cancellation puts before the title.
pub const CANCELLED_PREFIX: &str = "Canceled: ";

/// Where a ghost stands with its event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnState {
    /// Passband's event is on the calendar and kept current.
    Owned,
    /// Passband's event went (the user deleted it) or the user dismissed the
    /// ghost: never adopted again.
    Released,
    /// Google refused to delete Gmail's copy: left as Gmail made it, never
    /// tried again.
    Refused,
}

impl OwnState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owned => "owned",
            Self::Released => "released",
            Self::Refused => "refused",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "owned" => Some(Self::Owned),
            "released" => Some(Self::Released),
            "refused" => Some(Self::Refused),
            _ => None,
        }
    }
}

/// One ghost's binding to the event Passband made for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owned {
    pub ghost_id: i64,
    pub calendar_id: String,
    pub event_id: String,
    pub state: OwnState,
    /// What the mail said when Passband last wrote the event ([`fingerprint`]).
    pub fingerprint: String,
}

/// One thing to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Create Passband's event (`body`, under `new_event_id`), then delete
    /// Gmail's.
    Adopt {
        ghost_id: i64,
        calendar_id: String,
        gmail_event_id: String,
        new_event_id: String,
        body: Value,
        fingerprint: String,
    },
    /// The mail changed: patch Passband's event (`None`: nothing Google
    /// holds changes, only the stored fingerprint does).
    Update {
        ghost_id: i64,
        calendar_id: String,
        event_id: String,
        patch: Option<Value>,
        fingerprint: String,
    },
    /// A Gmail copy of an event Passband owns: delete it.
    DropDuplicate {
        ghost_id: i64,
        calendar_id: String,
        event_id: String,
    },
    /// Stop keeping this ghost's event, and never adopt for it again.
    Release { ghost_id: i64 },
    /// The ghost is gone: forget the binding, leave the event.
    Forget { ghost_id: i64 },
}

/// What of a ghost Passband writes, as last written. Stored as JSON so an
/// update can tell which part the mail changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Facts {
    start: String,
    end: Option<String>,
    location: Option<String>,
    cancelled: bool,
}

fn facts(ghost: &GhostEvent) -> Facts {
    Facts {
        start: ghost.start.to_string(),
        end: ghost_end(ghost).map(|e| e.to_string()),
        location: ghost.location.clone(),
        cancelled: ghost.mail_state == GhostMailState::Cancelled,
    }
}

/// The ghost's facts as stored on its binding.
pub fn fingerprint(ghost: &GhostEvent) -> String {
    serde_json::to_string(&facts(ghost)).expect("facts serialize")
}

/// A ghost's end when one is known: the user's correction, else the mail's.
/// The model's estimate is a guess and is never written to a calendar.
fn ghost_end(ghost: &GhostEvent) -> Option<EventTime> {
    ghost.user_end.or(ghost.stated_end)
}

/// The id Passband's event for this ghost goes under: Google's alphabet
/// (`a`-`v` and digits), and the ghost's creation second, so a store made
/// again from nothing never reuses an id Google remembers as deleted. One id
/// per ghost makes a create sent twice find the first.
pub fn owned_event_id(ghost: &GhostEvent) -> String {
    format!("passband{}t{}", ghost.id, ghost.created_at.timestamp())
}

/// May Passband take this event over? Gmail made it, it is on the primary
/// calendar, it is the user's by every organizer test
/// ([`SyncedEvent::users_own`]), nobody else is on it, and it has not ended.
pub fn adoptable(event: &SyncedEvent, account_email: &str, now: DateTime<Utc>) -> bool {
    event.row.from_gmail
        && event.calendar_primary
        && event.users_own(account_email)
        && event.row.other_guests() == 0
        && !event.row.guests_truncated
        && !ended(&event.row, now)
}

fn ended(row: &EventRow, now: DateTime<Utc>) -> bool {
    match row.end.unwrap_or(row.start) {
        EventTime::At(at) => at.with_timezone(&Utc) <= now,
        EventTime::Date(day) => day < now.date_naive(),
    }
}

/// Words too common in mail-made titles to say two items are one thing.
const COMMON: [&str; 22] = [
    "with",
    "from",
    "your",
    "reservation",
    "reservations",
    "booking",
    "booked",
    "dinner",
    "lunch",
    "breakfast",
    "brunch",
    "event",
    "table",
    "order",
    "confirmed",
    "confirmation",
    "flight",
    "ticket",
    "tickets",
    "party",
    "meeting",
    "people",
];

fn distinctive(text: &str) -> HashSet<String> {
    normalize_title(text)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4 && !COMMON.contains(w))
        .map(str::to_string)
        .collect()
}

/// Are this ghost and this event one thing? The same start, and the titles
/// match the way the timeline's display match does ([`ghost_matches`]), or
/// they share a distinctive word across title and place ("Reservation at
/// Dingles Public House" and a ghost "Dinner at Dingles").
pub fn same_event(ghost: &GhostEvent, row: &EventRow) -> bool {
    if !same_start(&ghost.start, &row.start) {
        return false;
    }
    if ghost_matches(
        &ghost.title,
        &ghost.start,
        row.summary.as_deref(),
        &row.start,
    ) {
        return true;
    }
    let mine = distinctive(&format!(
        "{} {}",
        ghost.title,
        ghost.location.as_deref().unwrap_or_default()
    ));
    let theirs = distinctive(&format!(
        "{} {}",
        row.summary.as_deref().unwrap_or_default(),
        row.location.as_deref().unwrap_or_default()
    ));
    !mine.is_disjoint(&theirs)
}

/// The event's end, or one made for it: a timed event an hour long, an
/// all-day one its one day.
fn end_or_default(row: &EventRow) -> EventTime {
    row.end.unwrap_or(match row.start {
        EventTime::At(at) => EventTime::At(at + Duration::hours(1)),
        day => day,
    })
}

/// Passband's event in place of Gmail's `gmail`: its title, times, colour
/// and free/busy, the cleaned `place` when the card has one (`Some("")`:
/// the location names no place), else Gmail's location, and Passband's note.
pub fn adopt_body(
    ghost: &GhostEvent,
    gmail: &EventRow,
    place: Option<&str>,
    id: &str,
) -> Option<Value> {
    let mut body = Map::new();
    body.insert("id".into(), json!(id));
    body.insert(
        "summary".into(),
        json!(gmail.summary.clone().unwrap_or_else(|| ghost.title.clone())),
    );
    body.insert(
        "start".into(),
        google_time(&gmail.start, false, false).ok()?,
    );
    body.insert(
        "end".into(),
        google_time(&end_or_default(gmail), true, false).ok()?,
    );
    let location = match place {
        Some(place) => (!place.is_empty()).then(|| place.to_string()),
        None => gmail.location.clone(),
    };
    if let Some(location) = location {
        body.insert("location".into(), json!(location));
    }
    body.insert("description".into(), json!(OWNED_NOTE));
    if gmail.transparent {
        body.insert("transparency".into(), json!("transparent"));
    }
    if let Some(color) = &gmail.color_id {
        body.insert("colorId".into(), json!(color));
    }
    body.insert(
        "extendedProperties".into(),
        json!({ "private": { "passband": "owned", "passband_ghost": ghost.id.to_string() } }),
    );
    Some(Value::Object(body))
}

/// The patch that brings Passband's event `owned` in line with what the mail
/// now says, given what it said when last written (`before`, a
/// [`fingerprint`]). Only what changed is sent; None when nothing Google
/// holds changes.
pub fn update_patch(ghost: &GhostEvent, owned: &EventRow, before: &str) -> Option<Value> {
    let before: Facts = serde_json::from_str(before).unwrap_or_default();
    let now = facts(ghost);
    let mut patch = Map::new();
    if now.start != before.start || now.end != before.end {
        let start = ghost.start;
        let end = ghost_end(ghost).unwrap_or_else(|| shifted_end(owned, start));
        if let (Ok(s), Ok(e)) = (
            google_time(&start, false, true),
            google_time(&end, true, true),
        ) {
            patch.insert("start".into(), s);
            patch.insert("end".into(), e);
        }
    }
    if now.location != before.location
        && let Some(location) = &now.location
    {
        patch.insert("location".into(), json!(location));
    }
    if now.cancelled != before.cancelled {
        let title = owned.summary.clone().unwrap_or_else(|| ghost.title.clone());
        let bare = title
            .strip_prefix(CANCELLED_PREFIX)
            .unwrap_or(&title)
            .to_string();
        if now.cancelled {
            patch.insert("summary".into(), json!(format!("{CANCELLED_PREFIX}{bare}")));
            patch.insert("transparency".into(), json!("transparent"));
        } else {
            patch.insert("summary".into(), json!(bare));
            patch.insert("transparency".into(), json!("opaque"));
        }
    }
    (!patch.is_empty()).then_some(Value::Object(patch))
}

/// A new end for an event moved to `start` with no end from the mail: the
/// same length it had, at the new start's precision.
fn shifted_end(owned: &EventRow, start: EventTime) -> EventTime {
    match (owned.start, owned.end, start) {
        (EventTime::At(a), Some(EventTime::At(b)), EventTime::At(s)) => EventTime::At(s + (b - a)),
        (EventTime::Date(a), Some(EventTime::Date(b)), EventTime::Date(s)) => {
            EventTime::Date(s + (b - a))
        }
        (_, _, EventTime::At(s)) => EventTime::At(s + Duration::hours(1)),
        (_, _, day) => day,
    }
}

/// Everything to do this pass, in order: bindings first (stop, update,
/// clear duplicates), then adoptions. `places` is the card's cleaned place
/// by raw location ([`super::place`]).
pub fn plan(
    now: DateTime<Utc>,
    account_email: &str,
    ghosts: &[GhostEvent],
    events: &[SyncedEvent],
    owned: &[Owned],
    places: &HashMap<String, Option<String>>,
) -> Vec<Step> {
    let by_id: HashMap<i64, &GhostEvent> = ghosts.iter().map(|g| (g.id, g)).collect();
    let find = |calendar: &str, id: &str| {
        events
            .iter()
            .find(|e| e.calendar_id == calendar && e.row.event_id == id)
    };
    let gmail: Vec<&SyncedEvent> = events
        .iter()
        .filter(|e| adoptable(e, account_email, now))
        .collect();
    let mut claimed: HashSet<(&str, &str)> = HashSet::new();
    let mut bound: HashSet<i64> = owned.iter().map(|o| o.ghost_id).collect();
    let mut steps = Vec::new();

    for o in owned.iter().filter(|o| o.state == OwnState::Owned) {
        let Some(ghost) = by_id.get(&o.ghost_id) else {
            steps.push(Step::Forget {
                ghost_id: o.ghost_id,
            });
            continue;
        };
        let Some(mine) = find(&o.calendar_id, &o.event_id) else {
            steps.push(Step::Release {
                ghost_id: o.ghost_id,
            });
            continue;
        };
        if ghost.user_state == Some(GhostUserState::Dismissed) {
            steps.push(Step::Release {
                ghost_id: o.ghost_id,
            });
            continue;
        }
        let print = fingerprint(ghost);
        // Only authenticated mail moves a real event.
        if print != o.fingerprint && ghost.verified() {
            steps.push(Step::Update {
                ghost_id: o.ghost_id,
                calendar_id: o.calendar_id.clone(),
                event_id: o.event_id.clone(),
                patch: update_patch(ghost, &mine.row, &o.fingerprint),
                fingerprint: print,
            });
        }
        for copy in &gmail {
            let key = (copy.calendar_id.as_str(), copy.row.event_id.as_str());
            let twin = same_start(&copy.row.start, &mine.row.start)
                && ghost_matches(
                    mine.row.summary.as_deref().unwrap_or_default(),
                    &mine.row.start,
                    copy.row.summary.as_deref(),
                    &copy.row.start,
                );
            if (same_event(ghost, &copy.row) || twin) && claimed.insert(key) {
                steps.push(Step::DropDuplicate {
                    ghost_id: o.ghost_id,
                    calendar_id: copy.calendar_id.clone(),
                    event_id: copy.row.event_id.clone(),
                });
            }
        }
    }

    for copy in &gmail {
        let key = (copy.calendar_id.as_str(), copy.row.event_id.as_str());
        if claimed.contains(&key) {
            continue;
        }
        let candidates: Vec<&GhostEvent> = ghosts
            .iter()
            .filter(|g| {
                !bound.contains(&g.id)
                    && g.verified()
                    && g.mail_state == GhostMailState::Live
                    && g.user_state != Some(GhostUserState::Dismissed)
                    && same_event(g, &copy.row)
            })
            .collect();
        let [ghost] = candidates.as_slice() else {
            continue;
        };
        let place = copy
            .row
            .location
            .as_ref()
            .and_then(|raw| places.get(raw))
            .map(|p| p.as_deref().unwrap_or_default());
        let id = owned_event_id(ghost);
        let Some(body) = adopt_body(ghost, &copy.row, place, &id) else {
            continue;
        };
        bound.insert(ghost.id);
        claimed.insert(key);
        steps.push(Step::Adopt {
            ghost_id: ghost.id,
            calendar_id: copy.calendar_id.clone(),
            gmail_event_id: copy.row.event_id.clone(),
            new_event_id: id,
            body,
            fingerprint: fingerprint(ghost),
        });
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gcal::{AccessRole, Guest, GuestResponse};
    use crate::triage::decision::{EventCommitment, EventRecordKind};

    const ME: &str = "me@example.com";

    fn now() -> DateTime<Utc> {
        "2026-10-28T12:00:00Z".parse().unwrap()
    }

    fn at(raw: &str) -> EventTime {
        EventTime::parse(raw).unwrap()
    }

    fn ghost(id: i64, title: &str, start: &str) -> GhostEvent {
        GhostEvent {
            id,
            account_id: 1,
            kind: EventRecordKind::Reservation,
            title: title.into(),
            start: at(start),
            start_timezone: None,
            stated_end: None,
            estimate: None,
            location: Some("Dingles Public House".into()),
            commitment: EventCommitment::Committed,
            mail_state: GhostMailState::Live,
            likelihood: None,
            user_state: None,
            user_state_at: None,
            user_end: None,
            history_minutes: None,
            supplier_message_id: Some(1),
            last_message_id: Some(1),
            created_at: "2026-10-20T10:00:00Z".parse().unwrap(),
            updated_at: "2026-10-20T10:00:00Z".parse().unwrap(),
            owner_scope: Some("dingles.example".into()),
            sources: Vec::new(),
        }
    }

    fn event(id: &str, title: &str, start: &str, end: &str, from_gmail: bool) -> SyncedEvent {
        SyncedEvent {
            calendar_id: "primary".into(),
            calendar_name: None,
            calendar_color: None,
            calendar_access: Some(AccessRole::Owner),
            calendar_primary: true,
            row: EventRow {
                event_id: id.into(),
                ical_uid: None,
                tentative: false,
                summary: Some(title.into()),
                location: Some("Dingles Public House, Tock is now part of the Resy network".into()),
                start: at(start),
                end: Some(at(end)),
                start_timezone: None,
                organizer_email: None,
                organizer_unreadable: false,
                from_gmail,
                organizer_self: true,
                self_response: None,
                transparent: false,
                html_link: None,
                updated_at: None,
                color_id: None,
                description: None,
                guests: Vec::new(),
                guests_truncated: false,
            },
        }
    }

    const START: &str = "2026-10-30T20:15:00-04:00";
    const END: &str = "2026-10-30T21:15:00-04:00";

    fn dingles() -> SyncedEvent {
        event(
            "gm1",
            "Reservation at Dingles Public House",
            START,
            END,
            true,
        )
    }

    fn plan_of(ghosts: &[GhostEvent], events: &[SyncedEvent], owned: &[Owned]) -> Vec<Step> {
        let places = HashMap::from([(
            "Dingles Public House, Tock is now part of the Resy network".to_string(),
            Some("Dingles Public House".to_string()),
        )]);
        plan(now(), ME, ghosts, events, owned, &places)
    }

    #[test]
    fn a_gmail_event_one_ghost_matches_is_adopted_with_the_clean_place() {
        let g = ghost(7, "Dinner at Dingles", START);
        let steps = plan_of(&[g.clone()], &[dingles()], &[]);
        let [
            Step::Adopt {
                ghost_id,
                gmail_event_id,
                new_event_id,
                body,
                ..
            },
        ] = steps.as_slice()
        else {
            panic!("one adoption, got {steps:?}");
        };
        assert_eq!((*ghost_id, gmail_event_id.as_str()), (7, "gm1"));
        assert_eq!(new_event_id, &owned_event_id(&g));
        assert!(
            new_event_id
                .chars()
                .all(|c| matches!(c, 'a'..='v' | '0'..='9'))
        );
        assert_eq!(body["summary"], "Reservation at Dingles Public House");
        assert_eq!(body["location"], "Dingles Public House");
        assert_eq!(body["description"], OWNED_NOTE);
        assert_eq!(body["extendedProperties"]["private"]["passband_ghost"], "7");
        assert!(body["start"]["dateTime"].is_string() && body["end"]["dateTime"].is_string());
    }

    #[test]
    fn invitations_other_calendars_past_and_ambiguous_events_are_left_alone() {
        let g = ghost(7, "Dinner at Dingles", START);
        // Someone else organizes it.
        let mut invite = dingles();
        invite.row.organizer_self = false;
        invite.row.organizer_email = Some("ellie@example.com".into());
        // Another guest is on it.
        let mut shared = dingles();
        shared.row.guests = vec![Guest {
            email: "rachel@example.com".into(),
            name: None,
            response: GuestResponse::Accepted,
            organizer: false,
            is_self: false,
        }];
        // Not the primary calendar.
        let mut elsewhere = dingles();
        elsewhere.calendar_primary = false;
        // Over already.
        let past = event(
            "gm2",
            "Reservation at Dingles Public House",
            "2026-10-01T20:00:00-04:00",
            "2026-10-01T21:00:00-04:00",
            true,
        );
        let mut g_past = g.clone();
        g_past.start = past.row.start;
        // Not Gmail's.
        let mine = event(
            "own",
            "Reservation at Dingles Public House",
            START,
            END,
            false,
        );
        for e in [invite, shared, elsewhere, past, mine] {
            assert!(
                plan_of(&[g.clone(), g_past.clone()], &[e.clone()], &[]).is_empty(),
                "{e:?}"
            );
        }
        // Two ghosts for one Gmail event: not a guess worth a write.
        let twin = ghost(8, "Dingles dinner", START);
        assert!(plan_of(&[g.clone(), twin], &[dingles()], &[]).is_empty());
        // No ghost at that time.
        let later = ghost(9, "Dinner at Dingles", "2026-10-31T20:15:00-04:00");
        assert!(plan_of(&[later], &[dingles()], &[]).is_empty());
        // A dismissed or cancelled ghost adopts nothing.
        let mut dismissed = g.clone();
        dismissed.user_state = Some(GhostUserState::Dismissed);
        let mut cancelled = g.clone();
        cancelled.mail_state = GhostMailState::Cancelled;
        assert!(plan_of(&[dismissed], &[dingles()], &[]).is_empty());
        assert!(plan_of(&[cancelled], &[dingles()], &[]).is_empty());
    }

    fn bound(g: &GhostEvent, state: OwnState) -> Owned {
        Owned {
            ghost_id: g.id,
            calendar_id: "primary".into(),
            event_id: owned_event_id(g),
            state,
            fingerprint: fingerprint(g),
        }
    }

    fn passbands(g: &GhostEvent, start: &str, end: &str) -> SyncedEvent {
        let mut e = event(
            &owned_event_id(g),
            "Reservation at Dingles Public House",
            start,
            end,
            false,
        );
        e.row.location = Some("Dingles Public House".into());
        e
    }

    #[test]
    fn an_owned_event_follows_the_mail_and_only_the_mail() {
        let g = ghost(7, "Dinner at Dingles", START);
        let o = bound(&g, OwnState::Owned);
        let mine = passbands(&g, START, END);
        // Nothing new in the mail: nothing to do, even if the user moved it.
        let mut moved_by_user = mine.clone();
        moved_by_user.row.start = at("2026-10-30T21:00:00-04:00");
        assert!(plan_of(&[g.clone()], &[moved_by_user], &[o.clone()]).is_empty());

        // The mail moves it an hour later: the start moves, the length kept.
        let mut later = g.clone();
        later.start = at("2026-10-30T21:15:00-04:00");
        let steps = plan_of(&[later.clone()], &[mine.clone()], &[o.clone()]);
        let [
            Step::Update {
                patch: Some(patch),
                fingerprint,
                ..
            },
        ] = steps.as_slice()
        else {
            panic!("one update, got {steps:?}");
        };
        assert_eq!(*fingerprint, super::fingerprint(&later));
        let start =
            DateTime::parse_from_rfc3339(patch["start"]["dateTime"].as_str().unwrap()).unwrap();
        let end = DateTime::parse_from_rfc3339(patch["end"]["dateTime"].as_str().unwrap()).unwrap();
        assert_eq!(
            start,
            DateTime::parse_from_rfc3339("2026-10-30T21:15:00-04:00").unwrap()
        );
        assert_eq!(end - start, Duration::hours(1));
        assert!(patch.get("summary").is_none() && patch.get("location").is_none());

        // Cancelled by mail: marked, shown free; restored: unmarked.
        let mut off = g.clone();
        off.mail_state = GhostMailState::Cancelled;
        let steps = plan_of(&[off.clone()], &[mine.clone()], &[o.clone()]);
        let [
            Step::Update {
                patch: Some(patch), ..
            },
        ] = steps.as_slice()
        else {
            panic!("one update, got {steps:?}");
        };
        assert_eq!(
            patch["summary"],
            "Canceled: Reservation at Dingles Public House"
        );
        assert_eq!(patch["transparency"], "transparent");
        let mut marked = mine.clone();
        marked.row.summary = Some("Canceled: Reservation at Dingles Public House".into());
        let steps = plan_of(&[g.clone()], &[marked], &[bound(&off, OwnState::Owned)]);
        let [
            Step::Update {
                patch: Some(patch), ..
            },
        ] = steps.as_slice()
        else {
            panic!("one update, got {steps:?}");
        };
        assert_eq!(patch["summary"], "Reservation at Dingles Public House");
        assert_eq!(patch["transparency"], "opaque");
    }

    #[test]
    fn gmail_adding_it_again_is_a_duplicate_and_a_deleted_event_is_let_go() {
        let g = ghost(7, "Dinner at Dingles", START);
        let o = bound(&g, OwnState::Owned);
        let mine = passbands(&g, START, END);
        let steps = plan_of(&[g.clone()], &[mine.clone(), dingles()], &[o.clone()]);
        assert_eq!(
            steps,
            vec![Step::DropDuplicate {
                ghost_id: 7,
                calendar_id: "primary".into(),
                event_id: "gm1".into(),
            }]
        );
        // The user deleted Passband's event: let go, and Gmail's copy stays.
        let steps = plan_of(&[g.clone()], &[dingles()], &[o.clone()]);
        assert_eq!(steps, vec![Step::Release { ghost_id: 7 }]);
        for state in [OwnState::Released, OwnState::Refused] {
            assert!(plan_of(&[g.clone()], &[dingles()], &[bound(&g, state)]).is_empty());
        }
        // The ghost is gone: forget the binding, leave the event.
        assert_eq!(
            plan_of(&[], &[mine], &[o.clone()]),
            vec![Step::Forget { ghost_id: 7 }]
        );
    }

    #[test]
    fn mail_that_failed_authentication_neither_adopts_nor_moves_anything() {
        let mut g = ghost(7, "Dinner at Dingles", START);
        g.owner_scope = None;
        assert!(plan_of(&[g.clone()], &[dingles()], &[]).is_empty());
        let o = bound(&g, OwnState::Owned);
        let mut later = g.clone();
        later.start = at("2026-10-30T21:15:00-04:00");
        let mine = passbands(&g, START, END);
        assert!(plan_of(&[later], &[mine], &[o]).is_empty());
    }

    #[test]
    fn words_common_to_every_reservation_do_not_make_a_match() {
        let g = ghost(7, "Dinner reservation", START);
        let mut g = g;
        g.location = None;
        let mut e = event("gm1", "Reservation at Other Place", START, END, true);
        e.row.location = None;
        assert!(!same_event(&g, &e.row));
        let mut named = ghost(8, "Dinner at Dingles", START);
        named.location = None;
        assert!(same_event(&named, &dingles().row));
    }
}
