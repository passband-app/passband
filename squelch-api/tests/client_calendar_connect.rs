//! `POST /client/calendar/connect`: the self-host daemon runs the Google
//! Calendar consent itself, end to end against a scripted Google on loopback
//! (the token endpoint and Gmail's profile), with the credentials in a file
//! backend under the temp dir. Nothing here reaches the network.
mod common;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener as StdListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};
use squelch_api::router;
use squelch_core::auth::{AuthScopes, GoogleEndpoints, LoopbackConsent};
use squelch_core::config::{
    CALENDAR_EVENTS_SCOPE, CALENDAR_SCOPES, CredentialBackend, GMAIL_READONLY_SCOPE,
    OAuthClientConfig,
};
use squelch_core::credentials::{CalendarCredentials, CredentialKind, StoredToken};
use tower::ServiceExt;

const ME: &str = "me@example.com";

fn client() -> OAuthClientConfig {
    OAuthClientConfig {
        client_id: "1234.apps.googleusercontent.com".into(),
        client_secret: "not-a-real-secret".into(),
    }
}

/// A credentials file holding the Gmail pair (no calendar on it), and the
/// resolver over it: a self-host that ran plain `squelchd auth`.
fn resolver(tag: &str, account_id: i64) -> (CalendarCredentials, PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "squelch-api-calconnect-{tag}-{}-{}.json",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    let token = StoredToken {
        access_token: "a".into(),
        refresh_token: Some("r".into()),
        expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
        scopes: Some(vec![GMAIL_READONLY_SCOPE.to_string()]),
    };
    squelch_core::credentials::store_tokens_backend(
        CredentialBackend::File,
        &path,
        ME,
        &[
            (CredentialKind::Read, token.clone()),
            (CredentialKind::Write, token),
        ],
    )
    .unwrap();
    let resolver = CalendarCredentials::new(
        account_id,
        ME.into(),
        CredentialBackend::File,
        path.clone(),
        client(),
    );
    (resolver, path)
}

fn calendar_slot(path: &std::path::Path) -> Option<StoredToken> {
    squelch_core::credentials::load_token_file(path, ME, CredentialKind::Calendar).ok()
}

/// A fake Google: one scripted answer per accepted connection, in order. The
/// token endpoint's answer, then the profile's.
fn scripted_google(answers: Vec<(&'static str, String)>) -> GoogleEndpoints {
    let listener = StdListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    std::thread::spawn(move || {
        for (status, body) in answers {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut scratch = [0u8; 8192];
            let _ = stream.read(&mut scratch);
            let raw = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(raw.as_bytes());
            let _ = stream.flush();
        }
    });
    GoogleEndpoints {
        token_url: format!("{base}/token"),
        profile_url: format!("{base}/profile"),
    }
}

fn token_answer(scope: &str) -> (&'static str, String) {
    (
        "200 OK",
        format!(
            r#"{{"access_token":"fresh-access","token_type":"Bearer","expires_in":3600,"refresh_token":"fresh-refresh","scope":"{scope}"}}"#
        ),
    )
}

fn profile_answer(email: &str) -> (&'static str, String) {
    ("200 OK", format!(r#"{{"emailAddress":"{email}"}}"#))
}

fn full_grant() -> String {
    format!("{GMAIL_READONLY_SCOPE} {}", CALENDAR_SCOPES.join(" "))
}

/// A self-host state: resolver attached, no console origin, an ephemeral
/// loopback port, and the exchange pointed at `google`.
struct SelfHost {
    app: axum::Router,
    path: PathBuf,
    kick: Arc<tokio::sync::Notify>,
}

fn self_host(tag: &str, google: GoogleEndpoints) -> SelfHost {
    let (state, _store, acct) = state_with(|_, _| {});
    let (resolver, path) = resolver(tag, acct);
    let kick = Arc::new(tokio::sync::Notify::new());
    let app = router(
        state
            .with_calendar_credentials(resolver)
            .with_calendar_sync_kick(kick.clone())
            .with_calendar_consent_port(0)
            .with_calendar_consent_endpoints(google),
    );
    SelfHost { app, path, kick }
}

async fn post_connect(app: &axum::Router) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(authed("POST", "/client/calendar/connect"))
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn stats_calendar(app: &axum::Router) -> Value {
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/stats"))
        .await
        .unwrap();
    body_json(resp).await["calendar"].clone()
}

fn params(url: &str) -> HashMap<String, String> {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
}

/// The browser following Google's redirect back to the loopback listener.
async fn redirect(port: u16, query: &str) -> (u16, String) {
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let resp = http
        .get(format!("http://127.0.0.1:{port}/?{query}"))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.text().await.unwrap())
}

fn redirect_port(url: &str) -> u16 {
    let p = params(url);
    url::Url::parse(&p["redirect_uri"]).unwrap().port().unwrap()
}

fn assert_plain(page: &str) {
    let lower = page.to_ascii_lowercase();
    assert!(!lower.contains("<script"), "{page}");
    assert!(!lower.contains("<a "), "{page}");
    assert!(!lower.contains("href"), "{page}");
    assert!(!page.contains('@'), "the page names an address: {page}");
}

// --- hosted -----------------------------------------------------------------

/// HOSTED: the control plane runs the consent. 409 with the fixed error,
/// nothing stored, nothing listening, and `self_connect: false`.
#[tokio::test(flavor = "multi_thread")]
async fn a_hosted_daemon_answers_409_and_stores_nothing() {
    let (state, _store, acct) = state_with(|_, _| {});
    let (resolver, path) = resolver("hosted", acct);
    let app = router(
        state
            .with_calendar_credentials(resolver)
            .with_console_sso_url(Some("https://signup.example".into()))
            .with_calendar_consent_port(0),
    );
    let (status, body) = post_connect(&app).await;
    eprintln!("hosted connect: {status} {body}");
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body,
        json!({ "error": "calendar consent for this mailbox runs on the control plane; open calendar.connect_url" })
    );
    assert!(calendar_slot(&path).is_none());

    let calendar = stats_calendar(&app).await;
    eprintln!("hosted stats calendar: {calendar}");
    assert_eq!(
        calendar,
        json!({
            "granted": false,
            "connect_url": "https://signup.example/reconnect?calendar=1&hint=me%40example.com",
            "connect_command": null,
            "self_connect": false,
        })
    );
    std::fs::remove_file(&path).ok();
}

/// No OAuth client: no resolver, nothing to run a consent with.
#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_with_no_oauth_client_cannot_self_connect() {
    let Harness { app, .. } = harness(|_, _| {});
    let (status, body) = post_connect(&app).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body,
        json!({ "error": "this daemon has no Google OAuth client configured, so it cannot run the calendar consent" })
    );
    assert_eq!(stats_calendar(&app).await["self_connect"], json!(false));
}

// --- self-host --------------------------------------------------------------

/// SELF-HOST: the answer is the consent URL with every parameter the CLI's
/// carries, and stats say the daemon can run it.
#[tokio::test(flavor = "multi_thread")]
async fn a_self_host_connect_answers_the_consent_url() {
    let h = self_host("url", GoogleEndpoints::default());

    let calendar = stats_calendar(&h.app).await;
    eprintln!("self-host stats calendar: {calendar}");
    assert_eq!(
        calendar,
        json!({
            "granted": false,
            "connect_url": null,
            "connect_command": "squelchd auth --calendar",
            "self_connect": true,
        })
    );

    let (status, body) = post_connect(&h.app).await;
    eprintln!("self-host connect: {status} {body}");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["expires_in"], json!(600));
    assert_eq!(body.as_object().unwrap().len(), 2, "{body}");
    let url = body["url"].as_str().unwrap();
    assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
    let p = params(url);
    let get = |k: &str| p.get(k).map(String::as_str);
    let port = redirect_port(url);
    assert_ne!(port, 0);
    assert_eq!(
        get("redirect_uri").unwrap(),
        format!("http://127.0.0.1:{port}")
    );
    assert_eq!(get("client_id"), Some("1234.apps.googleusercontent.com"));
    assert_eq!(get("response_type"), Some("code"));
    assert_eq!(get("scope"), Some(CALENDAR_SCOPES.join(" ").as_str()));
    assert_eq!(get("access_type"), Some("offline"));
    assert_eq!(get("prompt"), Some("consent"));
    assert_eq!(get("include_granted_scopes"), Some("true"));
    assert_eq!(get("login_hint"), Some(ME));
    assert_eq!(get("code_challenge_method"), Some("S256"));
    assert!(!get("code_challenge").unwrap().is_empty());
    assert!(get("state").unwrap().len() >= 16);
    assert!(!url.contains("not-a-real-secret"));

    // THE CLI'S URL: `squelchd auth --calendar` builds through the same
    // `LoopbackConsent::begin`, so for the same port the two differ only in
    // the two values each consent mints fresh.
    let cli = LoopbackConsent::begin(&client(), AuthScopes::Calendar, port, Some(ME)).unwrap();
    let mut ours = p.clone();
    let mut theirs = params(cli.url().as_str());
    for k in ["state", "code_challenge"] {
        assert!(ours.remove(k).is_some() && theirs.remove(k).is_some());
    }
    assert_eq!(ours, theirs);

    // Nothing stored by asking.
    assert!(calendar_slot(&h.path).is_none());
    std::fs::remove_file(&h.path).ok();
}

/// A wrong state, a missing one, and a request off the one route all store
/// nothing and get the plain refusal; the real redirect still works after
/// them, because a stray request must not cancel the person's consent.
#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_state_stores_nothing_and_shows_the_refusal() {
    let google = scripted_google(vec![token_answer(&full_grant()), profile_answer(ME)]);
    let h = self_host("wrong-state", google);
    let (_, body) = post_connect(&h.app).await;
    let url = body["url"].as_str().unwrap();
    let port = redirect_port(url);
    let state = params(url)["state"].clone();

    for query in [
        format!("code=c&state={}x", state),
        "code=c&state=guess".to_string(),
        "code=c".to_string(),
        String::new(),
    ] {
        let (status, page) = redirect(port, &query).await;
        assert_eq!(status, 400, "{query}: {page}");
        assert!(page.contains("This link is no longer valid"), "{page}");
        assert_plain(&page);
        assert!(calendar_slot(&h.path).is_none(), "{query} stored");
    }
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let other = http
        .get(format!("http://127.0.0.1:{port}/favicon.ico"))
        .send()
        .await
        .unwrap();
    assert_eq!(other.status().as_u16(), 404);
    assert!(h.kick.notified().now_or_never_is_pending());

    // The real redirect is still good.
    let (status, page) = redirect(port, &format!("code=the-code&state={state}")).await;
    assert_eq!(status, 200, "{page}");
    assert!(calendar_slot(&h.path).is_some());
    std::fs::remove_file(&h.path).ok();
}

/// A second Connect replaces the first: the first consent's state is refused
/// from then on, and the second's works.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_connect_invalidates_the_first_state() {
    let google = scripted_google(vec![token_answer(&full_grant()), profile_answer(ME)]);
    let h = self_host("second", google);
    let (_, first) = post_connect(&h.app).await;
    let (_, second) = post_connect(&h.app).await;
    let first_url = first["url"].as_str().unwrap();
    let second_url = second["url"].as_str().unwrap();
    let port = redirect_port(second_url);
    assert_eq!(port, redirect_port(first_url), "the listener is reused");
    let first_state = params(first_url)["state"].clone();
    let second_state = params(second_url)["state"].clone();
    assert_ne!(first_state, second_state);

    let (status, page) = redirect(port, &format!("code=c&state={first_state}")).await;
    assert_eq!(status, 400, "{page}");
    assert!(page.contains("This link is no longer valid"), "{page}");
    assert!(calendar_slot(&h.path).is_none());

    let (status, page) = redirect(port, &format!("code=c&state={second_state}")).await;
    assert_eq!(status, 200, "{page}");
    assert!(calendar_slot(&h.path).is_some());
    std::fs::remove_file(&h.path).ok();
}

/// A consent finished as a different Google account stores nothing, and the
/// page names neither address.
#[tokio::test(flavor = "multi_thread")]
async fn a_mismatched_account_stores_nothing_and_names_no_address() {
    let google = scripted_google(vec![
        token_answer(&full_grant()),
        profile_answer("stranger@elsewhere.com"),
    ]);
    let h = self_host("mismatch", google);
    let (_, body) = post_connect(&h.app).await;
    let url = body["url"].as_str().unwrap();
    let state = params(url)["state"].clone();
    let (status, page) = redirect(redirect_port(url), &format!("code=c&state={state}")).await;
    eprintln!("mismatch page: {status} {page}");
    assert_eq!(status, 403);
    assert!(
        page.contains("That was a different Google account than this Passband mailbox"),
        "{page}"
    );
    assert!(
        !page.contains("stranger") && !page.contains("me@"),
        "{page}"
    );
    assert_plain(&page);
    assert!(calendar_slot(&h.path).is_none());
    assert_eq!(stats_calendar(&h.app).await["granted"], json!(false));
    std::fs::remove_file(&h.path).ok();
}

/// The calendar box unticked: Google grants the Gmail scopes back and no
/// calendar. Nothing stored, and the page says why.
#[tokio::test(flavor = "multi_thread")]
async fn an_unticked_calendar_stores_nothing_and_says_so() {
    let google = scripted_google(vec![token_answer(GMAIL_READONLY_SCOPE)]);
    let h = self_host("unticked", google);
    let (_, body) = post_connect(&h.app).await;
    let url = body["url"].as_str().unwrap();
    let state = params(url)["state"].clone();
    let (status, page) = redirect(redirect_port(url), &format!("code=c&state={state}")).await;
    eprintln!("unticked page: {status} {page}");
    assert_eq!(status, 400);
    assert!(
        page.contains("Calendar was not connected because the calendar permission was not ticked"),
        "{page}"
    );
    assert_plain(&page);
    assert!(calendar_slot(&h.path).is_none());
    std::fs::remove_file(&h.path).ok();
}

/// Google's own "no" (the person pressed Cancel) stores nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_declined_consent_stores_nothing() {
    let h = self_host("declined", GoogleEndpoints::default());
    let (_, body) = post_connect(&h.app).await;
    let url = body["url"].as_str().unwrap();
    let state = params(url)["state"].clone();
    let (status, page) = redirect(
        redirect_port(url),
        &format!("error=access_denied&state={state}"),
    )
    .await;
    assert_eq!(status, 400);
    assert!(page.contains("Calendar was not connected"), "{page}");
    assert_plain(&page);
    assert!(calendar_slot(&h.path).is_none());
    std::fs::remove_file(&h.path).ok();
}

/// THE GOOD PATH: the Calendar slot is stored with its scopes, the page says
/// connected, stats report the grant at once (the cache was primed with
/// "not granted" first), a sync kick was requested, and the listener is gone.
#[tokio::test(flavor = "multi_thread")]
async fn a_good_callback_stores_the_calendar_slot_and_kicks_the_sync() {
    let google = scripted_google(vec![
        token_answer(&full_grant()),
        profile_answer("Me@Example.com"),
    ]);
    let h = self_host("good", google);
    // Prime the `granted` cache with "no".
    assert_eq!(stats_calendar(&h.app).await["granted"], json!(false));

    let (_, body) = post_connect(&h.app).await;
    let url = body["url"].as_str().unwrap();
    let port = redirect_port(url);
    let state = params(url)["state"].clone();
    let (status, page) = redirect(port, &format!("code=the-code&state={state}")).await;
    eprintln!("good page: {status} {page}");
    assert_eq!(status, 200);
    assert!(
        page.contains("Calendar is connected. You can close this tab and go back to Passband."),
        "{page}"
    );
    assert_plain(&page);

    let stored = calendar_slot(&h.path).expect("the Calendar slot is stored");
    assert!(stored.grants_calendar());
    assert!(stored.has_scope(CALENDAR_EVENTS_SCOPE));
    assert_eq!(stored.refresh_token.as_deref(), Some("fresh-refresh"));
    // The Gmail pair is untouched.
    let read =
        squelch_core::credentials::load_token_file(&h.path, ME, CredentialKind::Read).unwrap();
    assert_eq!(read.access_token, "a");

    let calendar = stats_calendar(&h.app).await;
    eprintln!("after connect, stats calendar: {calendar}");
    assert_eq!(calendar["granted"], json!(true));
    assert_eq!(calendar["self_connect"], json!(true));

    tokio::time::timeout(Duration::from_secs(2), h.kick.notified())
        .await
        .expect("a calendar sync kick was requested");

    // Consumed: the same state again finds nothing listening, or a refusal.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let again = http
        .get(format!("http://127.0.0.1:{port}/?code=c&state={state}"))
        .send()
        .await;
    assert!(
        again.is_err() || again.unwrap().status().as_u16() == 400,
        "the listener still redeems a consumed state"
    );
    std::fs::remove_file(&h.path).ok();
}

/// The door split: nothing about the consent on `/mcp` or the agent door.
#[tokio::test(flavor = "multi_thread")]
async fn the_agent_door_has_no_calendar_connect() {
    let h = self_host("doors", GoogleEndpoints::default());
    for (method, uri) in [
        ("POST", "/client/agent/calendar/connect"),
        ("GET", "/client/agent/calendar/connect"),
        ("POST", "/mcp/calendar/connect"),
        ("POST", "/mcp"),
    ] {
        let resp = h.app.clone().oneshot(authed(method, uri)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{method} {uri}");
    }
    // And the human door itself is bearer-authed like its neighbours.
    let resp = h
        .app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/client/calendar/connect")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // The agent's stats carry no calendar object.
    let resp = h
        .app
        .clone()
        .oneshot(authed("GET", "/client/agent/stats"))
        .await
        .unwrap();
    if resp.status() == StatusCode::OK {
        let body = body_json(resp).await;
        assert!(body.get("calendar").is_none(), "{body}");
    }
    std::fs::remove_file(&h.path).ok();
}

/// Helper trait so a test can say "no kick yet" without waiting.
trait NowOrNever {
    fn now_or_never_is_pending(self) -> bool;
}

impl<F: std::future::Future> NowOrNever for F {
    fn now_or_never_is_pending(self) -> bool {
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let mut fut = std::pin::pin!(self);
        fut.as_mut().poll(&mut cx).is_pending()
    }
}
