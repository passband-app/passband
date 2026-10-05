//! squelch-eval: build triage eval datasets from the local daemon db, replay
//! them against candidate models, and compare. See evals/README.md.
use anyhow::Result;
use clap::{Parser, Subcommand};
use squelch_eval::export::{ExportOptions, export, resolve_dataset, sample, show};
use squelch_eval::report::{ReportOptions, Verdict, adjudicate, report};
use squelch_eval::run::{FidelityFilter, RunOptions, init, load_candidates, run};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "squelch-eval", about = "Triage model comparison harness")]
struct Cli {
    /// Where datasets live. Never inside a repository: it is personal mail.
    #[arg(long, global = true, env = "SQUELCH_EVAL_ROOT")]
    root: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Snapshot the daemon db and build a dataset of gold cases.
    Export {
        /// The daemon db. Opened read-only, once, for VACUUM INTO.
        #[arg(long)]
        db: Option<PathBuf>,
        /// Parent directory for the dataset (default: the eval root).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Dataset directory name (default: mailbox-<UTC date>).
        #[arg(long)]
        name: Option<String>,
    },
    /// Write <root>/candidates.toml with commented examples, if absent.
    Init,
    /// Replay a dataset against a candidate model. Makes PAID calls.
    Run {
        /// A `[candidate.NAME]` table in candidates.toml.
        #[arg(long)]
        candidate: String,
        #[arg(long)]
        dataset: Option<PathBuf>,
        #[arg(long, default_value_t = 1)]
        reps: u32,
        #[arg(long, default_value_t = 4)]
        concurrency: usize,
        /// Stratified sample of about N cases, at least --floor per stratum.
        #[arg(long)]
        sample: Option<usize>,
        /// Smallest per-stratum sample; raise it to take rare strata whole.
        #[arg(long, default_value_t = squelch_eval::run::SAMPLE_FLOOR)]
        floor: usize,
        #[arg(long, default_value_t = 0)]
        seed: u64,
        /// exact, reconstructed (default: everything but suspect), or all.
        #[arg(long, value_enum, default_value_t = FidelityFilter::Reconstructed)]
        fidelity: FidelityFilter,
        /// Only these case ids.
        #[arg(long = "case")]
        cases: Vec<i64>,
        /// Override the candidate's effort.
        #[arg(long)]
        effort: Option<String>,
        /// Consent to the paid calls the run prints before starting.
        #[arg(long)]
        yes: bool,
        /// Stop starting new cases once this run's observed spend (USD,
        /// earlier sessions included) reaches this. Needs a priced model.
        #[arg(long)]
        max_usd: Option<f64>,
    },
    /// Score runs against gold and compare them.
    Report {
        /// Run directory names under <dataset>/runs (default: all).
        runs: Vec<String>,
        #[arg(long)]
        dataset: Option<PathBuf>,
        /// Report paired deltas against this run.
        #[arg(long)]
        baseline: Option<String>,
        /// List this run's disagreements with gold, highest harm first.
        #[arg(long)]
        diff: Option<String>,
        /// Prefix cache hit share for projected cost (default: the manifest's).
        #[arg(long)]
        cache_hit: Option<f64>,
        /// Also write the report as markdown. It can name senders and subjects.
        #[arg(long)]
        md: Option<PathBuf>,
        /// Allow --md inside a git repository.
        #[arg(long)]
        allow_repo_path: bool,
        #[arg(long, default_value_t = squelch_eval::report::DEFAULT_SEED)]
        seed: u64,
        /// With --diff RUN: also list the N rows whose user-facing text is
        /// least like gold's, side by side.
        #[arg(long, requires = "diff")]
        texts: Option<usize>,
        /// Skip text similarity (no embedding model is loaded).
        #[arg(long)]
        no_text: bool,
        /// Score against this run's decisions instead of gold (cases it did
        /// not score ok are left out; adjudications do not apply).
        #[arg(long)]
        reference: Option<String>,
    },
    /// Record a ruling on one field of one case.
    Adjudicate {
        case_id: i64,
        #[arg(long)]
        field: String,
        #[arg(long, value_enum)]
        verdict: Verdict,
        /// The run whose value is being ruled on.
        #[arg(long)]
        run: String,
        #[arg(long)]
        dataset: Option<PathBuf>,
    },
    /// Pretty-print one case.
    Show {
        case_id: i64,
        #[arg(long)]
        dataset: Option<PathBuf>,
    },
    /// Write N cases per stratum as markdown for review.
    Sample {
        #[arg(long)]
        per_stratum: usize,
        #[arg(long, default_value_t = 0)]
        seed: u64,
        #[arg(long)]
        dataset: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = cli.root.unwrap_or_else(squelch_eval::default_root);
    match cli.command {
        Command::Export { db, out, name } => {
            let options = ExportOptions {
                db: db.unwrap_or_else(squelch_core::config::default_db_path),
                out: out.unwrap_or(root),
                name: name.unwrap_or_else(|| {
                    format!("mailbox-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"))
                }),
            };
            let outcome = export(&options)?;
            println!("{}", serde_json::to_string_pretty(&outcome.manifest)?);
            let gold = &outcome.manifest["gold_validation"];
            println!(
                "gold validation: {} valid, {} invalid with no tools (marked suspect), {} invalid citing tool-fetched evidence",
                gold["valid"], gold["invalid_no_tools"], gold["invalid_with_tools"]
            );
            println!(
                "{} cases written to {}",
                outcome.cases,
                outcome.dir.display()
            );
        }
        Command::Init => {
            let (path, written) = init(&root)?;
            if written {
                println!("wrote {}", path.display());
            } else {
                println!("{} already exists; left as is", path.display());
            }
        }
        Command::Run {
            candidate,
            dataset,
            reps,
            concurrency,
            sample,
            floor,
            seed,
            fidelity,
            cases,
            effort,
            yes,
            max_usd,
        } => {
            let candidates = load_candidates(&root)?;
            let Some(config) = candidates.get(&candidate).cloned() else {
                anyhow::bail!(
                    "no [candidate.{candidate}] in candidates.toml (have: {})",
                    candidates.keys().cloned().collect::<Vec<_>>().join(", ")
                );
            };
            let dataset = resolve_dataset(&root, dataset.as_deref())?;
            let summary = run(&RunOptions {
                dataset,
                candidate_name: candidate,
                candidate: config,
                reps,
                concurrency,
                sample: sample.map(|n| (n, seed)),
                sample_floor: floor,
                fidelity,
                cases,
                effort,
                yes,
                backoff: std::time::Duration::from_secs(2),
                max_usd,
            })
            .await?;
            println!(
                "{} ok, {} failed, {} infra errors, {} unfinished, {} already done; {}",
                summary.ok,
                summary.failed,
                summary.infra_errors,
                summary.unfinished,
                summary.already_done,
                summary.dir.display()
            );
        }
        Command::Report {
            runs,
            dataset,
            baseline,
            diff,
            cache_hit,
            md,
            allow_repo_path,
            seed,
            texts,
            no_text,
            reference,
        } => {
            if let Some(hit) = cache_hit
                && !(0.0..=1.0).contains(&hit)
            {
                anyhow::bail!("--cache-hit is a share between 0 and 1");
            }
            let dataset = resolve_dataset(&root, dataset.as_deref())?;
            let options = ReportOptions {
                runs,
                baseline,
                diff,
                cache_hit,
                seed,
                texts,
                no_text,
                reference,
            };
            print!(
                "{}",
                report(&dataset, &options, md.as_deref(), allow_repo_path)?
            );
        }
        Command::Adjudicate {
            case_id,
            field,
            verdict,
            run,
            dataset,
        } => {
            let dataset = resolve_dataset(&root, dataset.as_deref())?;
            let ruling = adjudicate(&dataset, case_id, &field, verdict, &run)?;
            println!("{}", serde_json::to_string(&ruling)?);
        }
        Command::Show { case_id, dataset } => {
            let dataset = resolve_dataset(&root, dataset.as_deref())?;
            println!("{}", show(&dataset, case_id)?);
        }
        Command::Sample {
            per_stratum,
            seed,
            dataset,
        } => {
            let dataset = resolve_dataset(&root, dataset.as_deref())?;
            println!("{}", sample(&dataset, per_stratum, seed)?.display());
        }
    }
    Ok(())
}
