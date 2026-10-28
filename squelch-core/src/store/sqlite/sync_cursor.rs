//! The account's mail provider and the sync engine's opaque change-feed
//! cursor (`accounts.provider`, `sync_cursor`).
//!
//! THE LEGACY GMAIL CURSOR IS ADOPTED BOTH WAYS. Before `sync_cursor` existed
//! the engine kept Gmail's historyId in `sync_state` under mailbox
//! `'history'`, and other readers still look there (the stats page's
//! `last_history_id`). So for a Gmail account:
//!
//! - a READ with no `sync_cursor` row adopts the legacy row, so an upgraded
//!   install resumes its feed instead of re-running a first backfill, and a
//!   read with both takes the later historyId, so a rolled-back build that
//!   kept advancing the legacy row does not send this one back over mail it
//!   already walked;
//! - a WRITE of a historyId writes the legacy row in the same transaction, so
//!   a rolled-back build resumes from where this one stopped.
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
        // `as u64`, exactly as `sync_state` reads it back: the column holds a
        // u64 written through an i64 cast.
        let legacy = legacy.map(|l| l as u64);
        Ok(match (cursor, legacy) {
            (Some(cursor), Some(legacy)) => match cursor.trim().parse::<u64>() {
                Ok(current) => Some(SyncCursor::new(current.max(legacy).to_string())),
                // Not a historyId: not ours to second-guess. The source
                // decides what an unreadable cursor means.
                Err(_) => Some(SyncCursor::new(cursor)),
            },
            (Some(cursor), None) => Some(SyncCursor::new(cursor)),
            (None, Some(legacy)) => Some(SyncCursor::new(legacy.to_string())),
            (None, None) => None,
        })
    }

    pub(super) fn set_sync_cursor(&self, account_id: AccountId, cursor: &SyncCursor) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO sync_cursor(account_id, cursor, updated_at) VALUES(?1, ?2, ?3)
             ON CONFLICT(account_id) DO UPDATE SET
                 cursor = excluded.cursor, updated_at = excluded.updated_at",
            params![account_id, cursor.as_str(), Utc::now().to_rfc3339()],
        )?;
        let gmail =
            provider_conn(&tx, account_id)?.as_deref() == Some(MailProvider::Gmail.as_str());
        if gmail && let Ok(history_id) = cursor.as_str().trim().parse::<u64>() {
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
