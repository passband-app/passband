//! The budget poller: tops up a hosted tenant's triage key, once per budget
//! window, when it is about to run out.
//!
//! WHY. Every tenant's triage key carries a monthly budget on the gateway, and
//! when it is spent the daemon's triage calls fail and Passband quietly gets
//! dumb. The person cannot see a budget and should never feel one, so the fix
//! is not theirs to ask for: when the key reaches [`TOPUP_THRESHOLD`] of what
//! it may spend this window, this adds one more month of the key's own limit,
//! for this window only, and tells the operator.
//!
//! THE MECHANISM IS `llm topup`'s and signup's: [`BifrostClient::top_up_key`],
//! the gateway's one-cycle budget override, read back. No new key, no warden
//! install, no pod restart, and the extra allowance expires by itself at the
//! next reset.
//!
//! ONCE PER TENANT PER WINDOW, AND THE STORE IS WHAT SAYS SO. The gateway alone
//! cannot: an override REPLACES rather than stacks, and nothing on it records
//! who set it, so "already has an override" cannot tell this poller's top-up
//! from the signup allowance or an operator's. The window is named by the
//! gateway's own `last_reset`, which moves exactly when the spend resets, and
//! a `(tenant, window)` row is claimed in Postgres before the gateway is
//! touched (see [`crate::store::ControlStore::claim_auto_topup`]). Two control
//! tasks, or a restart mid-call, cannot both own a window.
//!
//! STACKING WITH WHAT IS ALREADY THERE. The new override is the override that
//! is live in this window plus one month, so the signup allowance (or an
//! operator's top-up) is kept and this adds to it rather than clobbering it. A
//! new person's first window can therefore reach three months' limit: one
//! base, one signup allowance for the backfill, one top-up for their own use.
//! That is the most any window can reach without the operator acting, because
//! this adds at most one month per window. An override in `forever` mode was
//! set by hand and is left alone.
//!
//! PRIVACY AND LOGS: labels, key ids and tallies. No dollar amounts.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::bifrost::BudgetState;
use crate::state::ControlState;

/// The share of what a key may spend this window at which it is topped up.
/// Below 100% so the top-up lands before triage starts failing: the poll runs
/// every few minutes, and the last few percent is what covers that gap.
pub const TOPUP_THRESHOLD: f64 = 0.95;

/// The most top-ups one pass may attempt. A bound on how far a bad reading can
/// reach before a person looks: a pass that wants more than this leaves the
/// rest for the next pass, a few minutes later.
pub const TOPUP_BATCH: usize = 10;

/// The most tenants one pass reads from the store.
const CANDIDATE_LIMIT: i64 = 10_000;

/// How long a claimed attempt that did not land is left before a pass may
/// retry it. Longer than any one attempt can take, so two control tasks never
/// run the same window's top-up at once; short enough that a failed one is
/// retried well before anybody notices.
pub const RETRY_AFTER: Duration = Duration::from_secs(30 * 60);

/// After the first failure alert for a tenant's window, the least time before
/// the next one. Every attempt is still logged.
pub const FAILURE_ALERT_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// How close to a window's end a key is left alone. The gateway anchors a
/// grant on ITS clock at the moment the write lands, so a write sent just
/// before a reset can land just after it, in the next window. Five minutes is
/// far longer than one pass's serial writes and much shorter than a window.
pub const BOUNDARY_MARGIN: chrono::Duration = chrono::Duration::minutes(5);

/// What the poller should do about one tenant's key.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Skip(Skip),
    /// Claim `cycle` and set the key's override to `target_usd`.
    TopUp {
        cycle: String,
        target_usd: f64,
    },
    /// The previous window's claim landed in THIS window (the write crossed
    /// the reset and its confirmation never came back): `sent_usd` is live
    /// here. Recorded as this window's top-up; see [`land_late`].
    LandedLate {
        cycle: String,
        sent_usd: f64,
    },
}

/// Why a key was left alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// The tenant was torn down.
    Inactive,
    /// The gateway did not say when this window opened, or how long one lasts,
    /// so there is no window to count a top-up against.
    UnknownWindow,
    /// The reading straddles a window boundary: the override was granted in a
    /// later window than `last_reset` names, or the window ends within
    /// [`BOUNDARY_MARGIN`]. The next pass reads it settled.
    WindowInFlux,
    /// A hand-set override this poller would clobber: `forever`, or one
    /// covering more than this window (every write here is one cycle, and an
    /// override replaces). The operator owns this key's limit.
    OperatorOverride,
    /// A zero limit, or one that will not parse: not a key a top-up can help.
    NoLimit,
    /// This window already had its top-up.
    AlreadyTopped,
    /// Spending is below [`TOPUP_THRESHOLD`] of what the key may spend.
    BelowThreshold,
}

/// The name of the window `budget` is in: its `last_reset`, rounded to the
/// second (the gateway's boundaries cross JSON with sub-microsecond noise) and
/// written in the store's stamp shape, so a later window is a later string.
pub fn cycle_key(last_reset: DateTime<Utc>) -> String {
    let secs =
        last_reset.timestamp() + i64::from(last_reset.timestamp_subsec_nanos() >= 500_000_000);
    crate::store::stamp(DateTime::from_timestamp(secs, 0).unwrap_or(last_reset))
}

/// A window length in the gateway's spelling: a whole number and one of
/// `s m h d w M Y`, where a month is 30 days and a year 365, as the gateway
/// itself counts them for a rolling budget. `None` for anything else.
pub fn reset_duration(spec: &str) -> Option<chrono::Duration> {
    let spec = spec.trim();
    let unit = spec.chars().last()?;
    let n: i64 = spec[..spec.len() - unit.len_utf8()].parse().ok()?;
    if n <= 0 {
        return None;
    }
    let secs = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86_400,
        'w' => 7 * 86_400,
        'M' => 30 * 86_400,
        'Y' => 365 * 86_400,
        _ => return None,
    };
    n.checked_mul(secs).map(chrono::Duration::seconds)
}

/// How many windows after `budget`'s current one the instant `t` falls in:
/// 0 for the current window, 1 for the next, -1 for the one before.
///
/// BY WINDOW, NOT BY TIMESTAMP. In v1.6.9 both `last_reset` and an override's
/// anchor are the same rolling boundary (`TableBudget::WindowStart`), but a
/// budget can carry a `last_reset` that is the moment a reset ran rather than
/// the boundary itself, and a timestamp compare would then put an override
/// granted in this window into the last one. Rounding the distance to whole
/// windows absorbs any such lag short of half a window.
pub fn window_offset(budget: &BudgetState, t: DateTime<Utc>) -> Option<i64> {
    let opened = budget.last_reset?;
    let length = reset_duration(&budget.reset_duration)?.num_seconds();
    let diff = (t - opened).num_seconds();
    Some((diff as f64 / length as f64).round() as i64)
}

/// The override that is live in `budget`'s current window, in dollars, read
/// the way the gateway reads it (`TableBudget::HasActiveOverride`): a cycles
/// override with cycles remaining. The gateway re-derives the remaining count
/// whenever `last_reset` advances, so an override from a window that has
/// closed reads zero here, and a multi-cycle one an operator granted reads as
/// live in every window it covers, whatever its anchor says.
pub fn live_override(budget: &BudgetState) -> f64 {
    let amount = budget.override_amount;
    if amount.is_finite()
        && amount > 0.0
        && budget.override_mode == "cycles"
        && budget.override_cycles_remaining > 0
    {
        amount
    } else {
        0.0
    }
}

/// What the store remembers about a tenant's top-ups.
#[derive(Debug, Clone, Copy, Default)]
pub struct History<'a> {
    /// The newest window topped up, as a store stamp.
    pub last_applied: Option<&'a str>,
    /// The newest claim still pending: its window and the amount it sent.
    pub pending: Option<(&'a str, f64)>,
}

/// Decide what to do about one tenant's triage budget at `now`.
///
/// The decision is pure; the store's claim is what makes "once per window"
/// hold under races.
pub fn decide(
    active: bool,
    budget: &BudgetState,
    history: History<'_>,
    now: DateTime<Utc>,
) -> Decision {
    if !active {
        return Decision::Skip(Skip::Inactive);
    }
    let (Some(opened), Some(_)) = (budget.last_reset, reset_duration(&budget.reset_duration))
    else {
        return Decision::Skip(Skip::UnknownWindow);
    };
    let anchor_offset = budget
        .override_anchor_reset
        .and_then(|a| window_offset(budget, a));
    if anchor_offset.is_some_and(|o| o >= 1) || window_closing(budget, now) {
        return Decision::Skip(Skip::WindowInFlux);
    }
    if budget.override_mode == "forever" || budget.override_cycles_remaining > 1 {
        return Decision::Skip(Skip::OperatorOverride);
    }
    let base = budget.max_limit;
    if !(base.is_finite() && base > 0.0) {
        return Decision::Skip(Skip::NoLimit);
    }
    // A stored window this one or later (an unreadable one counts as later:
    // the safe reading is "done").
    if history
        .last_applied
        .is_some_and(|stamp| stamp_offset(budget, stamp).is_none_or(|o| o >= 0))
    {
        return Decision::Skip(Skip::AlreadyTopped);
    }
    let cycle = cycle_key(opened);
    let existing = live_override(budget);
    // The PREVIOUS window's claim, whose amount is what this window carries,
    // granted in this window: the write crossed the reset. It is this
    // window's top-up, whatever the confirmation said at the time. Only the
    // window just before: an older claim cannot have crossed into this one.
    if let Some((claimed_window, claimed_usd)) = history.pending
        && stamp_offset(budget, claimed_window) == Some(-1)
        && anchor_offset == Some(0)
        && existing > 0.0
        && (existing - claimed_usd).abs() <= 0.005
    {
        return Decision::LandedLate {
            cycle,
            sent_usd: existing,
        };
    }
    let limit = base + existing;
    if !(budget.current_usage.is_finite() && budget.current_usage >= TOPUP_THRESHOLD * limit) {
        return Decision::Skip(Skip::BelowThreshold);
    }
    Decision::TopUp {
        cycle,
        target_usd: round_up_cents(existing + base),
    }
}

/// [`window_offset`] of a store stamp; `None` when either will not read.
pub fn stamp_offset(budget: &BudgetState, stamp: &str) -> Option<i64> {
    let t = DateTime::parse_from_rfc3339(stamp)
        .ok()?
        .with_timezone(&Utc);
    window_offset(budget, t)
}

/// Rounded up to the cent, as `llm topup` rounds, so a read-back compares
/// against what the gateway stores.
fn round_up_cents(usd: f64) -> f64 {
    (usd * 100.0).ceil() / 100.0
}

/// Whether `budget`'s window has ended, or ends within [`BOUNDARY_MARGIN`] of
/// `now`. An unreadable window counts as closing: nothing is written to it.
pub fn window_closing(budget: &BudgetState, now: DateTime<Utc>) -> bool {
    match (budget.last_reset, reset_duration(&budget.reset_duration)) {
        (Some(opened), Some(length)) => opened + length <= now + BOUNDARY_MARGIN,
        _ => true,
    }
}

/// What to do with a claim, given the override live on the key right now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Send {
    /// Write this override.
    Write(f64),
    /// The key already carries at least the claimed amount (an operator topped
    /// it up by hand after an earlier attempt failed): record the window as
    /// done and write nothing, so the larger figure is never lowered.
    AlreadyCovered,
}

/// The write a claim of `claimed_usd` turns into when the key carries
/// `live_usd` in this window. Never a value below what is live.
pub fn send_for(claimed_usd: f64, live_usd: f64) -> Send {
    if live_usd + 0.005 >= claimed_usd {
        Send::AlreadyCovered
    } else {
        Send::Write(claimed_usd)
    }
}

/// One budget pass. Returns how many top-ups were applied and read back.
///
/// Errors are eaten deliberately, as in the other pollers: a store or gateway
/// that does not answer is retried on the next pass. A top-up that fails is
/// different, because the tenant is about to go dark: every attempt is a loud
/// log line, the operator hears about the first one in a window and then at
/// most once per [`FAILURE_ALERT_EVERY`], and the claim stays pending so a
/// later pass retries it with the same amount.
pub async fn poll_budgets(state: &ControlState) -> usize {
    let Some((bifrost, _)) = state.bifrost() else {
        return 0;
    };
    let candidates = match state.store().auto_topup_candidates(CANDIDATE_LIMIT).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "budget top-up: could not read the candidate set");
            return 0;
        }
    };
    if candidates.is_empty() {
        return 0;
    }
    let keys = match bifrost.list_virtual_keys().await {
        Ok(keys) => keys,
        Err(e) => {
            tracing::warn!(error = %e, "budget top-up: gateway listing failed");
            return 0;
        }
    };
    let budgets: HashMap<&str, &[BudgetState]> = keys
        .iter()
        .map(|k| (k.id.as_str(), k.budgets.as_slice()))
        .collect();

    let now = Utc::now();
    let ago = |d: Duration| now - chrono::Duration::from_std(d).unwrap_or(chrono::Duration::zero());
    let retry_before = ago(RETRY_AFTER);
    let quiet_before = ago(FAILURE_ALERT_EVERY);
    let mut attempted = 0usize;
    let mut applied = 0usize;
    let mut unknown_window = 0usize;
    for c in &candidates {
        if attempted >= TOPUP_BATCH {
            break;
        }
        // A key the gateway does not list, or with other than the one budget
        // a mint gives it, is not one this poller will guess about.
        let Some([budget]) = budgets.get(c.vk_id.as_str()).copied() else {
            continue;
        };
        let label = c.label.as_str();
        let history = History {
            last_applied: c.last_applied_cycle.as_deref(),
            pending: c.pending_cycle.as_deref().zip(c.pending_target_usd),
        };
        // The clock is read per tenant, not once per pass: the writes ahead
        // of this one take time, and the boundary margin is measured from
        // when this one is decided.
        let decision = decide(c.active, budget, history, Utc::now());
        // A pending claim from a window that has ended is done with, whether
        // it just landed late (below) or never landed: closed, so it can
        // never be mistaken for anything again.
        if let Some((claimed_window, _)) = history.pending
            && stamp_offset(budget, claimed_window).is_some_and(|o| o < 0)
            && let Err(e) = state
                .store()
                .close_auto_topup(c.tenant_id, claimed_window)
                .await
        {
            tracing::error!(error = %e, %label, "budget top-up: could not close an old claim");
        }
        let (cycle, target) = match decision {
            Decision::TopUp { cycle, target_usd } => (cycle, target_usd),
            Decision::LandedLate { cycle, sent_usd } => {
                attempted += 1;
                land_late(state, bifrost, c, &cycle, sent_usd, now).await;
                continue;
            }
            Decision::Skip(Skip::UnknownWindow) => {
                unknown_window += 1;
                continue;
            }
            Decision::Skip(_) => continue,
        };
        let (claimed, attempt) = match state
            .store()
            .claim_auto_topup(c.tenant_id, &cycle, target, now, retry_before)
            .await
        {
            Ok(Some(claim)) => claim,
            // Done already, or another attempt is in flight. Not counted
            // against the batch: a held claim costs nothing, and counting it
            // would let a few stuck tenants starve everyone after them.
            Ok(None) => continue,
            Err(e) => {
                tracing::error!(error = %e, %label, "budget top-up: claim failed");
                continue;
            }
        };
        attempted += 1;
        let mark_applied = |cycle: String| async move {
            if let Err(e) = state
                .store()
                .mark_auto_topup_applied(c.tenant_id, &cycle, now)
                .await
            {
                // The tenant has the money; only the record is missing. A
                // retry finds the override live and writes nothing.
                tracing::error!(error = %e, %label, "budget top-up: applied but not recorded");
            }
        };
        // The claim may be a RETRY carrying the first attempt's amount, and
        // somebody may have topped the key up by hand since. Never lower it.
        let amount = match send_for(claimed, live_override(budget)) {
            Send::Write(amount) => amount,
            Send::AlreadyCovered => {
                // Reachable only at the threshold, so the tenant is about to
                // run dry on a top-up somebody already gave. Worth a person.
                mark_applied(cycle.clone()).await;
                tracing::warn!(%label, vk_id = %c.vk_id, "budget top-up: already covered, nothing written");
                state.alert(crate::alert::llm_auto_topup_covered(label));
                continue;
            }
        };
        // Once more, right before the write: a pass can be slow, and a write
        // that lands after the reset is granted in the next window. The claim
        // stays pending; the next pass reads the new window.
        if window_closing(budget, Utc::now()) {
            tracing::debug!(%label, "budget top-up: window closing, write deferred");
            continue;
        }
        match bifrost.top_up_key(&c.vk_id, amount).await {
            Ok(after) => {
                mark_applied(cycle.clone()).await;
                applied += 1;
                // The gateway anchors the grant on its own clock. If it put
                // this one in a later window, that window has had its top-up,
                // and gets one month of it rather than this window's stack.
                match after
                    .override_anchor_reset
                    .filter(|a| window_offset(budget, *a).is_some_and(|o| o >= 1))
                {
                    Some(anchor) => {
                        land_late(state, bifrost, c, &cycle_key(anchor), amount, now).await;
                    }
                    None => {
                        tracing::info!(%label, vk_id = %c.vk_id, "budget top-up: auto top-up applied");
                        state.alert(crate::alert::llm_auto_topup_applied(label));
                    }
                }
            }
            Err(e) => {
                let unconfirmed = matches!(e, crate::bifrost::BifrostError::Unconfirmed);
                if unconfirmed {
                    tracing::error!(
                        error = %e,
                        %label,
                        vk_id = %c.vk_id,
                        attempt,
                        "AUTO TOP-UP NOT CONFIRMED: the write may have landed; retried automatically with the same amount"
                    );
                } else {
                    tracing::error!(
                        error = %e,
                        %label,
                        vk_id = %c.vk_id,
                        attempt,
                        "AUTO TOP-UP FAILED: this tenant's triage stops when its budget runs out; retried automatically, or `squelch-control llm topup <label>` by hand"
                    );
                }
                let alert = if unconfirmed {
                    crate::alert::llm_auto_topup_unconfirmed(label, attempt)
                } else {
                    crate::alert::llm_auto_topup_failed(label, &e.to_string(), attempt)
                };
                match state
                    .store()
                    .take_topup_failure_alert(c.tenant_id, &cycle, unconfirmed, now, quiet_before)
                    .await
                {
                    Ok(true) => state.alert(alert),
                    Ok(false) => {}
                    // Better a repeat than silence while a tenant goes dark.
                    Err(_) => state.alert(alert),
                }
            }
        }
    }
    if unknown_window > 0 {
        // A gateway that stopped reporting `last_reset` turns this poller off
        // for every key, so it is worth a line every pass.
        tracing::warn!(unknown_window, "budget top-up: keys with no budget window");
    }
    if attempted > 0 {
        // PRIVACY: tallies only.
        tracing::debug!(attempted, applied, "budget top-up: pass finished");
    }
    applied
}

/// A top-up of `sent_usd` that landed in a later window than it was claimed
/// for. That window is recorded as topped up, so it gets no second one. If
/// the grant carried more than one month (the earlier window's stack), it is
/// brought down to one month, but only if the key still shows exactly that
/// grant in that window: anything else means a person has touched it since,
/// and it is theirs.
async fn land_late(
    state: &ControlState,
    bifrost: &crate::bifrost::BifrostClient,
    c: &crate::store::TopupCandidate,
    cycle: &str,
    sent_usd: f64,
    now: DateTime<Utc>,
) {
    let label = c.label.as_str();
    // Recorded FIRST: whatever happens to the cap, this window has had a
    // top-up and must not be given another.
    if let Err(e) = state
        .store()
        .record_auto_topup_applied(c.tenant_id, cycle, sent_usd, now)
        .await
    {
        tracing::error!(error = %e, %label, "budget top-up: late grant not recorded");
    }
    tracing::warn!(%label, vk_id = %c.vk_id, "budget top-up: applied in the next window");
    state.alert(crate::alert::llm_auto_topup_anchored_late(label));

    let reread = bifrost.list_virtual_keys().await.map(|keys| {
        keys.into_iter()
            .find(|k| k.id == c.vk_id)
            .and_then(|k| k.budgets.into_iter().next())
    });
    // Gone or unreadable: nothing to lower.
    let Ok(Some(b)) = reread else { return };
    let anchored = b.override_anchor_reset.and_then(|a| window_offset(&b, a));
    // Another amount, or granted in another window: a person has touched
    // it since, and it is theirs. One month or less: nothing to take off.
    let base = b.max_limit;
    if (b.override_amount - sent_usd).abs() > 0.005
        || anchored.is_none()
        || anchored != stamp_offset(&b, cycle)
        || sent_usd <= base + 0.005
    {
        return;
    }
    if let Err(e) = bifrost.top_up_key(&c.vk_id, round_up_cents(base)).await {
        tracing::error!(error = %e, %label, vk_id = %c.vk_id, "budget top-up: late grant not capped to one month");
        state.alert(crate::alert::llm_auto_topup_cap_failed(label));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    const WINDOW: &str = "2026-11-02T09:15:00.4Z";

    /// A key with a 75 limit, in the window opened at [`WINDOW`], that has
    /// spent `usage` and has no override.
    fn budget(usage: f64) -> BudgetState {
        BudgetState {
            id: "b-1".into(),
            max_limit: 75.0,
            current_usage: usage,
            override_amount: 0.0,
            last_reset: Some(at(WINDOW)),
            override_mode: String::new(),
            override_cycles_remaining: 0,
            override_anchor_reset: None,
            reset_duration: "1M".into(),
        }
    }

    /// The same, with the signup allowance (one month) live in this window.
    fn with_signup_allowance(usage: f64) -> BudgetState {
        BudgetState {
            override_amount: 75.0,
            override_mode: "cycles".into(),
            override_cycles_remaining: 1,
            override_anchor_reset: Some(at(WINDOW)),
            ..budget(usage)
        }
    }

    fn top_up(target_usd: f64) -> Decision {
        Decision::TopUp {
            cycle: cycle_key(at(WINDOW)),
            target_usd,
        }
    }

    /// Ten days into [`WINDOW`]'s 30-day window.
    fn mid_window() -> DateTime<Utc> {
        at(WINDOW) + chrono::Duration::days(10)
    }

    fn d(active: bool, b: &BudgetState, last: Option<&str>) -> Decision {
        let history = History {
            last_applied: last,
            pending: None,
        };
        decide(active, b, history, mid_window())
    }

    /// AT A BOUNDARY the listing can still name the old window after the
    /// gateway has granted an override in the new one. Claiming the old window
    /// then would let the new one claim again, so the key waits a pass.
    #[test]
    fn an_override_anchored_after_last_reset_waits_a_pass() {
        let b = BudgetState {
            override_anchor_reset: Some(at(WINDOW) + chrono::Duration::days(30)),
            ..with_signup_allowance(200.0)
        };
        assert_eq!(d(true, &b, None), Decision::Skip(Skip::WindowInFlux));
        // Sub-second noise on the same boundary is not a later window.
        let b = BudgetState {
            override_anchor_reset: Some(at(WINDOW) + chrono::Duration::milliseconds(300)),
            ..with_signup_allowance(150.0)
        };
        assert_eq!(d(true, &b, None), top_up(150.0));
    }

    /// The same boundary from the other side: `last_reset` plus one window is
    /// already past, so the reading is the old window's and is not acted on.
    #[test]
    fn a_window_that_should_have_closed_waits_a_pass() {
        let b = budget(80.0);
        let closes = at(WINDOW) + chrono::Duration::days(30);
        for now in [
            closes + chrono::Duration::seconds(5),
            // Inside the margin: a write sent now could land after the reset.
            closes - chrono::Duration::minutes(4),
        ] {
            assert_eq!(
                decide(true, &b, History::default(), now),
                Decision::Skip(Skip::WindowInFlux),
                "{now}"
            );
            assert!(window_closing(&b, now));
        }
        let before_margin = closes - chrono::Duration::minutes(6);
        assert_eq!(
            decide(true, &b, History::default(), before_margin),
            top_up(75.0)
        );
        assert!(!window_closing(&b, before_margin));
        // A length the gateway's own parser would not read is no window.
        let b = BudgetState {
            reset_duration: "monthly".into(),
            ..budget(80.0)
        };
        assert_eq!(d(true, &b, None), Decision::Skip(Skip::UnknownWindow));
    }

    #[test]
    fn window_lengths_parse_as_the_gateway_counts_them() {
        assert_eq!(reset_duration("1M"), Some(chrono::Duration::days(30)));
        assert_eq!(reset_duration("2w"), Some(chrono::Duration::days(14)));
        assert_eq!(reset_duration("1Y"), Some(chrono::Duration::days(365)));
        assert_eq!(reset_duration("30s"), Some(chrono::Duration::seconds(30)));
        for bad in ["", "M", "0M", "-1M", "1x", "1.5M"] {
            assert_eq!(reset_duration(bad), None, "{bad}");
        }
    }

    /// A retry carries the first attempt's amount. If an operator topped the
    /// key up by more since, the retry writes nothing and the larger figure
    /// stands; it never lowers a live override.
    #[test]
    fn a_retry_never_lowers_a_bigger_live_override() {
        assert_eq!(send_for(150.0, 300.0), Send::AlreadyCovered);
        assert_eq!(send_for(150.0, 150.0), Send::AlreadyCovered);
        assert_eq!(send_for(150.0, 75.0), Send::Write(150.0));
        assert_eq!(send_for(75.0, 0.0), Send::Write(75.0));
    }

    #[test]
    fn below_the_threshold_is_left_alone() {
        assert_eq!(
            d(true, &budget(0.0), None),
            Decision::Skip(Skip::BelowThreshold)
        );
        assert_eq!(
            d(true, &budget(71.24), None),
            Decision::Skip(Skip::BelowThreshold)
        );
    }

    #[test]
    fn at_the_threshold_and_not_topped_up_gets_one_more_month() {
        assert_eq!(d(true, &budget(71.25), None), top_up(75.0));
        // Over the limit already (the gateway lets the last call through) is
        // the same answer, not a bigger one.
        assert_eq!(d(true, &budget(90.0), None), top_up(75.0));
    }

    #[test]
    fn a_window_already_topped_up_is_left_alone() {
        let this = cycle_key(at(WINDOW));
        assert_eq!(
            d(true, &budget(80.0), Some(&this)),
            Decision::Skip(Skip::AlreadyTopped)
        );
    }

    #[test]
    fn a_new_window_is_eligible_again() {
        let last = cycle_key(at("2026-10-03T09:15:00.4Z"));
        assert_eq!(d(true, &budget(80.0), Some(&last)), top_up(75.0));
    }

    #[test]
    fn a_torn_down_tenant_is_left_alone() {
        assert_eq!(
            d(false, &budget(80.0), None),
            Decision::Skip(Skip::Inactive)
        );
    }

    /// The first window: the signup allowance is kept and the top-up goes on
    /// top of it, and the threshold is measured against both.
    #[test]
    fn the_signup_allowance_is_kept_and_stacked_on() {
        assert_eq!(
            d(true, &with_signup_allowance(140.0), None),
            Decision::Skip(Skip::BelowThreshold)
        );
        assert_eq!(d(true, &with_signup_allowance(142.5), None), top_up(150.0));
    }

    /// An override from a window that has closed counts for nothing, so it is
    /// neither added to the threshold nor carried into the new target.
    #[test]
    fn an_override_with_no_cycles_left_counts_for_nothing() {
        let b = BudgetState {
            override_cycles_remaining: 0,
            ..with_signup_allowance(72.0)
        };
        assert_eq!(d(true, &b, None), top_up(75.0));
    }

    /// An override covering more than this window was granted by hand, and a
    /// one-cycle write from here would replace it and cut it short. The key
    /// is the operator's: never topped up, whatever it has spent.
    #[test]
    fn a_multi_cycle_override_is_the_operators() {
        for usage in [0.0, 357.0, 1000.0] {
            let b = BudgetState {
                override_amount: 300.0,
                override_cycles_remaining: 2,
                override_anchor_reset: Some(at("2026-10-03T09:15:00.4Z")),
                ..with_signup_allowance(usage)
            };
            assert_eq!(
                d(true, &b, None),
                Decision::Skip(Skip::OperatorOverride),
                "{usage}"
            );
        }
    }

    /// An anchor and `last_reset` are compared by WINDOW. A `last_reset` that
    /// is when a reset ran, a few seconds after the boundary the override was
    /// anchored on, is still the same window: the override is live and not a
    /// sign of a later window.
    #[test]
    fn an_anchor_a_little_off_last_reset_is_the_same_window() {
        for skew in [-120, -3, 3, 120] {
            let b = BudgetState {
                override_anchor_reset: Some(at(WINDOW) + chrono::Duration::seconds(skew)),
                ..with_signup_allowance(142.5)
            };
            assert_eq!(window_offset(&b, b.override_anchor_reset.unwrap()), Some(0));
            assert_eq!(d(true, &b, None), top_up(150.0), "{skew}");
        }
        let b = with_signup_allowance(0.0);
        let next = at(WINDOW) + chrono::Duration::days(30) - chrono::Duration::seconds(3);
        assert_eq!(window_offset(&b, next), Some(1));
    }

    /// AN UNCONFIRMED WRITE THAT CROSSED THE RESET. The last window's claim is
    /// still pending, and this window carries exactly what it sent, granted
    /// in this window: it is this window's top-up. Recorded as such, and the
    /// last window's stack (base + allowance) is brought down to one month.
    #[test]
    fn a_pending_claim_that_landed_in_this_window_is_its_top_up() {
        let last_window = cycle_key(at("2026-10-03T09:15:00.4Z"));
        let landed = BudgetState {
            override_amount: 150.0,
            override_anchor_reset: Some(at(WINDOW) - chrono::Duration::seconds(2)),
            ..with_signup_allowance(10.0)
        };
        let history = History {
            last_applied: None,
            pending: Some((&last_window, 150.0)),
        };
        assert_eq!(
            decide(true, &landed, history, mid_window()),
            Decision::LandedLate {
                cycle: cycle_key(at(WINDOW)),
                sent_usd: 150.0,
            }
        );
        // One month landed: still this window's top-up.
        let one_month = BudgetState {
            override_amount: 75.0,
            ..landed.clone()
        };
        let history = History {
            last_applied: None,
            pending: Some((&last_window, 75.0)),
        };
        assert_eq!(
            decide(true, &one_month, history, mid_window()),
            Decision::LandedLate {
                cycle: cycle_key(at(WINDOW)),
                sent_usd: 75.0,
            }
        );
        // A different amount is somebody else's override, and a pending claim
        // for THIS window is just a retry: neither is a late landing.
        let history = History {
            last_applied: None,
            pending: Some((&last_window, 90.0)),
        };
        assert_eq!(
            decide(true, &landed, history, mid_window()),
            Decision::Skip(Skip::BelowThreshold)
        );
        let this_window = cycle_key(at(WINDOW));
        let history = History {
            last_applied: None,
            pending: Some((&this_window, 150.0)),
        };
        assert_eq!(
            decide(true, &landed, history, mid_window()),
            Decision::Skip(Skip::BelowThreshold)
        );
    }

    /// A STALE claim, from two or more windows back, cannot have crossed into
    /// this one. If an operator's top-up happens to match its amount, that is
    /// still the operator's, not a late landing to record or cap.
    #[test]
    fn a_claim_from_two_windows_back_is_not_a_late_landing() {
        let operator = BudgetState {
            override_amount: 150.0,
            override_anchor_reset: Some(at(WINDOW)),
            ..with_signup_allowance(10.0)
        };
        for back in ["2026-09-03T09:15:00.4Z", "2026-05-03T09:15:00.4Z"] {
            let stale = cycle_key(at(back));
            assert!(stamp_offset(&operator, &stale).unwrap() <= -2, "{back}");
            let history = History {
                last_applied: None,
                pending: Some((&stale, 150.0)),
            };
            assert_eq!(
                decide(true, &operator, history, mid_window()),
                Decision::Skip(Skip::BelowThreshold),
                "{back}"
            );
        }
    }

    /// A window recorded from a late grant's anchor may be a few seconds off
    /// the `last_reset` the listing later shows. It still counts.
    #[test]
    fn a_window_recorded_a_little_off_still_counts() {
        let anchor = cycle_key(at(WINDOW) - chrono::Duration::seconds(2));
        assert_eq!(
            d(true, &budget(80.0), Some(&anchor)),
            Decision::Skip(Skip::AlreadyTopped)
        );
    }

    #[test]
    fn a_hand_set_permanent_override_is_the_operators() {
        let b = BudgetState {
            override_mode: "forever".into(),
            override_amount: 10.0,
            ..budget(84.0)
        };
        assert_eq!(d(true, &b, None), Decision::Skip(Skip::OperatorOverride));
    }

    #[test]
    fn no_window_or_no_limit_is_left_alone() {
        let b = BudgetState {
            last_reset: None,
            ..budget(80.0)
        };
        assert_eq!(d(true, &b, None), Decision::Skip(Skip::UnknownWindow));
        let b = BudgetState {
            max_limit: 0.0,
            ..budget(80.0)
        };
        assert_eq!(d(true, &b, None), Decision::Skip(Skip::NoLimit));
    }

    /// The window name survives the gateway's sub-second noise and sorts by
    /// time as a string, which the store's `max()` relies on.
    #[test]
    fn the_window_name_is_stable_and_ordered() {
        assert_eq!(
            cycle_key(at("2026-11-02T09:15:00.000000001Z")),
            cycle_key(at("2026-11-02T09:14:59.999999999Z"))
        );
        assert_eq!(cycle_key(at(WINDOW)), "2026-11-02T09:15:00.000Z");
        assert!(cycle_key(at("2026-10-03T09:15:00Z")) < cycle_key(at(WINDOW)));
        assert!(cycle_key(at("2026-12-02T09:15:00Z")) > cycle_key(at(WINDOW)));
    }
}
