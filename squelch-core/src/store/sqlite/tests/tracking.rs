//! Read-tracking store tests: minted trackers, recorded opens, and the join
//! that gives one message its open history.

use super::super::*;
use super::support::*;
use crate::store::sqlite::tracking::{
    MAX_OPENS_PER_TOKEN, MAX_SCANNER_OPENS_PER_TOKEN, TRACKER_RETENTION_SECS,
};

/// A tracker + the sent message it points at, the shape every tracked send ends
/// up in once its echo has landed.
fn tracked(store: &SqliteStore, acct: AccountId, token: &str) -> i64 {
    let message_id = triaged(acct, "sent-1", "thread-77").seed(store);
    store.insert_send_tracker(acct, token, None, 1_000).unwrap();
    assert!(
        store
            .set_send_tracker_message(acct, token, message_id)
            .unwrap()
    );
    message_id
}

#[test]
fn tracker_and_opens_round_trip_through_the_join() {
    let (store, acct) = store();
    let message_id = tracked(&store, acct, "tok-abc");

    assert!(store.message_opens(acct, message_id).unwrap().is_empty());

    assert!(
        store
            .record_open(acct, "tok-abc", 1_100, Some("Apple Mail/16.0"), "unknown")
            .unwrap()
    );
    assert!(
        store
            .record_open(
                acct,
                "tok-abc",
                1_050,
                Some("… GoogleImageProxy"),
                "proxied"
            )
            .unwrap()
    );
    assert!(
        store
            .record_open(acct, "tok-abc", 1_200, None, "unknown")
            .unwrap()
    );

    let opens = store.message_opens(acct, message_id).unwrap();
    // Oldest first, regardless of insert order, and every column survives.
    assert_eq!(
        opens,
        vec![
            MessageOpen {
                opened_at: 1_050,
                user_agent: Some("… GoogleImageProxy".to_string()),
                classification: "proxied".to_string(),
            },
            MessageOpen {
                opened_at: 1_100,
                user_agent: Some("Apple Mail/16.0".to_string()),
                classification: "unknown".to_string(),
            },
            MessageOpen {
                opened_at: 1_200,
                user_agent: None,
                classification: "unknown".to_string(),
            },
        ]
    );
}

/// THE GUARD: an open can only be recorded against a tracker that exists. A
/// stranger guessing at tokens leaves nothing behind.
#[test]
fn an_unknown_token_records_nothing() {
    let (store, acct) = store();
    let message_id = tracked(&store, acct, "tok-abc");

    assert!(
        !store
            .record_open(acct, "tok-nope", 1_100, None, "unknown")
            .unwrap()
    );
    assert!(!store.record_open(acct, "", 1_100, None, "unknown").unwrap());
    assert!(store.message_opens(acct, message_id).unwrap().is_empty());
}

/// Trackers are account-scoped, so another account's token is as good as an
/// unknown one — for recording AND for reading back.
#[test]
fn trackers_do_not_cross_accounts() {
    let (store, mine) = store();
    let theirs = store.ensure_account("other@example.com").unwrap();
    let message_id = tracked(&store, mine, "tok-abc");

    assert!(
        !store
            .record_open(theirs, "tok-abc", 1_100, None, "unknown")
            .unwrap()
    );
    assert!(
        !store
            .set_send_tracker_message(theirs, "tok-abc", 999)
            .unwrap()
    );
    assert!(store.tracked_message(theirs, "tok-abc").unwrap().is_none());
    assert!(store.message_opens(theirs, message_id).unwrap().is_empty());
}

/// An open recorded before the echo backfilled the message id is kept, and the
/// join picks it up the moment the id lands.
#[test]
fn opens_survive_a_late_message_id_backfill() {
    let (store, acct) = store();
    store
        .insert_send_tracker(acct, "tok-late", None, 1_000)
        .unwrap();
    assert!(
        store
            .record_open(acct, "tok-late", 1_100, None, "unknown")
            .unwrap()
    );
    // Nothing to point a notification at yet.
    assert!(store.tracked_message(acct, "tok-late").unwrap().is_none());

    let message_id = triaged(acct, "sent-late", "thread-9").seed(&store);
    assert!(
        store
            .set_send_tracker_message(acct, "tok-late", message_id)
            .unwrap()
    );

    assert_eq!(store.message_opens(acct, message_id).unwrap().len(), 1);
    let tracked = store.tracked_message(acct, "tok-late").unwrap().unwrap();
    assert_eq!(tracked.message_id, message_id);
    assert_eq!(tracked.thread_id, "thread-9");
}

/// Two sends in the same thread each get their own tracker, and neither's opens
/// bleed into the other's history.
#[test]
fn each_tracker_reports_only_its_own_message() {
    let (store, acct) = store();
    let first = tracked(&store, acct, "tok-1");
    let second = triaged(acct, "sent-2", "thread-77").seed(&store);
    store
        .insert_send_tracker(acct, "tok-2", Some(second), 2_000)
        .unwrap();

    store
        .record_open(acct, "tok-1", 1_100, None, "unknown")
        .unwrap();
    store
        .record_open(acct, "tok-2", 2_100, None, "unknown")
        .unwrap();
    store
        .record_open(acct, "tok-2", 2_200, None, "unknown")
        .unwrap();

    assert_eq!(store.message_opens(acct, first).unwrap().len(), 1);
    assert_eq!(store.message_opens(acct, second).unwrap().len(), 2);
}

/// A live pixel URL is a capability anyone holding it can refetch forever — the
/// recipient, a forward, a scanner. The cap is what keeps that from being an
/// unbounded write channel into the user's database.
#[test]
fn one_token_stops_recording_at_the_cap() {
    let (store, acct) = store();
    let message_id = tracked(&store, acct, "tok-flood");

    for i in 0..(MAX_OPENS_PER_TOKEN as i64 + 25) {
        store
            .record_open(acct, "tok-flood", 1_000 + i, None, "unknown")
            .unwrap();
    }

    let opens = store.message_opens(acct, message_id).unwrap();
    assert_eq!(opens.len(), MAX_OPENS_PER_TOKEN);
    // The cap drops the tail, so the earliest opens — the ones that carry the
    // signal — are what survive.
    assert_eq!(opens[0].opened_at, 1_000);
    assert!(
        !store
            .record_open(acct, "tok-flood", 9_999, None, "unknown")
            .unwrap()
    );
}

/// The cap is per token, not per table: a busy tracker must not silence a
/// different message's opens.
#[test]
fn the_cap_does_not_leak_across_tokens() {
    let (store, acct) = store();
    let loud = tracked(&store, acct, "tok-loud");
    let quiet = triaged(acct, "sent-2", "thread-77").seed(&store);
    store
        .insert_send_tracker(acct, "tok-quiet", Some(quiet), 2_000)
        .unwrap();

    for i in 0..(MAX_OPENS_PER_TOKEN as i64 + 5) {
        store
            .record_open(acct, "tok-loud", 1_000 + i, None, "unknown")
            .unwrap();
    }
    assert!(
        store
            .record_open(acct, "tok-quiet", 2_000, None, "unknown")
            .unwrap()
    );

    assert_eq!(
        store.message_opens(acct, loud).unwrap().len(),
        MAX_OPENS_PER_TOKEN
    );
    assert_eq!(store.message_opens(acct, quiet).unwrap().len(), 1);
}

/// A pixel URL lives in the recipient's mailbox for as long as the mail does, so
/// the tracker has to retire on its own: past the window the same fetch that
/// recorded yesterday records nothing, while a recent send still collects.
#[test]
fn a_tracker_stops_collecting_past_the_retention_window() {
    let (store, acct) = store();
    let sent_at = 1_700_000_000;
    let old = triaged(acct, "sent-old", "thread-1").seed(&store);
    store
        .insert_send_tracker(acct, "tok-old", Some(old), sent_at)
        .unwrap();
    let recent = triaged(acct, "sent-new", "thread-2").seed(&store);
    store
        .insert_send_tracker(acct, "tok-new", Some(recent), sent_at)
        .unwrap();

    let at = |offset: i64| sent_at + offset;
    assert!(
        store
            .record_open(
                acct,
                "tok-old",
                at(TRACKER_RETENTION_SECS - 1),
                None,
                "unknown"
            )
            .unwrap()
    );
    assert!(
        !store
            .record_open(acct, "tok-old", at(TRACKER_RETENTION_SECS), None, "unknown")
            .unwrap()
    );
    assert!(
        !store
            .record_open(
                acct,
                "tok-old",
                at(TRACKER_RETENTION_SECS * 5),
                None,
                "unknown"
            )
            .unwrap()
    );

    // The window is per tracker, measured from ITS send.
    assert!(
        store
            .record_open(acct, "tok-new", at(60), None, "unknown")
            .unwrap()
    );

    assert_eq!(store.message_opens(acct, old).unwrap().len(), 1);
    assert_eq!(store.message_opens(acct, recent).unwrap().len(), 1);
}

/// What the send path does when the mail never reached the wire: the token goes,
/// and the tracker is a stranger from then on.
#[test]
fn deleting_a_tracker_takes_its_opens_with_it() {
    let (store, acct) = store();
    let message_id = tracked(&store, acct, "tok-gone");
    assert!(
        store
            .record_open(acct, "tok-gone", 1_100, None, "unknown")
            .unwrap()
    );

    assert!(store.delete_send_tracker(acct, "tok-gone").unwrap());
    assert!(store.message_opens(acct, message_id).unwrap().is_empty());
    assert!(store.tracked_message(acct, "tok-gone").unwrap().is_none());
    assert!(
        !store
            .record_open(acct, "tok-gone", 1_200, None, "unknown")
            .unwrap()
    );
    // Deleting what is already gone is not an error.
    assert!(!store.delete_send_tracker(acct, "tok-gone").unwrap());
}

/// `message_opens` carries no account of its own, so the delete has to be scoped
/// through the tracker — otherwise another account's token would take this
/// account's opens with it.
#[test]
fn deleting_a_tracker_does_not_cross_accounts() {
    let (store, mine) = store();
    let theirs = store.ensure_account("other@example.com").unwrap();
    let message_id = tracked(&store, mine, "tok-abc");
    assert!(
        store
            .record_open(mine, "tok-abc", 1_100, None, "unknown")
            .unwrap()
    );

    assert!(!store.delete_send_tracker(theirs, "tok-abc").unwrap());
    assert_eq!(store.message_opens(mine, message_id).unwrap().len(), 1);
    assert!(store.tracked_message(mine, "tok-abc").unwrap().is_some());
}

/// Every stored `unknown` and `scanner` open is relabelled with today's
/// classifier when the store opens, in BOTH directions, and `proxied` is never
/// touched.
#[test]
fn stored_opens_are_relabelled_both_ways_by_the_migration() {
    let (store, acct) = store();
    let message_id = tracked(&store, acct, "tok-scan");
    let bot = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/42.0.2311.135 Safari/537.36 Edge/12.246 Mozilla/5.0";
    for (ua, label) in [
        // Recorded before the label existed: becomes a scanner.
        (bot, "unknown"),
        // A person's open hidden by a marker that has since been removed
        // (okhttp was one, briefly): comes back.
        ("okhttp/4.12.0", "scanner"),
        ("Apple Mail/16.0", "unknown"),
        ("… GoogleImageProxy", "proxied"),
    ] {
        assert!(
            store
                .record_open(acct, "tok-scan", 1_100, Some(ua), label)
                .unwrap()
        );
    }
    {
        let conn = store.lock().unwrap();
        crate::store::sqlite::migrate::migrate(&conn).unwrap();
        // Twice: idempotent.
        crate::store::sqlite::migrate::migrate(&conn).unwrap();
    }
    let mut got: Vec<(String, String)> = store
        .message_opens(acct, message_id)
        .unwrap()
        .into_iter()
        .map(|o| (o.user_agent.unwrap(), o.classification))
        .collect();
    got.sort();
    let want = |ua: &str, c: &str| (ua.to_string(), c.to_string());
    assert_eq!(
        got,
        vec![
            want("Apple Mail/16.0", "unknown"),
            want(bot, "scanner"),
            want("okhttp/4.12.0", "unknown"),
            want("… GoogleImageProxy", "proxied"),
        ]
    );
}

/// Scanners have their own small allowance per token: a gateway that rescans
/// on every reprocess fills IT, and a person's open still lands afterwards.
#[test]
fn scanners_cannot_spend_a_persons_open_slots() {
    let (store, acct) = store();
    tracked(&store, acct, "tok-cap");
    for i in 0..MAX_SCANNER_OPENS_PER_TOKEN + 10 {
        let kept = store
            .record_open(acct, "tok-cap", 1_100 + i as i64, Some("curl/8"), "scanner")
            .unwrap();
        assert_eq!(kept, i < MAX_SCANNER_OPENS_PER_TOKEN, "scanner row {i}");
    }
    for i in 0..MAX_OPENS_PER_TOKEN {
        assert!(
            store
                .record_open(
                    acct,
                    "tok-cap",
                    2_000 + i as i64,
                    Some("Apple Mail/16.0"),
                    "unknown"
                )
                .unwrap(),
            "a person's open {i} still lands"
        );
    }
    assert!(
        !store
            .record_open(acct, "tok-cap", 3_000, Some("Apple Mail/16.0"), "unknown")
            .unwrap(),
        "the person's cap still holds"
    );
}
