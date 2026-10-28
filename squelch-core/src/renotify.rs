//! Smart re-notify (issue #625): bring mail that still needs handling back to
//! the owner's attention at a moment they can act on it, rather than only at
//! the moment it arrived.
//!
//! THIS MODULE IS THE DETERMINISTIC HALF, and it is pure: no store, no clock,
//! no network. Everything it decides comes from what the caller hands it (the
//! time, the trigger, the owner's busy intervals, the candidates the store
//! found, and what earlier re-notifies achieved), so every rule below is a
//! unit test away from being checked. The worker that feeds it and delivers
//! what it picks lives in `squelch-api` (`renotify.rs`), and the store half
//! (candidates, deliveries, sessions, busy intervals) in
//! `store/sqlite/renotify.rs`. See docs/RENOTIFY.md for the whole contract.
//!
//! THE RULES ALWAYS WIN. A later model layer may rank or decline within what
//! [`plan`] allows; nothing may fire outside it.
//!
//! - A re-notify fires on a TRIGGER: the Mac app coming online after a gap
//!   ([`Trigger::ClientOnline`]), or a few minutes after a meeting ended
//!   ([`Trigger::MeetingEnd`]).
//! - Never while the owner is in a meeting, never just before one, never
//!   outside working hours in the owner's zone, never past the day's budget.
//! - Only for mail that already earned a first notification, is still
//!   unhandled (the store's query), and is due again here: old enough that the
//!   first banner had its chance, young enough to still matter, under the
//!   per-message cap, and past a cooldown that DOUBLES each time a re-notify
//!   for it was ignored.
//! - Several picks are ONE banner (a digest), never a burst.
//!
//! DUE-SOON MAIL IS ITS OWN PATH. Opening a bill is not paying it: mail that
//! is still in For your eyes with a due date coming up comes back as the date
//! nears ([`Why::DueSoon`]), opened or not, and whatever the age ceiling says.
//! Once per due date, and not counted against the per-message cap.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Timelike, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::config::RenotifyConfig;
use crate::triage::decision::{StatedTime, SupportedTime};
use crate::types::EventKind;

crate::types::str_enum! {
    /// Why a re-notify fired. A CLOSED set: it is stored, it is a stats key,
    /// and the client may word a banner by it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Trigger {
        /// The Mac app connected after being away at least
        /// [`Policy::min_offline`]: the owner just sat down.
        ClientOnline => "client_online",
        /// A meeting ended [`Policy::meeting_end_delay`] ago and the next one is
        /// not about to start: a gap people often spend clearing mail.
        MeetingEnd => "meeting_end",
    }
}

/// One stretch of the owner's calendar that counts as busy. Built by the store
/// from start, end, transparency and the owner's response alone: no title, no
/// guests, no location ever reaches this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusySpan {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl BusySpan {
    fn contains(&self, at: DateTime<Utc>) -> bool {
        self.start <= at && at < self.end
    }
}

/// The tunables [`plan`] applies, already converted to durations. Built from
/// [`RenotifyConfig`] so the config's units stay the config's business.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// How long the Mac must have been away for a connection to count as the
    /// owner arriving, rather than a network blip or a short sleep.
    pub min_offline: Duration,
    /// How long after a meeting ends the meeting-end window opens.
    pub meeting_end_delay: Duration,
    /// How long that window stays open: a worker that ticks late still fires,
    /// one that comes back an hour later does not.
    pub meeting_end_window: Duration,
    /// A timed event longer than this is a block (out of office, a workshop),
    /// not a meeting: it still makes the owner busy, but its end is no trigger.
    pub max_meeting_len: Duration,
    /// Hold when a meeting starts within this long.
    pub meeting_guard: Duration,
    /// The working day in the owner's zone, `[start, end)` in whole hours.
    pub work_start_hour: u32,
    pub work_end_hour: u32,
    /// Re-notifies one day may carry, counted in batches (one banner each).
    pub daily_budget: u32,
    /// Messages one banner may name.
    pub max_items: usize,
    /// Times one message may be re-notified, ever.
    pub max_per_message: u32,
    /// The first notification must be at least this old.
    pub min_age: Duration,
    /// ... and at most this old: past it the mail has stopped being news.
    pub max_age: Duration,
    /// The wait after a re-notify before the same message may come again.
    /// Doubled for every re-notify already spent on it.
    pub cooldown: Duration,
    /// Due-soon mail becomes eligible this long before its due time...
    pub due_lead: Duration,
    /// ... and stays eligible this long after it.
    pub due_grace: Duration,
    /// A date-only due date ("due the 30th") is read as this hour of that day
    /// in the owner's zone.
    pub due_day_hour: u32,
}

impl Policy {
    pub fn from_config(cfg: &RenotifyConfig) -> Self {
        let mins = |m: u64| Duration::minutes(m.min(i64::MAX as u64 / 60_000) as i64);
        Self {
            min_offline: mins(cfg.min_offline_mins),
            meeting_end_delay: mins(cfg.meeting_end_delay_mins),
            meeting_end_window: mins(cfg.meeting_end_window_mins.max(1)),
            max_meeting_len: mins(cfg.max_meeting_mins),
            meeting_guard: mins(cfg.meeting_guard_mins),
            work_start_hour: cfg.work_start_hour.min(23),
            work_end_hour: cfg.work_end_hour.min(24),
            daily_budget: cfg.daily_budget,
            max_items: cfg.max_items.max(1),
            max_per_message: cfg.max_per_message,
            min_age: mins(cfg.min_age_mins),
            max_age: mins(cfg.max_age_mins),
            cooldown: mins(cfg.cooldown_mins),
            due_lead: mins(cfg.due_lead_mins),
            due_grace: mins(cfg.due_grace_mins),
            due_day_hour: cfg.due_day_hour.min(23),
        }
    }
}

impl Default for Policy {
    fn default() -> Self {
        Self::from_config(&RenotifyConfig::default())
    }
}

/// Why a message is a candidate. Rides the frame, so the app can word the
/// banner ("Still waiting" against "Due tomorrow").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    /// It notified once and is still unopened and unhandled.
    Unhandled,
    /// It is in For your eyes with a due date coming up, opened or not.
    DueSoon,
}

/// One message a re-notify may name. For [`Why::Unhandled`] the display
/// fields are the `events` row's own snapshot, so the banner says exactly what
/// the first one said; for [`Why::DueSoon`] they are the triage decision's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub why: Why,
    /// The `events` row this message first notified on, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<i64>,
    pub message_id: i64,
    pub thread_id: String,
    pub kind: EventKind,
    pub importance: u8,
    pub sender: String,
    pub one_line: String,
    /// The event's deadline snapshot (RFC 3339), passed through as display copy.
    pub deadline: Option<String>,
    /// [`Why::DueSoon`]: the due time this re-notify is for, resolved to an
    /// instant. Recorded with the banner, which is what makes it once per due
    /// date.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_at: Option<DateTime<Utc>>,
    /// When the first notification was written (for due-soon mail without
    /// one, when the message arrived).
    #[serde(skip)]
    pub notified_at: DateTime<Utc>,
    /// Re-notifies already spent on this message.
    #[serde(skip)]
    pub renotified: u32,
    /// The most recent of them.
    #[serde(skip)]
    pub last_renotified_at: Option<DateTime<Utc>>,
}

impl Candidate {
    fn deadline_at(&self) -> Option<DateTime<Utc>> {
        self.due_at.or_else(|| {
            self.deadline
                .as_deref()
                .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
                .map(|d| d.with_timezone(&Utc))
        })
    }
}

/// A due time as the triage decision states it, as one instant. A timestamp
/// with an offset is that instant. A local time is read in the zone the
/// decision names, else the owner's. A bare date is
/// [`Policy::due_day_hour`] that day in the owner's zone: "due the 30th"
/// means by the end of the working day. `None` for a value that cannot be
/// read, or a local time a clock change skips.
pub fn due_instant(policy: &Policy, time: &SupportedTime, zone: Tz) -> Option<DateTime<Utc>> {
    use chrono::TimeZone;
    let local_zone = time
        .timezone
        .as_deref()
        .and_then(crate::owner_clock::device_zone)
        .unwrap_or(zone);
    match time.stated()? {
        StatedTime::Instant(at) => Some(at),
        StatedTime::Local(local) => local_zone
            .from_local_datetime(&local)
            .earliest()
            .map(|t| t.with_timezone(&Utc)),
        StatedTime::Day(day) => zone
            .from_local_datetime(&day.and_hms_opt(policy.due_day_hour, 0, 0)?)
            .earliest()
            .map(|t| t.with_timezone(&Utc)),
    }
}

/// What earlier re-notifies achieved, one row per batch. The store builds it;
/// [`Habits`] reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub trigger: Trigger,
    pub fired_at: DateTime<Utc>,
    /// The banner was tapped.
    pub opened: bool,
    /// Messages the banner named.
    pub items: u32,
    /// Of those, how many the owner dealt with after it fired (opened, done or
    /// archived, or replied to).
    pub handled: u32,
}

impl Outcome {
    /// A banner the owner responded to in any way.
    pub fn engaged(&self) -> bool {
        self.opened || self.handled > 0
    }
}

/// Batches a trigger must have fired, unanswered, before it is paused.
pub const MUTE_AFTER_IGNORED: usize = 8;

/// THE PER-OWNER LEARNING, first rung: what the owner's own responses say
/// about each trigger. Deliberately simple and deterministic. A trigger whose
/// last [`MUTE_AFTER_IGNORED`] banners were ALL ignored is paused, because a
/// banner nobody responds to is costing attention and buying nothing; one
/// response anywhere in that run is enough to keep it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Habits {
    muted: Vec<Trigger>,
}

impl Habits {
    /// `outcomes` in any order; only the most recent run per trigger counts.
    pub fn learn(outcomes: &[Outcome]) -> Self {
        let mut by_trigger: BTreeMap<Trigger, Vec<&Outcome>> = BTreeMap::new();
        for o in outcomes {
            by_trigger.entry(o.trigger).or_default().push(o);
        }
        let mut muted = Vec::new();
        for (trigger, mut list) in by_trigger {
            list.sort_by_key(|o| std::cmp::Reverse(o.fired_at));
            let recent = &list[..list.len().min(MUTE_AFTER_IGNORED)];
            if recent.len() == MUTE_AFTER_IGNORED && recent.iter().all(|o| !o.engaged()) {
                muted.push(trigger);
            }
        }
        Self { muted }
    }

    pub fn is_muted(&self, trigger: Trigger) -> bool {
        self.muted.contains(&trigger)
    }
}

/// The moment [`plan`] is asked about.
#[derive(Debug, Clone)]
pub struct Moment<'a> {
    pub now: DateTime<Utc>,
    pub trigger: Trigger,
    /// The owner's zone, for working hours and the day the budget counts.
    pub zone: Tz,
    /// Busy intervals around `now`; order does not matter.
    pub busy: &'a [BusySpan],
    /// Batches already fired since the owner's local midnight.
    pub fired_today: u32,
    pub habits: &'a Habits,
}

/// Why [`plan`] fired nothing. Each is a metric label and a log word, so the
/// set is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// The owner is in a meeting right now.
    InMeeting,
    /// A meeting starts within [`Policy::meeting_guard`].
    MeetingSoon,
    /// Outside working hours in the owner's zone.
    OutsideHours,
    /// The day's [`Policy::daily_budget`] is spent.
    BudgetSpent,
    /// The owner has stopped responding to this trigger ([`Habits`]).
    Muted,
    /// Nothing is due.
    NothingDue,
}

impl Hold {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InMeeting => "in_meeting",
            Self::MeetingSoon => "meeting_soon",
            Self::OutsideHours => "outside_hours",
            Self::BudgetSpent => "budget_spent",
            Self::Muted => "muted",
            Self::NothingDue => "nothing_due",
        }
    }
}

/// Is the owner busy at `now`?
pub fn in_meeting(busy: &[BusySpan], now: DateTime<Utc>) -> bool {
    busy.iter().any(|s| s.contains(now))
}

/// Does a meeting start in `(now, now + guard]`?
pub fn meeting_soon(busy: &[BusySpan], now: DateTime<Utc>, guard: Duration) -> bool {
    busy.iter().any(|s| s.start > now && s.start <= now + guard)
}

/// Is `now` inside the working day in `zone`? A start at or past the end
/// reads as a window that wraps midnight (`22..6` is a night shift).
pub fn in_working_hours(policy: &Policy, zone: Tz, now: DateTime<Utc>) -> bool {
    let hour = now.with_timezone(&zone).hour();
    let (start, end) = (policy.work_start_hour, policy.work_end_hour);
    if start == end {
        return true;
    }
    if start < end {
        start <= hour && hour < end
    } else {
        hour >= start || hour < end
    }
}

/// THE MEETING-END TRIGGER: the end of the latest meeting whose window is open
/// at `now` and later than `after` (the last end the caller already acted on),
/// or `None`.
///
/// The window opens [`Policy::meeting_end_delay`] after the end and stays open
/// for [`Policy::meeting_end_window`]. A block longer than
/// [`Policy::max_meeting_len`] is not a meeting. An end that falls inside
/// another busy span is no gap at all (back-to-back meetings): only the last
/// end of a run counts, and [`plan`]'s own busy checks still apply when it
/// fires.
pub fn meeting_end_due(
    policy: &Policy,
    busy: &[BusySpan],
    now: DateTime<Utc>,
    after: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    busy.iter()
        .filter(|s| s.end - s.start <= policy.max_meeting_len)
        .map(|s| s.end)
        .filter(|&end| {
            let opens = end + policy.meeting_end_delay;
            opens <= now && now < opens + policy.meeting_end_window
        })
        .filter(|&end| after.is_none_or(|a| end > a))
        .filter(|&end| !busy.iter().any(|o| o.contains(end)))
        .max()
}

/// Is `c` due for a re-notify at `now`, judged on its own? The store already
/// ruled out handled mail; this is the timing half.
pub fn due(policy: &Policy, c: &Candidate, now: DateTime<Utc>) -> bool {
    if c.why == Why::DueSoon {
        return due_soon(policy, c, now);
    }
    let age = now - c.notified_at;
    if age < policy.min_age || age > policy.max_age {
        return false;
    }
    if c.renotified >= policy.max_per_message {
        return false;
    }
    match c.last_renotified_at {
        None => true,
        Some(last) => {
            // Exponential backoff on ignore: every earlier re-notify that is
            // still a candidate went unanswered, so each one doubles the wait.
            let shift = c.renotified.saturating_sub(1).min(16);
            let wait = policy
                .cooldown
                .checked_mul(1i32 << shift)
                .unwrap_or(Duration::MAX);
            now - last >= wait
        }
    }
}

/// The due-soon window: from [`Policy::due_lead`] before the due time to
/// [`Policy::due_grace`] after it. Not in the first [`Policy::min_age`] after
/// the mail arrived (its own banner has that long), and not within
/// [`Policy::min_age`] of any other re-notify for it. Once per due date is the
/// store's half: a due time already recorded is never a candidate again.
fn due_soon(policy: &Policy, c: &Candidate, now: DateTime<Utc>) -> bool {
    let Some(due_at) = c.due_at else {
        return false;
    };
    if now < due_at - policy.due_lead || now > due_at + policy.due_grace {
        return false;
    }
    if now - c.notified_at < policy.min_age {
        return false;
    }
    c.last_renotified_at
        .is_none_or(|last| now - last >= policy.min_age)
}

/// How strongly `c` deserves the banner, higher first. Importance is the base;
/// a deadline closing in is the relevance that RISES while mail waits, so it
/// counts most; mail that has already been re-notified yields to mail that
/// has not.
pub fn score(c: &Candidate, now: DateTime<Utc>) -> i64 {
    let mut s = i64::from(c.importance);
    if let Some(deadline) = c.deadline_at() {
        let left = deadline - now;
        s += if left < Duration::zero() {
            30
        } else if left <= Duration::hours(24) {
            40
        } else if left <= Duration::hours(72) {
            20
        } else {
            5
        };
    }
    if c.kind == EventKind::Urgent {
        s += 20;
    }
    if c.why == Why::DueSoon {
        // Its own reason to be here, on top of the deadline boost.
        s += 30;
        return s;
    }
    s - 15 * i64::from(c.renotified)
}

/// THE DECISION: which candidates one banner should name at this moment, best
/// first, or why none.
///
/// The order of the checks is the order a reader should reason in: the owner's
/// state first (busy, hours, budget, habit), then the mail. So a hold always
/// names the strongest reason, and a test of "in a meeting" does not depend on
/// there being mail.
pub fn plan(
    policy: &Policy,
    moment: &Moment<'_>,
    candidates: &[Candidate],
) -> Result<Vec<Candidate>, Hold> {
    let now = moment.now;
    if in_meeting(moment.busy, now) {
        return Err(Hold::InMeeting);
    }
    if meeting_soon(moment.busy, now, policy.meeting_guard) {
        return Err(Hold::MeetingSoon);
    }
    if !in_working_hours(policy, moment.zone, now) {
        return Err(Hold::OutsideHours);
    }
    if moment.fired_today >= policy.daily_budget {
        return Err(Hold::BudgetSpent);
    }
    if moment.habits.is_muted(moment.trigger) {
        return Err(Hold::Muted);
    }
    let mut picks: Vec<&Candidate> = candidates.iter().filter(|c| due(policy, c, now)).collect();
    if picks.is_empty() {
        return Err(Hold::NothingDue);
    }
    picks.sort_by(|a, b| {
        score(b, now)
            .cmp(&score(a, now))
            .then(b.notified_at.cmp(&a.notified_at))
            .then(b.message_id.cmp(&a.message_id))
    });
    // One entry per message, the best-scoring one: a message can be both
    // unhandled and due soon.
    let mut seen = std::collections::HashSet::new();
    picks.retain(|c| seen.insert(c.message_id));
    Ok(picks.into_iter().take(policy.max_items).cloned().collect())
}

/// One trigger's (or one hour's) record, for the stats route.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Tally {
    pub fired: u32,
    pub opened: u32,
    pub items: u32,
    pub handled: u32,
}

impl Tally {
    fn add(&mut self, o: &Outcome) {
        self.fired += 1;
        self.opened += u32::from(o.opened);
        self.items += o.items;
        self.handled += o.handled;
    }
}

/// What the owner's responses add up to: per trigger, and per hour of the day
/// in their zone. This is the habit data the later rungs (model priors,
/// per-owner tuning) read, and what the stats route serves.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub by_trigger: BTreeMap<&'static str, Tally>,
    /// Keyed by local hour, 0-23.
    pub by_hour: BTreeMap<u32, Tally>,
    /// Triggers [`Habits`] has paused.
    pub muted: Vec<&'static str>,
}

pub fn summarize(outcomes: &[Outcome], zone: Tz) -> Summary {
    let mut out = Summary::default();
    for o in outcomes {
        out.by_trigger.entry(o.trigger.as_str()).or_default().add(o);
        out.by_hour
            .entry(o.fired_at.with_timezone(&zone).hour())
            .or_default()
            .add(o);
    }
    let habits = Habits::learn(outcomes);
    out.muted = [Trigger::ClientOnline, Trigger::MeetingEnd]
        .into_iter()
        .filter(|t| habits.is_muted(*t))
        .map(|t| t.as_str())
        .collect();
    out
}

/// The owner's local midnight before `now`, in UTC: where today's budget
/// starts counting. A midnight that does not exist (a DST jump at 00:00) falls
/// back to the earliest instant of the local day.
pub fn local_day_start(zone: Tz, now: DateTime<Utc>) -> DateTime<Utc> {
    use chrono::TimeZone;
    let local = now.with_timezone(&zone).date_naive();
    for minutes in (0..=180).step_by(15) {
        let at = local.and_hms_opt(0, 0, 0).expect("midnight") + Duration::minutes(minutes);
        if let Some(t) = zone.from_local_datetime(&at).earliest() {
            return t.with_timezone(&Utc);
        }
    }
    now - Duration::hours(24)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(h: u32, m: u32) -> DateTime<Utc> {
        // A Wednesday, in UTC so the default working day reads plainly.
        Utc.with_ymd_and_hms(2026, 10, 28, h, m, 0).unwrap()
    }

    fn span(a: DateTime<Utc>, b: DateTime<Utc>) -> BusySpan {
        BusySpan { start: a, end: b }
    }

    fn cand(id: i64, importance: u8, notified_at: DateTime<Utc>) -> Candidate {
        Candidate {
            why: Why::Unhandled,
            event_id: Some(id),
            message_id: id,
            thread_id: format!("t{id}"),
            kind: EventKind::Surfaced,
            importance,
            sender: "alice@example.com".into(),
            one_line: format!("line {id}"),
            deadline: None,
            due_at: None,
            notified_at,
            renotified: 0,
            last_renotified_at: None,
        }
    }

    fn moment<'a>(now: DateTime<Utc>, busy: &'a [BusySpan], habits: &'a Habits) -> Moment<'a> {
        Moment {
            now,
            trigger: Trigger::ClientOnline,
            zone: Tz::UTC,
            busy,
            fired_today: 0,
            habits,
        }
    }

    #[test]
    fn a_due_candidate_fires_in_a_free_working_hour() {
        let p = Policy::default();
        let h = Habits::default();
        let picks = plan(&p, &moment(t(10, 0), &[], &h), &[cand(1, 70, t(8, 0))]).unwrap();
        assert_eq!(picks.len(), 1);
        assert_eq!(picks[0].message_id, 1);
    }

    #[test]
    fn never_in_a_meeting_and_never_right_before_one() {
        let p = Policy::default();
        let h = Habits::default();
        let c = [cand(1, 70, t(8, 0))];
        let busy = [span(t(9, 30), t(10, 30))];
        assert_eq!(
            plan(&p, &moment(t(10, 0), &busy, &h), &c),
            Err(Hold::InMeeting)
        );
        // The end itself is free: spans are half-open.
        assert!(plan(&p, &moment(t(10, 30), &busy, &h), &c).is_ok());

        let soon = [span(t(10, 10), t(11, 0))];
        assert_eq!(
            plan(&p, &moment(t(10, 0), &soon, &h), &c),
            Err(Hold::MeetingSoon)
        );
        // Past the guard it is far enough away.
        let later = [span(
            t(10, 0) + p.meeting_guard + Duration::minutes(1),
            t(12, 0),
        )];
        assert!(plan(&p, &moment(t(10, 0), &later, &h), &c).is_ok());
    }

    #[test]
    fn working_hours_are_read_in_the_owners_zone() {
        let p = Policy::default();
        let h = Habits::default();
        let c = [cand(1, 70, t(8, 0))];
        // 15:00 UTC is 08:00 in Los Angeles in October (PDT): inside.
        let mut m = moment(t(15, 0), &[], &h);
        m.zone = chrono_tz::America::Los_Angeles;
        assert!(plan(&p, &m, &c).is_ok());
        // 13:00 UTC is 06:00 there: outside.
        m.now = t(13, 0);
        assert_eq!(plan(&p, &m, &c), Err(Hold::OutsideHours));
    }

    #[test]
    fn a_window_that_wraps_midnight_is_a_night_shift() {
        let p = Policy {
            work_start_hour: 22,
            work_end_hour: 6,
            ..Policy::default()
        };
        assert!(in_working_hours(&p, Tz::UTC, t(23, 0)));
        assert!(in_working_hours(&p, Tz::UTC, t(3, 0)));
        assert!(!in_working_hours(&p, Tz::UTC, t(12, 0)));
    }

    #[test]
    fn the_daily_budget_and_a_muted_trigger_hold() {
        let p = Policy::default();
        let h = Habits::default();
        let c = [cand(1, 70, t(8, 0))];
        let mut m = moment(t(10, 0), &[], &h);
        m.fired_today = p.daily_budget;
        assert_eq!(plan(&p, &m, &c), Err(Hold::BudgetSpent));

        let ignored: Vec<Outcome> = (0..MUTE_AFTER_IGNORED as i64)
            .map(|i| Outcome {
                trigger: Trigger::ClientOnline,
                fired_at: t(9, 0) - Duration::days(i + 1),
                opened: false,
                items: 1,
                handled: 0,
            })
            .collect();
        let muted = Habits::learn(&ignored);
        let m = moment(t(10, 0), &[], &muted);
        assert_eq!(plan(&p, &m, &c), Err(Hold::Muted));
        // The other trigger is unaffected.
        let mut m = moment(t(10, 0), &[], &muted);
        m.trigger = Trigger::MeetingEnd;
        assert!(plan(&p, &m, &c).is_ok());
    }

    #[test]
    fn one_response_in_the_run_keeps_a_trigger() {
        let mut outcomes: Vec<Outcome> = (0..MUTE_AFTER_IGNORED as i64)
            .map(|i| Outcome {
                trigger: Trigger::MeetingEnd,
                fired_at: t(9, 0) - Duration::days(i + 1),
                opened: false,
                items: 2,
                handled: 0,
            })
            .collect();
        assert!(Habits::learn(&outcomes).is_muted(Trigger::MeetingEnd));
        // A handled item in the newest batch counts as a response.
        outcomes[0].handled = 1;
        assert!(!Habits::learn(&outcomes).is_muted(Trigger::MeetingEnd));
        // Fewer than the run never mutes.
        outcomes.truncate(MUTE_AFTER_IGNORED - 1);
        outcomes[0].handled = 0;
        assert!(!Habits::learn(&outcomes).is_muted(Trigger::MeetingEnd));
    }

    #[test]
    fn too_fresh_too_stale_and_capped_mail_is_not_due() {
        let p = Policy::default();
        let now = t(12, 0);
        assert!(!due(
            &p,
            &cand(1, 70, now - p.min_age + Duration::minutes(1)),
            now
        ));
        assert!(due(&p, &cand(1, 70, now - p.min_age), now));
        assert!(!due(
            &p,
            &cand(1, 70, now - p.max_age - Duration::minutes(1)),
            now
        ));
        let mut capped = cand(1, 70, t(8, 0));
        capped.renotified = p.max_per_message;
        capped.last_renotified_at = Some(now - Duration::days(1));
        assert!(!due(&p, &capped, now));
    }

    #[test]
    fn the_cooldown_doubles_with_each_ignored_renotify() {
        let p = Policy {
            max_per_message: 5,
            max_age: Duration::days(30),
            ..Policy::default()
        };
        let now = t(12, 0);
        let mut c = cand(1, 70, now - Duration::days(2));
        c.renotified = 1;
        c.last_renotified_at = Some(now - p.cooldown);
        assert!(due(&p, &c, now), "one spent: one cooldown");
        c.renotified = 2;
        assert!(!due(&p, &c, now), "two spent: twice the cooldown");
        c.last_renotified_at = Some(now - p.cooldown * 2);
        assert!(due(&p, &c, now));
    }

    #[test]
    fn picks_rank_by_relevance_and_cap_at_one_banner() {
        let p = Policy {
            max_items: 2,
            ..Policy::default()
        };
        let h = Habits::default();
        let now = t(10, 0);
        let plain = cand(1, 60, t(8, 0));
        let mut deadline = cand(2, 50, t(8, 0));
        deadline.deadline = Some((now + Duration::hours(5)).to_rfc3339());
        let mut urgent = cand(3, 40, t(8, 0));
        urgent.kind = EventKind::Urgent;
        let low = cand(4, 10, t(8, 0));
        let picks = plan(&p, &moment(now, &[], &h), &[plain, deadline, urgent, low]).unwrap();
        // 50+40 = 90 for the deadline, 60 plain, 40+20 = 60 urgent; the tie
        // goes to the higher message id (same notified_at).
        assert_eq!(
            picks.iter().map(|c| c.message_id).collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn nothing_due_is_its_own_hold() {
        let p = Policy::default();
        let h = Habits::default();
        assert_eq!(
            plan(&p, &moment(t(10, 0), &[], &h), &[]),
            Err(Hold::NothingDue)
        );
    }

    #[test]
    fn a_meeting_end_opens_a_window_after_its_delay() {
        let p = Policy::default();
        let end = t(11, 0);
        let busy = [span(t(10, 0), end)];
        assert_eq!(meeting_end_due(&p, &busy, end, None), None, "too soon");
        let open = end + p.meeting_end_delay;
        assert_eq!(meeting_end_due(&p, &busy, open, None), Some(end));
        assert_eq!(
            meeting_end_due(&p, &busy, open, Some(end)),
            None,
            "already acted on"
        );
        assert_eq!(
            meeting_end_due(&p, &busy, open + p.meeting_end_window, None),
            None,
            "the window closed"
        );
    }

    #[test]
    fn back_to_back_meetings_and_long_blocks_are_no_gap() {
        let p = Policy::default();
        let busy = [span(t(10, 0), t(11, 0)), span(t(11, 0), t(12, 0))];
        let at = t(11, 0) + p.meeting_end_delay;
        assert_eq!(meeting_end_due(&p, &busy, at, None), None);
        let at = t(12, 0) + p.meeting_end_delay;
        assert_eq!(meeting_end_due(&p, &busy, at, None), Some(t(12, 0)));

        let block = [span(t(8, 0), t(17, 0))];
        assert_eq!(
            meeting_end_due(&p, &block, t(17, 0) + p.meeting_end_delay, None),
            None
        );
    }

    #[test]
    fn summary_tallies_by_trigger_and_local_hour() {
        let outcomes = [
            Outcome {
                trigger: Trigger::ClientOnline,
                fired_at: t(16, 5),
                opened: true,
                items: 2,
                handled: 1,
            },
            Outcome {
                trigger: Trigger::MeetingEnd,
                fired_at: t(18, 0),
                opened: false,
                items: 1,
                handled: 0,
            },
        ];
        let s = summarize(&outcomes, chrono_tz::America::Los_Angeles);
        assert_eq!(s.by_trigger["client_online"].opened, 1);
        assert_eq!(s.by_trigger["meeting_end"].fired, 1);
        // 16:05 UTC is 09:05 PDT.
        assert_eq!(s.by_hour[&9].handled, 1);
        assert_eq!(s.by_hour[&11].fired, 1);
        assert!(s.muted.is_empty());
    }

    fn due_soon_cand(id: i64, due_at: DateTime<Utc>, arrived: DateTime<Utc>) -> Candidate {
        Candidate {
            why: Why::DueSoon,
            event_id: None,
            kind: EventKind::Deadline,
            due_at: Some(due_at),
            ..cand(id, 50, arrived)
        }
    }

    #[test]
    fn opened_mail_due_soon_comes_back_inside_its_window() {
        let p = Policy::default();
        let due_at = t(17, 0) + Duration::days(1);
        // Arrived two weeks ago: far past the ordinary age ceiling.
        let c = due_soon_cand(1, due_at, t(9, 0) - Duration::days(14));
        assert!(
            !due(&p, &c, due_at - p.due_lead - Duration::minutes(1)),
            "too early"
        );
        assert!(due(&p, &c, due_at - p.due_lead));
        assert!(
            due(&p, &c, due_at + p.due_grace),
            "a little overdue still counts"
        );
        assert!(!due(&p, &c, due_at + p.due_grace + Duration::minutes(1)));
    }

    #[test]
    fn due_soon_ignores_the_ordinary_cap_but_not_a_fresh_banner() {
        let p = Policy::default();
        let due_at = t(18, 0);
        let mut c = due_soon_cand(1, due_at, t(9, 0) - Duration::days(3));
        c.renotified = p.max_per_message;
        c.last_renotified_at = Some(t(8, 0));
        assert!(due(&p, &c, t(10, 0)), "the ordinary cap does not apply");
        c.last_renotified_at = Some(t(10, 0) - p.min_age + Duration::minutes(1));
        assert!(!due(&p, &c, t(10, 0)), "not right after another re-notify");
        // Mail that only just arrived has its own banner first.
        let fresh = due_soon_cand(2, due_at, t(10, 0) - Duration::minutes(5));
        assert!(!due(&p, &fresh, t(10, 0)));
    }

    #[test]
    fn a_message_both_unhandled_and_due_soon_is_named_once_as_due_soon() {
        let p = Policy::default();
        let h = Habits::default();
        let now = t(10, 0);
        let plain = cand(1, 70, t(8, 0));
        let dated = due_soon_cand(1, t(16, 0), t(8, 0));
        let picks = plan(&p, &moment(now, &[], &h), &[plain, dated]).unwrap();
        assert_eq!(picks.len(), 1);
        assert_eq!(picks[0].why, Why::DueSoon);
    }

    #[test]
    fn due_times_resolve_to_one_instant() {
        let p = Policy::default();
        let la = chrono_tz::America::Los_Angeles;
        let time = |value: &str, zone: Option<&str>| SupportedTime {
            value: value.into(),
            timezone: zone.map(Into::into),
            source_message_id: 1,
            interpreted_relative: false,
        };
        assert_eq!(
            due_instant(&p, &time("2026-10-28T10:00:00Z", None), la),
            Some(t(10, 0))
        );
        // A local time in the zone the decision names...
        assert_eq!(
            due_instant(&p, &time("2026-10-28T09:00", Some("America/New_York")), la),
            Some(t(13, 0))
        );
        // ...else the owner's.
        assert_eq!(
            due_instant(&p, &time("2026-10-28T09:00", None), la),
            Some(t(16, 0))
        );
        // A bare date is 17:00 that day for the owner (PDT, UTC-7).
        assert_eq!(
            due_instant(&p, &time("2026-10-28", None), la),
            Some(t(17, 0) + Duration::hours(7))
        );
        assert_eq!(due_instant(&p, &time("next Friday", None), la), None);
    }

    #[test]
    fn the_day_starts_at_local_midnight() {
        let la = chrono_tz::America::Los_Angeles;
        // 2026-10-28 16:00 UTC is 09:00 PDT; that day began at 07:00 UTC.
        assert_eq!(local_day_start(la, t(16, 0)), t(7, 0));
        assert_eq!(local_day_start(Tz::UTC, t(16, 0)), t(0, 0));
    }
}
