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
    ATTACHMENT_MISSING, CancelOutcome, ParentContext, ScheduledSend, ScheduledSendState,
};

/// The row shape, in [`map_scheduled`]'s order. Shared by the SELECTs and the
/// claim's `RETURNING`, so the two cannot drift.
const SCHEDULED_COLS: &str = "id, account_id, request, send_at, state, lease_until, attempts,
        last_error, message_id_header, sent_message_id, sent_thread_id, created_at, updated_at";

/// How long a settled (`sent`/`cancelled`) row is kept before the sweep.
pub const SCHEDULED_SETTLED_TTL: chrono::Duration = chrono::Duration::days(7);

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
    /// HUMAN-DOOR ONLY (`POST /client/scheduled`): insert one `scheduled` row
    /// and nothing else. See [`Self::schedule_send`] for the one-transaction
    /// form the handler wants (row + file claims + draft consumed).
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
        let tx = conn.transaction()?;
        super::outbound::sweep(&tx, account_id, now)?;
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

    /// THE WORKER'S CLAIM (contract §4 step 2): the oldest due `scheduled` row
    /// moves to `sending` under a lease of `lease` from `now`, `attempts` + 1,
    /// in ONE statement. The pick and the move cannot be split by another
    /// claimer, so two ticks (or two processes on one file) never both hold a
    /// row. `None` when nothing is due. Call in a loop until `None`.
    pub fn claim_due_scheduled_send(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
        lease: chrono::Duration,
    ) -> Result<Option<ScheduledSend>> {
        if lease <= chrono::Duration::zero() {
            return Err(CoreError::InvalidInput("lease must be positive".into()));
        }
        let conn = self.lock()?;
        let row = conn
            .query_row(
                &format!(
                    "UPDATE scheduled_sends
                     SET state = 'sending', lease_until = ?3, attempts = attempts + 1,
                         updated_at = ?2
                     WHERE id = (SELECT id FROM scheduled_sends
                                 WHERE account_id = ?1 AND state = 'scheduled'
                                   AND send_at <= ?2
                                 ORDER BY send_at, id LIMIT 1)
                     RETURNING {SCHEDULED_COLS}"
                ),
                params![account_id, now.to_rfc3339(), (now + lease).to_rfc3339()],
                map_scheduled,
            )
            .optional()?;
        let Some(mut row) = row else {
            return Ok(None);
        };
        row.attachments = scheduled_attachments(&conn, account_id, row.id)?;
        Ok(Some(row))
    }

    /// Recovery input (contract §4 step 1): `sending` rows whose lease has
    /// lapsed, i.e. a worker that died mid-send. Each must be answered by a
    /// Message-ID search BEFORE it can be retried: found → [`Self::settle_scheduled_sent`],
    /// not found → [`Self::requeue_scheduled_send`].
    pub fn expired_sending_scheduled_sends(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
    ) -> Result<Vec<ScheduledSend>> {
        let conn = self.lock()?;
        read_many(
            &conn,
            account_id,
            "state = 'sending' AND lease_until IS NOT NULL AND lease_until <= ?2",
            Some(now),
        )
    }

    /// `sending` → `sent`, with the echo's local ids when the echo landed.
    /// `false` when the row is not `sending` (already settled, cancelled out
    /// from under a stale worker, unknown, another account's).
    pub fn settle_scheduled_sent(
        &self,
        account_id: AccountId,
        id: i64,
        sent_message_id: Option<i64>,
        sent_thread_id: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE scheduled_sends
             SET state = 'sent', lease_until = NULL, sent_message_id = ?3,
                 sent_thread_id = ?4, updated_at = ?5
             WHERE account_id = ?1 AND id = ?2 AND state = 'sending'",
            params![
                account_id,
                id,
                sent_message_id,
                sent_thread_id,
                now.to_rfc3339()
            ],
        )?;
        Ok(n > 0)
    }

    /// A failed attempt with retries left: `sending` → `scheduled` at
    /// `next_send_at`, lease cleared, `last_error` kept for the listing.
    /// `last_error` is a REDACTED reason, never content. `false` when the row
    /// is not `sending`.
    pub fn settle_scheduled_retry(
        &self,
        account_id: AccountId,
        id: i64,
        next_send_at: DateTime<Utc>,
        last_error: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE scheduled_sends
             SET state = 'scheduled', lease_until = NULL, send_at = ?3,
                 last_error = ?4, updated_at = ?5
             WHERE account_id = ?1 AND id = ?2 AND state = 'sending'",
            params![
                account_id,
                id,
                next_send_at.to_rfc3339(),
                last_error,
                now.to_rfc3339()
            ],
        )?;
        Ok(n > 0)
    }

    /// Out of retries: `sending` → `failed`, lease cleared. The row waits for
    /// the user (send now, or cancel). `last_error` is a REDACTED reason.
    /// `false` when the row is not `sending`.
    pub fn settle_scheduled_failed(
        &self,
        account_id: AccountId,
        id: i64,
        last_error: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE scheduled_sends
             SET state = 'failed', lease_until = NULL, last_error = ?3, updated_at = ?4
             WHERE account_id = ?1 AND id = ?2 AND state = 'sending'",
            params![account_id, id, last_error, now.to_rfc3339()],
        )?;
        Ok(n > 0)
    }

    /// Recovery's "not found in Gmail" branch: `sending` → `scheduled`, lease
    /// cleared, `send_at` and `attempts` UNCHANGED (the lapsed attempt already
    /// counted; its `send_at` is past, so the next claim takes it at once).
    /// `false` when the row is not `sending`.
    pub fn requeue_scheduled_send(
        &self,
        account_id: AccountId,
        id: i64,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE scheduled_sends
             SET state = 'scheduled', lease_until = NULL, updated_at = ?3
             WHERE account_id = ?1 AND id = ?2 AND state = 'sending'",
            params![account_id, id, now.to_rfc3339()],
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
        let tx = conn.transaction()?;
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
        tx.execute(
            "UPDATE scheduled_sends
             SET state = 'cancelled', lease_until = NULL, updated_at = ?3
             WHERE account_id = ?1 AND id = ?2 AND state IN ('scheduled', 'failed')",
            params![account_id, id, now.to_rfc3339()],
        )?;
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
                 last_error = CASE WHEN state = 'failed' THEN NULL ELSE last_error END
             WHERE account_id = ?1 AND id = ?2 AND state IN ('scheduled', 'failed')",
            params![account_id, id, now.to_rfc3339()],
        )?;
        Ok(n > 0)
    }

    /// Drop settled (`sent`, `cancelled`) rows whose last move is older than
    /// [`SCHEDULED_SETTLED_TTL`], with any files still claimed by them (a
    /// cancel released its files and a send consumed its own, so this is a
    /// belt). ONE TRANSACTION. Returns how many rows went.
    pub fn sweep_scheduled_sends(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        let cutoff = (now - SCHEDULED_SETTLED_TTL).to_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        const STALE: &str = "SELECT id FROM scheduled_sends
             WHERE account_id = ?1 AND state IN ('sent', 'cancelled') AND updated_at < ?2";
        tx.execute(
            &format!(
                "DELETE FROM outbound_attachments
                 WHERE account_id = ?1 AND scheduled_send_id IN ({STALE})"
            ),
            params![account_id, cutoff],
        )?;
        let n = tx.execute(
            "DELETE FROM scheduled_sends
             WHERE account_id = ?1 AND state IN ('sent', 'cancelled') AND updated_at < ?2",
            params![account_id, cutoff],
        )?;
        tx.commit()?;
        Ok(n)
    }
}
