//! Hidden features on the daemon: what the control plane grants reaches
//! `/client/stats`, and what happens to it when the control plane goes quiet
//! or says no.

use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::get;
use serde_json::{Value, json};
use squelch_api::{ApiState, FEATURE_REFRESH, router};
use squelch_core::features::Features;
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

    for (status, body) in [(500, json!({})), (200, json!("not the shape"))] {
        control.answers(status, body);
        assert!(refresher.refresh_once().await < FEATURE_REFRESH);
        assert_eq!(stats_features(&state).await, json!(["calendar"]));
    }
}

/// A refusal IS a decision: the control plane no longer vouches for this
/// daemon, and off is the answer to every doubt. A 404 is a control plane too
/// old to have granted anything. What was pinned locally survives both.
#[tokio::test]
async fn a_refusal_clears_what_was_granted_and_leaves_what_was_pinned() {
    for status in [401, 404] {
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
