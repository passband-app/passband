//! Timing probes for the relationship ledger, IGNORED by default: they build
//! a synthetic mailbox the shape of a GitHub-notifications firehose and print
//! numbers rather than assert them (the asserted bounds are the plan tests in
//! `relationships`). Run with
//!
//! ```text
//! cargo test --release -p squelch-core --lib store::sqlite::tests::relationships_bench \
//!     -- --ignored --nocapture --test-threads=1
//! ```
//!
//! THE SHAPE: 240,000 inbound messages over a year, 40,000 of them from one
//! sender in 8,000 threads (five notifications a thread), the rest from
//! 30,000 other senders, a triage row on every message, an owner reply on one
//! thread in fifty, and a `senders` directory row per address.

use super::super::reading_senders::SenderMeasurer;
use super::super::relationships::{
    CHUNK_SQL, CORRECTIONS_SQL, INBOUND_SQL, INITIATED_SQL, NAME_SQL, THREADS_SQL,
};
use super::super::*;
use super::support::*;
use std::time::{Duration, Instant};

const FIREHOSE: &str = "notifications@github.com";
const FIREHOSE_MESSAGES: usize = 40_000;
const OTHER_MESSAGES: usize = 200_000;
const OTHER_SENDERS: usize = 30_000;

fn build(store: &SqliteStore, acct: AccountId) {
    let mut conn = store.lock().unwrap();
    let tx = conn.transaction().unwrap();
    let start = Utc::now() - chrono::Duration::days(365);
    let total = FIREHOSE_MESSAGES + OTHER_MESSAGES;
    {
        let mut msg = tx
            .prepare(
                "INSERT INTO messages(account_id, gmail_msg_id, thread_id, from_addr, from_name,
                     subject, received_at, snippet, body, is_sent)
                 VALUES(?1, ?2, ?3, ?4, ?5, 's', ?6, '', 'a few words of body text', ?7)",
            )
            .unwrap();
        let mut tri = tx
            .prepare("INSERT INTO triage(message_id, account_id, created_at) VALUES(?1, ?2, ?3)")
            .unwrap();
        for n in 0..total {
            // Interleaved in time, so the firehose's rows are spread through
            // the index the way a year of arrivals spreads them.
            let at = start + chrono::Duration::seconds((n as i64) * 365 * 86_400 / total as i64);
            let (from, thread) = if n % 6 == 0 && n / 6 < FIREHOSE_MESSAGES {
                (FIREHOSE.to_string(), format!("gh-{}", (n / 6) / 5))
            } else {
                let s = n % OTHER_SENDERS;
                (format!("person{s}@example.com"), format!("t-{n}"))
            };
            msg.execute(params![
                acct,
                format!("g{n}"),
                thread,
                from,
                "Name",
                at.to_rfc3339(),
                0
            ])
            .unwrap();
            let id = tx.last_insert_rowid();
            tri.execute(params![id, acct, at.to_rfc3339()]).unwrap();
            if n % 50 == 0 {
                let reply = at + chrono::Duration::minutes(30);
                msg.execute(params![
                    acct,
                    format!("r{n}"),
                    thread,
                    "me@example.com",
                    "Me",
                    reply.to_rfc3339(),
                    1
                ])
                .unwrap();
            }
        }
        let mut sender = tx
            .prepare(
                "INSERT OR IGNORE INTO senders(account_id, addr, msg_count, first_seen, last_received_at)
                 VALUES(?1, ?2, 1, ?3, ?3)",
            )
            .unwrap();
        for s in 0..OTHER_SENDERS {
            sender
                .execute(params![
                    acct,
                    format!("person{s}@example.com"),
                    start.to_rfc3339()
                ])
                .unwrap();
        }
        sender
            .execute(params![acct, FIREHOSE, start.to_rfc3339()])
            .unwrap();
    }
    tx.commit().unwrap();
    conn.execute_batch("ANALYZE").unwrap();
}

/// Best of `rounds` runs of `f`.
fn best(rounds: usize, mut f: impl FnMut()) -> Duration {
    (0..rounds)
        .map(|_| {
            let start = Instant::now();
            f();
            start.elapsed()
        })
        .min()
        .unwrap()
}

#[test]
#[ignore]
fn firehose_recompute() {
    let (store, acct) = store();
    build(&store, acct);
    {
        let conn = store.lock().unwrap();
        for (name, sql) in [
            ("INBOUND", INBOUND_SQL),
            ("THREADS", THREADS_SQL),
            ("NAME", NAME_SQL),
        ] {
            let mut plan = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            let lines: Vec<String> = plan
                .query_map(params![acct, FIREHOSE], |r| r.get::<_, String>(3))
                .unwrap()
                .map(|l| l.unwrap())
                .collect();
            let mut stmt = conn.prepare(sql).unwrap();
            let took = best(5, || {
                let mut rows = stmt.query(params![acct, FIREHOSE]).unwrap();
                while rows.next().unwrap().is_some() {}
            });
            eprintln!("BENCH {name}: {took:?}\n  {}", lines.join("\n  "));
        }
    }
    {
        let conn = store.lock().unwrap();
        for (name, sql) in [
            ("CORRECTIONS", CORRECTIONS_SQL),
            ("INITIATED", INITIATED_SQL),
        ] {
            let mut stmt = conn.prepare(sql).unwrap();
            let took = best(5, || {
                let mut rows = stmt.query(params![acct, FIREHOSE]).unwrap();
                while rows.next().unwrap().is_some() {}
            });
            eprintln!("BENCH {name}: {took:?}");
        }
        let took = best(3, || {
            SenderMeasurer::new(&conn, acct, Utc::now())
                .unwrap()
                .measure(FIREHOSE)
                .unwrap();
        });
        eprintln!("BENCH SenderMeasurer::measure (reading_senders, unchanged): {took:?}");
    }
    let cold = best(1, || store.recompute_relationship(acct, FIREHOSE).unwrap());
    eprintln!("BENCH recompute (first, full): {cold:?}");
    let again = best(5, || {
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT OR IGNORE INTO relationship_dirty(account_id, addr) VALUES(?1, ?2)",
                params![acct, FIREHOSE],
            )
            .unwrap();
        store.drain_relationship_dirty(acct, 10).unwrap();
    });
    eprintln!("BENCH re-marked drain, counts unchanged: {again:?}");
    let row = store.relationship(acct, FIREHOSE).unwrap().unwrap();
    eprintln!(
        "BENCH row: received {} threads {} replied {}",
        row.received_count, row.inbound_threads, row.replied_threads
    );
}

#[test]
#[ignore]
fn full_walk_chunks() {
    let (store, acct) = store();
    build(&store, acct);
    {
        let conn = store.lock().unwrap();
        let mut plan = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {CHUNK_SQL}"))
            .unwrap();
        let lines: Vec<String> = plan
            .query_map(params![acct, "person5000@example.com", 25], |r| {
                r.get::<_, String>(3)
            })
            .unwrap()
            .map(|l| l.unwrap())
            .collect();
        let mut stmt = conn.prepare(CHUNK_SQL).unwrap();
        let took = best(5, || {
            let mut rows = stmt
                .query(params![acct, "person5000@example.com", 25])
                .unwrap();
            while rows.next().unwrap().is_some() {}
        });
        eprintln!(
            "BENCH chunk address query: {took:?}\n  {}",
            lines.join("\n  ")
        );
    }
    let start = Instant::now();
    store.recompute_all_relationships(acct).unwrap();
    eprintln!(
        "BENCH full walk ({OTHER_SENDERS} senders): {:?}",
        start.elapsed()
    );
}
