//! Bearer auth for the daemon's routes. A bearer is OPTIONAL — a deployment
//! may serve `/v1/push` open-but-rate-limited — and that choice is made once at
//! router construction: these layers are mounted only when a token is
//! configured.
//!
//! There are two bearers and two layers. [`require_bearer`] accepts ONLY the
//! full `SQUELCH_RELAY_AUTH_TOKEN` and guards `GET /v1/opens`, the drain of a
//! single global queue. [`require_push_bearer`] guards `POST /v1/push` and
//! accepts the full token OR the push-only `SQUELCH_RELAY_PUSH_TOKEN`, the one
//! the hosted fleet holds, so a compromised tenant pod can ping device tokens
//! it already has and can NOT read or delete anyone's opens. With the push
//! token unset, the push layer accepts exactly what the full one does.
//!
//! `Authorization: Bearer <token>` is compared in CONSTANT TIME, so a timing
//! side channel cannot leak it prefix-by-prefix. Against two candidates BOTH
//! compares always run and their results are combined without short-circuit,
//! so timing does not say which class of token a guess was near either. Any
//! failure is a bare 401; no token is ever logged or echoed.
//!
//! The compare and the header parse are shared with the human door in
//! [`squelch_httpauth`] — one copy, one test suite, one place a fix lands. That
//! crate's only dependency is `subtle`, so sharing them does not link the mail
//! core into this blind, publicly-hosted binary.

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode, header::AUTHORIZATION},
    middleware::Next,
    response::Response,
};
use squelch_httpauth::{ct_eq, parse_bearer};

use crate::state::RelayState;

/// The bearer the request presented, if it presented a well-formed one.
fn presented(req: &Request<Body>) -> Option<&str> {
    req.headers()
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(parse_bearer)
}

/// Middleware: require the FULL bearer or return 401. Guards `GET /v1/opens`,
/// and is mounted only when `SQUELCH_RELAY_AUTH_TOKEN` is configured. The
/// push-only token is deliberately NOT a candidate here.
pub async fn require_bearer(
    State(state): State<RelayState>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(expected) = state.auth_token() else {
        // Unreachable in practice: the layer is not mounted without a token.
        // Fail CLOSED anyway rather than inventing an open path.
        return Err(StatusCode::UNAUTHORIZED);
    };

    match presented(&req) {
        Some(tok) if ct_eq(tok.as_bytes(), expected.as_bytes()) => Ok(next.run(req).await),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

/// Middleware: require the full bearer OR the push-only one, else 401. Guards
/// `POST /v1/push`, mounted only when `SQUELCH_RELAY_AUTH_TOKEN` is configured.
pub async fn require_push_bearer(
    State(state): State<RelayState>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(full) = state.auth_token() else {
        // Same fail-closed branch as `require_bearer`. A push token alone is
        // refused at boot, so it is never the only thing standing here.
        return Err(StatusCode::UNAUTHORIZED);
    };

    let ok = match presented(&req) {
        Some(tok) => {
            // BOTH compares, then a NON-short-circuiting `|`: `||` would skip
            // the second compare whenever the first matched, and the time
            // saved would tell a caller which token class it had.
            let full_ok = ct_eq(tok.as_bytes(), full.as_bytes());
            let push_ok = state
                .push_token()
                .is_some_and(|push| ct_eq(tok.as_bytes(), push.as_bytes()));
            full_ok | push_ok
        }
        None => false,
    };
    if ok {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode, header},
        middleware,
        routing::get,
    };
    use tower::ServiceExt;

    use crate::config::{Config, Environment};

    use super::*;

    const KEY: &str = include_str!("../tests/fixture_test_key.p8");
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    /// One bare route behind the layer: this proves the WIRING (401 vs pass
    /// through) and the fail-closed no-token branch, not the compare — that is
    /// tested once in `squelch_httpauth`.
    fn app(auth_token: Option<String>) -> Router {
        gated(auth_token, None, require_bearer)
    }

    /// The same bare route behind either layer, with either bearer set.
    fn gated<F, Fut>(auth_token: Option<String>, push_token: Option<String>, layer: F) -> Router
    where
        F: Fn(State<RelayState>, Request<Body>, Next) -> Fut + Clone + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Response, StatusCode>> + Send + 'static,
    {
        let state = RelayState::new(Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            apns_key_pem: KEY.to_string(),
            apns_key_id: "KEYID12345".into(),
            apns_team_id: "TEAMID6789".into(),
            apns_topics: vec!["dev.squelch.ios".into()],
            apns_env: Environment::Production,
            auth_token,
            push_token,
            apns_url_override: None,
            db_path: None,
            trusted_proxy_hops: 0,
            trusted_proxy_cidrs: None,
        })
        .unwrap();
        Router::new()
            .route("/gated", get(|| async { "ok" }))
            .layer(middleware::from_fn_with_state(state.clone(), layer))
            .with_state(state)
    }

    #[tokio::test]
    async fn gates_on_the_bearer_token() {
        for (name, header_value) in [
            ("no header", None),
            ("wrong token", Some("Bearer nope")),
            (
                "wrong scheme",
                Some("Basic 0123456789abcdef0123456789abcdef"),
            ),
            ("empty token", Some("Bearer ")),
        ] {
            let mut req = Request::builder().uri("/gated");
            if let Some(h) = header_value {
                req = req.header(header::AUTHORIZATION, h);
            }
            let resp = app(Some(TOKEN.into()))
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{name}");
        }

        let req = Request::builder()
            .uri("/gated")
            .header(header::AUTHORIZATION, format!("bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap();
        let resp = app(Some(TOKEN.into())).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Mounted without a configured token, the layer fails CLOSED.
    #[tokio::test]
    async fn no_configured_token_fails_closed() {
        let req = Request::builder()
            .uri("/gated")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap();
        let resp = app(None).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    const PUSH: &str = "fedcba9876543210fedcba9876543210";

    async fn status(app: Router, bearer: Option<&str>) -> StatusCode {
        let mut req = Request::builder().uri("/gated");
        if let Some(b) = bearer {
            req = req.header(header::AUTHORIZATION, format!("Bearer {b}"));
        }
        app.oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    /// The split itself: the push layer takes either bearer, the full layer
    /// takes ONLY the full one. A push token reaching the drain is the hole this
    /// whole token exists to close.
    #[tokio::test]
    async fn the_push_token_opens_only_the_push_layer() {
        let push = || gated(Some(TOKEN.into()), Some(PUSH.into()), require_push_bearer);
        let full = || gated(Some(TOKEN.into()), Some(PUSH.into()), require_bearer);

        assert_eq!(status(push(), Some(TOKEN)).await, StatusCode::OK);
        assert_eq!(status(push(), Some(PUSH)).await, StatusCode::OK);
        assert_eq!(status(full(), Some(TOKEN)).await, StatusCode::OK);
        assert_eq!(
            status(full(), Some(PUSH)).await,
            StatusCode::UNAUTHORIZED,
            "the push-only bearer reached the full-token layer"
        );

        for wrong in [None, Some("nope"), Some("fedcba9876543210fedcba987654321X")] {
            assert_eq!(
                status(push(), wrong).await,
                StatusCode::UNAUTHORIZED,
                "{wrong:?}"
            );
            assert_eq!(
                status(full(), wrong).await,
                StatusCode::UNAUTHORIZED,
                "{wrong:?}"
            );
        }
    }

    /// With no push token configured the push layer is the full layer: same
    /// cases, same answers. This is the backward-compatibility claim for every
    /// relay that never sets `SQUELCH_RELAY_PUSH_TOKEN`.
    #[tokio::test]
    async fn without_a_push_token_the_push_layer_is_the_full_layer() {
        for bearer in [None, Some(TOKEN), Some(PUSH), Some("nope")] {
            let a = status(gated(Some(TOKEN.into()), None, require_push_bearer), bearer).await;
            let b = status(gated(Some(TOKEN.into()), None, require_bearer), bearer).await;
            assert_eq!(a, b, "{bearer:?}");
        }
        assert_eq!(
            status(
                gated(Some(TOKEN.into()), None, require_push_bearer),
                Some(TOKEN)
            )
            .await,
            StatusCode::OK
        );
    }

    /// Both layers fail CLOSED without a full token, even if a push token were
    /// somehow present (config refuses that combination at boot).
    #[tokio::test]
    async fn the_push_layer_fails_closed_without_a_full_token() {
        for bearer in [Some(TOKEN), Some(PUSH)] {
            assert_eq!(
                status(gated(None, Some(PUSH.into()), require_push_bearer), bearer).await,
                StatusCode::UNAUTHORIZED
            );
        }
    }
}
