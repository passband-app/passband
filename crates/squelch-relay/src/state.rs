//! Shared handler state.

use std::fmt;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{Semaphore, SemaphorePermit};
use tokio::time::MissedTickBehavior;

use crate::clock::now_unix;
use crate::config::Config;
use crate::jwt::JwtSigner;
use crate::opens::{OpenStore, PURGE_EVERY, TRIM_EVERY};
use crate::ratelimit::{PIXEL_REQUESTS_PER_MINUTE, REQUESTS_PER_MINUTE, RateLimiter};

/// State threaded through the router, the auth layer, and the rate limiter.
/// Cheap to clone (one `Arc`): the config, HTTP client, token cache, and limiter
/// are process-wide singletons, and cloning must never fork the cache or the
/// buckets.
#[derive(Clone)]
pub struct RelayState {
    inner: Arc<Inner>,
}

/// Process-wide ceiling on concurrent APNs requests. `FANOUT` bounds one batch
/// only; without this, C concurrent pushes open C*FANOUT sockets.
const MAX_INFLIGHT_APNS: usize = 64;

/// Process-wide ceiling on open-buffer calls in flight. Each one is a blocking
/// thread parked on the buffer's single mutex, so without a bound a burst of
/// pixel fetches across many addresses (the pixel limiter is per address)
/// would fill tokio's blocking pool and queue the rest behind it, and that
/// pool is also where reqwest resolves the APNs hostname. Past the ceiling a
/// request waits for a permit asynchronously, which costs a task, not a
/// thread, and the pixel is served when its turn comes. The permit travels
/// with the blocking work, not with the awaiting handler: a client that
/// disconnects mid-fetch drops its handler, and the work it started is still
/// a thread on the mutex until it finishes.
const MAX_INFLIGHT_OPENS: usize = 16;

/// Why a buffer call did not run to completion.
#[derive(Debug)]
pub(crate) enum OpensError {
    Sqlite(rusqlite::Error),
    /// The blocking task panicked. The buffer's mutex recovers from poisoning,
    /// so the next call is unaffected.
    Panicked,
    /// The runtime is shutting down and the task never ran.
    Cancelled,
}

impl fmt::Display for OpensError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(e) => e.fmt(f),
            Self::Panicked => f.write_str("the buffer task panicked"),
            Self::Cancelled => f.write_str("the buffer task was cancelled at shutdown"),
        }
    }
}

struct Inner {
    config: Config,
    http: reqwest::Client,
    signer: JwtSigner,
    limiter: Mutex<RateLimiter>,
    /// The pixel route gets its OWN buckets. Both routes see the proxy's address
    /// and so share one bucket per limiter; on a single limiter, one popular
    /// newsletter's opens would spend the daemon's push budget.
    pixel_limiter: Mutex<RateLimiter>,
    apns_inflight: Semaphore,
    opens_inflight: Arc<Semaphore>,
    opens: OpenStore,
}

impl RelayState {
    /// Build state from validated config. Fails if the `.p8` is not a usable
    /// ES256 key, so the relay refuses to start rather than discovering it on
    /// the first push.
    pub fn new(config: Config) -> anyhow::Result<Self> {
        let signer = JwtSigner::new(
            &config.apns_key_pem,
            &config.apns_key_id,
            &config.apns_team_id,
        )?;
        // APNs expects providers to hold a long-lived HTTP/2 connection rather
        // than reconnecting per push; ALPN negotiates it, so a plain-HTTP test
        // override still works over 1.1.
        let http = reqwest::Client::builder()
            .pool_idle_timeout(Duration::from_secs(600))
            .timeout(Duration::from_secs(10))
            .build()?;
        let opens = OpenStore::open(config.db_path.as_deref())?;
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                http,
                signer,
                limiter: Mutex::new(RateLimiter::per_minute(REQUESTS_PER_MINUTE)),
                pixel_limiter: Mutex::new(RateLimiter::per_minute(PIXEL_REQUESTS_PER_MINUTE)),
                apns_inflight: Semaphore::new(MAX_INFLIGHT_APNS),
                opens_inflight: Arc::new(Semaphore::new(MAX_INFLIGHT_OPENS)),
                opens,
            }),
        })
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }

    pub(crate) fn signer(&self) -> &JwtSigner {
        &self.inner.signer
    }

    /// The configured bearer token, or `None` when the push route is open.
    pub fn auth_token(&self) -> Option<&str> {
        self.inner.config.auth_token.as_deref()
    }

    /// The push-only bearer, accepted by `POST /v1/push` and never by
    /// `GET /v1/opens`. `None` when unset, which is the relay as it always was.
    pub fn push_token(&self) -> Option<&str> {
        self.inner.config.push_token.as_deref()
    }

    /// Charge one push against `ip`'s bucket. False means "over the limit".
    /// Poisoning is RECOVERED, not propagated: the guarded value is a token
    /// bucket with no invariant a panic could corrupt, and `.expect()` would
    /// brick every future push while `/healthz` kept answering 200.
    pub(crate) fn check_rate(&self, ip: IpAddr) -> bool {
        self.inner
            .limiter
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .check(ip, Instant::now())
    }

    /// Charge one pixel fetch against `ip`'s separate bucket.
    pub(crate) fn check_pixel_rate(&self, ip: IpAddr) -> bool {
        self.inner
            .pixel_limiter
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .check(ip, Instant::now())
    }

    /// Run `f` against the open buffer on a blocking thread, under the
    /// in-flight ceiling, so no runtime worker ever parks on the buffer's
    /// mutex or its disk: `/v1/push` shares those workers, and a burst of
    /// opens from one campaign must not stall the APNs path behind the
    /// volume.
    pub(crate) async fn with_opens<T, F>(&self, f: F) -> Result<T, OpensError>
    where
        T: Send + 'static,
        F: FnOnce(&OpenStore) -> rusqlite::Result<T> + Send + 'static,
    {
        // The semaphore is never closed; a closed one can only mean teardown.
        let permit = self
            .inner
            .opens_inflight
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| OpensError::Cancelled)?;
        let inner = self.inner.clone();
        let task = tokio::task::spawn_blocking(move || {
            // Released when the work is done, whatever became of the caller.
            let _permit = permit;
            f(&inner.opens)
        });
        match task.await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(OpensError::Sqlite(e)),
            Err(e) if e.is_panic() => Err(OpensError::Panicked),
            Err(_) => Err(OpensError::Cancelled),
        }
    }

    /// The open buffer's sweeps on a clock, off every request path: the
    /// ceiling trim every [`TRIM_EVERY`] and the retention purge every
    /// [`PURGE_EVERY`], through [`Self::with_opens`] like any other buffer
    /// call. Spawned by [`crate::router`], so a relay that serves the pixel
    /// always sweeps the buffer it fills. Runs until the runtime drops, which
    /// waits for a sweep already on its blocking thread, so exit can take one
    /// trim.
    pub(crate) async fn run_maintenance(self) {
        let mut tick = tokio::time::interval(TRIM_EVERY);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // The first tick completes at once, and a buffer that young has
        // nothing to sweep.
        tick.tick().await;
        let mut ticks: u64 = 0;
        loop {
            tick.tick().await;
            ticks += 1;
            self.sweep(ticks).await;
        }
    }

    /// One tick of the maintenance clock, the `tick`th since boot: the trim
    /// every time, the purge every [`PURGE_EVERY`] worth of ticks. Counting
    /// ticks rather than reading a clock inside the store keeps the two
    /// cadences from aliasing: a gate of "a minute since the last" next to a
    /// tick of a minute skips about every other tick to jitter.
    async fn sweep(&self, tick: u64) {
        let ticks_per_purge = (PURGE_EVERY.as_secs() / TRIM_EVERY.as_secs()).max(1);
        let purge = tick.is_multiple_of(ticks_per_purge);
        let now = now_unix();
        let swept = self
            .with_opens(move |opens| {
                if purge {
                    opens.purge(now);
                }
                opens.trim();
                Ok(())
            })
            .await;
        match swept {
            Ok(()) | Err(OpensError::Cancelled) => {}
            Err(e) => tracing::warn!(error = %e, "open buffer maintenance did not run"),
        }
    }

    /// Wait for a slot in the process-wide APNs concurrency budget; the permit
    /// releases it when dropped.
    pub(crate) async fn apns_slot(&self) -> Option<SemaphorePermit<'_>> {
        // The semaphore is never closed; proceeding without a permit beats
        // dropping a push if that ever changes.
        self.inner.apns_inflight.acquire().await.ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Environment;
    use crate::opens::RETENTION_SECS;

    fn state() -> RelayState {
        RelayState::new(Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            apns_key_pem: include_str!("../tests/fixture_test_key.p8").to_string(),
            apns_key_id: "KEYID12345".into(),
            apns_team_id: "TEAMID6789".into(),
            apns_topics: vec!["com.example.app".into()],
            apns_env: Environment::Production,
            auth_token: None,
            push_token: None,
            apns_url_override: None,
            db_path: None,
            trusted_proxy_hops: 0,
            trusted_proxy_cidrs: None,
        })
        .unwrap()
    }

    /// The sweep purges on its tick count: a row past retention outlives nine
    /// ticks and not the tenth, and every tick trims (under the ceiling, to no
    /// effect). Through `with_opens`, so the permit path runs too.
    #[tokio::test]
    async fn the_sweep_purges_on_its_tick_count() {
        let state = state();
        let stale = now_unix() - RETENTION_SECS - 1;
        state.inner.opens.record("stale", stale, None).unwrap();
        state.inner.opens.record("fresh", now_unix(), None).unwrap();

        let ticks_per_purge = PURGE_EVERY.as_secs() / TRIM_EVERY.as_secs();
        for tick in 1..ticks_per_purge {
            state.sweep(tick).await;
            assert_eq!(
                state.inner.opens.drain(0).unwrap().len(),
                2,
                "tick {tick}: the trim alone, and under the ceiling it evicts nothing"
            );
        }
        state.sweep(ticks_per_purge).await;
        let left = state.inner.opens.drain(0).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].token, "fresh");
    }
}
