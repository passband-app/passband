//! Timing probes for the store's write paths, IGNORED by default: they fsync a
//! file-backed store and print numbers rather than assert them, so a change to
//! a write path can be measured rather than argued. Run with
//!
//! ```text
//! cargo test -p squelch-core --lib store::sqlite::tests::bench -- --ignored --nocapture
//! ```
//!
//! and, to count the syncs, under `strace -f -c -e trace=fsync,fdatasync` on
//! the test binary; `SQUELCH_BENCH_SKIP=1` runs only the setup, so the
//! difference between the two runs is the probe's own count.

use super::super::*;
use super::support::*;
use crate::store::agent_triage::AgentTriageStore;
use std::time::{Duration, Instant};

fn skip_measured() -> bool {
    std::env::var_os("SQUELCH_BENCH_SKIP").is_some()
}

/// What ingest's embed-at-write pays: one vector per call, one call per
/// message. (The drain's shape is `vector_batches`, below.)
#[test]
#[ignore]
fn vector_upserts() {
    let (store, acct, dir) = file_store("vectors");
    let ids: Vec<i64> = (0..64)
        .map(|i| triaged(acct, &format!("g{i}"), &format!("t{i}")).upsert(&store))
        .collect();
    if !skip_measured() {
        let vec = vec![0.25f32; VEC_DIMS];
        let mut best = Duration::MAX;
        for _ in 0..3 {
            let start = Instant::now();
            for id in &ids {
                store.upsert_message_vector(acct, *id, &vec).unwrap();
            }
            best = best.min(start.elapsed());
        }
        eprintln!(
            "BENCH vector_upserts: 64 upserts, best of 3 rounds: {best:?} ({:?} per vector)",
            best / 64
        );
    }
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}

/// What the backfill drain pays per batch of eight, written the way it writes
/// them: one call per batch.
#[test]
#[ignore]
fn vector_batches() {
    let (store, acct, dir) = file_store("vector-batches");
    let ids: Vec<i64> = (0..64)
        .map(|i| triaged(acct, &format!("g{i}"), &format!("t{i}")).upsert(&store))
        .collect();
    if !skip_measured() {
        let vec = vec![0.25f32; VEC_DIMS];
        let mut best = Duration::MAX;
        for _ in 0..3 {
            let start = Instant::now();
            for batch in ids.chunks(8) {
                let items: Vec<(i64, &[f32])> =
                    batch.iter().map(|id| (*id, vec.as_slice())).collect();
                store.upsert_message_vectors(acct, &items).unwrap();
            }
            best = best.min(start.elapsed());
        }
        eprintln!(
            "BENCH vector_batches: 64 vectors in 8 batches, best of 3 rounds: {best:?} ({:?} per batch)",
            best / 8
        );
    }
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}

/// What an agent read of a thread pays the first time, when every source in
/// it still wants an assessment job, and the second time, when none does.
#[test]
#[ignore]
fn agent_read_thread_enqueues() {
    let (store, acct, dir) = file_store("enqueues");
    for i in 0..10 {
        triaged(acct, &format!("g{i}"), "t")
            .from(&format!("sender{i}@external.example"))
            .upsert(&store);
    }
    if !skip_measured() {
        let start = Instant::now();
        let first = store.agent_read_thread(acct, "t", 50).unwrap();
        let cold = start.elapsed();
        let queued: i64 = store
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM agent_triage_jobs WHERE account_id = ?1",
                [acct],
                |r| r.get(0),
            )
            .unwrap();
        let start = Instant::now();
        let second = store.agent_read_thread(acct, "t", 50).unwrap();
        let warm = start.elapsed();
        eprintln!(
            "BENCH agent_read_thread: {} messages; first read enqueued {queued} jobs in {cold:?}; second read {} messages in {warm:?}",
            first.len(),
            second.len()
        );
    }
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Calendar search at the per-calendar cap (docs/CALENDAR.md §10.1): 5000
/// events with long descriptions and 20 guests each, searched the way the
/// search box asks as the user types. Prints the time per search, the
/// store's lock included.
#[test]
#[ignore]
fn calendar_search_at_the_cap() {
    use crate::gcal::search::{Audience, CalendarQuery, search};
    use crate::gcal::{
        CalendarMeta, EventItem, EventRow, Guest, GuestResponse, MAX_EVENTS_PER_CALENDAR, PRIMARY,
    };
    use crate::triage::ghost::EventTime;
    let (store, acct, _dir) = file_store("calsearch");
    store
        .calendar_set_selection(acct, &[CalendarMeta::primary_only()])
        .unwrap();
    let now = Utc::now();
    let words = [
        "standup", "review", "dinner", "flight", "dentist", "offsite", "sync", "lunch",
    ];
    let items: Vec<EventItem> = (0..MAX_EVENTS_PER_CALENDAR)
        .map(|i| {
            let start = now + chrono::Duration::hours(i - MAX_EVENTS_PER_CALENDAR / 2);
            EventItem::Upsert(Box::new(EventRow {
                event_id: format!("e{i}"),
                ical_uid: None,
                tentative: false,
                summary: Some(format!("{} {i}", words[i as usize % words.len()])),
                location: Some("Conference room B, 500 Howard St".into()),
                start: EventTime::At(start.fixed_offset()),
                end: None,
                start_timezone: None,
                organizer_email: Some("boss@example.com".into()),
                organizer_unreadable: false,
                organizer_self: false,
                self_response: None,
                transparent: false,
                html_link: None,
                updated_at: None,
                color_id: None,
                description: Some("Agenda: ".repeat(250)),
                from_gmail: false,
                guests: (0..20)
                    .map(|g| Guest {
                        email: format!("person{g}@example.com"),
                        name: Some(format!("Person Number{g}")),
                        response: GuestResponse::Accepted,
                        organizer: false,
                        is_self: false,
                    })
                    .collect(),
                guests_truncated: false,
                meeting: None,
            }))
        })
        .collect();
    for page in items.chunks(250) {
        store.calendar_apply_page(acct, PRIMARY, page, now).unwrap();
    }
    for raw in [
        "d",
        "di",
        "din",
        "dinner",
        "dinner 42",
        "person",
        "zebra",
        "über",
    ] {
        let Some(query) = CalendarQuery::parse(raw) else {
            println!("{raw:>10}: no query");
            continue;
        };
        let started = Instant::now();
        let runs = 5;
        let mut total = 0;
        for _ in 0..runs {
            total = search(&store, acct, &query, Audience::Owner, true, now, 20)
                .unwrap()
                .total;
        }
        println!(
            "{raw:>10}: {:>6.1} ms per search, {total} hits",
            started.elapsed().as_secs_f64() * 1000.0 / runs as f64
        );
    }
}
