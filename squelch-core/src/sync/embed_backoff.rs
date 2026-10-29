//! Backoff for the sync engine's calls to a REMOTE embedder.
//!
//! WHY IT EXISTS. A local embedder that built does not fail transiently, so
//! "a failed backfill batch stops the pass and the next tick retries" was the
//! whole policy, and `sync.poll_secs` is 5. Against the shared embedding service
//! (docs/EMBED-SERVICE.md §3) that policy is a fleet answering "I am full" with
//! twelve more requests a minute per tenant. So a transient failure (429, 503,
//! other 5xx, timeout, connect: see [`crate::embed::RemoteEmbedError`]) opens a
//! window during which this engine does not call the embedder at all: 5 s,
//! doubling to 5 minutes, reset on the first success, with the service's
//! `Retry-After` as a floor.
//!
//! WHAT IT DOES NOT SLOW DOWN: mail. Inside a window, ingest stores the message
//! and skips its vector exactly as it does with no embedder attached, raising
//! `vectors_dirty` so `backfill_missing_vectors` embeds it after the window. The
//! cost of an outage to ingest is the one call per window that discovers it is
//! still going, bounded by the client's own timeouts (connect 2 s, one text 5
//! s); a refused connection, the shape of a service that is simply gone, costs
//! nothing measurable.
//!
//! A PERMANENT remote failure (400, 401, a response in the wrong vector space)
//! opens a window too, at once at [`EMBED_BACKOFF_CAP`]: nothing about the
//! next request differs from the one refused, so a wrong token or a service
//! rebuilt on another model would otherwise be asked twelve times a minute,
//! for ever, by every tenant. A success closes it like any other window.
//! Local embedder errors never open a window: they are not a load problem,
//! and the existing stop-the-pass behaviour is unchanged for them.

use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

use crate::embed::{MAX_RETRY_AFTER, remote_failure};
use crate::error::CoreError;

/// The first window.
pub(crate) const EMBED_BACKOFF_FLOOR: Duration = Duration::from_secs(5);

/// The longest window. Five minutes: long enough that a fleet of tenants is
/// not what keeps a full service full, short enough that a recovered service
/// is back in use before anyone goes looking at search quality.
pub(crate) const EMBED_BACKOFF_CAP: Duration = Duration::from_secs(5 * 60);

// The Retry-After floor is capped where it is parsed; the two caps are one.
const _: () = assert!(MAX_RETRY_AFTER.as_secs() == EMBED_BACKOFF_CAP.as_secs());

/// The window after `failures` consecutive transient failures (1 = the first),
/// never shorter than the service's `retry_after`. Pure, so the schedule is
/// testable without a clock.
pub(crate) fn embed_backoff_delay(failures: u32, retry_after: Option<Duration>) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    let exp = EMBED_BACKOFF_FLOOR
        .saturating_mul(1u32 << doublings)
        .min(EMBED_BACKOFF_CAP);
    exp.max(retry_after.unwrap_or_default().min(EMBED_BACKOFF_CAP))
}

#[derive(Debug, Default)]
struct State {
    failures: u32,
    until: Option<Instant>,
}

/// One engine's backoff state. Per engine rather than shared like
/// `vectors_dirty`: only the sync engine embeds (ingest, translations, the
/// vector drain); the daemon's sweeps engine deletes vectors and never asks
/// for one.
#[derive(Debug, Default)]
pub(crate) struct EmbedBackoff {
    state: Mutex<State>,
}

impl EmbedBackoff {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // Plain data, no invariant a panic could break halfway.
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Whether the embedder may be called at `now`: no window is open.
    pub(crate) fn may_call(&self, now: Instant) -> bool {
        self.lock().until.is_none_or(|until| now >= until)
    }

    /// A call worked: close any window and forget the run of failures.
    pub(crate) fn succeeded(&self) {
        *self.lock() = State::default();
    }

    /// A call failed with `err`. If that is a remote failure, open a window
    /// and return its length, for the caller's one log line: the next rung of
    /// the ladder for a transient one, [`EMBED_BACKOFF_CAP`] for a permanent
    /// one. Anything else returns `None` and changes nothing.
    pub(crate) fn failed(&self, err: &CoreError, now: Instant) -> Option<Duration> {
        let remote = remote_failure(err)?;
        let mut state = self.lock();
        let delay = if remote.transient {
            state.failures = state.failures.saturating_add(1);
            embed_backoff_delay(state.failures, remote.retry_after)
        } else {
            EMBED_BACKOFF_CAP
        };
        state.until = Some(now + delay);
        Some(delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schedule_doubles_from_five_seconds_to_five_minutes() {
        let secs = |n| embed_backoff_delay(n, None).as_secs();
        assert_eq!(
            (1..=9).map(secs).collect::<Vec<_>>(),
            vec![5, 10, 20, 40, 80, 160, 300, 300, 300]
        );
        // Far past the cap, nothing overflows.
        assert_eq!(secs(u32::MAX), 300);
        // 0 is not a failure count anyone passes, but it is the floor too.
        assert_eq!(secs(0), 5);
    }

    #[test]
    fn retry_after_is_a_floor_not_a_ceiling() {
        let d = |n, s| embed_backoff_delay(n, Some(Duration::from_secs(s))).as_secs();
        assert_eq!(d(1, 30), 30, "the service asked for longer than the ladder");
        assert_eq!(d(5, 30), 80, "the ladder is already longer");
        assert_eq!(d(1, 86_400), 300, "and a floor is still capped");
    }

    /// A wrong token or a service in another vector space is not asked
    /// again on the next tick: the window opens at the cap straight away.
    #[test]
    fn a_permanent_remote_failure_opens_the_longest_window_at_once() {
        let b = EmbedBackoff::default();
        let t0 = Instant::now();
        let refused = CoreError::from(crate::embed::RemoteEmbedError::permanent("HTTP 401"));
        assert_eq!(b.failed(&refused, t0), Some(EMBED_BACKOFF_CAP));
        assert!(!b.may_call(t0 + Duration::from_secs(299)));
        assert!(b.may_call(t0 + EMBED_BACKOFF_CAP));

        // Still the cap the second time, and a success closes it.
        assert_eq!(b.failed(&refused, t0), Some(EMBED_BACKOFF_CAP));
        b.succeeded();
        assert!(b.may_call(t0));
    }

    #[test]
    fn only_a_remote_failure_opens_a_window() {
        let b = EmbedBackoff::default();
        let t0 = Instant::now();
        assert!(b.may_call(t0));

        // A local failure, and an error that is not an embed error at all.
        let local = CoreError::Other(anyhow::anyhow!("fastembed embed: boom"));
        assert_eq!(b.failed(&local, t0), None);
        assert_eq!(b.failed(&CoreError::NotFound, t0), None);
        assert!(b.may_call(t0));
    }

    #[tokio::test]
    async fn a_window_opens_grows_and_closes_on_success() {
        // A real RemoteEmbedError, from a real refused connection.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let err = tokio::task::spawn_blocking(move || {
            use crate::embed::Embedder;
            let settings = crate::config::EmbedConfig::default().settings();
            crate::embed::RemoteEmbedder::new(&format!("http://127.0.0.1:{port}"), None, &settings)
                .unwrap()
                .embed("hi")
                .unwrap_err()
        })
        .await
        .unwrap();

        let b = EmbedBackoff::default();
        let t0 = Instant::now();
        assert_eq!(b.failed(&err, t0), Some(Duration::from_secs(5)));
        assert!(!b.may_call(t0));
        assert!(!b.may_call(t0 + Duration::from_secs(4)));
        assert!(b.may_call(t0 + Duration::from_secs(5)));

        let t1 = t0 + Duration::from_secs(5);
        assert_eq!(b.failed(&err, t1), Some(Duration::from_secs(10)));
        assert!(!b.may_call(t1 + Duration::from_secs(9)));

        b.succeeded();
        assert!(b.may_call(t1));
        assert_eq!(
            b.failed(&err, t1),
            Some(Duration::from_secs(5)),
            "a success resets the ladder"
        );
    }
}
