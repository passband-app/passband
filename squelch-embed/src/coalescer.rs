//! Two queues in front of one inference thread (docs/EMBED-SERVICE.md §2).
//!
//! A request's LANE is its shape: exactly one text is interactive (a search
//! query, or one message being ingested), two or more is backfill. The service
//! cannot name a tenant and must not need to, so shape is the only thing it
//! has. PRIORITY IS STRICT: the worker takes the next interactive request
//! whenever one is waiting, and a backfill request only when none is, which
//! is what keeps a search from queuing behind somebody's signup backfill.
//!
//! ONE FORWARD PASS PER REQUEST. Requests are never merged, so no pass ever
//! holds two callers' texts. A merged pass is a side channel between tenants:
//! a batch pads to its longest member, so how long my pass took would say how
//! long somebody else's text was, and whether it ran at all would say they
//! were embedding just then. A request is at most [`INFERENCE_BATCH`] texts,
//! which is also the most one pass takes, so a request never splits either.
//! The cost is throughput on short overlapping requests, which at this
//! fleet's load is nothing; there is no coalescing window to wait out.
//!
//! No cache, and no dedup inside a request (a byte-identical text sent twice
//! is run twice): a content-keyed shortcut is a cross-tenant oracle on
//! latency (§4). The texts are dropped when the pass returns.
//!
//! A request whose caller has gone (the HTTP handler was dropped because the
//! connection closed or the client timed out) is SKIPPED when it reaches the
//! front of its lane, never run. So is one that waited past its lane's
//! DEADLINE ([`protocol::INTERACTIVE_DEADLINE`],
//! [`protocol::BACKFILL_DEADLINE`]), which sits just under the daemon's own
//! timeout: a pass started after the caller gave up is a pass for nobody.
//! Both count as `abandoned`; an expired request whose caller is somehow
//! still there is answered 429.
//!
//! Lane shares bound memory and fairness at once: [`BACKFILL_SHARE`] texts of
//! backfill and [`INTERACTIVE_SHARE`] of interactive may wait, and past a
//! lane's share a request is refused (429) rather than queued, after the lane
//! has been swept of callers that are gone or expired. A backfill flood
//! cannot 429 a search and a search flood cannot crowd backfill out of the
//! queue. Interactive load sustained above the service's whole throughput
//! would starve backfill, which at one short text per search is a load nobody
//! generates; if it ever happens, backfill's `abandoned` and `queue_full`
//! counts say so.
//!
//! IF THE INFERENCE THREAD DIES (a panic outside the guarded forward pass),
//! the service stops being ready, every queued caller is answered 500 at
//! once rather than left to time out, nothing new is queued, and
//! [`Coalescer::died`] resolves so the binary exits non-zero for the kubelet
//! to restart it. A process answering 503 forever is worse than a restart.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::{oneshot, watch};

use crate::log::Log;
use crate::metrics::{Metrics, Outcome};
use crate::protocol;
use crate::session::{Loader, Session};

/// Texts per forward pass, which is one request. THIS IS THE MEMORY BOUND:
/// batch 8 at 256 tokens is about +123 MB of ORT scratch, and the pod limit
/// is sized from it (§2). Equal to the wire bound, which is why a request
/// never splits; if one of the two moves, the other moves with it or this
/// module grows a splitter.
pub const INFERENCE_BATCH: usize = protocol::MAX_TEXTS_PER_CALL;

/// Backfill's share of the queue, in texts: 12 full requests, about as many
/// as can start inside [`protocol::BACKFILL_DEADLINE`] on a shared core.
pub const BACKFILL_SHARE: usize = 96;
/// Interactive's share: 16 searches or single-message ingests, a fraction of
/// a second of work at the front of the queue.
pub const INTERACTIVE_SHARE: usize = 16;
/// Texts that may wait across both lanes.
pub const MAX_QUEUED_TEXTS: usize = BACKFILL_SHARE + INTERACTIVE_SHARE;

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

    /// How long a request in this lane may wait for its pass to start.
    pub fn deadline(self) -> Duration {
        match self {
            Self::Interactive => protocol::INTERACTIVE_DEADLINE,
            Self::Backfill => protocol::BACKFILL_DEADLINE,
        }
    }

    /// The `Retry-After` on a 429 from this lane, in seconds. Interactive is
    /// short because its lane drains first and holds at most 16 single texts;
    /// in practice the daemon does not retry a search at all, it degrades to
    /// keyword results. Backfill is the time a full lane takes to move
    /// meaningfully at §7's throughput, and the daemon's backoff treats it as
    /// a floor.
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

/// Why a request was not queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// The lane was over its share, even after sweeping it.
    QueueFull(Lane),
    /// The inference thread is gone.
    Stopped,
}

/// Why a queued request got no vectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The forward pass failed, or returned the wrong shape.
    Inference,
    /// It waited past its lane's deadline and was dropped unrun.
    Expired,
    /// The inference thread died with it still queued.
    Stopped,
}

/// What a request is answered with: its own vectors, in its own order.
pub type Reply = Result<Vec<Vec<f32>>, Failure>;

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
    /// The inference thread has exited; nothing is queued any more.
    dead: bool,
}

struct Shared {
    queues: Mutex<Queues>,
    wake: Condvar,
    /// Per lane, by [`Lane::index`]: [`Lane::deadline`] in production.
    deadlines: [Duration; 2],
    /// Raised once, when the inference thread exits for any reason.
    died: watch::Sender<bool>,
    /// Lets a test kill the worker outside the guarded forward pass.
    #[cfg(test)]
    kill: std::sync::atomic::AtomicBool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queues> {
        // Nothing holding this lock can panic midway through an update that
        // matters, so a poisoned lock is still a consistent one.
        self.queues.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn expired(&self, job: &Job, now: Instant) -> bool {
        now.saturating_duration_since(job.enqueued) >= self.deadlines[job.lane.index()]
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

/// Runs when the inference thread exits, by return or by panic: the service
/// stops being ready, every queued caller is answered, and nothing new is
/// accepted.
struct WorkerExit<'a> {
    shared: &'a Shared,
    metrics: &'a Metrics,
}

impl Drop for WorkerExit<'_> {
    fn drop(&mut self) {
        self.metrics.set_not_ready();
        let jobs: Vec<Job> = {
            let mut q = self.shared.lock();
            q.dead = true;
            q.waiting = [0, 0];
            q.lanes.iter_mut().flat_map(|l| l.drain(..)).collect()
        };
        for job in jobs {
            let _ = job.reply.send(Err(Failure::Stopped));
            self.metrics.record(job.lane, Outcome::Error);
        }
        self.shared.died.send_replace(true);
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
        Self::start_with(
            loader,
            metrics,
            log,
            [Lane::Interactive.deadline(), Lane::Backfill.deadline()],
        )
    }

    /// [`Self::start`] with other lane deadlines, so a test can wait
    /// milliseconds rather than seconds.
    pub(crate) fn start_with(
        loader: Loader,
        metrics: Arc<Metrics>,
        log: Log,
        deadlines: [Duration; 2],
    ) -> (Self, oneshot::Receiver<Result<(), String>>) {
        let shared = Arc::new(Shared {
            queues: Mutex::new(Queues::default()),
            wake: Condvar::new(),
            deadlines,
            died: watch::Sender::new(false),
            #[cfg(test)]
            kill: std::sync::atomic::AtomicBool::new(false),
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
                let _exit = WorkerExit {
                    shared: &worker_shared,
                    metrics: &worker_metrics,
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

    /// Resolves once the inference thread has exited, for any reason. The
    /// binary exits non-zero on it.
    pub async fn died(&self) {
        let mut rx = self.shared.died.subscribe();
        let _ = rx.wait_for(|dead| *dead).await;
    }

    /// Queue `texts` (1..=[`INFERENCE_BATCH`], already validated and
    /// truncated) and return where the answer will arrive. Refused, not
    /// queued, if its lane is over its share once callers that are gone or
    /// past their deadline have been swept out of it.
    pub fn submit(&self, texts: Vec<String>) -> Result<oneshot::Receiver<Reply>, Refused> {
        debug_assert!((1..=INFERENCE_BATCH).contains(&texts.len()));
        let lane = Lane::of(texts.len());
        let n = texts.len();
        let (tx, rx) = oneshot::channel();
        {
            let mut q = self.shared.lock();
            if q.dead || q.shutdown {
                return Err(Refused::Stopped);
            }
            let i = lane.index();
            if q.waiting[i] + n > lane.share() {
                sweep(&self.shared, &mut q, lane, &self.metrics);
            }
            if q.waiting[i] + n > lane.share() {
                return Err(Refused::QueueFull(lane));
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

    #[cfg(test)]
    pub(crate) fn kill_worker(&self) {
        self.shared
            .kill
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Drop from `lane` every request whose caller is gone or whose deadline has
/// passed: they would be skipped at the front anyway, and until then they
/// hold share a live caller could use.
fn sweep(shared: &Shared, q: &mut Queues, lane: Lane, metrics: &Metrics) {
    let now = Instant::now();
    let i = lane.index();
    let jobs = std::mem::take(&mut q.lanes[i]);
    for job in jobs {
        if job.reply.is_closed() || shared.expired(&job, now) {
            q.waiting[i] -= job.texts.len();
            let _ = job.reply.send(Err(Failure::Expired));
            metrics.record(lane, Outcome::Abandoned);
        } else {
            q.lanes[i].push_back(job);
        }
    }
}

fn worker(shared: &Shared, mut session: Box<dyn Session>, metrics: &Metrics, log: &Log) {
    while let Some(job) = next_job(shared, metrics) {
        #[cfg(test)]
        if shared.kill.load(std::sync::atomic::Ordering::SeqCst) {
            // Unguarded, on purpose: this is the death `WorkerExit` covers.
            drop(job);
            panic!("the test killed the inference thread");
        }
        run(session.as_mut(), job, metrics, log);
    }
}

/// Block until a request is due, then take it: the interactive lane's front
/// whenever it has one, else the backfill lane's. Requests met at a front
/// whose caller is gone or whose deadline has passed are dropped unrun.
/// `None` on shutdown.
fn next_job(shared: &Shared, metrics: &Metrics) -> Option<Job> {
    let mut q = shared.lock();
    loop {
        if q.shutdown {
            return None;
        }
        let now = Instant::now();
        for lane in Lane::ALL {
            let i = lane.index();
            while let Some(job) = q.lanes[i].pop_front() {
                q.waiting[i] -= job.texts.len();
                if job.reply.is_closed() {
                    metrics.record(lane, Outcome::Abandoned);
                    continue;
                }
                if shared.expired(&job, now) {
                    let _ = job.reply.send(Err(Failure::Expired));
                    metrics.record(lane, Outcome::Abandoned);
                    continue;
                }
                metrics.observe_queue_wait(lane, now.saturating_duration_since(job.enqueued));
                return Some(job);
            }
        }
        q = shared.wake.wait(q).unwrap_or_else(PoisonError::into_inner);
    }
}

fn run(session: &mut dyn Session, job: Job, metrics: &Metrics, log: &Log) {
    let Job {
        texts, lane, reply, ..
    } = job;
    let n = texts.len();
    let started = Instant::now();
    let result = catch_unwind(AssertUnwindSafe(|| session.embed(&texts)));
    metrics.observe_inference(started.elapsed(), n);
    // One copy of each text, alive for exactly this pass.
    drop(texts);

    let vectors = match result {
        Ok(Ok(v)) if v.len() == n && v.iter().all(|x| x.len() == protocol::DIMS) => Some(v),
        Ok(Ok(_)) => {
            log.line(&format!(
                "inference returned the wrong shape for a request of {n} texts"
            ));
            None
        }
        // The error itself is not logged: it was formatted by code holding
        // the texts (see `session::SessionError`).
        Ok(Err(_)) => {
            log.line(&format!("inference failed for a request of {n} texts"));
            None
        }
        Err(_) => {
            log.line(&format!("inference panicked on a request of {n} texts"));
            None
        }
    };

    let outcome = match vectors {
        Some(vectors) => match reply.send(Ok(vectors)) {
            Ok(()) => Outcome::Ok,
            Err(_) => Outcome::Abandoned,
        },
        None => {
            // A caller that has gone gets nothing either way; it is still a
            // failed request, which is what an operator is counting.
            let _ = reply.send(Err(Failure::Inference));
            Outcome::Error
        }
    };
    metrics.record(lane, outcome);
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

    /// Hold the worker in a first pass so that what is submitted next is
    /// queued together, deterministically, rather than racing the worker.
    fn held() -> (Fake, Arc<Gate>) {
        let gate = Gate::closed();
        let fake = Fake {
            gate: Some(gate.clone()),
            ..Default::default()
        };
        (fake, gate)
    }

    /// No pass ever holds two callers' texts: two requests queued together
    /// are two passes, each answered with its own vectors in its own order.
    #[test]
    fn requests_are_never_merged_into_one_pass() {
        let (fake, gate) = held();
        let (c, _) = start(&fake);
        let hold = c.submit(vec!["hold".into()]).unwrap();
        gate.wait_for_arrivals(1);

        let a = texts("a", 3);
        let b = texts("b", 4);
        let one = vec!["one".to_string()];
        let ra = c.submit(a.clone()).unwrap();
        let rb = c.submit(b.clone()).unwrap();
        let r1 = c.submit(one.clone()).unwrap();
        gate.open();

        wait(hold).unwrap();
        let va = wait(ra).unwrap();
        let vb = wait(rb).unwrap();
        wait(r1).unwrap();
        assert_eq!(va, a.iter().map(|t| vector_for(t)).collect::<Vec<_>>());
        assert_eq!(vb, b.iter().map(|t| vector_for(t)).collect::<Vec<_>>());

        // One pass each: the interactive one first, then backfill in order.
        let seen = fake.seen();
        assert_eq!(seen, vec![vec!["hold".to_string()], one, a, b]);
    }

    #[test]
    fn an_interactive_request_overtakes_queued_backfill() {
        let (fake, gate) = held();
        let (c, _) = start(&fake);
        let hold = c.submit(texts("hold", 8)).unwrap();
        gate.wait_for_arrivals(1);

        let backfill = c.submit(texts("bf", 8)).unwrap();
        let backfill2 = c.submit(texts("bg", 2)).unwrap();
        let query = c.submit(vec!["query".into()]).unwrap();
        let query2 = c.submit(vec!["query2".into()]).unwrap();
        gate.open();
        for rx in [hold, backfill, backfill2, query, query2] {
            wait(rx).unwrap();
        }

        let seen = fake.seen();
        assert_eq!(seen.len(), 5, "{seen:?}");
        assert_eq!(seen[1], vec!["query".to_string()], "the queries ran first");
        assert_eq!(seen[2], vec!["query2".to_string()]);
        assert_eq!(seen[3], texts("bf", 8));
        assert_eq!(seen[4], texts("bg", 2));
    }

    #[test]
    fn the_shares_are_sixteen_and_ninety_six_texts() {
        assert_eq!(INTERACTIVE_SHARE, 16);
        assert_eq!(BACKFILL_SHARE, 96);
        assert_eq!(MAX_QUEUED_TEXTS, 112);
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
            Some(Refused::QueueFull(Lane::Backfill))
        );
        for i in 0..INTERACTIVE_SHARE {
            pending.push(c.submit(vec![format!("q{i}")]).unwrap());
        }
        assert_eq!(
            c.submit(vec!["over".into()]).err(),
            Some(Refused::QueueFull(Lane::Interactive))
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

    fn start_quick(fake: &Fake, deadline: Duration) -> (Coalescer, Arc<Metrics>) {
        let metrics = Arc::new(Metrics::new());
        let (c, loaded) = Coalescer::start_with(
            fake.loader(),
            metrics.clone(),
            Log::new(|_| {}),
            [deadline, deadline],
        );
        loaded.blocking_recv().unwrap().unwrap();
        (c, metrics)
    }

    /// A request that waited past its lane's deadline is dropped unrun, and
    /// a caller still holding on is told so rather than given a late pass.
    #[test]
    fn a_request_past_its_deadline_is_dropped_unrun() {
        let (fake, gate) = held();
        let (c, metrics) = start_quick(&fake, Duration::from_millis(100));
        let hold = c.submit(vec!["hold".into()]).unwrap();
        gate.wait_for_arrivals(1);

        let late = c.submit(texts("late", 2)).unwrap();
        let late_query = c.submit(vec!["late query".into()]).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        let fresh = c.submit(texts("fresh", 2)).unwrap();
        gate.open();
        wait(hold).unwrap();
        assert_eq!(wait(late), Err(Failure::Expired));
        assert_eq!(wait(late_query), Err(Failure::Expired));
        wait(fresh).unwrap();

        let seen = fake.seen();
        assert!(
            seen.iter().flatten().all(|t| !t.starts_with("late")),
            "{seen:?}"
        );
        let out = metrics.render(c.depth());
        assert!(
            out.contains("squelch_embed_requests_total{lane=\"backfill\",outcome=\"abandoned\"} 1"),
            "{out}"
        );
        assert!(
            out.contains(
                "squelch_embed_requests_total{lane=\"interactive\",outcome=\"abandoned\"} 1"
            ),
            "{out}"
        );
    }

    /// A full lane is swept of callers that are gone, and of requests past
    /// their deadline, before a new request is refused for want of room.
    #[test]
    fn a_full_lane_is_swept_before_it_refuses() {
        let (fake, gate) = held();
        let (c, _) = start_quick(&fake, Duration::from_millis(100));
        let hold = c.submit(texts("hold", 2)).unwrap();
        gate.wait_for_arrivals(1);

        // Callers that hung up.
        for i in 0..INTERACTIVE_SHARE {
            drop(c.submit(vec![format!("gone{i}")]).unwrap());
        }
        assert_eq!(c.depth()[0], INTERACTIVE_SHARE);
        let live = c.submit(vec!["live".into()]).expect("swept, not refused");
        assert_eq!(c.depth()[0], 1);

        // Callers still waiting, but past the deadline.
        let mut stale = Vec::new();
        for i in 0..BACKFILL_SHARE / 8 {
            stale.push(c.submit(texts(&format!("stale{i}-"), 8)).unwrap());
        }
        assert_eq!(
            c.submit(texts("refused", 2)).err(),
            Some(Refused::QueueFull(Lane::Backfill)),
            "nothing expired yet, so the share holds"
        );
        std::thread::sleep(Duration::from_millis(150));
        let after = c.submit(texts("after", 2)).expect("expired ones swept");
        for rx in stale {
            assert_eq!(wait(rx), Err(Failure::Expired));
        }
        assert_eq!(c.depth()[1], 2);

        gate.open();
        wait(hold).unwrap();
        // `live` and `after` are past the short deadline by now too; what
        // matters is that they were queued, not refused.
        let _ = wait(live);
        let _ = wait(after);
    }

    /// The inference thread dying outside the guarded pass is not a hang:
    /// every queued caller is answered at once, the service stops being
    /// ready, nothing new is queued, and `died` resolves for the binary.
    #[test]
    fn a_dead_worker_answers_its_queue_and_stops_being_ready() {
        let (fake, gate) = held();
        let (c, metrics) = start(&fake);
        let hold = c.submit(vec!["hold".into()]).unwrap();
        gate.wait_for_arrivals(1);
        let doomed = c.submit(texts("doomed", 2)).unwrap();
        let queued = c.submit(texts("queued", 3)).unwrap();
        c.kill_worker();
        gate.open();
        wait(hold).unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let bound = Duration::from_secs(5);
            for rx in [doomed, queued] {
                let reply = tokio::time::timeout(bound, rx)
                    .await
                    .expect("a queued caller is answered, not left to time out");
                assert!(!matches!(reply, Ok(Ok(_))), "{reply:?}");
            }
            tokio::time::timeout(bound, c.died())
                .await
                .expect("died resolves");
        });
        assert!(!c.is_ready());
        assert_eq!(c.submit(vec!["after".into()]).err(), Some(Refused::Stopped));
        assert_eq!(c.depth(), [0, 0]);
        let out = metrics.render(c.depth());
        assert!(out.contains("squelch_embed_session_ready 0"), "{out}");
        assert!(
            out.contains("squelch_embed_requests_total{lane=\"backfill\",outcome=\"error\"} 1"),
            "{out}"
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
    fn a_failed_or_misshapen_pass_fails_its_request() {
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
            assert_eq!(
                wait(c.submit(texts("t", 2)).unwrap()),
                Err(Failure::Inference)
            );
            let out = metrics.render(c.depth());
            assert!(
                out.contains("squelch_embed_requests_total{lane=\"backfill\",outcome=\"error\"} 1")
            );
        }
    }

    /// No window to wait out: a lone request runs as soon as it is queued.
    #[test]
    fn a_lone_request_runs_at_once() {
        let fake = Fake::default();
        let (c, _) = start(&fake);
        let t = Instant::now();
        wait(c.submit(vec!["alone".into()]).unwrap()).unwrap();
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
