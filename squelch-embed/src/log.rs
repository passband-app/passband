//! Where the service's log lines go.
//!
//! A handle rather than bare `eprintln!` so the tests can capture every line
//! the service writes and assert that none of them carries request text. The
//! discipline is the relay's: counts, byte lengths, statuses and durations,
//! never a text, never anything computed from one, never a peer address.
//! Every call site formats from those alone, and the tests are what keep it
//! that way.

use std::sync::Arc;

/// A cheap, clonable sink for one line at a time.
#[derive(Clone)]
pub struct Log(Arc<dyn Fn(&str) + Send + Sync>);

impl Log {
    /// The production sink: stderr, one prefixed line per call.
    pub fn stderr() -> Self {
        Self(Arc::new(|line| eprintln!("squelch-embed: {line}")))
    }

    /// Any other sink. The tests collect into a `Vec`.
    pub fn new(sink: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self(Arc::new(sink))
    }

    pub fn line(&self, line: &str) {
        (self.0)(line)
    }
}

impl std::fmt::Debug for Log {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Log")
    }
}
