//! Outbox (`scheduled_sends`) store tests: the state machine, the lease, and
//! the second attachment claim.

use super::super::migrate::migrate;
use super::super::*;
use super::support::*;
use crate::store::{
    ATTACHMENT_MISSING, CancelOutcome, MAX_SCHEDULED_PER_ACCOUNT, ParentContext,
    SCHEDULED_MAX_ATTEMPTS, ScheduledSend, ScheduledSendState, SearchClock, TOO_MANY_SCHEDULED,
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
            .expired_sending_scheduled_sends(acct, now, now)
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
            .settle_scheduled_retry(acct, r.id, 0, t(5), "http 500", false, t(1))
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
            .settle_scheduled_retry(acct, r.id, 1, t(2), "http 500", false, t(0))
            .unwrap()
    );
    let row = store.scheduled_send_by_id(acct, r.id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Scheduled);
    assert_eq!(row.send_at, t(2));
    assert_eq!(row.lease_until, None);
    assert_eq!(row.last_error.as_deref(), Some("http 500"));
    assert_eq!(row.attempts, 1);
    assert!(
        !row.maybe_sent,
        "a definitely-not-sent failure leaves the flag"
    );

    // Requeue: sending -> scheduled at the time given, attempts untouched, and
    // the row marked MAYBE SENT so its re-fire asks Gmail first.
    store
        .claim_due_scheduled_send(acct, t(2), LEASE)
        .unwrap()
        .unwrap();
    assert!(
        !store
            .requeue_scheduled_send(acct, r.id, 1, t(14), t(9))
            .unwrap(),
        "a settle naming an older claim is a no-op"
    );
    assert!(
        store
            .requeue_scheduled_send(acct, r.id, 2, t(14), t(9))
            .unwrap()
    );
    let row = store.scheduled_send_by_id(acct, r.id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Scheduled);
    assert_eq!(
        (row.attempts, row.send_at, row.lease_until, row.maybe_sent),
        (2, t(14), None, true)
    );

    // Failed.
    let claimed = store
        .claim_due_scheduled_send(acct, t(14), LEASE)
        .unwrap()
        .unwrap();
    assert!(claimed.maybe_sent, "the flag is sticky across claims");
    assert!(
        store
            .settle_scheduled_failed(acct, r.id, 3, "no write credential", false, None, t(14))
            .unwrap()
    );
    let row = store.scheduled_send_by_id(acct, r.id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Failed);
    assert_eq!(row.attempts, 3);
    assert_eq!(row.last_error.as_deref(), Some("no write credential"));
    assert!(
        !store
            .requeue_scheduled_send(acct, r.id, 3, t(9), t(9))
            .unwrap(),
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
            .settle_scheduled_sent(acct, r.id, 1, Some(42), Some("thr"), t(21))
            .unwrap()
    );
    let row = store.scheduled_send_by_id(acct, r.id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Sent);
    assert_eq!(row.sent_message_id, Some(42));
    assert_eq!(row.sent_thread_id.as_deref(), Some("thr"));
    assert_eq!(row.lease_until, None);
    assert!(
        !store
            .settle_scheduled_sent(acct, r.id, 1, None, None, t(22))
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
        .settle_scheduled_failed(acct, s.id, 1, "http 500", false, None, t(62))
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
        .settle_scheduled_sent(acct, old_sent.id, 1, None, None, t(1))
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
            .expired_sending_scheduled_sends(other, t(99), t(99))
            .unwrap()
            .is_empty()
    );
    assert!(
        !store
            .requeue_scheduled_send(other, row.id, 1, t(99), t(99))
            .unwrap()
    );
    assert!(
        !store
            .settle_scheduled_retry(other, row.id, 1, t(99), "x", false, t(99))
            .unwrap()
    );
    assert!(
        !store
            .unclaim_scheduled_send(other, row.id, 1, t(99), "x", SearchClock::Reset, t(99))
            .unwrap()
    );
    assert!(
        !store
            .defer_scheduled_recovery(other, row.id, 1, t(99), "x", t(99))
            .unwrap()
    );
    assert!(
        !store
            .settle_scheduled_failed(other, row.id, 1, "x", false, None, t(99))
            .unwrap()
    );
    assert!(
        !store
            .settle_scheduled_sent(other, row.id, 1, None, None, t(99))
            .unwrap()
    );
    store
        .settle_scheduled_sent(acct, row.id, 1, None, None, t(99))
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
            .settle_scheduled_retry(acct, r.id, 1, t(3), "failed:gmail", false, t(1))
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

#[test]
fn a_settle_from_an_older_claim_is_a_no_op() {
    let (store, acct) = store();
    let r = schedule(&store, acct, "stale", t(0));
    let first = store
        .claim_due_scheduled_send(acct, t(0), LEASE)
        .unwrap()
        .unwrap();
    assert_eq!(first.attempts, 1);
    // The first worker's lease lapsed; recovery requeued and a second claim
    // took the row.
    assert!(
        store
            .requeue_scheduled_send(acct, r.id, 1, t(5), t(5))
            .unwrap()
    );
    let second = store
        .claim_due_scheduled_send(acct, t(5), LEASE)
        .unwrap()
        .unwrap();
    assert_eq!(second.attempts, 2);
    // The first worker wakes up and settles: every settle names ITS claim.
    for stale in [
        store
            .settle_scheduled_sent(acct, r.id, 1, Some(1), None, t(6))
            .unwrap(),
        store
            .settle_scheduled_retry(acct, r.id, 1, t(9), "failed:gmail", false, t(6))
            .unwrap(),
        store
            .settle_scheduled_failed(acct, r.id, 1, "failed:gmail", false, None, t(6))
            .unwrap(),
        store
            .unclaim_scheduled_send(acct, r.id, 1, t(9), "paced", SearchClock::Reset, t(6))
            .unwrap(),
        store
            .defer_scheduled_recovery(acct, r.id, 1, t(9), "x", t(6))
            .unwrap(),
    ] {
        assert!(!stale, "a stale settle moved the row");
    }
    assert_eq!(
        store.scheduled_send_by_id(acct, r.id).unwrap().unwrap(),
        second,
        "untouched"
    );
    assert!(
        store
            .settle_scheduled_sent(acct, r.id, 2, None, None, t(6))
            .unwrap()
    );
}

#[test]
fn the_claim_never_takes_a_row_out_of_attempts() {
    let (store, acct) = store();
    let r = schedule(&store, acct, "spent", t(0));
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE scheduled_sends SET attempts = ?1 WHERE id = ?2",
            params![SCHEDULED_MAX_ATTEMPTS, r.id],
        )
        .unwrap();
    assert!(
        store
            .claim_due_scheduled_send(acct, t(10), LEASE)
            .unwrap()
            .is_none()
    );
    let ok = schedule(&store, acct, "fresh", t(1));
    assert_eq!(
        store
            .claim_due_scheduled_send(acct, t(10), LEASE)
            .unwrap()
            .unwrap()
            .id,
        ok.id,
        "the spent row does not block the next one"
    );
}

#[test]
fn the_lease_is_sized_from_the_claimed_row_and_its_files() {
    let (store, acct) = store();
    let big = store
        .stage_outbound_attachment(acct, "big", "text/plain", "cid-big", &[b'x'; 3000], t(0))
        .unwrap()
        .id;
    let row = store
        .schedule_send(acct, "{}", t(0), "<l@passband>", &[big], None, t(0))
        .unwrap();
    let got = store
        .claim_due_scheduled_send_with(acct, t(1), |r| {
            let bytes: i64 = r.attachments.iter().map(|a| a.size_bytes).sum();
            chrono::Duration::seconds(bytes)
        })
        .unwrap()
        .unwrap();
    assert_eq!(got.id, row.id);
    assert_eq!(got.attachments.len(), 1, "the files ride on the claim");
    assert_eq!(
        got.lease_until,
        Some(t(1) + chrono::Duration::seconds(3000))
    );
    // A non-positive lease is refused and claims nothing.
    let other = schedule(&store, acct, "zero", t(0));
    assert!(
        store
            .claim_due_scheduled_send_with(acct, t(1), |_| chrono::Duration::zero())
            .is_err()
    );
    assert_eq!(
        store
            .scheduled_send_by_id(acct, other.id)
            .unwrap()
            .unwrap()
            .state,
        ScheduledSendState::Scheduled
    );
}

#[test]
fn recovery_waits_out_a_grace_and_a_deferral() {
    let (store, acct) = store();
    let r = schedule(&store, acct, "grace", t(0));
    store
        .claim_due_scheduled_send(acct, t(0), LEASE)
        .unwrap()
        .unwrap();
    let lapse = t(0) + LEASE;
    let ids = |lapsed_before, now| -> Vec<i64> {
        store
            .expired_sending_scheduled_sends(acct, lapsed_before, now)
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect()
    };
    // The caller's grace: a lease that lapsed after `lapsed_before` waits.
    assert!(ids(lapse - chrono::Duration::seconds(1), lapse).is_empty());
    assert_eq!(ids(lapse, lapse), vec![r.id]);

    // A search that could not be made defers the next one; the lease stays.
    assert!(
        store
            .defer_scheduled_recovery(acct, r.id, 1, t(10), "recovery:unreachable", t(5))
            .unwrap()
    );
    let row = store.scheduled_send_by_id(acct, r.id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Sending);
    assert_eq!(
        row.lease_until,
        Some(lapse),
        "the give-up clock runs from it"
    );
    assert_eq!(row.last_error.as_deref(), Some("recovery:unreachable"));
    assert!(ids(t(9), t(9)).is_empty(), "deferred");
    assert_eq!(ids(t(10), t(10)), vec![r.id]);
}

#[test]
fn an_attempt_that_never_started_does_not_count() {
    let (store, acct) = store();
    let r = schedule(&store, acct, "paced", t(0));
    store
        .claim_due_scheduled_send(acct, t(0), LEASE)
        .unwrap()
        .unwrap();
    assert!(
        store
            .unclaim_scheduled_send(acct, r.id, 1, t(2), "paced", SearchClock::Reset, t(0))
            .unwrap()
    );
    let row = store.scheduled_send_by_id(acct, r.id).unwrap().unwrap();
    assert_eq!(
        (
            row.state,
            row.attempts,
            row.send_at,
            row.last_error.as_deref()
        ),
        (ScheduledSendState::Scheduled, 0, t(2), Some("paced"))
    );
}

#[test]
fn the_sweep_takes_month_old_failed_rows_and_released_leftovers() {
    let (store, acct) = store();
    let f = stage(&store, acct, "cid-failed");
    let failed = store
        .schedule_send(acct, "{}", t(0), "<f@passband>", &[f], None, t(0))
        .unwrap();
    store
        .claim_due_scheduled_send(acct, t(0), LEASE)
        .unwrap()
        .unwrap();
    store
        .settle_scheduled_failed(acct, failed.id, 1, "failed:gmail", false, None, t(1))
        .unwrap();
    // A file a cancel released and nobody re-claimed.
    let g = stage(&store, acct, "cid-released");
    let cancelled = store
        .schedule_send(acct, "{}", t(600), "<c@passband>", &[g], None, t(0))
        .unwrap();
    store
        .cancel_scheduled_send(acct, cancelled.id, t(1))
        .unwrap();

    // A week on: the failed row waits for the user, the leftover is past its
    // day and goes even though no composer ran.
    let week = t(2) + chrono::Duration::days(7);
    store.sweep_scheduled_sends(acct, week).unwrap();
    assert!(
        store
            .scheduled_send_by_id(acct, failed.id)
            .unwrap()
            .is_some()
    );
    assert_eq!(claims(&store, "cid-failed"), Some((None, Some(failed.id))));
    assert_eq!(claims(&store, "cid-released"), None);

    let month = t(2) + chrono::Duration::days(30);
    assert_eq!(store.sweep_scheduled_sends(acct, month).unwrap(), 1);
    assert!(
        store
            .scheduled_send_by_id(acct, failed.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(claims(&store, "cid-failed"), None, "its files went with it");
}

#[test]
fn an_account_holds_at_most_the_cap_of_live_rows() {
    let (store, acct) = store();
    for i in 0..MAX_SCHEDULED_PER_ACCOUNT {
        store
            .schedule_send(
                acct,
                "{}",
                t(60),
                &format!("<{i}@passband>"),
                &[],
                None,
                t(0),
            )
            .unwrap();
    }
    let err = store
        .schedule_send(acct, "{}", t(60), "<over@passband>", &[], None, t(0))
        .unwrap_err();
    assert!(
        matches!(&err, CoreError::InvalidInput(m) if m == TOO_MANY_SCHEDULED),
        "{err:?}"
    );
    // A cancelled row frees its slot; another account has its own.
    let first = store.list_scheduled_sends(acct).unwrap()[0].id;
    store.cancel_scheduled_send(acct, first, t(1)).unwrap();
    store
        .schedule_send(acct, "{}", t(60), "<again@passband>", &[], None, t(1))
        .unwrap();
    let other = store.ensure_account("other@example.com").unwrap();
    store
        .schedule_send(other, "{}", t(60), "<o@passband>", &[], None, t(1))
        .unwrap();
}

#[test]
fn a_scheduled_file_is_never_swapped_or_deleted_behind_the_row() {
    let (store, acct) = store();
    let f = stage(&store, acct, "cid-held");
    let row = store
        .schedule_send(acct, "{}", t(60), "<h@passband>", &[f], None, t(0))
        .unwrap();
    // The compose door cannot delete a file the outbox holds: the same
    // answer as an unknown id. Cancel is the only release.
    assert!(!store.delete_outbound_attachment(acct, f).unwrap());
    assert_eq!(claims(&store, "cid-held"), Some((None, Some(row.id))));

    // Suppose the row's file DID vanish and its id was handed to a fresh,
    // unclaimed upload: the worker's door must not read, size or consume it.
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE outbound_attachments SET scheduled_send_id = NULL, content_id = 'cid-swapped'
             WHERE id = ?1",
            params![f],
        )
        .unwrap();
    assert!(
        store
            .scheduled_outbound_attachments(acct, row.id, &[f])
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .scheduled_outbound_attachment_sizes(acct, row.id, &[f])
            .unwrap()
            .is_empty()
    );
    store
        .delete_scheduled_outbound_attachments(acct, row.id, &[f])
        .unwrap();
    assert_eq!(
        claims(&store, "cid-swapped"),
        Some((None, None)),
        "somebody else's upload survives the fired row's discard"
    );
}

/// TWO DOORS, core side: no store method the agent door or the triage agent
/// reads through (`external_*`, `agent_*`, `thread_view*`,
/// `load_agent_context`) may name the outbox. A structural check over the
/// source text of those functions' bodies, proven non-vacuous by the same
/// extractor finding the outbox in the human door's listing.
#[test]
fn no_agent_door_store_read_names_the_outbox() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/store");
    let mut sources = Vec::new();
    for sub in [dir.clone(), dir.join("sqlite")] {
        for entry in std::fs::read_dir(&sub).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                sources.push(std::fs::read_to_string(&path).unwrap());
            }
        }
    }
    let prefixes = [
        "fn external_",
        "fn agent_",
        "fn thread_view",
        "fn load_agent_context",
    ];
    let mut checked = 0;
    for src in &sources {
        for prefix in prefixes {
            for body in fn_bodies(src, prefix) {
                assert!(
                    !body.contains("scheduled_send"),
                    "an agent-door read names the outbox:\n{body}"
                );
                checked += 1;
            }
        }
    }
    assert!(checked >= 15, "only {checked} agent-door bodies found");
    // The extractor sees what it is looking for.
    let listing: Vec<String> = sources
        .iter()
        .flat_map(|s| fn_bodies(s, "fn list_scheduled_sends"))
        .collect();
    assert!(
        listing.iter().any(|b| b.contains("scheduled_sends")),
        "the extractor cannot see the outbox where it is"
    );
}

/// Every function body (brace-matched from the first `{` after the name) in
/// `src` whose definition starts with `prefix`. Trait declarations ending in
/// `;` before any `{` are skipped.
fn fn_bodies(src: &str, prefix: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = src[from..].find(prefix) {
        let start = from + at;
        from = start + prefix.len();
        let rest = &src[start..];
        let (Some(open), semi) = (rest.find('{'), rest.find(';')) else {
            continue;
        };
        if semi.is_some_and(|s| s < open) {
            continue;
        }
        let mut depth = 0usize;
        let mut end = None;
        for (i, c) in rest[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(open + i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        if let Some(end) = end {
            out.push(rest[..end].to_string());
        }
    }
    out
}

#[test]
fn a_failed_row_remembers_it_may_have_gone_and_when_its_attempt_ended() {
    let (store, acct) = store();
    let r = schedule(&store, acct, "f", t(0));
    store
        .claim_due_scheduled_send(acct, t(0), LEASE)
        .unwrap()
        .unwrap();
    // The claim stamps nothing: the fire needs the PREVIOUS attempt's end.
    assert_eq!(
        store
            .scheduled_send_by_id(acct, r.id)
            .unwrap()
            .unwrap()
            .last_attempt_at,
        None
    );
    assert!(
        store
            .settle_scheduled_failed(acct, r.id, 1, "failed:gmail", true, None, t(3))
            .unwrap()
    );
    let row = store.scheduled_send_by_id(acct, r.id).unwrap().unwrap();
    assert!(row.maybe_sent, "an ambiguous last attempt is recorded");
    assert_eq!(row.last_attempt_at, Some(t(3)));

    // Send-now grants a fresh budget and KEEPS both: the row is still owed a
    // search, a grace after its attempt ended.
    assert!(store.send_scheduled_now(acct, r.id, t(4)).unwrap());
    let row = store.scheduled_send_by_id(acct, r.id).unwrap().unwrap();
    assert_eq!(
        (row.attempts, row.maybe_sent, row.last_attempt_at),
        (0, true, Some(t(3)))
    );

    // A later failure that says "definitely not sent" never clears the flag.
    store
        .claim_due_scheduled_send(acct, t(4), LEASE)
        .unwrap()
        .unwrap();
    store
        .settle_scheduled_failed(acct, r.id, 1, "failed:request", false, None, t(5))
        .unwrap();
    let row = store.scheduled_send_by_id(acct, r.id).unwrap().unwrap();
    assert!(row.maybe_sent, "sticky");
    assert_eq!(row.last_attempt_at, Some(t(5)));

    // A give-up decided long after its attempt (a lapsed lease) stamps that
    // attempt's end, not the moment it gave up.
    assert!(store.send_scheduled_now(acct, r.id, t(6)).unwrap());
    store
        .claim_due_scheduled_send(acct, t(6), LEASE)
        .unwrap()
        .unwrap();
    assert!(
        store
            .settle_scheduled_failed(acct, r.id, 1, "failed:recovery", true, Some(t(7)), t(40))
            .unwrap()
    );
    let row = store.scheduled_send_by_id(acct, r.id).unwrap().unwrap();
    assert_eq!((row.last_attempt_at, row.updated_at), (Some(t(7)), t(40)));
}

#[test]
fn the_pre_fire_search_clock_starts_once_and_stops_when_a_search_answers() {
    let (store, acct) = store();
    let r = schedule(&store, acct, "s", t(0));
    let claim = |at| {
        store
            .claim_due_scheduled_send(acct, at, LEASE)
            .unwrap()
            .unwrap()
    };
    let unclaim = |at, clock| {
        assert!(
            store
                .unclaim_scheduled_send(acct, r.id, 1, at, "recovery:unreachable", clock, at)
                .unwrap()
        );
        store.scheduled_send_by_id(acct, r.id).unwrap().unwrap()
    };
    claim(t(0));
    let row = unclaim(t(1), SearchClock::Start);
    assert_eq!(row.search_deferred_since, Some(t(1)));
    let claimed = claim(t(1));
    assert_eq!(
        claimed.search_deferred_since,
        Some(t(1)),
        "the claim hands the clock to the fire"
    );
    let row = unclaim(t(3), SearchClock::Start);
    assert_eq!(
        row.search_deferred_since,
        Some(t(1)),
        "a running clock keeps its start"
    );
    claim(t(3));
    let row = unclaim(t(5), SearchClock::Keep);
    assert_eq!(row.search_deferred_since, Some(t(1)));
    claim(t(5));
    let row = unclaim(t(7), SearchClock::Reset);
    assert_eq!(row.search_deferred_since, None);

    // A retry settle stops it too, and stamps the attempt's end.
    claim(t(7));
    unclaim(t(8), SearchClock::Start);
    claim(t(8));
    store
        .settle_scheduled_retry(acct, r.id, 1, t(10), "failed:gmail", true, t(9))
        .unwrap();
    let row = store.scheduled_send_by_id(acct, r.id).unwrap().unwrap();
    assert_eq!(
        (row.search_deferred_since, row.last_attempt_at),
        (None, Some(t(9)))
    );
}

#[test]
fn a_requeue_stamps_the_lapsed_leases_end_as_the_attempts_end() {
    let (store, acct) = store();
    let r = schedule(&store, acct, "q", t(0));
    store
        .claim_due_scheduled_send(acct, t(0), LEASE)
        .unwrap()
        .unwrap();
    store
        .requeue_scheduled_send(acct, r.id, 1, t(20), t(15))
        .unwrap();
    let row = store.scheduled_send_by_id(acct, r.id).unwrap().unwrap();
    assert_eq!(row.last_attempt_at, Some(t(0) + LEASE));
    assert_eq!(row.lease_until, None);
}

/// THE STALE-MARKER SWEEP: a marker whose row is live (scheduled, sending,
/// failed) stays; one whose row settled or is gone goes, at most `limit` a
/// call; other settings and other accounts are never touched.
#[test]
fn the_orphan_settings_sweep_keeps_live_rows_markers_only() {
    let (store, acct) = store();
    let key_of = |h: &str| {
        let bare = h.trim().trim_start_matches('<').trim_end_matches('>');
        bare.split('@')
            .next()
            .filter(|t| !t.is_empty())
            .map(|t| format!("mark:{t}"))
    };
    let waiting = schedule(&store, acct, "waiting", t(30));
    let failed = schedule(&store, acct, "failed", t(30));
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE scheduled_sends SET state = 'failed' WHERE id = ?1",
            params![failed.id],
        )
        .unwrap();
    let sent = schedule(&store, acct, "sent", t(30));
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE scheduled_sends SET state = 'sent' WHERE id = ?1",
            params![sent.id],
        )
        .unwrap();
    for k in [
        "mark:waiting",
        "mark:failed",
        "mark:sent",
        "mark:gone1",
        "mark:gone2",
    ] {
        store.set_app_setting(acct, k, "accepted x@y").unwrap();
    }
    store.set_app_setting(acct, "other", "v").unwrap();
    let _ = waiting;

    assert_eq!(
        store
            .sweep_orphan_settings(acct, "mark:", &key_of, 2)
            .unwrap(),
        2
    );
    assert_eq!(
        store
            .sweep_orphan_settings(acct, "mark:", &key_of, 2)
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .sweep_orphan_settings(acct, "mark:", &key_of, 2)
            .unwrap(),
        0
    );
    for (k, kept) in [
        ("mark:waiting", true),
        ("mark:failed", true),
        ("mark:sent", false),
        ("mark:gone1", false),
        ("mark:gone2", false),
        ("other", true),
    ] {
        assert_eq!(
            store.get_app_setting(acct, k).unwrap().is_some(),
            kept,
            "{k}"
        );
    }
}
