//! Smart re-notify, the stored half (issue #625, docs/RENOTIFY.md): the Mac's
//! connection sessions, the candidates a re-notify may name, the owner's busy
//! intervals, and the record of every banner and what came of it.
//!
//! The decisions are not made here. [`crate::renotify`] is the pure planner;
//! this module only answers its questions and writes down what it chose.
//!
//! TIMES ARE COMPARED PARSED, NOT AS TEXT. `to_rfc3339` writes a variable
//! number of fraction digits, so two stamps in the same second can sort the
//! wrong way as strings. Reads here are bounded by id (newest first) and the
//! time tests run in Rust.

use super::*;
use crate::renotify::{BusySpan, Candidate, Outcome, Policy, Trigger, Why};
use crate::triage::ghost::EventTime;
use crate::types::EventKind;

/// Sessions kept per account. Enough to read a month of a Mac's comings and
/// goings; the trigger only ever needs the last one.
const SESSIONS_KEPT: i64 = 500;
/// Banners kept per account (their items go with them).
const BATCHES_KEPT: i64 = 2000;
/// Newest `events` rows a candidate scan reads. The planner's age ceiling is
/// days, so this is far more than will ever be due at once.
const CANDIDATE_SCAN: i64 = 400;

impl SqliteStore {
    /// Record a Mac event stream opening, and say how long the Mac had been
    /// away: `None` when this account has never had a session (a first
    /// connection is onboarding, not arrival), zero when another session is
    /// still open (a second window, or a reconnect that beat the old
    /// connection's teardown), else the time since the latest disconnect.
    pub fn open_client_session(
        &self,
        account_id: AccountId,
        platform: &str,
        now: DateTime<Utc>,
    ) -> Result<(i64, Option<chrono::Duration>)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let rows: Vec<Option<DateTime<Utc>>> = {
            let mut stmt = tx.prepare(
                "SELECT disconnected_at FROM client_sessions
                 WHERE account_id = ?1 AND platform = ?2
                 ORDER BY id DESC LIMIT 50",
            )?;
            stmt.query_map(params![account_id, platform], |r| dt_opt(r, 0))?
                .collect::<std::result::Result<_, _>>()?
        };
        let away = if rows.is_empty() {
            None
        } else if rows.iter().any(Option::is_none) {
            Some(chrono::Duration::zero())
        } else {
            rows.iter()
                .flatten()
                .max()
                .map(|last| (now - *last).max(chrono::Duration::zero()))
        };
        tx.execute(
            "INSERT INTO client_sessions(account_id, platform, connected_at) VALUES(?1,?2,?3)",
            params![account_id, platform, now.to_rfc3339()],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "DELETE FROM client_sessions WHERE account_id = ?1 AND id <= ?2 - ?3",
            params![account_id, id, SESSIONS_KEPT],
        )?;
        tx.commit()?;
        Ok((id, away))
    }

    /// Close one session. Idempotent: an already-closed row keeps its stamp.
    pub fn close_client_session(
        &self,
        account_id: AccountId,
        session_id: i64,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE client_sessions SET disconnected_at = ?3
             WHERE account_id = ?1 AND id = ?2 AND disconnected_at IS NULL",
            params![account_id, session_id, now.to_rfc3339()],
        )?;
        Ok(n > 0)
    }

    /// Close every session a previous process left open, at `now`. Run once at
    /// startup, before any stream is served. `now` rather than the real (lost)
    /// end is the conservative reading: it can only make the next gap look
    /// SHORTER, so a restart never manufactures an arrival.
    pub fn close_stale_client_sessions(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        let conn = self.lock()?;
        Ok(conn.execute(
            "UPDATE client_sessions SET disconnected_at = ?2
             WHERE account_id = ?1 AND disconnected_at IS NULL",
            params![account_id, now.to_rfc3339()],
        )?)
    }

    /// Is session `session_id` still open?
    pub fn client_session_open(&self, account_id: AccountId, session_id: i64) -> Result<bool> {
        let conn = self.lock()?;
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM client_sessions
             WHERE account_id = ?1 AND id = ?2 AND disconnected_at IS NULL)",
            params![account_id, session_id],
            |r| r.get(0),
        )?)
    }

    /// The owner's busy intervals overlapping `[from, to)`, from the calendars
    /// Passband reads. TIMES ONLY: no title, location or guest is selected,
    /// which is what lets the scheduler read the calendar at all (the tables
    /// are otherwise human-door only).
    ///
    /// Not busy: all-day events, events marked free (transparent), events the
    /// owner declined, and cancelled ones. A timed event with no end is taken
    /// as half an hour.
    pub fn calendar_busy(
        &self,
        account_id: AccountId,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<BusySpan>> {
        if to <= from {
            return Ok(Vec::new());
        }
        // An event that started up to a day before `from` can still overlap it.
        let low = super::ghosts::sort_key(&EventTime::At(
            (from - chrono::Duration::days(1)).fixed_offset(),
        ));
        let high = super::ghosts::sort_key(&EventTime::At(to.fixed_offset()));
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT e.starts_at, e.ends_at
             FROM calendar_events e
             JOIN calendar_calendars c ON c.account_id = e.account_id AND c.calendar_id = e.calendar_id
             WHERE e.account_id = ?1 AND c.selected = 1
               AND e.start_sort >= ?2 AND e.start_sort <= ?3
               AND e.all_day = 0 AND e.transparent = 0
               AND COALESCE(e.self_response, '') != 'declined'
               AND e.status != 'cancelled'",
        )?;
        let rows = stmt
            .query_map(params![account_id, low, high], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let at = |raw: &str| match EventTime::parse(raw)? {
            EventTime::At(t) => Some(t.with_timezone(&Utc)),
            EventTime::Date(_) => None,
        };
        let mut out: Vec<BusySpan> = rows
            .into_iter()
            .filter_map(|(start, end)| {
                let start = at(&start)?;
                let end = end
                    .as_deref()
                    .and_then(at)
                    .filter(|e| *e > start)
                    .unwrap_or(start + chrono::Duration::minutes(30));
                Some(BusySpan { start, end })
            })
            .filter(|s| s.end > from && s.start < to)
            .collect();
        out.sort_by_key(|s| (s.start, s.end));
        Ok(out)
    }

    /// Messages that notified once and are STILL UNHANDLED, newest first, with
    /// what re-notifies have already been spent on each. The planner decides
    /// which are due; this decides which are eligible at all:
    ///
    /// - it has an `events` row (it earned a first banner), that is not an
    ///   outbound-open notice and not an auth alert (a login code is stale by
    ///   the time a re-notify could fire);
    /// - it is received mail, not spam;
    /// - the owner has not opened it, marked it done or archived it, or set a
    ///   reminder (their own word on when it comes back wins);
    /// - nobody on the account has replied in its thread since.
    pub fn renotify_candidates(&self, account_id: AccountId) -> Result<Vec<Candidate>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT e.id, e.message_id, e.thread_id, e.kind, e.importance, e.sender,
                    e.one_line, e.deadline, e.created_at,
                    (SELECT COUNT(*) FROM renotify_items i
                     WHERE i.account_id = e.account_id AND i.message_id = e.message_id
                       AND i.due_at IS NULL)
             FROM events e
             JOIN messages m ON m.account_id = e.account_id AND m.id = e.message_id
             LEFT JOIN triage t ON t.account_id = m.account_id AND t.message_id = m.id
             WHERE e.account_id = ?1
               AND e.kind != 'opened' AND e.is_auth = 0 AND e.sealed_kind IS NULL
               AND m.is_sent = 0 AND m.is_spam = 0
               AND t.opened_at IS NULL
               AND COALESCE(t.status, 'new') != 'done'
               AND t.remind_at IS NULL
               AND NOT EXISTS (
                   SELECT 1 FROM messages s
                   WHERE s.account_id = m.account_id AND s.thread_id = m.thread_id
                     AND s.is_sent = 1 AND s.id > m.id)
             ORDER BY e.id DESC
             LIMIT ?2",
        )?;
        let mut out = stmt
            .query_map(params![account_id, CANDIDATE_SCAN], |r| {
                Ok(Candidate {
                    why: Why::Unhandled,
                    event_id: Some(r.get(0)?),
                    message_id: r.get(1)?,
                    thread_id: r.get(2)?,
                    kind: EventKind::parse(&r.get::<_, String>(3)?).unwrap_or(EventKind::Surfaced),
                    importance: r.get::<_, i64>(4)?.clamp(0, 255) as u8,
                    sender: r.get(5)?,
                    one_line: r.get(6)?,
                    deadline: r.get(7)?,
                    due_at: None,
                    notified_at: dt(r, 8)?,
                    renotified: r.get::<_, i64>(9)?.clamp(0, u32::MAX as i64) as u32,
                    last_renotified_at: None,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for c in &mut out {
            c.last_renotified_at = last_renotified(&conn, account_id, c.message_id)?;
        }
        Ok(out)
    }

    /// Mail in For your eyes with a due date, opened or not: the
    /// [`Why::DueSoon`] candidates. Read from the FYE list itself
    /// (`agent_triage::list_items`), so "in For your eyes" means exactly what
    /// the app shows, the owner's own pins and fired reminders included.
    ///
    /// The due time is the earliest unresolved action's due date, else the
    /// thread's `attention_at`, resolved by [`crate::renotify::due_instant`].
    /// Left out: auth mail, mail marked done, snoozed or replied to, and any
    /// message already re-notified for this same due time (once per due date;
    /// a due date that MOVES is a new one). The window itself is the
    /// planner's to judge.
    pub fn renotify_due_candidates(
        &self,
        account_id: AccountId,
        policy: &Policy,
        zone: chrono_tz::Tz,
        now: DateTime<Utc>,
    ) -> Result<Vec<Candidate>> {
        let conn = self.lock()?;
        let items = super::agent_triage::list_items(&conn, account_id, "fye", now)?;
        let mut still_open = conn.prepare_cached(
            "SELECT EXISTS(
                 SELECT 1 FROM messages m
                 LEFT JOIN triage t ON t.account_id = m.account_id AND t.message_id = m.id
                 WHERE m.account_id = ?1 AND m.id = ?2
                   AND m.is_sent = 0 AND m.is_spam = 0
                   AND COALESCE(t.status, 'new') != 'done'
                   AND t.remind_at IS NULL
                   AND NOT EXISTS (
                       SELECT 1 FROM messages s
                       WHERE s.account_id = m.account_id AND s.thread_id = m.thread_id
                         AND s.is_sent = 1 AND s.id > m.id))",
        )?;
        let mut already = conn.prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM renotify_items
             WHERE account_id = ?1 AND message_id = ?2 AND due_at = ?3)",
        )?;
        let mut event =
            conn.prepare_cached("SELECT id FROM events WHERE account_id = ?1 AND message_id = ?2")?;
        let mut out = Vec::new();
        for item in items {
            if item.decision.auth.is_auth() {
                continue;
            }
            let attention = &item.attention;
            let action_due = attention
                .actions
                .iter()
                .filter(|a| !a.resolved)
                .filter_map(|a| a.due.as_ref())
                .filter_map(|d| crate::renotify::due_instant(policy, d, zone))
                .min();
            let Some(due_at) = action_due.or_else(|| {
                attention
                    .factors
                    .attention_at
                    .as_ref()
                    .and_then(|d| crate::renotify::due_instant(policy, d, zone))
            }) else {
                continue;
            };
            let open: bool =
                still_open.query_row(params![account_id, item.message_id], |r| r.get(0))?;
            if !open {
                continue;
            }
            let done: bool = already.query_row(
                params![account_id, item.message_id, due_at.to_rfc3339()],
                |r| r.get(0),
            )?;
            if done {
                continue;
            }
            let Some(arrived) = DateTime::parse_from_rfc3339(&item.received_at)
                .ok()
                .map(|t| t.with_timezone(&Utc))
            else {
                continue;
            };
            let one_line = if item.decision.summary.trim().is_empty() {
                attention.summary.clone()
            } else {
                item.decision.summary.clone()
            };
            out.push(Candidate {
                why: Why::DueSoon,
                event_id: event
                    .query_row(params![account_id, item.message_id], |r| r.get(0))
                    .optional()?,
                message_id: item.message_id,
                thread_id: item.thread_id.clone(),
                kind: EventKind::Deadline,
                importance: (attention.factors.importance.clamp(0.0, 1.0) * 100.0).round() as u8,
                sender: item.from_addr.clone(),
                one_line,
                deadline: Some(due_at.to_rfc3339()),
                due_at: Some(due_at),
                notified_at: arrived,
                renotified: 0,
                last_renotified_at: last_renotified(&conn, account_id, item.message_id)?,
            });
        }
        Ok(out)
    }

    /// Write one banner and the messages it names, in order, each with the due
    /// time it was re-notified for when it came as [`Why::DueSoon`]. Returns its id,
    /// which rides the frame so the app can report a tap against it.
    pub fn record_renotify(
        &self,
        account_id: AccountId,
        trigger: Trigger,
        fired_at: DateTime<Utc>,
        listeners: usize,
        items: &[(i64, Option<DateTime<Utc>>)],
    ) -> Result<i64> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO renotify_batches(account_id, trigger, fired_at, listeners)
             VALUES(?1,?2,?3,?4)",
            params![
                account_id,
                trigger.as_str(),
                fired_at.to_rfc3339(),
                listeners as i64
            ],
        )?;
        let id = tx.last_insert_rowid();
        for (position, (message_id, due_at)) in items.iter().enumerate() {
            tx.execute(
                "INSERT INTO renotify_items(batch_id, account_id, message_id, position, due_at)
                 VALUES(?1,?2,?3,?4,?5)",
                params![
                    id,
                    account_id,
                    message_id,
                    position as i64,
                    due_at.map(|d| d.to_rfc3339())
                ],
            )?;
        }
        tx.execute(
            "DELETE FROM renotify_batches WHERE account_id = ?1 AND id <= ?2 - ?3",
            params![account_id, id, BATCHES_KEPT],
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Banners fired at or after `since`: the day's budget so far.
    pub fn renotify_fired_since(&self, account_id: AccountId, since: DateTime<Utc>) -> Result<u32> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT fired_at FROM renotify_batches WHERE account_id = ?1 ORDER BY id DESC LIMIT 100",
        )?;
        let fired = stmt
            .query_map(params![account_id], |r| dt(r, 0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(fired.iter().filter(|t| **t >= since).count() as u32)
    }

    /// The app reports a tap on banner `batch_id`, optionally naming which of
    /// its messages it opened. FIRST TAP ONLY, like `triage.opened_at`. False
    /// for a banner this account does not have, or a message it did not name.
    pub fn mark_renotify_opened(
        &self,
        account_id: AccountId,
        batch_id: i64,
        message_id: Option<i64>,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let conn = self.lock()?;
        if let Some(message_id) = message_id {
            let named: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM renotify_items
                 WHERE account_id = ?1 AND batch_id = ?2 AND message_id = ?3)",
                params![account_id, batch_id, message_id],
                |r| r.get(0),
            )?;
            if !named {
                return Ok(false);
            }
        }
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM renotify_batches WHERE account_id = ?1 AND id = ?2)",
            params![account_id, batch_id],
            |r| r.get(0),
        )?;
        if !exists {
            return Ok(false);
        }
        conn.execute(
            "UPDATE renotify_batches
             SET opened_at = ?3, opened_message_id = ?4
             WHERE account_id = ?1 AND id = ?2 AND opened_at IS NULL",
            params![account_id, batch_id, now.to_rfc3339(), message_id],
        )?;
        Ok(true)
    }

    /// Every banner fired at or after `since`, with what came of it: tapped,
    /// and how many of its messages the owner dealt with after it fired
    /// (opened, done or archived, or replied to). Newest first.
    pub fn renotify_outcomes(
        &self,
        account_id: AccountId,
        since: DateTime<Utc>,
    ) -> Result<Vec<Outcome>> {
        let conn = self.lock()?;
        let batches: Vec<(i64, String, DateTime<Utc>, bool)> = {
            let mut stmt = conn.prepare(
                "SELECT id, trigger, fired_at, opened_at IS NOT NULL FROM renotify_batches
                 WHERE account_id = ?1 ORDER BY id DESC LIMIT ?2",
            )?;
            stmt.query_map(params![account_id, BATCHES_KEPT], |r| {
                Ok((r.get(0)?, r.get(1)?, dt(r, 2)?, r.get(3)?))
            })?
            .collect::<std::result::Result<_, _>>()?
        };
        let mut items = conn.prepare_cached(
            "SELECT t.opened_at, t.status, t.resolved_at,
                    EXISTS(SELECT 1 FROM messages s
                           WHERE s.account_id = m.account_id AND s.thread_id = m.thread_id
                             AND s.is_sent = 1 AND s.id > m.id)
             FROM renotify_items i
             LEFT JOIN messages m ON m.account_id = i.account_id AND m.id = i.message_id
             LEFT JOIN triage t ON t.account_id = i.account_id AND t.message_id = i.message_id
             WHERE i.account_id = ?1 AND i.batch_id = ?2",
        )?;
        let mut out = Vec::new();
        for (id, trigger, fired_at, opened) in batches {
            if fired_at < since {
                continue;
            }
            let Some(trigger) = Trigger::parse(&trigger) else {
                continue;
            };
            let rows = items
                .query_map(params![account_id, id], |r| {
                    Ok((
                        dt_opt(r, 0)?,
                        r.get::<_, Option<String>>(1)?,
                        dt_opt(r, 2)?,
                        r.get::<_, Option<bool>>(3)?.unwrap_or(false),
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let handled = rows
                .iter()
                .filter(|(opened_at, status, resolved_at, replied)| {
                    // A reply in the thread is handling it whenever it came:
                    // the candidate query already excluded mail replied to
                    // BEFORE the banner.
                    *replied
                        || opened_at.is_some_and(|t| t >= fired_at)
                        || (status.as_deref() == Some("done")
                            && resolved_at.is_some_and(|t| t >= fired_at))
                })
                .count();
            out.push(Outcome {
                trigger,
                fired_at,
                opened,
                items: rows.len() as u32,
                handled: handled as u32,
            });
        }
        Ok(out)
    }
}

/// When `message_id` was last named in a banner, if ever (any reason).
fn last_renotified(
    conn: &Connection,
    account_id: AccountId,
    message_id: i64,
) -> Result<Option<DateTime<Utc>>> {
    let mut stmt = conn.prepare_cached(
        "SELECT b.fired_at FROM renotify_items i
         JOIN renotify_batches b ON b.id = i.batch_id
         WHERE i.account_id = ?1 AND i.message_id = ?2
         ORDER BY b.id DESC LIMIT 1",
    )?;
    Ok(stmt
        .query_row(params![account_id, message_id], |r| dt(r, 0))
        .optional()?)
}
