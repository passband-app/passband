//! `squelch-embed`: the shared per-node embedding service (docs/EMBED-SERVICE.md).
//!
//! Configuration is two environment variables and nothing else:
//!
//! - `SQUELCH_EMBED_MODEL_DIR`: the hf-hub cache holding the baked weights,
//!   default `/models`. Read-only is fine; nothing is written there.
//! - `SQUELCH_EMBED_TOKEN`: if set, `POST /embed` requires
//!   `Authorization: Bearer <token>`. If unset, it requires nothing and the
//!   NetworkPolicy is the whole boundary; the startup log says which.
//!
//! Both listeners bind before the session loads, so the kubelet's probe gets a
//! 503 rather than a refused connection while the weights come off disk. A
//! session that cannot be built exits the process non-zero, with the reason.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use squelch_embed::coalescer::Coalescer;
use squelch_embed::http::{self, AppState};
use squelch_embed::log::Log;
use squelch_embed::metrics::Metrics;
use squelch_embed::protocol;
use squelch_embed::session;

const DEFAULT_MODEL_DIR: &str = "/models";
/// The fleet token is `openssl rand -base64 32` (44 characters); anything far
/// shorter is a placeholder somebody forgot to replace.
const MIN_TOKEN_LEN: usize = 32;

fn main() -> ExitCode {
    let log = Log::stderr();

    if let Some(arg) = std::env::args().nth(1) {
        if arg == "--version" {
            println!("squelch-embed {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        log.line(
            "takes no arguments; configuration is SQUELCH_EMBED_MODEL_DIR and SQUELCH_EMBED_TOKEN",
        );
        return ExitCode::from(2);
    }

    let model_dir = std::env::var_os("SQUELCH_EMBED_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_MODEL_DIR));
    let token = match std::env::var("SQUELCH_EMBED_TOKEN") {
        // The example Secret's placeholder is long enough to pass the length
        // check, so it is refused by name.
        Ok(t) if t.contains("REPLACE_ME") => {
            log.line("SQUELCH_EMBED_TOKEN is still the example placeholder; refusing to start");
            return ExitCode::from(2);
        }
        Ok(t) if t.trim().len() >= MIN_TOKEN_LEN => Some(Arc::<str>::from(t.trim())),
        Ok(_) => {
            log.line(&format!(
                "SQUELCH_EMBED_TOKEN is set but shorter than {MIN_TOKEN_LEN} characters; \
                 refusing to start (unset it for no auth)"
            ));
            return ExitCode::from(2);
        }
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            log.line("SQUELCH_EMBED_TOKEN is not valid UTF-8; refusing to start");
            return ExitCode::from(2);
        }
    };

    // Small on purpose: the work is on the inference thread, and the runtime
    // only parses JSON and moves bytes.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            log.line(&format!("cannot start the async runtime: {e}"));
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(run(model_dir, token, log))
}

async fn run(model_dir: PathBuf, token: Option<Arc<str>>, log: Log) -> ExitCode {
    let work = match tokio::net::TcpListener::bind(("0.0.0.0", protocol::PORT)).await {
        Ok(l) => l,
        Err(e) => {
            log.line(&format!("cannot bind 0.0.0.0:{}: {e}", protocol::PORT));
            return ExitCode::FAILURE;
        }
    };
    let ops = match tokio::net::TcpListener::bind(("0.0.0.0", protocol::METRICS_PORT)).await {
        Ok(l) => l,
        Err(e) => {
            log.line(&format!(
                "cannot bind 0.0.0.0:{}: {e}",
                protocol::METRICS_PORT
            ));
            return ExitCode::FAILURE;
        }
    };
    log.line(&format!(
        "listening on 0.0.0.0:{} (POST {}, GET {}) and 0.0.0.0:{} (GET {}, GET /metrics)",
        protocol::PORT,
        protocol::EMBED_PATH,
        protocol::HEALTH_PATH,
        protocol::METRICS_PORT,
        protocol::HEALTH_PATH,
    ));
    if token.is_some() {
        log.line("auth: POST /embed requires the bearer token from SQUELCH_EMBED_TOKEN");
    } else {
        log.line(
            "auth: SQUELCH_EMBED_TOKEN is unset, so POST /embed is unauthenticated \
             and the NetworkPolicy is the only boundary",
        );
    }
    log.line(&format!(
        "loading {} from {}",
        protocol::MODEL_CODE,
        model_dir.display()
    ));

    let metrics = Arc::new(Metrics::new());
    let loader_dir = model_dir.clone();
    let (coalescer, loaded) = Coalescer::start(
        Box::new(move || session::load_fastembed(loader_dir)),
        metrics.clone(),
        log.clone(),
    );
    let state = AppState {
        coalescer,
        metrics,
        token,
        log: log.clone(),
    };

    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let stopped = |mut rx: tokio::sync::watch::Receiver<bool>| async move {
        let _ = rx.wait_for(|s| *s).await;
    };
    let work_server = tokio::spawn(http::serve(
        work,
        http::work_router(state.clone()),
        stopped(stop_rx.clone()),
        log.clone(),
    ));
    let ops_server = tokio::spawn(http::serve(
        ops,
        http::metrics_router(state),
        stopped(stop_rx),
        log.clone(),
    ));

    let code = tokio::select! {
        result = loaded => match result {
            Ok(Ok(())) => {
                // Ready. Serve until told to stop.
                terminated().await;
                log.line("shutting down");
                ExitCode::SUCCESS
            }
            Ok(Err(e)) => {
                log.line(&format!("cannot build the embedding session: {e}"));
                ExitCode::FAILURE
            }
            Err(_) => {
                log.line("the inference thread exited before the session was built");
                ExitCode::FAILURE
            }
        },
        () = terminated() => {
            log.line("shutting down before the session was ready");
            ExitCode::SUCCESS
        }
    };

    let _ = stop_tx.send(true);
    let _ = work_server.await;
    let _ = ops_server.await;
    code
}

/// SIGTERM (the kubelet) or Ctrl-C.
async fn terminated() {
    #[cfg(unix)]
    {
        let mut term =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(_) => {
                    let _ = tokio::signal::ctrl_c().await;
                    return;
                }
            };
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
