//! The sync engine's two touch points with the owner profile
//! (docs/UNDERSTANDING.md §2.3): the memory lines a triage run is handed, and
//! the nightly profile pass. Everything else lives in `triage::memory` and
//! `triage::profile`; this file only supplies what the engine owns (the store,
//! the feature set, the LLM connection, the clock).

use super::*;
use crate::triage::context::{ContextScope, MemoryContextProvider, MemoryEntry};
use crate::triage::memory::StoreMemoryContext;
use crate::triage::profile::{self, LlmProfileModel, PassOutcome};

impl<S: Store + 'static, C: CredentialStore + 'static + ?Sized> SyncEngine<S, C> {
    /// The learned-memory lines for one triage run over mail from `sender` in
    /// `thread_id`. `understanding` is the flag as the run read it, ONCE, for
    /// the prompt variant too: nothing when it is false, whatever the live
    /// flag says by now. Nothing on a store error either: a run without its
    /// memory is an ordinary run, never a failed job.
    ///
    /// Accepted: the eval runner builds its input with `snapshot()` alone, so it gets no memory lines.
    pub(super) fn memory_for(
        &self,
        thread_id: &str,
        sender: &str,
        understanding: bool,
    ) -> Vec<MemoryEntry> {
        let now = Utc::now();
        let zone = self.store.owner_zone(self.account_id).unwrap_or_default();
        let provider = StoreMemoryContext {
            store: &*self.store,
            enabled: understanding,
            now,
            today: now.with_timezone(&zone.zone).date_naive(),
        };
        provider
            .relevant_memory(&ContextScope {
                account_id: self.account_id,
                thread_id: thread_id.to_string(),
                sender: sender.to_string(),
            })
            .unwrap_or_else(|e| {
                eprintln!("squelch: memory context unavailable ({e}); triaging without it");
                Vec::new()
            })
    }

    /// One look at the profile pass, from the refine lane. Cheap when it is
    /// not due (two `app_settings` reads), and at most one model call a day
    /// when it is: every gate is `profile::run_pass`'s.
    ///
    /// IN THE REFINE LANE, NOT BESIDE IT. Once a day it holds one round for
    /// one model call; a lane of its own would be another select arm in
    /// `run_lanes` for a task that is idle every other minute of the day.
    pub(super) async fn profile_pass(&self) {
        if !self.features.enabled(crate::features::UNDERSTANDING) || !self.refine_can_spend() {
            return;
        }
        let Some(llm) = &self.stage2_llm else {
            return;
        };
        // The investigation model, qualified for a gateway exactly as the
        // triage worker qualifies it, so a tenant's allow-list that admits
        // triage admits this.
        let base = self
            .config
            .triage
            .agent
            .model
            .clone()
            .unwrap_or_else(|| self.config.stage2.model.clone());
        let model = if llm.provider == Stage2Provider::Anthropic
            && crate::triage::llm::is_gateway_url(&llm.url)
        {
            crate::triage::llm::qualify_gateway_model(&base).unwrap_or(base)
        } else {
            base
        };
        let model = LlmProfileModel {
            http: self.http.clone(),
            url: llm.url.clone(),
            api_key: llm.api_key.clone(),
            provider: llm.provider,
            model,
            effort: self.config.stage2.effort.clone(),
            timeout: Duration::from_secs(self.config.triage.agent.timeout_secs),
        };
        let zone = self.store.owner_zone(self.account_id).unwrap_or_default();
        match profile::run_pass(
            &*self.store,
            self.account_id,
            Utc::now(),
            zone,
            &self.features,
            &model,
        )
        .await
        {
            PassOutcome::Applied(report) => eprintln!(
                "squelch: profile pass: kept {}, revised {}, retired {}, added {}, rejected {}",
                report.kept,
                report.revised,
                report.retired,
                report.added,
                report.rejected.len()
            ),
            PassOutcome::Failed { kind } => {
                eprintln!("squelch: profile pass failed ({kind}); retries within today's cap")
            }
            _ => {}
        }
    }
}

/// The input a tainted run is re-run on: the same snapshot with the
/// understanding flag off for this job alone, so the flag-off prompt, and no
/// memory lines.
///
/// A run is TAINTED when its decision repeats a served memory line verbatim
/// (`triage::memory::memory_taints`, six words in a row): summaries, reasons
/// and notification text all cross to /mcp, and the prompt sentence alone is
/// what would otherwise keep the owner's profile off the agent door. It is
/// re-run once on this and that result commits, whatever it says; there is
/// no second re-run, and the job is never failed over it.
///
/// EVERYTHING THE FLAG ADDS TO A RUN'S INPUT IS STRIPPED HERE, in one place:
/// a later block gated on the flag owes a line in this function. Today that
/// is the memory lines and the relationship evidence's block in the initial
/// context (`triage_worker::attach_relationship`). The flag's one other
/// addition is not input but a tool: `read_sender_history`'s relationship
/// block, which the re-run site switches off on the evidence reader
/// (`StoreEvidence::withhold_relationship`) beside resetting its sources.
pub(super) fn scrubbed(
    input: &crate::triage::context::ContextSnapshot,
) -> crate::triage::context::ContextSnapshot {
    let mut scrubbed = input.clone();
    scrubbed.understanding = false;
    scrubbed.memory.clear();
    if let Some(initial) = scrubbed.initial.as_object_mut() {
        initial.remove("relationship");
    }
    scrubbed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SqliteStore;
    use crate::triage::memory::{MemoryRecord, Origin, Provenance, new_ulid};

    struct NoToken;

    #[async_trait::async_trait]
    impl CredentialStore for NoToken {
        async fn token(&self, _account: AccountId) -> Result<crate::credentials::OAuthToken> {
            Err(crate::error::CoreError::NotFound)
        }
    }

    /// The flag as the run read it decides, not the live set: with the live
    /// flag ON and a stored line that would be served, a run that read the
    /// flag as off gets no memory at all.
    #[test]
    fn a_run_that_read_the_flag_off_gets_no_memory() {
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let acct = store.ensure_account("me@example.com").unwrap();
        let now = Utc::now();
        store
            .insert_memory_entry_unchecked(
                acct,
                &MemoryRecord {
                    id: new_ulid(now),
                    scope: "owner".into(),
                    origin: Origin::Stated,
                    kind: "role".into(),
                    body: serde_json::json!({"what": "Runs a studio"}),
                    line: "Runs a studio.".into(),
                    provenance: Provenance::default(),
                    confidence: 1.0,
                    revision: 1,
                    created_at: now,
                    updated_at: now,
                    affirmed_at: now,
                    expires_at: None,
                    retired_at: None,
                    deleted_at: None,
                },
            )
            .unwrap();
        let engine = SyncEngine::new(
            store,
            Arc::new(NoToken),
            acct,
            "me@example.com".into(),
            Config::default(),
        )
        .with_features(crate::features::Features::pinned([
            crate::features::UNDERSTANDING,
        ]));
        assert_eq!(engine.memory_for("t", "sam@acme.example", true).len(), 1);
        assert!(engine.memory_for("t", "sam@acme.example", false).is_empty());
    }
}
