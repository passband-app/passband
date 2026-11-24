//! Background triage at half price: the Anthropic Message Batches API behind
//! the same Messages call the agent already makes.
//!
//! A background investigation (the signup backfill, the cutover, a window
//! re-triage, a refresh of mail already classified) has nobody waiting on it,
//! and it is most of what triage costs: a new mailbox's first week is a
//! backlog of hundreds of messages. The batch API bills the same request at
//! half the price and answers within minutes to hours instead of seconds.
//! Nothing about the request changes (same model, prompt, schema, effort),
//! so the decision is the one the synchronous call would have made.
//!
//! [`Batcher::send`] holds a call until its batch has ended and answers it
//! with the Messages response the synchronous endpoint would have returned.
//! Calls are grouped by quiet time: the batch lane claims its jobs together,
//! so their first turns arrive within a moment of each other and go out as
//! one batch, and the few follow-up turns arrive together when that batch's
//! results come back. A batch shares its cached prompt prefix across its
//! requests (measured: one write, then a read on every other request), but
//! not with the synchronous calls, so batches should be large.
//!
//! Failure is never worse than not batching:
//! - A batch the endpoint will not create (a 4xx: a gateway without the
//!   route, a key not allowed to batch) turns batching off for the process
//!   and answers [`BatchError::Unavailable`], which the caller takes as "send
//!   it synchronously". The live pass then takes background work back.
//! - A batch that has not ended within [`Pacing::max_wait`] is cancelled,
//!   and its calls answer as retryable, like a provider outage.
//! - A daemon that stops mid-batch loses the batch; its jobs' leases expire
//!   and they are claimed again. That batch is paid for twice, at most once
//!   per restart.
//!
//! Through a Bifrost gateway the virtual key rides `x-bf-vk` as it does on
//! every call, the provider prefix the gateway needs on a model id is
//! stripped from each request's params (the gateway passes batch params to
//! Anthropic as they are), and the results come back reshaped as one JSON
//! object rather than JSONL; both shapes are read. The gateway does not
//! charge batch spend to the tenant's budget, so the daemon's own daily run
//! caps are what bound it.
//!
//! REDACTION: never log a request, a response body or a key.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify, oneshot};

/// How a [`Batcher`] paces itself. Tests shorten all three.
#[derive(Debug, Clone, Copy)]
pub struct Pacing {
    /// Submit once no new call has arrived for this long.
    pub quiet: Duration,
    /// How often an in-flight batch's status is read.
    pub poll: Duration,
    /// A batch not ended by then is cancelled and its calls answer as
    /// retryable. The batch lane sizes its leases and deadlines from it.
    pub max_wait: Duration,
}

impl Default for Pacing {
    fn default() -> Self {
        Self {
            quiet: Duration::from_secs(3),
            poll: Duration::from_secs(20),
            max_wait: Duration::from_secs(3600),
        }
    }
}

/// Why [`Batcher::send`] did not answer with a Messages response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchError {
    /// Batching is off (or just turned itself off): send the call
    /// synchronously instead.
    Unavailable,
    /// The batch itself failed (`batch_timeout`, `batch_unreachable`, ...):
    /// not the provider answering this request, so the caller retries the
    /// job without opening the provider cooldown that pauses live triage.
    Failed(String),
}

/// The endpoint and credential a call is batched under. Calls batch together
/// only when all of it matches.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Target {
    /// The API base, `https://api.anthropic.com` for its Messages endpoint.
    pub base: String,
    /// Headers every batch call carries: the key, the version, a gateway's
    /// virtual-key header.
    pub headers: Vec<(&'static str, String)>,
}

/// The base for a Messages endpoint: `https://api.anthropic.com` for
/// `https://api.anthropic.com/v1/messages`. `None` for anything else.
pub fn base_of(messages_url: &str) -> Option<String> {
    let trimmed = messages_url.trim_end_matches('/');
    trimmed
        .strip_suffix("/v1/messages")
        .filter(|base| !base.is_empty())
        .map(str::to_owned)
}

struct Waiting {
    custom_id: String,
    params: Value,
    target: Target,
    reply: oneshot::Sender<Answer>,
}

/// A call's answer: the Messages status and body, or "send it yourself".
type Answer = Result<(u16, Value), BatchError>;

/// Counts for logs and tests.
#[derive(Debug, Default)]
pub struct Counters {
    pub batches: AtomicU64,
    pub requests: AtomicU64,
}

/// The process's batcher. Cheap to share; its submitter starts on first use.
pub struct Batcher {
    http: reqwest::Client,
    pacing: Pacing,
    waiting: Mutex<Vec<Waiting>>,
    arrived: Notify,
    next_id: AtomicU64,
    started: AtomicBool,
    disabled: AtomicBool,
    pub counters: Counters,
}

impl Batcher {
    pub fn new(http: reqwest::Client, pacing: Pacing) -> Arc<Self> {
        Arc::new(Self {
            http,
            pacing,
            waiting: Mutex::new(Vec::new()),
            arrived: Notify::new(),
            next_id: AtomicU64::new(0),
            started: AtomicBool::new(false),
            disabled: AtomicBool::new(false),
            counters: Counters::default(),
        })
    }

    pub fn pacing(&self) -> Pacing {
        self.pacing
    }

    /// False once an endpoint refused to create a batch: every call since
    /// answers [`BatchError::Unavailable`], and the batch lane stands down.
    pub fn available(&self) -> bool {
        !self.disabled.load(Ordering::Relaxed)
    }

    fn disable(&self, why: &str) {
        if !self.disabled.swap(true, Ordering::Relaxed) {
            eprintln!(
                "squelch: background triage batching is off for this run ({why}); \
                 background work runs synchronously"
            );
        }
    }

    /// Hold one Messages call until its batch has ended, and answer it as
    /// the Messages endpoint would have: `(status, body)`.
    pub async fn send(self: &Arc<Self>, target: Target, mut params: Value) -> Answer {
        if !self.available() {
            return Err(BatchError::Unavailable);
        }
        if let Some(object) = params.as_object_mut() {
            // A batch request is never streamed, and the gateway passes the
            // params through to Anthropic, which wants a bare model id.
            object.remove("stream");
            if let Some(Value::String(model)) = object.get_mut("model")
                && let Some(bare) = model.strip_prefix(super::llm::GATEWAY_PROVIDER_PREFIX)
            {
                *model = bare.to_owned();
            }
        }
        if !self.started.swap(true, Ordering::AcqRel) {
            tokio::spawn(submit_loop(self.clone()));
        }
        let custom_id = format!("r{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let (reply, answer) = oneshot::channel();
        self.waiting.lock().await.push(Waiting {
            custom_id,
            params,
            target,
            reply,
        });
        self.arrived.notify_one();
        answer
            .await
            .unwrap_or_else(|_| Err(BatchError::Failed("batch_stopped".into())))
    }
}

/// Wait for calls, let them settle for [`Pacing::quiet`], then send every
/// waiting call as one batch per target. Each batch is followed in its own
/// task, so calls that arrive meanwhile form the next one.
async fn submit_loop(batcher: Arc<Batcher>) {
    loop {
        batcher.arrived.notified().await;
        while tokio::time::timeout(batcher.pacing.quiet, batcher.arrived.notified())
            .await
            .is_ok()
        {}
        let calls = std::mem::take(&mut *batcher.waiting.lock().await);
        let mut by_target: HashMap<Target, Vec<Waiting>> = HashMap::new();
        for call in calls {
            by_target.entry(call.target.clone()).or_default().push(call);
        }
        for (target, calls) in by_target {
            tokio::spawn(follow(batcher.clone(), target, calls));
        }
    }
}

/// Create one batch, wait for it to end, and answer every call in it.
async fn follow(batcher: Arc<Batcher>, target: Target, calls: Vec<Waiting>) {
    if !batcher.available() {
        for call in calls {
            let _ = call.reply.send(Err(BatchError::Unavailable));
        }
        return;
    }
    let mut replies = HashMap::with_capacity(calls.len());
    let mut requests = Vec::with_capacity(calls.len());
    for call in calls {
        requests.push(json!({"custom_id": call.custom_id, "params": call.params}));
        replies.insert(call.custom_id, call.reply);
    }
    batcher.counters.batches.fetch_add(1, Ordering::Relaxed);
    batcher
        .counters
        .requests
        .fetch_add(requests.len() as u64, Ordering::Relaxed);
    match run_batch(&batcher, &target, requests).await {
        Ok(mut answers) => {
            for (custom_id, reply) in replies {
                let answer = answers
                    .remove(&custom_id)
                    .ok_or_else(|| BatchError::Failed("batch_result_missing".into()));
                let _ = reply.send(answer);
            }
        }
        Err(Failure::Refused(why)) => {
            batcher.disable(&why);
            for (_, reply) in replies {
                let _ = reply.send(Err(BatchError::Unavailable));
            }
        }
        Err(Failure::Transient(kind)) => {
            for (_, reply) in replies {
                let _ = reply.send(Err(BatchError::Failed(kind.clone())));
            }
        }
    }
}

enum Failure {
    /// The endpoint will not batch at all (a 4xx on create).
    Refused(String),
    /// This batch failed (transport, a 5xx, a timeout): its calls answer
    /// [`BatchError::Failed`] with this kind.
    Transient(String),
}

fn with_headers(request: reqwest::RequestBuilder, target: &Target) -> reqwest::RequestBuilder {
    target
        .headers
        .iter()
        .fold(request, |request, (name, value)| {
            request.header(*name, value)
        })
}

async fn get_json(batcher: &Batcher, target: &Target, url: &str) -> Result<Value, Failure> {
    let response = with_headers(batcher.http.get(url), target)
        .send()
        .await
        .map_err(|_| Failure::Transient("batch_unreachable".into()))?;
    let status = response.status().as_u16();
    if !response.status().is_success() {
        return Err(Failure::Transient(format!("batch_status_http_{status}")));
    }
    response
        .json()
        .await
        .map_err(|_| Failure::Transient("batch_unreadable".into()))
}

async fn run_batch(
    batcher: &Batcher,
    target: &Target,
    requests: Vec<Value>,
) -> Result<HashMap<String, (u16, Value)>, Failure> {
    let batches = format!("{}/v1/messages/batches", target.base);
    let response = with_headers(batcher.http.post(&batches), target)
        .header("content-type", "application/json")
        .json(&json!({"requests": requests}))
        .send()
        .await
        .map_err(|_| Failure::Transient("batch_unreachable".into()))?;
    let status = response.status().as_u16();
    if (400..500).contains(&status) && status != 429 {
        return Err(Failure::Refused(format!("batch create http_{status}")));
    }
    if !response.status().is_success() {
        return Err(Failure::Transient(format!("batch_create_http_{status}")));
    }
    let created: Value = response
        .json()
        .await
        .map_err(|_| Failure::Transient("batch_unreadable".into()))?;
    let Some(id) = created["id"].as_str().map(str::to_owned) else {
        return Err(Failure::Refused("batch create returned no id".into()));
    };
    let started = Instant::now();
    let mut state = created;
    while !ended(&state) {
        if started.elapsed() >= batcher.pacing.max_wait {
            let _ = with_headers(batcher.http.post(format!("{batches}/{id}/cancel")), target)
                .send()
                .await;
            return Err(Failure::Transient("batch_timeout".into()));
        }
        tokio::time::sleep(batcher.pacing.poll).await;
        state = get_json(batcher, target, &format!("{batches}/{id}")).await?;
    }
    // Always the base's own results route, never `results_url`: through a
    // gateway that URL is Anthropic's, where the virtual key means nothing.
    let response = with_headers(batcher.http.get(format!("{batches}/{id}/results")), target)
        .send()
        .await
        .map_err(|_| Failure::Transient("batch_unreachable".into()))?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        return Err(Failure::Transient(format!("batch_results_http_{status}")));
    }
    let text = response
        .text()
        .await
        .map_err(|_| Failure::Transient("batch_unreadable".into()))?;
    Ok(parse_results(&text))
}

/// Anthropic says `processing_status: ended`; a gateway may say `status`.
fn ended(state: &Value) -> bool {
    matches!(
        state["processing_status"]
            .as_str()
            .or_else(|| state["status"].as_str()),
        Some("ended" | "completed")
    )
}

/// `custom_id` to the answer its caller gets, from either results shape:
/// Anthropic's JSONL, one `{custom_id, result}` per line, or a gateway's one
/// object `{"results": [{custom_id, result, error}]}`.
pub fn parse_results(text: &str) -> HashMap<String, (u16, Value)> {
    let items: Vec<Value> = match serde_json::from_str::<Value>(text.trim()) {
        Ok(Value::Object(object)) if object.get("results").is_some_and(Value::is_array) => {
            object["results"].as_array().cloned().unwrap_or_default()
        }
        Ok(Value::Array(items)) => items,
        _ => text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect(),
    };
    items
        .iter()
        .filter_map(|item| {
            let custom_id = item["custom_id"].as_str()?.to_owned();
            Some((custom_id, answer_for(item)))
        })
        .collect()
}

/// One result as the Messages endpoint would have answered it.
fn answer_for(item: &Value) -> (u16, Value) {
    let result = &item["result"];
    match result["type"].as_str() {
        Some("succeeded") => (200, result["message"].clone()),
        Some("errored") => {
            let kind = result["error"]["error"]["type"]
                .as_str()
                .or_else(|| result["error"]["type"].as_str())
                .or_else(|| item["error"]["code"].as_str())
                .unwrap_or("api_error");
            // An invalid request is a fact about the request (a 400, as the
            // synchronous call would have said); anything else is the
            // provider's, retried like an outage.
            let status = if kind == "invalid_request_error" {
                400
            } else {
                500
            };
            (
                status,
                json!({"type": "error", "error": {"type": kind, "message": "errored in batch"}}),
            )
        }
        // Not the provider's answer to the request: the batch ran out of time
        // or was cancelled. Status 0 is read as a batch failure by the caller.
        Some(other @ ("expired" | "canceled")) => error_body(0, &format!("batch_{other}")),
        _ => error_body(502, "unrecognized batch result"),
    }
}

fn error_body(status: u16, message: &str) -> (u16, Value) {
    (
        status,
        json!({"type": "error", "error": {"type": "api_error", "message": message}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};

    #[derive(Default)]
    struct Upstream {
        batches: std::sync::Mutex<Vec<Vec<Value>>>,
        vk: std::sync::Mutex<Vec<Option<String>>>,
        refuse: bool,
        gateway_shape: bool,
        never_end: bool,
        cancelled: std::sync::Mutex<u32>,
    }

    async fn serve(upstream: Upstream) -> (String, Arc<Upstream>) {
        let state = Arc::new(upstream);
        let create = |State(s): State<Arc<Upstream>>,
                      headers: HeaderMap,
                      Json(body): Json<Value>| async move {
            if s.refuse {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": {"type": "not_found"}})),
                );
            }
            s.vk.lock().unwrap().push(
                headers
                    .get("x-bf-vk")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned),
            );
            let mut batches = s.batches.lock().unwrap();
            batches.push(body["requests"].as_array().unwrap().clone());
            (
                StatusCode::OK,
                Json(
                    json!({"id": format!("b{}", batches.len() - 1), "processing_status": "in_progress"}),
                ),
            )
        };
        let status = |State(s): State<Arc<Upstream>>, Path(id): Path<String>| async move {
            let state = if s.never_end { "in_progress" } else { "ended" };
            Json(json!({"id": id, "processing_status": state}))
        };
        let cancel = |State(s): State<Arc<Upstream>>, Path(_id): Path<String>| async move {
            *s.cancelled.lock().unwrap() += 1;
            Json(json!({}))
        };
        let results = |State(s): State<Arc<Upstream>>, Path(id): Path<String>| async move {
            let index: usize = id.trim_start_matches('b').parse().unwrap();
            let batch = s.batches.lock().unwrap()[index].clone();
            let items: Vec<Value> = batch
                .iter()
                .map(|r| {
                    let result = match r["params"]["plant"].as_str() {
                        Some("invalid") => json!({"type": "errored", "error": {"type": "error",
                            "error": {"type": "invalid_request_error", "message": "bad"}}}),
                        Some("expired") => json!({"type": "expired"}),
                        _ => json!({"type": "succeeded", "message": {
                            "model": r["params"]["model"],
                            "content": [{"type": "text", "text": r["params"]["say"]}]}}),
                    };
                    json!({"custom_id": r["custom_id"], "result": result})
                })
                .collect();
            if s.gateway_shape {
                Json(json!({"batch_id": id, "results": items})).to_string()
            } else {
                items
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        };
        let app = axum::Router::new()
            .route("/v1/messages/batches", post(create))
            .route("/v1/messages/batches/{id}", get(status))
            .route("/v1/messages/batches/{id}/cancel", post(cancel))
            .route("/v1/messages/batches/{id}/results", get(results))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, state)
    }

    fn fast() -> Pacing {
        Pacing {
            quiet: Duration::from_millis(50),
            poll: Duration::from_millis(20),
            max_wait: Duration::from_secs(5),
        }
    }

    fn target(base: &str) -> Target {
        Target {
            base: base.to_owned(),
            headers: vec![
                ("x-api-key", "k".into()),
                ("anthropic-version", "2023-06-01".into()),
                ("x-bf-vk", "k".into()),
            ],
        }
    }

    #[tokio::test]
    async fn concurrent_calls_share_one_batch_and_get_their_own_answers() {
        for gateway_shape in [false, true] {
            let (base, upstream) = serve(Upstream {
                gateway_shape,
                ..Default::default()
            })
            .await;
            let batcher = Batcher::new(reqwest::Client::new(), fast());
            let calls = (0..5).map(|i| {
                let batcher = batcher.clone();
                let target = target(&base);
                async move {
                    batcher
                        .send(
                            target,
                            json!({"model": "anthropic/claude-opus-5-5", "stream": true, "say": format!("m{i}")}),
                        )
                        .await
                }
            });
            let answers = futures::future::join_all(calls).await;
            for (i, answer) in answers.into_iter().enumerate() {
                let (status, body) = answer.unwrap();
                assert_eq!(status, 200);
                assert_eq!(body["content"][0]["text"], format!("m{i}"));
                // The gateway's provider prefix is stripped: Anthropic gets a bare id.
                assert_eq!(body["model"], "claude-opus-5-5");
            }
            let batches = upstream.batches.lock().unwrap();
            assert_eq!(
                batches.len(),
                1,
                "five calls inside the quiet time are one batch"
            );
            assert!(
                batches[0]
                    .iter()
                    .all(|r| r["params"].get("stream").is_none())
            );
            assert_eq!(upstream.vk.lock().unwrap()[0].as_deref(), Some("k"));
        }
    }

    #[tokio::test]
    async fn errored_and_expired_results_keep_their_status_class() {
        let (base, _) = serve(Upstream::default()).await;
        let batcher = Batcher::new(reqwest::Client::new(), fast());
        let invalid = batcher.send(target(&base), json!({"plant": "invalid"}));
        let expired = batcher.send(target(&base), json!({"plant": "expired"}));
        let (invalid, expired) = tokio::join!(invalid, expired);
        assert_eq!(invalid.unwrap().0, 400);
        assert_eq!(
            expired.unwrap().0,
            0,
            "an expired request is the batch's failure"
        );
    }

    #[tokio::test]
    async fn a_refused_create_turns_batching_off_and_says_send_it_yourself() {
        let (base, _) = serve(Upstream {
            refuse: true,
            ..Default::default()
        })
        .await;
        let batcher = Batcher::new(reqwest::Client::new(), fast());
        assert_eq!(
            batcher.send(target(&base), json!({"say": "x"})).await,
            Err(BatchError::Unavailable)
        );
        assert!(!batcher.available());
        // Every later call is answered at once, without a request.
        assert_eq!(
            batcher.send(target(&base), json!({"say": "y"})).await,
            Err(BatchError::Unavailable)
        );
    }

    #[tokio::test]
    async fn a_batch_past_its_wait_is_cancelled_and_answers_retryable() {
        let (base, upstream) = serve(Upstream {
            never_end: true,
            ..Default::default()
        })
        .await;
        let pacing = Pacing {
            max_wait: Duration::from_millis(200),
            ..fast()
        };
        let batcher = Batcher::new(reqwest::Client::new(), pacing);
        assert_eq!(
            batcher.send(target(&base), json!({"say": "x"})).await,
            Err(BatchError::Failed("batch_timeout".into()))
        );
        assert_eq!(*upstream.cancelled.lock().unwrap(), 1);
        assert!(batcher.available(), "a slow batch is not a refusal");
    }

    #[test]
    fn base_of_reads_only_a_messages_endpoint() {
        assert_eq!(
            base_of("https://api.anthropic.com/v1/messages").as_deref(),
            Some("https://api.anthropic.com")
        );
        assert_eq!(
            base_of("https://gw.example/anthropic/v1/messages/").as_deref(),
            Some("https://gw.example/anthropic")
        );
        assert_eq!(base_of("https://api.openai.com/v1/chat/completions"), None);
    }
}
