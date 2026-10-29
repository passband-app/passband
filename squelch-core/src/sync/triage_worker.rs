//! Durable model work. Ingest never waits for these decisions.
use super::*;
use crate::metrics::AgentVerdict;
use crate::store::agent_triage::{
    AgentCommitOutcome, AgentContext, AgentJob, AgentSourceSnapshot, AgentTriageStore,
};
use crate::triage::agent::{AgentConnection, run_agent};
use crate::triage::agent_config::DEADLINE_SLACK_SECS;
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
        account_id: account,
        message_id,
        kind: "triage".into(),
        trigger: "context".into(),
        ..Default::default()
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
                let page: String = message
                    .body
                    .chars()
                    .skip(offset)
                    .take(crate::triage::context::READ_MESSAGE_PAGE_CHARS)
                    .collect();
                let next = offset.saturating_add(page.chars().count());
                // Whether anything follows the page, without counting the
                // whole body.
                let more = message.body.chars().nth(next).is_some();
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
                        "user_corrections": context.corrections, "previous_decision": fed_back(context.previous_decision)}),
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

/// A stored decision as the agent may read it back: without its `done`,
/// whose reason is model-authored and untrusted and never goes into a prompt
/// (`store::sqlite::agent_triage::apply_done`). The human door still serves
/// the stored decision whole.
fn fed_back(
    decision: Option<crate::triage::decision::MessageDecision>,
) -> Option<crate::triage::decision::MessageDecision> {
    decision.map(|decision| crate::triage::decision::MessageDecision {
        done: None,
        ..decision
    })
}

/// The agent's starting context. `clock` is this run's "now" in the owner's
/// zone (#526): fixed once per run, so a zone that changes mid-run cannot
/// change an answer already given.
fn snapshot(
    context: &AgentContext,
    job: &AgentJob,
    limit: usize,
    clock: &crate::owner_clock::OwnerClock,
) -> ContextSnapshot {
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
            "previous_decision": fed_back(context.previous_decision.clone()),
            "attention": context.attention,
            "sender_preferences": context.matched_rules,
            "sender_is_contact": context.sender_is_contact,
            "user_corrections": context.corrections,
            "trigger": job.trigger,
            "access_only": context.message.is_sent || context.message.is_spam,
            // LOCAL, with the zone and weekday: "today", "tonight" and "due
            // tomorrow" are the owner's, and for an American evening UTC is
            // already tomorrow. Stored timestamps stay UTC instants.
            "now": clock.prompt_line(),
            "timezone": clock.zone.name(),
            "message_received_local": chrono::DateTime::parse_from_rfc3339(&context.message.received_at)
                .ok()
                .map(|at| clock.local(at.with_timezone(&Utc))),
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
            // Each lease outlives its deadline, so a run that uses all of
            // it is never reclaimed from under itself: an investigation's
            // the whole run's, an access job's the one call it makes under
            // the short access deadline, so a process that dies mid-check
            // (nothing dropped, no hand-back) holds its thread for seconds.
            match self.store.claim_agent_job_sized(
                self.account_id,
                kind,
                Utc::now(),
                (self.config.triage.agent.run_deadline_secs() + DEADLINE_SLACK_SECS) as i64,
                (self.config.notify.fast_deadline_secs() + DEADLINE_SLACK_SECS) as i64,
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

    /// Runs one claimed job to a written state: committed, failed, retried
    /// or deferred. Dropped before that (the refine lane's future is dropped
    /// at shutdown, or when `run_lanes` ends the lanes), the job is handed
    /// back at once, see [`ClaimGuard`].
    pub(super) async fn process_agent_job(&self, job: AgentJob, llm: &ResolvedLlm, day: &str) {
        let mut guard = ClaimGuard {
            store: &*self.store,
            metrics: &self.metrics,
            job: &job,
            max_attempts: self.config.triage.agent.max_attempts,
            armed: true,
        };
        if job.kind == "access" {
            self.process_access_job(&job, llm, day).await;
        } else {
            self.run_agent_job(&job, llm, day).await;
        }
        guard.armed = false;
    }

    async fn run_agent_job(&self, job: &AgentJob, llm: &ResolvedLlm, day: &str) {
        let mut context = match self.store.load_agent_context(job) {
            Ok(context) => context,
            Err(_) => {
                self.retry_agent(job, "context_unavailable");
                return;
            }
        };
        let now = Utc::now();
        let retry_after = self.agent_retry_after.load(Ordering::Relaxed);
        let episode = self.agent_last_cooldown_end.load(Ordering::Relaxed);
        if now.timestamp() < retry_after {
            let until = DateTime::from_timestamp(retry_after, 0).unwrap_or(now);
            let _ = self.store.defer_agent_job(job, until, "provider_cooldown");
            return;
        }
        if job.trigger.starts_with("revisit:") && !self.config.revisit.enabled {
            let _ = self.store.defer_agent_job(
                job,
                now + ChronoDuration::hours(1),
                "revisits_disabled",
            );
            return;
        }
        let limits = investigation_budget_limits(&self.config, job);
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
                    .defer_agent_job(job, tomorrow, "daily_budget_exhausted");
                self.metrics.record_agent(AgentVerdict::Deferred);
                return;
            }
            Err(_) => {
                self.retry_agent(job, "budget_unavailable");
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
            self.retry_agent(job, "context_unavailable");
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
        // Read once per run. A store that cannot answer is UTC, which is what
        // every run got before the zone existed, not a failed job.
        let clock = crate::owner_clock::OwnerClock::new(
            Utc::now(),
            self.store.owner_zone(self.account_id).unwrap_or_default(),
        );
        let input = snapshot(
            &context,
            job,
            config.context.initial_thread_messages,
            &clock,
        );
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
                self.note_provider_success();
                // An investigation answered: its ladder starts over, and the
                // two-timeouts rule may fail the next job that times out
                // twice.
                self.agent_throttle_strikes.store(0, Ordering::Relaxed);
                self.agent_answered_since_timeout
                    .store(true, Ordering::Relaxed);
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
                    job,
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
                        // Inbound mail is embedded only once it is decided
                        // (`embeddable_mail_sql!`): this decision may be the
                        // one that makes the message owed a vector.
                        self.vectors_dirty.store(true, Ordering::Relaxed);
                        if job.arrival_eligible {
                            self.notify_wake.notify_one();
                        }
                    }
                    Ok(AgentCommitOutcome::Stale) => {
                        self.metrics.record_agent(AgentVerdict::Stale);
                        self.retry_agent(job, "context_changed");
                    }
                    Err(_) => {
                        self.retry_agent(job, "commit_failed");
                    }
                }
            }
            Err(error) => {
                let start = JobStart {
                    generation: episode,
                    at: now,
                };
                self.handle_agent_failure(job, error, day, &limits, false, start)
            }
        }
    }

    /// Opens the account's provider cooldown for a failure that just landed,
    /// or joins the one a sibling opened, and returns its end and the
    /// account's stand-off from now in seconds (a longer cooldown may
    /// already be open, so that is not always this failure's own).
    ///
    /// A config failure (a bad key, a wrong model) is not going to fix
    /// itself: the long outage interval (less the jitter, so a fleet whose
    /// shared key went bad together does not probe the gateway together),
    /// and a new episode for the throttle ladder. A throttle or a server
    /// error is the provider asking for a moment, and says how long when it
    /// says anything: that long, else a short spread wait, doubling for
    /// every probe that fails in turn up to the outage interval, so a
    /// throttle that is not lifting (a spent monthly budget) is probed as
    /// the outage it is. Either way one probe is admitted when the cooldown
    /// lapses.
    ///
    /// A job that was in flight when another opened a cooldown (the other
    /// worker of the same pass: `concurrency` jobs fail together when the
    /// provider goes) is the same probe as the one that opened it, not the
    /// next, whether or not that cooldown has lapsed by the time its own
    /// request fails: the opener's rung, and no strike of its own. One rung
    /// per cooldown, not one per worker; the one predicate (the stamp the
    /// job started under against the stamp now) decides both the rung and
    /// the strike.
    ///
    /// An episode whose last cooldown ended longer ago than the outage
    /// interval before this job STARTED, with no investigation answering
    /// since to reset the ladder (the queue drained on a failure, or the
    /// recovery probe was an access check), is over: a fresh throttle hours
    /// later starts at the first rung and the provider's own number. The
    /// episode's end is its own stamp, which no success clears (so it is
    /// also the stamp "same probe" is judged by: the clearable one would
    /// make a sibling look like an opener when a success cleared it
    /// mid-flight), and the job's start is what is measured, not its
    /// failure: a probe that runs long before it fails is still the probe.
    ///
    /// Each lane has a ladder of its own: the investigation model and the
    /// fast access model are different models, throttled and recovering on
    /// their own, so a lane's failures climb its ladder and its success
    /// resets it, and neither touches the other's. The cooldown a failure
    /// opens is the account's (both lanes wait it out), at the failing
    /// lane's rung. The end returned is the account's: a longer cooldown a
    /// sibling opened wins, and the job is deferred to that, not to this
    /// failure's own.
    pub(super) fn open_cooldown(
        &self,
        retry_after: Option<std::time::Duration>,
        rejected: bool,
        start: JobStart,
        investigation: bool,
    ) -> (DateTime<Utc>, i64) {
        use crate::triage::agent_config::{MAX_TIMEOUT_SECS, MIN_OUTAGE_RETRY_SECS};
        let now = Utc::now();
        // The loader clamps and says so; this is for a config that did not
        // pass through it.
        let outage = self
            .config
            .triage
            .agent
            .outage_retry_secs
            .clamp(MIN_OUTAGE_RETRY_SECS, MAX_TIMEOUT_SECS);
        let same_probe = self.agent_last_cooldown_end.load(Ordering::Relaxed) != start.generation;
        let stale = !same_probe
            && start.generation > 0
            && start.at.timestamp().saturating_sub(start.generation) > outage as i64;
        let ladder = if investigation {
            &self.agent_throttle_strikes
        } else {
            &self.agent_access_strikes
        };
        if stale {
            ladder.store(0, Ordering::Relaxed);
        }
        let strikes = ladder.load(Ordering::Relaxed);
        let strikes = if same_probe {
            strikes.saturating_sub(1)
        } else {
            strikes
        };
        let jitter = clock_jitter_secs(now, self.account_id);
        let seconds = if rejected {
            throttle_cooldown_secs(None, u32::MAX, outage, jitter)
        } else {
            throttle_cooldown_secs(retry_after, strikes, outage, jitter)
        };
        let until = now + ChronoDuration::seconds(seconds as i64);
        let previous = self
            .agent_retry_after
            .fetch_max(until.timestamp(), Ordering::Relaxed);
        self.agent_last_cooldown_end
            .fetch_max(until.timestamp(), Ordering::Relaxed);
        if rejected {
            self.agent_throttle_strikes.store(0, Ordering::Relaxed);
            self.agent_access_strikes.store(0, Ordering::Relaxed);
        } else if !same_probe {
            // This failure opened a new cooldown: one strike on its lane's
            // ladder, added rather than stored, so two openers on one
            // engine (should a pass ever run beside another) cannot lose
            // one. The sibling of the same probe adds none, even when the
            // opener's cooldown has lapsed by the time its own request
            // fails.
            ladder.fetch_add(1, Ordering::Relaxed);
        }
        let effective = until.timestamp().max(previous);
        (
            DateTime::from_timestamp(effective, 0).unwrap_or(until),
            effective - now.timestamp(),
        )
    }

    /// The provider answered, in either lane: a cooldown that has lapsed is
    /// cleared so the next pass admits every worker rather than one probe.
    /// Each lane's ladder, and the investigation lane's "answered since"
    /// flag, are reset where that lane's model answers: the access lane's
    /// fast model answering says nothing about the investigation model.
    fn note_provider_success(&self) {
        // The gateway answered: the translate pass's release cap may now
        // charge a row that keeps failing alone.
        self.note_gateway_answer();
        let until = self.agent_retry_after.load(Ordering::Relaxed);
        if until > 0 && until <= Utc::now().timestamp() {
            let _ = self.agent_retry_after.compare_exchange(
                until,
                0,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        }
    }

    fn handle_agent_failure(
        &self,
        job: &AgentJob,
        error: crate::triage::agent::AgentFailure,
        day: &str,
        limits: &[(String, u32)],
        access: bool,
        start: JobStart,
    ) {
        // A turn of this run answered (a second call was made, which only
        // follows an answered first): the provider is answering, whatever
        // happened after. Not usage, which a gateway may omit.
        let answered_a_turn = error.model_calls > 1;
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
            // `until` is the account's stand-off, a sibling's longer one
            // included: the job is deferred to what will actually admit it.
            let (until, wait) = self.open_cooldown(error.retry_after, rejected, start, !access);
            eprintln!(
                "squelch: triage provider unavailable ({}); queued work retained, retrying in {}s",
                error.kind, wait
            );
            self.metrics.note_triage_cooldown(until.timestamp());
            let _ = self.store.defer_agent_job(job, until, &error.kind);
            self.metrics.record_agent(AgentVerdict::Retryable);
        } else {
            if answered_a_turn {
                // An earlier turn of this run answered, whatever the run
                // then failed with: the provider is answering. The rule
                // need not wait for some other job to say so, or a long
                // message alone on a quiet queue would walk every attempt.
                self.agent_answered_since_timeout
                    .store(true, Ordering::Relaxed);
            }
            self.retry_agent(job, &error.kind);
        }
    }

    async fn process_access_job(&self, job: &AgentJob, llm: &ResolvedLlm, day: &str) {
        let now = Utc::now();
        let retry_after = self.agent_retry_after.load(Ordering::Relaxed);
        let episode = self.agent_last_cooldown_end.load(Ordering::Relaxed);
        if now.timestamp() < retry_after {
            let until = DateTime::from_timestamp(retry_after, 0).unwrap_or(now);
            let _ = self.store.defer_agent_job(job, until, "provider_cooldown");
            return;
        }
        let message = match self.store.load_agent_access_message(job) {
            Ok(message) => message,
            Err(_) => {
                self.retry_agent(job, "context_unavailable");
                return;
            }
        };
        let limits = investigation_budget_limits(&self.config, job);
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
                    .defer_agent_job(job, tomorrow, "daily_budget_exhausted");
                self.metrics.record_agent(AgentVerdict::Deferred);
                return;
            }
            Err(_) => {
                self.retry_agent(job, "budget_unavailable");
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
            &self.config.triage.context,
        )
        .await
        {
            Ok(run) => {
                self.note_provider_success();
                self.agent_access_strikes.store(0, Ordering::Relaxed);
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
                    .commit_agent_access(job, &message, &run.decision, &metadata)
                {
                    Ok(AgentCommitOutcome::Applied) => {
                        self.metrics.record_agent(AgentVerdict::Applied);
                        // Own mail is embedded only once its access check
                        // settles (`embeddable_mail_sql!`): this commit may be
                        // the one that makes it owed a vector.
                        self.vectors_dirty.store(true, Ordering::Relaxed);
                    }
                    Ok(AgentCommitOutcome::Stale) => self.retry_agent(job, "context_changed"),
                    Err(_) => self.retry_agent(job, "commit_failed"),
                }
            }
            Err(error) => {
                let start = JobStart {
                    generation: episode,
                    at: now,
                };
                self.handle_agent_failure(job, error, day, &limits, true, start)
            }
        }
    }

    /// Schedules the next attempt, or fails the job when another would be
    /// waste: the attempt allowance is spent; the model refused the content
    /// (`is_content_refusal`: the same mail gets the same answer, in either
    /// lane); or this investigation timed out right after the previous
    /// attempt did WHILE the provider answered some other job in between.
    /// Every timed-out turn is a billed request (the provider may finish
    /// the answer after the deadline) that re-sends the same context, so
    /// two in a row with a success between them are the long message's
    /// verdict, not `max_attempts` tries at it. Two in a row with nothing
    /// answered since the first are as likely the provider's slow quarter
    /// hour as the message, and a job failed for that needs a person to
    /// re-triage it; the ordinary attempts, with their growing waits, leave
    /// it pending and mostly outlast the incident (deadlines never pause the
    /// account, see `is_provider_outage`). "Answered since" is the
    /// account's one flag: raised when an investigation answers (this run's
    /// own earlier turn included), lowered by every timeout, never raised
    /// by the access lane's fast model, which says nothing about this one.
    /// A timeout after a
    /// different failure (a commit race, a cooldown deferral) still gets its
    /// retry: the previous attempt's code is what the claim read back, see
    /// [`AgentJob::last_error`]. The access call keeps the ordinary
    /// allowance: its deadline is the short `fast_timeout_secs`, cut by a
    /// latency spike rather than a long message, and a failed access job
    /// leaves the message's access pending until a person re-triages it.
    fn retry_agent(&self, job: &AgentJob, code: &str) {
        let timed_out = code == crate::triage::agent::AGENT_TIMEOUT;
        let mut answered_since_last_timeout = false;
        if timed_out {
            // Raised only once an investigation has answered since the last
            // timeout (a fresh process has seen nothing); this timeout
            // lowers it again.
            answered_since_last_timeout = self
                .agent_answered_since_timeout
                .swap(false, Ordering::Relaxed);
        }
        let timed_out_twice = answered_since_last_timeout
            && job.last_error.as_deref() == Some(crate::triage::agent::AGENT_TIMEOUT);
        let next = next_attempt_wait(job.attempts, self.config.triage.agent.max_attempts);
        let wait = match next {
            Some(wait) if !crate::triage::llm::is_content_refusal(code) && !timed_out_twice => wait,
            _ => {
                self.metrics.record_agent(AgentVerdict::Failed);
                if let Err(error) = self.store.fail_agent_job(job, code) {
                    eprintln!("squelch: could not record failed triage job: {error}");
                }
                return;
            }
        };
        self.metrics.record_agent(AgentVerdict::Retryable);
        if let Err(error) = self.store.retry_agent_job(job, Utc::now() + wait, code) {
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
            "notification" => {
                (self.config.notify.fast_deadline_secs() + DEADLINE_SLACK_SECS) as i64
            }
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

/// `base` doubled `step` times, at most `cap`: the shape of every backoff
/// in this file (a job's retries, the throttle ladder), saturating rather
/// than overflowing at any step.
pub(super) fn doubling_backoff_secs(base: u64, step: u32, cap: u64) -> u64 {
    base.saturating_mul(1u64 << step.min(63)).min(cap)
}

/// A job's wait before its next attempt, after `attempts` of them: 30 s,
/// 60 s, 120 s, ... up to an hour. The retry's wait, and the hand-back's.
pub(super) fn retry_backoff_secs(attempts: u32) -> u64 {
    doubling_backoff_secs(15, attempts, 3600)
}

/// The first rung of the cooldown after a throttle (429) or a server error
/// (5xx, 529, a transport failure): short, and spread over a window, as is
/// every wait the ladder yields, so the tenants of one host, each with its
/// own engine seeing the same provider (and told the same `retry-after`),
/// do not all come back in the same second. The cooldown doubles for every
/// probe that fails in turn, `15, 30, 60, ...`, up to
/// [`AgentConfig::outage_retry_secs`], and starts over at the first success.
///
/// [`AgentConfig::outage_retry_secs`]: crate::triage::agent_config::AgentConfig::outage_retry_secs
pub(super) const THROTTLE_COOLDOWN_SECS: u64 = 15;
pub(super) const THROTTLE_COOLDOWN_JITTER_SECS: u64 = 15;

/// How long to stand off a provider that throttled or errored, given how
/// many probes have failed in a row (`strikes`, zero on the first failure
/// after a success) and the account's outage interval (`ceiling`).
///
/// The provider's own `retry-after` is believed on the first failure, from
/// a second up to the ceiling: the ceiling is the operator's "never longer
/// than this", and a gateway's hour-long header does not get to stop an
/// account's triage for an hour on one response. After the first failure
/// the header is a floor, not the answer: a throttle that keeps naming the
/// same few seconds (a spent monthly budget says `retry-after: 60` for the
/// rest of the month) would otherwise be probed every minute, each probe a
/// request. So the rung the strike count reaches,
/// [`THROTTLE_COOLDOWN_SECS`] doubled per strike, is the wait, or the
/// `retry-after` when that is longer; a jitter drawn from the clock rides
/// on either, and nothing exceeds the ceiling. The account used to wait
/// [`AgentConfig::outage_retry_secs`] (five minutes) for any 429, whatever
/// the provider asked for.
///
/// [`AgentConfig::outage_retry_secs`]: crate::triage::agent_config::AgentConfig::outage_retry_secs
pub(super) fn throttle_cooldown_secs(
    retry_after: Option<std::time::Duration>,
    strikes: u32,
    ceiling: u64,
    jitter: u64,
) -> u64 {
    let jitter = jitter.min(THROTTLE_COOLDOWN_JITTER_SECS);
    // Validation holds the interval at or above this; the floor is for a
    // caller that did not validate.
    let ceiling = ceiling.max(THROTTLE_COOLDOWN_SECS + THROTTLE_COOLDOWN_JITTER_SECS);
    // A `retry-after` of zero (an HTTP-date already past) is no number at
    // all, and takes the rung, as the multi-try path's `sleep_backoff` does.
    let asked = retry_after
        .filter(|asked| !asked.is_zero())
        .map(|asked| asked.as_secs().clamp(1, ceiling));
    let rung = doubling_backoff_secs(THROTTLE_COOLDOWN_SECS, strikes, ceiling);
    let wait = match (asked, strikes) {
        (Some(asked), 0) => asked,
        (Some(asked), _) => asked.max(rung),
        (None, _) => rung,
    };
    // The jitter rides UNDER the ceiling, not over it: at the terminal rung
    // every tenant told the same thing would otherwise wait exactly the
    // ceiling and probe in the same second, which is the case the spread is
    // for. So the ceiling is the longest wait, and the jitter's room sits
    // just below it.
    let wait = wait.min(ceiling - THROTTLE_COOLDOWN_JITTER_SECS) + jitter;
    match asked {
        // The provider's number is never undercut, on any strike: the
        // jitter and the rung spread above it, never below it, since a
        // probe before the provider's window closes is a certain throttle
        // and a wasted request. `asked` is already within the ceiling.
        Some(asked) => wait.max(asked),
        None => wait,
    }
}

/// Hands a claimed job back if its run is dropped before it reached a
/// terminal decision: the refine lane's future is dropped at shutdown (and
/// whenever `run_lanes` ends the lanes on a poll failure), and the lease,
/// sized for the whole run (eight minutes at the defaults), would otherwise
/// hold the job, and every other job on its thread, until it lapsed. The
/// hand-back writes no `last_error`, keeps the attempt the run took, and
/// makes the job due after the retry ladder's wait for that attempt
/// ([`AgentTriageStore::release_agent_job`]), so a job interrupted again
/// and again (a poll lane failing every minute under a long run, a run
/// that panics) is re-run on the ladder's spacing, not on the spot, and
/// runs out of attempts like any other rather than bouncing for ever on a
/// refunded one; and since the claim has no
/// attempts filter and a retry's check runs only after a failed attempt,
/// a job dropped on its last allowed attempt is failed here, as
/// [`INTERRUPTED`], rather than handed back. Disarmed once the run has
/// decided and attempted its terminal write, whether or not the store took
/// it: a run that decided to FAIL a job (a refusal, the attempts spent)
/// and met a busy store must not have the job handed back to be re-run,
/// and re-billed, at once; that job waits out its lease and the next claim
/// spends an attempt, as before this change. The budget unit the run
/// reserved stays spent either way: the request in flight may well have
/// been billed, and a daemon that keeps dropping runs (a crash loop) is
/// what the daily cap is for. A process that dies without dropping
/// anything (a kill, an OOM) still waits out the lease.
struct ClaimGuard<'a, S: AgentTriageStore> {
    store: &'a S,
    metrics: &'a crate::metrics::SyncMetrics,
    job: &'a AgentJob,
    max_attempts: u32,
    armed: bool,
}
impl<S: AgentTriageStore> Drop for ClaimGuard<'_, S> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        match next_attempt_wait(self.job.attempts, self.max_attempts) {
            None => {
                self.metrics.record_agent(AgentVerdict::Failed);
                let _ = self.store.fail_agent_job(self.job, INTERRUPTED);
            }
            Some(wait) => {
                self.metrics.record_agent(AgentVerdict::Retryable);
                // An access check is one short call that a door may be
                // waiting on, and the door's wait reads a parked job as
                // not waiting: its hand-back is due at once. An
                // investigation's is due on the ladder, so a lane dropped
                // again and again does not re-bill a long run on the spot.
                let due = if self.job.kind == "access" {
                    Utc::now()
                } else {
                    Utc::now() + wait
                };
                let _ = self.store.release_agent_job(self.job, due);
            }
        }
    }
}

/// What a job that has made `attempts` of its `max_attempts` gets next:
/// the wait before another, on the ladder, or `None` when the allowance is
/// spent. The one rule the retry and the hand-back share.
pub(super) fn next_attempt_wait(attempts: i64, max_attempts: u32) -> Option<ChronoDuration> {
    if attempts >= i64::from(max_attempts) {
        return None;
    }
    let attempts = u32::try_from(attempts).unwrap_or(u32::MAX);
    Some(ChronoDuration::seconds(retry_backoff_secs(attempts) as i64))
}
/// The `last_error` of a job failed by [`ClaimGuard`]: dropped on its last
/// allowed attempt.
pub(super) const INTERRUPTED: &str = "interrupted";

/// What a job's run started under: the account's last cooldown end as it
/// stood at admission (`generation`, the value of `agent_last_cooldown_end`
/// then, which only grows) and when. Both feed
/// [`SyncEngine::open_cooldown`] when the run fails.
#[derive(Clone, Copy)]
pub(super) struct JobStart {
    pub(super) generation: i64,
    pub(super) at: DateTime<Utc>,
}

/// The spread every stand-off carries, zero to
/// [`THROTTLE_COOLDOWN_JITTER_SECS`] seconds: drawn from the clock's
/// milliseconds and the account, so the tenants of one host whose failures
/// land in the same millisecond (one upstream burst) still draw different
/// numbers. Seven is odd, so adjacent accounts land in different buckets.
fn clock_jitter_secs(now: DateTime<Utc>, account: AccountId) -> u64 {
    let salt = u64::try_from(account).unwrap_or(0).wrapping_mul(7);
    (u64::from(now.timestamp_subsec_millis()).wrapping_add(salt))
        % (THROTTLE_COOLDOWN_JITTER_SECS + 1)
}

const _: () = assert!(
    THROTTLE_COOLDOWN_SECS + THROTTLE_COOLDOWN_JITTER_SECS
        == crate::triage::agent_config::MIN_OUTAGE_RETRY_SECS,
    "the outage interval's floor is the ladder's first wait"
);

/// Provider failures describe shared availability, not a permanently bad email.
pub(super) fn is_provider_outage(kind: &str) -> bool {
    crate::triage::llm::is_config_failure(kind)
        || matches!(kind, "transport" | "http_429")
        || kind
            .strip_prefix("http_")
            .and_then(|v| v.split(':').next())
            .and_then(|v| v.parse::<u16>().ok())
            .is_some_and(|status| (500..600).contains(&status))
}

/// The total limit bounds all investigations. Background work has a second
/// ceiling, leaving capacity for genuinely new inbound mail, and access
/// checks a lane of their own, whatever their trigger or lane flag, so
/// neither can spend the other's budget. Historical stage escalation caps
/// deliberately do not gate this only classification path.
pub(crate) fn investigation_budget_limits(config: &Config, job: &AgentJob) -> Vec<(String, u32)> {
    let mut limits = vec![(GLOBAL_BUDGET_KEY.into(), config.triage.agent.daily_run_cap)];
    if job.kind == "access" {
        limits.push((
            crate::triage::agent_config::ACCESS_BUDGET_KEY.into(),
            config.triage.agent.effective_access_daily_run_cap(),
        ));
    } else if !job.foreground {
        limits.push((
            crate::triage::agent_config::BACKGROUND_BUDGET_KEY.into(),
            config.triage.agent.effective_background_daily_run_cap(),
        ));
    }
    if job.trigger.starts_with("revisit:") {
        limits.push(("__agent_revisit__".into(), config.revisit.daily_cap));
    }
    // An external caller's search asked for this one. Its own ceiling, on top
    // of the two above: a sub-cap on the share of the access lane a day of
    // searches may take, so they cannot crowd out the owner's sent mail,
    // assessed from the same lane.
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
mod cooldown_tests {
    use super::*;
    use chrono::Timelike;

    const CEILING: u64 = 300;

    /// On the first failure the provider's `retry-after` is believed between
    /// a second and the ceiling, the jitter riding on it so the tenants told
    /// the same number spread out; without one the stand-off is short and
    /// spread.
    #[test]
    fn a_throttle_cooldown_is_what_the_provider_asked_or_a_short_spread_wait() {
        let secs = |s| Some(std::time::Duration::from_secs(s));
        assert_eq!(throttle_cooldown_secs(secs(7), 0, CEILING, 0), 7);
        // Zero (a past HTTP-date) is no number: the first rung.
        assert_eq!(
            throttle_cooldown_secs(secs(0), 0, CEILING, 0),
            THROTTLE_COOLDOWN_SECS
        );
        // Past the ceiling, the ceiling: the provider's number is honoured in
        // full up to it, jitter or no jitter.
        assert_eq!(throttle_cooldown_secs(secs(99_999), 0, CEILING, 0), CEILING);
        assert_eq!(throttle_cooldown_secs(secs(7), 0, CEILING, 15), 22);
        // A number within the jitter's room of the ceiling is never undercut.
        assert_eq!(throttle_cooldown_secs(secs(295), 0, CEILING, 0), 295);
        assert_eq!(throttle_cooldown_secs(secs(295), 0, CEILING, 15), CEILING);
        assert_eq!(throttle_cooldown_secs(secs(290), 0, CEILING, 15), CEILING);
        // Without a number, every value in the spread is reachable, and a
        // jitter past the room is held to it.
        let spread =
            THROTTLE_COOLDOWN_SECS..=THROTTLE_COOLDOWN_SECS + THROTTLE_COOLDOWN_JITTER_SECS;
        let seen: std::collections::BTreeSet<u64> = (0..=THROTTLE_COOLDOWN_JITTER_SECS)
            .map(|jitter| throttle_cooldown_secs(None, 0, CEILING, jitter))
            .collect();
        assert_eq!(seen.len() as u64, THROTTLE_COOLDOWN_JITTER_SECS + 1);
        assert!(seen.iter().all(|wait| spread.contains(wait)));
        assert_eq!(
            throttle_cooldown_secs(None, 0, CEILING, 999),
            THROTTLE_COOLDOWN_SECS + THROTTLE_COOLDOWN_JITTER_SECS
        );
    }

    /// The jitter is drawn from the clock's milliseconds and the account,
    /// so two tenants failing in the same millisecond draw different
    /// numbers, and every bucket is reachable for one account.
    #[test]
    fn the_jitter_spreads_tenants_that_fail_in_the_same_millisecond() {
        let at = Utc::now().with_nanosecond(0).unwrap();
        let tenants: std::collections::BTreeSet<u64> = (1..=4)
            .map(|account| clock_jitter_secs(at, account))
            .collect();
        assert_eq!(tenants.len(), 4);
        let buckets: std::collections::BTreeSet<u64> = (0..=THROTTLE_COOLDOWN_JITTER_SECS)
            .map(|m| clock_jitter_secs(at.with_nanosecond(m as u32 * 1_000_000).unwrap(), 1))
            .collect();
        assert_eq!(buckets.len() as u64, THROTTLE_COOLDOWN_JITTER_SECS + 1);
        assert!(buckets.iter().all(|j| *j <= THROTTLE_COOLDOWN_JITTER_SECS));
    }

    /// Every probe that fails in turn doubles the wait up to the outage
    /// interval, and a `retry-after` shorter than the rung no longer
    /// shortens it: a throttle that is not lifting is probed as an outage.
    #[test]
    fn a_throttle_that_keeps_failing_climbs_to_the_outage_interval() {
        let rungs: Vec<u64> = (0..8)
            .map(|strikes| throttle_cooldown_secs(None, strikes, CEILING, 0))
            .collect();
        // The terminal rung leaves the jitter its room below the ceiling.
        assert_eq!(rungs, [15, 30, 60, 120, 240, 285, 285, 285]);
        // The jitter rides on top of every rung, the terminal one included,
        // so tenants at the ceiling still spread.
        assert_eq!(throttle_cooldown_secs(None, 3, CEILING, 15), 135);
        assert_eq!(throttle_cooldown_secs(None, 5, CEILING, 15), 300);
        let at_ceiling: std::collections::BTreeSet<u64> = (0..=THROTTLE_COOLDOWN_JITTER_SECS)
            .map(|jitter| throttle_cooldown_secs(None, 9, CEILING, jitter))
            .collect();
        assert_eq!(at_ceiling.len() as u64, THROTTLE_COOLDOWN_JITTER_SECS + 1);
        assert_eq!(at_ceiling.iter().max(), Some(&CEILING));
        // A short retry-after is believed once, then is a floor...
        let secs = |s| Some(std::time::Duration::from_secs(s));
        assert_eq!(throttle_cooldown_secs(secs(60), 0, CEILING, 0), 60);
        assert_eq!(throttle_cooldown_secs(secs(60), 1, CEILING, 0), 60);
        assert_eq!(throttle_cooldown_secs(secs(60), 2, CEILING, 0), 60);
        assert_eq!(throttle_cooldown_secs(secs(60), 3, CEILING, 0), 120);
        // ...and a long one is believed up to the ceiling, on any strike.
        assert_eq!(throttle_cooldown_secs(secs(1800), 0, CEILING, 0), CEILING);
        assert_eq!(throttle_cooldown_secs(secs(1800), 5, CEILING, 0), CEILING);
        assert_eq!(throttle_cooldown_secs(secs(1800), 0, 3600, 0), 1800);
        // Within the jitter's room of the ceiling it is not undercut on a
        // later strike either.
        assert_eq!(throttle_cooldown_secs(secs(295), 3, CEILING, 0), 295);
        // The ceiling is never below the first rung and the jitter's room,
        // and a strike count past any ladder does not overflow.
        assert_eq!(throttle_cooldown_secs(None, 40, 1, 0), 15);
        assert_eq!(throttle_cooldown_secs(None, u32::MAX, 86_400, 0), 86_385);
    }

    /// The one doubling behind the ladder and a job's retries: saturating
    /// at every step, never past the cap.
    #[test]
    fn a_doubling_backoff_saturates_at_the_cap() {
        assert_eq!(doubling_backoff_secs(15, 0, 3600), 15);
        assert_eq!(doubling_backoff_secs(15, 3, 3600), 120);
        assert_eq!(doubling_backoff_secs(15, 8, 3600), 3600);
        assert_eq!(doubling_backoff_secs(15, u32::MAX, 3600), 3600);
        assert_eq!(doubling_backoff_secs(u64::MAX, 1, u64::MAX), u64::MAX);
        assert_eq!(doubling_backoff_secs(0, 70, 10), 0);
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

    fn utc_clock() -> crate::owner_clock::OwnerClock {
        crate::owner_clock::OwnerClock::new(Utc::now(), crate::owner_clock::OwnerZone::UTC)
    }

    /// #526's acceptance case. A bill saying "due tomorrow" lands at 8pm
    /// Pacific, which is 03:00 UTC the NEXT day. Read in UTC, "tomorrow" is two
    /// Pacific days out. The agent must be handed the owner's day instead: the
    /// mail arrived on Monday the 19th, so tomorrow is Tuesday the 20th.
    #[test]
    fn the_agent_reads_now_and_the_arrival_in_the_owners_zone() {
        use crate::owner_clock::{OwnerClock, OwnerZone, OwnerZoneSource};
        use chrono::TimeZone;

        let message = serde_json::json!({
            "id":1,"thread_id":"thread","from_addr":"billing@test","subject":"Your bill",
            "body":"Payment is due tomorrow.","received_at":"2026-10-20T03:00:00+00:00","is_sent":false,
            "is_spam":false,"status":"new","notify_eligible_at":null,"opened_at":null,"remind_at":null,
            "source":{"message_id":1,"content":"revision","user_state":"state","attention_revision":1}
        });
        let context: AgentContext = serde_json::from_value(serde_json::json!({
            "message":message,"thread":[],"previous_decision":null,"attention":null,"rules":[],
            "matched_rules":[],"sender_is_contact":false,"corrections":[],
            "revision":{"content":"revision","preferences":"preferences","user_state":"state","attention_revision":1},
            "memory":[],"run_metadata":{}
        }))
        .unwrap();
        let pacific = OwnerZone {
            zone: chrono_tz::Tz::America__Los_Angeles,
            source: OwnerZoneSource::Client,
        };
        let now = Utc.with_ymd_and_hms(2026, 10, 20, 3, 5, 0).unwrap();
        let snapshot = snapshot(&context, &read_job(1, 1), 8, &OwnerClock::new(now, pacific));

        assert_eq!(
            snapshot.initial["now"],
            "2026-10-19T20:05:00-07:00 (America/Los_Angeles, Monday)"
        );
        assert_eq!(snapshot.initial["timezone"], "America/Los_Angeles");
        assert_eq!(
            snapshot.initial["message_received_local"],
            "2026-10-19T20:00:00-07:00"
        );
        // The instant itself is untouched: only what the agent reads is local.
        assert_eq!(
            snapshot.initial["message"]["received_at"],
            "2026-10-20T03:00:00+00:00"
        );
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
        let snapshot = snapshot(&context, &read_job(1, 1), 2, &utc_clock());
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
        let snapshot = snapshot(&context, &job, 8, &utc_clock());
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

    /// A stored `done` is never fed back to the agent: its reason is
    /// model-authored, so a later run's starting context and a thread read
    /// carry the previous decision without it. The rest of it stays.
    #[test]
    fn a_stored_done_reason_never_reaches_the_agent() {
        let (store, account, ids) = evidence_fixture();
        let stored = crate::triage::decision::MessageDecision {
            summary: "Kept summary".into(),
            done: Some(crate::triage::decision::DoneRequest {
                reason: "PLANTED-DONE-REASON".into(),
                evidence: vec![],
            }),
            ..Default::default()
        };
        let raw = serde_json::to_string(&stored).unwrap().replace('\'', "''");
        let mut batch = String::new();
        for id in &ids {
            batch.push_str(&format!(
                "INSERT INTO agent_message_decisions(account_id,message_id,revision,decision_json,decided_at)
                 VALUES({account},{id},1,'{raw}','2026-10-01T00:00:00+00:00');"
            ));
        }
        store.execute_batch_for_test(&batch);
        let job = read_job(account, ids[0]);
        let context = store.load_agent_context(&job).unwrap();
        // The store still holds it (the owner's door serves it).
        assert!(context.previous_decision.as_ref().unwrap().done.is_some());
        let snapshot = snapshot(&context, &job, 8, &utc_clock());
        let initial = snapshot.initial.to_string();
        assert!(initial.contains("Kept summary"), "{initial}");
        assert!(!initial.contains("PLANTED-DONE-REASON"), "{initial}");
        assert!(snapshot.initial["previous_decision"]["done"].is_null());
        let reader = StoreEvidence {
            store: &store,
            account,
            limit: 8,
            sources: std::sync::Mutex::new(Vec::new()),
        };
        let read = reader
            .read(&EvidenceRequest::ReadThread {
                thread_id: "thread".into(),
            })
            .unwrap()
            .data
            .to_string();
        assert!(read.contains("Kept summary"), "{read}");
        assert!(!read.contains("PLANTED-DONE-REASON"), "{read}");
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
    /// access lane's budget, and a sub-cap of its own, so a day of agent
    /// searches runs out of ITS budget long before it touches the one the
    /// owner's sent mail is assessed from.
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
                    crate::store::ACCESS_BUDGET_KEY.into(),
                    config.triage.agent.effective_access_daily_run_cap()
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
                .stage2_budget_used(account, crate::store::ACCESS_BUDGET_KEY, "today")
                .unwrap(),
            3
        );
        // Every other access check is as it was: no sub-cap.
        job.trigger = "source_access".into();
        assert_eq!(investigation_budget_limits(&config, &job).len(), 2);
    }

    #[test]
    /// The two lanes fit under the total minus one together, so a day that
    /// spends both still leaves a run for new mail: a lowered total cuts the
    /// access lane first, splits the room when the background cap alone
    /// reaches it, and pauses the access lane only when there is no room to
    /// share.
    fn access_cannot_borrow_arrival_capacity_and_lowering_total_keeps_a_reserve() {
        let mut config = Config::default();
        let mut job = read_job(1, 1);
        job.kind = "access".into();
        job.foreground = true; // Defensive even if a malformed lane reaches us.
        let access = |config: &Config| investigation_budget_limits(config, &job)[1].clone();
        // The defaults: 200 and 500 under 10,000 leave 9,300.
        assert_eq!(access(&config), ("__agent_access__".into(), 500));
        assert_eq!(config.triage.agent.reserved_arrival_runs(), 9_300);
        // A total of 600: background keeps its 200, access gets 399, one left.
        config.triage.agent.daily_run_cap = 600;
        assert_eq!(config.triage.agent.lane_caps(), (200, 399));
        assert_eq!(config.triage.agent.reserved_arrival_runs(), 1);
        // A total of 300: background still keeps its 200, access gets 99.
        config.triage.agent.daily_run_cap = 300;
        assert_eq!(config.triage.agent.lane_caps(), (200, 99));
        // A total the background cap alone reaches: the room is shared.
        config.triage.agent.daily_run_cap = 200;
        assert_eq!(config.triage.agent.lane_caps(), (100, 99));
        assert_eq!(config.triage.agent.reserved_arrival_runs(), 1);
        // A small access lane takes only what it needs of a tight total.
        config.triage.agent.daily_run_cap = 151;
        config.triage.agent.background_daily_run_cap = 150;
        config.triage.agent.access_daily_run_cap = 10;
        assert_eq!(config.triage.agent.lane_caps(), (140, 10));
        config.triage.agent.background_daily_run_cap = 200;
        config.triage.agent.access_daily_run_cap = 500;
        // No room to share: the access lane waits for tomorrow's total.
        config.triage.agent.daily_run_cap = 2;
        assert_eq!(config.triage.agent.lane_caps(), (1, 0));
        assert_eq!(access(&config).1, 0);
        assert_eq!(config.triage.agent.reserved_arrival_runs(), 1);
        // Paused by the operator: nothing is shared with a lane set to zero.
        config.triage.agent.daily_run_cap = 200;
        config.triage.agent.access_daily_run_cap = 0;
        assert_eq!(config.triage.agent.lane_caps(), (199, 0));
    }

    /// THE MEASUREMENT behind the access lane's own key. A day of 150 access
    /// checks (spam and sent mail) and 150 backfill investigations against
    /// the 200-run background budget they used to share: 100 of the 300
    /// deferred to tomorrow, whichever came second. On their own keys, none,
    /// and each lane's own cap still holds.
    #[test]
    fn access_checks_and_the_backfill_no_longer_defer_each_other() {
        let store = SqliteStore::open_in_memory().unwrap();
        let account = store.ensure_account("lanes@example.com").unwrap();
        let config = Config::default();
        let agent = &config.triage.agent;
        let mut access = read_job(account, 1);
        access.kind = "access".into();
        access.trigger = "source_access".into();
        let backfill = read_job(account, 2);
        assert!(!backfill.foreground && backfill.kind == "triage");
        // Before: every access check charged the background key.
        let shared = vec![
            (GLOBAL_BUDGET_KEY.to_string(), agent.daily_run_cap),
            (
                crate::store::BACKGROUND_BUDGET_KEY.to_string(),
                agent.effective_background_daily_run_cap(),
            ),
        ];
        let refused = |day: &str, limits: &[(String, u32)], runs: u32| {
            (0..runs)
                .filter(|_| !store.reserve_agent_budget(account, day, limits).unwrap())
                .count()
        };
        let before = refused("before", &shared, 300);
        // After: each lane on its own key.
        let access_limits = investigation_budget_limits(&config, &access);
        let backfill_limits = investigation_budget_limits(&config, &backfill);
        assert_eq!(access_limits[1].0, crate::store::ACCESS_BUDGET_KEY);
        assert_eq!(backfill_limits[1].0, crate::store::BACKGROUND_BUDGET_KEY);
        let after = refused("after", &access_limits, 150) + refused("after", &backfill_limits, 150);
        eprintln!(
            "a day of 150 access checks and 150 backfill runs: {before} deferred on the shared \
             background budget, {after} on their own keys"
        );
        assert_eq!((before, after), (100, 0));
        let cap = agent.effective_access_daily_run_cap();
        assert_eq!(cap, 500);
        assert_eq!(refused("cap", &access_limits, cap + 10), 10);
        assert_eq!(
            store
                .stage2_budget_used(account, crate::store::BACKGROUND_BUDGET_KEY, "cap")
                .unwrap(),
            0
        );
    }

    #[test]
    fn investigation_deadline_is_not_a_shared_provider_outage() {
        assert!(!is_provider_outage("agent_timeout"));
        assert!(is_provider_outage("transport"));
        assert!(is_provider_outage("http_503"));
        assert!(is_provider_outage("http_403:permission_error"));
    }
}
