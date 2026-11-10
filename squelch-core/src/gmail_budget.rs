//! ONE Gmail quota budget per mailbox, shared by every caller in the process.
//!
//! Gmail meters each user of a Cloud project separately: 6,000 units a minute
//! under the May 2026 model, 15,000 under the legacy one. The per-user limit is
//! the one a single mailbox can trip, and it is the one the Cloud console does
//! not chart, so the only defence is never asking faster than it allows.
//!
//! The pacer used to live inside `SyncEngine`, and squelchd builds more than
//! one engine for the same mailbox (the sync loop, and the one-time sweeps
//! engine that harvests Sent contacts and heals blank bodies), while the write
//! door (archive, label, send) was not paced at all. Each engine held its own
//! 80% budget, so two of them could ask for 160% of the user's limit, and a
//! tenant sat in `RATE_LIMIT_EXCEEDED` with the project at under 1% of its own.
//! Owning the budget here, one `Arc` handed to every caller, is what makes the
//! 80% a property of the mailbox rather than of whichever caller you look at.
//!
//! It also adapts: see [`GmailBudget`] for why the published costs are only
//! the starting point.
//!
//! What it cannot see: another PROCESS on the same Google user and project,
//! such as a self-hosted squelchd syncing the mailbox a hosted tenant also
//! syncs. Those two share Google's budget and not this one.

use std::sync::Mutex as StdMutex;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::config::GmailQuotaProfile;

/// How long the whole mailbox stops asking after Google refuses for quota.
/// The per-user limit is a rolling minute, so a full minute lets whatever
/// filled it age out before anything is asked again.
const QUOTA_PAUSE: Duration = Duration::from_secs(60);

/// How long the mailbox must go without a refusal before it speeds up again,
/// and then only by one step.
const RECOVER_AFTER: Duration = Duration::from_secs(60);

/// The slowest the budget will go: 1/16 of the published rate.
const MAX_SLOWDOWN: u32 = 16;

/// Paces Gmail requests for one mailbox, charging each request by what Gmail
/// bills for it, and lets Google's own refusals set the pace.
///
/// It starts at 80% of the published per-user budget. The published costs are
/// not the whole story: Google can refuse a mailbox well below the published
/// limit with no other caller on the project. So a quota refusal
/// halves the rate and pauses the whole mailbox for a minute (sync, sweeps and
/// the write door together), and each minute without one speeds back up by a
/// tenth, never past the starting rate. The mailbox settles just under what
/// Google actually enforces for that user, whatever that is.
pub struct GmailBudget {
    profile: GmailQuotaProfile,
    /// The starting unit, 60 s over 80% of the published per-minute budget.
    base: Duration,
    /// The slowest unit the budget will back off to.
    max: Duration,
    pause: Duration,
    recover_after: Duration,
    /// Taking turns. Held across the wait, so callers are admitted one at a
    /// time and in order.
    gate: Mutex<()>,
    /// The pace, behind a short-lived lock so a refusal can move it while
    /// callers are queued on `gate`.
    state: StdMutex<Pace>,
}

struct Pace {
    /// When the next request may be sent.
    next: Instant,
    /// The time one unit currently occupies.
    unit: Duration,
    /// Since when the mailbox has gone unrefused; recovery counts from here.
    calm_since: Instant,
    /// The end of the current refusal hold. Refusals that land inside it are
    /// the same event (other requests already in flight when Google tripped)
    /// and extend the hold without halving the rate again.
    held_until: Instant,
}

impl GmailBudget {
    pub fn new(profile: GmailQuotaProfile) -> Self {
        // 60 s / (0.8 * 6,000) = 12.5 ms; 60 s / (0.8 * 15,000) = 5 ms.
        let unit = match profile {
            GmailQuotaProfile::Standard => Duration::from_micros(12_500),
            GmailQuotaProfile::Legacy => Duration::from_millis(5),
        };
        Self::with_unit(profile, unit)
    }

    /// An explicit starting unit. Tests use zero (unpaced) or a few ms.
    pub fn with_unit(profile: GmailQuotaProfile, unit: Duration) -> Self {
        let now = Instant::now();
        Self {
            profile,
            base: unit,
            max: unit * MAX_SLOWDOWN,
            pause: QUOTA_PAUSE,
            recover_after: RECOVER_AFTER,
            gate: Mutex::new(()),
            state: StdMutex::new(Pace {
                next: now,
                unit,
                calm_since: now,
                held_until: now,
            }),
        }
    }

    /// Override the refusal pause and the recovery interval. Tests only need
    /// this; production uses a minute for both.
    pub fn with_adaptation(mut self, pause: Duration, recover_after: Duration) -> Self {
        self.pause = pause;
        self.recover_after = recover_after;
        self
    }

    fn pace(&self) -> std::sync::MutexGuard<'_, Pace> {
        // A poisoned pace is still a pace: the fields are plain values.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Wait for a slot, then book the request's cost against the budget.
    ///
    /// Serialized across every caller holding this budget, 401 retries
    /// included. The next slot is set from NOW rather than from the previous
    /// slot, so an idle mailbox never banks a burst. A refusal that lands
    /// while a caller is waiting pushes that caller back too.
    pub async fn admit(&self, method: &str, path: &str) {
        let _turn = self.gate.lock().await;
        loop {
            let at = self.pace().next;
            if Instant::now() >= at {
                break;
            }
            tokio::time::sleep_until(at).await;
        }
        let cost = self.cost(method, path);
        let mut pace = self.pace();
        pace.next = Instant::now() + pace.unit * cost;
    }

    /// Google refused a request for quota. Halve the rate (down to 1/16 of
    /// the start) and hold every caller for the pause, or for Google's own
    /// Retry-After when that is longer. Returns the pause and the new unit,
    /// for the log line.
    pub fn note_quota_refusal(&self, retry_after: Option<Duration>) -> (Duration, Duration) {
        let hold = self.pause.max(retry_after.unwrap_or_default());
        let now = Instant::now();
        let until = now + hold;
        let mut pace = self.pace();
        // One trip, one halving: a refusal inside a hold is another request
        // that was already in flight, not a second signal.
        if now >= pace.held_until {
            pace.unit = (pace.unit * 2).min(self.max);
        }
        pace.held_until = pace.held_until.max(until);
        pace.next = pace.next.max(until);
        pace.calm_since = pace.held_until;
        (hold, pace.unit)
    }

    /// [`Self::admit`], but give up after `max_wait` and say how long the wait
    /// would have been. For the write door: a human's send must not sit for a
    /// minute behind a refusal hold while the app's request times out, so it
    /// is told to try again instead. A cancelled wait books nothing.
    pub async fn admit_within(
        &self,
        method: &str,
        path: &str,
        max_wait: Duration,
    ) -> Result<(), Duration> {
        // A hold that plainly outlasts the bound answers now, not after it.
        let known_wait = self.pace().next.saturating_duration_since(Instant::now());
        if known_wait > max_wait {
            return Err(known_wait);
        }
        match tokio::time::timeout(max_wait, self.admit(method, path)).await {
            Ok(()) => Ok(()),
            Err(_) => Err(self
                .pace()
                .next
                .saturating_duration_since(Instant::now())
                .max(Duration::from_secs(1))),
        }
    }

    /// Google answered. After a full recovery interval with no refusal, speed
    /// up by a tenth, never past the starting rate.
    pub fn note_success(&self) {
        let now = Instant::now();
        let mut pace = self.pace();
        if pace.unit > self.base
            && now.saturating_duration_since(pace.calm_since) >= self.recover_after
        {
            pace.unit = pace.unit.mul_f64(0.9).max(self.base);
            pace.calm_since = now;
        }
    }

    /// Units Gmail charges for one request, by method and API path (the part
    /// after `/gmail/v1/users/me`, query string removed).
    pub fn cost(&self, method: &str, path: &str) -> u32 {
        gmail_method_cost(self.profile, method, path)
    }

    /// A unit as a percentage of the starting rate, for log lines.
    pub fn rate_percent(&self, unit: Duration) -> f64 {
        if unit.is_zero() {
            return 100.0;
        }
        self.base.as_secs_f64() / unit.as_secs_f64() * 100.0
    }

    /// The current pacing unit, for tests, logs and diagnostics.
    pub fn unit(&self) -> Duration {
        self.pace().unit
    }
}

/// Whether a non-2xx Gmail response is a QUOTA refusal. 429 always is. 403 is
/// ambiguous: Google returns it both for "asking too fast" and for "this
/// credential may not do that", told apart only by the reason in the body.
/// Bounded and case-folded; the body is only matched, never logged.
pub fn is_quota_refusal(status: u16, body: &str) -> bool {
    const QUOTA_REASONS: [&str; 6] = [
        "rate_limit_exceeded",
        "quota_exceeded",
        "ratelimitexceeded",
        "userratelimitexceeded",
        "quotaexceeded",
        "dailylimitexceeded",
    ];
    match status {
        429 => true,
        403 => {
            let head: String = body.chars().take(2048).collect::<String>().to_lowercase();
            QUOTA_REASONS.iter().any(|r| head.contains(r))
        }
        _ => false,
    }
}

/// The path of a Gmail API URL relative to `base`, without its query string.
pub fn gmail_path<'a>(url: &'a str, base: &str) -> &'a str {
    url.strip_prefix(base)
        .unwrap_or(url)
        .split('?')
        .next()
        .unwrap_or_default()
}

/// Gmail's published unit costs. Reads of a single message are the one price
/// the May 2026 model raised (5 -> 20); everything else kept its old cost.
fn gmail_method_cost(profile: GmailQuotaProfile, method: &str, path: &str) -> u32 {
    let read = match profile {
        GmailQuotaProfile::Standard => 20,
        GmailQuotaProfile::Legacy => 5,
    };
    match (method, path) {
        ("POST", "/messages/send") => 100,
        ("POST", p) if p.ends_with("/trash") => 20,
        // messages.modify, the write door's other verb.
        ("POST", _) => 5,
        (_, "/profile") => 1,
        (_, "/history") => 2,
        (_, "/messages") => 5,
        (_, p) if p.starts_with("/labels/") => 1,
        _ => read,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn costs_follow_the_profile_and_the_method() {
        for (profile, read) in [
            (GmailQuotaProfile::Standard, 20),
            (GmailQuotaProfile::Legacy, 5),
        ] {
            let b = GmailBudget::new(profile);
            assert_eq!(b.cost("GET", "/messages/a"), read);
            assert_eq!(b.cost("GET", "/messages"), 5);
            assert_eq!(b.cost("GET", "/history"), 2);
            assert_eq!(b.cost("GET", "/labels/INBOX"), 1);
            assert_eq!(b.cost("GET", "/profile"), 1);
            assert_eq!(b.cost("POST", "/messages/send"), 100);
            assert_eq!(b.cost("POST", "/messages/a/modify"), 5);
            assert_eq!(b.cost("POST", "/messages/a/trash"), 20);
        }
    }

    /// The whole point: the budget's steady state stays under 80% of the
    /// per-user limit for the most expensive read, on both profiles.
    #[test]
    fn a_minute_of_reads_stays_under_eighty_percent_of_the_user_limit() {
        for (profile, limit) in [
            (GmailQuotaProfile::Standard, 6_000u32),
            (GmailQuotaProfile::Legacy, 15_000u32),
        ] {
            let b = GmailBudget::new(profile);
            let per_read = b.unit() * b.cost("GET", "/messages/a");
            let reads_per_minute = Duration::from_secs(60).as_micros() / per_read.as_micros();
            let units = reads_per_minute as u32 * b.cost("GET", "/messages/a");
            assert!(
                units <= limit * 8 / 10,
                "{units} units/min over 80% of {limit}"
            );
        }
    }

    #[test]
    fn path_strips_base_and_query() {
        let base = "https://gmail.googleapis.com/gmail/v1/users/me";
        assert_eq!(
            gmail_path(&format!("{base}/messages/a?format=raw"), base),
            "/messages/a"
        );
        assert_eq!(
            gmail_path(&format!("{base}/history?startHistoryId=1"), base),
            "/history"
        );
    }

    #[tokio::test]
    async fn a_refusal_halves_the_rate_and_holds_every_caller() {
        let b = GmailBudget::with_unit(GmailQuotaProfile::Standard, Duration::from_millis(1))
            .with_adaptation(Duration::from_millis(80), Duration::from_secs(3600));
        b.admit("GET", "/profile").await;
        let (hold, unit) = b.note_quota_refusal(None);
        assert_eq!(hold, Duration::from_millis(80));
        assert_eq!(unit, Duration::from_millis(2));
        let started = Instant::now();
        b.admit("GET", "/profile").await;
        assert!(
            started.elapsed() >= Duration::from_millis(70),
            "the pause must hold the next caller"
        );
    }

    #[tokio::test]
    async fn a_longer_retry_after_wins_over_the_pause() {
        let b = GmailBudget::with_unit(GmailQuotaProfile::Standard, Duration::ZERO)
            .with_adaptation(Duration::from_millis(10), Duration::from_secs(3600));
        let (hold, _) = b.note_quota_refusal(Some(Duration::from_millis(90)));
        assert_eq!(hold, Duration::from_millis(90));
    }

    /// The refusal lands while another caller is already waiting for its turn:
    /// that caller must honour the pause too, not fire on its old slot.
    #[tokio::test]
    async fn a_refusal_reaches_a_caller_already_waiting() {
        let b = std::sync::Arc::new(
            GmailBudget::with_unit(GmailQuotaProfile::Standard, Duration::from_millis(1))
                .with_adaptation(Duration::from_millis(120), Duration::from_secs(3600)),
        );
        b.admit("GET", "/messages/a").await; // books 20 ms
        let waiter = {
            let b = b.clone();
            tokio::spawn(async move {
                let started = Instant::now();
                b.admit("GET", "/profile").await;
                started.elapsed()
            })
        };
        tokio::time::sleep(Duration::from_millis(5)).await;
        b.note_quota_refusal(None);
        let waited = waiter.await.unwrap();
        assert!(
            waited >= Duration::from_millis(100),
            "waited only {waited:?}"
        );
    }

    /// Two requests were in flight when Google tripped: both come back
    /// refused, but that is one event and must cost one halving, not two.
    #[test]
    fn refusals_inside_one_hold_halve_the_rate_once() {
        let b = GmailBudget::with_unit(GmailQuotaProfile::Standard, Duration::from_millis(1))
            .with_adaptation(Duration::from_secs(60), Duration::from_secs(3600));
        b.note_quota_refusal(None);
        b.note_quota_refusal(None);
        b.note_quota_refusal(None);
        assert_eq!(b.unit(), Duration::from_millis(2));
    }

    /// Recovery counts from the END of the hold. Without that, the first
    /// success after a hold on a long-lived budget would speed straight back
    /// up, because the old calm window is already long past.
    #[tokio::test]
    async fn recovery_counts_from_the_end_of_the_hold() {
        let b = GmailBudget::with_unit(GmailQuotaProfile::Standard, Duration::from_millis(10))
            .with_adaptation(Duration::from_millis(200), Duration::from_millis(30));
        tokio::time::sleep(Duration::from_millis(60)).await;
        b.note_quota_refusal(None);
        b.note_success();
        assert_eq!(
            b.unit(),
            Duration::from_millis(20),
            "no speed-up during the hold"
        );
    }

    #[tokio::test]
    async fn a_bounded_wait_gives_up_and_says_how_long() {
        let b = GmailBudget::with_unit(GmailQuotaProfile::Standard, Duration::ZERO)
            .with_adaptation(Duration::from_secs(60), Duration::from_secs(3600));
        b.note_quota_refusal(None);
        let started = Instant::now();
        let wait = b
            .admit_within("POST", "/messages/send", Duration::from_millis(50))
            .await
            .expect_err("a 60 s hold outlasts a 50 ms bound");
        assert!(
            started.elapsed() < Duration::from_millis(20),
            "a known long hold answers at once"
        );
        assert!(wait >= Duration::from_secs(55), "reported {wait:?}");
        // Nothing was booked by the abandoned wait.
        assert!(
            b.admit_within("GET", "/profile", Duration::from_millis(10))
                .await
                .is_err()
        );
    }

    #[test]
    fn quota_refusals_are_told_apart_from_permission_refusals() {
        assert!(is_quota_refusal(429, ""));
        assert!(is_quota_refusal(
            403,
            r#"{"error":{"details":[{"reason":"RATE_LIMIT_EXCEEDED"}]}}"#
        ));
        assert!(is_quota_refusal(
            403,
            r#"{"error":{"errors":[{"reason":"userRateLimitExceeded"}]}}"#
        ));
        assert!(!is_quota_refusal(
            403,
            r#"{"error":{"details":[{"reason":"ACCESS_TOKEN_SCOPE_INSUFFICIENT"}]}}"#
        ));
        assert!(!is_quota_refusal(500, "rateLimitExceeded"));
    }

    #[test]
    fn slowdown_is_capped() {
        let b = GmailBudget::with_unit(GmailQuotaProfile::Standard, Duration::from_millis(1))
            .with_adaptation(Duration::ZERO, Duration::from_secs(3600));
        for _ in 0..10 {
            b.note_quota_refusal(None);
        }
        assert_eq!(b.unit(), Duration::from_millis(16));
    }

    #[tokio::test]
    async fn recovery_waits_for_a_calm_interval_and_stops_at_the_start() {
        let b = GmailBudget::with_unit(GmailQuotaProfile::Standard, Duration::from_millis(10))
            .with_adaptation(Duration::ZERO, Duration::from_millis(40));
        b.note_quota_refusal(None);
        assert_eq!(b.unit(), Duration::from_millis(20));
        b.note_success();
        assert_eq!(
            b.unit(),
            Duration::from_millis(20),
            "no speed-up before a calm interval"
        );
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(45)).await;
            b.note_success();
        }
        assert_eq!(
            b.unit(),
            Duration::from_millis(10),
            "recovers to the start and no further"
        );
    }

    /// Two callers holding ONE budget are paced against each other. This is
    /// the regression: two engines used to hold two budgets.
    #[tokio::test]
    async fn callers_sharing_a_budget_are_serialized() {
        let b = std::sync::Arc::new(GmailBudget::with_unit(
            GmailQuotaProfile::Standard,
            Duration::from_millis(1),
        ));
        let started = Instant::now();
        let (x, y) = (b.clone(), b.clone());
        // Three 20-unit reads across two callers: the third waits for two
        // bookings of 20 ms each.
        tokio::join!(
            async move {
                x.admit("GET", "/messages/a").await;
                x.admit("GET", "/messages/b").await;
            },
            async move { y.admit("GET", "/messages/c").await },
        );
        assert!(started.elapsed() >= Duration::from_millis(40));
    }
}
