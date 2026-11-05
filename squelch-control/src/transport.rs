//! Transport security headers over every route control serves.
//!
//! The CASA Tier 2 assessment's DAST scan (docs/VERIFICATION.md) flags any
//! response without `Strict-Transport-Security` or
//! `X-Content-Type-Options: nosniff`. Control knows its own scheme from
//! `SQUELCH_CONTROL_PUBLIC_URL`, so HSTS goes on whenever that is https and
//! never on a local http run. No `includeSubDomains`: `signup.passband.app`
//! speaks for itself, not for the rest of `passband.app`.

use axum::{
    extract::{Request, State},
    http::{HeaderValue, header},
    middleware::Next,
    response::Response,
};

/// One year, the floor the HSTS preload list and most scanners expect.
pub(crate) const HSTS: &str = "max-age=31536000";

pub(crate) async fn transport_security(
    State(secure): State<bool>,
    request: Request,
    next: Next,
) -> Response {
    let mut response = next.run(request).await;
    let h = response.headers_mut();
    if secure {
        h.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static(HSTS),
        );
    }
    h.entry(header::X_CONTENT_TYPE_OPTIONS)
        .or_insert(HeaderValue::from_static("nosniff"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, middleware, routing::get};
    use tower::ServiceExt;

    async fn headers(secure: bool) -> axum::http::HeaderMap {
        Router::new()
            .route("/", get(|| async { "ok" }))
            .layer(middleware::from_fn_with_state(secure, transport_security))
            .oneshot(
                Request::builder()
                    .uri("/missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .headers()
            .clone()
    }

    #[tokio::test]
    async fn hsts_on_https_only_and_on_a_404_too() {
        let h = headers(true).await;
        assert_eq!(h[header::STRICT_TRANSPORT_SECURITY], HSTS);
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        let h = headers(false).await;
        assert!(!h.contains_key(header::STRICT_TRANSPORT_SECURITY));
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    }
}
