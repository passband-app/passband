//! The agent door's wait on the access checks a search queued
//! (`store::archive_surface`), against a real store and a stand-in worker.

use super::super::*;
use crate::store::agent_triage::AgentTriageStore;
use crate::store::archive_surface::{
    ArchiveSurfacing, SurfaceWait, search_with_surfacing, surface_archive_matches,
};
use crate::sync::ingest::{RawFetched, ingest_with_rules};
use crate::triage::access::AccessDecision;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

fn old_mail(store: &SqliteStore, account: AccountId, key: &str) -> i64 {
    let at = Utc::now() - chrono::Duration::days(200);
    let raw = RawFetched {
        account_id: account,
        gmail_msg_id: key.into(),
        gmail_thread_id: Some(key.into()),
        raw: format!(
            "From: pat@old.example\r\nTo: me@localhost\r\nSubject: heron {key}\r\nDate: {}\r\n\r\nthe heron count",
            at.to_rfc2822()
        )
        .into_bytes(),
        internal_date: Some(at),
        is_sent: false,
        is_spam: false,
        account_addr: "me@localhost".into(),
    };
    let message = ingest_with_rules(&raw, &Default::default(), at, &[], |_| false);
    store.ingest_archive_message(&message).unwrap().unwrap()
}

fn fixture() -> (Arc<SqliteStore>, AccountId, i64) {
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let account = store.ensure_account("me@localhost").unwrap();
    let id = old_mail(&store, account, "one");
    (store, account, id)
}

fn queued(store: &SqliteStore, message: i64) -> i64 {
    store
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM agent_triage_jobs WHERE message_id=?1",
            [message],
            |row| row.get(0),
        )
        .unwrap()
}

fn wait(deadline_ms: u64) -> SurfaceWait {
    SurfaceWait {
        deadline: Duration::from_millis(deadline_ms),
        interval: Duration::from_millis(10),
    }
}

/// A worker standing in for the refine lane: parks on the poke, then
/// finishes the one queued check the way `finish` says.
fn worker(
    store: Arc<SqliteStore>,
    account: AccountId,
    poke: Arc<Notify>,
    allow: Option<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        poke.notified().await;
        let job = store
            .claim_agent_job(account, "access", Utc::now(), 120)
            .unwrap()
            .expect("the search queued a check before poking");
        match allow {
            Some(allowed) => {
                let original = store.load_agent_access_message(&job).unwrap();
                let decision = AccessDecision {
                    restricted: !allowed,
                    reason: "test".into(),
                };
                store
                    .commit_agent_access(&job, &original, &decision, &serde_json::json!({}))
                    .unwrap();
            }
            // The daily budget is spent: parked until tomorrow.
            None => {
                store
                    .defer_agent_job(
                        &job,
                        Utc::now() + chrono::Duration::hours(20),
                        "daily_budget_exhausted",
                    )
                    .unwrap();
            }
        }
    })
}

#[tokio::test]
async fn nothing_pending_and_nothing_newly_queued_never_wait() {
    let (store, account, id) = fixture();
    let poke = Arc::new(Notify::new());
    let started = std::time::Instant::now();
    assert!(
        !surface_archive_matches(&store, account, &[], Some(&poke), wait(5_000))
            .await
            .unwrap()
    );
    // Already queued by an earlier search: nothing new, so no wait either.
    store.request_archive_access(account, &[id]).unwrap();
    assert!(
        !surface_archive_matches(&store, account, &[id], Some(&poke), wait(5_000))
            .await
            .unwrap()
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(
        tokio::time::timeout(Duration::ZERO, poke.notified())
            .await
            .is_err(),
        "and nobody was poked"
    );
}

#[tokio::test]
async fn without_a_poke_handle_the_check_is_queued_and_not_awaited() {
    let (store, account, id) = fixture();
    let started = std::time::Instant::now();
    assert!(
        !surface_archive_matches(&store, account, &[id], None, wait(5_000))
            .await
            .unwrap()
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(queued(&store, id), 1);
}

#[tokio::test]
async fn the_wait_ends_as_soon_as_the_queued_checks_finish() {
    let (store, account, id) = fixture();
    let poke = Arc::new(Notify::new());
    let done = worker(store.clone(), account, poke.clone(), Some(true));
    let started = std::time::Instant::now();
    assert!(
        surface_archive_matches(&store, account, &[id], Some(&poke), wait(20_000))
            .await
            .unwrap(),
        "something was queued and waited for: search again"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "returned on completion, not at the deadline"
    );
    done.await.unwrap();
    assert!(store.agent_access_allowed(account, id).unwrap());
}

#[tokio::test]
async fn the_wait_ends_when_the_check_is_parked_on_the_budget() {
    let (store, account, id) = fixture();
    let poke = Arc::new(Notify::new());
    let done = worker(store.clone(), account, poke.clone(), None);
    let started = std::time::Instant::now();
    assert!(
        surface_archive_matches(&store, account, &[id], Some(&poke), wait(20_000))
            .await
            .unwrap()
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    done.await.unwrap();
    assert!(!store.agent_access_allowed(account, id).unwrap());
}

#[tokio::test]
async fn the_wait_gives_up_at_the_deadline() {
    let (store, account, id) = fixture();
    // Poked, but nobody is draining the queue.
    let poke = Arc::new(Notify::new());
    let started = std::time::Instant::now();
    assert!(
        surface_archive_matches(&store, account, &[id], Some(&poke), wait(150))
            .await
            .unwrap()
    );
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(150), "waited {waited:?}");
    assert!(waited < Duration::from_secs(5), "waited {waited:?}");
    assert!(
        tokio::time::timeout(Duration::ZERO, poke.notified())
            .await
            .is_ok(),
        "the lane was poked"
    );
}

/// THE ISSUE'S ACCEPTANCE CASE: a message older than the account's first
/// sync, stored by the walk, found by an exact-subject agent search after
/// one access check, for one model-call job in total.
#[tokio::test]
async fn an_old_message_is_found_by_subject_for_exactly_one_job() {
    let (store, account, id) = fixture();
    assert_eq!(queued(&store, id), 0, "the walk bought nothing");
    let poke = Arc::new(Notify::new());
    let done = worker(store.clone(), account, poke.clone(), Some(true));
    let surfacing = ArchiveSurfacing {
        archive_days: 365,
        poke: Some(poke),
        wait: wait(20_000),
    };
    let search = move |store: &SqliteStore| {
        store.external_search_surfacing(
            account,
            "heron one",
            crate::store::SearchSort::BestMatch,
            10,
        )
    };
    let (found, horizon) = search_with_surfacing(&store, account, &surfacing, search)
        .await
        .unwrap();
    done.await.unwrap();
    assert_eq!(found.hits.len(), 1);
    assert_eq!(found.hits[0].id, id);
    assert_eq!(found.hits[0].subject, "heron one");
    assert!(found.pending_archive.is_empty());
    assert!(horizon.archive.is_some());
    assert!(horizon.indexed_since.unwrap() < Utc::now() - chrono::Duration::days(199));

    // And again, and again: the same answer for the same one job.
    for _ in 0..2 {
        let (again, _) = search_with_surfacing(&store, account, &surfacing, search)
            .await
            .unwrap();
        assert_eq!(again.hits.len(), 1);
    }
    let jobs: i64 = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM agent_triage_jobs WHERE account_id=?1",
            [account],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(jobs, 1, "one access check, and nothing else, ever");
}
