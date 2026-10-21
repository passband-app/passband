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
//! mail. Those rows are RELEASED (their attempt handed back, their backoff
//! kept) and the pass cools down: for `triage.agent.outage_retry_secs` on a
//! configuration failure, for [`OUTAGE_COOLDOWN_SECS`] on an outage. The
//! cooldown is this pass's own: a translate model missing from an allow-list
//! says nothing about the triage model, and must not stop triage.
//!
//! UNLESS THE FAILURE IS THE MESSAGE'S OWN. Only a MESSAGE-SHAPED failure
//! can be: a timeout or transport error on this row's call, or a 400 its
//! content draws. When another row of the same round got an answer, the
//! gateway is fine and such a failure is charged to it as an ordinary
//! failure. And a row released `MAX_CONSECUTIVE_RELEASES` times in a row is
//! charged on the next one by the store, but only if something has answered
//! since that row's first release. For a timeout, any answer from the
//! gateway counts (`gateway_answer_ms`: a translation verdict, or a
//! successful agent triage run, [`SyncEngine::note_gateway_answer`]). For a
//! 400 only the TRANSLATE MODEL's own answers count (`translate_answer_ms`):
//! a 400 can be about the model (a gateway that cannot resolve a provider
//! for it, a parameter it refuses), and triage succeeding on another model
//! says nothing about that. In a total outage nothing answers, so every
//! row stays pending with its attempts untouched, however long it lasts.
//! A CAPACITY failure (429, 529, 503, any 5xx) and a key, budget or model
//! failure (401, 402, 403, 404) is never the message's: partial 429s beside
//! successes are the normal shape of a rate limit, so those always release
//! and cool down. The config failure counter
//! (`squelchd_translate_config_failures_total`, its own, never triage's)
//! counts a row's first release only.
//!
//! THE DAILY BUDGET (`[translate].daily_budget_usd`) is dollars, priced at
//! the translate prices from today's `translate` usage line. Each message's
//! cost is estimated before its call ([`translate::estimate_tokens`]) and
//! goes only if it fits what is left; the spend itself is settled from the
//! usage the provider reported. Over it, queued rows wait for the next UTC
//! day.

use super::*;
use crate::store::{TranslationJob, TranslationWrite};
use crate::translate::{self, TranslateInput, TranslateOutcome, TranslateRun};

/// A failure that is not about the message: a configuration failure or a
/// provider outage, which every other row would draw too.
fn failure_is_global(kind: &str) -> bool {
    crate::triage::llm::is_config_failure(kind) || super::triage_worker::is_provider_outage(kind)
}

/// A failure the message itself can draw: its own call timing out or
/// dropping (`transport`, a batch whose answer outruns the timeout), or a
/// 400 its content draws. Never a capacity failure (429, 5xx) and never a
/// key, budget or model failure (401, 402, 403, 404): those are about the
/// deployment whatever else answered. Any other 4xx is not global at all
/// and is already an ordinary failure.
fn failure_is_message_shaped(kind: &str) -> bool {
    kind == "transport" || kind == "http_400" || kind.starts_with("http_400:")
}

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
        let budget = self.config.translate.daily_budget_usd;
        let spent = match self.translate_spent_usd(&day) {
            Ok(spent) => spent,
            Err(e) => {
                eprintln!("squelch: translate budget read failed ({e}); skipping round");
                return false;
            }
        };
        if spent >= budget {
            self.translate_budget_exhausted(&day, spent);
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
        // ESTIMATED BEFORE THE CALL: a message goes only if its estimate
        // fits what today has left beside the others of this round. One that
        // does not is handed back untouched, for a later round or tomorrow.
        let mut reserved = 0.0;
        let mut charged = Vec::with_capacity(jobs.len());
        for job in jobs {
            let (tin, tout) = translate::estimate_tokens(TranslateInput {
                subject: &job.subject,
                body: &job.body,
                body_html: job.body_html.as_deref(),
            });
            let estimate = self.translate_price_usd(tin, tout, 0, 0);
            if spent + reserved + estimate <= budget {
                reserved += estimate;
                charged.push(job);
            } else if let Err(e) =
                self.store
                    .record_translation(&job, &TranslationWrite::Returned, now)
            {
                eprintln!("squelch: translate row not handed back ({e})");
            }
        }
        if charged.is_empty() {
            // Nothing fits today: as good as spent.
            self.translate_budget_exhausted(&day, spent);
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
        // A row whose call failed in a message-shaped way while another row
        // of this round got an answer: the gateway works, the failure is
        // this message's.
        let any_answer = runs.iter().any(|r| match &r.outcome {
            TranslateOutcome::Failed(kind) => !failure_is_global(kind),
            _ => true,
        });
        if any_answer {
            // Stamped before the rows are recorded, so a row released in
            // this same round (its first release later than this) is not
            // charged by the cap on the strength of it.
            self.note_translate_answer();
        }
        for (job, run) in charged.iter().zip(runs) {
            self.finish_translation(job, run, &model, &day, any_answer)
                .await;
        }
        true
    }

    /// Today's translate spend in dollars, from the usage the provider
    /// reported for the `translate` category, at the translate prices.
    fn translate_spent_usd(&self, day: &str) -> crate::error::Result<f64> {
        let u = self
            .store
            .category_usage_since(self.account_id, day, translate::USAGE_CATEGORY)?;
        Ok(self.translate_price_usd(
            u.input_tokens,
            u.output_tokens,
            u.cache_creation_tokens,
            u.cache_read_tokens,
        ))
    }

    /// Tokens priced at `[translate]`'s rates, cache tokens the way
    /// `metrics::estimate_cost_usd` prices them.
    fn translate_price_usd(&self, input: u64, output: u64, cache_w: u64, cache_r: u64) -> f64 {
        let (pin, pout) = (
            self.config.translate.price_in_per_mtok,
            self.config.translate.price_out_per_mtok,
        );
        (input as f64 * pin
            + output as f64 * pout
            + cache_w as f64 * pin * crate::config::CACHE_WRITE_INPUT_MULT
            + cache_r as f64 * pin * crate::config::CACHE_READ_INPUT_MULT)
            / 1_000_000.0
    }

    /// Log the day's budget as spent, once per UTC day. The same stamp tells
    /// ingest the translator has nothing left today ([`Self::translate_over_budget`]).
    fn translate_budget_exhausted(&self, day: &str, spent: f64) {
        if self.translate_budget().warn_once(CapKind::Translate, day) {
            eprintln!(
                "squelch: translate daily budget exhausted (${spent:.2} of ${:.2}); \
                 queued translations wait for tomorrow",
                self.config.translate.daily_budget_usd
            );
        }
    }

    /// Whether the translator has no budget left today: the pass found it
    /// spent, or today's billed usage already reaches it. Ingest asks it of a
    /// non-English message, to queue it without holding its triage (and a
    /// backfilled one `deferred`). A ledger it cannot read says no.
    pub(super) fn translate_over_budget(&self, now: DateTime<Utc>) -> bool {
        let day = now.format("%Y-%m-%d").to_string();
        let stamped = self
            .warn_days
            .lock()
            .map(|g| g.translate.as_deref() == Some(day.as_str()))
            .unwrap_or(false);
        stamped
            || self
                .translate_spent_usd(&day)
                .is_ok_and(|spent| spent >= self.config.translate.daily_budget_usd)
    }

    async fn finish_translation(
        &self,
        job: &TranslationJob,
        run: TranslateRun,
        model: &str,
        day: &str,
        any_answer: bool,
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
            TranslateOutcome::Failed(kind) if failure_is_message_shaped(kind) && any_answer => {
                // The gateway answered another row this round: this failure
                // is the message's own, an ordinary failure with backoff.
                eprintln!(
                    "squelch: translation failed for message {} ({kind}, attempt {}; \
                     other rows translated, so not a provider fault)",
                    job.message_id, job.attempts
                );
                TranslationWrite::Failed { model }
            }
            TranslateOutcome::Failed(kind) if crate::triage::llm::is_config_failure(kind) => {
                // Counted once per run of releases, not once per cycle: a
                // row the store is about to charge has been counted already.
                if job.releases == 0 {
                    self.metrics.record_translate_config_failure();
                }
                let secs = self.config.triage.agent.outage_retry_secs.min(86_400) as i64;
                self.translate_cool_down(now, secs, "rejected the request", kind);
                self.translate_release(kind)
            }
            TranslateOutcome::Failed(kind) if super::triage_worker::is_provider_outage(kind) => {
                self.translate_cool_down(now, OUTAGE_COOLDOWN_SECS, "is unavailable", kind);
                self.translate_release(kind)
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

    /// THE GATEWAY ANSWERED this account: a translation call reached any
    /// verdict, or an agent triage run (or access assessment) succeeded on
    /// the same gateway and key. The release cap charges a row only when
    /// this is later than the row's first release, so a lone row that times
    /// out every time on a healthy gateway runs out of attempts (each
    /// abandoned generation is billed by the provider and recorded as a
    /// zero-token call), while a true outage, where nothing answers, spends
    /// nothing.
    pub(super) fn note_gateway_answer(&self) {
        self.gateway_answer_ms
            .fetch_max(Utc::now().timestamp_millis(), Ordering::Relaxed);
    }

    /// THE TRANSLATE MODEL ANSWERED: a translation call reached a verdict.
    /// Also a gateway answer.
    fn note_translate_answer(&self) {
        self.translate_answer_ms
            .fetch_max(Utc::now().timestamp_millis(), Ordering::Relaxed);
        self.note_gateway_answer();
    }

    /// Hand a claim back. Only a message-shaped failure carries a last
    /// answer, so only it can be charged by the release cap: a timeout the
    /// gateway's last answer (triage included), a 400 the translate model's
    /// own. A capacity, key, budget or model failure never is.
    fn translate_release(&self, kind: &str) -> TranslationWrite<'static> {
        let last = if kind == "transport" {
            self.gateway_answer_ms.load(Ordering::Relaxed)
        } else if failure_is_message_shaped(kind) {
            self.translate_answer_ms.load(Ordering::Relaxed)
        } else {
            0
        };
        TranslationWrite::Released {
            last_answer_ms: (last > 0).then_some(last),
        }
    }

    /// The account's daily-budget ledger: its once-a-day warning slot.
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
