//! The contact card (`person_record`): each part is read from the source the
//! rest of the store trusts for it, and a stranger gets an empty card rather
//! than an error.

use super::super::*;
use super::support::*;
use crate::store::ContactEntry;
use crate::store::sqlite::groups::NewGroupMember;
use crate::types::{Disposition, GroupMode, SealedKind};
use chrono::TimeZone;

fn at(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, day, 12, 0, 0).unwrap()
}

#[test]
fn received_counts_the_listings_mail_and_dates_it_from_the_messages() {
    let (store, acct) = store();
    // Seeded NEWEST FIRST, the order the backfill walks. The directory's
    // `first_seen` would say day 20; the card must say day 2.
    for (gmail, thread, day) in [("g3", "t2", 20), ("g2", "t1", 9), ("g1", "t1", 2)] {
        triaged(acct, gmail, thread)
            .from("Dan@Example.com")
            .from_name(Some("Dan Smith"))
            .subject(&format!("subject {gmail}"))
            .received_at(at(day))
            .upsert(&store);
    }
    // None of these is mail "their mail" could return, so none is counted.
    triaged(acct, "g-spam", "t3")
        .from("dan@example.com")
        .is_spam(true)
        .received_at(at(25))
        .upsert(&store);
    triaged(acct, "g-otp", "t4")
        .from("dan@example.com")
        .subject("Your code")
        .body("123456")
        .received_at(at(26))
        .sealed(SealedKind::Otp)
        .seed(&store);

    let card = store.person_record(acct, "dan@example.com").unwrap();
    let received = card.received.expect("three ordinary messages");
    assert_eq!(received.count, 3);
    assert_eq!(received.first_at, at(2));
    assert_eq!(received.last_at, at(20));
    assert_eq!(card.display_name.as_deref(), Some("Dan Smith"));

    // Newest conversation first, titled by its newest message, sealed absent.
    let threads: Vec<_> = card
        .recent_threads
        .iter()
        .map(|t| (t.thread_id.as_str(), t.subject.as_str()))
        .collect();
    assert_eq!(threads, [("t2", "subject g3"), ("t1", "subject g2")]);
}

#[test]
fn sent_is_the_contacts_row_and_it_is_the_known_contact_floor() {
    let (store, acct) = store();
    store
        .merge_harvested_contacts(
            acct,
            &[ContactEntry {
                addr: "ann@fund.com".into(),
                display_name: Some("Ann Lee".into()),
                sent_count: 4,
                last_sent_at: Some(at(11)),
            }],
        )
        .unwrap();
    let card = store.person_record(acct, "ANN@fund.com").unwrap();
    assert!(card.known_contact);
    assert_eq!(
        card.known_contact,
        store.is_known_contact(acct, "ann@fund.com").unwrap()
    );
    let sent = card.sent.expect("the user wrote to her");
    assert_eq!(sent.count, 4);
    assert_eq!(sent.last_at, Some(at(11)));
    // No mail from her, so the name comes from the user's own Sent mail.
    assert!(card.received.is_none());
    assert_eq!(card.display_name.as_deref(), Some("Ann Lee"));
}

#[test]
fn the_rule_is_the_one_stage_one_would_apply_and_groups_are_listed() {
    let (store, acct) = store();
    store
        .set_sender_rule(acct, "*@fund.com", "only the totals", Disposition::Surface)
        .unwrap();
    store
        .create_send_group(
            acct,
            "Preseed Investors",
            GroupMode::Individual,
            "",
            &[NewGroupMember {
                addr: "Ann@Fund.com".into(),
                display_name: None,
            }],
        )
        .unwrap();

    let card = store.person_record(acct, "ann@fund.com").unwrap();
    let rule = card.rule.expect("the glob covers her");
    assert_eq!(rule.match_pattern, "*@fund.com");
    assert_eq!(
        rule.id,
        crate::triage::rules::match_sender_rule(
            "ann@fund.com",
            &store.list_sender_rules(acct).unwrap()
        )
        .unwrap()
        .id
    );
    let groups: Vec<_> = card.groups.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(groups, ["Preseed Investors"]);

    assert!(
        store
            .person_record(acct, "bo@elsewhere.com")
            .unwrap()
            .rule
            .is_none()
    );
}

#[test]
fn a_stranger_gets_an_empty_card_and_the_user_is_told_it_is_them() {
    let (store, acct) = store();
    let card = store.person_record(acct, "nobody@nowhere.example").unwrap();
    assert!(!card.is_self && !card.known_contact);
    assert!(card.received.is_none() && card.sent.is_none() && card.rule.is_none());
    assert!(card.groups.is_empty() && card.recent_threads.is_empty());
    assert!(card.display_name.is_none());

    assert!(store.person_record(acct, "Me@Example.com").unwrap().is_self);
}
