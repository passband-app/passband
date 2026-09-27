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
//! What it cannot see: another PROCESS on the same Google user and project,
//! such as a self-hosted squelchd syncing the mailbox a hosted tenant also
//! syncs. Those two share Google's budget and not this one.

use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::config::GmailQuotaProfile;

/// Paces Gmail requests so the mailbox never exceeds 80% of its per-user,
/// per-minute unit budget, charging each request by what Gmail bills for it.
pub struct GmailBudget {
    profile: GmailQuotaProfile,
    /// The time one unit occupies: 60 s over 80% of the per-minute budget.
    unit: Duration,
    /// When the next request may be admitted.
    next: Mutex<Instant>,
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

    /// An explicit unit length. Tests use zero (unpaced) or a few ms.
    pub fn with_unit(profile: GmailQuotaProfile, unit: Duration) -> Self {
        Self {
            profile,
            unit,
            next: Mutex::new(Instant::now()),
        }
    }

    /// Wait for a slot, then book the request's cost against the budget.
    ///
    /// Serialized across every caller holding this budget, 401 retries
    /// included. The next slot is set from NOW rather than from the previous
    /// slot, so an idle mailbox never banks a burst.
    pub async fn admit(&self, method: &str, path: &str) {
        let mut next = self.next.lock().await;
        tokio::time::sleep_until(*next).await;
        *next = Instant::now() + self.unit * self.cost(method, path);
    }

    /// Units Gmail charges for one request, by method and API path (the part
    /// after `/gmail/v1/users/me`, query string removed).
    pub fn cost(&self, method: &str, path: &str) -> u32 {
        gmail_method_cost(self.profile, method, path)
    }

    /// The pacing unit, for tests and diagnostics.
    pub fn unit(&self) -> Duration {
        self.unit
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
        // messages.modify and messages.trash, the write door's other verbs.
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
            assert_eq!(b.cost("POST", "/messages/a/trash"), 5);
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
