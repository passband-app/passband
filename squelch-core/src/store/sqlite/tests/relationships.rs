//! The relationship ledger (`relationships`): what each column counts, the
//! floors it counts under, how rows are kept current, and that every
//! per-address statement is a seek and not a walk.

use super::super::reading_senders::SenderMeasurer;
use super::super::relationships::{
    CONTACT_SQL, CORRECTIONS_SQL, INBOUND_SQL, INITIATED_SQL, LAST_SQL, NAME_SQL, RECENT_WINDOW,
    RELATIONSHIPS_BACKFILL_KEY, RelationshipSort, SEALED_COUNT_SQL, SEALED_PROBE_SQL, THREADS_SQL,
    WALK_SOURCES,
};
use super::super::*;
use super::support::*;
use crate::store::agent_triage::AgentTriageStore;
use crate::store::{ContactEntry, SyncState};
use crate::types::SealedKind;
use chrono::Duration;

const DAN: &str = "dan@example.com";

fn days_ago(days: i64) -> DateTime<Utc> {
    Utc::now() - Duration::days(days)
}

/// One inbound message from `from` in `thread`, with a triage row.
fn inbound(
    store: &SqliteStore,
    acct: AccountId,
    key: &str,
    thread: &str,
    from: &str,
    days: i64,
) -> i64 {
    triaged(acct, key, thread)
        .from(from)
        .received_at(days_ago(days))
        .seed(store)
}

/// One message the owner sent in `thread` to `to`, through the real ingest so
/// `message_recipients` is written the way production writes it.
fn sent(
    store: &SqliteStore,
    acct: AccountId,
    key: &str,
    thread: &str,
    to: &str,
    at: DateTime<Utc>,
) -> i64 {
    triaged(acct, key, thread)
        .from("me@example.com")
        .is_sent(true)
        .to_addrs(to)
        .received_at(at)
        .ingest(store)
}

fn mark_backfilled(store: &SqliteStore, acct: AccountId) {
    store
        .set_sync_state(
            acct,
            RELATIONSHIPS_BACKFILL_KEY,
            &SyncState {
                uidvalidity: 0,
                last_uid: 1,
            },
        )
        .unwrap();
}

fn ledgers_since(store: &SqliteStore, acct: AccountId, open_days: i64, dwell_days: i64) {
    store
        .set_app_setting(
            acct,
            OPEN_LEDGER_SINCE_KEY,
            &days_ago(open_days).to_rfc3339(),
        )
        .unwrap();
    store
        .set_app_setting(
            acct,
            DWELL_LEDGER_SINCE_KEY,
            &days_ago(dwell_days).to_rfc3339(),
        )
        .unwrap();
}

fn dirty(store: &SqliteStore, acct: AccountId) -> Vec<String> {
    let conn = store.lock().unwrap();
    let mut stmt = conn
        .prepare("SELECT addr FROM relationship_dirty WHERE account_id = ?1 ORDER BY addr")
        .unwrap();
    stmt.query_map([acct], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

#[test]
fn inbound_counts_the_listings_mail_and_dates_it_from_the_messages() {
    let (store, acct) = store();
    // Seeded NEWEST FIRST, the order the backfill walks: the directory's
    // `first_seen` would say 20 days ago, the mail says 300.
    for (key, days) in [("g3", 20), ("g2", 90), ("g1", 300)] {
        triaged(acct, key, key)
            .from("Dan@Example.com")
            .from_name(Some("Dan Smith"))
            .received_at(days_ago(days))
            .seed(&store);
    }
    // None of these is mail the listings would show, so none is counted.
    triaged(acct, "spam", "spam")
        .from(DAN)
        .is_spam(true)
        .received_at(days_ago(5))
        .seed(&store);
    triaged(acct, "otp", "otp")
        .from(DAN)
        .sealed(SealedKind::Otp)
        .received_at(days_ago(4))
        .seed(&store);
    let archived = inbound(&store, acct, "old", "old", DAN, 2_000);
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE messages SET is_archive = 1 WHERE id = ?1",
            [archived],
        )
        .unwrap();

    store
        .recompute_relationship(acct, "DAN@example.com")
        .unwrap();
    let row = store.relationship(acct, DAN).unwrap().expect("a row");
    assert_eq!(row.addr, DAN, "keyed by the lowercased address");
    assert_eq!(row.received_count, 3, "the sealed message is not counted");
    let first = row.received_first_at.unwrap();
    assert!((first - days_ago(300)).num_minutes().abs() < 5, "{first}");
    // The last arrival is the newest mail the listings would show (20 days
    // ago), not the sealed code that came after it (4 days ago).
    let last = row.received_last_at.unwrap();
    assert!((last - days_ago(20)).num_minutes().abs() < 5, "{last}");
    assert_eq!(row.display_name.as_deref(), Some("Dan Smith"));
    assert_eq!(row.inbound_threads, 3, "the sealed thread is not one");
    // Three mails over three hundred days.
    assert_eq!(row.received_per_month, Some(0.3));
}

#[test]
fn a_reply_is_a_later_owner_message_in_a_thread_they_wrote_into() {
    let (store, acct) = store();
    // t1: Dan writes, the owner answers 30 minutes later.
    inbound(&store, acct, "a", "t1", DAN, 10);
    sent(
        &store,
        acct,
        "a-r",
        "t1",
        "Dan <dan@example.com>",
        days_ago(10) + Duration::minutes(30),
    );
    // t2: the owner wrote first and Dan answered; nothing from the owner
    // after Dan, so it is not a replied thread (it is an initiated one).
    sent(&store, acct, "b-0", "t2", "dan@example.com", days_ago(9));
    inbound(&store, acct, "b", "t2", DAN, 8);
    // t3, t4: answered after an hour and after a day.
    inbound(&store, acct, "c", "t3", DAN, 7);
    sent(
        &store,
        acct,
        "c-r",
        "t3",
        "dan@example.com",
        days_ago(7) + Duration::hours(1),
    );
    inbound(&store, acct, "d", "t4", DAN, 6);
    sent(&store, acct, "d-r", "t4", "dan@example.com", days_ago(5));
    // t5: never answered.
    inbound(&store, acct, "e", "t5", DAN, 4);

    store.recompute_relationship(acct, DAN).unwrap();
    let row = store.relationship(acct, DAN).unwrap().unwrap();
    assert_eq!(row.inbound_threads, 5);
    assert_eq!(row.replied_threads, 3);
    // 30m, 1h, 1d; each fixture reads the clock, so allow its drift.
    let median = row.median_reply_ms.expect("three replied threads");
    assert!((median - 3_600_000).abs() < 1_000, "{median}");
    assert_eq!(row.initiated_count, 1, "only t2 opened with the owner");
}

#[test]
fn initiated_needs_the_first_message_of_the_thread_to_name_the_address() {
    let (store, acct) = store();
    // The owner starts a thread to Ann, and later copies Dan into it: Ann's
    // initiation, not Dan's.
    sent(&store, acct, "s1", "t1", "ann@fund.com", days_ago(10));
    sent(
        &store,
        acct,
        "s2",
        "t1",
        "ann@fund.com, dan@example.com",
        days_ago(9),
    );
    // And starts one with both of them on it.
    sent(
        &store,
        acct,
        "s3",
        "t2",
        "Ann <ann@fund.com>, dan@example.com",
        days_ago(8),
    );
    store.recompute_all_relationships(acct).unwrap();
    let ann = store.relationship(acct, "ann@fund.com").unwrap().unwrap();
    let dan = store.relationship(acct, DAN).unwrap().unwrap();
    assert_eq!(ann.initiated_count, 2);
    assert_eq!(dan.initiated_count, 1);
    // Nobody wrote to the owner, so neither has an inbound thread.
    assert_eq!((ann.inbound_threads, ann.received_count), (0, 0));
}

#[test]
fn sent_is_the_contacts_row_the_known_contact_floor_reads() {
    let (store, acct) = store();
    store
        .merge_harvested_contacts(
            acct,
            &[ContactEntry {
                addr: "ann@fund.com".into(),
                display_name: Some("Ann Lee".into()),
                sent_count: 4,
                last_sent_at: Some(days_ago(3)),
            }],
        )
        .unwrap();
    store.recompute_relationship(acct, "Ann@Fund.com").unwrap();
    let row = store.relationship(acct, "ANN@fund.com").unwrap().unwrap();
    assert_eq!(row.sent_count, 4);
    assert!(row.sent_last_at.is_some());
    assert_eq!(
        row.display_name.as_deref(),
        Some("Ann Lee"),
        "from the contacts row"
    );
    // A known contact is never a purge suggestion, and the ledger stores the
    // purge flow's own answer.
    assert_eq!(row.fatigue_score, Some(0.0));
}

#[test]
fn attention_is_the_measurement_the_fatigue_score_reads() {
    let (store, acct) = store();
    // Opens recorded for 60 days, dwell for 30.
    ledgers_since(&store, acct, 60, 30);
    let early = inbound(&store, acct, "early", "early", DAN, 50);
    let late = inbound(&store, acct, "late", "late", DAN, 10);
    inbound(&store, acct, "unopened", "unopened", DAN, 5);
    // Older than the open ledger: in the inbound count, not in attention.
    inbound(&store, acct, "ancient", "ancient", DAN, 80);
    for id in [early, late] {
        store
            .acknowledge_agent_message(acct, id, Utc::now())
            .unwrap();
        store.record_message_attention(acct, id, 9_000, 1).unwrap();
    }
    // `early` was opened BEFORE the dwell ledger: its open counts, its dwell
    // and click do not.
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE triage SET opened_at = ?2 WHERE message_id = ?1",
            params![early, days_ago(40).to_rfc3339()],
        )
        .unwrap();
    store.recompute_relationship(acct, DAN).unwrap();
    let row = store.relationship(acct, DAN).unwrap().unwrap();
    assert_eq!(
        row.received_count, 4,
        "inbound counts do not wait for a ledger"
    );
    assert_eq!(row.attention_rows, 3, "the 80-day-old mail predates opens");
    assert_eq!(row.opened_count, 2);
    assert_eq!(
        row.link_clicks, 1,
        "the pre-dwell open's click is not a measurement"
    );

    // The very same numbers the purge flow's score was computed from.
    let conn = store.lock().unwrap();
    let signals = SenderMeasurer::new(&conn, acct, Utc::now())
        .unwrap()
        .measure(DAN)
        .unwrap()
        .signals;
    drop(conn);
    assert_eq!(
        (row.attention_rows, row.opened_count, row.link_clicks),
        (
            i64::from(signals.received),
            i64::from(signals.opened),
            i64::from(signals.clicks)
        )
    );

    // The context reports the attention block's coverage, the ledger's age.
    mark_backfilled(&store, acct);
    let block = store.relationship_context(acct, DAN).unwrap().unwrap();
    assert_eq!(block["coverage_days"], 60);
    assert_eq!(block["attention"]["rows"], 3);
}

#[test]
fn the_attention_block_is_left_out_of_the_context_on_a_young_ledger() {
    let (store, acct) = store();
    ledgers_since(&store, acct, 3, 3);
    inbound(&store, acct, "a", "a", DAN, 1);
    store.recompute_relationship(acct, DAN).unwrap();
    mark_backfilled(&store, acct);
    let block = store.relationship_context(acct, DAN).unwrap().unwrap();
    assert_eq!(block["coverage_days"], 3);
    assert!(block.get("attention").is_none(), "{block}");
    assert_eq!(block["received"]["count"], 1);
}

#[test]
fn the_fatigue_score_is_the_reading_purges_own() {
    let (store, acct) = store();
    ledgers_since(&store, acct, 365, 365);
    for n in 0..10 {
        inbound(
            &store,
            acct,
            &format!("n{n}"),
            &format!("n{n}"),
            "news@letters.example",
            n * 3 + 1,
        );
    }
    store
        .recompute_relationship(acct, "news@letters.example")
        .unwrap();
    let row = store
        .relationship(acct, "news@letters.example")
        .unwrap()
        .unwrap();
    let conn = store.lock().unwrap();
    let measured = SenderMeasurer::new(&conn, acct, Utc::now())
        .unwrap()
        .measure("news@letters.example")
        .unwrap();
    assert!(
        measured.score.is_some(),
        "ten unopened mails over a year of ledger"
    );
    assert_eq!(
        row.fatigue_score,
        measured.score.map(|s| f32::from(s) / 100.0)
    );
    assert_eq!(row.fatigue_reasons, measured.reasons);
    assert_eq!(row.replied_threads, 0, "bulk mail: the expected shape");
}

#[test]
fn corrections_count_both_tables_on_the_address() {
    let (store, acct) = store();
    let a = inbound(&store, acct, "a", "a", DAN, 2);
    let conn = store.lock().unwrap();
    conn.execute(
        "INSERT INTO triage_feedback(account_id, message_id, corrected_at, dimension,
             to_value, original, sender, subject)
         VALUES(?1, ?2, ?3, 'tier', 'signal', '{}', 'Dan@Example.com', 's')",
        params![acct, a, Utc::now().to_rfc3339()],
    )
    .unwrap();
    // Not `external_access`: an owner's seal takes the mail out of the
    // ledger altogether, corrections and all.
    for field in ["kinds", "fye"] {
        conn.execute(
            "INSERT INTO agent_triage_corrections(account_id, message_id, field, value_json,
                 source_revision, updated_at)
             VALUES(?1, ?2, ?3, 'true', 1, ?4)",
            params![acct, a, field, Utc::now().to_rfc3339()],
        )
        .unwrap();
    }
    drop(conn);
    store.recompute_relationship(acct, DAN).unwrap();
    assert_eq!(
        store.relationship(acct, DAN).unwrap().unwrap().corrections,
        3
    );
}

#[test]
fn the_owners_own_address_and_an_address_whose_mail_is_gone_have_no_row() {
    let (store, acct) = store();
    inbound(&store, acct, "self", "self", "me@example.com", 1);
    let gone = inbound(&store, acct, "a", "a", DAN, 1);
    store.recompute_all_relationships(acct).unwrap();
    assert!(
        store
            .relationship(acct, "me@example.com")
            .unwrap()
            .is_none()
    );
    assert!(store.relationship(acct, DAN).unwrap().is_some());

    store
        .lock()
        .unwrap()
        .execute("DELETE FROM messages WHERE id = ?1", [gone])
        .unwrap();
    store.recompute_all_relationships(acct).unwrap();
    assert!(
        store.relationship(acct, DAN).unwrap().is_none(),
        "a row of zeros would read as known and ignored"
    );
}

#[test]
fn ingest_and_attention_mark_addresses_and_the_drain_clears_them() {
    let (store, acct) = store();
    // Inbound marks its sender; Sent marks each recipient; spam marks nobody.
    let id = triaged(acct, "in", "t1")
        .from("Dan@Example.com")
        .received_at(days_ago(1))
        .ingest(&store);
    triaged(acct, "out", "t2")
        .from("me@example.com")
        .is_sent(true)
        .to_addrs("Ann <ann@fund.com>, bo@co.example")
        .received_at(days_ago(1))
        .ingest(&store);
    triaged(acct, "junk", "t3")
        .from("junk@spam.example")
        .is_spam(true)
        .received_at(days_ago(1))
        .ingest(&store);
    assert_eq!(dirty(&store, acct), ["ann@fund.com", "bo@co.example", DAN]);

    assert_eq!(store.drain_relationship_dirty(acct, 100).unwrap(), 3);
    assert!(dirty(&store, acct).is_empty());
    assert!(store.relationship(acct, "ann@fund.com").unwrap().is_some());

    // The attention POST marks the message's sender again.
    store.record_message_attention(acct, id, 4_000, 0).unwrap();
    assert_eq!(dirty(&store, acct), [DAN]);
}

#[test]
fn the_drain_is_bounded_per_call() {
    let (store, acct) = store();
    for n in 0..60 {
        triaged(acct, &format!("m{n}"), &format!("t{n}"))
            .from(&format!("s{n:02}@example.com"))
            .received_at(days_ago(1))
            .ingest(&store);
    }
    assert_eq!(store.drain_relationship_dirty(acct, 40).unwrap(), 40);
    assert_eq!(dirty(&store, acct).len(), 20);
    assert_eq!(store.drain_relationship_dirty(acct, 40).unwrap(), 20);
    assert!(dirty(&store, acct).is_empty());
}

#[test]
fn the_context_is_absent_until_the_backfill_has_run() {
    let (store, acct) = store();
    inbound(&store, acct, "a", "a", DAN, 1);
    store.recompute_all_relationships(acct).unwrap();
    assert!(
        store.relationship_context(acct, DAN).unwrap().is_none(),
        "a partly filled table would make every unfilled sender a stranger"
    );
    mark_backfilled(&store, acct);
    assert!(store.relationship_context(acct, DAN).unwrap().is_some());
    // No row is no key, not a block of zeros.
    assert!(
        store
            .relationship_context(acct, "nobody@nowhere.example")
            .unwrap()
            .is_none()
    );
}

#[test]
fn the_full_recompute_walks_in_chunks_and_finds_every_address() {
    let (store, acct) = store();
    for n in 0..7 {
        inbound(
            &store,
            acct,
            &format!("m{n}"),
            &format!("t{n}"),
            &format!("s{n}@example.com"),
            1,
        );
    }
    let mut after: Option<String> = None;
    let mut chunks = 0;
    while let Some(last) = store
        .recompute_relationship_chunk(acct, after.as_deref(), 3)
        .unwrap()
    {
        after = Some(last);
        chunks += 1;
    }
    assert_eq!(chunks, 2, "3 + 3, then a short chunk ends the walk");
    let list = store
        .list_relationships(acct, RelationshipSort::Volume, 50)
        .unwrap();
    assert_eq!(list.rows.len(), 7);
    assert!(list.computed_at.is_some());
}

#[test]
fn the_list_sorts_by_what_it_was_asked() {
    let (store, acct) = store();
    for n in 0..3 {
        inbound(
            &store,
            acct,
            &format!("v{n}"),
            &format!("v{n}"),
            "volume@example.com",
            n + 3,
        );
    }
    inbound(&store, acct, "r", "r", "replied@example.com", 2);
    sent(&store, acct, "r-r", "r", "replied@example.com", days_ago(1));
    store.recompute_all_relationships(acct).unwrap();
    let first = |sort| {
        store.list_relationships(acct, sort, 10).unwrap().rows[0]
            .addr
            .clone()
    };
    assert_eq!(first(RelationshipSort::Volume), "volume@example.com");
    assert_eq!(first(RelationshipSort::Replies), "replied@example.com");
    assert_eq!(first(RelationshipSort::Recent), "replied@example.com");
    assert_eq!(
        RelationshipSort::parse("fatigue"),
        Some(RelationshipSort::Fatigue)
    );
    assert_eq!(RelationshipSort::parse("nope"), None);
}

/// Every per-address statement seeks one address; none walks the account.
#[test]
fn every_per_address_statement_is_an_indexed_probe() {
    let (store, _acct) = store();
    let conn = store.lock().unwrap();
    let plan = |sql: &str| -> Vec<String> {
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let nulls = vec![rusqlite::types::Value::Null; stmt.parameter_count()];
        stmt.query_map(rusqlite::params_from_iter(nulls), |r| r.get::<_, String>(3))
            .unwrap()
            .map(|line| line.unwrap())
            .collect()
    };
    for sql in [
        INBOUND_SQL,
        LAST_SQL,
        SEALED_PROBE_SQL,
        THREADS_SQL,
        NAME_SQL,
        CORRECTIONS_SQL,
    ] {
        let plan = plan(sql);
        assert!(
            plan.iter().any(|l| l.starts_with("SEARCH m USING")
                && l.contains("idx_messages_from_received (account_id=? AND from_addr=?)")),
            "walked instead of seeking\n{sql}\n{plan:#?}"
        );
        assert!(
            !plan.iter().any(|l| l.starts_with("SCAN m")),
            "a scan of messages\n{sql}\n{plan:#?}"
        );
        // The newest window is the index's own tail, not a sort of the
        // sender's whole history.
        assert!(
            !plan.iter().any(|l| l.contains("TEMP B-TREE FOR ORDER BY")),
            "sorted the sender's mail\n{sql}\n{plan:#?}"
        );
    }
    // The count is index-only: no table row, no probe, per message.
    let inbound = plan(INBOUND_SQL);
    assert!(
        inbound.len() == 1 && inbound[0].contains("COVERING INDEX"),
        "{inbound:#?}"
    );
    // Every walk that reads rows is bounded to the recent window.
    for sql in [THREADS_SQL, NAME_SQL, CORRECTIONS_SQL] {
        assert!(sql.contains(&format!("LIMIT {RECENT_WINDOW})")), "{sql}");
    }
    let corrections = plan(CORRECTIONS_SQL);
    assert!(
        corrections
            .iter()
            .any(|l| l.starts_with("SEARCH c USING") && l.contains("message_id=?")),
        "each correction is probed by its message\n{corrections:#?}"
    );
    let threads = plan(THREADS_SQL);
    assert!(
        threads
            .iter()
            .any(|l| l.contains("SEARCH s USING") && l.contains("idx_messages_thread")),
        "each reply probe seeks its thread\n{threads:#?}"
    );
    // The contacts read seeks its NOCASE index, for the sum and the name.
    let contact = plan(CONTACT_SQL);
    assert!(
        contact
            .iter()
            .filter(
                |l| l.contains("USING COVERING INDEX idx_contacts_addr_nocase")
                    || l.contains("USING INDEX idx_contacts_addr_nocase")
            )
            .count()
            >= 2,
        "{contact:#?}"
    );
    assert!(
        !contact.iter().any(|l| l.starts_with("SCAN")),
        "{contact:#?}"
    );
    // Each source of the full walk is a range seek from the cursor, never a
    // scan or a sort of the whole table (the quadratic walk this replaced).
    for sql in WALK_SOURCES {
        let walk = plan(sql);
        assert!(
            walk.iter()
                .any(|l| l.starts_with("SEARCH") && l.contains("addr>?")),
            "{sql}\n{walk:#?}"
        );
        assert!(
            !walk
                .iter()
                .any(|l| l.starts_with("SCAN") || l.contains("TEMP B-TREE")),
            "{sql}\n{walk:#?}"
        );
    }
    // The sealed count reads only the sealed rows, each by its partial index,
    // and reaches messages by key: never a walk of the sender's mail.
    let sealed = plan(SEALED_COUNT_SQL);
    for index in [
        "idx_triage_sealed",
        "idx_agent_decisions_sealed",
        "idx_agent_state_restricted",
        "idx_agent_corrections_sealed",
    ] {
        assert!(
            sealed.iter().any(|l| l.contains(index)),
            "{index}\n{sealed:#?}"
        );
    }
    assert!(
        sealed
            .iter()
            .any(|l| l.starts_with("SEARCH m USING INTEGER PRIMARY KEY")),
        "{sealed:#?}"
    );
    assert!(
        !sealed.iter().any(|l| l.starts_with("SCAN m")),
        "{sealed:#?}"
    );
    let initiated = plan(INITIATED_SQL);
    assert!(
        initiated
            .iter()
            .any(|l| l.contains("SEARCH r USING") && l.contains("idx_message_recipients_addr")),
        "{initiated:#?}"
    );
    assert!(
        !initiated
            .iter()
            .any(|l| l.starts_with("SCAN m") || l.starts_with("SCAN r")),
        "{initiated:#?}"
    );
}

#[test]
fn an_unchanged_recount_skips_the_window_but_refreshes_attention() {
    let (store, acct) = store();
    ledgers_since(&store, acct, 60, 60);
    let id = inbound(&store, acct, "a", "a", DAN, 2);
    store.recompute_relationship(acct, DAN).unwrap();
    assert_eq!(
        store.relationship(acct, DAN).unwrap().unwrap().corrections,
        0
    );

    // A correction changes no count, so a plain mark's recount keeps the
    // stored corrections; the open it also brings is refreshed.
    store
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO agent_triage_corrections(account_id, message_id, field, value_json,
                 source_revision, updated_at)
             VALUES(?1, ?2, 'fye', 'true', 1, ?3)",
            params![acct, id, Utc::now().to_rfc3339()],
        )
        .unwrap();
    store
        .acknowledge_agent_message(acct, id, Utc::now())
        .unwrap();
    store.record_message_attention(acct, id, 5_000, 2).unwrap();
    store.drain_relationship_dirty(acct, 10).unwrap();
    let light = store.relationship(acct, DAN).unwrap().unwrap();
    assert_eq!(light.corrections, 0, "the shortcut did not walk the window");
    assert_eq!(
        (light.opened_count, light.link_clicks),
        (1, 2),
        "attention moved"
    );

    // A full recount (the nightly walk, or a full mark) finds it.
    store.recompute_all_relationships(acct).unwrap();
    assert_eq!(
        store.relationship(acct, DAN).unwrap().unwrap().corrections,
        1
    );
}

#[test]
fn an_address_that_fails_its_recount_is_skipped_not_left_blocking_the_drain() {
    let (store, acct) = store();
    inbound(&store, acct, "a", "a", "aaa@example.com", 1);
    inbound(&store, acct, "b", "b", "bbb@example.com", 1);
    store.recompute_all_relationships(acct).unwrap();
    // A stored row that will not read back makes its own recount fail; it
    // sorts first, where it would hold every later mark forever.
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE relationships SET computed_at = 'garbage' WHERE addr = 'aaa@example.com'",
            [],
        )
        .unwrap();
    inbound(&store, acct, "a2", "a2", "aaa@example.com", 1);
    inbound(&store, acct, "b2", "b2", "bbb@example.com", 1);
    for addr in ["aaa@example.com", "bbb@example.com"] {
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO relationship_dirty(account_id, addr) VALUES(?1, ?2)",
                params![acct, addr],
            )
            .unwrap();
    }
    assert_eq!(dirty(&store, acct), ["aaa@example.com", "bbb@example.com"]);

    assert_eq!(store.drain_relationship_dirty(acct, 10).unwrap(), 2);
    assert!(dirty(&store, acct).is_empty(), "the failing mark went too");
    let stuck: String = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT computed_at FROM relationships WHERE addr = 'aaa@example.com'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stuck, "garbage", "control: the first address really failed");
    assert_eq!(
        store
            .relationship(acct, "bbb@example.com")
            .unwrap()
            .unwrap()
            .received_count,
        2,
        "the address after it was recounted"
    );
}

#[test]
fn a_decision_that_seals_mail_marks_its_sender_for_a_full_recount() {
    use super::reading_senders::into_reading_with;
    let (store, acct) = store();
    let ordinary = triaged(acct, "a", "t1")
        .from(DAN)
        .received_at(days_ago(2))
        .ingest(&store);
    let code = triaged(acct, "b", "t2")
        .from(DAN)
        .received_at(days_ago(1))
        .ingest(&store);
    into_reading_with(&store, acct, ordinary, |_| {});
    // Drained while the second is still pending: counted, as the module
    // header says it must be.
    store.drain_relationship_dirty(acct, 10).unwrap();
    let before = store.relationship(acct, DAN).unwrap().unwrap();
    assert_eq!((before.received_count, before.inbound_threads), (2, 2));

    into_reading_with(&store, acct, code, |d| d.external_access.restricted = true);
    let full: bool = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT full FROM relationship_dirty WHERE account_id = ?1 AND addr = ?2",
            params![acct, DAN],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        full,
        "the counts do not move, so the mark must refuse the shortcut"
    );
    store.drain_relationship_dirty(acct, 10).unwrap();
    let after = store.relationship(acct, DAN).unwrap().unwrap();
    assert_eq!(
        (after.received_count, after.inbound_threads),
        (1, 1),
        "the sealed message leaves the count and its thread within a round"
    );

    // And back: a re-triage that lifts the restriction unseals it, which
    // moves no count either, so that mark must be full too.
    store
        .enqueue_agent_triage(acct, code, "test-unseal", false)
        .unwrap();
    let job = store
        .claim_agent_job(acct, "triage", Utc::now() + Duration::days(2), 60)
        .unwrap()
        .expect("the re-triage is claimable");
    assert_eq!(job.message_id, code);
    let context = store.load_agent_context(&job).unwrap();
    let decision = crate::triage::decision::MessageDecision {
        summary: "Not a code after all".into(),
        ..Default::default()
    };
    store
        .commit_agent_decision(
            &job,
            &context,
            &decision,
            std::slice::from_ref(&context.message.source),
        )
        .unwrap();
    store.drain_relationship_dirty(acct, 10).unwrap();
    let unsealed = store.relationship(acct, DAN).unwrap().unwrap();
    assert_eq!(
        (unsealed.received_count, unsealed.inbound_threads),
        (2, 2),
        "the unsealed message is counted again within a round"
    );
}

#[test]
fn a_reply_to_a_reply_to_address_still_credits_the_sender_who_wrote() {
    let (store, acct) = store();
    triaged(acct, "n", "t1")
        .from("noreply@vendor.example")
        .received_at(days_ago(2))
        .ingest(&store);
    store.drain_relationship_dirty(acct, 10).unwrap();
    assert_eq!(
        store
            .relationship(acct, "noreply@vendor.example")
            .unwrap()
            .unwrap()
            .replied_threads,
        0
    );
    // The owner answers at the Reply-To address, not at the From.
    sent(
        &store,
        acct,
        "r",
        "t1",
        "support@vendor.example",
        days_ago(1),
    );
    assert!(
        dirty(&store, acct).contains(&"noreply@vendor.example".to_string()),
        "{:?}",
        dirty(&store, acct)
    );
    store.drain_relationship_dirty(acct, 10).unwrap();
    assert_eq!(
        store
            .relationship(acct, "noreply@vendor.example")
            .unwrap()
            .unwrap()
            .replied_threads,
        1
    );
}

#[test]
fn the_sent_history_harvest_marks_the_contacts_it_moves() {
    let (store, acct) = store();
    store
        .merge_harvested_contacts(
            acct,
            &[ContactEntry {
                addr: "Ann@Fund.com".into(),
                display_name: None,
                sent_count: 4,
                last_sent_at: Some(days_ago(3)),
            }],
        )
        .unwrap();
    assert_eq!(dirty(&store, acct), ["ann@fund.com"]);
    store.drain_relationship_dirty(acct, 10).unwrap();
    assert_eq!(
        store
            .relationship(acct, "ann@fund.com")
            .unwrap()
            .unwrap()
            .sent_count,
        4
    );
}

#[test]
fn sent_count_sums_every_spelling_of_the_address() {
    let (store, acct) = store();
    // The harvest lowercases; a per-message Sent ingest keeps the header's
    // spelling. Under a BINARY key those are two rows for one person.
    store
        .merge_harvested_contacts(
            acct,
            &[
                ContactEntry {
                    addr: "ann@fund.com".into(),
                    display_name: Some("Ann Lee".into()),
                    sent_count: 3,
                    last_sent_at: Some(days_ago(9)),
                },
                ContactEntry {
                    addr: "Ann@Fund.com".into(),
                    display_name: None,
                    sent_count: 2,
                    last_sent_at: Some(days_ago(2)),
                },
            ],
        )
        .unwrap();
    store.recompute_relationship(acct, "ann@fund.com").unwrap();
    let row = store.relationship(acct, "ann@fund.com").unwrap().unwrap();
    assert_eq!(row.sent_count, 5);
    let last = row.sent_last_at.unwrap();
    assert!((last - days_ago(2)).num_minutes().abs() < 5, "{last}");
    assert_eq!(row.display_name.as_deref(), Some("Ann Lee"));
}

#[test]
fn a_message_whose_date_will_not_parse_still_counts() {
    let (store, acct) = store();
    inbound(&store, acct, "good", "good", DAN, 3);
    let bad = inbound(&store, acct, "bad", "bad", DAN, 1);
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE messages SET received_at = 'garbage' WHERE id = ?1",
            [bad],
        )
        .unwrap();
    store.recompute_relationship(acct, DAN).unwrap();
    let row = store.relationship(acct, DAN).unwrap().unwrap();
    assert_eq!(row.received_count, 2);
    assert_eq!(row.inbound_threads, 2, "the undatable thread is still one");
    // MAX(received_at) is the unparseable text; the dated thread stands in.
    assert!(row.received_first_at.is_some() && row.received_last_at.is_some());
}

/// The sealed count is `known_sealed_mail_sql!` read from the other side:
/// for mail sealed by each of the macro's four arms, the two must agree, so
/// a new way to seal a message cannot reach the macro and miss the ledger.
#[test]
fn the_sealed_count_agrees_with_the_macro_arm_by_arm() {
    let (store, acct) = store();
    let ids: Vec<i64> = (0..6)
        .map(|n| inbound(&store, acct, &format!("m{n}"), &format!("t{n}"), DAN, 1))
        .collect();
    let conn = store.lock().unwrap();
    let now = Utc::now().to_rfc3339();
    // Arm 1: the retired sensitivity column.
    conn.execute(
        "UPDATE triage SET sensitivity = 'sealed' WHERE message_id = ?1",
        [ids[0]],
    )
    .unwrap();
    // Arm 2: an auth decision, and a restricting one.
    for (id, json) in [
        (ids[1], r#"{"auth":{"kinds":["otp"]}}"#),
        (ids[2], r#"{"external_access":{"restricted":true}}"#),
    ] {
        conn.execute(
            "INSERT INTO agent_message_decisions(account_id, message_id, revision, decision_json, decided_at)
             VALUES(?1, ?2, 1, ?3, ?4)",
            params![acct, id, json, now],
        )
        .unwrap();
    }
    // Arm 3: the access check said restricted.
    conn.execute(
        "INSERT INTO agent_message_state(account_id, message_id, content_snapshot, access)
         VALUES(?1, ?2, '', 'restricted')
         ON CONFLICT(account_id, message_id) DO UPDATE SET access = 'restricted'",
        params![acct, ids[3]],
    )
    .unwrap();
    // Arm 4: the owner sealed it, and NOTHING ELSE did, so a broken
    // correction arm loses this message. And a sixth sealed by the owner AND
    // by arm 1, which the UNION must count once.
    for id in [ids[4], ids[5]] {
        conn.execute(
            "INSERT INTO agent_triage_corrections(account_id, message_id, field, value_json,
                 source_revision, updated_at)
             VALUES(?1, ?2, 'external_access', 'true', 1, ?3)",
            params![acct, id, now],
        )
        .unwrap();
    }
    conn.execute(
        "UPDATE triage SET sensitivity = 'sealed' WHERE message_id = ?1",
        [ids[5]],
    )
    .unwrap();
    let ours: i64 = conn
        .query_row(SEALED_COUNT_SQL, params![acct, DAN], |r| r.get(0))
        .unwrap();
    // The small-sender probe is the macro itself; the two must agree.
    let probed: i64 = conn
        .query_row(SEALED_PROBE_SQL, params![acct, DAN], |r| r.get(0))
        .unwrap();
    assert_eq!(probed, ours, "the probe and the union disagree");
    let macros: i64 = conn
        .query_row(
            concat!(
                "SELECT COUNT(*) FROM messages m LEFT JOIN triage t ON t.message_id = m.id
                 WHERE m.account_id = ?1 AND m.from_addr = ?2 COLLATE NOCASE AND ",
                known_sealed_mail_sql!()
            ),
            params![acct, DAN],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!((ours, macros), (6, 6));
    drop(conn);
    inbound(&store, acct, "open", "open", DAN, 1);
    store.recompute_relationship(acct, DAN).unwrap();
    assert_eq!(
        store
            .relationship(acct, DAN)
            .unwrap()
            .unwrap()
            .received_count,
        1
    );
}
