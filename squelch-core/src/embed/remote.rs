//! An [`Embedder`] that asks the shared per-node embedding service instead of
//! holding an ONNX session in this process. Design record:
//! docs/EMBED-SERVICE.md §3; the wire contract is `squelch_embed::protocol`.
//!
//! THE POINT IS WHAT IS NOT HERE. A hosted tenant's session is 85-90% of its
//! pod; with this embedder the daemon builds none, and the one service on the
//! node holds the only copy. Self-host never reaches this file: no
//! `embed.url`, no `RemoteEmbedder`.
//!
//! What it owes the store, given that the far end is a process this one cannot
//! vouch for:
//!
//! - **The vector space.** The service runs exactly one model at one width and
//!   one sequence length. [`RemoteEmbedder::new`] refuses a daemon configured
//!   for anything else, and every response is checked against
//!   [`protocol::DIMS`] before a vector leaves this module. `dims()` is a
//!   constant, so `attach_embedder`'s width check proves nothing on its own;
//!   the per-response check is the one that catches a service rebuilt on
//!   another model, and it catches it here rather than as a per-row vec0 error
//!   halfway through a backfill.
//! - **Bounded calls.** Every call runs on a `spawn_blocking` thread at a site
//!   that used to be local CPU work. Unbounded, a half-open connection parks
//!   that thread forever, one per poll tick, until the pool is gone. So the
//!   connect and request timeouts are not optional and not configurable.
//! - **Bounded text.** Each text is cut to `embed.max_chars` HERE, before it is
//!   sent. Ingest and backfill already cut through `message_embed_text`, but a
//!   search query reaches the embedder as typed, and a pasted page is not
//!   something to put on the wire.
//! - **No text in any log or error.** Errors carry an HTTP status, the
//!   service's closed error word, or a transport class (timeout, connect), and
//!   nothing derived from a request or a response body.
//!
//! WHAT IS NOT CHECKED HERE: sensitivity. Sealed mail never reaches an
//! [`Embedder`] of any kind; the gate is the store-side `sensitivity ==
//! Normal` selection at ingest and in `messages_missing_vectors`, and a second
//! gate in here would be one more thing to keep in step with the first.

use std::io::Read;
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use reqwest::header::{AUTHORIZATION, HeaderValue, RETRY_AFTER};
use squelch_embed::protocol;
use url::Url;

use super::{EmbedSettings, Embedder};
use crate::error::{CoreError, Result};
use crate::text::truncate_chars;

/// Largest `POST /embed` response body read, in bytes. A full batch is
/// [`protocol::MAX_TEXTS_PER_CALL`] x [`protocol::DIMS`] floats, about 40 KB of
/// JSON; this is two orders of magnitude over that, so it only ever stops a
/// response that is not one.
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

/// How long an idle pooled connection is kept. Must stay under the service's
/// header-read timeout (10 s), which is also its idle keep-alive close.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(5);

/// Longest `Retry-After` honoured. The header is a floor on the sync engine's
/// backoff (`sync::embed_backoff_delay`), and a floor nobody bounded is a
/// service that can switch a fleet's embedding off for a day with one header.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(5 * 60);

/// How long a call may take. Fixed in production ([`Timeouts::PRODUCTION`]);
/// a field only so the tests can wait milliseconds rather than seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Timeouts {
    /// TCP connect. In-cluster this is sub-millisecond when anything is
    /// listening, so a slow connect is a missing pod, not a busy one.
    pub(crate) connect: Duration,
    /// The whole of a one-text request: an ingest or a search query, and the
    /// `/healthz` probe.
    pub(crate) single: Duration,
    /// The whole of a multi-text request: a backfill batch, which may queue
    /// behind other tenants' batches on the service's backfill lane.
    pub(crate) batch: Duration,
}

impl Timeouts {
    /// docs/EMBED-SERVICE.md §2.
    pub(crate) const PRODUCTION: Self = Self {
        connect: Duration::from_secs(2),
        single: Duration::from_secs(5),
        batch: Duration::from_secs(30),
    };
}

/// A failed call to the embedding service, CLASSIFIED for the caller's retry
/// policy. Travels inside [`CoreError::Other`]; [`remote_failure`] gets it
/// back out.
///
/// `what` is built from a status code, the service's closed error word or a
/// transport class, never from text, so it is safe in any log line.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("embedding service: {what}")]
pub struct RemoteEmbedError {
    /// Worth retrying later without changing anything: 408, 429, 503 and other 5xx,
    /// a timeout, a refused or failed connection. The sync engine backs off on
    /// these. Anything else (400, 401, a response in the wrong vector space) is
    /// PERMANENT: retrying it sends the same request to the same refusal.
    pub transient: bool,
    /// The service's `Retry-After`, in seconds, capped at [`MAX_RETRY_AFTER`].
    pub retry_after: Option<Duration>,
    what: String,
}

impl RemoteEmbedError {
    fn transient(what: impl Into<String>, retry_after: Option<Duration>) -> Self {
        Self {
            transient: true,
            retry_after,
            what: what.into(),
        }
    }

    fn permanent(what: impl Into<String>) -> Self {
        Self {
            transient: false,
            retry_after: None,
            what: what.into(),
        }
    }
}

impl From<RemoteEmbedError> for CoreError {
    fn from(e: RemoteEmbedError) -> Self {
        CoreError::Other(anyhow::Error::new(e))
    }
}

/// The [`RemoteEmbedError`] inside `err`, if that is what it is. `None` for
/// every other error, local embedder failures included, which is what keeps
/// the sync engine's backoff a remote-only behaviour.
pub fn remote_failure(err: &CoreError) -> Option<&RemoteEmbedError> {
    match err {
        CoreError::Other(e) => e.downcast_ref::<RemoteEmbedError>(),
        _ => None,
    }
}

/// [`Embedder`] over `POST {url}/embed`. See the module docs.
pub struct RemoteEmbedder {
    client: Client,
    embed_url: Url,
    health_url: Url,
    /// `Bearer <token>`, marked sensitive so reqwest keeps it out of its own
    /// debug output.
    auth: Option<HeaderValue>,
    max_chars: usize,
    timeouts: Timeouts,
}

impl std::fmt::Debug for RemoteEmbedder {
    // By hand, so the token cannot reach a `{:?}`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteEmbedder")
            .field("embed_url", &self.embed_url.as_str())
            .field("auth", &self.auth.as_ref().map(|_| "<redacted>"))
            .field("max_chars", &self.max_chars)
            .finish()
    }
}

impl RemoteEmbedder {
    /// Build against the service at `url`. No network: the one `/healthz`
    /// attempt is [`Self::probe`], which the caller makes.
    ///
    /// REFUSES, with the config key named, when this daemon's `embed.model`,
    /// `embed.dims` or (clamped) `embed.max_tokens` disagrees with the
    /// service's. Those three decide which vector space the store's
    /// `message_vecs` is in; a disagreement does not fail loudly on the wire,
    /// it produces 384 floats that stop being comparable with the ones already
    /// stored. Also refuses a URL that is not plain `http(s)://host[:port][/path]`.
    pub fn new(
        url: &str,
        token: Option<&str>,
        settings: &EmbedSettings,
        max_chars: usize,
    ) -> Result<Self> {
        Self::with_timeouts(url, token, settings, max_chars, Timeouts::PRODUCTION)
    }

    pub(crate) fn with_timeouts(
        url: &str,
        token: Option<&str>,
        settings: &EmbedSettings,
        max_chars: usize,
        timeouts: Timeouts,
    ) -> Result<Self> {
        check_vector_space(settings)?;
        let (embed_url, health_url) = service_urls(url)?;
        let auth = match token.map(str::trim).filter(|t| !t.is_empty()) {
            None => None,
            Some(t) => {
                let mut v = HeaderValue::from_str(&format!("Bearer {t}")).map_err(|_| {
                    CoreError::InvalidInput(
                        "SQUELCH_EMBED_TOKEN (embed.token) is not a valid header value".into(),
                    )
                })?;
                v.set_sensitive(true);
                Some(v)
            }
        };
        let client = Client::builder()
            .connect_timeout(timeouts.connect)
            // The ceiling for anything that forgets its per-request timeout.
            .timeout(timeouts.batch)
            // Mail text goes to exactly the configured host. A redirect would
            // send it somewhere nobody configured, and an inherited
            // HTTP(S)_PROXY would route an in-cluster call through a proxy that
            // then holds every tenant's mail.
            // Under the service's 10 s header-read timeout, which also closes
            // idle keep-alive connections: a pooled socket kept longer than
            // that is one the server has already hung up on, and the next
            // request after a quiet spell would fail on it.
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|e| CoreError::Other(anyhow::anyhow!("embedding service client: {e}")))?;
        Ok(Self {
            client,
            embed_url,
            health_url,
            auth,
            max_chars,
            timeouts,
        })
    }

    /// One `GET {url}/healthz`, bounded by the one-text timeout. `Ok` on a
    /// 200. The daemon makes this call once at startup and opens its embedder
    /// gate whichever way it resolves: a service that is down makes search
    /// keyword-only for a while, never an unready pod (docs/EMBED-SERVICE.md
    /// §3).
    pub fn probe(&self) -> std::result::Result<(), RemoteEmbedError> {
        let mut req = self
            .client
            .get(self.health_url.clone())
            .timeout(self.timeouts.single);
        if let Some(auth) = &self.auth {
            req = req.header(AUTHORIZATION, auth.clone());
        }
        let resp = req.send().map_err(transport_error)?;
        match resp.status() {
            StatusCode::OK => Ok(()),
            _ => Err(status_error(&resp)),
        }
    }

    /// One `POST /embed` of at most [`protocol::MAX_TEXTS_PER_CALL`] texts,
    /// already truncated. Returns exactly `texts.len()` vectors of
    /// [`protocol::DIMS`], or an error.
    fn post(&self, texts: Vec<String>) -> std::result::Result<Vec<Vec<f32>>, RemoteEmbedError> {
        debug_assert!(!texts.is_empty() && texts.len() <= protocol::MAX_TEXTS_PER_CALL);
        let n = texts.len();
        let timeout = if n == 1 {
            self.timeouts.single
        } else {
            self.timeouts.batch
        };
        let mut req = self
            .client
            .post(self.embed_url.clone())
            .timeout(timeout)
            .json(&protocol::EmbedRequest { texts });
        if let Some(auth) = &self.auth {
            req = req.header(AUTHORIZATION, auth.clone());
        }
        let resp = req.send().map_err(transport_error)?;
        if resp.status() != StatusCode::OK {
            return Err(status_error(&resp));
        }
        let mut body = Vec::new();
        resp.take(MAX_RESPONSE_BYTES)
            .read_to_end(&mut body)
            .map_err(|_| RemoteEmbedError::transient("response body read failed", None))?;
        let parsed: protocol::EmbedResponse = serde_json::from_slice(&body)
            .map_err(|_| RemoteEmbedError::permanent("response is not an embed response"))?;
        check_response(&parsed, n)?;
        Ok(parsed.vectors)
    }

    fn cut(&self, text: &str) -> String {
        truncate_chars(text, self.max_chars)
    }
}

impl Embedder for RemoteEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut out = self.post(vec![self.cut(text)])?;
        // `post` has already checked there is exactly one.
        Ok(out.pop().unwrap_or_default())
    }

    /// Chunked to [`protocol::MAX_TEXTS_PER_CALL`] per request, in order. A
    /// failed chunk fails the whole batch, which is what the backfill expects:
    /// it stops the pass and the rows stay owed.
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(protocol::MAX_TEXTS_PER_CALL) {
            let cut: Vec<String> = chunk.iter().map(|t| self.cut(t)).collect();
            out.extend(self.post(cut)?);
        }
        Ok(out)
    }

    /// The service's width, answered with no network call. NOT a check that
    /// the service agrees; [`check_response`] is that, per call.
    fn dims(&self) -> usize {
        protocol::DIMS
    }
}

/// The three settings that decide the vector space, against the service's.
fn check_vector_space(settings: &EmbedSettings) -> Result<()> {
    let refuse = |key: &str, ours: String, theirs: String| {
        Err(CoreError::InvalidInput(format!(
            "{key} is {ours}, but the embedding service at SQUELCH_EMBED_URL embeds with \
             {theirs}; vectors from the two would not be comparable. Unset {key} or set it \
             to {theirs}"
        )))
    };
    let code = super::fastembed_impl::resolved_model_code(&settings.model_name)?;
    if code != protocol::MODEL_CODE {
        return refuse(
            "embed.model",
            format!("'{}' ({code})", settings.model_name),
            protocol::MODEL_CODE.to_string(),
        );
    }
    if settings.dims != protocol::DIMS {
        return refuse(
            "embed.dims",
            settings.dims.to_string(),
            protocol::DIMS.to_string(),
        );
    }
    if settings.max_tokens != protocol::MAX_TOKENS {
        return refuse(
            "embed.max_tokens",
            settings.max_tokens.to_string(),
            protocol::MAX_TOKENS.to_string(),
        );
    }
    Ok(())
}

/// `{base}/embed` and `{base}/healthz` from the configured base URL.
///
/// http is allowed, and is what production uses: the service is in-cluster
/// and its traffic never leaves the node (docs/EMBED-SERVICE.md §4, "TLS:
/// no"). The warden's validator is where a public host is refused; this one
/// only refuses what cannot be a base URL at all.
fn service_urls(raw: &str) -> Result<(Url, Url)> {
    let bad = |why: &str| {
        Err(CoreError::InvalidInput(format!(
            "SQUELCH_EMBED_URL (embed.url) {why}; expected http(s)://host[:port]"
        )))
    };
    let Ok(base) = Url::parse(raw.trim()) else {
        return bad("is not a URL");
    };
    if !matches!(base.scheme(), "http" | "https") {
        return bad("must be http or https");
    }
    if base.host_str().is_none_or(str::is_empty) {
        return bad("has no host");
    }
    if !base.username().is_empty() || base.password().is_some() {
        return bad("carries credentials (use SQUELCH_EMBED_TOKEN)");
    }
    if base.query().is_some() || base.fragment().is_some() {
        return bad("carries a query or fragment");
    }
    let root = base.as_str().trim_end_matches('/');
    let join = |path: &str| {
        Url::parse(&format!("{root}{path}")).map_err(|_| {
            CoreError::InvalidInput("SQUELCH_EMBED_URL (embed.url) is not a URL".into())
        })
    };
    Ok((join(protocol::EMBED_PATH)?, join(protocol::HEALTH_PATH)?))
}

/// The response checks that stand between the service and the store: the
/// echoed width, the count, and every vector's length.
fn check_response(
    resp: &protocol::EmbedResponse,
    sent: usize,
) -> std::result::Result<(), RemoteEmbedError> {
    if resp.dims != protocol::DIMS {
        return Err(RemoteEmbedError::permanent(format!(
            "response dims {} != {}",
            resp.dims,
            protocol::DIMS
        )));
    }
    if resp.vectors.len() != sent {
        return Err(RemoteEmbedError::permanent(format!(
            "{} vectors for {sent} texts",
            resp.vectors.len()
        )));
    }
    if let Some(v) = resp.vectors.iter().find(|v| v.len() != protocol::DIMS) {
        return Err(RemoteEmbedError::permanent(format!(
            "a vector of length {} != {}",
            v.len(),
            protocol::DIMS
        )));
    }
    Ok(())
}

/// A non-200, classified by status alone. The body is not read: the status
/// already says everything the retry policy needs, and nothing free-form from
/// the far end then has a way into a log line.
fn status_error(resp: &Response) -> RemoteEmbedError {
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|s| Duration::from_secs(s).min(MAX_RETRY_AFTER));
    let what = format!("HTTP {}", status.as_u16());
    // 408 is the service's body-read deadline: a slow or stalled upload, not a
    // malformed one, so it goes with the load statuses.
    if status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::REQUEST_TIMEOUT
        || status.is_server_error()
    {
        RemoteEmbedError::transient(what, retry_after)
    } else {
        RemoteEmbedError::permanent(what)
    }
}

/// A request that never got a status. All transient: each is a service that
/// is absent, restarting or slow, and none is fixed by changing the request.
/// The class only, never reqwest's own message, which carries the URL.
fn transport_error(e: reqwest::Error) -> RemoteEmbedError {
    let what = if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "connect failed"
    } else {
        "request failed"
    };
    RemoteEmbedError::transient(what, None)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// The pins `squelch_embed::protocol` promises: both ends read the same
    /// numbers, and this is where moving one without the other fails CI.
    #[test]
    fn protocol_constants_are_the_daemon_defaults() {
        assert_eq!(protocol::MODEL_CODE, crate::embed::DEFAULT_MODEL_CODE);
        assert_eq!(protocol::DIMS, crate::store::sqlite::VEC_DIMS);
        assert_eq!(protocol::MAX_TOKENS, crate::embed::DEFAULT_EMBED_MAX_TOKENS);
        assert_eq!(
            protocol::MAX_TEXTS_PER_CALL,
            crate::config::EmbedConfig::default().backfill_batch
        );
        let d = crate::config::EmbedConfig::default();
        assert_eq!(d.dims, protocol::DIMS);
    }

    /// One request as the fake service saw it.
    #[derive(Debug, Clone)]
    struct Seen {
        line: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl Seen {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
        fn texts(&self) -> Vec<String> {
            serde_json::from_slice::<protocol::EmbedRequest>(&self.body)
                .unwrap()
                .texts
        }
    }

    /// `None` holds the connection open and never answers.
    type Reply = Option<(u16, Vec<(&'static str, String)>, String)>;

    /// A tiny HTTP/1.1 server on an ephemeral loopback port: enough of the
    /// protocol for reqwest's one request per connection, nothing more.
    fn serve(reply: impl Fn(&Seen) -> Reply + Send + 'static) -> (String, Arc<Mutex<Vec<Seen>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line.is_empty() {
                    continue;
                }
                let mut headers = Vec::new();
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    let h = h.trim_end();
                    if h.is_empty() {
                        break;
                    }
                    let (k, v) = h.split_once(':').unwrap();
                    headers.push((k.trim().to_string(), v.trim().to_string()));
                }
                let len = headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                    .map(|(_, v)| v.parse::<usize>().unwrap())
                    .unwrap_or(0);
                let mut body = vec![0u8; len];
                reader.read_exact(&mut body).unwrap();
                let req = Seen {
                    line: line.trim_end().to_string(),
                    headers,
                    body,
                };
                log.lock().unwrap().push(req.clone());
                match reply(&req) {
                    Some((status, extra, body)) => {
                        let mut out = format!(
                            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                             content-length: {}\r\nconnection: close\r\n",
                            body.len()
                        );
                        for (k, v) in extra {
                            out.push_str(&format!("{k}: {v}\r\n"));
                        }
                        out.push_str("\r\n");
                        out.push_str(&body);
                        let _ = stream.write_all(out.as_bytes());
                    }
                    None => {
                        std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_secs(30));
                            drop(stream);
                        });
                    }
                }
            }
        });
        (base, seen)
    }

    /// A well-behaved service: vector i's first component is the length in
    /// chars of text i, its second is i, so position and truncation are both
    /// readable off the answer.
    fn echo(seen: &Seen) -> Reply {
        if seen.line.starts_with("GET /healthz") {
            return Some((200, vec![], "ok".into()));
        }
        let vectors: Vec<Vec<f32>> = seen
            .texts()
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let mut v = vec![0.0; protocol::DIMS];
                v[0] = t.chars().count() as f32;
                v[1] = i as f32;
                v
            })
            .collect();
        let body = serde_json::to_string(&protocol::EmbedResponse {
            dims: protocol::DIMS,
            vectors,
        })
        .unwrap();
        Some((200, vec![], body))
    }

    fn settings() -> EmbedSettings {
        crate::config::EmbedConfig::default().settings()
    }

    fn remote(base: &str, token: Option<&str>) -> RemoteEmbedder {
        RemoteEmbedder::new(base, token, &settings(), 1000).unwrap()
    }

    fn err_of(r: Result<impl std::fmt::Debug>) -> RemoteEmbedError {
        let e = r.unwrap_err();
        remote_failure(&e)
            .unwrap_or_else(|| panic!("not a remote failure: {e}"))
            .clone()
    }

    #[test]
    fn embeds_positionally_and_chunks_past_the_wire_bound() {
        let (base, seen) = serve(echo);
        let e = remote(&base, None);
        assert_eq!(e.dims(), protocol::DIMS);
        assert!(seen.lock().unwrap().is_empty(), "dims() asks nobody");

        let one = e.embed("hello").unwrap();
        assert_eq!(one.len(), protocol::DIMS);
        assert_eq!(one[0], 5.0);

        let texts: Vec<String> = (0..19).map(|i| "x".repeat(i + 1)).collect();
        let out = e.embed_batch(&texts).unwrap();
        assert_eq!(out.len(), 19);
        for (i, v) in out.iter().enumerate() {
            assert_eq!(v[0], (i + 1) as f32, "vector {i} is text {i}'s");
            assert_eq!(v[1], (i % protocol::MAX_TEXTS_PER_CALL) as f32);
        }
        let seen = seen.lock().unwrap();
        let sizes: Vec<usize> = seen[1..].iter().map(|s| s.texts().len()).collect();
        assert_eq!(sizes, vec![8, 8, 3], "chunked to the wire bound, in order");
        assert!(seen.iter().all(|s| s.line.starts_with("POST /embed ")));

        // An empty batch is not a request.
        drop(seen);
        assert!(e.embed_batch(&[]).unwrap().is_empty());
    }

    #[test]
    fn text_is_truncated_before_it_is_sent() {
        let (base, seen) = serve(echo);
        let e = RemoteEmbedder::new(&base, None, &settings(), 10).unwrap();
        // Multibyte, so a byte cut would split a char.
        let long = "é".repeat(50);
        assert_eq!(e.embed(&long).unwrap()[0], 10.0);
        e.embed_batch(&[long.clone(), "short".into()]).unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].texts(), vec!["é".repeat(10)]);
        assert_eq!(seen[1].texts(), vec!["é".repeat(10), "short".into()]);
    }

    #[test]
    fn a_response_in_another_vector_space_is_refused() {
        let body = |dims: usize, n: usize, len: usize| {
            serde_json::to_string(&protocol::EmbedResponse {
                dims,
                vectors: vec![vec![0.0; len]; n],
            })
            .unwrap()
        };
        for (what, reply) in [
            ("echoed dims", body(768, 1, protocol::DIMS)),
            ("vector length", body(protocol::DIMS, 1, 768)),
            ("count", body(protocol::DIMS, 2, protocol::DIMS)),
            ("not json", "nope".to_string()),
        ] {
            let (base, _) = serve(move |_| Some((200, vec![], reply.clone())));
            let err = err_of(remote(&base, None).embed("hi"));
            assert!(!err.transient, "{what}: permanent");
        }
        // Too few for a batch.
        let short = body(protocol::DIMS, 1, protocol::DIMS);
        let (base, _) = serve(move |_| Some((200, vec![], short.clone())));
        let err = err_of(remote(&base, None).embed_batch(&["a".into(), "b".into()]));
        assert!(!err.transient);
    }

    #[test]
    fn statuses_are_classified_for_backoff() {
        for (status, transient) in [
            (429, true),
            (408, true),
            (503, true),
            (500, true),
            (502, true),
            (400, false),
            (401, false),
            (404, false),
        ] {
            let (base, _) = serve(move |_| {
                Some((
                    status,
                    vec![("retry-after", "7".into())],
                    r#"{"error":"queue_full"}"#.into(),
                ))
            });
            let err = err_of(remote(&base, None).embed("hi"));
            assert_eq!(err.transient, transient, "HTTP {status}");
            if transient {
                assert_eq!(err.retry_after, Some(Duration::from_secs(7)));
            }
            assert_eq!(err.to_string(), format!("embedding service: HTTP {status}"));
        }
        // A Retry-After past the cap is the cap.
        let (base, _) =
            serve(|_| Some((429, vec![("retry-after", "86400".into())], String::new())));
        let err = err_of(remote(&base, None).embed("hi"));
        assert_eq!(err.retry_after, Some(MAX_RETRY_AFTER));
    }

    #[test]
    fn nothing_listening_is_transient() {
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let err = err_of(remote(&format!("http://127.0.0.1:{port}"), None).embed("hi"));
        assert!(err.transient);
        assert_eq!(err.to_string(), "embedding service: connect failed");
    }

    #[test]
    fn a_service_that_never_answers_is_timed_out() {
        let (base, _) = serve(|_| None);
        let quick = Timeouts {
            connect: Duration::from_millis(500),
            single: Duration::from_millis(200),
            batch: Duration::from_millis(400),
        };
        let e = RemoteEmbedder::with_timeouts(&base, None, &settings(), 1000, quick).unwrap();
        let started = std::time::Instant::now();
        let err = err_of(e.embed("hi"));
        assert!(err.transient);
        assert_eq!(err.to_string(), "embedding service: timed out");
        let single = started.elapsed();
        assert!(single < Duration::from_secs(3), "{single:?}");

        let started = std::time::Instant::now();
        let err = err_of(e.embed_batch(&["a".into(), "b".into()]));
        assert_eq!(err.to_string(), "embedding service: timed out");
        let batch = started.elapsed();
        assert!(batch >= Duration::from_millis(400), "{batch:?}");
        assert!(batch < Duration::from_secs(3), "{batch:?}");

        assert!(e.probe().unwrap_err().transient);
    }

    #[test]
    fn the_bearer_is_sent_only_when_configured() {
        let (base, seen) = serve(echo);
        remote(&base, Some("s3cret")).embed("hi").unwrap();
        remote(&base, None).embed("hi").unwrap();
        remote(&base, Some("   ")).embed("hi").unwrap();
        remote(&base, Some("s3cret")).probe().unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].header("authorization"), Some("Bearer s3cret"));
        assert_eq!(seen[1].header("authorization"), None);
        assert_eq!(seen[2].header("authorization"), None, "blank is unset");
        assert_eq!(seen[3].header("authorization"), Some("Bearer s3cret"));
        assert!(seen[3].line.starts_with("GET /healthz "));

        // And the token never reaches a debug print.
        let shown = format!("{:?}", remote(&base, Some("s3cret")));
        assert!(!shown.contains("s3cret"), "{shown}");
    }

    #[test]
    fn the_probe_reports_health_and_nothing_else() {
        let (base, seen) = serve(|_| Some((503, vec![], r#"{"error":"not_ready"}"#.into())));
        let err = remote(&format!("{base}/"), None).probe().unwrap_err();
        assert!(err.transient);
        assert_eq!(err.to_string(), "embedding service: HTTP 503");
        // A trailing slash on the base does not double up.
        assert!(seen.lock().unwrap()[0].line.starts_with("GET /healthz "));
    }

    #[test]
    fn a_daemon_in_another_vector_space_is_refused_by_key() {
        let base = "http://squelch-embed:8850";
        let mut s = settings();
        s.dims = 768;
        let e = RemoteEmbedder::new(base, None, &s, 1000).unwrap_err();
        assert!(e.to_string().contains("embed.dims is 768"), "{e}");

        let mut s = settings();
        s.max_tokens = 512;
        let e = RemoteEmbedder::new(base, None, &s, 1000).unwrap_err();
        assert!(e.to_string().contains("embed.max_tokens is 512"), "{e}");

        // The int8 build of the same family is a different space.
        let mut s = settings();
        s.model_name = "BGESmallENV15Q".into();
        let e = RemoteEmbedder::new(base, None, &s, 1000).unwrap_err();
        assert!(e.to_string().contains("embed.model"), "{e}");

        // An alias that MEANS the service's model is the service's model.
        let mut s = settings();
        s.model_name = "BGESmallENV15".into();
        RemoteEmbedder::new(base, None, &s, 1000).unwrap();
    }

    #[test]
    fn only_a_plain_http_base_url_is_accepted() {
        let s = settings();
        for ok in [
            "http://squelch-embed:8850",
            "https://embed.example",
            " http://10.0.0.1:8850/ ",
            "http://squelch-embed:8850/base",
        ] {
            RemoteEmbedder::new(ok, None, &s, 1000).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in [
            "",
            "squelch-embed:8850",
            "ftp://squelch-embed",
            "http://user:pw@squelch-embed:8850",
            "http://squelch-embed:8850/?x=1",
            "http://squelch-embed:8850/#f",
        ] {
            let e = RemoteEmbedder::new(bad, None, &s, 1000).unwrap_err();
            assert!(e.to_string().contains("SQUELCH_EMBED_URL"), "{bad}: {e}");
        }
        let (embed, health) = service_urls("http://h:1/base/").unwrap();
        assert_eq!(embed.as_str(), "http://h:1/base/embed");
        assert_eq!(health.as_str(), "http://h:1/base/healthz");
    }

    #[test]
    fn a_local_failure_is_not_a_remote_one() {
        let e = CoreError::Other(anyhow::anyhow!("fastembed embed: boom"));
        assert!(remote_failure(&e).is_none());
        assert!(remote_failure(&CoreError::NotFound).is_none());
    }
}
