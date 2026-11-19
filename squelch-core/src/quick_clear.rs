//! QUICK CLEAR: "5 minutes clears 14 of your 23 emails". One banner, sent by
//! the smart re-notify worker when the owner has just sat down at the Mac,
//! that promises a short sprint and opens a queue holding exactly the items
//! it counted. See docs/RENOTIFY.md, "Quick clear".
//!
//! THIS MODULE IS PURE: no store, no clock. The store hands over the open
//! backlog ([`OpenItem`]) and what past sprints achieved
//! ([`CalibrationSample`]); this prices each item, fills the budget and says
//! whether the result is worth a banner.
//!
//! THE PROMISE HAS TO BE KEEPABLE. Every rule below errs toward leaving an
//! item OUT of the queue: an item we cannot price is not quick, and an item
//! we might have to touch carefully (sealed mail, auth mail, a reminder the
//! owner set, an open reply/pay/decide) is never in it. A sprint that runs
//! long once is not trusted again.
//!
//! WHAT AN ITEM COSTS, first match wins for eligibility, then the best
//! evidence wins for the number:
//!
//! 1. Sealed, auth or restricted mail, and mail with a reminder: never.
//! 2. A row smart clear would take on STORED FACTS ALONE
//!    ([`crate::triage::smart_clear::stored_reason`], never the model-judged
//!    `expired` path): zero each. The client turns them into one press,
//!    priced once at [`SMART_CLEAR_PRESS_SECS`].
//! 3. An open `reply`, `pay`, `decide` or `other` action on the thread: never
//!    quick. An open `review` costs [`REVIEW_SECS`], an `attend`
//!    [`ATTEND_SECS`], summed.
//! 4. No open action: `needs_user` with nothing to do is unknown, so never.
//!    A receipt, delivery or account notice costs [`RECORD_SECS`]. Reading
//!    content costs its words at [`READING_WPM`]. Anything else
//!    (informational, resolved, waiting on others) is a glance,
//!    [`GLANCE_SECS`].
//! 5. A sender the owner protects (a known contact, a `surface` rule, kept)
//!    is quick only as a plain glance with no open action.
//! 6. The model's own estimate (`decision.effort`, agent-triage-v26) replaces
//!    the heuristic's number. A `reply` or `involved` task is never quick.
//! 7. The owner's own dwell on this sender's mail, once there are
//!    [`MIN_DWELL_SAMPLES`] reported looks, replaces both.
//! 8. A per-owner [`multiplier`] from past sprints scales model and heuristic
//!    numbers (never dwell, which is already the owner's own clock), and
//!    anything over [`MAX_ITEM_SECS`] is left out.
//!
//! THE DENOMINATOR is the open backlog as the owner sees it: every card on
//! the For your eyes board (the Dock badge's number) plus the Reading rows
//! smart clear would take right now. Mail Passband files under Reading and
//! does not ask the owner about is not counted: an unread newsletter is not
//! a demand, so counting it would only make a true sprint read as a small
//! one. The store builds the list; [`plan`] counts it.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::triage::decision::{
    ActionKind, AttentionState, EffortTask, EmailKind, MessageDecision, ReadingKind,
    ThreadAttentionDecision,
};

/// The sprint the banner promises, in seconds.
pub const BUDGET_SECS: u32 = 300;
/// Fewer items than this fit, and there is no banner.
pub const MIN_FIT: usize = 5;
/// Less than this share of the backlog fits, and there is no banner.
pub const MIN_FIT_PERCENT: usize = 25;
/// The one smart-clear press, however many rows it takes.
pub const SMART_CLEAR_PRESS_SECS: u32 = 5;
/// Take in a notice and mark it done.
pub const GLANCE_SECS: u32 = 8;
/// A receipt, a delivery update, an account notice.
pub const RECORD_SECS: u32 = 5;
/// An open `review` action.
pub const REVIEW_SECS: u32 = 40;
/// An open `attend` action (add it, RSVP).
pub const ATTEND_SECS: u32 = 25;
/// What an open review or attend adds on top of a dwell-priced read.
pub const ACTION_OVERHEAD_SECS: u32 = 10;
/// Reading speed for a Reading-content item.
pub const READING_WPM: u32 = 250;
/// The least a read costs, however short.
pub const MIN_READ_SECS: u32 = 10;
/// The most one item may cost and still be in a sprint.
pub const MAX_ITEM_SECS: u32 = 120;
/// Reported looks at a sender's mail before their median dwell prices it.
pub const MIN_DWELL_SAMPLES: u32 = 3;
/// Measured items past sprints need before the multiplier moves off 1.
pub const CALIBRATION_MIN_SAMPLES: usize = 10;
/// The multiplier's range.
pub const CALIBRATION_MIN: f64 = 0.5;
pub const CALIBRATION_MAX: f64 = 2.0;
/// Quick-clear banners are at least this far apart.
pub const GAP_HOURS: i64 = 4;
/// At most this many quick-clear banners a day (owner's local day). Their
/// own budget: they never spend the re-notify `daily_budget` a bill coming
/// due later in the day needs.
pub const DAILY_CAP: u32 = 2;
/// After a pass whose plan held (too few, too small a share), the backlog
/// is not read again for this long: a held plan rarely changes in minutes,
/// and the read is the smart-clear scan.
pub const HELD_RECHECK_MINS: i64 = 60;
/// A gap from open to done longer than this is not one sitting.
pub const MAX_OPEN_TO_DONE_SECS: i64 = 600;

crate::types::str_enum! {
    /// Where an item's number came from. A CLOSED set: it rides the frame,
    /// is stored on the item, and keys the calibration.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Basis {
        SmartClear => "smart_clear",
        Dwell => "dwell",
        Model => "model",
        Heuristic => "heuristic",
    }
}

crate::types::str_enum! {
    /// What an item is, for the client's queue. A CLOSED set.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum ItemKind {
        /// Taken by the one smart-clear press.
        SmartClear => "smart_clear",
        /// Informational, resolved, or waiting on others: take it in.
        Glance => "glance",
        Receipt => "receipt",
        Delivery => "delivery",
        AccountService => "account_service",
        /// An open review action.
        Review => "review",
        /// An open attend action.
        Attend => "attend",
        /// Reading content.
        Read => "read",
    }
}

/// A sender's reported looks, at or after the dwell ledger's floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DwellHistory {
    pub samples: u32,
    pub median_ms: u64,
}

/// One item of the open backlog, as the store found it.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenItem {
    pub message_id: i64,
    pub thread_id: String,
    pub sender: String,
    pub one_line: String,
    pub decision: MessageDecision,
    /// The thread's CURRENT attention: a card is a thread.
    pub attention: ThreadAttentionDecision,
    /// Smart clear would take it on stored facts alone.
    pub smart_clear: bool,
    /// Kept close (`sealed_mail_sql!`): auth, restricted, legacy sealed.
    pub sealed: bool,
    /// The owner asked to see it again (`remind_at` or `reminded_at`).
    pub reminder: bool,
    /// A known contact, a `surface` rule, or a sender recently kept.
    pub sender_protected: bool,
    /// Words in the body; read only for Reading content.
    pub words: u32,
    pub sender_dwell: Option<DwellHistory>,
}

/// What one item costs, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Estimate {
    pub secs: u32,
    pub basis: Basis,
    pub kind: ItemKind,
}

fn ceil_secs(ms: u64) -> u32 {
    u32::try_from(ms.div_ceil(1000)).unwrap_or(u32::MAX)
}

/// The heuristic's verdict: `None` for never quick, else the number and kind.
fn heuristic(item: &OpenItem) -> Option<(u32, ItemKind, u32)> {
    let open: Vec<ActionKind> = item
        .attention
        .actions
        .iter()
        .filter(|a| !a.resolved)
        .map(|a| a.kind)
        .collect();
    if open.iter().any(|k| {
        matches!(
            k,
            ActionKind::Reply | ActionKind::Pay | ActionKind::Decide | ActionKind::Other
        )
    }) {
        return None;
    }
    if !open.is_empty() {
        let secs: u32 = open
            .iter()
            .map(|k| match k {
                ActionKind::Review => REVIEW_SECS,
                _ => ATTEND_SECS,
            })
            .sum();
        let kind = if open.contains(&ActionKind::Review) {
            ItemKind::Review
        } else {
            ItemKind::Attend
        };
        return Some((secs, kind, open.len() as u32));
    }
    if item.attention.state == AttentionState::NeedsUser {
        return None;
    }
    let kinds = &item.decision.kinds;
    for (email, kind) in [
        (EmailKind::Receipt, ItemKind::Receipt),
        (EmailKind::Delivery, ItemKind::Delivery),
        (EmailKind::AccountService, ItemKind::AccountService),
    ] {
        if kinds.contains(&email) {
            return Some((RECORD_SECS, kind, 0));
        }
    }
    if item.decision.reading_kind == Some(ReadingKind::Content) {
        let secs = (item.words.saturating_mul(60)).div_ceil(READING_WPM);
        return Some((secs.max(MIN_READ_SECS), ItemKind::Read, 0));
    }
    Some((GLANCE_SECS, ItemKind::Glance, 0))
}

/// What `item` costs before calibration, or `None` when it is never quick.
pub fn estimate(item: &OpenItem) -> Option<Estimate> {
    if item.sealed
        || item.decision.auth.is_auth()
        || item.decision.external_access.restricted
        || item.reminder
    {
        return None;
    }
    if item.smart_clear {
        return Some(Estimate {
            secs: 0,
            basis: Basis::SmartClear,
            kind: ItemKind::SmartClear,
        });
    }
    let (mut secs, kind, open_actions) = heuristic(item)?;
    if item.sender_protected && (open_actions > 0 || kind == ItemKind::Read) {
        return None;
    }
    let mut basis = Basis::Heuristic;
    if let Some(effort) = &item.decision.effort {
        if matches!(effort.task, EffortTask::Reply | EffortTask::Involved) {
            return None;
        }
        if let Some(model) = effort.secs() {
            secs = model;
            basis = Basis::Model;
        }
    }
    if let Some(dwell) = item.sender_dwell
        && dwell.samples >= MIN_DWELL_SAMPLES
    {
        secs = ceil_secs(dwell.median_ms).max(1) + ACTION_OVERHEAD_SECS * open_actions;
        basis = Basis::Dwell;
    }
    Some(Estimate { secs, basis, kind })
}

/// Apply the per-owner multiplier to an estimate. Dwell is the owner's own
/// clock and smart clear is free, so only model and heuristic numbers move.
fn calibrated(e: Estimate, multiplier: f64) -> Estimate {
    match e.basis {
        Basis::Model | Basis::Heuristic => {
            let m = if multiplier.is_finite() {
                multiplier.clamp(CALIBRATION_MIN, CALIBRATION_MAX)
            } else {
                1.0
            };
            Estimate {
                secs: ((f64::from(e.secs) * m).round() as u32).max(1),
                ..e
            }
        }
        _ => e,
    }
}

/// One item in the queue, in the frame's shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Planned {
    pub message_id: i64,
    pub thread_id: String,
    /// What the queue promises: the estimate after the owner's multiplier.
    pub est_secs: u32,
    /// The same estimate BEFORE the multiplier. Stored, never sent: the
    /// multiplier is learned against this, so it settles on the owner's
    /// real pace instead of measuring its own last correction.
    #[serde(skip)]
    pub raw_secs: u32,
    pub basis: Basis,
    pub kind: ItemKind,
    pub sender: String,
    pub one_line: String,
}

/// A sprint worth a banner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuickClear {
    pub budget_secs: u32,
    /// The queue's total, the smart-clear press included.
    pub est_secs: u32,
    pub fit_count: usize,
    pub open_count: usize,
    pub smart_clear_count: usize,
    /// In queue order: smart-clear rows first, then cheapest first.
    pub items: Vec<Planned>,
}

/// Why there is no quick-clear banner. A log word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickHold {
    /// Fewer than [`MIN_FIT`] items fit.
    TooFew,
    /// Under [`MIN_FIT_PERCENT`] of the backlog fits.
    SmallShare,
}

impl QuickHold {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TooFew => "quick_too_few",
            Self::SmallShare => "quick_small_share",
        }
    }
}

/// THE PLANNER: price every item, sort by cost, fill [`BUDGET_SECS`], and
/// keep the result only when it clears the floors. `multiplier` is
/// [`multiplier`]'s answer for this owner (1.0 when unknown).
pub fn plan(items: &[OpenItem], multiplier: f64) -> Result<QuickClear, QuickHold> {
    let mut seen = HashSet::new();
    let backlog: Vec<&OpenItem> = items.iter().filter(|i| seen.insert(i.message_id)).collect();
    let open_count = backlog.len();
    let mut priced: Vec<(&OpenItem, Estimate, u32)> = backlog
        .into_iter()
        .filter_map(|i| {
            let raw = estimate(i)?;
            Some((i, calibrated(raw, multiplier), raw.secs))
        })
        .filter(|(_, e, _)| e.secs <= MAX_ITEM_SECS)
        .collect();
    priced.sort_by(|(a, ea, _), (b, eb, _)| {
        ea.secs.cmp(&eb.secs).then(b.message_id.cmp(&a.message_id))
    });
    let mut total = 0u32;
    let mut queue = Vec::new();
    let mut smart = 0usize;
    for (item, e, raw_secs) in priced {
        let cost = if e.basis == Basis::SmartClear {
            if smart == 0 {
                SMART_CLEAR_PRESS_SECS
            } else {
                0
            }
        } else {
            e.secs
        };
        if total + cost > BUDGET_SECS {
            // Sorted cheapest first: nothing after this fits either.
            break;
        }
        total += cost;
        if e.basis == Basis::SmartClear {
            smart += 1;
        }
        queue.push(Planned {
            message_id: item.message_id,
            thread_id: item.thread_id.clone(),
            est_secs: e.secs,
            raw_secs,
            basis: e.basis,
            kind: e.kind,
            sender: item.sender.clone(),
            one_line: item.one_line.clone(),
        });
    }
    let fit = queue.len();
    if fit < MIN_FIT {
        return Err(QuickHold::TooFew);
    }
    if fit * 100 < MIN_FIT_PERCENT * open_count {
        return Err(QuickHold::SmallShare);
    }
    Ok(QuickClear {
        budget_secs: BUDGET_SECS,
        est_secs: total,
        fit_count: fit,
        open_count,
        smart_clear_count: smart,
        items: queue,
    })
}

/// One item of a past quick-clear banner, as the store found it afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalibrationSample {
    pub basis: Basis,
    pub predicted_secs: u32,
    /// What it really took, when that can be told ([`actual_secs`]).
    pub actual_secs: Option<u32>,
    /// Opened, done or replied to after the banner.
    pub handled: bool,
}

/// What an item really took after a banner fired at `fired_at`: the dwell
/// the client reported, else the time from opening it to marking it done
/// when that was one sitting. `None` when neither can be told (not opened
/// since the banner, or opened with nothing reported).
pub fn actual_secs(
    fired_at: DateTime<Utc>,
    opened_at: Option<DateTime<Utc>>,
    dwell_ms: u64,
    resolved_at: Option<DateTime<Utc>>,
) -> Option<u32> {
    let opened = opened_at.filter(|t| *t >= fired_at)?;
    if dwell_ms > 0 {
        return Some(ceil_secs(dwell_ms));
    }
    let took = (resolved_at? - opened).num_seconds();
    (0..=MAX_OPEN_TO_DONE_SECS)
        .contains(&took)
        .then_some(took as u32)
}

/// The per-owner multiplier: what measured items really took over what we
/// predicted, across model and heuristic items, clamped to
/// [`CALIBRATION_MIN`]..=[`CALIBRATION_MAX`]. 1.0 until
/// [`CALIBRATION_MIN_SAMPLES`] items have been measured.
pub fn multiplier(samples: &[CalibrationSample]) -> f64 {
    let measured: Vec<(u32, u32)> = samples
        .iter()
        .filter(|s| matches!(s.basis, Basis::Model | Basis::Heuristic))
        .filter_map(|s| Some((s.predicted_secs, s.actual_secs?)))
        .collect();
    let predicted: u64 = measured.iter().map(|(p, _)| u64::from(*p)).sum();
    if measured.len() < CALIBRATION_MIN_SAMPLES || predicted == 0 {
        return 1.0;
    }
    let actual: u64 = measured.iter().map(|(_, a)| u64::from(*a)).sum();
    (actual as f64 / predicted as f64).clamp(CALIBRATION_MIN, CALIBRATION_MAX)
}

/// One basis's record, for the stats route.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct BasisTally {
    pub items: u32,
    pub handled: u32,
    /// Items whose real time could be told.
    pub measured: u32,
    /// Over the measured items only, so the two compare.
    pub predicted_secs: u64,
    pub actual_secs: u64,
}

/// Predicted against actual, per basis, and the multiplier they give.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Calibration {
    pub by_basis: BTreeMap<&'static str, BasisTally>,
    pub multiplier: f64,
}

pub fn calibrate(samples: &[CalibrationSample]) -> Calibration {
    let mut by_basis: BTreeMap<&'static str, BasisTally> = BTreeMap::new();
    for s in samples {
        let t = by_basis.entry(s.basis.as_str()).or_default();
        t.items += 1;
        t.handled += u32::from(s.handled);
        if let Some(actual) = s.actual_secs {
            t.measured += 1;
            t.predicted_secs += u64::from(s.predicted_secs);
            t.actual_secs += u64::from(actual);
        }
    }
    Calibration {
        by_basis,
        multiplier: multiplier(samples),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triage::decision::{AttentionAction, EffortEstimate};
    use chrono::{Duration, TimeZone};

    fn item(id: i64) -> OpenItem {
        OpenItem {
            message_id: id,
            thread_id: format!("t{id}"),
            sender: "alice@example.com".into(),
            one_line: format!("line {id}"),
            decision: MessageDecision {
                kinds: vec![EmailKind::General],
                ..MessageDecision::default()
            },
            attention: ThreadAttentionDecision {
                show_in_fye: true,
                state: AttentionState::Informational,
                ..ThreadAttentionDecision::default()
            },
            smart_clear: false,
            sealed: false,
            reminder: false,
            sender_protected: false,
            words: 0,
            sender_dwell: None,
        }
    }

    fn with_action(mut i: OpenItem, kind: ActionKind) -> OpenItem {
        i.attention.state = AttentionState::NeedsUser;
        i.attention.actions.push(AttentionAction {
            kind,
            description: "do it".into(),
            ..AttentionAction::default()
        });
        i
    }

    fn with_kind(mut i: OpenItem, kind: EmailKind) -> OpenItem {
        i.decision.kinds = vec![kind];
        i
    }

    fn est(i: &OpenItem) -> Option<(u32, Basis, ItemKind)> {
        estimate(i).map(|e| (e.secs, e.basis, e.kind))
    }

    #[test]
    fn the_heuristic_prices_each_shape() {
        assert_eq!(
            est(&item(1)),
            Some((GLANCE_SECS, Basis::Heuristic, ItemKind::Glance))
        );
        let mut resolved = item(2);
        resolved.attention.state = AttentionState::Resolved;
        assert_eq!(est(&resolved).map(|e| e.2), Some(ItemKind::Glance));
        assert_eq!(
            est(&with_kind(item(3), EmailKind::Receipt)),
            Some((RECORD_SECS, Basis::Heuristic, ItemKind::Receipt))
        );
        assert_eq!(
            est(&with_kind(item(4), EmailKind::Delivery)).map(|e| e.2),
            Some(ItemKind::Delivery)
        );
        assert_eq!(
            est(&with_kind(item(5), EmailKind::AccountService)).map(|e| e.2),
            Some(ItemKind::AccountService)
        );
        assert_eq!(
            est(&with_action(item(6), ActionKind::Review)),
            Some((REVIEW_SECS, Basis::Heuristic, ItemKind::Review))
        );
        assert_eq!(
            est(&with_action(item(7), ActionKind::Attend)),
            Some((ATTEND_SECS, Basis::Heuristic, ItemKind::Attend))
        );
        // Two open actions add up.
        let both = with_action(with_action(item(8), ActionKind::Attend), ActionKind::Review);
        assert_eq!(
            est(&both),
            Some((
                REVIEW_SECS + ATTEND_SECS,
                Basis::Heuristic,
                ItemKind::Review
            ))
        );
    }

    #[test]
    fn reply_pay_decide_and_unknown_needs_are_never_quick() {
        for kind in [
            ActionKind::Reply,
            ActionKind::Pay,
            ActionKind::Decide,
            ActionKind::Other,
        ] {
            assert_eq!(est(&with_action(item(1), kind)), None, "{kind:?}");
            // Even beside a quick one.
            let mixed = with_action(with_action(item(1), ActionKind::Review), kind);
            assert_eq!(est(&mixed), None, "{kind:?} beside a review");
        }
        // A resolved reply is not open.
        let mut done = with_action(item(2), ActionKind::Reply);
        done.attention.actions[0].resolved = true;
        done.attention.state = AttentionState::Resolved;
        assert!(est(&done).is_some());
        // Needs the owner, with nothing named to do: unknown, so out.
        let mut needs = item(3);
        needs.attention.state = AttentionState::NeedsUser;
        assert_eq!(est(&needs), None);
    }

    #[test]
    fn sealed_auth_restricted_and_reminded_mail_is_never_in_the_queue() {
        let mut sealed = item(1);
        sealed.sealed = true;
        assert_eq!(est(&sealed), None);
        let mut auth = item(2);
        auth.decision.auth.kinds = vec![crate::triage::decision::AuthKind::LoginAlert];
        assert_eq!(est(&auth), None);
        let mut restricted = item(3);
        restricted.decision.external_access.restricted = true;
        assert_eq!(est(&restricted), None);
        let mut reminded = item(4);
        reminded.reminder = true;
        assert_eq!(est(&reminded), None);
        // Sealed beats a smart-clear flag too.
        sealed.smart_clear = true;
        assert_eq!(est(&sealed), None);
    }

    #[test]
    fn a_protected_sender_is_quick_only_as_a_plain_glance() {
        let mut glance = item(1);
        glance.sender_protected = true;
        assert!(est(&glance).is_some());
        let mut review = with_action(item(2), ActionKind::Review);
        review.sender_protected = true;
        assert_eq!(est(&review), None);
        let mut read = item(3);
        read.decision.reading_kind = Some(ReadingKind::Content);
        read.words = 250;
        assert!(est(&read).is_some());
        read.sender_protected = true;
        assert_eq!(est(&read), None);
    }

    #[test]
    fn reading_content_costs_its_words() {
        let mut read = item(1);
        read.decision.reading_kind = Some(ReadingKind::Content);
        read.words = 500;
        assert_eq!(est(&read), Some((120, Basis::Heuristic, ItemKind::Read)));
        read.words = 10;
        assert_eq!(est(&read).map(|e| e.0), Some(MIN_READ_SECS));
    }

    #[test]
    fn the_model_beats_the_heuristic_and_dwell_beats_both() {
        let mut i = with_kind(item(1), EmailKind::Receipt);
        i.decision.effort = Some(EffortEstimate {
            seconds: 12,
            task: EffortTask::Skim,
        });
        assert_eq!(est(&i), Some((12, Basis::Model, ItemKind::Receipt)));
        // Two looks are not a history yet.
        i.sender_dwell = Some(DwellHistory {
            samples: MIN_DWELL_SAMPLES - 1,
            median_ms: 3_500,
        });
        assert_eq!(est(&i).map(|e| e.1), Some(Basis::Model));
        i.sender_dwell = Some(DwellHistory {
            samples: MIN_DWELL_SAMPLES,
            median_ms: 3_500,
        });
        assert_eq!(est(&i), Some((4, Basis::Dwell, ItemKind::Receipt)));
        // A dwell-priced review still pays for its action.
        let mut r = with_action(item(2), ActionKind::Review);
        r.sender_dwell = Some(DwellHistory {
            samples: 5,
            median_ms: 6_000,
        });
        assert_eq!(
            est(&r),
            Some((6 + ACTION_OVERHEAD_SECS, Basis::Dwell, ItemKind::Review))
        );
    }

    #[test]
    fn a_model_saying_reply_or_involved_takes_it_out() {
        for task in [EffortTask::Reply, EffortTask::Involved] {
            let mut i = item(1);
            i.decision.effort = Some(EffortEstimate { seconds: 5, task });
            assert_eq!(est(&i), None, "{task:?}");
        }
        // A timeless estimate is no estimate: the heuristic stands.
        let mut i = item(2);
        i.decision.effort = Some(EffortEstimate {
            seconds: 0,
            task: EffortTask::Glance,
        });
        assert_eq!(est(&i).map(|e| e.1), Some(Basis::Heuristic));
    }

    #[test]
    fn smart_clear_rows_are_free_and_one_press_is_priced_once() {
        let mut items: Vec<OpenItem> = (1..=6)
            .map(|id| {
                let mut i = item(id);
                i.smart_clear = true;
                i
            })
            .collect();
        let q = plan(&items, 1.0).unwrap();
        assert_eq!(q.smart_clear_count, 6);
        assert_eq!(q.est_secs, SMART_CLEAR_PRESS_SECS);
        assert!(
            q.items
                .iter()
                .all(|p| p.basis == Basis::SmartClear && p.est_secs == 0)
        );
        // They lead the queue.
        items.push(with_kind(item(10), EmailKind::Receipt));
        let q = plan(&items, 1.0).unwrap();
        assert_eq!(q.items.last().unwrap().message_id, 10);
        assert_eq!(q.est_secs, SMART_CLEAR_PRESS_SECS + RECORD_SECS);
    }

    #[test]
    fn the_budget_is_filled_cheapest_first_and_the_counts_are_honest() {
        // 6 glances (48s), 6 reviews (240s), 4 replies: 16 open.
        let mut items: Vec<OpenItem> = (1..=6).map(item).collect();
        items.extend((11..=16).map(|id| with_action(item(id), ActionKind::Review)));
        items.extend((21..=24).map(|id| with_action(item(id), ActionKind::Reply)));
        let q = plan(&items, 1.0).unwrap();
        assert_eq!(q.open_count, 16, "replies count in the backlog");
        // 6 glances + 6 reviews = 48 + 240 = 288 <= 300.
        assert_eq!(q.fit_count, 12);
        assert_eq!(q.est_secs, 288);
        assert!(q.est_secs <= q.budget_secs);
        assert_eq!(q.items.len(), q.fit_count);
        // Cheapest first.
        let costs: Vec<u32> = q.items.iter().map(|p| p.est_secs).collect();
        let mut sorted = costs.clone();
        sorted.sort();
        assert_eq!(costs, sorted);
        // One more review would not fit (288 + 40 > 300).
        items.push(with_action(item(17), ActionKind::Review));
        let q = plan(&items, 1.0).unwrap();
        assert_eq!(q.fit_count, 12);
        assert_eq!(q.open_count, 17);
        // Equal costs go newest first, so the oldest review is the one left.
        assert!(q.items.iter().any(|p| p.message_id == 17));
        assert!(!q.items.iter().any(|p| p.message_id == 11));
    }

    #[test]
    fn a_message_listed_twice_counts_once() {
        let items: Vec<OpenItem> = (1..=5).chain([1]).map(item).collect();
        let q = plan(&items, 1.0).unwrap();
        assert_eq!(q.open_count, 5);
        assert_eq!(q.fit_count, 5);
    }

    #[test]
    fn the_floors_hold() {
        // Four quick items: too few, however small the backlog.
        let items: Vec<OpenItem> = (1..=4).map(item).collect();
        assert_eq!(plan(&items, 1.0), Err(QuickHold::TooFew));
        // Five quick of 20 open is exactly 25%: enough.
        let mut items: Vec<OpenItem> = (1..=5).map(item).collect();
        items.extend((11..=25).map(|id| with_action(item(id), ActionKind::Reply)));
        assert_eq!(plan(&items, 1.0).unwrap().fit_count, 5);
        // One more slow item: 5 of 21 is under 25%.
        items.push(with_action(item(30), ActionKind::Pay));
        assert_eq!(plan(&items, 1.0), Err(QuickHold::SmallShare));
    }

    #[test]
    fn an_item_too_long_for_a_sprint_is_left_out() {
        let mut items: Vec<OpenItem> = (1..=5).map(item).collect();
        let mut long = item(9);
        long.decision.effort = Some(EffortEstimate {
            seconds: i64::from(MAX_ITEM_SECS) + 1,
            task: EffortTask::LongRead,
        });
        items.push(long);
        let q = plan(&items, 1.0).unwrap();
        assert!(!q.items.iter().any(|p| p.message_id == 9));
        assert_eq!(q.open_count, 6);
    }

    #[test]
    fn the_multiplier_scales_model_and_heuristic_numbers_only() {
        let mut items: Vec<OpenItem> = (1..=5).map(item).collect();
        let mut dwell = item(9);
        dwell.sender_dwell = Some(DwellHistory {
            samples: 4,
            median_ms: 10_000,
        });
        items.push(dwell);
        let q = plan(&items, 2.0).unwrap();
        let by_id = |id: i64| {
            q.items
                .iter()
                .find(|p| p.message_id == id)
                .unwrap()
                .est_secs
        };
        assert_eq!(by_id(1), GLANCE_SECS * 2);
        assert_eq!(by_id(9), 10, "dwell is the owner's own clock");
        // The raw estimate rides along untouched: the multiplier is learned
        // against it, never against its own last correction.
        let raw = |id: i64| {
            q.items
                .iter()
                .find(|p| p.message_id == id)
                .unwrap()
                .raw_secs
        };
        assert_eq!(raw(1), GLANCE_SECS);
        assert_eq!(raw(9), 10);
        // Out-of-range and nonsense multipliers are held to the range.
        let q = plan(&items, 50.0).unwrap();
        assert_eq!(
            q.items.iter().find(|p| p.message_id == 1).unwrap().est_secs,
            (f64::from(GLANCE_SECS) * CALIBRATION_MAX) as u32
        );
        let q = plan(&items, f64::NAN).unwrap();
        assert_eq!(
            q.items.iter().find(|p| p.message_id == 1).unwrap().est_secs,
            GLANCE_SECS
        );
    }

    fn t(m: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 18, 10, 0, 0).unwrap() + Duration::minutes(m)
    }

    #[test]
    fn actual_time_is_dwell_else_one_sitting_from_open_to_done() {
        let fired = t(0);
        assert_eq!(actual_secs(fired, Some(t(1)), 4_200, None), Some(5));
        // Opened before the banner: says nothing about the sprint.
        assert_eq!(actual_secs(fired, Some(t(-1)), 4_200, None), None);
        assert_eq!(actual_secs(fired, None, 0, Some(t(2))), None);
        // No dwell: open to done, when that was one sitting.
        let opened = t(1);
        assert_eq!(
            actual_secs(fired, Some(opened), 0, Some(opened + Duration::seconds(30))),
            Some(30)
        );
        assert_eq!(
            actual_secs(fired, Some(opened), 0, Some(opened + Duration::hours(2))),
            None
        );
        assert_eq!(actual_secs(fired, Some(opened), 0, None), None);
    }

    fn sample(basis: Basis, predicted: u32, actual: Option<u32>) -> CalibrationSample {
        CalibrationSample {
            basis,
            predicted_secs: predicted,
            actual_secs: actual,
            handled: actual.is_some(),
        }
    }

    #[test]
    fn the_multiplier_waits_for_enough_measured_items_and_is_clamped() {
        let slow: Vec<CalibrationSample> = (0..CALIBRATION_MIN_SAMPLES - 1)
            .map(|_| sample(Basis::Heuristic, 10, Some(15)))
            .collect();
        assert_eq!(multiplier(&slow), 1.0);
        let mut enough = slow.clone();
        enough.push(sample(Basis::Model, 10, Some(15)));
        assert!((multiplier(&enough) - 1.5).abs() < 1e-9);
        // Unmeasured, dwell and smart-clear items do not count.
        let mut noise = slow.clone();
        noise.push(sample(Basis::Heuristic, 10, None));
        noise.push(sample(Basis::Dwell, 10, Some(100)));
        noise.push(sample(Basis::SmartClear, 0, Some(0)));
        assert_eq!(multiplier(&noise), 1.0);
        let wild: Vec<CalibrationSample> = (0..20)
            .map(|_| sample(Basis::Heuristic, 5, Some(500)))
            .collect();
        assert_eq!(multiplier(&wild), CALIBRATION_MAX);
        let fast: Vec<CalibrationSample> = (0..20)
            .map(|_| sample(Basis::Heuristic, 50, Some(1)))
            .collect();
        assert_eq!(multiplier(&fast), CALIBRATION_MIN);
    }

    #[test]
    fn calibration_tallies_predicted_against_actual_per_basis() {
        let c = calibrate(&[
            sample(Basis::Heuristic, 8, Some(6)),
            sample(Basis::Heuristic, 40, None),
            sample(Basis::Dwell, 4, Some(5)),
        ]);
        let h = c.by_basis["heuristic"];
        assert_eq!(h.items, 2);
        assert_eq!(h.measured, 1);
        assert_eq!(h.handled, 1);
        assert_eq!((h.predicted_secs, h.actual_secs), (8, 6));
        assert_eq!(c.by_basis["dwell"].actual_secs, 5);
        assert_eq!(c.multiplier, 1.0);
    }
}
