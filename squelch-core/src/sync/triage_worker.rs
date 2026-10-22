//! Durable model work. Ingest never waits for these decisions.
use super::*;
use crate::metrics::AgentVerdict;
use crate::store::agent_triage::{AgentCommitOutcome, AgentContext, AgentJob, AgentSourceSnapshot};
use crate::triage::agent::{AgentConnection, run_agent};
use crate::triage::context::{ContextSnapshot, EvidenceReader, EvidenceRequest, EvidenceResult};
use std::collections::BTreeMap;

struct StoreEvidence<'a, S: Store> {
    store: &'a S,
    account: AccountId,
    limit: usize,
    sources: std::sync::Mutex<Vec<AgentSourceSnapshot>>,
}

fn read_job(account: AccountId, message_id: i64) -> AgentJob {
    AgentJob {
        id: 0,
        account_id: account,
        message_id,
        kind: "triage".into(),
        trigger: "context".into(),
        lease_token: String::new(),
        attempts: 0,
        arrival_eligible: false,
        foreground: false,
    }
}

impl<S: Store> EvidenceReader for StoreEvidence<'_, S> {
    fn read(&self, request: &EvidenceRequest) -> std::result::Result<EvidenceResult, String> {
        let result = self
            .read_inner(request)
            .map_err(|_| "evidence_unavailable".to_owned())?;
        Ok(result)
    }
}

impl<S: Store> StoreEvidence<'_, S> {
    fn read_inner(&self, request: &EvidenceRequest) -> Result<EvidenceResult> {
        let mut revisions = BTreeMap::new();
        let messages = match request {
            EvidenceRequest::ReadMessage { message_id, offset } => {
                self.store
                    .snapshot_agent_sources(self.account, &[*message_id])?;
                let context = self
                    .store
                    .load_agent_context(&read_job(self.account, *message_id))?;
                let message = &context.message;
                self.sources
                    .lock()
                    .expect("evidence sources")
                    .push(message.source.clone());
                revisions.insert(
                    message.thread_id.clone(),
                    context.revision.attention_revision,
                );
                let offset = offset.unwrap_or(0);
                // Character offsets keep Unicode boundaries stable across pages.
                let page: String = message.body.chars().skip(offset).take(2000).collect();
                let next = offset.saturating_add(page.chars().count());
                let more = message.body.chars().count() > next;
                let mut value = serde_json::to_value(message)
                    .map_err(|_| CoreError::InvalidInput("message serialization".into()))?;
                value["body"] = page.into();
                value["body_truncated"] = more.into();
                value["body_offset"] = offset.into();
                value["next_offset"] = if more {
                    next.into()
                } else {
                    serde_json::Value::Null
                };
                return Ok(EvidenceResult {
                    data: serde_json::json!({"message": value}),
                    source_message_ids: vec![*message_id],
                    thread_revisions: revisions,
                });
            }
            EvidenceRequest::ReadThread { thread_id } => {
                let context = self.store.agent_thread_context(self.account, thread_id)?;
                // Attention and its message snapshots must describe one atomic
                // read; mixing a previous decision with newer source revisions
                // would let stale evidence pass commit validation.
                let messages = context
                    .thread
                    .iter()
                    .take(self.limit)
                    .cloned()
                    .collect::<Vec<_>>();
                // Initialize exactly the sources exposed by this read, before
                // recording provenance. Keep the original atomic content snapshot.
                self.store.snapshot_agent_sources(
                    self.account,
                    &messages.iter().map(|m| m.id).collect::<Vec<_>>(),
                )?;
                self.sources
                    .lock()
                    .expect("evidence sources")
                    .extend(messages.iter().map(|m| m.source.clone()));
                revisions.insert(thread_id.clone(), context.revision.attention_revision);
                return Ok(EvidenceResult {
                    source_message_ids: messages.iter().map(|m| m.id).collect(),
                    data: serde_json::json!({"messages": messages, "attention": context.attention,
                        "user_corrections": context.corrections, "previous_decision": context.previous_decision}),
                    thread_revisions: revisions,
                });
            }
            EvidenceRequest::SearchMail { query } => {
                self.store
                    .agent_search_mail(self.account, query, self.limit)?
            }
            EvidenceRequest::ReadSenderHistory { sender } => {
                self.store
                    .agent_sender_history(self.account, sender, self.limit)?
            }
            EvidenceRequest::ReadRecord { message_id } => {
                self.store
                    .snapshot_agent_sources(self.account, &[*message_id])?;
                let context = self
                    .store
                    .load_agent_context(&read_job(self.account, *message_id))?;
                self.sources
                    .lock()
                    .expect("evidence sources")
                    .push(context.message.source.clone());
                return Ok(EvidenceResult {
                    data: serde_json::json!({"records": context.previous_decision.map(|d| d.records)}),
                    source_message_ids: vec![*message_id],
                    thread_revisions: BTreeMap::new(),
                });
            }
            EvidenceRequest::ReadAttachment {
                message_id,
                attachment_id,
            } => {
                let message = self.store.agent_read_message(self.account, *message_id)?;
                self.sources
                    .lock()
                    .expect("evidence sources")
                    .push(message.source.clone());
                let thread = self
                    .store
                    .thread_view_with_html(self.account, &message.thread_id)?;
                let id = attachment_id
                    .parse::<i64>()
                    .map_err(|_| CoreError::NotFound)?;
                let belongs = thread
                    .messages
                    .iter()
                    .filter(|m| m.id == *message_id)
                    .flat_map(|m| &m.attachments)
                    .any(|a| a.id == id);
                if !belongs {
                    return Err(CoreError::NotFound);
                }
                let (_, mime, bytes) = self
                    .store
                    .attachment_bytes(self.account, id)?
                    .ok_or(CoreError::NotFound)?;
                let data = match bytes {
                    Some(bytes) if mime.starts_with("text/") || mime == "application/json" => {
                        let text = String::from_utf8(bytes).map_err(|_| {
                            CoreError::InvalidInput("unsupported attachment encoding".into())
                        })?;
                        serde_json::json!({"text": crate::text::truncate_chars(&text, 16_000), "truncated": text.chars().count() > 16_000})
                    }
                    Some(_) => serde_json::json!({"unsupported": true, "mime": mime}),
                    None => {
                        serde_json::json!({"unavailable": true, "reason": "attachment bytes not stored"})
                    }
                };
                return Ok(EvidenceResult {
                    data,
                    source_message_ids: vec![*message_id],
                    thread_revisions: BTreeMap::new(),
                });
            }
        };
        if let Some(message) = messages.first() {
            let context = self
                .store
                .load_agent_context(&read_job(self.account, message.id))?;
            revisions.insert(
                message.thread_id.clone(),
                context.revision.attention_revision,
            );
        }
        self.sources
            .lock()
            .expect("evidence sources")
            .extend(messages.iter().map(|m| m.source.clone()));
        let sources = messages.iter().map(|m| m.id).collect();
        Ok(EvidenceResult {
            data: serde_json::json!({"messages": messages}),
            source_message_ids: sources,
            thread_revisions: revisions,
        })
    }
}

fn snapshot(context: &AgentContext, job: &AgentJob, limit: usize) -> ContextSnapshot {
    let siblings = context
        .thread
        .iter()
        .filter(|message| message.id != context.message.id)
        .take(limit)
        .collect::<Vec<_>>();
    let mut sources = siblings.iter().map(|m| m.id).collect::<Vec<_>>();
    sources.push(context.message.id);
    sources.sort_unstable();
    sources.dedup();
    ContextSnapshot {
        account_id: job.account_id,
        message_id: job.message_id,
        thread_id: context.message.thread_id.clone(),
        initial: serde_json::json!({
            "message": context.message,
            "thread": siblings,
            "previous_decision": context.previous_decision,
            "attention": context.attention,
            "sender_preferences": context.matched_rules,
            "sender_is_contact": context.sender_is_contact,
            "user_corrections": context.corrections,
            "trigger": job.trigger,
            "access_only": context.message.is_sent || context.message.is_spam,
            "now": Utc::now(),
        }),
        source_message_ids: sources,
        thread_revisions: [(
            context.message.thread_id.clone(),
            context.revision.attention_revision,
        )]
        .into_iter()
        .collect(),
        rule_ids: context
            .matched_rules
            .iter()
            .filter_map(|r| r.get("id").and_then(|id| id.as_i64()))
            .collect(),
        memory: Vec::new(),
    }
}

impl<S: Store + 'static, C: CredentialStore + 'static + ?Sized> SyncEngine<S, C> {
    pub(super) async fn agent_triage_pass(&self) -> bool {
        if Utc::now().timestamp() < self.agent_retry_after.load(Ordering::Relaxed) {
            return false;
        }
        let Some(llm) = &self.stage2_llm else {
            return false;
        };
        if let Err(error) = self.config.triage.validate() {
            eprintln!("squelch: invalid triage configuration: {error}");
            return false;
        }
        let day = Utc::now().format("%Y-%m-%d").to_string();
        // The one definition of the history window, shared with the backfill
        // and the blank-body heal, so `sync.archive_days` cannot reach it.
        let since = self.backfill_since();
        if let Err(error) = self.store.initialize_agent_cutover(self.account_id, since) {
            eprintln!("squelch: triage cutover could not queue historical work: {error}");
            return false;
        }
        let circuit_generation = self.agent_retry_after.load(Ordering::Relaxed);
        // Half-open recovery admits one probe; queued work is not a probe storm.
        let concurrency = if circuit_generation > 0 {
            1
        } else {
            self.config.triage.agent.concurrency
        };
        let mut jobs = Vec::new();
        let mut revisits = 0;
        for _ in 0..concurrency {
            let kind =
                if self.config.revisit.enabled && revisits < self.config.revisit.batch_per_cycle {
                    "investigation"
                } else {
                    "initial_investigation"
                };
            match self.store.claim_agent_job(
                self.account_id,
                kind,
                Utc::now(),
                self.config.triage.agent.timeout_secs as i64 + 30,
            ) {
                Ok(Some(job)) => {
                    if job.trigger.starts_with("revisit:") {
                        revisits += 1;
                    }
                    jobs.push(job);
                }
                Ok(None) => break,
                Err(error) => {
                    eprintln!("squelch: triage claim failed: {error}");
                    break;
                }
            }
        }
        let claimed = !jobs.is_empty();
        futures::future::join_all(
            jobs.into_iter()
                .map(|job| self.process_agent_job(job, llm, &day)),
        )
        .await;
        claimed
    }

    pub(super) async fn process_agent_job(&self, job: AgentJob, llm: &ResolvedLlm, day: &str) {
        if job.kind == "access" {
            self.process_access_job(job, llm, day).await;
            return;
        }
        let mut context = match self.store.load_agent_context(&job) {
            Ok(context) => context,
            Err(_) => {
                self.retry_agent(&job, "context_unavailable");
                return;
            }
        };
        let now = Utc::now();
        let retry_after = self.agent_retry_after.load(Ordering::Relaxed);
        if now.timestamp() < retry_after {
            let until = DateTime::from_timestamp(retry_after, 0).unwrap_or(now);
            let _ = self.store.defer_agent_job(&job, until, "provider_cooldown");
            return;
        }
        if job.trigger.starts_with("revisit:") && !self.config.revisit.enabled {
            let _ = self.store.defer_agent_job(
                &job,
                now + ChronoDuration::hours(1),
                "revisits_disabled",
            );
            return;
        }
        let limits = investigation_budget_limits(&self.config, &job);
        match self
            .store
            .reserve_agent_budget(self.account_id, day, &limits)
        {
            Ok(true) => {}
            Ok(false) => {
                let tomorrow = (now.date_naive() + ChronoDuration::days(1))
                    .and_hms_opt(0, 0, 1)
                    .expect("valid midnight")
                    .and_utc();
                let _ = self
                    .store
                    .defer_agent_job(&job, tomorrow, "daily_budget_exhausted");
                self.metrics.record_agent(AgentVerdict::Deferred);
                return;
            }
            Err(_) => {
                self.retry_agent(&job, "budget_unavailable");
                return;
            }
        }
        let mut exposed_ids = context
            .thread
            .iter()
            .filter(|message| message.id != context.message.id)
            .take(self.config.triage.context.initial_thread_messages)
            .map(|message| message.id)
            .collect::<Vec<_>>();
        exposed_ids.push(context.message.id);
        exposed_ids.sort_unstable();
        exposed_ids.dedup();
        if self
            .store
            .snapshot_agent_sources(self.account_id, &exposed_ids)
            .is_err()
        {
            let _ = self
                .store
                .refund_agent_budget(self.account_id, day, &limits);
            self.retry_agent(&job, "context_unavailable");
            return;
        }
        // Snapshot the content shown in this investigation, not unseen siblings.
        // Initialization above ensures observed historical sources have an access
        // job; commit still compares the original evidence versions against now.
        let reader = StoreEvidence {
            store: self.store.as_ref(),
            account: self.account_id,
            limit: self.config.triage.context.max_related_messages,
            sources: std::sync::Mutex::new(
                exposed_ids
                    .iter()
                    .filter_map(|id| {
                        context
                            .thread
                            .iter()
                            .chain(std::iter::once(&context.message))
                            .find(|m| m.id == *id)
                            .map(|m| m.source.clone())
                    })
                    .collect(),
            ),
        };
        let mut config = self.config.triage.clone();
        let base_model = config
            .agent
            .model
            .as_deref()
            .unwrap_or(&self.config.stage2.model)
            .to_string();
        if llm.provider == Stage2Provider::Anthropic && crate::triage::llm::is_gateway_url(&llm.url)
        {
            config.agent.model = Some(
                crate::triage::llm::qualify_gateway_model(&base_model)
                    .unwrap_or(base_model.clone()),
            );
            if let Some(review) = &config.agent.review_model {
                config.agent.review_model = Some(
                    crate::triage::llm::qualify_gateway_model(review).unwrap_or(review.clone()),
                );
            }
        }
        let input = snapshot(&context, &job, config.context.initial_thread_messages);
        // A new account triages the jobs somebody is waiting on at the
        // onboarding effort for its first days (`Stage2Config::effort_for_job`):
        // the store's foreground lane, and nothing else. A person's
        // one-message request is on it already; their WINDOW re-triage (a
        // `manual:` trigger over days of mail) is not, by the store's own
        // design, since one click is hundreds of jobs the lane keeps off the
        // arrival reserve, and the effort follows the lane for the same
        // reason. The backfill and the rest of the background work run at
        // the steady-state effort from day one. A creation time the store
        // could not give reads as "not new" and takes the cheaper setting.
        let effort =
            self.config
                .stage2
                .effort_for_job(job.foreground, || self.account_created_at(), now);
        let connection = AgentConnection {
            http: &self.http,
            url: &llm.url,
            api_key: &llm.api_key,
            provider: llm.provider,
            model: &base_model,
            effort,
        };
        match run_agent(connection, &config, input, &reader).await {
            Ok(run) => {
                let until = self.agent_retry_after.load(Ordering::Relaxed);
                if until > 0 && until <= Utc::now().timestamp() {
                    let _ = self.agent_retry_after.compare_exchange(
                        until,
                        0,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    );
                }
                context.run_metadata = serde_json::json!({
                    "prompt_version": crate::triage::agent::PROMPT_VERSION,
                    "schema_version": 1,
                    "model": config.agent.model.as_deref().unwrap_or(&base_model),
                    "review_model": config.agent.review_model,
                    "effort": effort,
                    "config": config,
                    "model_turns": run.model_turns,
                    "tool_calls": run.tool_calls,
                    "source_message_ids": run.source_message_ids,
                    "usage": run.usage,
                });
                for usage in run.usage {
                    let _ = self
                        .store
                        .stage2_bump_usage(self.account_id, day, usage.into());
                }
                let sources = reader.sources.lock().expect("evidence sources").clone();
                match self.store.commit_agent_decision_with_policy(
                    &job,
                    &context,
                    &run.decision,
                    &sources,
                    &self.config.revisit,
                ) {
                    // For an arrival-eligible job the commit transaction
                    // queued a durable `deliberate_notification`; wake the
                    // lane that claims it. Any other job queued no push, and
                    // a poke per revisit or backfill decision would be a lane
                    // round each that finds nothing.
                    Ok(AgentCommitOutcome::Applied) => {
                        self.metrics.record_agent(AgentVerdict::Applied);
                        if job.arrival_eligible {
                            self.notify_wake.notify_one();
                        }
                    }
                    Ok(AgentCommitOutcome::Stale) => {
                        self.metrics.record_agent(AgentVerdict::Stale);
                        self.retry_agent(&job, "context_changed");
                    }
                    Err(_) => {
                        self.retry_agent(&job, "commit_failed");
                    }
                }
            }
            Err(error) => self.handle_agent_failure(&job, error, day, &limits, false),
        }
    }

    fn handle_agent_failure(
        &self,
        job: &AgentJob,
        error: crate::triage::agent::AgentFailure,
        day: &str,
        limits: &[(String, u32)],
        access: bool,
    ) {
        for usage in error.usage {
            if access {
                let _ = self.store.extract_bump_usage(
                    self.account_id,
                    day,
                    crate::triage::access::USAGE_CATEGORY,
                    usage.into(),
                );
            } else {
                let _ = self
                    .store
                    .stage2_bump_usage(self.account_id, day, usage.into());
            }
        }
        let rejected = crate::triage::llm::is_config_failure(&error.kind);
        if error.model_calls == 0 || (rejected && error.model_calls == 1) {
            let _ = self.store.refund_agent_budget(self.account_id, day, limits);
        }
        if is_provider_outage(&error.kind) {
            if rejected {
                self.metrics.record_llm_config_failure();
            }
            let until = Utc::now()
                + ChronoDuration::seconds(
                    self.config.triage.agent.outage_retry_secs.min(86400) as i64
                );
            eprintln!(
                "squelch: triage provider unavailable ({}); queued work retained, retrying in {}s",
                error.kind,
                self.config.triage.agent.outage_retry_secs.min(86400)
            );
            self.agent_retry_after
                .fetch_max(until.timestamp(), Ordering::Relaxed);
            self.metrics.note_triage_cooldown(until.timestamp());
            let _ = self.store.defer_agent_job(job, until, &error.kind);
            self.metrics.record_agent(AgentVerdict::Retryable);
        } else if crate::triage::llm::is_content_refusal(&error.kind) {
            // See is_content_refusal: fail once, in either lane.
            self.metrics.record_agent(AgentVerdict::Failed);
            if let Err(error) = self.store.fail_agent_job(job, &error.kind) {
                eprintln!("squelch: could not record refused triage job: {error}");
            }
        } else {
            self.retry_agent(job, &error.kind);
        }
    }

    async fn process_access_job(&self, job: AgentJob, llm: &ResolvedLlm, day: &str) {
        let now = Utc::now();
        let retry_after = self.agent_retry_after.load(Ordering::Relaxed);
        if now.timestamp() < retry_after {
            let until = DateTime::from_timestamp(retry_after, 0).unwrap_or(now);
            let _ = self.store.defer_agent_job(&job, until, "provider_cooldown");
            return;
        }
        let message = match self.store.load_agent_access_message(&job) {
            Ok(message) => message,
            Err(_) => {
                self.retry_agent(&job, "context_unavailable");
                return;
            }
        };
        let limits = investigation_budget_limits(&self.config, &job);
        match self
            .store
            .reserve_agent_budget(self.account_id, day, &limits)
        {
            Ok(true) => {}
            Ok(false) => {
                let tomorrow = (now.date_naive() + ChronoDuration::days(1))
                    .and_hms_opt(0, 0, 1)
                    .expect("midnight")
                    .and_utc();
                let _ = self
                    .store
                    .defer_agent_job(&job, tomorrow, "daily_budget_exhausted");
                self.metrics.record_agent(AgentVerdict::Deferred);
                return;
            }
            Err(_) => {
                self.retry_agent(&job, "budget_unavailable");
                return;
            }
        }
        let notify = &self.config.notify;
        let model = if llm.provider == Stage2Provider::Anthropic
            && crate::triage::llm::is_gateway_url(&llm.url)
        {
            crate::triage::llm::qualify_gateway_model(&notify.model).unwrap_or(notify.model.clone())
        } else {
            notify.model.clone()
        };
        let connection = AgentConnection {
            http: &self.http,
            url: &llm.url,
            api_key: &llm.api_key,
            provider: llm.provider,
            model: &model,
            effort: notify.effort.as_deref(),
        };
        match crate::triage::access::run_access(
            connection,
            notify,
            &message,
            self.config.triage.context.max_context_bytes,
        )
        .await
        {
            Ok(run) => {
                let until = self.agent_retry_after.load(Ordering::Relaxed);
                if until > 0 && until <= Utc::now().timestamp() {
                    let _ = self.agent_retry_after.compare_exchange(
                        until,
                        0,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    );
                }
                let metadata = serde_json::json!({"prompt_version":crate::triage::access::PROMPT_VERSION,
                    "model":model,"model_calls":run.model_calls,"usage":run.usage,"assessment":run.decision});
                for usage in run.usage {
                    let _ = self.store.extract_bump_usage(
                        self.account_id,
                        day,
                        crate::triage::access::USAGE_CATEGORY,
                        usage.into(),
                    );
                }
                match self
                    .store
                    .commit_agent_access(&job, &message, &run.decision, &metadata)
                {
                    Ok(AgentCommitOutcome::Applied) => {
                        self.metrics.record_agent(AgentVerdict::Applied)
                    }
                    Ok(AgentCommitOutcome::Stale) => self.retry_agent(&job, "context_changed"),
                    Err(_) => self.retry_agent(&job, "commit_failed"),
                }
            }
            Err(error) => self.handle_agent_failure(&job, error, day, &limits, true),
        }
    }

    fn retry_agent(&self, job: &AgentJob, code: &str) {
        if job.attempts >= i64::from(self.config.triage.agent.max_attempts) {
            self.metrics.record_agent(AgentVerdict::Failed);
            if let Err(error) = self.store.fail_agent_job(job, code) {
                eprintln!("squelch: could not record failed triage job: {error}");
            }
            return;
        }
        self.metrics.record_agent(AgentVerdict::Retryable);
        let seconds = 15_i64
            .saturating_mul(2_i64.pow(job.attempts.clamp(0, 8) as u32))
            .min(3600);
        if let Err(error) =
            self.store
                .retry_agent_job(job, Utc::now() + ChronoDuration::seconds(seconds), code)
        {
            eprintln!("squelch: could not schedule triage retry: {error}");
        }
    }

    /// THE NOTIFICATION LANE: claims and runs the two push-shaped job kinds.
    ///
    /// It used to loop on a fixed one-second sleep and open a `BEGIN
    /// IMMEDIATE` claim per kind per second whether or not any job existed:
    /// 172,800 write-lock acquisitions a day per tenant that found nothing,
    /// each serialized against ingest and the doors. Now a READ asks when
    /// each kind's next job is due (`next_agent_jobs_due`, a seek on the
    /// account's ready index); a kind is claimed only when one is, the sleep
    /// backs off from the poll interval to [`MAX_NOTIFY_IDLE_BACKOFF`] while
    /// the queue stays empty, a job scheduled ahead (a retry, a lapsing
    /// lease) cuts the sleep to its due time, and the producers'
    /// `notify_wake` ends it at once, so an arrival is claimed as fast as
    /// before and faster when the old loop would have been mid-sleep.
    ///
    /// A failing read is not a reason to stop claiming: the claim runs for
    /// every kind, meets the same error or finds the job the read could not
    /// describe, and the ladder keeps a broken store from being asked more
    /// than a dozen times a minute. The error is logged on the first failing
    /// read and once an hour while it persists, not once per round.
    pub(super) async fn agent_notification_lane(
        &self,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) {
        let interval = Duration::from_secs(self.config.triage.agent.worker_poll_secs);
        let mut idle_rounds = 0u32;
        let mut read_failing: Option<std::time::Instant> = None;
        loop {
            if *shutdown.borrow() {
                return;
            }
            let mut due = self.notification_jobs_due(&mut read_failing);
            let now = Utc::now();
            let mut jobs = Vec::new();
            for kind in NOTIFICATION_JOB_KINDS {
                let claimable = due.as_ref().map_or(true, |rows| {
                    rows.iter().any(|(k, at)| k == kind && *at <= now)
                });
                if !claimable {
                    continue;
                }
                for _ in 0..self.config.notify.fast_concurrency.max(1) {
                    match self.store.claim_agent_job(
                        self.account_id,
                        kind,
                        Utc::now(),
                        self.notification_lease_secs(kind),
                    ) {
                        Ok(Some(job)) => jobs.push(job),
                        Ok(None) => break,
                        Err(error) => {
                            eprintln!("squelch: notification claim failed: {error}");
                            break;
                        }
                    }
                }
            }
            let ran = !jobs.is_empty();
            idle_rounds = if ran {
                0
            } else {
                idle_rounds.saturating_add(1)
            };
            futures::future::join_all(jobs.into_iter().map(|job| async move {
                let context = match self.store.load_agent_context(&job) {
                    Ok(context) => context,
                    Err(_) => {
                        self.retry_agent(&job, "notify_context_unavailable");
                        return;
                    }
                };
                let result = if job.kind == "deliberate_notification" {
                    match &context.previous_decision {
                        Some(decision) => self.notify_lane().request_assessed(
                            &context,
                            decision.auth.is_auth(),
                            &decision.notification,
                            self.config
                                .triage
                                .agent
                                .model
                                .as_deref()
                                .unwrap_or(&self.config.stage2.model),
                        ),
                        None => {
                            self.retry_agent(&job, "notify_decision_unavailable");
                            return;
                        }
                    }
                } else if let Some(candidate) =
                    notify_lane::candidate_from_context(&context, &self.config.notify)
                {
                    self.notify_lane().clone().run(candidate).await
                } else {
                    Ok(())
                };
                match result {
                    Ok(()) => {
                        if self.store.complete_agent_job(&job).is_err() {
                            self.retry_agent(&job, "notify_completion_unavailable");
                        }
                    }
                    Err(_) => self.retry_agent(&job, "notify_store_unavailable"),
                }
            }))
            .await;
            // After a round that ran, read again: what ran may have queued
            // more (a retry of its own), and the rest of the queue may be due
            // by now. After an empty round the first read still holds:
            // nothing but a producer changes the queue, and a producer pokes.
            if ran {
                due = self.notification_jobs_due(&mut read_failing);
            }
            let until_due = due.ok().and_then(|rows| {
                rows.iter()
                    .map(|(_, at)| (*at - Utc::now()).to_std().unwrap_or(Duration::ZERO))
                    .min()
            });
            let wait = notification_wait(idle_rounds, interval, until_due);
            // A producer's poke ends the sleep and restarts the ladder: the
            // next round is the first after an arrival.
            tokio::select! {
                _ = self.notify_wake.notified() => { idle_rounds = 0; }
                _ = tokio::time::sleep(wait) => {}
                _ = shutdown.changed() => { if *shutdown.borrow() { return; } }
            }
        }
    }

    /// [`AgentTriageStore::next_agent_jobs_due`] for the lane's kinds.
    /// `failing` is when the failure was last logged: the error is logged on
    /// the first failing read, then once an hour while reads keep failing,
    /// so a persistent one stays visible without a line per round.
    fn notification_jobs_due(
        &self,
        failing: &mut Option<std::time::Instant>,
    ) -> std::result::Result<Vec<(String, DateTime<Utc>)>, ()> {
        match self
            .store
            .next_agent_jobs_due(self.account_id, &NOTIFICATION_JOB_KINDS)
        {
            Ok(rows) => {
                *failing = None;
                Ok(rows)
            }
            Err(error) => {
                if failing.is_none_or(|at| at.elapsed() >= Duration::from_secs(3600)) {
                    eprintln!("squelch: notification due read failed: {error}");
                    *failing = Some(std::time::Instant::now());
                }
                Err(())
            }
        }
    }

    /// How long a claimed notification job is leased: the fast model call's
    /// timeout with room to record its outcome, and a short lease for the
    /// deliberate one, which makes no model call of its own.
    fn notification_lease_secs(&self, kind: &str) -> i64 {
        match kind {
            "notification" => self.config.notify.fast_timeout_secs as i64 + 30,
            _ => 30,
        }
    }
}

/// HOW LONG THE NOTIFICATION LANE SLEEPS after a round. The idle ladder:
/// `interval` after a round that ran something (`idle_rounds` is zero),
/// doubling per empty round to [`MAX_NOTIFY_IDLE_BACKOFF`], or to the
/// interval itself when it is configured longer than that. Cut short by a
/// job already on the calendar `until_due` from now (a retry the lane
/// scheduled, a lease that has to lapse), to nothing when a round that ran
/// left more due than it could claim. A job due now after an EMPTY round is
/// one the read counts and the claim refused, which the read's own shape
/// leaves to a race (see `NEXT_JOBS_DUE_SQL`): it does not cut the ladder,
/// so such a row costs the lane a claim per rung, never a spin. Pure, so
/// the ladder is testable without a clock.
pub(super) fn notification_wait(
    idle_rounds: u32,
    interval: Duration,
    until_due: Option<Duration>,
) -> Duration {
    let backoff = interval
        .saturating_mul(1 << idle_rounds.min(5))
        .min(MAX_NOTIFY_IDLE_BACKOFF.max(interval));
    until_due
        .filter(|d| idle_rounds == 0 || !d.is_zero())
        .map_or(backoff, |d| d.min(backoff))
}

/// The longest the notification lane sleeps between looks at an empty queue.
/// Shorter than the refine lane's [`MAX_LANE_IDLE_BACKOFF`] because this lane
/// has producers it cannot hear: a correction, a rule change, a heal or a
/// sent reply can re-queue a pending push through the store, which has no
/// handle on the engine's `notify_wake`, and a push is the one output of the
/// daemon measured in seconds. Five seconds bounds those, at a dozen index
/// reads a minute over an empty queue; the old loop's two write-lock claims
/// a second were a hundred and twenty.
pub(super) const MAX_NOTIFY_IDLE_BACKOFF: Duration = Duration::from_secs(5);

/// The job kinds the notification lane claims, in claim order: an arrival's
/// push, then the deliberate one that follows a decision.
pub(super) const NOTIFICATION_JOB_KINDS: [&str; 2] = ["notification", "deliberate_notification"];

/// Provider failures describe shared availability, not a permanently bad email.
fn is_provider_outage(kind: &str) -> bool {
    crate::triage::llm::is_config_failure(kind)
        || matches!(kind, "transport" | "http_429")
        || kind
            .strip_prefix("http_")
            .and_then(|v| v.split(':').next())
            .and_then(|v| v.parse::<u16>().ok())
            .is_some_and(|status| (500..600).contains(&status))
}

/// The total limit bounds all investigations. Background work has a second
/// ceiling, leaving capacity for genuinely new inbound mail. Historical stage
/// escalation caps deliberately do not gate this only classification path.
fn investigation_budget_limits(config: &Config, job: &AgentJob) -> Vec<(String, u32)> {
    let mut limits = vec![(GLOBAL_BUDGET_KEY.into(), config.triage.agent.daily_run_cap)];
    if !job.foreground || job.kind == "access" {
        limits.push((
            crate::triage::agent_config::BACKGROUND_BUDGET_KEY.into(),
            config.triage.agent.effective_background_daily_run_cap(),
        ));
    }
    if job.trigger.starts_with("revisit:") {
        limits.push(("__agent_revisit__".into(), config.revisit.daily_cap));
    }
    // An external caller's search asked for this one. Its own ceiling, on top
    // of the two above, so a day of searches cannot spend the background
    // budget that the owner's sent mail is assessed from.
    if job.trigger == "archive_access" {
        limits.push((
            crate::store::ARCHIVE_ACCESS_BUDGET_KEY.into(),
            config.triage.agent.archive_access_daily_cap,
        ));
    }
    limits
}

#[cfg(test)]
mod notification_lane_tests {
    use super::*;

    /// THE SLEEP LADDER, as a table. Doubling from the interval while the
    /// queue stays empty and capped at five seconds (or the interval, when
    /// that is longer); the interval after a
    /// round that ran something, nothing at all when it left more due than
    /// it could claim; cut to a job already on the calendar; and a job due
    /// now after an empty round, which the claim refused, rides the ladder
    /// rather than spinning it.
    #[test]
    fn the_notification_wait_backs_off_cuts_to_the_calendar_and_never_spins() {
        let s = Duration::from_secs;
        for (idle_rounds, interval, until_due, expected, why) in [
            (1, s(1), None, s(2), "the first empty round doubles"),
            (2, s(1), None, s(4), "and keeps doubling"),
            (3, s(1), None, s(5), "to the cap"),
            (9, s(1), None, s(5), "which more rounds cannot pass"),
            (0, s(1), None, s(1), "a round that ran waits the interval"),
            (
                0,
                s(1),
                Some(s(0)),
                s(0),
                "unless it left more due right now",
            ),
            (3, s(1), Some(s(3)), s(3), "a job ahead cuts the backoff"),
            (1, s(1), Some(s(7)), s(2), "but never lengthens it"),
            (
                0,
                s(3600),
                None,
                s(3600),
                "an interval above the cap is honoured",
            ),
            (
                3,
                s(3600),
                None,
                s(3600),
                "and the ladder does not shorten it",
            ),
            (
                1,
                s(3600),
                Some(s(7)),
                s(7),
                "though a job ahead cuts it like any other",
            ),
            (
                1,
                s(1),
                Some(s(0)),
                s(2),
                "due but refused by the claim: the ladder, not a spin",
            ),
            (5, s(1), Some(s(0)), s(5), "all the way up it"),
        ] {
            assert_eq!(
                notification_wait(idle_rounds, interval, until_due),
                expected,
                "{why}: idle_rounds={idle_rounds} interval={interval:?} until_due={until_due:?}"
            );
        }
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    use crate::store::{SqliteStore, agent_triage::AgentTriageStore};

    fn evidence_fixture() -> (SqliteStore, AccountId, Vec<i64>) {
        let store = SqliteStore::open_in_memory().unwrap();
        let account = store.ensure_account("context@example.com").unwrap();
        let ids = (0..3)
            .map(|index| {
                store
                    .upsert_message(&crate::types::NewMessage {
                        account_id: account,
                        gmail_msg_id: format!("message-{index}"),
                        thread_id: "thread".into(),
                        from_addr: "sender@example.com".into(),
                        from_name: None,
                        subject: "Subject".into(),
                        received_at: Utc::now(),
                        snippet: String::new(),
                        body: format!("{}decisive tail", "x".repeat(28_000)),
                        body_html: None,
                        is_sent: false,
                        is_spam: false,
                        to_addrs: None,
                        cc_addrs: None,
                        bcc_addrs: None,
                        list_unsubscribe: None,
                        list_unsub_one_click: false,
                        auth_pass: None,
                    })
                    .unwrap()
            })
            .collect();
        (store, account, ids)
    }

    #[test]
    fn snapshot_exposes_subject_once_and_tracks_exact_siblings_and_preferences() {
        let message = |id| {
            serde_json::json!({
                "id":id,"thread_id":"thread","from_addr":"sender@test","subject":"subject",
                "body":format!("body-{id}"),"received_at":"2026-01-01","is_sent":false,
                "is_spam":false,"status":"new","notify_eligible_at":null,"opened_at":null,"remind_at":null,
                "source":{"message_id":id,"content":"revision","user_state":"state","attention_revision":1}
            })
        };
        let context: AgentContext = serde_json::from_value(serde_json::json!({
            "message":message(1),"thread":[message(1),message(2),message(3),message(4)],
            "previous_decision":null,"attention":null,"rules":[{"id":10},{"id":20}],
            "matched_rules":[{"id":10}],"sender_is_contact":true,"corrections":[],
            "revision":{"content":"revision","preferences":"preferences","user_state":"state","attention_revision":1},
            "memory":[],"run_metadata":{}
        })).unwrap();
        let snapshot = snapshot(&context, &read_job(1, 1), 2);
        assert_eq!(snapshot.source_message_ids, vec![1, 2, 3]);
        assert_eq!(
            snapshot.initial["thread"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["id"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!(snapshot.initial["message"]["id"], 1);
        assert_eq!(snapshot.rule_ids, vec![10]);
        assert_eq!(
            snapshot.initial["sender_preferences"],
            serde_json::json!([{"id":10}])
        );
        assert_eq!(snapshot.initial["sender_is_contact"], true);
    }

    #[test]
    fn snapshot_preserves_target_once_and_only_applicable_preferences() {
        let (store, account, ids) = evidence_fixture();
        let job = read_job(account, ids[0]);
        let mut context = store.load_agent_context(&job).unwrap();
        context.sender_is_contact = true;
        context.rules = vec![serde_json::json!({"id":99,"want_text":"unrelated"})];
        context.matched_rules = vec![serde_json::json!({"id":7,"want_text":"relevant"})];
        let snapshot = snapshot(&context, &job, 8);
        assert!(
            snapshot.initial["message"]["body"]
                .as_str()
                .unwrap()
                .ends_with("decisive tail")
        );
        assert_eq!(snapshot.initial["thread"].as_array().unwrap().len(), 2);
        assert!(
            snapshot.initial["thread"]
                .as_array()
                .unwrap()
                .iter()
                .all(|message| message["id"] != ids[0])
        );
        assert_eq!(snapshot.initial["sender_is_contact"], true);
        assert_eq!(snapshot.rule_ids, vec![7]);
        assert!(!snapshot.initial.to_string().contains("unrelated"));
    }

    #[test]
    fn read_thread_initializes_only_exposed_source_assessments() {
        let (store, account, ids) = evidence_fixture();
        let reader = StoreEvidence {
            store: &store,
            account,
            limit: 2,
            sources: std::sync::Mutex::new(Vec::new()),
        };
        let result = reader
            .read(&EvidenceRequest::ReadThread {
                thread_id: "thread".into(),
            })
            .unwrap();
        assert_eq!(result.source_message_ids.len(), 2);
        let mut queued = Vec::new();
        while let Some(job) = store
            .claim_agent_job(account, "access", Utc::now(), 60)
            .unwrap()
        {
            queued.push(job.message_id);
            assert!(!job.arrival_eligible);
            store.complete_agent_job(&job).unwrap();
        }
        queued.sort_unstable();
        let mut exposed = result.source_message_ids;
        exposed.sort_unstable();
        assert_eq!(queued, exposed);
        assert!(
            !queued.contains(&ids[0]),
            "unseen oldest sibling remains unqueued"
        );
    }

    #[test]
    fn migration_cannot_spend_arrival_reserve_and_old_escalation_caps_do_not_bind() {
        let store = SqliteStore::open_in_memory().unwrap();
        let account = store.ensure_account("arrival-budget@example.com").unwrap();
        let config = Config::default();
        let mut job = read_job(account, 1);
        let background = investigation_budget_limits(&config, &job);
        let agent = &config.triage.agent;
        let background_cap = agent.effective_background_daily_run_cap();
        // Simulate a cutover far larger than its background budget.
        for index in 0..background_cap + 1300 {
            assert_eq!(
                store
                    .reserve_agent_budget(account, "today", &background)
                    .unwrap(),
                index < background_cap
            );
        }
        job.foreground = true;
        let arrival = investigation_budget_limits(&config, &job);
        // This exceeds the retired 120/account, 5/sender and 3/thread limits.
        for _ in 0..agent.daily_run_cap - background_cap {
            assert!(
                store
                    .reserve_agent_budget(account, "today", &arrival)
                    .unwrap()
            );
        }
        assert!(
            !store
                .reserve_agent_budget(account, "today", &arrival)
                .unwrap()
        );
        assert_eq!(
            store
                .stage2_budget_used(account, GLOBAL_BUDGET_KEY, "today")
                .unwrap(),
            agent.daily_run_cap
        );
    }

    /// An archive access check is charged three times over: the total, the
    /// background budget, and a sub-cap of its own, so a day of agent searches
    /// runs out of ITS budget long before it touches the one the owner's sent
    /// mail is assessed from.
    #[test]
    fn an_archive_access_check_is_charged_to_its_own_daily_sub_cap_as_well() {
        let store = SqliteStore::open_in_memory().unwrap();
        let account = store.ensure_account("cap@example.com").unwrap();
        let mut config = Config::default();
        assert_eq!(config.triage.agent.archive_access_daily_cap, 50);
        config.triage.agent.archive_access_daily_cap = 3;
        let mut job = read_job(1, 1);
        job.kind = "access".into();
        job.trigger = "archive_access".into();
        let limits = investigation_budget_limits(&config, &job);
        assert_eq!(
            limits,
            vec![
                (GLOBAL_BUDGET_KEY.into(), config.triage.agent.daily_run_cap),
                (
                    crate::store::BACKGROUND_BUDGET_KEY.into(),
                    config.triage.agent.effective_background_daily_run_cap()
                ),
                (crate::store::ARCHIVE_ACCESS_BUDGET_KEY.into(), 3),
            ]
        );
        for run in 0..4 {
            assert_eq!(
                store
                    .reserve_agent_budget(account, "today", &limits)
                    .unwrap(),
                run < 3
            );
        }
        // The refused fourth charged nothing anywhere.
        assert_eq!(
            store
                .stage2_budget_used(account, crate::store::BACKGROUND_BUDGET_KEY, "today")
                .unwrap(),
            3
        );
        // Every other access check is as it was: no sub-cap.
        job.trigger = "source_access".into();
        assert_eq!(investigation_budget_limits(&config, &job).len(), 2);
    }

    #[test]
    fn access_cannot_borrow_arrival_capacity_and_lowering_total_keeps_a_reserve() {
        let mut config = Config::default();
        config.triage.agent.daily_run_cap = 2;
        let mut job = read_job(1, 1);
        job.kind = "access".into();
        job.foreground = true; // Defensive even if a malformed lane reaches us.
        assert_eq!(
            investigation_budget_limits(&config, &job),
            vec![
                (GLOBAL_BUDGET_KEY.into(), 2),
                ("__agent_background__".into(), 1)
            ]
        );
        config.triage.agent.background_daily_run_cap = 0;
        assert_eq!(investigation_budget_limits(&config, &job)[1].1, 0);
    }

    #[test]
    fn investigation_deadline_is_not_a_shared_provider_outage() {
        assert!(!is_provider_outage("agent_timeout"));
        assert!(is_provider_outage("transport"));
        assert!(is_provider_outage("http_503"));
        assert!(is_provider_outage("http_403:permission_error"));
    }
}
