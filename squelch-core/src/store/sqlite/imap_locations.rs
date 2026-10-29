//! The IMAP source's location table (`imap_locations`): which folder and UID
//! each of an IMAP account's messages was last seen at. Provider-private
//! lookup state: the engine never reads it, and the cursor stays in
//! `sync_cursor`. See `crate::mail::imap::MessageLocations`.

use super::*;
use crate::mail::imap::{MessageLocation, MessageLocations};

impl MessageLocations for SqliteStore {
    fn message_locations(
        &self,
        account_id: AccountId,
        msg_id: &str,
    ) -> Result<Vec<MessageLocation>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare_cached(
            "SELECT folder, uidvalidity, uid, message_id FROM imap_locations
             WHERE account_id = ?1 AND msg_id = ?2
             ORDER BY seen_at DESC, folder",
        )?;
        let rows = stmt
            .query_map(params![account_id, msg_id], |r| {
                Ok(MessageLocation {
                    folder: r.get(0)?,
                    // u32 on the wire, stored through i64: never negative.
                    uidvalidity: r.get::<_, i64>(1)? as u32,
                    uid: r.get::<_, i64>(2)? as u32,
                    message_id: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn record_message_locations(
        &self,
        account_id: AccountId,
        seen: &[(String, MessageLocation)],
    ) -> Result<()> {
        if seen.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let now = Utc::now().to_rfc3339();
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO imap_locations
                     (account_id, msg_id, folder, uidvalidity, uid, message_id, seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(account_id, msg_id, folder) DO UPDATE SET
                     uidvalidity = excluded.uidvalidity,
                     uid = excluded.uid,
                     message_id = COALESCE(excluded.message_id, imap_locations.message_id),
                     seen_at = excluded.seen_at",
            )?;
            for (msg_id, loc) in seen {
                stmt.execute(params![
                    account_id,
                    msg_id,
                    loc.folder,
                    i64::from(loc.uidvalidity),
                    i64::from(loc.uid),
                    loc.message_id,
                    now,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn forget_message_location(
        &self,
        account_id: AccountId,
        msg_id: &str,
        folder: &str,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "DELETE FROM imap_locations WHERE account_id = ?1 AND msg_id = ?2 AND folder = ?3",
            params![account_id, msg_id, folder],
        )?;
        Ok(())
    }

    fn content_digest(&self, account_id: AccountId, msg_id: &str) -> Result<Option<String>> {
        let conn = self.lock()?;
        Ok(conn
            .query_row(
                "SELECT content_sha FROM imap_digests WHERE account_id = ?1 AND msg_id = ?2",
                params![account_id, msg_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    fn record_content_digest(
        &self,
        account_id: AccountId,
        msg_id: &str,
        digest: &str,
    ) -> Result<()> {
        let conn = self.lock()?;
        // First writer wins: an id is never rebound to other content.
        conn.execute(
            "INSERT INTO imap_digests (account_id, msg_id, content_sha, recorded_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(account_id, msg_id) DO NOTHING",
            params![account_id, msg_id, digest, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }
}
