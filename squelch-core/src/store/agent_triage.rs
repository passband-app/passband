//! Durable jobs and human-facing projections for agent-owned triage.
//!
//! Storage validates concurrency and ownership. It does not classify mail.
use crate::triage::decision::{MessageDecision, ThreadAttentionDecision};
use crate::{error::Result, types::AccountId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// `Default` is for the synthetic job the doors and the evidence reader
/// build to call `load_agent_context` for a message: every field but the
/// account and the message is irrelevant there, and a new column should not
/// touch those sites.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentJob {
    pub id: i64,
    pub account_id: AccountId,
    pub message_id: i64,
    pub kind: String,
    pub trigger: String,
    pub lease_token: String,
    pub attempts: i64,
    /// What the previous attempt ended with, or what pushed the job back
    /// since: every retry (`retry_agent_job`) and every deferral
    /// (`defer_agent_job`: a cooldown, a spent budget) writes it, a hand-back
    /// (`release_agent_job`) leaves it, and no claim clears it. `None` on a
    /// first attempt. The worker reads it to tell a second consecutive
    /// timeout from a first; a deferral between two timeouts breaks that
    /// chain and buys the job one more try, a cheaper rule than a counter
    /// column for an interleaving that is rare.
    pub last_error: Option<String>,
    pub arrival_eligible: bool,
    /// The store's foreground lane: the first classification of a newly
    /// ingested ordinary message, a heal of such an arrival while it is
    /// still unclassified, or a person's one-message request. Independent of
    /// push eligibility. The claim serves it first, the daily budget
    /// reserves for it, and the onboarding effort follows it
    /// (`Stage2Config::effort_for_job`).
    pub foreground: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub source: AgentSourceSnapshot,
    pub id: i64,
    pub thread_id: String,
    pub from_addr: String,
    pub subject: String,
    pub body: String,
    pub received_at: String,
    /// Authored by the account, which is wider than the stored `is_sent`
    /// column: it is also true of the user's own mail that landed in the inbox.
    pub is_sent: bool,
    pub is_spam: bool,
    /// The store's `is_sent` column: Gmail's SENT label on a sighting that
    /// is not also INBOX, so mail the owner copied to themselves reads as
    /// received. Narrower than `is_sent` above, which also takes the owner's
    /// mail that landed in the inbox without an authentication verdict,
    /// which anyone can send. Not part of the model's view or the content
    /// snapshot.
    #[serde(skip)]
    pub labelled_sent: bool,
    /// Bulk mail: the message carries a `List-Unsubscribe` header. Read by
    /// the notify lane's trusted context only, so a mail-merged cold
    /// sequence cannot pass for a person writing one-to-one. Not part of
    /// the model's view or the content snapshot.
    #[serde(skip)]
    pub is_bulk: bool,
    pub status: String,
    pub notify_eligible_at: Option<String>,
    pub opened_at: Option<String>,
    pub remind_at: Option<String>,
    /// A MACHINE TRANSLATION into English of `subject` and `body`, present
    /// only for mail written in another language once the translate pass has
    /// produced one (docs/TRANSLATION.md). Context for reading the message;
    /// the original fields stay the source of truth. Hashed into the content
    /// snapshot when present (the agent door serves it beside the body, so
    /// the access assessment must have read it), and absent from the JSON
    /// when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translated_subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translated_body: Option<String>,
    /// Where From and the real sender disagree (#639): header facts the model
    /// weighs for phishing and impersonation. Not in the content snapshot:
    /// they are read off the same headers every sighting, so they never
    /// change on their own, and hashing them would re-triage every message
    /// once when this shipped. Absent when nothing disagreed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_signals: Option<crate::types::SenderSignals>,
}

/// Captured with the evidence read and checked before any decision writes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSourceSnapshot {
    pub message_id: i64,
    pub content: String,
    pub user_state: String,
    pub attention_revision: i64,
}

/// Opaque snapshots compared inside the commit transaction. These include the
/// actual content and explicit state, so all existing user-action APIs invalidate
/// stale work without requiring each caller to remember a revision increment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextRevision {
    pub content: String,
    pub preferences: String,
    pub user_state: String,
    pub attention_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentContext {
    pub message: AgentMessage,
    pub thread: Vec<AgentMessage>,
    pub previous_decision: Option<MessageDecision>,
    pub attention: Option<ThreadAttentionDecision>,
    pub rules: Vec<serde_json::Value>,
    pub matched_rules: Vec<serde_json::Value>,
    pub sender_is_contact: bool,
    /// The relationship ledger's block for the sender (docs/UNDERSTANDING.md
    /// §2.1), ABSENT rather than empty when there is no row or the first-run
    /// backfill has not filled the table. NEVER FILLED BY A CONTEXT LOAD: the
    /// worker attaches it under the `understanding` flag
    /// (`sync::triage_worker::attach_relationship`), so every other reader of
    /// a context runs without touching the ledger. Deterministic counts, no
    /// email text. NOT IN ANY REVISION FINGERPRINT, for the reason `triage.dwell_ms`
    /// is not in `user_state`: the ledger is recounted while mail is being
    /// read and triaged, and a recount changes nothing a run in flight
    /// decided, so it must not turn that run's commit stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship: Option<serde_json::Value>,
    pub corrections: Vec<serde_json::Value>,
    pub revision: ContextRevision,
    /// Reserved read-only seam. No learned-memory editor ships in this rewrite.
    pub memory: Vec<serde_json::Value>,
    /// Executor-owned model, prompt, configuration and usage metadata.
    pub run_metadata: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentCommitOutcome {
    Applied,
    Stale,
}

/// The most dwell ONE attention report may add, in milliseconds (ten minutes).
/// A report covers one stretch of a message being on screen; a longer stretch
/// than this is a window left open, not reading, and the client is not trusted
/// to have told the two apart.
pub const MAX_DWELL_REPORT_MS: u64 = 600_000;
/// The most dwell a message can ever accumulate (one hour). Past this the
/// number has stopped saying anything more ("read thoroughly" was settled long
/// before), and an unbounded sum is one buggy client loop away from a value
/// that drags a sender's median with it.
pub const MAX_DWELL_TOTAL_MS: u64 = 3_600_000;
/// The most link clicks ONE attention report may add. Nobody follows more
/// than a handful of links between two reports; a larger number is a client
/// bug or a replayed queue.
pub const MAX_LINK_CLICKS_REPORT: u32 = 20;
/// The most link clicks a message can ever accumulate. The only question the
/// total answers is "none, or some", so the ceiling exists to keep the column
/// bounded rather than to preserve precision.
pub const MAX_LINK_CLICKS_TOTAL: u32 = 1_000;

/// Explicit lifecycle and time window for Reading and Records inventories.
#[derive(Debug, Clone)]
pub struct AgentListQuery {
    /// None explicitly requests all dates. Default is the preceding 30 days.
    pub since: Option<DateTime<Utc>>,
    pub include_done: bool,
    /// Reading only: shape the answer as the owner's Reading BOARD: the
    /// `since` inventory, with its FIRST-LOAD mail (the first-run backfill's)
    /// cut to the onboarding week, at most `READING_BOARD_MAX`. Mail that is
    /// not first-load is never windowed or capped. The human door's default
    /// Reading view sets it; smart clear
    /// (which cleans the whole month), a caller that names its own dates,
    /// and the agent door do not.
    pub reading_board: bool,
}
impl Default for AgentListQuery {
    fn default() -> Self {
        Self {
            since: Some(Utc::now() - chrono::Duration::days(30)),
            include_done: false,
            reading_board: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentListItem {
    pub message_id: i64,
    pub decision_source_message_id: i64,
    pub thread_id: String,
    pub from_addr: String,
    /// The `From:` display name, so a feed card can say "Mopar.com National
    /// eStore" instead of printing the 32 hex characters its mailer uses as a
    /// local-part.
    ///
    /// HUMAN-DOOR ONLY, the same discipline as `Update::from_name` and for the
    /// same reason: this is UNTRUSTED, attacker-chosen header text. Every
    /// `external_*` listing clears it before the items leave the store, so an
    /// agent keeps seeing only the address it can verify, whatever a door's
    /// serializer goes on to pick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_name: Option<String>,
    pub subject: String,
    /// Effective list activity, capped at local ingestion; raw source dates
    /// remain available on AgentMessage and the human thread reader.
    pub received_at: String,
    pub decision: MessageDecision,
    pub attention: ThreadAttentionDecision,
    pub score: f64,
    pub unresolved_since: Option<String>,
    pub ranking: Option<crate::triage::ranking::RankBreakdown>,
}

/// Internal context reads are account scoped, but may inspect restricted mail.
/// External callers must use `agent_access_allowed` before exposing a source.
pub trait AgentTriageStore: Send + Sync {
    /// Reserve one bounded investigation in every scope atomically. A rejection
    /// charges none of the scopes; caps count investigations, not model turns.
    fn reserve_agent_budget(
        &self,
        account: AccountId,
        day: &str,
        limits: &[(String, u32)],
    ) -> Result<bool>;
    /// Refund a reservation rejected before any model work was accepted.
    fn refund_agent_budget(
        &self,
        account: AccountId,
        day: &str,
        limits: &[(String, u32)],
    ) -> Result<()>;

    fn agent_diagnostics(&self, account: AccountId, message: i64) -> Result<serde_json::Value>;
    fn human_agent_updates(
        &self,
        account: AccountId,
        query: &AgentInventoryQuery,
    ) -> Result<Vec<crate::types::AttentionUpdate>>;
    fn external_agent_fye(
        &self,
        account: AccountId,
        limit: usize,
        config: &crate::triage::agent_config::RankingConfig,
        now: DateTime<Utc>,
    ) -> Result<Vec<AgentListItem>>;
    fn external_agent_reading(
        &self,
        account: AccountId,
        limit: usize,
    ) -> Result<Vec<AgentListItem>>;
    fn external_agent_decision(&self, account: AccountId, message: i64) -> Result<MessageDecision>;
    fn external_agent_records(
        &self,
        account: AccountId,
        limit: usize,
    ) -> Result<Vec<AgentListItem>>;
    fn agent_reading_with_query(
        &self,
        account: AccountId,
        limit: usize,
        query: &AgentListQuery,
    ) -> Result<Vec<AgentListItem>>;
    fn agent_records_with_query(
        &self,
        account: AccountId,
        limit: usize,
        query: &AgentListQuery,
    ) -> Result<Vec<AgentListItem>>;
    fn external_agent_reading_with_query(
        &self,
        account: AccountId,
        limit: usize,
        query: &AgentListQuery,
    ) -> Result<Vec<AgentListItem>>;
    fn external_agent_records_with_query(
        &self,
        account: AccountId,
        limit: usize,
        query: &AgentListQuery,
    ) -> Result<Vec<AgentListItem>>;
    /// Is the row for `tracking_number` one the listing hides for certain:
    /// cleared (and not moved since), or silent by
    /// [`external_shipments_within`](crate::store::SqliteStore::external_shipments_within)'s
    /// SQL cut under the same `silence`? The agent door drops a delivery
    /// record whose row is hidden, rather than serve it off the record alone.
    fn agent_shipment_is_hidden(
        &self,
        account: AccountId,
        tracking_number: &str,
        silence: Option<crate::config::Silence>,
    ) -> Result<bool>;
    fn correct_agent_triage(
        &self,
        account: AccountId,
        message: i64,
        field: &str,
        value: &serde_json::Value,
        now: DateTime<Utc>,
    ) -> Result<()>;
    fn enqueue_agent_triage(
        &self,
        account: AccountId,
        message: i64,
        trigger: &str,
        arrival_eligible: bool,
    ) -> Result<()>;
    /// `investigation` claims full triage and access assessments;
    /// `initial_investigation` excludes autonomous revisits. Every kind gets
    /// `lease_seconds`: [`Self::claim_agent_job_sized`] with one lease.
    fn claim_agent_job(
        &self,
        account: AccountId,
        kind: &str,
        now: DateTime<Utc>,
        lease_seconds: i64,
    ) -> Result<Option<AgentJob>> {
        self.claim_agent_job_sized(account, kind, now, lease_seconds, lease_seconds)
    }
    /// The claim, with a lease of its own for an `access` job: one call
    /// under the short access deadline, where an investigation's lease
    /// covers a whole run, so a process that dies mid-check holds the thread
    /// for seconds, not the run's minutes. A claim cannot know which kind it
    /// will get before it looks, so both leases are passed and the row's
    /// kind picks.
    fn claim_agent_job_sized(
        &self,
        account: AccountId,
        kind: &str,
        now: DateTime<Utc>,
        lease_seconds: i64,
        access_lease_seconds: i64,
    ) -> Result<Option<AgentJob>>;
    /// WHEN THE NEXT JOB OF EACH OF `kinds` IS DUE: one row per kind with a
    /// pending job, carrying the time [`Self::claim_agent_job`] would first
    /// hand one out. At or before now for a queued job already available;
    /// later for one scheduled ahead (a retry) or leased until then. A READ
    /// over the rows the claim seeks, so a lane can learn it has nothing to
    /// claim, and when it next will, without taking the write lock the claim
    /// takes. A kind with nothing pending is absent.
    fn next_agent_jobs_due(
        &self,
        account: AccountId,
        kinds: &[&str],
    ) -> Result<Vec<(String, DateTime<Utc>)>>;
    fn load_agent_context(&self, job: &AgentJob) -> Result<AgentContext>;
    fn load_agent_access_message(&self, job: &AgentJob) -> Result<AgentMessage>;
    fn commit_agent_access(
        &self,
        job: &AgentJob,
        original: &AgentMessage,
        decision: &crate::triage::access::AccessDecision,
        metadata: &serde_json::Value,
    ) -> Result<AgentCommitOutcome>;
    fn commit_agent_decision(
        &self,
        job: &AgentJob,
        context: &AgentContext,
        decision: &MessageDecision,
        sources: &[AgentSourceSnapshot],
    ) -> Result<AgentCommitOutcome>;
    fn commit_agent_decision_with_policy(
        &self,
        job: &AgentJob,
        context: &AgentContext,
        decision: &MessageDecision,
        sources: &[AgentSourceSnapshot],
        policy: &crate::config::RevisitPassConfig,
    ) -> Result<AgentCommitOutcome>;
    fn correct_agent_triage_delta(
        &self,
        account: AccountId,
        message: i64,
        field: &str,
        add: &[String],
        remove: &[String],
        now: DateTime<Utc>,
    ) -> Result<()>;
    fn complete_agent_job(&self, job: &AgentJob) -> Result<bool>;
    fn fail_agent_job(&self, job: &AgentJob, error_code: &str) -> Result<bool>;
    fn retry_agent_job(
        &self,
        job: &AgentJob,
        retry_at: DateTime<Utc>,
        error_code: &str,
    ) -> Result<bool>;
    /// Make every job the daily caps parked due now, for a mailbox whose lane
    /// caps were just lifted: all but revisits, which answer to a cap of their
    /// own, and archive search checks past what their own sub-cap still has
    /// room for. Returns how many were released.
    fn release_budget_held_jobs(
        &self,
        account: AccountId,
        config: &crate::triage::agent_config::AgentConfig,
        now: DateTime<Utc>,
    ) -> Result<usize>;
    /// Requeue work that could not start, refunding only this lease's attempt.
    fn defer_agent_job(
        &self,
        job: &AgentJob,
        available_at: DateTime<Utc>,
        error_code: &str,
    ) -> Result<bool>;
    /// Hand a leased job back: queued, due at `available_at`, `last_error`
    /// as it was, the attempt the lease took KEPT. A hand-back is not a push-back: an
    /// `archive_access` check keeps its place at the head of the claim
    /// order, which reads `last_error IS NULL` (see `CLAIM_JOB_SQL`); the
    /// door's wait, which reads `attempts = 0` as untouched, sees it as
    /// picked up, which it was. The attempt is kept so a job interrupted
    /// again and again (a lane dropped on every poll failure, a run that
    /// panics) runs out of attempts like any other rather than bouncing for
    /// ever on a refunded one. The worker's drop guard, for a run dropped
    /// before it decided; a stale lease changes nothing.
    fn release_agent_job(&self, job: &AgentJob, available_at: DateTime<Utc>) -> Result<bool>;
    fn agent_fye(
        &self,
        account: AccountId,
        limit: usize,
        config: &crate::triage::agent_config::RankingConfig,
        now: DateTime<Utc>,
    ) -> Result<Vec<AgentListItem>>;
    fn acknowledge_agent_message(
        &self,
        account: AccountId,
        message: i64,
        now: DateTime<Utc>,
    ) -> Result<()>;
    /// Add one client report of reading attention to a message's totals:
    /// how long it was on screen and how many of its links were followed.
    ///
    /// ADDITIVE, because the client reports in pieces (a thread is closed and
    /// reopened, a phone and a Mac both read it) and no single report knows
    /// the total. Both the report and the stored total are clamped; see
    /// [`MAX_DWELL_REPORT_MS`] and its neighbours for why.
    ///
    /// A message that is NOT THERE and one KNOWN to be kept close (auth mail,
    /// restricted mail, a legacy sealed row) are both accepted and both write
    /// nothing, so the answer never says which of the two an id is, nor
    /// whether it is either. Mail whose access assessment is still PENDING is
    /// written: it is being read before triage reached it, and a refused
    /// report is lost for good. Every reader of these columns leaves kept-
    /// close mail out at read time, so a count on a row that later proves to
    /// be a login code is never read. That is deliberately not what
    /// [`Self::acknowledge_agent_message`] does (it is `NotFound` for a
    /// missing id): see the `/attention` handler for why a 404 is not
    /// available to this route. Such a row is refused because a login code
    /// has no reading behaviour worth keeping, and every statistic over these
    /// columns excludes that mail, so a stamp there could only ever be a
    /// stored fact about restricted mail that nothing reads.
    ///
    /// Touches neither `opened_at` nor anything else in the `user_state`
    /// fingerprint, so a report can never turn an in-flight triage commit
    /// `Stale`.
    fn record_message_attention(
        &self,
        account: AccountId,
        message: i64,
        dwell_ms: u64,
        link_clicks: u32,
    ) -> Result<()>;
    fn agent_records(&self, account: AccountId, limit: usize) -> Result<Vec<AgentListItem>>;
    fn agent_reading(&self, account: AccountId, limit: usize) -> Result<Vec<AgentListItem>>;
    fn agent_access_allowed(&self, account: AccountId, message: i64) -> Result<bool>;
    fn snapshot_agent_sources(
        &self,
        account: AccountId,
        ids: &[i64],
    ) -> Result<Vec<AgentSourceSnapshot>>;
    fn agent_thread_context(&self, account: AccountId, thread: &str) -> Result<AgentContext>;
    fn agent_read_message(&self, account: AccountId, message: i64) -> Result<AgentMessage>;
    /// [`Self::agent_read_message`] for the HUMAN door's message open, which
    /// may read anything the account holds. It differs on archive mail only:
    /// the row is returned (the triage agent's read answers `NotFound` for an
    /// archive row nobody has asked about) and opening it queues nothing.
    fn human_read_message(&self, account: AccountId, message: i64) -> Result<AgentMessage>;
    fn agent_read_thread(
        &self,
        account: AccountId,
        thread: &str,
        limit: usize,
    ) -> Result<Vec<AgentMessage>>;
    fn agent_search_mail(
        &self,
        account: AccountId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<AgentMessage>>;
    fn agent_sender_history(
        &self,
        account: AccountId,
        sender: &str,
        limit: usize,
    ) -> Result<Vec<AgentMessage>>;
    /// One-time, push-silent historical queue. Existing explicit user state is untouched.
    fn initialize_agent_cutover(&self, account: AccountId, since: DateTime<Utc>) -> Result<usize>;
}

/// Filters for the human mail inventory. None of these grant external access.
#[derive(Debug, Clone)]
pub struct AgentInventoryQuery {
    pub since: DateTime<Utc>,
    pub min_importance: Option<u8>,
    pub status: Option<crate::types::AttentionStatus>,
    pub band: Option<crate::store::SitrepBand>,
    pub pending_reminders: bool,
    pub spam: crate::store::SpamScope,
    pub ranking: crate::triage::agent_config::RankingConfig,
    /// KEYSET PAGING for the mail list's infinite scroll: only mail that
    /// arrived strictly before this (received_at, id), in the listing's own
    /// newest-first order. Unlike an offset it does not shift when new mail
    /// lands at the head between two pages.
    pub before: Option<(DateTime<Utc>, i64)>,
    /// Stop after this many rows. Honoured only where the listing's order is
    /// the SQL order (no standing-band or reminder re-sort), so the first N
    /// produced are the first N served.
    pub limit: Option<usize>,
}
