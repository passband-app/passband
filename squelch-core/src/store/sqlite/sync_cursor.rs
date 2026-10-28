//! The account's mail provider and the sync engine's opaque change-feed
//! cursor (`accounts.provider`, `sync_cursor`).
//!
//! FOR A GMAIL ACCOUNT THE LEGACY ROW STAYS IN CHARGE. Before `sync_cursor`
//! existed the engine kept Gmail's historyId in `sync_state` under mailbox
//! `'history'`, and three things still depend on that row: the stats page's
//! `last_history_id`, a rolled-back build (which reads nothing else), and the
//! documented RE-INGEST DRILL (docs/SECURITY.md §1: delete the row and restart
//! to re-fetch the window, which is how a sanitizer fix reaches stored mail).
//! So for a Gmail account:
//!
//! - a WRITE stores the historyId in both places in one transaction, and a
//!   Gmail cursor that is not a historyId is refused rather than stored where
//!   nothing could read it back;
//! - a READ with no legacy row is no cursor, whatever `sync_cursor` holds:
//!   that is the drill, and the next run backfills as a first run does;
//! - a READ with the legacy row and no `sync_cursor` row (an install upgraded
//!   from before the table) adopts it, so the feed resumes instead of
//!   re-running a first backfill;
//! - a READ with both takes the later historyId, so a rolled-back build that
//!   kept advancing the legacy row does not send this one back over mail it
//!   already walked, and no version change ever steps the feed backwards.
//!
//! Nothing here applies to any other provider: their cursors live in
//! `sync_cursor` alone and are never parsed.

use super::*;
use crate::mail::{MailProvider, SyncCursor};

/// The `sync_state` mailbox key of the legacy Gmail historyId row.
pub(super) const LEGACY_GMAIL_CURSOR_KEY: &str = "history";

impl SqliteStore {
    pub(super) fn account_provider(&self, account_id: AccountId) -> Result<MailProvider> {
        let conn = self.lock()?;
        let stored = provider_conn(&conn, account_id)?.ok_or(CoreError::NotFound)?;
        MailProvider::parse(&stored)
    }

    pub(super) fn sync_cursor(&self, account_id: AccountId) -> Result<Option<SyncCursor>> {
        let conn = self.lock()?;
        let cursor: Option<String> = conn
            .query_row(
                "SELECT cursor FROM sync_cursor WHERE account_id = ?1",
                params![account_id],
                |r| r.get(0),
            )
            .optional()?;
        if provider_conn(&conn, account_id)?.as_deref() != Some(MailProvider::Gmail.as_str()) {
            return Ok(cursor.map(SyncCursor::new));
        }
        let legacy: Option<i64> = conn
            .query_row(
                "SELECT last_uid FROM sync_state WHERE account_id = ?1 AND mailbox = ?2",
                params![account_id, LEGACY_GMAIL_CURSOR_KEY],
                |r| r.get(0),
            )
            .optional()?;
        // No legacy row is no cursor: a first run, or the re-ingest drill.
        let Some(legacy) = legacy else {
            return Ok(None);
        };
        // `as u64`, exactly as `sync_state` reads it back: the column holds a
        // u64 written through an i64 cast.
        let legacy = legacy as u64;
        // A `sync_cursor` value that is not a historyId was never written by
        // this code (the write refuses one); the legacy row stands.
        let current = cursor.and_then(|c| c.trim().parse::<u64>().ok());
        Ok(Some(SyncCursor::new(
            current.map_or(legacy, |c| c.max(legacy)).to_string(),
        )))
    }

    pub(super) fn set_sync_cursor(&self, account_id: AccountId, cursor: &SyncCursor) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let gmail =
            provider_conn(&tx, account_id)?.as_deref() == Some(MailProvider::Gmail.as_str());
        let history_id = if gmail {
            match cursor.as_str().trim().parse::<u64>() {
                Ok(history_id) => Some(history_id),
                // Stored, it would read back as the legacy row's value or as
                // no cursor at all (see the module doc): a silent rewind or a
                // backfill on every start. Said here instead, with nothing
                // written; the value is ours and not mail, so naming the
                // failure is safe.
                Err(_) => {
                    return Err(CoreError::InvalidInput(
                        "a Gmail sync cursor must be a historyId".into(),
                    ));
                }
            }
        } else {
            None
        };
        tx.execute(
            "INSERT INTO sync_cursor(account_id, cursor, updated_at) VALUES(?1, ?2, ?3)
             ON CONFLICT(account_id) DO UPDATE SET
                 cursor = excluded.cursor, updated_at = excluded.updated_at",
            params![account_id, cursor.as_str(), Utc::now().to_rfc3339()],
        )?;
        if let Some(history_id) = history_id {
            tx.execute(
                "INSERT INTO sync_state(account_id, mailbox, uidvalidity, last_uid)
                 VALUES(?1, ?2, 0, ?3)
                 ON CONFLICT(account_id, mailbox) DO UPDATE SET
                     uidvalidity = excluded.uidvalidity, last_uid = excluded.last_uid",
                params![account_id, LEGACY_GMAIL_CURSOR_KEY, history_id as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(super) fn has_message(&self, account_id: AccountId, provider_msg_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE account_id = ?1 AND gmail_msg_id = ?2)",
            params![account_id, provider_msg_id],
            |r| r.get(0),
        )?)
    }
}

/// The stored `accounts.provider` string, `None` for an unknown account.
fn provider_conn(conn: &Connection, account_id: AccountId) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT provider FROM accounts WHERE id = ?1",
            params![account_id],
            |r| r.get(0),
        )
        .optional()?)
}
