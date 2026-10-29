//! Per-field agreement between a candidate decision and gold. Programmatic,
//! no LLM judge: every metric is a number in [0, 1] (1 = agrees) or absent
//! when it does not apply, and a failed run scores 0 on every metric that
//! applies. Free text is compared by embedding similarity through
//! [`TextSimilarity`]; without one the `*_sim` metrics are absent.
//!
//! [`COVERAGE`] maps every leaf of the decision schema to the metrics that
//! score it, and a test fails when the schema grows a field it does not name.
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
//! | `text_safe` violation (a URL or credential in text) | 10 |
//! | login code disagreement                 | 3    |
//! | gold action with no candidate action    | 2 each, at most 4 |
//! | model failure (replaces all of the above) | 10 |
use crate::case::{Case, is_push};
use crate::run::ResultRow;
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Value, json};
use squelch_core::triage::agent_config::RankingConfig;
use squelch_core::triage::decision::{
    AttentionAction, AttentionState, AuthKind, EvidenceRef, MessageDecision, MessageDestination,
    RecordProposal, SupportedTime,
};
use std::collections::{BTreeMap, BTreeSet};

/// Bumped whenever a metric's definition changes; stored on each row and
/// printed by `report`, which always re-scores from the raw decision.
pub const SCORER_VERSION: u32 = 3;

/// Agreement metrics, headline first; the Step C set, then the rest.
pub const METRICS: &[&str] = &[
    "fye",
    "reading",
    "auth",
    "auth_kinds",
    "auth_critical",
    "restricted",
    "push",
    "kinds",
    "kinds_exact",
    "state",
    "actions",
    "records",
    "record_fields",
    "reading_kind",
    "ends",
    "revisit",
    "revisit_at",
    "done",
    "related_updates",
    "rule_exceptions",
    "importance",
    "login_code",
    "text_safe",
    "factors",
    "attention_at",
    "relevant_messages",
    "action_fields",
    "summary_sim",
    "attention_summary_sim",
    "push_text_sim",
    "action_text_sim",
    "record_text_sim",
    "reason_sim",
    "evidence",
];

/// Metrics that need a [`TextSimilarity`].
pub const TEXT_METRICS: &[&str] = &[
    "summary_sim",
    "attention_summary_sim",
    "push_text_sim",
    "action_text_sim",
    "record_text_sim",
    "reason_sim",
];

/// The user-facing text metrics `--texts` ranks by.
pub const USER_TEXT_METRICS: &[&str] = &[
    "summary_sim",
    "attention_summary_sim",
    "push_text_sim",
    "action_text_sim",
];

/// How `report` groups the agreement table (`rank_delta` is listed where it
/// belongs; it is lower-is-better and not in [`METRICS`]).
pub const SECTIONS: &[(&str, &[&str])] = &[
    (
        "Placement",
        &[
            "fye",
            "reading",
            "reading_kind",
            "ends",
            "kinds",
            "kinds_exact",
            "state",
            "revisit",
            "revisit_at",
            "done",
            "related_updates",
            "rule_exceptions",
        ],
    ),
    (
        "Security and push",
        &[
            "auth",
            "auth_kinds",
            "auth_critical",
            "restricted",
            "push",
            "importance",
            "login_code",
            "text_safe",
        ],
    ),
    (
        "Attention detail",
        &[
            "factors",
            "attention_at",
            "relevant_messages",
            "actions",
            "action_fields",
            "rank_delta",
        ],
    ),
    ("Records", &["records", "record_fields"]),
    ("Text", TEXT_METRICS),
    ("Grounding", &["evidence"]),
];

/// Fields with a positive class, the two costliest misses first.
pub const DIRECTIONAL: [&str; 6] = ["fye", "restricted", "auth", "push", "reading", "login_code"];

/// Fields an owner may adjudicate (`kinds` covers `kinds_exact` too).
pub const ADJUDICABLE: [&str; 15] = [
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
    "login_code",
    "related_updates",
    "attention_at",
];

/// The four model-set attention factors.
pub const FACTORS: [&str; 4] = ["urgency", "action_need", "personal_relevance", "importance"];

pub const HARM_FYE_MISS_NEEDS_USER: f64 = 10.0;
pub const HARM_FYE_MISS: f64 = 5.0;
pub const HARM_FYE_FALSE_ALARM: f64 = 2.0;
pub const HARM_RESTRICTED_MISS: f64 = 10.0;
pub const HARM_FALSE_RESTRICTION: f64 = 1.0;
pub const HARM_AUTH_MISS: f64 = 5.0;
/// A takeover-class account change (`auth.critical`) the candidate did not
/// mark critical: it would not reach FYE as one.
pub const HARM_CRITICAL_MISS: f64 = 5.0;
pub const HARM_FALSE_CRITICAL: f64 = 1.0;
pub const HARM_PUSH_MISS: f64 = 3.0;
pub const HARM_FALSE_PUSH: f64 = 2.0;
pub const HARM_READING_MISMATCH: f64 = 1.0;
pub const HARM_RECORD_KINDS: f64 = 2.0;
pub const HARM_RECORD_FIELDS: f64 = 1.0;
pub const HARM_KINDS: f64 = 0.5;
pub const HARM_TEXT_UNSAFE: f64 = 10.0;
pub const HARM_LOGIN_CODE: f64 = 3.0;
pub const HARM_UNPAIRED_ACTION: f64 = 2.0;
pub const HARM_UNPAIRED_ACTIONS_CAP: f64 = 4.0;
pub const HARM_MODEL_FAILURE: f64 = 10.0;

/// How every leaf of the decision schema is scored. A key is an exact path,
/// `prefix.*` (everything under it) or `*.suffix` (any path ending so); the
/// first matching entry wins.
pub enum Coverage {
    Metrics(&'static [&'static str]),
    Unscored(&'static str),
}

pub const COVERAGE: &[(&str, Coverage)] = &[
    (
        "related_updates[].expected_revision",
        Coverage::Unscored(
            "a concurrency guard the executor checks against the store, not a judgement",
        ),
    ),
    (
        "related_updates[].attention.*",
        Coverage::Unscored(
            "another thread's attention; scored as that thread's own decision, and `related_updates` checks which threads are touched",
        ),
    ),
    (
        "*.evidence[].location",
        Coverage::Unscored(
            "free-text pointers into a message; the cited message ids are scored by `evidence`",
        ),
    ),
    (
        "*.timezone",
        Coverage::Unscored("display metadata; the time itself is compared by the time rule"),
    ),
    (
        "*.interpreted_relative",
        Coverage::Unscored(
            "how the model read the date, not what it decided; the date is compared",
        ),
    ),
    ("*.source_message_id", Coverage::Metrics(&["evidence"])),
    ("*.evidence[].message_id", Coverage::Metrics(&["evidence"])),
    ("kinds", Coverage::Metrics(&["kinds", "kinds_exact"])),
    ("destinations", Coverage::Metrics(&["reading"])),
    ("reading_kind", Coverage::Metrics(&["reading_kind"])),
    ("summary", Coverage::Metrics(&["summary_sim", "text_safe"])),
    ("reason", Coverage::Metrics(&["reason_sim", "text_safe"])),
    (
        "auth.kinds",
        Coverage::Metrics(&["auth", "auth_kinds", "push"]),
    ),
    (
        "external_access.restricted",
        Coverage::Metrics(&["restricted"]),
    ),
    (
        "external_access.reason",
        Coverage::Metrics(&["reason_sim", "text_safe"]),
    ),
    ("attention.show_in_fye", Coverage::Metrics(&["fye"])),
    ("attention.state", Coverage::Metrics(&["state"])),
    (
        "attention.actions[].id",
        Coverage::Metrics(&["action_fields"]),
    ),
    (
        "attention.actions[].kind",
        Coverage::Metrics(&["actions", "action_fields"]),
    ),
    (
        "attention.actions[].description",
        Coverage::Metrics(&["action_text_sim", "text_safe"]),
    ),
    (
        "attention.actions[].resolved",
        Coverage::Metrics(&["actions", "action_fields"]),
    ),
    (
        "attention.actions[].due.value",
        Coverage::Metrics(&["action_fields"]),
    ),
    (
        "attention.summary",
        Coverage::Metrics(&["attention_summary_sim", "text_safe"]),
    ),
    (
        "attention.factors.urgency",
        Coverage::Metrics(&["factors", "rank_delta"]),
    ),
    (
        "attention.factors.action_need",
        Coverage::Metrics(&["factors", "rank_delta"]),
    ),
    (
        "attention.factors.personal_relevance",
        Coverage::Metrics(&["factors", "rank_delta"]),
    ),
    (
        "attention.factors.importance",
        Coverage::Metrics(&["factors", "rank_delta"]),
    ),
    (
        "attention.factors.attention_at.value",
        Coverage::Metrics(&["attention_at"]),
    ),
    (
        "attention.relevant_message_ids",
        Coverage::Metrics(&["relevant_messages"]),
    ),
    ("records[bill].kind", Coverage::Metrics(&["records"])),
    (
        "records[bill].merchant",
        Coverage::Metrics(&["record_fields"]),
    ),
    (
        "records[bill].amount",
        Coverage::Metrics(&["record_fields"]),
    ),
    (
        "records[bill].currency",
        Coverage::Metrics(&["record_fields"]),
    ),
    (
        "records[bill].due.value",
        Coverage::Metrics(&["record_fields"]),
    ),
    (
        "records[bill].autopay",
        Coverage::Metrics(&["record_fields"]),
    ),
    ("records[receipt].kind", Coverage::Metrics(&["records"])),
    (
        "records[receipt].merchant",
        Coverage::Metrics(&["record_fields"]),
    ),
    (
        "records[receipt].amount",
        Coverage::Metrics(&["record_fields"]),
    ),
    (
        "records[receipt].currency",
        Coverage::Metrics(&["record_fields"]),
    ),
    ("records[delivery].kind", Coverage::Metrics(&["records"])),
    (
        "records[delivery].carrier",
        Coverage::Metrics(&["record_fields"]),
    ),
    (
        "records[delivery].tracking_number",
        Coverage::Metrics(&["record_fields"]),
    ),
    (
        "records[delivery].status",
        Coverage::Metrics(&["record_fields"]),
    ),
    (
        "records[delivery].item_name",
        Coverage::Metrics(&["record_text_sim"]),
    ),
    (
        "records[delivery].merchant",
        Coverage::Metrics(&["record_fields"]),
    ),
    (
        "records[delivery].order_refs",
        Coverage::Metrics(&["record_fields"]),
    ),
    ("records[event].kind", Coverage::Metrics(&["records"])),
    (
        "records[event].title",
        Coverage::Metrics(&["record_text_sim"]),
    ),
    (
        "records[event].start.value",
        Coverage::Metrics(&["record_fields"]),
    ),
    (
        "records[event].location",
        Coverage::Metrics(&["record_text_sim"]),
    ),
    (
        "records[financial_update].kind",
        Coverage::Metrics(&["records"]),
    ),
    (
        "records[financial_update].institution",
        Coverage::Metrics(&["record_fields"]),
    ),
    (
        "records[financial_update].description",
        Coverage::Metrics(&["record_text_sim"]),
    ),
    (
        "related_updates[].thread_id",
        Coverage::Metrics(&["related_updates"]),
    ),
    (
        "rule_exceptions[].rule_id",
        Coverage::Metrics(&["rule_exceptions"]),
    ),
    (
        "rule_exceptions[].reason",
        Coverage::Metrics(&["reason_sim"]),
    ),
    (
        "notification.importance",
        Coverage::Metrics(&["importance", "push"]),
    ),
    (
        "notification.title",
        Coverage::Metrics(&["push_text_sim", "text_safe"]),
    ),
    (
        "notification.body",
        Coverage::Metrics(&["push_text_sim", "text_safe"]),
    ),
    (
        "notification.reason",
        Coverage::Metrics(&["reason_sim", "text_safe"]),
    ),
    (
        "notification.login_code.service",
        Coverage::Metrics(&["login_code"]),
    ),
    (
        "notification.login_code.code",
        Coverage::Metrics(&["login_code", "text_safe"]),
    ),
    ("revisit.at", Coverage::Metrics(&["revisit", "revisit_at"])),
    ("revisit.reason", Coverage::Metrics(&["reason_sim"])),
    ("auth.critical", Coverage::Metrics(&["auth_critical"])),
    ("ends.value", Coverage::Metrics(&["ends"])),
    (
        "done.reason",
        Coverage::Unscored(
            "model-authored and untrusted, never shown or logged; whether the model answered done is scored by `done`",
        ),
    ),
    (
        "records[event].end.value",
        Coverage::Metrics(&["record_fields"]),
    ),
    (
        "records[event].event_kind",
        Coverage::Metrics(&["record_fields"]),
    ),
    (
        "records[event].commitment",
        Coverage::Metrics(&["record_fields"]),
    ),
    (
        "records[event].change",
        Coverage::Metrics(&["record_fields"]),
    ),
    (
        "records[event].identity_refs",
        Coverage::Metrics(&["record_fields"]),
    ),
    (
        "records[event].likelihood.band",
        Coverage::Metrics(&["record_fields"]),
    ),
    (
        "records[event].likelihood.reason",
        Coverage::Unscored(
            "the model's explanation of the band; the band is scored by `record_fields`",
        ),
    ),
    (
        "records[event].duration_estimate.minutes",
        Coverage::Metrics(&["record_fields"]),
    ),
    (
        "records[event].duration_estimate.basis",
        Coverage::Unscored(
            "how the model reached the estimate; the minutes are scored by `record_fields`",
        ),
    ),
];

/// The first [`COVERAGE`] entry for a schema leaf path.
pub fn coverage_for(path: &str) -> Option<&'static Coverage> {
    lookup_in(COVERAGE, path)
}

fn lookup_in<'a>(table: &'a [(&str, Coverage)], path: &str) -> Option<&'a Coverage> {
    table.iter().find_map(|(key, coverage)| {
        let hit = if let Some(prefix) = key.strip_suffix(".*") {
            path.starts_with(&format!("{prefix}."))
        } else if let Some(suffix) = key.strip_prefix("*.") {
            path.ends_with(&format!(".{suffix}")) || path == suffix
        } else {
            path == *key
        };
        hit.then_some(coverage)
    })
}

/// The push threshold the scorer uses (production's default).
pub fn min_importance() -> u8 {
    crate::export::default_min_importance()
}

/// For one case: the candidate values an owner has ruled acceptable, per
/// field. Empty for a case nobody adjudicated.
pub type Accepted = BTreeMap<String, Vec<Value>>;

/// Similarity of two texts in [0, 1]. The report's implementation embeds
/// both and takes the cosine; tests use a deterministic stand-in.
pub trait TextSimilarity {
    fn similarity(&self, a: &str, b: &str) -> f64;
}

/// Whitespace-normalized equality short-circuits the embedder.
pub fn text_similarity(sim: &dyn TextSimilarity, a: &str, b: &str) -> f64 {
    let normalize = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let (a, b) = (normalize(a), normalize(b));
    if a == b {
        1.0
    } else if a.is_empty() || b.is_empty() {
        0.0
    } else {
        sim.similarity(&a, &b).clamp(0.0, 1.0)
    }
}

/// What scoring needs beyond the two decisions.
#[derive(Clone, Copy, Default)]
pub struct Context<'a> {
    /// The target email, for the credential check.
    pub subject: &'a str,
    pub body: &'a str,
    pub sim: Option<&'a dyn TextSimilarity>,
}

impl<'a> Context<'a> {
    pub fn for_case(case: &'a Case, sim: Option<&'a dyn TextSimilarity>) -> Self {
        let message = &case.context.initial["message"];
        Self {
            subject: message["subject"].as_str().unwrap_or_default(),
            body: message["body"].as_str().unwrap_or_default(),
            sim,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Score {
    pub metrics: BTreeMap<String, f64>,
    /// Lower is better; only when both show in FYE.
    pub rank_delta: Option<f64>,
    pub harm: f64,
    /// Per directional field: (effective gold, candidate). A failed run
    /// predicts negative.
    pub directional: BTreeMap<String, (bool, bool)>,
    /// Absolute error per attention factor (ok runs only).
    pub factor_errors: BTreeMap<String, f64>,
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

fn login_code_value(decision: &MessageDecision) -> Value {
    match &decision.notification.login_code {
        Some(code) => json!({"code": code.code, "service": code.service.trim().to_lowercase()}),
        None => Value::Null,
    }
}

fn update_threads(decision: &MessageDecision) -> BTreeSet<String> {
    decision
        .related_updates
        .iter()
        .map(|u| u.thread_id.clone())
        .collect()
}

fn rule_ids(decision: &MessageDecision) -> BTreeSet<i64> {
    decision.rule_exceptions.iter().map(|r| r.rule_id).collect()
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
        "auth_critical" => json!(decision.auth.critical),
        "ends" => json!(decision.ends.as_ref().map(|t| t.value.clone())),
        "done" => json!(decision.done.is_some()),
        "kinds" | "kinds_exact" => json!(sorted_names(decision.kinds.iter().copied())),
        "state" => json!(name(&decision.attention.state)),
        "actions" => json!(unresolved_actions(decision)),
        "records" => {
            let mut kinds: Vec<&str> = decision.records.iter().map(record_kind).collect();
            kinds.sort_unstable();
            json!(kinds)
        }
        "reading_kind" => json!(decision.reading_kind.as_ref().map(name)),
        "login_code" => login_code_value(decision),
        "related_updates" => json!(update_threads(decision)),
        "attention_at" => json!(
            decision
                .attention
                .factors
                .attention_at
                .as_ref()
                .map(|t| t.value.clone())
        ),
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq)]
enum When {
    Date(NaiveDate),
    Instant(DateTime<Utc>),
    Unparsed(String),
}

fn when(value: &str) -> When {
    let value = value.trim();
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return When::Date(date);
    }
    match DateTime::parse_from_rfc3339(value) {
        Ok(at) => When::Instant(at.with_timezone(&Utc)),
        Err(_) => When::Unparsed(value.to_owned()),
    }
}

/// The time rule: two dates agree when equal; two timestamps within an
/// hour; a date and a timestamp when the timestamp's UTC date is that date.
/// Text that parses as neither agrees only with identical text.
pub fn times_agree(a: &str, b: &str) -> bool {
    match (when(a), when(b)) {
        (When::Date(x), When::Date(y)) => x == y,
        (When::Instant(x), When::Instant(y)) => (x - y).num_seconds().abs() <= 3600,
        (When::Date(d), When::Instant(t)) | (When::Instant(t), When::Date(d)) => {
            t.date_naive() == d
        }
        (When::Unparsed(x), When::Unparsed(y)) => x == y,
        _ => false,
    }
}

/// Both absent agree; one absent does not.
fn optional_times_agree(a: &Option<SupportedTime>, b: &Option<SupportedTime>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => times_agree(&a.value, &b.value),
        _ => false,
    }
}

/// The name rule for merchants and institutions: lowercase, punctuation to
/// spaces, a trailing `com`/`inc`/`llc`/`ltd`/`co` dropped; then equal, or
/// one contains the other.
pub fn names_agree(a: Option<&str>, b: Option<&str>) -> bool {
    fn clean(name: &str) -> String {
        let spaced: String = name
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { ' ' })
            .collect();
        let mut words: Vec<&str> = spaced.split_whitespace().collect();
        while words.len() > 1
            && matches!(words.last(), Some(&("com" | "inc" | "llc" | "ltd" | "co")))
        {
            words.pop();
        }
        words.join(" ")
    }
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            let (a, b) = (clean(a), clean(b));
            a == b || (!a.is_empty() && !b.is_empty() && (a.contains(&b) || b.contains(&a)))
        }
        _ => false,
    }
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

fn lower(value: &Option<String>) -> Option<String> {
    value.as_ref().map(|v| v.trim().to_lowercase())
}

fn order_refs(refs: &[String]) -> BTreeSet<String> {
    refs.iter()
        .map(|r| {
            r.chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .trim_start_matches('#')
                .to_lowercase()
        })
        .collect()
}

/// Agreement on the fact that decides which candidate record is "the same"
/// one (higher is better).
fn key_agreement(gold: &RecordProposal, candidate: &RecordProposal) -> u8 {
    use RecordProposal::*;
    match (gold, candidate) {
        (
            Delivery {
                tracking_number, ..
            },
            Delivery {
                tracking_number: t, ..
            },
        ) => u8::from(tracking_number.is_some() && normalize(tracking_number) == normalize(t)),
        (Bill { amount, .. }, Bill { amount: a, .. })
        | (Receipt { amount, .. }, Receipt { amount: a, .. }) => {
            u8::from(amount.is_some() && amount_agrees(*amount, *a))
        }
        (
            Event { start, title, .. },
            Event {
                start: s, title: t, ..
            },
        ) => {
            2 * u8::from(start.is_some() && optional_times_agree(start, s))
                + u8::from(title.trim().eq_ignore_ascii_case(t.trim()))
        }
        (FinancialUpdate { institution, .. }, FinancialUpdate { institution: i, .. }) => {
            u8::from(names_agree(Some(institution), Some(i)))
        }
        _ => 0,
    }
}

/// For each gold record, the index of its candidate record: same kind,
/// greedy on the key fact, ties by order.
pub fn pair_records(gold: &MessageDecision, candidate: &MessageDecision) -> Vec<Option<usize>> {
    let mut used = vec![false; candidate.records.len()];
    gold.records
        .iter()
        .map(|record| {
            let kind = record_kind(record);
            let best = candidate
                .records
                .iter()
                .enumerate()
                .filter(|(i, c)| !used[*i] && record_kind(c) == kind)
                .max_by(|(i, a), (j, b)| {
                    key_agreement(record, a)
                        .cmp(&key_agreement(record, b))
                        .then(j.cmp(i))
                })
                .map(|(i, _)| i);
            if let Some(i) = best {
                used[i] = true;
            }
            best
        })
        .collect()
}

/// The exact facts of a gold record that agree with its pair, of how many.
fn record_facts(gold: &RecordProposal, candidate: Option<&RecordProposal>) -> (usize, usize) {
    use RecordProposal::*;
    let checks: Vec<bool> = match (gold, candidate) {
        (
            Bill {
                merchant,
                amount,
                currency,
                due,
                autopay,
                ..
            },
            Some(Bill {
                merchant: m,
                amount: a,
                currency: c,
                due: d,
                autopay: p,
                ..
            }),
        ) => vec![
            names_agree(Some(merchant), Some(m)),
            amount_agrees(*amount, *a),
            lower(currency) == lower(c),
            optional_times_agree(due, d),
            autopay == p,
        ],
        (Bill { .. }, _) => vec![false; 5],
        (
            Receipt {
                merchant,
                amount,
                currency,
                ..
            },
            Some(Receipt {
                merchant: m,
                amount: a,
                currency: c,
                ..
            }),
        ) => vec![
            names_agree(Some(merchant), Some(m)),
            amount_agrees(*amount, *a),
            lower(currency) == lower(c),
        ],
        (Receipt { .. }, _) => vec![false; 3],
        (
            Delivery {
                carrier,
                tracking_number,
                status,
                merchant,
                order_refs: refs,
                ..
            },
            Some(Delivery {
                carrier: c,
                tracking_number: t,
                status: s,
                merchant: m,
                order_refs: r,
                ..
            }),
        ) => vec![
            lower(carrier) == lower(c),
            normalize(tracking_number) == normalize(t),
            status == s,
            names_agree(merchant.as_deref(), m.as_deref()),
            order_refs(refs) == order_refs(r),
        ],
        (Delivery { .. }, _) => vec![false; 5],
        (
            Event {
                start,
                end,
                duration_estimate,
                event_kind,
                commitment,
                change,
                identity_refs,
                likelihood,
                ..
            },
            Some(Event {
                start: s,
                end: e,
                duration_estimate: d,
                event_kind: k,
                commitment: c,
                change: ch,
                identity_refs: refs,
                likelihood: l,
                ..
            }),
        ) => vec![
            optional_times_agree(start, s),
            optional_times_agree(end, e),
            durations_agree(
                duration_estimate.as_ref().map(|d| d.minutes),
                d.as_ref().map(|d| d.minutes),
            ),
            event_kind == k,
            commitment == c,
            change == ch,
            identity_refs_agree(identity_refs, refs),
            likelihood.as_ref().map(|l| &l.band) == l.as_ref().map(|l| &l.band),
        ],
        (Event { .. }, _) => vec![false; 8],
        (FinancialUpdate { institution, .. }, Some(FinancialUpdate { institution: i, .. })) => {
            vec![names_agree(Some(institution), Some(i))]
        }
        (FinancialUpdate { .. }, _) => vec![false],
    };
    (checks.iter().filter(|ok| **ok).count(), checks.len())
}

/// Durations agree when both are absent, or within a quarter of the gold's
/// length (at least 15 minutes either way): an estimate, not a fact.
fn durations_agree(gold: Option<i64>, candidate: Option<i64>) -> bool {
    match (gold, candidate) {
        (None, None) => true,
        (Some(g), Some(c)) => (g - c).abs() <= (g.abs() / 4).max(15),
        _ => false,
    }
}

/// Identity refs (iCalendar UIDs, confirmation numbers, event URLs) agree
/// when both are empty or they share one ref, compared without case or
/// surrounding space: one shared ref is enough to tie the event to its calendar
/// entry.
fn identity_refs_agree(gold: &[String], candidate: &[String]) -> bool {
    let set = |refs: &[String]| -> BTreeSet<String> {
        refs.iter()
            .map(|r| r.trim().to_ascii_lowercase())
            .filter(|r| !r.is_empty())
            .collect()
    };
    let (g, c) = (set(gold), set(candidate));
    (g.is_empty() && c.is_empty()) || g.intersection(&c).next().is_some()
}

/// The free-text fields of a record, by name.
fn record_texts(record: &RecordProposal) -> Vec<(&'static str, Option<&str>)> {
    use RecordProposal::*;
    match record {
        Delivery { item_name, .. } => vec![("item_name", item_name.as_deref())],
        Event {
            title, location, ..
        } => vec![("title", Some(title)), ("location", location.as_deref())],
        FinancialUpdate { description, .. } => vec![("description", Some(description))],
        _ => Vec::new(),
    }
}

/// For each gold action, its candidate action: the same non-null id first,
/// then the same kind (preferring one whose due agrees), in order.
pub fn pair_actions(gold: &[AttentionAction], candidate: &[AttentionAction]) -> Vec<Option<usize>> {
    let mut used = vec![false; candidate.len()];
    let mut pairs = vec![None; gold.len()];
    for (g, action) in gold.iter().enumerate() {
        if let Some(id) = &action.id
            && let Some(c) = candidate
                .iter()
                .enumerate()
                .position(|(i, c)| !used[i] && c.id.as_ref() == Some(id))
        {
            used[c] = true;
            pairs[g] = Some(c);
        }
    }
    for (g, action) in gold.iter().enumerate() {
        if pairs[g].is_some() {
            continue;
        }
        let same_kind = |i: &usize| !used[*i] && candidate[*i].kind == action.kind;
        let pick = (0..candidate.len())
            .filter(same_kind)
            .find(|i| optional_times_agree(&action.due, &candidate[*i].due))
            .or_else(|| (0..candidate.len()).find(same_kind));
        if let Some(c) = pick {
            used[c] = true;
            pairs[g] = Some(c);
        }
    }
    pairs
}

fn jaccard<T: Ord>(a: &BTreeSet<T>, b: &BTreeSet<T>) -> f64 {
    let union = a.union(b).count();
    if union == 0 {
        1.0
    } else {
        a.intersection(b).count() as f64 / union as f64
    }
}

fn cited(decision: &MessageDecision) -> BTreeSet<i64> {
    let mut ids = BTreeSet::new();
    let mut add = |refs: &[EvidenceRef]| ids.extend(refs.iter().map(|r| r.message_id));
    add(&decision.external_access.evidence);
    add(&decision.auth.evidence);
    add(&decision.attention.evidence);
    for action in &decision.attention.actions {
        add(&action.evidence);
    }
    for record in &decision.records {
        add(record.evidence());
    }
    for update in &decision.related_updates {
        add(&update.evidence);
    }
    for exception in &decision.rule_exceptions {
        add(&exception.evidence);
    }
    let mut times: Vec<&SupportedTime> = Vec::new();
    times.extend(decision.attention.factors.attention_at.as_ref());
    times.extend(
        decision
            .attention
            .actions
            .iter()
            .filter_map(|a| a.due.as_ref()),
    );
    for record in &decision.records {
        match record {
            RecordProposal::Bill { due, .. } => times.extend(due.as_ref()),
            RecordProposal::Event { start, .. } => times.extend(start.as_ref()),
            _ => {}
        }
    }
    ids.extend(times.iter().map(|t| t.source_message_id));
    ids
}

/// The candidate's own text, every field the prompt says must never carry
/// a credential or an access link.
fn all_text(decision: &MessageDecision) -> Vec<&str> {
    let mut texts = vec![
        decision.summary.as_str(),
        decision.reason.as_str(),
        decision.attention.summary.as_str(),
        decision.notification.title.as_str(),
        decision.notification.body.as_str(),
        decision.notification.reason.as_str(),
        decision.external_access.reason.as_str(),
    ];
    texts.extend(
        decision
            .attention
            .actions
            .iter()
            .map(|a| a.description.as_str()),
    );
    let mut refs: Vec<&EvidenceRef> = Vec::new();
    refs.extend(&decision.external_access.evidence);
    refs.extend(&decision.auth.evidence);
    refs.extend(&decision.attention.evidence);
    for action in &decision.attention.actions {
        refs.extend(&action.evidence);
    }
    for record in &decision.records {
        refs.extend(record.evidence());
    }
    for update in &decision.related_updates {
        refs.extend(&update.evidence);
    }
    for exception in &decision.rule_exceptions {
        refs.extend(&exception.evidence);
    }
    texts.extend(refs.iter().map(|r| r.location.as_str()));
    texts
}

/// Maximal runs of 4 to 8 ASCII digits.
fn digit_runs(text: &str) -> Vec<&str> {
    let mut runs = Vec::new();
    let mut start = None;
    for (i, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        match (c.is_ascii_digit(), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                if (4..=8).contains(&(i - s)) {
                    runs.push(&text[s..i]);
                }
                start = None;
            }
            _ => {}
        }
    }
    runs
}

/// The product invariant: no URL and no credential in any text the
/// candidate wrote. Credentials: its own extracted login code, and (on auth
/// or restricted mail) a 4 to 8 digit run from the email that gold never
/// wrote.
pub fn text_safe(gold: &MessageDecision, candidate: &MessageDecision, ctx: &Context) -> bool {
    let texts = all_text(candidate);
    if texts.iter().any(|t| {
        let t = t.to_lowercase();
        t.contains("http://") || t.contains("https://")
    }) {
        return false;
    }
    if let Some(code) = &candidate.notification.login_code {
        let code = code.code.trim();
        if !code.is_empty() && texts.iter().any(|t| t.contains(code)) {
            return false;
        }
    }
    if gold.auth.is_auth() || gold.external_access.restricted {
        let gold_text = all_text(gold);
        for text in &texts {
            for run in digit_runs(text) {
                let in_email = ctx.subject.contains(run) || ctx.body.contains(run);
                if in_email && !gold_text.iter().any(|g| g.contains(run)) {
                    return false;
                }
            }
        }
    }
    true
}

/// User-facing text: summary, attention summary, push title and body,
/// action descriptions.
pub fn user_text(decision: &MessageDecision) -> Vec<&str> {
    let mut texts = vec![
        decision.summary.as_str(),
        decision.attention.summary.as_str(),
        decision.notification.title.as_str(),
        decision.notification.body.as_str(),
    ];
    texts.extend(
        decision
            .attention
            .actions
            .iter()
            .map(|a| a.description.as_str()),
    );
    texts
}

/// (contains an em dash, characters) over the user-facing text.
pub fn style(decision: &MessageDecision) -> (bool, usize) {
    let texts = user_text(decision);
    (
        texts.iter().any(|t| t.contains('\u{2014}')),
        texts.iter().map(|t| t.chars().count()).sum(),
    )
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

fn factor_values(decision: &MessageDecision) -> [f64; 4] {
    let f = &decision.attention.factors;
    [f.urgency, f.action_need, f.personal_relevance, f.importance]
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

/// Which metrics apply when only gold is known (a failed run).
fn applies_from_gold(metric: &str, gold: &MessageDecision, has_sim: bool) -> bool {
    match metric {
        "auth_kinds" => gold.auth.is_auth(),
        "auth_critical" => is_account_change(gold),
        "ends" => gold.ends.is_some(),
        "done" => gold.done.is_some(),
        "actions" => !unresolved_actions(gold).is_empty(),
        "action_fields" => !gold.attention.actions.is_empty(),
        "action_text_sim" => has_sim && !gold.attention.actions.is_empty(),
        "records" | "record_fields" => !gold.records.is_empty(),
        "record_text_sim" => {
            has_sim
                && gold
                    .records
                    .iter()
                    .flat_map(record_texts)
                    .any(|(_, text)| text.is_some_and(|t| !t.trim().is_empty()))
        }
        "reading_kind" => is_reading(gold) && gold.reading_kind.is_some(),
        "revisit_at" => false,
        "attention_at" => gold.attention.factors.attention_at.is_some(),
        "login_code" => gold.notification.login_code.is_some(),
        "related_updates" => !gold.related_updates.is_empty(),
        "rule_exceptions" => !gold.rule_exceptions.is_empty(),
        m if TEXT_METRICS.contains(&m) => has_sim,
        _ => true,
    }
}

/// `auth.critical` only means something on an account change (validation
/// normalizes it to false otherwise).
fn is_account_change(decision: &MessageDecision) -> bool {
    decision.auth.kinds.contains(&AuthKind::AccountChange)
}

/// Score a candidate (None = the run failed) against gold.
pub fn score(
    gold: &MessageDecision,
    candidate: Option<&MessageDecision>,
    accepted: &Accepted,
    ctx: &Context,
) -> Score {
    let mut out = Score::default();
    let Some(c) = candidate else {
        for metric in METRICS {
            if applies_from_gold(metric, gold, ctx.sim.is_some()) {
                out.metrics.insert((*metric).into(), 0.0);
            }
        }
        for field in DIRECTIONAL {
            let gold_value = positive(field, gold);
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
    let agree = |ok: bool| f64::from(u8::from(ok));
    // Boolean fields: an accepted candidate value IS the gold for this case.
    let mut flag = |field: &str| -> (bool, bool) {
        let candidate_value = positive(field, c);
        let gold_value = if accepts(field) {
            candidate_value
        } else {
            positive(field, gold)
        };
        out.metrics
            .insert(field.into(), agree(gold_value == candidate_value));
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
    let set = |out: &mut Score, metric: &str, value: f64| {
        out.metrics.insert(metric.into(), value);
    };
    if gold.auth.is_auth() {
        set(&mut out, "auth_kinds", agree(same("auth_kinds")));
    }
    let critical = (gold.auth.critical, c.auth.critical);
    if is_account_change(gold) || is_account_change(c) {
        set(&mut out, "auth_critical", agree(critical.0 == critical.1));
    }
    if gold.ends.is_some() || c.ends.is_some() {
        set(
            &mut out,
            "ends",
            agree(optional_times_agree(&gold.ends, &c.ends)),
        );
    }
    if gold.done.is_some() || c.done.is_some() {
        set(
            &mut out,
            "done",
            agree(gold.done.is_some() == c.done.is_some()),
        );
    }
    let gold_kinds: BTreeSet<String> = sorted_names(gold.kinds.iter().copied())
        .into_iter()
        .collect();
    let candidate_kinds: BTreeSet<String> =
        sorted_names(c.kinds.iter().copied()).into_iter().collect();
    let kinds = if accepts("kinds") {
        1.0
    } else {
        jaccard(&gold_kinds, &candidate_kinds)
    };
    set(&mut out, "kinds", kinds);
    set(&mut out, "kinds_exact", agree(same("kinds")));
    set(&mut out, "state", agree(same("state")));
    if !unresolved_actions(gold).is_empty() || !unresolved_actions(c).is_empty() {
        set(&mut out, "actions", agree(same("actions")));
    }
    // Records.
    let record_pairs = pair_records(gold, c);
    let mut records_harm = 0.0;
    if !gold.records.is_empty() {
        let kinds_agree = same("records");
        set(&mut out, "records", agree(kinds_agree));
        let (mut agreeing, mut total) = (0, 0);
        for (record, pair) in gold.records.iter().zip(&record_pairs) {
            let (a, t) = record_facts(record, pair.map(|i| &c.records[i]));
            agreeing += a;
            total += t;
        }
        let fields = if total == 0 {
            1.0
        } else {
            agreeing as f64 / total as f64
        };
        set(&mut out, "record_fields", fields);
        if !kinds_agree {
            records_harm += HARM_RECORD_KINDS;
        }
        records_harm += HARM_RECORD_FIELDS * (1.0 - fields);
    }
    if is_reading(gold) && gold.reading_kind.is_some() && is_reading(c) {
        set(&mut out, "reading_kind", agree(same("reading_kind")));
    }
    if gold.attention.show_in_fye && c.attention.show_in_fye {
        out.rank_delta = Some((static_rank(c) - static_rank(gold)).abs());
    }
    if let (Some(g), Some(k)) = (&gold.revisit, &c.revisit) {
        set(
            &mut out,
            "revisit_at",
            agree((g.at - k.at).num_seconds().abs() <= 86_400),
        );
    }
    if !gold.related_updates.is_empty() || !c.related_updates.is_empty() {
        set(
            &mut out,
            "related_updates",
            agree(accepts("related_updates") || update_threads(gold) == update_threads(c)),
        );
    }
    if !gold.rule_exceptions.is_empty() || !c.rule_exceptions.is_empty() {
        set(
            &mut out,
            "rule_exceptions",
            agree(rule_ids(gold) == rule_ids(c)),
        );
    }
    let importance_gap =
        (f64::from(gold.notification.importance) - f64::from(c.notification.importance)).abs();
    set(
        &mut out,
        "importance",
        1.0 - (importance_gap / 100.0).min(1.0),
    );
    // Login code: a positive class (a code was extracted) and an exact fact.
    let mut login_harm = 0.0;
    let candidate_code = c.notification.login_code.is_some();
    let gold_code = if accepts("login_code") {
        candidate_code
    } else {
        gold.notification.login_code.is_some()
    };
    out.directional
        .insert("login_code".into(), (gold_code, candidate_code));
    if gold.notification.login_code.is_some() || candidate_code {
        let codes_agree = accepts("login_code")
            || match (&gold.notification.login_code, &c.notification.login_code) {
                (Some(g), Some(k)) => {
                    g.code == k.code && g.service.trim().eq_ignore_ascii_case(k.service.trim())
                }
                _ => false,
            };
        set(&mut out, "login_code", agree(codes_agree));
        if !codes_agree {
            login_harm = HARM_LOGIN_CODE;
        }
    }
    let safe = text_safe(gold, c, ctx);
    set(&mut out, "text_safe", agree(safe));
    // Attention detail.
    let (g, k) = (factor_values(gold), factor_values(c));
    let errors: Vec<f64> = g.iter().zip(&k).map(|(a, b)| (a - b).abs()).collect();
    for (factor, error) in FACTORS.iter().zip(&errors) {
        out.factor_errors.insert((*factor).into(), *error);
    }
    set(
        &mut out,
        "factors",
        1.0 - mean(&errors).unwrap_or(0.0).min(1.0),
    );
    let (gold_at, candidate_at) = (
        &gold.attention.factors.attention_at,
        &c.attention.factors.attention_at,
    );
    if gold_at.is_some() || candidate_at.is_some() {
        let ok = accepts("attention_at")
            || (gold_at.is_some() && optional_times_agree(gold_at, candidate_at));
        set(&mut out, "attention_at", agree(ok));
    }
    let relevant = |d: &MessageDecision| -> BTreeSet<i64> {
        d.attention.relevant_message_ids.iter().copied().collect()
    };
    set(
        &mut out,
        "relevant_messages",
        jaccard(&relevant(gold), &relevant(c)),
    );
    let action_pairs = pair_actions(&gold.attention.actions, &c.attention.actions);
    let unpaired = action_pairs.iter().filter(|p| p.is_none()).count();
    if !gold.attention.actions.is_empty() {
        let mut facts = 0usize;
        for (action, pair) in gold.attention.actions.iter().zip(&action_pairs) {
            if let Some(i) = pair {
                let other = &c.attention.actions[*i];
                facts += 1;
                facts += usize::from(action.resolved == other.resolved);
                facts += usize::from(optional_times_agree(&action.due, &other.due));
                facts += usize::from(action.id == other.id);
            }
        }
        set(
            &mut out,
            "action_fields",
            facts as f64 / (4 * gold.attention.actions.len()) as f64,
        );
    }
    set(&mut out, "evidence", jaccard(&cited(gold), &cited(c)));
    // Free text, when a similarity is available.
    if let Some(sim) = ctx.sim {
        let s = |a: &str, b: &str| text_similarity(sim, a, b);
        set(&mut out, "summary_sim", s(&gold.summary, &c.summary));
        set(
            &mut out,
            "attention_summary_sim",
            s(&gold.attention.summary, &c.attention.summary),
        );
        set(
            &mut out,
            "push_text_sim",
            (s(&gold.notification.title, &c.notification.title)
                + s(&gold.notification.body, &c.notification.body))
                / 2.0,
        );
        if !gold.attention.actions.is_empty() {
            let values: Vec<f64> = gold
                .attention
                .actions
                .iter()
                .zip(&action_pairs)
                .map(|(action, pair)| {
                    pair.map_or(0.0, |i| {
                        s(&action.description, &c.attention.actions[i].description)
                    })
                })
                .collect();
            set(&mut out, "action_text_sim", mean(&values).unwrap_or(0.0));
        }
        let mut record_values = Vec::new();
        for (record, pair) in gold.records.iter().zip(&record_pairs) {
            let other = pair.map(|i| record_texts(&c.records[i]));
            for (field, text) in record_texts(record) {
                let Some(text) = text.filter(|t| !t.trim().is_empty()) else {
                    continue;
                };
                let theirs = other
                    .as_ref()
                    .and_then(|o| o.iter().find(|(f, _)| *f == field))
                    .and_then(|(_, t)| *t);
                record_values.push(theirs.map_or(0.0, |t| s(text, t)));
            }
        }
        if let Some(value) = mean(&record_values) {
            set(&mut out, "record_text_sim", value);
        }
        let mut reasons = vec![
            s(&gold.reason, &c.reason),
            s(&gold.external_access.reason, &c.external_access.reason),
            s(&gold.notification.reason, &c.notification.reason),
        ];
        if let (Some(g), Some(k)) = (&gold.revisit, &c.revisit) {
            reasons.push(s(&g.reason, &k.reason));
        }
        for exception in &gold.rule_exceptions {
            if let Some(other) = c
                .rule_exceptions
                .iter()
                .find(|o| o.rule_id == exception.rule_id)
            {
                reasons.push(s(&exception.reason, &other.reason));
            }
        }
        set(&mut out, "reason_sim", mean(&reasons).unwrap_or(0.0));
    }
    let mut harm = records_harm + HARM_KINDS * (1.0 - kinds) + login_harm;
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
    harm += match critical {
        (true, false) => HARM_CRITICAL_MISS,
        (false, true) => HARM_FALSE_CRITICAL,
        _ => 0.0,
    };
    harm += match push {
        (true, false) => HARM_PUSH_MISS,
        (false, true) => HARM_FALSE_PUSH,
        _ => 0.0,
    };
    if reading.0 != reading.1 {
        harm += HARM_READING_MISMATCH;
    }
    if !safe {
        harm += HARM_TEXT_UNSAFE;
    }
    harm += (HARM_UNPAIRED_ACTION * unpaired as f64).min(HARM_UNPAIRED_ACTIONS_CAP);
    out.harm = harm;
    out
}

/// The positive class of a directional or boolean field.
fn positive(field: &str, decision: &MessageDecision) -> bool {
    match field {
        "login_code" => decision.notification.login_code.is_some(),
        _ => field_value(field, decision)
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    }
}

/// The `grade` map written onto each result row: the metrics (no text
/// similarity at run time), plus `harm` and `rank_delta` when present. A
/// convenience for other tools; `report` never reads it back.
pub fn grade(case: &Case, row: &ResultRow) -> BTreeMap<String, f64> {
    let score = score(
        &case.gold,
        row.decision.as_ref(),
        &Accepted::new(),
        &Context::for_case(case, None),
    );
    let mut grade = score.metrics;
    grade.insert("harm".into(), score.harm);
    if let Some(delta) = score.rank_delta {
        grade.insert("rank_delta".into(), delta);
    }
    grade
}

/// Every leaf path of a JSON schema: object properties joined by `.`, array
/// items as `[]` (or `[kind]` for a branch whose `kind` is one fixed value),
/// `$ref`s resolved through the root's `$defs`, nullable branches skipped.
/// An array of scalars is itself a leaf.
pub fn schema_leaves(schema: &Value) -> Vec<String> {
    fn resolve<'a>(node: &'a Value, root: &'a Value) -> &'a Value {
        match node.get("$ref").and_then(Value::as_str) {
            Some(target) => {
                let name = target.trim_start_matches("#/$defs/");
                resolve(&root["$defs"][name], root)
            }
            None => node,
        }
    }
    fn is_null(node: &Value) -> bool {
        node.get("type") == Some(&json!("null"))
    }
    fn is_scalar(node: &Value, root: &Value) -> bool {
        let node = resolve(node, root);
        if let Some(branches) = node.get("anyOf").and_then(Value::as_array) {
            return branches.iter().all(|b| is_null(b) || is_scalar(b, root));
        }
        node.get("properties").is_none() && node.get("items").is_none()
    }
    fn walk(node: &Value, root: &Value, path: &str, out: &mut Vec<String>) {
        let node = resolve(node, root);
        if let Some(branches) = node.get("anyOf").and_then(Value::as_array) {
            for branch in branches.iter().filter(|b| !is_null(b)) {
                let branch = resolve(branch, root);
                let fixed = branch["properties"]["kind"]["enum"]
                    .as_array()
                    .filter(|values| values.len() == 1)
                    .and_then(|values| values[0].as_str());
                match (fixed, path.strip_suffix("[]")) {
                    (Some(kind), Some(stem)) => walk(branch, root, &format!("{stem}[{kind}]"), out),
                    _ => walk(branch, root, path, out),
                }
            }
            return;
        }
        if let Some(properties) = node.get("properties").and_then(Value::as_object) {
            for (key, child) in properties {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                walk(child, root, &child_path, out);
            }
            return;
        }
        if let Some(items) = node.get("items") {
            if is_scalar(items, root) {
                out.push(path.to_owned());
            } else {
                walk(items, root, &format!("{path}[]"), out);
            }
            return;
        }
        out.push(path.to_owned());
    }
    let mut out = Vec::new();
    walk(schema, schema, "", &mut out);
    out.sort();
    out.dedup();
    out
}

/// Schema leaves with no [`COVERAGE`] entry in `table`, and table metric ids
/// that are not metrics. Both empty means every emitted field is accounted for.
pub fn coverage_gaps(leaves: &[String], table: &[(&str, Coverage)]) -> (Vec<String>, Vec<String>) {
    let missing = leaves
        .iter()
        .filter(|leaf| lookup_in(table, leaf).is_none())
        .cloned()
        .collect();
    let unknown = table
        .iter()
        .filter_map(|(_, coverage)| match coverage {
            Coverage::Metrics(ids) => Some(*ids),
            Coverage::Unscored(_) => None,
        })
        .flatten()
        .filter(|id| !METRICS.contains(id) && **id != "rank_delta")
        .map(|id| (*id).to_owned())
        .collect();
    (missing, unknown)
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
    use squelch_core::triage::login_code::LoginCode;

    /// Token-set Jaccard: deterministic, no model.
    pub(crate) struct FakeSim;
    impl TextSimilarity for FakeSim {
        fn similarity(&self, a: &str, b: &str) -> f64 {
            let tokens = |t: &str| -> BTreeSet<String> {
                t.to_lowercase()
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| !w.is_empty())
                    .map(str::to_owned)
                    .collect()
            };
            jaccard(&tokens(a), &tokens(b))
        }
    }

    fn ctx() -> Context<'static> {
        Context {
            sim: Some(&FakeSim),
            ..Default::default()
        }
    }

    fn plain(gold: &MessageDecision, candidate: Option<&MessageDecision>) -> Score {
        score(gold, candidate, &Accepted::new(), &ctx())
    }

    fn evidence() -> Vec<EvidenceRef> {
        vec![EvidenceRef {
            message_id: 1,
            location: "body".into(),
        }]
    }

    fn time(value: &str) -> Option<SupportedTime> {
        Some(SupportedTime {
            value: value.into(),
            timezone: None,
            source_message_id: 1,
            interpreted_relative: false,
        })
    }

    fn texts(decision: &mut MessageDecision, tag: &str) {
        decision.attention.summary = format!("{tag} needs a look");
        decision.notification.title = format!("{tag} title");
        decision.notification.body = format!("{tag} arrived today");
        decision.notification.reason = format!("{tag} matters");
        decision.external_access.reason = format!("{tag} holds no credential");
        decision.attention.relevant_message_ids = vec![1];
        decision.attention.evidence = evidence();
    }

    /// Gold decisions that between them exercise every metric.
    pub(crate) fn golds() -> Vec<MessageDecision> {
        let mut bill = MessageDecision {
            kinds: vec![EmailKind::Bill, EmailKind::AccountService],
            summary: "Electric bill due".into(),
            reason: "A bill with a due date".into(),
            ..Default::default()
        };
        texts(&mut bill, "bill");
        bill.attention.show_in_fye = true;
        bill.attention.state = AttentionState::NeedsUser;
        bill.attention.factors.urgency = 0.8;
        bill.attention.factors.attention_at = time("2026-10-04T15:00:00Z");
        bill.attention.actions = vec![
            AttentionAction {
                id: Some("act-real".into()),
                kind: ActionKind::Pay,
                description: "Pay the electric bill".into(),
                due: time("2026-10-05"),
                evidence: evidence(),
                ..Default::default()
            },
            AttentionAction {
                kind: ActionKind::Review,
                description: "Read the statement".into(),
                resolved: true,
                evidence: evidence(),
                ..Default::default()
            },
        ];
        bill.notification.importance = 70;
        bill.records = vec![
            RecordProposal::Bill {
                merchant: "Power Co".into(),
                amount: Some(81.2),
                currency: Some("USD".into()),
                due: time("2026-10-05"),
                autopay: Some(false),
                evidence: evidence(),
            },
            RecordProposal::Delivery {
                carrier: Some("ups".into()),
                tracking_number: Some("1Z 99".into()),
                status: "shipped".into(),
                item_name: Some("Desk lamp".into()),
                merchant: Some("Amazon.com".into()),
                order_refs: vec!["#123".into()],
                evidence: evidence(),
            },
            RecordProposal::Event {
                title: "Clinic visit".into(),
                start: time("2026-10-09"),
                location: Some("Main street clinic".into()),
                end: None,
                duration_estimate: None,
                event_kind: Default::default(),
                commitment: Default::default(),
                change: Default::default(),
                identity_refs: vec![],
                likelihood: None,
                evidence: evidence(),
            },
            RecordProposal::FinancialUpdate {
                institution: "First Bank".into(),
                description: "Deposit posted".into(),
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
            at: "2026-10-06T12:00:00Z".parse().unwrap(),
            reason: "check it was paid".into(),
        });
        bill.related_updates = vec![RelatedAttentionUpdate {
            thread_id: "other".into(),
            expected_revision: 1,
            attention: ThreadAttentionDecision::default(),
            evidence: evidence(),
        }];
        bill.rule_exceptions = vec![RuleException {
            rule_id: 4,
            reason: "a bill outranks the squelch rule".into(),
            evidence: evidence(),
        }];
        let mut otp = MessageDecision {
            kinds: vec![EmailKind::AuthenticationSecurity],
            summary: "Login code from Acme".into(),
            reason: "One-time code".into(),
            ..Default::default()
        };
        texts(&mut otp, "code");
        otp.auth.kinds = vec![AuthKind::Otp];
        otp.auth.evidence = evidence();
        otp.external_access.restricted = true;
        otp.notification.login_code = Some(LoginCode {
            service: "Acme".into(),
            code: "481516".into(),
        });
        let mut newsletter = MessageDecision {
            kinds: vec![EmailKind::Editorial],
            destinations: vec![MessageDestination::Reading],
            reading_kind: Some(ReadingKind::Content),
            summary: "A newsletter".into(),
            reason: "Editorial".into(),
            ..Default::default()
        };
        texts(&mut newsletter, "letter");
        newsletter.attention.state = AttentionState::Informational;
        newsletter.ends = time("2026-10-20");
        bill.done = Some(squelch_core::triage::decision::DoneRequest {
            reason: "paid".into(),
            evidence: evidence(),
        });
        let mut takeover = MessageDecision {
            kinds: vec![EmailKind::AuthenticationSecurity],
            summary: "Recovery phone changed".into(),
            reason: "Account change".into(),
            ..Default::default()
        };
        texts(&mut takeover, "phone");
        takeover.auth.kinds = vec![AuthKind::AccountChange];
        takeover.auth.critical = true;
        takeover.auth.evidence = evidence();
        takeover.attention.show_in_fye = true;
        vec![bill, otp, newsletter, takeover]
    }

    #[test]
    fn oracle_scores_one_everywhere_with_no_harm() {
        let mut seen = BTreeSet::new();
        for gold in golds() {
            let score = plain(&gold, Some(&gold));
            for (metric, value) in &score.metrics {
                assert_eq!(*value, 1.0, "{metric}");
                seen.insert(metric.clone());
            }
            assert_eq!(score.harm, 0.0);
            assert!(score.rank_delta.is_none_or(|d| d == 0.0));
            assert!(score.factor_errors.values().all(|e| *e == 0.0));
            let has = |m: &str| score.metrics.contains_key(m);
            assert_eq!(has("auth_kinds"), gold.auth.is_auth());
            assert_eq!(has("records"), !gold.records.is_empty());
            assert_eq!(has("record_fields"), !gold.records.is_empty());
            assert_eq!(has("reading_kind"), gold.reading_kind.is_some());
            assert_eq!(has("action_fields"), !gold.attention.actions.is_empty());
            assert_eq!(has("login_code"), gold.notification.login_code.is_some());
            assert_eq!(has("revisit_at"), gold.revisit.is_some());
        }
        let all: BTreeSet<String> = METRICS.iter().map(|m| (*m).to_owned()).collect();
        assert_eq!(seen, all, "every metric exercised");
        // Without a similarity the text metrics are absent, not zero.
        let without = score(
            &golds()[0],
            Some(&golds()[0]),
            &Accepted::new(),
            &Context::default(),
        );
        assert!(
            TEXT_METRICS
                .iter()
                .all(|m| !without.metrics.contains_key(*m))
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
            for field in ["fye", "auth", "restricted", "push", "login_code"] {
                let (mut hits, mut positives) = (0, 0);
                for gold in &golds {
                    let (g, c) = plain(gold, Some(&null)).directional[field];
                    positives += usize::from(g);
                    hits += usize::from(g && c);
                }
                assert!(positives > 0, "{field}");
                assert_eq!(hits, 0, "{field}: a null candidate recalls nothing");
            }
            let bill = plain(&golds[0], Some(&null));
            for metric in [
                "summary_sim",
                "action_fields",
                "record_fields",
                "evidence",
                "factors",
                "related_updates",
                "rule_exceptions",
                "attention_at",
            ] {
                assert!(bill.metrics[metric] < 1.0, "{metric}");
            }
            let harm: f64 = golds.iter().map(|g| plain(g, Some(&null)).harm).sum();
            assert!(harm > 20.0, "harm {harm}");
        }
    }

    #[test]
    fn a_failed_run_is_zero_everywhere_and_harm_ten() {
        for gold in golds() {
            let s = plain(&gold, None);
            assert!(s.metrics.values().all(|v| *v == 0.0));
            assert_eq!(s.harm, HARM_MODEL_FAILURE);
            assert!(s.directional.values().all(|(_, c)| !c));
            let oracle = plain(&gold, Some(&gold));
            // Everything the oracle scores, except what needs both sides.
            let applicable: BTreeSet<&String> = oracle
                .metrics
                .keys()
                .filter(|m| m.as_str() != "revisit_at")
                .collect();
            let failed: BTreeSet<&String> = s.metrics.keys().collect();
            assert_eq!(failed, applicable, "applicability from gold");
            let without = score(&gold, None, &Accepted::new(), &Context::default());
            assert!(
                TEXT_METRICS
                    .iter()
                    .all(|m| !without.metrics.contains_key(*m))
            );
        }
    }

    #[test]
    fn the_time_rule() {
        assert!(times_agree("2026-10-05", "2026-10-05"));
        assert!(!times_agree("2026-10-05", "2026-10-06"));
        assert!(times_agree("2026-10-05T10:00:00Z", "2026-10-05T10:59:59Z"));
        assert!(!times_agree("2026-10-05T10:00:00Z", "2026-10-05T11:00:01Z"));
        assert!(times_agree(
            "2026-10-05T12:00:00+02:00",
            "2026-10-05T10:30:00Z"
        ));
        assert!(times_agree("2026-10-05", "2026-10-05T23:00:00Z"));
        assert!(
            !times_agree("2026-10-05", "2026-10-05T23:00:00-05:00"),
            "UTC date"
        );
        assert!(times_agree("next Tuesday", "next Tuesday"));
        assert!(!times_agree("next Tuesday", "2026-10-05"));
        assert!(optional_times_agree(&None, &None));
        assert!(!optional_times_agree(&time("2026-10-05"), &None));
    }

    #[test]
    fn the_name_rule() {
        assert!(names_agree(Some("Amazon.com"), Some("amazon")));
        assert!(names_agree(Some("Acme, Inc."), Some("ACME")));
        assert!(names_agree(Some("Chase"), Some("JPMorgan Chase Bank")));
        assert!(names_agree(Some("Power Co"), Some("power")));
        assert!(!names_agree(Some("Chase"), Some("Wells Fargo")));
        // The suffix rule matters where containment alone would not match.
        assert!(names_agree(Some("Shop Inc"), Some("The Shop")));
        assert!(!names_agree(Some("Co"), None));
        assert!(names_agree(None, None));
        assert!(!names_agree(Some(""), Some("Shop")));
    }

    #[test]
    fn records_pair_by_their_key_fact_and_score_every_exact_fact() {
        let gold = &golds()[0];
        let mut close = gold.clone();
        if let RecordProposal::Bill {
            amount, merchant, ..
        } = &mut close.records[0]
        {
            *amount = Some(81.204);
            *merchant = "POWER".into();
        }
        if let RecordProposal::Delivery {
            tracking_number,
            order_refs,
            carrier,
            ..
        } = &mut close.records[1]
        {
            *tracking_number = Some("1z99".into());
            *order_refs = vec!["123".into()];
            *carrier = Some("UPS".into());
        }
        assert_eq!(plain(gold, Some(&close)).metrics["record_fields"], 1.0);
        // 22 exact facts: bill 5, delivery 5, event 8, financial 1, receipt 3.
        let mut wrong = gold.clone();
        if let RecordProposal::Bill { autopay, .. } = &mut wrong.records[0] {
            *autopay = None;
        }
        wrong.kinds = vec![EmailKind::Bill];
        let s = plain(gold, Some(&wrong));
        assert!((s.metrics["record_fields"] - 21.0 / 22.0).abs() < 1e-9);
        assert_eq!(s.metrics["kinds"], 0.5);
        assert_eq!(s.metrics["kinds_exact"], 0.0);
        assert!((s.harm - (1.0 / 22.0 + 0.25)).abs() < 1e-9);
        // The event's newer facts count too: a commitment, a change and an
        // end that disagree are three of its eight; a duration within a
        // quarter and a shared identity ref still agree.
        let mut event = gold.clone();
        if let RecordProposal::Event {
            commitment,
            change,
            end,
            ..
        } = &mut event.records[2]
        {
            *commitment = squelch_core::triage::decision::EventCommitment::Committed;
            *change = squelch_core::triage::decision::EventChange::Cancelled;
            *end = time("2026-10-10");
        }
        let s = plain(gold, Some(&event));
        assert!((s.metrics["record_fields"] - 19.0 / 22.0).abs() < 1e-9);
        assert!(durations_agree(Some(60), Some(70)));
        assert!(!durations_agree(Some(60), Some(90)));
        assert!(!durations_agree(Some(60), None));
        assert!(identity_refs_agree(
            &["UID-1".into(), "conf 9".into()],
            &[" uid-1 ".into()]
        ));
        assert!(!identity_refs_agree(&["UID-1".into()], &[]));
        // Two receipts: the one with the right amount is the pair, whatever
        // the order.
        let receipt = |amount: f64, merchant: &str| RecordProposal::Receipt {
            merchant: merchant.into(),
            amount: Some(amount),
            currency: None,
            evidence: evidence(),
        };
        let mut two = MessageDecision {
            records: vec![receipt(5.0, "Cafe"), receipt(9.0, "Bakery")],
            ..Default::default()
        };
        let mut swapped = two.clone();
        swapped.records.reverse();
        assert_eq!(pair_records(&two, &swapped), vec![Some(1), Some(0)]);
        assert_eq!(plain(&two, Some(&swapped)).metrics["record_fields"], 1.0);
        // An unpaired gold record scores 0 on all its facts.
        let one = MessageDecision {
            records: vec![receipt(5.0, "Cafe")],
            ..Default::default()
        };
        two.records[1] = receipt(9.0, "Bakery");
        assert!((plain(&two, Some(&one)).metrics["record_fields"] - 0.5).abs() < 1e-9);
    }

    #[test]
    fn actions_pair_by_id_then_kind_and_count_their_facts() {
        let gold = &golds()[0];
        // Reordered, the id still pairs the Pay action.
        let mut reordered = gold.clone();
        reordered.attention.actions.reverse();
        assert_eq!(
            pair_actions(&gold.attention.actions, &reordered.attention.actions),
            vec![Some(1), Some(0)]
        );
        assert_eq!(plain(gold, Some(&reordered)).metrics["action_fields"], 1.0);
        // A new id instead of carrying forward, and a wrong due: 2 of 8 facts.
        let mut drifted = gold.clone();
        drifted.attention.actions[0].id = None;
        drifted.attention.actions[0].due = time("2026-10-07");
        let s = plain(gold, Some(&drifted));
        assert!((s.metrics["action_fields"] - 6.0 / 8.0).abs() < 1e-9);
        // Same kind twice, ids decide: carried-forward ids pair across order.
        let with_id = |id: &str| AttentionAction {
            id: Some(id.into()),
            kind: ActionKind::Pay,
            ..Default::default()
        };
        assert_eq!(
            pair_actions(&[with_id("x"), with_id("y")], &[with_id("y"), with_id("x")]),
            vec![Some(1), Some(0)]
        );
        // Same kind twice: the one whose due agrees pairs.
        let pay = |due: &str| AttentionAction {
            kind: ActionKind::Pay,
            due: time(due),
            ..Default::default()
        };
        assert_eq!(
            pair_actions(
                &[pay("2026-10-05")],
                &[pay("2026-10-01"), pay("2026-10-05")]
            ),
            vec![Some(1)]
        );
        // Dropped actions: 0 for each, harm 2 each capped at 4.
        let mut none = gold.clone();
        none.attention.actions.clear();
        let s = plain(gold, Some(&none));
        assert_eq!(s.metrics["action_fields"], 0.0);
        assert_eq!(s.metrics["action_text_sim"], 0.0);
        let mut three = gold.clone();
        three
            .attention
            .actions
            .push(three.attention.actions[1].clone());
        let s = plain(&three, Some(&none));
        assert!(
            (s.harm - HARM_UNPAIRED_ACTIONS_CAP).abs() < 1e-9,
            "harm {}",
            s.harm
        );
    }

    #[test]
    fn factors_importance_and_grounding_arithmetic() {
        let gold = &golds()[0];
        let mut other = gold.clone();
        other.attention.factors.urgency = 0.4; // error 0.4
        other.attention.factors.importance = 0.2; // error 0.2
        other.notification.importance = 40; // 30 points off
        other.attention.relevant_message_ids = vec![1, 2];
        let s = plain(gold, Some(&other));
        assert!((s.metrics["factors"] - (1.0 - 0.6 / 4.0)).abs() < 1e-9);
        assert!((s.factor_errors["urgency"] - 0.4).abs() < 1e-9);
        assert!((s.metrics["importance"] - 0.7).abs() < 1e-9);
        assert_eq!(s.metrics["relevant_messages"], 0.5);
        let mut more = gold.clone();
        more.attention.evidence.push(EvidenceRef {
            message_id: 7,
            location: "subject".into(),
        });
        assert_eq!(plain(gold, Some(&more)).metrics["evidence"], 0.5);
        // A date's source message is a citation too.
        let mut sourced = gold.clone();
        sourced
            .attention
            .factors
            .attention_at
            .as_mut()
            .unwrap()
            .source_message_id = 7;
        assert_eq!(plain(gold, Some(&sourced)).metrics["evidence"], 0.5);
        let mut later = gold.clone();
        later.revisit.as_mut().unwrap().at += chrono::Duration::hours(25);
        later.attention.factors.attention_at = time("2026-10-04T16:30:00Z");
        let s = plain(gold, Some(&later));
        assert_eq!(s.metrics["revisit_at"], 0.0);
        assert_eq!(s.metrics["attention_at"], 0.0);
        assert_eq!(s.metrics["revisit"], 1.0);
    }

    #[test]
    fn login_codes_must_match_exactly() {
        let otp = &golds()[1];
        let mut other = otp.clone();
        other.notification.login_code.as_mut().unwrap().service = "ACME ".into();
        assert_eq!(plain(otp, Some(&other)).metrics["login_code"], 1.0);
        other.notification.login_code.as_mut().unwrap().code = "481517".into();
        let s = plain(otp, Some(&other));
        assert_eq!(s.metrics["login_code"], 0.0);
        assert_eq!(s.harm, HARM_LOGIN_CODE);
        other.notification.login_code = None;
        let s = plain(otp, Some(&other));
        assert_eq!(s.directional["login_code"], (true, false));
        // A code invented on mail that has none applies, and disagrees.
        let letter = &golds()[2];
        let mut invented = letter.clone();
        invented.notification.login_code = otp.notification.login_code.clone();
        let s = plain(letter, Some(&invented));
        assert_eq!(s.metrics["login_code"], 0.0);
        assert_eq!(s.directional["login_code"], (false, true));
    }

    #[test]
    fn text_safety_catches_links_and_credentials() {
        let otp = &golds()[1];
        let email = Context {
            subject: "Your Acme code",
            body: "Use 481516 to sign in. Account 99887766.",
            sim: None,
        };
        assert_eq!(
            score(otp, Some(otp), &Accepted::new(), &email).metrics["text_safe"],
            1.0
        );
        let mut link = otp.clone();
        link.notification.body = "Sign in at HTTPS://acme.example/login".into();
        let s = score(otp, Some(&link), &Accepted::new(), &email);
        assert_eq!(s.metrics["text_safe"], 0.0);
        assert_eq!(s.harm, HARM_TEXT_UNSAFE);
        let mut quoted = otp.clone();
        quoted.reason = "The code 481516 is fresh".into();
        assert_eq!(
            score(otp, Some(&quoted), &Accepted::new(), &email).metrics["text_safe"],
            0.0
        );
        // Its own extracted code, even where the email is not known.
        assert_eq!(
            score(otp, Some(&quoted), &Accepted::new(), &Context::default()).metrics["text_safe"],
            0.0
        );
        // A digit run from the email that gold never wrote, on auth mail.
        let mut account = otp.clone();
        account.notification.login_code = None;
        account.attention.summary = "Account 99887766 needs a look".into();
        assert_eq!(
            score(otp, Some(&account), &Accepted::new(), &email).metrics["text_safe"],
            0.0
        );
        // A number gold itself wrote is not a leak when the candidate does too.
        let mut gold_says = otp.clone();
        gold_says.attention.summary = "Account 99887766 needs a look".into();
        assert_eq!(
            score(&gold_says, Some(&account), &Accepted::new(), &email).metrics["text_safe"],
            1.0
        );
        let mut location = otp.clone();
        location.auth.evidence[0].location = "https://acme.example".into();
        assert_eq!(
            score(otp, Some(&location), &Accepted::new(), &email).metrics["text_safe"],
            0.0
        );
        // Not on ordinary mail: a year in the body is just a year.
        let letter = &golds()[2];
        let mut dated = letter.clone();
        dated.summary = "The 2026 issue arrived".into();
        let newsletter_email = Context {
            subject: "Issue 12",
            body: "Welcome to the 2026 edition.",
            sim: None,
        };
        assert_eq!(
            score(letter, Some(&dated), &Accepted::new(), &newsletter_email).metrics["text_safe"],
            1.0
        );
        assert_eq!(
            digit_runs("a 123 b 1234 c 123456789 d 12345678"),
            vec!["1234", "12345678"]
        );
    }

    #[test]
    fn style_counts_em_dashes_and_characters_in_user_text_only() {
        let mut d = MessageDecision {
            summary: "Bill due soon".into(),
            ..Default::default()
        };
        d.notification.body = "Pay it".into();
        assert_eq!(style(&d), (false, 19));
        d.reason = "internal \u{2014} not shown".into();
        assert!(!style(&d).0, "the reason is not user-facing");
        d.attention.actions.push(AttentionAction {
            description: "Pay \u{2014} today".into(),
            ..Default::default()
        });
        assert_eq!(style(&d), (true, 30));
    }

    struct Panics;
    impl TextSimilarity for Panics {
        fn similarity(&self, _: &str, _: &str) -> f64 {
            panic!("equal text must not reach the embedder")
        }
    }

    #[test]
    fn text_similarity_metrics() {
        assert_eq!(text_similarity(&Panics, "a  b\n", " a b"), 1.0);
        assert_eq!(text_similarity(&FakeSim, "", "word"), 0.0);
        let gold = &golds()[0];
        let mut rephrased = gold.clone();
        rephrased.summary = "Electric bill".into();
        rephrased.notification.title = "different".into();
        let s = plain(gold, Some(&rephrased));
        assert!((s.metrics["summary_sim"] - 2.0 / 3.0).abs() < 1e-9);
        assert!((s.metrics["push_text_sim"] - 0.5).abs() < 1e-9);
        // Record text: a dropped location scores 0 for that field only.
        let mut no_location = gold.clone();
        if let RecordProposal::Event { location, .. } = &mut no_location.records[2] {
            *location = None;
        }
        assert!(
            (plain(gold, Some(&no_location)).metrics["record_text_sim"] - 3.0 / 4.0).abs() < 1e-9
        );
        // The revisit and rule-exception reasons are part of reason_sim.
        let mut reasons = gold.clone();
        reasons.revisit.as_mut().unwrap().reason = "unrelated".into();
        assert!((plain(gold, Some(&reasons)).metrics["reason_sim"] - 4.0 / 5.0).abs() < 1e-9);
    }

    #[test]
    fn harm_follows_the_table() {
        let gold = &golds()[0];
        let mut buried = gold.clone();
        buried.attention.show_in_fye = false;
        assert_eq!(plain(gold, Some(&buried)).harm, 10.0);
        let otp = &golds()[1];
        let mut open = otp.clone();
        open.external_access.restricted = false;
        assert_eq!(plain(otp, Some(&open)).harm, 10.0);
        let mut noisy = golds()[2].clone();
        noisy.attention.show_in_fye = true;
        noisy.notification.importance = 90;
        assert_eq!(plain(&golds()[2], Some(&noisy)).harm, 4.0);
        let mut calmer = gold.clone();
        calmer.attention.factors.urgency = 0.3;
        let delta = plain(gold, Some(&calmer)).rank_delta.unwrap();
        assert!((delta - 15.0).abs() < 1e-9);
    }

    #[test]
    fn adjudication_flips_exactly_the_ruled_field() {
        let gold = &golds()[2];
        let mut candidate = gold.clone();
        candidate.attention.show_in_fye = true;
        candidate.attention.state = AttentionState::NeedsUser;
        let before = plain(gold, Some(&candidate));
        assert_eq!(before.metrics["fye"], 0.0);
        assert_eq!(before.metrics["state"], 0.0);
        let ruled = Accepted::from([("fye".to_owned(), vec![json!(true)])]);
        let after = score(gold, Some(&candidate), &ruled, &ctx());
        assert_eq!(after.metrics["fye"], 1.0);
        assert_eq!(after.directional["fye"], (true, true));
        assert_eq!(after.harm, before.harm - HARM_FYE_FALSE_ALARM);
        for (metric, value) in &before.metrics {
            if metric != "fye" {
                assert_eq!(after.metrics[metric], *value, "{metric}");
            }
        }
        let other = Accepted::from([("fye".to_owned(), vec![json!(false)])]);
        assert_eq!(score(gold, Some(&candidate), &other, &ctx()), before);
        // A ruled login code: agreement, no harm, and the positive class moves.
        let otp = &golds()[1];
        let mut none = otp.clone();
        none.notification.login_code = None;
        let ruled = Accepted::from([("login_code".to_owned(), vec![Value::Null])]);
        let s = score(otp, Some(&none), &ruled, &ctx());
        assert_eq!(s.metrics.get("login_code"), Some(&1.0));
        assert_eq!(s.directional["login_code"], (false, false));
    }

    #[test]
    fn every_emitted_field_is_scored_or_says_why_not() {
        let leaves = schema_leaves(&squelch_core::triage::agent::decision_schema());
        for expected in [
            "attention.factors.urgency",
            "records[bill].autopay",
            "notification.login_code.code",
            "attention.actions[].due.value",
            "related_updates[].attention.actions[].description",
            "kinds",
        ] {
            assert!(
                leaves.iter().any(|l| l == expected),
                "{expected} in {leaves:?}"
            );
        }
        let (missing, unknown) = coverage_gaps(&leaves, COVERAGE);
        assert!(missing.is_empty(), "unscored and unexplained: {missing:?}");
        assert!(unknown.is_empty(), "not metrics: {unknown:?}");
        // Dropping any single exact entry leaves a leaf unaccounted for.
        for (index, (key, _)) in COVERAGE.iter().enumerate() {
            if key.contains('*') {
                continue;
            }
            let fewer: Vec<(&str, Coverage)> = COVERAGE
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != index)
                .map(|(_, (k, c))| {
                    (
                        *k,
                        match c {
                            Coverage::Metrics(m) => Coverage::Metrics(m),
                            Coverage::Unscored(r) => Coverage::Unscored(r),
                        },
                    )
                })
                .collect();
            let (missing, _) = coverage_gaps(&leaves, &fewer);
            assert_eq!(missing, vec![(*key).to_owned()], "{key}");
        }
        // And a misspelt metric id is caught.
        let (_, unknown) =
            coverage_gaps(&leaves, &[("summary", Coverage::Metrics(&["sumary_sim"]))]);
        assert_eq!(unknown, vec!["sumary_sim".to_owned()]);
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
