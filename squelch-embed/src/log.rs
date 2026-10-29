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

/// Replace the process's panic hook with one that writes a fixed word and
/// the panic's source location to `log`, and NEVER the payload.
///
/// The default hook prints the panic message, and a panic message is
/// whatever the panicking code formatted: inside a tokenizer or ORT that can
/// be the text being embedded, which is another tenant's mail. The forward
/// pass runs under `catch_unwind` (`coalescer.rs`), but the hook runs before
/// the unwind is caught, so catching it does not keep the message out of the
/// log. The location (a source file and line of this build) is all an
/// operator needs to find it.
pub fn install_panic_hook(log: Log) {
    std::panic::set_hook(Box::new(move |info| log.line(&panic_line(info))));
}

fn panic_line(info: &std::panic::PanicHookInfo<'_>) -> String {
    let thread = std::thread::current();
    // Thread names here are ours ("squelch-embed-inference", tokio's).
    let thread = thread.name().unwrap_or("unnamed");
    match info.location() {
        Some(at) => format!(
            "panic in thread {thread} at {}:{}:{} (message withheld)",
            at.file(),
            at.line(),
            at.column()
        ),
        None => format!("panic in thread {thread} (message withheld)"),
    }
}

impl std::fmt::Debug for Log {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Log")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A panic whose message is mail text leaves only the word and the place.
    #[test]
    fn the_panic_hook_withholds_the_payload() {
        const CANARY: &str = "canary-subject-Px4 your code is 482913";
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = lines.clone();
        let previous = std::panic::take_hook();
        install_panic_hook(Log::new(move |l| sink.lock().unwrap().push(l.to_string())));
        let result = std::thread::Builder::new()
            .name("panic-hook-test".into())
            .spawn(|| panic!("tokenizer choked on {CANARY}"))
            .unwrap()
            .join();
        std::panic::set_hook(previous);
        assert!(result.is_err());

        let lines = lines.lock().unwrap();
        let ours: Vec<&String> = lines
            .iter()
            .filter(|l| l.contains("panic-hook-test"))
            .collect();
        assert_eq!(ours.len(), 1, "{lines:?}");
        assert!(ours[0].contains("log.rs:"), "{}", ours[0]);
        assert!(ours[0].contains("message withheld"), "{}", ours[0]);
        for line in lines.iter() {
            assert!(!line.contains("canary"), "{line}");
            assert!(!line.contains("482913"), "{line}");
        }
    }
}
