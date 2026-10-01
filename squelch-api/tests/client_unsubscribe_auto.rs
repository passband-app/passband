//! The AUTOMATIC unsubscribe end to end: `POST /client/unsubscribe` with
//! `auto: true`, a job that talks to a sender's site, and the ledger row the
//! client polls. The "sender's site" is an axum app on loopback, reachable only
//! because these tests set the fetcher's test hook; the SSRF guard itself is
//! unit-tested in `src/unsub_fetch.rs`.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::{get, post};
use common::*;
use squelch_api::router;
use squelch_core::config::Stage2Provider;
use squelch_core::store::{SqliteStore, Store};
use squelch_core::triage::unsub_page::UnsubPageClient;
use squelch_core::types::{Sensitivity, Tier, UnsubscribeRecord};
use tower::ServiceExt;

/// Every request the sender's site saw: "METHOD path body".
type Hits = Arc<Mutex<Vec<String>>>;

async fn record(hits: &Hits, method: &str, path: &str, body: &str) {
    hits.lock().unwrap().push(format!("{method} {path} {body}"));
}

const DONE: &str = "<html><head><title>Done</title></head><body>\
    <h1>You have been unsubscribed.</h1><p>Sorry to see you go! Tell us why?</p>\
    <form method=post action=/survey><input type=radio name=why value=a>\
    <button>Send</button></form></body></html>";

const CONFIRM: &str = "<html><body><p>Click below to unsubscribe from all mailings.</p>\
    <form method=post action=/confirm/go>\
    <input type=hidden name=token value=t0k&amp;1>\
    <input type=email name=email value=me+news@example.com>\
    <button>Unsubscribe</button></form></body></html>";

const PREFS: &str = "<html><body><h1>Email preferences</h1>\
    <p>Pick the topics you want. Unsubscribe from any below.</p>\
    <form method=post action=/prefs/save>\
    <input type=checkbox name=topic value=news checked> News\
    <input type=checkbox name=topic value=deals checked> Deals\
    <button>Save</button></form></body></html>";

/// The sender's site. Paths are the scenarios.
async fn sender_site() -> (String, Hits) {
    let hits: Hits = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route(
            "/oc",
            post(|State(h): State<Hits>, body: String| async move {
                record(&h, "POST", "/oc", &body).await;
                StatusCode::OK
            }),
        )
        .route(
            "/oc-405",
            post(|State(h): State<Hits>, body: String| async move {
                record(&h, "POST", "/oc-405", &body).await;
                StatusCode::METHOD_NOT_ALLOWED
            })
            .get(|State(h): State<Hits>| async move {
                record(&h, "GET", "/oc-405", "").await;
                Html(DONE)
            }),
        )
        .route(
            "/done",
            get(|State(h): State<Hits>| async move {
                record(&h, "GET", "/done", "").await;
                Html(DONE)
            }),
        )
        .route(
            "/hop",
            get(|State(h): State<Hits>| async move {
                record(&h, "GET", "/hop", "").await;
                Redirect::to("/done")
            }),
        )
        .route(
            "/confirm",
            get(|State(h): State<Hits>| async move {
                record(&h, "GET", "/confirm", "").await;
                ([("set-cookie", "sid=abc; Path=/")], Html(CONFIRM))
            }),
        )
        .route(
            "/confirm/go",
            post(
                |State(h): State<Hits>, headers: HeaderMap, body: String| async move {
                    let cookie = headers
                        .get("cookie")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    record(
                        &h,
                        "POST",
                        "/confirm/go",
                        &format!("{body} cookie={cookie}"),
                    )
                    .await;
                    Html(DONE)
                },
            ),
        )
        .route(
            "/confirm-nowhere",
            get(|| async { Html(CONFIRM.replace("/confirm/go", "/confirm/limbo")) }),
        )
        .route(
            "/confirm/limbo",
            post(|| async { Html("<p>Thanks! Your request was received.</p>") }),
        )
        .route(
            "/prefs",
            get(|State(h): State<Hits>| async move {
                record(&h, "GET", "/prefs", "").await;
                Html(PREFS)
            }),
        )
        .route(
            "/walled",
            get(|| async { (StatusCode::FORBIDDEN, "Just a moment...").into_response() }),
        )
        .route(
            "/gone",
            get(|| async { (StatusCode::NOT_FOUND, "nope").into_response() }),
        )
        .route(
            "/shell",
            get(|| async {
                Html("<html><body><div id=app></div><script src=/a.js></script></body></html>")
            }),
        )
        .with_state(hits.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), hits)
}

/// A one-shot model endpoint answering every call with `verdict`.
async fn model_answering(verdict: &'static str, form: i64) -> String {
    let body = serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::json!({ "verdict": verdict, "form": form }).to_string(),
        }],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 300, "output_tokens": 10 },
    })
    .to_string();
    let app = Router::new().fallback(move || {
        let body = body.clone();
        async move { ([("content-type", "application/json")], body) }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

struct Setup {
    header: String,
    one_click: bool,
    auth_pass: Option<bool>,
    is_spam: bool,
}

impl Setup {
    fn page(header: String) -> Self {
        Self {
            header,
            one_click: false,
            auth_pass: Some(true),
            is_spam: false,
        }
    }
}

fn seed(store: &SqliteStore, acct: i64, s: &Setup) -> i64 {
    let mut m = msg(acct, "g-auto", "t-auto", "Weekly deals", "body");
    m.from_addr = "News@Shop.example".into();
    m.list_unsubscribe = Some(format!("<{}>", s.header));
    m.list_unsub_one_click = s.one_click;
    m.auth_pass = s.auth_pass;
    m.is_spam = s.is_spam;
    let id = store.upsert_message(&m).unwrap();
    store
        .set_triage(
            id,
            acct,
            10,
            Tier::Noise,
            Sensitivity::Normal,
            None,
            "",
            "",
            None,
        )
        .unwrap();
    id
}

struct Run {
    app: axum::Router,
    store: Arc<SqliteStore>,
    acct: i64,
    mid: i64,
}

fn mount(s: &Setup, model: Option<String>) -> Run {
    let mut mid = 0;
    let (state, store, acct) = state_with(|store, acct| mid = seed(store, acct, s));
    let mut state = state.with_unsub_fetch_allow_local(true);
    if let Some(url) = model {
        state = state.with_unsub_page_reader(Some(UnsubPageClient::new(
            reqwest::Client::new(),
            url,
            "sk-test",
            Stage2Provider::Anthropic,
            "claude-haiku-4-5",
            None,
        )));
    }
    Run {
        app: router(state),
        store,
        acct,
        mid,
    }
}

async fn ask(run: &Run, auto: bool) -> serde_json::Value {
    let resp = run
        .app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/unsubscribe",
            serde_json::json!({ "message_id": run.mid, "auto": auto }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    body_json(resp).await
}

/// Poll the ledger the way the client does, until the job settles.
async fn settled(run: &Run) -> UnsubscribeRecord {
    for _ in 0..200 {
        let row = run.store.list_unsubscribes(run.acct).unwrap().remove(0);
        if row.status.as_deref() != Some("running") {
            return row;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("job never settled");
}

fn hits(h: &Hits) -> Vec<String> {
    h.lock().unwrap().clone()
}

/// RFC 8058 requires https, so an http header URL advertising one-click is
/// never POSTed to: it is read as a page. (The one-click POST itself is tested
/// inside the crate, against `unsub_job::run`, where the loopback site's plain
/// http is not in the way.)
#[tokio::test]
async fn http_one_click_is_read_as_a_page_never_posted() {
    let (site, h) = sender_site().await;
    let run = mount(
        &Setup {
            one_click: true,
            ..Setup::page(format!("{site}/oc-405"))
        },
        None,
    );
    let json = ask(&run, true).await;
    assert_eq!(json["method"], "auto");
    assert_eq!(json["status"], "running");
    assert!(json.get("url").is_none(), "nothing for the client to open");
    let row = settled(&run).await;
    assert_eq!(row.status.as_deref(), Some("done"));
    assert_eq!(row.method, "page");
    assert_eq!(hits(&h), vec!["GET /oc-405 "], "no POST to a non-https URL");
}

#[tokio::test]
async fn done_page_after_a_redirect_is_done_and_the_survey_is_skipped() {
    let (site, h) = sender_site().await;
    let run = mount(&Setup::page(format!("{site}/hop")), None);
    ask(&run, true).await;
    let row = settled(&run).await;
    assert_eq!(row.status.as_deref(), Some("done"));
    assert_eq!(row.method, "page");
    assert!(row.status_reason.is_none());
    assert_eq!(
        hits(&h),
        vec!["GET /hop ", "GET /done "],
        "the survey is never sent"
    );
}

#[tokio::test]
async fn confirm_form_is_submitted_once_with_the_session_cookie_and_proven_done() {
    let (site, h) = sender_site().await;
    // No model: the lone "Unsubscribe" button on an unsubscribe page is the
    // obvious action.
    let run = mount(&Setup::page(format!("{site}/confirm")), None);
    ask(&run, true).await;
    let row = settled(&run).await;
    assert_eq!(row.status.as_deref(), Some("done"), "{row:?}");
    let seen = hits(&h);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0], "GET /confirm ");
    assert_eq!(
        seen[1], "POST /confirm/go token=t0k%261&email=me%2Bnews%40example.com cookie=sid=abc",
        "hidden token decoded and re-encoded, the page's own prefill kept, session cookie kept"
    );
}

#[tokio::test]
async fn a_submit_that_does_not_say_done_goes_to_the_user() {
    let (site, _h) = sender_site().await;
    let run = mount(&Setup::page(format!("{site}/confirm-nowhere")), None);
    ask(&run, true).await;
    let row = settled(&run).await;
    assert_eq!(row.status.as_deref(), Some("needs_you"));
    assert_eq!(row.status_reason.as_deref(), Some("unclear"));
    assert_eq!(
        row.status_url.as_deref(),
        Some(format!("{site}/confirm-nowhere").as_str()),
        "the ORIGINAL link, so the user's browser replays the whole chain"
    );
}

#[tokio::test]
async fn preference_center_needs_the_user_and_nothing_is_submitted() {
    let (site, h) = sender_site().await;
    let model = model_answering("preferences", -1).await;
    let run = mount(&Setup::page(format!("{site}/prefs")), Some(model));
    ask(&run, true).await;
    let row = settled(&run).await;
    assert_eq!(row.status.as_deref(), Some("needs_you"));
    assert_eq!(row.status_reason.as_deref(), Some("choose_topics"));
    assert_eq!(
        hits(&h),
        vec!["GET /prefs "],
        "the Save button is never pressed"
    );

    // The model call is billed under its own category.
    let usage = run.store.list_usage_by_category(run.acct, 1).unwrap();
    assert!(usage.iter().any(|(c, _)| c == "unsub_page"), "{usage:?}");
}

#[tokio::test]
async fn without_a_model_a_preference_center_is_unclear_not_guessed() {
    let (site, h) = sender_site().await;
    let run = mount(&Setup::page(format!("{site}/prefs")), None);
    ask(&run, true).await;
    let row = settled(&run).await;
    assert_eq!(row.status.as_deref(), Some("needs_you"));
    assert_eq!(row.status_reason.as_deref(), Some("unclear"));
    assert_eq!(hits(&h), vec!["GET /prefs "]);
}

#[tokio::test]
async fn walls_dead_links_and_app_shells_say_why() {
    let (site, _h) = sender_site().await;
    for (path, reason) in [
        ("/walled", "needs_browser"),
        ("/gone", "link_error"),
        ("/shell", "needs_browser"),
    ] {
        let run = mount(&Setup::page(format!("{site}{path}")), None);
        ask(&run, true).await;
        let row = settled(&run).await;
        assert_eq!(row.status.as_deref(), Some("needs_you"), "{path}");
        assert_eq!(row.status_reason.as_deref(), Some(reason), "{path}");
    }
}

#[tokio::test]
async fn spam_and_unverified_mail_fall_back_to_the_browser_with_a_reason() {
    let (site, h) = sender_site().await;
    for (setup, why) in [
        (
            Setup {
                is_spam: true,
                ..Setup::page(format!("{site}/done"))
            },
            "spam",
        ),
        (
            Setup {
                auth_pass: Some(false),
                ..Setup::page(format!("{site}/done"))
            },
            "unverified",
        ),
        (
            Setup {
                auth_pass: None,
                ..Setup::page(format!("{site}/done"))
            },
            "unchecked",
        ),
    ] {
        let run = mount(&setup, None);
        let json = ask(&run, true).await;
        assert_eq!(json["method"], "browser");
        assert_eq!(json["auto_refused"], why);
        assert_eq!(json["url"], format!("{site}/done"));
        let row = run.store.list_unsubscribes(run.acct).unwrap().remove(0);
        assert_eq!(row.method, "browser");
        assert!(row.status.is_none());
    }
    assert!(hits(&h).is_empty(), "nothing was fetched");
}

#[tokio::test]
async fn an_old_client_gets_the_old_response_and_nothing_is_fetched() {
    let (site, h) = sender_site().await;
    let run = mount(&Setup::page(format!("{site}/done")), None);
    let json = ask(&run, false).await;
    assert_eq!(json["method"], "browser");
    assert_eq!(json["url"], format!("{site}/done"));
    assert!(json.get("auto_refused").is_none());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(hits(&h).is_empty());
}

#[tokio::test]
async fn an_orphaned_running_row_is_reaped_into_needs_you_on_listing() {
    let run = mount(&Setup::page("https://shop.example/u".into()), None);
    // A fresh running row with no owner is NOT reaped: it could be a job that
    // was claimed while the listing ran.
    run.store
        .begin_unsubscribe_job(
            run.acct,
            "fresh@shop.example",
            Some(run.mid),
            "https://shop.example/f",
            chrono::Utc::now(),
        )
        .unwrap();
    // A job that died with the last process: the row says running, has for
    // minutes, and no job in this one owns it.
    run.store
        .begin_unsubscribe_job(
            run.acct,
            "news@shop.example",
            Some(run.mid),
            "https://shop.example/u",
            chrono::Utc::now() - chrono::Duration::minutes(10),
        )
        .unwrap();
    let resp = run
        .app
        .clone()
        .oneshot(authed("GET", "/client/unsubscribes"))
        .await
        .unwrap();
    let rows = body_json(resp).await;
    let row = |sender: &str| {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|r| r["sender"] == sender)
            .unwrap()
            .clone()
    };
    let old = row("news@shop.example");
    assert_eq!(old["status"], "needs_you");
    assert_eq!(old["status_reason"], "unclear");
    assert_eq!(old["status_url"], "https://shop.example/u");
    assert_eq!(row("fresh@shop.example")["status"], "running");
}

#[tokio::test]
async fn opened_endpoint_moves_needs_you_to_opened_once() {
    let run = mount(&Setup::page("https://shop.example/u".into()), None);
    let sender = "news@shop.example";
    let long_ago = chrono::Utc::now() - chrono::Duration::days(10);
    run.store
        .begin_unsubscribe_job(
            run.acct,
            sender,
            Some(run.mid),
            "https://shop.example/u",
            long_ago,
        )
        .unwrap();
    run.store
        .finish_unsubscribe_job(
            run.acct,
            sender,
            "needs_you",
            "auto",
            Some("choose_topics"),
            long_ago,
        )
        .unwrap();

    // (The detector itself is tested against the real ingest path in
    // squelch-core's store tests; this test is about the endpoint.)

    // The user finishes on the site: the request now counts, from now.
    let resp = run
        .app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/unsubscribes/opened",
            serde_json::json!({ "sender": "News@Shop.example" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let row = run.store.list_unsubscribes(run.acct).unwrap().remove(0);
    assert_eq!(row.status.as_deref(), Some("opened"));
    assert!(row.requested_at > long_ago + chrono::Duration::days(9));

    // A second "opened" has no needs_you row to act on.
    let resp = run
        .app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/unsubscribes/opened",
            serde_json::json!({ "sender": sender }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_second_press_while_running_does_not_start_a_second_job() {
    let (site, h) = sender_site().await;
    let run = mount(&Setup::page(format!("{site}/done")), None);
    let (a, b) = tokio::join!(ask(&run, true), ask(&run, true));
    assert_eq!(a["status"], "running");
    assert_eq!(b["status"], "running");
    settled(&run).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(hits(&h), vec!["GET /done "], "one job, one fetch");
}
