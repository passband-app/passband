//! Google Calendar, the stored half: `calendar_calendars` (which calendars are
//! read, and each one's sync token and last outcome), `calendar_events`
//! (the events in the synced window) and `calendar_event_guests` (each
//! event's attendees, replaced whole with the event and deleted with it by a
//! trigger). Written by the sync loop
//! ([`crate::gcal::sync`]) and, one event at a time, by the human door after
//! Google accepted the user's own create, edit or delete; read only by the
//! human door. HUMAN DOOR ONLY: no agent route and no triage prompt reads
//! either table.
//!
//! EVERY WRITE IS ONE SHORT TRANSACTION over data already in hand. The sync
//! loop fetches a page, then hands it here; the store's one connection is
//! never held across a call to Google.
//!
//! Starts and ends are stored in the text form ghosts use
//! ([`EventTime`]'s `Display`) and their convention (an all-day end is the
//! inclusive last day, none for a one-day event), and `start_sort` is the same
//! UTC sort key, so the two listings apply one `[from, to)` rule, by start.
use super::ghosts::{in_range, listing_order, sort_key};
use super::*;
use crate::gcal::{
    AccessRole, CalendarErrorKind, CalendarMeta, CalendarStatus, EventItem, EventRow, Guest,
    GuestResponse, MAX_EVENTS_PER_CALENDAR, MAX_GUESTS, PRIMARY, PageApplied, SelfResponse,
    StoredCalendar, SyncedEvent,
};
use crate::triage::ghost::EventTime;
use chrono::FixedOffset;
use std::collections::HashSet;

/// One event calendar search matched: its search key, how many terms it
/// matched, and what ordering it needs, without the full row.
#[derive(Debug, Clone)]
pub struct EventMatch {
    pub key: i64,
    pub matched: usize,
    pub start: EventTime,
    pub end: Option<EventTime>,
    /// The owner's RSVP is a no.
    pub declined: bool,
}

/// Events one timeline listing returns at most: the ones NEAREST now, and the
/// listing says when it cut.
pub const LIST_CAP: usize = 5000;

/// Delete every event of a calendar (their guests go with them, by the
/// trigger) and forget how far it was synced, so the
/// next time it is read it is read in full.
fn forget_calendar(tx: &Connection, account: AccountId, calendar_id: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM calendar_events WHERE account_id=?1 AND calendar_id=?2",
        params![account, calendar_id],
    )?;
    tx.execute(
        "UPDATE calendar_calendars SET sync_token=NULL, synced_at=NULL, full_synced_at=NULL,
                last_error_kind=NULL, failures=0, retry_at=NULL, truncated=0
         WHERE account_id=?1 AND calendar_id=?2",
        params![account, calendar_id],
    )?;
    Ok(())
}

impl SqliteStore {
    /// Every calendar row of the account, the primary first.
    pub fn calendar_calendars(&self, account: AccountId) -> Result<Vec<StoredCalendar>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT calendar_id,summary,is_primary,selected,time_zone,sync_token,synced_at,
                    full_synced_at,last_error_kind,failures,retry_at,truncated,color,access_role
             FROM calendar_calendars WHERE account_id=?1
             ORDER BY is_primary DESC, calendar_id",
        )?;
        let rows = stmt
            .query_map([account], |r| {
                let kind: Option<String> = r.get(8)?;
                let role: Option<String> = r.get(13)?;
                Ok(StoredCalendar {
                    calendar_id: r.get(0)?,
                    summary: r.get(1)?,
                    is_primary: r.get(2)?,
                    selected: r.get(3)?,
                    time_zone: r.get(4)?,
                    sync_token: r.get(5)?,
                    synced_at: dt_opt(r, 6)?,
                    full_synced_at: dt_opt(r, 7)?,
                    last_error_kind: kind.as_deref().and_then(CalendarErrorKind::parse),
                    failures: r.get::<_, i64>(9)?.clamp(0, i64::from(u32::MAX)) as u32,
                    retry_at: dt_opt(r, 10)?,
                    truncated: r.get(11)?,
                    color: r.get(12)?,
                    access_role: role.as_deref().and_then(AccessRole::parse),
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// The calendars this pass reads, in one transaction: each is recorded as
    /// selected (its sync state kept), and every OTHER calendar of the account
    /// that was selected is deselected, its events deleted and its sync state
    /// cleared. A calendar the user hid, unsubscribed from, or that fell past
    /// the cap leaves the timeline at once.
    pub fn calendar_set_selection(
        &self,
        account: AccountId,
        calendars: &[CalendarMeta],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        for cal in calendars {
            tx.execute(
                "INSERT INTO calendar_calendars(account_id,calendar_id,summary,is_primary,selected,time_zone,
                                                color,access_role)
                 VALUES(?1,?2,?3,?4,1,?5,?6,?7)
                 ON CONFLICT(account_id,calendar_id) DO UPDATE SET
                    summary=COALESCE(excluded.summary,summary),
                    is_primary=excluded.is_primary,
                    selected=1,
                    time_zone=COALESCE(excluded.time_zone,time_zone),
                    color=COALESCE(excluded.color,color),
                    access_role=COALESCE(excluded.access_role,access_role)",
                params![
                    account,
                    cal.calendar_id,
                    cal.summary,
                    cal.is_primary,
                    cal.time_zone,
                    cal.color,
                    cal.access_role.map(AccessRole::as_str),
                ],
            )?;
        }
        let keep: HashSet<&str> = calendars.iter().map(|c| c.calendar_id.as_str()).collect();
        let stale: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT calendar_id FROM calendar_calendars WHERE account_id=?1 AND selected=1",
            )?;
            stmt.query_map([account], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|id| !keep.contains(id.as_str()))
                .collect()
        };
        for id in &stale {
            forget_calendar(&tx, account, id)?;
            tx.execute(
                "UPDATE calendar_calendars SET selected=0 WHERE account_id=?1 AND calendar_id=?2",
                params![account, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// What an events page says about its calendar (its title and zone), for
    /// a calendar the list did not describe. Absent values change nothing.
    pub fn calendar_note_meta(
        &self,
        account: AccountId,
        calendar_id: &str,
        summary: Option<&str>,
        time_zone: Option<&str>,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE calendar_calendars SET summary=COALESCE(?3,summary), time_zone=COALESCE(?4,time_zone)
             WHERE account_id=?1 AND calendar_id=?2",
            params![account, calendar_id, summary, time_zone],
        )?;
        Ok(())
    }

    /// Apply one events page in ONE transaction: upserts and removals, then
    /// the per-calendar cap. Past [`MAX_EVENTS_PER_CALENDAR`] the events
    /// FARTHEST from `now` by start are dropped (on a tie the later one, so
    /// the future goes before the past), whichever page brought them, and the
    /// calendar is marked truncated. Google's page order is undefined here,
    /// so a cap that kept whatever came first could keep next year and drop
    /// tomorrow.
    pub fn calendar_apply_page(
        &self,
        account: AccountId,
        calendar_id: &str,
        items: &[EventItem],
        now: DateTime<Utc>,
    ) -> Result<PageApplied> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut stored = Vec::new();
        for item in items {
            match item {
                EventItem::Remove(id) => {
                    tx.execute(
                        "DELETE FROM calendar_events WHERE account_id=?1 AND calendar_id=?2 AND event_id=?3",
                        params![account, calendar_id, id],
                    )?;
                }
                EventItem::Upsert(row) => {
                    upsert_event(&tx, account, calendar_id, row)?;
                    stored.push(row.event_id.clone());
                }
            }
        }
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM calendar_events WHERE account_id=?1 AND calendar_id=?2",
            params![account, calendar_id],
            |r| r.get(0),
        )?;
        let excess = count - MAX_EVENTS_PER_CALENDAR;
        let mut dropped = 0;
        if excess > 0 {
            let now_key = sort_key(&EventTime::At(now.fixed_offset()));
            dropped = tx.execute(
                "DELETE FROM calendar_events WHERE account_id=?1 AND calendar_id=?2 AND event_id IN (
                     SELECT event_id FROM calendar_events WHERE account_id=?1 AND calendar_id=?2
                     ORDER BY ABS(julianday(start_sort) - julianday(?3)) DESC,
                              start_sort DESC, event_id DESC
                     LIMIT ?4)",
                params![account, calendar_id, now_key, excess],
            )?;
            tx.execute(
                "UPDATE calendar_calendars SET truncated=1 WHERE account_id=?1 AND calendar_id=?2",
                params![account, calendar_id],
            )?;
        }
        tx.commit()?;
        Ok(PageApplied { stored, dropped })
    }

    /// A pass over one calendar finished: record its new sync token and the
    /// time, and clear its error and failure count. After a FULL sync, `seen`
    /// is every id it stored, and any other stored event is deleted (it was
    /// deleted at Google, or left the window, while no token was carrying
    /// changes); `cut` is whether the cap dropped events during that read,
    /// which is the calendar's truncated mark from now on. (An incremental
    /// read that drops events marks it as it applies the page, and only a
    /// full read that fits clears it.)
    ///
    /// Called ONLY when the read reached Google's last page. A read cut short
    /// (a failed page, the page cap, shutdown) never gets here, so it never
    /// sweeps on a partial `seen` and keeps the old token.
    pub fn calendar_finish_sync(
        &self,
        account: AccountId,
        calendar_id: &str,
        sync_token: Option<&str>,
        at: DateTime<Utc>,
        seen: Option<(&HashSet<String>, bool)>,
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let at = at.to_rfc3339();
        if let Some((seen, cut)) = seen {
            let stored: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT event_id FROM calendar_events WHERE account_id=?1 AND calendar_id=?2",
                )?;
                stmt.query_map(params![account, calendar_id], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<_, _>>()?
            };
            for id in stored.iter().filter(|id| !seen.contains(*id)) {
                tx.execute(
                    "DELETE FROM calendar_events WHERE account_id=?1 AND calendar_id=?2 AND event_id=?3",
                    params![account, calendar_id, id],
                )?;
            }
            tx.execute(
                "UPDATE calendar_calendars SET full_synced_at=?3, truncated=?4
                 WHERE account_id=?1 AND calendar_id=?2",
                params![account, calendar_id, at, cut],
            )?;
        }
        tx.execute(
            "UPDATE calendar_calendars SET sync_token=?3, synced_at=?4, last_error_kind=NULL,
                    failures=0, retry_at=NULL
             WHERE account_id=?1 AND calendar_id=?2",
            params![account, calendar_id, sync_token, at],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Google said the sync token is gone (410): forget it, so the next read
    /// of this calendar is a full one. The events stay until that full sync
    /// replaces them, so the timeline does not empty while it runs.
    pub fn calendar_clear_token(&self, account: AccountId, calendar_id: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE calendar_calendars SET sync_token=NULL WHERE account_id=?1 AND calendar_id=?2",
            params![account, calendar_id],
        )?;
        Ok(())
    }

    /// Record why reading a calendar failed: its kind, one more failure in a
    /// row, and `retry_at`, when the loop may read it again (None: on the
    /// loop's own schedule). The events stay as they were. A failure before
    /// any calendar is known (the token, the list) is recorded on the primary
    /// calendar, creating its row if need be, so the timeline can say so.
    pub fn calendar_record_error(
        &self,
        account: AccountId,
        calendar_id: &str,
        kind: CalendarErrorKind,
        retry_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let conn = self.lock()?;
        let retry_at = retry_at.map(|at| at.to_rfc3339());
        if calendar_id == PRIMARY {
            conn.execute(
                "INSERT INTO calendar_calendars(account_id,calendar_id,is_primary,selected,last_error_kind,
                                                failures,retry_at)
                 VALUES(?1,?2,1,1,?3,1,?4)
                 ON CONFLICT(account_id,calendar_id) DO UPDATE SET
                    last_error_kind=excluded.last_error_kind,
                    failures=failures+1, retry_at=excluded.retry_at",
                params![account, PRIMARY, kind.as_str(), retry_at],
            )?;
        } else {
            conn.execute(
                "UPDATE calendar_calendars SET last_error_kind=?3, failures=failures+1, retry_at=?4
                 WHERE account_id=?1 AND calendar_id=?2",
                params![account, calendar_id, kind.as_str(), retry_at],
            )?;
        }
        Ok(())
    }

    /// The timeline's `calendar` status ([`CalendarStatus`]). The error is
    /// the PRIMARY row's, which is also where a credential failure is
    /// recorded: one shared calendar that cannot be read is not the account
    /// failing. `synced_at` is the oldest read among the selected calendars
    /// the timeline can trust (the primary, and every secondary not currently
    /// failing), None while any of those has never been read or none exists,
    /// so a broken shared calendar does not make the whole timeline read as
    /// never synced. `truncated` is any selected calendar's cap mark.
    pub fn calendar_status(&self, account: AccountId) -> Result<CalendarStatus> {
        let calendars = self.calendar_calendars(account)?;
        let selected: Vec<_> = calendars.iter().filter(|c| c.selected).collect();
        let trusted: Vec<_> = selected
            .iter()
            .filter(|c| c.is_primary || c.last_error_kind.is_none())
            .collect();
        let synced_at = if trusted.is_empty() {
            None
        } else {
            trusted
                .iter()
                .map(|c| c.synced_at)
                .collect::<Option<Vec<_>>>()
                .and_then(|all| all.into_iter().min())
        };
        let error = selected
            .iter()
            .find(|c| c.calendar_id == PRIMARY)
            .and_then(|c| c.last_error_kind);
        Ok(CalendarStatus {
            synced_at,
            error,
            truncated: selected.iter().any(|c| c.truncated),
        })
    }

    /// Stored events of selected calendars whose start falls in `[from, to)`,
    /// by the ghosts' rule ([`in_range`]: an instant compares as an instant, a
    /// date by date), ordered by start in `from`'s offset, then the primary
    /// calendar first. At most [`LIST_CAP`], and when there are more, the
    /// ones whose start is NEAREST `now`; the `bool` says the listing cut.
    pub fn list_calendar_events(
        &self,
        account: AccountId,
        from: DateTime<FixedOffset>,
        to: DateTime<FixedOffset>,
        now: DateTime<Utc>,
    ) -> Result<(Vec<SyncedEvent>, bool)> {
        self.list_calendar_events_capped(account, from, to, now, LIST_CAP)
    }

    /// [`Self::list_calendar_events`] with the cap given.
    pub fn list_calendar_events_capped(
        &self,
        account: AccountId,
        from: DateTime<FixedOffset>,
        to: DateTime<FixedOffset>,
        now: DateTime<Utc>,
        cap: usize,
    ) -> Result<(Vec<SyncedEvent>, bool)> {
        if to <= from {
            return Ok((Vec::new(), false));
        }
        let conn = self.lock()?;
        // The SQL bound is padded, because a date's sort key is its midnight
        // in UTC while `in_range` reads the date in `from`'s offset; the
        // exact range test runs on each row below, BEFORE the cap counts it.
        let pad = chrono::Duration::days(2);
        let low = sort_key(&EventTime::At(from - pad));
        let high = sort_key(&EventTime::At(to + pad));
        // Read outward from now: forward from it and backward from it, each
        // side stopping once it has more in-range rows than the cap, so the
        // nearest `cap` are among what was read and nothing more is.
        let split = sort_key(&EventTime::At(now.fixed_offset())).clamp(low.clone(), high.clone());
        let select = |cond: &str, order: &str| {
            format!(
                "SELECT {EVENT_COLUMNS}
                 FROM calendar_events e
                 JOIN calendar_calendars c ON c.account_id=e.account_id AND c.calendar_id=e.calendar_id
                 WHERE e.account_id=?1 AND c.selected=1 AND {cond}
                 ORDER BY e.start_sort {order}, c.is_primary DESC, e.calendar_id, e.event_id"
            )
        };
        let mut near: Vec<(bool, SyncedEvent)> = Vec::new();
        for (sql, lo, hi) in [
            (
                select("e.start_sort>=?2 AND e.start_sort<=?3", "ASC"),
                &split,
                &high,
            ),
            (
                select("e.start_sort>=?2 AND e.start_sort<?3", "DESC"),
                &low,
                &split,
            ),
        ] {
            let mut stmt = conn.prepare(&sql)?;
            let mut rows = stmt.query(params![account, lo, hi])?;
            let mut taken = 0;
            while taken <= cap {
                let Some(r) = rows.next()? else { break };
                let Some((primary, event)) =
                    synced_event(r)?.filter(|(_, e)| in_range(&e.row.start, from, to))
                else {
                    continue;
                };
                near.push((primary, event));
                taken += 1;
            }
        }
        let truncated = near.len() > cap;
        if truncated {
            let distance = |e: &SyncedEvent| {
                let at = listing_order(&e.row.start, &chrono::FixedOffset::east_opt(0).unwrap());
                (at - now).abs()
            };
            near.sort_by_key(|(_, e)| distance(e));
            near.truncate(cap);
        }
        // Every guest list in the padded range in one read, then handed to
        // the events that were kept.
        let mut guests = {
            let mut stmt = conn.prepare(
                "SELECT g.calendar_id,g.event_id,g.email,g.name,g.response,g.is_organizer,g.is_self
                 FROM calendar_event_guests g
                 JOIN calendar_events e ON e.account_id=g.account_id
                      AND e.calendar_id=g.calendar_id AND e.event_id=g.event_id
                 WHERE g.account_id=?1 AND e.start_sort>=?2 AND e.start_sort<=?3
                 ORDER BY g.calendar_id, g.event_id, g.position",
            )?;
            let mut by_event: std::collections::HashMap<(String, String), Vec<Guest>> =
                Default::default();
            let mut rows = stmt.query(params![account, low, high])?;
            while let Some(r) = rows.next()? {
                by_event
                    .entry((r.get(0)?, r.get(1)?))
                    .or_default()
                    .push(guest(r, 2)?);
            }
            by_event
        };
        for (_, event) in &mut near {
            if let Some(list) =
                guests.remove(&(event.calendar_id.clone(), event.row.event_id.clone()))
            {
                event.row.guests = list;
            }
        }
        let offset = *from.offset();
        near.sort_by(|(pa, a), (pb, b)| {
            (
                listing_order(&a.row.start, &offset),
                !pa,
                &a.calendar_id,
                &a.row.event_id,
            )
                .cmp(&(
                    listing_order(&b.row.start, &offset),
                    !pb,
                    &b.calendar_id,
                    &b.row.event_id,
                ))
        });
        Ok((near.into_iter().map(|(_, e)| e).collect(), truncated))
    }

    /// Calendar search's events (docs/CALENDAR.md §10.1): every event of a
    /// calendar being read that the word index matches for at least
    /// `min_matched` of `terms`, with how many it matched and its times. A term matches a
    /// word it begins, after porter stemming ("dinners" finds "Dinner").
    /// `addresses` lets a term match the organizer's or a guest's address;
    /// without it that column is never queried.
    pub fn calendar_search_events(
        &self,
        account: AccountId,
        terms: &[String],
        addresses: bool,
        min_matched: usize,
    ) -> Result<Vec<EventMatch>> {
        let columns = if addresses {
            ""
        } else {
            "{title place notes people} : "
        };
        let conn = self.lock()?;
        // One index read per term, counted here. The FTS table drives the
        // join (CROSS JOIN), as `messages_fts` does: the planner must not
        // walk every key and probe MATCH per row.
        let mut counts: std::collections::HashMap<i64, usize> = Default::default();
        {
            let mut stmt = conn.prepare_cached(
                "SELECT k.id FROM calendar_search_fts f
                 CROSS JOIN calendar_search_keys k ON k.id=f.rowid
                 WHERE calendar_search_fts MATCH ?1 AND k.account_id=?2",
            )?;
            for term in terms {
                // Terms are alphanumeric words, so quoting is all they need.
                let expr = format!("{columns}\"{}\"*", term.replace('"', ""));
                for id in stmt.query_map(params![expr, account], |r| r.get::<_, i64>(0))? {
                    *counts.entry(id?).or_default() += 1;
                }
            }
        }
        let wanted: Vec<(i64, usize)> = counts
            .into_iter()
            .filter(|(_, n)| *n >= min_matched.max(1))
            .collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        // What ordering needs, for every match in one read: the full rows
        // are read only for the page served ([`Self::calendar_events_by_key`]).
        let matched: std::collections::HashMap<i64, usize> = wanted.iter().copied().collect();
        let ids = serde_json::to_string(&wanted.iter().map(|(id, _)| id).collect::<Vec<_>>())
            .map_err(|e| CoreError::InvalidInput(e.to_string()))?;
        let mut stmt = conn.prepare_cached(
            "SELECT k.id,e.starts_at,e.ends_at,e.self_response
             FROM json_each(?1) j
             CROSS JOIN calendar_search_keys k ON k.id=j.value
             JOIN calendar_events e ON e.account_id=k.account_id AND e.calendar_id=k.calendar_id
                  AND e.event_id=k.event_id
             JOIN calendar_calendars c ON c.account_id=e.account_id AND c.calendar_id=e.calendar_id
             WHERE k.account_id=?2 AND c.selected=1",
        )?;
        let rows = stmt
            .query_map(params![ids, account], |r| {
                let id: i64 = r.get(0)?;
                let starts_at: String = r.get(1)?;
                let ends_at: Option<String> = r.get(2)?;
                let response: Option<String> = r.get(3)?;
                Ok((id, starts_at, ends_at, response))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, starts_at, ends_at, response)| {
                Some(EventMatch {
                    key: id,
                    matched: matched[&id],
                    start: EventTime::parse(&starts_at)?,
                    end: ends_at.as_deref().and_then(EventTime::parse),
                    declined: response.as_deref().and_then(SelfResponse::parse)
                        == Some(SelfResponse::Declined),
                })
            })
            .collect())
    }

    /// The events behind search keys, each with its key and calendar. A key
    /// whose event is gone, or whose calendar is no longer read, is skipped.
    /// Guests stay empty until [`Self::calendar_fill_guests`].
    pub fn calendar_events_by_key(
        &self,
        account: AccountId,
        keys: &[i64],
    ) -> Result<Vec<(i64, SyncedEvent)>> {
        let conn = self.lock()?;
        let sql = format!(
            "SELECT {EVENT_COLUMNS}
             FROM calendar_search_keys k
             JOIN calendar_events e ON e.account_id=k.account_id AND e.calendar_id=k.calendar_id
                  AND e.event_id=k.event_id
             JOIN calendar_calendars c ON c.account_id=e.account_id AND c.calendar_id=e.calendar_id
             WHERE k.id=?1 AND k.account_id=?2 AND c.selected=1"
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        let mut found = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(Some((_, event))) = stmt
                .query_row(params![key, account], synced_event)
                .optional()?
            {
                found.push((*key, event));
            }
        }
        Ok(found)
    }

    /// Read each event's guests into it, in Google's order.
    pub fn calendar_fill_guests(
        &self,
        account: AccountId,
        events: &mut [&mut SyncedEvent],
    ) -> Result<()> {
        let conn = self.lock()?;
        let mut guests = conn.prepare_cached(
            "SELECT email,name,response,is_organizer,is_self FROM calendar_event_guests
             WHERE account_id=?1 AND calendar_id=?2 AND event_id=?3 ORDER BY position",
        )?;
        for event in events.iter_mut() {
            event.row.guests = guests
                .query_map(
                    params![account, event.calendar_id, event.row.event_id],
                    |r| guest(r, 0),
                )?
                .collect::<std::result::Result<_, _>>()?;
        }
        Ok(())
    }

    /// One stored event of a calendar being read, with its calendar's name,
    /// colour and role. None when there is no such event, or its calendar is
    /// no longer read.
    pub fn calendar_event(
        &self,
        account: AccountId,
        calendar_id: &str,
        event_id: &str,
    ) -> Result<Option<SyncedEvent>> {
        let conn = self.lock()?;
        let sql = format!(
            "SELECT {EVENT_COLUMNS}
             FROM calendar_events e
             JOIN calendar_calendars c ON c.account_id=e.account_id AND c.calendar_id=e.calendar_id
             WHERE e.account_id=?1 AND e.calendar_id=?2 AND e.event_id=?3 AND c.selected=1"
        );
        let found = conn
            .query_row(&sql, params![account, calendar_id, event_id], synced_event)
            .optional()?;
        let Some((_, mut event)) = found.flatten() else {
            return Ok(None);
        };
        let mut stmt = conn.prepare(
            "SELECT email,name,response,is_organizer,is_self FROM calendar_event_guests
             WHERE account_id=?1 AND calendar_id=?2 AND event_id=?3 ORDER BY position",
        )?;
        event.row.guests = stmt
            .query_map(params![account, calendar_id, event_id], |r| guest(r, 0))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(Some(event))
    }

    /// THE USER'S OWN WRITE, after Google accepted it: store Google's answer
    /// (already read by [`crate::gcal::read_written_event`]) under its
    /// calendar. `false`, storing nothing, when the calendar stopped being
    /// read in the meantime. The next sync pass carries the same event and
    /// changes nothing.
    pub fn calendar_put_event(
        &self,
        account: AccountId,
        calendar_id: &str,
        row: &EventRow,
    ) -> Result<bool> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let selected: Option<bool> = tx
            .query_row(
                "SELECT selected FROM calendar_calendars WHERE account_id=?1 AND calendar_id=?2",
                params![account, calendar_id],
                |r| r.get(0),
            )
            .optional()?;
        if selected != Some(true) {
            return Ok(false);
        }
        // The event and its guest list land together or not at all.
        upsert_event(&tx, account, calendar_id, row)?;
        tx.commit()?;
        Ok(true)
    }

    /// The user deleted an event at Google: remove its row. Whether a row was
    /// there.
    pub fn calendar_remove_event(
        &self,
        account: AccountId,
        calendar_id: &str,
        event_id: &str,
    ) -> Result<bool> {
        let conn = self.lock()?;
        Ok(conn.execute(
            "DELETE FROM calendar_events WHERE account_id=?1 AND calendar_id=?2 AND event_id=?3",
            params![account, calendar_id, event_id],
        )? > 0)
    }
}

/// The columns [`synced_event`] reads, over `calendar_events e` joined to its
/// `calendar_calendars c`.
const EVENT_COLUMNS: &str = "e.calendar_id,c.summary,e.event_id,e.ical_uid,e.status,e.summary,
    e.location,e.starts_at,e.ends_at,e.start_timezone,e.organizer_email,e.self_response,
    e.transparent,e.html_link,e.updated_at,c.is_primary,c.color,c.access_role,e.color_id,
    e.organizer_self,e.description,e.guests_truncated,e.organizer_unreadable";

/// One guest read from `email,name,response,is_organizer,is_self` starting
/// at column `at`.
fn guest(r: &rusqlite::Row<'_>, at: usize) -> rusqlite::Result<Guest> {
    let response: String = r.get(at + 2)?;
    Ok(Guest {
        email: r.get(at)?,
        name: r.get(at + 1)?,
        response: GuestResponse::parse(&response),
        organizer: r.get(at + 3)?,
        is_self: r.get(at + 4)?,
    })
}

/// One event read by [`EVENT_COLUMNS`], with whether its calendar is the
/// primary one. None for a start that does not parse (never written so).
/// Its guests are read apart, by the caller.
fn synced_event(r: &rusqlite::Row<'_>) -> rusqlite::Result<Option<(bool, SyncedEvent)>> {
    let starts_at: String = r.get(7)?;
    let Some(start) = EventTime::parse(&starts_at) else {
        return Ok(None);
    };
    let status: String = r.get(4)?;
    let ends_at: Option<String> = r.get(8)?;
    let response: Option<String> = r.get(11)?;
    let role: Option<String> = r.get(17)?;
    let primary: bool = r.get(15)?;
    Ok(Some((
        primary,
        SyncedEvent {
            calendar_id: r.get(0)?,
            calendar_name: r.get(1)?,
            calendar_color: r.get(16)?,
            calendar_access: role.as_deref().and_then(AccessRole::parse),
            calendar_primary: primary,
            row: EventRow {
                event_id: r.get(2)?,
                ical_uid: r.get(3)?,
                tentative: status == "tentative",
                summary: r.get(5)?,
                location: r.get(6)?,
                start,
                end: ends_at.as_deref().and_then(EventTime::parse),
                start_timezone: r.get(9)?,
                organizer_email: r.get(10)?,
                organizer_unreadable: r.get(22)?,
                organizer_self: r.get(19)?,
                self_response: response.as_deref().and_then(SelfResponse::parse),
                transparent: r.get(12)?,
                html_link: r.get(13)?,
                updated_at: r.get(14)?,
                color_id: r.get(18)?,
                description: r.get(20)?,
                guests: Vec::new(),
                guests_truncated: r.get(21)?,
            },
        },
    )))
}

/// Calendar search's index row for one event (schema.sql, CALENDAR SEARCH),
/// replaced whole from what is being stored. Called only by [`upsert_event`],
/// the one way an event is written; deletes are the trigger's.
fn index_event(
    tx: &Connection,
    account: AccountId,
    calendar_id: &str,
    row: &EventRow,
) -> Result<()> {
    tx.execute(
        "INSERT INTO calendar_search_keys(account_id,calendar_id,event_id) VALUES(?1,?2,?3)
         ON CONFLICT(account_id,calendar_id,event_id) DO NOTHING",
        params![account, calendar_id, row.event_id],
    )?;
    let id: i64 = tx.query_row(
        "SELECT id FROM calendar_search_keys WHERE account_id=?1 AND calendar_id=?2 AND event_id=?3",
        params![account, calendar_id, row.event_id],
        |r| r.get(0),
    )?;
    let guests = || row.guests.iter().take(MAX_GUESTS);
    let people: Vec<&str> = guests().filter_map(|g| g.name.as_deref()).collect();
    let addresses: Vec<&str> = row
        .organizer_email
        .as_deref()
        .into_iter()
        .chain(guests().map(|g| g.email.as_str()))
        .collect();
    tx.execute("DELETE FROM calendar_search_fts WHERE rowid=?1", [id])?;
    tx.execute(
        "INSERT INTO calendar_search_fts(rowid,title,place,notes,people,addresses)
         VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            id,
            row.summary,
            row.location,
            row.description,
            people.join(" "),
            addresses.join(" "),
        ],
    )?;
    Ok(())
}

fn upsert_event(
    tx: &Connection,
    account: AccountId,
    calendar_id: &str,
    row: &EventRow,
) -> Result<()> {
    tx.execute(
        "INSERT INTO calendar_events(account_id,calendar_id,event_id,ical_uid,status,summary,location,
             starts_at,start_sort,ends_at,all_day,start_timezone,organizer_email,self_response,
             transparent,html_link,updated_at,color_id,organizer_self,description,guests_truncated,
             organizer_unreadable)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)
         ON CONFLICT(account_id,calendar_id,event_id) DO UPDATE SET
             ical_uid=excluded.ical_uid, status=excluded.status, summary=excluded.summary,
             location=excluded.location, starts_at=excluded.starts_at,
             start_sort=excluded.start_sort, ends_at=excluded.ends_at, all_day=excluded.all_day,
             start_timezone=excluded.start_timezone, organizer_email=excluded.organizer_email,
             self_response=excluded.self_response, transparent=excluded.transparent,
             html_link=excluded.html_link, updated_at=excluded.updated_at,
             color_id=excluded.color_id, organizer_self=excluded.organizer_self,
             description=excluded.description, guests_truncated=excluded.guests_truncated,
             organizer_unreadable=excluded.organizer_unreadable",
        params![
            account,
            calendar_id,
            row.event_id,
            row.ical_uid,
            if row.tentative { "tentative" } else { "confirmed" },
            row.summary,
            row.location,
            row.start.to_string(),
            sort_key(&row.start),
            row.end.map(|e| e.to_string()),
            row.all_day(),
            row.start_timezone,
            row.organizer_email,
            row.self_response.map(SelfResponse::as_str),
            row.transparent,
            row.html_link,
            row.updated_at,
            row.color_id,
            row.organizer_self,
            row.description,
            row.guests_truncated,
            row.organizer_unreadable,
        ],
    )?;
    // The guest list is replaced whole: what Google sent is what is held.
    tx.execute(
        "DELETE FROM calendar_event_guests WHERE account_id=?1 AND calendar_id=?2 AND event_id=?3",
        params![account, calendar_id, row.event_id],
    )?;
    for (position, guest) in row.guests.iter().take(MAX_GUESTS).enumerate() {
        tx.execute(
            "INSERT INTO calendar_event_guests(account_id,calendar_id,event_id,position,email,name,
                 response,is_organizer,is_self)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                account,
                calendar_id,
                row.event_id,
                position as i64,
                guest.email,
                guest.name,
                guest.response.as_str(),
                guest.organizer,
                guest.is_self,
            ],
        )?;
    }
    index_event(tx, account, calendar_id, row)?;
    Ok(())
}
