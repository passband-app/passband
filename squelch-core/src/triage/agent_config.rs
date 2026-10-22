//! Central tuning levers. Scores change ordering, never destination membership.
use serde::{Deserialize, Serialize};

/// `wake_budget.thread_id` for the account's total daily investigation runs.
/// Gmail thread ids are hex, so no real thread can collide with it.
pub const TOTAL_BUDGET_KEY: &str = "__global__";
/// `wake_budget.thread_id` for the background share of the daily runs.
pub const BACKGROUND_BUDGET_KEY: &str = "__agent_background__";
/// The access lane's own daily budget key in `wake_budget`: every `access`
/// job is charged here and NOT to the background budget, so a day of spam
/// and sent mail (every piece of either gets an access check) cannot spend
/// the budget the migration backfill and thread refreshes run on, nor they
/// its.
pub const ACCESS_BUDGET_KEY: &str = "__agent_access__";
/// Bytes of an access check's input that are not the body: sender, subject,
/// the JSON around them and the cut's two fields. What `max_context_bytes`
/// must hold beyond six bytes a character of `access_body_chars` (a control
/// character is written as `\u00XX`).
pub const ACCESS_INPUT_SLACK_BYTES: usize = 4_096;

/// Where one account stands against its daily investigation caps.
///
/// The caps here are what the worker enforces TODAY: the configured cap plus
/// any one-day grant an operator added with `squelchd triage grant`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DailyBudgetStatus {
    pub total_used: u32,
    pub total_cap: u32,
    pub background_used: u32,
    pub background_cap: u32,
    pub access_used: u32,
    pub access_cap: u32,
    /// Jobs for newly arrived mail parked until tomorrow by the total cap.
    /// Nonzero means new mail is going untriaged today.
    pub held_arrival: u64,
    /// Background jobs (backfill, refreshes) parked until tomorrow. A
    /// backlog draining at the background cap sits here by design.
    pub held_background: u64,
    /// Access checks (the owner's sent mail, spam, archive rows a caller
    /// asked to read) parked until tomorrow by a daily cap. A grant releases
    /// an archive check only within its own sub-cap's room; the client's
    /// budget-parked count leaves archive checks out.
    pub held_access: u64,
    /// Autonomous revisits parked until tomorrow. They also answer to
    /// `[revisit].daily_cap`, which a grant does not raise, so a grant never
    /// releases them.
    pub held_revisit: u64,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentTriageConfig {
    pub agent: AgentConfig,
    pub context: ContextConfig,
    pub ranking: RankingConfig,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// None uses the deployment's resolved triage model.
    pub model: Option<String>,
    pub review_model: Option<String>,
    pub max_model_turns: usize,
    pub max_tool_calls: usize,
    pub max_review_calls: usize,
    /// Wall clock for ONE model turn (the request and its answer), not the
    /// whole investigation: a run of several turns gets this per turn, and
    /// [`Self::run_deadline_secs`] in total. It used to bound the whole run,
    /// so an `xhigh` investigation that thought for two minutes across three
    /// turns timed out at 90 s and was billed for every turn it had made.
    pub timeout_secs: u64,
    pub concurrency: usize,
    /// Queue polling is independent from Gmail polling.
    pub worker_poll_secs: u64,
    /// Maximum investigations per account per UTC day; each run has its own turn cap.
    ///
    /// A runaway breaker, not a working budget: it is set far above the mail
    /// any real mailbox receives so that new mail is never left untriaged by
    /// it. Arrival spend is bounded by the mail that actually arrives,
    /// background spend by the cap below, and, hosted, all of it by the
    /// tenant's monthly LLM budget.
    pub daily_run_cap: u32,
    /// Background investigations share the total cap but cannot consume the
    /// foreground reserve. Zero pauses background work; notification is separate.
    pub background_daily_run_cap: u32,
    /// Access checks per UTC day (kind `access`: the owner's sent mail, spam,
    /// an archive row a caller asked to read), on a key of their own rather
    /// than the background budget's. Measured before this knob: every piece
    /// of spam and sent mail gets an access check, so a spammy day spent the
    /// 200-run background budget on access checks and deferred the migration
    /// backfill to tomorrow, or the backfill spent it and deferred the
    /// owner's sent mail. Zero pauses access checks; the mail stays readable
    /// by the human. Held with the background cap under the total minus one
    /// ([`Self::lane_caps`]): a lowered total cuts this lane first, or splits
    /// the room with background when that cap alone reaches it.
    pub access_daily_run_cap: u32,
    /// Access checks an external caller's search may buy for archive mail per
    /// UTC day (trigger `archive_access`). Charged IN ADDITION to the total and
    /// access caps: a sub-cap on the share of the access lane a day of
    /// searches may take, so they cannot crowd out the owner's own sent mail,
    /// which is assessed from the same lane. Zero stops surfacing; the mail
    /// stays searchable by the human.
    pub archive_access_daily_cap: u32,
    /// Failed investigations remain inspectable and can be explicitly retried.
    pub max_attempts: u32,
    /// The account's stand-off after a configuration failure (a rejected
    /// key, an unknown model), and the ceiling of the shorter, doubling
    /// cooldown a throttle or a server error gets (15 s, 30 s, 60 s, ...
    /// up to this). Outages do not consume attempts.
    pub outage_retry_secs: u64,
}
impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: None,
            review_model: None,
            max_model_turns: 4,
            max_tool_calls: 8,
            max_review_calls: 1,
            timeout_secs: 90,
            concurrency: 2,
            worker_poll_secs: 1,
            daily_run_cap: 10_000,
            background_daily_run_cap: 200,
            access_daily_run_cap: 500,
            archive_access_daily_cap: 50,
            max_attempts: 6,
            outage_retry_secs: 300,
        }
    }
}
/// The longest any triage wait may be: a model turn, the run deadline built
/// on it, the outage interval. A day, so the arithmetic on top of each (the
/// run's slack, the request's backstop, a lease) never overflows. The
/// config loader clamps to it, with a line on stderr, rather than refusing
/// a setting an operator may already have.
pub const MAX_TIMEOUT_SECS: u64 = 86_400;
/// The room a lease leaves past the deadline it covers, and a request's own
/// HTTP timeout past the turn's: enough for the store write that follows a
/// run, or for the deadline's own timer to fire first, and too little to
/// matter if it is ever used up.
pub const DEADLINE_SLACK_SECS: u64 = 30;
/// The shortest outage interval: the throttle ladder's first rung and its
/// jitter (`sync::triage_worker::THROTTLE_COOLDOWN_SECS` and the jitter
/// beside it), which the interval is the ceiling of. A shorter one would be
/// exceeded by the ladder's own first wait; the loader clamps to this too.
pub const MIN_OUTAGE_RETRY_SECS: u64 = 30;

impl AgentConfig {
    /// The whole investigation's wall clock: [`Self::timeout_secs`] per turn
    /// for every turn it may take, and one turn more for the work between
    /// turns (the store reads behind a tool call, the review call), at most
    /// a day whatever the operator wrote. The job's lease is sized from
    /// this too, so a run that uses all of it is never reclaimed from under
    /// itself.
    pub fn run_deadline_secs(&self) -> u64 {
        self.timeout_secs
            .saturating_mul((self.max_model_turns.max(1) as u64).saturating_add(1))
            .min(MAX_TIMEOUT_SECS)
    }

    /// The two lanes' caps as enforced: each at most the total minus one, and
    /// together at most that, so a day that spends both still leaves a run for
    /// first classification. The background lane keeps its cap and the access
    /// lane gets what is left; only when that would pause an access lane the
    /// operator set (the background cap alone reaches the total) is the room
    /// shared instead, the access lane held to half of it and the background
    /// lane to what that leaves, so neither is paused by a total it was never
    /// set against and neither is cut more than the total requires. Both are
    /// exposed by the API, and the loader notes at boot when they differ from
    /// what was set.
    pub fn lane_caps(&self) -> (u32, u32) {
        let room = self.daily_run_cap.saturating_sub(1);
        let background = self.background_daily_run_cap.min(room);
        let access = self.access_daily_run_cap.min(room - background);
        if access > 0 || self.access_daily_run_cap == 0 || room < 2 {
            return (background, access);
        }
        let access = self.access_daily_run_cap.min(room / 2);
        let background = self.background_daily_run_cap.min(room - access);
        (background, self.access_daily_run_cap.min(room - background))
    }

    /// The background lane's cap in force ([`Self::lane_caps`]).
    pub fn effective_background_daily_run_cap(&self) -> u32 {
        self.lane_caps().0
    }

    /// The access lane's cap in force ([`Self::lane_caps`]).
    pub fn effective_access_daily_run_cap(&self) -> u32 {
        self.lane_caps().1
    }

    /// Runs the two lanes leave the day's new mail: the total less both caps
    /// in force, at least one.
    pub fn reserved_arrival_runs(&self) -> u32 {
        let (background, access) = self.lane_caps();
        self.daily_run_cap
            .saturating_sub(background)
            .saturating_sub(access)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextConfig {
    pub initial_thread_messages: usize,
    /// Of the `initial_thread_messages` siblings (newest first), how many
    /// arrive with their whole (condensed) body. The rest carry an excerpt of
    /// `sibling_excerpt_chars` and `body_excerpted: true`; `read_thread`
    /// returns their whole (condensed) bodies and `read_message` the raw
    /// pages when the model wants them. Measured before this knob:
    /// threads of five or more messages cost 2.5x a single message's input.
    pub full_sibling_bodies: usize,
    pub sibling_excerpt_chars: usize,
    /// The most of a condensed body that rides whole into the initial
    /// context, in characters: the target's and the newest
    /// `full_sibling_bodies` siblings'. Past it the body is cut and says so
    /// the way a `read_message` page does (`body_truncated`, `next_offset`
    /// into the stored text), so the model pages the tail when it decides
    /// the tail could matter. Measured before this knob: a 55k-character
    /// newsletter rode nearly whole into every turn of its run, about 14k
    /// input tokens a turn, and the top tenth of calls by input carried half
    /// of all uncached input. The retired stages capped bodies at 6k and 12k
    /// characters; 16k keeps every ordinary message whole.
    pub full_body_chars: usize,
    /// The most of a body the access check reads, in characters, long links
    /// shortened first and quoted text kept (the caller the check is for
    /// reads the whole stored body, quotes included). Past it the body is
    /// cut and the check is told so. Measured before this knob: the check
    /// sent the raw body up to `max_context_bytes` (120 KB) and refused
    /// anything larger, so a 55k-character newsletter cost about 16k input
    /// tokens to be told it holds no credentials, and a larger one was never
    /// assessed at all. The retired stages capped bodies at 6k and 12k; 8k
    /// keeps every ordinary message whole, and a message states its codes
    /// and links near the top.
    pub access_body_chars: usize,
    pub max_related_messages: usize,
    pub max_context_bytes: usize,
    pub max_tool_result_bytes: usize,
}
impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            initial_thread_messages: 8,
            full_sibling_bodies: 2,
            sibling_excerpt_chars: 400,
            full_body_chars: 16_000,
            access_body_chars: 8_000,
            max_related_messages: 20,
            max_context_bytes: 120_000,
            max_tool_result_bytes: 24_000,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RankingConfig {
    pub urgency_weight: f64,
    pub action_need_weight: f64,
    pub personal_relevance_weight: f64,
    pub recency_weight: f64,
    pub waiting_weight: f64,
    pub importance_weight: f64,
    pub recency_half_life_hours: f64,
    pub waiting_saturation_days: f64,
}
impl Default for RankingConfig {
    fn default() -> Self {
        Self {
            urgency_weight: 30.0,
            action_need_weight: 25.0,
            personal_relevance_weight: 20.0,
            recency_weight: 20.0,
            waiting_weight: 5.0,
            importance_weight: 2.0,
            recency_half_life_hours: 24.0,
            waiting_saturation_days: 7.0,
        }
    }
}
impl AgentTriageConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.agent.daily_run_cap == 0
            || self.agent.max_attempts == 0
            || self.agent.max_model_turns == 0
            || self.agent.timeout_secs == 0
            || self.agent.concurrency == 0
            || self.agent.worker_poll_secs == 0
            || self.agent.outage_retry_secs == 0
            || self.context.max_context_bytes == 0
            || self.context.max_tool_result_bytes == 0
            || self.context.full_body_chars == 0
            || self.context.access_body_chars == 0
        {
            return Err("triage budgets must be positive".into());
        }
        self.ranking.validate()
    }
}
impl RankingConfig {
    pub fn validate(&self) -> Result<(), String> {
        if [
            self.urgency_weight,
            self.action_need_weight,
            self.personal_relevance_weight,
            self.recency_weight,
            self.waiting_weight,
            self.importance_weight,
        ]
        .iter()
        .any(|v| !v.is_finite() || *v < 0.0)
        {
            return Err("ranking weights must be finite and nonnegative".into());
        }
        if [self.recency_half_life_hours, self.waiting_saturation_days]
            .iter()
            .any(|v| !v.is_finite() || *v <= 0.0)
        {
            return Err("ranking time scales must be finite and positive".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The run's deadline is one turn's deadline for every turn it may
    /// take and one more for the work between turns, at most a day; the
    /// job's lease is sized from it.
    #[test]
    fn the_run_deadline_covers_every_turn_and_the_work_between() {
        let agent = AgentConfig::default();
        assert_eq!(agent.max_model_turns, 4);
        assert_eq!(agent.run_deadline_secs(), agent.timeout_secs * 5);
        let one = AgentConfig {
            max_model_turns: 0,
            ..Default::default()
        };
        assert_eq!(one.run_deadline_secs(), one.timeout_secs * 2);
        for absurd in [
            AgentConfig {
                timeout_secs: 100_000,
                max_model_turns: 100,
                ..Default::default()
            },
            AgentConfig {
                max_model_turns: usize::MAX,
                ..Default::default()
            },
        ] {
            assert_eq!(absurd.run_deadline_secs(), MAX_TIMEOUT_SECS);
        }
    }
}
