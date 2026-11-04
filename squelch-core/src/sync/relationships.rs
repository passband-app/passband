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
//!   the clock when nothing arrives. The day it last completed is stamped in
//!   `sync_state`, so a restart does not run it twice; a daemon that was off
//!   at 03:00 runs it on its first round after.
//! - THE FIRST-RUN BACKFILL, the same walk, once per store: a daemon upgraded
//!   onto an existing mailbox has mail and no rows. Until it completes the
//!   agent context carries no `relationship` key at all. Stamped only AFTER
//!   the whole walk.
//!
//! THE WALK IS SPREAD OVER ROUNDS. One round runs at most
//! `relationships::WALK_CHUNKS_PER_ROUND` chunks and saves where it stopped
//! (`relationships::RELATIONSHIPS_WALK_CURSOR_KEY`), so triage interleaves
//! with a backfill of any size instead of waiting behind all of it, and a
//! restart resumes from the cursor instead of from the first address. Each
//! chunk is one lock acquisition, and the doors get the mutex back between
//! them.
//!
//! COSTS NO MODEL CALL, so it runs whether or not a poll has succeeded or a
//! model is configured.

use chrono::{DateTime, Datelike, Duration as ChronoDuration, NaiveDate, Utc};

use super::SyncEngine;
use crate::credentials::CredentialStore;
use crate::error::Result;
use crate::owner_clock::{OwnerClock, OwnerZone};
use crate::store::sqlite::relationships::{
    DIRTY_DRAIN_MAX, RECOMPUTE_CHUNK, RELATIONSHIPS_BACKFILL_KEY, RELATIONSHIPS_DAILY_KEY,
    RELATIONSHIPS_WALK_CURSOR_KEY, RELATIONSHIPS_WALK_RETRY_KEY, WALK_CHUNKS_PER_ROUND,
    WALK_RETRY_SECS,
};
use crate::store::{Store, SyncState};
use crate::types::AccountId;

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

/// What one round did to the full walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalkProgress {
    /// No walk was due or running.
    Idle,
    /// The walk ran this round's chunks and stopped after this address.
    Paused(String),
    /// The walk reached the end this round and stamped the day.
    Completed,
    /// The walk failed this round, or an earlier failure's wait has not run
    /// out; its cursor is kept for when it has.
    BackingOff,
}

/// One round of ledger work, synchronously: the walk's share for this round
/// (`today` is the [`ledger_day`]), then the dirty set. Public within the
/// crate so a test drives rounds without an engine.
pub(crate) fn ledger_round<S: Store + ?Sized>(
    store: &S,
    account: AccountId,
    today: u64,
    now: DateTime<Utc>,
) -> Result<WalkProgress> {
    let backfilled = store
        .sync_state(account, RELATIONSHIPS_BACKFILL_KEY)?
        .is_some();
    let last_day = store
        .sync_state(account, RELATIONSHIPS_DAILY_KEY)?
        .map(|s| s.last_uid);
    let cursor = store.get_app_setting(account, RELATIONSHIPS_WALK_CURSOR_KEY)?;
    // A walk already running finishes whatever the day says; a new one starts
    // only when one is owed.
    let start = match cursor {
        Some(cursor) => Some(cursor),
        None if full_recompute_due(backfilled, last_day, today) => Some(String::new()),
        None => None,
    };
    // A wait that will not parse is no wait: the failure falls toward trying.
    let waiting = store
        .get_app_setting(account, RELATIONSHIPS_WALK_RETRY_KEY)?
        .and_then(|at| DateTime::parse_from_rfc3339(&at).ok())
        .is_some_and(|at| now < at.with_timezone(&Utc));
    let progress = match start {
        None => WalkProgress::Idle,
        Some(_) if waiting => WalkProgress::BackingOff,
        Some(mut after) => {
            let mut done = false;
            let mut failed = false;
            for _ in 0..WALK_CHUNKS_PER_ROUND {
                match store.recompute_relationship_chunk(account, Some(&after), RECOMPUTE_CHUNK) {
                    Ok(Some(last)) => after = last,
                    Ok(None) => {
                        done = true;
                        break;
                    }
                    Err(e) => {
                        eprintln!(
                            "squelch: relationship ledger walk failed ({e}); \
                             retrying in {WALK_RETRY_SECS}s from where it stopped"
                        );
                        failed = true;
                        break;
                    }
                }
            }
            if failed {
                store.set_app_setting(account, RELATIONSHIPS_WALK_CURSOR_KEY, &after)?;
                store.set_app_setting(
                    account,
                    RELATIONSHIPS_WALK_RETRY_KEY,
                    &(now + ChronoDuration::seconds(WALK_RETRY_SECS)).to_rfc3339(),
                )?;
                WalkProgress::BackingOff
            } else if done {
                let stamp = |key: &str, last_uid: u64| {
                    store.set_sync_state(
                        account,
                        key,
                        &SyncState {
                            uidvalidity: 0,
                            last_uid,
                        },
                    )
                };
                stamp(RELATIONSHIPS_DAILY_KEY, today)?;
                if !backfilled {
                    stamp(RELATIONSHIPS_BACKFILL_KEY, 1)?;
                }
                store.delete_app_setting(account, RELATIONSHIPS_WALK_CURSOR_KEY)?;
                store.delete_app_setting(account, RELATIONSHIPS_WALK_RETRY_KEY)?;
                WalkProgress::Completed
            } else {
                store.set_app_setting(account, RELATIONSHIPS_WALK_CURSOR_KEY, &after)?;
                WalkProgress::Paused(after)
            }
        }
    };
    store.drain_relationship_dirty(account, DIRTY_DRAIN_MAX)?;
    Ok(progress)
}

impl<S: Store + 'static, C: CredentialStore + 'static + ?Sized> SyncEngine<S, C> {
    /// One refine round's ledger work. Cannot fail the round: an error is
    /// logged and the next round tries again.
    pub(super) async fn relationship_round(&self) {
        let zone = self.store.owner_zone(self.account_id).unwrap_or_default();
        let now = Utc::now();
        let today = ledger_day(now, zone);
        if let Err(e) = ledger_round(&*self.store, self.account_id, today, now) {
            eprintln!("squelch: relationship ledger round failed ({e})");
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

#[cfg(test)]
mod walk_tests {
    use super::*;
    use crate::store::SqliteStore;

    /// `n` senders, one message each.
    fn store_with_senders(n: usize) -> (SqliteStore, AccountId) {
        let store = SqliteStore::open_in_memory().unwrap();
        let account = store.ensure_account("me@example.com").unwrap();
        for i in 0..n {
            store
                .upsert_message(&crate::types::NewMessage {
                    account_id: account,
                    gmail_msg_id: format!("m{i}"),
                    thread_id: format!("t{i}"),
                    from_addr: format!("s{i:04}@example.com"),
                    from_name: None,
                    subject: "s".into(),
                    received_at: Utc::now(),
                    snippet: String::new(),
                    body: String::new(),
                    body_html: None,
                    is_sent: false,
                    is_spam: false,
                    to_addrs: None,
                    cc_addrs: None,
                    bcc_addrs: None,
                    list_unsubscribe: None,
                    list_unsub_one_click: false,
                    auth_pass: None,
                    sender_signals: None,
                })
                .unwrap();
        }
        (store, account)
    }

    fn rows(store: &SqliteStore, account: AccountId) -> usize {
        store
            .list_relationships(
                account,
                crate::store::sqlite::relationships::RelationshipSort::Volume,
                500,
            )
            .unwrap()
            .rows
            .len()
    }

    /// The walk is spread over rounds, resumes from its cursor (including
    /// across a restart, which is a fresh round over the same store), and
    /// stamps the backfill and the day only when it reaches the end.
    #[test]
    fn the_walk_runs_a_bounded_share_per_round_and_resumes_from_its_cursor() {
        let per_round = WALK_CHUNKS_PER_ROUND * RECOMPUTE_CHUNK;
        let (store, account) = store_with_senders(per_round + 10);
        let first = ledger_round(&store, account, 20261104, Utc::now()).unwrap();
        let WalkProgress::Paused(cursor) = first else {
            panic!("a backfill bigger than a round must pause: {first:?}");
        };
        // This round's share, in address order: the senders up to about the
        // 200th (the universe may hold one more address that sorts first,
        // the account's own, which is recounted and has no row).
        assert!(
            cursor == format!("s{:04}@example.com", per_round - 1)
                || cursor == format!("s{:04}@example.com", per_round - 2),
            "{cursor}"
        );
        assert_eq!(
            store
                .get_app_setting(account, RELATIONSHIPS_WALK_CURSOR_KEY)
                .unwrap()
                .as_deref(),
            Some(cursor.as_str())
        );
        assert!(
            store
                .sync_state(account, RELATIONSHIPS_BACKFILL_KEY)
                .unwrap()
                .is_none(),
            "a half-filled table is not backfilled"
        );

        assert_eq!(
            ledger_round(&store, account, 20261104, Utc::now()).unwrap(),
            WalkProgress::Completed
        );
        assert_eq!(rows(&store, account), per_round + 10);
        assert!(
            store
                .get_app_setting(account, RELATIONSHIPS_WALK_CURSOR_KEY)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .sync_state(account, RELATIONSHIPS_BACKFILL_KEY)
                .unwrap()
                .is_some()
        );
        // The same ledger day again: nothing owed.
        assert_eq!(
            ledger_round(&store, account, 20261104, Utc::now()).unwrap(),
            WalkProgress::Idle
        );
        // The next day walks again.
        assert!(matches!(
            ledger_round(&store, account, 20261105, Utc::now()).unwrap(),
            WalkProgress::Paused(_)
        ));
    }
}

#[cfg(test)]
mod backoff_tests {
    use super::*;
    use crate::store::SqliteStore;

    /// A failed walk's wait holds every walk round until it runs out, and
    /// the cursor it saved is where the walk picks up, not the first address.
    #[test]
    fn a_failed_walk_waits_an_hour_and_resumes_from_its_cursor() {
        let store = SqliteStore::open_in_memory().unwrap();
        let account = store.ensure_account("me@example.com").unwrap();
        let now = Utc::now();
        store
            .set_app_setting(account, RELATIONSHIPS_WALK_CURSOR_KEY, "m@example.com")
            .unwrap();
        store
            .set_app_setting(
                account,
                RELATIONSHIPS_WALK_RETRY_KEY,
                &(now + ChronoDuration::minutes(30)).to_rfc3339(),
            )
            .unwrap();
        assert_eq!(
            ledger_round(&store, account, 20261104, now).unwrap(),
            WalkProgress::BackingOff
        );
        assert_eq!(
            store
                .get_app_setting(account, RELATIONSHIPS_WALK_CURSOR_KEY)
                .unwrap()
                .as_deref(),
            Some("m@example.com"),
            "the cursor is kept through the wait"
        );
        // Once the wait has run out the walk resumes, finishes (the store is
        // empty past the cursor), and clears both keys.
        let later = now + ChronoDuration::minutes(31);
        assert_eq!(
            ledger_round(&store, account, 20261104, later).unwrap(),
            WalkProgress::Completed
        );
        for key in [RELATIONSHIPS_WALK_CURSOR_KEY, RELATIONSHIPS_WALK_RETRY_KEY] {
            assert!(
                store.get_app_setting(account, key).unwrap().is_none(),
                "{key}"
            );
        }
    }
}
