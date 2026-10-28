//! `squelchd` — the squelch daemon / CLI.
//!
//! - `auth`: run the OAuth consent flow and store tokens (keyring or file).
//! - `run`: sync-only loop, no HTTP.
//! - `serve`: sync loop plus one axum server hosting the agent door (`/mcp`) and
//!   the human door (`/client/*`). The agent door is open (Host guard only) on
//!   self-host, or requires OAuth bearer tokens when `SQUELCH_MCP_OAUTH_ISSUER`
//!   and `SQUELCH_MCP_PUBLIC_URL` are set; see `squelch_mcp::auth`.
//! - `token`: issue / list / revoke the human door's per-device tokens, plus
//!   `first-paired`, the one machine-readable line the hosted warden reads.
//! - `pair`: mint a short code a new device trades for one of those tokens.

use chrono::{Duration, SecondsFormat};
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use squelch_core::auth::{
    AuthFlowOptions, AuthScopes, ConsentBind, CredentialTransfer, DEFAULT_HEADLESS_PORT,
    TransferCredential, check_token_covers_slot, decode_transfer, encode_transfer, printable,
    run_auth_flow, run_broker_flow, run_export_flow, verify_transfer_credential,
};
use squelch_core::config::{Config, CredentialBackend, OAuthClientConfig, Stage2CapSources};
use squelch_core::credentials::{
    CredentialKind, CredentialStore, ensure_backend, load_token_backend, store_for_backend,
    store_token_backend, store_tokens_backend, write_private,
};
use squelch_core::embed::{Embedder, LazyEmbedder, ReapOutcome};
use squelch_core::metrics::SyncMetrics;
use squelch_core::store::sqlite::device_tokens::PAIRING_TTL_SECS;
use squelch_core::store::{SqliteStore, Store};
use squelch_core::sync::{SyncEngine, wait_for_embedder_gate};
use squelch_core::types::AccountId;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio_util::sync::CancellationToken;

/// Loopback only, by design: a reverse proxy (`tailscale serve`) fronts this.
const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8848";

#[derive(Parser)]
#[command(
    name = "squelchd",
    about = "squelch local-first email intelligence daemon"
)]
struct Cli {
    /// Path to config.toml (defaults to ~/.config/squelch/config.toml).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Authorize a Gmail account and store tokens in the configured backend
    /// (OS keyring, or a mode-0600 JSON file on headless hosts).
    ///
    /// One Gmail consent (`gmail.modify`, the only Gmail scope Passband asks
    /// for), stored in BOTH the Read slot used by sync and the Write slot used
    /// by human-door actions; sync/triage never load the write one. Then an
    /// optional Google Calendar consent.
    ///
    /// HEADLESS: `--headless [--port N]` prints the consent URL and binds a
    /// FIXED loopback port (default 8847) to forward with
    /// `ssh -L 8847:127.0.0.1:8847 <host>`; both flows reuse that one port.
    /// `--broker <URL>` needs no port and no tunnel at all: consent lands on a
    /// relay that parks the code for this daemon to collect.
    ///
    /// HEADLESS WITHOUT A TUNNEL OR A RELAY: run `squelchd auth --export` on a
    /// machine that has a browser and pipe the blob it prints into
    /// `squelchd auth --import` on the daemon's host. Both machines must use the
    /// same OAuth client_id/client_secret.
    Auth(AuthArgs),
    /// Run the sync loop only. No HTTP doors are served.
    Run,
    /// Run the sync loop plus one HTTP server hosting both the agent door
    /// (`/mcp`) and the human door (`/client/*`).
    Serve(ServeArgs),
    /// Issue, list and revoke the per-device tokens the human door accepts.
    ///
    /// One token per device means one revocation per device: losing a phone
    /// costs that phone's row, not a rotation of the shared secret on every
    /// client you own. `SQUELCH_API_TOKEN` keeps working alongside these and is
    /// never deprecated.
    Token(TokenArgs),
    /// Mint a short pairing code (and a deep link) a new device can trade for
    /// its own token.
    ///
    /// The device claims it at `POST /client/pair`, which is the one human-door
    /// route served without a bearer, since the device has none yet. One code is
    /// live per account at a time, it expires in minutes, and it is spent on
    /// first use or after a handful of failed claims.
    Pair(PairArgs),
    /// See where this mailbox stands against its daily triage caps, and lift
    /// one for the rest of the day.
    Triage(TriageArgs),
}

#[derive(Args)]
struct TriageArgs {
    #[command(subcommand)]
    command: TriageCommand,
}

#[derive(Subcommand)]
enum TriageCommand {
    /// Print today's runs against the daily caps and how many jobs they are
    /// holding until tomorrow.
    Status,
    /// Allow RUNS more investigations today and let the held jobs run now.
    ///
    /// Works on a running daemon: the worker reads the grant on its next job.
    /// The grant lasts until midnight UTC and REPLACES any earlier grant for
    /// the day, so `grant 0` takes it back. Each run is a model job, so this
    /// spends against the LLM budget.
    Grant {
        /// Extra runs for today, on top of the configured cap.
        runs: u32,
        /// Raise the background cap (backfill, refreshes, revisits) instead of
        /// the total. Background runs still count toward the total, so this
        /// only helps while the total has room.
        #[arg(long)]
        background: bool,
        /// Raise the access lane's cap (the owner's sent mail, spam, archive
        /// rows a caller asked to read) instead of the total. Access runs
        /// count toward the total too.
        #[arg(long, conflicts_with = "background")]
        access: bool,
    },
}

#[derive(Args)]
struct TokenArgs {
    #[command(subcommand)]
    command: TokenCommand,
}

#[derive(Subcommand)]
enum TokenCommand {
    /// Mint a token and print it ONCE. squelchd stores only a hash and cannot
    /// show it again; a lost token is re-issued, never recovered.
    Issue {
        /// The label this token carries in `token list`, so the row to revoke
        /// later is the one you can recognize ("braelyn's iphone").
        #[arg(long)]
        name: String,
    },
    /// List every token this account has been issued, revoked ones included.
    /// Prints no token material, not even hashes.
    List,
    /// Revoke a token by id. Effective on the very next request: the door reads
    /// the row every time and caches nothing.
    Revoke {
        /// The id from `token list`.
        id: i64,
    },
    /// Print when a client device FIRST paired with this mailbox: one RFC3339
    /// timestamp, or the word `none`.
    ///
    /// MACHINE-READABLE, not a report. The hosted warden execs exactly this in a
    /// tenant pod to answer "did anybody ever actually run the app" (the
    /// activation signal), so the contract is one line on stdout and nothing
    /// else, forever.
    ///
    /// Its own subcommand rather than a flag on `token list`, and that is the
    /// point: a listing carries device NAMES, and a caller that only needs a
    /// timestamp must not be able to over-share by accident. One line out means
    /// there is nothing else in it to leak. The console-session exclusion and
    /// the "a revoked device still paired" rule both live in the store, in one
    /// place, rather than being re-derived by whoever reads the output.
    FirstPaired,
}

#[derive(Args)]
struct PairArgs {
    /// The base URL the device should claim against, baked into the deep link.
    /// Defaults to this daemon's loopback address, which is right when the
    /// device is this machine; point it at whatever a phone can actually reach
    /// (a `tailscale serve` hostname, say) when it is not.
    #[arg(long)]
    url: Option<String>,
}

#[derive(Args)]
struct ServeArgs {
    /// Address to bind both doors to; defaults to `127.0.0.1:8848`, overridable
    /// via `SQUELCH_BIND`. Keep it on loopback behind a reverse proxy.
    #[arg(long)]
    bind: Option<String>,
}

#[derive(Args)]
struct AuthArgs {
    /// Kept so existing scripts still run. Plain `auth` now fills the Write
    /// slot too (one `gmail.modify` consent), so this changes nothing.
    #[arg(long, hide = true)]
    write: bool,

    /// Mint the CALENDAR credential (calendar.events +
    /// calendar.calendarlist.readonly) into its own slot, and touch nothing
    /// else: the Read and Write credentials stay exactly as they are.
    ///
    /// Optional, and only for the calendar page. Plain `squelchd auth` already
    /// offers this consent at the end while no calendar credential is stored;
    /// this flag is for connecting it later. Run plain `squelchd auth` first:
    /// the consent is matched to this mailbox through the Gmail access this
    /// OAuth client already has. Works with --headless, --broker and
    /// --export. Conflicts with --write, which renews the Gmail pair, and with
    /// --import, where the blob decides what is stored.
    #[arg(long, conflicts_with_all = ["write", "import"])]
    calendar: bool,

    /// Headless mode: do NOT auto-open a browser, and bind the loopback
    /// listener to a FIXED port so it can be SSH-forwarded.
    #[arg(long)]
    headless: bool,

    /// Fixed port for --headless, and for --export --expose-consent-listener
    /// (default 8847). Ignored otherwise.
    #[arg(long, default_value_t = DEFAULT_HEADLESS_PORT)]
    port: u16,

    /// Run consent through a hosted consent relay instead of a loopback
    /// listener.
    ///
    /// For hosts where no browser can reach 127.0.0.1 (docker, a NAS, a VPS):
    /// this prints a link to open on any device, and the broker parks Google's
    /// one-time code until this daemon collects it. The broker never sees a
    /// token and cannot mint one: the PKCE verifier stays here. Replaces
    /// --headless, --port, --export, and --import rather than decorating them.
    // NO `conflicts_with_all` here, and none naming "broker" elsewhere: clap
    // counts an env-sourced value as present for conflicts, so with
    // SQUELCH_BROKER_URL exported every one of those pairs would fail on a flag
    // the operator never typed — including `--import`, which is the sanctioned
    // replacement for the broker (docs/BROKER.md: DO NOT DEPLOY). The conflicts
    // are enforced in `resolve_transport`, which can see WHERE the value came
    // from and only refuses what was actually asked for on the command line.
    #[arg(long, value_name = "URL", env = "SQUELCH_BROKER_URL")]
    broker: Option<String>,

    /// Run consent HERE and print a credential blob on stdout; store nothing.
    ///
    /// For a daemon no browser can reach at all: run this on a laptop, then feed
    /// the one line it prints to `squelchd auth --import` on the daemon's host.
    /// Google delivers a code only to loopback on the machine running the
    /// browser, so the TOKEN moves instead of the code. Needs no account_email
    /// configured, and reports on stderr which mailbox Google named; when one IS
    /// configured, the consent must land on it. Write the blob with
    /// `--out <path>` (mode 0600) rather than a redirect, which takes the
    /// ambient umask. With --write the blob carries BOTH credentials. Replaces
    /// the loopback and broker transports.
    #[arg(long, conflicts_with_all = ["headless", "import"])]
    export: bool,

    /// With --export: write the blob to PATH, created mode 0600, instead of
    /// stdout.
    ///
    /// `--export > cred.txt` creates that file with the ambient umask, which is
    /// 0644 on most hosts, and the file is a live refresh token. This is the
    /// same private write every other credential in squelch gets.
    #[arg(
        long,
        value_name = "PATH",
        requires = "export",
        conflicts_with = "import"
    )]
    out: Option<PathBuf>,

    /// Store a credential blob read from STDIN, minted by `--export` elsewhere.
    ///
    /// Never argv: the blob is a live refresh token, and argv is visible in `ps`
    /// and lands in shell history. The blob's mailbox must match this daemon's
    /// configured account_email. What it carries decides what is stored, so this
    /// conflicts with --write.
    #[arg(long, conflicts_with_all = ["headless", "write", "export"])]
    import: bool,

    /// With --export: bind the consent listener to every interface instead of
    /// loopback, on --port.
    ///
    /// For running the export INSIDE a container, where `docker run -p
    /// 8847:8847` cannot reach a listener on the container's own 127.0.0.1.
    /// EXPOSURE: for the length of one consent, anything that can route to this
    /// host on that port can connect to the listener. What it could deliver is
    /// an authorization code, which is useless without the PKCE verifier held in
    /// this process and is checked against a per-run `state` first. Still a real
    /// change in reach, which is why it is opt-in.
    // `conflicts_with` is spelled out alongside `requires` because clap DROPS a
    // requirement that conflicts with a flag already on the line: with
    // `requires = "export"` alone, `--import --expose-consent-listener` parses.
    #[arg(long, requires = "export", conflicts_with = "import")]
    expose_consent_listener: bool,
}

/// Ceiling on a pasted credential blob. A real one is about a kilobyte; this is
/// slack, and it exists so a redirected file or a wedged pipe cannot be read
/// into memory without bound.
const MAX_BLOB_BYTES: usize = 64 * 1024;

/// Where one flag's value came from.
///
/// `--broker` also reads `SQUELCH_BROKER_URL`, and clap counts an env-sourced
/// value as PRESENT for conflict resolution. That is why the broker's conflicts
/// are enforced here instead: an ambient URL in the environment must not refuse
/// a transport the operator asked for by name, least of all `--import`, which is
/// what replaced the broker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlagSource {
    /// Typed on this command line.
    CommandLine,
    /// A default or an environment variable: inherited, not asked for.
    Ambient,
}

/// Which of `auth`'s transport flags were typed rather than inherited.
#[derive(Debug, Clone, Copy)]
struct AuthFlagSources {
    broker: FlagSource,
    port: FlagSource,
}

impl AuthFlagSources {
    /// Read clap's own record of where each value came from.
    fn from_matches(matches: &clap::ArgMatches) -> Self {
        let Some(auth) = matches.subcommand_matches("auth") else {
            return Self {
                broker: FlagSource::Ambient,
                port: FlagSource::Ambient,
            };
        };
        let source = |id: &str| match auth.value_source(id) {
            Some(clap::parser::ValueSource::CommandLine) => FlagSource::CommandLine,
            _ => FlagSource::Ambient,
        };
        Self {
            broker: source("broker"),
            port: source("port"),
        }
    }
}

/// Which consent transport one `auth` run uses. Each REPLACES the others rather
/// than decorating them.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Transport {
    /// Consent here, print a blob, store nothing.
    Export,
    /// Store a blob minted elsewhere.
    Import,
    /// Consent lands on a relay that parks the code.
    Broker(String),
    /// Consent lands on a listener this process owns.
    Loopback,
}

/// The broker URL this run actually has, and the one notion of "a broker is
/// present" every decision here uses.
///
/// clap reports `SQUELCH_BROKER_URL=` as a VALUE, so an empty variable left in a
/// compose file would otherwise make plain `auth` a `Broker("")` run that dies on
/// a URL parse nobody asked for. Blank is how an environment says nothing. A
/// `--broker ""` that was TYPED is a different statement and still fails in
/// `broker_base`, where the operator can see what they typed.
fn broker_url(args: &AuthArgs, sources: AuthFlagSources) -> Option<&str> {
    let url = args.broker.as_deref()?;
    if sources.broker == FlagSource::CommandLine {
        return Some(url);
    }
    (!url.trim().is_empty()).then_some(url)
}

/// Pick the transport, and refuse only the combinations the operator actually
/// asked for.
///
/// An ambient `SQUELCH_BROKER_URL` is a default, not an instruction: any
/// transport named on the command line wins over it, silently and without an
/// exit 2 about a flag nobody typed. A `--broker` that WAS typed still conflicts
/// with every other transport, which is the check clap can no longer make.
fn resolve_transport(
    args: &AuthArgs,
    sources: AuthFlagSources,
) -> Result<Transport, squelch_core::CoreError> {
    let broker = broker_url(args, sources);
    let typed_broker = broker.is_some() && sources.broker == FlagSource::CommandLine;
    let refuse = |other: &str| {
        Err(other_err(format!(
            "--broker and {other} are two different ways to run consent, and this run asked for \
             both; pick one."
        )))
    };

    if args.export {
        if typed_broker {
            return refuse("--export");
        }
        return Ok(Transport::Export);
    }
    if args.import {
        if typed_broker {
            return refuse("--import");
        }
        return Ok(Transport::Import);
    }
    match broker {
        Some(url) if typed_broker => {
            if args.headless {
                return refuse("--headless");
            }
            if sources.port == FlagSource::CommandLine {
                return refuse("--port");
            }
            Ok(Transport::Broker(url.to_string()))
        }
        // Inherited from the environment: it is the transport only when nothing
        // on the command line names another one.
        Some(url) => {
            if args.headless || sources.port == FlagSource::CommandLine {
                Ok(Transport::Loopback)
            } else {
                Ok(Transport::Broker(url.to_string()))
            }
        }
        None => Ok(Transport::Loopback),
    }
}

/// Whether a `--port` typed on THIS command line will go unbound.
///
/// A fixed port is bound by `--headless`, which is what an SSH tunnel forwards,
/// and by `--export --expose-consent-listener`, which is what a published
/// container port maps to. Every other run takes an ephemeral port, so a typed
/// `--port` there is a tunnel or a `-p` mapping that will never be connected to,
/// and silence about it looks exactly like a port that was honored.
///
/// A typed port beside a TYPED `--broker` never reaches this: `resolve_transport`
/// refuses that pair. Beside an AMBIENT one it resolves to `Loopback`, where the
/// port is still ignored unless the run is headless.
fn typed_port_is_ignored(args: &AuthArgs, transport: &Transport, sources: AuthFlagSources) -> bool {
    if sources.port != FlagSource::CommandLine {
        return false;
    }
    let bound = match transport {
        Transport::Loopback => args.headless,
        Transport::Export => args.expose_consent_listener,
        Transport::Import | Transport::Broker(_) => false,
    };
    !bound
}

fn other_err(msg: String) -> squelch_core::CoreError {
    squelch_core::CoreError::Other(anyhow::anyhow!(msg))
}

/// How long shutdown waits for the calendar sync loop before aborting it.
const CALENDAR_STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// The floor on tokio worker threads, whatever the box reports.
///
/// Left to its default the runtime sizes itself by
/// `available_parallelism()`, which honours the cgroup CPU quota, and the
/// hosted tenant pod's limit is one CPU: ONE worker thread, on which both
/// HTTP doors, every SSE pump, every timer, and the sync engine's and triage
/// worker's direct store calls all run. The store is a `Mutex<Connection>`,
/// so whenever a `spawn_blocking` handler, an embed or a triage commit holds
/// it, that lone worker parks in `lock()` and nothing else progresses: no
/// accept, no auth, no keep-alive, no tick.
///
/// Four, because three always-on tasks make direct store calls and can all
/// be parked in `lock()` at the same moment (the sync engine, the triage
/// worker, an MCP tool call), and the door should still answer then. Workers
/// park when idle, so on a one-core box the extra three cost a stack each
/// and nothing more. A floor and not a cap: a self-host box with eight cores
/// keeps eight, as it always has.
const MIN_WORKER_THREADS: usize = 4;

/// tokio's own override. Read here, because calling `worker_threads` on the
/// builder is exactly what makes tokio stop reading it, and an operator's
/// no-rebuild knob for a stalled daemon should not vanish silently.
const WORKER_THREADS_ENV: &str = "TOKIO_WORKER_THREADS";

/// The box's parallelism as the runtime sees it: cgroup-quota-aware, so on a
/// hosted pod it reads 1 whatever the node has.
fn available_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// Worker threads for a runtime: the operator's override when it is set and a
/// positive integer, else the box's `parallelism`; either floored at
/// [`MIN_WORKER_THREADS`].
fn worker_threads_for(parallelism: usize, env_override: Option<&str>) -> usize {
    env_override
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(parallelism)
        .max(MIN_WORKER_THREADS)
}

/// How long after boot the planner-statistics pass runs, then hourly.
///
/// A fresh install's first hour (the initial backfill and the first triage
/// burst) is the heaviest of its life, and with no statistics at all it is
/// exactly the pathology the pass exists to fix, so the first ticks come
/// early; an upgraded install gets its first full pass a minute in, with the
/// door already bound. Hourly after that keeps up with a process that runs
/// for weeks while its tables grow.
const STATISTICS_REFRESH_AFTER_SECS: [u64; 3] = [60, 540, 3000];

/// Spawn the planner-statistics pass ([`SqliteStore::refresh_statistics`])
/// on [`STATISTICS_REFRESH_AFTER_SECS`], then hourly, for both long-lived
/// modes (`serve` and `run`). Off the open path on purpose: see the store
/// method. Behind the mutex via `spawn_blocking`; never fatal; not
/// shutdown-aware for the same reason the shredder is not: the next tick is
/// minutes or an hour away and the runtime drop ends it.
fn spawn_statistics_refresh(store: Arc<SqliteStore>) {
    tokio::spawn(async move {
        let mut early = STATISTICS_REFRESH_AFTER_SECS.into_iter();
        loop {
            let secs = early.next().unwrap_or(3600);
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
            let store = store.clone();
            match tokio::task::spawn_blocking(move || store.refresh_statistics()).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => eprintln!("squelchd: planner statistics refresh failed: {e}"),
                Err(_) => eprintln!("squelchd: planner statistics refresh panicked"),
            }
        }
    });
}

fn build_runtime() -> Result<tokio::runtime::Runtime, squelch_core::CoreError> {
    let parallelism = available_parallelism();
    let env_override = std::env::var(WORKER_THREADS_ENV).ok();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads_for(parallelism, env_override.as_deref()))
        .enable_all()
        .build()
        .map_err(|e| other_err(format!("tokio runtime: {e}")))?;
    // Said once at boot, by both long-lived modes, so a stall is diagnosed
    // against the number that applies rather than the cores the node claims.
    eprintln!(
        "squelchd: tokio runtime: {} worker thread(s), quota-aware parallelism {}{}",
        runtime.metrics().num_workers(),
        parallelism,
        env_override
            .as_deref()
            .map(|v| format!(", {WORKER_THREADS_ENV}={v}"))
            .unwrap_or_default()
    );
    Ok(runtime)
}

/// Load config plus the Stage-2 cap sources, which `serve` reports as "default"
/// vs "config" on `/client/triage-config`.
fn load_config(cli: &Cli) -> (Config, Stage2CapSources) {
    match &cli.config {
        Some(path) => Config::load_from_with_cap_sources(path),
        None => Config::load_with_cap_sources(),
    }
}

/// The READ-bound credential store for the sync engine, per configured backend.
/// Every sync engine `serve` builds for the mailbox is built here, so none can
/// exist without the mailbox's one Gmail budget. `SyncEngine::new` alone gives
/// an engine a PRIVATE budget, which is right for `squelchd run` (the only Gmail
/// caller in its process) and wrong here: the sync loop and the sweeps engine
/// each pacing alone asked Gmail for up to 160% of the user's limit (#263).
fn mailbox_engine(
    store: Arc<SqliteStore>,
    creds: Arc<dyn CredentialStore>,
    account_id: AccountId,
    email: String,
    config: Config,
    budget: &Arc<squelch_core::gmail_budget::GmailBudget>,
) -> SyncEngine<SqliteStore, dyn CredentialStore> {
    SyncEngine::new(store, creds, account_id, email, config).with_gmail_budget(budget.clone())
}

/// Build the semantic-recall embedder. `None` on failure — search then degrades
/// to keyword-only.
///
/// The concrete [`LazyEmbedder`] rather than `Arc<dyn Embedder>`, because the
/// reaper in [`spawn_embedder_reaper`] needs the unload method and the trait
/// deliberately does not carry it: unloading is a property of THIS embedder, not
/// of embedding. Attach sites coerce.
fn build_embedder(config: &Config, metrics: Option<Arc<SyncMetrics>>) -> Option<Arc<LazyEmbedder>> {
    let built = LazyEmbedder::new(&config.embed.settings()).map(|e| match metrics {
        Some(m) => e.with_metrics(m),
        None => e,
    });
    match built {
        Ok(e) => Some(Arc::new(e)),
        Err(e) => {
            eprintln!(
                "squelch: embedder unavailable ({e}); semantic recall disabled \
                 (keyword search + triage unaffected)"
            );
            None
        }
    }
}

/// How often the reaper looks. A minute is far below any sane idle window and
/// costs one uncontended `try_lock` when there is nothing to do, so there is no
/// reason to make it a knob.
///
/// It is also a FLOOR on `[embed] idle_unload_secs`: nothing is unloaded
/// between ticks, so the effective window is the configured one rounded up to
/// the next minute. The startup line below prints both numbers rather than
/// leaving an operator who set 5 wondering why it took 65.
const EMBEDDER_REAP_INTERVAL_SECS: u64 = 60;

/// How many consecutive contended ticks before the reaper says so. An embed
/// holding the lock is normal and happens all day; a whole hour of them is a
/// session no reaper can ever reach, which is the memory this feature exists to
/// give back sitting there anyway.
const EMBEDDER_CONTENDED_TICKS_BEFORE_WARNING: u32 = 60;

/// Drop the embedding session once it has gone `idle_secs` without a call. That
/// session is 85-90% of a tenant daemon's memory (~250-300 MB against ~25-40 MB
/// for everything else, SQLite included) and a mailbox is idle nearly all the
/// time, so a daemon that holds it forever is a pod that is mostly ONNX arenas
/// waiting for mail. Reloading costs ~200 ms off the cached weights, paid by
/// whichever search or poll tick lands first.
///
/// `idle_secs == 0` means never unload: no task is spawned at all, and the
/// daemon behaves exactly as it did before this existed.
fn spawn_embedder_reaper(
    embedder: Arc<LazyEmbedder>,
    idle_secs: u64,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    if idle_secs == 0 {
        return;
    }
    let max_idle = std::time::Duration::from_secs(idle_secs);
    let interval = std::time::Duration::from_secs(EMBEDDER_REAP_INTERVAL_SECS);
    // Once, at startup, with the EFFECTIVE numbers: the configured window and
    // the tick that rounds it up are two different values and an operator who
    // reads only the first will misread every unload line that follows.
    eprintln!(
        "squelchd: embedder reaper armed: unloads after {idle_secs} s idle, \
         checked every {EMBEDDER_REAP_INTERVAL_SECS} s (so the session goes at \
         {idle_secs}-{} s past the last use)",
        idle_secs + EMBEDDER_REAP_INTERVAL_SECS
    );
    tokio::spawn(async move {
        let mut contended_run: u32 = 0;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                // An Err is the SENDER being dropped, which is the daemon on its
                // way out. It has to end the task rather than fall through: a
                // dropped sender makes `changed()` return instantly forever, and
                // this loop would spin a core until the process died.
                res = shutdown.changed() => {
                    if res.is_err() {
                        return;
                    }
                }
            }
            if *shutdown.borrow() {
                return;
            }
            // Dropping an ONNX session and trimming glibc's arenas are both
            // millisecond-scale rather than instant, and this runs on the same
            // runtime as the doors.
            let e = embedder.clone();
            let outcome = match tokio::task::spawn_blocking(move || e.reap_if_idle(max_idle)).await
            {
                Ok(outcome) => outcome,
                Err(e) => {
                    // A panic inside the reap (a session whose Drop blew up, say)
                    // must not vanish into a Kept: it would repeat every tick
                    // with the gauge pinned at 1 and nothing in the log. Say so
                    // and leave the contention count alone.
                    eprintln!(
                        "squelchd: embedder reap task join error ({e}); trying again next tick"
                    );
                    continue;
                }
            };
            match outcome {
                ReapOutcome::Unloaded => {
                    contended_run = 0;
                    eprintln!(
                        "squelchd: embedder unloaded after {idle_secs} s idle; \
                         reloads on the next embed or search"
                    );
                }
                ReapOutcome::Kept => contended_run = 0,
                ReapOutcome::Contended => {
                    contended_run += 1;
                    // ONCE per run, not per tick: the whole point is a line an
                    // operator can find, and a line every minute forever is a
                    // line nobody reads. The task keeps going either way — a
                    // reaper that gave up would be the leak it was built to
                    // stop.
                    // Every N ticks rather than once: permanent contention is
                    // the case this exists for, and one line at minute 60 has
                    // rotated out of the log by the time anyone looks.
                    if contended_run.is_multiple_of(EMBEDDER_CONTENDED_TICKS_BEFORE_WARNING) {
                        eprintln!(
                            "squelchd: the embedder lock has been held on every \
                             reap for {} minutes; the session is not being \
                             unloaded and its memory is not coming back",
                            EMBEDDER_CONTENDED_TICKS_BEFORE_WARNING as u64
                                * EMBEDDER_REAP_INTERVAL_SECS
                                / 60
                        );
                    }
                }
            }
        }
    });
}

/// Mirror the loaded `.env` into config.toml so other binaries and non-repo CWDs
/// resolve the same account/paths. Env-only secrets (`SQUELCH_API_TOKEN`) are
/// never written. Best-effort: failure warns, never fatal.
fn mirror_env_to_config(env_path: &std::path::Path) {
    let Some(config_path) = Config::default_path() else {
        return;
    };
    let pairs: Vec<(String, String)> = match dotenvy::from_path_iter(env_path) {
        Ok(iter) => iter.flatten().collect(),
        Err(e) => {
            eprintln!("squelchd: could not re-read {}: {e}", env_path.display());
            return;
        }
    };
    match squelch_core::config::mirror_env_pairs_to_config(&pairs, &config_path) {
        Ok(true) => eprintln!(
            "squelchd: mirrored .env settings into {}",
            config_path.display()
        ),
        Ok(false) => {}
        Err(e) => eprintln!(
            "squelchd: could not mirror .env into {}: {e}",
            config_path.display()
        ),
    }
}

fn main() -> ExitCode {
    // Dev convenience: pick up a `.env` before config reads the environment.
    // Never overrides already-exported vars; prod boxes use systemd env instead.
    if let Ok(path) = dotenvy::dotenv() {
        eprintln!("squelchd: loaded env from {}", path.display());
        mirror_env_to_config(&path);
    }

    // Parsed through `ArgMatches` rather than `Cli::parse()` so `auth` can see
    // which of its flags were typed and which came out of the environment.
    let matches = Cli::command().get_matches();
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };
    let flag_sources = AuthFlagSources::from_matches(&matches);
    let (config, cap_sources) = load_config(&cli);

    let result = match &cli.command {
        Command::Auth(args) => cmd_auth(&config, args, flag_sources),
        Command::Run => run_daemon(config),
        Command::Serve(args) => cmd_serve(config, cap_sources, args),
        Command::Token(args) => cmd_token(&config, args),
        Command::Pair(args) => cmd_pair(&config, args),
        Command::Triage(args) => cmd_triage(&config, args),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Uppercase tag for the progress banners.
fn scope_word(scopes: AuthScopes) -> &'static str {
    match scopes {
        AuthScopes::Read => "READ",
        AuthScopes::Write => "WRITE",
        AuthScopes::Calendar => "CALENDAR",
    }
}

/// Which consents one `auth` invocation runs, in order. ONE Gmail consent
/// (`gmail.modify`), with or without `--write`: the only Gmail scope covers
/// reading and writing, so a second Gmail screen would ask for the same thing
/// again. Its token fills both Gmail slots ([`gmail_slots`]).
///
/// `--calendar` runs the Calendar consent ALONE. Calendar is an optional
/// grant on top of a working mailbox, so asking for it must never re-run (and
/// so never risk) the Gmail consent.
fn auth_plan(args: &AuthArgs) -> Vec<AuthScopes> {
    if args.calendar {
        vec![AuthScopes::Calendar]
    } else {
        vec![AuthScopes::Read]
    }
}

/// The slots one consent's token is stored in. The Gmail consent fills Read
/// AND Write with the one token: the slots stay separate so sync can only
/// ever load Read, which is what keeps the two doors apart (token scope never
/// did, since Google unions grants per project).
fn gmail_slots(scopes: AuthScopes) -> Vec<CredentialKind> {
    match scopes {
        AuthScopes::Read | AuthScopes::Write => vec![CredentialKind::Read, CredentialKind::Write],
        AuthScopes::Calendar => vec![CredentialKind::Calendar],
    }
}

/// Whether a Gmail run (plain `auth` or `--write`) should end by OFFERING
/// the optional calendar consent, so a new self-host is asked for calendar at
/// setup rather than on the Calendar page later. Offered only while no calendar credential is stored, so
/// a repair run does not ask again, and never on `--calendar`, which is that
/// consent already.
///
/// NOT ON `--export`: the blob is printed only after every consent, so an
/// abandoned optional screen would hold the Gmail tokens hostage for the full
/// consent wait, and a blob carrying a Calendar entry is refused whole by an
/// importer older than daemon 0.0.13. Export users add `--calendar`.
///
/// A third consent into the Calendar slot rather than more scopes on the
/// Read consent, so the Read consent screen stays Gmail-only. What keeps the
/// two doors apart is still which slot a code path loads (Google unions
/// grants per project, so a later token may carry more than it asked for).
fn wants_first_calendar(args: &AuthArgs, calendar_stored: bool) -> bool {
    !args.calendar && !args.export && !args.import && !calendar_stored
}

/// A yes to "connect calendar now?": empty (Enter) or anything starting
/// with y. Everything else, including a closed stdin, is a no.
fn says_yes(answer: &str) -> bool {
    let a = answer.trim().to_ascii_lowercase();
    a.is_empty() || a.starts_with('y')
}

/// Ask before the optional calendar consent, and only when a person is at a
/// terminal: a consent nobody opens waits out the full timeout, so with no
/// terminal, or on a no, it is skipped with the command that connects it
/// later.
fn confirm_first_calendar() -> bool {
    use std::io::{IsTerminal, Write};
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        eprintln!(
            "\nsquelchd: Google Calendar is optional and not connected; run \
             `squelchd auth --calendar` any time to connect it."
        );
        return false;
    }
    eprint!(
        "\nsquelchd: your mailbox is set up. Also connect Google Calendar now, so Passband can \
         put events from your mail beside your schedule? It is one more Google screen. [Y/n] "
    );
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    if stdin.read_line(&mut answer).unwrap_or(0) == 0 || !says_yes(&answer) {
        eprintln!("squelchd: skipped. Run `squelchd auth --calendar` any time to connect it.");
        return false;
    }
    true
}

/// Run the optional calendar consent. NEVER fatal: the mailbox is fully set
/// up by now, so an unticked box or a failed consent is a warning and a hint,
/// and the run still succeeds.
fn mint_first_calendar(run: impl FnOnce() -> Result<(), squelch_core::CoreError>) -> bool {
    match run() {
        Ok(()) => true,
        Err(e) => {
            eprintln!(
                "squelchd: calendar was not connected ({e}). Your mailbox is set up; run \
                 `squelchd auth --calendar` any time to connect it."
            );
            false
        }
    }
}

/// Run the OAuth consent flow(s) and persist tokens for the configured account.
fn cmd_auth(
    config: &Config,
    args: &AuthArgs,
    sources: AuthFlagSources,
) -> Result<(), squelch_core::CoreError> {
    let transport = resolve_transport(args, sources)?;
    if broker_url(args, sources).is_some() && !matches!(transport, Transport::Broker(_)) {
        eprintln!(
            "squelchd: SQUELCH_BROKER_URL is set, but this run asked for another transport on \
             the command line, so the broker is not being used."
        );
    }
    // Said once here, before the transport dispatch, so the export path is
    // covered too and says it exactly once.
    if typed_port_is_ignored(args, &transport, sources) {
        eprintln!(
            "squelchd: --port {} is ignored on this run; a fixed port is bound only by \
             --headless, and by --export --expose-consent-listener.",
            args.port
        );
    }

    let broker = match &transport {
        Transport::Export => return cmd_auth_export(config, args),
        Transport::Import => return cmd_auth_import(config),
        Transport::Broker(url) => Some(url.as_str()),
        Transport::Loopback => None,
    };

    let client = config.oauth_client()?;
    let email = config.require_account_email()?;
    let backend = config.credential_backend;
    // Before the browser opens: a backend this build cannot use is refused
    // here, not after the consent it would have had nowhere to put.
    ensure_backend(backend)?;
    let creds_path = config.resolve_credentials_path();

    let plan = auth_plan(args);
    let total = plan.len();
    for (i, scopes) in plan.iter().copied().enumerate() {
        let step = i + 1;
        eprintln!(
            "\nsquelchd: minting {} credential ({step}/{total})...",
            scope_word(scopes)
        );
        // Strictly sequential: each flow's loopback listener is dropped before
        // the next binds, so headless can reuse the one fixed port.
        if let Err(e) = mint_credential(&client, &email, backend, &creds_path, scopes, args, broker)
        {
            if step > 1 {
                eprintln!(
                    "squelchd: the credential(s) minted earlier in this run ARE stored; \
                     re-run `squelchd auth` to finish the {} credential.",
                    scope_word(scopes)
                );
            }
            return Err(e);
        }
    }
    // Read AFTER the Gmail consents, so a keyring prompt can never stand in
    // front of the screen that matters. Missing or unreadable both read as
    // "not stored": the worst case is one optional offer made again.
    let calendar_stored =
        load_token_backend(backend, &creds_path, &email, CredentialKind::Calendar).is_ok();
    if wants_first_calendar(args, calendar_stored) && confirm_first_calendar() {
        mint_first_calendar(|| {
            mint_credential(
                &client,
                &email,
                backend,
                &creds_path,
                AuthScopes::Calendar,
                args,
                broker,
            )
        });
    }
    Ok(())
}

/// One consent flow, persisted into that kind's own slot; scopes from different
/// kinds never share a slot.
fn mint_credential(
    client: &OAuthClientConfig,
    email: &str,
    backend: CredentialBackend,
    creds_path: &std::path::Path,
    scopes: AuthScopes,
    args: &AuthArgs,
    broker: Option<&str>,
) -> Result<(), squelch_core::CoreError> {
    println!(
        "Authorizing Google account: {email} [{}] via {:?} backend",
        scopes.label(),
        backend
    );

    let token = match broker {
        // The broker flow needs no listener, so no port and no tunnel; the
        // scope set, the slot it lands in, and the exchange are unchanged.
        // Both arms are handed `email`: the exchange refuses to return a token
        // for any other mailbox, so a consent finished on the wrong Google
        // account fails here instead of being stored under this one.
        Some(broker) => run_broker_flow(client, email, broker, scopes)?,
        None => {
            let opts = AuthFlowOptions {
                scopes,
                headless: args.headless,
                port: args.port,
            };
            run_auth_flow(client, email, &opts)?
        }
    };
    // Every slot checked before any is written, so a grant without
    // gmail.modify stores nothing rather than a Write slot that cannot write.
    let slots = gmail_slots(scopes);
    for kind in &slots {
        check_token_covers_slot(*kind, &token)?;
    }
    for kind in slots {
        store_and_announce(backend, creds_path, email, kind, &token)?;
    }
    if token.refresh_token.is_some() {
        println!("A refresh token was captured; squelch can renew access automatically.");
    } else {
        println!(
            "WARNING: no refresh token was returned. You may need to revoke prior access at \
             https://myaccount.google.com/permissions and re-run `squelchd auth`."
        );
    }
    Ok(())
}

/// Persist one token into its kind's slot and say where it landed, reading it
/// back first so "stored" means stored. Never prints the token material.
fn store_and_announce(
    backend: CredentialBackend,
    creds_path: &std::path::Path,
    email: &str,
    kind: CredentialKind,
    token: &squelch_core::credentials::StoredToken,
) -> Result<(), squelch_core::CoreError> {
    store_token_backend(backend, creds_path, email, kind, token)?;
    let _ = load_token_backend(backend, creds_path, email, kind)?;
    announce_stored(backend, creds_path, email, kind);
    Ok(())
}

/// Say where one credential landed. Split out because an import writes every
/// slot before it announces ANY of them: a line saying "stored" that a later
/// failure walks back is worse than no line at all.
fn announce_stored(
    backend: CredentialBackend,
    creds_path: &std::path::Path,
    email: &str,
    kind: CredentialKind,
) {
    match backend {
        CredentialBackend::Keyring => {
            println!(
                "\nStored {kind:?} credentials for {email} in the OS keyring (service \"squelch\")."
            );
        }
        CredentialBackend::File => {
            println!(
                "\nStored {kind:?} credentials for {email} in {} (mode 0600).",
                creds_path.display()
            );
        }
    }
}

/// Fail unless both consents in one export named the same mailbox.
///
/// A blob names ONE mailbox, and the importer stores every entry under it. Two
/// consents finished as different Google accounts would file a stranger's token
/// in this account's other slot.
///
/// Both values are whatever Google's userinfo named, and this message reaches a
/// terminal, so both are disarmed on the way in: raw, an `ESC` in one clears the
/// screen and a `BEL` retitles the window.
fn check_export_same_account(first: &str, mailbox: &str) -> Result<(), squelch_core::CoreError> {
    if first.trim().eq_ignore_ascii_case(mailbox.trim()) {
        return Ok(());
    }
    let (first, mailbox) = (printable(first), printable(mailbox));
    Err(squelch_core::CoreError::Credential(format!(
        "the first consent authorized {first} but this one authorized {mailbox}; a \
         credential blob carries one account, so nothing was exported. Re-run \
         `squelchd auth --export --write` and approve both screens as the same account."
    )))
}

/// `--export`: run consent on THIS machine and print a transfer blob, storing
/// nothing.
///
/// No `account_email` is REQUIRED: the exporting machine is whatever laptop has
/// a browser, not the daemon's host, and the mailbox is whatever Google names on
/// the consent screen. It is reported on stderr so the operator can see they
/// approved as the account they meant to. When one IS configured here (same
/// binary, same config.toml, possibly the same .env), the check is available and
/// runs: a consent finished under the wrong Google session would otherwise mint
/// and PRINT a live refresh token, and printing it is the part that cannot be
/// taken back.
fn cmd_auth_export(config: &Config, args: &AuthArgs) -> Result<(), squelch_core::CoreError> {
    let client = config.oauth_client()?;
    let expected_account = config
        .account_email
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty());
    if let Some(account) = expected_account {
        eprintln!(
            "squelchd: this machine is configured for {account}, so every consent must be \
             approved as that account."
        );
    }
    let bind = if args.expose_consent_listener {
        ConsentBind::AllInterfaces { port: args.port }
    } else {
        ConsentBind::Loopback
    };

    let plan = auth_plan(args);
    let total = plan.len();
    let mut account: Option<String> = None;
    let mut credentials: Vec<TransferCredential> = Vec::new();
    for (i, scopes) in plan.iter().copied().enumerate() {
        eprintln!(
            "\nsquelchd: authorizing {} credential ({}/{total})...",
            scope_word(scopes),
            i + 1
        );
        let (mailbox, token) = run_export_flow(&client, expected_account, scopes, bind)?;

        if let Some(first) = &account {
            check_export_same_account(first, &mailbox)?;
        }
        // A blob without a refresh token buys one hour and then fails in a way
        // nobody connects back to this paste, so it is refused where the fix is
        // still obvious rather than on the daemon.
        if token.refresh_token.is_none() {
            return Err(squelch_core::CoreError::Credential(format!(
                "Google returned no refresh token for the {} credential, so this blob would stop \
                 working within the hour and nothing was exported. Revoke squelch at \
                 https://myaccount.google.com/permissions and re-run `squelchd auth --export`.",
                scope_word(scopes)
            )));
        }
        account = Some(mailbox);
        let slots = gmail_slots(scopes);
        for kind in &slots {
            check_token_covers_slot(*kind, &token)?;
        }
        for kind in slots {
            credentials.push(TransferCredential {
                kind,
                token: token.clone(),
            });
        }
    }

    // Slots, not consents: the one Gmail consent exports as Read and Write.
    let total = credentials.len();
    let account = account.ok_or_else(|| other_err("no credential was authorized".to_string()))?;
    let blob = encode_transfer(&CredentialTransfer::new(account.clone(), credentials))?;

    match &args.out {
        // Mode 0600 from the moment it exists, which a shell redirect cannot do:
        // `> cred.txt` takes the ambient umask, and the file is a live token.
        Some(path) => {
            let mut bytes = blob.into_bytes();
            bytes.push(b'\n');
            write_private(path, &bytes)?;
            eprintln!(
                "\nsquelchd: wrote {} credential(s) for {account} to {} (mode 0600).",
                total,
                path.display()
            );
        }
        // The ONE thing on stdout, so a redirect is still a file that imports
        // exactly as pasted.
        None => {
            println!("{blob}");
            eprintln!("\nsquelchd: exported {total} credential(s) for {account}.");
        }
    }

    eprintln!(
        "squelchd: that line is a LIVE refresh token in plaintext. Import it, then delete it \
         from the file, the clipboard, and anywhere you pasted it."
    );
    eprintln!("squelchd: import it on the daemon's host with, for example:");
    eprintln!("    docker exec -i <container> squelchd auth --import < cred.txt");
    eprintln!(
        "squelchd: the importing host must use the SAME SQUELCH_CLIENT_ID and \
         SQUELCH_CLIENT_SECRET, since a refresh token only works for the client that minted it."
    );
    Ok(())
}

/// `--import`: store a blob minted by `--export` on another machine.
///
/// A blob is unsigned JSON, so nothing it SAYS about itself is evidence: the
/// `account` field is the paster's claim, and so is the `kind` that decides
/// which slot each token lands in. Every entry is therefore taken to Google
/// before ANY of them is written, which is the same question the exchange asks
/// on every other path to a stored token.
fn cmd_auth_import(config: &Config) -> Result<(), squelch_core::CoreError> {
    let client = config.oauth_client()?;
    let email = config.require_account_email()?;
    let backend = config.credential_backend;
    // Before any credential is verified with Google: the same refusal as
    // `auth`, for the same reason.
    ensure_backend(backend)?;
    let creds_path = config.resolve_credentials_path();

    let transfer = decode_transfer(&read_transfer_blob()?)?;
    // The cheap local checks first: no network is spent on a blob that could not
    // land whatever Google says about it.
    check_transfer_account(&email, &transfer.account)?;
    check_transfer_usable(&transfer)?;

    println!(
        "Checking {} credential(s) with Google before anything is stored...",
        transfer.credentials.len()
    );
    // What gets STORED is what verification minted, not what was pasted. The
    // refresh it already ran is the only part of a blob Google ever named, so
    // keeping the blob's access token and expiry alongside it would persist two
    // attacker-authored fields; an absent `expires_at` in particular reads as
    // "never expires" and would pin a bearer string of the paster's choosing on
    // every Gmail call.
    let mut entries: Vec<(CredentialKind, squelch_core::credentials::StoredToken)> = Vec::new();
    for cred in &transfer.credentials {
        let verified = verify_transfer_credential(&client, &email, cred.kind, &cred.token)?;
        println!(
            "  {:?}: Google confirms this token opens {email} with the scopes the {:?} slot needs.",
            cred.kind, cred.kind
        );
        entries.push((cred.kind, verified));
    }

    // One unit: either both slots come from this import or neither does, so the
    // two doors can never end up describing different consents.
    store_tokens_backend(backend, &creds_path, &email, &entries)?;
    for (kind, _) in &entries {
        let _ = load_token_backend(backend, &creds_path, &email, *kind)?;
    }
    for (kind, _) in &entries {
        announce_stored(backend, &creds_path, &email, *kind);
    }
    println!("\nsquelch can renew access automatically; the exported blob is no longer needed.");
    Ok(())
}

/// Read the blob from STDIN.
///
/// Never argv: the blob is a live refresh token, and argv is world-readable in
/// `ps` and lands in shell history.
///
/// A TTY gets ONE LINE, because that is what a paste is and because reading to
/// EOF there would need a Ctrl-D nobody is expecting: the prompt would sit there
/// looking hung after the enter. A pipe still gets read to EOF, which is what
/// `< cred.txt` delivers. Both are capped.
fn read_transfer_blob() -> Result<String, squelch_core::CoreError> {
    use std::io::IsTerminal;

    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return read_blob_capped(stdin.lock(), BlobRead::ToEndOfInput);
    }

    eprintln!("squelchd: paste the single line from `squelchd auth --export` and press enter.");
    eprintln!(
        "squelchd: the paste is not echoed. Piping the file in is still safer, since a terminal \
         keeps its own copy of anything it does echo: squelchd auth --import < cred.txt"
    );
    // Echo off for the same reason a password prompt does it, restored when the
    // guard drops.
    let echo = TerminalEcho::suppressed();
    let blob = read_blob_capped(stdin.lock(), BlobRead::OneLine);
    drop(echo);
    // The enter that ended the line was not echoed either.
    eprintln!();
    blob
}

/// How far [`read_blob_capped`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlobRead {
    /// A paste at a prompt, which ends at the enter that follows it.
    OneLine,
    /// A pipe or a redirect, which ends at EOF.
    ToEndOfInput,
}

/// The read itself, split from the terminal handling so both shapes are
/// testable. Capped either way, because a redirected file or a wedged pipe is
/// not a blob.
fn read_blob_capped(
    reader: impl std::io::BufRead,
    how: BlobRead,
) -> Result<String, squelch_core::CoreError> {
    use std::io::{BufRead, Read};

    let mut buf = String::new();
    // One byte past the ceiling: reading it is how an overrun shows up.
    let mut capped = reader.take(MAX_BLOB_BYTES as u64 + 1);
    match how {
        BlobRead::OneLine => capped.read_line(&mut buf),
        BlobRead::ToEndOfInput => capped.read_to_string(&mut buf),
    }
    .map_err(|e| other_err(format!("reading the credential blob from stdin: {e}")))?;

    if buf.len() > MAX_BLOB_BYTES {
        return Err(other_err(format!(
            "more than {MAX_BLOB_BYTES} bytes arrived on stdin, which no credential blob is; \
             nothing was stored"
        )));
    }
    let blob = buf.trim().to_string();
    if blob.is_empty() {
        return Err(other_err(
            "nothing arrived on stdin, so there was no credential blob to import. Pipe one in: \
             `squelchd auth --import < cred.txt`."
                .to_string(),
        ));
    }
    Ok(blob)
}

/// Terminal echo, off for as long as this is alive and back on when it drops.
///
/// The line pasted at the import prompt is a live refresh token. A terminal that
/// echoes it leaves a copy in the scrollback of whatever window it was pasted
/// into, which outlives the paste by however long that window does.
///
/// Restoring is best effort by construction: a run killed by a signal never
/// reaches the drop, and the fix for that terminal is `stty sane`. Failing to
/// suppress at all is not fatal either, because refusing the import over it
/// would be worse than the echo.
struct TerminalEcho {
    #[cfg(unix)]
    restore: Option<libc::termios>,
}

impl TerminalEcho {
    #[cfg(unix)]
    fn suppressed() -> Self {
        // SAFETY: both calls take a pointer to a termios this frame owns, and
        // neither retains it. A non-tty stdin fails tcgetattr, which is handled.
        unsafe {
            let mut term: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut term) != 0 {
                return Self { restore: None };
            }
            let restore = term;
            term.c_lflag &= !libc::ECHO;
            // FLUSH so anything typed ahead of the prompt is dropped rather than
            // echoed, and on restore so a stray second line does not fall
            // through to the shell.
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &term) != 0 {
                return Self { restore: None };
            }
            Self {
                restore: Some(restore),
            }
        }
    }

    #[cfg(not(unix))]
    fn suppressed() -> Self {
        Self {}
    }
}

impl Drop for TerminalEcho {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(term) = self.restore.as_ref() {
            // SAFETY: `term` is the settings read in `suppressed`, unchanged.
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, term);
            }
        }
    }
}

/// Fail unless the blob was minted for the mailbox this daemon is configured
/// for.
///
/// Same rule as the consent-time check in squelch-core, for the same reason:
/// storing a token under the wrong address makes the sync loop ingest a
/// stranger's mail and serve it as this account's own. Trimmed and
/// case-insensitive, because Gmail addresses are ASCII and both values are
/// hand-typed somewhere.
///
/// This is a CHEAP PRE-FILTER, not the guarantee. The blob's `account` is a
/// string whoever pasted it chose; only [`verify_transfer_credential`] can say
/// what a token actually opens. It runs first because it costs nothing and
/// catches the honest mistake (the wrong file, the wrong account) before any
/// network call.
///
/// The blob's value is attacker-chosen and this message reaches a terminal, so
/// it is disarmed on the way in: raw, an `ESC` in it clears the screen and a
/// `BEL` retitles the window.
fn check_transfer_account(configured: &str, blob: &str) -> Result<(), squelch_core::CoreError> {
    if configured.trim().eq_ignore_ascii_case(blob.trim()) {
        return Ok(());
    }
    let blob = printable(blob);
    Err(squelch_core::CoreError::Credential(format!(
        "this blob was exported for {blob}, but this daemon is configured for {configured}; \
         nothing was stored. Export again while signed in as {configured}, or fix account_email \
         in the config."
    )))
}

/// Fail unless every entry carries a refresh token.
///
/// An access token alone works for about an hour and then fails on a refresh
/// nobody will connect back to a paste from yesterday, so it is refused now.
fn check_transfer_usable(transfer: &CredentialTransfer) -> Result<(), squelch_core::CoreError> {
    for cred in &transfer.credentials {
        if cred.token.refresh_token.is_none() {
            return Err(squelch_core::CoreError::Credential(format!(
                "the {:?} credential in this blob has no refresh token, so it would stop working \
                 within the hour; nothing was stored. Re-run `squelchd auth --export` on the \
                 machine with the browser.",
                cred.kind
            )));
        }
    }
    Ok(())
}

// ---- per-device human-door tokens -----------------------------------------
//
// `token` and `pair` talk to the STORE directly, exactly as `auth` talks to the
// credential backend directly: they are operator commands run at a shell, and
// requiring a running daemon to mint the credential that lets you reach the
// daemon would be a circle. These are the store's FIRST second-process writers,
// which is why `SqliteStore::set_pragmas` puts every connection in WAL with a
// busy timeout: a live `squelchd serve` on the same file is then not a conflict,
// and each command holds the write lock for one transaction.
//
// NOTHING HERE PRINTS SECRET MATERIAL EXCEPT THE ONE LINE THAT IS SUPPOSED TO.
// `token issue` prints its plaintext once, `pair` prints its code once, and
// `token list` prints neither, not even the stored hashes.

/// `triage status` and `triage grant`: the store directly, like `token`, so
/// they work in a tenant pod beside the running daemon.
fn cmd_triage(config: &Config, args: &TriageArgs) -> Result<(), squelch_core::CoreError> {
    use squelch_core::triage::agent_config::{
        ACCESS_BUDGET_KEY, BACKGROUND_BUDGET_KEY, TOTAL_BUDGET_KEY,
    };
    let (store, account_id) = open_account_store(config)?;
    let agent = &config.triage.agent;
    if let TriageCommand::Grant {
        runs,
        background,
        access,
    } = args.command
    {
        let (key, lane) = if background {
            (BACKGROUND_BUDGET_KEY, "background")
        } else if access {
            (ACCESS_BUDGET_KEY, "access")
        } else {
            (TOTAL_BUDGET_KEY, "total")
        };
        let released = store.grant_daily_runs(account_id, key, runs, agent, chrono::Utc::now())?;
        println!(
            "granted {runs} extra {lane} runs until midnight UTC; released {released} held jobs"
        );
    }
    let status = store.daily_budget_status(account_id, agent, chrono::Utc::now())?;
    println!(
        "total       {} of {} runs today",
        status.total_used, status.total_cap
    );
    println!(
        "background  {} of {} runs today",
        status.background_used, status.background_cap
    );
    println!(
        "access      {} of {} runs today",
        status.access_used, status.access_cap
    );
    println!(
        "held until tomorrow: {} new mail, {} background, {} access, {} revisits",
        status.held_arrival, status.held_background, status.held_access, status.held_revisit
    );
    if status.total_used >= status.total_cap
        && (status.held_background > 0
            || status.held_access > 0
            || matches!(
                args.command,
                TriageCommand::Grant {
                    background: true,
                    ..
                } | TriageCommand::Grant { access: true, .. }
            ))
    {
        println!(
            "note: the total is spent, so no lane grant (--background, --access) helps until it has room: `squelchd triage grant <runs>`"
        );
    }
    if status.held_revisit > 0 {
        println!(
            "note: revisits also answer to [revisit].daily_cap, which a grant does not raise; they run tomorrow"
        );
    }
    Ok(())
}

/// The URL scheme Passband registers for pairing deep links.
const PAIR_LINK_SCHEME: &str = "passband://pair";

/// Open the store and resolve the configured account, without a daemon.
fn open_account_store(
    config: &Config,
) -> Result<(SqliteStore, AccountId), squelch_core::CoreError> {
    let email = config.require_account_email()?;
    let store = SqliteStore::open(&config.db_path)?;
    let account_id = store.ensure_account(&email)?;
    Ok((store, account_id))
}

/// Whether the human door has a master token configured. The VALUE is never
/// read out; only its presence, which is the difference between an empty token
/// list meaning "nothing can get in" and meaning "only the master key can".
fn master_token_configured() -> bool {
    std::env::var("SQUELCH_API_TOKEN").is_ok_and(|t| !t.trim().is_empty())
}

/// An RFC3339 stamp in UTC, seconds resolution: precise enough to correlate with
/// a log line, short enough to sit in a column.
fn stamp(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn cmd_token(config: &Config, args: &TokenArgs) -> Result<(), squelch_core::CoreError> {
    let (store, account_id) = open_account_store(config)?;
    match &args.command {
        TokenCommand::Issue { name } => {
            let issued = store.issue_device_token(account_id, name)?;
            eprintln!(
                "squelchd: issued device token {} for {:?}.",
                issued.id, issued.name
            );
            // THE PLAINTEXT, ALONE ON STDOUT, so `squelchd token issue --name x
            // > token.txt` is a file containing a token and nothing else.
            // Everything a human reads goes to stderr, like `auth --export`.
            println!("{}", issued.token);
            eprintln!(
                "squelchd: that is the ONLY time this token is shown. squelchd keeps a hash of it \
                 and cannot print it again; a lost token is re-issued, never recovered. Revoke \
                 this one with `squelchd token revoke {}`.",
                issued.id
            );
            Ok(())
        }
        TokenCommand::List => {
            let tokens = store.list_device_tokens(account_id)?;
            if tokens.is_empty() {
                println!("No device tokens have been issued.");
                if master_token_configured() {
                    eprintln!(
                        "squelchd: the human door accepts SQUELCH_API_TOKEN only. \
                         Run `squelchd pair` to give a device its own revocable token."
                    );
                } else {
                    eprintln!(
                        "squelchd: SQUELCH_API_TOKEN is unset too, so the human door currently \
                         accepts nothing. Run `squelchd pair`, or \
                         `squelchd token issue --name <name>`."
                    );
                }
                return Ok(());
            }
            // Width from the data so a long device name does not wrap the
            // columns after it; the store caps names, so this is bounded.
            let width = tokens
                .iter()
                .map(|t| t.name.chars().count())
                .max()
                .unwrap_or(4)
                .max(4);
            println!(
                "{:>5}  {:<width$}  {:<20}  {:<20}  STATUS",
                "ID", "NAME", "CREATED", "LAST USED"
            );
            for t in &tokens {
                println!(
                    "{:>5}  {:<width$}  {:<20}  {:<20}  {}",
                    t.id,
                    t.name,
                    stamp(t.created_at),
                    t.last_used_at
                        .map(stamp)
                        .unwrap_or_else(|| "never".to_string()),
                    match t.revoked_at {
                        // Revoked rows are LISTED, not hidden: a revocation the
                        // operator cannot see afterwards is not much of a control.
                        Some(at) => format!("revoked {}", stamp(at)),
                        None => "active".to_string(),
                    }
                );
            }
            Ok(())
        }
        TokenCommand::Revoke { id } => {
            if store.revoke_device_token(account_id, *id)? {
                println!("Revoked device token {id}.");
                eprintln!(
                    "squelchd: it stops working on the very next request; the door reads the row \
                     every time and caches nothing."
                );
            } else {
                // Unknown and already-revoked are one answer, which is also what
                // the store returns: revoking twice is not a failure.
                println!("No active device token {id} for this account.");
            }
            Ok(())
        }
        TokenCommand::FirstPaired => {
            // EXACTLY ONE LINE ON STDOUT, and nothing else on it, ever — the
            // `token issue` discipline for the same reason: something machine
            // is reading this. Here that reader is `squelch-warden`, which
            // execs the command in a tenant pod and parses the first non-empty
            // line; a second line, a header, or a friendly note would be a
            // parse failure on a route the tenant cannot see or fix.
            //
            // Seconds precision and a `Z` offset, not the store's raw column:
            // one spelling crosses the wire, so the control plane never has to
            // decide whether two renderings of the same instant are the same
            // stamp. Nanoseconds would say when a device was minted to a
            // resolution nobody asked for.
            //
            // `none` rather than an empty line, because an empty line is what a
            // broken exec also produces.
            let first = store.first_client_pairing_at(account_id)?;
            println!(
                "{}",
                first
                    .map(|at| at.to_rfc3339_opts(SecondsFormat::Secs, true))
                    .unwrap_or_else(|| "none".to_string())
            );
            Ok(())
        }
    }
}

/// Percent-encode a value for a query string. Everything outside the unreserved
/// set goes, so a base URL's `://`, its slashes, and any `&` in it land in the
/// deep link as DATA rather than as structure the parser on the other side would
/// read as the end of the parameter.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The base URL the pairing link names, trailing slashes trimmed.
///
/// Refused unless it is http(s): the link tells a device where to POST a live
/// pairing code, and a typo that produced a `file:` or scheme-less value would
/// be dropped by the app with nothing to explain it. Better to fail here, where
/// the operator can see what they typed.
///
/// Without `--url` the link is built from `bind`, which the caller resolves the
/// SAME way `serve` does. Naming the compiled-in default instead would hand an
/// operator on a non-default port a link to a port nothing listens on, and the
/// device would fail with nothing to explain it. A WILDCARD bind keeps its port
/// and prints as loopback: `0.0.0.0` is not an address anything can POST to, and
/// loopback is right whenever the device being paired is this machine.
fn pair_base_url(args: &PairArgs, bind: SocketAddr) -> Result<String, squelch_core::CoreError> {
    let raw = match args.url.as_deref().map(str::trim) {
        Some(u) if !u.is_empty() => u,
        _ => {
            let addr = if bind.ip().is_unspecified() {
                let host = match bind.ip() {
                    IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
                    IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
                };
                SocketAddr::new(host, bind.port())
            } else {
                bind
            };
            // `SocketAddr`'s Display brackets an IPv6 literal, which is what a
            // URL authority needs.
            return Ok(format!("http://{addr}"));
        }
    };
    let base = raw.trim_end_matches('/');
    let lower = base.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(other_err(format!(
            "--url must be an http(s) address the device can reach, for example \
             http://{DEFAULT_BIND_ADDR}; got `{raw}`"
        )));
    }
    Ok(base.to_string())
}

/// Whether a base URL would carry the code and the token it buys over the
/// network in the clear: plain http to somewhere that is not this machine.
///
/// Loopback over http is fine — nothing leaves the box — so only the off-box
/// case is worth shouting about.
fn is_cleartext_offbox(base: &str) -> bool {
    let lower = base.to_ascii_lowercase();
    // https is safe, and anything else was already refused by `pair_base_url`.
    let Some(rest) = lower.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // An IPv6 literal is bracketed, so the port has to come off after the
    // closing bracket rather than at the first colon.
    let host = match authority.split_once(']') {
        Some((bracketed, _)) => bracketed.trim_start_matches('['),
        None => authority.split(':').next().unwrap_or_default(),
    };
    let loopback = host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    !loopback
}

fn cmd_pair(config: &Config, args: &PairArgs) -> Result<(), squelch_core::CoreError> {
    // Before the store: a bad --url (or a bad SQUELCH_BIND behind the default)
    // must not spend a code the operator then has to wait out or supersede.
    let base = pair_base_url(args, resolve_bind(&ServeArgs { bind: None })?)?;
    let (store, account_id) = open_account_store(config)?;

    let minted = store.mint_pairing_code(account_id, Duration::seconds(PAIRING_TTL_SECS))?;

    println!("Pairing code: {}", minted.code);
    println!(
        "Valid for {} minutes, until {}.",
        PAIRING_TTL_SECS / 60,
        stamp(minted.expires_at)
    );
    println!();
    println!("On the device you are pairing, open this link:");
    println!();
    println!(
        "  {PAIR_LINK_SCHEME}?url={}&code={}",
        percent_encode(&base),
        percent_encode(&minted.code)
    );
    println!();
    println!("Or type the code into Passband, pointed at {base}");

    if is_cleartext_offbox(&base) {
        eprintln!(
            "\nsquelchd: WARNING: {base} is plain http to another machine. The pairing code and \
             the device token it mints would cross the network in the clear, readable by anything \
             on the path, which is enough for a stranger to pair themselves. Put https in front \
             (`tailscale serve` is the short version) and pass that address as --url."
        );
    }

    eprintln!(
        "\nsquelchd: that code is a credential. It is good for one device, it is spent the first \
         time it is claimed, and a handful of wrong guesses burns it. Minting another supersedes \
         this one."
    );
    if !master_token_configured() {
        eprintln!(
            "squelchd: SQUELCH_API_TOKEN is unset, so pairing is currently the only way into the \
             human door on this daemon."
        );
    }
    Ok(())
}

/// Ctrl-C, or the SIGTERM a container runtime stops us with.
///
/// SIGTERM is not optional in the image: squelchd is PID 1 there
/// (deploy/DOCKER.md), and PID 1 has no default disposition for a signal it
/// does not handle, so without this every hosted roll would wait out the
/// grace period for a SIGKILL and skip the graceful path entirely (sync's
/// flush, the scheduled-send pass settling its row).
async fn stop_requested() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            // Losing the handler costs a harder stop, never a mail: a send
            // cut off mid-flight is answered by lease recovery.
            Err(_) => {
                eprintln!("squelchd: no SIGTERM handler; stopping on Ctrl-C only");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Sync-only loop with graceful Ctrl-C shutdown.
fn run_daemon(config: Config) -> Result<(), squelch_core::CoreError> {
    // Fail fast on config problems before spinning up the runtime. The store
    // constructor refuses a backend this build cannot use too; asking here
    // keeps the refusal ahead of opening the store and building the embedder.
    let email = config.require_account_email()?;
    let client = config.oauth_client()?;
    ensure_backend(config.credential_backend)?;

    let mut store = SqliteStore::open(&config.db_path)?;
    let account_id = store.ensure_account(&email)?;

    // Attach the embedder to both the store (query-side) and the engine
    // (write-side); `None` keeps everything working without vector recall.
    // No metrics registry: this mode serves no scrape endpoint, so the
    // loaded/unloaded gauge would have no reader.
    let embedder = build_embedder(&config, None);
    if let Some(e) = &embedder {
        store = store.with_embedder(e.clone() as Arc<dyn Embedder>)?;
    }
    let store = Arc::new(store);
    // Read off config before the engine takes ownership of it below.
    let idle_unload_secs = config.embed.idle_unload_secs;

    let creds = store_for_backend(
        config.credential_backend,
        account_id,
        email.clone(),
        CredentialKind::Read,
        config.resolve_credentials_path(),
        client,
    )?;

    let runtime = build_runtime()?;
    runtime.block_on(async move {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("\nsquelch: shutdown requested; finishing in-flight work...");
                let _ = shutdown_tx.send(true);
            }
        });

        spawn_statistics_refresh(store.clone());
        // No history retention pass: `SqliteStore::prune_history` exists and
        // is tested, but nothing schedules it. Run diagnostics and fast-lane
        // assessments are kept until it is decided what the triage eval and
        // cost work need from them (see `store/sqlite/retention.rs`).

        let mut engine = SyncEngine::new(store, creds, account_id, email, config);
        if let Some(e) = embedder {
            spawn_embedder_reaper(e.clone(), idle_unload_secs, shutdown_rx.clone());
            engine = engine.with_embedder(e as Arc<dyn Embedder>);
        }
        engine.run(shutdown_rx).await
    })?;

    eprintln!("squelch: sync stopped.");
    Ok(())
}

/// Resolve the `serve` bind address: `--bind` > `SQUELCH_BIND` > loopback
/// default. Parsed eagerly so a bad value fails before we open anything.
fn resolve_bind(args: &ServeArgs) -> Result<SocketAddr, squelch_core::CoreError> {
    let raw = args
        .bind
        .clone()
        .or_else(|| {
            std::env::var("SQUELCH_BIND")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| DEFAULT_BIND_ADDR.to_string());
    raw.parse()
        .map_err(|e| other_err(format!("invalid bind address `{raw}`: {e}")))
}

/// Say, at startup, whether outbound mail can be tracked and how opens are
/// expected to get back — the two switches are independent and BOTH failure
/// modes are silent.
///
/// `[tracking] base_url` decides where the pixel POINTS, and so which door the
/// recipient's mail client knocks on. `[pusher] relay_url` decides whether this
/// daemon DRAINS a relay. Point the pixel at a relay while leaving `relay_url`
/// unset and every layer still reports success: the pixel serves, the relay
/// buffers the row, and this daemon simply never asks for it. From Passband
/// that is indistinguishable from nobody ever opening the mail.
fn report_tracking_posture(config: &Config) {
    let configured = config
        .tracking
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty());
    let Some(base) = configured else {
        eprintln!(
            "squelchd: read tracking disabled (no SQUELCH_TRACK_URL / [tracking] base_url); sends go out untracked"
        );
        return;
    };

    // The human door drops a scheme-less base URL, because it would ride into
    // outbound HTML as a RELATIVE url and resolve against the recipient's mail
    // client. Silent there; loud here. The shared predicate, so this report,
    // minting and the opens-poller gate cannot disagree about what counts.
    if !squelch_core::tracking::is_mintable_base_url(base) {
        eprintln!(
            "squelchd: read tracking DISABLED: base_url `{base}` has no http(s) scheme, so it was rejected; sends go out untracked"
        );
        return;
    }

    eprintln!("squelchd: read tracking enabled, pixel at {base}/t/{{token}}");
    match config
        .pusher
        .relay_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        None => eprintln!(
            "squelchd:   no relay configured, so opens are recorded ONLY if that URL reaches THIS daemon's /t/ route. \
             If it points at a relay, opens will buffer there and never be collected."
        ),
        // Every hosted tenant: its own vhost serves the pixel, and the relay
        // is held for iOS push alone.
        Some(relay) if !squelch_core::tracking::pixel_lands_on_relay(base, relay) => eprintln!(
            "squelchd:   the pixel is not on the configured relay, so opens are recorded ONLY if that URL reaches THIS daemon's /t/ route; \
             the relay is used for push alone and its opens buffer is never drained from here."
        ),
        Some(_) => {}
    }
}

/// Resolve `[metrics] bind` (env `SQUELCH_METRICS_BIND`) into a socket address.
/// `None` means the config never asked for a metrics listener. A value that is
/// present but unparseable is FATAL, and eagerly so, for the same reason as
/// [`resolve_bind`]: an operator who asked for a scrape endpoint and silently
/// got none has monitoring that reports nothing and looks fine doing it.
fn resolve_metrics_bind(config: &Config) -> Result<Option<SocketAddr>, squelch_core::CoreError> {
    let Some(raw) = config
        .metrics
        .bind
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    raw.parse()
        .map(Some)
        .map_err(|e| other_err(format!("invalid metrics bind address `{raw}`: {e}")))
}

/// Say, at startup, whether anything can scrape or probe this daemon.
///
/// Both routes on that listener are UNAUTHENTICATED — neither a scraper nor a
/// kubelet has a credential to offer — so its bind address is the whole access
/// control, which is why this prints rather than hides. `/metrics` exposes
/// counts and sizes only, with no sender, subject or message id ever becoming a
/// label, and `/healthz` exposes one word out of two.
fn report_metrics_posture(config: &Config) {
    match config
        .metrics
        .bind
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(addr) => eprintln!(
            "squelchd: metrics and readiness enabled at http://{addr}/metrics and /healthz (no auth; bind it where only your scraper reaches it)"
        ),
        None => eprintln!(
            "squelchd: metrics disabled (set SQUELCH_METRICS_BIND / [metrics] bind, e.g. 127.0.0.1:9848, to serve GET /metrics and GET /healthz)"
        ),
    }
}

/// Whether the daemon has finished starting, as flags the startup path flips.
///
/// The doors bind BEFORE the daemon is up, deliberately (see [`cmd_serve`]), so
/// "the socket accepts" and "this mailbox is serving" are different questions,
/// and only the second one is worth an orchestrator's attention. Both flags
/// start false, which is what makes a startup that FAILS after the bind report
/// not-ready: a daemon that died on the way up has flipped nothing.
///
/// Two conditions, and each rules out a state a bare TCP accept calls healthy:
///
/// - **The sync engine's task is alive.** It is spawned before the doors bind
///   and it is what makes a mailbox a mailbox. If that task ends or panics,
///   both doors go on accepting connections in front of a store nothing is
///   filling, and every probe that only opens a socket passes forever.
/// - **The background embedder init has settled.** SETTLED, not succeeded: a
///   first-run model download is the long pole of a cold start and is precisely
///   what a probe must wait out, but an embedder that can NEVER build (no
///   egress to the model host, a corrupt cache) leaves a daemon that syncs,
///   serves both doors and searches by keyword. Refusing to call that ready
///   would take a working mailbox down over a degraded search index.
///
/// ## What this does NOT catch, and why it is left that way
///
/// A credential Google has stopped accepting does not make a daemon unready.
/// `SyncEngine::run` retries with backoff on every error and returns only when
/// shutdown is signalled, so a rejected token, an expired grant and a network
/// that is down all leave the task alive and this flag set. The first flag
/// therefore means "the sync task has not died", not "sync is working".
///
/// That is the deliberate answer rather than a gap to close later, because
/// readiness is what a Service routes on. A mailbox whose OAuth grant expired
/// still holds every message it has ever synced, still serves both doors, and
/// is reached through exactly one route — which is also where its owner has to
/// go to re-consent. Answering 503 would pull that pod out of its own Service
/// and take the door away at the moment it is needed, and it would stop the
/// hosted fleet roller dead — that pass reads a tenant carrying today's render
/// and not serving it as a casualty, and refuses to roll anything else at all.
/// A dead credential is a thing to ALERT on, and the metrics door next to this
/// one is where that signal belongs.
///
/// The store is deliberately not a flag. `SqliteStore::open` and its migrations
/// run before the runtime exists and a failure exits the process, so there is
/// no state in which anything is listening and the store is not open.
#[derive(Clone, Default)]
struct Readiness {
    flags: Arc<ReadinessFlags>,
}

struct ReadinessFlags {
    sync_running: AtomicBool,
    /// A watch sender rather than an `AtomicBool` because two things now read
    /// this bit: `/healthz`, and the sync engine, whose first backfill parks
    /// until the embedder init has resolved (see
    /// `SyncEngine::with_embedder_gate`). ONE bit for both, so settling the
    /// probe always releases the backfill, and there is no second flag to forget
    /// in a new arm of the init task. The reverse does not hold: the backfill
    /// also releases ITSELF at `EMBEDDER_GATE_CEILING` (three minutes) with the
    /// probe still 503, and that is the same 180 s as the warden's default ready
    /// timeout, so a wedged init fails the signup at about the instant the
    /// backfill starts without it. The ceiling's log line describes the
    /// daemon, not the tenant's fate.
    embedder_settled: tokio::sync::watch::Sender<bool>,
}

impl Default for ReadinessFlags {
    fn default() -> Self {
        Self {
            sync_running: AtomicBool::new(false),
            embedder_settled: tokio::sync::watch::channel(false).0,
        }
    }
}

/// Clears the sync flag when the sync task ends, HOWEVER it ends: a returned
/// error, a panic and an abort all drop this, and all three leave a daemon that
/// is no longer syncing.
struct SyncRunning(Arc<ReadinessFlags>);

impl Drop for SyncRunning {
    fn drop(&mut self) {
        self.0.sync_running.store(false, Ordering::Relaxed);
    }
}

impl Readiness {
    /// Mark the sync engine running, and hand back the guard that unmarks it.
    ///
    /// Called BEFORE the task is spawned, with the guard moved into it. Setting
    /// the flag from inside the task instead would race a sync loop that fails
    /// on its first tick: its guard would drop before the parent ever set the
    /// flag, and a dead engine would be reported as a running one for the life
    /// of the process.
    fn sync_started(&self) -> SyncRunning {
        self.flags.sync_running.store(true, Ordering::Relaxed);
        SyncRunning(self.flags.clone())
    }

    /// Mark the background embedder init resolved, whichever way it resolved.
    /// Every gate handed out by [`Readiness::embedder_gate`] opens in the same
    /// motion, because it is the same bit.
    fn embedder_settled(&self) {
        // `send_replace`, not `send`: the latter is an error when no receiver is
        // alive, and a daemon whose sync task has already died still has a probe
        // to answer.
        self.flags.embedder_settled.send_replace(true);
    }

    /// A receiver on the embedder-settled bit, for the sync engine's first
    /// backfill to wait on. Already true if the init resolved before the caller
    /// asked, so a late subscriber never waits for something that has happened.
    fn embedder_gate(&self) -> tokio::sync::watch::Receiver<bool> {
        self.flags.embedder_settled.subscribe()
    }

    /// Both conditions, and nothing derived from either: the caller gets one
    /// bit because one bit is all `/healthz` may say.
    fn is_up(&self) -> bool {
        self.flags.sync_running.load(Ordering::Relaxed) && *self.flags.embedder_settled.borrow()
    }
}

/// Everything the metrics door reads. Shared with the daemon proper; the only
/// thing it owns is its concurrency gate.
#[derive(Clone)]
struct MetricsState {
    metrics: Arc<squelch_core::metrics::SyncMetrics>,
    store: Arc<SqliteStore>,
    account_id: AccountId,
    config: Arc<Config>,
    /// ONE store read at a time, and the permit is held across the whole read.
    /// This route carries no credential and every store call queues on the
    /// daemon-wide mutex that both doors and the sync loop wait on, so a scrape
    /// flood is capped here rather than allowed to serialize the daemon — the
    /// same reasoning as `PAIR_CONCURRENCY`, one slot tighter because a scrape
    /// is a periodic housekeeping read, not a user waiting on a screen.
    gate: Arc<tokio::sync::Semaphore>,
}

/// The db-derived half of a scrape, or `None` if the store could not be read.
async fn gather_store_metrics(
    state: &MetricsState,
) -> Option<squelch_core::metrics::StoreSnapshot> {
    let _permit = state.gate.acquire().await.ok()?;
    let store = state.store.clone();
    let config = state.config.clone();
    let account_id = state.account_id;

    let gathered = tokio::task::spawn_blocking(move || {
        // Band counts are not exported (they are a windowed view of the same
        // rows the tier gauges already carry), so this window only satisfies
        // `stats`'s signature.
        let bands_since = chrono::Utc::now() - Duration::days(30);
        let stats = store.stats(account_id, bands_since)?;
        let llm = squelch_core::metrics::usage_from_ledger(
            store.list_usage_by_category(account_id, squelch_core::metrics::LEDGER_ALL_DAYS)?,
        );
        let (db_bytes, wal_bytes) = squelch_core::metrics::db_file_sizes(&config.db_path);
        // A COUNT, never a listing: the exported gauge is unlabelled, so no
        // device name can reach an unauthenticated scrape even in principle.
        let devices_paired = store.count_client_devices(account_id)?;
        // Platform and version only, already folded: two phones on one build
        // are one entry, and nothing that names a device comes back.
        let clients = store.client_versions_seen(account_id)?;
        let client_actions = store.client_actions_seen(account_id)?;
        Ok::<_, squelch_core::CoreError>(squelch_core::metrics::StoreSnapshot {
            llm_cost_usd: squelch_core::metrics::estimate_cost_usd(&llm, &config),
            stats,
            llm,
            db_bytes,
            wal_bytes,
            devices_paired,
            clients,
            client_actions,
            triage_jobs: store.triage_job_counts(account_id)?,
            triage_budget: store.daily_budget_status(
                account_id,
                &config.triage.agent,
                chrono::Utc::now(),
            )?,
            retriage: store.retriage_progress(account_id)?,
        })
    })
    .await;

    match gathered {
        Ok(Ok(snapshot)) => Some(snapshot),
        // A HALF-WORKING SCRAPE BEATS A 500. The in-process counters are still
        // true and are exactly what an operator needs when the store is the
        // thing that is unhappy; a failed scrape would instead take the whole
        // series down and read as "the daemon is gone".
        Ok(Err(e)) => {
            eprintln!(
                "squelchd: metrics scrape could not read the store ({e}); serving counters only"
            );
            None
        }
        Err(e) => {
            eprintln!("squelchd: metrics scrape task join error ({e}); serving counters only");
            None
        }
    }
}

async fn serve_metrics(
    axum::extract::State(state): axum::extract::State<MetricsState>,
) -> impl axum::response::IntoResponse {
    let db = gather_store_metrics(&state).await;
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        squelch_core::metrics::render(&state.metrics, db.as_ref()),
    )
}

/// `GET /healthz`: `200` once the daemon is up, `503` until then, and nothing
/// else in either direction.
///
/// Two states and one word each, because anything that can open the metrics
/// port can read this: the listener carries no authentication of any kind, so
/// every byte answered here is a byte an unauthenticated caller gets. A phase
/// name, a count or a reason would each describe the tenant behind the port,
/// and none of them tells a probe anything it can act on — a probe has exactly
/// two moves.
async fn serve_healthz(
    axum::extract::State(readiness): axum::extract::State<Readiness>,
) -> impl axum::response::IntoResponse {
    if readiness.is_up() {
        (axum::http::StatusCode::OK, "ok")
    } else {
        (axum::http::StatusCode::SERVICE_UNAVAILABLE, "starting")
    }
}

/// The metrics door: `GET /metrics`, `GET /healthz`, and nothing else, on a
/// listener of its own. Kept off the main router deliberately — the doors sit
/// behind whatever proxy fronts them, and an unauthenticated scrape route must
/// not inherit that exposure.
///
/// Readiness belongs on THIS listener for the same reason and one more: the
/// doors serve nothing unauthenticated, so a probe against them would have to
/// hold a bearer token, and a token an orchestrator holds is a credential in a
/// pod spec. This listener is already separate, already unauthenticated, and
/// already the one a scraper is pointed at.
///
/// The two routes carry SEPARATE state, which is load bearing rather than tidy.
/// A scrape reads the store behind [`MetricsState`]'s single permit; a
/// readiness route that queued on the same permit would answer late whenever
/// the store is busy, and a late answer is a healthy tenant pulled out of
/// service. `/healthz` holds no store handle at all, so it cannot acquire that
/// dependency by a later edit.
fn build_metrics_router(state: MetricsState, readiness: Readiness) -> axum::Router {
    axum::Router::new()
        .route("/metrics", axum::routing::get(serve_metrics))
        .with_state(state)
        .merge(
            axum::Router::new()
                .route("/healthz", axum::routing::get(serve_healthz))
                .with_state(readiness),
        )
}

/// What the console says about the agent door, from the resolved mode.
fn agent_door_status(auth: &squelch_mcp::AgentDoorAuth) -> squelch_api::AgentDoorStatus {
    use squelch_mcp::AgentDoorAuth;
    match auth {
        AgentDoorAuth::Open => squelch_api::AgentDoorStatus::Open,
        AgentDoorAuth::OAuth(c) => squelch_api::AgentDoorStatus::OAuth {
            public_url: c.public_url().to_string(),
        },
        AgentDoorAuth::Misconfigured(_) => squelch_api::AgentDoorStatus::Unavailable,
    }
}

/// The router hosting both doors: `/mcp` (agent door, sealed-absent; it can
/// leave drafts unless the owner turned that off, and send only if the owner
/// turned that on, in Settings > Agents) and
/// `/client/*` (human door, bearer-authed, where every write to Gmail is
/// performed). They share the store; the agent door never sees the write
/// credential, and an allowed agent write is the human door's pipeline run on
/// its behalf (`ApiState::agent_mailer`).
///
/// The agent door's auth is `agent_door`, resolved once by the caller: Open
/// (Host guard only, the self-host door), OAuth (bearer JWTs bound to this
/// mailbox, plus the public protected-resource metadata), or Misconfigured
/// (`/mcp` answers 503 while the human door serves on). The two doors are
/// separate routers merged side by side, so neither inherits the other's
/// layers: the agent door never sees the human door's bearer check, CORS or
/// body limits, and the human door never sees the agent door's JWT check.
fn build_serve_router(
    store: Arc<SqliteStore>,
    account_email: &str,
    api_state: squelch_api::ApiState,
    mcp_cancel: CancellationToken,
    metrics: Arc<SyncMetrics>,
    agent_door: &squelch_mcp::AgentDoorAuth,
) -> anyhow::Result<axum::Router> {
    // The agent door gets the HUMAN DOOR'S OWN listing policy, read back off the
    // state rather than resolved from config a second time: the two doors must
    // agree about which packages exist, and the way that broke before was each
    // side deciding for itself.
    let agent_router = squelch_mcp::agent_door_router(
        store,
        account_email,
        api_state.shipment_policy(),
        api_state.triage_ranking(),
        mcp_cancel,
        agent_door,
        // The agent door's only way to write mail is the human door's own
        // send pipeline, and only as far as the owner's setting allows
        // (drafts unless they chose otherwise; sending only if they chose it).
        // It never holds the write credential.
        Some(api_state.agent_mailer()),
        // And the human door's own archive settings: one depth, one poke.
        api_state.archive_surfacing(),
    )?;
    // The latency histogram wraps BOTH doors, outermost, so the number it
    // records is everything the daemon did for the request including auth. It
    // is a `Router::layer`, which runs after routing: that is what gives it the
    // route template rather than the path.
    let app = squelch_api::router(api_state).merge(agent_router).layer(
        axum::middleware::from_fn_with_state(metrics, squelch_api::record_http_metrics),
    );
    Ok(app)
}

/// One runtime hosting the sync loop (READ credential only), both HTTP doors,
/// the auth-mail shredder, and background embedder init. Ctrl-C cancels MCP
/// sessions, stops axum, then flushes sync.
fn cmd_serve(
    config: Config,
    cap_sources: Stage2CapSources,
    args: &ServeArgs,
) -> Result<(), squelch_core::CoreError> {
    // Fail fast on config/address problems before opening the store or runtime.
    let bind = resolve_bind(args)?;
    let metrics_bind = resolve_metrics_bind(&config)?;
    let email = config.require_account_email()?;
    let client = config.oauth_client()?;
    // The store constructor below refuses a backend this build cannot use
    // too; asking here keeps the refusal ahead of opening the store, its
    // migrations, the embedder and the runtime.
    ensure_backend(config.credential_backend)?;

    let store = SqliteStore::open(&config.db_path)?;
    let account_id = store.ensure_account(&email)?;
    let store = Arc::new(store);

    let backend = config.credential_backend;
    let creds_path = config.resolve_credentials_path();
    // A writer that died between its temp write and its rename left a temp
    // file holding a token; every write sweeps them too, but a daemon that
    // never refreshes would otherwise keep one forever.
    if backend == squelch_core::config::CredentialBackend::File {
        squelch_core::credentials::remove_stale_temp_files(&creds_path);
    }

    // Manual-refresh signal: `POST /client/refresh` fires it, the poll loop
    // wakes early.
    let refresh = Arc::new(tokio::sync::Notify::new());

    // On-demand spam signal: `POST /client/spam/refresh` fires it and the poll
    // loop runs ONE spam-window fetch. Its own handle rather than a second use
    // of `refresh`, because the SPAM label is the one thing the loop
    // deliberately does not track and folding the two together would put that
    // fetch back on every ordinary refresh.
    let spam_refresh = Arc::new(tokio::sync::Notify::new());

    // Refine poke: an agent search that just queued access checks for archive
    // mail fires it (MCP `search_mail`, `GET /client/agent/search`) and the
    // refine lane starts on them now instead of at its next interval.
    let refine_poke = squelch_core::store::archive_surface::RefinePoke::new();

    // Wake channel: the store pokes it on every real `append_event`, and each
    // open `GET /client/events` stream re-reads the table past its own cursor.
    let event_tx = squelch_api::attach_event_channel(&store)?;

    // The APNs pusher is the second reader of that log, with its own persisted
    // cursor. Absence of `SQUELCH_RELAY_URL` is the whole feature flag. A relay
    // that IS configured but whose client won't build is a misconfiguration, not
    // "the feature is off" — say so loudly and keep serving mail.
    let pusher = match squelch_core::push::Pusher::from_config(
        store.clone() as Arc<dyn squelch_core::store::Store>,
        account_id,
        &config,
    ) {
        Ok(pusher) => pusher,
        Err(e) => {
            eprintln!(
                "squelchd: APNs pusher NOT started: a relay is configured but its HTTP client could not be built: {e}"
            );
            None
        }
    };
    // Subscribe BEFORE the sender is handed to the human door, so nothing
    // appended during startup is missed.
    let pusher_wake = event_tx.subscribe();

    // The opens poller runs the same relay in the other direction: it collects
    // read-tracking pixel hits the relay saw and appends the `opened` events the
    // pusher then delivers. Gated on `SQUELCH_RELAY_URL` AND `SQUELCH_TRACK_URL`
    // both: the relay's opens buffer is one global queue whose cursor ack
    // DELETES, so a daemon that mints no pixels (a hosted tenant given a relay
    // for push alone) must never collect. A daemon whose pixel is reached
    // directly needs none of this.
    let opens_poller = match squelch_core::tracking::OpensPoller::from_config(
        store.clone() as Arc<dyn squelch_core::store::Store>,
        account_id,
        &config,
    ) {
        Ok(poller) => poller,
        Err(e) => {
            eprintln!(
                "squelchd: opens poller NOT started: a relay is configured but its HTTP client could not be built: {e}"
            );
            None
        }
    };

    // Read tracking has two independent switches and no runtime error path: a
    // send the client asked to track just goes out untracked, and an open the
    // daemon never collects looks exactly like nobody opening the mail. Neither
    // is visible from Passband, so the posture is stated here or nowhere.
    report_tracking_posture(&config);
    report_metrics_posture(&config);

    // ONE registry for the process: the sync engine writes it, the metrics door
    // reads it. Built here so both sides get the same Arc.
    let sync_metrics = squelch_core::metrics::SyncMetrics::new();
    // ONE Gmail quota budget for this mailbox, handed to every Gmail caller:
    // the sync engine, the one-time sweeps engine, and the write door. Each
    // used to pace itself, so together they could ask for more than the
    // per-user limit (see squelch_core::gmail_budget).
    let gmail_budget = std::sync::Arc::new(squelch_core::gmail_budget::GmailBudget::new(
        config.sync.gmail_quota_profile,
    ));

    // Same shape, different question: the startup path below flips these and
    // the metrics door's `/healthz` reads them. See [`Readiness`] for which
    // conditions count as up and why.
    let readiness = Readiness::default();

    // The carrier poller keeps the shipments tracker moving between emails.
    // CARRIER CREDENTIALS ARE ITS ONLY FEATURE FLAG: with none configured
    // `from_config` returns None and no carrier API is ever contacted. Built
    // after the metrics registry so its polls land in the same scrape as
    // everything else.
    let shipment_poller = match squelch_core::carriers::poller::ShipmentPoller::from_config(
        store.clone() as Arc<dyn squelch_core::store::Store>,
        account_id,
        &config,
    ) {
        Ok(poller) => poller.map(|p| p.with_metrics(sync_metrics.clone())),
        Err(e) => {
            eprintln!(
                "squelchd: shipment poller NOT started: carriers are configured but the poller could not be built: {e}"
            );
            None
        }
    };
    // The kick handle + the carriers behind it, for `POST /client/shipments/poll`.
    // Taken here because `run` consumes the poller below; `None` when no poller
    // was built, which the endpoint reports as `kicked: false`.
    let shipment_kick = shipment_poller
        .as_ref()
        .map(|p| (p.kick_handle(), p.enabled_carriers()));

    // The human door SERVES WITH OR WITHOUT SQUELCH_API_TOKEN. Without it the
    // door accepts only the per-device tokens in the store, and a daemon with
    // neither still comes up 401ing everything: pairing is how the first
    // credential arrives, and `POST /client/pair` needs a door to arrive at.
    // The shared config->state wiring also attaches the WRITE-bound credential
    // store that enables the action endpoints; the sync engine below gets a
    // separate READ-bound store and never sees this one.
    // The agent door's auth mode, resolved from env ONCE: the router and the
    // console's description of the door are both built from this one value,
    // so they cannot disagree.
    // `for_account` is also applied inside the router; applying it here too
    // keeps the startup line and the console truthful about the same result.
    let agent_door = squelch_mcp::AgentDoorAuth::from_env().for_account(&email);

    // The human door counts its open event streams here and the sync engine
    // reads the count: a client with the app open keeps the poll at its
    // interval whatever `sync.idle_poll_secs` says.
    let event_listeners = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let engine_listeners = event_listeners.clone();
    let api_state = squelch_api::ApiState::from_config(store.clone(), &email, &config, cap_sources)
        .map_err(|e| other_err(format!("{e}")))?
        .with_agent_door(agent_door_status(&agent_door))
        .with_refresh(refresh.clone())
        .with_event_listeners(event_listeners)
        .with_spam_refresh(spam_refresh.clone())
        .with_refine_poke(refine_poke.clone())
        // THE SAME registry the sync engine writes, so `/client/stats` answers
        // "is this mailbox still connected" from live state rather than from a
        // copy taken at boot.
        .with_sync_metrics(sync_metrics.clone())
        .with_gmail_budget(gmail_budget.clone())
        .with_event_notifier(event_tx);
    // Carrier polling is BYOK, so the no-poller path is the common one and is
    // spelled out rather than left implicit: the door still serves
    // `/client/shipments/poll`, it just answers `kicked: false`.
    let api_state = match shipment_kick {
        Some((kick, carriers)) => api_state.with_shipment_poll_kick(kick, carriers),
        None => api_state,
    };

    // Google Calendar sync, read only. Built over the human door's own token
    // resolver whenever an OAuth client is configured, GRANTED OR NOT: without
    // a grant the loop idles and looks again each minute, because a grant can
    // arrive under a running daemon (`squelchd auth --calendar`). Its own task
    // with its own backoff, never inside the Gmail sync loop: a calendar
    // failure of any kind must not fail or delay Gmail sync, triage or ingest.
    let calendar_sync = api_state.calendar().cloned().and_then(|creds| {
        match squelch_core::gcal::sync::CalendarSync::new(store.clone(), account_id, creds) {
            Ok(sync) => Some(sync),
            Err(e) => {
                eprintln!(
                    "squelchd: calendar sync NOT started: its HTTP client could not be built: {e}"
                );
                None
            }
        }
    });
    let api_state = match &calendar_sync {
        Some(sync) => api_state.with_calendar_sync_kick(sync.kick_handle()),
        None => api_state,
    };

    // READ-bound store, shared by the sync loop and the contacts harvest.
    // Built before the runtime, so nothing has been spawned when it refuses.
    let sync_creds = store_for_backend(
        backend,
        account_id,
        email.clone(),
        CredentialKind::Read,
        creds_path,
        client,
    )?;

    let runtime = build_runtime()?;
    runtime.block_on(async move {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let mcp_cancel = CancellationToken::new();

        // The human door needs the same shutdown signal: SSE streams are
        // infinite and `with_graceful_shutdown` waits for open connections, so
        // without this one resident client holds the daemon open forever.
        let api_state = api_state.with_shutdown(shutdown_rx.clone());

        // POSTs opaque pings to the blind relay. Shares the sync loop's shutdown
        // watch and is awaited on the way out so it stops gracefully.
        let pusher_handle = match pusher {
            Some(pusher) => {
                eprintln!("squelchd: APNs pusher enabled (relay configured)");
                sync_metrics.set_apns_pusher_enabled(true);
                let pusher = pusher
                    .with_metrics(sync_metrics.clone())
                    .with_draft_pings(api_state.subscribe_draft_pings());
                let shutdown_rx = shutdown_rx.clone();
                Some(tokio::spawn(async move {
                    pusher.run(pusher_wake, shutdown_rx).await
                }))
            }
            None => {
                // The normal case — iOS push is opt-in — so a detail, not a warning.
                eprintln!(
                    "squelchd: APNs pusher disabled (no SQUELCH_RELAY_URL / [pusher] relay_url)"
                );
                None
            }
        };

        // Keeps the hidden-feature set in step with the control plane. Hosted
        // only: a daemon with no control token has nobody to ask, and what is
        // on there is whatever SQUELCH_FEATURES pins. Not awaited on the way
        // out: it holds no state worth flushing and exits on the same watch.
        // Read BEFORE the refresher starts, so this line names only what the
        // variable pinned and never what the control plane went on to grant.
        let pinned = api_state.features().list();
        if !pinned.is_empty() {
            eprintln!(
                "squelchd: hidden features pinned on by SQUELCH_FEATURES: {}",
                pinned.join(", ")
            );
        }
        if let Some(refresher) = api_state.feature_refresher() {
            eprintln!("squelchd: hidden features follow the control plane");
            tokio::spawn(refresher.run(shutdown_rx.clone()));
        }

        // Drains observed opens off the same relay. Shares the shutdown watch
        // and is awaited on the way out, like the pusher.
        let opens_handle = match opens_poller {
            Some(poller) => {
                eprintln!("squelchd: opens poller enabled (relay and tracking configured)");
                let shutdown_rx = shutdown_rx.clone();
                Some(tokio::spawn(async move { poller.run(shutdown_rx).await }))
            }
            None => {
                eprintln!(
                    "squelchd: opens poller disabled (needs SQUELCH_RELAY_URL / [pusher] relay_url, and SQUELCH_TRACK_URL / [tracking] base_url pointing at that relay)"
                );
                None
            }
        };

        // Polls the carrier APIs for shipments detected in mail. Same shape as
        // the two above: shares the shutdown watch, awaited on the way out.
        let shipment_handle = match shipment_poller {
            Some(poller) => {
                eprintln!(
                    "squelchd: shipment poller enabled (carriers: {})",
                    poller.enabled_carriers().join(", ")
                );
                let shutdown_rx = shutdown_rx.clone();
                Some(tokio::spawn(async move { poller.run(shutdown_rx).await }))
            }
            None => {
                // The normal case — carrier polling is BYOK — so a detail, not a
                // warning.
                eprintln!(
                    "squelchd: shipment poller disabled (no carrier credentials in [carriers])"
                );
                None
            }
        };

        // Reads Google Calendar. Same shape as the pollers above: shares the
        // shutdown watch, awaited on the way out.
        let calendar_handle = match calendar_sync {
            Some(sync) => {
                eprintln!("squelchd: calendar sync enabled (idles until calendar is granted)");
                let shutdown_rx = shutdown_rx.clone();
                Some(tokio::spawn(async move { sync.run(shutdown_rx).await }))
            }
            None => {
                eprintln!("squelchd: calendar sync disabled (no OAuth client configured)");
                None
            }
        };

        // No embedder override: the loop resolves it from the shared store each
        // tick, so it picks up the background-attached one. The gate is what
        // keeps a brand-new tenant's FIRST backfill from racing that attach:
        // without it the whole 30-day window ingests with no vector while the
        // model is still downloading, and the vector pass then has to embed all
        // of it in batches, which is the memory that OOM-killed tenant daemons.
        // Only the first backfill waits; polling and catch-up never do.
        // ONE "a vector may be missing" flag for both engines over this store:
        // the sync engine's refine lane reads it, and the sweeps engine's
        // blank-body heal raises it when it deletes a healed row's vector for
        // the drain to redo. Two flags would be two engines each believing
        // the other's debt is paid.
        let vectors_dirty = Arc::new(AtomicBool::new(true));
        let sync_handle = {
            let store = store.clone();
            let email = email.clone();
            let config = config.clone();
            let refresh = refresh.clone();
            let creds = sync_creds.clone();
            let sync_metrics = sync_metrics.clone();
            let sync_budget = gmail_budget.clone();
            let vectors_dirty = vectors_dirty.clone();
            // Taken before the spawn and dropped by the task, so readiness
            // tracks the engine and not the intention to start one.
            let sync_running = readiness.sync_started();
            let embedder_gate = readiness.embedder_gate();
            tokio::spawn(async move {
                let _sync_running = sync_running;
                mailbox_engine(store, creds, account_id, email, config, &sync_budget)
                    .with_refresh(refresh)
                    .with_event_listeners(engine_listeners)
                    .with_spam_refresh(spam_refresh)
                    .with_refine_poke(refine_poke)
                    .with_metrics(sync_metrics)
                    .with_embedder_gate(embedder_gate)
                    .with_vectors_dirty(vectors_dirty)
                    .run(shutdown_rx)
                    .await
            })
        };

        // One-time history sweeps on the read credential: the contacts harvest
        // seeding recipient autocomplete and the recipients backfill filling
        // To/Cc (and Bcc on the user's own sends) for every message that
        // predates the per-header columns (both headers-only), then the
        // blank-body heal re-fetching mail stored with an empty text body beside
        // a full HTML one (one raw GET per such message, and it hands each row
        // back to triage). Staggered past the startup sync burst and run one
        // after the other so they never contend for the same Gmail quota. Any
        // of them failing just retries on the next daemon start — each done
        // flag is only set on completion — and none can block the sync loop,
        // which runs in its own task.
        //
        // The stagger is measured from when that burst can actually START, which
        // is why this waits on the SAME embedder gate the first backfill does: on
        // a cold model cache the backfill now begins minutes into the run, and a
        // sweep that is one metadata GET per STORED MESSAGE (the heaviest Gmail
        // consumer here, and heavier since it stopped being Sent-only) landing on
        // top of a 30-day raw backfill on one credential is how a tenant earns a
        // 429 — which bubbles out of `run_once` and bounces the whole sync
        // lifecycle through backoff. The sweep paces itself on top of that.
        {
            let store = store.clone();
            let email = email.clone();
            let config = config.clone();
            let creds = sync_creds.clone();
            let sweep_metrics = sync_metrics.clone();
            let sweep_budget = gmail_budget.clone();
            let vectors_dirty = vectors_dirty.clone();
            let mut embedder_gate = readiness.embedder_gate();
            tokio::spawn(async move {
                wait_for_embedder_gate(&mut embedder_gate).await;
                tokio::time::sleep(std::time::Duration::from_secs(180)).await;
                // The same metrics the sync loop reports into, so a Gmail
                // refusal during a sweep lands on the same panel and alert;
                // the same vector flag, so the heal's deletions are drained.
                let engine =
                    mailbox_engine(store, creds, account_id, email, config, &sweep_budget)
                        .with_metrics(sweep_metrics)
                        .with_vectors_dirty(vectors_dirty);
                if let Err(e) = engine.harvest_sent_contacts().await {
                    eprintln!(
                        "squelchd: sent-contacts harvest incomplete (retries next start): {e}"
                    );
                }
                if let Err(e) = engine.backfill_recipients().await {
                    eprintln!(
                        "squelchd: recipients backfill incomplete (retries next start): {e}"
                    );
                }
                if let Err(e) = engine.heal_blank_bodies().await {
                    eprintln!("squelchd: blank-body heal incomplete (retries next start): {e}");
                }
            });
        }

        // One-shot shipment re-detect: the tracking-number detector was tightened
        // (eBay item ids and marketing digit-runs used to mint phantom rows), so
        // every existing row is re-judged against its own feeder message ONCE and
        // the ones the detector no longer produces are deleted. Store-only and
        // fast, so it runs inline on a blocking thread rather than on a delay —
        // no network, no Gmail quota.
        //
        // The ONCE is the store's job, not this call site's: it checks and writes
        // its own done-flag in the SAME TRANSACTION as the deletions, so the pass
        // cannot complete without being recorded. A re-run that skipped an
        // unrecorded pass would reap the rows the extractor has since written,
        // which no regex can reproduce. A failure here simply retries next start.
        {
            let store = store.clone();
            tokio::task::spawn_blocking(move || {
                match store.shipments_redetect_cleanup(account_id) {
                    // Count only — a tracking number is never logged.
                    Ok(n) => {
                        eprintln!("squelchd: shipment re-detect removed {n} stale shipment row(s)")
                    }
                    Err(_) => eprintln!("squelchd: shipment re-detect failed (retries next start)"),
                }
            });
        }

        // One-shot receipt re-parse: the amount patterns admitted a two-digit
        // fraction and nothing longer, so an unrounded float printed as a total
        // ("67.28999999999999 USD" — Amazon really does send this) matched in
        // the MIDDLE of the number and stored its tail as the amount. Every
        // existing row is re-parsed from its own message ONCE. Same shape as the
        // re-detect above: store-only, no network, and the ONCE is the store's
        // job, flag and corrections in one transaction.
        {
            let store = store.clone();
            tokio::task::spawn_blocking(move || match store.receipts_reparse_cleanup(account_id) {
                // Count only — an amount is never logged.
                Ok(n) => eprintln!("squelchd: receipt re-parse corrected {n} receipt total(s)"),
                Err(_) => eprintln!("squelchd: receipt re-parse failed (retries next start)"),
            });
        }

        // Auth-mail retention runs here because this process owns the write
        // credential (sync is bound to gmail.readonly by hard invariant). No-op
        // unless the shredder is enabled AND a write credential exists.
        {
            let shred_state = api_state.clone();
            tokio::spawn(async move {
                // Stagger past the startup sync burst; retention is in days.
                tokio::time::sleep(std::time::Duration::from_secs(120)).await;
                loop {
                    match squelch_api::run_shred_pass(&shred_state).await {
                        Ok(0) => {}
                        Ok(n) => eprintln!("squelchd: shredder trashed {n} old auth message(s)"),
                        // Never fatal; the error is redacted by construction.
                        Err(_) => eprintln!("squelchd: shredder pass failed"),
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                }
            });
        }

        spawn_statistics_refresh(store.clone());
        // No history retention pass: `SqliteStore::prune_history` exists and
        // is tested, but nothing schedules it. Run diagnostics and fast-lane
        // assessments are kept until it is decided what the triage eval and
        // cost work need from them (see `store/sqlite/retention.rs`).

        // SCHEDULED SEND fires here for the same reason the shredder runs
        // here: it sends on the WRITE credential (docs/SCHEDULED-SEND.md §4).
        // The store is the clock — the worker sleeps until the next row's
        // `send_at` and a handler's poke, no timer per row — so a restart
        // costs punctuality and never a mail. Unlike the shredder it HONOURS
        // THE SHUTDOWN WATCH: between rounds, and between rows inside a pass
        // (the pass reads the watch `api_state` carries), so a pass in flight
        // settles the row in hand and claims no more instead of leaving work
        // to lease recovery.
        let scheduled_handle = {
            let state = api_state.clone();
            let shutdown_rx = shutdown_tx.subscribe();
            let timing = squelch_api::ScheduledWorkerTiming::daemon();
            eprintln!(
                "squelchd: scheduled-send worker enabled (sleeps to the next due send; \
                 maintenance every {}s)",
                timing.maintenance.as_secs()
            );
            tokio::spawn(squelch_api::scheduled_send_worker(
                state,
                shutdown_rx,
                timing,
            ))
        };

        // SMART RE-NOTIFY (docs/RENOTIFY.md). Always spawned, and idle unless
        // the `renotify` hidden feature is on for this mailbox: the feature is
        // read at every pass, so ticking it on the admin page needs no restart.
        // Before the listener binds, so no Mac stream can open a session the
        // sweep would then close while it is live.
        squelch_api::close_stale_renotify_sessions(&api_state);
        let renotify_handle = tokio::spawn(squelch_api::renotify_worker(
            api_state.clone(),
            shutdown_tx.subscribe(),
            squelch_api::RenotifyTiming::daemon(),
        ));

        // Bind BEFORE building the embedder: its first-run model download must
        // not leave the doors unreachable.
        let app = build_serve_router(
            store.clone(),
            &email,
            api_state,
            mcp_cancel.clone(),
            sync_metrics.clone(),
            &agent_door,
        )
        .map_err(squelch_core::CoreError::Other)?;
        let listener = tokio::net::TcpListener::bind(bind)
            .await
            .map_err(|e| other_err(format!("bind {bind}: {e}")))?;
        let bound = listener.local_addr().unwrap_or(bind);
        // Single startup line. No tokens or message content are ever logged.
        // The agent door's mode is `AgentDoorAuth::describe`, the same text the
        // standalone squelch-mcp binary prints: the public URL in OAuth mode
        // (the door's own metadata publishes it) and nothing else of the
        // config; a misconfiguration names the setting at fault, never its
        // value.
        eprintln!(
            "squelchd: serving agent door http://{bound}/mcp ({mode}) and human door http://{bound}/client/*",
            mode = agent_door.describe(),
        );

        // The metrics door, on its OWN listener: a second bind, a one-route
        // router, and the same shutdown watch the doors and sync share. Absent
        // `[metrics] bind`, none of this exists.
        let metrics_handle = match metrics_bind {
            Some(addr) => {
                let listener = tokio::net::TcpListener::bind(addr)
                    .await
                    .map_err(|e| other_err(format!("bind metrics {addr}: {e}")))?;
                let bound = listener.local_addr().unwrap_or(addr);
                eprintln!(
                    "squelchd: serving metrics http://{bound}/metrics and readiness http://{bound}/healthz"
                );
                let app = build_metrics_router(
                    MetricsState {
                        metrics: sync_metrics.clone(),
                        store: store.clone(),
                        account_id,
                        config: Arc::new(config.clone()),
                        gate: Arc::new(tokio::sync::Semaphore::new(1)),
                    },
                    readiness.clone(),
                );
                // Its own receiver off the sender, so it does not depend on
                // where in this block the sync engine took the original.
                let mut shutdown_rx = shutdown_tx.subscribe();
                Some(tokio::spawn(async move {
                    squelch_api::serve(listener, app, async move {
                        loop {
                            if *shutdown_rx.borrow() {
                                return;
                            }
                            if shutdown_rx.changed().await.is_err() {
                                return;
                            }
                        }
                    })
                    .await
                }))
            }
            None => None,
        };

        // Build off the async workers and attach to the shared store when ready;
        // search is keyword-only until then.
        {
            let store = store.clone();
            let config = config.clone();
            let readiness = readiness.clone();
            let sync_metrics = sync_metrics.clone();
            // Read off config here because the blocking closure below takes it.
            let idle_unload_secs = config.embed.idle_unload_secs;
            // Its own receiver, taken here rather than inside the task, for the
            // same reason the metrics server takes one: it must not depend on
            // where in this block the sync engine took the original.
            let reaper_shutdown = shutdown_tx.subscribe();
            eprintln!(
                "squelchd: initializing semantic-recall embedder in the background \
                 (first run downloads the model; the server is already serving, \
                 search is keyword-only until the embedder is ready)"
            );
            tokio::spawn(async move {
                let built =
                    tokio::task::spawn_blocking(move || build_embedder(&config, Some(sync_metrics)))
                        .await;
                match built {
                    Ok(Some(embedder)) => {
                        match store.attach_embedder(embedder.clone() as Arc<dyn Embedder>) {
                            Ok(_) => eprintln!(
                                "squelchd: embedder ready — semantic + hybrid search now enabled"
                            ),
                            Err(e) => eprintln!(
                                "squelchd: embedder attach failed ({e}); search stays keyword-only"
                            ),
                        }
                        // Spawned even when the attach failed, and especially
                        // then: an embedder nobody can reach still loaded a
                        // session at construction, and this is what puts that
                        // 250 MB down instead of holding it until restart.
                        spawn_embedder_reaper(embedder, idle_unload_secs, reaper_shutdown);
                    }
                    Ok(None) => { /* build_embedder already logged the reason */ }
                    Err(e) => eprintln!(
                        "squelchd: embedder init task join error ({e}); search stays keyword-only"
                    ),
                }
                // Every arm above, including the ones that gave up: this is the
                // last step of startup, and readiness waits on it having
                // HAPPENED rather than on it having worked. See [`Readiness`].
                // The sync engine's first backfill waits on this same call for
                // the same reason: an init that failed is one to stop waiting
                // for, not one to wait out.
                readiness.embedder_settled();
            });
        }

        // Graceful shutdown: stop accepting, cancel MCP sessions, signal sync.
        // On Ctrl-C, or the SIGTERM a container stop sends (see
        // `stop_requested`): either way sync flushes and the scheduled-send
        // pass settles the row in hand.
        let shutdown_signal = {
            let mcp_cancel = mcp_cancel.clone();
            let shutdown_tx = shutdown_tx.clone();
            async move {
                stop_requested().await;
                eprintln!("squelchd: shutdown requested; stopping doors and flushing sync...");
                mcp_cancel.cancel();
                let _ = shutdown_tx.send(true);
            }
        };

        // `squelch_api::serve` rather than `axum::serve`: the same loop with
        // a live hyper timer, so a connection that never finishes its request
        // head is closed rather than held forever.
        let serve_result = squelch_api::serve(listener, app, shutdown_signal).await;

        // Tell sync to stop even if the server exited for another reason, then
        // wait for it to flush.
        let _ = shutdown_tx.send(true);
        match sync_handle.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("squelchd: sync ended with error: {e}"),
            Err(e) => eprintln!("squelchd: sync task join error: {e}"),
        }
        if let Some(handle) = pusher_handle {
            match handle.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => eprintln!("squelchd: APNs pusher ended with error: {e}"),
                Err(e) => eprintln!("squelchd: APNs pusher task join error: {e}"),
            }
        }
        if let Some(handle) = opens_handle {
            match handle.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => eprintln!("squelchd: opens poller ended with error: {e}"),
                Err(e) => eprintln!("squelchd: opens poller task join error: {e}"),
            }
        }
        if let Some(handle) = shipment_handle {
            match handle.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => eprintln!("squelchd: shipment poller ended with error: {e}"),
                Err(e) => eprintln!("squelchd: shipment poller task join error: {e}"),
            }
        }
        // Bounded: the loop stops a pass in flight on the shutdown signal,
        // and this is the backstop if anything in it ever stops listening.
        // Its writes are each their own transaction, so aborting it leaves
        // the store consistent; it holds nothing the mail engine needs.
        if let Some(mut handle) = calendar_handle {
            match tokio::time::timeout(CALENDAR_STOP_GRACE, &mut handle).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(e))) => eprintln!("squelchd: calendar sync ended with error: {e}"),
                Ok(Err(e)) => eprintln!("squelchd: calendar sync task join error: {e}"),
                Err(_) => {
                    eprintln!(
                        "squelchd: calendar sync did not stop within {}s; aborting it",
                        CALENDAR_STOP_GRACE.as_secs()
                    );
                    handle.abort();
                }
            }
        }
        if scheduled_handle.await.is_err() {
            eprintln!("squelchd: scheduled-send worker task join error");
        }
        if renotify_handle.await.is_err() {
            eprintln!("squelchd: renotify worker task join error");
        }
        if let Some(handle) = metrics_handle {
            match handle.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => eprintln!("squelchd: metrics server ended with error: {e}"),
                Err(e) => eprintln!("squelchd: metrics server task join error: {e}"),
            }
        }

        serve_result.map_err(|e| other_err(format!("http serve: {e}")))
    })?;

    eprintln!("squelchd: stopped.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use squelch_core::credentials::{FileCredentialStore, StoredToken};

    const BROKER: &str = "https://auth.passband.email";

    /// A one-CPU quota (the hosted tenant pod) gets the floor, not one; a
    /// wider box keeps its count; the operator's override is honoured and
    /// floored like everything else; and the real runtime is sized by exactly
    /// this rule, so dropping the builder call cannot pass on a wide CI box.
    #[test]
    fn the_runtime_never_runs_on_a_single_worker() {
        assert_eq!(worker_threads_for(1, None), MIN_WORKER_THREADS);
        assert_eq!(worker_threads_for(8, None), 8);
        assert_eq!(worker_threads_for(8, Some("6")), 6);
        assert_eq!(worker_threads_for(8, Some("1")), MIN_WORKER_THREADS);
        assert_eq!(worker_threads_for(8, Some("nonsense")), 8);
        assert_eq!(worker_threads_for(8, Some("")), 8);
        let runtime = build_runtime().expect("a runtime");
        assert_eq!(
            runtime.metrics().num_workers(),
            worker_threads_for(
                available_parallelism(),
                std::env::var(WORKER_THREADS_ENV).ok().as_deref()
            )
        );
    }

    /// PID 1 in the image stops on SIGTERM or not at all: the stop future must
    /// resolve on it, not only on Ctrl-C.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_stop_future_resolves_on_sigterm() {
        use tokio::signal::unix::{SignalKind, signal};
        // Held for the whole test, so the process has a SIGTERM handler before
        // the signal is raised whatever the timing below: the default
        // disposition would kill the test binary.
        let _keep = signal(SignalKind::terminate()).expect("a SIGTERM handler");
        let stop = tokio::spawn(stop_requested());
        // Let the spawned future register its own handler before the signal.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        // SAFETY: signalling our own pid; the handler above is installed.
        assert_eq!(unsafe { libc::kill(libc::getpid(), libc::SIGTERM) }, 0);
        tokio::time::timeout(std::time::Duration::from_secs(5), stop)
            .await
            .expect("stop_requested resolved on SIGTERM")
            .unwrap();
    }

    /// Every flag off, for tests that vary one at a time.
    fn bare_auth_args() -> AuthArgs {
        AuthArgs {
            write: false,
            calendar: false,
            headless: false,
            port: DEFAULT_HEADLESS_PORT,
            broker: None,
            export: false,
            out: None,
            import: false,
            expose_consent_listener: false,
        }
    }

    /// Nothing typed on the command line: the state an `auth` run is in when
    /// only the environment has an opinion.
    fn ambient_sources() -> AuthFlagSources {
        AuthFlagSources {
            broker: FlagSource::Ambient,
            port: FlagSource::Ambient,
        }
    }

    /// Parse an argv the way `main` does, so `value_source` is available and
    /// tests see exactly what the binary sees.
    fn parse_auth(argv: &[&str]) -> (AuthArgs, AuthFlagSources) {
        let matches = Cli::command().try_get_matches_from(argv).expect("parses");
        let sources = AuthFlagSources::from_matches(&matches);
        match Cli::from_arg_matches(&matches).unwrap().command {
            Command::Auth(args) => (args, sources),
            _ => panic!("expected auth subcommand"),
        }
    }

    /// `auth` parses: flags default off, and `--write`/`--headless --port` stick.
    #[test]
    fn auth_subcommand_parses() {
        let cli = Cli::parse_from(["squelchd", "auth"]);
        match cli.command {
            Command::Auth(args) => {
                assert!(!args.write);
                assert!(!args.headless);
                assert_eq!(args.port, DEFAULT_HEADLESS_PORT);
                // `SQUELCH_BROKER_URL` is the other way this gets set, so the
                // default only holds when the environment is quiet.
                if std::env::var_os("SQUELCH_BROKER_URL").is_none() {
                    assert!(args.broker.is_none());
                }
            }
            _ => panic!("expected auth subcommand"),
        }

        let cli = Cli::parse_from([
            "squelchd",
            "auth",
            "--write",
            "--headless",
            "--port",
            "9100",
        ]);
        match cli.command {
            Command::Auth(args) => {
                assert!(args.write);
                assert!(args.headless);
                assert_eq!(args.port, 9100);
            }
            _ => panic!("expected auth subcommand"),
        }
    }

    /// ONE Gmail consent (`gmail.modify`), with or without `--write`, on every
    /// transport, and its token fills BOTH Gmail slots. The slots stay
    /// separate: that is what keeps sync off the write path.
    #[test]
    fn one_gmail_consent_fills_both_slots() {
        for args in [
            bare_auth_args(),
            AuthArgs {
                write: true,
                ..bare_auth_args()
            },
            AuthArgs {
                write: true,
                broker: Some("https://auth.passband.email".to_string()),
                ..bare_auth_args()
            },
        ] {
            assert_eq!(auth_plan(&args), vec![AuthScopes::Read]);
        }
        assert_eq!(
            gmail_slots(AuthScopes::Read),
            vec![CredentialKind::Read, CredentialKind::Write]
        );
        assert_eq!(
            gmail_slots(AuthScopes::Calendar),
            vec![CredentialKind::Calendar]
        );
        assert_ne!(
            CredentialKind::Write.slot_key("you@x.com"),
            CredentialKind::Read.slot_key("you@x.com"),
            "the two credentials must land in separate storage slots"
        );
        assert_eq!(
            AuthScopes::Read.scopes(),
            vec![squelch_core::config::GMAIL_MODIFY_SCOPE.to_string()]
        );
    }

    /// A Gmail run offers the optional calendar consent while no calendar
    /// credential is stored; never once one is, never on `--calendar` (that IS
    /// the consent), and never on `--export` or `--import`.
    #[test]
    fn a_first_gmail_run_offers_calendar_once() {
        let plain = bare_auth_args();
        assert!(wants_first_calendar(&plain, false));
        assert!(!wants_first_calendar(&plain, true), "already stored");
        let write = AuthArgs {
            write: true,
            ..bare_auth_args()
        };
        assert!(wants_first_calendar(&write, false));
        for args in [
            AuthArgs {
                calendar: true,
                ..bare_auth_args()
            },
            AuthArgs {
                export: true,
                ..bare_auth_args()
            },
            AuthArgs {
                import: true,
                ..bare_auth_args()
            },
        ] {
            assert!(!wants_first_calendar(&args, false));
        }
        // The Gmail plan is unchanged: calendar is never folded into it.
        assert_eq!(auth_plan(&plain), vec![AuthScopes::Read]);
        assert_eq!(auth_plan(&write), vec![AuthScopes::Read]);
    }

    /// Enter or a y is a yes; anything else is a no.
    #[test]
    fn the_calendar_offer_defaults_to_yes() {
        for yes in ["", "\n", "y\n", "Y", "yes", " YES \n"] {
            assert!(says_yes(yes), "{yes:?}");
        }
        for no in ["n\n", "no", "N", "later", "q"] {
            assert!(!says_yes(no), "{no:?}");
        }
    }

    /// The optional step swallows its failure, so a run whose mailbox is set
    /// up never fails over calendar.
    #[test]
    fn a_failed_calendar_step_is_not_fatal() {
        assert!(!mint_first_calendar(|| Err(other_err("unticked".into()))));
        assert!(mint_first_calendar(|| Ok(())));
    }

    /// `--calendar` mints the Calendar credential and NOTHING ELSE: no Read
    /// or Write consent runs, so neither slot can be touched. Its own slot,
    /// the two calendar scopes requested, only the events scope required.
    #[test]
    fn auth_plan_calendar_mints_only_the_calendar_credential() {
        for transport in [None, Some(BROKER.to_string())] {
            let plan = auth_plan(&AuthArgs {
                calendar: true,
                broker: transport,
                ..bare_auth_args()
            });
            assert_eq!(plan, vec![AuthScopes::Calendar]);
        }
        let plan = auth_plan(&AuthArgs {
            calendar: true,
            headless: true,
            ..bare_auth_args()
        });
        assert_eq!(plan, vec![AuthScopes::Calendar]);
        assert_eq!(plan[0].kind(), CredentialKind::Calendar);
        assert_eq!(
            CredentialKind::Calendar.slot_key("you@x.com"),
            "you@x.com#calendar"
        );
        assert_eq!(
            plan[0].scopes(),
            vec![
                squelch_core::config::CALENDAR_EVENTS_SCOPE.to_string(),
                squelch_core::config::CALENDAR_LIST_SCOPE.to_string(),
            ]
        );
        assert!(
            plan[0].scopes().iter().all(|s| !s.contains("gmail")),
            "the calendar consent requests no Gmail scope"
        );

        // It parses with --headless, and refuses the flags that would renew
        // the Gmail pair or let a blob decide.
        let (args, _) = parse_auth(["squelchd", "auth", "--calendar", "--headless"].as_ref());
        assert!(args.calendar && args.headless);
        for clash in [
            ["squelchd", "auth", "--calendar", "--write"],
            ["squelchd", "auth", "--calendar", "--import"],
        ] {
            assert!(
                Cli::command().try_get_matches_from(clash).is_err(),
                "{clash:?} must be refused"
            );
        }
    }

    /// `--broker` REPLACES the loopback listener rather than decorating it, so
    /// asking for both is a usage error rather than a silent pick. The refusal
    /// is `resolve_transport`'s rather than clap's, because clap cannot tell a
    /// typed `--broker` from an inherited `SQUELCH_BROKER_URL`.
    #[test]
    fn a_typed_broker_excludes_every_other_transport() {
        let (args, sources) = parse_auth(["squelchd", "auth", "--broker", BROKER].as_ref());
        assert_eq!(args.broker.as_deref(), Some(BROKER));
        assert_eq!(sources.broker, FlagSource::CommandLine);
        assert_eq!(
            resolve_transport(&args, sources).unwrap(),
            Transport::Broker(BROKER.to_string())
        );

        for argv in [
            vec!["squelchd", "auth", "--broker", BROKER, "--headless"],
            vec!["squelchd", "auth", "--broker", BROKER, "--port", "9100"],
            vec!["squelchd", "auth", "--broker", BROKER, "--export"],
            vec!["squelchd", "auth", "--broker", BROKER, "--import"],
        ] {
            let (args, sources) = parse_auth(&argv);
            let err = resolve_transport(&args, sources)
                .expect_err(&format!("{argv:?} was accepted"))
                .to_string();
            assert!(err.contains("--broker"), "{argv:?}: {err}");
            assert!(err.contains("pick one"), "{argv:?}: {err}");
        }
    }

    /// An exported `SQUELCH_BROKER_URL` is a default, not an instruction.
    ///
    /// clap counts an env-sourced value as PRESENT for conflicts, so with the
    /// broker's conflicts declared there, an ambient URL made `auth --import`
    /// exit 2 naming a flag the operator never typed. Export and import are the
    /// sanctioned replacement for the broker (docs/BROKER.md: DO NOT DEPLOY), so
    /// an environment variable must not be able to disable them.
    #[test]
    fn an_ambient_broker_url_never_disables_another_transport() {
        let ambient = |extra: &[&str]| {
            let mut args = bare_auth_args();
            args.broker = Some(BROKER.to_string());
            for flag in extra {
                match *flag {
                    "--export" => args.export = true,
                    "--import" => args.import = true,
                    "--headless" => args.headless = true,
                    other => panic!("unhandled {other}"),
                }
            }
            args
        };

        // Alone it IS the transport; that is the point of the variable.
        assert_eq!(
            resolve_transport(&ambient(&[]), ambient_sources()).unwrap(),
            Transport::Broker(BROKER.to_string())
        );
        // Anything named on the command line wins over it, silently.
        assert_eq!(
            resolve_transport(&ambient(&["--import"]), ambient_sources()).unwrap(),
            Transport::Import
        );
        assert_eq!(
            resolve_transport(&ambient(&["--export"]), ambient_sources()).unwrap(),
            Transport::Export
        );
        assert_eq!(
            resolve_transport(&ambient(&["--headless"]), ambient_sources()).unwrap(),
            Transport::Loopback
        );
        let typed_port = AuthFlagSources {
            broker: FlagSource::Ambient,
            port: FlagSource::CommandLine,
        };
        assert_eq!(
            resolve_transport(&ambient(&[]), typed_port).unwrap(),
            Transport::Loopback
        );

        // And with no broker anywhere, the listener is the transport.
        assert_eq!(
            resolve_transport(&bare_auth_args(), ambient_sources()).unwrap(),
            Transport::Loopback
        );
    }

    /// `SQUELCH_BROKER_URL=` is how a leftover compose entry says nothing, and
    /// clap hands it over as a value all the same.
    ///
    /// Taken as a URL it resolves to `Broker("")` and dies on a parse nobody
    /// asked for: plain `squelchd auth` would stop working on any host that ever
    /// had the variable in a file. Blank from the environment is no broker at
    /// all, which is also what the "SQUELCH_BROKER_URL is set" note must decide.
    #[test]
    fn an_empty_ambient_broker_url_is_no_broker_at_all() {
        for blank in ["", "   ", "\t\n"] {
            let ambient = |extra: fn(&mut AuthArgs)| {
                let mut args = bare_auth_args();
                args.broker = Some(blank.to_string());
                extra(&mut args);
                args
            };

            let args = ambient(|_| {});
            assert!(
                broker_url(&args, ambient_sources()).is_none(),
                "{blank:?} must not read as a broker, note included"
            );
            assert_eq!(
                resolve_transport(&args, ambient_sources()).unwrap(),
                Transport::Loopback,
                "{blank:?}"
            );

            // And it refuses nothing, because there is nothing to conflict with.
            for (args, want) in [
                (ambient(|a| a.import = true), Transport::Import),
                (ambient(|a| a.export = true), Transport::Export),
                (ambient(|a| a.headless = true), Transport::Loopback),
            ] {
                assert_eq!(
                    resolve_transport(&args, ambient_sources()).unwrap(),
                    want,
                    "{blank:?}"
                );
            }
        }

        // A `--broker ""` that was TYPED is a different statement: it stays a
        // broker run here and fails later, where the operator typed it.
        let (args, sources) = parse_auth(&["squelchd", "auth", "--broker", ""]);
        assert_eq!(sources.broker, FlagSource::CommandLine);
        assert_eq!(
            resolve_transport(&args, sources).unwrap(),
            Transport::Broker(String::new())
        );
    }

    /// A `--port` the run will never bind is a tunnel or a `-p` mapping that
    /// will never be connected to, and silence about it looks exactly like a
    /// port that was honored.
    #[test]
    fn a_typed_port_nothing_will_bind_is_called_out() {
        let ignored = |argv: &[&str]| {
            let (args, sources) = parse_auth(argv);
            let transport = resolve_transport(&args, sources).expect("resolves");
            typed_port_is_ignored(&args, &transport, sources)
        };

        // Bound: the two runs that fix a port on purpose.
        for argv in [
            vec!["squelchd", "auth", "--headless", "--port", "9100"],
            vec![
                "squelchd",
                "auth",
                "--export",
                "--expose-consent-listener",
                "--port",
                "9100",
            ],
        ] {
            assert!(!ignored(&argv), "{argv:?}");
        }

        // Ignored: an ephemeral port is bound instead, whatever was typed.
        for argv in [
            vec!["squelchd", "auth", "--port", "9100"],
            vec!["squelchd", "auth", "--export", "--port", "9100"],
            vec!["squelchd", "auth", "--import", "--port", "9100"],
        ] {
            assert!(ignored(&argv), "{argv:?}");
        }

        // A port nobody typed is the default, and the default is never a
        // promise the operator made.
        for argv in [
            vec!["squelchd", "auth"],
            vec!["squelchd", "auth", "--headless"],
            vec!["squelchd", "auth", "--export"],
        ] {
            assert!(!ignored(&argv), "{argv:?}");
        }

        // An ambient broker plus a typed port resolves to Loopback, which binds
        // an ephemeral port unless the run is headless — so the warning belongs
        // there too.
        let mut args = bare_auth_args();
        args.broker = Some(BROKER.to_string());
        let typed_port = AuthFlagSources {
            broker: FlagSource::Ambient,
            port: FlagSource::CommandLine,
        };
        let transport = resolve_transport(&args, typed_port).unwrap();
        assert_eq!(transport, Transport::Loopback);
        assert!(typed_port_is_ignored(&args, &transport, typed_port));
        args.headless = true;
        assert!(!typed_port_is_ignored(&args, &transport, typed_port));

        // A typed port beside a TYPED broker never gets this far.
        let argv = ["squelchd", "auth", "--broker", BROKER, "--port", "9100"];
        let (args, sources) = parse_auth(&argv);
        assert!(resolve_transport(&args, sources).is_err());
    }

    /// The parse itself must no longer refuse these pairs, or the runtime check
    /// above never gets a chance to make the distinction.
    #[test]
    fn the_broker_conflicts_are_no_longer_clap_s_to_enforce() {
        for argv in [
            vec!["squelchd", "auth", "--import", "--broker", BROKER],
            vec!["squelchd", "auth", "--export", "--broker", BROKER],
        ] {
            assert!(
                Cli::try_parse_from(&argv).is_ok(),
                "{argv:?} must reach resolve_transport"
            );
        }
    }

    /// `--export` and `--import` are transports of their own: each replaces the
    /// loopback listener and the broker rather than decorating them, and asking
    /// for two at once is a usage error rather than a silent pick.
    #[test]
    fn export_and_import_parse_and_exclude_the_other_transports() {
        let cli = Cli::parse_from(["squelchd", "auth", "--export", "--write"]);
        match cli.command {
            Command::Auth(args) => {
                assert!(args.export);
                assert!(args.write, "one blob can carry both credentials");
                assert!(!args.import);
                assert!(!args.expose_consent_listener, "exposure is opt-in");
            }
            _ => panic!("expected auth subcommand"),
        }

        // The exposed listener needs a FIXED port, since that is what a
        // published container port maps to.
        let cli = Cli::parse_from([
            "squelchd",
            "auth",
            "--export",
            "--expose-consent-listener",
            "--port",
            "9100",
        ]);
        match cli.command {
            Command::Auth(args) => {
                assert!(args.expose_consent_listener);
                assert_eq!(args.port, 9100);
            }
            _ => panic!("expected auth subcommand"),
        }

        let cli = Cli::parse_from(["squelchd", "auth", "--import"]);
        match cli.command {
            Command::Auth(args) => {
                assert!(args.import);
                assert!(!args.export);
            }
            _ => panic!("expected auth subcommand"),
        }

        // Every one of these is a conflict between flags that can ONLY come
        // from the command line, so clap can still settle them itself.
        for argv in [
            vec!["squelchd", "auth", "--export", "--import"],
            vec!["squelchd", "auth", "--export", "--headless"],
            vec!["squelchd", "auth", "--import", "--headless"],
            // The blob decides what it carries, so --write has nothing to say.
            vec!["squelchd", "auth", "--import", "--write"],
            // Exposure is a property of the export listener and nothing else.
            vec!["squelchd", "auth", "--expose-consent-listener"],
            vec!["squelchd", "auth", "--import", "--expose-consent-listener"],
            // So is --out.
            vec!["squelchd", "auth", "--out", "cred.txt"],
            vec!["squelchd", "auth", "--import", "--out", "cred.txt"],
        ] {
            assert!(Cli::try_parse_from(&argv).is_err(), "{argv:?} was accepted");
        }
    }

    /// `--export --out PATH` exists so the blob is never written through a shell
    /// redirect, which takes the ambient umask (0644 on most hosts) for a file
    /// that is a live refresh token.
    #[cfg(unix)]
    #[test]
    fn the_exported_blob_lands_mode_0600() {
        use std::os::unix::fs::PermissionsExt;

        let (args, _) = parse_auth(&["squelchd", "auth", "--export", "--out", "cred.txt"]);
        assert_eq!(args.out.as_deref(), Some(std::path::Path::new("cred.txt")));

        let path = std::env::temp_dir().join(format!("squelch-export-{}.txt", std::process::id()));
        let blob = encode_transfer(&CredentialTransfer::new(
            "me@gmail.com".to_string(),
            vec![TransferCredential {
                kind: CredentialKind::Read,
                token: StoredToken {
                    access_token: "access".to_string(),
                    refresh_token: Some("refresh".to_string()),
                    expires_at: None,
                    scopes: None,
                },
            }],
        ))
        .unwrap();

        // Pre-create it world-readable: --out has to fix the mode, not inherit
        // whatever was there.
        std::fs::write(&path, b"stale").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let mut bytes = blob.clone().into_bytes();
        bytes.push(b'\n');
        write_private(&path, &bytes).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
        // And what landed still imports exactly as pasted.
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(decode_transfer(&written).unwrap().account, "me@gmail.com");
        std::fs::remove_file(&path).ok();
    }

    /// A TTY paste ends at the enter that follows it. Reading to EOF there would
    /// need a Ctrl-D nobody expects, which is the prompt hanging.
    #[test]
    fn a_pasted_blob_ends_at_the_newline_and_a_piped_one_at_eof() {
        let blob = "squelch-cred-v1.payload";
        // What a terminal delivers: the line, the enter, and nothing more that
        // this read may wait for.
        assert_eq!(
            read_blob_capped(format!("{blob}\n").as_bytes(), BlobRead::OneLine).unwrap(),
            blob
        );
        // A second line is not part of the paste.
        assert_eq!(
            read_blob_capped(format!("{blob}\nextra\n").as_bytes(), BlobRead::OneLine).unwrap(),
            blob
        );
        // A pipe has no prompt and no trailing newline to rely on.
        assert_eq!(
            read_blob_capped(blob.as_bytes(), BlobRead::ToEndOfInput).unwrap(),
            blob
        );

        // Nothing at all is the mistake worth naming, either way.
        for how in [BlobRead::OneLine, BlobRead::ToEndOfInput] {
            let err = read_blob_capped("   \n".as_bytes(), how)
                .unwrap_err()
                .to_string();
            assert!(err.contains("nothing arrived on stdin"), "{how:?}: {err}");
        }

        // The cap holds for a wedged pipe and for a terminal that somehow
        // delivers one enormous line.
        let flood = "x".repeat(MAX_BLOB_BYTES + 10);
        for how in [BlobRead::OneLine, BlobRead::ToEndOfInput] {
            let err = read_blob_capped(flood.as_bytes(), how)
                .unwrap_err()
                .to_string();
            assert!(err.contains("no credential blob is"), "{how:?}: {err}");
        }
    }

    /// A blob minted for another mailbox must be refused by name: storing it
    /// would make the sync loop ingest a stranger's mail as this account's.
    #[test]
    fn an_imported_blob_for_another_account_is_refused_and_names_both() {
        let err = check_transfer_account("me@gmail.com", "someone.else@gmail.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("someone.else@gmail.com"), "{err}");
        assert!(err.contains("me@gmail.com"), "{err}");
        assert!(err.contains("nothing was stored"), "{err}");

        // Google echoes its own casing and both values are hand-typed, so
        // neither case nor stray whitespace is a different mailbox.
        assert!(check_transfer_account("me@gmail.com", "me@gmail.com").is_ok());
        assert!(check_transfer_account("Me@Gmail.COM", " me@gmail.com\n").is_ok());
        // A subaddress or another domain IS a different mailbox.
        assert!(check_transfer_account("me@gmail.com", "me+x@gmail.com").is_err());
        assert!(check_transfer_account("me@gmail.com", "me@example.com").is_err());
    }

    /// The account inside a blob is a string whoever pasted it chose, and this
    /// refusal prints it. Raw, an ESC in it clears the screen and a BEL retitles
    /// the window, so it reaches the terminal as text or not at all.
    #[test]
    fn a_blob_cannot_paint_the_terminal_through_the_refusal() {
        let hostile = "\u{1b}[2J\u{1b}]0;pwned\u{7}victim@gmail.com\u{202e}";
        let err = check_transfer_account("me@gmail.com", hostile)
            .unwrap_err()
            .to_string();
        for sneaky in ['\u{1b}', '\u{7}', '\u{202e}', '\n', '\r'] {
            assert!(!err.contains(sneaky), "{sneaky:?} survived into: {err}");
        }
        // Disarmed, not dropped: what is left is still legible.
        assert!(err.contains("victim@gmail.com"), "{err}");
        assert!(err.contains("nothing was stored"), "{err}");

        // And length is theirs to choose unless it is bounded here too.
        let err = check_transfer_account("me@gmail.com", &"a".repeat(100_000))
            .unwrap_err()
            .to_string();
        assert!(
            err.len() < 1_000,
            "unbounded blob text: {} chars",
            err.len()
        );
    }

    /// Two consents in one export must have named the same mailbox, and the
    /// refusal prints both. They come from Google's userinfo by way of whichever
    /// session finished the consent, so they reach the terminal as text or not
    /// at all.
    #[test]
    fn a_mismatched_export_names_both_mailboxes_without_painting_the_terminal() {
        let hostile = "\u{1b}[2J\u{1b}]0;pwned\u{7}stranger@gmail.com\u{202e}";
        for (first, mailbox) in [(hostile, "me@gmail.com"), ("me@gmail.com", hostile)] {
            let err = check_export_same_account(first, mailbox)
                .unwrap_err()
                .to_string();
            for sneaky in ['\u{1b}', '\u{7}', '\u{202e}', '\n', '\r'] {
                assert!(!err.contains(sneaky), "{sneaky:?} survived into: {err}");
            }
            // Disarmed, not dropped: both mailboxes are still legible.
            assert!(err.contains("stranger@gmail.com"), "{err}");
            assert!(err.contains("me@gmail.com"), "{err}");
            assert!(err.contains("nothing was exported"), "{err}");
        }

        // And length is theirs to choose unless it is bounded here too.
        let err = check_export_same_account(&"a".repeat(100_000), "me@gmail.com")
            .unwrap_err()
            .to_string();
        assert!(
            err.len() < 1_000,
            "unbounded mailbox text: {} chars",
            err.len()
        );

        // Google echoes its own casing and the consent screen adds no
        // whitespace worth caring about; anything else IS a second account.
        assert!(check_export_same_account("me@gmail.com", "me@gmail.com").is_ok());
        assert!(check_export_same_account("Me@Gmail.COM", " me@gmail.com\n").is_ok());
        assert!(check_export_same_account("me@gmail.com", "me+x@gmail.com").is_err());
        assert!(check_export_same_account("me@gmail.com", "me@example.com").is_err());
    }

    /// An access-token-only entry works for an hour and then fails in a way
    /// nobody connects back to the paste, so import refuses it up front.
    #[test]
    fn an_imported_blob_without_a_refresh_token_is_refused() {
        let entry = |refresh: Option<&str>| TransferCredential {
            kind: CredentialKind::Write,
            token: StoredToken {
                access_token: "access".to_string(),
                refresh_token: refresh.map(|r| r.to_string()),
                expires_at: None,
                scopes: None,
            },
        };

        let usable =
            CredentialTransfer::new("me@gmail.com".to_string(), vec![entry(Some("refresh"))]);
        assert!(check_transfer_usable(&usable).is_ok());

        let doomed = CredentialTransfer::new("me@gmail.com".to_string(), vec![entry(None)]);
        let err = check_transfer_usable(&doomed).unwrap_err().to_string();
        assert!(err.contains("no refresh token"), "{err}");
        assert!(err.contains("nothing was stored"), "{err}");

        // Judged before ANY entry is written, so one bad credential cannot
        // leave the two slots disagreeing about which consent they came from.
        let mixed = CredentialTransfer::new(
            "me@gmail.com".to_string(),
            vec![entry(Some("refresh")), entry(None)],
        );
        assert!(check_transfer_usable(&mixed).is_err());
    }

    /// The daemon must read exactly what the exporter wrote, prefix included.
    #[test]
    fn a_blob_round_trips_from_export_to_import() {
        let minted = CredentialTransfer::new(
            "me@gmail.com".to_string(),
            vec![TransferCredential {
                kind: CredentialKind::Read,
                token: StoredToken {
                    access_token: "access".to_string(),
                    refresh_token: Some("refresh".to_string()),
                    expires_at: None,
                    scopes: None,
                },
            }],
        );
        let blob = encode_transfer(&minted).unwrap();
        // What a terminal paste actually delivers.
        let landed = decode_transfer(&format!("{blob}\n")).unwrap();
        assert!(check_transfer_account("me@gmail.com", &landed.account).is_ok());
        assert!(check_transfer_usable(&landed).is_ok());
        assert_eq!(landed.credentials[0].kind, CredentialKind::Read);
        assert_eq!(
            landed.credentials[0].token.refresh_token.as_deref(),
            Some("refresh")
        );
    }

    /// The pairing link's `url` parameter carries a whole URL inside a query
    /// value, so every character that means something to a query parser has to
    /// come out the other side as data.
    #[test]
    fn a_base_url_survives_the_deep_link_intact() {
        assert_eq!(
            percent_encode("http://127.0.0.1:8848"),
            "http%3A%2F%2F127.0.0.1%3A8848"
        );
        // An `&` or a `#` in the base would otherwise end the parameter early
        // and hand the rest to whatever came next.
        assert_eq!(percent_encode("a&b=c#d"), "a%26b%3Dc%23d");
        // The code's own alphabet is unreserved, so it rides through unchanged
        // and stays readable in the printed link.
        assert_eq!(percent_encode("ABCD-EFGH"), "ABCD-EFGH");
    }

    /// `--url` decides where a device POSTs a live pairing code, so a value that
    /// could not be one fails HERE, where the operator can see what they typed,
    /// rather than silently in an app that drops the link.
    #[test]
    fn pair_url_defaults_to_loopback_and_refuses_non_http() {
        let bind: SocketAddr = DEFAULT_BIND_ADDR.parse().unwrap();
        let with = |url: Option<&str>| {
            pair_base_url(
                &PairArgs {
                    url: url.map(str::to_string),
                },
                bind,
            )
        };

        let default = format!("http://{DEFAULT_BIND_ADDR}");
        assert_eq!(with(None).unwrap(), default);
        // A blank or whitespace `--url` is an environment saying nothing, the
        // same reading `auth` gives an empty `SQUELCH_BROKER_URL`.
        assert_eq!(with(Some("")).unwrap(), default);
        assert_eq!(with(Some("   ")).unwrap(), default);

        // Trailing slashes go, so the claim path is never `//client/pair`.
        assert_eq!(
            with(Some("https://box.tail.ts.net//")).unwrap(),
            "https://box.tail.ts.net"
        );
        assert_eq!(
            with(Some("HTTPS://Box.example")).unwrap(),
            "HTTPS://Box.example"
        );

        for bad in ["box.tail.ts.net", "file:///etc/passwd", "passband://pair"] {
            assert!(with(Some(bad)).is_err(), "{bad} must be refused");
        }
    }

    /// The default link follows the bind `serve` actually resolves, port and
    /// all: an operator who moved the daemon off 8848 would otherwise print a
    /// link to a port nothing is listening on.
    #[test]
    fn the_default_pair_url_follows_the_resolved_bind() {
        let default_for =
            |bind: &str| pair_base_url(&PairArgs { url: None }, bind.parse().unwrap()).unwrap();

        assert_eq!(default_for("127.0.0.1:9000"), "http://127.0.0.1:9000");
        // A wildcard bind keeps the PORT and prints as loopback: `0.0.0.0` is
        // not somewhere a device can POST.
        assert_eq!(default_for("0.0.0.0:9000"), "http://127.0.0.1:9000");
        assert_eq!(default_for("[::]:9000"), "http://[::1]:9000");
        // A specific non-loopback bind is named as-is; the operator chose it.
        assert_eq!(default_for("192.168.1.9:8848"), "http://192.168.1.9:8848");
        // `--url` still wins over every one of those.
        assert_eq!(
            pair_base_url(
                &PairArgs {
                    url: Some("https://box.tail.ts.net".to_string())
                },
                "0.0.0.0:9000".parse().unwrap()
            )
            .unwrap(),
            "https://box.tail.ts.net"
        );
    }

    /// The pairing code and the token it buys ride this URL. Plain http off the
    /// box means both are readable in transit, which is worth a warning; loopback
    /// and https are not.
    #[test]
    fn cleartext_off_box_pairing_is_the_only_thing_warned_about() {
        for quiet in [
            "http://127.0.0.1:8848",
            "http://localhost:8848",
            "http://[::1]:8848",
            "https://box.tail.ts.net",
            "HTTPS://Box.example",
        ] {
            assert!(!is_cleartext_offbox(quiet), "{quiet} needs no warning");
        }
        for loud in [
            "http://192.168.1.9:8848",
            "http://nas.local:8848",
            "http://[2001:db8::1]:8848",
            "HTTP://Nas.Local",
        ] {
            assert!(is_cleartext_offbox(loud), "{loud} must be warned about");
        }
    }

    /// The token subcommands parse the shapes the README documents.
    #[test]
    fn token_and_pair_subcommands_parse() {
        match Cli::parse_from(["squelchd", "token", "issue", "--name", "my phone"]).command {
            Command::Token(args) => match args.command {
                TokenCommand::Issue { name } => assert_eq!(name, "my phone"),
                _ => panic!("expected issue"),
            },
            _ => panic!("expected token subcommand"),
        }
        match Cli::parse_from(["squelchd", "token", "revoke", "7"]).command {
            Command::Token(args) => match args.command {
                TokenCommand::Revoke { id } => assert_eq!(id, 7),
                _ => panic!("expected revoke"),
            },
            _ => panic!("expected token subcommand"),
        }
        assert!(matches!(
            Cli::parse_from(["squelchd", "token", "list"]).command,
            Command::Token(TokenArgs {
                command: TokenCommand::List
            })
        ));
        // The warden execs this exact spelling in a tenant pod; a rename here
        // is a rename of a wire contract.
        assert!(matches!(
            Cli::parse_from(["squelchd", "token", "first-paired"]).command,
            Command::Token(TokenArgs {
                command: TokenCommand::FirstPaired
            })
        ));
        // A name is not optional: an unlabeled token is one nobody can pick out
        // of `token list` to revoke.
        assert!(Cli::try_parse_from(["squelchd", "token", "issue"]).is_err());

        match Cli::parse_from(["squelchd", "triage", "grant", "500", "--background"]).command {
            Command::Triage(TriageArgs {
                command:
                    TriageCommand::Grant {
                        runs,
                        background,
                        access,
                    },
            }) => assert_eq!((runs, background, access), (500, true, false)),
            _ => panic!("expected triage grant"),
        }
        match Cli::parse_from(["squelchd", "triage", "grant", "50", "--access"]).command {
            Command::Triage(TriageArgs {
                command:
                    TriageCommand::Grant {
                        runs,
                        background,
                        access,
                    },
            }) => assert_eq!((runs, background, access), (50, false, true)),
            _ => panic!("expected triage grant"),
        }
        // One lane at a time.
        assert!(
            Cli::try_parse_from([
                "squelchd",
                "triage",
                "grant",
                "5",
                "--background",
                "--access"
            ])
            .is_err()
        );
        assert!(matches!(
            Cli::parse_from(["squelchd", "triage", "status"]).command,
            Command::Triage(TriageArgs {
                command: TriageCommand::Status
            })
        ));

        match Cli::parse_from(["squelchd", "pair"]).command {
            Command::Pair(args) => assert!(args.url.is_none()),
            _ => panic!("expected pair subcommand"),
        }
        match Cli::parse_from(["squelchd", "pair", "--url", "https://box.example"]).command {
            Command::Pair(args) => assert_eq!(args.url.as_deref(), Some("https://box.example")),
            _ => panic!("expected pair subcommand"),
        }
    }

    /// `serve` parses, with and without an explicit `--bind`.
    #[test]
    fn serve_subcommand_parses() {
        let cli = Cli::parse_from(["squelchd", "serve"]);
        match cli.command {
            Command::Serve(args) => assert!(args.bind.is_none()),
            _ => panic!("expected serve subcommand"),
        }

        let cli = Cli::parse_from(["squelchd", "serve", "--bind", "0.0.0.0:9000"]);
        match cli.command {
            Command::Serve(args) => assert_eq!(args.bind.as_deref(), Some("0.0.0.0:9000")),
            _ => panic!("expected serve subcommand"),
        }
    }

    /// Bind resolution: flag > env > loopback default, and a bad value errors.
    #[test]
    fn resolve_bind_precedence_and_default() {
        let args = ServeArgs {
            bind: Some("127.0.0.1:9999".to_string()),
        };
        assert_eq!(
            resolve_bind(&args).unwrap(),
            "127.0.0.1:9999".parse::<SocketAddr>().unwrap()
        );

        let args = ServeArgs { bind: None };
        // Guard against a stray env var in the test process.
        unsafe {
            std::env::remove_var("SQUELCH_BIND");
        }
        assert_eq!(
            resolve_bind(&args).unwrap(),
            DEFAULT_BIND_ADDR.parse::<SocketAddr>().unwrap()
        );

        let args = ServeArgs {
            bind: Some("not-an-addr".to_string()),
        };
        assert!(resolve_bind(&args).is_err());
    }

    /// Two engines built for one mailbox hold ONE Gmail budget. The sync loop
    /// and the sweeps engine both come from `mailbox_engine`; if either went
    /// back to `SyncEngine::new`'s private budget, the mailbox would be paced
    /// twice over again (#263).
    #[test]
    fn mailbox_engines_share_one_gmail_budget() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("in-memory store"));
        let account_id = store.ensure_account("me@localhost").expect("account");
        let creds: Arc<dyn CredentialStore> = Arc::new(FileCredentialStore::new(
            account_id,
            "me@localhost".into(),
            PathBuf::from("/nonexistent/credentials.json"),
            OAuthClientConfig {
                client_id: "test".into(),
                client_secret: "test".into(),
            },
        ));
        let budget = Arc::new(squelch_core::gmail_budget::GmailBudget::new(
            Default::default(),
        ));
        let config = Config::default();
        let sync = mailbox_engine(
            store.clone(),
            creds.clone(),
            account_id,
            "me@localhost".into(),
            config.clone(),
            &budget,
        );
        let sweeps = mailbox_engine(
            store,
            creds,
            account_id,
            "me@localhost".into(),
            config,
            &budget,
        );
        assert!(Arc::ptr_eq(sync.gmail_budget(), &budget));
        assert!(Arc::ptr_eq(sweeps.gmail_budget(), &budget));
    }

    /// Both doors are mounted: `/client/stats` is bearer-gated (401, not 404)
    /// and `/mcp` exists.
    #[tokio::test]
    async fn router_mounts_both_doors() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let store = Arc::new(SqliteStore::open_in_memory().expect("in-memory store"));
        let account_id = store.ensure_account("me@localhost").expect("account");
        let api_state = squelch_api::ApiState::new(store.clone(), account_id, "test-token");
        let cancel = CancellationToken::new();
        let metrics = SyncMetrics::new();
        let app = build_serve_router(
            store,
            "me@localhost",
            api_state,
            cancel,
            metrics,
            &squelch_mcp::AgentDoorAuth::Open,
        )
        .expect("router builds");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/client/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "/client/stats must be mounted (auth-gated, not missing)"
        );

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/client/stats")
                    .header("authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "/client/stats must answer 200 with a valid bearer"
        );

        let resp = app
            .oneshot(Request::builder().uri("/mcp").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::NOT_FOUND, "/mcp must be mounted");
    }

    // -----------------------------------------------------------------------
    // The agent door's auth modes, on the REAL combined router.
    // -----------------------------------------------------------------------

    const AGENT_ISSUER: &str = "https://test-tenant.authkit.app";
    const AGENT_PUBLIC_URL: &str = "https://me.passband.email/mcp";
    const AGENT_HOST: &str = "me.passband.email";
    const MASTER: &str = "test-token";
    /// OAuth mode refuses the `me@localhost` placeholder, so these tests use
    /// a real-shaped address.
    const AGENT_ACCOUNT: &str = "me@example.com";

    /// Both doors, as `serve` builds them, with the agent door in `auth` mode.
    fn serve_router(
        auth: &squelch_mcp::AgentDoorAuth,
    ) -> (Arc<SqliteStore>, AccountId, Arc<SyncMetrics>, axum::Router) {
        let store = Arc::new(SqliteStore::open_in_memory().expect("in-memory store"));
        let account_id = store.ensure_account(AGENT_ACCOUNT).expect("account");
        let api_state = squelch_api::ApiState::new(store.clone(), account_id, MASTER);
        let metrics = SyncMetrics::new();
        let app = build_serve_router(
            store.clone(),
            AGENT_ACCOUNT,
            api_state,
            CancellationToken::new(),
            metrics.clone(),
            auth,
        )
        .expect("router builds");
        (store, account_id, metrics, app)
    }

    fn oauth_auth(jwks_url: &str) -> squelch_mcp::AgentDoorAuth {
        let auth = squelch_mcp::AgentDoorAuth::from_values(
            Some(AGENT_ISSUER),
            Some(AGENT_PUBLIC_URL),
            Some(jwks_url),
        );
        assert!(
            matches!(auth, squelch_mcp::AgentDoorAuth::OAuth(_)),
            "{auth:?}"
        );
        auth
    }

    /// A valid agent JWT for this daemon's mailbox.
    fn agent_jwt() -> String {
        use squelch_mcp::testing::{KEY_A_PEM, KID_A, sign, valid_claims};
        sign(
            &valid_claims(AGENT_ISSUER, AGENT_PUBLIC_URL, AGENT_ACCOUNT),
            KEY_A_PEM,
            KID_A,
        )
    }

    /// An MCP `initialize`, as an agent sends it.
    fn mcp_initialize(host: &str, bearer: Option<&str>) -> axum::http::Request<axum::body::Body> {
        let mut b = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("host", host)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        if let Some(t) = bearer {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        b.body(axum::body::Body::from(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
        ))
        .unwrap()
    }

    fn get_with(
        uri: &str,
        host: &str,
        bearer: Option<&str>,
    ) -> axum::http::Request<axum::body::Body> {
        let mut b = axum::http::Request::builder().uri(uri).header("host", host);
        if let Some(t) = bearer {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        b.body(axum::body::Body::empty()).unwrap()
    }

    async fn status_of(
        app: &axum::Router,
        req: axum::http::Request<axum::body::Body>,
    ) -> axum::http::StatusCode {
        use tower::ServiceExt as _;
        app.clone().oneshot(req).await.unwrap().status()
    }

    /// OAuth mode: `/mcp` challenges, the metadata answers, and the human
    /// door (and everything that is NOT a door) behaves exactly as in Open.
    #[tokio::test]
    async fn oauth_mode_challenges_mcp_and_leaves_the_rest_of_the_router_alone() {
        use axum::http::{StatusCode, header};
        use tower::ServiceExt as _;

        let jwks = squelch_mcp::testing::StubJwks::start().await;
        let (_s, _a, _m, app) = serve_router(&oauth_auth(&jwks.url));
        let (_s, _a, _m, open) = serve_router(&squelch_mcp::AgentDoorAuth::Open);

        let resp = app
            .clone()
            .oneshot(mcp_initialize(AGENT_HOST, None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            resp.headers()[header::WWW_AUTHENTICATE],
            "Bearer resource_metadata=\"https://me.passband.email/.well-known/oauth-protected-resource/mcp\""
        );

        let resp = app
            .clone()
            .oneshot(get_with(
                "/.well-known/oauth-protected-resource/mcp",
                AGENT_HOST,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");

        // Everything else answers exactly as it does with the door open.
        for (uri, bearer) in [
            ("/client/stats", None),
            ("/client/stats", Some(MASTER)),
            ("/healthz", None),
            ("/console", None),
            ("/", None),
            ("/no-such-route", None),
        ] {
            assert_eq!(
                status_of(&app, get_with(uri, AGENT_HOST, bearer)).await,
                status_of(&open, get_with(uri, AGENT_HOST, bearer)).await,
                "{uri} with bearer {:?}",
                bearer.is_some()
            );
        }
        assert_eq!(
            status_of(&app, get_with("/client/stats", AGENT_HOST, Some(MASTER))).await,
            StatusCode::OK
        );
    }

    /// I3, one direction: a valid agent JWT, which DOES open the agent door,
    /// is not a human door credential.
    #[tokio::test]
    async fn an_agent_jwt_does_not_open_the_human_door() {
        use axum::http::StatusCode;

        let jwks = squelch_mcp::testing::StubJwks::start().await;
        let (_s, _a, _m, app) = serve_router(&oauth_auth(&jwks.url));
        let jwt = agent_jwt();

        assert_eq!(
            status_of(&app, mcp_initialize(AGENT_HOST, Some(&jwt))).await,
            StatusCode::OK,
            "control: the token is a working agent credential"
        );
        for uri in ["/client/stats", "/client/updates", "/client/v2/feed"] {
            assert_eq!(
                status_of(&app, get_with(uri, AGENT_HOST, Some(&jwt))).await,
                StatusCode::UNAUTHORIZED,
                "{uri}"
            );
        }
    }

    /// I3, the other direction: a live device token and the master token,
    /// both of which DO open the human door, are refused at the agent door.
    #[tokio::test]
    async fn human_door_credentials_do_not_open_the_agent_door() {
        use axum::http::StatusCode;

        let jwks = squelch_mcp::testing::StubJwks::start().await;
        let (store, account_id, _m, app) = serve_router(&oauth_auth(&jwks.url));
        let device = store
            .issue_device_token(account_id, "phone")
            .expect("device token");
        assert!(device.token.starts_with("sqd_"));

        for cred in [device.token.as_str(), MASTER] {
            assert_eq!(
                status_of(&app, get_with("/client/stats", AGENT_HOST, Some(cred))).await,
                StatusCode::OK,
                "control: a working human door credential"
            );
            assert_eq!(
                status_of(&app, mcp_initialize(AGENT_HOST, Some(cred))).await,
                StatusCode::UNAUTHORIZED
            );
        }
        assert_eq!(jwks.fetches(), 0, "neither is even shaped like a JWT");
    }

    /// A bad agent-door setting must not take the mailbox down: `/mcp` is 503
    /// in every combination, and the human door serves on.
    #[tokio::test]
    async fn a_misconfigured_agent_door_leaves_the_human_door_serving() {
        use axum::http::StatusCode;

        let (_s, _a, _m, open) = serve_router(&squelch_mcp::AgentDoorAuth::Open);
        for auth in [
            squelch_mcp::AgentDoorAuth::from_values(Some(AGENT_ISSUER), None, None),
            squelch_mcp::AgentDoorAuth::from_values(None, Some(AGENT_PUBLIC_URL), None),
            squelch_mcp::AgentDoorAuth::from_values(Some("nope"), Some(AGENT_PUBLIC_URL), None),
        ] {
            assert!(matches!(auth, squelch_mcp::AgentDoorAuth::Misconfigured(_)));
            let (_s, _a, _m, app) = serve_router(&auth);
            for host in ["localhost", AGENT_HOST] {
                assert_eq!(
                    status_of(&app, mcp_initialize(host, None)).await,
                    StatusCode::SERVICE_UNAVAILABLE
                );
            }
            assert_eq!(
                status_of(&app, get_with("/client/stats", "localhost", Some(MASTER))).await,
                StatusCode::OK
            );
            for uri in [
                "/healthz",
                "/console",
                "/.well-known/oauth-protected-resource/mcp",
            ] {
                assert_eq!(
                    status_of(&app, get_with(uri, "localhost", None)).await,
                    status_of(&open, get_with(uri, "localhost", None)).await,
                    "{uri}"
                );
            }
        }
    }

    /// The latency histogram labels `/mcp` with the same route template in
    /// every mode (the merge did not change what `record_http_metrics` sees),
    /// and the metadata route by its template too.
    #[tokio::test]
    async fn the_agent_door_keeps_one_route_label_in_every_mode() {
        let jwks = squelch_mcp::testing::StubJwks::start().await;
        let mut labels = Vec::new();
        for auth in [
            squelch_mcp::AgentDoorAuth::Open,
            oauth_auth(&jwks.url),
            squelch_mcp::AgentDoorAuth::from_values(Some(AGENT_ISSUER), None, None),
        ] {
            let (_s, _a, metrics, app) = serve_router(&auth);
            let _ = status_of(&app, mcp_initialize("localhost", None)).await;
            let _ = status_of(
                &app,
                get_with(
                    "/.well-known/oauth-protected-resource/mcp",
                    AGENT_HOST,
                    None,
                ),
            )
            .await;
            let text = squelch_core::metrics::render(&metrics, None);
            let mut routes: Vec<String> = text
                .lines()
                .filter(|l| l.contains("method=\"POST\""))
                .filter_map(|l| l.split("route=\"").nth(1))
                .filter_map(|r| r.split('"').next())
                .map(str::to_owned)
                .collect();
            routes.sort();
            routes.dedup();
            if matches!(auth, squelch_mcp::AgentDoorAuth::OAuth(_)) {
                assert!(
                    text.contains("route=\"/.well-known/oauth-protected-resource/mcp\""),
                    "{text}"
                );
            }
            labels.push(routes);
        }
        assert_eq!(labels[0], vec!["/mcp".to_string()], "{labels:?}");
        assert_eq!(labels[0], labels[1], "OAuth changed the /mcp label");
        assert_eq!(labels[0], labels[2], "Misconfigured changed the /mcp label");
    }

    /// The metrics bind IS the feature flag: absent or blank means no listener,
    /// and a present-but-bad value fails the daemon rather than quietly serving
    /// no metrics.
    #[test]
    fn resolve_metrics_bind_is_the_feature_flag() {
        let mut config = Config::default();
        assert_eq!(resolve_metrics_bind(&config).unwrap(), None);

        config.metrics.bind = Some("   ".to_string());
        assert_eq!(resolve_metrics_bind(&config).unwrap(), None);

        config.metrics.bind = Some(" 127.0.0.1:9848 ".to_string());
        assert_eq!(
            resolve_metrics_bind(&config).unwrap(),
            Some("127.0.0.1:9848".parse::<SocketAddr>().unwrap())
        );

        config.metrics.bind = Some("nine-thousand".to_string());
        assert!(resolve_metrics_bind(&config).is_err());
    }

    /// A metrics door over an empty in-memory store, for the tests that care
    /// about the router rather than about the numbers.
    fn test_metrics_state() -> MetricsState {
        let store = Arc::new(SqliteStore::open_in_memory().expect("in-memory store"));
        let account_id = store.ensure_account("me@localhost").expect("account");
        let config = Config {
            // A path that does not exist: the db-size gauges must report 0 rather
            // than measuring whatever sits at the developer's default db path.
            db_path: std::env::temp_dir().join("squelch-metrics-door-test.db"),
            ..Config::default()
        };
        MetricsState {
            metrics: squelch_core::metrics::SyncMetrics::new(),
            store,
            account_id,
            config: Arc::new(config),
            gate: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }

    /// The metrics door serves `/metrics` and `/healthz` and NOTHING else — no
    /// door of the main router leaks onto this unauthenticated listener.
    #[tokio::test]
    async fn metrics_door_serves_only_metrics() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let app = build_metrics_router(test_metrics_state(), Readiness::default());

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("squelchd_build_info{version="));
        // The store read succeeded, so the db-derived block is present.
        assert!(text.contains("squelchd_db_size_bytes{file=\"wal\"} 0\n"));
        assert!(text.contains("squelchd_store_messages_sealed 0\n"));

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/client/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "the metrics listener hosts no other route"
        );
    }

    /// `/healthz` has two states and one word, and it reaches the second one
    /// only when the whole startup path has run.
    ///
    /// The first assertion is the bug this route exists for: a daemon that has
    /// bound its listeners and nothing more is NOT ready, however cleanly it
    /// accepts a connection.
    #[tokio::test]
    async fn healthz_is_up_only_once_startup_finished() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let readiness = Readiness::default();
        let app = build_metrics_router(test_metrics_state(), readiness.clone());
        let probe = |app: axum::Router| async move {
            let resp = app
                .oneshot(
                    Request::builder()
                        .uri("/healthz")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = resp.status();
            let body = axum::body::to_bytes(resp.into_body(), 1 << 10)
                .await
                .unwrap();
            (status, String::from_utf8(body.to_vec()).unwrap())
        };

        // Bound, and nothing else: the state the old TCP-accept probe called
        // Ready two seconds in.
        let (status, body) = probe(app.clone()).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body.split_whitespace().count(), 1, "one word, no detail");

        // Half started is not started. Each condition alone is still a 503.
        let sync_running = readiness.sync_started();
        assert_eq!(probe(app.clone()).await.0, StatusCode::SERVICE_UNAVAILABLE);

        // The sync engine's gate is this same bit, which is the point of it
        // being one bit: a gate taken before the settle opens with it, and one
        // taken after is already open.
        let gate_before = readiness.embedder_gate();
        assert!(!*gate_before.borrow(), "shut until the init resolves");
        readiness.embedder_settled();
        assert!(*gate_before.borrow(), "settling the probe opens the gate");
        assert!(
            *readiness.embedder_gate().borrow(),
            "and a late one is open"
        );

        let (status, body) = probe(app.clone()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "ok");

        // And a startup that came apart AFTER the bind reports 503 rather than
        // 200, which is the whole point: dropping the guard is what a sync task
        // that returned, panicked or was aborted does.
        drop(sync_running);
        let (status, body) = probe(app).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body.split_whitespace().count(), 1);
    }

    // -----------------------------------------------------------------------
    // The agent door's writes, end to end: the real daemon router, the real
    // send pipeline (ApiState::agent_mailer), and MCP over the wire. The
    // halves are tested apart (squelch-mcp with a recording mailer,
    // squelch-api's mailer called directly); this is the one place both run
    // together, as an agent meets them. SECURITY.md §4c and §5.
    // -----------------------------------------------------------------------

    fn ingest_for_agent(store: &SqliteStore, account: AccountId, word: &str) -> i64 {
        use squelch_core::sync::ingest::{RawFetched, ingest_with_rules};
        let now = chrono::Utc::now();
        let raw = RawFetched {
            account_id: account,
            gmail_msg_id: format!("m-{word}"),
            gmail_thread_id: Some(format!("t-{word}")),
            raw: format!(
                "From: Pat <pat@example.com>\r\nTo: {AGENT_ACCOUNT}\r\nSubject: About {word}\r\nDate: {}\r\n\r\nNotes on {word}.",
                now.to_rfc2822()
            )
            .into_bytes(),
            internal_date: Some(now),
            is_sent: false,
            is_spam: false,
            account_addr: AGENT_ACCOUNT.into(),
        };
        let message = ingest_with_rules(&raw, &Default::default(), now, &[], |_| false);
        store.ingest_message(&message).expect("ingest")
    }

    /// Finish every queued agent job, restricting the messages in `restricted`.
    fn assess_for_agent(store: &SqliteStore, account: AccountId, restricted: &[i64]) {
        use squelch_core::store::agent_triage::{AgentCommitOutcome, AgentTriageStore as _};
        use squelch_core::triage::decision::MessageDecision;
        while let Some(job) = ["triage", "access"].into_iter().find_map(|kind| {
            store
                .claim_agent_job(account, kind, chrono::Utc::now(), 120)
                .expect("claim")
        }) {
            let context = store.load_agent_context(&job).expect("context");
            let mut decision = MessageDecision {
                summary: "A meaningful update".into(),
                ..Default::default()
            };
            decision.external_access.restricted = restricted.contains(&job.message_id);
            decision.attention.show_in_fye = true;
            decision.attention.summary = "An update worth reading".into();
            decision.attention.relevant_message_ids = vec![job.message_id];
            let sources = vec![context.message.source.clone()];
            assert_eq!(
                store
                    .commit_agent_decision(&job, &context, &decision, &sources)
                    .expect("commit"),
                AgentCommitOutcome::Applied
            );
        }
    }

    #[tokio::test]
    async fn the_agent_doors_writes_keep_their_guards_through_the_real_pipeline() {
        use serde_json::json;
        use squelch_core::agent_write::AgentWriteAccess;
        use squelch_mcp::testing::wire::{Session, refused};

        let (store, account, _m, app) = serve_router(&squelch_mcp::AgentDoorAuth::Open);
        let visible = ingest_for_agent(&store, account, "lighthouse");
        let restricted = ingest_for_agent(&store, account, "vaultcode");
        assess_for_agent(&store, account, &[restricted]);

        let read_only = [
            "get_deadlines",
            "get_inbox_updates",
            "get_reading",
            "get_shipments",
            "get_thread",
            "list_sender_rules",
            "search_mail",
            "set_sender_rule",
        ];
        let tools = |extra: &[&str]| {
            let mut t: Vec<String> = read_only
                .iter()
                .chain(extra)
                .map(|s| s.to_string())
                .collect();
            t.sort();
            t
        };

        // An owner who never opened the setting: drafts, never sending.
        let mut session = Session::open(app.clone(), "localhost", None).await;
        assert_eq!(
            session.tool_names().await,
            tools(&["create_draft", "open_draft"])
        );

        // A reply to restricted mail is refused with the same answer as a
        // message that does not exist, and the restricted text stays out.
        for target in [restricted, 999_999] {
            let (text, reply) = session
                .tool(
                    "create_draft",
                    json!({"body": "Sounds good.", "reply_to_message_id": target}),
                )
                .await;
            assert!(refused(&reply), "reply to {target}: {text}");
            assert!(text.contains("not found"), "{text}");
            assert!(!text.contains("vaultcode"), "{text}");
        }
        // Control: a reply to allowed mail is drafted, addressed to its sender.
        let (text, reply) = session
            .tool(
                "create_draft",
                json!({"body": "Sounds good.", "reply_to_message_id": visible}),
            )
            .await;
        assert!(!refused(&reply), "control: {text}");
        let (text, reply) = session
            .tool(
                "send_email",
                json!({"body": "x", "to": "a@example.com", "subject": "x", "human_approved": true}),
            )
            .await;
        assert!(
            refused(&reply),
            "send_email is not offered at drafts: {text}"
        );

        // Sending allowed: the outbound guard still blocks a secret anywhere
        // in the composition, with no override, and never echoes it.
        AgentWriteAccess::Send
            .save(store.as_ref(), account)
            .expect("save level");
        let mut session = Session::open(app.clone(), "localhost", None).await;
        assert_eq!(
            session.tool_names().await,
            tools(&["create_draft", "open_draft", "send_email"])
        );
        let key = "AKIAIOSFODNN7EXAMPLE";
        for (field, value) in [
            ("subject", format!("keys {key}")),
            ("body", format!("use {key}")),
        ] {
            let mut args = json!({
                "body": "hello", "to": "a@example.com", "subject": "hello", "human_approved": true
            });
            args[field] = json!(value);
            let (text, reply) = session.tool("send_email", args).await;
            assert!(refused(&reply), "{field}: {text}");
            assert!(
                text.contains("outbound guard blocked"),
                "{field}: refused by the guard, not a missing credential: {text}"
            );
            assert!(
                !text.contains(key),
                "{field}: the guard echoed the secret: {text}"
            );
        }
        // The guard's audit rows say what was blocked, never the secret.
        let audit = store.list_audit(account, 100).expect("audit");
        assert!(
            audit
                .iter()
                .any(|e| e.detail.as_deref() == Some("blocked:guard")),
            "control: the blocks were audited: {audit:?}"
        );
        for entry in &audit {
            let row = format!("{:?} {:?} {:?}", entry.action, entry.target, entry.detail);
            assert!(!row.contains(key), "audit row carries the secret: {row}");
        }

        let (text, reply) = session
            .tool(
                "send_email",
                json!({"body": "Sounds good.", "reply_to_message_id": restricted, "human_approved": true}),
            )
            .await;
        assert!(refused(&reply) && text.contains("not found"), "{text}");
    }
}
