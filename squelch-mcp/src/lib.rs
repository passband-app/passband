//! squelch-mcp: MCP server exposing squelch's read-mostly tools (agent door).
//!
//! [`agent_door_router`] is the one construction path for the whole HTTP door
//! (the MCP service, its auth, and the OAuth metadata routes), shared by the
//! MCP bin and `squelchd serve`. It is deliberately the ONLY public way to
//! get an HTTP agent door: the bare rmcp service underneath carries no auth,
//! so it is private, and nothing outside this crate can mount it by accident
//! in front of a hosted tenant. Sealed (auth-related) messages are
//! structurally absent from every tool result — see docs/SECURITY.md.

pub mod auth;
pub mod server;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

use std::sync::Arc;

use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use squelch_core::store::SqliteStore;
use tokio_util::sync::CancellationToken;

pub use auth::AgentDoorAuth;
pub use server::SquelchServer;

/// Path the Streamable HTTP transport is mounted at; clients connect to
/// `http://<addr>/mcp`. Shared so the MCP bin and `squelchd serve` agree.
pub const MCP_PATH: &str = "/mcp";

/// The concrete Streamable HTTP service type.
type SquelchHttpService = StreamableHttpService<SquelchServer, LocalSessionManager>;

/// Build the MCP Streamable HTTP tower `Service` for the agent door, with the
/// Host allow-list exactly as today: loopback plus `SQUELCH_MCP_ALLOWED_HOSTS`.
///
/// This is the bare service with NO AUTH, which is why it is private: it is
/// exactly what Open mode serves and nothing else may mount it. Every host goes
/// through [`agent_door_router`], which applies the configured [`AgentDoorAuth`].
///
/// Cancelling `cancellation` terminates active MCP sessions.
///
/// `shipment_policy` is the operator's `[carriers]` listing policy, and the
/// caller must pass the SAME value it gave `ApiState::with_shipment_policy`: the
/// agent door's package list is supposed to match the human door's.
fn streamable_http_service(
    store: Arc<SqliteStore>,
    account_email: &str,
    shipment_policy: squelch_core::config::ShipmentListPolicy,
    ranking: squelch_core::triage::agent_config::RankingConfig,
    cancellation: CancellationToken,
) -> anyhow::Result<SquelchHttpService> {
    // DNS-rebinding guard: rmcp defaults to loopback-only Host headers, which
    // 403s requests proxied by `tailscale serve` (Host: *.ts.net). Additive —
    // loopback is never dropped.
    build_service(
        store,
        account_email,
        shipment_policy,
        ranking,
        cancellation,
        squelch_core::config::mcp_allowed_hosts(),
    )
}

fn build_service(
    store: Arc<SqliteStore>,
    account_email: &str,
    shipment_policy: squelch_core::config::ShipmentListPolicy,
    ranking: squelch_core::triage::agent_config::RankingConfig,
    cancellation: CancellationToken,
    allowed_hosts: Vec<String>,
) -> anyhow::Result<SquelchHttpService> {
    let template = SquelchServer::new(store, account_email)?
        .with_shipment_policy(shipment_policy)
        .with_ranking_config(ranking);
    Ok(StreamableHttpService::new(
        move || Ok(template.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_cancellation_token(cancellation)
            .with_allowed_hosts(allowed_hosts),
    ))
}

/// Build the whole agent door as an `axum::Router`, to be merged into the
/// host's router. The one construction path, so the door (and its auth) is
/// identical whichever binary hosts it.
///
/// What the router contains depends on `auth`:
///
/// - [`AgentDoorAuth::Open`]: the MCP service at [`MCP_PATH`] with no layer at
///   all and today's Host allow-list. Byte-for-byte the pre-OAuth door (I4).
///   No metadata routes.
/// - [`AgentDoorAuth::OAuth`]: the MCP service behind
///   [`auth::require_agent_token`], with the public URL's host (and
///   `host:port`) appended to the Host allow-list, plus the two
///   unauthenticated protected-resource-metadata GET routes. The public host
///   is added HERE and only here (I5): the warden never renders
///   `SQUELCH_MCP_ALLOWED_HOSTS`, so an older daemon image behind a published
///   Ingress path still answers 403 from the Host guard.
/// - [`AgentDoorAuth::Misconfigured`]: every request under [`MCP_PATH`]
///   answers 503 and the MCP service is never built. No metadata routes. An
///   error is logged here, once, at startup.
///
/// The returned router has no fallback, so it merges into one that does.
pub fn agent_door_router(
    store: Arc<SqliteStore>,
    account_email: &str,
    shipment_policy: squelch_core::config::ShipmentListPolicy,
    ranking: squelch_core::triage::agent_config::RankingConfig,
    cancellation: CancellationToken,
    auth: &AgentDoorAuth,
) -> anyhow::Result<axum::Router> {
    use axum::routing::{any, get};

    match auth {
        AgentDoorAuth::Open => {
            let service = streamable_http_service(
                store,
                account_email,
                shipment_policy,
                ranking,
                cancellation,
            )?;
            Ok(axum::Router::new().nest_service(MCP_PATH, service))
        }
        AgentDoorAuth::Misconfigured(why) => {
            // The reason names the setting, never its value (I6).
            eprintln!("squelch-mcp: ERROR agent door misconfigured, /mcp will answer 503: {why}");
            // `nest_service`, like the other two modes, so the route template
            // the latency histogram records for `/mcp` is the same in all three.
            Ok(axum::Router::new().nest_service(MCP_PATH, any(auth::misconfigured_door)))
        }
        AgentDoorAuth::OAuth(config) => {
            let mut allowed_hosts = squelch_core::config::mcp_allowed_hosts();
            allowed_hosts.extend(config.public_hosts().iter().cloned());
            let service = build_service(
                store,
                account_email,
                shipment_policy,
                ranking,
                cancellation,
                allowed_hosts,
            )?;
            let state = auth::AuthLayerState::new(config, account_email);
            // `route_layer`, not `layer`: the guard wraps the MCP routes only,
            // leaves the router without a fallback, and so cannot end up in
            // front of the metadata routes merged in below.
            let door = axum::Router::new()
                .nest_service(MCP_PATH, service)
                .route_layer(axum::middleware::from_fn_with_state(
                    state,
                    auth::require_agent_token,
                ));

            let body =
                axum::body::Bytes::from(auth::protected_resource_metadata(config).to_string());
            let meta = move || {
                let body = body.clone();
                async move { auth::metadata_response(body) }
            };
            let metadata = axum::Router::new()
                .route(auth::PROTECTED_RESOURCE_METADATA_PATH, get(meta.clone()))
                .route(auth::PROTECTED_RESOURCE_METADATA_ROOT_PATH, get(meta));

            eprintln!("squelch-mcp: agent door requires OAuth bearer tokens");
            Ok(door.merge(metadata))
        }
    }
}
