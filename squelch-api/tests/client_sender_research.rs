//! `/client/person/research`, the contact card's "Who is this?"
//! (docs/SENDER-RESEARCH.md): GET is a store read, POST runs research once
//! against a loopback provider that counts its requests, and a second POST
//! inside the cache window spends nothing. Every sender is invented.
mod common;

use axum::{Json, Router, routing::post};
use common::*;
use serde_json::{Value, json};
use squelch_core::config::{ResearchConfig, Stage2Provider};
use squelch_core::triage::research::{self, ResearchClient};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt;

const SENDER: &str = "pat@lumen.example";

async fn provider() -> (String, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let app = Router::new().route(
        "/",
        post(move |Json(_): Json<Value>| {
            count.fetch_add(1, Ordering::SeqCst);
            async move {
                Json(json!({
                    "stop_reason": "tool_use",
                    "content": [
                        {"type": "web_search_tool_result", "tool_use_id": "s1", "content": [
                            {"type": "web_search_result", "url": "https://lumen.example/about", "title": "About"}
                        ]},
                        {"type": "tool_use", "id": "t1", "name": research::SUBMIT_TOOL, "input": {
                            "who": "Nothing ties this address to a person at Lumen Labs.",
                            "organization": {"name": "Lumen Labs", "website": "https://lumen.example",
                                             "what_it_does": null, "size_hint": null},
                            "checks": [{"claim": "Works at Lumen Labs", "status": "not_found"}],
                            "verdict": "unverified",
                            "sources": ["https://lumen.example/about"]
                        }}
                    ],
                    "usage": {"input_tokens": 50, "output_tokens": 20,
                              "server_tool_use": {"web_search_requests": 1}}
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, calls)
}

#[tokio::test]
async fn the_card_reads_and_runs_research_once() {
    let (url, calls) = provider().await;
    let (state, _store, _acct) = state_with(|_, _| {});
    let client = ResearchClient::new(
        reqwest::Client::new(),
        url,
        "test-key",
        Stage2Provider::Anthropic,
        "test-model",
        &ResearchConfig::default(),
        std::time::Duration::from_secs(30),
    );
    let app =
        squelch_api::router(state.with_sender_research(Some(client), ResearchConfig::default()));
    let uri = format!(
        "/client/person/research?addr={}",
        SENDER.replace('@', "%40")
    );

    let before = body_json(app.clone().oneshot(authed("GET", &uri)).await.unwrap()).await;
    assert_eq!(before["status"], "none");
    assert_eq!(before["available"], true);

    let ran = body_json(app.clone().oneshot(authed("POST", &uri)).await.unwrap()).await;
    assert_eq!(ran["status"], "done", "{ran}");
    assert_eq!(ran["result"]["verdict"], "unverified");
    assert_eq!(
        ran["result"]["sources"],
        json!(["https://lumen.example/about"])
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Inside the cache window: answered from the store, nothing spent.
    let again = body_json(app.clone().oneshot(authed("POST", &uri)).await.unwrap()).await;
    assert_eq!(again["status"], "done");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let read = body_json(app.oneshot(authed("GET", &uri)).await.unwrap()).await;
    assert_eq!(read["result"], ran["result"]);
}

#[tokio::test]
async fn without_a_model_the_button_runs_nothing_and_says_so() {
    let h = harness(|_, _| {});
    let uri = format!(
        "/client/person/research?addr={}",
        SENDER.replace('@', "%40")
    );
    let resp = body_json(h.app.clone().oneshot(authed("POST", &uri)).await.unwrap()).await;
    assert_eq!(resp["status"], "none");
    assert_eq!(resp["available"], false);
    let bad = h
        .app
        .oneshot(authed("GET", "/client/person/research?addr=nobody"))
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
}

#[tokio::test]
async fn the_route_needs_the_clients_token() {
    let h = harness(|_, _| {});
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/client/person/research?addr=pat%40lumen.example")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = h.app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 401);
}
