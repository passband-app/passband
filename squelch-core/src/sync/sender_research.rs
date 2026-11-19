//! The sync engine's touch point with sender research
//! (docs/SENDER-RESEARCH.md): one automatic round per refine round, at most
//! one call, under the daily automatic cap. Everything else is
//! `triage::research`.

use super::*;
use crate::triage::research::{self, AutoRound, ResearchClient};

impl<S: Store + 'static, C: CredentialStore + 'static + ?Sized> SyncEngine<S, C> {
    /// Cheap when idle (one budget read and one claim query), and gated like
    /// every other model spend in the refine lane.
    pub(super) async fn sender_research_pass(&self) {
        if !self.config.research.enabled || !self.refine_can_spend() {
            return;
        }
        match research::auto_round(
            &*self.store,
            &self.config.research,
            self.account_id,
            || ResearchClient::from_config(&self.config),
            Utc::now(),
        )
        .await
        {
            // The address is a stranger's: not logged.
            Ok(AutoRound::Ran { status, .. }) => {
                eprintln!("squelch: sender research ran ({})", status.as_str())
            }
            Ok(_) => {}
            Err(e) => eprintln!("squelch: sender research round failed ({e})"),
        }
    }
}
