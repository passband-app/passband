//! History retention: run diagnostics and fast-lane assessments past
//! `HISTORY_RETENTION_DAYS` go; the window, and the revisit answers the
//! revisit pass reads, stay.

use super::super::retention::{ASSESSMENTS, PRUNE_CHUNK_ROWS, RUNS};
use super::super::*;
use super::support::*;
use chrono::Duration;

fn ago(now: DateTime<Utc>, days: i64) -> String {
    (now - Duration::days(days)).to_rfc3339()
}

fn job(store: &SqliteStore, acct: AccountId, message: i64, trigger: &str) -> i64 {
    let conn = store.lock().unwrap();
    conn.execute(
        "INSERT INTO agent_triage_jobs(account_id,message_id,kind,trigger,input_revision,state,available_at)
         VALUES(?1,?2,'triage',?3,1,'completed',?4)",
        params![acct, message, trigger, Utc::now().to_rfc3339()],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn run(store: &SqliteStore, job: i64, outcome: &str, at: &str) -> i64 {
    let conn = store.lock().unwrap();
    conn.execute(
        "INSERT INTO agent_triage_runs(job_id,account_id,message_id,outcome,completed_at,metadata_json)
         SELECT id,account_id,message_id,?2,?3,'{\"config\":{}}' FROM agent_triage_jobs WHERE id=?1",
        params![job, outcome, at],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn assessment(store: &SqliteStore, acct: AccountId, message: i64, at: &str) -> i64 {
    let conn = store.lock().unwrap();
    conn.execute(
        "INSERT INTO notification_assessments(account_id,message_id,lane,is_auth,importance,
             one_line,reason,model,prompt_version,assessed_at)
         VALUES(?1,?2,'fast',0,40,'A line','A reason','m','v1',?3)",
        params![acct, message, at],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn ids(store: &SqliteStore, table: &str) -> Vec<i64> {
    let conn = store.lock().unwrap();
    let mut stmt = conn
        .prepare(&format!("SELECT id FROM {table} ORDER BY id"))
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .map(|id| id.unwrap())
        .collect()
}

/// A pass that must succeed: what it did.
fn prune(store: &SqliteStore, now: DateTime<Utc>) -> HistoryPruned {
    let (pruned, outcome) = store.prune_history(now);
    outcome.unwrap();
    pruned
}

fn metadata(store: &SqliteStore, run: i64) -> String {
    store
        .lock()
        .unwrap()
        .query_row(
            "SELECT metadata_json FROM agent_triage_runs WHERE id=?1",
            [run],
            |r| r.get(0),
        )
        .unwrap()
}

fn backlog(store: &SqliteStore, acct: AccountId, job: i64, message: i64, rows: i64, at: &str) {
    store
        .lock()
        .unwrap()
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < ?3)
             INSERT INTO agent_triage_runs(job_id,account_id,message_id,outcome,completed_at)
             SELECT ?1, ?2, ?4, 'applied', ?5 FROM n",
            params![job, acct, rows, message, at],
        )
        .unwrap();
}

/// What goes is what nothing decides on and the window has passed: every
/// run of an ordinary job, a revisit's runs that did not answer it, a
/// message's older answers, and old assessments, in every account. What stays
/// is the last 90 days and each message's newest answered revisit, which the
/// revisit pass reads as "already answered" and would otherwise pay to ask
/// again; past the window it keeps only what that pass reads.
#[test]
fn history_retention_keeps_the_window_and_the_newest_revisit_answer() {
    let (store, acct) = store();
    let other = store.ensure_account("other@example.com").unwrap();
    let now = Utc::now();
    let message = ingest_normal(&store, acct, "g-ret", "t-ret", Tier::Signal, 50, now);
    let theirs = ingest_normal(&store, other, "g-oth", "t-oth", Tier::Signal, 50, now);
    let ingest = job(&store, acct, message, "ingest");
    let first_ask = job(
        &store,
        acct,
        message,
        &format!("revisit:event_ended:{}", ago(now, 130)),
    );
    let second_ask = job(
        &store,
        acct,
        message,
        &format!("revisit:event_ended:{}", ago(now, 120)),
    );
    let their_ingest = job(&store, other, theirs, "ingest");

    let old_applied = run(&store, ingest, "applied", &ago(now, 200));
    let old_stale = run(&store, ingest, "stale", &ago(now, 150));
    let older_answer = run(&store, first_ask, "applied", &ago(now, 129));
    let newest_answer = run(&store, second_ask, "applied", &ago(now, 119));
    let unanswered = run(&store, second_ask, "stale", &ago(now, 118));
    let their_old = run(&store, their_ingest, "applied", &ago(now, 100));
    let just_out = run(&store, ingest, "completed", &ago(now, 91));
    let just_in = run(&store, ingest, "applied", &ago(now, 89));
    let their_recent = run(&store, their_ingest, "applied", &ago(now, 2));
    let today = run(&store, ingest, "applied", &ago(now, 0));

    let old_assessment = assessment(&store, acct, message, &ago(now, 100));
    let recent_assessment = assessment(&store, acct, message, &ago(now, 3));
    // A clock that ran ahead stamped this one; it is no reason to stop.
    let future_assessment = assessment(&store, other, theirs, &ago(now, -30));

    let pruned = prune(&store, now);
    assert_eq!(
        pruned,
        HistoryPruned {
            runs: 6,
            runs_trimmed: 1,
            assessments: 1,
        }
    );
    assert_eq!(
        ids(&store, "agent_triage_runs"),
        vec![newest_answer, just_in, their_recent, today],
        "gone: {old_applied} {old_stale} {older_answer} {unanswered} {their_old} {just_out}"
    );
    assert_eq!(
        metadata(&store, newest_answer),
        "{}",
        "slimmed past the window"
    );
    assert_eq!(
        metadata(&store, just_in),
        "{\"config\":{}}",
        "whole inside it"
    );
    assert_eq!(
        ids(&store, "notification_assessments"),
        vec![recent_assessment, future_assessment],
        "gone: {old_assessment}"
    );
    // The jobs are the dedupe and are never touched.
    assert_eq!(
        ids(&store, "agent_triage_jobs"),
        vec![ingest, first_ask, second_ask, their_ingest]
    );

    // A second pass the same day finds nothing.
    assert_eq!(prune(&store, now), HistoryPruned::default());
}

/// A first pass on a store with a long history clears it a page at a time,
/// all of it. A clock that stepped back is no reason to delete inside the
/// window: a recent run stored below an older one stays, and the old one
/// stored after it waits for a later pass. A clock that ran ahead does not
/// stop the walk at the row it stamped in the future.
#[test]
fn history_retention_clears_a_backlog_and_never_inside_the_window() {
    let (store, acct) = store();
    let now = Utc::now();
    let message = ingest_normal(&store, acct, "g-big", "t-big", Tier::Signal, 50, now);
    let ingest = job(&store, acct, message, "ingest");
    let rows = 2 * PRUNE_CHUNK_ROWS + 500;
    backlog(&store, acct, ingest, message, rows, &ago(now, 300));
    let future = run(&store, ingest, "applied", &ago(now, -400));
    let old_after_future = run(&store, ingest, "applied", &ago(now, 300));
    let skewed_recent = run(&store, ingest, "applied", &ago(now, 2));
    let old_after_recent = run(&store, ingest, "applied", &ago(now, 300));
    let today = run(&store, ingest, "applied", &ago(now, 0));

    let pruned = prune(&store, now);
    assert_eq!(pruned.runs, rows as u64 + 1, "{old_after_future} gone");
    assert_eq!(
        ids(&store, "agent_triage_runs"),
        vec![future, skewed_recent, old_after_recent, today]
    );

    // A quarter later, with mail still arriving: the window has passed the
    // skewed run, so it goes, and so does the old one stored after it. The
    // future-stamped run is still younger than the window and stays.
    let later = now + Duration::days(HISTORY_RETENTION_DAYS) + Duration::hours(12);
    let fresh = run(&store, ingest, "applied", &later.to_rfc3339());
    assert_eq!(prune(&store, later).runs, 3);
    assert_eq!(ids(&store, "agent_triage_runs"), vec![future, fresh]);
}

/// Each page deletes at most `PRUNE_CHUNK_ROWS`, in its own transaction under
/// its own hold of the lock, and the walk ends at the page that reaches the
/// window.
#[test]
fn history_retention_deletes_a_page_at_a_time() {
    let (store, acct) = store();
    let now = Utc::now();
    let message = ingest_normal(&store, acct, "g-pg", "t-pg", Tier::Signal, 50, now);
    let ingest = job(&store, acct, message, "ingest");
    let rows = 2 * PRUNE_CHUNK_ROWS + PRUNE_CHUNK_ROWS / 2;
    backlog(&store, acct, ingest, message, rows, &ago(now, 200));
    run(&store, ingest, "applied", &ago(now, 0));
    let cutoff = (now - Duration::days(HISTORY_RETENTION_DAYS)).to_rfc3339();
    let now_text = now.to_rfc3339();
    let mut steps = Vec::new();
    let mut after = 0;
    loop {
        let step = store.prune_page(&RUNS, after, &cutoff, &now_text).unwrap();
        steps.push(step.deleted);
        match step.next {
            Some(next) => after = next,
            None => break,
        }
    }
    let full = PRUNE_CHUNK_ROWS as u64;
    assert_eq!(steps, vec![full, full, full / 2]);
}

/// The clock is trusted: a pass on a clock a year ahead takes everything
/// it says is old, except the row something decides on. The newest answered
/// revisit stays whatever its age, and so the revisit pass never pays twice
/// for a wrong clock.
#[test]
fn a_clock_far_ahead_never_takes_the_kept_answer() {
    let (store, acct) = store();
    let now = Utc::now();
    let message = ingest_normal(&store, acct, "g-clk", "t-clk", Tier::Signal, 50, now);
    let ingest = job(&store, acct, message, "ingest");
    let ask = job(
        &store,
        acct,
        message,
        &format!("revisit:event_ended:{}", ago(now, 2)),
    );
    let answer = run(&store, ask, "applied", &ago(now, 1));
    run(&store, ingest, "applied", &ago(now, 0));
    assessment(&store, acct, message, &ago(now, 0));

    let ahead = now + Duration::days(365);
    assert_eq!(
        prune(&store, ahead),
        HistoryPruned {
            runs: 1,
            runs_trimmed: 1,
            assessments: 1,
        }
    );
    assert_eq!(ids(&store, "agent_triage_runs"), vec![answer]);
}

/// Best effort: a table that fails does not hold the other back, the error
/// comes back, and the pages committed before it still count.
#[test]
fn a_failing_table_does_not_hold_the_other_back() {
    let (store, acct) = store();
    let now = Utc::now();
    let message = ingest_normal(&store, acct, "g-err", "t-err", Tier::Signal, 50, now);
    let ingest = job(&store, acct, message, "ingest");
    run(&store, ingest, "applied", &ago(now, 200));
    assessment(&store, acct, message, &ago(now, 200));
    let kept = assessment(&store, acct, message, &ago(now, 0));
    store.execute_batch_for_test(
        "CREATE TEMP TRIGGER no_run_deletes BEFORE DELETE ON agent_triage_runs
         BEGIN SELECT RAISE(FAIL, 'injected'); END;",
    );

    let (pruned, outcome) = store.prune_history(now);
    assert!(outcome.is_err());
    assert_eq!(
        pruned,
        HistoryPruned {
            runs: 0,
            runs_trimmed: 0,
            assessments: 1,
        }
    );
    assert_eq!(ids(&store, "notification_assessments"), vec![kept]);
}

#[test]
fn history_retention_on_empty_tables_is_a_no_op() {
    let (store, _) = store();
    assert_eq!(prune(&store, Utc::now()), HistoryPruned::default());
}

/// Every step seeks by rowid and none scans a table, so a pass costs what it
/// walks and deletes, not the size of the table: the page reads up from where
/// the last one ended, in rowid order with no sort, and the delete and trim
/// take that range. The revisit exception probes the job by its key and a
/// message's other runs by `idx_agent_triage_runs_message`.
#[test]
fn history_retention_seeks_by_rowid() {
    let (store, _) = store();
    let conn = store.lock().unwrap();
    for (table, name, alias) in [
        (&RUNS, "agent_triage_runs", "r"),
        (
            &ASSESSMENTS,
            "notification_assessments",
            "notification_assessments",
        ),
    ] {
        let steps = plan(&conn, table.page);
        assert!(
            steps
                .iter()
                .any(|s| s == &format!("SEARCH {name} USING INTEGER PRIMARY KEY (rowid>?)")),
            "{steps:?}"
        );
        // The only scan is of the page itself, at most a chunk of rows.
        assert!(
            !steps
                .iter()
                .any(|s| s.starts_with(&format!("SCAN {name}")) || s.contains("TEMP B-TREE")),
            "{steps:?}"
        );
        let steps = plan(&conn, table.delete);
        assert!(
            steps
                .iter()
                .any(|s| s
                    == &format!("SEARCH {alias} USING INTEGER PRIMARY KEY (rowid>? AND rowid<?)")),
            "{steps:?}"
        );
        assert!(!steps.iter().any(|s| s.starts_with("SCAN")), "{steps:?}");
    }
    let steps = plan(&conn, RUNS.delete);
    for probe in [
        "SEARCH j USING INTEGER PRIMARY KEY (rowid=?)",
        "SEARCH nj USING INTEGER PRIMARY KEY (rowid=?)",
    ] {
        assert!(steps.iter().any(|s| s == probe), "{probe}: {steps:?}");
    }
    assert!(
        steps.iter().any(|s| s.starts_with("SEARCH n USING")
            && s.contains("idx_agent_triage_runs_message")),
        "{steps:?}"
    );
    let steps = plan(&conn, RUNS.trim.unwrap());
    assert_eq!(
        steps,
        vec!["SEARCH agent_triage_runs USING INTEGER PRIMARY KEY (rowid>? AND rowid<?)"]
    );
}
