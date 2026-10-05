//! One eval case: an inbound email, the context the agent saw as of its gold
//! run, and the stored decision that run produced.
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use squelch_core::triage::context::ContextSnapshot;
use squelch_core::triage::decision::{EmailKind, MessageDecision, MessageDestination};
use squelch_core::triage::llm::Usage;
use std::path::Path;

/// How faithfully the case's context reproduces what the gold run saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fidelity {
    /// A lone message with no prior attention, and nothing the as-of filter
    /// had to drop: the reconstruction is the original.
    Exact,
    /// Rebuilt from timestamps (siblings, attention, user state).
    Reconstructed,
    /// Known lossy: an earlier decision in the thread is gone, or the rebuilt
    /// prompt's size disagrees with the tokens the gold run was billed for.
    Suspect,
}

impl Fidelity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Reconstructed => "reconstructed",
            Self::Suspect => "suspect",
        }
    }
}

/// The gold run's ledger entry, from `agent_triage_runs.metadata_json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldMeta {
    pub model: String,
    pub prompt_version: String,
    pub model_turns: u64,
    pub tool_calls: u64,
    /// Per call, in call order. `usage[0].input_tokens` is the uncached
    /// remainder of the first call, which is the first user message.
    pub usage: Vec<Usage>,
    pub decided_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Case {
    /// The target message id.
    pub id: i64,
    pub thread_id: String,
    /// T: the gold run's `completed_at`. Everything in `context` is as of T.
    pub as_of: DateTime<Utc>,
    pub trigger: String,
    pub fidelity: Fidelity,
    pub fidelity_notes: Vec<String>,
    /// `tags[0]` is the stratum ([`STRATA`]); then every other label that
    /// applies, in [`STRATA`] order, then `thread` for a multi-message thread.
    pub tags: Vec<String>,
    pub context: ContextSnapshot,
    pub gold: MessageDecision,
    pub gold_meta: GoldMeta,
    pub subject: String,
    pub from_addr: String,
}

/// Strata in precedence order: a case's stratum is the first that applies.
pub const STRATA: [&str; 8] = [
    "fye",
    "auth",
    "restricted",
    "push",
    "records",
    "correspondence",
    "reading",
    "other",
];

/// Production's deliberate-push rule (`NotifyConfig::min_importance`).
pub fn is_push(decision: &MessageDecision, min_importance: u8) -> bool {
    decision.auth.is_auth() || decision.notification.importance >= min_importance
}

/// Every label in [`STRATA`] that applies to `gold`, in order; `other` only
/// when nothing else does.
pub fn labels(gold: &MessageDecision, min_importance: u8) -> Vec<&'static str> {
    let mut out = Vec::new();
    for label in STRATA {
        let applies = match label {
            "fye" => gold.attention.show_in_fye,
            "auth" => gold.auth.is_auth(),
            "restricted" => gold.external_access.restricted,
            "push" => is_push(gold, min_importance),
            "records" => !gold.records.is_empty(),
            "correspondence" => gold.kinds.contains(&EmailKind::Correspondence),
            "reading" => gold.destinations.contains(&MessageDestination::Reading),
            _ => out.is_empty(),
        };
        if applies {
            out.push(label);
        }
    }
    out
}

pub fn tags(gold: &MessageDecision, min_importance: u8, multi_message_thread: bool) -> Vec<String> {
    let mut out: Vec<String> = labels(gold, min_importance)
        .into_iter()
        .map(str::to_owned)
        .collect();
    if multi_message_thread {
        out.push("thread".into());
    }
    out
}

/// The integer in a prompt version like `agent-triage-v4`.
pub fn prompt_version_number(version: &str) -> Option<u32> {
    version.rsplit_once('v').and_then(|(_, n)| n.parse().ok())
}

/// Condensation of the initial context shipped with prompt v4.
pub const CONDENSED_FROM_VERSION: u32 = 4;

pub fn read_cases(path: &Path) -> Result<Vec<Case>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line)
                .with_context(|| format!("{} line {}", path.display(), index + 1))
        })
        .collect()
}

/// The first user message `run_agent` sends for `context` (turn 0 of
/// `run_bounded`), with `initial` already prepared.
pub fn first_user_message(context: &ContextSnapshot) -> Result<String> {
    Ok(serde_json::to_string(&serde_json::json!({
        "context": context, "investigation": [], "must_finish": false
    }))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use squelch_core::triage::decision::{AuthAssessment, AuthKind};

    #[test]
    fn stratum_is_the_first_label_and_other_only_stands_alone() {
        let mut gold = MessageDecision::default();
        assert_eq!(tags(&gold, 50, false), vec!["other"]);
        gold.destinations = vec![MessageDestination::Reading];
        gold.kinds = vec![EmailKind::Correspondence];
        assert_eq!(
            tags(&gold, 50, true),
            vec!["correspondence", "reading", "thread"]
        );
        gold.auth = AuthAssessment {
            kinds: vec![AuthKind::Otp],
            evidence: vec![],
        };
        // Auth always pushes.
        assert_eq!(
            tags(&gold, 50, false),
            vec!["auth", "push", "correspondence", "reading"]
        );
        gold.attention.show_in_fye = true;
        assert_eq!(tags(&gold, 50, false)[0], "fye");
        assert_eq!(prompt_version_number("agent-triage-v12"), Some(12));
    }
}
