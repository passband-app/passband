//! The shared per-node embedding service, and the wire contract its callers use.
//!
//! Design record: `docs/EMBED-SERVICE.md`. [`protocol`] is what a hosted
//! `squelchd` and the service agree on; it builds with no features. Everything
//! else is the service itself, behind the `server` feature.

pub mod protocol;
