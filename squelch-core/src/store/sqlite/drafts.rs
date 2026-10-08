//! Local drafts: the human door's unsent compositions.

use super::*;

/// The picker's PARENT CONTEXT (thread, subject, sender address, sender
/// name), in that order, read off the `m` that [`parent_join`] brings in.
/// Shared by drafts and the outbox (`scheduled.rs`), which show the same
/// "who / what" line for a composition that replies to or forwards a stored
/// message.
pub(super) const PARENT_COLS: &str = "m.thread_id, m.subject, m.from_addr, m.from_name";

/// `LEFT JOIN messages m` onto the row aliased `owner`, matching the message
/// whose id is the SQL expression `parent_id`. PINNED TO THE SAME ACCOUNT, so
/// a composition can never borrow another account's message by id; plain new
/// mail, and a composition whose parent is gone, join nothing and read `NULL`s.
pub(super) fn parent_join(owner: &str, parent_id: &str) -> String {
    format!(
        "LEFT JOIN messages m
      ON m.account_id = {owner}.account_id
     AND m.id = {parent_id}"
    )
}

/// Every draft read goes through this one SELECT, so the row shape and the
/// parent join cannot drift between the listing and the single-row reads.
///
/// The LEFT JOIN fills the picker's context from the replied-to or forwarded
/// message (see [`parent_join`]).
fn draft_select() -> String {
    format!(
        "SELECT d.id, d.account_id, d.reply_to_message_id,
        d.forward_of_message_id, d.to_addr, d.cc_addr, d.bcc_addr, d.subject, d.body,
        d.created_at, d.updated_at, {PARENT_COLS}, d.from_agent
    FROM drafts d
    {}",
        parent_join(
            "d",
            "COALESCE(d.reply_to_message_id, d.forward_of_message_id)"
        )
    )
}

/// One [`draft_select`] row into a [`Draft`].
fn map_draft(r: &rusqlite::Row<'_>) -> rusqlite::Result<Draft> {
    Ok(Draft {
        id: r.get(0)?,
        account_id: r.get(1)?,
        reply_to_message_id: r.get(2)?,
        forward_of_message_id: r.get(3)?,
        to_addr: r.get(4)?,
        cc_addr: r.get(5)?,
        bcc_addr: r.get(6)?,
        subject: r.get(7)?,
        body: r.get(8)?,
        // Filled by the caller, which still holds the connection: a row map
        // cannot run a second query.
        attachments: Vec::new(),
        created_at: dt(r, 9)?,
        updated_at: dt(r, 10)?,
        thread_id: r.get(11)?,
        parent_subject: r.get(12)?,
        parent_from_addr: r.get(13)?,
        parent_from_name: r.get(14)?,
        from_agent: r.get(15)?,
    })
}

/// One draft by id with its files, on a connection the caller already holds.
fn read_draft(conn: &Connection, account_id: AccountId, id: i64) -> Result<Option<Draft>> {
    let row = conn
        .query_row(
            &format!("{} WHERE d.account_id = ?1 AND d.id = ?2", draft_select()),
            params![account_id, id],
            map_draft,
        )
        .optional()?;
    let Some(mut draft) = row else {
        return Ok(None);
    };
    draft.attachments = super::outbound::draft_attachments(conn, account_id, id)?;
    Ok(Some(draft))
}

impl SqliteStore {
    /// HUMAN-DOOR ONLY (`/client/drafts`): save one draft, addressed by `key`
    /// (see [`DraftKey`]). Returns the stored row, or `None` ONLY for
    /// [`DraftKey::NewById`] naming an unknown id or another account's (the
    /// handler turns both into the same 404).
    ///
    /// A row carries at most one of the reply/forward keys, and the enum makes
    /// "both" unrepresentable: a key-addressed insert writes exactly one.
    pub fn upsert_draft(
        &self,
        account_id: AccountId,
        key: DraftKey,
        fields: DraftFields<'_>,
        now: DateTime<Utc>,
    ) -> Result<Option<Draft>> {
        let conn = self.lock()?;
        // SELECT-then-write under ONE held lock instead of an upsert: the
        // uniqueness lives in PARTIAL indexes, which `ON CONFLICT(...)` cannot
        // name as a conflict target.
        let existing: Option<i64> = match key {
            DraftKey::Reply(parent) => conn
                .query_row(
                    "SELECT id FROM drafts WHERE account_id = ?1 AND reply_to_message_id = ?2",
                    params![account_id, parent],
                    |r| r.get(0),
                )
                .optional()?,
            DraftKey::Forward(parent) => conn
                .query_row(
                    "SELECT id FROM drafts WHERE account_id = ?1 AND forward_of_message_id = ?2",
                    params![account_id, parent],
                    |r| r.get(0),
                )
                .optional()?,
            DraftKey::NewById(id) => {
                let found: Option<i64> = conn
                    .query_row(
                        "SELECT id FROM drafts WHERE account_id = ?1 AND id = ?2",
                        params![account_id, id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if found.is_none() {
                    return Ok(None);
                }
                found
            }
            DraftKey::NewCreate => None,
            // `id DESC` breaks an `updated_at` tie deterministically.
            DraftKey::NewLatest => conn
                .query_row(
                    "SELECT id FROM drafts
                     WHERE account_id = ?1
                       AND reply_to_message_id IS NULL AND forward_of_message_id IS NULL
                     ORDER BY updated_at DESC, id DESC LIMIT 1",
                    params![account_id],
                    |r| r.get(0),
                )
                .optional()?,
        };
        let id = match existing {
            // An edit is the SAME composition: `id`, `created_at` and the key
            // are left alone, only the fields and `updated_at` move.
            Some(id) => {
                conn.execute(
                    "UPDATE drafts SET to_addr = ?3, cc_addr = ?4, bcc_addr = ?5,
                            subject = ?6, body = ?7, updated_at = ?8
                     WHERE account_id = ?1 AND id = ?2",
                    params![
                        account_id,
                        id,
                        fields.to_addr,
                        fields.cc_addr,
                        fields.bcc_addr,
                        fields.subject,
                        fields.body,
                        now.to_rfc3339()
                    ],
                )?;
                id
            }
            None => {
                let (reply_to, forward_of) = match key {
                    DraftKey::Reply(p) => (Some(p), None),
                    DraftKey::Forward(p) => (None, Some(p)),
                    DraftKey::NewById(_) | DraftKey::NewCreate | DraftKey::NewLatest => {
                        (None, None)
                    }
                };
                conn.execute(
                    "INSERT INTO drafts(account_id, reply_to_message_id, forward_of_message_id,
                         to_addr, cc_addr, bcc_addr, subject, body, created_at, updated_at)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?9)",
                    params![
                        account_id,
                        reply_to,
                        forward_of,
                        fields.to_addr,
                        fields.cc_addr,
                        fields.bcc_addr,
                        fields.subject,
                        fields.body,
                        now.to_rfc3339(),
                    ],
                )?;
                conn.last_insert_rowid()
            }
        };
        read_draft(&conn, account_id, id)
    }

    /// AGENT DOOR'S ONE DRAFT WRITE (issue #394), reached only through
    /// squelch-api's `AgentMailer`: insert a draft marked `from_agent`, as a
    /// reply to `reply_to` or as a new message, WITH ITS AUDIT ROW.
    ///
    /// INSERT ONLY, NEVER AN EDIT. [`Self::upsert_draft`] edits the one reply
    /// draft a message has, which is right for the owner's composer and wrong
    /// here: the row it would overwrite is something the owner is in the middle
    /// of writing, and an agent cannot read drafts to know it is there. So a
    /// reply whose message already has a draft writes nothing.
    ///
    /// BOUNDED. An account holds at most `cap` unsent agent drafts; past that
    /// nothing is written until the owner sends or discards some. Without it a
    /// looping (or hostile) agent fills the drafts list and the database.
    ///
    /// FAIL-CLOSED, like `set_sender_rule_audited`: the draft and its ledger
    /// row (`audit`, whose detail is completed here with the new id) share ONE
    /// transaction, so an agent's draft never stands untraced.
    pub fn create_agent_draft(
        &self,
        account_id: AccountId,
        reply_to: Option<i64>,
        fields: DraftFields<'_>,
        now: DateTime<Utc>,
        cap: usize,
        audit: &NewAuditEntry,
    ) -> Result<AgentDraftOutcome> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        if let Some(parent) = reply_to {
            let taken: Option<i64> = tx
                .query_row(
                    "SELECT id FROM drafts WHERE account_id = ?1 AND reply_to_message_id = ?2",
                    params![account_id, parent],
                    |r| r.get(0),
                )
                .optional()?;
            if taken.is_some() {
                return Ok(AgentDraftOutcome::ReplyDraftExists);
            }
        }
        let held: i64 = tx.query_row(
            "SELECT COUNT(*) FROM drafts WHERE account_id = ?1 AND from_agent = 1",
            params![account_id],
            |r| r.get(0),
        )?;
        if held as usize >= cap {
            return Ok(AgentDraftOutcome::TooMany);
        }
        tx.execute(
            "INSERT INTO drafts(account_id, reply_to_message_id, to_addr, cc_addr, bcc_addr,
                 subject, body, from_agent, created_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,1,?8,?8)",
            params![
                account_id,
                reply_to,
                fields.to_addr,
                fields.cc_addr,
                fields.bcc_addr,
                fields.subject,
                fields.body,
                now.to_rfc3339(),
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO audit_log(account_id, ts, actor, action, target, detail)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                account_id,
                now.to_rfc3339(),
                audit.actor,
                audit.action,
                audit.target,
                format!("ok:{id}"),
            ],
        )?;
        tx.commit()?;
        Ok(AgentDraftOutcome::Created(id))
    }

    /// One draft by id, files included. `None` for an unknown id and for
    /// another account's.
    pub fn draft_by_id(&self, account_id: AccountId, id: i64) -> Result<Option<Draft>> {
        let conn = self.lock()?;
        read_draft(&conn, account_id, id)
    }

    /// Every draft for an account, most recently touched first.
    pub fn list_drafts(&self, account_id: AccountId) -> Result<Vec<Draft>> {
        let conn = self.lock()?;
        // Drafts are human-only; external access restrictions do not hide them.
        let mut stmt = conn.prepare(&format!(
            "{} WHERE d.account_id = ?1 ORDER BY d.updated_at DESC, d.id DESC",
            draft_select()
        ))?;
        let mut out = stmt
            .query_map(params![account_id], map_draft)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for draft in &mut out {
            draft.attachments = super::outbound::draft_attachments(&conn, account_id, draft.id)?;
        }
        Ok(out)
    }

    /// Discard one draft by id. `false` when nothing matched, so another
    /// account's id is indistinguishable from an unknown one (the handler turns
    /// both into 404).
    ///
    /// THE DRAFT'S FILES GO WITH IT. A staged upload lives as long as the draft
    /// that claimed it (see `outbound_attachments` in schema.sql); a discarded
    /// draft's files would otherwise sit unreachable until the sweep, holding
    /// their bytes for nothing.
    ///
    /// ONE TRANSACTION, like `claim_outbound_attachments`: files gone with the
    /// draft left standing (or the reverse) is a state neither door expects.
    pub fn delete_draft(&self, account_id: AccountId, id: i64) -> Result<bool> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM outbound_attachments WHERE account_id = ?1 AND draft_id = ?2",
            params![account_id, id],
        )?;
        let n = tx.execute(
            "DELETE FROM drafts WHERE account_id = ?1 AND id = ?2",
            params![account_id, id],
        )?;
        tx.commit()?;
        Ok(n > 0)
    }
}
