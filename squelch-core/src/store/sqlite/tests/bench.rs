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
