//! The agent door's wait on the access checks a search queued
//! (`store::archive_surface`), against a real store and a stand-in worker.

use super::super::*;
use crate::store::agent_triage::AgentTriageStore;
use crate::store::archive_surface::{
    ArchiveSurfacing, OLDER_MAIL_POLICY, RefinePoke, SurfaceWait, await_thread_access,
    search_with_surfacing, surface_archive_matches,
};
use crate::sync::ingest::{RawFetched, ingest_with_rules};
use crate::triage::access::AccessDecision;
use std::sync::Arc;
use std::time::Duration;

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

/// `idle` as long as the deadline unless a test is about the early give-up.
fn wait(deadline_ms: u64) -> SurfaceWait {
    SurfaceWait {
        deadline: Duration::from_millis(deadline_ms),
        idle: Duration::from_millis(deadline_ms),
        interval: Duration::from_millis(10),
    }
}

/// A handle a lane holds and has called runnable, as the refine lane does at
/// the top of a healthy round.
fn live_poke() -> RefinePoke {
    let poke = RefinePoke::new();
    poke.set_runnable(true);
    poke
}

fn surfacing(poke: Option<RefinePoke>, wait: SurfaceWait) -> ArchiveSurfacing {
    ArchiveSurfacing {
        archive_days: 365,
        poke,
        wait,
        caps: Default::default(),
    }
}

async fn was_poked(poke: &RefinePoke) -> bool {
    tokio::time::timeout(Duration::ZERO, poke.poked())
        .await
        .is_ok()
}

/// What the stand-in worker does with the one check it claims.
#[derive(Clone, Copy)]
enum Finish {
    Allow,
    Restrict,
    /// The daily budget is spent: parked until tomorrow.
    Park,
    /// Leased and never finished: a model call that outlives the wait.
    Hold,
}

/// A worker standing in for the refine lane: parks on the poke, then
/// finishes the one queued check the way `finish` says.
fn worker(
    store: Arc<SqliteStore>,
    account: AccountId,
    poke: RefinePoke,
    finish: Finish,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        poke.poked().await;
        let job = store
            .claim_agent_job(account, "access", Utc::now(), 120)
            .unwrap()
            .expect("the door queued a check before poking");
        match finish {
            Finish::Allow | Finish::Restrict => {
                let original = store.load_agent_access_message(&job).unwrap();
                let decision = AccessDecision {
                    restricted: matches!(finish, Finish::Restrict),
                    reason: "test".into(),
                };
                store
                    .commit_agent_access(&job, &original, &decision, &serde_json::json!({}))
                    .unwrap();
            }
            Finish::Park => {
                store
                    .defer_agent_job(
                        &job,
                        Utc::now() + chrono::Duration::hours(20),
                        "daily_budget_exhausted",
                    )
                    .unwrap();
            }
            Finish::Hold => {}
        }
    })
}

#[tokio::test]
async fn nothing_pending_and_nothing_newly_queued_never_wait() {
    let (store, account, id) = fixture();
    let poke = live_poke();
    let with = surfacing(Some(poke.clone()), wait(5_000));
    let started = std::time::Instant::now();
    assert!(
        !surface_archive_matches(&store, account, &[], &with)
            .await
            .unwrap()
    );
    // Already queued by an earlier search: nothing new, so no wait either.
    store
        .request_archive_access(account, &[id], Default::default(), Utc::now())
        .unwrap();
    assert!(
        !surface_archive_matches(&store, account, &[id], &with)
            .await
            .unwrap()
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!was_poked(&poke).await, "and nobody was poked");
}

/// NO WORKER, NO WAIT. Three ways there is nobody to wait for: no handle at
/// all (a test), a handle no lane ever took (the standalone MCP binary, were
/// it ever given one), and a lane that last said it cannot spend (no poll
/// yet, no model, an open circuit). Each queues the check and answers now.
#[tokio::test]
async fn a_door_waits_only_for_a_lane_that_can_run() {
    let stopped = RefinePoke::new();
    stopped.set_runnable(true);
    stopped.set_runnable(false);
    for poke in [None, Some(RefinePoke::new()), Some(stopped)] {
        let (store, account, id) = fixture();
        let started = std::time::Instant::now();
        assert!(
            !surface_archive_matches(&store, account, &[id], &surfacing(poke, wait(5_000)))
                .await
                .unwrap()
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(queued(&store, id), 1, "queued for whoever drains it");
    }
}

#[tokio::test]
async fn the_wait_ends_as_soon_as_the_queued_checks_finish() {
    let (store, account, id) = fixture();
    let poke = live_poke();
    let done = worker(store.clone(), account, poke.clone(), Finish::Allow);
    let started = std::time::Instant::now();
    assert!(
        surface_archive_matches(&store, account, &[id], &surfacing(Some(poke), wait(20_000)))
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
    let poke = live_poke();
    let done = worker(store.clone(), account, poke.clone(), Finish::Park);
    let started = std::time::Instant::now();
    assert!(
        surface_archive_matches(&store, account, &[id], &surfacing(Some(poke), wait(20_000)))
            .await
            .unwrap()
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    done.await.unwrap();
    assert!(!store.agent_access_allowed(account, id).unwrap());
}

/// A lane that says it can run but picks nothing up (it is in the middle of a
/// long triage) costs the caller `idle`, not the whole deadline, and there is
/// nothing new to search for afterwards.
#[tokio::test]
async fn the_wait_gives_up_early_when_no_check_is_picked_up() {
    let (store, account, id) = fixture();
    let poke = live_poke();
    let patience = SurfaceWait {
        deadline: Duration::from_secs(30),
        idle: Duration::from_millis(150),
        interval: Duration::from_millis(10),
    };
    let started = std::time::Instant::now();
    assert!(
        !surface_archive_matches(
            &store,
            account,
            &[id],
            &surfacing(Some(poke.clone()), patience)
        )
        .await
        .unwrap(),
        "nothing moved, so there is nothing to search again for"
    );
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(150), "waited {waited:?}");
    assert!(waited < Duration::from_secs(10), "waited {waited:?}");
    assert!(was_poked(&poke).await, "the lane was poked");
}

/// A check that IS being run is waited for past `idle`, up to the hard cap
/// and no further.
#[tokio::test]
async fn the_wait_stops_at_the_deadline_while_a_check_is_in_flight() {
    let (store, account, id) = fixture();
    let poke = live_poke();
    let leased = worker(store.clone(), account, poke.clone(), Finish::Hold);
    let patience = SurfaceWait {
        deadline: Duration::from_millis(400),
        idle: Duration::from_millis(40),
        interval: Duration::from_millis(10),
    };
    let started = std::time::Instant::now();
    assert!(
        surface_archive_matches(&store, account, &[id], &surfacing(Some(poke), patience))
            .await
            .unwrap()
    );
    let waited = started.elapsed();
    leased.await.unwrap();
    assert!(waited >= Duration::from_millis(400), "waited {waited:?}");
    assert!(waited < Duration::from_secs(10), "waited {waited:?}");
}

/// Each door lowers the shared wait to its own ceiling and never raises it.
#[test]
fn a_door_caps_the_wait_and_never_raises_it() {
    let shared = SurfaceWait::default();
    assert_eq!(shared.deadline, SurfaceWait::MCP_DEADLINE);
    assert_eq!(shared.idle, Duration::from_secs(2));
    assert_eq!(SurfaceWait::MCP_DEADLINE, Duration::from_secs(8));
    assert_eq!(SurfaceWait::HTTP_DEADLINE, Duration::from_secs(6));
    assert_eq!(
        shared.capped(SurfaceWait::HTTP_DEADLINE).deadline,
        Duration::from_secs(6)
    );
    assert_eq!(wait(50).capped(SurfaceWait::HTTP_DEADLINE), wait(50));
}

/// A thread with a live, readable message and one archive ancestor nobody
/// has assessed.
fn mixed_thread(store: &SqliteStore, account: AccountId) -> (i64, i64) {
    let old = old_mail(store, account, "mixed");
    let live = store
        .upsert_message(&crate::types::NewMessage {
            account_id: account,
            gmail_msg_id: "live".into(),
            thread_id: "mixed".into(),
            from_addr: "pat@old.example".into(),
            from_name: None,
            subject: "osprey plans".into(),
            received_at: Utc::now(),
            snippet: String::new(),
            body: "osprey".into(),
            body_html: None,
            is_sent: false,
            is_spam: false,
            to_addrs: None,
            cc_addrs: None,
            bcc_addrs: None,
            list_unsubscribe: None,
            list_unsub_one_click: false,
            auth_pass: None,
        })
        .unwrap();
    let conn = store.lock().unwrap();
    conn.execute(
        "INSERT INTO agent_message_state(account_id,message_id,content_snapshot,revision,access)
         VALUES(?1,?2,'s',1,'allowed')",
        params![account, live],
    )
    .unwrap();
    (old, live)
}

/// THE THREAD OPEN WAITS FOR THE VERDICT IT ASKED FOR. Without the wait the
/// first read serves the thread with the ancestor missing, and the next one,
/// after the check comes back restricted, says the thread does not exist.
#[tokio::test]
async fn a_thread_open_waits_for_its_sibling_checks_before_the_guard() {
    for finish in [Finish::Restrict, Finish::Allow] {
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let account = store.ensure_account("me@localhost").unwrap();
        let (old, live) = mixed_thread(&store, account);
        let poke = live_poke();
        let done = worker(store.clone(), account, poke.clone(), finish);
        await_thread_access(
            &store,
            account,
            "mixed",
            &surfacing(Some(poke), wait(20_000)),
        )
        .await
        .unwrap();
        done.await.unwrap();
        let view = store.thread_view(account, "mixed");
        match finish {
            Finish::Restrict => assert!(matches!(view, Err(CoreError::NotFound))),
            _ => {
                let ids: Vec<i64> = view.unwrap().messages.iter().map(|m| m.id).collect();
                assert_eq!(ids, vec![old, live]);
            }
        }
        assert_eq!(queued(&store, old), 1, "one check, queued by the open");
    }
}

/// With no lane to wait for, the open still queues the check and the read
/// that follows omits the sibling, as before.
#[tokio::test]
async fn a_thread_open_without_a_lane_queues_and_omits() {
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let account = store.ensure_account("me@localhost").unwrap();
    let (old, live) = mixed_thread(&store, account);
    let started = std::time::Instant::now();
    await_thread_access(&store, account, "mixed", &surfacing(None, wait(5_000)))
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(queued(&store, old), 1);
    let ids: Vec<i64> = store
        .thread_view(account, "mixed")
        .unwrap()
        .messages
        .iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(ids, vec![live]);
    assert_eq!(queued(&store, old), 1, "the read queued nothing more");
}

/// THE ISSUE'S ACCEPTANCE CASE: a message older than the account's first
/// sync, stored by the walk, found by an exact-subject agent search after
/// one access check, for one model-call job in total.
#[tokio::test]
async fn an_old_message_is_found_by_subject_for_exactly_one_job() {
    let (store, account, id) = fixture();
    assert_eq!(queued(&store, id), 0, "the walk bought nothing");
    let poke = live_poke();
    let done = worker(store.clone(), account, poke.clone(), Finish::Allow);
    let surfacing = surfacing(Some(poke), wait(20_000));
    let search = move |store: &SqliteStore| {
        store.external_search_surfacing(
            account,
            "heron one",
            crate::store::SearchSort::BestMatch,
            10,
        )
    };
    let (hits, horizon) = search_with_surfacing(&store, account, &surfacing, search)
        .await
        .unwrap();
    done.await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, id);
    assert_eq!(hits[0].subject, "heron one");
    assert_eq!(horizon.archive.unwrap().older_mail, OLDER_MAIL_POLICY);

    // And again, and again: the same answer for the same one job.
    for _ in 0..2 {
        let (again, _) = search_with_surfacing(&store, account, &surfacing, search)
            .await
            .unwrap();
        assert_eq!(again.len(), 1);
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
