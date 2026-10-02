//! Google Calendar, the stored half: `calendar_calendars` (which calendars are
//! read, and each one's sync token and last outcome) and `calendar_events`
//! (the events in the synced window). Written only by the sync loop
//! ([`crate::gcal::sync`]), read only by the human door's timeline. HUMAN DOOR
//! ONLY: no agent route and no triage prompt reads either table.
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
    CalendarErrorKind, CalendarMeta, EventItem, EventRow, MAX_EVENTS_PER_CALENDAR, PRIMARY,
    SelfResponse, StoredCalendar, SyncedEvent,
};
use crate::triage::ghost::EventTime;
use chrono::FixedOffset;
use std::collections::HashSet;

/// Events one timeline listing returns at most.
const LIST_CAP: i64 = 5000;

/// Delete every event of a calendar and forget how far it was synced, so the
/// next time it is read it is read in full.
fn forget_calendar(tx: &Connection, account: AccountId, calendar_id: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM calendar_events WHERE account_id=?1 AND calendar_id=?2",
        params![account, calendar_id],
    )?;
    tx.execute(
        "UPDATE calendar_calendars SET sync_token=NULL, synced_at=NULL, full_synced_at=NULL,
                last_error_kind=NULL
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
                    full_synced_at,last_error_kind
             FROM calendar_calendars WHERE account_id=?1
             ORDER BY is_primary DESC, calendar_id",
        )?;
        let rows = stmt
            .query_map([account], |r| {
                let kind: Option<String> = r.get(8)?;
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
                "INSERT INTO calendar_calendars(account_id,calendar_id,summary,is_primary,selected,time_zone)
                 VALUES(?1,?2,?3,?4,1,?5)
                 ON CONFLICT(account_id,calendar_id) DO UPDATE SET
                    summary=COALESCE(excluded.summary,summary),
                    is_primary=excluded.is_primary,
                    selected=1,
                    time_zone=COALESCE(excluded.time_zone,time_zone)",
                params![
                    account,
                    cal.calendar_id,
                    cal.summary,
                    cal.is_primary,
                    cal.time_zone
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

    /// Apply one events page in ONE transaction: upserts and removals. A new
    /// event is stored only while the calendar holds fewer than
    /// [`MAX_EVENTS_PER_CALENDAR`]; one already stored always updates.
    /// Returns the ids now stored from this page (a full sync's "seen" set).
    pub fn calendar_apply_page(
        &self,
        account: AccountId,
        calendar_id: &str,
        items: &[EventItem],
    ) -> Result<Vec<String>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM calendar_events WHERE account_id=?1 AND calendar_id=?2",
            params![account, calendar_id],
            |r| r.get(0),
        )?;
        let mut stored = Vec::new();
        for item in items {
            match item {
                EventItem::Remove(id) => {
                    count -= tx.execute(
                        "DELETE FROM calendar_events WHERE account_id=?1 AND calendar_id=?2 AND event_id=?3",
                        params![account, calendar_id, id],
                    )? as i64;
                }
                EventItem::Upsert(row) => {
                    let exists: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM calendar_events
                         WHERE account_id=?1 AND calendar_id=?2 AND event_id=?3)",
                        params![account, calendar_id, row.event_id],
                        |r| r.get(0),
                    )?;
                    if !exists && count >= MAX_EVENTS_PER_CALENDAR {
                        continue;
                    }
                    upsert_event(&tx, account, calendar_id, row)?;
                    if !exists {
                        count += 1;
                    }
                    stored.push(row.event_id.clone());
                }
            }
        }
        tx.commit()?;
        Ok(stored)
    }

    /// A pass over one calendar finished: record its new sync token and the
    /// time, and clear its error. After a FULL sync, `seen` is every id it
    /// stored, and any other stored event is deleted (it was deleted at
    /// Google, or left the window, while no token was carrying changes).
    pub fn calendar_finish_sync(
        &self,
        account: AccountId,
        calendar_id: &str,
        sync_token: Option<&str>,
        at: DateTime<Utc>,
        seen: Option<&HashSet<String>>,
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let at = at.to_rfc3339();
        if let Some(seen) = seen {
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
                "UPDATE calendar_calendars SET full_synced_at=?3 WHERE account_id=?1 AND calendar_id=?2",
                params![account, calendar_id, at],
            )?;
        }
        tx.execute(
            "UPDATE calendar_calendars SET sync_token=?3, synced_at=?4, last_error_kind=NULL
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

    /// Record why reading a calendar failed. The events stay as they were.
    /// A failure before any calendar is known (the token, the list) is
    /// recorded on the primary calendar, creating its row if need be, so the
    /// timeline can say so.
    pub fn calendar_record_error(
        &self,
        account: AccountId,
        calendar_id: &str,
        kind: CalendarErrorKind,
    ) -> Result<()> {
        let conn = self.lock()?;
        if calendar_id == PRIMARY {
            conn.execute(
                "INSERT INTO calendar_calendars(account_id,calendar_id,is_primary,selected,last_error_kind)
                 VALUES(?1,?2,1,1,?3)
                 ON CONFLICT(account_id,calendar_id) DO UPDATE SET last_error_kind=excluded.last_error_kind",
                params![account, PRIMARY, kind.as_str()],
            )?;
        } else {
            conn.execute(
                "UPDATE calendar_calendars SET last_error_kind=?3 WHERE account_id=?1 AND calendar_id=?2",
                params![account, calendar_id, kind.as_str()],
            )?;
        }
        Ok(())
    }

    /// The timeline's `calendar` status: when every read calendar was last
    /// synced (the OLDEST of them, None while any has never synced or none
    /// exists), and the worst current error among them (auth, then quota,
    /// then unavailable).
    pub fn calendar_status(
        &self,
        account: AccountId,
    ) -> Result<(Option<DateTime<Utc>>, Option<CalendarErrorKind>)> {
        let calendars = self.calendar_calendars(account)?;
        let selected: Vec<_> = calendars.iter().filter(|c| c.selected).collect();
        let synced_at = if selected.is_empty() {
            None
        } else {
            selected
                .iter()
                .map(|c| c.synced_at)
                .collect::<Option<Vec<_>>>()
                .and_then(|all| all.into_iter().min())
        };
        let error = selected.iter().filter_map(|c| c.last_error_kind).max();
        Ok((synced_at, error))
    }

    /// Stored events of selected calendars whose start falls in `[from, to)`,
    /// by the ghosts' rule ([`in_range`]: an instant compares as an instant, a
    /// date by date), ordered by start in `from`'s offset, then the primary
    /// calendar first. At most [`LIST_CAP`].
    pub fn list_calendar_events(
        &self,
        account: AccountId,
        from: DateTime<FixedOffset>,
        to: DateTime<FixedOffset>,
    ) -> Result<Vec<SyncedEvent>> {
        if to <= from {
            return Ok(Vec::new());
        }
        let conn = self.lock()?;
        let pad = chrono::Duration::days(2);
        let low = sort_key(&EventTime::At(from - pad));
        let high = sort_key(&EventTime::At(to + pad));
        let mut stmt = conn.prepare(&format!(
            "SELECT e.calendar_id,c.summary,e.event_id,e.ical_uid,e.status,e.summary,e.location,
                    e.starts_at,e.ends_at,e.start_timezone,e.organizer_email,e.self_response,
                    e.transparent,e.html_link,e.updated_at,c.is_primary
             FROM calendar_events e
             JOIN calendar_calendars c ON c.account_id=e.account_id AND c.calendar_id=e.calendar_id
             WHERE e.account_id=?1 AND c.selected=1 AND e.start_sort>=?2 AND e.start_sort<=?3
             ORDER BY e.start_sort, c.is_primary DESC, e.calendar_id, e.event_id
             LIMIT {LIST_CAP}"
        ))?;
        let rows = stmt
            .query_map(params![account, low, high], |r| {
                let status: String = r.get(4)?;
                let starts_at: String = r.get(7)?;
                let ends_at: Option<String> = r.get(8)?;
                let response: Option<String> = r.get(11)?;
                let primary: bool = r.get(15)?;
                Ok((
                    EventTime::parse(&starts_at),
                    primary,
                    SyncedEvent {
                        calendar_id: r.get(0)?,
                        calendar_name: r.get(1)?,
                        row: EventRow {
                            event_id: r.get(2)?,
                            ical_uid: r.get(3)?,
                            tentative: status == "tentative",
                            summary: r.get(5)?,
                            location: r.get(6)?,
                            // Replaced below; a row whose start does not
                            // parse is dropped there.
                            start: EventTime::Date(chrono::NaiveDate::MIN),
                            end: ends_at.as_deref().and_then(EventTime::parse),
                            start_timezone: r.get(9)?,
                            organizer_email: r.get(10)?,
                            self_response: response.as_deref().and_then(SelfResponse::parse),
                            transparent: r.get(12)?,
                            html_link: r.get(13)?,
                            updated_at: r.get(14)?,
                        },
                    },
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut listed: Vec<(bool, SyncedEvent)> = rows
            .into_iter()
            .filter_map(|(start, primary, mut event)| {
                let start = start.filter(|s| in_range(s, from, to))?;
                event.row.start = start;
                Some((primary, event))
            })
            .collect();
        let offset = *from.offset();
        listed.sort_by(|(pa, a), (pb, b)| {
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
        Ok(listed.into_iter().map(|(_, e)| e).collect())
    }
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
             transparent,html_link,updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
         ON CONFLICT(account_id,calendar_id,event_id) DO UPDATE SET
             ical_uid=excluded.ical_uid, status=excluded.status, summary=excluded.summary,
             location=excluded.location, starts_at=excluded.starts_at,
             start_sort=excluded.start_sort, ends_at=excluded.ends_at, all_day=excluded.all_day,
             start_timezone=excluded.start_timezone, organizer_email=excluded.organizer_email,
             self_response=excluded.self_response, transparent=excluded.transparent,
             html_link=excluded.html_link, updated_at=excluded.updated_at",
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
        ],
    )?;
    Ok(())
}
