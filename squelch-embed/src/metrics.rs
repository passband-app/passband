//! The service's counters, in Prometheus text format, on [`crate::protocol::METRICS_PORT`].
//!
//! Hand-rolled exposition in the shape of `squelch-core/src/metrics.rs`: a
//! handful of families and one GET is not worth a client library.
//!
//! EVERY LABEL VALUE COMES FROM AN ENUM. `lane` is [`LaneLabel`] and `outcome`
//! is [`Outcome`], both closed, and nothing a request carried (its text, its
//! length, the address it came from) can become a label or a series. That is
//! the claim the scrape is safe to leave unauthenticated on: a per-tenant or
//! per-peer series would put the fleet's mail volume, tenant by tenant, on a
//! port anything in the monitoring namespace can read. The test at the bottom
//! of this file walks a rendered exposition and refuses any label value
//! outside these sets and anything that looks like an IP address.
//!
//! Every family is PRESENT AT ZERO, so a service that has served nothing and a
//! scrape that is broken do not look the same.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use crate::coalescer::Lane;

/// How one `POST /embed` ended. Each request is counted exactly once: the
/// handler records the outcomes it decides (everything before the queue), the
/// inference worker the ones it decides (everything after), because a handler
/// whose caller hung up is dropped before it could record anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 200.
    Ok,
    /// 400: empty, too many texts, bad JSON, body too large or too slow.
    Rejected,
    /// 429: the request's lane was full.
    QueueFull,
    /// 503: the session was not built yet.
    NotReady,
    /// 500: inference failed, or returned the wrong shape.
    Error,
    /// 401: missing or wrong bearer token.
    Unauthorized,
    /// The caller was gone by the time its texts reached the front of the
    /// queue (skipped, never run) or by the time its vectors came back.
    Abandoned,
}

impl Outcome {
    pub const ALL: [Self; 7] = [
        Self::Ok,
        Self::Rejected,
        Self::QueueFull,
        Self::NotReady,
        Self::Error,
        Self::Unauthorized,
        Self::Abandoned,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Rejected => "rejected",
            Self::QueueFull => "queue_full",
            Self::NotReady => "not_ready",
            Self::Error => "error",
            Self::Unauthorized => "unauthorized",
            Self::Abandoned => "abandoned",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// The `lane` label on `requests_total`. The two lanes, plus `none` for a
/// request refused before it had a lane: a 401 is decided before the body is
/// read, and a body that does not parse, or carries no texts, has no shape to
/// classify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneLabel {
    Interactive,
    Backfill,
    None,
}

impl LaneLabel {
    pub const ALL: [Self; 3] = [Self::Interactive, Self::Backfill, Self::None];

    pub fn label(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Backfill => "backfill",
            Self::None => "none",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

impl From<Lane> for LaneLabel {
    fn from(lane: Lane) -> Self {
        match lane {
            Lane::Interactive => Self::Interactive,
            Lane::Backfill => Self::Backfill,
        }
    }
}

impl From<Option<Lane>> for LaneLabel {
    fn from(lane: Option<Lane>) -> Self {
        lane.map_or(Self::None, Self::from)
    }
}

/// Seconds a request waits between being queued and its batch starting. The
/// interesting range runs from "straight in" (a few ms) to "a
/// signup backfill is in front of you" (tens of seconds, §2's arithmetic for a
/// full backfill lane on a shared core).
pub const QUEUE_WAIT_BUCKETS: [f64; 12] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 10.0, 30.0, 60.0,
];

/// Seconds one forward pass takes. A single short query is a few ms, a full
/// batch of 8 at 256 tokens on a shared core is the better part of a second
/// or more (§7).
pub const INFERENCE_BUCKETS: [f64; 10] = [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 10.0];

/// Texts per forward pass, 1 to [`crate::protocol::MAX_TEXTS_PER_CALL`]. One
/// bucket per possible size, so the histogram is the exact distribution: how
/// big the requests are is the question it answers: a pass is one request,
/// never a merge of several.
pub const BATCH_SIZE_BUCKETS: [f64; 8] = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];

/// One unlabelled histogram over fixed bounds. Buckets are stored
/// non-cumulative and cumulated at render, so an observation is one atomic add.
#[derive(Debug)]
struct Histogram {
    bounds: &'static [f64],
    buckets: Vec<AtomicU64>,
    overflow: AtomicU64,
    count: AtomicU64,
    /// The sum in microseconds (or in units, for a size histogram scaled the
    /// same way), so it stays an integer under an atomic.
    sum_micros: AtomicU64,
}

impl Histogram {
    fn new(bounds: &'static [f64]) -> Self {
        Self {
            bounds,
            buckets: bounds.iter().map(|_| AtomicU64::new(0)).collect(),
            overflow: AtomicU64::new(0),
            count: AtomicU64::new(0),
            sum_micros: AtomicU64::new(0),
        }
    }

    fn observe(&self, value: f64) {
        let value = if value.is_finite() {
            value.max(0.0)
        } else {
            0.0
        };
        match self.bounds.iter().position(|b| value <= *b) {
            Some(i) => self.buckets[i].fetch_add(1, Ordering::Relaxed),
            None => self.overflow.fetch_add(1, Ordering::Relaxed),
        };
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_micros
            .fetch_add((value * 1e6).round() as u64, Ordering::Relaxed);
    }

    fn render(&self, e: &mut Exposition, name: &str, labels: &[(&str, &str)]) {
        let bucket = format!("{name}_bucket");
        let mut cumulative = 0u64;
        for (i, bound) in self.bounds.iter().enumerate() {
            cumulative += self.buckets[i].load(Ordering::Relaxed);
            let le = fmt_value(*bound);
            let mut l = labels.to_vec();
            l.push(("le", le.as_str()));
            e.sample(&bucket, &l, cumulative as f64);
        }
        let total = self.count.load(Ordering::Relaxed);
        let mut l = labels.to_vec();
        l.push(("le", "+Inf"));
        e.sample(&bucket, &l, total as f64);
        e.sample(
            &format!("{name}_sum"),
            labels,
            self.sum_micros.load(Ordering::Relaxed) as f64 / 1e6,
        );
        e.sample(&format!("{name}_count"), labels, total as f64);
    }
}

/// The registry. Atomics only; lost on restart by design.
#[derive(Debug)]
pub struct Metrics {
    requests: [[AtomicU64; Outcome::ALL.len()]; LaneLabel::ALL.len()],
    texts: [AtomicU64; 2],
    queue_wait: [Histogram; 2],
    inference: Histogram,
    batch_size: Histogram,
    session_ready: AtomicBool,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            requests: Default::default(),
            texts: Default::default(),
            queue_wait: [
                Histogram::new(&QUEUE_WAIT_BUCKETS),
                Histogram::new(&QUEUE_WAIT_BUCKETS),
            ],
            inference: Histogram::new(&INFERENCE_BUCKETS),
            batch_size: Histogram::new(&BATCH_SIZE_BUCKETS),
            session_ready: AtomicBool::new(false),
        }
    }

    pub fn record(&self, lane: impl Into<LaneLabel>, outcome: Outcome) {
        self.requests[lane.into().index()][outcome.index()].fetch_add(1, Ordering::Relaxed);
    }

    /// Texts accepted into a lane's queue.
    pub fn accepted_texts(&self, lane: Lane, n: usize) {
        self.texts[lane.index()].fetch_add(n as u64, Ordering::Relaxed);
    }

    pub fn observe_queue_wait(&self, lane: Lane, waited: Duration) {
        self.queue_wait[lane.index()].observe(waited.as_secs_f64());
    }

    pub fn observe_inference(&self, took: Duration, texts: usize) {
        self.inference.observe(took.as_secs_f64());
        self.batch_size.observe(texts as f64);
    }

    pub fn set_ready(&self) {
        self.session_ready.store(true, Ordering::Release);
    }

    /// The inference thread is gone: `/healthz` answers 503 again and the
    /// readiness probe takes the pod out of the Service.
    pub fn set_not_ready(&self) {
        self.session_ready.store(false, Ordering::Release);
    }

    pub fn is_ready(&self) -> bool {
        self.session_ready.load(Ordering::Acquire)
    }

    /// The exposition. `depth` is texts waiting per lane right now, read from
    /// the coalescer at scrape time, indexed by [`Lane::index`].
    pub fn render(&self, depth: [usize; 2]) -> String {
        let mut e = Exposition::default();

        e.family(
            "squelch_embed_build_info",
            "gauge",
            "Always 1; the labels say which build and which model this is.",
        );
        e.sample(
            "squelch_embed_build_info",
            &[
                ("version", env!("CARGO_PKG_VERSION")),
                ("model", crate::protocol::MODEL_CODE),
            ],
            1.0,
        );

        e.family(
            "squelch_embed_session_ready",
            "gauge",
            "1 once the ONNX session is built and /healthz answers 200.",
        );
        e.sample(
            "squelch_embed_session_ready",
            &[],
            if self.is_ready() { 1.0 } else { 0.0 },
        );

        e.family(
            "squelch_embed_requests_total",
            "counter",
            "POST /embed requests by lane and how they ended. lane=none is a request \
             refused before it had a shape (401, unparseable or empty body).",
        );
        for lane in LaneLabel::ALL {
            for outcome in Outcome::ALL {
                e.sample(
                    "squelch_embed_requests_total",
                    &[("lane", lane.label()), ("outcome", outcome.label())],
                    self.requests[lane.index()][outcome.index()].load(Ordering::Relaxed) as f64,
                );
            }
        }

        e.family(
            "squelch_embed_texts_total",
            "counter",
            "Texts accepted into a lane's queue.",
        );
        for lane in Lane::ALL {
            e.sample(
                "squelch_embed_texts_total",
                &[("lane", lane.label())],
                self.texts[lane.index()].load(Ordering::Relaxed) as f64,
            );
        }

        e.family(
            "squelch_embed_queue_depth",
            "gauge",
            "Texts waiting in a lane right now, not counting the batch in flight.",
        );
        for lane in Lane::ALL {
            e.sample(
                "squelch_embed_queue_depth",
                &[("lane", lane.label())],
                depth[lane.index()] as f64,
            );
        }

        e.family(
            "squelch_embed_queue_wait_seconds",
            "histogram",
            "Time from a request being queued to its batch starting, by lane.",
        );
        for lane in Lane::ALL {
            self.queue_wait[lane.index()].render(
                &mut e,
                "squelch_embed_queue_wait_seconds",
                &[("lane", lane.label())],
            );
        }

        e.family(
            "squelch_embed_inference_seconds",
            "histogram",
            "Duration of one forward pass.",
        );
        self.inference
            .render(&mut e, "squelch_embed_inference_seconds", &[]);

        e.family(
            "squelch_embed_batch_size",
            "histogram",
            "Texts per forward pass. One pass is one request; requests are never merged.",
        );
        self.batch_size
            .render(&mut e, "squelch_embed_batch_size", &[]);

        e.finish()
    }
}

/// Builder for the text exposition format (v0.0.4), as in squelch-core.
#[derive(Debug, Default)]
struct Exposition {
    out: String,
}

impl Exposition {
    fn family(&mut self, name: &str, kind: &str, help: &str) {
        self.out.push_str("# HELP ");
        self.out.push_str(name);
        self.out.push(' ');
        for c in help.chars() {
            match c {
                '\\' => self.out.push_str("\\\\"),
                '\n' => self.out.push_str("\\n"),
                c => self.out.push(c),
            }
        }
        self.out.push_str("\n# TYPE ");
        self.out.push_str(name);
        self.out.push(' ');
        self.out.push_str(kind);
        self.out.push('\n');
    }

    fn sample(&mut self, name: &str, labels: &[(&str, &str)], value: f64) {
        self.out.push_str(name);
        if !labels.is_empty() {
            self.out.push('{');
            for (i, (k, v)) in labels.iter().enumerate() {
                if i > 0 {
                    self.out.push(',');
                }
                self.out.push_str(k);
                self.out.push_str("=\"");
                for c in v.chars() {
                    match c {
                        '\\' => self.out.push_str("\\\\"),
                        '"' => self.out.push_str("\\\""),
                        '\n' => self.out.push_str("\\n"),
                        c => self.out.push(c),
                    }
                }
                self.out.push('"');
            }
            self.out.push('}');
        }
        self.out.push(' ');
        self.out.push_str(&fmt_value(value));
        self.out.push('\n');
    }

    fn finish(self) -> String {
        self.out
    }
}

fn fmt_value(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "+Inf" } else { "-Inf" }.to_string();
    }
    if v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    format!("{v}")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Every `key="value"` pair on every sample line of an exposition.
    pub(crate) fn label_pairs(exposition: &str) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        for line in exposition.lines().filter(|l| !l.starts_with('#')) {
            let Some(open) = line.find('{') else { continue };
            let close = line.rfind('}').expect("unbalanced labels");
            for pair in line[open + 1..close].split(',') {
                let (k, v) = pair.split_once('=').expect("label without =");
                pairs.push((k.to_string(), v.trim_matches('"').to_string()));
            }
        }
        pairs
    }

    /// True if `s` contains four dot-separated decimal groups, which is how an
    /// IPv4 address (a peer, a pod IP) would look if one ever leaked in.
    pub(crate) fn has_ipv4_shape(s: &str) -> bool {
        s.split(|c: char| !(c.is_ascii_digit() || c == '.'))
            .any(|tok| {
                let groups: Vec<_> = tok.split('.').collect();
                groups.len() >= 4
                    && groups
                        .windows(4)
                        .any(|w| w.iter().all(|g| !g.is_empty() && g.len() <= 3))
            })
    }

    /// The closed sets, as the scrape would read them. `le` is the histogram
    /// bound and `version`/`model` are build constants; everything else must
    /// be a lane or an outcome.
    pub(crate) fn assert_closed(exposition: &str) {
        let lanes: Vec<_> = LaneLabel::ALL.iter().map(|l| l.label()).collect();
        let outcomes: Vec<_> = Outcome::ALL.iter().map(|o| o.label()).collect();
        for (k, v) in label_pairs(exposition) {
            match k.as_str() {
                "lane" => assert!(lanes.contains(&v.as_str()), "lane={v}"),
                "outcome" => assert!(outcomes.contains(&v.as_str()), "outcome={v}"),
                "le" => assert!(v == "+Inf" || v.parse::<f64>().is_ok(), "le={v}"),
                "version" => assert_eq!(v, env!("CARGO_PKG_VERSION")),
                "model" => assert_eq!(v, crate::protocol::MODEL_CODE),
                other => panic!("unexpected label key {other}"),
            }
        }
        assert!(!has_ipv4_shape(exposition), "{exposition}");
    }

    #[test]
    fn the_ipv4_detector_detects() {
        assert!(has_ipv4_shape("peer=\"10.42.0.7\""));
        assert!(has_ipv4_shape("x 192.168.1.1:443"));
        assert!(!has_ipv4_shape("le=\"0.005\""));
        assert!(!has_ipv4_shape("version=\"0.0.14\""));
    }

    #[test]
    fn a_fresh_registry_renders_every_family_at_zero_with_closed_labels() {
        let m = Metrics::new();
        let out = m.render([0, 0]);
        for family in [
            "squelch_embed_build_info",
            "squelch_embed_session_ready 0",
            "squelch_embed_requests_total{lane=\"none\",outcome=\"unauthorized\"} 0",
            "squelch_embed_texts_total{lane=\"backfill\"} 0",
            "squelch_embed_queue_depth{lane=\"interactive\"} 0",
            "squelch_embed_queue_wait_seconds_count{lane=\"backfill\"} 0",
            "squelch_embed_inference_seconds_count 0",
            "squelch_embed_batch_size_bucket{le=\"8\"} 0",
        ] {
            assert!(out.contains(family), "missing {family}\n{out}");
        }
        assert_closed(&out);
    }

    #[test]
    fn observations_land_in_the_right_series() {
        let m = Metrics::new();
        m.record(Lane::Interactive, Outcome::Ok);
        m.record(None, Outcome::Unauthorized);
        m.accepted_texts(Lane::Backfill, 8);
        m.observe_inference(Duration::from_millis(30), 3);
        m.set_ready();
        let out = m.render([1, 16]);
        assert!(
            out.contains("squelch_embed_requests_total{lane=\"interactive\",outcome=\"ok\"} 1")
        );
        assert!(
            out.contains("squelch_embed_requests_total{lane=\"none\",outcome=\"unauthorized\"} 1")
        );
        assert!(out.contains("squelch_embed_texts_total{lane=\"backfill\"} 8"));
        assert!(out.contains("squelch_embed_queue_depth{lane=\"backfill\"} 16"));
        assert!(out.contains("squelch_embed_batch_size_bucket{le=\"2\"} 0"));
        assert!(out.contains("squelch_embed_batch_size_bucket{le=\"3\"} 1"));
        assert!(out.contains("squelch_embed_inference_seconds_bucket{le=\"0.025\"} 0"));
        assert!(out.contains("squelch_embed_inference_seconds_bucket{le=\"0.05\"} 1"));
        assert!(out.contains("squelch_embed_session_ready 1"));
        assert_closed(&out);
    }
}
