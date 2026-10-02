//! Per-field agreement between a candidate decision and gold. Programmatic,
//! no LLM judge: every metric is a number in [0, 1] (1 = agrees) or absent
//! when it does not apply, and a failed run scores 0 on every metric that
//! applies.
//!
//! Harm is one weighted cost per case, for sorting and for a single summary
//! number. The constants:
//!
//! | event                                   | harm |
//! |-----------------------------------------|------|
//! | FYE miss, gold state `needs_user`       | 10   |
//! | FYE miss, any other gold state          | 5    |
//! | FYE false alarm                         | 2    |
//! | restricted miss (credential left open)  | 10   |
//! | false restriction                       | 1    |
//! | auth miss                               | 5    |
//! | push miss                               | 3    |
//! | false push                              | 2    |
//! | Reading mismatch                        | 1    |
//! | record kinds mismatch                   | 2    |
//! | record field disagreement               | up to 1 (1 - `record_fields`) |
//! | kinds                                   | up to 0.5 (0.5 x (1 - Jaccard)) |
//! | model failure (replaces all of the above) | 10 |
use crate::case::{Case, is_push};
use crate::run::ResultRow;
use serde_json::{Value, json};
use squelch_core::triage::agent_config::RankingConfig;
use squelch_core::triage::decision::{
    AttentionState, MessageDecision, MessageDestination, RecordProposal,
};
use std::collections::BTreeMap;

/// Bumped whenever a metric's definition changes; stored on each row and
/// printed by `report`, which always re-scores from the raw decision.
pub const SCORER_VERSION: u32 = 1;

/// Agreement metrics, headline first.
pub const METRICS: [&str; 14] = [
    "fye",
    "reading",
    "auth",
    "auth_kinds",
    "restricted",
    "push",
    "kinds",
    "kinds_exact",
    "state",
    "actions",
    "records",
    "record_fields",
    "reading_kind",
    "revisit",
];

/// Fields with a positive class, the two costliest misses first.
pub const DIRECTIONAL: [&str; 5] = ["fye", "restricted", "auth", "push", "reading"];

/// Fields an owner may adjudicate (`kinds` covers `kinds_exact` too).
pub const ADJUDICABLE: [&str; 12] = [
    "fye",
    "reading",
    "auth",
    "auth_kinds",
    "restricted",
    "push",
    "kinds",
    "state",
    "actions",
    "records",
    "reading_kind",
    "revisit",
];

pub const HARM_FYE_MISS_NEEDS_USER: f64 = 10.0;
pub const HARM_FYE_MISS: f64 = 5.0;
pub const HARM_FYE_FALSE_ALARM: f64 = 2.0;
pub const HARM_RESTRICTED_MISS: f64 = 10.0;
pub const HARM_FALSE_RESTRICTION: f64 = 1.0;
pub const HARM_AUTH_MISS: f64 = 5.0;
pub const HARM_PUSH_MISS: f64 = 3.0;
pub const HARM_FALSE_PUSH: f64 = 2.0;
pub const HARM_READING_MISMATCH: f64 = 1.0;
pub const HARM_RECORD_KINDS: f64 = 2.0;
pub const HARM_RECORD_FIELDS: f64 = 1.0;
pub const HARM_KINDS: f64 = 0.5;
pub const HARM_MODEL_FAILURE: f64 = 10.0;

/// The push threshold the scorer uses (production's default).
pub fn min_importance() -> u8 {
    crate::export::default_min_importance()
}

/// For one case: the candidate values an owner has ruled acceptable, per
/// field. Empty for a case nobody adjudicated.
pub type Accepted = BTreeMap<String, Vec<Value>>;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Score {
    pub metrics: BTreeMap<String, f64>,
    /// Lower is better; only when both show in FYE.
    pub rank_delta: Option<f64>,
    pub harm: f64,
    /// Per directional field: (effective gold, candidate). A failed run
    /// predicts negative.
    pub directional: BTreeMap<String, (bool, bool)>,
}

fn name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn sorted_names<T: serde::Serialize>(items: impl IntoIterator<Item = T>) -> Vec<String> {
    let mut names: Vec<String> = items.into_iter().map(|item| name(&item)).collect();
    names.sort();
    names
}

pub fn record_kind(record: &RecordProposal) -> &'static str {
    match record {
        RecordProposal::Bill { .. } => "bill",
        RecordProposal::Receipt { .. } => "receipt",
        RecordProposal::Delivery { .. } => "delivery",
        RecordProposal::Event { .. } => "event",
        RecordProposal::FinancialUpdate { .. } => "financial_update",
    }
}

fn is_reading(decision: &MessageDecision) -> bool {
    decision.destinations.contains(&MessageDestination::Reading)
}

fn unresolved_actions(decision: &MessageDecision) -> Vec<String> {
    sorted_names(
        decision
            .attention
            .actions
            .iter()
            .filter(|action| !action.resolved)
            .map(|action| action.kind),
    )
}

/// The value of an adjudicable field, as recorded in a ruling.
pub fn field_value(field: &str, decision: &MessageDecision) -> Option<Value> {
    Some(match field {
        "fye" => json!(decision.attention.show_in_fye),
        "reading" => json!(is_reading(decision)),
        "auth" => json!(decision.auth.is_auth()),
        "restricted" => json!(decision.external_access.restricted),
        "push" => json!(is_push(decision, min_importance())),
        "revisit" => json!(decision.revisit.is_some()),
        "auth_kinds" => json!(sorted_names(decision.auth.kinds.iter().copied())),
        "kinds" | "kinds_exact" => json!(sorted_names(decision.kinds.iter().copied())),
        "state" => json!(name(&decision.attention.state)),
        "actions" => json!(unresolved_actions(decision)),
        "records" => {
            let mut kinds: Vec<&str> = decision.records.iter().map(record_kind).collect();
            kinds.sort_unstable();
            json!(kinds)
        }
        "reading_kind" => json!(decision.reading_kind.as_ref().map(name)),
        _ => return None,
    })
}

fn amount_agrees(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() < 0.005,
        (None, None) => true,
        _ => false,
    }
}

fn normalize(value: &Option<String>) -> Option<String> {
    value.as_ref().map(|text| {
        text.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_lowercase()
    })
}

/// (agreeing key facts, gold key facts) for one gold record and its paired
/// candidate record, if any.
fn record_facts(gold: &RecordProposal, candidate: Option<&RecordProposal>) -> (usize, usize) {
    use RecordProposal::*;
    let time = |t: &Option<squelch_core::triage::decision::SupportedTime>| {
        t.as_ref().map(|t| t.value.clone())
    };
    let checks: Vec<bool> = match (gold, candidate) {
        (
            Bill { amount, due, .. },
            Some(Bill {
                amount: a, due: d, ..
            }),
        ) => {
            vec![amount_agrees(*amount, *a), time(due) == time(d)]
        }
        (Bill { .. }, _) => vec![false, false],
        (Receipt { amount, .. }, Some(Receipt { amount: a, .. })) => {
            vec![amount_agrees(*amount, *a)]
        }
        (Receipt { .. }, _) => vec![false],
        (
            Delivery {
                tracking_number,
                status,
                ..
            },
            Some(Delivery {
                tracking_number: t,
                status: s,
                ..
            }),
        ) => vec![normalize(tracking_number) == normalize(t), status == s],
        (Delivery { .. }, _) => vec![false, false],
        (Event { start, .. }, Some(Event { start: s, .. })) => vec![time(start) == time(s)],
        (Event { .. }, _) => vec![false],
        (FinancialUpdate { institution, .. }, Some(FinancialUpdate { institution: i, .. })) => {
            vec![institution.trim().to_lowercase() == i.trim().to_lowercase()]
        }
        (FinancialUpdate { .. }, _) => vec![false],
    };
    (checks.iter().filter(|ok| **ok).count(), checks.len())
}

/// Share of the gold records' key facts the candidate states the same way,
/// pairing records by kind in order.
fn record_fields(gold: &MessageDecision, candidate: &MessageDecision) -> f64 {
    let mut used: BTreeMap<&str, usize> = BTreeMap::new();
    let (mut agree, mut total) = (0, 0);
    for record in &gold.records {
        let kind = record_kind(record);
        let index = used.entry(kind).or_insert(0);
        let paired = candidate
            .records
            .iter()
            .filter(|r| record_kind(r) == kind)
            .nth(*index);
        *index += 1;
        let (a, t) = record_facts(record, paired);
        agree += a;
        total += t;
    }
    if total == 0 {
        1.0
    } else {
        agree as f64 / total as f64
    }
}

fn jaccard(a: &[String], b: &[String]) -> f64 {
    let a: std::collections::BTreeSet<_> = a.iter().collect();
    let b: std::collections::BTreeSet<_> = b.iter().collect();
    let union = a.union(&b).count();
    if union == 0 {
        1.0
    } else {
        a.intersection(&b).count() as f64 / union as f64
    }
}

/// `30*urgency + 25*action_need + 20*personal_relevance + 2*importance` at
/// the default ranking weights: the part of the FYE rank the model sets.
pub fn static_rank(decision: &MessageDecision) -> f64 {
    let weights = RankingConfig::default();
    let f = &decision.attention.factors;
    weights.urgency_weight * f.urgency
        + weights.action_need_weight * f.action_need
        + weights.personal_relevance_weight * f.personal_relevance
        + weights.importance_weight * f.importance
}

/// Score a candidate (None = the run failed) against gold.
pub fn score(
    gold: &MessageDecision,
    candidate: Option<&MessageDecision>,
    accepted: &Accepted,
) -> Score {
    let mut out = Score::default();
    let gold_actions = unresolved_actions(gold);
    let gold_reading_kind = is_reading(gold) && gold.reading_kind.is_some();
    let Some(c) = candidate else {
        for metric in METRICS {
            let applies = match metric {
                "auth_kinds" => gold.auth.is_auth(),
                "actions" => !gold_actions.is_empty(),
                "records" | "record_fields" => !gold.records.is_empty(),
                "reading_kind" => gold_reading_kind,
                _ => true,
            };
            if applies {
                out.metrics.insert(metric.into(), 0.0);
            }
        }
        for field in DIRECTIONAL {
            let gold_value = field_value(field, gold)
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            out.directional.insert(field.into(), (gold_value, false));
        }
        out.harm = HARM_MODEL_FAILURE;
        return out;
    };
    let accepts = |field: &str| {
        field_value(field, c).is_some_and(|value| {
            accepted
                .get(field)
                .is_some_and(|values| values.contains(&value))
        })
    };
    // Boolean fields: an accepted candidate value IS the gold for this case.
    let mut flag = |field: &str| -> (bool, bool) {
        let candidate_value = field_value(field, c)
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let gold_value = if accepts(field) {
            candidate_value
        } else {
            field_value(field, gold)
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        };
        out.metrics.insert(
            field.into(),
            f64::from(u8::from(gold_value == candidate_value)),
        );
        (gold_value, candidate_value)
    };
    let fye = flag("fye");
    let reading = flag("reading");
    let auth = flag("auth");
    let restricted = flag("restricted");
    let push = flag("push");
    flag("revisit");
    for (field, pair) in [
        ("fye", fye),
        ("reading", reading),
        ("auth", auth),
        ("restricted", restricted),
        ("push", push),
    ] {
        out.directional.insert(field.into(), pair);
    }
    let same = |field: &str| accepts(field) || field_value(field, c) == field_value(field, gold);
    if gold.auth.is_auth() {
        out.metrics
            .insert("auth_kinds".into(), f64::from(u8::from(same("auth_kinds"))));
    }
    let gold_kinds = sorted_names(gold.kinds.iter().copied());
    let candidate_kinds = sorted_names(c.kinds.iter().copied());
    let kinds = if accepts("kinds") {
        1.0
    } else {
        jaccard(&gold_kinds, &candidate_kinds)
    };
    out.metrics.insert("kinds".into(), kinds);
    out.metrics
        .insert("kinds_exact".into(), f64::from(u8::from(same("kinds"))));
    out.metrics
        .insert("state".into(), f64::from(u8::from(same("state"))));
    if !gold_actions.is_empty() || !unresolved_actions(c).is_empty() {
        out.metrics
            .insert("actions".into(), f64::from(u8::from(same("actions"))));
    }
    let mut records_harm = 0.0;
    if !gold.records.is_empty() {
        let kinds_agree = same("records");
        out.metrics
            .insert("records".into(), f64::from(u8::from(kinds_agree)));
        let fields = record_fields(gold, c);
        out.metrics.insert("record_fields".into(), fields);
        if !kinds_agree {
            records_harm += HARM_RECORD_KINDS;
        }
        records_harm += HARM_RECORD_FIELDS * (1.0 - fields);
    }
    if gold_reading_kind && is_reading(c) {
        out.metrics.insert(
            "reading_kind".into(),
            f64::from(u8::from(same("reading_kind"))),
        );
    }
    if gold.attention.show_in_fye && c.attention.show_in_fye {
        out.rank_delta = Some((static_rank(c) - static_rank(gold)).abs());
    }
    let mut harm = records_harm + HARM_KINDS * (1.0 - kinds);
    harm += match fye {
        (true, false) if gold.attention.state == AttentionState::NeedsUser => {
            HARM_FYE_MISS_NEEDS_USER
        }
        (true, false) => HARM_FYE_MISS,
        (false, true) => HARM_FYE_FALSE_ALARM,
        _ => 0.0,
    };
    harm += match restricted {
        (true, false) => HARM_RESTRICTED_MISS,
        (false, true) => HARM_FALSE_RESTRICTION,
        _ => 0.0,
    };
    if auth == (true, false) {
        harm += HARM_AUTH_MISS;
    }
    harm += match push {
        (true, false) => HARM_PUSH_MISS,
        (false, true) => HARM_FALSE_PUSH,
        _ => 0.0,
    };
    if reading.0 != reading.1 {
        harm += HARM_READING_MISMATCH;
    }
    out.harm = harm;
    out
}

/// The `grade` map written onto each result row: the metrics, plus `harm`
/// and `rank_delta` when present. A convenience for other tools; `report`
/// never reads it back.
pub fn grade(case: &Case, row: &ResultRow) -> BTreeMap<String, f64> {
    let score = score(&case.gold, row.decision.as_ref(), &Accepted::new());
    let mut grade = score.metrics;
    grade.insert("harm".into(), score.harm);
    if let Some(delta) = score.rank_delta {
        grade.insert("rank_delta".into(), delta);
    }
    grade
}

/// Weighted mean of `(weight, value)`.
pub fn weighted_mean(values: &[(f64, f64)]) -> Option<f64> {
    let total: f64 = values.iter().map(|(w, _)| w).sum();
    (total > 0.0).then(|| values.iter().map(|(w, x)| w * x).sum::<f64>() / total)
}

pub const BOOTSTRAP_RESAMPLES: usize = 2000;

/// 95% percentile bootstrap CI of the weighted mean, resampling cases.
pub fn bootstrap_ci(values: &[(f64, f64)], resamples: usize, seed: u64) -> Option<(f64, f64)> {
    if values.is_empty() {
        return None;
    }
    let mut rng = crate::SplitMix64::new(seed);
    let mut means = Vec::with_capacity(resamples);
    let mut draw = Vec::with_capacity(values.len());
    for _ in 0..resamples {
        draw.clear();
        for _ in 0..values.len() {
            draw.push(values[rng.below(values.len() as u64) as usize]);
        }
        if let Some(mean) = weighted_mean(&draw) {
            means.push(mean);
        }
    }
    means.sort_by(f64::total_cmp);
    let at = |p: f64| means[((p * (means.len() - 1) as f64).round() as usize).min(means.len() - 1)];
    Some((at(0.025), at(0.975)))
}

/// A weighted mean with its CI and sample size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Estimate {
    pub mean: f64,
    pub low: f64,
    pub high: f64,
    pub n: usize,
}

impl Estimate {
    pub fn of(values: &[(f64, f64)], seed: u64) -> Option<Self> {
        let mean = weighted_mean(values)?;
        let (low, high) = bootstrap_ci(values, BOOTSTRAP_RESAMPLES, seed)?;
        Some(Self {
            mean,
            low,
            high,
            n: values.len(),
        })
    }
    /// The CI excludes zero: a real difference at 95%.
    pub fn excludes_zero(&self) -> bool {
        self.low > 0.0 || self.high < 0.0
    }
}

/// Paired difference `run - baseline` over keys both have, weighted by the
/// run's case weight, with a paired bootstrap CI.
pub fn paired_delta(
    run: &BTreeMap<i64, (f64, f64)>,
    baseline: &BTreeMap<i64, (f64, f64)>,
    seed: u64,
) -> Option<Estimate> {
    let pairs: Vec<(f64, f64)> = run
        .iter()
        .filter_map(|(key, (weight, value))| {
            baseline.get(key).map(|(_, base)| (*weight, value - base))
        })
        .collect();
    Estimate::of(&pairs, seed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use squelch_core::triage::decision::*;

    fn evidence() -> Vec<EvidenceRef> {
        vec![EvidenceRef {
            message_id: 1,
            location: "body".into(),
        }]
    }

    /// Gold decisions that exercise every metric.
    pub(crate) fn golds() -> Vec<MessageDecision> {
        let time = |value: &str| {
            Some(SupportedTime {
                value: value.into(),
                timezone: None,
                source_message_id: 1,
                interpreted_relative: false,
            })
        };
        let mut bill = MessageDecision {
            kinds: vec![EmailKind::Bill, EmailKind::AccountService],
            summary: "Electric bill due".into(),
            reason: "A bill with a due date".into(),
            ..Default::default()
        };
        bill.attention.show_in_fye = true;
        bill.attention.state = AttentionState::NeedsUser;
        bill.attention.factors.urgency = 0.8;
        bill.attention.actions.push(AttentionAction {
            kind: ActionKind::Pay,
            description: "Pay it".into(),
            evidence: evidence(),
            ..Default::default()
        });
        bill.notification.importance = 70;
        bill.records = vec![
            RecordProposal::Bill {
                merchant: "Power".into(),
                amount: Some(81.2),
                currency: Some("USD".into()),
                due: time("2026-10-05"),
                autopay: None,
                evidence: evidence(),
            },
            RecordProposal::Delivery {
                carrier: None,
                tracking_number: Some("1Z 99".into()),
                status: "shipped".into(),
                item_name: None,
                merchant: None,
                order_refs: vec![],
                evidence: evidence(),
            },
            RecordProposal::Event {
                title: "Visit".into(),
                start: time("2026-10-09"),
                location: None,
                evidence: evidence(),
            },
            RecordProposal::FinancialUpdate {
                institution: "Bank".into(),
                description: "Deposit".into(),
                evidence: evidence(),
            },
            RecordProposal::Receipt {
                merchant: "Shop".into(),
                amount: Some(3.0),
                currency: None,
                evidence: evidence(),
            },
        ];
        bill.revisit = Some(RevisitRequest {
            at: chrono::Utc::now(),
            reason: "check".into(),
        });
        let mut otp = MessageDecision {
            kinds: vec![EmailKind::AuthenticationSecurity],
            summary: "Login code".into(),
            reason: "One-time code".into(),
            ..Default::default()
        };
        otp.auth.kinds = vec![AuthKind::Otp];
        otp.external_access.restricted = true;
        let mut newsletter = MessageDecision {
            kinds: vec![EmailKind::Editorial],
            destinations: vec![MessageDestination::Reading],
            reading_kind: Some(ReadingKind::Content),
            summary: "A newsletter".into(),
            reason: "Editorial".into(),
            ..Default::default()
        };
        newsletter.attention.state = AttentionState::Informational;
        vec![bill, otp, newsletter]
    }

    #[test]
    fn oracle_scores_one_everywhere_with_no_harm() {
        let mut seen = std::collections::BTreeSet::new();
        for gold in golds() {
            let score = score(&gold, Some(&gold), &Accepted::new());
            for (metric, value) in &score.metrics {
                assert_eq!(*value, 1.0, "{metric}");
                seen.insert(metric.clone());
            }
            assert_eq!(score.harm, 0.0);
            assert!(score.rank_delta.is_none_or(|d| d == 0.0));
            // Conditional metrics appear exactly when they apply.
            let has = |m: &str| score.metrics.contains_key(m);
            assert_eq!(has("auth_kinds"), gold.auth.is_auth());
            assert_eq!(has("records"), !gold.records.is_empty());
            assert_eq!(has("record_fields"), !gold.records.is_empty());
            assert_eq!(has("reading_kind"), gold.reading_kind.is_some());
            assert_eq!(has("actions"), !gold.attention.actions.is_empty());
        }
        assert_eq!(
            seen.len(),
            METRICS.len(),
            "every metric exercised: {seen:?}"
        );
    }

    #[test]
    fn null_candidates_score_below_oracle_on_positive_recall() {
        let golds = golds();
        let mut promo = MessageDecision {
            kinds: vec![EmailKind::Promotional],
            destinations: vec![MessageDestination::Reading],
            summary: "Promotion".into(),
            reason: "Marketing".into(),
            ..Default::default()
        };
        promo.reading_kind = Some(ReadingKind::Promotion);
        for null in [MessageDecision::default(), promo] {
            for field in ["fye", "auth", "restricted", "push"] {
                let (mut hits, mut positives) = (0, 0);
                for gold in &golds {
                    let s = score(gold, Some(&null), &Accepted::new());
                    let (g, c) = s.directional[field];
                    positives += usize::from(g);
                    hits += usize::from(g && c);
                }
                assert!(positives > 0, "{field}");
                assert_eq!(hits, 0, "{field}: a null candidate recalls nothing");
            }
            let harm: f64 = golds
                .iter()
                .map(|g| score(g, Some(&null), &Accepted::new()).harm)
                .sum();
            assert!(harm > 20.0, "harm {harm}");
        }
    }

    #[test]
    fn a_failed_run_is_zero_everywhere_and_harm_ten() {
        for gold in golds() {
            let s = score(&gold, None, &Accepted::new());
            assert!(!s.metrics.is_empty());
            assert!(s.metrics.values().all(|v| *v == 0.0));
            assert_eq!(s.harm, HARM_MODEL_FAILURE);
            assert!(s.directional.values().all(|(_, c)| !c));
            assert_eq!(
                s.metrics.contains_key("auth_kinds"),
                gold.auth.is_auth(),
                "applicability from gold"
            );
        }
    }

    #[test]
    fn record_fields_and_kinds_are_partial_credit() {
        let gold = &golds()[0];
        let mut close = gold.clone();
        if let RecordProposal::Bill { amount, .. } = &mut close.records[0] {
            *amount = Some(81.204);
        }
        if let RecordProposal::Delivery {
            tracking_number, ..
        } = &mut close.records[1]
        {
            *tracking_number = Some("1z99".into());
        }
        assert_eq!(
            score(gold, Some(&close), &Accepted::new()).metrics["record_fields"],
            1.0
        );
        let mut wrong = gold.clone();
        if let RecordProposal::Bill { due, .. } = &mut wrong.records[0] {
            *due = None;
        }
        wrong.kinds = vec![EmailKind::Bill];
        let s = score(gold, Some(&wrong), &Accepted::new());
        // 7 key facts in the gold records, one wrong.
        assert!((s.metrics["record_fields"] - 6.0 / 7.0).abs() < 1e-9);
        assert_eq!(s.metrics["kinds"], 0.5);
        assert_eq!(s.metrics["kinds_exact"], 0.0);
        assert!((s.harm - (1.0 / 7.0 + 0.25)).abs() < 1e-9);
    }

    #[test]
    fn harm_follows_the_table() {
        let gold = &golds()[0];
        let mut buried = gold.clone();
        buried.attention.show_in_fye = false;
        // needs_user FYE miss.
        assert_eq!(score(gold, Some(&buried), &Accepted::new()).harm, 10.0);
        let otp = &golds()[1];
        let mut open = otp.clone();
        open.external_access.restricted = false;
        assert_eq!(score(otp, Some(&open), &Accepted::new()).harm, 10.0);
        let mut noisy = golds()[2].clone();
        noisy.attention.show_in_fye = true;
        noisy.notification.importance = 90;
        // FYE false alarm + false push.
        assert_eq!(score(&golds()[2], Some(&noisy), &Accepted::new()).harm, 4.0);
        // Rank: 30 per unit of urgency at the default weights.
        let mut calmer = gold.clone();
        calmer.attention.factors.urgency = 0.3;
        let delta = score(gold, Some(&calmer), &Accepted::new())
            .rank_delta
            .unwrap();
        assert!((delta - 15.0).abs() < 1e-9);
    }

    #[test]
    fn adjudication_flips_exactly_the_ruled_field() {
        let gold = &golds()[2];
        let mut candidate = gold.clone();
        candidate.attention.show_in_fye = true;
        candidate.attention.state = AttentionState::NeedsUser;
        let before = score(gold, Some(&candidate), &Accepted::new());
        assert_eq!(before.metrics["fye"], 0.0);
        assert_eq!(before.metrics["state"], 0.0);
        let ruled = Accepted::from([("fye".to_owned(), vec![json!(true)])]);
        let after = score(gold, Some(&candidate), &ruled);
        assert_eq!(after.metrics["fye"], 1.0);
        assert_eq!(after.directional["fye"], (true, true));
        assert_eq!(after.harm, before.harm - HARM_FYE_FALSE_ALARM);
        for (metric, value) in &before.metrics {
            if metric != "fye" {
                assert_eq!(after.metrics[metric], *value, "{metric}");
            }
        }
        // A ruling for a different value does not apply.
        let other = Accepted::from([("fye".to_owned(), vec![json!(false)])]);
        assert_eq!(score(gold, Some(&candidate), &other), before);
    }

    #[test]
    fn weights_move_the_weighted_mean() {
        assert_eq!(weighted_mean(&[(1.0, 1.0), (1.0, 0.0)]), Some(0.5));
        assert_eq!(weighted_mean(&[(3.0, 1.0), (1.0, 0.0)]), Some(0.75));
        assert_eq!(weighted_mean(&[]), None);
    }

    #[test]
    fn paired_bootstrap_is_seeded_and_covers_a_planted_delta() {
        let mut rng = crate::SplitMix64::new(11);
        let mut base = BTreeMap::new();
        let mut run = BTreeMap::new();
        for key in 0..400 {
            let x = (rng.below(1000) as f64) / 1000.0;
            let noise = (rng.below(200) as f64 - 100.0) / 1000.0;
            base.insert(key, (1.0, x));
            run.insert(key, (1.0, x + 0.1 + noise));
        }
        let first = paired_delta(&run, &base, 5).unwrap();
        assert_eq!(first, paired_delta(&run, &base, 5).unwrap());
        assert_ne!(first, paired_delta(&run, &base, 6).unwrap());
        assert!(first.low <= 0.1 && 0.1 <= first.high, "{first:?}");
        assert!(first.excludes_zero());
        let same = paired_delta(&base, &base, 5).unwrap();
        assert!(!same.excludes_zero());
    }
}
