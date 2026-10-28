//! The account's mail provider and the sync engine's opaque change-feed
//! cursor (`accounts.provider`, `sync_cursor`).
//!
//! FOR A GMAIL ACCOUNT THE LEGACY ROW IS THE CURSOR. Before `sync_cursor`
//! existed the engine kept Gmail's historyId in `sync_state` under mailbox
//! `'history'`, and three things still depend on that row: the stats page's
//! `last_history_id`, a rolled-back build (which reads nothing else), and the
//! operator's hand on it (docs/SECURITY.md §1: delete it and restart to
//! re-fetch the window, which is how a sanitizer fix reaches stored mail;
//! lower it to replay history, or set 0 to force a catch-up). So for a Gmail
//! account a READ is the legacy row, exactly as the engine always read it,
//! and a WRITE stores the historyId in both places in one transaction; a Gmail
//! cursor that is not a historyId is refused rather than stored where nothing
//! could read it back. Its `sync_cursor` row is a mirror for the provider-
//! neutral table, never read for a Gmail account.
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

    /// Set which provider an account is. `squelchd auth --imap` is the only
    /// writer, and it writes LAST, once the server config and the password
    /// are stored, so an account never reads as IMAP without them.
    pub fn set_account_provider(
        &self,
        account_id: AccountId,
        provider: MailProvider,
    ) -> Result<()> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE accounts SET provider = ?1 WHERE id = ?2",
            params![provider.as_str(), account_id],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound);
        }
        Ok(())
    }

    pub(super) fn sync_cursor(&self, account_id: AccountId) -> Result<Option<SyncCursor>> {
        let conn = self.lock()?;
        if provider_conn(&conn, account_id)?.as_deref() == Some(MailProvider::Gmail.as_str()) {
            let legacy: Option<i64> = conn
                .query_row(
                    "SELECT last_uid FROM sync_state WHERE account_id = ?1 AND mailbox = ?2",
                    params![account_id, LEGACY_GMAIL_CURSOR_KEY],
                    |r| r.get(0),
                )
                .optional()?;
            // `as u64`, exactly as `sync_state` reads it back: the column
            // holds a u64 written through an i64 cast.
            return Ok(legacy.map(|l| SyncCursor::new((l as u64).to_string())));
        }
        let cursor: Option<String> = conn
            .query_row(
                "SELECT cursor FROM sync_cursor WHERE account_id = ?1",
                params![account_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(cursor.map(SyncCursor::new))
    }

    pub(super) fn set_sync_cursor(&self, account_id: AccountId, cursor: &SyncCursor) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        // An unknown account has no provider to encode for, and a row for it
        // would be an orphan nothing reads.
        let provider = provider_conn(&tx, account_id)?.ok_or(CoreError::NotFound)?;
        let gmail = provider == MailProvider::Gmail.as_str();
        let history_id = if gmail {
            match cursor.as_str().trim().parse::<u64>() {
                Ok(history_id) => Some(history_id),
                // It could not be mirrored, and the legacy row is what a read
                // returns (see the module doc): stored, it would be lost
                // without a word. Said here instead, with nothing written.
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
