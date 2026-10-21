//! THE TRANSLATE PASS (docs/TRANSLATION.md, "Pipeline"): the refine lane's
//! first job each round, before agent triage, so triage usually sees the
//! English beside the original. Triage never waits for it.
//!
//! One round claims a small batch of `pending` (or backed-off `failed`) rows,
//! translates them concurrently, bills every usage the provider reported to
//! [`crate::translate::USAGE_CATEGORY`] (a failed call still spent tokens),
//! records the result, and puts the English into the search index: the FTS
//! row is rewritten by the store in the same transaction as the result, and
//! the translation's vector is embedded here, off the store lock.
//!
//! A CONFIGURATION FAILURE STOPS THE PASS. A rejected key, an exhausted
//! budget, a model the gateway's allow-list does not carry: every other row
//! would fail the same way, and a row marked failed for it would spend one of
//! its bounded attempts on a fact about the deployment rather than about the
//! mail. Those rows are RELEASED (their attempt handed back) and the pass
//! cools down for `triage.agent.outage_retry_secs`. The cooldown is this
//! pass's own: a translate model missing from an allow-list says nothing
//! about the triage model, and must not stop triage.

use super::*;
use crate::store::{TranslationJob, TranslationWrite};
use crate::translate::{self, TranslateInput, TranslateOutcome, TranslateRun};

/// Rows one round claims. Small on purpose: the round runs ahead of triage on
/// the same lane, and four concurrent calls bound how long new mail waits
/// behind them.
pub(super) const TRANSLATE_BATCH: usize = 4;

impl<S: Store + 'static, C: CredentialStore + 'static + ?Sized> SyncEngine<S, C> {
    /// One round of the translate pass. `true` when it claimed anything, which
    /// is what keeps the refine lane from backing off while work is queued.
    pub(super) async fn translate_pass(&self) -> bool {
        let Some(backend) = self.translator.clone() else {
            return false;
        };
        // The refine lane's own gate (a poll has succeeded, a model is
        // configured, the triage circuit is closed), then this pass's.
        if !self.refine_can_spend() {
            return false;
        }
        let now = Utc::now();
        if now.timestamp() < self.translate_retry_after.load(Ordering::Relaxed) {
            return false;
        }
        let jobs = match self
            .store
            .claim_translations(self.account_id, now, TRANSLATE_BATCH)
        {
            Ok(jobs) => jobs,
            Err(e) => {
                eprintln!("squelch: translate claim failed ({e})");
                return false;
            }
        };
        if jobs.is_empty() {
            return false;
        }
        let day = now.format("%Y-%m-%d").to_string();
        let runs = futures::future::join_all(jobs.iter().map(|job| {
            let backend = backend.clone();
            async move {
                translate::translate_message(
                    backend.as_ref(),
                    TranslateInput {
                        subject: &job.subject,
                        body: &job.body,
                        body_html: job.body_html.as_deref(),
                    },
                )
                .await
            }
        }))
        .await;
        let model = backend.model().to_string();
        for (job, run) in jobs.iter().zip(runs) {
            self.finish_translation(job, run, &model, &day).await;
        }
        true
    }

    async fn finish_translation(
        &self,
        job: &TranslationJob,
        run: TranslateRun,
        model: &str,
        day: &str,
    ) {
        // BILLED FIRST, whatever the outcome: a refused or malformed answer
        // still cost what it cost.
        for usage in run.usage {
            if let Err(e) = self.store.extract_bump_usage(
                self.account_id,
                day,
                translate::USAGE_CATEGORY,
                usage.into(),
            ) {
                eprintln!("squelch: translate usage not recorded ({e})");
            }
        }
        let now = Utc::now();
        let write = match &run.outcome {
            TranslateOutcome::Translated(t) => TranslationWrite::Translated {
                translated: t,
                model,
                prompt_version: translate::PROMPT_VERSION,
            },
            TranslateOutcome::NotNeeded => TranslationWrite::NotNeeded {
                model,
                prompt_version: translate::PROMPT_VERSION,
            },
            TranslateOutcome::Failed(kind) if crate::triage::llm::is_config_failure(kind) => {
                self.metrics.record_llm_config_failure();
                let secs = self.config.triage.agent.outage_retry_secs.min(86_400) as i64;
                let until = now.timestamp() + secs;
                if self
                    .translate_retry_after
                    .fetch_max(until, Ordering::Relaxed)
                    < now.timestamp()
                {
                    // Once per cooldown, not once per row of the batch.
                    eprintln!(
                        "squelch: translate provider rejected the request ({kind}); \
                         queued translations retained, retrying in {secs}s"
                    );
                }
                TranslationWrite::Released
            }
            TranslateOutcome::Failed(kind) => {
                // Redacted: the id and the kind, never the text.
                eprintln!(
                    "squelch: translation failed for message {} ({kind}, attempt {})",
                    job.message_id, job.attempts
                );
                TranslationWrite::Failed { model }
            }
        };
        if let Err(e) = self.store.record_translation(job.message_id, &write, now) {
            eprintln!(
                "squelch: translation for message {} not recorded ({e})",
                job.message_id
            );
            return;
        }
        if let TranslateOutcome::Translated(t) = &run.outcome {
            let text =
                crate::embed::message_embed_text(&t.subject, &t.body, self.config.embed.max_chars);
            self.embed_translation(job.message_id, text).await;
        }
    }

    /// [`Self::embed_and_store`] for a translation's vector. A failure leaves
    /// `vectors_dirty` raised, and the drain covers translations too.
    async fn embed_translation(&self, message_id: i64, text: String) {
        let Some(embedder) = self.embedder() else {
            self.vectors_dirty.store(true, Ordering::Relaxed);
            return;
        };
        let account_id = self.account_id;
        let store = self.store.clone();
        let result = tokio::task::spawn_blocking(move || {
            let vec = embedder.embed(&text)?;
            store.upsert_translation_vector(account_id, message_id, &vec)
        })
        .await;
        match &result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!(
                "squelch: translation embed failed for message {message_id} (recoverable via backfill): {e}"
            ),
            Err(e) => eprintln!(
                "squelch: translation embed task join error for message {message_id}: {e}"
            ),
        }
        if !matches!(result, Ok(Ok(()))) {
            self.vectors_dirty.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests;
