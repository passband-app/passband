//! The agent door's OAuth mode, end to end: the real `agent_door_router`, a
//! real in-process JWKS server, and RS256 tokens signed with test-only keys.
//!
//! Every security test here was mutation-checked: the check it names was
//! removed from `src/auth.rs` (or `src/lib.rs`), the test was run and seen to
//! fail, and the check was restored.
//!
//! Keys and the stub JWKS server come from `squelch_mcp::testing` (the
//! `testing` feature), shared with squelchd's router tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use base64::Engine as _;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{Value, json};
use squelch_core::store::SqliteStore;
use squelch_mcp::auth::{Refusal, Verifier};
use squelch_mcp::testing::{
    KEY_A_N, KEY_A_PEM, KEY_A_PUB_PEM, KEY_B_N, KEY_B_PEM, KID_A as KID, KID_B, StubJwks, jwk,
    unix_now as now, valid_claims,
};
use squelch_mcp::{AgentDoorAuth, agent_door_router};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;

const ISSUER: &str = "https://test-tenant.authkit.app";
const PUBLIC_URL: &str = "https://alice.passband.email/mcp";
const PUBLIC_HOST: &str = "alice.passband.email";
const ACCOUNT: &str = "alice@example.com";
const METADATA_URL: &str = "https://alice.passband.email/.well-known/oauth-protected-resource/mcp";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn door(auth: &AgentDoorAuth) -> axum::Router {
    let store = Arc::new(SqliteStore::open_in_memory().expect("in-memory store"));
    store.ensure_account(ACCOUNT).expect("account");
    agent_door_router(
        store,
        ACCOUNT,
        Default::default(),
        Default::default(),
        CancellationToken::new(),
        auth,
    )
    .expect("router builds")
}

fn oauth_config(jwks_url: &str) -> squelch_mcp::auth::OAuthConfig {
    match AgentDoorAuth::from_values(Some(ISSUER), Some(PUBLIC_URL), Some(jwks_url)) {
        AgentDoorAuth::OAuth(c) => c,
        other => panic!("expected OAuth, got {other:?}"),
    }
}

fn oauth_door(jwks_url: &str) -> axum::Router {
    door(&AgentDoorAuth::OAuth(oauth_config(jwks_url)))
}

/// Claims that pass every check. Tests mutate one field at a time.
fn good_claims() -> Value {
    valid_claims(ISSUER, PUBLIC_URL, ACCOUNT)
}

fn sign_with(claims: &Value, pem: &str, kid: &str) -> String {
    squelch_mcp::testing::sign(claims, pem, kid)
}

fn sign(claims: &Value) -> String {
    sign_with(claims, KEY_A_PEM, KID)
}

/// An MCP `initialize`, shaped like the Passband app's probe
/// (`AgentConnect.swift`): JSON body, and the Accept header rmcp requires.
fn initialize(host: &str, bearer: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::HOST, host)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if let Some(t) = bearer {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    b.body(Body::from(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"passband-probe","version":"1"}}}"#,
    ))
    .unwrap()
}

async fn send(app: &axum::Router, req: Request<Body>) -> Response<Body> {
    app.clone().oneshot(req).await.unwrap()
}

async fn body_bytes(resp: Response<Body>) -> Vec<u8> {
    axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

/// A bad-token refusal: 401, the `invalid_token` challenge, and an empty body
/// that says nothing about which check failed.
async fn assert_invalid_token(app: &axum::Router, token: &str) {
    let resp = send(app, initialize(PUBLIC_HOST, Some(token))).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        resp.headers()[header::WWW_AUTHENTICATE],
        format!("Bearer resource_metadata=\"{METADATA_URL}\", error=\"invalid_token\"")
    );
    assert!(
        body_bytes(resp).await.is_empty(),
        "refusal body must be empty"
    );
}

async fn assert_passes(app: &axum::Router, token: &str) {
    let resp = send(app, initialize(PUBLIC_HOST, Some(token))).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a valid token must reach MCP"
    );
}

fn get(path: &str, host: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header(header::HOST, host)
        .body(Body::empty())
        .unwrap()
}

// ---------------------------------------------------------------------------
// I1: the mailbox binding. First, because it is the one that matters most.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_correctly_signed_token_for_another_mailbox_is_refused() {
    let jwks = StubJwks::start().await;
    let app = oauth_door(&jwks.url);

    // Control: the very same token shape with OUR email passes, so the only
    // difference below is whose mailbox the token names.
    assert_passes(&app, &sign(&good_claims())).await;

    let mut c = good_claims();
    c["email"] = json!("mallory@example.com");
    assert_invalid_token(&app, &sign(&c)).await;
}

#[tokio::test]
async fn a_token_with_no_email_claim_is_refused() {
    let jwks = StubJwks::start().await;
    let app = oauth_door(&jwks.url);
    let mut c = good_claims();
    c.as_object_mut().unwrap().remove("email");
    assert_invalid_token(&app, &sign(&c)).await;
}

#[tokio::test]
async fn the_email_compare_ignores_ascii_case_and_surrounding_whitespace() {
    let jwks = StubJwks::start().await;
    let app = oauth_door(&jwks.url);
    let mut c = good_claims();
    c["email"] = json!("  Alice@EXAMPLE.com ");
    assert_passes(&app, &sign(&c)).await;
}

// ---------------------------------------------------------------------------
// The happy path and the challenge
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_authorization_header_is_a_401_naming_the_resource_metadata() {
    let jwks = StubJwks::start().await;
    let app = oauth_door(&jwks.url);
    let resp = send(&app, initialize(PUBLIC_HOST, None)).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        resp.headers()[header::WWW_AUTHENTICATE],
        format!("Bearer resource_metadata=\"{METADATA_URL}\"")
    );
    assert!(body_bytes(resp).await.is_empty());
    assert_eq!(jwks.fetches(), 0, "no token must not drive a JWKS fetch");
}

#[tokio::test]
async fn a_valid_token_reaches_the_mcp_service() {
    let jwks = StubJwks::start().await;
    let app = oauth_door(&jwks.url);
    let resp = send(&app, initialize(PUBLIC_HOST, Some(&sign(&good_claims())))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = String::from_utf8(body_bytes(resp).await).unwrap();
    assert!(body.contains("\"protocolVersion\""), "{body}");
    assert_eq!(jwks.fetches(), 1);

    // The key is cached: a second request does not refetch.
    assert_passes(&app, &sign(&good_claims())).await;
    assert_eq!(jwks.fetches(), 1);
}

#[tokio::test]
async fn a_valid_token_with_a_foreign_host_header_is_still_403() {
    let jwks = StubJwks::start().await;
    let app = oauth_door(&jwks.url);
    let resp = send(
        &app,
        initialize("evil.example", Some(&sign(&good_claims()))),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// Claims
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_token_for_another_tenants_audience_is_refused() {
    let jwks = StubJwks::start().await;
    let app = oauth_door(&jwks.url);
    let mut c = good_claims();
    c["aud"] = json!("https://bob.passband.email/mcp");
    assert_invalid_token(&app, &sign(&c)).await;
}

#[tokio::test]
async fn a_token_with_no_audience_is_refused() {
    let jwks = StubJwks::start().await;
    let app = oauth_door(&jwks.url);
    let mut c = good_claims();
    c.as_object_mut().unwrap().remove("aud");
    assert_invalid_token(&app, &sign(&c)).await;
}

#[tokio::test]
async fn an_audience_array_containing_ours_passes_and_one_slash_is_forgiven() {
    let jwks = StubJwks::start().await;
    let app = oauth_door(&jwks.url);
    let mut c = good_claims();
    c["aud"] = json!(["https://bob.passband.email/mcp", PUBLIC_URL]);
    assert_passes(&app, &sign(&c)).await;

    c["aud"] = json!("https://alice.passband.email/mcp/");
    assert_passes(&app, &sign(&c)).await;

    c["aud"] = json!([
        "https://bob.passband.email/mcp",
        "https://carol.passband.email/mcp"
    ]);
    assert_invalid_token(&app, &sign(&c)).await;
}

#[tokio::test]
async fn a_token_from_another_issuer_is_refused() {
    let jwks = StubJwks::start().await;
    let app = oauth_door(&jwks.url);
    let mut c = good_claims();
    c["iss"] = json!("https://someone-else.authkit.app");
    assert_invalid_token(&app, &sign(&c)).await;

    c.as_object_mut().unwrap().remove("iss");
    assert_invalid_token(&app, &sign(&c)).await;

    // One trailing slash is forgiven, on either side.
    c["iss"] = json!(format!("{ISSUER}/"));
    assert_passes(&app, &sign(&c)).await;
}

#[tokio::test]
async fn an_expired_token_is_refused_past_the_leeway() {
    let jwks = StubJwks::start().await;
    let app = oauth_door(&jwks.url);
    let mut c = good_claims();
    c["exp"] = json!(now() - 120);
    assert_invalid_token(&app, &sign(&c)).await;

    // Inside the 60s leeway: still accepted.
    c["exp"] = json!(now() - 10);
    assert_passes(&app, &sign(&c)).await;
}

#[tokio::test]
async fn a_token_with_no_exp_is_refused() {
    let jwks = StubJwks::start().await;
    let app = oauth_door(&jwks.url);
    let mut c = good_claims();
    c.as_object_mut().unwrap().remove("exp");
    assert_invalid_token(&app, &sign(&c)).await;
}

#[tokio::test]
async fn a_token_whose_nbf_is_in_the_future_is_refused() {
    let jwks = StubJwks::start().await;
    let app = oauth_door(&jwks.url);
    let mut c = good_claims();
    c["nbf"] = json!(now() + 600);
    assert_invalid_token(&app, &sign(&c)).await;

    c["nbf"] = json!(now() - 5);
    assert_passes(&app, &sign(&c)).await;
}

// ---------------------------------------------------------------------------
// Keys and algorithms
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_token_signed_by_a_different_key_under_our_kid_is_refused() {
    let jwks = StubJwks::start().await;
    let app = oauth_door(&jwks.url);
    assert_invalid_token(&app, &sign_with(&good_claims(), KEY_B_PEM, KID)).await;
}

#[tokio::test]
async fn unknown_kids_are_refused_and_do_not_drive_repeated_jwks_fetches() {
    let jwks = StubJwks::start().await;
    let app = oauth_door(&jwks.url);

    assert_invalid_token(&app, &sign_with(&good_claims(), KEY_A_PEM, "random-kid-1")).await;
    assert_eq!(jwks.fetches(), 1, "the first miss may fetch once");

    assert_invalid_token(&app, &sign_with(&good_claims(), KEY_A_PEM, "random-kid-2")).await;
    assert_invalid_token(&app, &sign_with(&good_claims(), KEY_A_PEM, "random-kid-3")).await;
    assert_eq!(
        jwks.fetches(),
        1,
        "a second unknown kid inside the throttle window must not refetch"
    );

    // And the real kid still works off the cache.
    assert_passes(&app, &sign(&good_claims())).await;
    assert_eq!(jwks.fetches(), 1);
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[tokio::test]
async fn an_alg_none_token_is_refused_before_any_key_lookup() {
    let jwks = StubJwks::start().await;
    let app = oauth_door(&jwks.url);
    let header = json!({ "alg": "none", "typ": "JWT", "kid": KID });
    for sig in ["", "x"] {
        let token = format!(
            "{}.{}.{sig}",
            b64url(header.to_string().as_bytes()),
            b64url(good_claims().to_string().as_bytes())
        );
        assert_invalid_token(&app, &token).await;
    }
    assert_eq!(jwks.fetches(), 0);
}

#[tokio::test]
async fn an_hs256_token_keyed_with_our_public_key_is_refused_before_any_key_lookup() {
    let jwks = StubJwks::start().await;
    let app = oauth_door(&jwks.url);
    // Algorithm confusion: an attacker who knows the (public) RSA key signs
    // HS256 with its bytes as the HMAC secret, hoping the verifier feeds the
    // same bytes to HMAC.
    let mut h = Header::new(Algorithm::HS256);
    h.kid = Some(KID.to_string());
    for secret in [KEY_A_PUB_PEM.as_bytes(), KEY_A_N.as_bytes()] {
        let token =
            jsonwebtoken::encode(&h, &good_claims(), &EncodingKey::from_secret(secret)).unwrap();
        assert_invalid_token(&app, &token).await;
    }
    assert_eq!(
        jwks.fetches(),
        0,
        "a non-RS256 token must not reach the key cache"
    );
}

// ---------------------------------------------------------------------------
// I3 and resource bounds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn device_tokens_and_opaque_strings_are_not_agent_credentials() {
    let jwks = StubJwks::start().await;
    let app = oauth_door(&jwks.url);
    for t in [
        "sqd_0123456789abcdef0123456789abcdef0123456789abcdef",
        "test-token",
        "opaque-string-that-is-not-a-jwt",
        "a.b.c",
    ] {
        assert_invalid_token(&app, t).await;
    }
    assert_eq!(jwks.fetches(), 0);
}

#[tokio::test]
async fn a_token_over_8_kib_is_refused_without_a_jwks_fetch() {
    let jwks = StubJwks::start().await;
    let app = oauth_door(&jwks.url);
    // A token that is otherwise VALID, so the size cap is the only thing
    // standing between it and the service.
    let mut c = good_claims();
    c["pad"] = json!("x".repeat(9 * 1024));
    let token = sign(&c);
    assert!(token.len() > 8 * 1024);
    assert_invalid_token(&app, &token).await;
    assert_eq!(jwks.fetches(), 0);
}

#[tokio::test]
async fn an_unreachable_jwks_from_the_start_is_503_never_a_pass() {
    // Bind and drop: nothing listens on this port.
    let dead = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap()
    };
    let app = oauth_door(&format!("http://{dead}/oauth2/jwks"));
    for _ in 0..2 {
        let resp = send(&app, initialize(PUBLIC_HOST, Some(&sign(&good_claims())))).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(body_bytes(resp).await.is_empty());
    }
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_protected_resource_metadata_names_the_issuer_and_is_cors_open() {
    let jwks = StubJwks::start().await;
    let app = oauth_door(&jwks.url);
    for path in [
        "/.well-known/oauth-protected-resource/mcp",
        "/.well-known/oauth-protected-resource",
    ] {
        let resp = send(&app, get(path, PUBLIC_HOST)).await;
        assert_eq!(resp.status(), StatusCode::OK, "{path}");
        assert_eq!(resp.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
        assert!(
            resp.headers()[header::CACHE_CONTROL]
                .to_str()
                .unwrap()
                .contains("max-age=")
        );
        let body: Value = serde_json::from_slice(&body_bytes(resp).await).unwrap();
        assert_eq!(
            body,
            json!({
                "resource": PUBLIC_URL,
                "authorization_servers": [ISSUER],
                "bearer_methods_supported": ["header"],
                "resource_name": "Passband",
            })
        );
    }
    assert_eq!(jwks.fetches(), 0);
}

// ---------------------------------------------------------------------------
// Open mode (I4, I5)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn open_mode_serves_mcp_without_a_token_exactly_as_before() {
    let app = door(&AgentDoorAuth::Open);
    let resp = send(&app, initialize("localhost", None)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get(header::WWW_AUTHENTICATE).is_none());
}

#[tokio::test]
async fn open_mode_has_no_metadata_routes() {
    let app = door(&AgentDoorAuth::Open);
    for path in [
        "/.well-known/oauth-protected-resource/mcp",
        "/.well-known/oauth-protected-resource",
    ] {
        let resp = send(&app, get(path, "localhost")).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn open_mode_still_refuses_the_would_be_public_hostname() {
    // An OLD daemon image (or a new one with OAuth unset) behind a newly
    // published Ingress path must answer 403, not serve mail.
    let app = door(&AgentDoorAuth::Open);
    let resp = send(&app, initialize(PUBLIC_HOST, None)).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// Misconfigured (I2)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_misconfigured_door_answers_503_and_publishes_no_metadata() {
    let cases = [
        AgentDoorAuth::from_values(Some(ISSUER), None, None),
        AgentDoorAuth::from_values(None, Some(PUBLIC_URL), None),
        AgentDoorAuth::from_values(Some("not a url"), Some(PUBLIC_URL), None),
    ];
    for auth in &cases {
        assert!(matches!(auth, AgentDoorAuth::Misconfigured(_)), "{auth:?}");
        let app = door(auth);
        for host in ["localhost", PUBLIC_HOST] {
            let resp = send(&app, initialize(host, None)).await;
            assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE, "{auth:?}");
        }
        let resp = send(&app, get("/mcp/anything", "localhost")).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        for path in [
            "/.well-known/oauth-protected-resource/mcp",
            "/.well-known/oauth-protected-resource",
        ] {
            let resp = send(&app, get(path, "localhost")).await;
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }
}

// ---------------------------------------------------------------------------
// JWKS refresh behaviour, at the verifier with an injected clock
// ---------------------------------------------------------------------------

/// A clock that only moves when the test says so.
#[derive(Clone)]
struct FakeClock {
    base: Instant,
    offset_secs: Arc<AtomicU64>,
}

impl FakeClock {
    fn new() -> Self {
        Self {
            base: Instant::now(),
            offset_secs: Arc::new(AtomicU64::new(0)),
        }
    }
    fn set(&self, secs: u64) {
        self.offset_secs.store(secs, Ordering::SeqCst);
    }
    fn clock(&self) -> squelch_mcp::auth::Clock {
        let me = self.clone();
        Arc::new(move || me.base + Duration::from_secs(me.offset_secs.load(Ordering::SeqCst)))
    }
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

#[tokio::test]
async fn a_cached_key_is_served_while_a_slow_jwks_refresh_is_in_flight() {
    let jwks = StubJwks::start().await;
    let clock = FakeClock::new();
    let v = Arc::new(Verifier::with_clock(
        &oauth_config(&jwks.url),
        ACCOUNT,
        clock.clock(),
    ));

    // Warm the cache with key A.
    let good = bearer(&sign(&good_claims()));
    assert_eq!(v.verify(Some(&good)).await, Ok(()));
    assert_eq!(jwks.fetches(), 1);

    // Past the throttle window, an unknown kid starts a refresh that stalls.
    clock.set(61);
    jwks.set_delay(Duration::from_secs(3));
    let stranger = bearer(&sign_with(&good_claims(), KEY_A_PEM, "rotated-in-soon"));
    let slow = {
        let v = v.clone();
        tokio::spawn(async move { v.verify(Some(&stranger)).await })
    };
    // Wait until the stub has actually received that refresh.
    let deadline = Instant::now() + Duration::from_secs(2);
    while jwks.fetches() < 2 {
        assert!(Instant::now() < deadline, "the refresh never started");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // A request whose key IS cached must not queue behind it.
    let hit = tokio::time::timeout(Duration::from_millis(500), v.verify(Some(&good))).await;
    assert_eq!(
        hit,
        Ok(Ok(())),
        "a cache hit waited on the in-flight refresh"
    );
    assert!(!slow.is_finished(), "the refresh should still be in flight");

    assert_eq!(slow.await.unwrap(), Err(Refusal::UnknownKey));
    assert_eq!(jwks.fetches(), 2);
}

#[tokio::test]
async fn concurrent_misses_share_one_jwks_fetch() {
    let jwks = StubJwks::start().await;
    jwks.set_delay(Duration::from_millis(300));
    let v = Arc::new(Verifier::with_clock(
        &oauth_config(&jwks.url),
        ACCOUNT,
        FakeClock::new().clock(),
    ));
    let good = bearer(&sign(&good_claims()));
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let (v, t) = (v.clone(), good.clone());
            tokio::spawn(async move { v.verify(Some(&t)).await })
        })
        .collect();
    for t in tasks {
        assert_eq!(t.await.unwrap(), Ok(()));
    }
    assert_eq!(jwks.fetches(), 1);
}

#[tokio::test]
async fn an_unknown_kid_spray_cannot_lock_out_a_newly_published_key_past_the_window() {
    let jwks = StubJwks::start().await;
    let clock = FakeClock::new();
    let v = Verifier::with_clock(&oauth_config(&jwks.url), ACCOUNT, clock.clock());
    let spray = |n: u32| bearer(&sign_with(&good_claims(), KEY_A_PEM, &format!("spray-{n}")));

    // t=0: the attacker's first unknown kid spends the fetch.
    assert_eq!(v.verify(Some(&spray(0))).await, Err(Refusal::UnknownKey));
    assert_eq!(jwks.fetches(), 1);

    // The issuer rotates in key B right after.
    jwks.publish(json!({ "keys": [jwk(KID, KEY_A_N), jwk(KID_B, KEY_B_N)] }));
    let rotated = bearer(&sign_with(&good_claims(), KEY_B_PEM, KID_B));

    // Inside the window the new key waits, and the spray keeps going. Neither
    // the legitimate miss nor the spray may extend the window.
    clock.set(30);
    assert_eq!(v.verify(Some(&rotated)).await, Err(Refusal::UnknownKey));
    for (t, n) in [(45, 1), (55, 2), (59, 3)] {
        clock.set(t);
        assert_eq!(v.verify(Some(&spray(n))).await, Err(Refusal::UnknownKey));
    }
    assert_eq!(jwks.fetches(), 1, "throttled misses must not fetch");

    // One window after the LAST FETCH, the new key works.
    clock.set(60);
    assert_eq!(v.verify(Some(&rotated)).await, Ok(()));
    assert_eq!(jwks.fetches(), 2);
}
