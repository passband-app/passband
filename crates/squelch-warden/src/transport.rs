//! Transport security headers over every route the warden serves.
//!
//! - `Strict-Transport-Security`, ONLY when the request reached us over https.
//!   On carrier that is Traefik terminating TLS and saying so in
//!   `X-Forwarded-Proto`. A local run on plain http never sees the header, so
//!   nothing changes there.
//! - `X-Content-Type-Options: nosniff`, unless a handler already set it.
//!
//! Both are what a DAST scan (the CASA assessment runs one) reports on
//! every response that lacks them.

use axum::{
    extract::Request,
    http::{HeaderValue, header},
    middleware::Next,
    response::Response,
};
use squelch_httpauth::{HSTS, forwarded_https};

pub async fn transport_security(request: Request, next: Next) -> Response {
    let https = request
        .headers()
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(forwarded_https);
    let mut response = next.run(request).await;
    let h = response.headers_mut();
    if https {
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

    async fn headers(proto: Option<&str>) -> axum::http::HeaderMap {
        let mut req = Request::builder().uri("/missing");
        if let Some(p) = proto {
            req = req.header("x-forwarded-proto", p);
        }
        Router::new()
            .route("/", get(|| async { "ok" }))
            .layer(middleware::from_fn(transport_security))
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .headers()
            .clone()
    }

    #[tokio::test]
    async fn hsts_only_behind_https_and_on_a_404_too() {
        let h = headers(Some("https")).await;
        assert_eq!(h[header::STRICT_TRANSPORT_SECURITY], HSTS);
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        let h = headers(None).await;
        assert!(!h.contains_key(header::STRICT_TRANSPORT_SECURITY));
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    }
}
