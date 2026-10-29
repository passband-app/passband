//! The IMAP source's location table: one row per (message, folder), upserted
//! on sight, forgotten one folder at a time, and private to its account.

use super::super::*;
use super::support::*;
use crate::mail::imap::{MessageLocation, MessageLocations};

fn loc(folder: &str, uidvalidity: u32, uid: u32, mid: Option<&str>) -> MessageLocation {
    MessageLocation {
        folder: folder.into(),
        uidvalidity,
        uid,
        message_id: mid.map(str::to_string),
    }
}

fn folders(store: &SqliteStore, acct: AccountId, id: &str) -> Vec<(String, u32, u32)> {
    let mut v: Vec<_> = store
        .message_locations(acct, id)
        .unwrap()
        .into_iter()
        .map(|l| (l.folder, l.uidvalidity, l.uid))
        .collect();
    v.sort();
    v
}

#[test]
fn a_message_in_two_folders_has_two_rows_and_a_resighting_updates_in_place() {
    let (store, acct) = store();
    assert!(store.message_locations(acct, "imap:a").unwrap().is_empty());
    store
        .record_message_locations(
            acct,
            &[
                ("imap:a".into(), loc("INBOX", 7, 3, Some("a@x.org"))),
                ("imap:a".into(), loc("Sent", 9, 1, Some("a@x.org"))),
                ("imap:b".into(), loc("INBOX", 7, 4, None)),
            ],
        )
        .unwrap();
    assert_eq!(
        folders(&store, acct, "imap:a"),
        vec![("INBOX".into(), 7, 3), ("Sent".into(), 9, 1)]
    );
    // Seen again at a new UID (the folder was rebuilt): the row moves, and a
    // sighting without the Message-ID text does not erase it.
    store
        .record_message_locations(acct, &[("imap:a".into(), loc("INBOX", 8, 11, None))])
        .unwrap();
    let rows = store.message_locations(acct, "imap:a").unwrap();
    let inbox = rows.iter().find(|l| l.folder == "INBOX").unwrap();
    assert_eq!((inbox.uidvalidity, inbox.uid), (8, 11));
    assert_eq!(inbox.message_id.as_deref(), Some("a@x.org"));
    assert_eq!(rows.len(), 2);
}

#[test]
fn forgetting_one_folder_keeps_the_others_and_other_accounts() {
    let (store, acct) = store();
    let other = store.ensure_account("other@example.com").unwrap();
    let both = [
        ("imap:a".to_string(), loc("INBOX", 7, 3, None)),
        ("imap:a".to_string(), loc("Archive", 2, 5, None)),
    ];
    store.record_message_locations(acct, &both).unwrap();
    store.record_message_locations(other, &both[..1]).unwrap();
    store
        .forget_message_location(acct, "imap:a", "INBOX")
        .unwrap();
    assert_eq!(
        folders(&store, acct, "imap:a"),
        vec![("Archive".into(), 2, 5)]
    );
    assert_eq!(
        folders(&store, other, "imap:a"),
        vec![("INBOX".into(), 7, 3)]
    );
    // u32 values above i64's positive half of u32 survive the i64 column.
    store
        .record_message_locations(
            acct,
            &[("imap:z".into(), loc("INBOX", u32::MAX, u32::MAX, None))],
        )
        .unwrap();
    assert_eq!(
        folders(&store, acct, "imap:z"),
        vec![("INBOX".into(), u32::MAX, u32::MAX)]
    );
}

/// An id's content digest (Amendment I) is bound once: a later record with
/// another digest changes nothing, and accounts never see each other's.
#[test]
fn a_content_digest_is_bound_once_per_account() {
    let store = SqliteStore::open_in_memory().unwrap();
    let a = store.ensure_account("a@x.org").unwrap();
    let b = store.ensure_account("b@x.org").unwrap();
    assert_eq!(store.content_digest(a, "imap:1").unwrap(), None);
    store
        .record_content_digest(a, "imap:1", "h1:first")
        .unwrap();
    store
        .record_content_digest(a, "imap:1", "h1:second")
        .unwrap();
    assert_eq!(
        store.content_digest(a, "imap:1").unwrap().as_deref(),
        Some("h1:first")
    );
    assert_eq!(store.content_digest(b, "imap:1").unwrap(), None);
}

/// AMENDMENT J: a body-bound digest an earlier build recorded reads as none
/// (a legacy id) and gives way to the first header digest recorded.
#[test]
fn a_legacy_digest_reads_as_none_and_gives_way_once() {
    let store = SqliteStore::open_in_memory().unwrap();
    let a = store.ensure_account("a@x.org").unwrap();
    store
        .record_content_digest(a, "imap:1", "0123abcd")
        .unwrap();
    assert_eq!(store.content_digest(a, "imap:1").unwrap(), None);
    store.record_content_digest(a, "imap:1", "h1:new").unwrap();
    store
        .record_content_digest(a, "imap:1", "h1:later")
        .unwrap();
    assert_eq!(
        store.content_digest(a, "imap:1").unwrap().as_deref(),
        Some("h1:new")
    );
}

/// AMENDMENT J RULE 1: locations come back in the order their PLACE was
/// recorded, and re-recording the same place keeps its turn.
#[test]
fn locations_come_back_first_recorded_first() {
    use crate::mail::imap::MessageLocation;
    let store = SqliteStore::open_in_memory().unwrap();
    let a = store.ensure_account("a@x.org").unwrap();
    let loc = |folder: &str, uid| MessageLocation {
        folder: folder.into(),
        uidvalidity: 1,
        uid,
        message_id: None,
    };
    store
        .record_message_locations(a, &[("imap:1".into(), loc("Sent", 4))])
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    store
        .record_message_locations(a, &[("imap:1".into(), loc("INBOX", 9))])
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    // Sent seen again at the same place: still first.
    store
        .record_message_locations(a, &[("imap:1".into(), loc("Sent", 4))])
        .unwrap();
    let folders: Vec<String> = store
        .message_locations(a, "imap:1")
        .unwrap()
        .into_iter()
        .map(|l| l.folder)
        .collect();
    assert_eq!(folders, ["Sent", "INBOX"]);
}
