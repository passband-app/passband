//! History retention: the per-run diagnostics the triage worker and the fast
//! notification lane write, kept for [`HISTORY_RETENTION_DAYS`] and then
//! deleted, so the two tables stop growing with every run for the life of the
//! store.
//!
//! Only these two tables, on purpose. The other never-pruned tables are
//! records something still reads to decide: a terminal triage job is the
//! dedupe that stops the same message being investigated (and billed) twice;
//! an `opened` event is the only thing that stops a second "opened" push; the
//! shred ledger is what stops mail being trashed twice; `notify_decisions` is
//! the labelled eval corpus and says so; the audit log and the open counts are
//! shown to the user whole. A run row and an assessment row are read only by
//! the human inspector (the thread viewer's triage panel and
//! `GET /client/v2/triage/{id}`), but for one kind of run, kept below.

use super::*;

/// How long a run's diagnostics (`agent_triage_runs`) and a fast-lane
/// notification assessment (`notification_assessments`) are kept. They are
/// for inspecting how recent mail was judged: an applied run carries the
/// prompt version, model, the whole triage configuration and the token usage
/// of every turn, 1.1-3 KB, and an assessment the model's own reason.
pub const HISTORY_RETENTION_DAYS: i64 = 90;

/// Rows looked at, and at most deleted, per hold of the store mutex. The
/// first pass on a store with a long history (every run since install) clears
/// it in short steps, between which the sync engine, the worker and the doors
/// get the lock, rather than in one long delete or one long scan.
pub(super) const PRUNE_CHUNK_ROWS: i64 = 500;

/// What [`SqliteStore::prune_history`] deleted.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HistoryPruned {
    pub runs: u64,
    pub assessments: u64,
}

/// The next [`PRUNE_CHUNK_ROWS`] runs above `?1` by rowid: the last id among
/// them, the first one inside the window (`?2` the cutoff, `?3` now), and how
/// many there were.
///
/// Ids are assigned in commit order and `completed_at` is the commit's own
/// clock, so the window starts at the first row inside it and everything
/// below is older. Walking up from the bottom a page at a time finds that row
/// without an index on the timestamp, and each pass's walk is short: once one
/// has run, the rows below the window are a day's worth and the few that are
/// kept.
///
/// A row stamped after now (a clock that ran ahead, then was corrected) is not
/// the window's start: taken for it, it would stop every pass at itself until
/// the clock caught up. It is walked past here and kept by the delete's own
/// timestamp check.
pub(super) const RUNS_PAGE_SQL: &str = "SELECT MAX(id),
        MIN(CASE WHEN julianday(completed_at) >= julianday(?2)
                  AND julianday(completed_at) <= julianday(?3) THEN id END),
        COUNT(*)
   FROM (SELECT id, completed_at FROM agent_triage_runs
          WHERE id > ?1 ORDER BY id LIMIT ?4)";

/// The old runs in `(?1, ?2)`, one page's worth at most. The timestamp is
/// checked again, so a run is deleted only if it is itself older than the
/// window, whatever order a clock that stepped back stored it in.
///
/// KEPT: an `applied` run of a `revisit:` job. The ghost-event revisit pass
/// reads the newest one per message as "when was this last answered" and
/// asks the model again (a billed run) if it finds none. Revisits are capped
/// per message, so these few rows stay.
pub(super) const RUNS_DELETE_SQL: &str = "DELETE FROM agent_triage_runs
  WHERE id > ?1 AND id < ?2
    AND julianday(completed_at) < julianday(?3)
    AND NOT (outcome = 'applied' AND EXISTS (
          SELECT 1 FROM agent_triage_jobs j
           WHERE j.id = agent_triage_runs.job_id
             AND j.account_id = agent_triage_runs.account_id
             AND j.trigger LIKE 'revisit:%'))";

/// [`RUNS_PAGE_SQL`] for assessments. `id` is AUTOINCREMENT and
/// `assessed_at` the insert's clock.
pub(super) const ASSESSMENTS_PAGE_SQL: &str = "SELECT MAX(id),
        MIN(CASE WHEN julianday(assessed_at) >= julianday(?2)
                  AND julianday(assessed_at) <= julianday(?3) THEN id END),
        COUNT(*)
   FROM (SELECT id, assessed_at FROM notification_assessments
          WHERE id > ?1 ORDER BY id LIMIT ?4)";

/// [`RUNS_DELETE_SQL`] for assessments, with no exception: the notification
/// ledger, not this table, is what stops a second model call.
pub(super) const ASSESSMENTS_DELETE_SQL: &str = "DELETE FROM notification_assessments
  WHERE id > ?1 AND id < ?2
    AND julianday(assessed_at) < julianday(?3)";

impl SqliteStore {
    /// Delete run diagnostics and fast-lane assessments older than
    /// [`HISTORY_RETENTION_DAYS`] before `now`, for every account.
    ///
    /// A page of [`PRUNE_CHUNK_ROWS`] at a time, each under its own hold of
    /// the mutex. The pages freed go to SQLite's freelist and are reused by
    /// later writes, so the file stops growing for this history rather than
    /// shrinking: the store is not `auto_vacuum`, and a `VACUUM` rewrites the
    /// whole file under the write lock.
    ///
    /// Off the open path and on a timer, like
    /// [`refresh_statistics`](Self::refresh_statistics): the daemon calls it
    /// daily (`spawn_history_retention` in squelchd).
    pub fn prune_history(&self, now: DateTime<Utc>) -> Result<HistoryPruned> {
        let cutoff = (now - chrono::Duration::days(HISTORY_RETENTION_DAYS)).to_rfc3339();
        let now = now.to_rfc3339();
        Ok(HistoryPruned {
            runs: self.prune_table(RUNS_PAGE_SQL, RUNS_DELETE_SQL, &cutoff, &now)?,
            assessments: self.prune_table(
                ASSESSMENTS_PAGE_SQL,
                ASSESSMENTS_DELETE_SQL,
                &cutoff,
                &now,
            )?,
        })
    }

    /// Walk the table up from the bottom a page at a time, deleting each
    /// page's old rows, until a page reaches the window or the end.
    fn prune_table(&self, page: &str, delete: &str, cutoff: &str, now: &str) -> Result<u64> {
        let mut after = 0i64;
        let mut deleted = 0u64;
        loop {
            let done = {
                let conn = self.lock()?;
                let (last, window, seen): (Option<i64>, Option<i64>, i64) = conn
                    .prepare_cached(page)?
                    .query_row(params![after, cutoff, now, PRUNE_CHUNK_ROWS], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })?;
                let Some(last) = last else {
                    return Ok(deleted);
                };
                let below = window.unwrap_or(last + 1);
                deleted += conn
                    .prepare_cached(delete)?
                    .execute(params![after, below, cutoff])? as u64;
                after = last;
                window.is_some() || seen < PRUNE_CHUNK_ROWS
            };
            if done {
                return Ok(deleted);
            }
            // A waiter on the mutex gets in before the next page.
            std::thread::yield_now();
        }
    }
}
