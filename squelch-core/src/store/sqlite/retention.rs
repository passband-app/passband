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
/// it in short steps rather than in one long delete or one long scan.
pub(super) const PRUNE_CHUNK_ROWS: i64 = 500;

/// The pause between pages. The store mutex is not fair: a thread that
/// unlocks and locks again at once usually wins it back from a waiter that
/// was woken, so without a pause the sync engine or a door request could wait
/// out the whole walk instead of one page.
const PAGE_PAUSE: std::time::Duration = std::time::Duration::from_millis(1);

/// What [`SqliteStore::prune_history`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HistoryPruned {
    /// Runs deleted.
    pub runs: u64,
    /// Answered-revisit runs past the window that were kept, with their
    /// metadata emptied.
    pub runs_trimmed: u64,
    /// Fast-lane assessments deleted.
    pub assessments: u64,
}

/// How one table is pruned. Every statement takes `?1` the id the page starts
/// after; the page also takes `?2` the cutoff, `?3` now and `?4` the page size;
/// the delete and trim take `?2` the id the page stops before and `?3` the
/// cutoff.
pub(super) struct Pruning {
    /// The next page above `?1` by rowid: its last id, the first id inside
    /// the window, and how many rows it held.
    pub(super) page: &'static str,
    pub(super) delete: &'static str,
    /// Rows kept past the window that are slimmed instead.
    pub(super) trim: Option<&'static str>,
}

/// Runs.
///
/// THE PAGE. Ids are assigned in commit order and `completed_at` is taken in
/// the committing transaction, so the window starts at the first row inside
/// it and everything below is older. Walking up from the bottom a page at a
/// time finds that row without an index on the timestamp, and each pass's
/// walk is short: once one has run, what lies below the window is a few
/// hours' worth of rows and the slimmed answers kept there, a hundred bytes
/// each.
///
/// A row stamped after now is not taken for the window's start. That is a
/// clock that ran ahead and was corrected, and taken for the start it would
/// stop every pass at itself, and the pruning with it, until the real clock
/// caught up with the wrong one. It is walked past, and kept by the delete's
/// own timestamp check. The price is paid only while the clock reads behind
/// rows already stored: a pass then walks up to the first row it can place
/// in the window, deleting nothing that is not old by its own stamp.
///
/// THE DELETE rechecks every row's own timestamp, so nothing inside the window
/// goes, whatever order a clock that stepped back stored it in. It keeps a
/// message's NEWEST ANSWERED REVISIT ([`revisit_answer_sql!`]), the one row
/// the revisit pass reads. Its older answers go.
///
/// THE TRIM empties the metadata of the answers kept past the window: the
/// revisit pass reads only their outcome, job and time, so a kept answer
/// costs a hundred bytes rather than its 1-3 KB, however many messages have
/// one.
pub(super) const RUNS: Pruning = Pruning {
    page: "SELECT MAX(id),
            MIN(CASE WHEN julianday(completed_at) >= julianday(?2)
                      AND julianday(completed_at) <= julianday(?3) THEN id END),
            COUNT(*)
       FROM (SELECT id, completed_at FROM agent_triage_runs
              WHERE id > ?1 ORDER BY id LIMIT ?4)",
    delete: concat!(
        "DELETE FROM agent_triage_runs AS r
          WHERE r.id > ?1 AND r.id < ?2
            AND julianday(r.completed_at) < julianday(?3)
            AND NOT EXISTS (
                  SELECT 1 FROM agent_triage_jobs j WHERE ",
        revisit_answer_sql!("r", "j"),
        " AND NOT EXISTS (
                    SELECT 1 FROM agent_triage_runs n, agent_triage_jobs nj
                     WHERE n.account_id = r.account_id AND n.message_id = r.message_id AND ",
        revisit_answer_sql!("n", "nj"),
        " AND julianday(n.completed_at) > julianday(r.completed_at)))"
    ),
    trim: Some(
        "UPDATE agent_triage_runs SET metadata_json = '{}'
          WHERE id > ?1 AND id < ?2
            AND julianday(completed_at) < julianday(?3)
            AND metadata_json <> '{}'",
    ),
};

/// Fast-lane assessments, the same walk with no exception: the notification
/// ledger, not this table, is what stops a second model call. `id` is
/// AUTOINCREMENT and `assessed_at` is taken just before the insert, outside
/// the store lock, so two assessments landing together may be stored a moment
/// out of order; the delete's own timestamp check is what keeps the window
/// whole, here as for runs.
pub(super) const ASSESSMENTS: Pruning = Pruning {
    page: "SELECT MAX(id),
            MIN(CASE WHEN julianday(assessed_at) >= julianday(?2)
                      AND julianday(assessed_at) <= julianday(?3) THEN id END),
            COUNT(*)
       FROM (SELECT id, assessed_at FROM notification_assessments
              WHERE id > ?1 ORDER BY id LIMIT ?4)",
    delete: "DELETE FROM notification_assessments
              WHERE id > ?1 AND id < ?2
                AND julianday(assessed_at) < julianday(?3)",
    trim: None,
};

/// One page of [`SqliteStore::prune_page`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct PageStep {
    pub(super) deleted: u64,
    pub(super) trimmed: u64,
    /// Where the next page starts, or `None` when this one reached the window
    /// or the end of the table.
    pub(super) next: Option<i64>,
}

impl SqliteStore {
    /// Delete run diagnostics and fast-lane assessments older than
    /// [`HISTORY_RETENTION_DAYS`] before `now`, for every account: what it
    /// did, and the first error if it stopped short.
    ///
    /// THE CLOCK IS TRUSTED, as the shredder trusts it with mail: a clock far
    /// ahead deletes diagnostics early. Never a decision, and never the one
    /// row something decides on (a message's newest answered revisit), which
    /// is kept whatever its age.
    ///
    /// A page of [`PRUNE_CHUNK_ROWS`] at a time, one write transaction and
    /// one hold of the mutex each. The pages freed go to SQLite's freelist and
    /// are reused by later writes, so the file stops growing for this history
    /// rather than shrinking: the store is not `auto_vacuum`, and a `VACUUM`
    /// rewrites the whole file under the write lock.
    ///
    /// Off the open path and on a timer, like
    /// [`refresh_statistics`](Self::refresh_statistics) (`spawn_history_retention`
    /// in squelchd), and best effort: both tables are tried even when the
    /// first fails, so a busy file does not hold the other back, and the
    /// counts include the pages committed before an error.
    pub fn prune_history(&self, now: DateTime<Utc>) -> (HistoryPruned, Result<()>) {
        let cutoff = (now - chrono::Duration::days(HISTORY_RETENTION_DAYS)).to_rfc3339();
        let now = now.to_rfc3339();
        let mut pruned = HistoryPruned::default();
        let runs = self.prune_table(&RUNS, &cutoff, &now, |step| {
            pruned.runs += step.deleted;
            pruned.runs_trimmed += step.trimmed;
        });
        let assessments = self.prune_table(&ASSESSMENTS, &cutoff, &now, |step| {
            pruned.assessments += step.deleted;
        });
        (pruned, runs.and(assessments))
    }

    /// Walk one table up from the bottom a page at a time until a page
    /// reaches the window or the end, reporting each committed page.
    fn prune_table(
        &self,
        table: &Pruning,
        cutoff: &str,
        now: &str,
        mut committed: impl FnMut(PageStep),
    ) -> Result<()> {
        let mut after = 0;
        loop {
            let step = self.prune_page(table, after, cutoff, now)?;
            committed(step);
            match step.next {
                Some(next) => after = next,
                None => return Ok(()),
            }
            std::thread::sleep(PAGE_PAUSE);
        }
    }

    /// One page above `after`: its old rows deleted (or trimmed, where kept),
    /// in one write transaction under one hold of the mutex.
    ///
    /// IMMEDIATE, so the write lock is taken before the page is read: a
    /// deferred read that another process's commit overtook could not be
    /// upgraded, and would fail at once rather than wait out the busy
    /// timeout. Plain `prepare`, not the statement cache: these run a few
    /// times every six hours, and would only push hot statements out of it.
    pub(super) fn prune_page(
        &self,
        table: &Pruning,
        after: i64,
        cutoff: &str,
        now: &str,
    ) -> Result<PageStep> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (last, window, seen): (Option<i64>, Option<i64>, i64) = tx
            .prepare(table.page)?
            .query_row(params![after, cutoff, now, PRUNE_CHUNK_ROWS], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
        let Some(last) = last else {
            return Ok(PageStep::default());
        };
        let before = window.unwrap_or(last + 1);
        let deleted = tx
            .prepare(table.delete)?
            .execute(params![after, before, cutoff])? as u64;
        let trimmed = match table.trim {
            Some(trim) => tx.prepare(trim)?.execute(params![after, before, cutoff])? as u64,
            None => 0,
        };
        tx.commit()?;
        Ok(PageStep {
            deleted,
            trimmed,
            next: (window.is_none() && seen == PRUNE_CHUNK_ROWS).then_some(last),
        })
    }
}
