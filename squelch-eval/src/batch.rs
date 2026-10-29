//! A local Message Batches gateway: the same `POST /v1/messages` the triage
//! agent already calls, answered from the Anthropic Batch API at half price.
//!
//! The agent is a bounded multi-turn loop that makes one Messages call per
//! turn and waits for it. Nothing in it changes for a batch run: the runner
//! points [`run_agent`](squelch_core::triage::agent::run_agent) at this
//! gateway's URL instead of the provider's, starts every case at once, and
//! the gateway holds each call open until its batch has ended.
//!
//! Calls are grouped by quiet time: every case's first turn arrives within a
//! moment of the others (building a context is local), so they go out as one
//! batch; the follow-up turns of the few cases that read evidence arrive
//! together when that batch's results are handed back, and go out as the
//! next. Nothing about the grouping is specific to triage, so any flow that
//! calls the Messages API can be batched the same way.
//!
//! A result maps back to its caller by `custom_id`. A succeeded result is the
//! Messages response itself, verbatim. An errored one is answered with the
//! provider's own status class (an `invalid_request_error` as 400, anything
//! else as 500, so the runner's infra retry takes it); an expired or canceled
//! request as 503; a result missing from the file as 502.

use anyhow::{Context, Result, bail};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, Notify, oneshot};

/// The Messages API version the batch endpoints are called with when the
/// caller did not send one.
const DEFAULT_VERSION: &str = "2023-06-01";

/// What the batch endpoints need from the calls they carry: the key, the API
/// version and any beta header the agent sent with them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Auth {
    api_key: String,
    version: String,
    beta: Option<String>,
}

impl Auth {
    fn from_headers(headers: &HeaderMap) -> Self {
        let get = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        Self {
            api_key: get("x-api-key").unwrap_or_default(),
            version: get("anthropic-version").unwrap_or_else(|| DEFAULT_VERSION.into()),
            beta: get("anthropic-beta"),
        }
    }

    fn apply(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let request = request
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", &self.version);
        match &self.beta {
            Some(beta) => request.header("anthropic-beta", beta),
            None => request,
        }
    }
}

/// One held call.
struct Waiting {
    custom_id: String,
    params: Value,
    auth: Auth,
    reply: oneshot::Sender<(StatusCode, Value)>,
}

/// How the gateway paces itself. Tests shorten both.
#[derive(Debug, Clone, Copy)]
pub struct Pacing {
    /// Submit once no new call has arrived for this long.
    pub quiet: Duration,
    /// How often an in-flight batch's status is read.
    pub poll: Duration,
}

impl Default for Pacing {
    fn default() -> Self {
        Self {
            quiet: Duration::from_secs(2),
            poll: Duration::from_secs(20),
        }
    }
}

/// Counts for the run summary and for tests.
#[derive(Debug, Default)]
pub struct Counters {
    pub batches: AtomicU64,
    pub requests: AtomicU64,
}

struct Shared {
    upstream: String,
    http: reqwest::Client,
    pacing: Pacing,
    waiting: Mutex<Vec<Waiting>>,
    arrived: Notify,
    next_id: AtomicU64,
    counters: Arc<Counters>,
}

/// A running gateway. Dropping it stops the server and its submitter.
pub struct Gateway {
    /// The Messages endpoint to hand the agent.
    pub url: String,
    pub counters: Arc<Counters>,
    server: tokio::task::JoinHandle<()>,
    submitter: tokio::task::JoinHandle<()>,
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.server.abort();
        self.submitter.abort();
    }
}

/// The batch API's base for a Messages endpoint: `https://api.anthropic.com`
/// for `https://api.anthropic.com/v1/messages`.
pub fn upstream_base(messages_url: &str) -> Result<String> {
    let trimmed = messages_url.trim_end_matches('/');
    match trimmed.strip_suffix("/v1/messages") {
        Some(base) if !base.is_empty() => Ok(base.to_owned()),
        _ => bail!(
            "batch mode needs an Anthropic Messages endpoint ending in /v1/messages, not {messages_url}"
        ),
    }
}

/// Start a gateway on a loopback port in front of `upstream` (a base URL, see
/// [`upstream_base`]).
pub async fn start(upstream: String, http: reqwest::Client, pacing: Pacing) -> Result<Gateway> {
    let counters = Arc::new(Counters::default());
    let shared = Arc::new(Shared {
        upstream,
        http,
        pacing,
        waiting: Mutex::new(Vec::new()),
        arrived: Notify::new(),
        next_id: AtomicU64::new(0),
        counters: counters.clone(),
    });
    let app = axum::Router::new()
        .route("/v1/messages", axum::routing::post(hold))
        .with_state(shared.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind the batch gateway")?;
    let url = format!("http://{}/v1/messages", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let submitter = tokio::spawn(submit_loop(shared));
    Ok(Gateway {
        url,
        counters,
        server,
        submitter,
    })
}

/// `POST /v1/messages`: queue the call and answer when its batch has ended.
async fn hold(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Json(mut params): Json<Value>,
) -> Response {
    // A batch request is never streamed.
    if let Some(object) = params.as_object_mut() {
        object.remove("stream");
    }
    let custom_id = format!("r{}", shared.next_id.fetch_add(1, Ordering::Relaxed));
    let (reply, answer) = oneshot::channel();
    shared.waiting.lock().await.push(Waiting {
        custom_id,
        params,
        auth: Auth::from_headers(&headers),
        reply,
    });
    shared.arrived.notify_one();
    match answer.await {
        Ok((status, body)) => (status, Json(body)).into_response(),
        Err(_) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"type": "error", "error": {"type": "api_error", "message": "batch gateway stopped"}})),
        )
            .into_response(),
    }
}

/// Wait for calls, let them settle for [`Pacing::quiet`], then send all that
/// are waiting as one batch per credential. Each batch is followed in its own
/// task, so calls that arrive meanwhile form the next one.
async fn submit_loop(shared: Arc<Shared>) {
    loop {
        shared.arrived.notified().await;
        loop {
            let settled =
                tokio::time::timeout(shared.pacing.quiet, shared.arrived.notified()).await;
            if settled.is_err() {
                break;
            }
        }
        let calls = std::mem::take(&mut *shared.waiting.lock().await);
        let mut by_auth: HashMap<Auth, Vec<Waiting>> = HashMap::new();
        for call in calls {
            by_auth.entry(call.auth.clone()).or_default().push(call);
        }
        for (auth, calls) in by_auth {
            tokio::spawn(follow(shared.clone(), auth, calls));
        }
    }
}

/// Create one batch, wait for it to end, and answer every call in it.
async fn follow(shared: Arc<Shared>, auth: Auth, calls: Vec<Waiting>) {
    let mut replies: HashMap<String, oneshot::Sender<(StatusCode, Value)>> = HashMap::new();
    let mut requests = Vec::with_capacity(calls.len());
    for call in calls {
        requests.push(json!({"custom_id": call.custom_id, "params": call.params}));
        replies.insert(call.custom_id, call.reply);
    }
    shared.counters.batches.fetch_add(1, Ordering::Relaxed);
    shared
        .counters
        .requests
        .fetch_add(requests.len() as u64, Ordering::Relaxed);
    let outcome = run_batch(&shared, &auth, requests).await;
    match outcome {
        Ok(mut results) => {
            for (custom_id, reply) in replies {
                let answer = results.remove(&custom_id).unwrap_or_else(|| {
                    gateway_error(StatusCode::BAD_GATEWAY, "result missing from the batch")
                });
                let _ = reply.send(answer);
            }
        }
        Err(error) => {
            let message = format!("batch failed: {error:#}");
            for (_, reply) in replies {
                let _ = reply.send(gateway_error(StatusCode::BAD_GATEWAY, &message));
            }
        }
    }
}

fn gateway_error(status: StatusCode, message: &str) -> (StatusCode, Value) {
    (
        status,
        json!({"type": "error", "error": {"type": "api_error", "message": message}}),
    )
}

/// Submit, poll until `ended`, read the results file: `custom_id` to the
/// answer its caller gets.
async fn run_batch(
    shared: &Shared,
    auth: &Auth,
    requests: Vec<Value>,
) -> Result<HashMap<String, (StatusCode, Value)>> {
    let batches = format!("{}/v1/messages/batches", shared.upstream);
    let created: Value = auth
        .apply(shared.http.post(&batches))
        .json(&json!({"requests": requests}))
        .send()
        .await
        .context("create the batch")?
        .error_for_status()
        .context("create the batch")?
        .json()
        .await
        .context("read the created batch")?;
    let id = created["id"]
        .as_str()
        .context("the created batch has no id")?
        .to_owned();
    let mut status = created;
    while status["processing_status"] != "ended" {
        tokio::time::sleep(shared.pacing.poll).await;
        status = auth
            .apply(shared.http.get(format!("{batches}/{id}")))
            .send()
            .await
            .context("read the batch status")?
            .error_for_status()
            .context("read the batch status")?
            .json()
            .await
            .context("parse the batch status")?;
    }
    let results_url = match status["results_url"].as_str() {
        Some(url) => url.to_owned(),
        None => format!("{batches}/{id}/results"),
    };
    let text = auth
        .apply(shared.http.get(&results_url))
        .send()
        .await
        .context("read the batch results")?
        .error_for_status()
        .context("read the batch results")?
        .text()
        .await
        .context("read the batch results")?;
    let mut answers = HashMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let row: Value = serde_json::from_str(line).context("parse a batch result line")?;
        let Some(custom_id) = row["custom_id"].as_str() else {
            continue;
        };
        answers.insert(custom_id.to_owned(), answer_for(&row["result"]));
    }
    Ok(answers)
}

/// A batch result as the Messages endpoint would have answered it.
fn answer_for(result: &Value) -> (StatusCode, Value) {
    match result["type"].as_str() {
        Some("succeeded") => (StatusCode::OK, result["message"].clone()),
        Some("errored") => {
            let error = result["error"].clone();
            let kind = error["error"]["type"]
                .as_str()
                .or_else(|| error["type"].as_str())
                .unwrap_or("api_error");
            let status = if kind == "invalid_request_error" {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            let body = if error.is_null() {
                json!({"type": "error", "error": {"type": kind, "message": "errored in batch"}})
            } else {
                error
            };
            (status, body)
        }
        Some(other @ ("expired" | "canceled")) => gateway_error(
            StatusCode::SERVICE_UNAVAILABLE,
            &format!("request {other} in the batch"),
        ),
        _ => gateway_error(StatusCode::BAD_GATEWAY, "unrecognized batch result"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::Path;
    use axum::routing::{get, post};

    /// A Batch API stand-in: records each batch's requests and answers every
    /// request with a message that echoes its `custom_id`, except those whose
    /// params name a planted outcome.
    #[derive(Default)]
    struct Upstream {
        batches: std::sync::Mutex<Vec<Vec<Value>>>,
        headers: std::sync::Mutex<Vec<(String, String, Option<String>)>>,
    }

    async fn serve_upstream() -> (String, Arc<Upstream>) {
        let state = Arc::new(Upstream::default());
        let create = |State(s): State<Arc<Upstream>>,
                      headers: HeaderMap,
                      Json(body): Json<Value>| async move {
            let get = |n: &str| {
                headers
                    .get(n)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned)
            };
            s.headers.lock().unwrap().push((
                get("x-api-key").unwrap_or_default(),
                get("anthropic-version").unwrap_or_default(),
                get("anthropic-beta"),
            ));
            let mut batches = s.batches.lock().unwrap();
            batches.push(body["requests"].as_array().unwrap().clone());
            Json(
                json!({"id": format!("b{}", batches.len() - 1), "processing_status": "in_progress"}),
            )
        };
        let status = |Path(id): Path<String>| async move {
            Json(json!({"id": id, "processing_status": "ended"}))
        };
        let results = |State(s): State<Arc<Upstream>>, Path(id): Path<String>| async move {
            let index: usize = id.trim_start_matches('b').parse().unwrap();
            let batch = s.batches.lock().unwrap()[index].clone();
            let lines: Vec<String> = batch
                .iter()
                .filter(|r| r["params"]["plant"] != "missing")
                .map(|r| {
                    let custom_id = r["custom_id"].clone();
                    let result = match r["params"]["plant"].as_str() {
                        Some("invalid") => json!({"type": "errored", "error": {"type": "error",
                            "error": {"type": "invalid_request_error", "message": "bad"}}}),
                        Some("overloaded") => json!({"type": "errored", "error": {"type": "error",
                            "error": {"type": "overloaded_error", "message": "busy"}}}),
                        Some("expired") => json!({"type": "expired"}),
                        _ => json!({"type": "succeeded", "message": {
                            "id": custom_id, "model": r["params"]["model"],
                            "stream_was": r["params"].get("stream").cloned().unwrap_or(Value::Null),
                            "content": [{"type": "text", "text": r["params"]["say"]}]}}),
                    };
                    json!({"custom_id": custom_id, "result": result}).to_string()
                })
                .collect();
            lines.join("\n")
        };
        let app = axum::Router::new()
            .route("/v1/messages/batches", post(create))
            .route("/v1/messages/batches/{id}", get(status))
            .route("/v1/messages/batches/{id}/results", get(results))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, state)
    }

    fn fast() -> Pacing {
        Pacing {
            quiet: Duration::from_millis(150),
            poll: Duration::from_millis(10),
        }
    }

    async fn call(url: &str, params: Value) -> (StatusCode, Value) {
        let resp = reqwest::Client::new()
            .post(url)
            .header("x-api-key", "k-test")
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "extended-cache-ttl-2025-04-11")
            .json(&params)
            .send()
            .await
            .unwrap();
        let status = StatusCode::from_u16(resp.status().as_u16()).unwrap();
        (status, resp.json().await.unwrap())
    }

    #[test]
    fn the_batch_base_is_the_messages_endpoint_without_its_path() {
        assert_eq!(
            upstream_base("https://api.anthropic.com/v1/messages").unwrap(),
            "https://api.anthropic.com"
        );
        assert_eq!(
            upstream_base("http://127.0.0.1:9/v1/messages/").unwrap(),
            "http://127.0.0.1:9"
        );
        assert!(upstream_base("https://api.openai.com/v1/chat/completions").is_err());
    }

    /// Calls that arrive together go out as one batch; the answers find their
    /// callers; a wave that arrives after the first batch ends is a second.
    #[tokio::test]
    async fn concurrent_calls_share_a_batch_and_each_gets_its_own_answer() {
        let (base, upstream) = serve_upstream().await;
        let gateway = start(base, reqwest::Client::new(), fast()).await.unwrap();
        let first: Vec<_> = (0..12)
            .map(|i| {
                let url = gateway.url.clone();
                tokio::spawn(async move {
                    call(&url, json!({"model": "claude-opus-5-5", "say": format!("case {i}"), "stream": true})).await
                })
            })
            .collect();
        for (i, handle) in first.into_iter().enumerate() {
            let (status, body) = handle.await.unwrap();
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["content"][0]["text"], format!("case {i}"));
            assert_eq!(body["model"], "claude-opus-5-5");
            assert_eq!(body["stream_was"], Value::Null, "stream is stripped");
        }
        assert_eq!(upstream.batches.lock().unwrap().len(), 1);
        assert_eq!(upstream.batches.lock().unwrap()[0].len(), 12);
        // The follow-up turns: a second wave, a second batch.
        let (status, body) = call(&gateway.url, json!({"model": "m", "say": "turn 2"})).await;
        assert_eq!(
            (status, body["content"][0]["text"].clone()),
            (StatusCode::OK, json!("turn 2"))
        );
        assert_eq!(upstream.batches.lock().unwrap().len(), 2);
        assert_eq!(gateway.counters.batches.load(Ordering::Relaxed), 2);
        assert_eq!(gateway.counters.requests.load(Ordering::Relaxed), 13);
        // The credential and headers the agent sent are what the batch used.
        let headers = upstream.headers.lock().unwrap().clone();
        assert!(headers.iter().all(|(key, version, beta)| key == "k-test"
            && version == "2023-06-01"
            && beta.as_deref() == Some("extended-cache-ttl-2025-04-11")));
    }

    /// Failures come back in the status class the runner already handles: a
    /// request the API refused is a 400; anything retryable a 5xx.
    #[tokio::test]
    async fn failed_results_map_to_the_status_a_live_call_would_have_had() {
        let (base, _) = serve_upstream().await;
        let gateway = start(base, reqwest::Client::new(), fast()).await.unwrap();
        let mut handles = Vec::new();
        for plant in ["invalid", "overloaded", "expired", "missing", "ok"] {
            let url = gateway.url.clone();
            handles.push((
                plant,
                tokio::spawn(async move {
                    call(&url, json!({"model": "m", "plant": plant, "say": "x"})).await
                }),
            ));
        }
        for (plant, handle) in handles {
            let (status, body) = handle.await.unwrap();
            let expected = match plant {
                "invalid" => StatusCode::BAD_REQUEST,
                "overloaded" => StatusCode::INTERNAL_SERVER_ERROR,
                "expired" => StatusCode::SERVICE_UNAVAILABLE,
                "missing" => StatusCode::BAD_GATEWAY,
                _ => StatusCode::OK,
            };
            assert_eq!(status, expected, "{plant}: {body}");
        }
    }

    /// An unreachable batch API fails every held call, rather than holding it
    /// forever.
    #[tokio::test]
    async fn an_unreachable_batch_api_fails_the_held_calls() {
        let gateway = start("http://127.0.0.1:9".into(), reqwest::Client::new(), fast())
            .await
            .unwrap();
        let (status, body) = call(&gateway.url, json!({"model": "m", "say": "x"})).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("batch failed"),
            "{body}"
        );
    }
}
