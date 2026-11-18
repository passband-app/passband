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

/// Slack for comparing two window boundaries: they cross JSON with
/// sub-microsecond noise, and a real window is a month.
const BOUNDARY_SLACK: chrono::Duration = chrono::Duration::seconds(1);

/// What the poller should do about one tenant's key.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Skip(Skip),
    /// Claim `cycle` and set the key's override to `target_usd`.
    TopUp {
        cycle: String,
        target_usd: f64,
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
    /// later window than `last_reset` names, or that window should already
    /// have closed. The listing lags the gateway's own reset by a little, and
    /// claiming the old window then would let the new one claim again. The
    /// next pass reads it settled.
    WindowInFlux,
    /// A hand-set permanent override. The operator owns this key's limit.
    ForeverOverride,
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

/// The override that is live in `budget`'s current window, in dollars.
///
/// An override anchored before the window opened belongs to a window that has
/// closed and counts for nothing, even if the listing still shows it.
pub fn live_override(budget: &BudgetState) -> f64 {
    if !(budget.override_amount.is_finite() && budget.override_amount > 0.0) {
        return 0.0;
    }
    match (budget.override_anchor_reset, budget.last_reset) {
        (Some(anchor), Some(opened)) if anchor < opened - BOUNDARY_SLACK => 0.0,
        _ => budget.override_amount,
    }
}

/// Decide what to do about one tenant's triage budget at `now`.
///
/// `last_applied_cycle` is the newest window this tenant was automatically
/// topped up in (from the store). The decision is pure; the store's claim is
/// what makes "once per window" hold under races.
pub fn decide(
    active: bool,
    budget: &BudgetState,
    last_applied_cycle: Option<&str>,
    now: DateTime<Utc>,
) -> Decision {
    if !active {
        return Decision::Skip(Skip::Inactive);
    }
    let (Some(opened), Some(length)) = (budget.last_reset, reset_duration(&budget.reset_duration))
    else {
        return Decision::Skip(Skip::UnknownWindow);
    };
    if budget
        .override_anchor_reset
        .is_some_and(|anchor| anchor > opened + BOUNDARY_SLACK)
        || opened + length <= now
    {
        return Decision::Skip(Skip::WindowInFlux);
    }
    if budget.override_mode == "forever" {
        return Decision::Skip(Skip::ForeverOverride);
    }
    let base = budget.max_limit;
    if !(base.is_finite() && base > 0.0) {
        return Decision::Skip(Skip::NoLimit);
    }
    let cycle = cycle_key(opened);
    // `>=`, not `==`: a stored window newer than the gateway's would mean the
    // gateway's clock went back, and the safe reading of that is "done".
    if last_applied_cycle.is_some_and(|last| last >= cycle.as_str()) {
        return Decision::Skip(Skip::AlreadyTopped);
    }
    let existing = live_override(budget);
    let limit = base + existing;
    if !(budget.current_usage.is_finite() && budget.current_usage >= TOPUP_THRESHOLD * limit) {
        return Decision::Skip(Skip::BelowThreshold);
    }
    // Rounded up to the cent, as `llm topup` rounds, so the read-back compares
    // against what the gateway stores.
    let target_usd = ((existing + base) * 100.0).ceil() / 100.0;
    Decision::TopUp { cycle, target_usd }
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
        let (cycle, target) = match decide(c.active, budget, c.last_applied_cycle.as_deref(), now) {
            Decision::TopUp { cycle, target_usd } => (cycle, target_usd),
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
                tracing::error!(error = %e, label = %c.label, "budget top-up: claim failed");
                continue;
            }
        };
        attempted += 1;
        let label = c.label.as_str();
        let mark_applied = || async {
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
                mark_applied().await;
                tracing::info!(%label, vk_id = %c.vk_id, "budget top-up: already covered, nothing written");
                continue;
            }
        };
        match bifrost.top_up_key(&c.vk_id, amount).await {
            Ok(()) => {
                mark_applied().await;
                applied += 1;
                tracing::info!(%label, vk_id = %c.vk_id, "budget top-up: auto top-up applied");
                state.alert(crate::alert::llm_auto_topup_applied(label));
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
                match state
                    .store()
                    .take_topup_failure_alert(c.tenant_id, &cycle, now, quiet_before)
                    .await
                {
                    Ok(true) => state.alert(if unconfirmed {
                        crate::alert::llm_auto_topup_unconfirmed(label, attempt)
                    } else {
                        crate::alert::llm_auto_topup_failed(label, &e.to_string(), attempt)
                    }),
                    Ok(false) => {}
                    // Better a repeat than silence while a tenant goes dark.
                    Err(_) => state.alert(crate::alert::llm_auto_topup_failed(
                        label,
                        &e.to_string(),
                        attempt,
                    )),
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
            override_anchor_reset: None,
            reset_duration: "1M".into(),
        }
    }

    /// The same, with the signup allowance (one month) live in this window.
    fn with_signup_allowance(usage: f64) -> BudgetState {
        BudgetState {
            override_amount: 75.0,
            override_mode: "cycles".into(),
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
        decide(active, b, last, mid_window())
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
        let closed = at(WINDOW) + chrono::Duration::days(30) + chrono::Duration::seconds(5);
        assert_eq!(
            decide(true, &b, None, closed),
            Decision::Skip(Skip::WindowInFlux)
        );
        let just_before = at(WINDOW) + chrono::Duration::days(30) - chrono::Duration::seconds(5);
        assert_eq!(decide(true, &b, None, just_before), top_up(75.0));
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
    fn an_override_from_a_closed_window_counts_for_nothing() {
        let b = BudgetState {
            override_anchor_reset: Some(at("2026-10-03T09:15:00Z")),
            ..with_signup_allowance(72.0)
        };
        assert_eq!(d(true, &b, None), top_up(75.0));
    }

    #[test]
    fn a_hand_set_permanent_override_is_the_operators() {
        let b = BudgetState {
            override_mode: "forever".into(),
            override_amount: 10.0,
            ..budget(84.0)
        };
        assert_eq!(d(true, &b, None), Decision::Skip(Skip::ForeverOverride));
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
