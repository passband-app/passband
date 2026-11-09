//! Per-route latency for both doors: the middleware that feeds
//! [`squelch_core::metrics::HttpMetrics`].
//!
//! Applied with `Router::layer` so it runs AFTER routing, which is what puts
//! the matched template in the request's extensions. A request that matched
//! nothing has no template and records as [`HTTP_ROUTE_UNMATCHED`]: its actual
//! path is whatever the client typed and must never become a label.
//!
//! The clock stops when the handler returns its response HEAD, before the body
//! streams; see the histogram's docs for why that is the number wanted here.
//!
//! A handler that awaits a model before it answers owes [`ROUTE_CLASSES`] a
//! row; a test walks the table against the real router.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use squelch_core::metrics::{HTTP_ROUTE_UNMATCHED, RouteClass, SyncMetrics};

/// Every (method, route template) that is not [`RouteClass::Local`]. Keyed on
/// the method too: `GET /client/rules` is a store read, `POST /client/rules`
/// infers a disposition.
pub const ROUTE_CLASSES: &[(&str, &str, RouteClass)] = &[
    ("GET", "/client/v2/fye/headline", RouteClass::Llm),
    ("GET", "/client/rules/suggest", RouteClass::Llm),
    ("POST", "/client/rules", RouteClass::Llm),
    ("PUT", "/client/rules/{id}", RouteClass::Llm),
    // A relay: the response head is the gateway's, so the clock stops on the
    // provider's time to first byte.
    ("POST", "/client/assistant/messages", RouteClass::Llm),
    // NOT HERE, on purpose: `/client/memory` (GET, and PUT/DELETE on `{id}`)
    // is a store read and two store writes. The profile pass that fills it
    // spends its model call in the sync engine, never on a request.
];

/// Anything not in the table, an unmatched path included, is the daemon's own
/// time.
pub(crate) fn route_class(method: &str, route: &str) -> RouteClass {
    crate::limits::route_lookup(ROUTE_CLASSES, method, route, RouteClass::Local)
}

pub async fn record_http_metrics(
    State(metrics): State<Arc<SyncMetrics>>,
    req: Request,
    next: Next,
) -> Response {
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| HTTP_ROUTE_UNMATCHED.to_owned());
    let method = req.method().clone();
    let started = Instant::now();
    let resp = next.run(req).await;
    metrics.http().observe(
        &route,
        method.as_str(),
        resp.status().as_u16(),
        route_class(method.as_str(), &route),
        started.elapsed().as_secs_f64(),
    );
    resp
}
