//! Outbox (`scheduled_sends`) store tests: the state machine, the lease, and
//! the second attachment claim.

use super::super::migrate::migrate;
use super::super::*;
use super::support::*;
use crate::store::{
    ATTACHMENT_MISSING, CancelOutcome, ParentContext, ScheduledSend, ScheduledSendState,
};

/// A fixed base instant, so every clock move is asserted, not raced.
fn t(minutes: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-07-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
        + chrono::Duration::minutes(minutes)
}

const LEASE: chrono::Duration = chrono::Duration::seconds(120);

/// A row scheduled for `at`, created at `t(0)`, told apart by its request.
fn schedule(store: &SqliteStore, acct: AccountId, tag: &str, at: DateTime<Utc>) -> ScheduledSend {
    store
        .insert_scheduled_send(
            acct,
            &format!(r#"{{"to":"a@example.com","body":"{tag}"}}"#),
            at,
            &format!("<{tag}@passband>"),
            t(0),
        )
        .unwrap()
}

fn stage(store: &SqliteStore, acct: AccountId, cid: &str) -> i64 {
    store
        .stage_outbound_attachment(acct, cid, "text/plain", cid, b"x", t(0))
        .unwrap()
        .id
}

/// `(draft_id, scheduled_send_id)` of the file with this content id, `None`
/// when no such row exists. BY CONTENT ID: row ids are bare rowids and get
/// reused, so an id a test assumes is fresh proves nothing.
fn claims(store: &SqliteStore, cid: &str) -> Option<(Option<i64>, Option<i64>)> {
    store
        .lock()
        .unwrap()
        .query_row(
            "SELECT draft_id, scheduled_send_id FROM outbound_attachments WHERE content_id = ?1",
            params![cid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .unwrap()
}

fn new_draft(store: &SqliteStore, acct: AccountId) -> i64 {
    store
        .upsert_draft(acct, DraftKey::NewCreate, DraftFields::default(), t(0))
        .unwrap()
        .unwrap()
        .id
}

#[test]
fn insert_round_trips_and_the_listing_orders_by_send_at_and_hides_settled_rows() {
    let (store, acct) = store();
    let late = schedule(&store, acct, "late", t(90));
    let early = schedule(&store, acct, "early", t(30));
    let tie = schedule(&store, acct, "tie", t(30));
    assert_eq!(late.state, ScheduledSendState::Scheduled);
    assert_eq!(late.attempts, 0);
    assert_eq!(late.send_at, t(90));
    assert_eq!(late.message_id_header, "<late@passband>");
    assert!(late.lease_until.is_none() && late.last_error.is_none());
    assert_eq!(
        store.scheduled_send_by_id(acct, late.id).unwrap().unwrap(),
        late
    );

    // A settled row leaves the listing; a failed one stays (it wants the user).
    let gone = schedule(&store, acct, "gone", t(10));
    assert!(matches!(
        store.cancel_scheduled_send(acct, gone.id, t(1)).unwrap(),
        CancelOutcome::Cancelled(_)
    ));
    let bodies: Vec<String> = store
        .list_scheduled_sends(acct)
        .unwrap()
        .into_iter()
        .map(|r| r.request)
        .collect();
    assert_eq!(bodies.len(), 3);
    assert!(bodies[0].contains("early") && bodies[1].contains("tie"));
    assert!(bodies[2].contains("late"));
    assert!(early.id < tie.id, "the tie breaks by id");
}

#[test]
fn claim_takes_exactly_one_due_row_under_a_lease() {
    let (store, acct) = store();
    let due = schedule(&store, acct, "due", t(10));
    let later = schedule(&store, acct, "later", t(60));

    assert!(
        store
            .claim_due_scheduled_send(acct, t(5), LEASE)
            .unwrap()
            .is_none(),
        "nothing is due yet"
    );
    let got = store
        .claim_due_scheduled_send(acct, t(10), LEASE)
        .unwrap()
        .expect("send_at <= now is due");
    assert_eq!(got.id, due.id);
    assert_eq!(got.state, ScheduledSendState::Sending);
    assert_eq!(got.attempts, 1);
    assert_eq!(got.lease_until, Some(t(10) + LEASE));
    assert!(
        store
            .claim_due_scheduled_send(acct, t(10), LEASE)
            .unwrap()
            .is_none(),
        "the one due row is held: a second claim finds nothing"
    );
    let later = store.scheduled_send_by_id(acct, later.id).unwrap().unwrap();
    assert_eq!(
        later.state,
        ScheduledSendState::Scheduled,
        "not due, not claimed"
    );
    assert!(
        store
            .claim_due_scheduled_send(acct, t(10), chrono::Duration::zero())
            .is_err()
    );
}

#[test]
fn only_a_lapsed_lease_is_offered_for_recovery() {
    let (store, acct) = store();
    let a = schedule(&store, acct, "a", t(0));
    let b = schedule(&store, acct, "b", t(1));
    store
        .claim_due_scheduled_send(acct, t(0), LEASE)
        .unwrap()
        .unwrap();
    // Claimed a minute later, so its lease runs a minute longer.
    store
        .claim_due_scheduled_send(acct, t(1), LEASE)
        .unwrap()
        .unwrap();

    let at = |now| -> Vec<i64> {
        store
            .expired_sending_scheduled_sends(acct, now)
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect()
    };
    assert!(at(t(1)).is_empty(), "both leases still run");
    assert_eq!(at(t(0) + LEASE), vec![a.id], "a's lease ends exactly now");
    assert_eq!(at(t(10)), vec![a.id, b.id]);
}

#[test]
fn retry_failed_requeue_and_sent_each_move_only_a_sending_row() {
    let (store, acct) = store();
    let r = schedule(&store, acct, "r", t(0));
    assert!(
        !store
            .settle_scheduled_retry(acct, r.id, t(5), "http 500", t(1))
            .unwrap(),
        "a row nobody claimed cannot be settled"
    );

    // Retry: back to scheduled at the new time, lease gone, reason kept.
    store
        .claim_due_scheduled_send(acct, t(0), LEASE)
        .unwrap()
        .unwrap();
    assert!(
        store
            .settle_scheduled_retry(acct, r.id, t(2), "http 500", t(0))
            .unwrap()
    );
    let row = store.scheduled_send_by_id(acct, r.id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Scheduled);
    assert_eq!(row.send_at, t(2));
    assert_eq!(row.lease_until, None);
    assert_eq!(row.last_error.as_deref(), Some("http 500"));
    assert_eq!(row.attempts, 1);

    // Requeue: sending -> scheduled, attempts and send_at untouched.
    store
        .claim_due_scheduled_send(acct, t(2), LEASE)
        .unwrap()
        .unwrap();
    assert!(store.requeue_scheduled_send(acct, r.id, t(9)).unwrap());
    let row = store.scheduled_send_by_id(acct, r.id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Scheduled);
    assert_eq!(
        (row.attempts, row.send_at, row.lease_until),
        (2, t(2), None)
    );

    // Failed.
    store
        .claim_due_scheduled_send(acct, t(9), LEASE)
        .unwrap()
        .unwrap();
    assert!(
        store
            .settle_scheduled_failed(acct, r.id, "no write credential", t(9))
            .unwrap()
    );
    let row = store.scheduled_send_by_id(acct, r.id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Failed);
    assert_eq!(row.attempts, 3);
    assert_eq!(row.last_error.as_deref(), Some("no write credential"));
    assert!(
        !store.requeue_scheduled_send(acct, r.id, t(9)).unwrap(),
        "a failed row is not the worker's any more"
    );

    // Send now puts it back in line for the next tick with a FRESH retry
    // budget (the user asked again); then it sends.
    assert!(store.send_scheduled_now(acct, r.id, t(20)).unwrap());
    let row = store.scheduled_send_by_id(acct, r.id).unwrap().unwrap();
    assert_eq!(
        (row.state, row.send_at, row.attempts, row.last_error),
        (ScheduledSendState::Scheduled, t(20), 0, None)
    );
    let claimed = store
        .claim_due_scheduled_send(acct, t(20), LEASE)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.attempts, 1, "the budget restarted from zero");
    assert!(
        !store.send_scheduled_now(acct, r.id, t(20)).unwrap(),
        "a sending row cannot be pulled forward"
    );
    assert!(
        store
            .settle_scheduled_sent(acct, r.id, Some(42), Some("thr"), t(21))
            .unwrap()
    );
    let row = store.scheduled_send_by_id(acct, r.id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Sent);
    assert_eq!(row.sent_message_id, Some(42));
    assert_eq!(row.sent_thread_id.as_deref(), Some("thr"));
    assert_eq!(row.lease_until, None);
    assert!(
        !store
            .settle_scheduled_sent(acct, r.id, None, None, t(22))
            .unwrap(),
        "settling twice is a no-op"
    );
    assert!(!store.send_scheduled_now(acct, r.id, t(23)).unwrap());
}

#[test]
fn scheduling_moves_claims_from_its_draft_and_consumes_the_draft() {
    let (store, acct) = store();
    let kept = stage(&store, acct, "cid-kept");
    let dropped = stage(&store, acct, "cid-dropped");
    let loose = stage(&store, acct, "cid-loose");
    let d = new_draft(&store, acct);
    store
        .claim_outbound_attachments(acct, d, &[kept, dropped], t(0))
        .unwrap();

    // The request names `kept` (the draft's) and `loose` (never claimed), not
    // `dropped`, which was removed from the tray after the last autosave.
    let row = store
        .schedule_send(
            acct,
            "{}",
            t(60),
            "<s@passband>",
            &[kept, loose],
            Some(d),
            t(1),
        )
        .unwrap();
    assert_eq!(claims(&store, "cid-kept"), Some((None, Some(row.id))));
    assert_eq!(claims(&store, "cid-loose"), Some((None, Some(row.id))));
    assert_eq!(claims(&store, "cid-dropped"), None, "went with the draft");
    assert!(store.draft_by_id(acct, d).unwrap().is_none(), "consumed");
    assert_eq!(
        row.attachments.iter().map(|a| a.id).collect::<Vec<_>>(),
        vec![kept, loose]
    );
}

#[test]
fn scheduling_is_all_or_nothing_and_never_steals_another_drafts_file() {
    let (store, acct) = store();
    let mine = stage(&store, acct, "cid-mine");
    let theirs = stage(&store, acct, "cid-theirs");
    let d_mine = new_draft(&store, acct);
    let d_theirs = new_draft(&store, acct);
    store
        .claim_outbound_attachments(acct, d_mine, &[mine], t(0))
        .unwrap();
    store
        .claim_outbound_attachments(acct, d_theirs, &[theirs], t(0))
        .unwrap();

    let err = store
        .schedule_send(
            acct,
            "{}",
            t(60),
            "<s@passband>",
            &[mine, theirs],
            Some(d_mine),
            t(1),
        )
        .unwrap_err();
    assert!(
        matches!(&err, CoreError::InvalidInput(m) if m == ATTACHMENT_MISSING),
        "{err:?}"
    );
    // Nothing moved, no row, the draft stands.
    assert_eq!(claims(&store, "cid-mine"), Some((Some(d_mine), None)));
    assert_eq!(claims(&store, "cid-theirs"), Some((Some(d_theirs), None)));
    assert!(store.list_scheduled_sends(acct).unwrap().is_empty());
    assert!(store.draft_by_id(acct, d_mine).unwrap().is_some());

    // The standalone claim: same rule, and it moves `from_draft`'s file.
    let row = schedule(&store, acct, "s", t(60));
    let err = store
        .claim_attachments_for_scheduled_send(acct, row.id, &[theirs], Some(d_mine), t(1))
        .unwrap_err();
    assert!(matches!(err, CoreError::InvalidInput(_)));
    assert_eq!(claims(&store, "cid-theirs"), Some((Some(d_theirs), None)));
    store
        .claim_attachments_for_scheduled_send(acct, row.id, &[mine], Some(d_mine), t(1))
        .unwrap();
    assert_eq!(claims(&store, "cid-mine"), Some((None, Some(row.id))));
    assert_eq!(
        store
            .scheduled_send_attachments(acct, row.id)
            .unwrap()
            .iter()
            .map(|a| a.id)
            .collect::<Vec<_>>(),
        vec![mine]
    );

    // Nor can a second scheduled send take the first one's file.
    let other = schedule(&store, acct, "o", t(60));
    assert!(
        store
            .claim_attachments_for_scheduled_send(acct, other.id, &[mine], None, t(1))
            .is_err()
    );
    assert_eq!(claims(&store, "cid-mine"), Some((None, Some(row.id))));

    // And a plain send (no draft, no schedule) cannot read or consume it.
    assert!(
        store
            .outbound_attachments(acct, None, &[mine])
            .unwrap()
            .is_empty()
    );
    store
        .delete_outbound_attachments(acct, None, &[mine])
        .unwrap();
    assert!(claims(&store, "cid-mine").is_some());
    // The worker's door can.
    assert_eq!(
        store
            .scheduled_outbound_attachments(acct, row.id, &[mine])
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .scheduled_outbound_attachment_sizes(acct, row.id, &[mine])
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .scheduled_outbound_attachments(acct, other.id, &[mine])
            .unwrap()
            .is_empty()
    );
    store
        .delete_scheduled_outbound_attachments(acct, row.id, &[mine])
        .unwrap();
    assert_eq!(claims(&store, "cid-mine"), None, "consumed by its own send");
}

#[test]
fn cancel_releases_files_and_a_sending_row_refuses() {
    let (store, acct) = store();
    let f = stage(&store, acct, "cid-f");
    let row = store
        .schedule_send(
            acct,
            r#"{"body":"x"}"#,
            t(60),
            "<c@passband>",
            &[f],
            None,
            t(0),
        )
        .unwrap();

    // Released days later: the unclaimed clock restarts at the cancel, so the
    // reopened composer's autosave finds the file instead of sweeping it.
    let later = t(0) + chrono::Duration::days(3);
    let CancelOutcome::Cancelled(got) = store.cancel_scheduled_send(acct, row.id, later).unwrap()
    else {
        panic!("a scheduled row cancels");
    };
    assert_eq!(got.state, ScheduledSendState::Cancelled);
    assert_eq!(got.request, r#"{"body":"x"}"#);
    assert_eq!(got.attachments.len(), 1, "the files it held, for the tray");
    assert_eq!(claims(&store, "cid-f"), Some((None, None)), "unclaimed");
    let d = new_draft(&store, acct);
    store
        .claim_outbound_attachments(acct, d, &[f], later + chrono::Duration::minutes(1))
        .unwrap();
    assert_eq!(claims(&store, "cid-f"), Some((Some(d), None)), "re-claimed");
    assert_eq!(
        store.cancel_scheduled_send(acct, row.id, later).unwrap(),
        CancelOutcome::NotFound,
        "already cancelled"
    );

    // Sending: refused, and nothing changes.
    let g = stage(&store, acct, "cid-g");
    let s = store
        .schedule_send(acct, "{}", t(60), "<g@passband>", &[g], None, t(0))
        .unwrap();
    let claimed = store
        .claim_due_scheduled_send(acct, t(60), LEASE)
        .unwrap()
        .unwrap();
    assert_eq!(
        store.cancel_scheduled_send(acct, s.id, t(61)).unwrap(),
        CancelOutcome::Sending
    );
    assert_eq!(
        store.scheduled_send_by_id(acct, s.id).unwrap().unwrap(),
        claimed
    );
    assert_eq!(claims(&store, "cid-g"), Some((None, Some(s.id))));

    // A failed row cancels too.
    store
        .settle_scheduled_failed(acct, s.id, "http 500", t(62))
        .unwrap();
    assert!(matches!(
        store.cancel_scheduled_send(acct, s.id, t(63)).unwrap(),
        CancelOutcome::Cancelled(_)
    ));
    assert_eq!(
        store.cancel_scheduled_send(acct, 999_999, t(63)).unwrap(),
        CancelOutcome::NotFound
    );
}

#[test]
fn sweep_takes_only_week_old_settled_rows_and_their_files() {
    let (store, acct) = store();
    let old_sent = schedule(&store, acct, "old-sent", t(0));
    store
        .claim_due_scheduled_send(acct, t(0), LEASE)
        .unwrap()
        .unwrap();
    store
        .settle_scheduled_sent(acct, old_sent.id, None, None, t(1))
        .unwrap();
    let old_cancel = schedule(&store, acct, "old-cancel", t(500));
    store
        .cancel_scheduled_send(acct, old_cancel.id, t(1))
        .unwrap();
    // A settled row that somehow still holds a file: the sweep's belt.
    let stuck = stage(&store, acct, "cid-stuck");
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE outbound_attachments SET scheduled_send_id = ?1 WHERE id = ?2",
            params![old_sent.id, stuck],
        )
        .unwrap();
    let recent_cancel = schedule(&store, acct, "recent", t(500));
    let week = chrono::Duration::days(7);
    store
        .cancel_scheduled_send(acct, recent_cancel.id, t(1) + week)
        .unwrap();
    let waiting = schedule(&store, acct, "waiting", t(0) + week * 3);
    let live = stage(&store, acct, "cid-live");
    store
        .claim_attachments_for_scheduled_send(acct, waiting.id, &[live], None, t(0))
        .unwrap();

    let n = store.sweep_scheduled_sends(acct, t(2) + week).unwrap();
    assert_eq!(n, 2);
    let left: Vec<String> = store
        .lock()
        .unwrap()
        .prepare("SELECT request FROM scheduled_sends ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(left.len(), 2);
    assert!(left[0].contains("recent") && left[1].contains("waiting"));
    assert_eq!(claims(&store, "cid-stuck"), None, "went with its row");
    assert_eq!(claims(&store, "cid-live"), Some((None, Some(waiting.id))));
}

#[test]
fn the_attachment_sweep_takes_a_file_whose_scheduled_send_is_gone() {
    let (store, acct) = store();
    let live_row = schedule(&store, acct, "live", t(60));
    let live = stage(&store, acct, "cid-live");
    store
        .claim_attachments_for_scheduled_send(acct, live_row.id, &[live], None, t(0))
        .unwrap();
    // Pinned to an id no scheduled send will ever get, not to a deleted row's:
    // `scheduled_sends.id` is a bare rowid and the next insert would reuse it.
    let orphan = stage(&store, acct, "cid-orphan");
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE outbound_attachments SET scheduled_send_id = 999999 WHERE id = ?1",
            params![orphan],
        )
        .unwrap();
    // A fresh upload runs the sweep (nothing is stale by age).
    stage(&store, acct, "cid-fresh");
    assert_eq!(claims(&store, "cid-orphan"), None, "its referent is gone");
    assert_eq!(claims(&store, "cid-live"), Some((None, Some(live_row.id))));
    assert_eq!(
        claims(&store, "cid-fresh"),
        Some((None, None)),
        "the arm's IS NOT NULL keeps an unclaimed row from reading as an orphan"
    );
}

#[test]
fn every_outbox_query_is_account_scoped() {
    let (store, acct) = store();
    let other = store.ensure_account("other@example.com").unwrap();
    let f = stage(&store, acct, "cid-f");
    let row = store
        .schedule_send(acct, "{}", t(0), "<a@passband>", &[f], None, t(0))
        .unwrap();

    assert!(store.scheduled_send_by_id(other, row.id).unwrap().is_none());
    assert!(store.list_scheduled_sends(other).unwrap().is_empty());
    assert!(
        store
            .scheduled_send_attachments(other, row.id)
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .scheduled_outbound_attachments(other, row.id, &[f])
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .claim_due_scheduled_send(other, t(10), LEASE)
            .unwrap()
            .is_none()
    );
    assert!(!store.send_scheduled_now(other, row.id, t(1)).unwrap());
    assert_eq!(
        store.cancel_scheduled_send(other, row.id, t(1)).unwrap(),
        CancelOutcome::NotFound
    );
    // Another account's scheduling cannot take this account's file.
    let theirs = schedule(&store, other, "theirs", t(0));
    assert!(
        store
            .claim_attachments_for_scheduled_send(other, theirs.id, &[f], None, t(0))
            .is_err()
    );
    store
        .release_scheduled_send_attachments(other, row.id, t(1))
        .unwrap();
    assert_eq!(claims(&store, "cid-f"), Some((None, Some(row.id))));

    store
        .claim_due_scheduled_send(acct, t(10), LEASE)
        .unwrap()
        .unwrap();
    assert!(
        store
            .expired_sending_scheduled_sends(other, t(99))
            .unwrap()
            .is_empty()
    );
    assert!(!store.requeue_scheduled_send(other, row.id, t(99)).unwrap());
    assert!(
        !store
            .settle_scheduled_retry(other, row.id, t(99), "x", t(99))
            .unwrap()
    );
    assert!(
        !store
            .settle_scheduled_failed(other, row.id, "x", t(99))
            .unwrap()
    );
    assert!(
        !store
            .settle_scheduled_sent(other, row.id, None, None, t(99))
            .unwrap()
    );
    store
        .settle_scheduled_sent(acct, row.id, None, None, t(99))
        .unwrap();
    assert_eq!(
        store
            .sweep_scheduled_sends(other, t(99) + chrono::Duration::days(30))
            .unwrap(),
        0
    );
    assert!(store.scheduled_send_by_id(acct, row.id).unwrap().is_some());
}

#[test]
fn an_older_db_gains_the_scheduled_claim_column_and_reopens_idempotently() {
    // An install from before scheduled send: `outbound_attachments` with only
    // the draft claim and one staged file. Through `init` (schema.sql THEN
    // migrate) so a schema.sql index on the new column would fail right here.
    //
    // A FILE, not memory: the second `init` below reopens the same DB.
    register_vec_extension();
    let dir = std::env::temp_dir().join(format!("squelch-scheduled-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("scheduled.db");
    let _ = std::fs::remove_file(&path);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE outbound_attachments (
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL, draft_id INTEGER,
             filename TEXT NOT NULL, mime TEXT NOT NULL, size_bytes INTEGER NOT NULL,
             content_id TEXT NOT NULL, data BLOB NOT NULL, created_at TEXT NOT NULL);
         INSERT INTO outbound_attachments(account_id, filename, mime, size_bytes,
             content_id, data, created_at)
             VALUES (1, 'a.txt', 'text/plain', 1, 'cid-old', x'61',
                     '2026-07-01T00:00:00+00:00');",
    )
    .unwrap();
    let store = SqliteStore::init(conn).unwrap();
    let acct = store.ensure_account("me@example.com").unwrap();
    assert_eq!(acct, 1, "the fixture's row belongs to the first account");
    assert_eq!(
        claims(&store, "cid-old"),
        Some((None, None)),
        "not backfilled"
    );
    let has_index: bool = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_outbound_attachments_scheduled')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(has_index);
    // The whole path works on the upgraded file.
    let old_id: i64 = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT id FROM outbound_attachments WHERE content_id = 'cid-old'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let row = store
        .schedule_send(acct, "{}", t(0), "<m@passband>", &[old_id], None, t(0))
        .unwrap();
    assert_eq!(claims(&store, "cid-old"), Some((None, Some(row.id))));
    migrate(&store.lock().unwrap()).unwrap();
    drop(store);
    let store = SqliteStore::init(Connection::open(&path).unwrap()).unwrap();
    assert_eq!(store.list_scheduled_sends(acct).unwrap().len(), 1);
    assert_eq!(claims(&store, "cid-old"), Some((None, Some(row.id))));
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn send_now_on_a_pending_retry_keeps_its_budget() {
    let (store, acct) = store();
    let r = schedule(&store, acct, "retry", t(1));
    store
        .claim_due_scheduled_send(acct, t(1), LEASE)
        .unwrap()
        .unwrap();
    assert!(
        store
            .settle_scheduled_retry(acct, r.id, t(3), "failed:gmail", t(1))
            .unwrap()
    );
    // Pulling a pending retry forward is not a new request: attempts and the
    // reason stay, only the clock moves.
    assert!(store.send_scheduled_now(acct, r.id, t(2)).unwrap());
    let row = store.scheduled_send_by_id(acct, r.id).unwrap().unwrap();
    assert_eq!(
        (
            row.state,
            row.send_at,
            row.attempts,
            row.last_error.as_deref()
        ),
        (ScheduledSendState::Scheduled, t(2), 1, Some("failed:gmail"))
    );
}

#[test]
fn the_outbox_listing_carries_the_parent_context_drafts_carry() {
    let (store, acct) = store();
    let parent = triaged(acct, "g1", "t1").ingest(&store);
    let other = store.ensure_account("other@example.com").unwrap();
    let theirs = triaged(other, "g2", "t2").ingest(&store);

    let reply = store
        .insert_scheduled_send(
            acct,
            &format!(r#"{{"reply_to_message_id":{parent},"body":"yes"}}"#),
            t(3),
            "<r@passband>",
            t(0),
        )
        .unwrap();
    let fwd = store
        .insert_scheduled_send(
            acct,
            &format!(r#"{{"forward_of_message_id":{parent},"to":"b@x","body":""}}"#),
            t(2),
            "<f@passband>",
            t(0),
        )
        .unwrap();
    // An id that exists only on another account borrows nothing.
    let borrowed = store
        .insert_scheduled_send(
            acct,
            &format!(r#"{{"reply_to_message_id":{theirs},"body":"x"}}"#),
            t(4),
            "<b@passband>",
            t(0),
        )
        .unwrap();
    let cold = schedule(&store, acct, "cold", t(1));
    // A corrupt request must not take the whole listing down with it.
    let corrupt = store
        .insert_scheduled_send(acct, "not json {", t(5), "<c@passband>", t(0))
        .unwrap();
    // Settled rows are not listed, exactly as `list_scheduled_sends`.
    let gone = schedule(&store, acct, "gone", t(0));
    assert!(matches!(
        store.cancel_scheduled_send(acct, gone.id, t(0)).unwrap(),
        CancelOutcome::Cancelled(_)
    ));

    let listed = store.list_scheduled_sends_with_parent(acct).unwrap();
    let ids: Vec<i64> = listed.iter().map(|(s, _)| s.id).collect();
    assert_eq!(
        ids,
        vec![cold.id, fwd.id, reply.id, borrowed.id, corrupt.id]
    );
    assert_eq!(
        ids,
        store
            .list_scheduled_sends(acct)
            .unwrap()
            .iter()
            .map(|s| s.id)
            .collect::<Vec<_>>(),
        "same rows, same order as the plain listing"
    );
    let parent_ctx = ParentContext {
        thread_id: Some("t1".into()),
        subject: Some("Lunch?".into()),
        from_addr: Some("alice@example.com".into()),
        from_name: Some("Alice".into()),
    };
    assert_eq!(listed[0].1, ParentContext::default(), "new mail has none");
    assert_eq!(listed[1].1, parent_ctx, "a forward's original");
    assert_eq!(listed[2].1, parent_ctx, "a reply's parent");
    assert_eq!(listed[3].1, ParentContext::default());
    assert_eq!(listed[4].1, ParentContext::default());
}
