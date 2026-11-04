//! The relationship ledger's pass (docs/UNDERSTANDING.md §2.1): when its rows
//! are recounted. The counting is `store::sqlite::relationships`; this file is
//! only the schedule, and it is one call from the refine lane so the lane's
//! own code stays about triage.
//!
//! THREE TRIGGERS, each cheap when there is nothing to do:
//!
//! - THE DIRTY SET, every refine round, BEFORE the triage pass: ingest marks a
//!   new message's sender, so a brand-new sender has a row by the time its
//!   first job is claimed. Bounded per round
//!   (`relationships::DIRTY_DRAIN_MAX`); a flood is carried by later rounds.
//! - THE NIGHTLY FULL RECOMPUTE, once per owner-local day from 03:00
//!   (`owner_clock`), because the monthly rate and the ledger floors move with
//!   the clock when nothing arrives. The day it last ran is stamped in
//!   `sync_state`, so a restart does not run it twice; a daemon that was off
//!   at 03:00 runs it on its first round after.
//! - THE FIRST-RUN BACKFILL, the same walk, once per store: a daemon upgraded
//!   onto an existing mailbox has mail and no rows. Until it completes the
//!   agent context carries no `relationship` key at all. Stamped only AFTER
//!   the whole walk, so a crash mid-walk walks again.
//!
//! COSTS NO MODEL CALL, so it runs whether or not a poll has succeeded or a
//! model is configured. The full walk yields between chunks: each chunk is
//! one lock acquisition (`relationships::RECOMPUTE_CHUNK` addresses), and the
//! doors get the mutex back between them.

use chrono::{DateTime, Datelike, Duration as ChronoDuration, NaiveDate, Utc};

use super::SyncEngine;
use crate::credentials::CredentialStore;
use crate::owner_clock::{OwnerClock, OwnerZone};
use crate::store::sqlite::relationships::{
    DIRTY_DRAIN_MAX, RECOMPUTE_CHUNK, RELATIONSHIPS_BACKFILL_KEY, RELATIONSHIPS_DAILY_KEY,
};
use crate::store::{Store, SyncState};

/// The owner-local hour the nightly recompute is due from.
pub const FULL_RECOMPUTE_LOCAL_HOUR: u32 = 3;

/// The owner-local "ledger day" `now` falls in, as `yyyymmdd`: the calendar
/// date of `now` shifted back [`FULL_RECOMPUTE_LOCAL_HOUR`] hours, so the day
/// turns over at 03:00 and not at midnight. One run per value.
pub fn ledger_day(now: DateTime<Utc>, zone: OwnerZone) -> u64 {
    let local = OwnerClock::new(now, zone).local_now();
    let shifted = local - ChronoDuration::hours(i64::from(FULL_RECOMPUTE_LOCAL_HOUR));
    let day: NaiveDate = shifted.date_naive();
    u64::from(day.year().max(0) as u32) * 10_000
        + u64::from(day.month()) * 100
        + u64::from(day.day())
}

/// Is a full recompute owed? Always before the first-run backfill; after it,
/// once per [`ledger_day`].
pub fn full_recompute_due(backfilled: bool, last_day: Option<u64>, today: u64) -> bool {
    !backfilled || last_day != Some(today)
}

impl<S: Store + 'static, C: CredentialStore + 'static + ?Sized> SyncEngine<S, C> {
    /// One refine round's ledger work. Cannot fail the round: an error is
    /// logged and the next round tries again.
    pub(super) async fn relationship_round(&self) {
        let account = self.account_id;
        let backfilled = match self.store.sync_state(account, RELATIONSHIPS_BACKFILL_KEY) {
            Ok(state) => state.is_some(),
            Err(e) => {
                eprintln!("squelch: relationship ledger state unreadable ({e}); skipping round");
                return;
            }
        };
        let last_day = self
            .store
            .sync_state(account, RELATIONSHIPS_DAILY_KEY)
            .ok()
            .flatten()
            .map(|s| s.last_uid);
        let zone = self.store.owner_zone(account).unwrap_or_default();
        let today = ledger_day(Utc::now(), zone);
        if full_recompute_due(backfilled, last_day, today)
            && self.recompute_all_relationships().await
        {
            let stamp = |key: &str, last_uid: u64| {
                self.store.set_sync_state(
                    account,
                    key,
                    &SyncState {
                        uidvalidity: 0,
                        last_uid,
                    },
                )
            };
            let stamped = stamp(RELATIONSHIPS_DAILY_KEY, today).and_then(|()| {
                if backfilled {
                    Ok(())
                } else {
                    stamp(RELATIONSHIPS_BACKFILL_KEY, 1)
                }
            });
            if let Err(e) = stamped {
                eprintln!("squelch: relationship ledger stamp failed ({e})");
            }
        }
        if let Err(e) = self
            .store
            .drain_relationship_dirty(account, DIRTY_DRAIN_MAX)
        {
            eprintln!("squelch: relationship ledger drain failed ({e})");
        }
    }

    /// The full walk, chunk by chunk, yielding between chunks. `true` when it
    /// reached the end.
    async fn recompute_all_relationships(&self) -> bool {
        let mut after: Option<String> = None;
        loop {
            match self.store.recompute_relationship_chunk(
                self.account_id,
                after.as_deref(),
                RECOMPUTE_CHUNK,
            ) {
                Ok(Some(last)) => after = Some(last),
                Ok(None) => return true,
                Err(e) => {
                    eprintln!("squelch: relationship ledger recompute failed ({e})");
                    return false;
                }
            }
            tokio::task::yield_now().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owner_clock::OwnerZoneSource;
    use chrono::TimeZone;

    fn pacific() -> OwnerZone {
        OwnerZone {
            zone: chrono_tz::Tz::America__Los_Angeles,
            source: OwnerZoneSource::Client,
        }
    }

    #[test]
    fn the_ledger_day_turns_over_at_three_in_the_owners_zone() {
        // 02:59 Pacific on Nov 4 is still Nov 3's ledger day...
        let before = Utc.with_ymd_and_hms(2026, 11, 4, 10, 59, 0).unwrap();
        assert_eq!(ledger_day(before, pacific()), 20261103);
        // ...and 03:00 Pacific is Nov 4's, though UTC turned over hours ago.
        let at = Utc.with_ymd_and_hms(2026, 11, 4, 11, 0, 0).unwrap();
        assert_eq!(ledger_day(at, pacific()), 20261104);
        assert_eq!(ledger_day(at, OwnerZone::UTC), 20261104);
    }

    #[test]
    fn the_recompute_runs_once_a_day_and_always_before_the_backfill() {
        assert!(
            full_recompute_due(false, Some(20261104), 20261104),
            "no backfill yet"
        );
        assert!(full_recompute_due(true, None, 20261104));
        assert!(full_recompute_due(true, Some(20261103), 20261104));
        assert!(
            !full_recompute_due(true, Some(20261104), 20261104),
            "a restart the same day does not rerun it"
        );
    }
}
