//! `squelch-eval report`: score runs against gold, compare them, and cost
//! them. Every number is recomputed from the raw rows: a stored `grade` is
//! never read back, so a row from an older scorer cannot skew a table.
//!
//! `squelch-eval adjudicate` records the owner's ruling on one field of one
//! case; `report` then counts a ruled candidate value as agreement for every
//! run that gave that value.
use crate::case::{Case, Fidelity, read_cases};
use crate::export::{CASES_FILE, MANIFEST_FILE};
use crate::pricing::{Prices, builtin};
use crate::run::{
    Candidate, ERRORS_FILE, ErrorRow, RESULTS_FILE, RUN_FILE, ResultRow, TRACES_DIR, append_line,
};
use crate::score::{
    ADJUDICABLE, Accepted, Context as ScoreContext, DIRECTIONAL, Estimate, FACTORS, METRICS,
    SCORER_VERSION, SECTIONS, Score, TextSimilarity, USER_TEXT_METRICS, field_value, paired_delta,
    score, style, weighted_mean,
};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use squelch_core::triage::decision::MessageDecision;
use squelch_core::triage::llm::Usage;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

pub const ADJUDICATIONS_FILE: &str = "adjudications.jsonl";
pub const DEFAULT_SEED: u64 = 0x5EED;
/// Disagreements on these fields are listed by `--diff`.
pub const DIFF_FIELDS: [&str; 6] = ["fye", "restricted", "auth", "push", "reading", "records"];
/// The per-stratum and per-fidelity breakdown columns.
pub const HEADLINE: [&str; 5] = ["fye", "reading", "auth", "restricted", "push"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// The candidate's value is right.
    Candidate,
    /// Gold stands.
    Gold,
    /// Both are acceptable.
    Either,
}

/// One line of `adjudications.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ruling {
    pub case_id: i64,
    pub field: String,
    pub verdict: Verdict,
    /// The run whose value was ruled on.
    pub run: String,
    /// That run's value on the field; the ruling applies to any run that
    /// gave the same value.
    pub value: Value,
    pub at: DateTime<Utc>,
}

/// Accepted candidate values per case and field. For each (case, field,
/// value) the latest ruling wins, so a later `gold` revokes an earlier
/// `candidate`.
#[derive(Debug, Default, Clone)]
pub struct Adjudications {
    accepted: HashMap<i64, Accepted>,
    empty: Accepted,
}

impl Adjudications {
    pub fn from_rulings(rulings: &[Ruling]) -> Self {
        let mut latest: BTreeMap<(i64, String, String), (Verdict, Value)> = BTreeMap::new();
        for ruling in rulings {
            latest.insert(
                (
                    ruling.case_id,
                    ruling.field.clone(),
                    ruling.value.to_string(),
                ),
                (ruling.verdict, ruling.value.clone()),
            );
        }
        let mut accepted: HashMap<i64, Accepted> = HashMap::new();
        for ((case, field, _), (verdict, value)) in latest {
            if verdict != Verdict::Gold {
                accepted
                    .entry(case)
                    .or_default()
                    .entry(field)
                    .or_default()
                    .push(value);
            }
        }
        Self {
            accepted,
            empty: Accepted::new(),
        }
    }

    pub fn load(dataset: &Path) -> Result<Self> {
        let rulings: Vec<Ruling> = read_lines(&dataset.join(ADJUDICATIONS_FILE))?;
        Ok(Self::from_rulings(&rulings))
    }

    pub fn for_case(&self, case: i64) -> &Accepted {
        self.accepted.get(&case).unwrap_or(&self.empty)
    }

    pub fn len(&self) -> usize {
        self.accepted.values().map(|a| a.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn read_lines<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(line) {
            Ok(value) => out.push(value),
            // A torn last line is a crash mid-append, not data.
            Err(_) if index + 1 == text.lines().count() => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("{} line {}", path.display(), index + 1));
            }
        }
    }
    Ok(out)
}

/// A run as written by `run`.
#[derive(Debug, Clone)]
pub struct RunData {
    pub name: String,
    pub dir: PathBuf,
    pub meta: Value,
    pub rows: Vec<ResultRow>,
    pub errors: Vec<ErrorRow>,
    pub prices: Option<Prices>,
    pub model: String,
}

impl RunData {
    /// `spec` is a directory name under `<dataset>/runs/` or a path.
    pub fn load(dataset: &Path, spec: &str) -> Result<Self> {
        let under = dataset.join("runs").join(spec);
        let dir = if under.is_dir() {
            under
        } else {
            PathBuf::from(spec)
        };
        let meta: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(RUN_FILE))
                .with_context(|| format!("no run at {}", dir.display()))?,
        )?;
        let candidate: Option<Candidate> = serde_json::from_value(meta["candidate"].clone()).ok();
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| spec.to_owned());
        Ok(Self {
            name,
            rows: read_lines(&dir.join(RESULTS_FILE))?,
            errors: read_lines(&dir.join(ERRORS_FILE))?,
            prices: candidate.as_ref().and_then(Candidate::prices),
            model: candidate.map(|c| c.model).unwrap_or_default(),
            meta,
            dir,
        })
    }
}

/// Every run directory under the dataset, by name.
pub fn list_runs(dataset: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dataset.join("runs"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().join(RUN_FILE).exists())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Record a ruling on `field` of `case_id`, taking the value from `run`'s
/// first successful row for that case.
pub fn adjudicate(
    dataset: &Path,
    case_id: i64,
    field: &str,
    verdict: Verdict,
    run: &str,
) -> Result<Ruling> {
    if !ADJUDICABLE.contains(&field) {
        bail!(
            "{field} cannot be adjudicated; one of: {}",
            ADJUDICABLE.join(", ")
        );
    }
    let data = RunData::load(dataset, run)?;
    let id = case_id.to_string();
    let decision = data
        .rows
        .iter()
        .filter(|row| row.prompt_id == id)
        .find_map(|row| row.decision.as_ref())
        .with_context(|| format!("{} has no successful row for case {case_id}", data.name))?;
    let ruling = Ruling {
        case_id,
        field: field.to_owned(),
        verdict,
        run: data.name.clone(),
        value: field_value(field, decision).context("field has no value")?,
        at: Utc::now(),
    };
    append_line(&dataset.join(ADJUDICATIONS_FILE), &ruling)?;
    Ok(ruling)
}

/// One case's rows in a run, reps averaged.
#[derive(Debug, Clone, Default)]
pub struct CaseAgg {
    pub weight: f64,
    pub reps: usize,
    pub metrics: BTreeMap<String, f64>,
    pub harm: f64,
    pub rank_delta: Option<f64>,
    pub cost: Option<f64>,
    pub projected: Option<f64>,
    /// Input, output, cache read, cache write.
    pub tokens: [f64; 4],
    pub turns: f64,
    pub tool_share: f64,
    /// Mean absolute error per attention factor, over successful reps.
    pub factor_errors: BTreeMap<String, f64>,
    /// Share of successful reps whose user-facing text has an em dash.
    pub em_dash: Option<f64>,
    /// Candidate characters over gold characters (user-facing text).
    pub length_ratio: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct Directional {
    pub gold_positives: usize,
    pub recall: Option<f64>,
    pub precision: Option<f64>,
    pub false_positive_rate: Option<f64>,
    pub misses: usize,
    pub false_alarms: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Costs {
    pub observed_per_1k: Option<f64>,
    pub projected_per_1k: Option<f64>,
    pub wasted: Option<f64>,
    pub total: Option<f64>,
    /// Why a cost is missing.
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RunStats {
    pub name: String,
    pub model: String,
    pub cases: usize,
    pub rows: usize,
    pub failed: usize,
    pub infra_errors: usize,
    pub unknown_cases: usize,
    pub metrics: BTreeMap<String, Estimate>,
    pub harm: Option<Estimate>,
    pub rank_delta: Option<Estimate>,
    pub directional: BTreeMap<String, Directional>,
    pub turns: Option<f64>,
    pub tool_rate: Option<f64>,
    pub latency_p50: Option<f64>,
    pub latency_p95: Option<f64>,
    pub tokens: [Option<f64>; 4],
    pub costs: Costs,
    pub factor_mae: BTreeMap<String, f64>,
    pub em_dash_rate: Option<f64>,
    /// Median over cases.
    pub length_ratio: Option<f64>,
    pub per_case: BTreeMap<i64, CaseAgg>,
    /// Per scored row: (case, rep, score).
    pub scores: Vec<(i64, u32, Score)>,
}

#[derive(Debug, Clone, Default)]
pub struct ProductionStats {
    pub model: String,
    pub runs: usize,
    pub turns: Option<f64>,
    pub tool_rate: Option<f64>,
    pub tokens: [Option<f64>; 4],
    pub costs: Costs,
    /// Gold's own em dash rate in user-facing text.
    pub em_dash_rate: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct DiffEntry {
    pub case: i64,
    pub rep: u32,
    pub harm: f64,
    pub from_addr: String,
    pub subject: String,
    /// (field, gold value, candidate value)
    pub fields: Vec<(String, String, String)>,
    /// Every other metric below 1 for this row, in [`METRICS`] order.
    pub others: Vec<(String, f64)>,
    /// (label, gold text, candidate text): summary, push title, push body.
    pub texts: Vec<(String, String, String)>,
    pub gold_reason: String,
    pub candidate_reason: String,
    pub trace: PathBuf,
}

/// A `--texts` entry: one row's user-facing text next to gold's.
#[derive(Debug, Clone)]
pub struct TextEntry {
    pub case: i64,
    pub rep: u32,
    pub subject: String,
    pub mean_similarity: f64,
    pub texts: Vec<(String, String, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct ReportOptions {
    pub runs: Vec<String>,
    pub baseline: Option<String>,
    pub diff: Option<String>,
    pub cache_hit: Option<f64>,
    pub seed: u64,
    /// With `diff`: list this many rows with the least similar user text.
    pub texts: Option<usize>,
    /// Skip text similarity (no embedder).
    pub no_text: bool,
}

/// One stratum or fidelity group: (cases, headline means, mean harm).
pub type GroupStats = (usize, BTreeMap<String, f64>, f64);

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub header: Vec<String>,
    pub hit: Option<f64>,
    pub production: Option<ProductionStats>,
    pub runs: Vec<RunStats>,
    pub baseline: Option<String>,
    /// Per non-baseline run: metric -> paired delta.
    pub deltas: BTreeMap<String, BTreeMap<String, Estimate>>,
    /// Run -> "stratum:<s>" / "fidelity:<f>" -> (cases, headline means, harm).
    pub breakdown: BTreeMap<String, BTreeMap<String, GroupStats>>,
    pub diff_run: Option<String>,
    pub diff: Vec<DiffEntry>,
    /// `--texts`: the rows to read, or why there are none.
    pub texts: Option<Result<Vec<TextEntry>, String>>,
}

fn sum_usage(calls: &[Usage]) -> [f64; 4] {
    calls.iter().fold([0.0; 4], |acc, u| {
        [
            acc[0] + u.input_tokens as f64,
            acc[1] + u.output_tokens as f64,
            acc[2] + u.cache_read_input_tokens as f64,
            acc[3] + u.cache_creation_input_tokens as f64,
        ]
    })
}

/// Nearest-rank percentile.
fn percentile(values: &mut [f64], p: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let rank = ((p * values.len() as f64).ceil() as usize).clamp(1, values.len());
    Some(values[rank - 1])
}

fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, n) = values.fold((0.0, 0usize), |(s, n), v| (s + v, n + 1));
    (n > 0).then(|| sum / n as f64)
}

/// Score one run against the cases. Pure: tests feed it directly.
pub fn run_stats(
    run: &RunData,
    cases: &HashMap<i64, &Case>,
    adjudications: &Adjudications,
    hit: Option<f64>,
    seed: u64,
    sim: Option<&dyn TextSimilarity>,
) -> RunStats {
    let mut stats = RunStats {
        name: run.name.clone(),
        model: run.model.clone(),
        rows: run.rows.len(),
        infra_errors: run.errors.len(),
        ..Default::default()
    };
    let mut by_case: BTreeMap<i64, Vec<(&ResultRow, Score)>> = BTreeMap::new();
    for row in &run.rows {
        let Some(case) = row
            .prompt_id
            .parse()
            .ok()
            .and_then(|id: i64| cases.get(&id))
        else {
            stats.unknown_cases += 1;
            continue;
        };
        if row.status != "ok" {
            stats.failed += 1;
        }
        let scored = score(
            &case.gold,
            row.decision.as_ref(),
            adjudications.for_case(case.id),
            &ScoreContext::for_case(case, sim),
        );
        stats.scores.push((case.id, row.rep, scored.clone()));
        by_case.entry(case.id).or_default().push((row, scored));
    }
    stats.cases = by_case.len();
    let fields = DIRECTIONAL.len();
    let mut cells = vec![(0.0, 0.0, 0.0, 0.0); fields];
    let mut raw = vec![(0usize, 0usize, 0usize); fields];
    let mut latencies = Vec::new();
    for (id, rows) in &by_case {
        let case = cases[id];
        let weight = rows[0].0.weight;
        let reps = rows.len();
        let share = weight / reps as f64;
        let mut agg = CaseAgg {
            weight,
            reps,
            ..Default::default()
        };
        for metric in METRICS {
            let values: Vec<f64> = rows
                .iter()
                .filter_map(|(_, s)| s.metrics.get(*metric).copied())
                .collect();
            if let Some(value) = mean(values.into_iter()) {
                agg.metrics.insert((*metric).into(), value);
            }
        }
        agg.harm = mean(rows.iter().map(|(_, s)| s.harm)).unwrap_or(0.0);
        agg.rank_delta = mean(rows.iter().filter_map(|(_, s)| s.rank_delta));
        for factor in FACTORS {
            if let Some(error) = mean(
                rows.iter()
                    .filter_map(|(_, s)| s.factor_errors.get(factor).copied()),
            ) {
                agg.factor_errors.insert(factor.into(), error);
            }
        }
        let (_, gold_chars) = style(&case.gold);
        let decided: Vec<(bool, usize)> = rows
            .iter()
            .filter_map(|(row, _)| row.decision.as_ref().map(style))
            .collect();
        agg.em_dash = mean(decided.iter().map(|(dash, _)| f64::from(u8::from(*dash))));
        if gold_chars > 0 {
            agg.length_ratio = mean(
                decided
                    .iter()
                    .map(|(_, chars)| *chars as f64 / gold_chars as f64),
            );
        }
        let n = reps as f64;
        for (row, scored) in rows {
            let tokens = sum_usage(&row.calls);
            for (slot, value) in agg.tokens.iter_mut().zip(tokens) {
                *slot += value / n;
            }
            agg.turns += row.model_turns as f64 / n;
            agg.tool_share += f64::from(u8::from(row.tool_calls > 0)) / n;
            latencies.push(row.latency_s);
            for (index, field) in DIRECTIONAL.iter().enumerate() {
                let (gold, candidate) = scored.directional[*field];
                let cell = &mut cells[index];
                match (gold, candidate) {
                    (true, true) => cell.0 += share,
                    (true, false) => {
                        cell.1 += share;
                        raw[index].1 += 1;
                    }
                    (false, true) => {
                        cell.2 += share;
                        raw[index].2 += 1;
                    }
                    (false, false) => cell.3 += share,
                }
                raw[index].0 += usize::from(gold);
            }
        }
        if let Some(prices) = &run.prices {
            let costs: Vec<f64> = rows
                .iter()
                .map(|(row, _)| row.calls.iter().map(|c| prices.cost(c)).sum())
                .collect();
            agg.cost = mean(costs.into_iter());
            if let Some(hit) = hit {
                agg.projected = mean(rows.iter().map(|(row, _)| {
                    row.calls
                        .iter()
                        .map(|c| prices.projected_cost(c, hit))
                        .sum()
                }));
            }
        }
        stats.per_case.insert(*id, agg);
    }
    let pairs = |pick: &dyn Fn(&CaseAgg) -> Option<f64>| -> Vec<(f64, f64)> {
        stats
            .per_case
            .values()
            .filter_map(|agg| pick(agg).map(|value| (agg.weight, value)))
            .collect()
    };
    for (index, metric) in METRICS.iter().enumerate() {
        let values = pairs(&|agg: &CaseAgg| agg.metrics.get(*metric).copied());
        if let Some(estimate) = Estimate::of(&values, seed.wrapping_add(index as u64)) {
            stats.metrics.insert((*metric).into(), estimate);
        }
    }
    stats.harm = Estimate::of(&pairs(&|agg: &CaseAgg| Some(agg.harm)), seed ^ 0xA);
    stats.rank_delta = Estimate::of(&pairs(&|agg: &CaseAgg| agg.rank_delta), seed ^ 0xB);
    stats.turns = weighted_mean(&pairs(&|agg: &CaseAgg| Some(agg.turns)));
    stats.tool_rate = weighted_mean(&pairs(&|agg: &CaseAgg| Some(agg.tool_share)));
    for slot in 0..4 {
        stats.tokens[slot] = weighted_mean(&pairs(&|agg: &CaseAgg| Some(agg.tokens[slot])));
    }
    for factor in FACTORS {
        if let Some(mae) = weighted_mean(&pairs(&|agg: &CaseAgg| {
            agg.factor_errors.get(factor).copied()
        })) {
            stats.factor_mae.insert(factor.into(), mae);
        }
    }
    stats.em_dash_rate = weighted_mean(&pairs(&|agg: &CaseAgg| agg.em_dash));
    let mut ratios: Vec<f64> = stats
        .per_case
        .values()
        .filter_map(|a| a.length_ratio)
        .collect();
    stats.length_ratio = percentile(&mut ratios, 0.5);
    stats.latency_p50 = percentile(&mut latencies.clone(), 0.5);
    stats.latency_p95 = percentile(&mut latencies, 0.95);
    for (index, field) in DIRECTIONAL.iter().enumerate() {
        let (tp, missed, false_alarm, tn) = cells[index];
        let ratio = |a: f64, b: f64| (a + b > 0.0).then(|| a / (a + b));
        stats.directional.insert(
            (*field).into(),
            Directional {
                gold_positives: raw[index].0,
                recall: ratio(tp, missed),
                precision: ratio(tp, false_alarm),
                false_positive_rate: ratio(false_alarm, tn),
                misses: raw[index].1,
                false_alarms: raw[index].2,
            },
        );
    }
    stats.costs = run_costs(run, &stats);
    stats
}

fn run_costs(run: &RunData, stats: &RunStats) -> Costs {
    let Some(prices) = &run.prices else {
        return Costs {
            note: Some(format!(
                "no price for {}; add one to candidates.toml",
                run.model
            )),
            ..Default::default()
        };
    };
    let recorded: u64 = run
        .rows
        .iter()
        .flat_map(|row| &row.calls)
        .map(|c| c.input_tokens + c.cache_read_input_tokens + c.cache_creation_input_tokens)
        .sum();
    if recorded == 0 && !run.rows.is_empty() {
        return Costs {
            note: Some(
                "no input tokens recorded: a runner bug, not a result; cost withheld".into(),
            ),
            ..Default::default()
        };
    }
    let per_case = |pick: &dyn Fn(&CaseAgg) -> Option<f64>| {
        let values: Vec<(f64, f64)> = stats
            .per_case
            .values()
            .filter_map(|agg| pick(agg).map(|v| (agg.weight, v)))
            .collect();
        weighted_mean(&values).map(|mean| mean * 1000.0)
    };
    let wasted: f64 = run
        .errors
        .iter()
        .flat_map(|error| &error.calls)
        .map(|c| prices.cost(c))
        .sum();
    let scored: f64 = run
        .rows
        .iter()
        .flat_map(|row| &row.calls)
        .map(|c| prices.cost(c))
        .sum();
    Costs {
        observed_per_1k: per_case(&|agg: &CaseAgg| agg.cost),
        projected_per_1k: per_case(&|agg: &CaseAgg| agg.projected),
        wasted: Some(wasted),
        total: Some(scored + wasted),
        note: None,
    }
}

/// What the mailbox costs today: the gold runs on the current gold model,
/// priced at that model.
pub fn production_stats(cases: &[Case], model: &str, hit: Option<f64>) -> ProductionStats {
    let runs: Vec<&Case> = cases
        .iter()
        .filter(|c| c.gold_meta.model == model)
        .collect();
    let mut stats = ProductionStats {
        model: model.to_owned(),
        runs: runs.len(),
        turns: mean(runs.iter().map(|c| c.gold_meta.model_turns as f64)),
        tool_rate: mean(
            runs.iter()
                .map(|c| f64::from(u8::from(c.gold_meta.tool_calls > 0))),
        ),
        em_dash_rate: mean(runs.iter().map(|c| f64::from(u8::from(style(&c.gold).0)))),
        ..Default::default()
    };
    for slot in 0..4 {
        stats.tokens[slot] = mean(runs.iter().map(|c| sum_usage(&c.gold_meta.usage)[slot]));
    }
    stats.costs = match builtin(model) {
        Some(prices) => {
            let observed = mean(runs.iter().map(|c| {
                c.gold_meta
                    .usage
                    .iter()
                    .map(|u| prices.cost(u))
                    .sum::<f64>()
            }));
            let projected = hit.and_then(|hit| {
                mean(runs.iter().map(|c| {
                    c.gold_meta
                        .usage
                        .iter()
                        .map(|u| prices.projected_cost(u, hit))
                        .sum::<f64>()
                }))
            });
            Costs {
                observed_per_1k: observed.map(|v| v * 1000.0),
                projected_per_1k: projected.map(|v| v * 1000.0),
                ..Default::default()
            }
        }
        None => Costs {
            note: Some(format!("no built-in price for {model}")),
            ..Default::default()
        },
    };
    stats
}

/// Build the report from loaded data. Pure. `sim` scores the free text;
/// `text_line` says how (or why not) and becomes the header's last line.
#[allow(clippy::too_many_arguments)]
pub fn build(
    dataset_name: &str,
    cases: &[Case],
    manifest: &Value,
    runs: &[RunData],
    adjudications: &Adjudications,
    options: &ReportOptions,
    sim: Option<&dyn TextSimilarity>,
    text_line: &str,
) -> Result<Report> {
    let by_id: HashMap<i64, &Case> = cases.iter().map(|c| (c.id, c)).collect();
    let current_model = manifest["production_cache_hit"]["model"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| {
            cases
                .iter()
                .max_by_key(|c| c.as_of)
                .map(|c| c.gold_meta.model.clone())
        })
        .unwrap_or_default();
    let measured = manifest["production_cache_hit"]["share"].as_f64();
    let (hit, hit_line) = match (options.cache_hit, measured) {
        (Some(hit), _) => (
            Some(hit),
            format!(
                "Projected cost assumes a {:.1}% prefix cache hit share (from --cache-hit).",
                hit * 100.0
            ),
        ),
        (None, Some(hit)) => (
            Some(hit),
            format!(
                "Projected cost assumes a {:.1}% prefix cache hit share, measured over the gold runs on {current_model} (manifest.json).",
                hit * 100.0
            ),
        ),
        (None, None) => (
            None,
            "No cache hit share is known, so projected cost is omitted; pass --cache-hit.".into(),
        ),
    };
    let count = |f: Fidelity| cases.iter().filter(|c| c.fidelity == f).count();
    let mut report = Report {
        header: vec![
            format!(
                "Dataset {dataset_name}: {} cases ({} exact, {} reconstructed, {} suspect); scorer v{SCORER_VERSION}; {} adjudicated field(s).",
                cases.len(),
                count(Fidelity::Exact),
                count(Fidelity::Reconstructed),
                count(Fidelity::Suspect),
                adjudications.len(),
            ),
            hit_line,
            format!(
                "Caveat: gold is one sample from the incumbent model ({current_model}), so agreement rewards imitating it. Read --diff and adjudicate before trusting a gap."
            ),
            text_line.to_owned(),
        ],
        hit,
        production: Some(production_stats(cases, &current_model, hit)),
        baseline: options.baseline.clone(),
        diff_run: options.diff.clone(),
        ..Default::default()
    };
    for run in runs {
        report.runs.push(run_stats(
            run,
            &by_id,
            adjudications,
            hit,
            options.seed,
            sim,
        ));
    }
    if let Some(baseline) = &options.baseline {
        let base = report
            .runs
            .iter()
            .find(|r| &r.name == baseline)
            .with_context(|| format!("baseline {baseline} is not among the runs"))?;
        let values = |stats: &RunStats, metric: &str| -> BTreeMap<i64, (f64, f64)> {
            stats
                .per_case
                .iter()
                .filter_map(|(id, agg)| {
                    let value = if metric == "harm" {
                        Some(agg.harm)
                    } else {
                        agg.metrics.get(metric).copied()
                    };
                    value.map(|v| (*id, (agg.weight, v)))
                })
                .collect()
        };
        for stats in report.runs.iter().filter(|r| &r.name != baseline) {
            let mut deltas = BTreeMap::new();
            for (index, metric) in METRICS.iter().chain(["harm"].iter()).enumerate() {
                if let Some(delta) = paired_delta(
                    &values(stats, metric),
                    &values(base, metric),
                    options.seed.wrapping_add(100 + index as u64),
                ) {
                    deltas.insert((*metric).to_owned(), delta);
                }
            }
            report.deltas.insert(stats.name.clone(), deltas);
        }
    }
    for stats in &report.runs {
        let mut groups: BTreeMap<String, Vec<&CaseAgg>> = BTreeMap::new();
        for (id, agg) in &stats.per_case {
            let case = by_id[id];
            groups
                .entry(format!("stratum:{}", case.tags[0]))
                .or_default()
                .push(agg);
            groups
                .entry(format!("fidelity:{}", case.fidelity.as_str()))
                .or_default()
                .push(agg);
        }
        let mut table = BTreeMap::new();
        for (group, aggs) in groups {
            let mut means = BTreeMap::new();
            for metric in HEADLINE {
                let values: Vec<(f64, f64)> = aggs
                    .iter()
                    .filter_map(|a| a.metrics.get(metric).map(|v| (a.weight, *v)))
                    .collect();
                if let Some(value) = weighted_mean(&values) {
                    means.insert(metric.to_owned(), value);
                }
            }
            let harm: Vec<(f64, f64)> = aggs.iter().map(|a| (a.weight, a.harm)).collect();
            table.insert(
                group,
                (aggs.len(), means, weighted_mean(&harm).unwrap_or(0.0)),
            );
        }
        report.breakdown.insert(stats.name.clone(), table);
    }
    if options.texts.is_some() && options.diff.is_none() {
        bail!("--texts reads one run's text; name it with --diff RUN");
    }
    if let Some(name) = &options.diff {
        let (stats, run) = report
            .runs
            .iter()
            .zip(runs)
            .find(|(s, _)| &s.name == name)
            .with_context(|| format!("--diff {name} is not among the runs"))?;
        report.diff = diff_entries(stats, run, &by_id);
        if let Some(n) = options.texts {
            report.texts = Some(if sim.is_some() {
                Ok(text_entries(stats, run, &by_id, n))
            } else {
                Err("text similarity is unavailable, so there is nothing to rank by".into())
            });
        }
    }
    Ok(report)
}

fn row_for(run: &RunData, case: i64, rep: u32) -> &ResultRow {
    run.rows
        .iter()
        .find(|r| r.prompt_id == case.to_string() && r.rep == rep)
        .expect("scored row")
}

/// Summary and push text, gold next to the candidate.
fn headline_texts(
    gold: &MessageDecision,
    candidate: Option<&MessageDecision>,
) -> Vec<(String, String, String)> {
    let pick = |f: fn(&MessageDecision) -> &str| candidate.map(f).unwrap_or("(failed)").to_owned();
    vec![
        ("summary".into(), gold.summary.clone(), pick(|d| &d.summary)),
        (
            "push title".into(),
            gold.notification.title.clone(),
            pick(|d| &d.notification.title),
        ),
        (
            "push body".into(),
            gold.notification.body.clone(),
            pick(|d| &d.notification.body),
        ),
    ]
}

fn diff_entries(stats: &RunStats, run: &RunData, cases: &HashMap<i64, &Case>) -> Vec<DiffEntry> {
    let mut entries = Vec::new();
    for (case_id, rep, scored) in &stats.scores {
        let case = cases[case_id];
        let row = row_for(run, *case_id, *rep);
        let shown = |field: &str, decision: Option<&MessageDecision>| {
            decision
                .and_then(|d| field_value(field, d))
                .map(|v| v.to_string())
                .unwrap_or_else(|| "(failed)".into())
        };
        let fields: Vec<(String, String, String)> = DIFF_FIELDS
            .iter()
            .filter(|field| scored.metrics.get(**field).is_some_and(|v| *v < 1.0))
            .map(|field| {
                (
                    (*field).to_owned(),
                    shown(field, Some(&case.gold)),
                    shown(field, row.decision.as_ref()),
                )
            })
            .collect();
        if fields.is_empty() {
            continue;
        }
        let others = METRICS
            .iter()
            .filter(|m| !DIFF_FIELDS.contains(m))
            .filter_map(|m| {
                scored
                    .metrics
                    .get(*m)
                    .filter(|v| **v < 1.0)
                    .map(|v| ((*m).to_owned(), *v))
            })
            .collect();
        entries.push(DiffEntry {
            case: *case_id,
            rep: *rep,
            harm: scored.harm,
            from_addr: case.from_addr.clone(),
            subject: case.subject.clone(),
            fields,
            others,
            texts: headline_texts(&case.gold, row.decision.as_ref()),
            gold_reason: case.gold.reason.clone(),
            candidate_reason: match (&row.decision, &row.failure) {
                (Some(decision), _) => decision.reason.clone(),
                (None, Some(failure)) => format!("(run failed: {failure})"),
                (None, None) => "(no decision)".into(),
            },
            trace: run
                .dir
                .join(TRACES_DIR)
                .join(format!("{case_id}_rep{rep}.json")),
        });
    }
    entries.sort_by(|a, b| {
        b.harm
            .total_cmp(&a.harm)
            .then(a.case.cmp(&b.case))
            .then(a.rep.cmp(&b.rep))
    });
    entries
}

/// The `n` rows with the lowest mean user-facing text similarity.
fn text_entries(
    stats: &RunStats,
    run: &RunData,
    cases: &HashMap<i64, &Case>,
    n: usize,
) -> Vec<TextEntry> {
    let mut entries: Vec<TextEntry> = stats
        .scores
        .iter()
        .filter_map(|(case_id, rep, scored)| {
            let values: Vec<f64> = USER_TEXT_METRICS
                .iter()
                .filter_map(|m| scored.metrics.get(*m).copied())
                .collect();
            let mean_similarity = mean(values.into_iter())?;
            let case = cases[case_id];
            let decision = row_for(run, *case_id, *rep).decision.as_ref();
            let mut texts = headline_texts(&case.gold, decision);
            texts.insert(
                1,
                (
                    "attention summary".into(),
                    case.gold.attention.summary.clone(),
                    decision
                        .map(|d| d.attention.summary.clone())
                        .unwrap_or_else(|| "(failed)".into()),
                ),
            );
            let actions = |d: &MessageDecision| {
                d.attention
                    .actions
                    .iter()
                    .map(|a| a.description.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            texts.push((
                "actions".into(),
                actions(&case.gold),
                decision.map(actions).unwrap_or_else(|| "(failed)".into()),
            ));
            Some(TextEntry {
                case: *case_id,
                rep: *rep,
                subject: case.subject.clone(),
                mean_similarity,
                texts,
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        a.mean_similarity
            .total_cmp(&b.mean_similarity)
            .then(a.case.cmp(&b.case))
            .then(a.rep.cmp(&b.rep))
    });
    entries.truncate(n);
    entries
}

fn money(value: Option<f64>) -> String {
    // `+ 0.0` turns the empty sum's -0.0 into 0.0.
    value.map_or("-".into(), |v| match v + 0.0 {
        v if v != 0.0 && v.abs() < 0.01 => format!("{v:.6}"),
        v => format!("{v:.4}"),
    })
}

fn num(value: Option<f64>, places: usize) -> String {
    value.map_or("-".into(), |v| format!("{v:.places$}"))
}

fn estimate(value: Option<&Estimate>) -> String {
    value.map_or("-".into(), |e| {
        format!("{:.3} [{:.3}, {:.3}] n={}", e.mean, e.low, e.high, e.n)
    })
}

fn cell(text: &str) -> String {
    text.replace('|', "/").replace('\n', " ")
}

/// Markdown; also what `report` prints.
pub fn render(report: &Report) -> String {
    let mut out = String::new();
    for line in &report.header {
        let _ = writeln!(out, "{line}");
    }
    let _ = writeln!(out, "\n## Cost and behavior (per email unless noted)\n");
    let _ = writeln!(
        out,
        "| run | cases | failed | infra errors | turns | tool rate | latency p50 / p95 s | input | output | cache read | cache write | $ per 1k observed | $ per 1k projected | $ wasted on retries | $ run total |"
    );
    let _ = writeln!(out, "|{}", "---|".repeat(15));
    if let Some(p) = &report.production {
        let _ = writeln!(
            out,
            "| production ({} gold runs on {}) | {} | - | - | {} | {} | - | {} | {} | {} | {} | {} | {} | - | - |{}",
            p.runs,
            p.model,
            p.runs,
            num(p.turns, 2),
            num(p.tool_rate, 2),
            num(p.tokens[0], 0),
            num(p.tokens[1], 0),
            num(p.tokens[2], 0),
            num(p.tokens[3], 0),
            money(p.costs.observed_per_1k),
            money(p.costs.projected_per_1k),
            p.costs
                .note
                .as_deref()
                .map(|n| format!(" {n}"))
                .unwrap_or_default(),
        );
    }
    for r in &report.runs {
        let _ = writeln!(
            out,
            "| {} ({}) | {} | {} | {} | {} | {} | {} / {} | {} | {} | {} | {} | {} | {} | {} | {} |{}",
            r.name,
            r.model,
            r.cases,
            r.failed,
            r.infra_errors,
            num(r.turns, 2),
            num(r.tool_rate, 2),
            num(r.latency_p50, 1),
            num(r.latency_p95, 1),
            num(r.tokens[0], 0),
            num(r.tokens[1], 0),
            num(r.tokens[2], 0),
            num(r.tokens[3], 0),
            money(r.costs.observed_per_1k),
            money(r.costs.projected_per_1k),
            money(r.costs.wasted),
            money(r.costs.total),
            r.costs
                .note
                .as_deref()
                .map(|n| format!(" {n}"))
                .unwrap_or_default(),
        );
    }
    let names: Vec<&str> = report.runs.iter().map(|r| r.name.as_str()).collect();
    let _ = writeln!(out, "\n## Agreement with gold (weighted mean [95% CI] n)\n");
    let _ = writeln!(out, "| metric | {} |", names.join(" | "));
    let _ = writeln!(out, "|---|{}", "---|".repeat(names.len()));
    for (section, metrics) in SECTIONS {
        let _ = writeln!(out, "| **{section}** |{}", " |".repeat(names.len()));
        for metric in *metrics {
            let cells: Vec<String> = report
                .runs
                .iter()
                .map(|r| {
                    if *metric == "rank_delta" {
                        estimate(r.rank_delta.as_ref())
                    } else {
                        estimate(r.metrics.get(*metric))
                    }
                })
                .collect();
            let label = if *metric == "rank_delta" {
                "rank_delta (lower is better)"
            } else {
                metric
            };
            let _ = writeln!(out, "| {label} | {} |", cells.join(" | "));
        }
    }
    let harm: Vec<String> = report
        .runs
        .iter()
        .map(|r| estimate(r.harm.as_ref()))
        .collect();
    let _ = writeln!(out, "| **harm** (lower is better) | {} |", harm.join(" | "));
    let _ = writeln!(
        out,
        "\n## Attention factors (mean absolute error; lower is better)\n"
    );
    let _ = writeln!(out, "| run | {} |", FACTORS.join(" | "));
    let _ = writeln!(out, "|---|{}", "---|".repeat(FACTORS.len()));
    for r in &report.runs {
        let cells: Vec<String> = FACTORS
            .iter()
            .map(|f| num(r.factor_mae.get(*f).copied(), 3))
            .collect();
        let _ = writeln!(out, "| {} | {} |", r.name, cells.join(" | "));
    }
    let _ = writeln!(out, "\n## Text style (user-facing text; informational)\n");
    let _ = writeln!(out, "| run | em dash rate | length vs gold (median) |");
    let _ = writeln!(out, "|---|---|---|");
    if let Some(p) = &report.production {
        let _ = writeln!(
            out,
            "| production (gold) | {} | 1.00 |",
            num(p.em_dash_rate, 3)
        );
    }
    for r in &report.runs {
        let _ = writeln!(
            out,
            "| {} | {} | {} |",
            r.name,
            num(r.em_dash_rate, 3),
            num(r.length_ratio, 2)
        );
    }
    let _ = writeln!(
        out,
        "\n## Directional (positive class; failed runs count as negative)\n"
    );
    let label = |field: &str| match field {
        "fye" => "FYE (a miss is a buried item)",
        "restricted" => "restricted (a miss is a credential left open)",
        "auth" => "auth",
        "push" => "push (auth or importance >= threshold)",
        "login_code" => "login code extracted",
        _ => "Reading",
    };
    let _ = writeln!(
        out,
        "| field | run | gold positives | recall | precision | false-positive rate | misses | false alarms |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|---|");
    for field in DIRECTIONAL {
        for r in &report.runs {
            let d = &r.directional[field];
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                label(field),
                r.name,
                d.gold_positives,
                num(d.recall, 3),
                num(d.precision, 3),
                num(d.false_positive_rate, 3),
                d.misses,
                d.false_alarms
            );
        }
    }
    if let Some(baseline) = &report.baseline {
        let names: Vec<&String> = report.deltas.keys().collect();
        let _ = writeln!(
            out,
            "\n## Versus {baseline} (paired: run minus baseline, * = CI excludes zero)\n"
        );
        let _ = writeln!(
            out,
            "| metric | {} |",
            names
                .iter()
                .map(|n| n.as_str())
                .collect::<Vec<_>>()
                .join(" | ")
        );
        let _ = writeln!(out, "|---|{}", "---|".repeat(names.len()));
        let delta_cell = |name: &String, metric: &str| {
            report.deltas[name].get(metric).map_or("-".into(), |d| {
                format!(
                    "{:+.3} [{:+.3}, {:+.3}] n={}{}",
                    d.mean,
                    d.low,
                    d.high,
                    d.n,
                    if d.excludes_zero() { " *" } else { "" }
                )
            })
        };
        for (section, metrics) in SECTIONS {
            let _ = writeln!(out, "| **{section}** |{}", " |".repeat(names.len()));
            for metric in metrics.iter().filter(|m| **m != "rank_delta") {
                let cells: Vec<String> = names.iter().map(|n| delta_cell(n, metric)).collect();
                let _ = writeln!(out, "| {metric} | {} |", cells.join(" | "));
            }
        }
        let cells: Vec<String> = names.iter().map(|n| delta_cell(n, "harm")).collect();
        let _ = writeln!(out, "| **harm** | {} |", cells.join(" | "));
    }
    for (run, table) in &report.breakdown {
        let _ = writeln!(out, "\n## {run} by stratum and fidelity\n");
        let _ = writeln!(out, "| group | cases | {} | harm |", HEADLINE.join(" | "));
        let _ = writeln!(out, "|---|---|{}---|", "---|".repeat(HEADLINE.len()));
        for (group, (n, means, harm)) in table {
            let cells: Vec<String> = HEADLINE
                .iter()
                .map(|m| num(means.get(*m).copied(), 3))
                .collect();
            let _ = writeln!(out, "| {group} | {n} | {} | {harm:.2} |", cells.join(" | "));
        }
    }
    if let Some(run) = &report.diff_run {
        let _ = writeln!(
            out,
            "\n## Disagreements: {run} ({} rows, highest harm first)\n",
            report.diff.len()
        );
        for entry in &report.diff {
            let _ = writeln!(
                out,
                "### {} rep {}: {} (harm {:.2})\n",
                entry.case, entry.rep, entry.subject, entry.harm
            );
            let _ = writeln!(out, "- from: {}", entry.from_addr);
            for (field, gold, candidate) in &entry.fields {
                let _ = writeln!(out, "- {field}: gold {gold}, candidate {candidate}");
            }
            if !entry.others.is_empty() {
                let others: Vec<String> = entry
                    .others
                    .iter()
                    .map(|(m, v)| format!("{m} {v:.2}"))
                    .collect();
                let _ = writeln!(out, "- also below 1: {}", others.join(", "));
            }
            for (label, gold, candidate) in &entry.texts {
                let _ = writeln!(
                    out,
                    "- {label}: gold \"{gold}\" / candidate \"{candidate}\""
                );
            }
            let _ = writeln!(out, "- gold reason: {}", entry.gold_reason);
            let _ = writeln!(out, "- candidate reason: {}", entry.candidate_reason);
            let _ = writeln!(out, "- trace: {}\n", entry.trace.display());
        }
    }
    match &report.texts {
        Some(Ok(entries)) => {
            let _ = writeln!(
                out,
                "\n## Least similar user-facing text ({} rows)\n",
                entries.len()
            );
            for entry in entries {
                let _ = writeln!(
                    out,
                    "### {} rep {}: {} (mean similarity {:.2})\n",
                    entry.case, entry.rep, entry.subject, entry.mean_similarity
                );
                let _ = writeln!(out, "| field | gold | candidate |");
                let _ = writeln!(out, "|---|---|---|");
                for (label, gold, candidate) in &entry.texts {
                    let _ = writeln!(out, "| {label} | {} | {} |", cell(gold), cell(candidate));
                }
                let _ = writeln!(out);
            }
        }
        Some(Err(why)) => {
            let _ = writeln!(
                out,
                "\n## Least similar user-facing text\n\nNot available: {why}."
            );
        }
        None => {}
    }
    out
}

/// Load, build, render; write `md` too when given (never inside a repository
/// unless `allow_repo_path`, because a diff names senders and subjects).
/// Text similarity uses the daemon's embedding model when its weights are on
/// this machine, cached at `<dataset>/embeddings.jsonl`.
pub fn report(
    dataset: &Path,
    options: &ReportOptions,
    md: Option<&Path>,
    allow_repo_path: bool,
) -> Result<String> {
    if let Some(path) = md
        && !allow_repo_path
    {
        crate::refuse_repo_path(path)?;
    }
    if options.no_text {
        return report_with(
            dataset,
            options,
            md,
            allow_repo_path,
            None,
            "Text similarity: off (--no-text); the *_sim metrics are omitted.",
        );
    }
    match crate::textsim::production() {
        Ok((embedder, model)) => {
            let sim = crate::textsim::EmbeddingSimilarity::open(
                &dataset.join(crate::textsim::CACHE_FILE),
                &model,
                embedder,
            );
            report_embedded(dataset, options, md, allow_repo_path, &sim, &model)
        }
        Err(why) => report_with(
            dataset,
            options,
            md,
            allow_repo_path,
            None,
            &format!(
                "Text similarity unavailable: {why}; the *_sim metrics are omitted (pass --no-text to skip trying)."
            ),
        ),
    }
}

/// Embed everything a scoring pass will compare in batches (a dry pass
/// collects the texts), then score for real and keep the cache.
pub fn report_embedded(
    dataset: &Path,
    options: &ReportOptions,
    md: Option<&Path>,
    allow_repo_path: bool,
    sim: &crate::textsim::EmbeddingSimilarity,
    model: &str,
) -> Result<String> {
    let recorder = crate::textsim::RecordingSimilarity::default();
    report_with(dataset, options, None, true, Some(&recorder), "")?;
    let embedded = sim.prefetch(recorder.texts.into_inner())?;
    let line = format!(
        "Text similarity: cosine of {model} embeddings against gold's wording ({}, {embedded} newly embedded); read it against the production-model run and --texts, not as an absolute.",
        sim.cache_note
    );
    let text = report_with(dataset, options, md, allow_repo_path, Some(sim), &line)?;
    sim.save()?;
    Ok(text)
}

fn report_with(
    dataset: &Path,
    options: &ReportOptions,
    md: Option<&Path>,
    allow_repo_path: bool,
    sim: Option<&dyn TextSimilarity>,
    text_line: &str,
) -> Result<String> {
    if let Some(path) = md
        && !allow_repo_path
    {
        crate::refuse_repo_path(path)?;
    }
    let cases = read_cases(&dataset.join(CASES_FILE))?;
    let manifest: Value = std::fs::read_to_string(dataset.join(MANIFEST_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);
    let mut names = if options.runs.is_empty() {
        list_runs(dataset)
    } else {
        options.runs.clone()
    };
    for extra in [&options.baseline, &options.diff].into_iter().flatten() {
        if !names.contains(extra) {
            names.push(extra.clone());
        }
    }
    let runs = names
        .iter()
        .map(|name| RunData::load(dataset, name))
        .collect::<Result<Vec<_>>>()?;
    let adjudications = Adjudications::load(dataset)?;
    let name = dataset
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let built = build(
        &name,
        &cases,
        &manifest,
        &runs,
        &adjudications,
        options,
        sim,
        text_line,
    )?;
    let text = render(&built);
    if let Some(path) = md {
        crate::private_write(path, text.as_bytes())?;
    }
    Ok(text)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::GoldMeta;
    use crate::export::{ExportOptions, SNAPSHOT_FILE, export};
    use crate::fixture::{self, Mock, Reply};
    use crate::run::run;
    use serde_json::json;
    use squelch_core::store::SqliteStore;
    use squelch_core::triage::context::ContextSnapshot;
    use std::sync::Arc;

    fn synthetic_case(id: i64, stratum: &str) -> Case {
        let as_of: DateTime<Utc> = "2026-09-25T12:00:00Z".parse().unwrap();
        Case {
            id,
            thread_id: format!("t{id}"),
            as_of,
            trigger: "arrival".into(),
            fidelity: Fidelity::Exact,
            fidelity_notes: Vec::new(),
            tags: vec![stratum.into()],
            context: ContextSnapshot {
                account_id: 1,
                message_id: id,
                thread_id: format!("t{id}"),
                initial: json!({"message": {"id": id, "thread_id": format!("t{id}"), "body": "synthetic"},
                    "attention": null, "now": as_of.to_rfc3339()}),
                source_message_ids: vec![id],
                ..Default::default()
            },
            gold: fixture::gold(id, "gold", stratum == "fye"),
            gold_meta: GoldMeta {
                model: "claude-opus-5-5".into(),
                prompt_version: "agent-triage-v5".into(),
                model_turns: 1,
                tool_calls: 0,
                usage: Vec::new(),
                decided_at: as_of,
            },
            subject: format!("Synthetic {id}"),
            from_addr: "sender@example.com".into(),
        }
    }

    /// A 1000-case dataset with one 600-case stratum, sampled at 100: every
    /// row of the big strata carries weight 10, and the report's mean must
    /// follow the weights, not the row count.
    #[tokio::test]
    async fn sampled_weights_reach_the_reported_mean() {
        let dir = fixture::temp_dir("weights");
        let dataset = dir.join("synthetic");
        crate::private_dir(&dataset).unwrap();
        drop(SqliteStore::open(dataset.join(SNAPSHOT_FILE)).unwrap());
        let cases: Vec<Case> = (1..=1000)
            .map(|id| {
                let stratum = match id {
                    1..=390 => "fye",
                    391..=990 => "reading",
                    _ => "other",
                };
                synthetic_case(id, stratum)
            })
            .collect();
        let lines: String = cases
            .iter()
            .map(|c| serde_json::to_string(c).unwrap() + "\n")
            .collect();
        std::fs::write(dataset.join(CASES_FILE), lines).unwrap();
        let manifest = json!({"production_cache_hit": {"model": "claude-opus-5-5", "share": 0.9}});
        std::fs::write(dataset.join(MANIFEST_FILE), manifest.to_string()).unwrap();
        let mock = Arc::new(Mock::default());
        let (url, server) = fixture::serve(mock).await;
        let mut options = fixture::run_options(&dataset, &url);
        options.sample = Some((100, 7));
        options.concurrency = 8;
        run(&options).await.unwrap();
        server.abort();
        let data = RunData::load(&dataset, "mock").unwrap();
        assert_eq!(data.rows.len(), 39 + 60 + 10);
        assert!(data.rows.iter().any(|r| r.weight == 10.0));
        assert!(data.rows.iter().any(|r| r.weight == 1.0));
        // The candidate always puts mail in FYE; only the fye stratum agrees.
        let weighted = 390.0 / 1000.0;
        let unweighted = 39.0 / 109.0;
        let report = build(
            "synthetic",
            &cases,
            &manifest,
            &[data],
            &Adjudications::default(),
            &ReportOptions::default(),
            None,
            "",
        )
        .unwrap();
        let fye = report.runs[0].metrics["fye"];
        assert!((fye.mean - weighted).abs() < 1e-9, "{fye:?}");
        assert!((fye.mean - unweighted).abs() > 0.03);
        assert!(fye.low <= weighted && weighted <= fye.high);
        let fye_rates = &report.runs[0].directional["fye"];
        assert_eq!(fye_rates.gold_positives, 39);
        // Rates are weighted too: precision is the weighted FYE share.
        assert!((fye_rates.precision.unwrap() - weighted).abs() < 1e-9);
        assert_eq!(fye_rates.false_alarms, 70, "raw counts stay raw");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The production row prices only the gold runs on the current model.
    #[test]
    fn production_row_covers_the_current_model_only() {
        let usage = |input| Usage {
            input_tokens: input,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_creation: None,
        };
        let mut today = synthetic_case(1, "fye");
        today.gold_meta.usage = vec![usage(1_000)];
        let mut old = synthetic_case(2, "fye");
        old.gold_meta.model = "claude-opus-5".into();
        old.gold_meta.usage = vec![usage(9_000)];
        let stats = production_stats(&[today, old], "claude-opus-5-5", None);
        assert_eq!(stats.runs, 1);
        // 1000 input tokens at $4/MTok, per 1k emails.
        assert!((stats.costs.observed_per_1k.unwrap() - 4.0).abs() < 1e-9);
        assert!(stats.costs.projected_per_1k.is_none());
    }

    struct Setup {
        fixture: fixture::Fixture,
        dataset: PathBuf,
        cases: Vec<Case>,
        manifest: Value,
    }

    async fn fixture_runs() -> Setup {
        let fixture = fixture::build();
        let dataset = export(&ExportOptions {
            db: fixture.db.clone(),
            out: fixture.dir.join("evals"),
            name: "set".into(),
        })
        .unwrap()
        .dir;
        let mock = Arc::new(Mock::default());
        mock.scripts.lock().unwrap().insert(
            fixture.ids["A"],
            vec![
                Reply::ReadThread("t1".into()),
                Reply::Status(500),
                Reply::Valid,
            ]
            .into(),
        );
        let (url, server) = fixture::serve(mock).await;
        run(&fixture::run_options(&dataset, &url)).await.unwrap();
        let mut second = fixture::run_options(&dataset, &url);
        second.candidate_name = "mock2".into();
        run(&second).await.unwrap();
        server.abort();
        let cases = read_cases(&dataset.join(CASES_FILE)).unwrap();
        let manifest =
            serde_json::from_str(&std::fs::read_to_string(dataset.join(MANIFEST_FILE)).unwrap())
                .unwrap();
        Setup {
            fixture,
            dataset,
            cases,
            manifest,
        }
    }

    fn runs(setup: &Setup) -> Vec<RunData> {
        ["mock", "mock2"]
            .iter()
            .map(|n| RunData::load(&setup.dataset, n).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn report_costs_header_production_and_diff() {
        let setup = fixture_runs().await;
        let options = ReportOptions {
            baseline: Some("mock".into()),
            diff: Some("mock".into()),
            ..Default::default()
        };
        let report = build(
            "set",
            &setup.cases,
            &setup.manifest,
            &runs(&setup),
            &Adjudications::default(),
            &options,
            None,
            "",
        )
        .unwrap();
        assert_eq!(report.header.len(), 4);
        assert!(
            report.header[0]
                .starts_with("Dataset set: 5 cases (1 exact, 3 reconstructed, 1 suspect)"),
            "{}",
            report.header[0]
        );
        assert!(report.header[0].contains(&format!("scorer v{SCORER_VERSION}")));
        assert!(report.header[1].contains("100.0%") && report.header[1].contains("manifest.json"));
        assert!(
            report.header[2].starts_with(
                "Caveat: gold is one sample from the incumbent model (claude-opus-5-5)"
            )
        );
        // Production: every gold run is on the current model. Mean input 9000,
        // 400 out, 6000 cache read at Opus 5.5: (36000 + 8000 + 1200) / 1e6.
        let production = report.production.as_ref().unwrap();
        assert_eq!(production.runs, 5);
        assert!((production.costs.observed_per_1k.unwrap() - 45.2).abs() < 1e-9);
        // One call per scored row: (10*1 + 8*2 + 2*0.1) / 1e6 at the mock's
        // prices. A's first attempt paid one call before its 500.
        let mock = &report.runs[0];
        let call = 26.2e-6;
        assert_eq!((mock.rows, mock.infra_errors, mock.failed), (4, 1, 0));
        assert!((mock.costs.observed_per_1k.unwrap() - call * 1000.0).abs() < 1e-12);
        assert!((mock.costs.wasted.unwrap() - call).abs() < 1e-12);
        assert!((mock.costs.total.unwrap() - 5.0 * call).abs() < 1e-12);
        assert!(report.runs[1].costs.wasted == Some(0.0));
        // D's gold is Reading and out of FYE; the candidate disagrees on both.
        assert_eq!(mock.metrics["fye"].mean, 0.75);
        assert_eq!(mock.directional["fye"].false_alarms, 1);
        assert_eq!(report.diff.len(), 1);
        let entry = &report.diff[0];
        assert_eq!(entry.case, setup.fixture.ids["D"]);
        let fields: Vec<&str> = entry.fields.iter().map(|f| f.0.as_str()).collect();
        assert_eq!(fields, vec!["fye", "reading"]);
        assert!(entry.trace.exists());
        assert!(entry.gold_reason.contains("Gold reason for D"));
        let delta = report.deltas["mock2"]["fye"];
        assert_eq!(delta.mean, 0.0);
        assert!(!delta.excludes_zero());
        let text = render(&report);
        assert!(text.lines().take(3).all(|l| !l.is_empty()));
        assert!(text.contains("## Disagreements: mock"));
        assert!(text.contains("| production (5 gold runs on claude-opus-5-5)"));
        assert!(!text.contains("-0.0"), "no negative zero");
        assert!(
            text.contains("| 0.000131 |"),
            "run total shown to the micro-dollar"
        );
    }

    #[tokio::test]
    async fn stored_grades_are_ignored_and_rulings_flip_agreement() {
        let setup = fixture_runs().await;
        let mut tampered = runs(&setup);
        for row in &mut tampered[0].rows {
            row.grade = BTreeMap::from([("fye".to_owned(), 0.0)]);
            row.scorer_version = 0;
        }
        let none = Adjudications::default();
        let report = build(
            "set",
            &setup.cases,
            &setup.manifest,
            &tampered,
            &none,
            &ReportOptions::default(),
            None,
            "",
        )
        .unwrap();
        assert_eq!(
            report.runs[0].metrics["fye"].mean, 0.75,
            "re-scored, not read back"
        );
        let d = setup.fixture.ids["D"];
        let ruling = adjudicate(&setup.dataset, d, "fye", Verdict::Candidate, "mock").unwrap();
        assert_eq!(ruling.value, json!(true));
        assert!(
            adjudicate(
                &setup.dataset,
                d,
                "record_fields",
                Verdict::Candidate,
                "mock"
            )
            .is_err()
        );
        let ruled = Adjudications::load(&setup.dataset).unwrap();
        assert_eq!(ruled.len(), 1);
        let options = ReportOptions {
            diff: Some("mock".into()),
            ..Default::default()
        };
        let after = build(
            "set",
            &setup.cases,
            &setup.manifest,
            &runs(&setup),
            &ruled,
            &options,
            None,
            "",
        )
        .unwrap();
        for stats in &after.runs {
            assert_eq!(
                stats.metrics["fye"].mean, 1.0,
                "{} gave the ruled value",
                stats.name
            );
            assert_eq!(
                stats.metrics["reading"].mean, 0.75,
                "only the ruled field moves"
            );
        }
        let fields: Vec<&str> = after.diff[0].fields.iter().map(|f| f.0.as_str()).collect();
        assert_eq!(fields, vec!["reading"]);
        // A later gold ruling on the same value revokes it.
        adjudicate(&setup.dataset, d, "fye", Verdict::Gold, "mock").unwrap();
        assert!(Adjudications::load(&setup.dataset).unwrap().is_empty());
    }

    #[tokio::test]
    async fn costs_are_withheld_not_zeroed() {
        let setup = fixture_runs().await;
        let mut silent = runs(&setup);
        for row in &mut silent[0].rows {
            for call in &mut row.calls {
                *call = Usage {
                    input_tokens: 0,
                    output_tokens: 3,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_creation: None,
                };
            }
        }
        silent[1].prices = None;
        let report = build(
            "set",
            &setup.cases,
            &setup.manifest,
            &silent,
            &Adjudications::default(),
            &ReportOptions::default(),
            None,
            "",
        )
        .unwrap();
        assert!(report.runs[0].costs.observed_per_1k.is_none());
        assert!(
            report.runs[0]
                .costs
                .note
                .as_deref()
                .unwrap()
                .contains("no input tokens")
        );
        assert!(report.runs[1].costs.total.is_none());
        assert!(
            report.runs[1]
                .costs
                .note
                .as_deref()
                .unwrap()
                .contains("no price")
        );
        // A different cache assumption changes only the projection.
        let half = ReportOptions {
            cache_hit: Some(0.5),
            ..Default::default()
        };
        let projected = build(
            "set",
            &setup.cases,
            &setup.manifest,
            &runs(&setup),
            &Adjudications::default(),
            &half,
            None,
            "",
        )
        .unwrap();
        assert!(
            projected.header[1].contains("50.0%") && projected.header[1].contains("--cache-hit")
        );
        let full = build(
            "set",
            &setup.cases,
            &setup.manifest,
            &runs(&setup),
            &Adjudications::default(),
            &ReportOptions::default(),
            None,
            "",
        )
        .unwrap();
        assert_eq!(
            projected.runs[0].costs.observed_per_1k,
            full.runs[0].costs.observed_per_1k
        );
        assert!(projected.runs[0].costs.projected_per_1k > full.runs[0].costs.projected_per_1k);
    }

    /// Token-set Jaccard, standing in for embeddings.
    struct TokenSim;
    impl TextSimilarity for TokenSim {
        fn similarity(&self, a: &str, b: &str) -> f64 {
            let tokens = |t: &str| -> std::collections::BTreeSet<String> {
                t.to_lowercase()
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| !w.is_empty())
                    .map(str::to_owned)
                    .collect()
            };
            let (a, b) = (tokens(a), tokens(b));
            a.intersection(&b).count() as f64 / a.union(&b).count().max(1) as f64
        }
    }

    #[tokio::test]
    async fn the_agreement_table_is_sectioned_with_factor_and_style_tables() {
        let setup = fixture_runs().await;
        let report = build(
            "set",
            &setup.cases,
            &setup.manifest,
            &runs(&setup),
            &Adjudications::default(),
            &ReportOptions::default(),
            Some(&TokenSim),
            "Text similarity: token overlap (test).",
        )
        .unwrap();
        assert_eq!(report.header[3], "Text similarity: token overlap (test).");
        let mock = &report.runs[0];
        // The mock answers every case in its own words.
        let summary = mock.metrics["summary_sim"].mean;
        assert!(summary > 0.0 && summary < 1.0, "{summary}");
        assert_eq!(mock.metrics["text_safe"].mean, 1.0);
        assert!(mock.factor_mae.contains_key("urgency"));
        assert!(mock.length_ratio.is_some());
        let text = render(&report);
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("{needle} missing"))
        };
        let order = [
            "| **Placement** |",
            "| **Security and push** |",
            "| **Attention detail** |",
            "| **Records** |",
            "| **Text** |",
            "| **Grounding** |",
            "| **harm** (lower is better) |",
            "## Attention factors",
            "## Text style",
        ];
        for pair in order.windows(2) {
            assert!(at(pair[0]) < at(pair[1]), "{} before {}", pair[0], pair[1]);
        }
        assert!(text.contains("| rank_delta (lower is better) |"));
        assert!(text.contains("| production (gold) |"));
        // Every metric has a row.
        for metric in METRICS {
            assert!(text.contains(&format!("| {metric} | ")), "{metric}");
        }
        // Without a similarity the text rows are empty, not zero.
        let plain = build(
            "set",
            &setup.cases,
            &setup.manifest,
            &runs(&setup),
            &Adjudications::default(),
            &ReportOptions::default(),
            None,
            "Text similarity: off.",
        )
        .unwrap();
        assert!(!plain.runs[0].metrics.contains_key("summary_sim"));
        assert!(render(&plain).contains("| summary_sim | - | - |"));
    }

    #[tokio::test]
    async fn texts_lists_the_least_similar_rows_and_diff_names_the_rest() {
        let setup = fixture_runs().await;
        let options = ReportOptions {
            diff: Some("mock".into()),
            texts: Some(2),
            ..Default::default()
        };
        let report = build(
            "set",
            &setup.cases,
            &setup.manifest,
            &runs(&setup),
            &Adjudications::default(),
            &options,
            Some(&TokenSim),
            "",
        )
        .unwrap();
        let entries = report.texts.as_ref().unwrap().as_ref().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].mean_similarity <= entries[1].mean_similarity);
        let all = build(
            "set",
            &setup.cases,
            &setup.manifest,
            &runs(&setup),
            &Adjudications::default(),
            &ReportOptions {
                texts: Some(10),
                ..options.clone()
            },
            Some(&TokenSim),
            "",
        )
        .unwrap();
        let every = all.texts.unwrap().unwrap();
        assert_eq!(every.len(), 4, "one per scored row");
        // A row whose user-facing text is gold's own ranks last.
        let a_id = setup.fixture.ids["A"];
        let gold_a = setup
            .cases
            .iter()
            .find(|c| c.id == a_id)
            .unwrap()
            .gold
            .clone();
        let mut echoed = runs(&setup);
        for row in &mut echoed[0].rows {
            if row.prompt_id == a_id.to_string() {
                let decision = row.decision.as_mut().unwrap();
                decision.summary = gold_a.summary.clone();
                decision.attention.summary = gold_a.attention.summary.clone();
                decision.notification = gold_a.notification.clone();
                decision.attention.actions = gold_a.attention.actions.clone();
            }
        }
        let ranked = build(
            "set",
            &setup.cases,
            &setup.manifest,
            &echoed,
            &Adjudications::default(),
            &ReportOptions {
                texts: Some(4),
                ..options.clone()
            },
            Some(&TokenSim),
            "",
        )
        .unwrap()
        .texts
        .unwrap()
        .unwrap();
        assert_eq!(ranked.last().unwrap().case, a_id);
        assert_eq!(ranked.last().unwrap().mean_similarity, 1.0);
        assert_ne!(ranked[0].case, a_id);
        assert_eq!(
            every[0].mean_similarity, entries[0].mean_similarity,
            "lowest first"
        );
        let labels: Vec<&str> = entries[0].texts.iter().map(|t| t.0.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "summary",
                "attention summary",
                "push title",
                "push body",
                "actions"
            ]
        );
        let text = render(&report);
        assert!(text.contains("## Least similar user-facing text (2 rows)"));
        assert!(text.contains("| summary | Gold summary for "));
        // The diff names every other metric below 1 and shows the texts.
        let d = &report.diff[0];
        assert!(
            d.others.iter().any(|(m, _)| m == "summary_sim"),
            "{:?}",
            d.others
        );
        assert!(
            !d.others.iter().any(|(m, _)| m == "fye"),
            "listed fields are not repeated"
        );
        assert!(text.contains("- also below 1: "));
        assert!(text.contains("- summary: gold \"Gold summary for D"));
        // Without text similarity there is nothing to rank by, and --texts
        // without --diff is refused.
        let unranked = build(
            "set",
            &setup.cases,
            &setup.manifest,
            &runs(&setup),
            &Adjudications::default(),
            &options,
            None,
            "",
        )
        .unwrap();
        assert!(unranked.texts.unwrap().is_err());
        let orphan = ReportOptions {
            texts: Some(1),
            ..Default::default()
        };
        assert!(
            build(
                "set",
                &setup.cases,
                &setup.manifest,
                &runs(&setup),
                &Adjudications::default(),
                &orphan,
                Some(&TokenSim),
                ""
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn the_report_embeds_each_text_once_across_reports() {
        let setup = fixture_runs().await;
        let cache = setup.dataset.join(crate::textsim::CACHE_FILE);
        let open = || {
            crate::textsim::EmbeddingSimilarity::open(
                &cache,
                "stub-model",
                Box::new(squelch_core::embed::StubEmbedder::new(64)),
            )
        };
        let options = ReportOptions::default();
        let first = open();
        let text =
            report_embedded(&setup.dataset, &options, None, false, &first, "stub-model").unwrap();
        let header = text.lines().nth(3).unwrap();
        assert!(
            header.contains("stub-model") && header.contains("no cache yet"),
            "{header}"
        );
        assert!(!header.contains(", 0 newly embedded"), "{header}");
        assert!(cache.exists());
        let second = open();
        let again =
            report_embedded(&setup.dataset, &options, None, false, &second, "stub-model").unwrap();
        let header = again.lines().nth(3).unwrap();
        assert!(header.contains(", 0 newly embedded"), "{header}");
        assert!(again.contains("| summary_sim | 0."));
        // Same numbers from the cache as from a fresh embedding.
        assert_eq!(
            text.lines().skip(4).collect::<Vec<_>>(),
            again.lines().skip(4).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn markdown_refuses_a_repository_path() {
        let setup = fixture_runs().await;
        let repo = fixture::temp_dir("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let inside = repo.join("report.md");
        let options = ReportOptions {
            no_text: true,
            ..Default::default()
        };
        assert!(report(&setup.dataset, &options, Some(&inside), false).is_err());
        assert!(!inside.exists());
        let text = report(&setup.dataset, &options, Some(&inside), true).unwrap();
        assert_eq!(std::fs::read_to_string(&inside).unwrap(), text);
        let outside = setup.fixture.dir.join("report.md");
        report(&setup.dataset, &options, Some(&outside), false).unwrap();
        std::fs::remove_dir_all(repo).unwrap();
    }
}
