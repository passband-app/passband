//! The lapsed-user note's read (`SqliteStore::lapsed_note_candidates`).
//! Every sender here is invented, at `.example`.

use super::super::lapsed_note::NoteKind;
use super::super::*;
use super::reading_senders::into_reading_with;
use super::support::*;
use crate::triage::decision::{AuthKind, EmailKind, RecordProposal};
use chrono::Duration;

const SHOP: &str = "orders@shop.example";

fn week_ago() -> DateTime<Utc> {
    Utc::now() - Duration::days(7)
}

/// One message from `from`, `days_ago` old, its subject `key`, in a thread of
/// its own.
fn mail(store: &SqliteStore, acct: AccountId, key: &str, from: &str, days_ago: i64) -> i64 {
    triaged(acct, key, key)
        .from(from)
        .from_name(Some("Shop"))
        .subject(key)
        .body("the whole body, never listed")
        .snippet(&format!("snippet of {key}"))
        .received_at(Utc::now() - Duration::days(days_ago))
        .seed(store)
}

/// The CONTROL every exclusion test seeds beside what it excludes, so an
/// empty answer can never pass for the wrong reason.
const CONTROL: &str = "control";

/// A message from `from`, `days_ago` old, decided as a plain receipt that
/// stayed out of For your eyes and Reading.
fn receipt(store: &SqliteStore, acct: AccountId, key: &str, from: &str, days_ago: i64) -> i64 {
    let id = mail(store, acct, key, from, days_ago);
    into_reading_with(store, acct, id, |d| {
        d.kinds = vec![EmailKind::Receipt];
        d.destinations.clear();
        d.records = vec![RecordProposal::Receipt {
            merchant: "Shop".into(),
            amount: Some(12.0),
            currency: Some("USD".into()),
            evidence: vec![],
        }];
    });
    id
}

fn delivery(store: &SqliteStore, acct: AccountId, key: &str, status: &str) -> i64 {
    let id = mail(store, acct, key, "ship@carrier.example", 1);
    let status = status.to_string();
    into_reading_with(store, acct, id, move |d| {
        d.kinds = vec![EmailKind::Delivery];
        d.destinations.clear();
        d.records = vec![RecordProposal::Delivery {
            carrier: None,
            tracking_number: None,
            status,
            item_name: None,
            merchant: None,
            order_refs: vec![],
            evidence: vec![],
        }];
    });
    id
}

fn subjects(store: &SqliteStore, acct: AccountId) -> Vec<String> {
    store
        .lapsed_note_candidates(acct, week_ago())
        .unwrap()
        .into_iter()
        .map(|c| c.subject)
        .collect()
}

#[test]
fn handled_receipts_and_deliveries_are_listed_newest_first() {
    let (store, acct) = store();
    receipt(&store, acct, "older", SHOP, 3);
    receipt(&store, acct, "newer", SHOP, 1);
    delivery(&store, acct, "box", "shipped");
    let got = store.lapsed_note_candidates(acct, week_ago()).unwrap();
    let keys: Vec<&str> = got.iter().map(|c| c.subject.as_str()).collect();
    // `mail` keys the subject; the delivery is a day old like "newer", and
    // the later insert breaks the tie.
    assert_eq!(keys, vec!["box", "newer", "older"], "{got:?}");
    assert_eq!(got[0].kind, NoteKind::Shipped);
    assert_eq!(got[1].kind, NoteKind::Receipt);
    assert_eq!(got[1].from_addr, SHOP);
    assert_eq!(
        got[1].snippet, "snippet of newer",
        "the stored snippet, never the body"
    );
}

#[test]
fn delivery_statuses_name_the_label_and_a_problem_is_left_out() {
    let (store, acct) = store();
    delivery(&store, acct, "ordered", "ordered");
    delivery(&store, acct, "delivered", "delivered");
    delivery(&store, acct, "broken", "exception");
    let got = store.lapsed_note_candidates(acct, week_ago()).unwrap();
    let pairs: Vec<(&str, NoteKind)> = got.iter().map(|c| (c.subject.as_str(), c.kind)).collect();
    assert!(pairs.contains(&("ordered", NoteKind::Order)), "{pairs:?}");
    assert!(
        pairs.contains(&("delivered", NoteKind::Delivered)),
        "{pairs:?}"
    );
    assert!(!pairs.iter().any(|(s, _)| *s == "broken"), "{pairs:?}");
}

#[test]
fn older_than_the_window_is_out() {
    let (store, acct) = store();
    receipt(&store, acct, CONTROL, SHOP, 2);
    receipt(&store, acct, "old", SHOP, 9);
    assert_eq!(subjects(&store, acct), vec![CONTROL]);
}

#[test]
fn mail_placed_in_fye_or_reading_is_out() {
    let (store, acct) = store();
    receipt(&store, acct, CONTROL, SHOP, 2);
    let fye = mail(&store, acct, "fye", SHOP, 1);
    into_reading_with(&store, acct, fye, |d| {
        d.kinds = vec![EmailKind::Receipt];
        d.destinations.clear();
        d.attention.show_in_fye = true;
    });
    let reading = mail(&store, acct, "reading", SHOP, 1);
    into_reading_with(&store, acct, reading, |d| {
        d.kinds = vec![EmailKind::Receipt];
    });
    assert_eq!(subjects(&store, acct), vec![CONTROL]);
}

#[test]
fn other_kinds_and_bills_are_out() {
    let (store, acct) = store();
    receipt(&store, acct, CONTROL, SHOP, 2);
    let promo = mail(&store, acct, "promo", SHOP, 1);
    into_reading_with(&store, acct, promo, |d| {
        d.kinds = vec![EmailKind::Promotional];
        d.destinations.clear();
    });
    let bill = mail(&store, acct, "bill", SHOP, 1);
    into_reading_with(&store, acct, bill, |d| {
        d.kinds = vec![EmailKind::Receipt, EmailKind::Bill];
        d.destinations.clear();
    });
    assert_eq!(subjects(&store, acct), vec![CONTROL]);
}

#[test]
fn auth_mail_is_out() {
    let (store, acct) = store();
    receipt(&store, acct, CONTROL, SHOP, 2);
    let code = mail(&store, acct, "code", SHOP, 1);
    into_reading_with(&store, acct, code, |d| {
        d.kinds = vec![EmailKind::Receipt];
        d.destinations.clear();
        d.auth.kinds = vec![AuthKind::Otp];
    });
    assert_eq!(subjects(&store, acct), vec![CONTROL]);
}

#[test]
fn undecided_mail_is_out() {
    let (store, acct) = store();
    receipt(&store, acct, CONTROL, SHOP, 2);
    mail(&store, acct, "pending", SHOP, 1);
    assert_eq!(subjects(&store, acct), vec![CONTROL]);
}

#[test]
fn mail_opened_in_passband_is_out() {
    let (store, acct) = store();
    receipt(&store, acct, CONTROL, SHOP, 2);
    let id = receipt(&store, acct, "seen", SHOP, 1);
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE triage SET opened_at = ?2 WHERE message_id = ?1",
            params![id, Utc::now().to_rfc3339()],
        )
        .unwrap();
    assert_eq!(subjects(&store, acct), vec![CONTROL]);
}

#[test]
fn mail_from_a_known_contact_is_out() {
    let (store, acct) = store();
    receipt(&store, acct, CONTROL, SHOP, 2);
    receipt(&store, acct, "friend", "Pat@Friend.example", 1);
    store
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO contacts(account_id, addr, sent_count, first_seen)
             VALUES(?1, 'pat@friend.example', 2, ?2)",
            params![acct, Utc::now().to_rfc3339()],
        )
        .unwrap();
    assert_eq!(subjects(&store, acct), vec![CONTROL]);
}

#[test]
fn spam_and_failed_authentication_are_out() {
    let (store, acct) = store();
    receipt(&store, acct, CONTROL, SHOP, 2);
    let forged = receipt(&store, acct, "forged", SHOP, 1);
    let spam = receipt(&store, acct, "spam", SHOP, 1);
    let conn = store.lock().unwrap();
    conn.execute("UPDATE messages SET auth_pass = 0 WHERE id = ?1", [forged])
        .unwrap();
    conn.execute("UPDATE messages SET is_spam = 1 WHERE id = ?1", [spam])
        .unwrap();
    drop(conn);
    assert_eq!(subjects(&store, acct), vec![CONTROL]);
}

#[test]
fn another_accounts_mail_is_out() {
    let (store, acct) = store();
    receipt(&store, acct, CONTROL, SHOP, 2);
    let other = store.ensure_account("other@example.com").unwrap();
    receipt(&store, other, "theirs", SHOP, 1);
    assert_eq!(subjects(&store, acct), vec![CONTROL]);
}

/// Mail the owner wrote themselves (a note to self, a copy from another
/// client) is theirs, whatever the decision on it says.
#[test]
fn the_owners_own_mail_is_out() {
    let (store, acct) = store();
    receipt(&store, acct, CONTROL, SHOP, 2);
    // Own mail is never triaged (its job is an access check), so the decision
    // here is a leftover from a row once read as inbound: the case the
    // authorship test exists for.
    let mine = receipt(&store, acct, "mine", SHOP, 1);
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE messages SET from_addr = 'me@example.com', auth_pass = NULL WHERE id = ?1",
            [mine],
        )
        .unwrap();
    assert_eq!(subjects(&store, acct), vec![CONTROL]);
}

/// A scam receipt can pass DMARC; the stored sender signals still give it
/// away, and the note never lists it.
#[test]
fn a_sender_that_looks_off_is_out() {
    let (store, acct) = store();
    receipt(&store, acct, CONTROL, SHOP, 2);
    let spoof = receipt(&store, acct, "spoof", SHOP, 1);
    let via = receipt(&store, acct, "via", SHOP, 1);
    let conn = store.lock().unwrap();
    conn.execute(
        "UPDATE messages SET sender_signals = '{\"name_spoof\":\"help@bank.example\"}' WHERE id = ?1",
        [spoof],
    )
    .unwrap();
    conn.execute(
        "UPDATE messages SET sender_signals = '{\"via\":\"mailer.example\"}' WHERE id = ?1",
        [via],
    )
    .unwrap();
    drop(conn);
    let mut got = subjects(&store, acct);
    got.sort();
    assert_eq!(
        got,
        vec![CONTROL.to_string(), "via".to_string()],
        "a mail service is fine"
    );
}

/// The candidate carries the provider id the liveness check asks about.
#[test]
fn a_candidate_names_its_provider_id() {
    let (store, acct) = store();
    receipt(&store, acct, "g-id", SHOP, 1);
    let got = store.lapsed_note_candidates(acct, week_ago()).unwrap();
    assert_eq!(got[0].provider_id, "g-id");
}
