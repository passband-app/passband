//! The two listeners: the work on [`protocol::PORT`] (`POST /embed`,
//! `GET /healthz`) and the operator's on [`protocol::METRICS_PORT`]
//! (`GET /healthz`, `GET /metrics`), which no Service carries.
//!
//! Order of checks on `POST /embed`, cheapest refusal first: the bearer token
//! (before a byte of body is read), the body (size, read deadline, shape),
//! readiness, the lane's share. Only then does a request wait for the worker.
//!
//! NO PEER ADDRESS REACHES A HANDLER. The accept loop below hands axum the
//! connection without its `SocketAddr`, so no handler, log line or metric here
//! can name a caller even by mistake: every caller is a tenant pod, and a
//! per-peer anything is a per-tenant anything with one lookup (§4).

use std::future::Future;
use std::io;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder;
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;

use crate::coalescer::{Coalescer, Failure, Lane, Refused};
use crate::log::Log;
use crate::metrics::{LaneLabel, Metrics, Outcome};
use crate::protocol::{self, EmbedRequest, EmbedResponse, ErrorResponse};

/// How long a connection may go without completing a request head, and how
/// long a request body may take to arrive. Both bound how long a slow or
/// half-open caller can hold a socket and a task; neither can hold a queue
/// slot, because nothing is queued until the body is in. Ten seconds is far
/// past a few KB over the pod network and short enough that a stuck peer
/// costs little. The same timer closes an idle keep-alive connection after
/// this long, so a client pooling connections to this service must idle them
/// out sooner (the daemon's client should set its pool idle timeout below
/// it).
pub const READ_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct AppState {
    pub coalescer: Coalescer,
    pub metrics: Arc<Metrics>,
    /// `None` means no authentication (the startup log says so).
    pub token: Option<Arc<str>>,
    pub log: Log,
}

impl AppState {
    pub fn new(
        coalescer: Coalescer,
        metrics: Arc<Metrics>,
        token: Option<Arc<str>>,
        log: Log,
    ) -> Self {
        Self {
            coalescer,
            metrics,
            token,
            log,
        }
    }
}

/// `POST /embed` and `GET /healthz`, for [`protocol::PORT`].
pub fn work_router(state: AppState) -> Router {
    Router::new()
        .route(protocol::EMBED_PATH, post(embed))
        .route(protocol::HEALTH_PATH, get(health))
        .with_state(state)
}

/// `GET /healthz` and `GET /metrics`, for [`protocol::METRICS_PORT`].
pub fn metrics_router(state: AppState) -> Router {
    Router::new()
        .route(protocol::HEALTH_PATH, get(health))
        .route("/metrics", get(metrics))
        .with_state(state)
}

fn error(status: StatusCode, word: &str) -> Response {
    (
        status,
        axum::Json(ErrorResponse {
            error: word.to_string(),
        }),
    )
        .into_response()
}

fn too_many(retry_after_secs: u64) -> Response {
    let mut r = error(StatusCode::TOO_MANY_REQUESTS, protocol::ERR_QUEUE_FULL);
    r.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(retry_after_secs));
    r
}

fn authorized(token: &str, headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(squelch_httpauth::parse_bearer)
        .is_some_and(|presented| squelch_httpauth::ct_eq(presented.as_bytes(), token.as_bytes()))
}

/// Cut `text` to at most [`protocol::MAX_CHARS_PER_TEXT`] characters, on a
/// character boundary. Never a refusal: a search box is where someone pastes
/// anything, and a 400 there is a 500 in the client.
pub fn truncate(text: &mut String) {
    if let Some((cut, _)) = text.char_indices().nth(protocol::MAX_CHARS_PER_TEXT) {
        text.truncate(cut);
    }
}

async fn embed(State(s): State<AppState>, headers: HeaderMap, body: Body) -> Response {
    if let Some(token) = &s.token
        && !authorized(token, &headers)
    {
        s.metrics.record(LaneLabel::None, Outcome::Unauthorized);
        return error(StatusCode::UNAUTHORIZED, protocol::ERR_UNAUTHORIZED);
    }

    let read = axum::body::to_bytes(body, protocol::MAX_BODY_BYTES);
    let bytes = match tokio::time::timeout(READ_TIMEOUT, read).await {
        Ok(Ok(bytes)) => bytes,
        // Too large, or the connection failed mid-body.
        Ok(Err(_)) => {
            s.metrics.record(LaneLabel::None, Outcome::Rejected);
            return error(StatusCode::BAD_REQUEST, protocol::ERR_BAD_REQUEST);
        }
        Err(_) => {
            s.metrics.record(LaneLabel::None, Outcome::Rejected);
            return error(StatusCode::REQUEST_TIMEOUT, protocol::ERR_BAD_REQUEST);
        }
    };
    let Ok(EmbedRequest { mut texts }) = serde_json::from_slice::<EmbedRequest>(&bytes) else {
        s.metrics.record(LaneLabel::None, Outcome::Rejected);
        return error(StatusCode::BAD_REQUEST, protocol::ERR_BAD_REQUEST);
    };
    drop(bytes);

    let n = texts.len();
    let lane = (n > 0).then(|| Lane::of(n));
    if n == 0 || n > protocol::MAX_TEXTS_PER_CALL {
        s.metrics.record(lane, Outcome::Rejected);
        return error(StatusCode::BAD_REQUEST, protocol::ERR_BAD_REQUEST);
    }
    let lane = Lane::of(n);

    if !s.coalescer.is_ready() {
        s.metrics.record(lane, Outcome::NotReady);
        return error(StatusCode::SERVICE_UNAVAILABLE, protocol::ERR_NOT_READY);
    }

    texts.iter_mut().for_each(truncate);

    let reply = match s.coalescer.submit(texts) {
        Ok(rx) => rx,
        Err(Refused::QueueFull(lane)) => {
            s.metrics.record(lane, Outcome::QueueFull);
            return too_many(lane.retry_after_secs());
        }
        // The worker died between the readiness check and here.
        Err(Refused::Stopped) => {
            s.metrics.record(lane, Outcome::NotReady);
            return error(StatusCode::SERVICE_UNAVAILABLE, protocol::ERR_NOT_READY);
        }
    };

    // From here the worker records the outcome: if this future is dropped
    // (the caller hung up), it still knows how the request ended.
    match reply.await {
        Ok(Ok(vectors)) => axum::Json(EmbedResponse {
            dims: protocol::DIMS,
            vectors,
        })
        .into_response(),
        // Waited past the lane's deadline: the service was too busy, which
        // is a 429, not a fault.
        Ok(Err(Failure::Expired)) => too_many(lane.retry_after_secs()),
        Ok(Err(Failure::Inference | Failure::Stopped)) | Err(_) => {
            error(StatusCode::INTERNAL_SERVER_ERROR, protocol::ERR_INTERNAL)
        }
    }
}

async fn health(State(s): State<AppState>) -> Response {
    if s.coalescer.is_ready() {
        (StatusCode::OK, "ok\n").into_response()
    } else {
        error(StatusCode::SERVICE_UNAVAILABLE, protocol::ERR_NOT_READY)
    }
}

async fn metrics(State(s): State<AppState>) -> Response {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        s.metrics.render(s.coalescer.depth()),
    )
        .into_response()
}

/// Serve `app` on `listener` until `shutdown` resolves, then let in-flight
/// requests finish. The accept loop from squelch-api's `serve.rs`, with two
/// differences: the header-read timeout is [`READ_TIMEOUT`], and the peer
/// address is dropped at accept rather than handed to the router.
///
/// HTTP/1 only, because hyper's HTTP/2 server has no header-read timeout and
/// nothing speaks h2c to this service.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    shutdown: impl Future<Output = ()>,
    log: Log,
) -> io::Result<()> {
    let mut builder = Builder::new(TokioExecutor::new()).http1_only();
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(READ_TIMEOUT);
    let graceful = GracefulShutdown::new();
    let mut shutdown = pin!(shutdown);
    let bound = listener
        .local_addr()
        .map_or_else(|_| "?".to_string(), |a| a.to_string());

    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                // The peer address is discarded here, on purpose (module docs).
                Ok((stream, _peer)) => stream,
                Err(e) => {
                    if !matches!(
                        e.kind(),
                        io::ErrorKind::ConnectionRefused
                            | io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::ConnectionReset
                    ) {
                        log.line(&format!("accept error on {bound}: {}", e.kind()));
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    continue;
                }
            },
            () = &mut shutdown => break,
        };
        let conn = builder
            .serve_connection(TokioIo::new(stream), TowerToHyperService::new(app.clone()))
            .into_owned();
        let conn = graceful.watch(conn);
        tokio::spawn(async move {
            let _ = conn.await;
        });
    }

    drop(listener);
    graceful.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;
    use crate::coalescer::tests::{Fake, Gate, vector_for};
    use crate::coalescer::{BACKFILL_SHARE, INTERACTIVE_SHARE};
    use crate::metrics::tests::{assert_closed, has_ipv4_shape};

    const TOKEN: &str = "a-test-token-that-is-long-enough-000";

    struct Harness {
        state: AppState,
        fake: Fake,
        lines: Arc<Mutex<Vec<String>>>,
    }

    async fn harness_with(fake: Fake, token: Option<&str>) -> Harness {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        let log = Log::new(move |l| sink.lock().unwrap().push(l.to_string()));
        let metrics = Arc::new(Metrics::new());
        let (coalescer, loaded) = Coalescer::start(fake.loader(), metrics.clone(), log.clone());
        loaded.await.unwrap().unwrap();
        Harness {
            state: AppState::new(coalescer, metrics, token.map(Arc::from), log),
            fake,
            lines,
        }
    }

    async fn harness(token: Option<&str>) -> Harness {
        harness_with(Fake::default(), token).await
    }

    fn post(body: impl Into<String>, bearer: Option<&str>) -> Request<Body> {
        let mut r =
            Request::post(protocol::EMBED_PATH).header(header::CONTENT_TYPE, "application/json");
        if let Some(b) = bearer {
            r = r.header(header::AUTHORIZATION, format!("Bearer {b}"));
        }
        r.body(Body::from(body.into())).unwrap()
    }

    fn body_for(texts: &[&str]) -> String {
        serde_json::to_string(&EmbedRequest {
            texts: texts.iter().map(|t| t.to_string()).collect(),
        })
        .unwrap()
    }

    async fn call(router: Router, req: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
        let resp = router.oneshot(req).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, headers, bytes.to_vec())
    }

    fn error_word(body: &[u8]) -> String {
        serde_json::from_slice::<ErrorResponse>(body).unwrap().error
    }

    async fn get_path(router: Router, path: &str) -> (StatusCode, Vec<u8>) {
        let (s, _, b) = call(router, Request::get(path).body(Body::empty()).unwrap()).await;
        (s, b)
    }

    #[tokio::test]
    async fn embeds_positionally_and_echoes_dims() {
        let h = harness(None).await;
        let (status, _, body) = call(
            work_router(h.state.clone()),
            post(body_for(&["one", "two", "three"]), None),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let resp: EmbedResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(resp.dims, protocol::DIMS);
        assert_eq!(
            resp.vectors,
            vec![vector_for("one"), vector_for("two"), vector_for("three")]
        );
        assert!(resp.vectors.iter().all(|v| v.len() == protocol::DIMS));
    }

    #[tokio::test]
    async fn over_length_text_is_truncated_not_refused() {
        let h = harness(None).await;
        // Two-byte characters, so a byte-index cut would land mid-character.
        let long = "é".repeat(protocol::MAX_CHARS_PER_TEXT + 100);
        let (status, _, _) = call(
            work_router(h.state.clone()),
            post(body_for(&[&long, "short"]), None),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let seen = h.fake.seen();
        assert_eq!(seen[0][0].chars().count(), protocol::MAX_CHARS_PER_TEXT);
        assert_eq!(seen[0][1], "short");

        // Exactly at the bound is untouched.
        let mut at = "a".repeat(protocol::MAX_CHARS_PER_TEXT);
        truncate(&mut at);
        assert_eq!(at.len(), protocol::MAX_CHARS_PER_TEXT);
    }

    #[tokio::test]
    async fn malformed_requests_are_bad_request() {
        let h = harness(None).await;
        let nine: Vec<&str> = vec!["t"; protocol::MAX_TEXTS_PER_CALL + 1];
        let huge = format!(
            "{{\"texts\":[\"{}\"]}}",
            "a".repeat(protocol::MAX_BODY_BYTES)
        );
        for body in [
            body_for(&[]),
            body_for(&nine),
            r#"{"texts":["a"],"tenant":"acme"}"#.to_string(),
            "not json".to_string(),
            r#"{"texts":"a"}"#.to_string(),
            huge,
        ] {
            let (status, _, resp) = call(work_router(h.state.clone()), post(body, None)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(error_word(&resp), protocol::ERR_BAD_REQUEST);
        }
        assert!(h.fake.seen().is_empty());
    }

    /// A dead inference thread is a 500 for whoever was queued and a 503 for
    /// everyone after, never a hang.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dead_worker_is_500_for_the_queue_and_503_after() {
        let gate = Gate::closed();
        let h = harness_with(
            Fake {
                gate: Some(gate.clone()),
                ..Default::default()
            },
            None,
        )
        .await;
        let router = work_router(h.state.clone());
        let hold = tokio::spawn(call(router.clone(), post(body_for(&["hold"]), None)));
        let g = gate.clone();
        tokio::task::spawn_blocking(move || g.wait_for_arrivals(1))
            .await
            .unwrap();
        let first = tokio::spawn(call(router.clone(), post(body_for(&["a", "b"]), None)));
        let second = tokio::spawn(call(router.clone(), post(body_for(&["c", "d"]), None)));
        until_depth(&h.state, [0, 4]).await;
        h.state.coalescer.kill_worker();
        gate.open();
        assert_eq!(hold.await.unwrap().0, StatusCode::OK);
        for p in [first, second] {
            let (status, _, body) = tokio::time::timeout(Duration::from_secs(5), p)
                .await
                .expect("answered, not left hanging")
                .unwrap();
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(error_word(&body), protocol::ERR_INTERNAL);
        }
        tokio::time::timeout(Duration::from_secs(5), h.state.coalescer.died())
            .await
            .unwrap();
        let (status, _, body) = call(router.clone(), post(body_for(&["e"]), None)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error_word(&body), protocol::ERR_NOT_READY);
        assert_eq!(
            get_path(metrics_router(h.state.clone()), "/healthz")
                .await
                .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    /// A request that waited past its lane's deadline is a 429 with that
    /// lane's Retry-After: busy, not broken.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_request_past_its_deadline_is_429() {
        let gate = Gate::closed();
        let fake = Fake {
            gate: Some(gate.clone()),
            ..Default::default()
        };
        let metrics = Arc::new(Metrics::new());
        let (coalescer, loaded) = Coalescer::start_with(
            fake.loader(),
            metrics.clone(),
            Log::new(|_| {}),
            [Duration::from_millis(100), Duration::from_millis(100)],
        );
        loaded.await.unwrap().unwrap();
        let state = AppState::new(coalescer, metrics, None, Log::new(|_| {}));
        let router = work_router(state.clone());
        let hold = tokio::spawn(call(router.clone(), post(body_for(&["hold"]), None)));
        let g = gate.clone();
        tokio::task::spawn_blocking(move || g.wait_for_arrivals(1))
            .await
            .unwrap();
        let late = tokio::spawn(call(router.clone(), post(body_for(&["x", "y"]), None)));
        until_depth(&state, [0, 2]).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        gate.open();
        assert_eq!(hold.await.unwrap().0, StatusCode::OK);
        let (status, headers, body) = late.await.unwrap();
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(error_word(&body), protocol::ERR_QUEUE_FULL);
        assert_eq!(
            headers[header::RETRY_AFTER],
            Lane::Backfill.retry_after_secs().to_string()
        );
        assert_eq!(fake.seen().len(), 1, "only the held pass ran");
    }

    #[tokio::test]
    async fn the_token_is_required_when_set_and_compared_exactly() {
        let h = harness(Some(TOKEN)).await;
        let wrong = format!("{TOKEN}x");
        for bearer in [None, Some("nope"), Some(wrong.as_str())] {
            let (status, _, body) =
                call(work_router(h.state.clone()), post(body_for(&["a"]), bearer)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{bearer:?}");
            assert_eq!(error_word(&body), protocol::ERR_UNAUTHORIZED);
        }
        assert!(
            h.fake.seen().is_empty(),
            "nothing unauthorized reached the session"
        );
        let (status, _, _) = call(
            work_router(h.state.clone()),
            post(body_for(&["a"]), Some(TOKEN)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        // /healthz is not behind the token, on either port.
        assert_eq!(
            get_path(work_router(h.state.clone()), "/healthz").await.0,
            StatusCode::OK
        );
        assert_eq!(
            get_path(metrics_router(h.state.clone()), "/healthz")
                .await
                .0,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn no_token_configured_means_no_auth() {
        let h = harness(None).await;
        let (status, _, _) = call(work_router(h.state.clone()), post(body_for(&["a"]), None)).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _, _) = call(
            work_router(h.state.clone()),
            post(body_for(&["a"]), Some("anything")),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn not_ready_until_the_session_is_built() {
        let gate = Gate::closed();
        let fake = Fake::default();
        let loader_gate = gate.clone();
        let inner = fake.loader();
        let metrics = Arc::new(Metrics::new());
        let (coalescer, loaded) = Coalescer::start(
            Box::new(move || {
                loader_gate.wait_open();
                inner()
            }),
            metrics.clone(),
            Log::new(|_| {}),
        );
        let state = AppState::new(coalescer, metrics, None, Log::new(|_| {}));

        for router in [work_router(state.clone()), metrics_router(state.clone())] {
            let (status, body) = get_path(router, "/healthz").await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(error_word(&body), protocol::ERR_NOT_READY);
        }
        let (status, _, body) =
            call(work_router(state.clone()), post(body_for(&["a"]), None)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error_word(&body), protocol::ERR_NOT_READY);
        let (_, metrics_body) = get_path(metrics_router(state.clone()), "/metrics").await;
        assert!(
            String::from_utf8(metrics_body)
                .unwrap()
                .contains("squelch_embed_session_ready 0")
        );

        gate.open();
        loaded.await.unwrap().unwrap();
        assert_eq!(
            get_path(work_router(state.clone()), "/healthz").await.0,
            StatusCode::OK
        );
        assert_eq!(
            get_path(metrics_router(state.clone()), "/healthz").await.0,
            StatusCode::OK
        );
        let (status, _, _) = call(work_router(state), post(body_for(&["a"]), None)).await;
        assert_eq!(status, StatusCode::OK);
    }

    /// Wait until the coalescer reports exactly `depth` texts waiting.
    async fn until_depth(state: &AppState, depth: [usize; 2]) {
        for _ in 0..500 {
            if state.coalescer.depth() == depth {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!(
            "depth never reached {depth:?}: {:?}",
            state.coalescer.depth()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_full_lane_is_429_with_retry_after_and_the_other_lane_still_queues() {
        let gate = Gate::closed();
        let h = harness_with(
            Fake {
                gate: Some(gate.clone()),
                ..Default::default()
            },
            None,
        )
        .await;
        let router = work_router(h.state.clone());

        // Hold the worker inside a first batch.
        let hold = tokio::spawn(call(router.clone(), post(body_for(&["hold"]), None)));
        let g = gate.clone();
        tokio::task::spawn_blocking(move || g.wait_for_arrivals(1))
            .await
            .unwrap();

        let eight: Vec<String> = (0..8).map(|i| format!("b{i}")).collect();
        let eight: Vec<&str> = eight.iter().map(String::as_str).collect();
        let mut pending = Vec::new();
        for _ in 0..BACKFILL_SHARE / 8 {
            pending.push(tokio::spawn(call(
                router.clone(),
                post(body_for(&eight), None),
            )));
        }
        until_depth(&h.state, [0, BACKFILL_SHARE]).await;

        let (status, headers, body) = call(router.clone(), post(body_for(&["x", "y"]), None)).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(error_word(&body), protocol::ERR_QUEUE_FULL);
        assert_eq!(
            headers[header::RETRY_AFTER],
            Lane::Backfill.retry_after_secs().to_string()
        );

        // A search still gets in line.
        pending.push(tokio::spawn(call(
            router.clone(),
            post(body_for(&["query"]), None),
        )));
        until_depth(&h.state, [1, BACKFILL_SHARE]).await;

        // And the interactive lane has its own ceiling.
        for i in 1..INTERACTIVE_SHARE {
            let q = format!("q{i}");
            pending.push(tokio::spawn(call(
                router.clone(),
                post(body_for(&[&q]), None),
            )));
        }
        until_depth(&h.state, [INTERACTIVE_SHARE, BACKFILL_SHARE]).await;
        let (status, headers, _) = call(router.clone(), post(body_for(&["one more"]), None)).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            headers[header::RETRY_AFTER],
            Lane::Interactive.retry_after_secs().to_string()
        );

        gate.open();
        assert_eq!(hold.await.unwrap().0, StatusCode::OK);
        for p in pending {
            assert_eq!(p.await.unwrap().0, StatusCode::OK);
        }
        let out = h.state.metrics.render(h.state.coalescer.depth());
        assert!(
            out.contains(
                "squelch_embed_requests_total{lane=\"backfill\",outcome=\"queue_full\"} 1"
            )
        );
        assert!(out.contains(
            "squelch_embed_requests_total{lane=\"interactive\",outcome=\"queue_full\"} 1"
        ));
    }

    #[tokio::test]
    async fn inference_failure_is_500_internal() {
        let h = harness_with(
            Fake {
                fail: true,
                ..Default::default()
            },
            None,
        )
        .await;
        let (status, _, body) =
            call(work_router(h.state.clone()), post(body_for(&["a"]), None)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(error_word(&body), protocol::ERR_INTERNAL);
    }

    /// §4, the claims that can be checked from outside: drive every path with
    /// a text that is a canary AND looks like a peer address, then require
    /// that neither the exposition nor any log line carries it, that every
    /// label is from the closed sets, and that nothing IP-shaped appears.
    #[tokio::test]
    async fn no_text_or_address_reaches_metrics_or_logs() {
        const CANARY: &str = "10.42.0.7 canary-subject-Zq8";
        let ok = harness(Some(TOKEN)).await;
        let failing = harness_with(
            Fake {
                fail: true,
                ..Default::default()
            },
            Some(TOKEN),
        )
        .await;
        let misshapen = harness_with(
            Fake {
                short_vectors: true,
                ..Default::default()
            },
            Some(TOKEN),
        )
        .await;

        for h in [&ok, &failing, &misshapen] {
            let router = work_router(h.state.clone());
            for (body, bearer) in [
                (body_for(&[CANARY]), Some(TOKEN)),
                (body_for(&[CANARY, CANARY]), Some(TOKEN)),
                (body_for(&[CANARY]), None),
                (body_for(&[CANARY]), Some(CANARY)),
                (format!("{{\"texts\":[\"{CANARY}\"],\"x\":1}}"), Some(TOKEN)),
                (format!("{CANARY} not json"), Some(TOKEN)),
            ] {
                call(router.clone(), post(body, bearer)).await;
            }
            let (_, metrics) = get_path(metrics_router(h.state.clone()), "/metrics").await;
            let metrics = String::from_utf8(metrics).unwrap();
            assert!(!metrics.contains("canary"), "{metrics}");
            assert_closed(&metrics);

            let lines = h.lines.lock().unwrap().clone();
            for line in &lines {
                assert!(!line.contains("canary"), "{line}");
                assert!(!has_ipv4_shape(line), "{line}");
            }
        }
        // The failure paths did log, so the check above covered real lines.
        assert!(
            failing
                .lines
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.contains("inference failed"))
        );
        assert!(
            misshapen
                .lines
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.contains("wrong shape"))
        );
    }

    #[tokio::test]
    async fn unknown_paths_are_404_and_metrics_are_not_on_the_work_port() {
        let h = harness(None).await;
        assert_eq!(
            get_path(work_router(h.state.clone()), "/metrics").await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get_path(metrics_router(h.state.clone()), "/embed").await.0,
            StatusCode::NOT_FOUND
        );
        let (status, body) = get_path(metrics_router(h.state.clone()), "/metrics").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            String::from_utf8(body)
                .unwrap()
                .contains("squelch_embed_requests_total")
        );
    }

    /// The real listener: binds, answers, and stops on shutdown.
    #[tokio::test]
    async fn serve_answers_over_a_socket() {
        let h = harness(None).await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve(
            listener,
            work_router(h.state.clone()),
            async {
                let _ = stop_rx.await;
            },
            h.state.log.clone(),
        ));

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        s.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        assert!(out.starts_with("HTTP/1.1 200"), "{out}");

        stop_tx.send(()).unwrap();
        server.await.unwrap().unwrap();
    }
}
