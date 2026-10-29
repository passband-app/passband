//! Hidden features on the daemon: what the control plane grants reaches
//! `/client/stats`, and what happens to it when the control plane goes quiet
//! or says no.

use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::get;
use serde_json::{Value, json};
use squelch_api::{ApiState, FEATURE_REFRESH, router};
use squelch_core::features::Features;
use squelch_core::store::Store;
use tower::ServiceExt;

mod common;
use common::{authed, body_json};

/// What the mock control plane answers next, and the bearers it was shown.
#[derive(Clone, Default)]
struct Control {
    answer: Arc<Mutex<(u16, Value)>>,
    bearers: Arc<Mutex<Vec<String>>>,
}

impl Control {
    fn answers(&self, status: u16, body: Value) {
        *self.answer.lock().unwrap() = (status, body);
    }
}

/// A control plane with one route, on a real socket, because the daemon's side
/// of this is a `reqwest` client and the wire is the thing under test.
async fn spawn_control() -> (String, Control) {
    let control = Control::default();
    let seen = control.clone();
    let app = axum::Router::new().route(
        "/tenant/features",
        get(move |headers: HeaderMap| {
            let seen = seen.clone();
            async move {
                seen.bearers.lock().unwrap().push(
                    headers
                        .get(header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string(),
                );
                let (status, body) = seen.answer.lock().unwrap().clone();
                (StatusCode::from_u16(status).unwrap(), axum::Json(body))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), control)
}

fn daemon(control: Option<&str>, features: Features) -> ApiState {
    let (state, _store, _acct) = common::state_with(|_, _| {});
    state
        .with_sharing(
            control.and_then(|url| {
                squelch_api::Sharing::new(url.to_string(), "pbs_share_token".into())
            }),
        )
        .with_features(features)
}

async fn stats_features(state: &ApiState) -> Value {
    let body = body_json(
        router(state.clone())
            .oneshot(authed("GET", "/client/stats"))
            .await
            .unwrap(),
    )
    .await;
    body["features"].clone()
}

/// The resting state of every daemon: the key is there, and it is empty. A
/// client reads an answer rather than absence.
#[tokio::test]
async fn stats_lists_nothing_until_something_is_on() {
    let state = daemon(None, Features::none());
    assert_eq!(stats_features(&state).await, json!([]));
}

/// A self-host has nobody to ask, so there is no loop to run, and what it
/// pinned locally is what the app is told.
#[tokio::test]
async fn a_daemon_with_no_control_plane_has_only_what_it_pinned() {
    let state = daemon(None, Features::pinned(["calendar"]));
    assert!(state.feature_refresher().is_none());
    assert_eq!(stats_features(&state).await, json!(["calendar"]));
}

/// The whole path: the control plane grants, the daemon asks with its control
/// token, and the app is told on the next stats read. Then a box is unticked
/// and the feature is gone, with no restart in between.
#[tokio::test]
async fn a_grant_reaches_the_app_and_an_unticked_box_takes_it_away() {
    let (url, control) = spawn_control().await;
    control.answers(200, json!({ "features": ["calendar", "agent_send"] }));
    let state = daemon(Some(&url), Features::none());
    let refresher = state.feature_refresher().expect("a control plane to ask");

    assert_eq!(refresher.refresh_once().await, FEATURE_REFRESH);
    assert_eq!(
        stats_features(&state).await,
        json!(["agent_send", "calendar"])
    );
    assert!(state.features().enabled("calendar"));
    assert_eq!(
        control.bearers.lock().unwrap().as_slice(),
        ["Bearer pbs_share_token"]
    );

    control.answers(200, json!({ "features": ["agent_send"] }));
    refresher.refresh_once().await;
    assert_eq!(stats_features(&state).await, json!(["agent_send"]));
}

/// A control plane that is down has not decided anything, so the daemon keeps
/// what it last heard and asks again sooner than usual.
#[tokio::test]
async fn an_outage_keeps_the_last_answer() {
    let (url, control) = spawn_control().await;
    control.answers(200, json!({ "features": ["calendar"] }));
    let state = daemon(Some(&url), Features::none());
    let refresher = state.feature_refresher().unwrap();
    refresher.refresh_once().await;

    for (status, body) in [
        (500, json!({})),
        (503, json!({ "error": "unavailable" })),
        // Not "too old to grant anything": an edge with nothing behind it says
        // this too, and that must not switch a fleet's features off.
        (404, json!({})),
        (200, json!("not the shape")),
    ] {
        control.answers(status, body);
        assert!(refresher.refresh_once().await < FEATURE_REFRESH);
        assert_eq!(stats_features(&state).await, json!(["calendar"]));
    }
}

/// A refusal IS a decision: the control plane no longer vouches for this
/// daemon, and off is the answer to every doubt. What was pinned locally
/// survives it.
#[tokio::test]
async fn a_refusal_clears_what_was_granted_and_leaves_what_was_pinned() {
    for status in [401, 403] {
        let (url, control) = spawn_control().await;
        control.answers(200, json!({ "features": ["calendar"] }));
        let state = daemon(Some(&url), Features::pinned(["local_only"]));
        let refresher = state.feature_refresher().unwrap();
        refresher.refresh_once().await;
        assert_eq!(
            stats_features(&state).await,
            json!(["calendar", "local_only"])
        );

        control.answers(status, json!({ "error": "unauthorized" }));
        refresher.refresh_once().await;
        assert_eq!(
            stats_features(&state).await,
            json!(["local_only"]),
            "{status}"
        );
    }
}

/// The control plane's answer is untrusted input. Strings that could not be
/// keys never reach the app, however they got into the answer.
#[tokio::test]
async fn an_answer_is_filtered_down_to_things_shaped_like_keys() {
    let (url, control) = spawn_control().await;
    control.answers(
        200,
        json!({ "features": ["calendar", "<script>", "Has Space", "", "ok_2"] }),
    );
    let state = daemon(Some(&url), Features::none());
    state.feature_refresher().unwrap().refresh_once().await;
    assert_eq!(stats_features(&state).await, json!(["calendar", "ok_2"]));
}

// --- `calendar`, a retired key -----------------------------------------------
//
// The Calendar page was a hidden feature until it launched for everyone. Its
// key is retired (`squelch_core::features::RETIRED`): nothing gates on it, and
// a control plane still serving an old `tenant_features` row for it changes
// nothing.

/// Every route behind the Calendar page, with a body that is WRONG on
/// purpose where a body is read: a route that is there answers it (a 400, a
/// 403, a 409), where a route that is not would answer the opaque 404.
fn calendar_routes() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        ("GET", "/client/timeline", Value::Null),
        ("PATCH", "/client/timeline/ghosts/1", json!({ "state": 7 })),
        ("POST", "/client/timeline/refresh", Value::Null),
        ("POST", "/client/calendar/connect", Value::Null),
        ("GET", "/client/calendar/people?name=steven", Value::Null),
        ("POST", "/client/calendar/events", json!({ "title": 7 })),
        (
            "PATCH",
            "/client/calendar/events/primary/ev1",
            json!({ "title": 7 }),
        ),
        ("DELETE", "/client/calendar/events/primary/ev1", Value::Null),
        (
            "PUT",
            "/client/person/timezone",
            json!({ "addr": "x", "zone": null }),
        ),
    ]
}

async fn call(app: &axum::Router, method: &str, uri: &str, body: &Value) -> (StatusCode, Value) {
    let req = if body.is_null() {
        common::authed(method, uri)
    } else {
        common::authed_json(method, uri, body.clone())
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn stats(app: &axum::Router) -> Value {
    body_json(
        app.clone()
            .oneshot(authed("GET", "/client/stats"))
            .await
            .unwrap(),
    )
    .await
}

/// CALENDAR FOR EVERYONE. With no hidden feature at all, every calendar
/// route reaches its handler, the one write refused for want of a grant is
/// audited as the handler always audits it, and stats say `timeline: true`
/// with the `calendar` object.
#[tokio::test]
async fn the_calendar_routes_are_there_with_no_hidden_feature_at_all() {
    let (state, store, acct) = common::state_with(|_, _| {});
    let app = router(state.with_features(Features::none()));

    for (method, uri, body) in calendar_routes() {
        let (status, answer) = call(&app, method, uri, &body).await;
        assert_ne!(
            answer,
            json!({ "error": "not found" }),
            "{method} {uri} {status}"
        );
        assert_ne!(status, StatusCode::NOT_FOUND, "{method} {uri}: {answer}");
    }
    assert!(
        !store.list_audit(acct, 50).unwrap().is_empty(),
        "the refused writes are audited"
    );
    let on = stats(&app).await;
    assert_eq!(on["timeline"], json!(true));
    assert_eq!(on["calendar"]["granted"], json!(false));
    assert_eq!(on["features"], json!([]));
}

/// A LEFTOVER ROW DECIDES NOTHING. A control plane that still grants the
/// retired `calendar` key passes it through to the app (the daemon passes on
/// every well-formed key), and the page is the same with it, without it, and
/// when it goes away.
#[tokio::test]
async fn a_leftover_calendar_grant_changes_nothing() {
    let (url, control) = spawn_control().await;
    control.answers(200, json!({ "features": ["calendar"] }));
    let state = daemon(Some(&url), Features::none());
    let refresher = state.feature_refresher().unwrap();
    let app = router(state.clone());
    refresher.refresh_once().await;
    assert_eq!(stats_features(&state).await, json!(["calendar"]));
    let (status, body) = call(&app, "GET", "/client/timeline", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(stats(&app).await["timeline"], json!(true));

    control.answers(200, json!({ "features": [] }));
    refresher.refresh_once().await;
    assert_eq!(stats_features(&state).await, json!([]));
    let (status, body) = call(&app, "GET", "/client/timeline", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(stats(&app).await["timeline"], json!(true));
}

/// The bearer still comes first: no credential is a 401 on a calendar route
/// as on every other.
#[tokio::test]
async fn an_unauthenticated_caller_gets_a_401_on_the_calendar() {
    let app = router(daemon(None, Features::none()));
    let resp = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/client/timeline")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
