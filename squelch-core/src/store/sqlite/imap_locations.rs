//! The IMAP source's location table (`imap_locations`): which folder and UID
//! each of an IMAP account's messages was last seen at. Provider-private
//! lookup state: the engine never reads it, and the cursor stays in
//! `sync_cursor`. See `crate::mail::imap::MessageLocations`.

use super::*;
use crate::mail::imap::{DIGEST_PREFIX, MessageLocation, MessageLocations};

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
             ORDER BY seen_at ASC, rowid ASC",
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
                     -- When the row's PLACE was recorded, not when it was last
                     -- seen: a re-sighting of the same copy keeps its turn
                     -- (Amendment J, first-seen wins).
                     seen_at = CASE
                         WHEN imap_locations.uidvalidity = excluded.uidvalidity
                          AND imap_locations.uid = excluded.uid
                         THEN imap_locations.seen_at
                         ELSE excluded.seen_at END",
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
        let d: Option<String> = conn
            .query_row(
                "SELECT content_sha FROM imap_digests WHERE account_id = ?1 AND msg_id = ?2",
                params![account_id, msg_id],
                |r| r.get(0),
            )
            .optional()?;
        // A body-bound digest from an Amendment I build is no digest at all
        // now: the id is a legacy one (rule 4).
        Ok(d.filter(|d| d.starts_with(DIGEST_PREFIX)))
    }

    fn record_content_digest(
        &self,
        account_id: AccountId,
        msg_id: &str,
        digest: &str,
    ) -> Result<()> {
        let conn = self.lock()?;
        // First writer wins: an id is never rebound to other content. Only a
        // legacy (unprefixed, Amendment I) digest gives way.
        conn.execute(
            "INSERT INTO imap_digests (account_id, msg_id, content_sha, recorded_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(account_id, msg_id) DO UPDATE SET
                 content_sha = excluded.content_sha,
                 recorded_at = excluded.recorded_at
             WHERE substr(imap_digests.content_sha, 1, 3) <> ?5",
            params![
                account_id,
                msg_id,
                digest,
                Utc::now().to_rfc3339(),
                DIGEST_PREFIX
            ],
        )?;
        Ok(())
    }
}
