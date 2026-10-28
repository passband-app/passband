//! squelch-mcp binary. Transport is chosen here and only here: stdio by
//! default, or the MCP Streamable HTTP transport at `/mcp` when given
//! `--http [addr]` / `SQUELCH_MCP_HTTP`. Tool logic lives in [`server`].

use std::net::SocketAddr;
use std::sync::Arc;

use rmcp::ServiceExt;
use rmcp::transport::stdio;
use squelch_core::store::SqliteStore;
use squelch_mcp::server::SquelchServer;
use squelch_mcp::{AgentDoorAuth, MCP_PATH, agent_door_router};
use tokio_util::sync::CancellationToken;

/// Default bind address for HTTP mode. Loopback only — never default to a
/// non-loopback interface; a reverse proxy is expected to front this listener.
const DEFAULT_HTTP_ADDR: &str = "127.0.0.1:8848";

/// Where the Streamable HTTP transport is mounted; sourced from the lib.
const HTTP_MCP_PATH: &str = MCP_PATH;

/// How the server talks to clients. Selected once, in `main`.
enum Transport {
    /// stdio (the default).
    Stdio,
    /// Streamable HTTP bound to `addr` (loopback by default).
    Http(SocketAddr),
}

/// The operator's shipments LISTING policy, read from the same config file the
/// daemon reads. The standalone bin has no `[carriers]` credentials to care
/// about, but it must still honour `max_failures` / `stale_after_days`: an
/// agent's package list is supposed to match its user's, and defaulting here
/// would reintroduce exactly the drift this value exists to remove.
fn shipment_policy() -> squelch_core::config::ShipmentListPolicy {
    squelch_core::config::Config::load().carriers.list_policy()
}

/// The archive depth from the daemon's own config, so `search_mail` reports
/// the same horizon here as under `squelchd serve`. No poke: this binary runs
/// no sync engine, so a matched archive row is queued for the daemon's worker
/// and never waited on.
fn archive_surfacing() -> squelch_core::store::archive_surface::ArchiveSurfacing {
    let config = squelch_core::config::Config::load();
    squelch_core::store::archive_surface::ArchiveSurfacing {
        archive_days: config.sync.archive_days,
        caps: (&config.triage.agent).into(),
        ..Default::default()
    }
}

/// Build the server object, so tests can construct it without binding a transport.
fn build_server() -> anyhow::Result<SquelchServer> {
    let store = Arc::new(SqliteStore::open(squelch_core::config::resolve_db_path())?);
    Ok(
        SquelchServer::new(store, &squelch_core::config::account_email())?
            .with_shipment_policy(shipment_policy())
            .with_ranking_config(squelch_core::config::Config::load().triage.ranking)
            .with_archive_surfacing(archive_surfacing()),
    )
}

/// Decide the transport from CLI args and env: `--http [addr]` or
/// `SQUELCH_MCP_HTTP` selects HTTP, otherwise stdio. An explicit `--http <addr>`
/// wins over the env var, which wins over the loopback default — the bind
/// address is never silently widened past loopback.
fn select_transport() -> anyhow::Result<Transport> {
    let mut args = std::env::args().skip(1);
    let mut http_flag = false;
    let mut flag_addr: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--http" => {
                http_flag = true;
                // Optional inline address: `--http 127.0.0.1:9000`.
                if let Some(next) = args.next() {
                    if next.starts_with('-') {
                        return Err(anyhow::anyhow!("unexpected argument `{next}` after --http"));
                    }
                    flag_addr = Some(next);
                }
            }
            other => {
                return Err(anyhow::anyhow!("unknown argument: {other}"));
            }
        }
    }

    let env_addr = std::env::var("SQUELCH_MCP_HTTP").ok();
    let use_http = http_flag || env_addr.is_some();
    if !use_http {
        return Ok(Transport::Stdio);
    }

    // Flag address wins over env; empty env means "HTTP at the default addr".
    let addr_str = flag_addr
        .or_else(|| env_addr.filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| DEFAULT_HTTP_ADDR.to_string());

    let addr: SocketAddr = addr_str
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid HTTP bind address `{addr_str}`: {e}"))?;
    Ok(Transport::Http(addr))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match select_transport()? {
        Transport::Stdio => {
            let server = build_server()?;
            let running = server.serve(stdio()).await?;
            running.waiting().await?;
        }
        Transport::Http(addr) => serve_http(addr).await?,
    }
    Ok(())
}

/// Serve the MCP Streamable HTTP transport on `addr` until ctrl-c.
async fn serve_http(addr: SocketAddr) -> anyhow::Result<()> {
    let store = Arc::new(SqliteStore::open(squelch_core::config::resolve_db_path())?);

    let shutdown = CancellationToken::new();
    // Construction errors surface before we bind. The door's auth mode comes
    // from the same env the daemon reads, through the same constructor.
    let account_email = squelch_core::config::account_email();
    // Bound to the account here as well as inside the router, so the mode
    // printed below is the mode the router serves.
    let auth = AgentDoorAuth::from_env().for_account(&account_email);
    let router = agent_door_router(
        store,
        &account_email,
        shipment_policy(),
        squelch_core::config::Config::load().triage.ranking,
        shutdown.child_token(),
        &auth,
        // No send pipeline in this binary: it serves the read tools only.
        // Drafts and sending are offered by `squelchd serve`.
        None,
        archive_surfacing(),
        // No feature flags or calendar grant here: no `search_calendar`.
        Default::default(),
    )?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr().unwrap_or(addr);

    // No tokens or message content are ever logged.
    eprintln!(
        "squelch-mcp: serving MCP Streamable HTTP on http://{bound}{HTTP_MCP_PATH} ({mode})",
        mode = auth.describe(),
    );

    // On ctrl-c, cancel the token: that terminates active MCP sessions.
    let shutdown_signal = {
        let shutdown = shutdown.clone();
        async move {
            let _ = tokio::signal::ctrl_c().await;
            eprintln!("squelch-mcp: shutting down");
            shutdown.cancel();
        }
    };

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: the server constructs with its 12 tools registered, and with
    /// no send pipeline and no calendar behind it (this binary) offers only the
    /// 8 that neither write mail nor read the calendar.
    #[test]
    fn constructs_server_and_offers_no_mail_writing_tool() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("in-memory store"));
        let server = SquelchServer::new(store, "me@localhost").expect("server");
        assert_eq!(server.tool_count(), 12, "expected exactly 12 MCP tools");
        let offered = server.offered_tool_names();
        assert_eq!(offered.len(), 8, "{offered:?}");
        assert!(!offered.iter().any(|n| n == "create_draft"
            || n == "open_draft"
            || n == "send_email"
            || n == "search_calendar"));
    }

    /// The agent door never reaches the draft store, the staged uploads or the
    /// outbox DIRECTLY (docs/SECURITY.md §4). It may leave a draft or send
    /// mail when the owner allows it (issue #394), but only by handing a
    /// composition to the host's `AgentMailer`; it cannot list, read, edit or
    /// delete a draft, and it holds no way to build or queue a message itself.
    ///
    /// THE TOOL LIST CANNOT SHOW THIS: a tool called `search` or `get_email`
    /// could quietly fold drafts (or their staged files) into what it returns
    /// and the list would look exactly as clean. So the crate's own source is
    /// read and must not name the draft or staged-upload store surface at all.
    /// The scan matches the store's IDENTIFIERS, case-sensitively, with `//`
    /// comments (doc comments included) stripped first, so prose that merely
    /// mentions drafts never trips it. This test module is cut off before the
    /// scan, since it has to spell the needles.
    #[test]
    fn the_mcp_crate_never_reaches_the_draft_store_itself() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut scanned = 0;
        for entry in std::fs::read_dir(&src).expect("read squelch-mcp/src") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read source");
            let text = if path.file_name().and_then(|n| n.to_str()) == Some("main.rs") {
                text.split("#[cfg(test)]")
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            } else {
                text
            };
            let code = strip_line_comments(&text);
            for needle in [
                "list_drafts",
                "draft_by_id",
                "upsert_draft",
                "create_agent_draft",
                "delete_draft",
                "DraftKey",
                "outbound_attachment",
                // The outbox (docs/SCHEDULED-SEND.md) is drafts' sibling: the
                // user's own finished mail, human door only.
                "scheduled_send",
                "ScheduledSend",
                "list_scheduled_sends",
                "claim_attachments_for_scheduled",
            ] {
                assert!(
                    !code.contains(needle),
                    "{} references `{needle}`: the agent door reaches drafts and the outbox only through AgentMailer",
                    path.display()
                );
            }
            scanned += 1;
        }
        assert!(scanned >= 3, "scanned only {scanned} source files");
    }

    /// Drop every `//` comment (so `///` and `//!` too) from Rust source,
    /// leaving `//` inside a plain string literal alone. Raw strings are not
    /// tracked; a quote in one can at worst strip too much of that line.
    fn strip_line_comments(src: &str) -> String {
        let mut out = String::with_capacity(src.len());
        for line in src.lines() {
            let bytes = line.as_bytes();
            let (mut in_str, mut escaped, mut cut) = (false, false, line.len());
            for i in 0..bytes.len() {
                let b = bytes[i];
                if in_str {
                    match (escaped, b) {
                        (true, _) => escaped = false,
                        (false, b'\\') => escaped = true,
                        (false, b'"') => in_str = false,
                        _ => {}
                    }
                } else if b == b'"' {
                    in_str = true;
                } else if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
                    cut = i;
                    break;
                }
            }
            out.push_str(&line[..cut]);
            out.push('\n');
        }
        out
    }

    #[test]
    fn comment_stripping_keeps_code_and_drops_prose() {
        let src = "/// upsert_draft in a doc\nlet a = 1; // list_drafts here\nlet u = \"http://x\"; draft_by_id();\n";
        let code = strip_line_comments(src);
        assert!(!code.contains("upsert_draft"));
        assert!(!code.contains("list_drafts"));
        assert!(
            code.contains("draft_by_id"),
            "code after a `//` in a string survives"
        );
    }
}
