//! Calendar reminders, the stored half ([`crate::gcal::remind`],
//! docs/CALENDAR.md "Reminders and tickets"): each item's plan
//! (`calendar_reminders`), each firing (`calendar_reminder_fires`, kept 7
//! days so the phone can fetch one by id), the owner's on/off preference
//! (`app_settings` key [`ENABLED_KEY`]), and the linked mail a plan reads.
//! HUMAN DOOR ONLY: no agent route reads any of it.

use super::ghosts::{ELIGIBLE, ELIGIBLE_JOINS};
use super::translations::SEALED_SQL;
use super::*;
use crate::gcal::remind::{LinkedAttachment, LinkedMail, PROMPT_VERSION, Plan, StoredPlan};
use std::collections::{HashMap, HashSet};

/// The owner preference: `'0'` is off; absent (or anything else) is on.
pub const ENABLED_KEY: &str = "calendar_reminders";
/// How long a firing is kept, and fetchable.
pub const FIRES_KEPT: chrono::Duration = chrono::Duration::days(7);
/// Message ids per query, under SQLite's bound-parameter ceiling.
const IDS_PER_QUERY: usize = 400;

impl SqliteStore {
    /// The linked mail a reminder plan may read, by message id: subject,
    /// sender display name, received date and the non-inline attachments
    /// with bytes stored. A message that is spam, sent by the owner, sealed
    /// (an auth decision, or the legacy sealed row), or not allowed by the
    /// access review is NOT RETURNED, whatever the caller asked for: the
    /// ghost listing already holds its sources to the same rule
    /// (`ghosts::ELIGIBLE`), and this read does not trust that it did.
    pub fn calendar_linked_mail(
        &self,
        account: AccountId,
        ids: &[i64],
    ) -> Result<HashMap<i64, LinkedMail>> {
        let conn = self.lock()?;
        let mut out = HashMap::new();
        let mut wanted: Vec<i64> = ids.to_vec();
        wanted.sort_unstable();
        wanted.dedup();
        let mut attachments = conn.prepare(
            "SELECT id, filename, mime FROM attachments
             WHERE account_id=?1 AND message_id=?2 AND data IS NOT NULL
               AND NOT (content_id IS NOT NULL AND lower(mime) LIKE 'image/%')
             ORDER BY id",
        )?;
        for chunk in wanted.chunks(IDS_PER_QUERY) {
            let marks = vec!["?"; chunk.len()].join(",");
            let mut stmt = conn.prepare(&format!(
                "SELECT m.id, m.thread_id, m.subject, m.from_name, m.received_at
                 FROM messages m {ELIGIBLE_JOINS}
                 WHERE m.account_id=? AND m.id IN ({marks}) AND {ELIGIBLE}
                   AND NOT {SEALED_SQL}"
            ))?;
            let mut bound: Vec<rusqlite::types::Value> = vec![account.into()];
            bound.extend(chunk.iter().map(|id| rusqlite::types::Value::Integer(*id)));
            let rows = stmt
                .query_map(rusqlite::params_from_iter(bound), |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (id, thread_id, subject, from_name, received) in rows {
                let Ok(received_at) = DateTime::parse_from_rfc3339(&received) else {
                    continue;
                };
                let attachments = attachments
                    .query_map(params![account, id], |r| {
                        Ok(LinkedAttachment {
                            id: r.get(0)?,
                            filename: r.get(1)?,
                            mime: r.get(2)?,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                out.insert(
                    id,
                    LinkedMail {
                        message_id: id,
                        thread_id,
                        subject,
                        sender_name: from_name.filter(|n| !n.trim().is_empty()),
                        received_at: received_at.with_timezone(&Utc),
                        attachments,
                    },
                );
            }
        }
        Ok(out)
    }

    /// Every stored plan, by item key. A row whose plan does not parse is
    /// skipped (it is asked again).
    pub fn calendar_reminder_plans(
        &self,
        account: AccountId,
    ) -> Result<HashMap<String, StoredPlan>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT item_key, version, prompt_version, plan_json
             FROM calendar_reminders WHERE account_id=?1",
        )?;
        let rows = stmt
            .query_map(params![account], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(key, version, prompt_version, json)| {
                let plan: Plan = serde_json::from_str(&json).ok()?;
                Some((
                    key,
                    StoredPlan {
                        version,
                        prompt_version,
                        plan,
                    },
                ))
            })
            .collect())
    }

    /// Store an item's plan for `version`, under the current prompt,
    /// replacing whatever it had.
    pub fn calendar_reminder_put(
        &self,
        account: AccountId,
        key: &str,
        version: &str,
        plan: &Plan,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let conn = self.lock()?;
        let json = serde_json::to_string(plan)
            .map_err(|e| CoreError::Other(anyhow::anyhow!("reminder plan: {e}")))?;
        conn.execute(
            "INSERT INTO calendar_reminders(account_id,item_key,version,prompt_version,plan_json,planned_at)
             VALUES(?1,?2,?3,?4,?5,?6)
             ON CONFLICT(account_id,item_key) DO UPDATE SET version=excluded.version,
                 prompt_version=excluded.prompt_version, plan_json=excluded.plan_json,
                 planned_at=excluded.planned_at",
            params![account, key, version, PROMPT_VERSION, json, now.to_rfc3339()],
        )?;
        Ok(())
    }

    /// Forget the plans of items that are gone or over: every key not in
    /// `keep`. How many went.
    pub fn calendar_reminders_prune(
        &self,
        account: AccountId,
        keep: &HashSet<String>,
    ) -> Result<usize> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let keys: Vec<String> = tx
            .prepare("SELECT item_key FROM calendar_reminders WHERE account_id=?1")?
            .query_map(params![account], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut gone = 0;
        for key in keys.iter().filter(|k| !keep.contains(*k)) {
            gone += tx.execute(
                "DELETE FROM calendar_reminders WHERE account_id=?1 AND item_key=?2",
                params![account, key],
            )?;
        }
        tx.commit()?;
        Ok(gone)
    }

    /// The (item key, start key) pairs that have fired and are still kept.
    pub fn calendar_reminder_fired(&self, account: AccountId) -> Result<HashSet<(String, String)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT item_key, start_key FROM calendar_reminder_fires WHERE account_id=?1",
        )?;
        let rows = stmt.query_map(params![account], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// FIRE ONCE: record a firing of `key` for the start `start_key`, its
    /// frame made by `frame` from the new row's id, in one transaction. None
    /// when that start has already fired (nothing written): a reminder fires
    /// once per start instant.
    pub fn calendar_reminder_fire(
        &self,
        account: AccountId,
        key: &str,
        start_key: &str,
        now: DateTime<Utc>,
        frame: impl FnOnce(i64) -> String,
    ) -> Result<Option<(i64, String)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO calendar_reminder_fires(account_id,item_key,start_key,fired_at,frame)
             VALUES(?1,?2,?3,?4,'')",
            params![account, key, start_key, now.timestamp()],
        )?;
        if inserted == 0 {
            return Ok(None);
        }
        let id = tx.last_insert_rowid();
        let body = frame(id);
        tx.execute(
            "UPDATE calendar_reminder_fires SET frame=?2 WHERE id=?1",
            params![id, body],
        )?;
        tx.commit()?;
        Ok(Some((id, body)))
    }

    /// One firing's frame, for this account and within [`FIRES_KEPT`] of
    /// `now`. None for an unknown id, another account's, or an expired one:
    /// the route answers all three with the same 404.
    pub fn calendar_reminder_frame(
        &self,
        account: AccountId,
        id: i64,
        now: DateTime<Utc>,
    ) -> Result<Option<String>> {
        let conn = self.lock()?;
        let since = (now - FIRES_KEPT).timestamp();
        Ok(conn
            .query_row(
                "SELECT frame FROM calendar_reminder_fires
                 WHERE account_id=?1 AND id=?2 AND fired_at>=?3 AND frame<>''",
                params![account, id, since],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Forget firings older than [`FIRES_KEPT`]. How many went.
    pub fn calendar_reminder_fires_prune(
        &self,
        account: AccountId,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        let conn = self.lock()?;
        Ok(conn.execute(
            "DELETE FROM calendar_reminder_fires WHERE account_id=?1 AND fired_at<?2",
            params![account, (now - FIRES_KEPT).timestamp()],
        )?)
    }

    /// The owner preference: on unless set off.
    pub fn calendar_reminders_enabled(&self, account: AccountId) -> Result<bool> {
        let conn = self.lock()?;
        let v: Option<String> = conn
            .query_row(
                "SELECT value FROM app_settings WHERE account_id=?1 AND key=?2",
                params![account, ENABLED_KEY],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v.as_deref().map(str::trim) != Some("0"))
    }

    /// Set the owner preference.
    pub fn set_calendar_reminders_enabled(&self, account: AccountId, on: bool) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO app_settings(account_id, key, value) VALUES(?1, ?2, ?3)
             ON CONFLICT(account_id, key) DO UPDATE SET value = excluded.value",
            params![account, ENABLED_KEY, if on { "1" } else { "0" }],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (SqliteStore, AccountId) {
        let s = SqliteStore::open_in_memory().unwrap();
        let a = s.ensure_account("me@example.com").unwrap();
        (s, a)
    }

    fn now() -> DateTime<Utc> {
        "2026-10-30T18:00:00Z".parse().unwrap()
    }

    #[test]
    fn a_start_fires_once_and_a_moved_start_fires_again() {
        let (s, a) = store();
        let first = s
            .calendar_reminder_fire(
                a,
                "e:primary/abc",
                "2026-10-31T03:00:00+00:00",
                now(),
                |id| format!("{{\"calendar_reminder\":{id}}}"),
            )
            .unwrap()
            .unwrap();
        assert_eq!(first.1, format!("{{\"calendar_reminder\":{}}}", first.0));
        let again = s
            .calendar_reminder_fire(
                a,
                "e:primary/abc",
                "2026-10-31T03:00:00+00:00",
                now(),
                |_| panic!("a start that fired is not built again"),
            )
            .unwrap();
        assert_eq!(again, None);
        let moved = s
            .calendar_reminder_fire(
                a,
                "e:primary/abc",
                "2026-10-31T04:00:00+00:00",
                now(),
                |id| id.to_string(),
            )
            .unwrap();
        assert!(moved.is_some());
        assert_eq!(s.calendar_reminder_fired(a).unwrap().len(), 2);
    }

    #[test]
    fn a_frame_is_this_accounts_and_kept_seven_days() {
        let (s, a) = store();
        let b = s.ensure_account("other@example.com").unwrap();
        let (id, _) = s
            .calendar_reminder_fire(a, "g:1", "2026-10-31", now(), |id| format!("f{id}"))
            .unwrap()
            .unwrap();
        assert_eq!(
            s.calendar_reminder_frame(a, id, now()).unwrap(),
            Some(format!("f{id}"))
        );
        assert_eq!(s.calendar_reminder_frame(b, id, now()).unwrap(), None);
        assert_eq!(s.calendar_reminder_frame(a, id + 1, now()).unwrap(), None);
        let later = now() + FIRES_KEPT + chrono::Duration::seconds(1);
        assert_eq!(s.calendar_reminder_frame(a, id, later).unwrap(), None);
        assert_eq!(s.calendar_reminder_fires_prune(b, later).unwrap(), 0);
        assert_eq!(s.calendar_reminder_fires_prune(a, now()).unwrap(), 0);
        assert_eq!(s.calendar_reminder_fires_prune(a, later).unwrap(), 1);
    }

    #[test]
    fn plans_round_trip_and_prune_by_key() {
        let (s, a) = store();
        let plan = Plan {
            remind: true,
            lead_minutes: 60,
            line: Some("Bring your ID".into()),
            open: None,
            ticket: None,
            same_as: vec![3],
        };
        s.calendar_reminder_put(a, "e:primary/abc", "v1", &plan, now())
            .unwrap();
        s.calendar_reminder_put(a, "g:2", "v1", &plan, now())
            .unwrap();
        let plans = s.calendar_reminder_plans(a).unwrap();
        assert_eq!(plans["g:2"].plan, plan);
        assert_eq!(plans["g:2"].prompt_version, PROMPT_VERSION);
        let keep = HashSet::from(["g:2".to_string()]);
        assert_eq!(s.calendar_reminders_prune(a, &keep).unwrap(), 1);
        assert_eq!(
            s.calendar_reminder_plans(a)
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            vec!["g:2"]
        );
    }

    /// Seed a received, reviewed message with one PDF, one inline image and
    /// one attachment over the ingest cap.
    fn seed(s: &SqliteStore, a: AccountId, id: i64) {
        let conn = s.lock().unwrap();
        conn.execute(
            "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,from_name,subject,
                                  received_at,snippet)
             VALUES(?1,?2,?3,?4,'tix@ticketmaster.example','Ticketmaster','Your tickets',
                    '2026-10-01T10:00:00+00:00','')",
            params![id, a, format!("g{id}"), format!("t{id}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_message_state(account_id,message_id,content_snapshot,access)
             VALUES(?1,?2,'','allowed')",
            params![a, id],
        )
        .unwrap();
        for (n, (name, mime, cid, data)) in [
            ("tickets.pdf", "application/pdf", None, Some(vec![1u8])),
            ("logo.png", "image/png", Some("logo@x"), Some(vec![1u8])),
            ("huge.pdf", "application/pdf", None, None),
        ]
        .into_iter()
        .enumerate()
        {
            conn.execute(
                "INSERT INTO attachments(account_id,message_id,filename,mime,size_bytes,data,content_id)
                 VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![a, id, name, mime, n as i64 + 1, data, cid],
            )
            .unwrap();
        }
    }

    fn exec(s: &SqliteStore, sql: &str) {
        s.lock().unwrap().execute_batch(sql).unwrap();
    }

    #[test]
    fn linked_mail_is_never_spam_sealed_or_unreviewed_mail() {
        let (s, a) = store();
        for id in 1..=5 {
            seed(&s, a, id);
        }
        exec(&s, "UPDATE messages SET is_spam=1 WHERE id=2");
        exec(
            &s,
            "INSERT INTO agent_message_decisions(account_id,message_id,revision,decision_json,decided_at)
             VALUES(1,3,1,'{\"auth\":{\"kinds\":[\"login_code\"]}}','2026-10-01T10:00:00Z')",
        );
        exec(
            &s,
            "UPDATE agent_message_state SET access='restricted' WHERE message_id=4",
        );
        exec(&s, "UPDATE messages SET is_sent=1 WHERE id=5");
        let got = s.calendar_linked_mail(a, &[1, 2, 3, 4, 5, 99]).unwrap();
        assert_eq!(got.keys().copied().collect::<Vec<_>>(), vec![1]);
        let m = &got[&1];
        assert_eq!(m.subject, "Your tickets");
        assert_eq!(m.sender_name.as_deref(), Some("Ticketmaster"));
        assert_eq!(m.thread_id, "t1");
        // The inline image and the attachment with no bytes are not offered.
        let names: Vec<&str> = m.attachments.iter().map(|a| a.filename.as_str()).collect();
        assert_eq!(names, vec!["tickets.pdf"]);
        // Another account asks for the same id and gets nothing.
        let b = s.ensure_account("other@example.com").unwrap();
        assert!(s.calendar_linked_mail(b, &[1]).unwrap().is_empty());
    }

    #[test]
    fn the_preference_is_on_until_turned_off() {
        let (s, a) = store();
        assert!(s.calendar_reminders_enabled(a).unwrap());
        s.set_calendar_reminders_enabled(a, false).unwrap();
        assert!(!s.calendar_reminders_enabled(a).unwrap());
        assert_eq!(
            s.get_app_setting(a, ENABLED_KEY).unwrap().as_deref(),
            Some("0")
        );
        s.set_calendar_reminders_enabled(a, true).unwrap();
        assert!(s.calendar_reminders_enabled(a).unwrap());
    }
}
