//! Two queues in front of one inference thread (docs/EMBED-SERVICE.md §2).
//!
//! A request's LANE is its shape: exactly one text is interactive (a search
//! query, or one message being ingested), two or more is backfill. The service
//! cannot name a tenant and must not need to, so shape is the only thing it
//! has. The interactive lane always drains first, which is what keeps a
//! search from queuing behind somebody's signup backfill.
//!
//! BATCHING ONLY MERGES. A request is at most [`INFERENCE_BATCH`] texts, which
//! is also the most one forward pass takes, so a request never splits across
//! two runs and nothing here has to reassemble one. The worker waits up to
//! [`COALESCE_WINDOW`] after the oldest waiting request arrived, or until
//! [`INFERENCE_BATCH`] texts are waiting, then takes whole requests, lane by
//! lane and in arrival order within a lane, while they fit. A full 8-text
//! backfill request fills a batch by itself; merging buys a forward pass only
//! when short requests actually overlap.
//!
//! Two tenants' texts can sit in one batch. The batch is a positional array
//! that lives for one forward pass: results go back by index to the request
//! that supplied them, and the texts are dropped when the pass returns. No
//! cache, and no dedup inside a batch either (a byte-identical text from two
//! requests is run twice): a content-keyed shortcut is a cross-tenant oracle
//! on latency (§4).
//!
//! A request whose caller has gone (the HTTP handler was dropped because the
//! connection closed or the client timed out) is SKIPPED when it reaches the
//! front of its lane, never run.
//!
//! Lane shares bound memory and fairness at once: 192 texts of backfill and 64
//! of interactive may wait, and past a lane's share a request is refused
//! (429) rather than queued, so a backfill flood cannot 429 a search and a
//! search flood cannot crowd backfill out of the queue. Priority is strict:
//! interactive load sustained above the service's whole throughput would
//! starve backfill, which at one short text per search is a load nobody
//! generates; if it ever happens, backfill's 429s and queue-wait say so.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

use crate::log::Log;
use crate::metrics::{Metrics, Outcome};
use crate::protocol;
use crate::session::{Loader, Session};

/// How long a waiting request may sit for company before its batch fires.
/// Invisible at query time, where a search already spends tens of ms in
/// SQLite.
pub const COALESCE_WINDOW: Duration = Duration::from_millis(10);

/// Texts per forward pass. THIS IS THE MEMORY BOUND: batch 8 at 256 tokens is
/// about +123 MB of ORT scratch, and the pod limit is sized from it (§2).
/// Equal to the wire bound, which is why a request never splits; if one of
/// the two moves, the other moves with it or this module grows a splitter.
pub const INFERENCE_BATCH: usize = protocol::MAX_TEXTS_PER_CALL;

/// Texts that may wait across both lanes.
pub const MAX_QUEUED_TEXTS: usize = 256;
/// Backfill's share of [`MAX_QUEUED_TEXTS`]: 24 full requests.
pub const BACKFILL_SHARE: usize = 192;
/// Interactive's share: 64 searches or single-message ingests.
pub const INTERACTIVE_SHARE: usize = MAX_QUEUED_TEXTS - BACKFILL_SHARE;

/// Which queue a request waits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    Interactive,
    Backfill,
}

impl Lane {
    pub const ALL: [Self; 2] = [Self::Interactive, Self::Backfill];

    /// By shape alone. The one misclassification, a backfill whose last chunk
    /// has one row, costs one forward pass of queue-jumping.
    pub fn of(texts: usize) -> Self {
        if texts == 1 {
            Self::Interactive
        } else {
            Self::Backfill
        }
    }

    pub fn share(self) -> usize {
        match self {
            Self::Interactive => INTERACTIVE_SHARE,
            Self::Backfill => BACKFILL_SHARE,
        }
    }

    /// The `Retry-After` on a 429 from this lane, in seconds. Interactive is
    /// short because its lane drains first and holds at most 64 single texts,
    /// a few seconds of work; in practice the daemon does not retry a search
    /// at all, it degrades to keyword results. Backfill is the time a full
    /// lane takes to move meaningfully at §7's throughput, and the daemon's
    /// backoff treats it as a floor.
    pub fn retry_after_secs(self) -> u64 {
        match self {
            Self::Interactive => 2,
            Self::Backfill => 10,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Backfill => "backfill",
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }
}

/// The lane was over its share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueFull(pub Lane);

/// The forward pass failed, or returned the wrong shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InferenceFailed;

/// What a request is answered with: its own vectors, in its own order.
pub type Reply = Result<Vec<Vec<f32>>, InferenceFailed>;

struct Job {
    texts: Vec<String>,
    lane: Lane,
    enqueued: Instant,
    reply: oneshot::Sender<Reply>,
}

#[derive(Default)]
struct Queues {
    lanes: [VecDeque<Job>; 2],
    /// Texts waiting per lane, which is what the shares bound.
    waiting: [usize; 2],
    shutdown: bool,
}

struct Shared {
    queues: Mutex<Queues>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queues> {
        // Nothing holding this lock can panic midway through an update that
        // matters, so a poisoned lock is still a consistent one.
        self.queues.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Stops the worker when the last [`Coalescer`] clone goes.
struct StopOnDrop(Arc<Shared>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.lock().shutdown = true;
        self.0.wake.notify_all();
    }
}

/// The handle the HTTP side submits through. Cheap to clone.
#[derive(Clone)]
pub struct Coalescer {
    shared: Arc<Shared>,
    metrics: Arc<Metrics>,
    _stop: Arc<StopOnDrop>,
}

impl Coalescer {
    /// Start the inference thread. It builds the session first (from `loader`,
    /// on that thread, so the listeners answer while the weights load) and
    /// reports how that went on the returned channel: `Err` means the process
    /// should exit, because a service that will never be ready is better
    /// restarted by the kubelet than left answering 503.
    pub fn start(
        loader: Loader,
        metrics: Arc<Metrics>,
        log: Log,
    ) -> (Self, oneshot::Receiver<Result<(), String>>) {
        let shared = Arc::new(Shared {
            queues: Mutex::new(Queues::default()),
            wake: Condvar::new(),
        });
        let (loaded_tx, loaded_rx) = oneshot::channel();
        let worker_shared = shared.clone();
        let worker_metrics = metrics.clone();
        std::thread::Builder::new()
            .name("squelch-embed-inference".to_string())
            .spawn(move || {
                let started = Instant::now();
                let session = match catch_unwind(AssertUnwindSafe(loader)) {
                    Ok(Ok(session)) => session,
                    Ok(Err(e)) => {
                        let _ = loaded_tx.send(Err(e));
                        return;
                    }
                    Err(_) => {
                        let _ = loaded_tx.send(Err("session loader panicked".to_string()));
                        return;
                    }
                };
                worker_metrics.set_ready();
                log.line(&format!(
                    "session ready: {} ({}-dim, max_length {}) in {} ms",
                    protocol::MODEL_CODE,
                    protocol::DIMS,
                    protocol::MAX_TOKENS,
                    started.elapsed().as_millis()
                ));
                let _ = loaded_tx.send(Ok(()));
                worker(&worker_shared, session, &worker_metrics, &log);
            })
            .expect("spawn the inference thread");
        let stop = Arc::new(StopOnDrop(shared.clone()));
        (
            Self {
                shared,
                metrics,
                _stop: stop,
            },
            loaded_rx,
        )
    }

    pub fn is_ready(&self) -> bool {
        self.metrics.is_ready()
    }

    /// Queue `texts` (1..=[`INFERENCE_BATCH`], already validated and
    /// truncated) and return where the answer will arrive. Refused, not
    /// queued, if its lane is over its share.
    pub fn submit(&self, texts: Vec<String>) -> Result<oneshot::Receiver<Reply>, QueueFull> {
        debug_assert!((1..=INFERENCE_BATCH).contains(&texts.len()));
        let lane = Lane::of(texts.len());
        let n = texts.len();
        let (tx, rx) = oneshot::channel();
        {
            let mut q = self.shared.lock();
            let i = lane.index();
            if q.waiting[i] + n > lane.share() {
                return Err(QueueFull(lane));
            }
            q.waiting[i] += n;
            q.lanes[i].push_back(Job {
                texts,
                lane,
                enqueued: Instant::now(),
                reply: tx,
            });
        }
        self.metrics.accepted_texts(lane, n);
        self.shared.wake.notify_one();
        Ok(rx)
    }

    /// Texts waiting per lane, indexed by [`Lane::index`].
    pub fn depth(&self) -> [usize; 2] {
        self.shared.lock().waiting
    }
}

fn worker(shared: &Shared, mut session: Box<dyn Session>, metrics: &Metrics, log: &Log) {
    while let Some(batch) = next_batch(shared, metrics) {
        run_batch(session.as_mut(), batch, metrics, log);
    }
}

/// Block until a batch is due, then take it. `None` on shutdown.
fn next_batch(shared: &Shared, metrics: &Metrics) -> Option<Vec<Job>> {
    let mut q = shared.lock();
    loop {
        if q.shutdown {
            return None;
        }
        let waiting: usize = q.waiting.iter().sum();
        if waiting == 0 {
            q = shared.wake.wait(q).unwrap_or_else(PoisonError::into_inner);
            continue;
        }
        let oldest = q
            .lanes
            .iter()
            .filter_map(|l| l.front().map(|j| j.enqueued))
            .min()
            .expect("waiting > 0 means a job is queued");
        let due = oldest + COALESCE_WINDOW;
        let now = Instant::now();
        if waiting >= INFERENCE_BATCH || now >= due {
            let batch = take_batch(&mut q, metrics);
            if batch.is_empty() {
                // Everything that was due had been abandoned; look again.
                continue;
            }
            return Some(batch);
        }
        q = shared
            .wake
            .wait_timeout(q, due - now)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

/// Whole requests, interactive lane first, arrival order within a lane, while
/// they fit in [`INFERENCE_BATCH`]. Abandoned requests met at a lane's front
/// are dropped without running.
fn take_batch(q: &mut Queues, metrics: &Metrics) -> Vec<Job> {
    let mut batch = Vec::new();
    let mut texts = 0;
    for lane in Lane::ALL {
        let i = lane.index();
        while let Some(front) = q.lanes[i].front() {
            if front.reply.is_closed() {
                let job = q.lanes[i].pop_front().expect("front exists");
                q.waiting[i] -= job.texts.len();
                metrics.record(lane, Outcome::Abandoned);
                continue;
            }
            if texts + front.texts.len() > INFERENCE_BATCH {
                break;
            }
            let job = q.lanes[i].pop_front().expect("front exists");
            q.waiting[i] -= job.texts.len();
            texts += job.texts.len();
            metrics.observe_queue_wait(lane, job.enqueued.elapsed());
            batch.push(job);
        }
    }
    batch
}

fn run_batch(session: &mut dyn Session, mut batch: Vec<Job>, metrics: &Metrics, log: &Log) {
    // Move the texts out rather than copy them: one copy of each, alive for
    // exactly this pass.
    let mut texts = Vec::with_capacity(INFERENCE_BATCH);
    let mut spans = Vec::with_capacity(batch.len());
    for job in &mut batch {
        spans.push(job.texts.len());
        texts.append(&mut job.texts);
    }
    let n = texts.len();
    let started = Instant::now();
    let result = catch_unwind(AssertUnwindSafe(|| session.embed(&texts)));
    metrics.observe_inference(started.elapsed(), n);
    drop(texts);

    let vectors = match result {
        Ok(Ok(v)) if v.len() == n && v.iter().all(|x| x.len() == protocol::DIMS) => Some(v),
        Ok(Ok(_)) => {
            log.line(&format!(
                "inference returned the wrong shape for a batch of {n} texts from {} requests",
                batch.len()
            ));
            None
        }
        // The error itself is not logged: it was formatted by code holding
        // the texts (see `session::SessionError`).
        Ok(Err(_)) => {
            log.line(&format!(
                "inference failed for a batch of {n} texts from {} requests",
                batch.len()
            ));
            None
        }
        Err(_) => {
            log.line(&format!(
                "inference panicked on a batch of {n} texts from {} requests",
                batch.len()
            ));
            None
        }
    };

    match vectors {
        Some(vectors) => {
            let mut vectors = vectors.into_iter();
            for (job, k) in batch.into_iter().zip(spans) {
                let own: Vec<Vec<f32>> = vectors.by_ref().take(k).collect();
                let outcome = match job.reply.send(Ok(own)) {
                    Ok(()) => Outcome::Ok,
                    Err(_) => Outcome::Abandoned,
                };
                metrics.record(job.lane, outcome);
            }
        }
        None => {
            for job in batch {
                // A caller that has gone gets nothing either way; it is still
                // a failed request, which is what an operator is counting.
                let _ = job.reply.send(Err(InferenceFailed));
                metrics.record(job.lane, Outcome::Error);
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::session::SessionError;

    /// A deterministic stand-in vector: every component derived from the
    /// text, so a test can tell whose vector came back where.
    pub(crate) fn vector_for(text: &str) -> Vec<f32> {
        let mut h: u32 = 2166136261;
        for b in text.bytes() {
            h = (h ^ b as u32).wrapping_mul(16777619);
        }
        let mut v = vec![0.0; protocol::DIMS];
        v[0] = (h % 1_000_000) as f32;
        v[1] = text.chars().count() as f32;
        v
    }

    /// Lets a test hold the worker inside a batch.
    #[derive(Default)]
    pub(crate) struct Gate {
        state: Mutex<(bool, usize)>,
        cv: Condvar,
    }

    impl Gate {
        pub(crate) fn closed() -> Arc<Self> {
            Arc::new(Self::default())
        }
        pub(crate) fn open(&self) {
            self.state.lock().unwrap().0 = true;
            self.cv.notify_all();
        }
        /// Block until opened, counting this caller as having arrived.
        fn pass(&self) {
            let mut s = self.state.lock().unwrap();
            s.1 += 1;
            self.cv.notify_all();
            while !s.0 {
                s = self.cv.wait(s).unwrap();
            }
        }
        /// Block until opened, without counting as an arrival.
        pub(crate) fn wait_open(&self) {
            let mut s = self.state.lock().unwrap();
            while !s.0 {
                s = self.cv.wait(s).unwrap();
            }
        }
        /// Block until `n` callers have arrived at the gate.
        pub(crate) fn wait_for_arrivals(&self, n: usize) {
            let mut s = self.state.lock().unwrap();
            while s.1 < n {
                s = self.cv.wait(s).unwrap();
            }
        }
    }

    /// What the fake does, and what it saw: every batch, in order.
    #[derive(Clone, Default)]
    pub(crate) struct Fake {
        pub(crate) batches: Arc<Mutex<Vec<Vec<String>>>>,
        pub(crate) gate: Option<Arc<Gate>>,
        pub(crate) fail: bool,
        pub(crate) short_vectors: bool,
    }

    impl Session for Fake {
        fn embed(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, SessionError> {
            self.batches.lock().unwrap().push(texts.to_vec());
            if let Some(gate) = &self.gate {
                gate.pass();
            }
            if self.fail {
                return Err(SessionError);
            }
            Ok(texts
                .iter()
                .map(|t| {
                    let mut v = vector_for(t);
                    if self.short_vectors {
                        v.pop();
                    }
                    v
                })
                .collect())
        }
    }

    impl Fake {
        pub(crate) fn loader(&self) -> Loader {
            let fake = self.clone();
            Box::new(move || Ok(Box::new(fake) as Box<dyn Session>))
        }
        pub(crate) fn seen(&self) -> Vec<Vec<String>> {
            self.batches.lock().unwrap().clone()
        }
    }

    pub(crate) fn start(fake: &Fake) -> (Coalescer, Arc<Metrics>) {
        let metrics = Arc::new(Metrics::new());
        let (c, loaded) = Coalescer::start(fake.loader(), metrics.clone(), Log::new(|_| {}));
        loaded.blocking_recv().unwrap().unwrap();
        (c, metrics)
    }

    fn texts(prefix: &str, n: usize) -> Vec<String> {
        (0..n).map(|i| format!("{prefix}{i}")).collect()
    }

    fn wait(rx: oneshot::Receiver<Reply>) -> Reply {
        rx.blocking_recv().expect("worker answered")
    }

    /// Hold the worker in a first batch so that what is submitted next is
    /// queued together, deterministically, rather than racing the window.
    fn held() -> (Fake, Arc<Gate>) {
        let gate = Gate::closed();
        let fake = Fake {
            gate: Some(gate.clone()),
            ..Default::default()
        };
        (fake, gate)
    }

    #[test]
    fn two_requests_merged_into_one_batch_get_their_own_vectors_in_order() {
        let (fake, gate) = held();
        let (c, _) = start(&fake);
        let hold = c.submit(vec!["hold".into()]).unwrap();
        gate.wait_for_arrivals(1);

        let a = texts("a", 3);
        let b = texts("b", 4);
        let ra = c.submit(a.clone()).unwrap();
        let rb = c.submit(b.clone()).unwrap();
        gate.open();

        wait(hold).unwrap();
        let va = wait(ra).unwrap();
        let vb = wait(rb).unwrap();
        assert_eq!(va, a.iter().map(|t| vector_for(t)).collect::<Vec<_>>());
        assert_eq!(vb, b.iter().map(|t| vector_for(t)).collect::<Vec<_>>());

        // ONE pass for both, in arrival order.
        let seen = fake.seen();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert_eq!(seen[1], [a, b].concat());
    }

    #[test]
    fn an_interactive_request_overtakes_queued_backfill() {
        let (fake, gate) = held();
        let (c, _) = start(&fake);
        let hold = c.submit(texts("hold", 8)).unwrap();
        gate.wait_for_arrivals(1);

        let backfill = c.submit(texts("bf", 8)).unwrap();
        let query = c.submit(vec!["query".into()]).unwrap();
        gate.open();
        for rx in [hold, backfill, query] {
            wait(rx).unwrap();
        }

        let seen = fake.seen();
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert_eq!(seen[1], vec!["query".to_string()], "the query ran first");
        assert_eq!(seen[2], texts("bf", 8));
    }

    #[test]
    fn a_request_never_splits_and_whole_requests_fill_around_it() {
        let (fake, gate) = held();
        let (c, _) = start(&fake);
        let hold = c.submit(vec!["hold".into()]).unwrap();
        gate.wait_for_arrivals(1);

        // 5 + 5 cannot share a pass; the second 5 waits for the next one
        // rather than being cut to 3.
        let r1 = c.submit(texts("x", 5)).unwrap();
        let r2 = c.submit(texts("y", 5)).unwrap();
        gate.open();
        for rx in [hold, r1, r2] {
            wait(rx).unwrap();
        }
        let seen = fake.seen();
        assert_eq!(seen[1], texts("x", 5));
        assert_eq!(seen[2], texts("y", 5));
    }

    #[test]
    fn each_lane_refuses_past_its_share_and_not_the_others() {
        let (fake, gate) = held();
        let (c, _) = start(&fake);
        let hold = c.submit(vec!["hold".into()]).unwrap();
        gate.wait_for_arrivals(1);

        let mut pending = Vec::new();
        for i in 0..BACKFILL_SHARE / 8 {
            pending.push(c.submit(texts(&format!("b{i}-"), 8)).unwrap());
        }
        assert_eq!(
            c.submit(texts("over", 2)).err(),
            Some(QueueFull(Lane::Backfill))
        );
        for i in 0..INTERACTIVE_SHARE {
            pending.push(c.submit(vec![format!("q{i}")]).unwrap());
        }
        assert_eq!(
            c.submit(vec!["over".into()]).err(),
            Some(QueueFull(Lane::Interactive))
        );
        assert_eq!(c.depth(), [INTERACTIVE_SHARE, BACKFILL_SHARE]);

        gate.open();
        wait(hold).unwrap();
        for rx in pending {
            wait(rx).unwrap();
        }
        assert_eq!(c.depth(), [0, 0]);
    }

    #[test]
    fn an_abandoned_request_is_skipped_not_run() {
        let (fake, gate) = held();
        let (c, metrics) = start(&fake);
        let hold = c.submit(vec!["hold".into()]).unwrap();
        gate.wait_for_arrivals(1);

        drop(c.submit(texts("gone", 3)).unwrap());
        let kept = c.submit(texts("kept", 2)).unwrap();
        gate.open();
        wait(hold).unwrap();
        wait(kept).unwrap();

        let seen = fake.seen();
        assert!(
            seen.iter().flatten().all(|t| !t.starts_with("gone")),
            "{seen:?}"
        );
        let out = metrics.render(c.depth());
        assert!(
            out.contains("squelch_embed_requests_total{lane=\"backfill\",outcome=\"abandoned\"} 1")
        );
    }

    /// No dedup: two byte-identical texts are two rows of the batch and two
    /// vectors, and the same text sent again is run again (no cache).
    #[test]
    fn identical_texts_are_each_run_every_time() {
        let fake = Fake::default();
        let (c, _) = start(&fake);
        let same = vec!["same".to_string(), "same".to_string()];
        assert_eq!(wait(c.submit(same.clone()).unwrap()).unwrap().len(), 2);
        assert_eq!(wait(c.submit(same.clone()).unwrap()).unwrap().len(), 2);
        assert_eq!(fake.seen(), vec![same.clone(), same]);
    }

    #[test]
    fn a_failed_or_misshapen_pass_fails_every_request_in_it() {
        for fake in [
            Fake {
                fail: true,
                ..Default::default()
            },
            Fake {
                short_vectors: true,
                ..Default::default()
            },
        ] {
            let (c, metrics) = start(&fake);
            assert_eq!(wait(c.submit(texts("t", 2)).unwrap()), Err(InferenceFailed));
            let out = metrics.render(c.depth());
            assert!(
                out.contains("squelch_embed_requests_total{lane=\"backfill\",outcome=\"error\"} 1")
            );
        }
    }

    #[test]
    fn a_lone_request_fires_after_the_window_not_never() {
        let fake = Fake::default();
        let (c, _) = start(&fake);
        let t = Instant::now();
        wait(c.submit(vec!["alone".into()]).unwrap()).unwrap();
        assert!(t.elapsed() >= COALESCE_WINDOW);
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_failed_loader_reports_and_never_becomes_ready() {
        let metrics = Arc::new(Metrics::new());
        let (c, loaded) = Coalescer::start(
            Box::new(|| Err("no weights".to_string())),
            metrics,
            Log::new(|_| {}),
        );
        assert_eq!(
            loaded.blocking_recv().unwrap(),
            Err("no weights".to_string())
        );
        assert!(!c.is_ready());
    }
}
