//! History retention: run diagnostics and fast-lane assessments past
//! `HISTORY_RETENTION_DAYS` go; the window, and the revisit answers the
//! revisit pass reads, stay.

use super::super::retention::{
    ASSESSMENTS_DELETE_SQL, ASSESSMENTS_PAGE_SQL, PRUNE_CHUNK_ROWS, RUNS_DELETE_SQL, RUNS_PAGE_SQL,
};
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

/// What goes is what nothing decides on and the window has passed: every
/// run outcome of an ordinary job, a revisit's runs that did not answer it,
/// and old assessments. What stays is the last 90 days and every applied
/// revisit run, which the ghost-event revisit pass reads as "already
/// answered" and would otherwise pay to ask again.
#[test]
fn history_retention_keeps_the_window_and_the_revisit_answers() {
    let (store, acct) = store();
    let now = Utc::now();
    let message = ingest_normal(&store, acct, "g-ret", "t-ret", Tier::Signal, 50, now);
    let ingest = job(&store, acct, message, "ingest");
    let revisit = job(
        &store,
        acct,
        message,
        &format!("revisit:event_ended:{}", ago(now, 120)),
    );

    let old_applied = run(&store, ingest, "applied", &ago(now, 200));
    let old_stale = run(&store, ingest, "stale", &ago(now, 150));
    let old_answer = run(&store, revisit, "applied", &ago(now, 119));
    let old_unanswered = run(&store, revisit, "stale", &ago(now, 118));
    let just_out = run(&store, ingest, "completed", &ago(now, 91));
    let just_in = run(&store, ingest, "applied", &ago(now, 89));
    let recent = run(&store, ingest, "applied", &ago(now, 1));

    let old_assessment = assessment(&store, acct, message, &ago(now, 100));
    let recent_assessment = assessment(&store, acct, message, &ago(now, 3));

    let pruned = store.prune_history(now).unwrap();
    assert_eq!(
        pruned,
        HistoryPruned {
            runs: 4,
            assessments: 1
        }
    );
    assert_eq!(
        ids(&store, "agent_triage_runs"),
        vec![old_answer, just_in, recent],
        "gone: {old_applied}, {old_stale}, {old_unanswered}, {just_out}"
    );
    assert_eq!(
        ids(&store, "notification_assessments"),
        vec![recent_assessment],
        "gone: {old_assessment}"
    );
    // The jobs are the dedupe and are never touched.
    assert_eq!(ids(&store, "agent_triage_jobs"), vec![ingest, revisit]);

    // A second pass the same day finds nothing.
    assert_eq!(store.prune_history(now).unwrap(), HistoryPruned::default());
}

/// A first pass on a store with a long history clears it a chunk at a time,
/// all of it. A clock that stepped back is no reason to delete inside the
/// window: a recent run stored below an older one stays, and the old ones
/// stored after it wait for a later pass. A clock that ran ahead does not
/// stop the pass at the row it stamped in the future.
#[test]
fn history_retention_clears_a_backlog_in_chunks_and_never_inside_the_window() {
    let (store, acct) = store();
    let now = Utc::now();
    let message = ingest_normal(&store, acct, "g-big", "t-big", Tier::Signal, 50, now);
    let ingest = job(&store, acct, message, "ingest");
    let backlog = 2 * PRUNE_CHUNK_ROWS + 500;
    {
        let conn = store.lock().unwrap();
        conn.execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < ?3)
             INSERT INTO agent_triage_runs(job_id,account_id,message_id,outcome,completed_at)
             SELECT ?1, ?2, ?4, 'applied', ?5 FROM n",
            params![ingest, acct, backlog, message, ago(now, 300)],
        )
        .unwrap();
    }
    let future = run(&store, ingest, "applied", &ago(now, -400));
    let old_after_future = run(&store, ingest, "applied", &ago(now, 300));
    let skewed_recent = run(&store, ingest, "applied", &ago(now, 2));
    let old_after_recent = run(&store, ingest, "applied", &ago(now, 300));
    let recent = run(&store, ingest, "applied", &ago(now, 1));

    let pruned = store.prune_history(now).unwrap();
    assert_eq!(pruned.runs, backlog as u64 + 1, "{old_after_future} gone");
    assert_eq!(
        ids(&store, "agent_triage_runs"),
        vec![future, skewed_recent, old_after_recent, recent]
    );

    // Once the window has passed the skewed run, the one stored after it
    // goes, and so does everything else old; the future-stamped run is still
    // younger than the window and stays.
    let later = now + Duration::days(HISTORY_RETENTION_DAYS) + Duration::hours(12);
    assert_eq!(store.prune_history(later).unwrap().runs, 3);
    assert_eq!(ids(&store, "agent_triage_runs"), vec![future]);
}

#[test]
fn history_retention_on_empty_tables_is_a_no_op() {
    let (store, _) = store();
    assert_eq!(
        store.prune_history(Utc::now()).unwrap(),
        HistoryPruned::default()
    );
}

/// Every step seeks by rowid and none scans, so a pass costs what it walks
/// and deletes, not the size of the table: the page reads up from where the
/// last one ended, in rowid order with no sort, and the delete takes that
/// range. The revisit exception is a primary-key probe of the job.
#[test]
fn history_retention_seeks_by_rowid() {
    let (store, _) = store();
    let conn = store.lock().unwrap();
    for (page, delete, table) in [
        (RUNS_PAGE_SQL, RUNS_DELETE_SQL, "agent_triage_runs"),
        (
            ASSESSMENTS_PAGE_SQL,
            ASSESSMENTS_DELETE_SQL,
            "notification_assessments",
        ),
    ] {
        let steps = plan(&conn, page);
        assert!(
            steps
                .iter()
                .any(|s| s == &format!("SEARCH {table} USING INTEGER PRIMARY KEY (rowid>?)")),
            "{steps:?}"
        );
        // The only scan is of the page itself, at most a chunk of rows.
        assert!(
            !steps
                .iter()
                .any(|s| s.starts_with(&format!("SCAN {table}")) || s.contains("TEMP B-TREE")),
            "{steps:?}"
        );
        let steps = plan(&conn, delete);
        assert!(
            steps
                .iter()
                .any(|s| s
                    == &format!("SEARCH {table} USING INTEGER PRIMARY KEY (rowid>? AND rowid<?)")),
            "{steps:?}"
        );
        assert!(!steps.iter().any(|s| s.starts_with("SCAN")), "{steps:?}");
    }
    let steps = plan(&conn, RUNS_DELETE_SQL);
    assert!(
        steps
            .iter()
            .any(|s| s == "SEARCH j USING INTEGER PRIMARY KEY (rowid=?)"),
        "{steps:?}"
    );
}
