//! THE PROFILE PASS (docs/UNDERSTANDING.md §2.3): once a day per account, at
//! the owner's local 03:00, one model call reads what the owner's own
//! behaviour says about them and keeps, revises, retires or adds the typed
//! entries in `memory_entries` ([`super::memory`]).
//!
//! GATED, IN THIS ORDER, and every gate before the call costs nothing:
//!
//! 1. The `understanding` hidden feature is on. Off is as if it did not exist.
//! 2. The dwell ledger covers at least [`sender_fatigue::MIN_COVERED_DAYS`].
//!    Under that, "opened 2 of 38" is a young ledger talking, not the owner.
//! 3. ONE SUCCESSFUL RUN PER ACCOUNT PER OWNER-LOCAL DAY, and at most
//!    [`ATTEMPTS_PER_DAY`] attempts, counted on the [`BUDGET_KEY`] sentinel in
//!    `wake_budget` BEFORE the call, like every other cap here: a retry storm
//!    (a crash loop, a daemon restarted ten times) cannot buy a second run.
//! 4. Something changed since the last run: the owner replied to something,
//!    corrected triage, or a live entry is within
//!    [`memory::EXPIRY_WARNING_DAYS`] of lapsing. A quiet day costs nothing.
//!
//! WHAT IT READS is [`ProfileInput`], built by the [`ProfileInputSource`] seam
//! so the relationship ledger (§2.1) can replace today's tables without this
//! file changing. Sealed and auth mail are absent STRUCTURALLY: the source's
//! SQL never selects a thread that holds any (`sealed_mail_sql!`), so there is
//! nothing for a prompt to leak. And of the mail it does read, only the
//! OWNER'S OWN WORDS: the first [`OWNER_REPLY_CHARS`] of their reply, quote cut
//! off, never the inbound body. That is both the stronger signal and the
//! smaller injection surface.
//!
//! WHAT IT WRITES is checked op by op ([`memory::validate_ops`]) inside the
//! store transaction that applies it, so nothing the validator checked can move
//! before the write.

use super::llm::{self, LlmOutcome, LlmRequest, Usage};
use super::memory::{self, MemoryRecord, Origin, ProfileOps};
use super::sender_fatigue;
use crate::config::Stage2Provider;
use crate::error::Result;
use crate::features::{self, Features};
use crate::owner_clock::OwnerZone;
use crate::store::Store;
use crate::types::AccountId;
use chrono::{DateTime, Duration, Timelike, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// The prompt's version, stored nowhere yet but bumped with every edit to
/// `prompts/profile-v1.txt`, so a run's diagnostics can tell prompts apart
/// once they are recorded.
pub const PROMPT_VERSION: &str = "profile-v1";
const SYSTEM_TEMPLATE: &str = include_str!("prompts/profile-v1.txt");

/// The system prompt, with the cap filled in from the constant the validator
/// enforces, so the two cannot disagree. Composed once per process: the
/// stable prefix is what the provider caches, and it must be identical bytes
/// on every call.
pub fn system_prompt() -> &'static str {
    static SYSTEM: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        SYSTEM_TEMPLATE.replacen(
            "{max_live_entries}",
            &memory::MAX_LIVE_ENTRIES.to_string(),
            1,
        )
    });
    SYSTEM.as_str()
}

/// The `wake_budget` sentinel the pass counts its attempts on. Its own key, for
/// the reason every lane has one: nothing else's backlog may spend it, and it
/// may spend nothing else's. Gmail thread ids are hex, so no thread collides.
pub const BUDGET_KEY: &str = "__memory_profile__";
/// Attempts per OWNER-LOCAL day. Two, so one transport failure is retried
/// once; never more, because each is a stage-2-sized call.
///
/// EVERY DAILY RULE HERE IS KEYED ON THE OWNER'S LOCAL DATE, the attempt
/// sentinel's `wake_budget.day` included, never the UTC date. The pass is due
/// at local 03:00, so a UTC key splits one local night across two UTC days: an
/// owner in California would get two attempts before 17:00 and two more after,
/// and a run at local 20:00 (after UTC midnight) would mark tomorrow's local
/// run as done. Usage alone stays on the UTC day, because the usage ledger is
/// UTC everywhere.
pub const ATTEMPTS_PER_DAY: u32 = 2;
/// `app_settings` keys: the instant of the last successful run (the change
/// gate's "since"); the owner-local date of that success (the once-a-day
/// check); and the owner-local date the pass last finished deciding about (so
/// a skipped day is not re-checked every refine round until midnight).
pub const RAN_AT_KEY: &str = "memory_profile_ran_at";
pub const RAN_DAY_KEY: &str = "memory_profile_ran_day";
pub const DAY_KEY: &str = "memory_profile_day";
/// When the pass may next try after a CONFIGURATION failure (a rejected key,
/// a model the gateway's allow-list does not carry, a schema the provider
/// refuses). Such a failure is refunded, because it spent nothing, and that
/// is exactly why it needs a cooldown of its own: refunded and unstamped, it
/// would be retried every refine round all day, a request and a log line
/// each time. Kept in `app_settings` rather than an engine atomic (the
/// translate pass's `translate_retry_after`) so the pass owns all of its own
/// gates and a restart does not reopen it.
pub const RETRY_AFTER_KEY: &str = "memory_profile_retry_after";
/// The cooldown after a configuration failure: long enough that a broken
/// allow-list costs a handful of requests a day, short enough that a fix
/// made in the afternoon still gets tonight's run.
pub const CONFIG_COOLDOWN: Duration = Duration::hours(1);
/// The owner-local hour the pass becomes due. After the ledger's own nightly
/// work, and before the owner is up.
pub const DUE_HOUR: u32 = 3;
/// What one run is shown, at most.
pub const MAX_CONTACTS: usize = 40;
pub const MAX_LOW_ATTENTION: usize = 20;
pub const MAX_GROUPS: usize = 20;
pub const MAX_GROUP_MEMBERS_SHOWN: usize = 12;
pub const MAX_ATTENDEES: usize = 30;
pub const MAX_REPLIED_THREADS: usize = 30;
pub const MAX_PARTICIPANTS: usize = 8;
pub const LOOKBACK_DAYS: i64 = 30;
pub const OWNER_REPLY_CHARS: usize = 500;
pub const MAX_SUBJECT_CHARS: usize = 160;
/// The byte budget of the whole user message, like the agent context's.
pub const MAX_INPUT_BYTES: usize = 48_000;
/// The model's output ceiling: forty entries of a few hundred bytes each.
pub const MAX_OUTPUT_TOKENS: u32 = 8_000;

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ContactRow {
    pub addr: String,
    pub name: Option<String>,
    pub sent_count: i64,
    pub last_sent_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct LowAttentionSender {
    pub addr: String,
    pub received: u32,
    pub opened: u32,
    pub cleared_unopened: u32,
    /// `sender_fatigue::score` over the same counts, so an `ignores` entry
    /// rests on the number the purge flow shows, never on dwell alone.
    pub fatigue_score: Option<u8>,
    pub reasons: Vec<sender_fatigue::Reason>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct GroupRow {
    pub name: String,
    pub member_count: usize,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Attendee {
    pub addr: String,
    pub meetings: i64,
}

/// One thread the owner answered: what it was about, who was on it, and what
/// THE OWNER wrote. Never the inbound body.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct RepliedThread {
    /// The owner's reply, citable as provenance.
    pub message_id: i64,
    pub subject: String,
    pub participants: Vec<String>,
    pub replied_at: String,
    pub owner_reply: String,
}

/// An entry as the pass is shown it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EntryView {
    pub id: String,
    pub origin: Origin,
    pub kind: String,
    pub scope: String,
    pub fields: Value,
    pub line: String,
    pub confidence: f64,
    pub affirmed_at: String,
    pub expires_at: Option<String>,
}

impl EntryView {
    pub fn of(record: &MemoryRecord) -> Self {
        Self {
            id: record.id.clone(),
            origin: record.origin,
            kind: record.kind.clone(),
            scope: record.scope.clone(),
            fields: record.body.clone(),
            line: record.line.clone(),
            confidence: record.confidence,
            affirmed_at: record.affirmed_at.to_rfc3339(),
            expires_at: record.expires_at.map(|d| d.to_rfc3339()),
        }
    }
}

/// Everything one run is shown. The store-derived lists come from a
/// [`ProfileInputSource`]; the pass adds the clock, the coverage and the
/// current entries.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ProfileInput {
    pub today: String,
    pub coverage_days: u32,
    pub contacts: Vec<ContactRow>,
    pub low_attention_senders: Vec<LowAttentionSender>,
    pub send_groups: Vec<GroupRow>,
    pub meeting_attendees: Vec<Attendee>,
    pub replied_threads: Vec<RepliedThread>,
    pub entries: Vec<EntryView>,
}

impl ProfileInput {
    /// The message ids this run shows the model: the only ids a `new` entry
    /// may cite (the validator's [`memory::Facts::exposed_ids`]).
    pub fn exposed_ids(&self) -> BTreeSet<i64> {
        self.replied_threads.iter().map(|t| t.message_id).collect()
    }

    /// Hold the encoded input under `max_bytes`, dropping the oldest replied
    /// threads first (they are the bulk), then the tails of the lists. An
    /// input that still does not fit is an error: the entries alone are
    /// bounded by the cap, so that is a bug, not a big mailbox.
    pub fn fit(&mut self, max_bytes: usize) -> std::result::Result<String, &'static str> {
        loop {
            let encoded = serde_json::to_string(self).map_err(|_| "input_encode")?;
            if encoded.len() <= max_bytes {
                return Ok(encoded);
            }
            if self.replied_threads.pop().is_some() {
                continue;
            }
            if self.contacts.pop().is_some()
                | self.low_attention_senders.pop().is_some()
                | self.meeting_attendees.pop().is_some()
                | self.send_groups.pop().is_some()
            {
                continue;
            }
            return Err("input_budget_exceeded");
        }
    }
}

/// What a source is asked for.
#[derive(Debug, Clone, Copy)]
pub struct ProfileInputOptions {
    /// Read calendar attendees: the `calendar` feature is on.
    pub calendar: bool,
}

/// THE SEAM. Today's source reads `contacts`, `senders`, `send_groups`,
/// `calendar_events` and the owner's replies; the relationship ledger
/// (UNDERSTANDING.md §2.1) implements the same method over its own table
/// when it lands, and nothing downstream changes.
///
/// A source MUST leave sealed and auth mail out in its own SQL, and must never
/// put an inbound body into [`RepliedThread::owner_reply`].
pub trait ProfileInputSource {
    fn profile_input(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
        options: ProfileInputOptions,
    ) -> Result<ProfileInput>;
}

/// The owner's own words in a reply: the text before the quoted earlier
/// messages, links and long numbers blanked, cut to [`OWNER_REPLY_CHARS`].
/// `None` for a forward, where the "quoted" part is somebody else's message
/// and there is no telling the owner's words from it.
pub fn owner_words(body: &str) -> Option<String> {
    let own = super::condense::own_text(body)?;
    let out = crate::text::truncate_chars(&blank_sensitive(&own), OWNER_REPLY_CHARS);
    (!out.trim().is_empty()).then_some(out)
}

/// Every piece of mail text the pass is shown goes through this: the owner's
/// reply AND the thread's subject (a subject is where a code, a tracking link
/// or a case number sits most often). Whitespace folds to single spaces; a
/// word that is a link becomes `[link]` and one carrying four or more digits
/// becomes `[number]`. A code, a phone number, an account number: none of it
/// is anything a profile should be built from, and a code is exactly what
/// the prompt says never to repeat.
pub fn blank_sensitive(text: &str) -> String {
    text.split_whitespace()
        .map(|word| {
            let lower = word.to_ascii_lowercase();
            if lower.contains("://") || lower.starts_with("www.") {
                "[link]"
            } else if word.chars().filter(char::is_ascii_digit).count() >= 4 {
                "[number]"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The output schema: strict JSON, one flat entry shape for every kind (most
/// fields null), because a per-kind `anyOf` would multiply the grammar the
/// provider compiles and the validator re-checks every field anyway.
pub fn output_schema() -> Value {
    let nullable = |t: Value| json!({"anyOf": [t, {"type": "null"}]});
    let string = json!({"type": "string"});
    let kinds: Vec<&str> = memory::MemoryKind::ALL.iter().map(|k| k.as_str()).collect();
    let relations: Vec<&str> = memory::Relation::ALL.iter().map(|r| r.as_str()).collect();
    let entry = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["scope", "kind", "what", "name", "note", "until", "addr", "relation",
                     "basis", "confidence", "message_ids", "addrs"],
        "properties": {
            "scope": string,
            "kind": {"type": "string", "enum": kinds},
            "what": nullable(string.clone()),
            "name": nullable(string.clone()),
            "note": nullable(string.clone()),
            "until": nullable(string.clone()),
            "addr": nullable(string.clone()),
            "relation": nullable(json!({"type": "string", "enum": relations})),
            "basis": nullable(string.clone()),
            "confidence": {"type": "number"},
            "message_ids": {"type": "array", "items": {"type": "integer"}},
            "addrs": {"type": "array", "items": {"type": "string"}},
        }
    });
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["ops"],
        "properties": {
            "ops": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["op", "id", "entry"],
                    "properties": {
                        "op": {"type": "string", "enum": ["keep", "revise", "retire", "new"]},
                        "id": nullable(string),
                        "entry": nullable(entry),
                    }
                }
            }
        }
    })
}

/// What one model call came back with: the ops or a redacted failure kind,
/// and every usage the provider reported (a failed call still spent tokens).
pub struct ProfileCall {
    pub result: std::result::Result<ProfileOps, String>,
    pub usage: Vec<Usage>,
    /// Calls made, so one that reported no usage (a timeout) is still billed
    /// as a call.
    pub calls: usize,
}

/// The model behind the pass. A trait so the gates can be tested without a
/// socket: the budget test's whole point is that the second run never gets
/// here.
#[async_trait::async_trait]
pub trait ProfileModel: Send + Sync {
    async fn propose(&self, user: &str) -> ProfileCall;
}

/// The production model: the configured investigation model, through the same
/// transport and usage accounting every other pass uses ([`llm`]).
pub struct LlmProfileModel {
    pub http: reqwest::Client,
    pub url: String,
    pub api_key: String,
    pub provider: Stage2Provider,
    pub model: String,
    pub effort: Option<String>,
    pub timeout: std::time::Duration,
}

#[async_trait::async_trait]
impl ProfileModel for LlmProfileModel {
    async fn propose(&self, user: &str) -> ProfileCall {
        let request = LlmRequest {
            model: &self.model,
            system: system_prompt(),
            user,
            schema: output_schema(),
            effort: self.effort.as_deref(),
            // One attempt: the daily attempt cap is this pass's retry policy,
            // and a client-side retry would spend a second call inside one
            // charge.
            max_tries: 1,
            max_tokens: Some(MAX_OUTPUT_TOKENS),
            disable_thinking: false,
        };
        let call = tokio::time::timeout(
            self.timeout + std::time::Duration::from_secs(5),
            llm::classify_llm_within(
                &self.http,
                &self.url,
                &self.api_key,
                self.provider,
                &request,
                self.timeout,
            ),
        )
        .await;
        let (result, usage) = match call {
            Err(_) => (Err("profile_timeout".to_string()), None),
            Ok(Err(error)) => (Err(error.kind), None),
            Ok(Ok(outcome)) => {
                let usage = outcome.usage();
                let result = match outcome {
                    LlmOutcome::Ok(text, _) => serde_json::from_str::<ProfileOps>(&text)
                        .map_err(|_| "json_parse".to_string()),
                    LlmOutcome::Refused(_) => Err("profile_refused".to_string()),
                    LlmOutcome::Failed(kind, _) => Err(kind),
                };
                (result, usage)
            }
        };
        ProfileCall {
            result,
            usage: usage.into_iter().collect(),
            calls: 1,
        }
    }
}

/// How one invocation of the pass ended.
#[derive(Debug, Clone, PartialEq)]
pub enum PassOutcome {
    /// Before the owner's 03:00, or today's decision is already made.
    NotDue,
    /// The `understanding` feature is off.
    Off,
    /// A configuration failure is cooling down ([`RETRY_AFTER_KEY`]).
    CoolingDown,
    NotEnoughCoverage {
        days: f32,
    },
    /// A run already succeeded this owner-local day, or its attempts are spent.
    AlreadyRan,
    Unchanged,
    /// The model call or the store failed; `kind` is redacted.
    Failed {
        kind: String,
    },
    Applied(ApplyReport),
}

/// What an applied document did.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ApplyReport {
    pub kept: usize,
    pub revised: usize,
    pub retired: usize,
    pub added: usize,
    pub rejected: Vec<(usize, memory::Rejection)>,
}

fn setting_instant<S: Store + ?Sized>(
    store: &S,
    account: AccountId,
    key: &str,
) -> Option<DateTime<Utc>> {
    store
        .get_app_setting(account, key)
        .ok()
        .flatten()
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|d| d.with_timezone(&Utc))
}

/// One invocation of the pass: every gate, then at most one model call, billed
/// to the `profile` usage line (every usage the provider reported, and a bare
/// call for one that reported none, as the translate pass bills).
pub async fn run_pass<S: Store + ?Sized>(
    store: &S,
    account: AccountId,
    now: DateTime<Utc>,
    zone: OwnerZone,
    features: &Features,
    model: &dyn ProfileModel,
) -> PassOutcome {
    let local = now.with_timezone(&zone.zone);
    let local_day = local.date_naive().to_string();
    if local.hour() < DUE_HOUR
        || store
            .get_app_setting(account, DAY_KEY)
            .ok()
            .flatten()
            .as_deref()
            == Some(local_day.as_str())
    {
        return PassOutcome::NotDue;
    }
    // 1. The feature. NOT stamped: a flag ticked at noon should run today.
    if !features.enabled(features::UNDERSTANDING) {
        return PassOutcome::Off;
    }
    if setting_instant(store, account, RETRY_AFTER_KEY).is_some_and(|until| now < until) {
        return PassOutcome::CoolingDown;
    }
    let decided = |outcome: PassOutcome| {
        // Best-effort: a stamp that fails to write costs one re-check next
        // round, never a second run (the sentinel below holds that).
        let _ = store.set_app_setting(account, DAY_KEY, &local_day);
        outcome
    };
    // 2. Coverage.
    let coverage = match store.memory_coverage_days(account, now) {
        Ok(days) => days,
        Err(e) => {
            return PassOutcome::Failed {
                kind: format!("store: {e}"),
            };
        }
    };
    if coverage < sender_fatigue::MIN_COVERED_DAYS {
        return decided(PassOutcome::NotEnoughCoverage { days: coverage });
    }
    // 3. Once an owner-local day, and the attempt cap (see ATTEMPTS_PER_DAY).
    let utc_day = now.format("%Y-%m-%d").to_string();
    let ran_at = setting_instant(store, account, RAN_AT_KEY);
    if store
        .get_app_setting(account, RAN_DAY_KEY)
        .ok()
        .flatten()
        .as_deref()
        == Some(local_day.as_str())
    {
        return decided(PassOutcome::AlreadyRan);
    }
    match store.stage2_budget_used(account, BUDGET_KEY, &local_day) {
        Ok(used) if used >= ATTEMPTS_PER_DAY => return decided(PassOutcome::AlreadyRan),
        Ok(_) => {}
        Err(e) => {
            return PassOutcome::Failed {
                kind: format!("store: {e}"),
            };
        }
    }
    // 4. Something changed.
    match store.profile_changed_since(account, ran_at, now) {
        Ok(true) => {}
        Ok(false) => return decided(PassOutcome::Unchanged),
        Err(e) => {
            return PassOutcome::Failed {
                kind: format!("store: {e}"),
            };
        }
    }

    let options = ProfileInputOptions {
        calendar: features.enabled(features::CALENDAR),
    };
    let mut input = match store.profile_input(account, now, options) {
        Ok(input) => input,
        Err(e) => {
            return PassOutcome::Failed {
                kind: format!("store: {e}"),
            };
        }
    };
    let entries = match store.memory_entries(account) {
        Ok(entries) => entries,
        Err(e) => {
            return PassOutcome::Failed {
                kind: format!("store: {e}"),
            };
        }
    };
    input.today = local_day.clone();
    input.coverage_days = coverage.floor() as u32;
    input.entries = entries
        .iter()
        .filter(|r| r.is_live(now))
        .map(EntryView::of)
        .collect();
    let user = match input.fit(MAX_INPUT_BYTES) {
        Ok(user) => user,
        Err(kind) => return decided(PassOutcome::Failed { kind: kind.into() }),
    };
    let exposed = input.exposed_ids();

    // CHARGED BEFORE THE CALL, so a call that never comes back still counts.
    if let Err(e) = store.stage2_increment_budget(account, BUDGET_KEY, &local_day) {
        return PassOutcome::Failed {
            kind: format!("budget: {e}"),
        };
    }
    let call = model.propose(&user).await;
    let unreported = call.calls.saturating_sub(call.usage.len());
    for tokens in call
        .usage
        .iter()
        .copied()
        .map(crate::store::UsageTokens::from)
        .chain(std::iter::repeat_n(
            crate::store::UsageTokens::default(),
            unreported,
        ))
    {
        if let Err(e) = store.extract_bump_usage(
            account,
            &utc_day,
            crate::metrics::PROFILE_USAGE_CATEGORY,
            tokens,
        ) {
            eprintln!("squelch: profile usage not recorded ({e})");
        }
    }
    let ops = match call.result {
        Ok(ops) => ops,
        Err(kind) => {
            // A configuration failure spent nothing and is the same for every
            // try: give the attempt back, as every other cap here does.
            // It also cools down, or the refund would make it retry every
            // round (see RETRY_AFTER_KEY).
            if llm::is_config_failure(&kind) {
                let _ = store.stage2_refund_budget(account, BUDGET_KEY, &local_day);
                let _ = store.set_app_setting(
                    account,
                    RETRY_AFTER_KEY,
                    &(now + CONFIG_COOLDOWN).to_rfc3339(),
                );
            }
            return PassOutcome::Failed { kind };
        }
    };
    match store.apply_profile_ops(account, &ops.ops, &exposed, now) {
        Ok(report) => {
            let _ = store.set_app_setting(account, RAN_AT_KEY, &now.to_rfc3339());
            let _ = store.set_app_setting(account, RAN_DAY_KEY, &local_day);
            decided(PassOutcome::Applied(report))
        }
        Err(e) => PassOutcome::Failed {
            kind: format!("store: {e}"),
        },
    }
}

/// The change gate's expiry arm, pure: a live inferred entry lapsing within
/// [`memory::EXPIRY_WARNING_DAYS`].
pub fn any_expiring(entries: &[MemoryRecord], now: DateTime<Utc>) -> bool {
    entries.iter().any(|r| {
        r.is_live(now)
            && r.expires_at
                .is_some_and(|at| at - now <= Duration::days(memory::EXPIRY_WARNING_DAYS))
    })
}

#[cfg(test)]
mod tests;
