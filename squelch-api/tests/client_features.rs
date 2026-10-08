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

// --- the `calendar` flag -----------------------------------------------------

/// Every route behind the Calendar page, with a body that is WRONG on
/// purpose where a body is read: a route that is on answers it (a 400, a
/// 403, a 409), and a route that is off answers 404 without reading it.
fn calendar_routes() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        ("GET", "/client/timeline", Value::Null),
        ("PATCH", "/client/timeline/ghosts/1", json!({ "state": 7 })),
        ("POST", "/client/timeline/refresh", Value::Null),
        ("POST", "/client/calendar/connect", Value::Null),
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

/// OFF IS AS IF THE FEATURE DID NOT EXIST. Every calendar route answers the
/// opaque 404 before it reads a body, a refused write leaves no audit row,
/// and stats say `timeline: false` with no `calendar` object. ON, the same
/// requests reach their handlers, and the one write that is refused for want
/// of a grant is audited as the handler always audits it.
#[tokio::test]
async fn the_calendar_routes_are_not_there_until_the_flag_is_on() {
    let features = Features::none();
    let (state, store, acct) = common::state_with(|_, _| {});
    let app = router(state.with_features(features.clone()));

    for (method, uri, body) in calendar_routes() {
        let (status, answer) = call(&app, method, uri, &body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}: {answer}");
        assert_eq!(answer, json!({ "error": "not found" }), "{method} {uri}");
    }
    assert!(
        store.list_audit(acct, 50).unwrap().is_empty(),
        "a write to a feature that is off is not an attempt at anything"
    );
    let off = stats(&app).await;
    assert_eq!(off["timeline"], json!(false));
    assert!(off.get("calendar").is_none(), "{off}");

    // Flipped under the same router, with no restart.
    features.replace_granted(["calendar"]);
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
        "with the flag on, the refused writes are audited as before"
    );
    let on = stats(&app).await;
    assert_eq!(on["timeline"], json!(true));
    assert_eq!(on["calendar"]["granted"], json!(false));

    // And back off.
    features.replace_granted(Vec::<String>::new());
    let (status, _) = call(&app, "GET", "/client/timeline", &Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(stats(&app).await["timeline"], json!(false));
}

/// The whole path, live: the operator ticks the box, the daemon's next
/// refresh opens the routes, and unticking it closes them again.
#[tokio::test]
async fn a_ticked_calendar_box_opens_the_page_on_the_next_refresh() {
    let (url, control) = spawn_control().await;
    control.answers(200, json!({ "features": [] }));
    let state = daemon(Some(&url), Features::none());
    let refresher = state.feature_refresher().unwrap();
    let app = router(state.clone());
    refresher.refresh_once().await;
    let (status, _) = call(&app, "GET", "/client/timeline", &Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    control.answers(200, json!({ "features": ["calendar"] }));
    refresher.refresh_once().await;
    let (status, body) = call(&app, "GET", "/client/timeline", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(stats(&app).await["timeline"], json!(true));

    control.answers(200, json!({ "features": [] }));
    refresher.refresh_once().await;
    let (status, _) = call(&app, "GET", "/client/timeline", &Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The bearer still comes first: no credential is a 401 whatever the flag
/// says, so a stranger learns nothing about it.
#[tokio::test]
async fn an_unauthenticated_caller_gets_a_401_flag_or_no_flag() {
    for features in [Features::none(), Features::pinned(["calendar"])] {
        let app = router(daemon(None, features));
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
}
