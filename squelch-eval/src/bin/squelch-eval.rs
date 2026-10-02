//! squelch-eval: build triage eval datasets from the local daemon db, replay
//! them against candidate models, and compare. See evals/README.md.
use anyhow::Result;
use clap::{Parser, Subcommand};
use squelch_eval::export::{ExportOptions, export, resolve_dataset, sample, show};
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
        /// Stratified sample of about N cases (floor 15 per stratum).
        #[arg(long)]
        sample: Option<usize>,
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
            seed,
            fidelity,
            cases,
            effort,
            yes,
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
                fidelity,
                cases,
                effort,
                yes,
                backoff: std::time::Duration::from_secs(2),
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
