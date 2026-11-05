//! Transport security headers over every door the daemon serves.
//!
//! Two headers, both ones a DAST scan (the CASA Tier 2 assessment runs one,
//! docs/VERIFICATION.md) flags on every response that lacks them:
//!
//! - `Strict-Transport-Security`, ONLY when the request reached us over https.
//!   On hosted that is Traefik terminating TLS and saying so in
//!   `X-Forwarded-Proto`. Self-host on loopback http never sees the header, so
//!   nothing changes there. A client spoofing the header over plain http gains
//!   nothing: browsers ignore HSTS on an http response (RFC 6797 §8.1). No
//!   `includeSubDomains`: a tenant vhost speaks for itself, not its siblings.
//! - `X-Content-Type-Options: nosniff`, unless a handler already set it.

use axum::{
    extract::Request,
    http::{HeaderValue, header},
    middleware::Next,
    response::Response,
};

/// One year, the floor the HSTS preload list and most scanners expect.
pub const HSTS: &str = "max-age=31536000";

pub async fn transport_security(request: Request, next: Next) -> Response {
    let https = request
        .headers()
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .next()
                .is_some_and(|p| p.trim().eq_ignore_ascii_case("https"))
        });
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

    fn app() -> Router {
        Router::new()
            .route("/", get(|| async { "ok" }))
            .layer(middleware::from_fn(transport_security))
    }

    async fn headers(proto: Option<&str>) -> axum::http::HeaderMap {
        let mut req = Request::builder().uri("/");
        if let Some(p) = proto {
            req = req.header("x-forwarded-proto", p);
        }
        app()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .headers()
            .clone()
    }

    #[tokio::test]
    async fn hsts_only_behind_https() {
        assert_eq!(
            headers(Some("https")).await[header::STRICT_TRANSPORT_SECURITY],
            HSTS
        );
        assert_eq!(
            headers(Some("HTTPS, http")).await[header::STRICT_TRANSPORT_SECURITY],
            HSTS
        );
        assert!(
            !headers(Some("http"))
                .await
                .contains_key(header::STRICT_TRANSPORT_SECURITY)
        );
        assert!(
            !headers(None)
                .await
                .contains_key(header::STRICT_TRANSPORT_SECURITY)
        );
    }

    #[tokio::test]
    async fn nosniff_everywhere() {
        assert_eq!(
            headers(None).await[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
    }
}
