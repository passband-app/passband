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
//! A ROUTE THAT WAITS ON A MODEL IS DECLARED IN [`LLM_ROUTES`]. Its seconds are
//! the provider's, not the daemon's, and the histogram labels them
//! `class="llm"` so no panel has to plot them beside a store read or know
//! their names. Adding a handler that awaits a model before it answers means
//! adding its (method, template) there; a test walks the table against the
//! real router, so a renamed route cannot quietly fall back to `local`.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use squelch_core::metrics::{HTTP_ROUTE_UNMATCHED, RouteClass, SyncMetrics};

/// Every (method, route template) whose handler awaits a language model before
/// it answers. Keyed on the method too: `GET /client/rules` is a store read,
/// `POST /client/rules` infers a disposition.
pub const LLM_ROUTES: &[(&str, &str)] = &[
    ("GET", "/client/v2/fye/headline"),
    ("GET", "/client/rules/suggest"),
    ("POST", "/client/rules"),
    ("PUT", "/client/rules/{id}"),
];

/// The [`RouteClass`] of a request, from the table above. Anything not in it,
/// an unmatched path included, is the daemon's own time.
pub fn route_class(method: &str, route: &str) -> RouteClass {
    if LLM_ROUTES.iter().any(|(m, r)| *m == method && *r == route) {
        RouteClass::Llm
    } else {
        RouteClass::Local
    }
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
