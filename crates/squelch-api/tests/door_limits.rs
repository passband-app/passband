//! The human door's limits (tranche A item A13, #483), over real TCP: the
//! header-read timer that closes a connection whose request never arrives,
//! the per-route ceiling that answers a stuck request 504, and the store-call
//! slots that keep a wedged store from collecting a blocking thread per
//! request. The two `#[ignore]` probes at the end print the `BENCH` lines in
//! the PR:
//!
//! ```sh
//! cargo test -p squelch-api --test door_limits -- --ignored --nocapture
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use squelch_api::{
    ApiState, ROUTE_CEILINGS, RequestTimeouts, STORE_CALL_SLOTS, attach_event_channel,
    record_http_metrics, router, serve, serve_with_header_timeout,
};
use squelch_core::metrics::{SyncMetrics, render};
use squelch_core::store::{NewEvent, SqliteStore, Store};
use squelch_core::types::{EventKind, Tier};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tower::ServiceExt;

mod common;
use common::{TOKEN, authed, calendar_on, msg, state_with};

/// Serve `app` the way the daemon does, forever.
async fn serve_app(app: axum::Router) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, app, std::future::pending()));
    addr
}

/// [`serve_app`] with a header-read timer a test can wait out.
async fn serve_app_with_header_timer(app: axum::Router, timer: Duration) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve_with_header_timeout(
        listener,
        app,
        std::future::pending(),
        timer,
    ));
    addr
}

/// A file-backed store (so a second connection can wedge it), one account and
/// one message, under a directory of its own.
struct FileStore {
    dir: std::path::PathBuf,
    path: std::path::PathBuf,
    store: Arc<SqliteStore>,
    acct: i64,
    message_id: i64,
}

impl FileStore {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("door-limits-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.db");
        let store = Arc::new(SqliteStore::open(&path).unwrap());
        let acct = store.ensure_account("me@example.com").unwrap();
        let message_id = store
            .upsert_message(&msg(acct, "g1", "t1", "hello", "body"))
            .unwrap();
        Self {
            dir,
            path,
            store,
            acct,
            message_id,
        }
    }

    fn state(&self) -> ApiState {
        ApiState::new(self.store.clone(), self.acct, TOKEN)
    }

    /// A second connection holding the write lock: every write through the
    /// store now waits on SQLite's busy handler (5 s) and fails, the shape of
    /// a stuck writer.
    fn wedge(&self) -> rusqlite::Connection {
        let wedge = rusqlite::Connection::open(&self.path).unwrap();
        wedge.execute_batch("BEGIN IMMEDIATE").unwrap();
        wedge
    }

    /// `POST /client/v2/messages/{id}/opened`: one store write.
    fn write_url(&self, addr: std::net::SocketAddr) -> String {
        format!(
            "http://{addr}/client/v2/messages/{}/opened",
            self.message_id
        )
    }
}

impl Drop for FileStore {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// After a wedge is dropped, the calls it parked finish on their threads
/// within SQLite's busy-handler retry; wait that out before the store's
/// files go.
async fn settled() {
    tokio::time::sleep(Duration::from_millis(500)).await;
}

/// Read until the peer closes, or give up after `within`. `true` means it
/// closed.
async fn closed_within(sock: &mut TcpStream, within: Duration) -> bool {
    let mut buf = [0u8; 1024];
    tokio::time::timeout(within, async {
        loop {
            match sock.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    })
    .await
    .is_ok()
}

/// Poll `cond` every 10 ms until it holds, or panic after `within`.
async fn wait_for(within: Duration, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + within;
    while !cond() {
        assert!(Instant::now() < deadline, "not within {within:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap()
}

// --- the header-read timer ----------------------------------------------------

/// A client that sends part of a request head and then nothing used to hold
/// its connection for as long as it liked; it is now closed at the timer,
/// and the door goes on serving.
#[tokio::test]
async fn a_half_open_request_is_closed_at_the_header_timer() {
    let _serial = SERIAL.lock().await;
    let (state, _store, _acct) = state_with(|_, _| {});
    let addr = serve_app_with_header_timer(router(state), Duration::from_millis(500)).await;

    let mut half = TcpStream::connect(addr).await.unwrap();
    half.write_all(b"GET /client/stats HTTP/1.1\r\nHost: probe\r\n")
        .await
        .unwrap();
    let started = Instant::now();
    assert!(
        closed_within(&mut half, Duration::from_secs(5)).await,
        "the half-open connection was still open after 5 s"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(400),
        "{:?}",
        started.elapsed()
    );

    let resp = client()
        .get(format!("http://{addr}/client/stats"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

/// The same timer closes a keep-alive connection that has sat idle between
/// requests, which is the half of its behaviour the value in
/// `HEADER_READ_TIMEOUT` is chosen for (the module docs on Traefik).
#[tokio::test]
async fn an_idle_keep_alive_connection_is_closed_at_the_header_timer_too() {
    let _serial = SERIAL.lock().await;
    let (state, _store, _acct) = state_with(|_, _| {});
    let addr = serve_app_with_header_timer(router(state), Duration::from_millis(500)).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(
        format!(
            "GET /client/stats HTTP/1.1\r\nHost: probe\r\nAuthorization: Bearer {TOKEN}\r\n\r\n"
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let mut head = vec![0u8; 4096];
    let n = sock.read(&mut head).await.unwrap();
    assert!(
        head[..n].starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&head[..n])
    );
    // Answered, and left idle: closed at the timer, not held.
    assert!(
        closed_within(&mut sock, Duration::from_secs(5)).await,
        "the idle keep-alive connection was still open after 5 s"
    );
}

// --- the per-route ceiling ----------------------------------------------------

/// A request whose store call never returns is answered 504 at its ceiling,
/// in the door's own error shape, and the door answers the next request once
/// the store does.
#[tokio::test]
async fn a_request_past_its_ceiling_is_a_504_in_the_door_error_shape() {
    let _serial = SERIAL.lock().await;
    let fixture = FileStore::new("ceiling");
    let state = fixture
        .state()
        .with_request_timeouts(RequestTimeouts::all(Duration::from_millis(500)));
    let addr = serve_app(router(state.clone())).await;
    let wedge = fixture.wedge();

    let started = Instant::now();
    let resp = client()
        .post(fixture.write_url(addr))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(resp.status(), 504);
    assert_eq!(
        resp.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({ "error": "request timed out", "code": "timeout" })
    );
    assert!(
        elapsed >= Duration::from_millis(500) && elapsed < Duration::from_secs(3),
        "{elapsed:?}"
    );

    // The cut request's write is still parked on its thread, slot in hand,
    // until the store answers it; once the wedge drops it settles, and the
    // next request runs under the same ceiling with nothing in its way.
    drop(wedge);
    wait_for(Duration::from_secs(10), || {
        state.store_call_slots_free() == STORE_CALL_SLOTS
    })
    .await;
    let resp = client()
        .post(fixture.write_url(addr))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

/// The ceiling is on the response head. A stream whose head came back at
/// once keeps delivering long after the ceiling would have cut a request.
#[tokio::test]
async fn the_events_stream_outlives_the_request_ceiling() {
    let _serial = SERIAL.lock().await;
    let (state, store, acct) = state_with(|_, _| {});
    let event_tx = attach_event_channel(&store).unwrap();
    let state = state
        .with_event_notifier(event_tx)
        .with_request_timeouts(RequestTimeouts::all(Duration::from_millis(200)))
        .with_store_call_slots(1);
    let addr = serve_app(router(state.clone())).await;

    let mut resp = client()
        .get(format!("http://{addr}/client/events"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    // The one slot is back once the head is out and the pump's first read
    // (nothing to replay) is done: an open stream waiting for events holds
    // none.
    wait_for(Duration::from_secs(5), || {
        state.store_call_slots_free() == 1
    })
    .await;

    // Well past the ceiling, then an event: it arrives on the same stream.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let message_id = store
        .upsert_message(&msg(acct, "g-ev", "t-ev", "Notification", "An arrival"))
        .unwrap();
    store
        .append_event(&NewEvent {
            account_id: acct,
            message_id,
            thread_id: "t-ev".into(),
            kind: EventKind::Surfaced,
            tier: Tier::Signal,
            importance: 70,
            sender: "alice@example.com".into(),
            one_line: "still streaming".into(),
            deadline: None,
            is_auth: false,
            sealed_kind: None,
        })
        .unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(5), resp.chunk())
        .await
        .expect("a frame within 5 s")
        .unwrap()
        .expect("the stream is still open");
    assert!(
        String::from_utf8_lossy(&frame).contains("still streaming"),
        "{}",
        String::from_utf8_lossy(&frame)
    );
}

/// The ceiling is the ROUTE's, through the real router: under the same wedge
/// a local route is cut at the local ceiling and a long one (the attachment
/// upload, whose first store call is a write) at the long one. This pins the
/// matched-template lookup the table walk below cannot.
#[tokio::test]
async fn a_long_route_runs_under_the_long_ceiling() {
    let _serial = SERIAL.lock().await;
    let fixture = FileStore::new("per-route");
    let state = fixture.state().with_request_timeouts(RequestTimeouts {
        local: Duration::from_millis(200),
        upstream: Duration::from_millis(200),
        long: Duration::from_millis(1500),
    });
    let addr = serve_app(router(state)).await;
    let wedge = fixture.wedge();
    let client = client();

    let started = Instant::now();
    let resp = client
        .post(fixture.write_url(addr))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 504);
    let local = started.elapsed();
    assert!(local < Duration::from_secs(1), "{local:?}");

    let started = Instant::now();
    let resp = client
        .post(format!(
            "http://{addr}/client/compose/attachments?filename=note.txt"
        ))
        .bearer_auth(TOKEN)
        .header("content-type", "text/plain")
        .body("hello")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 504);
    let long = started.elapsed();
    assert!(
        long >= Duration::from_millis(1500) && long < Duration::from_secs(4),
        "{long:?}"
    );
    drop(wedge);
    settled().await;
}

/// The table is strings, and a string does not notice its route being
/// renamed: every row has to land on a real (method, template), or the route
/// it meant is running under the local ceiling.
#[tokio::test]
async fn every_ceiling_row_is_a_real_route() {
    let _serial = SERIAL.lock().await;
    let (state, _store, _acct) = state_with(|_, _| {});
    let metrics = SyncMetrics::new();
    let app = router(state.with_features(calendar_on())).layer(
        axum::middleware::from_fn_with_state(metrics.clone(), record_http_metrics),
    );

    for (method, template, _) in ROUTE_CEILINGS {
        let uri = template
            .replace("{message_id}", "1")
            .replace("{calendar_id}", "primary")
            .replace("{id}", "1");
        assert!(!uri.contains('{'), "fill in the parameter of {template}");
        let resp = app.clone().oneshot(authed(method, &uri)).await.unwrap();
        assert_ne!(resp.status(), 405, "{method} is not a verb of {template}");
        let text = render(&metrics, None);
        let series = format!(r#"route="{template}",method="{method}",status="#);
        assert!(
            text.lines()
                .any(|l| l.contains(&series) && l.contains("_count")),
            "{method} {template} matched no route\n{text}"
        );
    }
}

// --- the in-flight cap --------------------------------------------------------

fn threads_now() -> usize {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Threads:"))
                .and_then(|l| l["Threads:".len()..].trim().parse().ok())
        })
        .unwrap_or(0)
}

/// One test at a time: two of them count the process's threads, and every
/// other test here spawns some.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A thread that keeps the high-water mark of the process's thread count.
fn watch_threads() -> (
    Arc<AtomicUsize>,
    Arc<AtomicBool>,
    std::thread::JoinHandle<()>,
) {
    let peak = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let handle = {
        let (peak, stop) = (peak.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                peak.fetch_max(threads_now(), Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    };
    (peak, stop, handle)
}

/// Sixty requests against a wedged store, two slots: two store calls park
/// on blocking threads, the other fifty-eight wait for a slot and are
/// answered 504 at the ceiling without ever taking a thread. The two slots
/// stay taken for as long as the wedge holds the two threads, whatever
/// became of the requests that took them.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cap_bounds_the_blocking_threads_a_wedge_parks() {
    let _serial = SERIAL.lock().await;
    let fixture = FileStore::new("cap");
    let state = fixture
        .state()
        .with_request_timeouts(RequestTimeouts::all(Duration::from_secs(1)))
        .with_store_call_slots(2);
    let addr = serve_app(router(state.clone())).await;
    let wedge = fixture.wedge();

    let base = threads_now();
    let (peak, stop, sampler) = watch_threads();
    let client = client();
    let mut tasks = Vec::new();
    for _ in 0..60 {
        let (client, url) = (client.clone(), fixture.write_url(addr));
        tasks.push(tokio::spawn(async move {
            client
                .post(url)
                .bearer_auth(TOKEN)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }));
    }
    let mut statuses = Vec::new();
    for t in tasks {
        statuses.push(t.await.unwrap());
    }
    stop.store(true, Ordering::Relaxed);
    sampler.join().unwrap();

    assert!(statuses.iter().all(|s| *s == 504), "{statuses:?}");
    let peak = peak.load(Ordering::Relaxed);
    // Two parked store calls, the sampler itself, and slack for whatever
    // else the runtime spawns; sixty would have been sixty.
    assert!(
        peak <= base + 2 + 10,
        "peak {peak} threads over a base of {base}: the cap did not hold"
    );
    // The two parked calls hold their slots until the store answers them.
    assert_eq!(state.store_call_slots_free(), 0);
    drop(wedge);
    settled().await;
    assert_eq!(state.store_call_slots_free(), 2);
}

/// The bearer check's device-token lookup has its own four permits, and each
/// rides into the blocking closure the way a store-call slot does: forty
/// device-token requests queued behind a parked write take four threads,
/// not forty, and are answered 504 at the ceiling.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_device_token_check_keeps_its_permit_on_the_thread_it_parks() {
    let _serial = SERIAL.lock().await;
    let fixture = FileStore::new("device-auth");
    let phone = fixture
        .store
        .issue_device_token(fixture.acct, "phone")
        .unwrap()
        .token;
    let state = fixture
        .state()
        .with_request_timeouts(RequestTimeouts::all(Duration::from_secs(1)));
    let addr = serve_app(router(state)).await;
    let wedge = fixture.wedge();

    let base = threads_now();
    let (peak, stop, sampler) = watch_threads();
    let client = client();
    // One write, parked in the busy handler with the store mutex held: every
    // lookup behind it parks on the mutex.
    let write = {
        let (client, url) = (client.clone(), fixture.write_url(addr));
        tokio::spawn(async move {
            client
                .post(url)
                .bearer_auth(TOKEN)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut tasks = Vec::new();
    for _ in 0..40 {
        let (client, phone) = (client.clone(), phone.clone());
        tasks.push(tokio::spawn(async move {
            client
                .get(format!("http://{addr}/client/stats"))
                .bearer_auth(phone)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }));
    }
    let mut statuses = vec![write.await.unwrap()];
    for t in tasks {
        statuses.push(t.await.unwrap());
    }
    stop.store(true, Ordering::Relaxed);
    sampler.join().unwrap();

    assert!(statuses.iter().all(|s| *s == 504), "{statuses:?}");
    let peak = peak.load(Ordering::Relaxed);
    assert!(
        peak <= base + 1 + squelch_api::DEVICE_AUTH_CONCURRENCY + 10,
        "peak {peak} threads over a base of {base}: the lookups took threads past their permits"
    );
    drop(wedge);
    settled().await;
}

// --- probes -------------------------------------------------------------------

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn proc_status(key: &str) -> Option<String> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find(|l| l.starts_with(key))
        .map(|l| l[key.len()..].trim().to_string())
}

fn rss_kb() -> usize {
    proc_status("VmRSS:")
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
        .unwrap_or(0)
}

fn open_fds() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .map(|d| d.count())
        .unwrap_or(0)
}

/// `true` while the server side still holds `sock` open: nothing readable
/// within 20 ms and no EOF or reset.
async fn still_open(sock: &TcpStream) -> bool {
    let mut buf = [0u8; 64];
    match tokio::time::timeout(Duration::from_millis(20), sock.peek(&mut buf)).await {
        Err(_) => true,
        Ok(Ok(0)) => false,
        Ok(Ok(_)) => {
            // Something was written (a 408, say): drain it and see whether
            // the server then closed.
            let mut sink = vec![0u8; 4096];
            let _ = sock.try_read(&mut sink);
            matches!(
                tokio::time::timeout(Duration::from_millis(20), sock.peek(&mut buf)).await,
                Err(_) | Ok(Ok(1..))
            )
        }
        Ok(Err(_)) => false,
    }
}

/// `SQUELCH_PROBE_CONNS` (default 1000) connections that each send part of a
/// request head and then nothing, sampled at `SQUELCH_PROBE_SAMPLE_SECS`
/// (default `5,65,185,305`) seconds: how many the server still holds, and what
/// the process holds for them (RSS and file descriptors count BOTH ends,
/// since the client sockets live in this process too).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn half_open_connections_cost() {
    let _serial = SERIAL.lock().await;
    let (state, _store, _acct) = state_with(|_, _| {});
    let addr = serve_app(router(state)).await;

    let conns = env_usize("SQUELCH_PROBE_CONNS", 1000);
    let samples: Vec<u64> = std::env::var("SQUELCH_PROBE_SAMPLE_SECS")
        .unwrap_or_else(|_| "5,65,185,305".into())
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();

    let rss0 = rss_kb();
    let fds0 = open_fds();
    let started = Instant::now();
    let mut socks = Vec::with_capacity(conns);
    for _ in 0..conns {
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(b"GET /client/stats HTTP/1.1\r\nHost: probe\r\n")
            .await
            .unwrap();
        socks.push(s);
    }
    eprintln!(
        "BENCH half_open conns={conns} connect_ms={} rss_delta_kb={} fds_delta={}",
        started.elapsed().as_millis(),
        rss_kb().saturating_sub(rss0),
        open_fds().saturating_sub(fds0)
    );
    for at in samples {
        let until = started + Duration::from_secs(at);
        tokio::time::sleep_until(tokio::time::Instant::from_std(until)).await;
        let mut open = 0usize;
        for s in &socks {
            if still_open(s).await {
                open += 1;
            }
        }
        eprintln!(
            "BENCH half_open t={at}s still_open={open}/{conns} rss_delta_kb={} fds_delta={}",
            rss_kb().saturating_sub(rss0),
            open_fds().saturating_sub(fds0)
        );
    }
}

/// `SQUELCH_PROBE_STORM` (default 600) concurrent writes against a wedged
/// store: the peak thread count the process reaches, and what each request
/// got and when.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn wedged_store_storm_cost() {
    let _serial = SERIAL.lock().await;
    let fixture = FileStore::new("storm");
    let addr = serve_app(router(fixture.state())).await;
    let wedge = fixture.wedge();

    let storm = env_usize("SQUELCH_PROBE_STORM", 600);
    let base_threads = threads_now();
    let (peak, stop, sampler) = watch_threads();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(90))
        .build()
        .unwrap();
    let started = Instant::now();
    let mut tasks = Vec::with_capacity(storm);
    for _ in 0..storm {
        let (client, url) = (client.clone(), fixture.write_url(addr));
        tasks.push(tokio::spawn(async move {
            let r = client.post(&url).bearer_auth(TOKEN).send().await;
            let at = started.elapsed();
            match r {
                Ok(resp) => (resp.status().as_u16(), at),
                Err(e) if e.is_timeout() => (0, at),
                Err(_) => (1, at),
            }
        }));
    }
    let mut outcomes: std::collections::BTreeMap<u16, Vec<Duration>> = Default::default();
    for t in tasks {
        let (status, at) = t.await.unwrap();
        outcomes.entry(status).or_default().push(at);
    }
    stop.store(true, Ordering::Relaxed);
    sampler.join().unwrap();
    drop(wedge);
    settled().await;

    eprintln!(
        "BENCH storm n={storm} base_threads={base_threads} peak_threads={} total_ms={}",
        peak.load(Ordering::Relaxed),
        started.elapsed().as_millis()
    );
    for (status, mut ats) in outcomes {
        ats.sort();
        let label = match status {
            0 => "client-timeout".to_string(),
            1 => "connection-error".to_string(),
            s => s.to_string(),
        };
        eprintln!(
            "BENCH storm outcome={label} count={} first_ms={} median_ms={} last_ms={}",
            ats.len(),
            ats[0].as_millis(),
            ats[ats.len() / 2].as_millis(),
            ats[ats.len() - 1].as_millis()
        );
    }
}
