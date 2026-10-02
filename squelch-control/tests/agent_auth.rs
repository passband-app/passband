//! THE AGENT HOP end to end, against a MOCK Google, a MOCK warden and a STUB
//! WorkOS.
//!
//! `GET /agent/auth` is WorkOS AuthKit's "Login URI" in Standalone Connect
//! mode: AuthKit runs an agent's OAuth flow and sends the person's browser here
//! with one opaque `external_auth_id`; this service proves who they are through
//! the app login's identity-only Google hop, finds their tenant from the mailbox
//! Google names, and only then tells WorkOS who it was. What that promises, and
//! therefore what is asserted here:
//!
//! - with WorkOS unconfigured the route does not exist (404),
//! - the ONLY input is `external_auth_id`, shape-checked before anything else,
//!   and a malformed one opens no session and reaches nobody,
//! - a well-formed one is a 302 to Google asking for identity alone, and the id
//!   is parked in the SERVER-SIDE session, never in the cookie,
//! - WorkOS is called exactly once, with our bearer and exactly the body the
//!   contract names, and only after Google has authenticated a mailbox that has
//!   an ACTIVE tenant here; a mailbox with none is told so and WorkOS hears
//!   nothing,
//! - the browser is sent to WorkOS's `redirect_uri` only when it is an absolute
//!   https URL; every WorkOS failure is one "try again" page that repeats
//!   nothing WorkOS said,
//! - the session kinds cannot be crossed: no console or app session can make a
//!   completion call, and an agent session cannot be finished as either,
//! - it shares the login hops' rate-limit bucket.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    body::to_bytes,
    extract::State as AxumState,
    http::{HeaderMap, Request, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use base64::Engine as _;
use serde_json::{Value, json};
use squelch_control::config::{Config, WorkosConfig};
use squelch_control::cookie::{self, SessionClaim};
use squelch_control::ratelimit::CONSOLE_AUTH_REQUESTS_PER_MINUTE;
use squelch_control::sessions::SessionKind;
use squelch_control::warden::HttpWarden;
use squelch_control::{ControlState, router};
use tower::ServiceExt as _;

/// The throwaway Postgres schema every harness in this file is built on.
mod common;

const ACCESS_TOKEN: &str = "ya29.THE-SECRET-ACCESS-TOKEN";
/// How the tenant row was written. The store folds it on insert, so what it
/// HOLDS is `ada@example.com`; the capitals are here so the test proves the
/// stored value, not this literal, is what WorkOS is told.
const STORED_AS: &str = "Ada@Example.com";
const STORED: &str = "ada@example.com";
const LABEL: &str = "ada";
const PAIR_CODE: &str = "ABCD-EFGH";
const COOKIE_KEY: [u8; 32] = [42; 32];
/// An AuthKit-shaped id (a ULID).
const EXTERNAL_AUTH_ID: &str = "01J3X4Y5Z6A7B8C9D0E1F2G3H4";
const WORKOS_KEY: &str = "sk_test_THE-WORKOS-API-KEY";
/// What the stub WorkOS answers a completion with, by default.
const REDIRECT: &str = "https://foo.authkit.app/oauth2/authorize?state=authkit-state-xyz";
/// A marker the stub puts in every non-default answer, so a test can prove no
/// part of WorkOS's body reached the browser.
const WORKOS_MARKER: &str = "WORKOS-BODY-MARKER";

#[derive(Default)]
struct Recorder {
    /// Who the mock Google says is signed in.
    signed_in_as: Option<String>,
    /// `email_verified`, as OIDC reports it.
    email_verified: Option<Value>,
    /// Token-exchange calls Google saw.
    exchanges: usize,
    /// Labels the warden was asked to pair.
    pair_calls: Vec<String>,
    /// Every completion WorkOS saw: its Authorization header and JSON body.
    completions: Vec<(String, Value)>,
    /// When set, WorkOS answers with this status and raw body.
    workos_answer: Option<(u16, String)>,
    /// When set, WorkOS sleeps this long before answering.
    workos_delay: Option<Duration>,
}

type Shared = Arc<Mutex<Recorder>>;

async fn spawn(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    format!("http://{addr}")
}

/// A mock Google: the token endpoint and OIDC userinfo, nothing else.
async fn spawn_google(rec: Shared) -> String {
    let app = Router::new()
        .route(
            "/token",
            post(|AxumState(rec): AxumState<Shared>| async move {
                rec.lock().unwrap().exchanges += 1;
                Json(json!({
                    "access_token": ACCESS_TOKEN,
                    "expires_in": 3599,
                    "token_type": "Bearer",
                    "scope": "openid https://www.googleapis.com/auth/userinfo.email",
                }))
            }),
        )
        .route(
            "/userinfo",
            get(|AxumState(rec): AxumState<Shared>| async move {
                let r = rec.lock().unwrap();
                let mut answer = serde_json::Map::new();
                answer.insert("sub".into(), json!("1234567890"));
                answer.insert("email".into(), json!(r.signed_in_as.clone().unwrap()));
                if let Some(v) = &r.email_verified {
                    answer.insert("email_verified".into(), v.clone());
                }
                Json(Value::Object(answer))
            }),
        )
        .with_state(rec);
    spawn(app).await
}

/// A mock warden with the pair route the console and app logins use, so this
/// file can prove those flows never reach WorkOS.
async fn spawn_warden(rec: Shared) -> String {
    let app = Router::new()
        .route(
            "/v1/tenants/{label}/pair",
            post(
                |AxumState(rec): AxumState<Shared>,
                 axum::extract::Path(label): axum::extract::Path<String>| async move {
                    rec.lock().unwrap().pair_calls.push(label.clone());
                    Json(json!({
                        "pair_code": PAIR_CODE,
                        "pair_url": format!("https://{label}.passband.test"),
                        "deep_link": format!("passband://pair?url=x&code={PAIR_CODE}"),
                    }))
                },
            ),
        )
        .with_state(rec);
    spawn(app).await
}

/// The stub WorkOS: one route, recording everything it is sent.
async fn spawn_workos(rec: Shared) -> String {
    let app = Router::new()
        .route(
            "/authkit/oauth2/complete",
            post(
                |AxumState(rec): AxumState<Shared>, headers: HeaderMap, body: String| async move {
                    let (answer, delay) = {
                        let mut r = rec.lock().unwrap();
                        r.completions.push((
                            headers
                                .get(header::AUTHORIZATION)
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or_default()
                                .to_string(),
                            serde_json::from_str(&body).unwrap_or(Value::Null),
                        ));
                        (r.workos_answer.clone(), r.workos_delay)
                    };
                    if let Some(d) = delay {
                        tokio::time::sleep(d).await;
                    }
                    match answer {
                        Some((status, body)) => (
                            StatusCode::from_u16(status).unwrap(),
                            [(header::CONTENT_TYPE, "application/json")],
                            body,
                        )
                            .into_response(),
                        None => Json(json!({ "redirect_uri": REDIRECT })).into_response(),
                    }
                },
            ),
        )
        .with_state(rec);
    spawn(app).await
}

struct Harness {
    app: Router,
    /// A handle on the SAME state the router serves, so a test can look inside
    /// the session table.
    state: ControlState,
    rec: Shared,
    /// This deployment's own origin: what a same-origin POST says it is from.
    public_url: String,
}

impl Harness {
    /// The agent door ON, on an https deployment.
    async fn new() -> Self {
        Self::build(true, "https://signup.passband.test").await
    }

    async fn build(workos_on: bool, public_url: &str) -> Self {
        let rec: Shared = Arc::new(Mutex::new(Recorder {
            // Google answers in another case than the row was written in: the
            // lookup must still find it, and `user.id` must still be folded.
            signed_in_as: Some("ADA@Example.COM".into()),
            email_verified: Some(Value::Bool(true)),
            ..Recorder::default()
        }));
        let google = spawn_google(rec.clone()).await;
        let warden_url = spawn_warden(rec.clone()).await;
        let workos_url = spawn_workos(rec.clone()).await;

        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            public_url: public_url.into(),
            redirect_uri: format!("{public_url}/oauth/callback"),
            base_domain: "passband.test".into(),
            client_id: "test-client-id".into(),
            client_secret: "test-client-secret".into(),
            cookie_key: COOKIE_KEY.to_vec(),
            warden_url: warden_url.clone(),
            warden_token: "warden-bearer".into(),
            database_url: "postgres://unused".into(),
            trusted_proxy_hops: 0,
            token_url: format!("{google}/token"),
            auth_url: format!("{google}/authorize"),
            profile_url: format!("{google}/profile"),
            userinfo_url: format!("{google}/userinfo"),
            alert_webhook_url: None,
            bifrost: None,
            waitlist: None,
            workos: workos_on.then(|| WorkosConfig {
                api_key: WORKOS_KEY.into(),
                api_url: workos_url,
            }),
        };

        let (store, _url) = common::fresh_store().await;
        store.insert_tenant(LABEL, STORED_AS).await.unwrap();

        let warden = Arc::new(
            HttpWarden::new(warden_url, "warden-bearer".into(), Duration::from_secs(5)).unwrap(),
        );
        let state = ControlState::new(config, store, warden).unwrap();
        Self {
            app: router(state.clone()),
            state,
            rec,
            public_url: public_url.to_string(),
        }
    }

    async fn get(&self, uri: &str, cookie: Option<&str>) -> (StatusCode, HeaderMap, String) {
        let mut req = Request::builder().method("GET").uri(uri);
        if let Some(c) = cookie {
            req = req.header(header::COOKIE, c);
        }
        let resp = self
            .app
            .clone()
            .oneshot(req.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = String::from_utf8_lossy(&to_bytes(resp.into_body(), 1 << 20).await.unwrap())
            .to_string();
        (status, headers, body)
    }

    /// Start a login at `uri` and return `(consent url, cookie pair)`.
    async fn start(&self, uri: &str) -> (String, String) {
        let (status, headers, body) = self.get(uri, None).await;
        assert_eq!(status, StatusCode::FOUND, "{uri}: {body}");
        let location = headers[header::LOCATION].to_str().unwrap().to_string();
        let cookie = headers[header::SET_COOKIE].to_str().unwrap().to_string();
        (location, cookie.split(';').next().unwrap().to_string())
    }

    async fn start_agent(&self) -> (String, String) {
        self.start(&format!("/agent/auth?external_auth_id={EXTERNAL_AUTH_ID}"))
            .await
    }

    /// Come back from Google with the `state` this consent carried.
    async fn finish(&self, consent: &str, cookie: &str) -> (StatusCode, HeaderMap, String) {
        self.get(
            &format!(
                "/oauth/callback?code=the-auth-code&state={}",
                query_param(consent, "state")
            ),
            Some(cookie),
        )
        .await
    }

    async fn run_agent(&self) -> (StatusCode, HeaderMap, String) {
        let (consent, cookie) = self.start_agent().await;
        self.finish(&consent, &cookie).await
    }

    /// A form POST with whatever cookie and origin headers the test chooses.
    async fn post(
        &self,
        uri: &str,
        cookie: Option<&str>,
        origin: Option<&str>,
        body: String,
    ) -> (StatusCode, HeaderMap, String) {
        let mut req = Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        if let Some(c) = cookie {
            req = req.header(header::COOKIE, c);
        }
        if let Some(o) = origin {
            req = req.header(header::ORIGIN, o);
        }
        let resp = self
            .app
            .clone()
            .oneshot(req.body(axum::body::Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = String::from_utf8_lossy(&to_bytes(resp.into_body(), 1 << 20).await.unwrap())
            .to_string();
        (status, headers, body)
    }

    /// Walk the agent hop through Google, follow the callback's 303, and
    /// return `(fresh cookie pair, csrf token, page body)`.
    async fn reach_confirm(&self) -> (String, String, String) {
        let (status, headers, body) = self.run_agent().await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
        assert_eq!(headers[header::LOCATION], "/agent/confirm");
        let cookie = fresh_cookie(&headers);
        let (status, _, page) = self.get("/agent/confirm", Some(&cookie)).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        (cookie, csrf_of(&page), page)
    }

    /// Press a button on the confirmation page, from this origin.
    async fn decide(
        &self,
        cookie: &str,
        csrf: &str,
        decision: &str,
    ) -> (StatusCode, HeaderMap, String) {
        let origin = self.public_url.clone();
        self.post(
            "/agent/confirm",
            Some(cookie),
            Some(&origin),
            format!("csrf={csrf}&decision={decision}"),
        )
        .await
    }

    /// The whole flow: Google, the confirmation page, Continue.
    async fn run_agent_through(&self) -> (StatusCode, HeaderMap, String) {
        let (cookie, csrf, _) = self.reach_confirm().await;
        self.decide(&cookie, &csrf, "continue").await
    }

    fn completions(&self) -> Vec<(String, Value)> {
        self.rec.lock().unwrap().completions.clone()
    }
}

fn query_param(url: &str, name: &str) -> String {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

/// The flow cookie an answer SETS (not the one it clears), as a cookie pair.
fn fresh_cookie(headers: &HeaderMap) -> String {
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap())
        .find(|c| c.starts_with("passband_signup=") && !c.contains("Max-Age=0"))
        .expect("a fresh flow cookie")
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

/// The CSRF token on a confirmation page. Base64url, so it needs no decoding.
fn csrf_of(body: &str) -> String {
    let (_, rest) = body
        .split_once(r#"name="csrf" value=""#)
        .expect("a csrf field");
    rest.split('"').next().unwrap().to_string()
}

/// The decoded JSON payload of a `passband_signup=<value>` cookie pair.
fn cookie_payload(pair: &str) -> Value {
    let value = pair.split_once('=').unwrap().1;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value.split_once('.').unwrap().0)
        .unwrap();
    serde_json::from_slice(&payload).unwrap()
}

/// Every `Set-Cookie` on a response, joined, for "was the session cleared".
fn set_cookies(headers: &HeaderMap) -> String {
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect::<Vec<_>>()
        .join("|")
}

/// The page a reader can see, without the shell's stylesheet (whose comments
/// would otherwise answer every "must not appear" assertion).
fn visible(body: &str) -> &str {
    body.split_once("</style>").map_or(body, |(_, v)| v)
}

// ---- the door ---------------------------------------------------------------

/// With WorkOS unconfigured there is no door at all: a 404, not a refusal page,
/// and no session opened, whatever is presented.
#[tokio::test]
async fn the_agent_door_does_not_exist_without_workos() {
    let h = Harness::build(false, "https://signup.passband.test").await;
    for uri in [
        format!("/agent/auth?external_auth_id={EXTERNAL_AUTH_ID}"),
        "/agent/auth".to_string(),
    ] {
        let (status, headers, _) = h.get(&uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert!(headers.get(header::SET_COOKIE).is_none(), "{uri}");
    }
    assert_eq!(h.state.live_sessions(), 0);
}

/// THE ONE INPUT IS SHAPE-CHECKED FIRST, and every way it can be wrong is the
/// same page, opens no session, sets no cookie, and reaches neither Google nor
/// WorkOS.
#[tokio::test]
async fn a_malformed_external_auth_id_is_refused_and_opens_nothing() {
    let h = Harness::new().await;
    let overlong = "a".repeat(129);
    let mut answers = Vec::new();
    // TEN CASES AT MOST: this route shares a bucket of that size, and the
    // eleventh would be a 429 rather than the refusal under test.
    for query in [
        "",                                       // no parameter at all
        "?external_auth_id=",                     // empty
        "?other=01J3X4Y5Z6A7B8C9D0E1F2G3H4",      // the wrong parameter
        &format!("?external_auth_id={overlong}"), // one over the ceiling
        "?external_auth_id=has%20space",
        "?external_auth_id=a%2Fb",
        "?external_auth_id=%22%7D%2C%22user%22", // JSON injection
        "?external_auth_id=line%0Abreak",
        "?external_auth_id=caf%C3%A9",
        "?external_auth_id=https%3A%2F%2Fevil.test",
    ] {
        let (status, headers, body) = h.get(&format!("/agent/auth{query}"), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {body}");
        assert!(headers.get(header::LOCATION).is_none(), "{query}");
        assert!(
            headers.get(header::SET_COOKIE).is_none(),
            "{query}: a refused login opens no session"
        );
        answers.push(body);
    }
    for (i, body) in answers.iter().enumerate() {
        assert_eq!(body, &answers[0], "refusal {i} differs from the others");
    }
    assert!(
        answers[0].contains("could not be completed"),
        "{}",
        answers[0]
    );
    assert!(!visible(&answers[0]).contains('\u{2014}'), "no em dashes");

    assert_eq!(h.state.live_sessions(), 0, "no session was opened");
    let rec = h.rec.lock().unwrap();
    assert_eq!(rec.exchanges, 0, "nothing reached Google");
    assert!(rec.completions.is_empty(), "nor WorkOS");
}

/// A well-formed id is a 302 to Google asking for IDENTITY ONLY, and the id is
/// parked in the server-side session, of the agent kind, and NOT in the cookie.
#[tokio::test]
async fn a_well_formed_id_starts_the_google_hop_with_the_id_held_server_side() {
    let h = Harness::new().await;
    let (consent, cookie) = h.start_agent().await;

    assert!(consent.contains("/authorize"), "{consent}");
    let scope = query_param(&consent, "scope");
    assert_eq!(
        scope.split(' ').collect::<Vec<_>>(),
        vec!["openid", "email"]
    );
    assert!(!consent.contains("access_type"), "{consent}");
    assert_eq!(query_param(&consent, "code_challenge_method"), "S256");
    // The id goes to Google in no form: Google has no business with it.
    assert!(!consent.contains(EXTERNAL_AUTH_ID), "{consent}");

    // The cookie carries the agent MARKER and nothing of the id.
    let payload = cookie_payload(&cookie);
    assert_eq!(payload["agent"], Value::Bool(true), "{payload}");
    assert_eq!(payload["label"], "", "{payload}");
    assert!(payload.get("invite").is_none(), "{payload}");
    assert!(!payload.to_string().contains(EXTERNAL_AUTH_ID), "{payload}");
    assert!(!cookie.contains(EXTERNAL_AUTH_ID));

    // The session table holds the id, under the session id the cookie names.
    assert_eq!(h.state.live_sessions(), 1);
    let sid = payload["sid"].as_str().unwrap();
    let session = h
        .state
        .sessions()
        .take(sid, Instant::now())
        .expect("a live session");
    assert_eq!(
        session.kind,
        SessionKind::Agent {
            external_auth_id: EXTERNAL_AUTH_ID.to_string()
        }
    );
    assert_eq!(session.label, "");
    assert!(
        h.completions().is_empty(),
        "nothing reaches WorkOS at the door"
    );
}

// ---- the callback -----------------------------------------------------------

/// THE HAPPY PATH: exactly one completion, with our bearer and EXACTLY the body
/// the contract names, and the browser sent to the URL WorkOS answered with.
///
/// `user.id` is Google's answer folded to lowercase; `user.email` is the tenant
/// row as stored. Google answers `ADA@Example.COM` here, so both halves are
/// tested rather than coinciding.
#[tokio::test]
async fn an_agent_login_completes_at_workos_and_follows_its_redirect() {
    let h = Harness::new().await;
    let (status, headers, body) = h.run_agent_through().await;
    // 303: the answer to a POST is a GET of AuthKit's URL.
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    assert_eq!(headers[header::LOCATION].to_str().unwrap(), REDIRECT);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store, no-cache");
    assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
    // The flow cookie is NOT cleared on success: a second press already in
    // flight must still carry it to be matched to the done marker.
    assert!(!set_cookies(&headers).contains("Max-Age=0"), "{headers:?}");

    let completions = h.completions();
    assert_eq!(completions.len(), 1, "exactly one completion");
    let (auth, sent) = &completions[0];
    assert_eq!(auth, &format!("Bearer {WORKOS_KEY}"));
    assert_eq!(
        sent,
        &json!({
            "external_auth_id": EXTERNAL_AUTH_ID,
            "user": { "id": "ada@example.com", "email": STORED },
        })
    );

    // Nothing was paired: an agent login mints no device credential.
    assert!(h.rec.lock().unwrap().pair_calls.is_empty());
    // The confirmation is spent; only the seconds-long done marker remains.
    assert_eq!(h.state.live_sessions(), 1);
}

/// A Google account with NO mailbox here is told so plainly, after Google, and
/// WorkOS hears nothing: no token is ever issued for an address this service
/// does not run.
#[tokio::test]
async fn a_mailbox_with_no_tenant_is_refused_and_workos_is_never_called() {
    let h = Harness::new().await;
    h.rec.lock().unwrap().signed_in_as = Some("grace@example.com".into());

    let (status, headers, body) = h.run_agent().await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(headers.get(header::LOCATION).is_none());
    let seen = visible(&body);
    assert!(seen.contains("No Passband mailbox"), "{body}");
    assert!(seen.contains("nothing was connected"), "{body}");
    assert!(!seen.contains("grace@example.com"), "{body}");
    assert!(!seen.contains('\u{2014}'), "no em dashes");
    assert!(set_cookies(&headers).contains("Max-Age=0"));

    assert!(h.completions().is_empty(), "WorkOS was called");
    assert!(h.rec.lock().unwrap().pair_calls.is_empty());
}

/// An address Google will not vouch for is not an identity: refused, and
/// WorkOS hears nothing.
#[tokio::test]
async fn an_unvouched_google_answer_never_reaches_workos() {
    for verified in [None, Some(Value::Bool(false))] {
        let h = Harness::new().await;
        h.rec.lock().unwrap().email_verified = verified.clone();
        let (status, headers, body) = h.run_agent().await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{verified:?}: {body}");
        assert!(headers.get(header::LOCATION).is_none());
        assert!(h.completions().is_empty(), "{verified:?}");
    }
}

/// EVERY WORKOS FAILURE IS A PAGE OF OURS, in one of two words: AuthKit
/// refusing THIS authorization (a 4xx other than 401/403: a spent or expired
/// `external_auth_id`) says the request expired and to start again from the
/// agent, because trying again here can never work; everything else (a key
/// problem, an outage, an unreadable answer, a `redirect_uri` that is not an
/// absolute https URL) says try again. No browser is sent anywhere, and nothing
/// WorkOS said reaches the page.
#[tokio::test]
async fn every_workos_failure_is_one_of_our_pages() {
    let redirect = |uri: &str| json!({ "redirect_uri": uri, "note": WORKOS_MARKER }).to_string();
    // A REFUSAL THAT STILL CARRIES A PLAUSIBLE URL. The non-2xx cases answer
    // with a perfectly acceptable https `redirect_uri`, so what is under test
    // is that the STATUS decides, not whether the body happens to parse.
    let refused_with_redirect =
        || json!({ "redirect_uri": REDIRECT, "message": WORKOS_MARKER }).to_string();
    // Answers that mean "this authorization is gone": the EXPIRED page.
    for (name, status) in [("400", 400), ("404", 404), ("409", 409)] {
        let h = Harness::new().await;
        h.rec.lock().unwrap().workos_answer = Some((status, refused_with_redirect()));
        let (got, headers, body) = h.run_agent_through().await;
        assert_eq!(got, StatusCode::BAD_REQUEST, "{name}: {body}");
        assert!(headers.get(header::LOCATION).is_none(), "{name}");
        let seen = visible(&body);
        assert!(seen.contains("has expired"), "{name}: {body}");
        assert!(seen.contains("start connecting again"), "{name}: {body}");
        assert!(
            !seen.contains("try again in a few minutes"),
            "{name}: {body}"
        );
        assert!(!body.contains(WORKOS_MARKER), "{name}: {body}");
        assert!(!seen.contains('\u{2014}'), "{name}: no em dashes");
        assert!(set_cookies(&headers).contains("Max-Age=0"), "{name}");
        assert_eq!(h.completions().len(), 1, "{name}: one attempt, no retry");
    }

    // Everything else: the UNAVAILABLE page.
    let cases: Vec<(&str, u16, String)> = vec![
        ("401", 401, refused_with_redirect()),
        ("403", 403, refused_with_redirect()),
        ("429", 429, refused_with_redirect()),
        ("500", 500, refused_with_redirect()),
        ("503", 503, refused_with_redirect()),
        ("malformed json", 200, format!("{{not json {WORKOS_MARKER}")),
        (
            "no redirect_uri",
            200,
            json!({ "message": WORKOS_MARKER }).to_string(),
        ),
        ("javascript:", 200, redirect("javascript:alert(1)")),
        ("plain http", 200, redirect("http://foo.authkit.app/x")),
        ("relative", 200, redirect("/oauth2/authorize")),
        ("scheme-relative", 200, redirect("//evil.test/x")),
        (
            "userinfo",
            200,
            redirect("https://foo.authkit.app@evil.test/x"),
        ),
    ];
    for (name, status, answer) in cases {
        let h = Harness::new().await;
        h.rec.lock().unwrap().workos_answer = Some((status, answer));

        let (got, headers, body) = h.run_agent_through().await;
        assert_eq!(got, StatusCode::BAD_GATEWAY, "{name}: {body}");
        assert!(headers.get(header::LOCATION).is_none(), "{name}");
        assert!(body.contains("try again"), "{name}: {body}");
        assert!(!body.contains("has expired"), "{name}: {body}");
        assert!(!body.contains(WORKOS_MARKER), "{name}: {body}");
        assert!(!body.contains("evil.test"), "{name}: {body}");
        assert!(!body.contains("javascript"), "{name}: {body}");
        assert!(!body.contains(WORKOS_KEY), "{name}: {body}");
        assert!(set_cookies(&headers).contains("Max-Age=0"), "{name}");
        assert_eq!(h.completions().len(), 1, "{name}: one attempt, no retry");
    }
}

/// A WorkOS that does not answer within the budget is the same page.
#[tokio::test]
async fn a_slow_workos_is_the_unavailable_page() {
    let h = Harness::new().await;
    h.rec.lock().unwrap().workos_delay = Some(Duration::from_secs(12));
    let (cookie, csrf, _) = h.reach_confirm().await;
    let started = Instant::now();
    let (status, headers, body) = h.decide(&cookie, &csrf, "continue").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(headers.get(header::LOCATION).is_none());
    // The ten-second budget, not the stub's twelve.
    assert!(
        started.elapsed() < Duration::from_secs(12),
        "{:?}",
        started.elapsed()
    );
}

/// Plain http is a local development affordance: on a deployment that is
/// itself served over http, an http `redirect_uri` is followed.
#[tokio::test]
async fn an_insecure_dev_run_follows_a_plain_http_redirect() {
    let h = Harness::build(true, "http://127.0.0.1:8852").await;
    let local = "http://127.0.0.1:9999/oauth2/authorize?x=1";
    h.rec.lock().unwrap().workos_answer = Some((200, json!({ "redirect_uri": local }).to_string()));
    let (status, headers, body) = h.run_agent_through().await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    assert_eq!(headers[header::LOCATION].to_str().unwrap(), local);
}

/// One-shot, like every callback: a replay finds nothing, no second
/// confirmation is parked, and WorkOS hears nothing.
#[tokio::test]
async fn a_replayed_agent_callback_finds_nothing() {
    let h = Harness::new().await;
    let (consent, cookie) = h.start_agent().await;
    let (first, _, _) = h.finish(&consent, &cookie).await;
    assert_eq!(first, StatusCode::SEE_OTHER);
    // Replayed with the PRE-Google cookie, whose session the first spent.
    let (second, headers, body) = h.finish(&consent, &cookie).await;
    assert_eq!(second, StatusCode::BAD_REQUEST, "{body}");
    assert!(headers.get(header::LOCATION).is_none());
    // In the agent flow's own words, and offering no signup form.
    let seen = visible(&body);
    assert!(seen.contains("Go back to your agent"), "{body}");
    assert!(!seen.contains(r#"href="/""#), "{body}");
    assert!(h.completions().is_empty());
    assert_eq!(h.state.live_sessions(), 1, "only the first confirmation");
}

// ---- the kinds cannot be crossed ---------------------------------------------

/// The other login kinds never make a completion call: a console login and an
/// app login each finish exactly as they always have, and WorkOS hears nothing.
#[tokio::test]
async fn console_and_app_logins_never_reach_workos() {
    let h = Harness::new().await;
    h.rec.lock().unwrap().signed_in_as = Some(STORED.into());

    let (consent, cookie) = h.start(&format!("/console/auth?tenant={LABEL}")).await;
    let (status, headers, _) = h.finish(&consent, &cookie).await;
    assert_eq!(status, StatusCode::FOUND);
    assert_eq!(
        headers[header::LOCATION].to_str().unwrap(),
        format!("https://{LABEL}.passband.test/console/callback?code={PAIR_CODE}")
    );

    let (consent, cookie) = h.start("/app/auth").await;
    let (status, _, body) = h.finish(&consent, &cookie).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("passband://pair"), "{body}");

    assert_eq!(h.rec.lock().unwrap().pair_calls.len(), 2);
    assert!(
        h.completions().is_empty(),
        "a non-agent session reached WorkOS"
    );
}

/// Forge a cookie with a VALID MAC for a live session's id but a different
/// flow marker. The MAC cannot rule this out; the callback's kind check must.
fn forged(sid: &str, app: bool, agent: bool) -> String {
    let value = cookie::sign(
        &COOKIE_KEY,
        &SessionClaim {
            sid: sid.to_string(),
            label: String::new(),
            invite: None,
            app,
            reconnect: false,
            agent,
            iat: chrono::Utc::now().timestamp(),
        },
    );
    format!("passband_signup={value}")
}

/// AN AGENT SESSION CANNOT BE FINISHED AS AN APP LOGIN. A cookie that claims
/// "app" for an agent session is refused before Google is even asked, so no
/// pairing code is minted and no completion is made.
#[tokio::test]
async fn an_agent_session_cannot_be_finished_as_an_app_login() {
    let h = Harness::new().await;
    let (consent, cookie) = h.start_agent().await;
    let sid = cookie_payload(&cookie)["sid"].as_str().unwrap().to_string();

    for (app, agent) in [(true, false), (false, false)] {
        // (false, false) is a CONSOLE claim with an empty label.
        let (status, headers, body) = h.finish(&consent, &forged(&sid, app, agent)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{app}/{agent}: {body}");
        assert!(headers.get(header::LOCATION).is_none());
        assert!(!body.contains("passband://"), "{body}");
    }
    let rec = h.rec.lock().unwrap();
    assert_eq!(rec.exchanges, 0, "refused before Google");
    assert!(rec.pair_calls.is_empty());
    assert!(rec.completions.is_empty());
}

/// AN APP SESSION CANNOT BE FINISHED AS AN AGENT LOGIN: a cookie claiming
/// "agent" for an app session is refused, so no app session can ever be turned
/// into a completion call.
#[tokio::test]
async fn an_app_session_cannot_be_finished_as_an_agent_login() {
    let h = Harness::new().await;
    let (consent, cookie) = h.start("/app/auth").await;
    let sid = cookie_payload(&cookie)["sid"].as_str().unwrap().to_string();

    let (status, headers, body) = h.finish(&consent, &forged(&sid, false, true)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(headers.get(header::LOCATION).is_none());
    let rec = h.rec.lock().unwrap();
    assert_eq!(rec.exchanges, 0);
    assert!(rec.pair_calls.is_empty());
    assert!(rec.completions.is_empty());
}

/// ONE BUDGET FOR EVERY LOGIN HOP. Alternating `/app/auth` and `/agent/auth`
/// buys no second allowance.
#[tokio::test]
async fn the_agent_hop_shares_the_login_budget() {
    let h = Harness::new().await;
    let capacity = CONSOLE_AUTH_REQUESTS_PER_MINUTE as usize;
    let mut refused = 0;
    for i in 0..(capacity + 5) {
        let uri = if i % 2 == 0 {
            format!("/agent/auth?external_auth_id={EXTERNAL_AUTH_ID}")
        } else {
            "/app/auth".to_string()
        };
        let (status, _, _) = h.get(&uri, None).await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            refused += 1;
        }
    }
    assert_eq!(refused, 5, "the shared bucket holds one stated capacity");
}

// ---- the confirmation step ----------------------------------------------------

/// THE CALLBACK NO LONGER TALKS TO WORKOS. It parks the verified result under
/// a FRESH session id and cookie and answers a 303 to the stable
/// `GET /agent/confirm`, which renders OUR page naming the mailbox, with zero
/// completions made; the page is uncacheable, unframeable, and free of em
/// dashes.
#[tokio::test]
async fn the_callback_shows_a_confirmation_and_tells_workos_nothing() {
    let h = Harness::new().await;
    let (consent, pre_google) = h.start_agent().await;
    let (status, redirect, body) = h.finish(&consent, &pre_google).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    // A fixed path with no query: Google's code does not stay in the address bar.
    assert_eq!(redirect[header::LOCATION], "/agent/confirm");
    assert_eq!(redirect[header::CACHE_CONTROL], "no-store, no-cache");
    assert!(
        !body.contains(STORED),
        "the callback itself renders nothing"
    );
    assert!(
        h.completions().is_empty(),
        "WorkOS was called before the person agreed"
    );
    let fresh = fresh_cookie(&redirect);
    let (status, headers, body) = h.get("/agent/confirm", Some(&fresh)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        headers.get(header::SET_COOKIE).is_none(),
        "the page sets and clears nothing"
    );

    let seen = visible(&body);
    // The mailbox as the record stores it, so the person sees WHICH account.
    assert!(
        seen.contains(&format!("<strong>{STORED}</strong>")),
        "{body}"
    );
    assert!(
        seen.contains("An AI agent is asking to read the mail in"),
        "{body}"
    );
    assert!(
        seen.contains("If someone sent you a link to this page, close it."),
        "{body}"
    );
    assert!(seen.contains("chosen by whoever built it"), "{body}");
    assert!(seen.contains(r#"action="/agent/confirm""#), "{body}");
    assert!(seen.contains(r#"value="continue""#), "{body}");
    assert!(seen.contains(r#"value="cancel""#), "{body}");
    assert!(!seen.contains('\u{2014}'), "no em dashes");
    assert!(!seen.contains("<script"), "no javascript");
    assert!(
        !body.contains(EXTERNAL_AUTH_ID),
        "the id is not on the page"
    );

    assert_eq!(headers[header::CACHE_CONTROL], "no-store, no-cache");
    assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
    assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
    let csp = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
    assert!(csp.contains("default-src 'none'"), "{csp}");

    // A FRESH session: the cookie names a different id from the one that went
    // to Google, the old one is gone, and the new one holds the verified result.
    assert!(
        !set_cookies(&redirect).contains("passband_signup=; "),
        "the fresh cookie must not be cleared in the same answer"
    );
    let (cookie, csrf) = (fresh, csrf_of(&body));
    let old_sid = cookie_payload(&pre_google)["sid"]
        .as_str()
        .unwrap()
        .to_string();
    let new_sid = cookie_payload(&cookie)["sid"].as_str().unwrap().to_string();
    assert_ne!(old_sid, new_sid);
    assert!(!cookie.contains(EXTERNAL_AUTH_ID));
    assert_eq!(h.state.live_sessions(), 1);
    let parked = h
        .state
        .sessions()
        .take(&new_sid, Instant::now())
        .expect("the parked confirmation");
    assert_eq!(
        parked.kind,
        SessionKind::AgentConfirm {
            external_auth_id: EXTERNAL_AUTH_ID.into(),
            stored_email: STORED.into(),
            user_id: "ada@example.com".into(),
        }
    );
    assert_eq!(parked.state, csrf, "the form's token is the session's");
    assert_eq!(parked.label, LABEL);
}

/// A DOUBLE-CLICKED Continue: the second press, from the same browser with the
/// same token, gets the SAME 303 the first did, and WorkOS was asked exactly
/// once. A different browser gets nothing, and once the few-second marker is
/// gone a replay is refused as before, still with one completion.
#[tokio::test]
async fn a_double_clicked_continue_gets_the_same_redirect_once() {
    let h = Harness::new().await;
    let (cookie, csrf, _) = h.reach_confirm().await;
    let (first, a, _) = h.decide(&cookie, &csrf, "continue").await;
    assert_eq!(first, StatusCode::SEE_OTHER);
    let (second, b, body) = h.decide(&cookie, &csrf, "continue").await;
    assert_eq!(second, StatusCode::SEE_OTHER, "{body}");
    assert_eq!(a[header::LOCATION], b[header::LOCATION]);
    assert_eq!(b[header::LOCATION].to_str().unwrap(), REDIRECT);
    assert_eq!(b[header::CACHE_CONTROL], "no-store, no-cache");
    assert_eq!(h.completions().len(), 1, "exactly one completion");

    // Another browser: no cookie, or a cookie naming a different session.
    let origin = h.public_url.clone();
    let (none, h1, _) = h
        .post(
            "/agent/confirm",
            None,
            Some(&origin),
            format!("csrf={csrf}&decision=continue"),
        )
        .await;
    assert_eq!(none, StatusCode::BAD_REQUEST);
    assert!(h1.get(header::LOCATION).is_none());
    let (_, other) = h.start_agent().await;
    let (other_status, h2, _) = h.decide(&other, &csrf, "continue").await;
    assert_eq!(other_status, StatusCode::BAD_REQUEST);
    assert!(h2.get(header::LOCATION).is_none());
    // Same browser, wrong token: refused, not the redirect.
    let (wrong, h3, _) = h.decide(&cookie, "not-the-token", "continue").await;
    assert_eq!(wrong, StatusCode::BAD_REQUEST);
    assert!(h3.get(header::LOCATION).is_none());

    // After the marker's few seconds, a replay is refused as it always was.
    let later = Instant::now() + squelch_control::sessions::AGENT_DONE_TTL + Duration::from_secs(1);
    h.state.sessions().sweep(later);
    let (third, headers, body) = h.decide(&cookie, &csrf, "continue").await;
    assert_eq!(third, StatusCode::BAD_REQUEST, "{body}");
    assert!(headers.get(header::LOCATION).is_none());
    assert!(visible(&body).contains("Go back to your agent"), "{body}");
    assert_eq!(h.completions().len(), 1, "still exactly one completion");
}

/// THE PAGE IS A READ. Asking for it twice shows it twice, with the SAME one
/// token, and leaves the parked session exactly where it was; Continue then
/// works once.
#[tokio::test]
async fn the_confirmation_page_can_be_shown_twice_without_being_spent() {
    let h = Harness::new().await;
    let (cookie, csrf, first) = h.reach_confirm().await;
    let (status, headers, second) = h.get("/agent/confirm", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(csrf_of(&second), csrf, "one token per confirmation");
    assert_eq!(visible(&first), visible(&second));
    assert_eq!(headers[header::CACHE_CONTROL], "no-store, no-cache");
    assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
    assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
    let csp = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
    // No CORS grant: a cross-origin fetch of this page is opaque.
    assert!(headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
    assert_eq!(h.state.live_sessions(), 1, "nothing was consumed");

    let (done, _, _) = h.decide(&cookie, &csrf, "continue").await;
    assert_eq!(done, StatusCode::SEE_OTHER);
    assert_eq!(h.completions().len(), 1);
}

/// A REFRESH OF THE OLD CALLBACK URL (stale code and state, new cookie) does
/// not destroy the parked confirmation: it is sent back to the page, Google is
/// not asked again, and Continue still works afterwards.
#[tokio::test]
async fn refreshing_the_callback_does_not_destroy_the_confirmation() {
    let h = Harness::new().await;
    let (consent, pre_google) = h.start_agent().await;
    let (_, redirect, _) = h.finish(&consent, &pre_google).await;
    let cookie = fresh_cookie(&redirect);
    let exchanges = h.rec.lock().unwrap().exchanges;

    // The browser re-requests the callback URL, now holding the NEW cookie.
    for _ in 0..2 {
        let (status, headers, body) = h.finish(&consent, &cookie).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
        assert_eq!(headers[header::LOCATION], "/agent/confirm");
        assert!(
            !set_cookies(&headers).contains("Max-Age=0"),
            "the cookie survives"
        );
    }
    assert_eq!(
        h.rec.lock().unwrap().exchanges,
        exchanges,
        "no second exchange"
    );
    assert_eq!(
        h.state.live_sessions(),
        1,
        "the confirmation is still parked"
    );

    let (status, _, page) = h.get("/agent/confirm", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    let (done, headers, _) = h.decide(&cookie, &csrf_of(&page), "continue").await;
    assert_eq!(done, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION].to_str().unwrap(), REDIRECT);
    assert_eq!(h.completions().len(), 1);
}

/// The page answers only to a cookie naming a LIVE confirmation: no cookie,
/// another flow's cookie, a pre-Google agent cookie, and an expired
/// confirmation all get the refusal, with no mailbox on it.
#[tokio::test]
async fn the_confirmation_page_needs_a_live_confirmation() {
    let h = Harness::new().await;
    let (cookie, _, _) = h.reach_confirm().await;
    let (_, app_cookie) = h.start("/app/auth").await;
    let (_, early) = h.start_agent().await;
    for (name, c) in [
        ("no cookie", None),
        ("app cookie", Some(app_cookie.as_str())),
        ("pre-google cookie", Some(early.as_str())),
    ] {
        let (status, _, body) = h.get("/agent/confirm", c).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {body}");
        assert!(!body.contains(STORED), "{name}: {body}");
        assert!(!body.contains("csrf"), "{name}: {body}");
    }
    // ...and the refusals took nothing: three sessions still live.
    assert_eq!(h.state.live_sessions(), 3);

    let later =
        Instant::now() + squelch_control::sessions::AGENT_CONFIRM_TTL + Duration::from_secs(1);
    h.state.sessions().sweep(later);
    let (status, _, body) = h.get("/agent/confirm", Some(&cookie)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(!body.contains(STORED), "{body}");
}

/// Every way a POST can fail to be THIS browser pressing THIS page's button is
/// refused, and WorkOS hears nothing: a wrong token, no token, no cookie,
/// another kind's cookie, and a cross-site origin.
#[tokio::test]
async fn a_confirmation_that_is_not_ours_is_refused() {
    // Each case gets a harness of its own, so one case's refusal (which spends
    // the session) cannot be what makes the next one fail.
    for case in [
        "wrong token",
        "no token",
        "no cookie",
        "cross-site",
        "app cookie",
        "pre-google cookie",
    ] {
        let h = Harness::new().await;
        let (cookie, csrf, _) = h.reach_confirm().await;
        let origin = h.public_url.clone();
        let (status, headers, body) = match case {
            "wrong token" => {
                let wrong = format!("{}A", &csrf[..csrf.len() - 1]);
                let wrong = if wrong == csrf {
                    format!("{}B", &csrf[..csrf.len() - 1])
                } else {
                    wrong
                };
                h.decide(&cookie, &wrong, "continue").await
            }
            "no token" => {
                h.post(
                    "/agent/confirm",
                    Some(&cookie),
                    Some(&origin),
                    "decision=continue".into(),
                )
                .await
            }
            "no cookie" => {
                h.post(
                    "/agent/confirm",
                    None,
                    Some(&origin),
                    format!("csrf={csrf}&decision=continue"),
                )
                .await
            }
            "cross-site" => {
                h.post(
                    "/agent/confirm",
                    Some(&cookie),
                    Some("https://evil.test"),
                    format!("csrf={csrf}&decision=continue"),
                )
                .await
            }
            "app cookie" => {
                // A real, live APP session's cookie, with the right token for
                // the confirmation that is also live.
                let (_, app_cookie) = h.start("/app/auth").await;
                h.decide(&app_cookie, &csrf, "continue").await
            }
            "pre-google cookie" => {
                // A live agent session that has NOT been through Google.
                let (_, early) = h.start_agent().await;
                h.decide(&early, &csrf, "continue").await
            }
            _ => unreachable!(),
        };
        assert_eq!(status, StatusCode::BAD_REQUEST, "{case}: {body}");
        assert!(headers.get(header::LOCATION).is_none(), "{case}");
        assert!(
            visible(&body).contains("Nothing was connected"),
            "{case}: {body}"
        );
        assert!(h.completions().is_empty(), "{case}: WorkOS was called");
    }
}

/// CANCEL DISCARDS the parked result: a "nothing was connected" page, no
/// completion, and a Continue pressed afterwards finds nothing.
#[tokio::test]
async fn cancel_discards_the_pending_state() {
    let h = Harness::new().await;
    let (cookie, csrf, _) = h.reach_confirm().await;
    let (status, headers, body) = h.decide(&cookie, &csrf, "cancel").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get(header::LOCATION).is_none());
    let seen = visible(&body);
    assert!(seen.contains("Nothing was connected"), "{body}");
    assert!(!seen.contains('\u{2014}'), "no em dashes");
    assert!(set_cookies(&headers).contains("Max-Age=0"));
    assert_eq!(h.state.live_sessions(), 0, "the state is gone");

    let (later, _, body) = h.decide(&cookie, &csrf, "continue").await;
    assert_eq!(later, StatusCode::BAD_REQUEST, "{body}");
    assert!(h.completions().is_empty());
}

/// A confirmation left past its five minutes is gone. Driven through the
/// clock the session table already takes: a sweep "five minutes and a second
/// from now" drops it, and Continue then finds nothing.
#[tokio::test]
async fn an_expired_confirmation_is_refused() {
    let h = Harness::new().await;
    let (cookie, csrf, _) = h.reach_confirm().await;
    let later =
        Instant::now() + squelch_control::sessions::AGENT_CONFIRM_TTL + Duration::from_secs(1);
    assert_eq!(
        h.state.sessions().sweep(later),
        1,
        "the confirmation expired"
    );
    let (status, _, body) = h.decide(&cookie, &csrf, "continue").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(h.completions().is_empty());
}

/// With WorkOS unconfigured the confirm route does not exist either, in
/// either method.
#[tokio::test]
async fn the_confirm_route_does_not_exist_without_workos() {
    let h = Harness::build(false, "https://signup.passband.test").await;
    let (status, _, _) = h.get("/agent/confirm", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = h
        .post(
            "/agent/confirm",
            None,
            Some("https://signup.passband.test"),
            "csrf=x&decision=continue".into(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Wait until the stub WorkOS has RECEIVED `n` completion requests. The stub
/// records a request before its configured delay, so this returns while the
/// first press's call is still in flight.
async fn until_workos_received(h: &Harness, n: usize) {
    for _ in 0..500 {
        if h.completions().len() >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("WorkOS never received {n} completion(s)");
}

/// A REAL DOUBLE CLICK: the second press lands WHILE the first is still
/// waiting on WorkOS. It finds the in-flight placeholder, waits, and gets the
/// same 303; WorkOS saw exactly one completion.
#[tokio::test]
async fn a_second_press_during_the_completion_waits_and_gets_the_same_redirect() {
    let h = Harness::new().await;
    let (cookie, csrf, _) = h.reach_confirm().await;
    h.rec.lock().unwrap().workos_delay = Some(Duration::from_millis(800));

    let first = h.decide(&cookie, &csrf, "continue");
    let second = async {
        until_workos_received(&h, 1).await;
        // In flight: received, not yet answered.
        h.decide(&cookie, &csrf, "continue").await
    };
    let ((s1, h1, b1), (s2, h2, b2)) = tokio::join!(first, second);
    assert_eq!(s1, StatusCode::SEE_OTHER, "{b1}");
    assert_eq!(s2, StatusCode::SEE_OTHER, "{b2}");
    assert_eq!(h1[header::LOCATION].to_str().unwrap(), REDIRECT);
    assert_eq!(h2[header::LOCATION], h1[header::LOCATION]);
    assert_eq!(h.completions().len(), 1, "exactly one completion");
}

/// The same shape when WorkOS FAILS: both presses get the same page class,
/// and there was still exactly one completion.
#[tokio::test]
async fn a_second_press_during_a_failing_completion_gets_the_same_failure() {
    for (status, page) in [(404u16, "has expired"), (500, "try again")] {
        let h = Harness::new().await;
        let (cookie, csrf, _) = h.reach_confirm().await;
        {
            let mut r = h.rec.lock().unwrap();
            r.workos_delay = Some(Duration::from_millis(800));
            r.workos_answer = Some((status, json!({ "message": WORKOS_MARKER }).to_string()));
        }
        let first = h.decide(&cookie, &csrf, "continue");
        let second = async {
            until_workos_received(&h, 1).await;
            h.decide(&cookie, &csrf, "continue").await
        };
        let ((s1, h1, b1), (s2, h2, b2)) = tokio::join!(first, second);
        assert_eq!(s1, s2, "{status}: {b1} / {b2}");
        assert!(h1.get(header::LOCATION).is_none(), "{status}");
        assert!(h2.get(header::LOCATION).is_none(), "{status}");
        assert!(b1.contains(page), "{status}: {b1}");
        assert!(b2.contains(page), "{status}: {b2}");
        assert!(!b2.contains(WORKOS_MARKER), "{status}: {b2}");
        assert_eq!(h.completions().len(), 1, "{status}: exactly one completion");
    }
}

/// A wrong token, or a Cancel, during the in-flight window is refused at
/// once, and the first press still completes.
#[tokio::test]
async fn a_bad_press_during_the_completion_is_refused_and_disturbs_nothing() {
    for bad in ["wrong token", "cancel"] {
        let h = Harness::new().await;
        let (cookie, csrf, _) = h.reach_confirm().await;
        h.rec.lock().unwrap().workos_delay = Some(Duration::from_millis(800));
        let first = h.decide(&cookie, &csrf, "continue");
        let second = async {
            until_workos_received(&h, 1).await;
            let started = Instant::now();
            let answer = match bad {
                "wrong token" => h.decide(&cookie, "not-the-token", "continue").await,
                _ => h.decide(&cookie, &csrf, "cancel").await,
            };
            // Refused without waiting on the first press.
            assert!(started.elapsed() < Duration::from_millis(500), "{bad}");
            answer
        };
        let ((s1, h1, b1), (s2, h2, b2)) = tokio::join!(first, second);
        assert_eq!(s2, StatusCode::BAD_REQUEST, "{bad}: {b2}");
        assert!(h2.get(header::LOCATION).is_none(), "{bad}");
        assert_eq!(s1, StatusCode::SEE_OTHER, "{bad}: {b1}");
        assert_eq!(h1[header::LOCATION].to_str().unwrap(), REDIRECT);
        assert_eq!(h.completions().len(), 1, "{bad}");
    }
}
