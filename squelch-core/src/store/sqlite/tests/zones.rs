//! A correspondent's timezone (`zones`): what ingest records, whom it is
//! credited to, what never counts, and how the card serves it.

use super::super::*;
use super::support::*;
use crate::timezone::{ZoneConfidence, ZoneSource};
use chrono::Duration;

/// `days` ago at noon UTC, so every fixture sits inside the twelve-month
/// window whatever day the suite runs.
fn ago(days: i64) -> DateTime<Utc> {
    Utc::now() - Duration::days(days)
}

fn zone_row(
    store: &SqliteStore,
    acct: AccountId,
    addr: &str,
) -> Option<(Option<String>, Option<String>)> {
    store
        .lock()
        .unwrap()
        .query_row(
            "SELECT zone, user_zone FROM sender_zones WHERE account_id = ?1 AND addr = ?2",
            params![acct, addr],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .unwrap()
}

fn count(store: &SqliteStore, sql: &str) -> i64 {
    store
        .lock()
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

/// Three messages from `from` a week apart, all stamped `offset`.
fn three_from(store: &SqliteStore, acct: AccountId, from: &str, prefix: &str, offset: i32) {
    for i in 0..3 {
        triaged(acct, &format!("{prefix}{i}"), &format!("t-{prefix}{i}"))
            .from(from)
            .received_at(ago(7 * i + 1))
            .date_offset(Some(offset))
            .ingest(store);
    }
}

#[test]
fn offsets_on_a_senders_mail_become_their_zone_on_the_card() {
    let (store, acct) = store();
    // India: +5:30, which no other listed zone shares.
    three_from(&store, acct, "Priya@Example.in", "g", 330);
    let offsets: Vec<Option<i32>> = {
        let conn = store.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT date_offset_minutes FROM messages ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    };
    assert_eq!(offsets, [Some(330); 3]);

    let tz = store
        .person_record(acct, "priya@example.in")
        .unwrap()
        .timezone
        .expect("three matching offsets");
    assert_eq!(tz.zone, "Asia/Kolkata");
    assert_eq!(tz.source, ZoneSource::Offsets);
    assert_eq!(tz.confidence, ZoneConfidence::High);
}

#[test]
fn a_re_ingest_does_not_count_a_message_twice() {
    let (store, acct) = store();
    // Two New York messages and one Tokyo one: no zone has 90%.
    for (gmail, offset) in [("g1", -240), ("g2", -240), ("g3", 540)] {
        triaged(acct, gmail, gmail)
            .from("dan@example.com")
            .received_at(ago(3))
            .date_offset(Some(offset))
            .ingest(&store);
    }
    assert_eq!(zone_row(&store, acct, "dan@example.com"), None);
    // Re-sighting the New York mail ten times changes nothing: the
    // observation is the row, and the row is one.
    for _ in 0..10 {
        triaged(acct, "g1", "g1")
            .from("dan@example.com")
            .received_at(ago(3))
            .date_offset(Some(-240))
            .ingest(&store);
    }
    assert_eq!(zone_row(&store, acct, "dan@example.com"), None);
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM messages WHERE date_offset_minutes = -240"
        ),
        2
    );
    // A re-fetch with no opinion (no Date header parsed) keeps the offset.
    triaged(acct, "g1", "g1")
        .from("dan@example.com")
        .received_at(ago(3))
        .date_offset(None)
        .ingest(&store);
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM messages WHERE date_offset_minutes = -240"
        ),
        2
    );
}

#[test]
fn an_invitation_is_credited_to_its_organizer_not_the_from() {
    let (store, acct) = store();
    triaged(acct, "g1", "t1")
        .from("calendar-notification@google.com")
        .received_at(ago(2))
        .date_offset(Some(0))
        .invite("Sam@Example.com", "America/Chicago")
        .ingest(&store);
    let tz = store
        .person_record(acct, "sam@example.com")
        .unwrap()
        .timezone
        .expect("one invitation is a majority of one");
    assert_eq!(tz.zone, "America/Chicago");
    assert_eq!(tz.source, ZoneSource::Invite);
    assert_eq!(tz.confidence, ZoneConfidence::High);
    assert!(
        store
            .person_record(acct, "calendar-notification@google.com")
            .unwrap()
            .timezone
            .is_none()
    );
    // Re-ingesting the same invitation replaces its row, never adds one.
    triaged(acct, "g1", "t1")
        .from("calendar-notification@google.com")
        .received_at(ago(2))
        .invite("sam@example.com", "America/Chicago")
        .ingest(&store);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM sender_zone_invites"), 1);
}

#[test]
fn sent_and_spam_mail_are_never_observations_until_rescued() {
    let (store, acct) = store();
    for i in 0..3 {
        triaged(acct, &format!("s{i}"), "t")
            .from("dan@example.com")
            .is_sent(true)
            .received_at(ago(i + 1))
            .date_offset(Some(540))
            .ingest(&store);
    }
    let spam: Vec<i64> = (0..3)
        .map(|i| {
            triaged(acct, &format!("p{i}"), "t")
                .from("eve@example.com")
                .is_spam(true)
                .received_at(ago(i + 1))
                .date_offset(Some(540))
                .invite("mallory@example.com", "Asia/Tokyo")
                .ingest(&store)
        })
        .collect();
    for addr in ["dan@example.com", "eve@example.com", "mallory@example.com"] {
        assert_eq!(zone_row(&store, acct, addr), None, "{addr}");
    }
    // An ordinary message from each recomputes them, and still the sent and
    // spam mail does not vote: one offset is not three, and one invitation
    // outvotes three that do not count.
    for (gmail, from) in [("i1", "dan@example.com"), ("i2", "eve@example.com")] {
        triaged(acct, gmail, "t")
            .from(from)
            .received_at(ago(1))
            .date_offset(Some(540))
            .ingest(&store);
    }
    triaged(acct, "i3", "t")
        .from("calendar@example.com")
        .received_at(ago(1))
        .invite("mallory@example.com", "Europe/Berlin")
        .ingest(&store);
    assert_eq!(zone_row(&store, acct, "dan@example.com"), None);
    assert_eq!(zone_row(&store, acct, "eve@example.com"), None);
    assert_eq!(
        zone_row(&store, acct, "mallory@example.com"),
        Some((Some("Europe/Berlin".into()), None))
    );
    // The user's "not spam" makes the mail count, offsets and invitation
    // alike, without a re-fetch.
    for id in &spam {
        assert!(store.clear_spam(acct, *id).unwrap());
    }
    assert_eq!(
        zone_row(&store, acct, "eve@example.com"),
        Some((Some("Asia/Tokyo".into()), None))
    );
    assert_eq!(
        zone_row(&store, acct, "mallory@example.com"),
        Some((Some("Asia/Tokyo".into()), None))
    );
}

#[test]
fn utc_stamped_mail_writes_no_row_and_never_reads_the_history() {
    let (store, acct) = store();
    for i in 0..5 {
        triaged(acct, &format!("n{i}"), "t")
            .from("news@shop.example")
            .received_at(ago(i + 1))
            .date_offset(Some(0))
            .ingest(&store);
    }
    assert_eq!(zone_row(&store, acct, "news@shop.example"), None);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM sender_zones"), 0);
}

#[test]
fn the_accounts_own_address_never_gets_a_row() {
    let (store, acct) = store();
    // A note to self arriving in the inbox, and an invitation the user sent
    // that echoed back, under another spelling of the address.
    three_from(&store, acct, "Me@Example.com", "self", 540);
    triaged(acct, "inv", "t")
        .from("calendar-notification@google.com")
        .received_at(ago(1))
        .invite("ME@example.com", "Asia/Tokyo")
        .ingest(&store);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM sender_zones"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM sender_zone_invites"), 0);
    let card = store.person_record(acct, "me@example.com").unwrap();
    assert!(card.is_self && card.timezone.is_none());
    // And the user cannot set one either.
    assert!(matches!(
        store.set_person_timezone(acct, "Me@example.com", Some("Asia/Tokyo")),
        Err(CoreError::InvalidInput(_))
    ));
}

#[test]
fn zones_are_kept_per_account() {
    let (store, acct) = store();
    let other = store.ensure_account("other@example.com").unwrap();
    three_from(&store, acct, "dan@example.com", "a", 330);
    three_from(&store, other, "dan@example.com", "b", 540);
    store
        .set_person_timezone(other, "dan@example.com", Some("Europe/Berlin"))
        .unwrap();
    let mine = store.person_record(acct, "dan@example.com").unwrap();
    let theirs = store.person_record(other, "dan@example.com").unwrap();
    assert_eq!(mine.timezone.unwrap().zone, "Asia/Kolkata");
    assert_eq!(theirs.timezone.unwrap().zone, "Europe/Berlin");
    // Another account's invitation does not vote here either.
    triaged(other, "inv", "t")
        .from("x@example.com")
        .received_at(ago(1))
        .invite("ann@example.com", "Asia/Tokyo")
        .ingest(&store);
    assert!(
        store
            .person_record(acct, "ann@example.com")
            .unwrap()
            .timezone
            .is_none()
    );
}

#[test]
fn deleting_a_message_removes_its_invitation_observation() {
    let (store, acct) = store();
    let id = triaged(acct, "inv", "t")
        .from("x@example.com")
        .received_at(ago(1))
        .invite("ann@example.com", "Asia/Tokyo")
        .ingest(&store);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM sender_zone_invites"), 1);
    store
        .lock()
        .unwrap()
        .execute("DELETE FROM messages WHERE id = ?1", params![id])
        .unwrap();
    assert_eq!(count(&store, "SELECT COUNT(*) FROM sender_zone_invites"), 0);
}

#[test]
fn the_override_wins_and_clearing_it_hands_back_to_inference() {
    let (store, acct) = store();
    three_from(&store, acct, "dan@example.com", "g", 330);
    let set = store
        .set_person_timezone(acct, "Dan@Example.com", Some("America/Chicago"))
        .unwrap()
        .unwrap();
    assert_eq!(
        (set.zone.as_str(), set.source, set.confidence),
        ("America/Chicago", ZoneSource::User, ZoneConfidence::High)
    );
    // New mail keeps feeding inference underneath; the card still says Chicago.
    triaged(acct, "g9", "t")
        .from("dan@example.com")
        .received_at(ago(1))
        .date_offset(Some(330))
        .ingest(&store);
    assert_eq!(
        zone_row(&store, acct, "dan@example.com"),
        Some((Some("Asia/Kolkata".into()), Some("America/Chicago".into())))
    );
    assert_eq!(
        store
            .person_record(acct, "dan@example.com")
            .unwrap()
            .timezone
            .unwrap()
            .source,
        ZoneSource::User
    );
    let cleared = store
        .set_person_timezone(acct, "dan@example.com", None)
        .unwrap()
        .unwrap();
    assert_eq!(
        (cleared.zone.as_str(), cleared.source),
        ("Asia/Kolkata", ZoneSource::Offsets)
    );
    // An override for someone inference knows nothing about is a row of its
    // own, and clearing it leaves nothing behind.
    store
        .set_person_timezone(acct, "ann@example.com", Some("Europe/Berlin"))
        .unwrap();
    assert_eq!(
        store
            .set_person_timezone(acct, "ann@example.com", None)
            .unwrap(),
        None
    );
    assert_eq!(zone_row(&store, acct, "ann@example.com"), None);
    assert!(matches!(
        store.set_person_timezone(acct, "ann@example.com", Some("Eastern Standard Time")),
        Err(CoreError::InvalidInput(_))
    ));
}

#[test]
fn a_failing_recompute_does_not_fail_ingest() {
    let (store, acct) = store();
    store
        .lock()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER zones_fail BEFORE INSERT ON sender_zones
             BEGIN SELECT RAISE(ABORT, 'forced'); END;",
        )
        .unwrap();
    three_from(&store, acct, "dan@example.com", "g", 330);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM messages"), 3);
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM triage"),
        3,
        "the whole ingest committed"
    );
    // The first two inferred nothing and so inserted nothing; the third's
    // savepoint took its observation's other writes with it.
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM messages WHERE date_offset_minutes IS NOT NULL"
        ),
        2
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM sender_zones"), 0);
    // The control: without the trigger the same mail does infer.
    store
        .lock()
        .unwrap()
        .execute_batch("DROP TRIGGER zones_fail;")
        .unwrap();
    three_from(&store, acct, "dan@example.com", "g", 330);
    assert!(zone_row(&store, acct, "dan@example.com").is_some());
}

#[test]
fn a_failure_that_ends_the_transaction_fails_ingest() {
    // RAISE(ROLLBACK) stands in for SQLITE_FULL and friends: SQLite has
    // already rolled the message write back, so swallowing would let the
    // rest of ingest commit without it.
    let (store, acct) = store();
    store
        .lock()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER zones_kill BEFORE INSERT ON sender_zones
             BEGIN SELECT RAISE(ROLLBACK, 'forced'); END;",
        )
        .unwrap();
    // Two first, which infer nothing and so insert nothing; the third is the
    // one whose recompute inserts.
    for g in ["g2", "g3"] {
        triaged(acct, g, "t")
            .from("dan@example.com")
            .received_at(ago(2))
            .date_offset(Some(330))
            .ingest(&store);
    }
    let b = triaged(acct, "g1", "t")
        .from("dan@example.com")
        .received_at(ago(1))
        .date_offset(Some(330));
    assert!(store.ingest_message(&b.build()).is_err());
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM messages WHERE gmail_msg_id = 'g1'"
        ),
        0
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM triage"),
        count(&store, "SELECT COUNT(*) FROM messages"),
        "nothing committed half a message"
    );
}

/// The per-ingest recompute for a sender with 5,000 stored messages, every
/// one an observation inside the window (the worst case: UTC-stamped mail is
/// never read at all). Printed, and bounded loosely so a debug build on a
/// loaded machine does not flake.
#[test]
fn the_recompute_for_a_five_thousand_message_sender_stays_cheap() {
    let (store, acct) = store();
    {
        let mut conn = store.lock().unwrap();
        let tx = conn.transaction().unwrap();
        let tz: chrono_tz::Tz = "America/New_York".parse().unwrap();
        for i in 0..5000i64 {
            let at = Utc::now() - Duration::minutes(i * 100);
            let offset = {
                use chrono::{Offset, TimeZone};
                tz.offset_from_utc_datetime(&at.naive_utc())
                    .fix()
                    .local_minus_utc()
                    / 60
            };
            tx.execute(
                "INSERT INTO messages(account_id, gmail_msg_id, thread_id, from_addr, subject,
                     received_at, snippet, date_offset_minutes)
                 VALUES(?1, ?2, ?2, 'bulk@example.com', 's', ?3, '', ?4)",
                params![acct, format!("b{i}"), at.to_rfc3339(), offset],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    let mut samples = Vec::new();
    for _ in 0..20 {
        let conn = store.lock().unwrap();
        let start = std::time::Instant::now();
        super::super::zones::refresh_sender_zone_conn(&conn, acct, "bulk@example.com", Utc::now())
            .unwrap();
        samples.push(start.elapsed());
        drop(conn);
    }
    samples.sort();
    let median = samples[samples.len() / 2];
    eprintln!(
        "sender_zone recompute, 5000 observations: median {median:?}, max {:?}",
        samples.last().unwrap()
    );
    assert!(median < std::time::Duration::from_millis(50), "{median:?}");
    assert_eq!(
        zone_row(&store, acct, "bulk@example.com")
            .unwrap()
            .0
            .as_deref(),
        Some("America/New_York")
    );
}

#[test]
fn the_offsets_read_is_an_indexed_probe_that_stops_at_the_cap() {
    let (store, _) = store();
    let conn = store.lock().unwrap();
    let plan: Vec<String> = {
        let mut stmt = conn
            .prepare(&format!(
                "EXPLAIN QUERY PLAN {}",
                super::super::zones::OFFSETS_SQL
            ))
            .unwrap();
        stmt.query_map(params![1, "a@b.co", "2026-01-01", 500], |r| {
            r.get::<_, String>(3)
        })
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap()
    };
    let plan = plan.join(" | ");
    assert!(plan.contains("idx_messages_zone_obs"), "{plan}");
    // Newest-first straight off the index: no sort of the sender's history.
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");
}
