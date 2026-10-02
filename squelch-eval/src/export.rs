//! `squelch-eval export`: snapshot the daemon db, pick the eligible gold
//! decisions, rebuild each one's context as of its run, and audit the result.
//!
//! The live db is opened READ-ONLY, once, for `VACUUM INTO`. Several store
//! "reads" write (source assessments queue access jobs), so every other read
//! here goes through the copy.
use crate::asof::{AsOfData, context_as_of, open_snapshot_sql, parse_ts};
use crate::case::{
    CONDENSED_FROM_VERSION, Case, Fidelity, GoldMeta, STRATA, first_user_message, is_push, labels,
    prompt_version_number, tags,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use squelch_core::store::SqliteStore;
use squelch_core::store::agent_triage::{AgentJob, AgentTriageStore};
use squelch_core::sync::triage_worker::snapshot;
use squelch_core::triage::agent::{bound_initial_context, prepare_initial_context};
use squelch_core::triage::agent_config::AgentTriageConfig;
use squelch_core::triage::decision::MessageDecision;
use squelch_core::triage::llm::Usage;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

pub const SNAPSHOT_FILE: &str = "snapshot.db";
pub const CASES_FILE: &str = "cases.jsonl";
pub const MANIFEST_FILE: &str = "manifest.json";
/// Gold text shorter than this is not leak-checked: a two-word reason can
/// occur in an email by chance, and a real leak (a previous decision or its
/// attention) carries the full sentences.
pub const LEAK_MIN_CHARS: usize = 20;

/// Production's push threshold (`NotifyConfig::min_importance`).
pub fn default_min_importance() -> u8 {
    squelch_core::config::NotifyConfig::default().min_importance
}

pub struct ExportOptions {
    pub db: PathBuf,
    pub out: PathBuf,
    pub name: String,
}

pub struct ExportOutcome {
    pub dir: PathBuf,
    pub cases: usize,
    pub manifest: Value,
}

pub fn export(options: &ExportOptions) -> Result<ExportOutcome> {
    crate::refuse_repo_path(&options.out)?;
    if options.name.is_empty() || options.name.contains(['/', '\\']) || options.name == ".." {
        bail!("invalid dataset name {:?}", options.name);
    }
    let dir = options.out.join(&options.name);
    let copy = dir.join(SNAPSHOT_FILE);
    if copy.exists() {
        bail!("{} already exists; choose another --name", copy.display());
    }
    crate::private_dir(&options.out)?;
    crate::private_dir(&dir)?;
    let snapshot_notes = snapshot_db(&options.db, &copy)?;
    let store = SqliteStore::open(&copy).context("open snapshot store")?;
    let conn = open_snapshot_sql(&copy)?;
    let config = AgentTriageConfig::default();
    let min_importance = default_min_importance();
    let built = build_cases(&store, &conn, &config, min_importance)?;
    let audit = audit(&built, &config)?;
    let mut lines = String::new();
    for case in &built.cases {
        lines.push_str(&serde_json::to_string(case)?);
        lines.push('\n');
    }
    crate::private_write(&dir.join(CASES_FILE), lines.as_bytes())?;
    let manifest = manifest(options, &built, audit, snapshot_notes, min_importance);
    crate::private_write(
        &dir.join(MANIFEST_FILE),
        serde_json::to_string_pretty(&manifest)?.as_bytes(),
    )?;
    Ok(ExportOutcome {
        dir,
        cases: built.cases.len(),
        manifest,
    })
}

/// `VACUUM INTO` from a read-only connection (a consistent copy under WAL that
/// never writes the source), then shed what the eval never reads.
pub fn snapshot_db(source: &Path, dest: &Path) -> Result<Vec<String>> {
    if !source.exists() {
        bail!("no database at {}", source.display());
    }
    // Opening any store registers the statically linked vec0 extension for
    // every later connection in this process; the copy's cleanup needs it.
    drop(SqliteStore::open_in_memory()?);
    {
        let live = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("open {} read-only", source.display()))?;
        let target = dest.to_str().context("snapshot path is not valid UTF-8")?;
        live.execute("VACUUM INTO ?1", [target])
            .context("VACUUM INTO snapshot")?;
    }
    crate::set_mode(dest, 0o600)?;
    // Bring the copy to this binary's schema before touching it.
    drop(SqliteStore::open(dest).context("migrate snapshot")?);
    let mut notes = Vec::new();
    let conn = Connection::open(dest)?;
    let cleared = conn.execute(
        "UPDATE messages SET body_html=NULL WHERE body_html IS NOT NULL",
        [],
    )?;
    notes.push(format!("body_html cleared on {cleared} messages"));
    match conn.execute("DELETE FROM message_vecs", []) {
        Ok(rows) => notes.push(format!("message_vecs cleared ({rows} rows)")),
        Err(error) => notes.push(format!("message_vecs left in place: {error}")),
    }
    conn.execute_batch("VACUUM")?;
    Ok(notes)
}

#[derive(Debug, Clone)]
struct Run {
    account_id: i64,
    completed_at: String,
    trigger: String,
    metadata: Value,
}

/// Cases plus everything the size check and the audit need about them.
pub struct Built {
    pub cases: Vec<Case>,
    pub skipped: BTreeMap<String, usize>,
    /// Per case: the first user message a replay sends today (current prepare).
    pub prepared_bytes: Vec<usize>,
    /// Per case: the same, shaped as the gold run's prompt version shaped it.
    pub gold_shape_bytes: Vec<usize>,
    pub bytes_per_token_median: BTreeMap<String, f64>,
}

pub fn build_cases(
    store: &SqliteStore,
    conn: &Connection,
    config: &AgentTriageConfig,
    min_importance: u8,
) -> Result<Built> {
    let data = AsOfData::load(conn)?;
    let mut runs: BTreeMap<i64, Vec<Run>> = BTreeMap::new();
    let mut stmt = conn.prepare(
        "SELECT r.message_id,r.account_id,r.completed_at,r.metadata_json,j.trigger
         FROM agent_triage_runs r JOIN agent_triage_jobs j ON j.id=r.job_id
         WHERE j.kind='triage' AND r.outcome='applied' ORDER BY r.id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            Run {
                account_id: row.get(1)?,
                completed_at: row.get(2)?,
                metadata: serde_json::from_str(&row.get::<_, String>(3)?).unwrap_or(Value::Null),
                trigger: row.get(4)?,
            },
        ))
    })?;
    for row in rows {
        let (id, run) = row?;
        runs.entry(id).or_default().push(run);
    }
    let mut ids: Vec<i64> = runs.keys().copied().collect();
    ids.extend(data.decisions.keys().copied());
    ids.extend(data.undecodable_decisions.iter().copied());
    ids.sort_unstable();
    ids.dedup();
    let mut skipped = BTreeMap::new();
    let mut skip = |reason: &str| *skipped.entry(reason.to_owned()).or_insert(0) += 1;
    let mut cases = Vec::new();
    for id in ids {
        if data.undecodable_decisions.contains(&id) {
            skip("gold_undecodable");
            continue;
        }
        let Some(gold) = data.decisions.get(&id) else {
            skip("no_gold_decision");
            continue;
        };
        let run = match runs.get(&id).map(Vec::as_slice) {
            None | Some([]) => {
                skip("no_applied_triage_run");
                continue;
            }
            Some([run]) => run,
            Some(_) => {
                skip("multiple_applied_triage_runs");
                continue;
            }
        };
        let usage: Vec<Usage> =
            serde_json::from_value(run.metadata["usage"].clone()).unwrap_or_default();
        if usage.is_empty() {
            skip("no_usage");
            continue;
        }
        let Some(as_of) = parse_ts(&run.completed_at) else {
            skip("bad_timestamp");
            continue;
        };
        let job = AgentJob {
            id: 0,
            account_id: run.account_id,
            message_id: id,
            kind: "triage".into(),
            trigger: run.trigger.clone(),
            lease_token: String::new(),
            attempts: 0,
            arrival_eligible: false,
            foreground: false,
        };
        let Ok(mut context) = store.load_agent_context(&job) else {
            skip("context_unavailable");
            continue;
        };
        if context.message.is_sent {
            skip("sent_or_own_mail");
            continue;
        }
        if context.message.is_spam {
            skip("spam");
            continue;
        }
        let report = context_as_of(&data, &mut context, as_of, id);
        let mut input = snapshot(&context, &job, config.context.initial_thread_messages);
        input.initial["now"] = json!(as_of.to_rfc3339());
        let multi = context.thread.len() > 1;
        let fidelity = if !report.multi_run_messages.is_empty() {
            Fidelity::Suspect
        } else if !multi
            && report.prior_attention_entries == 0
            && report.dropped_rules.is_empty()
            && !report.contact_dropped
        {
            Fidelity::Exact
        } else {
            Fidelity::Reconstructed
        };
        let text = |key: &str| run.metadata[key].as_str().unwrap_or("unknown").to_owned();
        let count = |key: &str| run.metadata[key].as_u64().unwrap_or(0);
        cases.push(Case {
            id,
            thread_id: context.message.thread_id.clone(),
            as_of,
            trigger: run.trigger.clone(),
            fidelity,
            fidelity_notes: report.notes(),
            tags: tags(&gold.decision, min_importance, multi),
            context: input,
            gold: gold.decision.clone(),
            gold_meta: GoldMeta {
                model: text("model"),
                prompt_version: text("prompt_version"),
                model_turns: count("model_turns"),
                tool_calls: count("tool_calls"),
                usage,
                decided_at: gold.decided_at,
            },
            subject: context.message.subject.clone(),
            from_addr: context.message.from_addr.clone(),
        });
    }
    let mut prepared_bytes = Vec::with_capacity(cases.len());
    let mut gold_shape_bytes = Vec::with_capacity(cases.len());
    for case in &cases {
        let (prepared, shaped) = user_message_sizes(case, config)?;
        prepared_bytes.push(prepared);
        gold_shape_bytes.push(shaped);
    }
    let samples: Vec<(String, usize, u64)> = cases
        .iter()
        .zip(&gold_shape_bytes)
        .map(|(case, bytes)| {
            (
                case.gold_meta.prompt_version.clone(),
                *bytes,
                case.gold_meta.usage[0].input_tokens,
            )
        })
        .collect();
    let (medians, outliers) = size_outliers(&samples);
    for (case, outlier) in cases.iter_mut().zip(outliers) {
        match outlier {
            SizeCheck::Ok => {}
            SizeCheck::Unchecked => case
                .fidelity_notes
                .push("gold run reported no first-call input tokens; size unchecked".into()),
            SizeCheck::Outlier { ratio, median } => {
                case.fidelity = Fidelity::Suspect;
                case.fidelity_notes.push(format!(
                    "rebuilt prompt is {ratio:.2} bytes/token against a {median:.2} median for {}",
                    case.gold_meta.prompt_version
                ));
            }
        }
    }
    Ok(Built {
        cases,
        skipped,
        prepared_bytes,
        gold_shape_bytes,
        bytes_per_token_median: medians,
    })
}

/// (bytes of the first user message a replay sends now, bytes as the gold
/// run's prompt version built it: condensed only from v4).
pub fn user_message_sizes(case: &Case, config: &AgentTriageConfig) -> Result<(usize, usize)> {
    let mut prepared = case.context.clone();
    prepare_initial_context(&mut prepared.initial, &config.context);
    let prepared_len = first_user_message(&prepared)?.len();
    let condensed = prompt_version_number(&case.gold_meta.prompt_version)
        .is_some_and(|version| version >= CONDENSED_FROM_VERSION);
    if condensed {
        return Ok((prepared_len, prepared_len));
    }
    let mut bounded = case.context.clone();
    bound_initial_context(&mut bounded.initial, &config.context);
    Ok((prepared_len, first_user_message(&bounded)?.len()))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SizeCheck {
    Ok,
    Unchecked,
    Outlier { ratio: f64, median: f64 },
}

/// Bytes per billed token, judged against the median of the same prompt
/// version: outside [0.5x, 2x] the rebuilt prompt is not the one billed.
pub fn size_outliers(samples: &[(String, usize, u64)]) -> (BTreeMap<String, f64>, Vec<SizeCheck>) {
    let mut by_version: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for (version, bytes, tokens) in samples {
        if *tokens > 0 {
            by_version
                .entry(version.clone())
                .or_default()
                .push(*bytes as f64 / *tokens as f64);
        }
    }
    let medians: BTreeMap<String, f64> = by_version
        .into_iter()
        .map(|(version, mut ratios)| (version, median(&mut ratios)))
        .collect();
    let checks = samples
        .iter()
        .map(|(version, bytes, tokens)| {
            if *tokens == 0 {
                return SizeCheck::Unchecked;
            }
            let ratio = *bytes as f64 / *tokens as f64;
            let median = medians[version];
            if ratio < 0.5 * median || ratio > 2.0 * median {
                SizeCheck::Outlier { ratio, median }
            } else {
                SizeCheck::Ok
            }
        })
        .collect();
    (medians, checks)
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// Nearest-rank percentile of an already sorted slice.
fn percentile(sorted: &[usize], p: f64) -> usize {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

/// A gold `summary` or `reason` inside the context means the answer leaked
/// in (through a previous decision or attention). Hard error.
pub fn check_leakage(cases: &[Case]) -> Result<()> {
    for case in cases {
        let context = serde_json::to_string(&case.context)?;
        for (field, text) in [
            ("summary", &case.gold.summary),
            ("reason", &case.gold.reason),
        ] {
            let text = text.trim();
            if text.chars().count() < LEAK_MIN_CHARS {
                continue;
            }
            // Compare in the context's own JSON escaping.
            let encoded = serde_json::to_string(text)?;
            let needle = &encoded[1..encoded.len() - 1];
            if context.contains(needle) {
                bail!(
                    "LEAK: case {} context contains its gold {field}; the export is wrong, not the case",
                    case.id
                );
            }
        }
    }
    Ok(())
}

/// Tier-1 dataset audit. Fails only on leakage.
pub fn audit(built: &Built, config: &AgentTriageConfig) -> Result<Value> {
    check_leakage(&built.cases)?;
    let mut groups: HashMap<(String, String, u64), Vec<i64>> = HashMap::new();
    for case in &built.cases {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        case.context.initial["message"]["body"]
            .as_str()
            .unwrap_or_default()
            .hash(&mut hasher);
        groups
            .entry((
                case.from_addr.to_ascii_lowercase(),
                case.subject.clone(),
                hasher.finish(),
            ))
            .or_default()
            .push(case.id);
    }
    let mut duplicates: Vec<Vec<i64>> = groups.into_values().filter(|ids| ids.len() > 1).collect();
    duplicates.sort();
    let mut balance: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut bump = |field: &str, value: String| {
        *balance
            .entry(field.to_owned())
            .or_default()
            .entry(value)
            .or_insert(0) += 1
    };
    for case in &built.cases {
        let gold = &case.gold;
        let applied = labels(gold, default_min_importance());
        for label in STRATA.iter().take(STRATA.len() - 1) {
            bump(label, applied.contains(label).to_string());
        }
        bump("state", enum_name(&gold.attention.state));
        bump(
            "reading_kind",
            gold.reading_kind
                .as_ref()
                .map(enum_name)
                .unwrap_or_else(|| "null".into()),
        );
        for kind in &gold.kinds {
            bump("kinds", enum_name(kind));
        }
        for record in &gold.records {
            bump("records", serde_json::to_value(record)?["kind"].to_string());
        }
        bump("revisit", gold.revisit.is_some().to_string());
        bump("stratum", case.tags[0].clone());
        bump("fidelity", case.fidelity.as_str().into());
    }
    let mut sizes = built.prepared_bytes.clone();
    sizes.sort_unstable();
    let over_budget: Vec<i64> = built
        .cases
        .iter()
        .zip(&built.prepared_bytes)
        .filter(|(_, bytes)| **bytes > config.context.max_context_bytes)
        .map(|(case, _)| case.id)
        .collect();
    Ok(json!({
        "duplicates": duplicates,
        "label_balance": balance,
        "user_message_bytes": {
            "min": sizes.first().copied().unwrap_or(0),
            "median": percentile(&sizes, 0.5),
            "p95": percentile(&sizes, 0.95),
            "max": sizes.last().copied().unwrap_or(0),
        },
        "over_max_context_bytes": over_budget,
        "leakage": "none",
    }))
}

fn enum_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn manifest(
    options: &ExportOptions,
    built: &Built,
    audit: Value,
    snapshot_notes: Vec<String>,
    min_importance: u8,
) -> Value {
    let mut by_stratum = BTreeMap::<String, usize>::new();
    let mut by_fidelity = BTreeMap::<String, usize>::new();
    let mut by_version = BTreeMap::<String, usize>::new();
    let mut by_model = BTreeMap::<String, usize>::new();
    for case in &built.cases {
        *by_stratum.entry(case.tags[0].clone()).or_default() += 1;
        *by_fidelity
            .entry(case.fidelity.as_str().into())
            .or_default() += 1;
        *by_version
            .entry(case.gold_meta.prompt_version.clone())
            .or_default() += 1;
        *by_model.entry(case.gold_meta.model.clone()).or_default() += 1;
    }
    json!({
        "source_db": options.db.display().to_string(),
        "exported_at": Utc::now().to_rfc3339(),
        "daemon_prompt_version": squelch_core::triage::agent::PROMPT_VERSION,
        "min_importance": min_importance,
        "cases": built.cases.len(),
        "counts": {
            "by_stratum": by_stratum,
            "by_fidelity": by_fidelity,
            "by_prompt_version": by_version,
            "by_gold_model": by_model,
        },
        "skipped": built.skipped,
        "bytes_per_token_median": built.bytes_per_token_median,
        "production_cache_hit": production_cache_hit(&built.cases),
        "snapshot": snapshot_notes,
        "audit": audit,
    })
}

/// `sum(cache_read) / sum(cache_read + cache_creation)` over the gold runs on
/// the current gold model (the model of the newest gold run).
pub fn production_cache_hit(cases: &[Case]) -> Value {
    let Some(current) = cases.iter().max_by_key(|case| case.as_of) else {
        return Value::Null;
    };
    let model = &current.gold_meta.model;
    let (mut read, mut write) = (0u64, 0u64);
    for case in cases.iter().filter(|case| &case.gold_meta.model == model) {
        for usage in &case.gold_meta.usage {
            read += usage.cache_read_input_tokens;
            write += usage.cache_creation_input_tokens;
        }
    }
    let share = if read + write == 0 {
        Value::Null
    } else {
        json!(read as f64 / (read + write) as f64)
    };
    json!({"model": model, "share": share, "cache_read_tokens": read, "cache_write_tokens": write})
}

/// The dataset to read: `explicit`, else the newest one under `root`.
pub fn resolve_dataset(root: &Path, explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = explicit {
        return Ok(dir.to_path_buf());
    }
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(root).with_context(|| format!("read {}", root.display()))? {
        let dir = entry?.path();
        let Ok(meta) = std::fs::metadata(dir.join(MANIFEST_FILE)) else {
            continue;
        };
        let modified = meta.modified()?;
        if newest.as_ref().is_none_or(|(at, _)| modified > *at) {
            newest = Some((modified, dir));
        }
    }
    newest
        .map(|(_, dir)| dir)
        .with_context(|| format!("no dataset under {}; run export first", root.display()))
}

/// `show`: one case, pretty, for human review.
pub fn show(dataset: &Path, id: i64) -> Result<String> {
    let cases = crate::case::read_cases(&dataset.join(CASES_FILE))?;
    let case = cases
        .iter()
        .find(|case| case.id == id)
        .with_context(|| format!("no case {id} in {}", dataset.display()))?;
    Ok(serde_json::to_string_pretty(case)?)
}

/// A code fence longer than any backtick run in `text`.
pub fn fence_for(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    "`".repeat(longest.max(2) + 1)
}

/// `sample`: `per_stratum` cases from each stratum (seeded), as markdown for
/// the owner's read. Returns the file written.
pub fn sample(dataset: &Path, per_stratum: usize, seed: u64) -> Result<PathBuf> {
    let cases = crate::case::read_cases(&dataset.join(CASES_FILE))?;
    let mut out = format!(
        "# Eval sample: {per_stratum} per stratum (seed {seed})\n\nRead each and say whether the gold placement is right.\n"
    );
    for stratum in STRATA {
        let mut members: Vec<&Case> = cases.iter().filter(|c| c.tags[0] == stratum).collect();
        if members.is_empty() {
            continue;
        }
        members.sort_by_key(|case| case.id);
        crate::SplitMix64::new(seed).shuffle(&mut members);
        out.push_str(&format!(
            "\n## {stratum} ({} of {})\n",
            per_stratum.min(members.len()),
            members.len()
        ));
        for case in members.into_iter().take(per_stratum) {
            out.push_str(&sample_entry(case));
        }
    }
    let path = dataset.join(format!("sample-{per_stratum}-seed{seed}.md"));
    crate::private_write(&path, out.as_bytes())?;
    Ok(path)
}

fn sample_entry(case: &Case) -> String {
    let body: String = case.context.initial["message"]["body"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .take(600)
        .collect();
    let fence = fence_for(&body);
    let gold: &MessageDecision = &case.gold;
    let records: Vec<String> = gold
        .records
        .iter()
        .filter_map(|r| serde_json::to_value(r).ok())
        .map(|r| r["kind"].as_str().unwrap_or_default().to_owned())
        .collect();
    format!(
        "\n### {id}: {subject}\n\n- from: {from}\n- as of: {as_of} ({fidelity}; tags {tags})\n\n{fence}text\n{body}\n{fence}\n\n\
         - FYE: {fye} (state {state})\n- Reading: {reading} (kind {reading_kind})\n- kinds: {kinds}\n\
         - auth: {auth}; restricted: {restricted}\n- push: {push} (importance {importance})\n- records: {records}\n\
         - summary: {summary}\n- reason: {reason}\n",
        id = case.id,
        subject = case.subject,
        from = case.from_addr,
        as_of = case.as_of.to_rfc3339(),
        fidelity = case.fidelity.as_str(),
        tags = case.tags.join(", "),
        fye = gold.attention.show_in_fye,
        state = enum_name(&gold.attention.state),
        reading = gold
            .destinations
            .contains(&squelch_core::triage::decision::MessageDestination::Reading),
        reading_kind = gold
            .reading_kind
            .as_ref()
            .map(enum_name)
            .unwrap_or_else(|| "none".into()),
        kinds = gold
            .kinds
            .iter()
            .map(enum_name)
            .collect::<Vec<_>>()
            .join(", "),
        auth = gold
            .auth
            .kinds
            .iter()
            .map(enum_name)
            .collect::<Vec<_>>()
            .join(", "),
        restricted = gold.external_access.restricted,
        push = is_push(gold, default_min_importance()),
        importance = gold.notification.importance,
        records = if records.is_empty() {
            "none".into()
        } else {
            records.join(", ")
        },
        summary = gold.summary,
        reason = gold.reason,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{self, Fixture};
    use chrono::Duration;

    fn exported() -> (Fixture, PathBuf, Vec<Case>, Value) {
        let fixture = fixture::build();
        let out = fixture.dir.join("evals");
        let outcome = export(&ExportOptions {
            db: fixture.db.clone(),
            out: out.clone(),
            name: "set".into(),
        })
        .unwrap();
        let cases = crate::case::read_cases(&outcome.dir.join(CASES_FILE)).unwrap();
        (fixture, outcome.dir, cases, outcome.manifest)
    }

    fn case<'a>(cases: &'a [Case], fixture: &Fixture, name: &str) -> &'a Case {
        cases.iter().find(|c| c.id == fixture.ids[name]).unwrap()
    }

    fn thread_ids(case: &Case) -> Vec<i64> {
        case.context.initial["thread"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_i64().unwrap())
            .collect()
    }

    #[test]
    fn eligibility_keeps_single_run_inbound_gold_only() {
        let (fixture, _, cases, manifest) = exported();
        let mut ids: Vec<i64> = cases.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        let mut want: Vec<i64> = ["A", "B", "C", "D", "G"]
            .iter()
            .map(|n| fixture.ids[n])
            .collect();
        want.sort_unstable();
        assert_eq!(ids, want);
        assert_eq!(
            manifest["skipped"],
            json!({"multiple_applied_triage_runs": 2, "spam": 1})
        );
        assert_eq!(manifest["cases"], 5);
        let a = case(&cases, &fixture, "A");
        assert_eq!(a.gold_meta.usage[0].input_tokens, 5_000);
        assert_eq!(a.gold_meta.model, "claude-opus-5-5");
        assert_eq!(a.as_of, fixture.t0 + Duration::minutes(10));
        assert_eq!(a.trigger, "arrival");
    }

    #[test]
    fn as_of_drops_later_sibling_rule_contact_and_user_state() {
        let (fixture, _, cases, _) = exported();
        let (a, b, d) = (
            case(&cases, &fixture, "A"),
            case(&cases, &fixture, "B"),
            case(&cases, &fixture, "D"),
        );
        // A lone message with nothing dropped is exact, and new at its T.
        assert_eq!(a.fidelity, Fidelity::Exact, "{:?}", a.fidelity_notes);
        assert!(thread_ids(a).is_empty());
        assert_eq!(a.context.initial["message"]["status"], "new");
        assert_eq!(a.context.initial["now"], json!(a.as_of.to_rfc3339()));
        // B: C was ingested after B's run; A was resolved before it; B itself
        // had not been surfaced or opened yet.
        assert_eq!(thread_ids(b), vec![fixture.ids["A"]]);
        assert_eq!(b.context.source_message_ids.len(), 2);
        assert_eq!(b.fidelity, Fidelity::Reconstructed);
        assert!(b.tags.contains(&"thread".to_owned()));
        assert_eq!(b.context.initial["thread"][0]["status"], "done");
        assert_eq!(b.context.initial["message"]["status"], "new");
        assert_eq!(b.context.initial["message"]["opened_at"], Value::Null);
        assert_eq!(b.context.initial["previous_decision"], Value::Null);
        assert_eq!(b.context.initial["sender_is_contact"], true);
        // D: the rule and the contact postdate its run.
        assert_eq!(d.context.rule_ids, vec![fixture.kept_rule]);
        assert_ne!(fixture.kept_rule, fixture.dropped_rule);
        assert_eq!(d.context.initial["sender_is_contact"], false);
        assert_eq!(d.fidelity, Fidelity::Reconstructed);
        let notes = d.fidelity_notes.join("\n");
        assert!(
            notes.contains("rule") && notes.contains("contact"),
            "{notes}"
        );
        let suspect: Vec<i64> = cases
            .iter()
            .filter(|c| c.fidelity == Fidelity::Suspect)
            .map(|c| c.id)
            .collect();
        assert_eq!(suspect, vec![fixture.ids["G"]]);
    }

    #[test]
    fn attention_is_the_latest_decision_before_t_with_its_revision_count() {
        let (fixture, _, cases, _) = exported();
        let (a, b, c) = (
            case(&cases, &fixture, "A"),
            case(&cases, &fixture, "B"),
            case(&cases, &fixture, "C"),
        );
        assert_eq!(a.context.initial["attention"], Value::Null);
        assert_eq!(a.context.thread_revisions["t1"], 0);
        // B sees A's attention, with the id production would have assigned.
        assert_eq!(
            b.context.initial["attention"]["summary"],
            "attention after A"
        );
        assert_eq!(
            b.context.initial["attention"]["actions"][0]["id"],
            format!("act-{}-0", fixture.ids["A"])
        );
        assert_eq!(b.context.thread_revisions["t1"], 1);
        // C sees B's, never its own (decided a millisecond before its T).
        assert_eq!(
            c.context.initial["attention"]["summary"],
            "attention after B"
        );
        assert_eq!(c.context.thread_revisions["t1"], 2);
        assert_eq!(thread_ids(c).len(), 2);
        assert_eq!(c.context.initial["thread"][0]["status"], "open");
    }

    #[test]
    fn snapshot_sheds_html_and_never_touches_the_source() {
        let (fixture, dir, _, manifest) = exported();
        let source = Connection::open(&fixture.db).unwrap();
        let live: i64 = source
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE body_html IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(live, 8);
        let copy = Connection::open(dir.join(SNAPSHOT_FILE)).unwrap();
        let left: i64 = copy
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE body_html IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, 0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir), 0o700);
            assert_eq!(mode(&dir.join(CASES_FILE)), 0o600);
            assert_eq!(mode(&dir.join(MANIFEST_FILE)), 0o600);
            assert_eq!(mode(&dir.join(SNAPSHOT_FILE)), 0o600);
        }
        assert_eq!(manifest["production_cache_hit"]["share"], json!(1.0));
        let notes = manifest["snapshot"].to_string();
        assert!(notes.contains("body_html cleared on 8"), "{notes}");
        assert!(notes.contains("message_vecs cleared"), "{notes}");
        assert_eq!(manifest["audit"]["over_max_context_bytes"], json!([]));
    }

    /// A sibling decided more than once has lost its earlier decision, so the
    /// attention the gold run saw cannot be rebuilt.
    #[test]
    fn a_thread_with_a_multi_run_message_is_suspect() {
        let (fixture, _, cases, _) = exported();
        let g = case(&cases, &fixture, "G");
        assert_eq!(g.fidelity, Fidelity::Suspect);
        assert_eq!(thread_ids(g), vec![fixture.ids["F"]]);
        let notes = g.fidelity_notes.join("\n");
        assert!(
            notes.contains(&format!("message {} has 2 applied runs", fixture.ids["F"])),
            "{notes}"
        );
    }

    #[test]
    fn leakage_check_names_the_case() {
        let (fixture, _, mut cases, _) = exported();
        assert!(check_leakage(&cases).is_ok());
        let b = cases.iter_mut().find(|c| c.id == fixture.ids["B"]).unwrap();
        b.context.initial["thread"][0]["body"] = json!(format!("quoted: {}", b.gold.reason));
        let error = check_leakage(&cases).unwrap_err().to_string();
        assert!(
            error.contains(&format!("case {}", fixture.ids["B"])),
            "{error}"
        );
        assert!(error.contains("reason"));
    }

    #[test]
    fn size_check_flags_prompts_far_from_their_version_median() {
        let v = |s: &str| s.to_owned();
        let samples = vec![
            (v("v5"), 4_000, 1_000),
            (v("v5"), 8_000, 2_000),
            (v("v5"), 20_000, 1_000),
            (v("v5"), 1_000, 1_000),
            (v("v1"), 1_000, 1_000),
            (v("v1"), 500, 0),
        ];
        let (medians, checks) = size_outliers(&samples);
        assert_eq!(medians["v5"], 4.0);
        assert_eq!(checks[0], SizeCheck::Ok);
        assert_eq!(checks[1], SizeCheck::Ok);
        assert!(matches!(checks[2], SizeCheck::Outlier { .. }));
        assert!(matches!(checks[3], SizeCheck::Outlier { .. }));
        assert_eq!(checks[4], SizeCheck::Ok);
        assert_eq!(checks[5], SizeCheck::Unchecked);
    }

    #[test]
    fn show_and_sample_read_the_dataset() {
        let (fixture, dir, _, _) = exported();
        assert!(show(&dir, fixture.ids["B"]).unwrap().contains("Subject B"));
        assert!(show(&dir, -1).is_err());
        let path = sample(&dir, 1, 3).unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("## fye (1 of 3)"), "{text}");
        assert!(text.contains("## reading (1 of 2)"));
        assert_eq!(fence_for("a ```` b"), "`````");
        assert_eq!(fence_for("plain"), "```");
        assert_eq!(
            resolve_dataset(&fixture.dir.join("evals"), None).unwrap(),
            dir
        );
    }
}
