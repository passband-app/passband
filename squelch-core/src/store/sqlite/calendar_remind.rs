//! Calendar reminders, the stored half ([`crate::gcal::remind`],
//! docs/CALENDAR.md "Reminders and tickets"): each item's plan
//! (`calendar_reminders`), each firing (`calendar_reminder_fires`, kept 7
//! days so the phone can fetch one by id), the owner's on/off preference
//! (`app_settings` key [`ENABLED_KEY`]), and the linked mail a plan reads.
//! HUMAN DOOR ONLY: no agent route reads any of it.

use super::ghosts::{ELIGIBLE, ELIGIBLE_JOINS};
use super::translations::SEALED_SQL;
use super::*;
use crate::gcal::remind::{
    Fire, LinkedAttachment, LinkedMail, PROMPT_VERSION, Plan, StoredPlan, backoff,
};
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
                        // A display name that is an address ("tix@x.com",
                        // "Jane (jane@corp.com)") is no name: the prompt
                        // never carries an address.
                        sender_name: from_name.filter(|n| {
                            !n.trim().is_empty() && !n.contains(['@', '\u{FF20}', '\u{FE6B}'])
                        }),
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
    /// replacing whatever it had, and clear its backoff.
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
        conn.execute(
            "DELETE FROM calendar_reminder_backoff WHERE account_id=?1 AND item_key=?2",
            params![account, key],
        )?;
        Ok(())
    }

    /// An ask about `key` failed (no answer, a refusal, a timeout): back it
    /// off ([`backoff`]) from `now`.
    pub fn calendar_reminder_failed(
        &self,
        account: AccountId,
        key: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let conn = self.lock()?;
        let failures: u32 = conn
            .query_row(
                "SELECT failures FROM calendar_reminder_backoff WHERE account_id=?1 AND item_key=?2",
                params![account, key],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0)
            + 1;
        let retry_at = (now + backoff(failures)).timestamp();
        conn.execute(
            "INSERT INTO calendar_reminder_backoff(account_id,item_key,failures,retry_at)
             VALUES(?1,?2,?3,?4)
             ON CONFLICT(account_id,item_key) DO UPDATE SET failures=excluded.failures,
                 retry_at=excluded.retry_at",
            params![account, key, failures, retry_at],
        )?;
        Ok(())
    }

    /// The items backed off at `now`: not to be asked about yet.
    pub fn calendar_reminder_backed_off(
        &self,
        account: AccountId,
        now: DateTime<Utc>,
    ) -> Result<HashSet<String>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT item_key FROM calendar_reminder_backoff WHERE account_id=?1 AND retry_at>?2",
        )?;
        let rows = stmt.query_map(params![account, now.timestamp()], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// THE DAILY CAP, durable: take one of `day`'s `cap` model calls, in one
    /// statement, counting every ATTEMPT (a timeout or a failure too). False
    /// when none is left. Days before `day` are forgotten as it goes.
    pub fn calendar_reminder_take_call(
        &self,
        account: AccountId,
        day: &str,
        cap: u32,
    ) -> Result<bool> {
        let conn = self.lock()?;
        conn.execute(
            "DELETE FROM calendar_reminder_calls WHERE account_id=?1 AND day<?2",
            params![account, day],
        )?;
        let taken = conn.execute(
            "INSERT INTO calendar_reminder_calls(account_id,day,calls) VALUES(?1,?2,1)
             ON CONFLICT(account_id,day) DO UPDATE SET calls=calls+1 WHERE calls<?3",
            params![account, day, cap],
        )?;
        Ok(taken > 0 && cap > 0)
    }

    /// Of `addrs`, the ones that are KNOWN CONTACTS (the owner has written to
    /// them: the store's one definition, `is_known_contact`), lowercased.
    pub fn calendar_known_among(
        &self,
        account: AccountId,
        addrs: &[String],
    ) -> Result<HashSet<String>> {
        let conn = self.lock()?;
        let mut out = HashSet::new();
        for addr in addrs {
            let addr = addr.trim().to_ascii_lowercase();
            if !addr.is_empty()
                && !out.contains(&addr)
                && super::messages::is_known_contact_conn(&conn, account, &addr)?
            {
                out.insert(addr);
            }
        }
        Ok(out)
    }

    /// Forget the plans (and backoffs) of items that are gone or over: every
    /// key not in `keep`. With `events` false the events' keys (`e:`) are
    /// all kept: a pass that could not list the calendar (no grant, a
    /// keychain read that failed) knows nothing of them. How many plans went.
    pub fn calendar_reminders_prune(
        &self,
        account: AccountId,
        keep: &HashSet<String>,
        events: bool,
    ) -> Result<usize> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut gone = 0;
        for table in ["calendar_reminders", "calendar_reminder_backoff"] {
            let keys: Vec<String> = tx
                .prepare(&format!("SELECT item_key FROM {table} WHERE account_id=?1"))?
                .query_map(params![account], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for key in keys
                .iter()
                .filter(|k| !keep.contains(*k) && (events || !k.starts_with("e:")))
            {
                let n = tx.execute(
                    &format!("DELETE FROM {table} WHERE account_id=?1 AND item_key=?2"),
                    params![account, key],
                )?;
                if table == "calendar_reminders" {
                    gone += n;
                }
            }
        }
        tx.commit()?;
        Ok(gone)
    }

    /// The firings still kept, oldest first.
    pub fn calendar_reminder_fired(&self, account: AccountId) -> Result<Vec<Fire>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT item_key, start_key, fired_at, covers FROM calendar_reminder_fires
             WHERE account_id=?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![account], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .map(|(item_key, start_key, fired_at, covers)| Fire {
                item_key,
                start_key,
                fired_at: DateTime::from_timestamp(fired_at, 0).unwrap_or_default(),
                covers: serde_json::from_str(&covers).unwrap_or_default(),
            })
            .collect())
    }

    /// FIRE ONCE: record a firing of `key` for the start `start_key`, with
    /// the ghost keys it `covers`, its frame made by `frame` from the new
    /// row's id, in one transaction. None when that start has already fired
    /// (nothing written): a reminder fires once per start instant.
    pub fn calendar_reminder_fire(
        &self,
        account: AccountId,
        key: &str,
        start_key: &str,
        covers: &[String],
        now: DateTime<Utc>,
        frame: impl FnOnce(i64) -> String,
    ) -> Result<Option<(i64, String)>> {
        let covers = serde_json::to_string(covers)
            .map_err(|e| CoreError::Other(anyhow::anyhow!("reminder covers: {e}")))?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO calendar_reminder_fires(account_id,item_key,start_key,fired_at,covers,frame)
             VALUES(?1,?2,?3,?4,?5,'')",
            params![account, key, start_key, now.timestamp(), covers],
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
                &["g:4".to_string()],
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
                &[],
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
                &[],
                now(),
                |id| id.to_string(),
            )
            .unwrap();
        assert!(moved.is_some());
        let fires = s.calendar_reminder_fired(a).unwrap();
        assert_eq!(fires.len(), 2);
        assert_eq!(fires[0].covers, vec!["g:4"], "what it stood for is kept");
        assert_eq!(fires[0].fired_at, now());
        assert!(fires[1].covers.is_empty());
    }

    #[test]
    fn a_frame_is_this_accounts_and_kept_seven_days() {
        let (s, a) = store();
        let b = s.ensure_account("other@example.com").unwrap();
        let (id, _) = s
            .calendar_reminder_fire(a, "g:1", "2026-10-31", &[], now(), |id| format!("f{id}"))
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
        // A pass that could not list the calendar keeps the events' plans.
        assert_eq!(s.calendar_reminders_prune(a, &keep, false).unwrap(), 0);
        assert_eq!(s.calendar_reminders_prune(a, &keep, true).unwrap(), 1);
        assert_eq!(
            s.calendar_reminder_plans(a)
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            vec!["g:2"]
        );
    }

    #[test]
    fn a_fire_id_is_never_reused_once_the_week_is_pruned() {
        let (s, a) = store();
        let (first, _) = s
            .calendar_reminder_fire(a, "g:1", "2026-10-31", &[], now(), |_| "f".into())
            .unwrap()
            .unwrap();
        let later = now() + FIRES_KEPT + chrono::Duration::seconds(1);
        assert_eq!(s.calendar_reminder_fires_prune(a, later).unwrap(), 1);
        let (second, _) = s
            .calendar_reminder_fire(a, "g:1", "2026-11-09", &[], later, |_| "f".into())
            .unwrap()
            .unwrap();
        assert!(second > first, "{second} after {first}");
    }

    #[test]
    fn an_old_fires_table_is_rebuilt_with_covers() {
        let (s, a) = store();
        exec(
            &s,
            "DROP TABLE calendar_reminder_fires;
             CREATE TABLE calendar_reminder_fires (
                 id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
                 item_key TEXT NOT NULL, start_key TEXT NOT NULL,
                 fired_at INTEGER NOT NULL, frame TEXT NOT NULL,
                 UNIQUE(account_id, item_key, start_key));
             INSERT INTO calendar_reminder_fires(account_id,item_key,start_key,fired_at,frame)
                 VALUES(1,'g:1','2026-10-31',0,'f');",
        );
        let conn = s.lock().unwrap();
        super::super::migrate::rebuild_calendar_reminder_fires(&conn).unwrap();
        // Twice is once.
        super::super::migrate::rebuild_calendar_reminder_fires(&conn).unwrap();
        drop(conn);
        assert!(s.calendar_reminder_fired(a).unwrap().is_empty());
        s.calendar_reminder_fire(a, "g:1", "2026-10-31", &["g:2".into()], now(), |_| {
            "f".into()
        })
        .unwrap()
        .unwrap();
        assert_eq!(s.calendar_reminder_fired(a).unwrap()[0].covers, vec!["g:2"]);
    }

    #[test]
    fn a_failed_ask_backs_off_doubling_and_a_plan_clears_it() {
        let (s, a) = store();
        let t = now();
        s.calendar_reminder_failed(a, "g:1", t).unwrap();
        let off = |at| s.calendar_reminder_backed_off(a, at).unwrap();
        assert!(off(t).contains("g:1"));
        assert!(off(t + chrono::Duration::minutes(29)).contains("g:1"));
        assert!(off(t + chrono::Duration::minutes(30)).is_empty());
        s.calendar_reminder_failed(a, "g:1", t).unwrap();
        assert!(off(t + chrono::Duration::minutes(59)).contains("g:1"));
        for _ in 0..6 {
            s.calendar_reminder_failed(a, "g:1", t).unwrap();
        }
        assert!(off(t + chrono::Duration::minutes(239)).contains("g:1"));
        assert!(
            off(t + chrono::Duration::minutes(240)).is_empty(),
            "at most 4h"
        );
        let plan = Plan {
            remind: true,
            lead_minutes: 30,
            line: None,
            open: None,
            ticket: None,
            same_as: Vec::new(),
        };
        s.calendar_reminder_failed(a, "g:1", t).unwrap();
        s.calendar_reminder_put(a, "g:1", "v", &plan, t).unwrap();
        assert!(off(t).is_empty(), "a plan clears it");
        // Pruned with the plans.
        s.calendar_reminder_failed(a, "g:9", t).unwrap();
        s.calendar_reminders_prune(a, &HashSet::new(), true)
            .unwrap();
        assert!(off(t).is_empty());
    }

    #[test]
    fn the_daily_cap_counts_every_attempt_and_survives_a_reopen() {
        let (s, a) = store();
        for _ in 0..3 {
            assert!(s.calendar_reminder_take_call(a, "2026-10-30", 3).unwrap());
        }
        assert!(!s.calendar_reminder_take_call(a, "2026-10-30", 3).unwrap());
        // Another account, and the next day, have their own.
        let b = s.ensure_account("other@example.com").unwrap();
        assert!(s.calendar_reminder_take_call(b, "2026-10-30", 3).unwrap());
        assert!(s.calendar_reminder_take_call(a, "2026-10-31", 3).unwrap());
        assert!(!s.calendar_reminder_take_call(a, "2026-10-31", 0).unwrap());
    }

    #[test]
    fn known_organizers_are_the_known_contact_floor() {
        let (s, a) = store();
        exec(
            &s,
            "INSERT INTO contacts(account_id,addr,sent_count,first_seen) VALUES(1,'Pal@Example.com',2,'2026-01-01');
             INSERT INTO contacts(account_id,addr,sent_count,first_seen) VALUES(1,'lurker@example.com',0,'2026-01-01');",
        );
        let got = s
            .calendar_known_among(
                a,
                &[
                    "pal@example.com".into(),
                    " PAL@example.com".into(),
                    "lurker@example.com".into(),
                    "stranger@example.com".into(),
                ],
            )
            .unwrap();
        assert_eq!(got, HashSet::from(["pal@example.com".to_string()]));
    }

    #[test]
    fn a_display_name_that_is_an_address_is_no_name() {
        let (s, a) = store();
        seed(&s, a, 1);
        seed(&s, a, 2);
        exec(
            &s,
            "UPDATE messages SET from_name='tix@ticketmaster.example' WHERE id=1",
        );
        exec(
            &s,
            "UPDATE messages SET from_name='Jane (jane\u{FF20}corp.example)' WHERE id=2",
        );
        let got = s.calendar_linked_mail(a, &[1, 2]).unwrap();
        assert_eq!(got[&1].sender_name, None);
        assert_eq!(got[&2].sender_name, None);
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
