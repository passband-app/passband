//! The shared per-node embedding service, and the wire contract its callers use.
//!
//! Design record: `docs/EMBED-SERVICE.md`. [`protocol`] is what a hosted
//! `squelchd` and the service agree on; it builds with no features. Everything
//! else is the service itself, behind the `server` feature.
//!
//! What the service holds, in one sentence: an ONNX session, two in-memory
//! queues of texts waiting for it, and counters. It opens no store, writes no
//! file, keeps no text past the forward pass that reads it, and is never told
//! which tenant a text came from. The modules below are each a piece of that
//! sentence, and §4 of the design record is the list of claims they are tested
//! against: no text in a log line or a metric, no cache, no dedup, no peer
//! address anywhere.

pub mod protocol;

#[cfg(feature = "server")]
pub mod coalescer;
#[cfg(feature = "server")]
pub mod http;
#[cfg(feature = "server")]
pub mod log;
#[cfg(feature = "server")]
pub mod metrics;
#[cfg(feature = "server")]
pub mod session;
