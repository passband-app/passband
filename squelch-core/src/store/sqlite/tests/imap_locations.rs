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
