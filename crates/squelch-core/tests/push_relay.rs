//! End-to-end tests for the APNs pusher against a mock relay: an axum app on an
//! ephemeral loopback port, so every assertion is made on the bytes that
//! actually left the daemon. `the_relay_never_sees_content` needs exactly that —
//! a struct-level assertion could be satisfied by a second, chattier code path.
//! The rest pin the cursor rules: at-least-once, and never a skip.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use squelch_core::metrics::{SyncMetrics, render};
use squelch_core::push::{CURSOR_KEY, Pusher};
use squelch_core::store::{NewEvent, SqliteStore, Store};
use squelch_core::types::{EventKind, Tier};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch};

const DEV_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1";
const DEV_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb2";
const DEV_C: &str = "ccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc3";

/// Content planted in every event so a leak onto the wire is greppable.
const SENDER: &str = "cfo@acme-example.com";
const ONE_LINE: &str = "wire transfer needs approval before 5pm";

// ---- mock relay -------------------------------------------------------------

#[derive(Clone, Default)]
struct Relay {
    /// Every request body, in order.
    seen: Arc<Mutex<Vec<Value>>>,
    /// Every `Authorization` header value, in order.
    auth: Arc<Mutex<Vec<Option<String>>>>,
    /// When set, answer 500 instead of pushing (relay outage).
    fail: Arc<AtomicBool>,
    /// When set, the relay is fine and APNs is not: HTTP 200 with every token
    /// reported `status: 0`.
    apns_down: Arc<AtomicBool>,
    /// Tokens APNs considers retired: reported back as `410 Unregistered`.
    dead: Arc<Mutex<Vec<String>>>,
    /// Tokens APNs is throttling: reported back as `429 TooManyRequests`.
    throttled: Arc<Mutex<Vec<String>>>,
    /// Per-token APNs status overrides, checked FIRST: `(token, status)`. How
    /// the permanent-rejection and 403 cases are staged.
    statuses: Arc<Mutex<Vec<(String, u16)>>>,
    /// When set, answer `307` pointing at this absolute URL — a relay trying to
    /// walk the pusher (and its bearer) somewhere else.
    redirect_to: Arc<Mutex<Option<String>>>,
    /// When set, fail at the RELAY hop with this HTTP status every request
    /// whose body names this `environment`: how one bucket of a multi-request
    /// fan-out is failed while the others succeed.
    fail_env: Arc<Mutex<Option<(String, u16)>>>,
    /// Per-token `(status, reason)` overrides, checked before `statuses`: how
    /// a specific APNs reason such as `IdleTimeout` is staged.
    reasons: Arc<Mutex<Vec<(String, u16, String)>>>,
}

impl Relay {
    fn bodies(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }
    fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
    /// How many requests carried `event_id` for `token`, whatever the relay
    /// answered: the number of times the pusher tried to push it there.
    fn pushes_of(&self, token: &str, event_id: i64) -> usize {
        self.bodies()
            .iter()
            .filter(|b| {
                b["event_id"] == json!(event_id)
                    && b["device_tokens"]
                        .as_array()
                        .is_some_and(|t| t.iter().any(|t| t == token))
            })
            .count()
    }
}

async fn push(State(relay): State<Relay>, headers: HeaderMap, body: Bytes) -> Response {
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    relay.seen.lock().unwrap().push(parsed.clone());
    relay.auth.lock().unwrap().push(
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
    );

    if relay.fail.load(Ordering::SeqCst) {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    if let Some((env, status)) = relay.fail_env.lock().unwrap().clone()
        && parsed["environment"].as_str() == Some(env.as_str())
    {
        return StatusCode::from_u16(status).unwrap().into_response();
    }

    // A hostile/misconfigured relay bouncing the token-bearing POST elsewhere.
    // 307 preserves method AND body, which is exactly what makes it dangerous.
    if let Some(location) = relay.redirect_to.lock().unwrap().clone() {
        return (
            StatusCode::TEMPORARY_REDIRECT,
            [(axum::http::header::LOCATION, location)],
        )
            .into_response();
    }

    let dead = relay.dead.lock().unwrap().clone();
    let statuses = relay.statuses.lock().unwrap().clone();
    let reasons = relay.reasons.lock().unwrap().clone();
    let throttled = relay.throttled.lock().unwrap().clone();
    let apns_down = relay.apns_down.load(Ordering::SeqCst);
    let tokens = parsed["device_tokens"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let results: Vec<Value> = tokens
        .iter()
        .filter_map(|t| t.as_str())
        .map(|t| {
            if let Some((_, status, reason)) = reasons.iter().find(|(d, _, _)| d == t) {
                json!({ "token": t, "status": status, "reason": reason })
            } else if let Some((_, status)) = statuses.iter().find(|(d, _)| d == t) {
                json!({ "token": t, "status": status, "reason": "Staged" })
            } else if dead.iter().any(|d| d == t) {
                json!({ "token": t, "status": 410, "reason": "Unregistered" })
            } else if apns_down {
                json!({ "token": t, "status": 0, "reason": "unreachable" })
            } else if throttled.iter().any(|d| d == t) {
                json!({ "token": t, "status": 429, "reason": "TooManyRequests" })
            } else {
                json!({ "token": t, "status": 200, "apns_id": "mock" })
            }
        })
        .collect();
    Json(json!({ "results": results })).into_response()
}

async fn spawn_relay() -> (String, Relay) {
    let relay = Relay::default();
    let app = Router::new()
        .route("/v1/push", post(push))
        .with_state(relay.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), relay)
}

// ---- daemon-side harness ----------------------------------------------------

struct Harness {
    store: Arc<SqliteStore>,
    acct: i64,
    shutdown: watch::Sender<bool>,
    handle: tokio::task::JoinHandle<squelch_core::Result<()>>,
}

fn seed_message(store: &SqliteStore, account: i64, label: i64) -> i64 {
    use squelch_core::types::NewMessage;
    store
        .upsert_message(&NewMessage {
            account_id: account,
            gmail_msg_id: format!("push-{label}"),
            thread_id: format!("thread{label}"),
            from_addr: SENDER.into(),
            from_name: None,
            subject: "Notification".into(),
            received_at: chrono::Utc::now(),
            snippet: String::new(),
            body: "An arrival".into(),
            body_html: None,
            is_sent: false,
            is_spam: false,
            to_addrs: None,
            cc_addrs: None,
            bcc_addrs: None,
            list_unsubscribe: None,
            list_unsub_one_click: false,
            auth_pass: None,
            sender_signals: None,
        })
        .unwrap()
}

impl Harness {
    /// Append one event exactly as the sync engine does — through the store, so
    /// the attached notifier fires and the pusher wakes.
    fn emit(&self, message_id: i64) -> i64 {
        let message_id = seed_message(&self.store, self.acct, message_id);
        self.store
            .append_event(&NewEvent {
                account_id: self.acct,
                message_id,
                thread_id: format!("thread{message_id}"),
                kind: EventKind::Urgent,
                tier: Tier::PastDue,
                importance: 95,
                sender: SENDER.to_string(),
                one_line: ONE_LINE.to_string(),
                deadline: Some("2026-08-01T17:00:00Z".to_string()),
                is_auth: false,
                sealed_kind: None,
            })
            .unwrap()
            .unwrap()
    }

    fn register(&self, token: &str) {
        self.store
            .upsert_device(self.acct, token, "ios", None, None)
            .unwrap();
    }

    fn register_tagged(&self, token: &str, tag: &str) {
        self.store
            .upsert_device(self.acct, token, "ios", Some(tag), None)
            .unwrap();
    }

    fn register_in(&self, token: &str, tag: Option<&str>, environment: &str) {
        self.store
            .upsert_device(self.acct, token, "ios", tag, Some(environment))
            .unwrap();
    }

    fn cursor(&self) -> Option<i64> {
        self.store
            .sync_state(self.acct, CURSOR_KEY)
            .unwrap()
            .map(|s| s.last_uid as i64)
    }

    fn devices(&self) -> Vec<String> {
        self.store
            .list_devices(self.acct)
            .unwrap()
            .into_iter()
            .map(|d| d.token)
            .collect()
    }

    /// Signal shutdown and wait for the task, failing if it does not stop
    /// promptly — a pusher that ignores the signal would hold `squelchd` open.
    async fn stop(self) -> Duration {
        let started = Instant::now();
        let _ = self.shutdown.send(true);
        tokio::time::timeout(Duration::from_secs(5), self.handle)
            .await
            .expect("the pusher must end on the shutdown signal")
            .expect("join")
            .expect("clean exit");
        started.elapsed()
    }
}

/// Wire up store + notifier + pusher exactly as `squelchd serve` does, then
/// spawn it. `seed` runs BEFORE the task starts, for the cold-start cases.
fn start(base_url: &str, seed: impl FnOnce(&SqliteStore, i64)) -> Harness {
    start_with_auth(base_url, None, seed)
}

/// [`start`] with a relay bearer configured, for the cases that care where that
/// bearer does — and does not — end up.
fn start_with_auth(
    base_url: &str,
    relay_token: Option<&str>,
    seed: impl FnOnce(&SqliteStore, i64),
) -> Harness {
    start_configured(base_url, relay_token, None, None, seed)
}

/// [`start`] with a configured FALLBACK environment
/// (`[pusher] environment` / `SQUELCH_RELAY_APNS_ENV`).
fn start_with_env(
    base_url: &str,
    environment: Option<&str>,
    seed: impl FnOnce(&SqliteStore, i64),
) -> Harness {
    start_configured(base_url, None, environment, None, seed)
}

/// [`start`] reporting into the given metrics registry, as `squelchd` wires it.
fn start_with_metrics(
    base_url: &str,
    metrics: Arc<SyncMetrics>,
    seed: impl FnOnce(&SqliteStore, i64),
) -> Harness {
    start_configured(base_url, None, None, Some(metrics), seed)
}

fn start_configured(
    base_url: &str,
    relay_token: Option<&str>,
    environment: Option<&str>,
    metrics: Option<Arc<SyncMetrics>>,
    seed: impl FnOnce(&SqliteStore, i64),
) -> Harness {
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let acct = store.ensure_account("me@example.com").unwrap();
    let (events, _) = broadcast::channel::<i64>(256);
    store.attach_event_notifier(events.clone()).unwrap();
    seed(&store, acct);

    let (shutdown, shutdown_rx) = watch::channel(false);
    let mut pusher = Pusher::for_test(store.clone() as Arc<dyn Store>, acct, base_url)
        .with_relay_auth(relay_token)
        .with_environment(environment);
    if let Some(metrics) = metrics {
        pusher = pusher.with_metrics(metrics);
    }
    let handle = tokio::spawn(pusher.run(events.subscribe(), shutdown_rx));
    Harness {
        store,
        acct,
        shutdown,
        handle,
    }
}

/// Poll `pred` until it holds or the deadline passes.
async fn wait_for(what: &str, timeout: Duration, mut pred: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

// ---- tests ------------------------------------------------------------------

/// The happy path: one POST per event, oldest first, cursor advancing to the
/// last accepted id.
#[tokio::test]
async fn a_push_advances_the_cursor() {
    let (base, relay) = spawn_relay().await;
    let h = start(&base, |_, _| {});
    h.register(DEV_A);

    // The first drain adopts the (empty) head as its cursor.
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor() == Some(0)
    })
    .await;

    let first = h.emit(1);
    let last = h.emit(2);
    wait_for("both pushes", Duration::from_secs(5), || relay.count() == 2).await;
    wait_for("the cursor to catch up", Duration::from_secs(2), || {
        h.cursor() == Some(last)
    })
    .await;

    // One request per event, in order, each carrying its own id and thread.
    let bodies = relay.bodies();
    assert_eq!(bodies[0]["event_id"], json!(first));
    assert_eq!(bodies[1]["event_id"], json!(last));
    assert_eq!(bodies[0]["collapse_id"], json!("thread-thread1"));
    assert_eq!(bodies[1]["collapse_id"], json!("thread-thread2"));
    assert_eq!(bodies[0]["device_tokens"], json!([DEV_A]));

    h.stop().await;
}

/// A phone holding two mailboxes registers the same device with each daemon
/// under a different tag. One daemon's push must name ITS tag, so the receiver
/// can tell whose event 41 arrived — event ids being per-daemon ints that
/// collide across accounts.
#[tokio::test]
async fn a_tagged_device_gets_its_tag_on_the_wire() {
    let (base, relay) = spawn_relay().await;
    let h = start(&base, |_, _| {});
    h.register_tagged(DEV_A, "3F2A9C1E-7B44-4D0A-9E21-8C5B6D0F1A32");
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor().is_some()
    })
    .await;

    let id = h.emit(1);
    wait_for("the push", Duration::from_secs(5), || relay.count() == 1).await;

    let body = relay.bodies().remove(0);
    assert_eq!(
        body["event_id"],
        json!(format!("3F2A9C1E-7B44-4D0A-9E21-8C5B6D0F1A32:{id}"))
    );
    // The tag joins the id and changes NOTHING else: the body is the same
    // closed, content-free shape it is without one.
    let mut keys: Vec<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["collapse_id", "device_tokens", "event_id"]);

    h.stop().await;
}

/// Two accounts on one phone, one daemon: each device carries its own tag, so
/// the fan-out splits into one request per tag rather than one shared payload
/// that would tell both receivers the same wrong thing.
#[tokio::test]
async fn each_tag_gets_its_own_request() {
    let (base, relay) = spawn_relay().await;
    let h = start(&base, |_, _| {});
    h.register_tagged(DEV_A, "acct-a");
    h.register_tagged(DEV_B, "acct-b");
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor().is_some()
    })
    .await;

    let id = h.emit(1);
    wait_for("both requests", Duration::from_secs(5), || {
        relay.count() == 2
    })
    .await;

    let bodies = relay.bodies();
    let mut seen: Vec<(String, String)> = bodies
        .iter()
        .map(|b| {
            (
                b["event_id"].as_str().unwrap().to_string(),
                b["device_tokens"][0].as_str().unwrap().to_string(),
            )
        })
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        vec![
            (format!("acct-a:{id}"), DEV_A.to_string()),
            (format!("acct-b:{id}"), DEV_B.to_string()),
        ]
    );

    h.stop().await;
}

/// A token's environment is the TOKEN's: a dev build registered `sandbox` on a
/// daemon with no configured environment must reach the sandbox host, not the
/// relay's production default, where APNs would reject it.
#[tokio::test]
async fn a_sandbox_device_sends_sandbox_with_nothing_configured() {
    let (base, relay) = spawn_relay().await;
    let h = start(&base, |_, _| {});
    h.register_in(DEV_A, None, "sandbox");
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor().is_some()
    })
    .await;

    h.emit(1);
    wait_for("the push", Duration::from_secs(5), || relay.count() == 1).await;
    let body = relay.bodies().remove(0);
    assert_eq!(body["environment"], json!("sandbox"));
    assert_eq!(body["device_tokens"], json!([DEV_A]));

    h.stop().await;
}

/// One mailbox, one tag, two builds: the Xcode build's sandbox token and the
/// TestFlight build's production token cannot share a request, because a
/// request names one APNs host. Two requests, each carrying its own.
#[tokio::test]
async fn one_tag_in_two_environments_is_two_requests() {
    let (base, relay) = spawn_relay().await;
    let h = start(&base, |_, _| {});
    h.register_in(DEV_A, Some("acct-a"), "sandbox");
    h.register_in(DEV_B, Some("acct-a"), "production");
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor().is_some()
    })
    .await;

    let id = h.emit(1);
    wait_for("both requests", Duration::from_secs(5), || {
        relay.count() == 2
    })
    .await;
    let mut seen: Vec<(String, String, String)> = relay
        .bodies()
        .iter()
        .map(|b| {
            assert_eq!(b["device_tokens"].as_array().unwrap().len(), 1);
            (
                b["device_tokens"][0].as_str().unwrap().to_string(),
                b["environment"].as_str().unwrap().to_string(),
                b["event_id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        vec![
            (
                DEV_A.to_string(),
                "sandbox".to_string(),
                format!("acct-a:{id}")
            ),
            (
                DEV_B.to_string(),
                "production".to_string(),
                format!("acct-a:{id}")
            ),
        ]
    );

    h.stop().await;
}

/// The configured environment is the FALLBACK, not an override: a device that
/// declared its own keeps it, and only a device that never said gets the
/// configured one.
#[tokio::test]
async fn a_devices_own_environment_beats_the_configured_one() {
    let (base, relay) = spawn_relay().await;
    let h = start_with_env(&base, Some("production"), |_, _| {});
    h.register_in(DEV_A, None, "sandbox");
    h.register(DEV_B); // legacy row: no environment
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor().is_some()
    })
    .await;

    h.emit(1);
    wait_for("both requests", Duration::from_secs(5), || {
        relay.count() == 2
    })
    .await;
    let mut seen: Vec<(String, String)> = relay
        .bodies()
        .iter()
        .map(|b| {
            (
                b["device_tokens"][0].as_str().unwrap().to_string(),
                b["environment"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        vec![
            (DEV_A.to_string(), "sandbox".to_string()),
            (DEV_B.to_string(), "production".to_string()),
        ]
    );

    h.stop().await;
}

/// A device that never declared one, on a pusher with none configured: the
/// field is OMITTED, not nulled, and the relay applies its own default.
#[tokio::test]
async fn an_undeclared_device_omits_the_environment_when_none_is_configured() {
    let (base, relay) = spawn_relay().await;
    let h = start(&base, |_, _| {});
    h.register(DEV_A);
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor().is_some()
    })
    .await;

    h.emit(1);
    wait_for("the push", Duration::from_secs(5), || relay.count() == 1).await;
    let body = relay.bodies().remove(0);
    assert!(
        body.as_object().unwrap().get("environment").is_none(),
        "no environment anywhere must mean no field: {body}"
    );

    h.stop().await;
}

/// A PERMANENT rejection must not wedge the pusher. Every token answering 400
/// (`BadDeviceToken`, a sandbox token sent to production) advances the cursor:
/// retrying the same request cannot help, and holding would stall this daemon
/// forever. The row is NOT deleted, because a 400 is also what config drift
/// looks like, and deleting on it would unregister every phone.
#[tokio::test]
async fn every_token_rejected_advances_the_cursor_and_keeps_the_rows() {
    let (base, relay) = spawn_relay().await;
    relay
        .statuses
        .lock()
        .unwrap()
        .extend([(DEV_A.to_string(), 400), (DEV_B.to_string(), 400)]);

    let h = start(&base, |_, _| {});
    h.register(DEV_A);
    h.register(DEV_B);
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor() == Some(0)
    })
    .await;

    let id = h.emit(1);
    wait_for(
        "the cursor past the rejected event",
        Duration::from_secs(5),
        || h.cursor() == Some(id),
    )
    .await;
    // Room to retry before believing it did not.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(relay.count(), 1, "a rejected event must not be retried");
    assert_eq!(h.devices().len(), 2, "a 400 is not a 410: rows stay");

    h.stop().await;
}

/// Rejected plus RETRYABLE with nothing delivered still holds: the status-0
/// token may land on a retry, so the event is not history yet.
#[tokio::test]
async fn a_rejection_beside_a_retryable_failure_holds_the_cursor() {
    let (base, relay) = spawn_relay().await;
    relay
        .statuses
        .lock()
        .unwrap()
        .extend([(DEV_A.to_string(), 400), (DEV_B.to_string(), 0)]);

    let h = start(&base, |_, _| {});
    h.register(DEV_A);
    h.register(DEV_B);
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor() == Some(0)
    })
    .await;

    h.emit(1);
    wait_for("the failed attempt", Duration::from_secs(5), || {
        relay.count() >= 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        h.cursor(),
        Some(0),
        "nothing landed and something can retry"
    );
    assert_eq!(h.devices().len(), 2);

    h.stop().await;
}

/// 403 is a 4xx that is NOT permanent for the token: it is the relay's own
/// provider token being bad, which an operator fixes. It holds the cursor
/// rather than writing the event off.
#[tokio::test]
async fn a_403_holds_the_cursor() {
    let (base, relay) = spawn_relay().await;
    relay
        .statuses
        .lock()
        .unwrap()
        .push((DEV_A.to_string(), 403));

    let h = start(&base, |_, _| {});
    h.register(DEV_A);
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor() == Some(0)
    })
    .await;

    let id = h.emit(1);
    wait_for("the failed attempt", Duration::from_secs(5), || {
        relay.count() >= 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(h.cursor(), Some(0), "a 403 must not advance the cursor");
    assert_eq!(h.devices().len(), 1);

    // The operator fixes the provider token; the retry lands.
    relay.statuses.lock().unwrap().clear();
    wait_for("the retry to land", Duration::from_secs(20), || {
        h.cursor() == Some(id)
    })
    .await;

    h.stop().await;
}

/// THE INVARIANT. The pusher reads full event rows; the relay sees an id and a
/// collapse id and nothing else. Asserted on the wire.
#[tokio::test]
async fn the_relay_never_sees_content() {
    let (base, relay) = spawn_relay().await;
    let h = start(&base, |_, _| {});
    h.register(DEV_A);
    h.register(DEV_B);
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor().is_some()
    })
    .await;

    h.emit(1);
    wait_for("the push", Duration::from_secs(5), || relay.count() == 1).await;

    let body = relay.bodies().remove(0);
    let obj = body.as_object().expect("a JSON object");

    // The shape is closed. A new key here is a privacy regression, not a feature.
    let mut keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["collapse_id", "device_tokens", "event_id"]);

    // And no content survives anywhere in the encoded bytes.
    let raw = body.to_string();
    for leak in [
        SENDER,
        ONE_LINE,
        "past_due",
        "urgent",
        "95",
        "2026-08-01",
        "sender",
        "one_line",
        "tier",
        "importance",
        "deadline",
    ] {
        assert!(!raw.contains(leak), "content leaked to the relay: {leak}");
    }

    // Multi-device fan-out is one request, not one per device.
    assert_eq!(relay.count(), 1);
    assert_eq!(body["device_tokens"], json!([DEV_A, DEV_B]));

    h.stop().await;
}

/// APNs `410 Unregistered` comes back through the relay verbatim precisely so
/// THIS daemon deletes the row. The relay remembers nothing.
#[tokio::test]
async fn a_410_deletes_the_device() {
    let (base, relay) = spawn_relay().await;
    relay.dead.lock().unwrap().push(DEV_B.to_string());

    let h = start(&base, |_, _| {});
    h.register(DEV_A);
    h.register(DEV_B);
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor().is_some()
    })
    .await;

    let id = h.emit(1);
    wait_for(
        "the dead device to be dropped",
        Duration::from_secs(5),
        || h.devices() == vec![DEV_A.to_string()],
    )
    .await;

    // The live device still got its push, and the event is not retried forever.
    wait_for("the cursor", Duration::from_secs(2), || {
        h.cursor() == Some(id)
    })
    .await;
    assert_eq!(relay.count(), 1);

    // A later event goes only to the survivor.
    h.emit(2);
    wait_for("the second push", Duration::from_secs(5), || {
        relay.count() == 2
    })
    .await;
    assert_eq!(relay.bodies()[1]["device_tokens"], json!([DEV_A]));

    h.stop().await;
}

/// A relay outage must DELAY pushes, never skip them: the cursor stays put and
/// the task retries after its backoff.
#[tokio::test]
async fn a_relay_failure_leaves_the_cursor_and_retries() {
    let (base, relay) = spawn_relay().await;
    relay.fail.store(true, Ordering::SeqCst);

    let h = start(&base, |_, _| {});
    h.register(DEV_A);
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor() == Some(0)
    })
    .await;

    let id = h.emit(1);
    wait_for("the failed attempt", Duration::from_secs(5), || {
        relay.count() >= 1
    })
    .await;

    // The event was NOT consumed by a relay that refused it.
    assert_eq!(h.cursor(), Some(0), "a 500 must not advance the cursor");

    // Heal the relay; the backoff retry picks the very same event back up.
    relay.fail.store(false, Ordering::SeqCst);
    wait_for("the retry to land", Duration::from_secs(20), || {
        h.cursor() == Some(id)
    })
    .await;
    assert!(
        relay.count() >= 2,
        "the failed event was retried, not dropped"
    );
    assert_eq!(relay.bodies().last().unwrap()["event_id"], json!(id));

    h.stop().await;
}

/// THE OTHER HOP: the relay answers 200 while APNs behind it takes nothing, so
/// the event must be retried on the same terms as a relay outage — a 200 from
/// the relay is not evidence of delivery.
#[tokio::test]
async fn an_apns_failure_inside_a_200_leaves_the_cursor_and_retries() {
    let (base, relay) = spawn_relay().await;
    relay.apns_down.store(true, Ordering::SeqCst);

    let h = start(&base, |_, _| {});
    h.register(DEV_A);
    h.register(DEV_B);
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor() == Some(0)
    })
    .await;

    let id = h.emit(1);
    wait_for("the failed attempt", Duration::from_secs(5), || {
        relay.count() >= 1
    })
    .await;

    // The relay said 200. Apple took nothing. The event is NOT consumed.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        h.cursor(),
        Some(0),
        "a 200 carrying no delivered token must not advance the cursor"
    );
    // And the devices are still registered: status 0 is not 410.
    assert_eq!(h.devices().len(), 2);

    // Heal Apple; the backoff retry picks the very same event back up.
    relay.apns_down.store(false, Ordering::SeqCst);
    wait_for("the retry to land", Duration::from_secs(20), || {
        h.cursor() == Some(id)
    })
    .await;
    assert!(
        relay.count() >= 2,
        "the undelivered event was retried, not dropped"
    );
    assert_eq!(relay.bodies().last().unwrap()["event_id"], json!(id));

    h.stop().await;
}

/// THE BEARER STOPS AT THE CONFIGURED RELAY. A hostile relay (or a host with a
/// sloppy rewrite rule) answering `307` must not walk the token-bearing POST
/// elsewhere; refusing redirects makes that an ordinary non-2xx.
#[tokio::test]
async fn a_relay_redirect_is_refused_and_never_carries_the_bearer() {
    let (elsewhere_base, elsewhere) = spawn_relay().await;
    let (base, relay) = spawn_relay().await;
    *relay.redirect_to.lock().unwrap() = Some(format!("{elsewhere_base}/v1/push"));

    let h = start_with_auth(&base, Some("relay-secret"), |_, _| {});
    h.register(DEV_A);
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor() == Some(0)
    })
    .await;

    let id = h.emit(1);
    wait_for("the redirected attempt", Duration::from_secs(5), || {
        relay.count() >= 1
    })
    .await;
    // Give a follower time to follow before believing this one did not.
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        elsewhere.count(),
        0,
        "the pusher followed the relay's redirect"
    );
    assert!(
        elsewhere.auth.lock().unwrap().is_empty(),
        "the relay bearer reached a host the operator never configured"
    );
    // It DID reach the configured relay — this is the same request, simply not
    // followed onward.
    assert_eq!(
        relay.auth.lock().unwrap()[0].as_deref(),
        Some("Bearer relay-secret")
    );
    assert_eq!(
        h.cursor(),
        Some(0),
        "a 3xx is a non-2xx: the cursor must not advance"
    );

    // And it lands in the ordinary backoff: stop redirecting, the retry lands.
    *relay.redirect_to.lock().unwrap() = None;
    wait_for("the retry to land", Duration::from_secs(20), || {
        h.cursor() == Some(id)
    })
    .await;
    assert_eq!(elsewhere.count(), 0);

    h.stop().await;
}

/// A PARTIAL delivery is a delivery. One throttled device must not make the
/// pusher re-notify every healthy one — the cursor advances and the 429 is a
/// log line, not a retry.
#[tokio::test]
async fn a_partial_delivery_still_advances_the_cursor() {
    let (base, relay) = spawn_relay().await;
    relay.throttled.lock().unwrap().push(DEV_B.to_string());

    let h = start(&base, |_, _| {});
    h.register(DEV_A);
    h.register(DEV_B);
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor() == Some(0)
    })
    .await;

    let id = h.emit(1);
    wait_for("the push", Duration::from_secs(5), || relay.count() == 1).await;
    wait_for("the cursor", Duration::from_secs(2), || {
        h.cursor() == Some(id)
    })
    .await;

    // Give it room to retry before believing it did not, and confirm the
    // throttled device is still registered (429 is not 410).
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(relay.count(), 1, "a landed event must not be re-pushed");
    assert_eq!(h.devices().len(), 2);

    h.stop().await;
}

/// With nothing registered there is nobody to deliver to, so an unpushed event
/// is history rather than backlog: the cursor advances and no socket is ever
/// opened toward the relay.
#[tokio::test]
async fn no_devices_advances_the_cursor_without_a_request() {
    let (base, relay) = spawn_relay().await;
    let h = start(&base, |_, _| {});
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor() == Some(0)
    })
    .await;

    h.emit(1);
    let last = h.emit(2);
    wait_for("the cursor to skip past", Duration::from_secs(5), || {
        h.cursor() == Some(last)
    })
    .await;
    assert_eq!(relay.count(), 0, "the relay must never have been called");

    // Registering now does not resurrect the skipped events, by design.
    h.register(DEV_A);
    let fresh = h.emit(3);
    wait_for("the first real push", Duration::from_secs(5), || {
        relay.count() == 1
    })
    .await;
    assert_eq!(relay.bodies()[0]["event_id"], json!(fresh));

    h.stop().await;
}

/// A first-ever start joins at the HEAD of the log. A phone that registers today
/// must not be handed last week's mail as a notification storm — the same rule a
/// cursorless SSE client gets.
#[tokio::test]
async fn a_cold_start_joins_at_the_head() {
    let (base, relay) = spawn_relay().await;
    let h = start(&base, |store, acct| {
        for message_id in 1..=3 {
            let message_id = seed_message(store, acct, message_id);
            store
                .append_event(&NewEvent {
                    account_id: acct,
                    message_id,
                    thread_id: format!("old{message_id}"),
                    kind: EventKind::Surfaced,
                    tier: Tier::Signal,
                    importance: 70,
                    sender: SENDER.to_string(),
                    one_line: ONE_LINE.to_string(),
                    deadline: None,
                    is_auth: false,
                    sealed_kind: None,
                })
                .unwrap()
                .unwrap();
        }
        store.upsert_device(acct, DEV_A, "ios", None, None).unwrap();
    });

    wait_for("the head cursor", Duration::from_secs(2), || {
        h.cursor() == Some(3)
    })
    .await;
    // Give it room to misbehave before believing it did not.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(relay.count(), 0, "history must not be replayed as pushes");

    // What happens NEXT is delivered normally.
    let fresh = h.emit(4);
    wait_for("the new event", Duration::from_secs(5), || {
        relay.count() == 1
    })
    .await;
    assert_eq!(relay.bodies()[0]["event_id"], json!(fresh));

    h.stop().await;
}

/// The task must end on the daemon's shutdown watch even while parked on its
/// idle interval — otherwise graceful shutdown waits out a full minute.
#[tokio::test]
async fn shutdown_ends_the_task_promptly() {
    let (base, _relay) = spawn_relay().await;
    let h = start(&base, |_, _| {});
    wait_for("the first drain", Duration::from_secs(2), || {
        h.cursor().is_some()
    })
    .await;

    // Parked on the 60s idle tick at this point.
    let elapsed = h.stop().await;
    assert!(
        elapsed < Duration::from_secs(2),
        "shutdown took {elapsed:?}; the pusher is not watching the signal"
    );
}

/// The relay bearer is presented when configured, and the wake channel closing
/// (the store, and with it the process, going away) ends the task.
#[tokio::test]
async fn the_relay_bearer_is_presented_when_configured() {
    let (base, relay) = spawn_relay().await;
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let acct = store.ensure_account("me@example.com").unwrap();
    let (events, _) = broadcast::channel::<i64>(256);
    store.attach_event_notifier(events.clone()).unwrap();
    store.upsert_device(acct, DEV_A, "ios", None, None).unwrap();

    let (shutdown, shutdown_rx) = watch::channel(false);
    let pusher = Pusher::for_test(store.clone() as Arc<dyn Store>, acct, &base)
        .with_relay_auth(Some("relay-secret"));
    let handle = tokio::spawn(pusher.run(events.subscribe(), shutdown_rx));

    // Let the cold start settle on the (empty) head first: an event appended
    // before the first drain would legitimately be joined past, not pushed.
    wait_for("the initial cursor", Duration::from_secs(2), || {
        store.sync_state(acct, CURSOR_KEY).unwrap().is_some()
    })
    .await;

    let message_id = seed_message(&store, acct, 1);
    store
        .append_event(&NewEvent {
            account_id: acct,
            message_id,
            thread_id: "t1".to_string(),
            kind: EventKind::Urgent,
            tier: Tier::PastDue,
            importance: 95,
            sender: SENDER.to_string(),
            one_line: ONE_LINE.to_string(),
            deadline: None,
            is_auth: false,
            sealed_kind: None,
        })
        .unwrap()
        .unwrap();

    wait_for("the push", Duration::from_secs(5), || relay.count() == 1).await;
    assert_eq!(
        relay.auth.lock().unwrap()[0].as_deref(),
        Some("Bearer relay-secret")
    );

    let _ = shutdown.send(true);
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("prompt shutdown")
        .expect("join")
        .expect("clean exit");
}

#[tokio::test]
async fn an_opened_queued_arrival_is_skipped_before_relay_dispatch() {
    use squelch_core::store::{SyncState, agent_triage::AgentTriageStore};
    let (base, relay) = spawn_relay().await;
    let h = start(&base, |store, account| {
        let message = seed_message(store, account, 1);
        store
            .set_triage(
                message,
                account,
                0,
                Tier::Noise,
                squelch_core::types::Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
        store
            .append_event(&NewEvent {
                account_id: account,
                message_id: message,
                thread_id: "thread1".into(),
                kind: EventKind::Urgent,
                tier: Tier::Signal,
                importance: 0,
                sender: SENDER.into(),
                one_line: "Login alert".into(),
                deadline: None,
                is_auth: false,
                sealed_kind: None,
            })
            .unwrap()
            .unwrap();
        store
            .acknowledge_agent_message(account, message, chrono::Utc::now())
            .unwrap();
        store
            .upsert_device(account, DEV_A, "ios", None, None)
            .unwrap();
        store
            .set_sync_state(
                account,
                CURSOR_KEY,
                &SyncState {
                    uidvalidity: 0,
                    last_uid: 0,
                },
            )
            .unwrap();
    });
    wait_for(
        "the suppressed event cursor",
        Duration::from_secs(2),
        || h.cursor() == Some(1),
    )
    .await;
    assert_eq!(relay.count(), 0);
    let fresh = h.emit(2);
    wait_for("the new arrival push", Duration::from_secs(5), || {
        relay.count() == 1
    })
    .await;
    assert_eq!(relay.bodies()[0]["event_id"], json!(fresh));
    h.stop().await;
}

// ---- a relay failure partway through a fan-out ------------------------------

/// THE RULE: an event that reached anybody is never re-pushed. Bucket A
/// (sandbox) delivers, then bucket B's request is refused at the relay hop
/// (the relay 400s an environment it will not serve). Before this rule the
/// whole outcome was thrown away and A was re-pushed the same event every
/// backoff period, forever. Now the cursor moves past it, A got it ONCE, and
/// the next event still reaches A.
#[tokio::test]
async fn a_later_relay_failure_after_a_delivery_advances_and_never_repushes() {
    let (base, relay) = spawn_relay().await;
    *relay.fail_env.lock().unwrap() = Some(("production".to_string(), 400));

    let h = start(&base, |_, _| {});
    // Registration order is bucket order: A's request goes out first.
    h.register_in(DEV_A, None, "sandbox");
    h.register_in(DEV_B, None, "production");
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor() == Some(0)
    })
    .await;

    let first = h.emit(1);
    wait_for(
        "the cursor past the partly delivered event",
        Duration::from_secs(5),
        || h.cursor() == Some(first),
    )
    .await;
    assert_eq!(relay.pushes_of(DEV_A, first), 1);
    assert_eq!(
        relay.pushes_of(DEV_B, first),
        1,
        "B's request was attempted"
    );

    // The pusher backed off (the relay just failed); the NEXT event still
    // reaches A once it comes back around, and is the next thing it sends.
    let second = h.emit(2);
    wait_for("the next event at A", Duration::from_secs(20), || {
        relay.pushes_of(DEV_A, second) == 1
    })
    .await;
    wait_for(
        "the cursor past the next event",
        Duration::from_secs(5),
        || h.cursor() == Some(second),
    )
    .await;
    // Across the whole retry window the first event went to A exactly once.
    assert_eq!(
        relay.pushes_of(DEV_A, first),
        1,
        "a delivered event was pushed again"
    );
    assert_eq!(relay.pushes_of(DEV_B, first), 1, "nor is B retried for it");
    assert_eq!(h.devices().len(), 2, "a relay failure deletes nothing");

    h.stop().await;
}

/// The FIRST request failing at the relay hop delivered nothing, so nothing
/// rules out a retry: the cursor holds and the event lands once the relay
/// recovers, with each device getting it once.
#[tokio::test]
async fn a_first_request_relay_failure_holds_the_cursor_and_retries() {
    let (base, relay) = spawn_relay().await;
    *relay.fail_env.lock().unwrap() = Some(("production".to_string(), 503));

    let h = start(&base, |_, _| {});
    // B (production) is the FIRST bucket this time, so the fan-out stops
    // before A's request is ever sent.
    h.register_in(DEV_B, None, "production");
    h.register_in(DEV_A, None, "sandbox");
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor() == Some(0)
    })
    .await;

    let id = h.emit(1);
    wait_for("the failed attempt", Duration::from_secs(5), || {
        relay.count() >= 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(h.cursor(), Some(0), "nothing landed: the cursor must hold");
    assert_eq!(
        relay.pushes_of(DEV_A, id),
        0,
        "the fan-out stops at the failure"
    );

    *relay.fail_env.lock().unwrap() = None;
    wait_for("the retry to land", Duration::from_secs(20), || {
        h.cursor() == Some(id)
    })
    .await;
    assert_eq!(relay.pushes_of(DEV_A, id), 1);
    assert!(
        relay.pushes_of(DEV_B, id) >= 2,
        "B was retried, not dropped"
    );

    h.stop().await;
}

/// A 410 seen before the relay hop failed is still definitive: the dead row is
/// deleted even though the event as a whole is held for retry.
#[tokio::test]
async fn a_410_before_a_relay_failure_still_deletes_the_row() {
    let (base, relay) = spawn_relay().await;
    relay.dead.lock().unwrap().push(DEV_A.to_string());
    *relay.fail_env.lock().unwrap() = Some(("production".to_string(), 500));

    let h = start(&base, |_, _| {});
    h.register_in(DEV_A, None, "sandbox");
    h.register_in(DEV_B, None, "production");
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor() == Some(0)
    })
    .await;

    h.emit(1);
    wait_for("the dead row to go", Duration::from_secs(5), || {
        h.devices() == vec![DEV_B.to_string()]
    })
    .await;
    // Nobody took the event, so it is held for the retry.
    assert_eq!(h.cursor(), Some(0));

    h.stop().await;
}

/// `400 IdleTimeout` is APNs closing an idle connection, not a verdict on the
/// token: unlike every other 400 it HOLDS the cursor and the retry lands.
#[tokio::test]
async fn a_400_idle_timeout_holds_the_cursor_and_retries() {
    let (base, relay) = spawn_relay().await;
    relay
        .reasons
        .lock()
        .unwrap()
        .push((DEV_A.to_string(), 400, "IdleTimeout".to_string()));

    let h = start(&base, |_, _| {});
    h.register(DEV_A);
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor() == Some(0)
    })
    .await;

    let id = h.emit(1);
    wait_for("the failed attempt", Duration::from_secs(5), || {
        relay.count() >= 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        h.cursor(),
        Some(0),
        "IdleTimeout must not write the event off"
    );
    assert_eq!(h.devices().len(), 1, "and must not delete the row");

    relay.reasons.lock().unwrap().clear();
    wait_for("the retry to land", Duration::from_secs(20), || {
        h.cursor() == Some(id)
    })
    .await;

    h.stop().await;
}

/// The pusher counts every per-token result into the registry it was handed,
/// by outcome. A daemon whose pushes all fail must be VISIBLE on the dashboard.
#[tokio::test]
async fn per_token_results_are_counted_into_the_metrics() {
    let (base, relay) = spawn_relay().await;
    relay
        .statuses
        .lock()
        .unwrap()
        .push((DEV_B.to_string(), 400));
    relay.dead.lock().unwrap().push(DEV_C.to_string());

    let metrics = SyncMetrics::new();
    let h = start_with_metrics(&base, metrics.clone(), |_, _| {});
    h.register(DEV_A);
    h.register(DEV_B);
    h.register(DEV_C);
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor() == Some(0)
    })
    .await;

    let id = h.emit(1);
    wait_for("the cursor", Duration::from_secs(5), || {
        h.cursor() == Some(id)
    })
    .await;

    let text = render(&metrics, None);
    for (outcome, n) in [
        ("delivered", 1),
        ("rejected", 1),
        ("dead", 1),
        ("failed", 0),
    ] {
        let line = format!("squelchd_apns_push_results_total{{outcome=\"{outcome}\"}} {n}\n");
        assert!(
            text.contains(&line),
            "missing `{}` in the exposition",
            line.trim()
        );
    }

    h.stop().await;
}

/// An agent's draft review request: pushed once, to TAGGED devices only, as
/// `"<tag>:draft:<id>:<created>"` with the relay's draft fallback line, and
/// without touching the event cursor (it is not an event).
#[tokio::test]
async fn a_draft_ping_goes_to_tagged_devices_once() {
    let (base, relay) = spawn_relay().await;
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let acct = store.ensure_account("me@example.com").unwrap();
    let (events, _) = broadcast::channel::<i64>(256);
    store.attach_event_notifier(events.clone()).unwrap();
    let (pings, _) = broadcast::channel::<squelch_core::push::DraftPing>(8);
    let (shutdown, shutdown_rx) = watch::channel(false);
    let pusher = Pusher::for_test(store.clone() as Arc<dyn Store>, acct, &base)
        .with_draft_pings(pings.subscribe());
    let handle = tokio::spawn(pusher.run(events.subscribe(), shutdown_rx));
    let h = Harness {
        store,
        acct,
        shutdown,
        handle,
    };
    h.register(DEV_A);
    h.register_tagged(DEV_B, "3F2A9C1E-7B44-4D0A-9E21-8C5B6D0F1A32");
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor().is_some()
    })
    .await;
    let cursor = h.cursor();

    pings
        .send(squelch_core::push::DraftPing {
            draft_id: 12,
            created: 1_790_000_000,
        })
        .unwrap();
    wait_for("the draft push", Duration::from_secs(5), || {
        relay.count() == 1
    })
    .await;
    // Give a second, wrong request time to show up if one were coming.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(relay.count(), 1, "one request, the untagged device skipped");

    let body = relay.bodies().remove(0);
    assert_eq!(body["device_tokens"], json!([DEV_B]));
    assert_eq!(
        body["event_id"],
        json!("3F2A9C1E-7B44-4D0A-9E21-8C5B6D0F1A32:draft:12:1790000000")
    );
    assert_eq!(body["kind"], json!("draft"));
    assert_eq!(body["collapse_id"], json!("draft-12-1790000000"));
    assert_eq!(h.cursor(), cursor, "a draft ping is not an event");

    h.stop().await;
}

/// A calendar reminder: pushed once, to TAGGED devices only, as
/// `"<tag>:cal:<fire id>:<fired>"` with the relay's calendar fallback line,
/// and without touching the event cursor.
#[tokio::test]
async fn a_calendar_ping_goes_to_tagged_devices_once() {
    let (base, relay) = spawn_relay().await;
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let acct = store.ensure_account("me@example.com").unwrap();
    let (events, _) = broadcast::channel::<i64>(256);
    store.attach_event_notifier(events.clone()).unwrap();
    let (pings, _) = broadcast::channel::<squelch_core::push::CalendarPing>(8);
    let (shutdown, shutdown_rx) = watch::channel(false);
    let pusher = Pusher::for_test(store.clone() as Arc<dyn Store>, acct, &base)
        .with_calendar_pings(pings.subscribe());
    let handle = tokio::spawn(pusher.run(events.subscribe(), shutdown_rx));
    let h = Harness {
        store,
        acct,
        shutdown,
        handle,
    };
    h.register(DEV_A);
    h.register_tagged(DEV_B, "3F2A9C1E-7B44-4D0A-9E21-8C5B6D0F1A32");
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor().is_some()
    })
    .await;
    let cursor = h.cursor();

    pings
        .send(squelch_core::push::CalendarPing {
            fire_id: 7,
            fired: 1_793_210_000,
        })
        .unwrap();
    wait_for("the calendar push", Duration::from_secs(5), || {
        relay.count() == 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(relay.count(), 1, "one request, the untagged device skipped");

    let body = relay.bodies().remove(0);
    assert_eq!(body["device_tokens"], json!([DEV_B]));
    assert_eq!(
        body["event_id"],
        json!("3F2A9C1E-7B44-4D0A-9E21-8C5B6D0F1A32:cal:7:1793210000")
    );
    assert_eq!(body["kind"], json!("calendar"));
    assert_eq!(body["collapse_id"], json!("cal-7-1793210000"));
    assert_eq!(h.cursor(), cursor, "a calendar ping is not an event");

    h.stop().await;
}

/// The morning brief: pushed once, to tagged IPHONES only (an untagged
/// phone and a Mac row are both skipped), as `"<tag>:brief:<YYYYMMDD>"` with
/// the relay's brief fallback line, one request per tag so each mailbox keeps
/// its own banner, and without touching the event cursor.
#[tokio::test]
async fn a_brief_ping_goes_to_tagged_iphones_once() {
    let (base, relay) = spawn_relay().await;
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let acct = store.ensure_account("me@example.com").unwrap();
    let (events, _) = broadcast::channel::<i64>(256);
    store.attach_event_notifier(events.clone()).unwrap();
    let (pings, _) = broadcast::channel::<squelch_core::push::BriefPing>(8);
    let (shutdown, shutdown_rx) = watch::channel(false);
    let pusher = Pusher::for_test(store.clone() as Arc<dyn Store>, acct, &base)
        .with_brief_pings(pings.subscribe());
    let handle = tokio::spawn(pusher.run(events.subscribe(), shutdown_rx));
    let h = Harness {
        store,
        acct,
        shutdown,
        handle,
    };
    h.register(DEV_A);
    h.register_tagged(DEV_B, "3F2A9C1E-7B44-4D0A-9E21-8C5B6D0F1A32");
    // A Mac row (the release-push work registers these): never briefed.
    h.store
        .upsert_device(
            h.acct,
            DEV_C,
            "macos",
            Some("3F2A9C1E-7B44-4D0A-9E21-8C5B6D0F1A32"),
            None,
        )
        .unwrap();
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor().is_some()
    })
    .await;
    let cursor = h.cursor();

    pings
        .send(squelch_core::push::BriefPing {
            day: chrono::NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(),
        })
        .unwrap();
    wait_for("the brief push", Duration::from_secs(5), || {
        relay.count() == 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(relay.count(), 1, "one request: untagged and Mac skipped");

    let body = relay.bodies().remove(0);
    assert_eq!(body["device_tokens"], json!([DEV_B]));
    assert_eq!(
        body["event_id"],
        json!("3F2A9C1E-7B44-4D0A-9E21-8C5B6D0F1A32:brief:20261105")
    );
    assert_eq!(body["kind"], json!("brief"));
    assert_eq!(
        body["collapse_id"],
        json!("brief-20261105-3F2A9C1E-7B44-4D0A-9E21-8C5B6D0F1A32")
    );
    assert_eq!(h.cursor(), cursor, "a brief ping is not an event");

    h.stop().await;
}

/// A banner clear: pushed once, to TAGGED devices only, as
/// `"<tag>:clear:<event id>"` with the relay's silent `clear` kind, and
/// without touching the event cursor.
#[tokio::test]
async fn a_clear_ping_goes_to_tagged_devices_once() {
    let (base, relay) = spawn_relay().await;
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let acct = store.ensure_account("me@example.com").unwrap();
    let (events, _) = broadcast::channel::<i64>(256);
    store.attach_event_notifier(events.clone()).unwrap();
    let (pings, _) = broadcast::channel::<squelch_core::push::ClearPing>(8);
    let (shutdown, shutdown_rx) = watch::channel(false);
    let pusher = Pusher::for_test(store.clone() as Arc<dyn Store>, acct, &base)
        .with_clear_pings(pings.subscribe());
    let handle = tokio::spawn(pusher.run(events.subscribe(), shutdown_rx));
    let h = Harness {
        store,
        acct,
        shutdown,
        handle,
    };
    h.register(DEV_A);
    h.register_tagged(DEV_B, "3F2A9C1E-7B44-4D0A-9E21-8C5B6D0F1A32");
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor().is_some()
    })
    .await;
    let cursor = h.cursor();

    pings
        .send(squelch_core::push::ClearPing { event_id: 41 })
        .unwrap();
    wait_for("the clear push", Duration::from_secs(5), || {
        relay.count() == 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(relay.count(), 1, "one request, the untagged device skipped");

    let body = relay.bodies().remove(0);
    assert_eq!(body["device_tokens"], json!([DEV_B]));
    assert_eq!(
        body["event_id"],
        json!("3F2A9C1E-7B44-4D0A-9E21-8C5B6D0F1A32:clear:41")
    );
    assert_eq!(body["kind"], json!("clear"));
    assert_eq!(body["collapse_id"], json!("clear-41"));
    assert_eq!(h.cursor(), cursor, "a clear ping is not an event");

    h.stop().await;
}

// ---- release announcements ---------------------------------------------------

const MAC_DEV: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd4";
const PHONE_TAG: &str = "3F2A9C1E-7B44-4D0A-9E21-8C5B6D0F1A32";
const MAC_TAG: &str = "9B1D4E2F-0C3A-4F5B-8D6E-7A1B2C3D4E5F";

/// A Sparkle feed with one release published long enough ago to be past the
/// grace period.
const FEED: &str = r#"<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" version="2.0"><channel>
<item><title>0.0.99</title><pubDate>Thu, 01 Jan 2026 00:00:00 +0000</pubDate>
<sparkle:shortVersionString>0.0.99</sparkle:shortVersionString></item>
</channel></rss>"#;

async fn spawn_feed() -> String {
    let app = Router::new().route("/appcast.xml", axum::routing::get(|| async { FEED }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/appcast.xml")
}

fn release_pusher(base: &str, feed: &str) -> (Arc<SqliteStore>, i64, Pusher) {
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let acct = store.ensure_account("me@example.com").unwrap();
    let pusher =
        Pusher::for_test(store.clone() as Arc<dyn Store>, acct, base).with_release_feed(feed);
    store
        .upsert_device(acct, DEV_A, "ios", Some(PHONE_TAG), Some("production"))
        .unwrap();
    store
        .upsert_device(acct, MAC_DEV, "macos", Some(MAC_TAG), Some("production"))
        .unwrap();
    // A daemon that has checked before: the first check ever only records.
    store
        .set_app_setting(acct, squelch_core::release_feed::ANNOUNCED_KEY, "0.0.98")
        .unwrap();
    (store, acct, pusher)
}

fn announced(store: &SqliteStore, acct: i64) -> Option<String> {
    store
        .get_app_setting(acct, squelch_core::release_feed::ANNOUNCED_KEY)
        .unwrap()
}

/// The first check a daemon ever makes records the current release and
/// pushes nothing: rolling this out is not itself an announcement.
#[tokio::test]
async fn the_first_check_ever_announces_nothing() {
    let (base, relay) = spawn_relay().await;
    let feed = spawn_feed().await;
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let acct = store.ensure_account("me@example.com").unwrap();
    store
        .upsert_device(acct, MAC_DEV, "macos", Some(MAC_TAG), Some("production"))
        .unwrap();
    let pusher =
        Pusher::for_test(store.clone() as Arc<dyn Store>, acct, &base).with_release_feed(&feed);
    pusher.check_release().await;
    assert_eq!(relay.count(), 0);
    assert_eq!(announced(&store, acct).as_deref(), Some("0.0.99"));
}

/// A release goes to the Mac under the Mac's own topic, once. The phone hears
/// nothing about a Mac release.
#[tokio::test]
async fn a_release_goes_to_the_mac_once_under_its_own_topic() {
    let (base, relay) = spawn_relay().await;
    let feed = spawn_feed().await;
    let (store, acct, pusher) = release_pusher(&base, &feed);

    pusher.check_release().await;
    assert_eq!(relay.count(), 1);
    let body = relay.bodies().remove(0);
    assert_eq!(body["device_tokens"], json!([MAC_DEV]));
    assert_eq!(body["kind"], json!("release"));
    assert_eq!(body["topic"], json!(squelch_core::push::MAC_TOPIC));
    assert_eq!(
        body["event_id"],
        json!(format!("{MAC_TAG}:release:macos:0.0.99"))
    );
    assert_eq!(body["collapse_id"], json!("release-macos-0.0.99"));
    assert_eq!(
        store
            .get_app_setting(acct, squelch_core::release_feed::ANNOUNCED_KEY)
            .unwrap()
            .as_deref(),
        Some("0.0.99")
    );

    // Once per version.
    pusher.check_release().await;
    assert_eq!(
        relay.count(),
        1,
        "the same release is never announced twice"
    );
}

/// A Mac that already connected on the new version means nobody is told.
#[tokio::test]
async fn a_mac_already_on_the_release_hears_nothing() {
    let (base, relay) = spawn_relay().await;
    let feed = spawn_feed().await;
    let (store, acct, pusher) = release_pusher(&base, &feed);
    let issued = store.issue_device_token(acct, "laptop").unwrap();
    store
        .verify_device_token_from(
            &issued.token,
            Some(&squelch_core::store::ClientTag::parse("macos/0.0.99").unwrap()),
        )
        .unwrap()
        .unwrap();

    pusher.check_release().await;
    assert_eq!(relay.count(), 0);
    assert_eq!(
        store
            .get_app_setting(acct, squelch_core::release_feed::ANNOUNCED_KEY)
            .unwrap()
            .as_deref(),
        Some("0.0.99"),
        "recorded, so it is never sent later"
    );
}

/// A relay outage leaves the release unannounced for the next check.
#[tokio::test]
async fn a_release_that_reached_nobody_is_tried_again() {
    let (base, relay) = spawn_relay().await;
    let feed = spawn_feed().await;
    let (store, acct, pusher) = release_pusher(&base, &feed);
    relay.fail.store(true, Ordering::SeqCst);
    pusher.check_release().await;
    assert_eq!(announced(&store, acct).as_deref(), Some("0.0.98"));
    relay.fail.store(false, Ordering::SeqCst);
    pusher.check_release().await;
    assert_eq!(relay.count(), 2);
    assert!(
        store
            .get_app_setting(acct, squelch_core::release_feed::ANNOUNCED_KEY)
            .unwrap()
            .is_some()
    );
}

/// A feed whose only release past 0.0.98 is a quiet hotfix.
const QUIET_FEED: &str = r#"<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" xmlns:passband="https://passband.app/xml-namespaces/appcast" version="2.0"><channel>
<item><title>0.0.99</title><pubDate>Thu, 01 Jan 2026 00:00:00 +0000</pubDate>
<sparkle:shortVersionString>0.0.99</sparkle:shortVersionString>
<passband:quiet>yes</passband:quiet></item>
</channel></rss>"#;

/// A quiet release is recorded and nobody is pushed.
#[tokio::test]
async fn a_quiet_release_is_not_announced() {
    let (base, relay) = spawn_relay().await;
    let app = Router::new().route("/appcast.xml", axum::routing::get(|| async { QUIET_FEED }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (store, acct, pusher) = release_pusher(&base, &format!("http://{addr}/appcast.xml"));
    pusher.check_release().await;
    assert_eq!(relay.count(), 0);
    assert_eq!(announced(&store, acct).as_deref(), Some("0.0.99"));
}

/// Mail never goes to a Mac: it has its own banners, and no extension to fill
/// a push in.
#[tokio::test]
async fn mail_events_skip_mac_devices() {
    let (base, relay) = spawn_relay().await;
    let h = start(&base, |_, _| {});
    h.register_tagged(DEV_A, PHONE_TAG);
    h.store
        .upsert_device(h.acct, MAC_DEV, "macos", Some(MAC_TAG), Some("production"))
        .unwrap();
    wait_for("the initial cursor", Duration::from_secs(2), || {
        h.cursor().is_some()
    })
    .await;

    h.emit(1);
    wait_for("the push", Duration::from_secs(5), || relay.count() == 1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(relay.count(), 1);
    assert_eq!(relay.bodies()[0]["device_tokens"], json!([DEV_A]));

    h.stop().await;
}
