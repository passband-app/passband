//! The outbox: mail the user finished and told the daemon to send later. See
//! the `scheduled_sends` table comment in schema.sql for the state machine
//! every function here moves one step, and docs/SCHEDULED-SEND.md for the
//! contract.
//!
//! EVERY STATE-MOVING UPDATE NAMES THE STATE IT MOVES FROM. The worker and the
//! human door race on the same row (a cancel against a claim, a settle from a
//! worker whose lease already lapsed and was recovered), and a guarded UPDATE
//! is what makes the loser a no-op instead of a resurrection.

use super::drafts::{PARENT_COLS, parent_join};
use super::outbound::{move_claims_to_scheduled, release_scheduled_claims, scheduled_attachments};
use super::*;
use crate::store::{
    ATTACHMENT_MISSING, CancelOutcome, MAX_SCHEDULED_PER_ACCOUNT, ParentContext,
    SCHEDULED_MAX_ATTEMPTS, ScheduledSend, ScheduledSendState, SearchClock, TOO_MANY_SCHEDULED,
};

/// The row shape, in [`map_scheduled`]'s order. Shared by the SELECTs and the
/// claim's `RETURNING`, so the two cannot drift.
const SCHEDULED_COLS: &str = "id, account_id, request, send_at, state, lease_until, attempts,
        last_error, message_id_header, sent_message_id, sent_thread_id, created_at, updated_at,
        maybe_sent, last_attempt_at, search_deferred_since";

/// How long a settled (`sent`/`cancelled`) row is kept before the sweep.
pub const SCHEDULED_SETTLED_TTL: chrono::Duration = chrono::Duration::days(7);

/// How long a `failed` row waits for the user before the sweep takes it and
/// its files. Longer than a settled row's week: it is still owed a decision,
/// and the listing shows it as "didn't send" the whole time.
pub const SCHEDULED_FAILED_TTL: chrono::Duration = chrono::Duration::days(30);

/// One [`SCHEDULED_COLS`] row into a [`ScheduledSend`], attachments empty (the
/// caller fills them: a row map cannot run a second query).
fn map_scheduled(r: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduledSend> {
    let state: String = r.get(4)?;
    let state = ScheduledSendState::parse(&state).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            format!("unknown scheduled_sends.state {state:?}").into(),
        )
    })?;
    Ok(ScheduledSend {
        id: r.get(0)?,
        account_id: r.get(1)?,
        request: r.get(2)?,
        send_at: dt(r, 3)?,
        state,
        lease_until: dt_opt(r, 5)?,
        attempts: r.get(6)?,
        last_error: r.get(7)?,
        message_id_header: r.get(8)?,
        sent_message_id: r.get(9)?,
        sent_thread_id: r.get(10)?,
        created_at: dt(r, 11)?,
        updated_at: dt(r, 12)?,
        maybe_sent: r.get::<_, i64>(13)? != 0,
        last_attempt_at: dt_opt(r, 14)?,
        search_deferred_since: dt_opt(r, 15)?,
        attachments: Vec::new(),
    })
}

/// One row by id with its files, on a connection the caller already holds.
fn read_scheduled(
    conn: &Connection,
    account_id: AccountId,
    id: i64,
) -> Result<Option<ScheduledSend>> {
    let row = conn
        .query_row(
            &format!(
                "SELECT {SCHEDULED_COLS} FROM scheduled_sends WHERE account_id = ?1 AND id = ?2"
            ),
            params![account_id, id],
            map_scheduled,
        )
        .optional()?;
    let Some(mut row) = row else {
        return Ok(None);
    };
    row.attachments = scheduled_attachments(conn, account_id, id)?;
    Ok(Some(row))
}

/// Rows matching `filter` (a WHERE fragment over `?1` = account and, when it
/// needs one, `?2` = now), oldest `send_at` first, files filled.
fn read_many(
    conn: &Connection,
    account_id: AccountId,
    filter: &str,
    now: Option<DateTime<Utc>>,
) -> Result<Vec<ScheduledSend>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SCHEDULED_COLS} FROM scheduled_sends
         WHERE account_id = ?1 AND ({filter})
         ORDER BY send_at ASC, id ASC"
    ))?;
    // rusqlite refuses more binds than placeholders, so `?2` binds only when
    // the filter names it.
    let rows = match now {
        Some(now) => stmt.query_map(params![account_id, now.to_rfc3339()], map_scheduled)?,
        None => stmt.query_map(params![account_id], map_scheduled)?,
    };
    let mut out = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    for row in &mut out {
        row.attachments = scheduled_attachments(conn, account_id, row.id)?;
    }
    Ok(out)
}

/// Insert one `scheduled` row; its id.
fn insert_row(
    conn: &Connection,
    account_id: AccountId,
    request: &str,
    send_at: DateTime<Utc>,
    message_id_header: &str,
    now: DateTime<Utc>,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO scheduled_sends(account_id, request, send_at, state, attempts,
             message_id_header, created_at, updated_at)
         VALUES(?1, ?2, ?3, 'scheduled', 0, ?4, ?5, ?5)",
        params![
            account_id,
            request,
            send_at.to_rfc3339(),
            message_id_header,
            now.to_rfc3339()
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

impl SqliteStore {
    /// TEST HOOK: insert one `scheduled` row and nothing else. The handler
    /// uses [`Self::schedule_send`] (row + file claims + draft consumed, one
    /// transaction). `pub` only because squelch-api's integration tests seed
    /// rows with it.
    #[doc(hidden)]
    pub fn insert_scheduled_send(
        &self,
        account_id: AccountId,
        request: &str,
        send_at: DateTime<Utc>,
        message_id_header: &str,
        now: DateTime<Utc>,
    ) -> Result<ScheduledSend> {
        let conn = self.lock()?;
        let id = insert_row(&conn, account_id, request, send_at, message_id_header, now)?;
        read_scheduled(&conn, account_id, id)?.ok_or(CoreError::NotFound)
    }

    /// HUMAN-DOOR ONLY (`POST /client/scheduled`, contract §3 step 7) as ONE
    /// TRANSACTION: insert the row, move `attachment_ids` onto it (from
    /// `from_draft` or unclaimed, see
    /// [`Self::claim_attachments_for_scheduled_send`]), and delete `from_draft`
    /// with whatever files it still held that the request does not name (files
    /// removed from the tray). From here the row IS the composition.
    ///
    /// All or nothing: a file that cannot move answers
    /// `CoreError::InvalidInput(ATTACHMENT_MISSING)` and leaves no row, no
    /// moved claim and the draft intact. Three separate calls could leave a
    /// row whose files never arrived, or a consumed draft with no row.
    #[allow(clippy::too_many_arguments)]
    pub fn schedule_send(
        &self,
        account_id: AccountId,
        request: &str,
        send_at: DateTime<Utc>,
        message_id_header: &str,
        attachment_ids: &[i64],
        from_draft: Option<i64>,
        now: DateTime<Utc>,
    ) -> Result<ScheduledSend> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        super::outbound::sweep(&tx, account_id, now)?;
        // THE CAP, counted inside the transaction that inserts, so two
        // schedules racing at 99 cannot both land.
        let live: i64 = tx.query_row(
            "SELECT COUNT(*) FROM scheduled_sends
             WHERE account_id = ?1 AND state IN ('scheduled', 'sending', 'failed')",
            params![account_id],
            |r| r.get(0),
        )?;
        if live as usize >= MAX_SCHEDULED_PER_ACCOUNT {
            return Err(CoreError::InvalidInput(TOO_MANY_SCHEDULED.into()));
        }
        let id = insert_row(&tx, account_id, request, send_at, message_id_header, now)?;
        if !move_claims_to_scheduled(&tx, account_id, id, attachment_ids, from_draft)? {
            // Dropping `tx` rolls the insert and any partial move back.
            return Err(CoreError::InvalidInput(ATTACHMENT_MISSING.into()));
        }
        if let Some(draft) = from_draft {
            // `delete_draft`'s two statements, inside this transaction.
            tx.execute(
                "DELETE FROM outbound_attachments WHERE account_id = ?1 AND draft_id = ?2",
                params![account_id, draft],
            )?;
            tx.execute(
                "DELETE FROM drafts WHERE account_id = ?1 AND id = ?2",
                params![account_id, draft],
            )?;
        }
        let row = read_scheduled(&tx, account_id, id)?.ok_or(CoreError::NotFound)?;
        tx.commit()?;
        Ok(row)
    }

    /// One row by id, files included. `None` for an unknown id and for another
    /// account's.
    pub fn scheduled_send_by_id(
        &self,
        account_id: AccountId,
        id: i64,
    ) -> Result<Option<ScheduledSend>> {
        let conn = self.lock()?;
        read_scheduled(&conn, account_id, id)
    }

    /// The outbox listing (`GET /client/scheduled`): rows still owed an outcome
    /// or the user's attention (`scheduled`, `sending`, `failed`), oldest
    /// `send_at` first. Settled rows (`sent`, `cancelled`) are not listed: the
    /// Sent list shows the sent mail itself.
    pub fn list_scheduled_sends(&self, account_id: AccountId) -> Result<Vec<ScheduledSend>> {
        let conn = self.lock()?;
        read_many(
            &conn,
            account_id,
            "state IN ('scheduled', 'sending', 'failed')",
            None,
        )
    }

    /// [`Self::list_scheduled_sends`] with each row's PARENT CONTEXT: the
    /// replied-to or forwarded message's thread, subject and sender, through
    /// the same account-pinned join drafts use ([`parent_join`]). The parent
    /// id lives inside the frozen `request`, so it is read out with
    /// `json_extract` (a reply's id, else a forward's). Same rows, same order.
    pub fn list_scheduled_sends_with_parent(
        &self,
        account_id: AccountId,
    ) -> Result<Vec<(ScheduledSend, ParentContext)>> {
        let conn = self.lock()?;
        // Qualified: `messages` shares column names (`id`, `subject`, ...).
        let cols = SCHEDULED_COLS
            .split(',')
            .map(|c| format!("s.{}", c.trim()))
            .collect::<Vec<_>>()
            .join(", ");
        // GUARDED BY `json_valid`: `json_extract` RAISES on malformed JSON,
        // and one corrupt row (which the worker fails as `failed:request`
        // and which then stays listed, `failed`) would otherwise 500 the
        // whole listing for good. CASE is evaluated lazily, so an invalid
        // request joins nothing.
        let join = parent_join(
            "s",
            "CASE WHEN json_valid(s.request) THEN
                 COALESCE(json_extract(s.request, '$.reply_to_message_id'),
                          json_extract(s.request, '$.forward_of_message_id'))
             END",
        );
        let mut stmt = conn.prepare(&format!(
            "SELECT {cols}, {PARENT_COLS}
             FROM scheduled_sends s
             {join}
             WHERE s.account_id = ?1 AND s.state IN ('scheduled', 'sending', 'failed')
             ORDER BY s.send_at ASC, s.id ASC"
        ))?;
        let base = SCHEDULED_COLS.split(',').count();
        let rows = stmt.query_map(params![account_id], |r| {
            let send = map_scheduled(r)?;
            let parent = ParentContext {
                thread_id: r.get(base)?,
                subject: r.get(base + 1)?,
                from_addr: r.get(base + 2)?,
                from_name: r.get(base + 3)?,
            };
            Ok((send, parent))
        })?;
        let mut out = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        for (row, _) in &mut out {
            row.attachments = scheduled_attachments(&conn, account_id, row.id)?;
        }
        Ok(out)
    }

    /// THE WORKER'S CLAIM (contract §4 step 2) with a flat lease: see
    /// [`Self::claim_due_scheduled_send_with`], which the worker calls to size
    /// the lease per row.
    pub fn claim_due_scheduled_send(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
        lease: chrono::Duration,
    ) -> Result<Option<ScheduledSend>> {
        if lease <= chrono::Duration::zero() {
            return Err(CoreError::InvalidInput("lease must be positive".into()));
        }
        self.claim_due_scheduled_send_with(account_id, now, |_| lease)
    }

    /// THE WORKER'S CLAIM (contract §4 step 2): the oldest due `scheduled` row
    /// with attempts left moves to `sending` under a lease of `lease_for(row)`
    /// from `now`, `attempts` + 1, in ONE IMMEDIATE TRANSACTION. The pick and
    /// the move cannot be split by another claimer (the write lock is taken
    /// before the pick), so two ticks, or two processes on one file, never
    /// both hold a row. `None` when nothing is due. Call in a loop until
    /// `None`.
    ///
    /// The lease is sized FROM THE ROW, files included, because a send's own
    /// Gmail budget grows with its bytes: a flat lease shorter than a 25 MB
    /// upload would hand a mail still on the wire to recovery. The returned
    /// row's `attempts` NAMES THIS CLAIM: every settle takes it back, so a
    /// settle from an older claim is a no-op.
    pub fn claim_due_scheduled_send_with(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
        lease_for: impl Fn(&ScheduledSend) -> chrono::Duration,
    ) -> Result<Option<ScheduledSend>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let id: Option<i64> = tx
            .query_row(
                "SELECT id FROM scheduled_sends
                 WHERE account_id = ?1 AND state = 'scheduled' AND send_at <= ?2
                   AND attempts < ?3
                 ORDER BY send_at, id LIMIT 1",
                params![account_id, now.to_rfc3339(), SCHEDULED_MAX_ATTEMPTS],
                |r| r.get(0),
            )
            .optional()?;
        let Some(id) = id else {
            return Ok(None);
        };
        let Some(picked) = read_scheduled(&tx, account_id, id)? else {
            return Ok(None);
        };
        let lease = lease_for(&picked);
        if lease <= chrono::Duration::zero() {
            return Err(CoreError::InvalidInput("lease must be positive".into()));
        }
        let row = tx
            .query_row(
                &format!(
                    "UPDATE scheduled_sends
                     SET state = 'sending', lease_until = ?4, attempts = attempts + 1,
                         recover_at = NULL, updated_at = ?3
                     WHERE account_id = ?1 AND id = ?2 AND state = 'scheduled'
                     RETURNING {SCHEDULED_COLS}"
                ),
                params![account_id, id, now.to_rfc3339(), (now + lease).to_rfc3339()],
                map_scheduled,
            )
            .optional()?;
        let Some(mut row) = row else {
            return Ok(None);
        };
        row.attachments = picked.attachments;
        tx.commit()?;
        Ok(Some(row))
    }

    /// Recovery input (contract §4 step 1): `sending` rows whose lease lapsed
    /// at or before `lapsed_before`, i.e. a worker that died mid-send, and
    /// whose last recovery search (if one failed) asked to wait until past
    /// `now`. The worker passes `now - RECOVERY_GRACE` as `lapsed_before`, so
    /// a send that finished just as its lease ran out has time to be indexed
    /// before Gmail is asked about it. Each must be answered by a Message-ID
    /// search BEFORE it can be retried: found → [`Self::settle_scheduled_sent`],
    /// not found → [`Self::requeue_scheduled_send`], could not ask →
    /// [`Self::defer_scheduled_recovery`].
    pub fn expired_sending_scheduled_sends(
        &self,
        account_id: AccountId,
        lapsed_before: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<Vec<ScheduledSend>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SCHEDULED_COLS} FROM scheduled_sends
             WHERE account_id = ?1 AND state = 'sending'
               AND lease_until IS NOT NULL AND lease_until <= ?2
               AND (recover_at IS NULL OR recover_at <= ?3)
             ORDER BY send_at ASC, id ASC"
        ))?;
        let rows = stmt.query_map(
            params![account_id, lapsed_before.to_rfc3339(), now.to_rfc3339()],
            map_scheduled,
        )?;
        let mut out = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        for row in &mut out {
            row.attachments = scheduled_attachments(&conn, account_id, row.id)?;
        }
        Ok(out)
    }

    /// `sending` → `sent` under claim `attempts`, with the echo's local ids
    /// when the echo landed. `false` when the row is not `sending` under that
    /// claim (already settled, cancelled out from under a stale worker,
    /// recovered and re-claimed, unknown, another account's).
    #[allow(clippy::too_many_arguments)]
    pub fn settle_scheduled_sent(
        &self,
        account_id: AccountId,
        id: i64,
        attempts: u32,
        sent_message_id: Option<i64>,
        sent_thread_id: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE scheduled_sends
             SET state = 'sent', lease_until = NULL, recover_at = NULL,
                 search_deferred_since = NULL,
                 sent_message_id = ?4, sent_thread_id = ?5, updated_at = ?6
             WHERE account_id = ?1 AND id = ?2 AND state = 'sending' AND attempts = ?3",
            params![
                account_id,
                id,
                attempts,
                sent_message_id,
                sent_thread_id,
                now.to_rfc3339()
            ],
        )?;
        Ok(n > 0)
    }

    /// A failed attempt with retries left: `sending` → `scheduled` at
    /// `next_send_at` under claim `attempts`, lease cleared, `last_error` kept
    /// for the listing. `maybe_sent` says the attempt failed AFTER the mail was
    /// posted to Gmail; it is OR-ed into the row's sticky flag, so every later
    /// fire asks Gmail first. `last_error` is a REDACTED reason, never content.
    /// `now` is stamped as `last_attempt_at` (the attempt ENDED now), and the
    /// pre-fire search clock stops: whatever this attempt was, it got past it.
    /// `false` when the row is not `sending` under that claim.
    #[allow(clippy::too_many_arguments)]
    pub fn settle_scheduled_retry(
        &self,
        account_id: AccountId,
        id: i64,
        attempts: u32,
        next_send_at: DateTime<Utc>,
        last_error: &str,
        maybe_sent: bool,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE scheduled_sends
             SET state = 'scheduled', lease_until = NULL, recover_at = NULL, send_at = ?4,
                 last_error = ?5, maybe_sent = MAX(maybe_sent, ?6), last_attempt_at = ?7,
                 search_deferred_since = NULL, updated_at = ?7
             WHERE account_id = ?1 AND id = ?2 AND state = 'sending' AND attempts = ?3",
            params![
                account_id,
                id,
                attempts,
                next_send_at.to_rfc3339(),
                last_error,
                maybe_sent as i64,
                now.to_rfc3339()
            ],
        )?;
        Ok(n > 0)
    }

    /// An attempt that NEVER STARTED, so it must not count: `sending` →
    /// `scheduled` at `next_send_at` under claim `attempts`, with the claim's
    /// increment taken back. For the two answers that say "not now" rather
    /// than "this mail failed": Gmail's budget pacing the mailbox, a pre-fire
    /// search that could not be made, and a `maybe_sent` row still inside its
    /// grace. `search` moves the pre-fire search's give-up clock
    /// ([`SearchClock`]); `last_attempt_at` is untouched (no attempt ran).
    /// `false` when the row is not `sending` under that claim.
    #[allow(clippy::too_many_arguments)]
    pub fn unclaim_scheduled_send(
        &self,
        account_id: AccountId,
        id: i64,
        attempts: u32,
        next_send_at: DateTime<Utc>,
        last_error: &str,
        search: SearchClock,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let clock = match search {
            SearchClock::Reset => "NULL",
            SearchClock::Start => "COALESCE(search_deferred_since, ?6)",
            SearchClock::Keep => "search_deferred_since",
        };
        let conn = self.lock()?;
        let n = conn.execute(
            &format!(
                "UPDATE scheduled_sends
                 SET state = 'scheduled', lease_until = NULL, recover_at = NULL, send_at = ?4,
                     last_error = ?5, attempts = MAX(attempts - 1, 0),
                     search_deferred_since = {clock}, updated_at = ?6
                 WHERE account_id = ?1 AND id = ?2 AND state = 'sending' AND attempts = ?3"
            ),
            params![
                account_id,
                id,
                attempts,
                next_send_at.to_rfc3339(),
                last_error,
                now.to_rfc3339()
            ],
        )?;
        Ok(n > 0)
    }

    /// Out of retries: `sending` → `failed` under claim `attempts`, lease
    /// cleared. The row waits for the user (send now, or cancel). `last_error`
    /// is a REDACTED reason.
    ///
    /// `maybe_sent` is OR-ed into the sticky flag exactly as a retry does: a
    /// row that failed on an attempt that MAY have reached Gmail (an ambiguous
    /// last post, a crash loop, a recovery that never got to ask) must be
    /// searched for before "send now" sends it, or the fresh budget send-now
    /// grants would post it blind. `now` is stamped as `last_attempt_at`.
    /// `false` when the row is not `sending` under that claim.
    #[allow(clippy::too_many_arguments)]
    pub fn settle_scheduled_failed(
        &self,
        account_id: AccountId,
        id: i64,
        attempts: u32,
        last_error: &str,
        maybe_sent: bool,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE scheduled_sends
             SET state = 'failed', lease_until = NULL, recover_at = NULL, last_error = ?4,
                 maybe_sent = MAX(maybe_sent, ?5), last_attempt_at = ?6,
                 search_deferred_since = NULL, updated_at = ?6
             WHERE account_id = ?1 AND id = ?2 AND state = 'sending' AND attempts = ?3",
            params![
                account_id,
                id,
                attempts,
                last_error,
                maybe_sent as i64,
                now.to_rfc3339()
            ],
        )?;
        Ok(n > 0)
    }

    /// Recovery's "not found in Gmail" branch: `sending` → `scheduled` at
    /// `send_at` under claim `attempts`, lease cleared, `attempts` UNCHANGED
    /// (the lapsed attempt already counted), and `maybe_sent` set: the lapsed
    /// attempt may still surface in Gmail's index, so the re-fire asks again
    /// before it sends. `last_attempt_at` becomes the lapsed lease's end (the
    /// latest the lost attempt can have posted). `false` when the row is not
    /// `sending` under that claim.
    pub fn requeue_scheduled_send(
        &self,
        account_id: AccountId,
        id: i64,
        attempts: u32,
        send_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE scheduled_sends
             SET state = 'scheduled', last_attempt_at = COALESCE(lease_until, ?5),
                 lease_until = NULL, recover_at = NULL, send_at = ?4,
                 maybe_sent = 1, search_deferred_since = NULL, updated_at = ?5
             WHERE account_id = ?1 AND id = ?2 AND state = 'sending' AND attempts = ?3",
            params![
                account_id,
                id,
                attempts,
                send_at.to_rfc3339(),
                now.to_rfc3339()
            ],
        )?;
        Ok(n > 0)
    }

    /// Recovery COULD NOT ASK (no write credential, or the search failed): the
    /// row stays `sending`, its lease untouched (the give-up clock runs from
    /// it), `last_error` says why for the listing, and recovery does not ask
    /// again before `recover_at`. One Gmail call per row per minute, not per
    /// tick. `false` when the row is not `sending` under claim `attempts`.
    pub fn defer_scheduled_recovery(
        &self,
        account_id: AccountId,
        id: i64,
        attempts: u32,
        recover_at: DateTime<Utc>,
        last_error: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE scheduled_sends
             SET recover_at = ?4, last_error = ?5, updated_at = ?6
             WHERE account_id = ?1 AND id = ?2 AND state = 'sending' AND attempts = ?3",
            params![
                account_id,
                id,
                attempts,
                recover_at.to_rfc3339(),
                last_error,
                now.to_rfc3339()
            ],
        )?;
        Ok(n > 0)
    }

    /// HUMAN-DOOR ONLY (`DELETE /client/scheduled/{id}`): `scheduled` or
    /// `failed` → `cancelled`, and its files back to unclaimed, in ONE
    /// TRANSACTION (a cancelled row still holding files, or free files beside a
    /// live row, is a state neither door expects).
    ///
    /// The returned row's `attachments` are the files it HELD, read before the
    /// release, so the reopened composer can redraw its tray; its `request`
    /// carries their ids. A `sending` row answers [`CancelOutcome::Sending`]
    /// and is left exactly as it was: the mail is in Gmail's hands.
    pub fn cancel_scheduled_send(
        &self,
        account_id: AccountId,
        id: i64,
        now: DateTime<Utc>,
    ) -> Result<CancelOutcome> {
        let mut conn = self.lock()?;
        // IMMEDIATE: the write lock is taken before the read, so the state
        // read below is the state the UPDATE moves (another process's claim
        // cannot land in between).
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let Some(mut row) = read_scheduled(&tx, account_id, id)? else {
            return Ok(CancelOutcome::NotFound);
        };
        match row.state {
            ScheduledSendState::Sending => return Ok(CancelOutcome::Sending),
            // Nothing left to cancel: the handler's 404, like an unknown id.
            ScheduledSendState::Sent | ScheduledSendState::Cancelled => {
                return Ok(CancelOutcome::NotFound);
            }
            ScheduledSendState::Scheduled | ScheduledSendState::Failed => {}
        }
        let n = tx.execute(
            "UPDATE scheduled_sends
             SET state = 'cancelled', lease_until = NULL, updated_at = ?3
             WHERE account_id = ?1 AND id = ?2 AND state IN ('scheduled', 'failed')",
            params![account_id, id, now.to_rfc3339()],
        )?;
        // The read above said the row was cancellable and the lock has been
        // held since, so anything but one row is a store that moved under us:
        // never answer `Cancelled` (and release the files) for a row that was
        // not. Dropping `tx` rolls back.
        if n != 1 {
            return Err(CoreError::Other(anyhow::anyhow!(
                "scheduled send {id} moved during cancel"
            )));
        }
        release_scheduled_claims(&tx, account_id, id, now)?;
        tx.commit()?;
        row.state = ScheduledSendState::Cancelled;
        row.lease_until = None;
        row.updated_at = now;
        Ok(CancelOutcome::Cancelled(Box::new(row)))
    }

    /// HUMAN-DOOR ONLY (`POST /client/scheduled/{id}/now`): `scheduled` or
    /// `failed` → `scheduled` with `send_at = now`; the next tick sends it.
    ///
    /// A `failed` row starts a FRESH RETRY BUDGET (`attempts = 0`,
    /// `last_error` cleared): the user looked at "didn't send" and explicitly
    /// asked again, and a row that kept `attempts = 3` would fail terminally
    /// on its very next hiccup without a single retry. A `scheduled` row keeps
    /// both: pulling a pending retry forward is not a new request. `false` for
    /// a row in any other state, an unknown id or another account's.
    ///
    /// NOT A WAY PAST THE GRACE: `maybe_sent` and `last_attempt_at` are kept,
    /// and the worker holds a `maybe_sent` row back until its grace is out
    /// whatever `send_at` says, so the search it owes can see the mail.
    pub fn send_scheduled_now(
        &self,
        account_id: AccountId,
        id: i64,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE scheduled_sends
             SET state = 'scheduled', send_at = ?3, lease_until = NULL, updated_at = ?3,
                 attempts = CASE WHEN state = 'failed' THEN 0 ELSE attempts END,
                 last_error = CASE WHEN state = 'failed' THEN NULL ELSE last_error END,
                 search_deferred_since =
                     CASE WHEN state = 'failed' THEN NULL ELSE search_deferred_since END
             WHERE account_id = ?1 AND id = ?2 AND state IN ('scheduled', 'failed')",
            params![account_id, id, now.to_rfc3339()],
        )?;
        Ok(n > 0)
    }

    /// Drop settled (`sent`, `cancelled`) rows whose last move is older than
    /// [`SCHEDULED_SETTLED_TTL`], and `failed` rows older than
    /// [`SCHEDULED_FAILED_TTL`], with any files still claimed by them (a
    /// cancel released its files and a send consumed its own, so for settled
    /// rows this is a belt; a failed row still holds its files). Then the
    /// staged-file sweep ([`super::outbound::sweep`]), so files a cancel
    /// released and nobody re-claimed go at their TTL even when no composer
    /// runs. ONE TRANSACTION. Returns how many outbox rows went.
    pub fn sweep_scheduled_sends(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        let settled_cutoff = (now - SCHEDULED_SETTLED_TTL).to_rfc3339();
        let failed_cutoff = (now - SCHEDULED_FAILED_TTL).to_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        const STALE: &str = "SELECT id FROM scheduled_sends
             WHERE account_id = ?1
               AND ((state IN ('sent', 'cancelled') AND updated_at < ?2)
                    OR (state = 'failed' AND updated_at < ?3))";
        tx.execute(
            &format!(
                "DELETE FROM outbound_attachments
                 WHERE account_id = ?1 AND scheduled_send_id IN ({STALE})"
            ),
            params![account_id, settled_cutoff, failed_cutoff],
        )?;
        let n = tx.execute(
            &format!("DELETE FROM scheduled_sends WHERE account_id = ?1 AND id IN ({STALE})"),
            params![account_id, settled_cutoff, failed_cutoff],
        )?;
        super::outbound::sweep(&tx, account_id, now)?;
        tx.commit()?;
        Ok(n)
    }
}
