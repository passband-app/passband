//! `squelch-eval run`: replay a dataset through the production `run_agent`
//! with a candidate model, one `(case, rep)` at a time.
//!
//! Every request goes to the candidate's provider and is PAID. [`consent`]
//! names the volume and the endpoint and refuses without `--yes`.
use crate::asof::{AsOfData, AsOfReader, open_snapshot_sql};
use crate::case::{Case, Fidelity, first_user_message, read_cases};
use crate::export::{CASES_FILE, SNAPSHOT_FILE};
use crate::pricing::{Prices, builtin};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use squelch_core::config::Stage2Provider;
use squelch_core::store::SqliteStore;
use squelch_core::triage::agent::{
    AgentConnection, PROMPT_VERSION, prepare_initial_context, run_agent,
};
use squelch_core::triage::agent_config::AgentTriageConfig;
use squelch_core::triage::decision::MessageDecision;
use squelch_core::triage::llm::{self, Usage};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const CANDIDATES_FILE: &str = "candidates.toml";
pub const RESULTS_FILE: &str = "results.jsonl";
pub const ERRORS_FILE: &str = "errors.jsonl";
pub const RUN_FILE: &str = "run.json";
pub const TRACES_DIR: &str = "traces";
/// Attempts per `(case, rep)` for infrastructure failures.
pub const MAX_ATTEMPTS: u32 = 3;
/// Smallest per-stratum sample, so a rare stratum still says something.
pub const SAMPLE_FLOOR: usize = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Anthropic,
    Openai,
}

/// One `[candidate.<name>]` table. The API key itself is never stored: only
/// the name of the environment variable that holds it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub provider: Provider,
    pub model: String,
    /// Full endpoint; default is the provider's production URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// USD per million tokens; see [`Candidate::prices`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_5m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_1h: Option<f64>,
}

impl Candidate {
    pub fn stage2_provider(&self) -> Stage2Provider {
        match self.provider {
            Provider::Anthropic => Stage2Provider::Anthropic,
            Provider::Openai => Stage2Provider::OpenAI,
        }
    }

    pub fn url(&self) -> String {
        self.url
            .clone()
            .unwrap_or_else(|| llm::provider_url(self.stage2_provider()).to_owned())
    }

    /// The candidate's own `input`/`output` (cache prices derived unless
    /// given), else the built-in table with any per-field override. None
    /// means unpriced, never free.
    pub fn prices(&self) -> Option<Prices> {
        let mut prices = match (self.input, self.output) {
            (Some(input), Some(output)) => Prices::from_input_output(input, output),
            _ => builtin(&self.model)?,
        };
        if let Some(value) = self.cache_read {
            prices.cache_read = value;
        }
        if let Some(value) = self.cache_write_5m {
            prices.cache_write_5m = value;
        }
        if let Some(value) = self.cache_write_1h {
            prices.cache_write_1h = value;
        }
        Some(prices)
    }
}

#[derive(Debug, Default, Deserialize)]
struct CandidatesFile {
    #[serde(default)]
    candidate: BTreeMap<String, Candidate>,
}

pub const CANDIDATES_TEMPLATE: &str = r#"# squelch-eval candidates: one table per candidate, used as
#   squelch-eval run --candidate NAME
#
# The API key is never written here. `api_key_env` names the environment
# variable that holds it.
#
# Prices are USD per million tokens. First-party Claude models are built in;
# any other model needs `input` and `output`. Unless given, cache reads cost
# 0.1x input, 5 minute cache writes 1.25x and 1 hour writes 2x.
#
# [candidate.production]
# provider = "anthropic"
# model = "claude-opus-5-5"
# api_key_env = "ANTHROPIC_API_KEY"
#
# [candidate.sonnet-low]
# provider = "anthropic"
# model = "claude-sonnet-5-5"
# api_key_env = "ANTHROPIC_API_KEY"
# effort = "low"
#
# [candidate.other-provider]
# provider = "openai"            # any OpenAI-compatible chat completions API
# model = "vendor-model-name"
# url = "https://api.example.com/v1/chat/completions"
# api_key_env = "EXAMPLE_API_KEY"
# input = 0.50
# output = 2.00
# cache_read = 0.05
"#;

/// Write `candidates.toml` with commented examples unless it exists. Returns
/// the path and whether it was written.
pub fn init(root: &Path) -> Result<(PathBuf, bool)> {
    crate::refuse_repo_path(root)?;
    crate::private_dir(root)?;
    let path = root.join(CANDIDATES_FILE);
    if path.exists() {
        return Ok((path, false));
    }
    crate::private_write(&path, CANDIDATES_TEMPLATE.as_bytes())?;
    Ok((path, true))
}

pub fn load_candidates(root: &Path) -> Result<BTreeMap<String, Candidate>> {
    let path = root.join(CANDIDATES_FILE);
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "read {}; `squelch-eval init` writes one with examples",
            path.display()
        )
    })?;
    let file: CandidatesFile =
        toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    Ok(file.candidate)
}

/// Which reconstructions to replay. Each level includes the ones above it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FidelityFilter {
    Exact,
    /// Exact and reconstructed: everything but suspect.
    #[default]
    Reconstructed,
    All,
}

impl FidelityFilter {
    pub fn admits(self, fidelity: Fidelity) -> bool {
        match self {
            Self::Exact => fidelity == Fidelity::Exact,
            Self::Reconstructed => fidelity != Fidelity::Suspect,
            Self::All => true,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StratumSample {
    pub size: usize,
    pub sampled: usize,
    pub weight: f64,
}

/// A stratified sample: `(index into cases, weight)` in case-id order, and
/// the per-stratum arithmetic behind the weights.
#[derive(Debug, Clone)]
pub struct Selection {
    pub picks: Vec<(usize, f64)>,
    pub strata: BTreeMap<String, StratumSample>,
}

/// Stratified by `tags[0]`: every stratum gets `min(size, max(floor,
/// round(n * share)))` cases, each weighted `size / sampled`. Deterministic
/// for (cases, n, seed), so two candidates replay the same cases.
pub fn sample_cases(cases: &[Case], n: usize, seed: u64) -> Selection {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, case) in cases.iter().enumerate() {
        let stratum = case.tags.first().cloned().unwrap_or_else(|| "other".into());
        groups.entry(stratum).or_default().push(index);
    }
    let total = cases.len().max(1) as f64;
    let mut picks = Vec::new();
    let mut strata = BTreeMap::new();
    for (position, (stratum, mut members)) in groups.into_iter().enumerate() {
        let size = members.len();
        let share = (n as f64 * size as f64 / total).round() as usize;
        let take = size.min(share.max(SAMPLE_FLOOR));
        members.sort_by_key(|&index| cases[index].id);
        let stream = seed ^ (position as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        crate::SplitMix64::new(stream).shuffle(&mut members);
        let weight = size as f64 / take as f64;
        picks.extend(members.into_iter().take(take).map(|index| (index, weight)));
        strata.insert(
            stratum,
            StratumSample {
                size,
                sampled: take,
                weight,
            },
        );
    }
    picks.sort_by_key(|&(index, _)| cases[index].id);
    Selection { picks, strata }
}

/// Summed over a `(case, rep)`'s calls.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    /// Present only when some call reported the TTL split.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_1h_input_tokens: Option<u64>,
}

impl UsageTotals {
    pub fn sum(calls: &[Usage]) -> Self {
        let mut total = Self::default();
        for call in calls {
            total.input_tokens += call.input_tokens;
            total.output_tokens += call.output_tokens;
            total.cache_read_input_tokens += call.cache_read_input_tokens;
            total.cache_creation_input_tokens += call.cache_creation_input_tokens;
            if let Some(split) = call.cache_creation {
                *total.cache_creation_1h_input_tokens.get_or_insert(0) +=
                    split.ephemeral_1h_input_tokens;
            }
        }
        total
    }
}

/// One graded `(case, rep)` in `results.jsonl`. Field names are a contract
/// with `report`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultRow {
    pub prompt_id: String,
    pub rep: u32,
    pub tags: Vec<String>,
    pub weight: f64,
    pub model: String,
    /// `ok` or `failed`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<MessageDecision>,
    pub usage: UsageTotals,
    pub calls: Vec<Usage>,
    pub model_turns: usize,
    pub tool_calls: usize,
    pub latency_s: f64,
    pub grade: BTreeMap<String, f64>,
}

/// One infrastructure failure in `errors.jsonl`: never scored, retried.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorRow {
    pub prompt_id: String,
    pub rep: u32,
    pub attempt: u32,
    pub kind: String,
    /// `infra`, `config` or `harness`.
    pub class: String,
    /// Billed even though it failed; `report` counts it in cost.
    pub usage: UsageTotals,
    pub calls: Vec<Usage>,
    pub model_turns: usize,
    pub latency_s: f64,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// The candidate's answer: graded, `status: failed`.
    Model,
    /// Transport, rate limit, 5xx, or anything not about the answer: retried.
    Infra,
    /// Every case would fail the same way: abort the run.
    Config,
    /// `context_changed_during_run` means the as-of reader and the case
    /// disagree on a revision. That is a bug here, not a model result.
    Harness,
}

impl FailureClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Infra => "infra",
            Self::Config => "config",
            Self::Harness => "harness",
        }
    }
}

pub fn classify(kind: &str) -> FailureClass {
    if llm::is_config_failure(kind) {
        return FailureClass::Config;
    }
    match kind {
        "agent_turn_budget_exhausted"
        | "agent_tool_budget_exceeded"
        | "agent_review_budget_exceeded"
        | "agent_refused"
        | "agent_timeout"
        | "context_budget_exceeded" => FailureClass::Model,
        "context_changed_during_run" => FailureClass::Harness,
        _ => FailureClass::Infra,
    }
}

pub struct RunOptions {
    pub dataset: PathBuf,
    pub candidate_name: String,
    pub candidate: Candidate,
    pub reps: u32,
    pub concurrency: usize,
    /// `(n, seed)`.
    pub sample: Option<(usize, u64)>,
    pub fidelity: FidelityFilter,
    /// Restrict to these case ids.
    pub cases: Vec<i64>,
    /// Overrides the candidate's effort.
    pub effort: Option<String>,
    /// The paid-call consent.
    pub yes: bool,
    /// First infra retry waits about this long, doubling, jittered.
    pub backoff: Duration,
}

#[derive(Debug, Default)]
pub struct RunSummary {
    pub dir: PathBuf,
    pub selected: usize,
    pub already_done: usize,
    pub ok: usize,
    pub failed: usize,
    pub infra_errors: usize,
    pub unfinished: usize,
}

pub fn run_dir(dataset: &Path, name: &str, effort: Option<&str>) -> PathBuf {
    let label = match effort {
        Some(effort) => format!("{name}@{effort}"),
        None => name.to_owned(),
    };
    dataset.join("runs").join(label)
}

pub fn is_loopback(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]" | "::1"))
}

/// The paid-call gate. Prints the volume and destination; refuses without
/// `yes`.
pub fn consent(
    cases: usize,
    reps: u32,
    pending: usize,
    model: &str,
    url: &str,
    yes: bool,
) -> Result<()> {
    let host = reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.to_owned());
    eprintln!("{cases} cases x {reps} reps on {model} via {host} ({pending} runs still to make)");
    if !yes {
        bail!("these are paid model calls; rerun with --yes to make them");
    }
    Ok(())
}

/// `(prompt_id, rep)` already in `results.jsonl`.
pub fn completed_keys(path: &Path) -> Result<BTreeSet<(String, u32)>> {
    let mut keys = BTreeSet::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(keys);
    };
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        // A torn last line (a crash mid-write) is not a completed key.
        if let Ok(row) = serde_json::from_str::<Value>(line)
            && let (Some(id), Some(rep)) = (row["prompt_id"].as_str(), row["rep"].as_u64())
        {
            keys.insert((id.to_owned(), rep as u32));
        }
    }
    Ok(keys)
}

/// Append one JSON line, creating the file owner-only, and never gluing onto
/// a torn last line.
pub fn append_line<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let fresh = !path.exists();
    let torn = !fresh
        && std::fs::read(path)
            .map(|bytes| bytes.last().is_some_and(|b| *b != b'\n'))
            .unwrap_or(false);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    if fresh {
        crate::set_mode(path, 0o600)?;
    }
    let mut line = serde_json::to_string(value)?;
    if torn {
        line.insert(0, '\n');
    }
    line.push('\n');
    file.write_all(line.as_bytes())?;
    Ok(())
}

fn git_sha() -> String {
    let dir = env!("CARGO_MANIFEST_DIR");
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    match git(&["rev-parse", "HEAD"]) {
        Some(sha) => match git(&["status", "--porcelain"]) {
            Some(changes) if !changes.is_empty() => format!("{sha}-dirty"),
            _ => sha,
        },
        None => "unknown".into(),
    }
}

/// What one `(case, rep)` produced across its attempts.
struct PairOutcome {
    errors: Vec<ErrorRow>,
    row: Option<ResultRow>,
    abort: Option<String>,
}

struct Shared<'a> {
    store: &'a SqliteStore,
    data: &'a AsOfData,
    config: AgentTriageConfig,
    http: reqwest::Client,
    url: String,
    api_key: String,
    candidate: &'a Candidate,
    effort: Option<String>,
    traces: PathBuf,
    backoff: Duration,
}

pub async fn run(options: &RunOptions) -> Result<RunSummary> {
    let all = read_cases(&options.dataset.join(CASES_FILE))?;
    for id in &options.cases {
        if !all.iter().any(|case| case.id == *id) {
            bail!("no case {id} in {}", options.dataset.display());
        }
    }
    let cases: Vec<Case> = all
        .into_iter()
        .filter(|case| options.fidelity.admits(case.fidelity))
        .filter(|case| options.cases.is_empty() || options.cases.contains(&case.id))
        .collect();
    let selection = match options.sample {
        Some((n, seed)) => sample_cases(&cases, n, seed),
        None => Selection {
            picks: (0..cases.len()).map(|index| (index, 1.0)).collect(),
            strata: BTreeMap::new(),
        },
    };
    let effort = options.effort.clone().or(options.candidate.effort.clone());
    let dir = run_dir(&options.dataset, &options.candidate_name, effort.as_deref());
    let traces = dir.join(TRACES_DIR);
    crate::private_dir(&traces)?;
    let run_file = dir.join(RUN_FILE);
    if let Ok(previous) = std::fs::read_to_string(&run_file) {
        let previous: Value = serde_json::from_str(&previous)?;
        if previous["candidate"]["model"] != json!(options.candidate.model) {
            bail!(
                "{} holds a run of {}, not {}; use another candidate name",
                dir.display(),
                previous["candidate"]["model"],
                options.candidate.model
            );
        }
    }
    let done = completed_keys(&dir.join(RESULTS_FILE))?;
    let mut pending = Vec::new();
    for &(index, weight) in &selection.picks {
        for rep in 0..options.reps {
            if !done.contains(&(cases[index].id.to_string(), rep)) {
                pending.push((index, weight, rep));
            }
        }
    }
    let mut summary = RunSummary {
        dir: dir.clone(),
        selected: selection.picks.len(),
        already_done: selection.picks.len() * options.reps as usize - pending.len(),
        ..Default::default()
    };
    if pending.is_empty() {
        return Ok(summary);
    }
    let url = options.candidate.url();
    #[cfg(test)]
    if !is_loopback(&url) {
        bail!("tests only ever call a loopback provider");
    }
    consent(
        selection.picks.len(),
        options.reps,
        pending.len(),
        &options.candidate.model,
        &url,
        options.yes,
    )?;
    let api_key = match &options.candidate.api_key_env {
        Some(var) => std::env::var(var)
            .with_context(|| format!("the candidate's key is read from ${var}, which is unset"))?,
        None => String::new(),
    };
    let snapshot = options.dataset.join(SNAPSHOT_FILE);
    let store = SqliteStore::open(&snapshot).context("open dataset snapshot")?;
    let data = AsOfData::load(&open_snapshot_sql(&snapshot)?)?;
    let started = Utc::now();
    let mut meta = json!({
        "candidate_name": options.candidate_name,
        "candidate": options.candidate,
        "effort": effort,
        "url": url,
        "dataset": options.dataset.display().to_string(),
        "reps": options.reps,
        "concurrency": options.concurrency,
        "fidelity": options.fidelity,
        "cases": selection.picks.len(),
        "sample": options.sample.map(|(n, seed)| json!({
            "n": n, "seed": seed, "floor": SAMPLE_FLOOR, "strata": selection.strata,
        })),
        "prompt_version": PROMPT_VERSION,
        "harness_git_sha": git_sha(),
        "started_at": started,
        "finished_at": Value::Null,
    });
    crate::private_write(&run_file, serde_json::to_string_pretty(&meta)?.as_bytes())?;
    let shared = Shared {
        store: &store,
        data: &data,
        config: AgentTriageConfig::default(),
        http: reqwest::Client::new(),
        url,
        api_key,
        candidate: &options.candidate,
        effort,
        traces,
        backoff: options.backoff,
    };
    let mut outcomes = futures::stream::iter(
        pending
            .iter()
            .map(|&(index, weight, rep)| run_pair(&shared, &cases[index], weight, rep)),
    )
    .buffer_unordered(options.concurrency.max(1));
    let mut abort = None;
    let mut finished = 0;
    while let Some(outcome) = outcomes.next().await {
        finished += 1;
        for error in &outcome.errors {
            append_line(&dir.join(ERRORS_FILE), error)?;
            summary.infra_errors += 1;
        }
        match &outcome.row {
            Some(row) => {
                append_line(&dir.join(RESULTS_FILE), row)?;
                if row.status == "ok" {
                    summary.ok += 1;
                } else {
                    summary.failed += 1;
                }
            }
            None => summary.unfinished += 1,
        }
        if outcome.abort.is_some() {
            abort = outcome.abort;
            break;
        }
    }
    drop(outcomes);
    summary.unfinished += pending.len() - finished;
    meta["finished_at"] = json!(Utc::now());
    meta["aborted"] = json!(abort);
    crate::private_write(&run_file, serde_json::to_string_pretty(&meta)?.as_bytes())?;
    if let Some(reason) = abort {
        bail!("{reason}");
    }
    Ok(summary)
}

async fn run_pair(shared: &Shared<'_>, case: &Case, weight: f64, rep: u32) -> PairOutcome {
    let mut outcome = PairOutcome {
        errors: Vec::new(),
        row: None,
        abort: None,
    };
    let mut prepared = case.context.clone();
    prepare_initial_context(&mut prepared.initial, &shared.config.context);
    let user_message = first_user_message(&prepared).unwrap_or_default();
    for attempt in 1..=MAX_ATTEMPTS {
        let reader = AsOfReader::new(
            shared.store,
            shared.data,
            case.context.account_id,
            case.id,
            case.as_of,
            shared.config.context.max_related_messages,
        );
        let connection = AgentConnection {
            http: &shared.http,
            url: &shared.url,
            api_key: &shared.api_key,
            provider: shared.candidate.stage2_provider(),
            model: &shared.candidate.model,
            effort: shared.effort.as_deref(),
        };
        let started = Instant::now();
        let result = run_agent(connection, &shared.config, case.context.clone(), &reader).await;
        let latency_s = started.elapsed().as_secs_f64();
        let tools = reader.trace();
        let (calls, turns, ending) = match &result {
            Ok(run) => (
                run.usage.clone(),
                run.model_turns,
                json!({"decision": run.decision}),
            ),
            Err(failure) => (
                failure.usage.clone(),
                failure.model_calls,
                json!({"failure": failure.kind}),
            ),
        };
        let class = result.as_ref().err().map(|failure| classify(&failure.kind));
        // The graded attempt owns `<id>_rep<k>.json`; infra attempts keep
        // their own file so a retry does not overwrite the evidence.
        let name = if matches!(class, None | Some(FailureClass::Model)) {
            format!("{}_rep{rep}.json", case.id)
        } else {
            format!("{}_rep{rep}_attempt{attempt}.json", case.id)
        };
        let mut trace = json!({
            "prompt_version": PROMPT_VERSION,
            "case_id": case.id,
            "rep": rep,
            "attempt": attempt,
            "model": shared.candidate.model,
            "user_message": user_message,
            "tools": tools,
            "calls": calls,
            "latency_s": latency_s,
            "gold": case.gold,
        });
        for (key, value) in ending.as_object().into_iter().flatten() {
            trace[key] = value.clone();
        }
        // A trace that cannot be written must not lose the result.
        if let Ok(text) = serde_json::to_string_pretty(&trace) {
            let _ = crate::private_write(&shared.traces.join(name), text.as_bytes());
        }
        let row = |status: &str, failure: Option<String>, decision, tool_calls| ResultRow {
            prompt_id: case.id.to_string(),
            rep,
            tags: case.tags.clone(),
            weight,
            model: shared.candidate.model.clone(),
            status: status.into(),
            failure,
            decision,
            usage: UsageTotals::sum(&calls),
            calls: calls.clone(),
            model_turns: turns,
            tool_calls,
            latency_s,
            grade: BTreeMap::new(),
        };
        match result {
            Ok(run) => {
                let mut graded = row("ok", None, Some(run.decision), run.tool_calls);
                graded.grade = crate::score::grade(case, &graded);
                outcome.row = Some(graded);
                return outcome;
            }
            Err(failure) => {
                let class = classify(&failure.kind);
                if class == FailureClass::Model {
                    let mut graded = row("failed", Some(failure.kind), None, tools.len());
                    graded.grade = crate::score::grade(case, &graded);
                    outcome.row = Some(graded);
                    return outcome;
                }
                if class == FailureClass::Harness {
                    eprintln!(
                        "squelch-eval: HARNESS BUG: case {} rep {rep} hit {}; the as-of reader disagrees with the case's thread revisions",
                        case.id, failure.kind
                    );
                }
                outcome.errors.push(ErrorRow {
                    prompt_id: case.id.to_string(),
                    rep,
                    attempt,
                    kind: failure.kind.clone(),
                    class: class.as_str().into(),
                    usage: UsageTotals::sum(&calls),
                    calls: calls.clone(),
                    model_turns: turns,
                    latency_s,
                    at: Utc::now(),
                });
                if class == FailureClass::Config {
                    outcome.abort = Some(format!(
                        "the provider rejected the configuration ({}); every case would fail the same way, so the run stopped",
                        failure.kind
                    ));
                    return outcome;
                }
                if attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(backoff_delay(shared.backoff, case.id, rep, attempt)).await;
                }
            }
        }
    }
    outcome
}

/// `base * 2^(attempt-1)`, jittered into [0.5x, 1.5x) by a seeded stream so
/// concurrent retries spread out.
pub fn backoff_delay(base: Duration, case: i64, rep: u32, attempt: u32) -> Duration {
    let mut rng = crate::SplitMix64::new(
        (case as u64) ^ (u64::from(rep) << 32) ^ u64::from(attempt).wrapping_mul(0x9E37_79B9),
    );
    let jitter = 0.5 + (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
    base.mul_f64(2f64.powi(attempt as i32 - 1) * jitter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::GoldMeta;
    use crate::export::{ExportOptions, export};
    use crate::fixture::{self, Fixture};
    use std::collections::{HashMap, VecDeque};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    enum Reply {
        Valid,
        Invalid,
        Status(u16),
        ReadThread(String),
    }

    /// An OpenAI-wire provider on a loopback port. Each case (found by the
    /// `message_id` in the user message) follows its own script, then answers
    /// with a valid decision.
    #[derive(Default)]
    struct Mock {
        scripts: Mutex<HashMap<i64, VecDeque<Reply>>>,
        calls: AtomicUsize,
    }

    async fn provider(
        axum::extract::State(mock): axum::extract::State<Arc<Mock>>,
        axum::Json(request): axum::Json<Value>,
    ) -> (axum::http::StatusCode, axum::Json<Value>) {
        mock.calls.fetch_add(1, Ordering::SeqCst);
        let user = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "user")
            .unwrap()["content"]
            .as_str()
            .unwrap()
            .to_owned();
        let id = serde_json::from_str::<Value>(&user).unwrap()["context"]["message_id"]
            .as_i64()
            .unwrap();
        let reply = mock
            .scripts
            .lock()
            .unwrap()
            .get_mut(&id)
            .and_then(VecDeque::pop_front)
            .unwrap_or(Reply::Valid);
        let step = match reply {
            Reply::Status(code) => {
                return (
                    axum::http::StatusCode::from_u16(code).unwrap(),
                    axum::Json(json!({"error": {"type": "mock_error"}})),
                );
            }
            Reply::Valid => {
                json!({"step": "finish", "decision": fixture::gold(id, "candidate", true)})
            }
            Reply::Invalid => {
                let mut decision = fixture::gold(id, "candidate", true);
                decision.attention.relevant_message_ids = vec![999_999];
                json!({"step": "finish", "decision": decision})
            }
            Reply::ReadThread(thread) => json!({"step": "read_evidence",
                "requests": [{"tool": "read_thread", "thread_id": thread}]}),
        };
        (
            axum::http::StatusCode::OK,
            axum::Json(json!({
                "choices": [{"message": {"content": json!({"result": step}).to_string()}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 12, "completion_tokens": 8,
                    "prompt_tokens_details": {"cached_tokens": 2}}
            })),
        )
    }

    struct Harness {
        fixture: Fixture,
        dataset: PathBuf,
        mock: Arc<Mock>,
        url: String,
        server: tokio::task::JoinHandle<()>,
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    async fn harness(scripts: Vec<(&str, Vec<Reply>)>) -> Harness {
        let fixture = fixture::build();
        let outcome = export(&ExportOptions {
            db: fixture.db.clone(),
            out: fixture.dir.join("evals"),
            name: "set".into(),
        })
        .unwrap();
        let mock = Arc::new(Mock::default());
        for (name, replies) in scripts {
            mock.scripts
                .lock()
                .unwrap()
                .insert(fixture.ids[name], replies.into());
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let app = axum::Router::new()
            .route("/", axum::routing::post(provider))
            .with_state(mock.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Harness {
            fixture,
            dataset: outcome.dir,
            mock,
            url,
            server,
        }
    }

    fn options(h: &Harness) -> RunOptions {
        RunOptions {
            dataset: h.dataset.clone(),
            candidate_name: "mock".into(),
            candidate: Candidate {
                provider: Provider::Openai,
                model: "mock-model".into(),
                url: Some(h.url.clone()),
                api_key_env: None,
                effort: None,
                input: Some(1.0),
                output: Some(2.0),
                cache_read: None,
                cache_write_5m: None,
                cache_write_1h: None,
            },
            reps: 1,
            concurrency: 2,
            sample: None,
            fidelity: FidelityFilter::Reconstructed,
            cases: Vec::new(),
            effort: None,
            yes: true,
            backoff: Duration::from_millis(1),
        }
    }

    fn rows(h: &Harness) -> Vec<ResultRow> {
        lines(&run_dir(&h.dataset, "mock", None).join(RESULTS_FILE))
    }

    fn errors(h: &Harness) -> Vec<ErrorRow> {
        lines(&run_dir(&h.dataset, "mock", None).join(ERRORS_FILE))
    }

    fn lines<T: serde::de::DeserializeOwned>(path: &Path) -> Vec<T> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn row<'a>(rows: &'a [ResultRow], h: &Harness, name: &str) -> &'a ResultRow {
        let id = h.fixture.ids[name].to_string();
        rows.iter().find(|r| r.prompt_id == id).unwrap()
    }

    #[tokio::test]
    async fn a_full_run_writes_rows_traces_and_run_json() {
        let h = harness(vec![("B", vec![Reply::ReadThread("t1".into())])]).await;
        let summary = run(&options(&h)).await.unwrap();
        assert_eq!((summary.selected, summary.ok, summary.failed), (4, 4, 0));
        let rows = rows(&h);
        assert_eq!(rows.len(), 4, "suspect G is excluded by default");
        assert!(
            rows.iter()
                .all(|r| r.status == "ok" && r.weight == 1.0 && r.model == "mock-model")
        );
        let a = row(&rows, &h, "A");
        assert_eq!((a.model_turns, a.tool_calls, a.calls.len()), (1, 0, 1));
        assert_eq!(a.usage.input_tokens, 10);
        assert_eq!(a.usage.cache_read_input_tokens, 2);
        assert_eq!(a.tags, vec!["fye"]);
        assert!(a.decision.as_ref().unwrap().summary.contains("candidate"));
        // B read its thread: one tool call, served as of B's T without C.
        let b = row(&rows, &h, "B");
        assert_eq!(
            (b.model_turns, b.tool_calls, b.usage.input_tokens),
            (2, 1, 20)
        );
        let dir = run_dir(&h.dataset, "mock", None);
        let trace: Value = serde_json::from_str(
            &std::fs::read_to_string(
                dir.join(TRACES_DIR)
                    .join(format!("{}_rep0.json", h.fixture.ids["B"])),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(trace["prompt_version"], PROMPT_VERSION);
        assert_eq!(trace["tools"].as_array().unwrap().len(), 1);
        let seen = &trace["tools"][0]["source_message_ids"];
        assert!(
            seen.as_array()
                .unwrap()
                .contains(&json!(h.fixture.ids["A"]))
        );
        assert!(
            !seen
                .as_array()
                .unwrap()
                .contains(&json!(h.fixture.ids["C"]))
        );
        assert_eq!(
            trace["gold"]["summary"],
            "Gold summary for B, which must never leak"
        );
        assert!(trace["decision"].is_object());
        assert!(
            trace["user_message"]
                .as_str()
                .unwrap()
                .starts_with("{\"context\"")
        );
        assert_eq!(std::fs::read_dir(dir.join(TRACES_DIR)).unwrap().count(), 4);
        let meta: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(RUN_FILE)).unwrap()).unwrap();
        assert_eq!(meta["candidate"]["model"], "mock-model");
        assert!(meta["candidate"].get("api_key").is_none());
        assert!(meta["finished_at"].is_string());
        assert!(errors(&h).is_empty());
    }

    #[tokio::test]
    async fn resume_skips_completed_keys_and_never_duplicates() {
        let h = harness(vec![]).await;
        run(&options(&h)).await.unwrap();
        let calls = h.mock.calls.load(Ordering::SeqCst);
        assert_eq!(calls, 4);
        let again = run(&options(&h)).await.unwrap();
        assert_eq!((again.already_done, again.ok), (4, 0));
        assert_eq!(h.mock.calls.load(Ordering::SeqCst), calls);
        let mut more = options(&h);
        more.reps = 2;
        let second = run(&more).await.unwrap();
        assert_eq!((second.already_done, second.ok), (4, 4));
        let rows = rows(&h);
        let keys: BTreeSet<_> = rows.iter().map(|r| (r.prompt_id.clone(), r.rep)).collect();
        assert_eq!((rows.len(), keys.len()), (8, 8));
        let mut other = options(&h);
        other.candidate.model = "another-model".into();
        assert!(run(&other).await.is_err(), "a run dir belongs to one model");
    }

    #[tokio::test]
    async fn server_errors_are_retried_into_errors_jsonl_and_never_scored() {
        let h = harness(vec![
            ("A", vec![Reply::Status(500), Reply::Valid]),
            (
                "C",
                vec![Reply::Status(503), Reply::Status(500), Reply::Status(429)],
            ),
        ])
        .await;
        let summary = run(&options(&h)).await.unwrap();
        assert_eq!(
            (summary.ok, summary.unfinished, summary.infra_errors),
            (3, 1, 4)
        );
        let errors = errors(&h);
        let a = h.fixture.ids["A"].to_string();
        let c = h.fixture.ids["C"].to_string();
        let attempts = |id: &str| {
            errors
                .iter()
                .filter(|e| e.prompt_id == id)
                .map(|e| e.attempt)
                .collect::<Vec<_>>()
        };
        assert_eq!(attempts(&a), vec![1]);
        assert_eq!(attempts(&c), vec![1, 2, 3]);
        assert!(errors.iter().all(|e| e.class == "infra"));
        assert_eq!(
            errors
                .iter()
                .map(|e| e.kind.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["http_500", "http_503", "http_429"])
        );
        let rows = rows(&h);
        assert_eq!(row(&rows, &h, "A").status, "ok");
        assert!(
            !rows.iter().any(|r| r.prompt_id == c),
            "an infra failure is not a grade"
        );
        // A later resume finishes the case.
        let resumed = run(&options(&h)).await.unwrap();
        assert_eq!((resumed.already_done, resumed.ok), (3, 1));
    }

    #[tokio::test]
    async fn repair_turns_and_exhausted_budgets_are_graded_outcomes() {
        let h = harness(vec![
            ("C", vec![Reply::Invalid, Reply::Valid]),
            ("D", vec![Reply::Invalid; 4]),
        ])
        .await;
        run(&options(&h)).await.unwrap();
        let rows = rows(&h);
        let c = row(&rows, &h, "C");
        assert_eq!(
            (c.status.as_str(), c.model_turns, c.calls.len()),
            ("ok", 2, 2)
        );
        assert_eq!(c.usage.input_tokens, 20);
        assert_eq!(c.usage.output_tokens, 16);
        let d = row(&rows, &h, "D");
        assert_eq!(d.status, "failed");
        assert_eq!(d.failure.as_deref(), Some("agent_turn_budget_exhausted"));
        assert!(d.decision.is_none());
        assert_eq!((d.model_turns, d.calls.len()), (4, 4));
        assert!(errors(&h).is_empty());
    }

    #[tokio::test]
    async fn a_rejected_configuration_stops_the_run() {
        let h = harness(vec![("A", vec![Reply::Status(401)])]).await;
        let mut single = options(&h);
        single.concurrency = 1;
        let error = run(&single).await.unwrap_err().to_string();
        assert!(error.contains("http_401"), "{error}");
        let errors = errors(&h);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].class, "config");
        assert!(rows(&h).is_empty());
        assert_eq!(h.mock.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn no_paid_call_without_consent() {
        let h = harness(vec![]).await;
        let mut refused = options(&h);
        refused.yes = false;
        let error = run(&refused).await.unwrap_err().to_string();
        assert!(error.contains("--yes"), "{error}");
        assert_eq!(h.mock.calls.load(Ordering::SeqCst), 0);
        assert!(rows(&h).is_empty());
    }

    /// Every tool, aimed at the target: none returns its gold.
    #[tokio::test]
    async fn no_tool_returns_the_targets_gold() {
        let h = harness(vec![]).await;
        let snapshot = h.dataset.join(SNAPSHOT_FILE);
        let store = SqliteStore::open(&snapshot).unwrap();
        let data = AsOfData::load(&open_snapshot_sql(&snapshot).unwrap()).unwrap();
        let cases = read_cases(&h.dataset.join(CASES_FILE)).unwrap();
        use squelch_core::triage::context::{EvidenceReader, EvidenceRequest};
        for case in &cases {
            let reader = AsOfReader::new(
                &store,
                &data,
                case.context.account_id,
                case.id,
                case.as_of,
                20,
            );
            let requests = [
                EvidenceRequest::ReadMessage {
                    message_id: case.id,
                    offset: None,
                },
                EvidenceRequest::ReadThread {
                    thread_id: case.thread_id.clone(),
                },
                EvidenceRequest::SearchMail {
                    query: case.subject.clone(),
                },
                EvidenceRequest::SearchMail {
                    query: "Gold summary leak".into(),
                },
                EvidenceRequest::ReadSenderHistory {
                    sender: case.from_addr.clone(),
                },
                EvidenceRequest::ReadRecord {
                    message_id: case.id,
                },
                EvidenceRequest::ReadAttachment {
                    message_id: case.id,
                    attachment_id: "1".into(),
                },
            ];
            for request in &requests {
                let text = match reader.read(request) {
                    Ok(result) => {
                        // Revisions must agree with the case, or the run dies.
                        for (thread, revision) in &result.thread_revisions {
                            if let Some(expected) = case.context.thread_revisions.get(thread) {
                                assert_eq!(revision, expected, "case {} {request:?}", case.id);
                            }
                        }
                        result.data.to_string()
                    }
                    Err(error) => error,
                };
                for gold in [
                    &case.gold.summary,
                    &case.gold.reason,
                    &case.gold.attention.summary,
                ] {
                    assert!(
                        !text.contains(gold.as_str()),
                        "case {} {request:?} leaked {gold}",
                        case.id
                    );
                }
            }
            assert_eq!(
                reader
                    .read(&EvidenceRequest::ReadRecord {
                        message_id: case.id
                    })
                    .unwrap_err(),
                "evidence_unavailable"
            );
        }
        // A message ingested after T does not exist yet.
        let b = cases.iter().find(|c| c.id == h.fixture.ids["B"]).unwrap();
        let reader = AsOfReader::new(&store, &data, b.context.account_id, b.id, b.as_of, 20);
        let later = EvidenceRequest::ReadMessage {
            message_id: h.fixture.ids["C"],
            offset: None,
        };
        assert!(reader.read(&later).is_err());
        // An earlier decision is visible through read_record, as of T.
        let records = reader
            .read(&EvidenceRequest::ReadRecord {
                message_id: h.fixture.ids["A"],
            })
            .unwrap();
        assert_eq!(records.data, json!({"records": []}));
        let search = reader
            .read(&EvidenceRequest::ReadSenderHistory {
                sender: "alice@a.example".into(),
            })
            .unwrap();
        assert!(!search.source_message_ids.contains(&h.fixture.ids["C"]));
        // User state is B's at its T: not yet surfaced or opened.
        let own = reader
            .read(&EvidenceRequest::ReadMessage {
                message_id: b.id,
                offset: None,
            })
            .unwrap();
        assert_eq!(own.data["message"]["status"], "new");
        assert_eq!(own.data["message"]["opened_at"], Value::Null);
        let listed = search.data["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == json!(b.id))
            .unwrap();
        assert_eq!(listed["status"], "new");
        assert_eq!(reader.trace().len(), 4);
    }

    fn synthetic(id: i64, stratum: &str) -> Case {
        Case {
            id,
            thread_id: String::new(),
            as_of: Utc::now(),
            trigger: String::new(),
            fidelity: Fidelity::Exact,
            fidelity_notes: Vec::new(),
            tags: vec![stratum.into()],
            context: Default::default(),
            gold: Default::default(),
            gold_meta: GoldMeta {
                model: String::new(),
                prompt_version: String::new(),
                model_turns: 1,
                tool_calls: 0,
                usage: Vec::new(),
                decided_at: Utc::now(),
            },
            subject: String::new(),
            from_addr: String::new(),
        }
    }

    #[test]
    fn sampling_is_stratified_deterministic_and_weights_sum_to_the_population() {
        let mut cases = Vec::new();
        for (stratum, count) in [("fye", 40), ("reading", 100), ("other", 5)] {
            for _ in 0..count {
                let id = cases.len() as i64 + 1;
                cases.push(synthetic(id, stratum));
            }
        }
        let picked = sample_cases(&cases, 30, 7);
        let size = |s: &str| (picked.strata[s].size, picked.strata[s].sampled);
        assert_eq!(size("fye"), (40, 15), "floor");
        assert_eq!(size("reading"), (100, 21), "round(30 * 100/145)");
        assert_eq!(size("other"), (5, 5), "never more than the stratum");
        let total: f64 = picked.picks.iter().map(|(_, w)| w).sum();
        assert!((total - 145.0).abs() < 1e-9);
        let ids = |s: &Selection| {
            s.picks
                .iter()
                .map(|(i, _)| cases[*i].id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&picked), ids(&sample_cases(&cases, 30, 7)));
        assert_ne!(ids(&picked), ids(&sample_cases(&cases, 30, 8)));
        // Input order does not matter; ids do.
        let mut shuffled = cases.clone();
        shuffled.reverse();
        let again = sample_cases(&shuffled, 30, 7);
        let mut a = ids(&picked);
        let mut b: Vec<i64> = again.picks.iter().map(|(i, _)| shuffled[*i].id).collect();
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b);
    }

    #[test]
    fn failure_classes() {
        for kind in [
            "agent_turn_budget_exhausted",
            "agent_tool_budget_exceeded",
            "agent_review_budget_exceeded",
            "agent_refused",
            "agent_timeout",
            "context_budget_exceeded",
        ] {
            assert_eq!(classify(kind), FailureClass::Model, "{kind}");
        }
        for kind in [
            "transport",
            "http_429",
            "http_500",
            "http_529",
            "response_decode",
        ] {
            assert_eq!(classify(kind), FailureClass::Infra, "{kind}");
        }
        assert_eq!(
            classify("http_401:authentication_error"),
            FailureClass::Config
        );
        assert_eq!(classify("http_400"), FailureClass::Config);
        assert_eq!(
            classify("context_changed_during_run"),
            FailureClass::Harness
        );
        let delay = backoff_delay(Duration::from_secs(2), 7, 0, 3);
        assert!(delay >= Duration::from_secs(4) && delay < Duration::from_secs(12));
        assert_eq!(delay, backoff_delay(Duration::from_secs(2), 7, 0, 3));
    }

    #[test]
    fn candidates_file_template_parses_and_prices_resolve() {
        let root = fixture::temp_dir("candidates");
        let (path, written) = init(&root).unwrap();
        assert!(written);
        assert!(!init(&root).unwrap().1, "never overwritten");
        assert!(
            load_candidates(&root).unwrap().is_empty(),
            "examples are commented out"
        );
        let uncommented: String = CANDIDATES_TEMPLATE
            .lines()
            .map(|line| {
                line.strip_prefix("# ")
                    .filter(|l| l.starts_with('[') || l.contains(" = "))
                    .unwrap_or("")
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, uncommented).unwrap();
        let candidates = load_candidates(&root).unwrap();
        assert_eq!(candidates.len(), 3);
        assert_eq!(
            candidates["production"].prices(),
            builtin("claude-opus-5-5")
        );
        assert_eq!(candidates["sonnet-low"].effort.as_deref(), Some("low"));
        let other = candidates["other-provider"].prices().unwrap();
        assert_eq!(
            (other.input, other.cache_read, other.cache_write_1h),
            (0.5, 0.05, 1.0)
        );
        let unpriced = Candidate {
            model: "mystery".into(),
            input: None,
            output: None,
            ..candidates["other-provider"].clone()
        };
        assert!(unpriced.prices().is_none(), "unpriced is not free");
        std::fs::write(
            &path,
            "[candidate.x]\nprovider = \"anthropic\"\nmodel = \"m\"\napi_key = \"sk-secret\"\n",
        )
        .unwrap();
        assert!(load_candidates(&root).is_err(), "a literal key is rejected");
        std::fs::remove_dir_all(root).unwrap();
    }
}
