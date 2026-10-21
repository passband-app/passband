//! THE TRANSLATE PASS (docs/TRANSLATION.md, "Pipeline"): its own lane beside
//! the refine lane, so a long translation never holds up triage of new mail.
//! Triage holds back only the message whose translation is pending, for a
//! bounded wait, so foreign mail is normally triaged once, on the English.
//!
//! One round claims a small batch of `pending` (or backed-off `failed`) rows,
//! translates them concurrently, bills every usage the provider reported to
//! [`crate::translate::USAGE_CATEGORY`] (a failed call still spent tokens,
//! and a call that reported none, a timeout, is still counted as a call),
//! records the result over the claim's own lease, and puts the English into
//! the search index: the FTS row is rewritten by the store in the same
//! transaction as the result, and the translation's vector is embedded here,
//! off the store lock, only when the result was actually written.
//!
//! A FAILURE THAT IS NOT ABOUT THE MESSAGE STOPS THE PASS. A rejected key, an
//! exhausted budget, a model the gateway's allow-list does not carry, and
//! equally a provider outage (5xx, 429, 529, a timeout): every other row
//! would fail the same way, and a row marked failed for it would spend one of
//! its bounded attempts on a fact about the deployment rather than about the
//! mail. Those rows are RELEASED (their attempt and their daily-cap charge
//! handed back, their backoff kept) and the pass cools down: for
//! `triage.agent.outage_retry_secs` on a configuration failure, for
//! [`OUTAGE_COOLDOWN_SECS`] on an outage. The cooldown is this pass's own: a
//! translate model missing from an allow-list says nothing about the triage
//! model, and must not stop triage.
//!
//! THE DAILY CAP (`[translate].daily_cap`) counts messages sent to the
//! translator. Over it, queued rows wait for the next UTC day.

use super::*;
use crate::store::{TranslationJob, TranslationWrite};
use crate::translate::{self, TranslateInput, TranslateOutcome, TranslateRun};

/// Rows one round claims. Small on purpose: a round's results land together,
/// and four concurrent calls bound one account's share of the gateway.
pub(super) const TRANSLATE_BATCH: usize = 4;
/// The pass's cooldown after a provider outage (5xx, 429, 529, timeout):
/// shorter than a configuration failure's, because an outage usually clears
/// by itself.
pub(super) const OUTAGE_COOLDOWN_SECS: i64 = 120;

impl<S: Store + 'static, C: CredentialStore + 'static + ?Sized> SyncEngine<S, C> {
    /// THE TRANSLATE LANE: rounds of [`Self::translate_pass`] back to back
    /// while there is work, then parked until the refine lane's next round
    /// pokes it (`translate_wake`) or the interval floor passes. No `Result`
    /// for the refine lane's reason. Every gate is inside the pass.
    pub(super) async fn translate_lane(&self, shutdown: &mut tokio::sync::watch::Receiver<bool>) {
        let interval = Duration::from_secs(self.config.triage.agent.worker_poll_secs);
        loop {
            if *shutdown.borrow() {
                return;
            }
            if self.translator.is_some() && self.translate_pass().await {
                // Translations landed and queued their re-triage: the refine
                // lane may be asleep on its idle backoff.
                self.refine_wake.notify_one();
                continue;
            }
            tokio::select! {
                _ = self.translate_wake.notified() => {}
                _ = tokio::time::sleep(interval.max(Duration::from_secs(1)).min(Duration::from_secs(30))) => {}
                _ = shutdown.changed() => {
                    if *shutdown.borrow() { return; }
                }
            }
        }
    }

    /// One round of the translate pass. `true` when it claimed anything.
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
        let day = now.format("%Y-%m-%d").to_string();
        let cap = self.config.translate.daily_cap;
        let room = match self
            .store
            .stage2_budget_used(self.account_id, TRANSLATE_BUDGET_KEY, &day)
        {
            Ok(used) if used >= cap => {
                if self.translate_budget().warn_once(CapKind::Translate, &day) {
                    eprintln!(
                        "squelch: translate daily budget exhausted ({used}/{cap}); \
                         queued translations wait for tomorrow"
                    );
                }
                return false;
            }
            Ok(used) => (cap - used) as usize,
            Err(e) => {
                eprintln!("squelch: translate budget read failed ({e}); skipping round");
                return false;
            }
        };
        let jobs =
            match self
                .store
                .claim_translations(self.account_id, now, TRANSLATE_BATCH.min(room))
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
        // CHARGED BEFORE THE CALL, the cap's usual rule, so a retry storm
        // cannot run past it. A row the store cannot charge is released.
        let mut charged = Vec::with_capacity(jobs.len());
        for job in jobs {
            match self
                .store
                .stage2_increment_budget(self.account_id, TRANSLATE_BUDGET_KEY, &day)
            {
                Ok(_) => charged.push(job),
                Err(e) => {
                    eprintln!("squelch: translate budget increment failed ({e}); releasing row");
                    let _ = self
                        .store
                        .record_translation(&job, &TranslationWrite::Released, now);
                }
            }
        }
        if charged.is_empty() {
            // Nothing went out: not "work done", or a store that cannot
            // write would spin the lane.
            return false;
        }
        let runs = futures::future::join_all(charged.iter().map(|job| {
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
        for (job, run) in charged.iter().zip(runs) {
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
        // still cost what it cost. A call that reported no usage (a timeout,
        // a transport error) is still a call, and is counted as one.
        let unreported = run.model_calls.saturating_sub(run.usage.len());
        let tokens = run
            .usage
            .into_iter()
            .map(crate::store::UsageTokens::from)
            .chain(std::iter::repeat_n(
                crate::store::UsageTokens::default(),
                unreported,
            ));
        for tokens in tokens {
            if let Err(e) = self.store.extract_bump_usage(
                self.account_id,
                day,
                translate::USAGE_CATEGORY,
                tokens,
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
            TranslateOutcome::Rejected(reason) => {
                // Redacted: the id and the engine's reason code, never text.
                eprintln!(
                    "squelch: translation rejected for message {} ({reason})",
                    job.message_id
                );
                TranslationWrite::Rejected {
                    model,
                    prompt_version: translate::PROMPT_VERSION,
                }
            }
            TranslateOutcome::Failed(kind) if crate::triage::llm::is_config_failure(kind) => {
                self.metrics.record_llm_config_failure();
                let secs = self.config.triage.agent.outage_retry_secs.min(86_400) as i64;
                self.translate_cool_down(now, secs, "rejected the request", kind);
                self.translate_budget()
                    .refund(TRANSLATE_BUDGET_KEY, day, "translate pass");
                TranslationWrite::Released
            }
            TranslateOutcome::Failed(kind) if super::triage_worker::is_provider_outage(kind) => {
                self.translate_cool_down(now, OUTAGE_COOLDOWN_SECS, "is unavailable", kind);
                self.translate_budget()
                    .refund(TRANSLATE_BUDGET_KEY, day, "translate pass");
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
        let wrote = match self.store.record_translation(job, &write, now) {
            Ok(wrote) => wrote,
            Err(e) => {
                eprintln!(
                    "squelch: translation for message {} not recorded ({e})",
                    job.message_id
                );
                return;
            }
        };
        // A result that wrote nothing (the row was healed and re-queued, or
        // re-claimed after the lease) must not get a vector either.
        if wrote && let TranslateOutcome::Translated(t) = &run.outcome {
            let text =
                crate::embed::message_embed_text(&t.subject, &t.body, self.config.embed.max_chars);
            self.embed_translation(job.message_id, text).await;
        }
    }

    /// The account's daily-budget ledger, for the translate cap.
    fn translate_budget(&self) -> BudgetLedger<'_, S> {
        BudgetLedger {
            store: &*self.store,
            account_id: self.account_id,
            warn_days: &self.warn_days,
        }
    }

    /// Start (or extend) the pass's own cooldown, logging once per cooldown
    /// rather than once per row of the batch.
    fn translate_cool_down(&self, now: DateTime<Utc>, secs: i64, what: &str, kind: &str) {
        let until = now.timestamp() + secs;
        if self
            .translate_retry_after
            .fetch_max(until, Ordering::Relaxed)
            < now.timestamp()
        {
            eprintln!(
                "squelch: translate provider {what} ({kind}); \
                 queued translations retained, retrying in {secs}s"
            );
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
