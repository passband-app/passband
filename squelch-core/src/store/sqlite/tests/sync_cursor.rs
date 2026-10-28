//! The account's mail provider and the opaque sync cursor, including the
//! adoption of the legacy Gmail historyId row in both directions.

use super::super::sync_cursor::LEGACY_GMAIL_CURSOR_KEY;
use super::super::*;
use super::support::*;
use crate::mail::{MailProvider, SyncCursor};

fn legacy(store: &SqliteStore, acct: AccountId) -> Option<u64> {
    store
        .sync_state(acct, LEGACY_GMAIL_CURSOR_KEY)
        .unwrap()
        .map(|s| s.last_uid)
}

fn set_legacy(store: &SqliteStore, acct: AccountId, history_id: u64) {
    store
        .set_sync_state(
            acct,
            LEGACY_GMAIL_CURSOR_KEY,
            &SyncState {
                uidvalidity: 0,
                last_uid: history_id,
            },
        )
        .unwrap();
}

fn cursor(store: &SqliteStore, acct: AccountId) -> Option<String> {
    store
        .sync_cursor(acct)
        .unwrap()
        .map(|c| c.as_str().to_string())
}

#[test]
fn every_account_is_gmail_unless_its_row_says_otherwise() {
    let (store, acct) = store();
    assert_eq!(store.account_provider(acct).unwrap(), MailProvider::Gmail);
}

#[test]
fn an_unknown_account_has_no_provider() {
    let (store, _) = store();
    assert!(matches!(
        store.account_provider(9_999),
        Err(CoreError::NotFound)
    ));
}

/// A database a newer build wrote must not have its account read as Gmail by
/// this one.
#[test]
fn a_provider_this_build_does_not_know_is_an_error_not_gmail() {
    let (store, acct) = store();
    store.execute_batch_for_test(&format!(
        "UPDATE accounts SET provider = 'imap' WHERE id = {acct}"
    ));
    assert!(matches!(
        store.account_provider(acct),
        Err(CoreError::InvalidInput(_))
    ));
}

#[test]
fn a_first_run_has_no_cursor() {
    let (store, acct) = store();
    assert_eq!(cursor(&store, acct), None);
}

/// AN UPGRADED INSTALL has only the legacy row, and must resume its feed from
/// it rather than read as a first run and re-backfill the window.
#[test]
fn an_upgraded_gmail_account_adopts_its_legacy_history_id() {
    let (store, acct) = store();
    let big = (u32::MAX as u64) + 123_456;
    set_legacy(&store, acct, big);
    assert_eq!(cursor(&store, acct), Some(big.to_string()));
}

/// The legacy row's zero is a cursor that exists, which is not the same as no
/// cursor: the engine treats it as "not a first run, but catch up".
#[test]
fn a_legacy_zero_is_adopted_as_a_cursor_not_as_a_first_run() {
    let (store, acct) = store();
    set_legacy(&store, acct, 0);
    assert_eq!(cursor(&store, acct), Some("0".into()));
}

/// A ROLLED-BACK BUILD reads only the legacy row, so a write must land there
/// too, or the old build walks history from wherever it last stopped.
#[test]
fn a_gmail_cursor_write_is_mirrored_into_the_legacy_row() {
    let (store, acct) = store();
    store
        .set_sync_cursor(acct, &SyncCursor::new("4242"))
        .unwrap();
    assert_eq!(cursor(&store, acct), Some("4242".into()));
    assert_eq!(legacy(&store, acct), Some(4242));
    // And the stats page, which reads the legacy row, agrees.
    let stats = store.stats(acct, Utc::now()).unwrap();
    assert_eq!(stats.last_history_id, Some(4242));
}

/// THE LEGACY ROW IS THE CURSOR, whichever way it moved: a rolled-back build
/// that kept polling advanced it, and an operator lowered it to replay history
/// or zeroed it to force a catch-up. Both are honoured, as they always were.
#[test]
fn the_legacy_row_is_read_as_it_stands_whichever_way_it_moved() {
    let (store, acct) = store();
    store
        .set_sync_cursor(acct, &SyncCursor::new("100"))
        .unwrap();
    set_legacy(&store, acct, 250);
    assert_eq!(cursor(&store, acct), Some("250".into()), "advanced");
    set_legacy(&store, acct, 50);
    assert_eq!(cursor(&store, acct), Some("50".into()), "rewound by hand");
    set_legacy(&store, acct, 0);
    assert_eq!(cursor(&store, acct), Some("0".into()), "zeroed: a catch-up");
}

#[test]
fn a_cursor_for_an_unknown_account_is_refused() {
    let (store, _) = store();
    assert!(matches!(
        store.set_sync_cursor(9_999, &SyncCursor::new("1")),
        Err(CoreError::NotFound)
    ));
    assert_eq!(
        store.query_i64_for_test("SELECT COUNT(*) FROM sync_cursor"),
        Some(0)
    );
}

/// The adoption is Gmail's alone. Another provider's cursor is opaque: never
/// parsed, never mirrored, never mixed with a stray legacy row.
#[test]
fn another_providers_cursor_is_never_parsed_or_mirrored() {
    let (store, acct) = store();
    store.execute_batch_for_test(&format!(
        "UPDATE accounts SET provider = 'imap' WHERE id = {acct}"
    ));
    set_legacy(&store, acct, 999);
    assert_eq!(cursor(&store, acct), None, "a legacy row is Gmail's only");
    store.set_sync_cursor(acct, &SyncCursor::new("7")).unwrap();
    assert_eq!(cursor(&store, acct), Some("7".into()));
    assert_eq!(legacy(&store, acct), Some(999), "not mirrored");
}

/// A Gmail cursor IS a historyId. Anything else could not be mirrored, and
/// would read back as the legacy row's value or as no cursor at all: so it is
/// refused, and nothing is written.
#[test]
fn a_gmail_cursor_that_is_not_a_history_id_is_refused_and_nothing_written() {
    let (store, acct) = store();
    set_legacy(&store, acct, 10);
    assert!(matches!(
        store.set_sync_cursor(acct, &SyncCursor::new("not-a-number")),
        Err(CoreError::InvalidInput(_))
    ));
    assert_eq!(cursor(&store, acct), Some("10".into()));
    assert_eq!(
        store.query_i64_for_test("SELECT COUNT(*) FROM sync_cursor"),
        Some(0)
    );
}

/// THE RE-INGEST DRILL (docs/SECURITY.md §1) still works: deleting the legacy
/// row is no cursor, whatever the new table holds, so the next start runs a
/// first backfill and re-fetches the window. A sanitizer fix reaches stored
/// mail this way.
#[test]
fn deleting_the_legacy_row_is_still_the_re_ingest_drill() {
    let (store, acct) = store();
    store
        .set_sync_cursor(acct, &SyncCursor::new("500"))
        .unwrap();
    store.execute_batch_for_test(&format!(
        "DELETE FROM sync_state WHERE account_id = {acct} AND mailbox = 'history'"
    ));
    assert_eq!(cursor(&store, acct), None);
    // And the first backfill's write brings both rows back.
    store
        .set_sync_cursor(acct, &SyncCursor::new("600"))
        .unwrap();
    assert_eq!(cursor(&store, acct), Some("600".into()));
    assert_eq!(legacy(&store, acct), Some(600));
}

/// A value past i64::MAX survives the legacy column's i64 cast both ways, as
/// it always did through `sync_state`.
#[test]
fn a_history_id_past_i64_max_round_trips() {
    let (store, acct) = store();
    let huge = u64::MAX - 7;
    store
        .set_sync_cursor(acct, &SyncCursor::new(huge.to_string()))
        .unwrap();
    assert_eq!(cursor(&store, acct), Some(huge.to_string()));
    assert_eq!(legacy(&store, acct), Some(huge));
}

#[test]
fn cursors_are_per_account() {
    let (store, a) = store();
    let b = store.ensure_account("other@example.com").unwrap();
    store.set_sync_cursor(a, &SyncCursor::new("1")).unwrap();
    store.set_sync_cursor(b, &SyncCursor::new("2")).unwrap();
    store.set_sync_cursor(a, &SyncCursor::new("3")).unwrap();
    assert_eq!(cursor(&store, a), Some("3".into()));
    assert_eq!(cursor(&store, b), Some("2".into()));
    assert_eq!(legacy(&store, b), Some(2));
}

#[test]
fn has_message_answers_by_provider_id_and_account() {
    let (store, acct) = store();
    let other = store.ensure_account("other@example.com").unwrap();
    triaged(acct, "g-1", "t-1").ingest(&store);
    assert!(store.has_message(acct, "g-1").unwrap());
    assert!(!store.has_message(acct, "g-2").unwrap());
    assert!(!store.has_message(other, "g-1").unwrap());
}

#[test]
fn a_fresh_account_takes_its_configured_provider() {
    let (store, acct) = store();
    store
        .bind_account_provider(acct, MailProvider::Outlook)
        .unwrap();
    assert_eq!(store.account_provider(acct).unwrap(), MailProvider::Outlook);
    // Idempotent once bound.
    store
        .bind_account_provider(acct, MailProvider::Outlook)
        .unwrap();
}

#[test]
fn a_gmail_account_that_ever_synced_never_turns_outlook() {
    // Each of the three traces of a sync refuses the flip on its own.
    let with_message = || {
        let (store, acct) = store();
        triaged(acct, "g-1", "t-1").ingest(&store);
        (store, acct)
    };
    let with_cursor = || {
        let (store, acct) = store();
        store.set_sync_cursor(acct, &SyncCursor::new("42")).unwrap();
        (store, acct)
    };
    let with_legacy = || {
        let (store, acct) = store();
        set_legacy(&store, acct, 42);
        (store, acct)
    };
    for (store, acct) in [with_message(), with_cursor(), with_legacy()] {
        assert!(matches!(
            store.bind_account_provider(acct, MailProvider::Outlook),
            Err(CoreError::InvalidInput(_))
        ));
        assert_eq!(store.account_provider(acct).unwrap(), MailProvider::Gmail);
    }
}

#[test]
fn an_outlook_account_is_never_rebound_to_gmail() {
    let (store, acct) = store();
    store
        .bind_account_provider(acct, MailProvider::Outlook)
        .unwrap();
    // Even untouched: only the column default may move, never a bound value.
    assert!(
        store
            .bind_account_provider(acct, MailProvider::Gmail)
            .is_err()
    );
    assert_eq!(store.account_provider(acct).unwrap(), MailProvider::Outlook);
}

/// An Outlook cursor is opaque text in `sync_cursor` and never touches the
/// legacy Gmail row.
#[test]
fn an_outlook_cursor_lives_only_in_sync_cursor() {
    let (store, acct) = store();
    store
        .bind_account_provider(acct, MailProvider::Outlook)
        .unwrap();
    let c = SyncCursor::new(r#"{"v":1,"inbox":"a","sent":"b"}"#);
    store.set_sync_cursor(acct, &c).unwrap();
    assert_eq!(store.sync_cursor(acct).unwrap(), Some(c));
    assert_eq!(legacy(&store, acct), None);
}

/// A stored spam or archive-tier row is not "stored" to the incremental skip:
/// a move into the inbox must re-ingest it so the upsert can heal the row.
#[test]
fn has_message_reads_spam_and_archive_rows_as_absent() {
    let (store, acct) = store();
    triaged(acct, "s-1", "t-1").ingest(&store);
    triaged(acct, "a-1", "t-2").ingest(&store);
    {
        let conn = store.lock().unwrap();
        conn.execute(
            "UPDATE messages SET is_spam = 1 WHERE gmail_msg_id = 's-1'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE messages SET is_archive = 1 WHERE gmail_msg_id = 'a-1'",
            [],
        )
        .unwrap();
    }
    assert!(!store.has_message(acct, "s-1").unwrap());
    assert!(!store.has_message(acct, "a-1").unwrap());
}
