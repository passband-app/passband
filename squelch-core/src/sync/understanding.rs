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
    /// `thread_id`. Nothing while the `understanding` feature is off, and
    /// nothing on a store error: a run without its memory is an ordinary run,
    /// never a failed job.
    pub(super) fn memory_for(&self, thread_id: &str, sender: &str) -> Vec<MemoryEntry> {
        let now = Utc::now();
        let zone = self.store.owner_zone(self.account_id).unwrap_or_default();
        let provider = StoreMemoryContext {
            store: &*self.store,
            features: &self.features,
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
