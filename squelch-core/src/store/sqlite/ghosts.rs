//! Calendar ghosts, the stored half: the projection of the agent's `event`
//! records into `ghost_events`, and the human door's reads and writes on it.
//!
//! Canonical decisions own these rows, as they own agent-created shipments
//! (`agent_deliveries`). A reconcile rebuilds every ghost the decided message
//! touches from the stored decisions of that ghost's remaining sources, so a
//! retraction walks a ghost back and the last retraction deletes it. The
//! three user_* columns are the exception: only the user writes them, and no
//! rebuild touches them.
//!
//! THE PROJECTION NEVER BLOCKS TRIAGE. [`reconcile_guarded`] runs it under a
//! savepoint inside the triage commit; an error rolls back the ghosts alone,
//! logs ids and an error kind (never a title, location or subject), and the
//! commit proceeds.
//!
//! SENT, SPAM AND RESTRICTED MAIL NEVER FEEDS A GHOST. Reconcile skips it, a
//! rebuild drops it, and [`SqliteStore::list_ghost_events`] serves only ghosts
//! with at least one eligible source, judged in SQL, so a status that changed
//! after the last rebuild cannot leak one.
use super::*;
use crate::triage::decision::{
    EventChange, EventCommitment, EventLikelihood, EventRecordKind, MessageDecision,
};
use crate::triage::ghost::{self, ESTIMATE_MINUTES, EventTime, GhostEstimate, GhostProposal};
use crate::types::{GhostEvent, GhostMailState, GhostSource, GhostUserState};
use chrono::FixedOffset;
use std::collections::{BTreeMap, BTreeSet};

/// A message may feed a ghost: not sent, not spam, and not restricted. The
/// same test as the call site's, in SQL, for messages `m` and their access
/// state `a` (LEFT JOINed: no state row is not a restriction).
const ELIGIBLE: &str = "m.is_sent=0 AND m.is_spam=0 AND COALESCE(a.access,'pending')<>'restricted'";

/// A snake_case decision enum's stored name.
fn label<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        _ => String::new(),
    }
}

/// The enum a stored name names, or its default for a name this build does
/// not know (a newer binary's vocabulary must not fail a listing).
fn unlabel<T: serde::de::DeserializeOwned + Default>(raw: &str) -> T {
    serde_json::from_value(serde_json::Value::String(raw.to_string())).unwrap_or_default()
}

/// `start_sort`: a start as one UTC text shape that compares correctly as
/// text. A date-only start sorts at 00:00Z of its date; the listing widens its
/// SQL range to cover any zone that date might be read in.
fn sort_key(time: &EventTime) -> String {
    match time {
        EventTime::Date(day) => format!("{}T00:00:00Z", day.format("%Y-%m-%d")),
        EventTime::At(at) => at
            .with_timezone(&Utc)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
    }
}

/// What failed, as a kind a log line may carry: no message text. A SQLite
/// failure keeps its result code.
fn error_kind(error: &CoreError) -> String {
    match error {
        CoreError::Sqlite(rusqlite::Error::SqliteFailure(code, _)) => {
            format!("sqlite:{:?}", code.code)
        }
        CoreError::Sqlite(_) => "sqlite".into(),
        CoreError::NotFound => "not_found".into(),
        CoreError::InvalidInput(_) => "invalid_input".into(),
        CoreError::Credential(_) => "credential".into(),
        CoreError::Other(_) => "other".into(),
    }
}

/// [`reconcile`] under its own savepoint, for the triage commit. Infallible on
/// purpose: a ghost is worth less than the triage it rides on.
///
/// `eligible` is false for sent, spam and restricted mail, which then feeds
/// nothing and is detached from whatever it fed.
pub(super) fn reconcile_guarded(
    conn: &Connection,
    account: AccountId,
    message: i64,
    thread_id: &str,
    eligible: bool,
    current: &MessageDecision,
) {
    if let Err(e) = conn.execute_batch("SAVEPOINT ghost_projection") {
        eprintln!(
            "squelch: ghost projection skipped for account {account} message {message} ({})",
            error_kind(&e.into())
        );
        return;
    }
    match reconcile(conn, account, message, thread_id, eligible, current) {
        Ok(()) => {
            if let Err(e) = conn.execute_batch("RELEASE ghost_projection") {
                eprintln!(
                    "squelch: ghost projection release failed for account {account} message {message} ({})",
                    error_kind(&e.into())
                );
            }
        }
        Err(e) => {
            eprintln!(
                "squelch: ghost projection failed for account {account} message {message} ({}); triage committed without it",
                error_kind(&e)
            );
            // If even this fails the outer transaction is already gone, and
            // its commit reports that on its own.
            let _ = conn.execute_batch("ROLLBACK TO ghost_projection; RELEASE ghost_projection");
        }
    }
}

/// Ghost ids holding any of `keys`, lowest first.
fn ghosts_holding(conn: &Connection, account: AccountId, keys: &[String]) -> Result<BTreeSet<i64>> {
    let mut statement = conn
        .prepare_cached("SELECT ghost_id FROM ghost_event_keys WHERE account_id=?1 AND key=?2")?;
    let mut found = BTreeSet::new();
    for key in keys {
        if let Some(id) = statement
            .query_row(params![account, key], |r| r.get::<_, i64>(0))
            .optional()?
        {
            found.insert(id);
        }
    }
    Ok(found)
}

/// Project one message's current decision.
///
/// 1. Affected = ghosts this message fed, plus ghosts holding any key of its
///    current event records.
/// 2. Detach the message, then attach each laundered event record to the ghost
///    holding its keys (a new one when none does; when several do, they merge
///    into the lowest id).
/// 3. Rebuild every affected ghost from its remaining sources.
pub(super) fn reconcile(
    conn: &Connection,
    account: AccountId,
    message: i64,
    thread_id: &str,
    eligible: bool,
    current: &MessageDecision,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let proposals: Vec<(GhostProposal, Vec<String>)> = if eligible {
        current
            .records
            .iter()
            .filter_map(ghost::launder)
            .map(|p| {
                let keys = ghost::identity_keys(&p, thread_id);
                (p, keys)
            })
            .collect()
    } else {
        Vec::new()
    };
    let mut affected: BTreeSet<i64> = {
        let mut statement = conn.prepare(
            "SELECT ghost_id FROM ghost_event_sources WHERE account_id=?1 AND message_id=?2",
        )?;
        statement
            .query_map(params![account, message], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?
    };
    if proposals.is_empty() && affected.is_empty() {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM ghost_event_sources WHERE account_id=?1 AND message_id=?2",
        params![account, message],
    )?;
    for (proposal, keys) in &proposals {
        let holding = ghosts_holding(conn, account, keys)?;
        let target = match holding.first() {
            Some(&survivor) => {
                for &other in holding.iter().skip(1) {
                    merge(conn, account, survivor, other)?;
                    affected.remove(&other);
                }
                survivor
            }
            None => {
                // A placeholder the rebuild below fills in.
                conn.execute(
                    "INSERT INTO ghost_events(account_id,kind,title,commitment,created_at,updated_at)
                     VALUES(?1,?2,?3,?4,?5,?5)",
                    params![
                        account,
                        label(&proposal.kind),
                        proposal.title,
                        label(&proposal.commitment),
                        now
                    ],
                )?;
                conn.last_insert_rowid()
            }
        };
        affected.insert(target);
        for key in keys {
            conn.execute(
                "INSERT OR IGNORE INTO ghost_event_keys(account_id,ghost_id,key) VALUES(?1,?2,?3)",
                params![account, target, key],
            )?;
        }
        conn.execute(
            "INSERT OR IGNORE INTO ghost_event_sources(account_id,ghost_id,message_id,role)
             VALUES(?1,?2,?3,?4)",
            params![account, target, message, label(&proposal.change)],
        )?;
    }
    for id in affected {
        rebuild(conn, account, id, &now)?;
    }
    Ok(())
}

/// Fold ghost `other` into `survivor`: its sources and keys move, and it is
/// deleted. The survivor keeps its own user columns; where it has none it
/// takes the other's (state and its stamp together), so a mark the user made
/// on either half is not lost.
fn merge(conn: &Connection, account: AccountId, survivor: i64, other: i64) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO ghost_event_sources(account_id,ghost_id,message_id,role)
         SELECT account_id,?2,message_id,role FROM ghost_event_sources
         WHERE account_id=?1 AND ghost_id=?3",
        params![account, survivor, other],
    )?;
    conn.execute(
        "UPDATE ghost_event_keys SET ghost_id=?2 WHERE account_id=?1 AND ghost_id=?3",
        params![account, survivor, other],
    )?;
    // Every SET reads the row as it was, so user_state_at sees the old
    // user_state.
    conn.execute(
        "UPDATE ghost_events SET
             user_state_at=CASE WHEN user_state IS NULL
                 THEN (SELECT o.user_state_at FROM ghost_events o WHERE o.account_id=?1 AND o.id=?3)
                 ELSE user_state_at END,
             user_state=COALESCE(user_state,
                 (SELECT o.user_state FROM ghost_events o WHERE o.account_id=?1 AND o.id=?3)),
             user_ends_at=COALESCE(user_ends_at,
                 (SELECT o.user_ends_at FROM ghost_events o WHERE o.account_id=?1 AND o.id=?3))
         WHERE account_id=?1 AND id=?2",
        params![account, survivor, other],
    )?;
    conn.execute(
        "DELETE FROM ghost_events WHERE account_id=?1 AND id=?2",
        params![account, other],
    )?;
    Ok(())
}

/// One retained event proposal feeding a ghost.
struct Fed {
    message: i64,
    received: DateTime<Utc>,
    /// The record's position in its decision, so two records of one message
    /// order stably.
    position: usize,
    proposal: GhostProposal,
    keys: Vec<String>,
}

/// Rebuild one ghost from the stored decisions of its sources, loading only
/// those messages. A ghost no proposal feeds any more is deleted with its keys
/// and sources.
fn rebuild(conn: &Connection, account: AccountId, id: i64, now: &str) -> Result<()> {
    let held: BTreeSet<String> = {
        let mut statement =
            conn.prepare("SELECT key FROM ghost_event_keys WHERE account_id=?1 AND ghost_id=?2")?;
        statement
            .query_map(params![account, id], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?
    };
    type SourceRow = (i64, String, String, bool, Option<String>);
    let rows: Vec<SourceRow> = {
        let mut statement = conn.prepare(&format!(
            "SELECT s.message_id,m.thread_id,m.received_at,{ELIGIBLE},d.decision_json
             FROM ghost_event_sources s
             JOIN messages m ON m.account_id=s.account_id AND m.id=s.message_id
             LEFT JOIN agent_message_state a ON a.account_id=s.account_id AND a.message_id=s.message_id
             LEFT JOIN agent_message_decisions d ON d.account_id=s.account_id AND d.message_id=s.message_id
             WHERE s.account_id=?1 AND s.ghost_id=?2"
        ))?;
        statement
            .query_map(params![account, id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<std::result::Result<_, _>>()?
    };
    let mut feds: Vec<Fed> = Vec::new();
    // A source whose stored decision cannot be read keeps its row: it is not
    // known to have stopped feeding the ghost, and one bad row must not
    // rewrite the rest.
    let mut unreadable: BTreeSet<i64> = BTreeSet::new();
    for (message, thread, received, eligible, encoded) in rows {
        if !eligible {
            continue;
        }
        let Some(encoded) = encoded else { continue };
        let decision: MessageDecision = match serde_json::from_str(&encoded) {
            Ok(decision) => decision,
            Err(_) => {
                eprintln!(
                    "squelch: ghost rebuild skipped an unreadable decision (account {account} message {message})"
                );
                unreadable.insert(message);
                continue;
            }
        };
        let received = DateTime::parse_from_rfc3339(&received)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or(DateTime::<Utc>::MIN_UTC);
        for (position, proposal) in decision
            .records
            .iter()
            .enumerate()
            .filter_map(|(i, r)| ghost::launder(r).map(|p| (i, p)))
        {
            let keys = ghost::identity_keys(&proposal, &thread);
            if keys.iter().any(|k| held.contains(k)) {
                feds.push(Fed {
                    message,
                    received,
                    position,
                    proposal,
                    keys,
                });
            }
        }
    }
    if feds.is_empty() {
        if unreadable.is_empty() {
            conn.execute(
                "DELETE FROM ghost_events WHERE account_id=?1 AND id=?2",
                params![account, id],
            )?;
        }
        return Ok(());
    }
    feds.sort_by(|a, b| {
        b.received
            .cmp(&a.received)
            .then(b.message.cmp(&a.message))
            .then(a.position.cmp(&b.position))
    });
    let newest = &feds[0];
    // The newest proposal that still describes the event: a cancellation mail
    // often repeats nothing, and the card should still say what was cancelled.
    let supplier = feds
        .iter()
        .find(|f| f.proposal.change != EventChange::Cancelled)
        .unwrap_or(newest);
    let mail_state = if newest.proposal.change == EventChange::Cancelled {
        "cancelled"
    } else {
        "live"
    };
    let commitment = if feds.iter().any(|f| {
        f.proposal.commitment == EventCommitment::Committed
            || f.proposal.change == EventChange::Agreed
    }) {
        EventCommitment::Committed
    } else {
        newest.proposal.commitment
    };
    let p = &supplier.proposal;
    conn.execute(
        "UPDATE ghost_events SET kind=?3,title=?4,starts_at=?5,start_sort=?6,start_timezone=?7,
             ends_at=?8,est_minutes=?9,est_basis=?10,location=?11,venue_key=?12,commitment=?13,
             mail_state=?14,likelihood_band=?15,likelihood_reason=?16,last_message_id=?17,
             updated_at=?18
         WHERE account_id=?1 AND id=?2",
        params![
            account,
            id,
            label(&p.kind),
            p.title,
            p.start.map(|s| s.to_string()),
            p.start.as_ref().map(sort_key),
            p.start_timezone,
            p.end.map(|e| e.to_string()),
            p.estimate.as_ref().map(|e| e.minutes),
            p.estimate.as_ref().map(|e| e.basis.as_str()),
            p.location,
            p.location.as_deref().and_then(ghost::venue_key),
            label(&commitment),
            mail_state,
            p.likelihood.as_ref().map(|l| label(&l.band)),
            p.likelihood.as_ref().map(|l| l.reason.as_str()),
            newest.message,
            now,
        ],
    )?;
    conn.execute(
        "DELETE FROM ghost_event_keys WHERE account_id=?1 AND ghost_id=?2",
        params![account, id],
    )?;
    for key in feds.iter().flat_map(|f| f.keys.iter()) {
        conn.execute(
            "INSERT OR IGNORE INTO ghost_event_keys(account_id,ghost_id,key) VALUES(?1,?2,?3)",
            params![account, id, key],
        )?;
    }
    // Each source's role is its change; a message with two records here takes
    // its first.
    let mut roles: BTreeMap<i64, String> = BTreeMap::new();
    for f in &feds {
        roles
            .entry(f.message)
            .or_insert_with(|| label(&f.proposal.change));
    }
    let stale: Vec<i64> = {
        let mut statement = conn.prepare(
            "SELECT message_id FROM ghost_event_sources WHERE account_id=?1 AND ghost_id=?2",
        )?;
        statement
            .query_map(params![account, id], |r| r.get(0))?
            .collect::<std::result::Result<Vec<i64>, _>>()?
            .into_iter()
            .filter(|m| !roles.contains_key(m) && !unreadable.contains(m))
            .collect()
    };
    for message in stale {
        conn.execute(
            "DELETE FROM ghost_event_sources WHERE account_id=?1 AND ghost_id=?2 AND message_id=?3",
            params![account, id, message],
        )?;
    }
    for (message, role) in roles {
        conn.execute(
            "UPDATE ghost_event_sources SET role=?4 WHERE account_id=?1 AND ghost_id=?2 AND message_id=?3",
            params![account, id, message, role],
        )?;
    }
    Ok(())
}

/// Does a start fall in `[from, to)`? An instant compares as an instant. A
/// date compares as a date in the caller's offset: it is in range when that
/// day overlaps the range at all.
fn in_range(start: &EventTime, from: DateTime<FixedOffset>, to: DateTime<FixedOffset>) -> bool {
    match start {
        EventTime::At(at) => *at >= from && *at < to,
        EventTime::Date(day) => {
            let last = to.with_timezone(from.offset());
            let first_day = from.date_naive();
            let end_day = last.date_naive();
            // `to` exactly at midnight excludes that day.
            let ends_inside = last.time() > chrono::NaiveTime::MIN;
            *day >= first_day && (*day < end_day || (ends_inside && *day == end_day))
        }
    }
}

impl SqliteStore {
    pub(super) fn list_ghost_events(
        &self,
        account: AccountId,
        from: DateTime<FixedOffset>,
        to: DateTime<FixedOffset>,
    ) -> Result<Vec<GhostEvent>> {
        if to <= from {
            return Ok(Vec::new());
        }
        let conn = self.lock()?;
        // Coarse in SQL, exact in Rust: two days either side covers a date
        // read in any zone (offsets run -12h to +14h).
        let pad = chrono::Duration::days(2);
        let low = sort_key(&EventTime::At(from - pad));
        let high = sort_key(&EventTime::At(to + pad));
        let mut statement = conn.prepare(&format!(
            "SELECT g.id,g.kind,g.title,g.starts_at,g.start_timezone,g.ends_at,g.est_minutes,
                    g.est_basis,g.location,g.commitment,g.mail_state,g.likelihood_band,
                    g.likelihood_reason,g.user_state,g.user_state_at,g.user_ends_at,p.minutes,
                    g.last_message_id,g.created_at,g.updated_at
             FROM ghost_events g
             LEFT JOIN event_duration_priors p
               ON p.account_id=g.account_id AND p.venue_key=g.venue_key AND p.kind=g.kind
             WHERE g.account_id=?1 AND g.start_sort IS NOT NULL
               AND g.start_sort>=?2 AND g.start_sort<=?3
               AND EXISTS(SELECT 1 FROM ghost_event_sources s
                   JOIN messages m ON m.account_id=s.account_id AND m.id=s.message_id
                   LEFT JOIN agent_message_state a ON a.account_id=s.account_id AND a.message_id=s.message_id
                   WHERE s.account_id=g.account_id AND s.ghost_id=g.id AND {ELIGIBLE})
             ORDER BY g.start_sort,g.id"
        ))?;
        let rows = statement
            .query_map(params![account, low, high], |r| {
                let starts_at: String = r.get(3)?;
                let kind: String = r.get(1)?;
                let commitment: String = r.get(9)?;
                let mail_state: String = r.get(10)?;
                let band: Option<String> = r.get(11)?;
                let reason: Option<String> = r.get(12)?;
                let user_state: Option<String> = r.get(13)?;
                let ends_at: Option<String> = r.get(5)?;
                let user_ends_at: Option<String> = r.get(15)?;
                let est_minutes: Option<u32> = r.get(6)?;
                let est_basis: Option<String> = r.get(7)?;
                Ok((
                    EventTime::parse(&starts_at),
                    GhostEvent {
                        id: r.get(0)?,
                        account_id: account,
                        kind: unlabel::<EventRecordKind>(&kind),
                        title: r.get(2)?,
                        // Replaced below; a row whose start does not parse
                        // is dropped there.
                        start: EventTime::Date(chrono::NaiveDate::MIN),
                        start_timezone: r.get(4)?,
                        stated_end: ends_at.as_deref().and_then(EventTime::parse),
                        estimate: est_minutes.map(|minutes| GhostEstimate {
                            minutes,
                            basis: est_basis.unwrap_or_default(),
                        }),
                        location: r.get(8)?,
                        commitment: unlabel::<EventCommitment>(&commitment),
                        mail_state: if mail_state == "cancelled" {
                            GhostMailState::Cancelled
                        } else {
                            GhostMailState::Live
                        },
                        likelihood: band.map(|band| EventLikelihood {
                            band: unlabel(&band),
                            reason: reason.unwrap_or_default(),
                        }),
                        user_state: user_state.as_deref().and_then(GhostUserState::parse),
                        user_state_at: dt_opt(r, 14)?,
                        user_end: user_ends_at.as_deref().and_then(EventTime::parse),
                        history_minutes: r.get(16)?,
                        last_message_id: r.get(17)?,
                        created_at: dt(r, 18)?,
                        updated_at: dt(r, 19)?,
                        sources: Vec::new(),
                    },
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut sources = conn.prepare(&format!(
            "SELECT s.message_id,m.thread_id,s.role FROM ghost_event_sources s
             JOIN messages m ON m.account_id=s.account_id AND m.id=s.message_id
             LEFT JOIN agent_message_state a ON a.account_id=s.account_id AND a.message_id=s.message_id
             WHERE s.account_id=?1 AND s.ghost_id=?2 AND {ELIGIBLE}
             ORDER BY m.received_at,s.message_id"
        ))?;
        let mut listed = Vec::new();
        for (start, mut ghost) in rows {
            let Some(start) = start.filter(|s| in_range(s, from, to)) else {
                continue;
            };
            ghost.start = start;
            // Only eligible sources are served: a restricted message's id is
            // not the human door's to point at from here.
            ghost.sources = sources
                .query_map(params![account, ghost.id], |r| {
                    let role: String = r.get(2)?;
                    Ok(GhostSource {
                        message_id: r.get(0)?,
                        thread_id: r.get(1)?,
                        role: unlabel(&role),
                    })
                })?
                .collect::<std::result::Result<_, _>>()?;
            listed.push(ghost);
        }
        Ok(listed)
    }

    pub(super) fn set_ghost_user_state(
        &self,
        account: AccountId,
        id: i64,
        state: Option<GhostUserState>,
        at: DateTime<Utc>,
    ) -> Result<bool> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE ghost_events SET user_state=?3,user_state_at=?4 WHERE account_id=?1 AND id=?2",
            params![
                account,
                id,
                state.map(GhostUserState::as_str),
                state.map(|_| at.to_rfc3339())
            ],
        )?;
        Ok(changed > 0)
    }

    pub(super) fn set_ghost_user_end(
        &self,
        account: AccountId,
        id: i64,
        end: Option<EventTime>,
        at: DateTime<Utc>,
    ) -> Result<bool> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some((starts_at, location, kind)) = tx
            .query_row(
                "SELECT starts_at,location,kind FROM ghost_events WHERE account_id=?1 AND id=?2",
                params![account, id],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(false);
        };
        if let Some(end) = &end {
            let start = starts_at
                .as_deref()
                .and_then(EventTime::parse)
                .ok_or_else(|| CoreError::InvalidInput("ghost has no start".into()))?;
            // Mixed precision is never "after": a timed end on a date-only
            // ghost is refused rather than stored and ignored.
            if !end.is_after(&start) {
                return Err(CoreError::InvalidInput(
                    "end must be after the start, at the same precision".into(),
                ));
            }
            if let (EventTime::At(s), EventTime::At(e), Some(venue)) =
                (start, end, location.as_deref().and_then(ghost::venue_key))
            {
                let minutes = u32::try_from((*e - s).num_minutes())
                    .unwrap_or(u32::MAX)
                    .clamp(*ESTIMATE_MINUTES.start(), *ESTIMATE_MINUTES.end());
                tx.execute(
                    "INSERT INTO event_duration_priors(account_id,venue_key,kind,minutes,updated_at)
                     VALUES(?1,?2,?3,?4,?5) ON CONFLICT(account_id,venue_key,kind) DO UPDATE SET
                     minutes=excluded.minutes,updated_at=excluded.updated_at",
                    params![account, venue, kind, minutes, at.to_rfc3339()],
                )?;
            }
        }
        tx.execute(
            "UPDATE ghost_events SET user_ends_at=?3 WHERE account_id=?1 AND id=?2",
            params![account, id, end.map(|e| e.to_string())],
        )?;
        tx.commit()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triage::decision::{
        DurationEstimate, LikelihoodBand, RecordProposal, SupportedTime,
    };
    use crate::types::GhostState;

    /// Accounts 1 and 2, no mail: each test adds the messages it needs.
    fn fixture() -> SqliteStore {
        let store = SqliteStore::open_in_memory().unwrap();
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO accounts(id,email,created_at) VALUES(1,'a@test',?1),(2,'b@test',?1)",
                [Utc::now().to_rfc3339()],
            )
            .unwrap();
        store
    }

    /// A message `minutes_ago` old, in `thread`.
    fn mail(store: &SqliteStore, account: AccountId, id: i64, thread: &str, minutes_ago: i64) {
        let conn = store.lock().unwrap();
        conn.execute(
            "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body)
             VALUES(?1,?2,?3,?4,'tickets@test','Your tickets',?5,'','')",
            params![
                id,
                account,
                id.to_string(),
                thread,
                (Utc::now() - chrono::Duration::minutes(minutes_ago)).to_rfc3339()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO triage(account_id,message_id,created_at) VALUES(?1,?2,?3)",
            params![account, id, Utc::now().to_rfc3339()],
        )
        .unwrap();
    }

    fn time(value: &str) -> Option<SupportedTime> {
        Some(SupportedTime {
            value: value.into(),
            timezone: Some("America/New_York".into()),
            source_message_id: 0,
            interpreted_relative: false,
        })
    }

    struct Ev<'a> {
        title: &'a str,
        start: Option<&'a str>,
        refs: &'a [&'a str],
        change: EventChange,
        commitment: EventCommitment,
        location: Option<&'a str>,
        kind: EventRecordKind,
    }
    impl Default for Ev<'_> {
        fn default() -> Self {
            Ev {
                title: "Show",
                start: Some("2026-10-03T20:00:00-04:00"),
                refs: &["ABC123"],
                change: EventChange::Created,
                commitment: EventCommitment::Committed,
                location: Some("Brooklyn Steel, 319 Frost St"),
                kind: EventRecordKind::Ticket,
            }
        }
    }
    impl Ev<'_> {
        fn record(&self) -> RecordProposal {
            RecordProposal::Event {
                title: self.title.into(),
                start: self.start.and_then(time),
                location: self.location.map(str::to_string),
                end: None,
                duration_estimate: Some(DurationEstimate {
                    minutes: 150,
                    basis: "doors 7, show 8".into(),
                }),
                event_kind: self.kind,
                commitment: self.commitment,
                change: self.change,
                identity_refs: self.refs.iter().map(|r| r.to_string()).collect(),
                likelihood: Some(EventLikelihood {
                    band: LikelihoodBand::Likely,
                    reason: "a friend asked".into(),
                }),
                evidence: vec![],
            }
        }
        fn decision(&self) -> MessageDecision {
            decided(vec![self.record()])
        }
    }
    fn decided(records: Vec<RecordProposal>) -> MessageDecision {
        MessageDecision {
            records,
            ..Default::default()
        }
    }

    /// Store a decision the way the triage commit does (decision row, then
    /// access, then the projection, in one transaction), with the call site's
    /// eligibility test. Unguarded, so a projection error fails the test.
    fn decide(store: &SqliteStore, account: AccountId, id: i64, decision: &MessageDecision) {
        let mut conn = store.lock().unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO agent_message_decisions(account_id,message_id,revision,decision_json,decided_at)
             VALUES(?1,?2,1,?3,?4) ON CONFLICT(account_id,message_id) DO UPDATE SET decision_json=excluded.decision_json",
            params![account, id, serde_json::to_string(decision).unwrap(), Utc::now().to_rfc3339()],
        )
        .unwrap();
        let access = if decision.external_access.restricted {
            "restricted"
        } else {
            "allowed"
        };
        tx.execute(
            "INSERT INTO agent_message_state(account_id,message_id,content_snapshot,access)
             VALUES(?1,?2,'',?3) ON CONFLICT(account_id,message_id) DO UPDATE SET access=excluded.access",
            params![account, id, access],
        )
        .unwrap();
        let (thread, sent, spam): (String, bool, bool) = tx
            .query_row(
                "SELECT thread_id,is_sent,is_spam FROM messages WHERE account_id=?1 AND id=?2",
                params![account, id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        let eligible = !sent && !spam && !decision.external_access.restricted;
        reconcile(&tx, account, id, &thread, eligible, decision).unwrap();
        tx.commit().unwrap();
    }

    fn wide() -> (DateTime<FixedOffset>, DateTime<FixedOffset>) {
        let at = |s| DateTime::parse_from_rfc3339(s).unwrap();
        (at("2000-01-01T00:00:00Z"), at("2100-01-01T00:00:00Z"))
    }
    fn listed(store: &SqliteStore, account: AccountId) -> Vec<GhostEvent> {
        let (from, to) = wide();
        store.list_ghost_events(account, from, to).unwrap()
    }
    fn count(store: &SqliteStore, table: &str) -> i64 {
        store
            .lock()
            .unwrap()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    fn keys(store: &SqliteStore, ghost: i64) -> Vec<String> {
        let conn = store.lock().unwrap();
        let mut statement = conn
            .prepare("SELECT key FROM ghost_event_keys WHERE ghost_id=?1 ORDER BY key")
            .unwrap();
        statement
            .query_map([ghost], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }
    fn roles(ghost: &GhostEvent) -> Vec<(i64, EventChange)> {
        ghost
            .sources
            .iter()
            .map(|s| (s.message_id, s.role))
            .collect()
    }
    fn at(raw: &str) -> EventTime {
        EventTime::parse(raw).unwrap()
    }

    #[test]
    fn one_event_record_makes_one_ghost_with_its_keys_and_source() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        decide(&store, 1, 1, &Ev::default().decision());
        let ghosts = listed(&store, 1);
        assert_eq!(ghosts.len(), 1);
        let g = &ghosts[0];
        assert_eq!(g.title, "Show");
        assert_eq!(g.kind, EventRecordKind::Ticket);
        assert_eq!(g.start, at("2026-10-03T20:00:00-04:00"));
        assert_eq!(g.start_timezone.as_deref(), Some("America/New_York"));
        assert_eq!(g.stated_end, None, "no end was stated");
        assert_eq!(
            g.estimate,
            Some(GhostEstimate {
                minutes: 150,
                basis: "doors 7, show 8".into()
            }),
            "the guess is kept apart from the end"
        );
        assert_eq!(g.commitment, EventCommitment::Committed);
        assert_eq!(g.mail_state, GhostMailState::Live);
        assert_eq!(g.last_message_id, Some(1));
        assert_eq!(roles(g), vec![(1, EventChange::Created)]);
        assert_eq!(g.sources[0].thread_id, "t1");
        assert_eq!(keys(&store, g.id), vec!["ref:abc123"]);
        // A decision without an event makes nothing.
        mail(&store, 1, 2, "t2", 30);
        decide(&store, 1, 2, &MessageDecision::default());
        assert_eq!(count(&store, "ghost_events"), 1);
    }

    /// The modification mail is a different thread, and the shared booking
    /// number is what makes it the same event.
    #[test]
    fn a_shared_ref_in_another_thread_moves_the_same_ghost() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        mail(&store, 1, 2, "t2", 30);
        decide(&store, 1, 1, &Ev::default().decision());
        decide(
            &store,
            1,
            2,
            &Ev {
                refs: &["Confirmation #ABC-123"],
                change: EventChange::Updated,
                start: Some("2026-10-04T21:00:00-04:00"),
                ..Default::default()
            }
            .decision(),
        );
        let ghosts = listed(&store, 1);
        assert_eq!(ghosts.len(), 1);
        assert_eq!(ghosts[0].start, at("2026-10-04T21:00:00-04:00"));
        assert_eq!(
            roles(&ghosts[0]),
            vec![(1, EventChange::Created), (2, EventChange::Updated)]
        );
        assert_eq!(ghosts[0].last_message_id, Some(2));
    }

    #[test]
    fn the_newest_proposal_decides_cancelled_and_newer_mail_uncancels() {
        let store = fixture();
        for (id, ago) in [(1, 90), (2, 60), (3, 30)] {
            mail(&store, 1, id, &format!("t{id}"), ago);
        }
        decide(&store, 1, 1, &Ev::default().decision());
        decide(
            &store,
            1,
            2,
            &Ev {
                change: EventChange::Cancelled,
                title: "Cancelled: Show",
                start: None,
                ..Default::default()
            }
            .decision(),
        );
        let g = listed(&store, 1).remove(0);
        assert_eq!(g.mail_state, GhostMailState::Cancelled);
        assert_eq!(
            (g.title.as_str(), g.start),
            ("Show", at("2026-10-03T20:00:00-04:00")),
            "the event's facts come from the newest proposal that still describes it"
        );
        assert_eq!(g.state(Utc::now().fixed_offset()), GhostState::Cancelled);
        decide(
            &store,
            1,
            3,
            &Ev {
                change: EventChange::Updated,
                ..Default::default()
            }
            .decision(),
        );
        assert_eq!(listed(&store, 1)[0].mail_state, GhostMailState::Live);
    }

    #[test]
    fn retraction_deletes_a_lone_ghost_and_rebuilds_a_shared_one() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        mail(&store, 1, 2, "t2", 30);
        decide(&store, 1, 1, &Ev::default().decision());
        decide(&store, 1, 1, &MessageDecision::default());
        assert_eq!(count(&store, "ghost_events"), 0);
        assert_eq!(count(&store, "ghost_event_keys"), 0);
        assert_eq!(count(&store, "ghost_event_sources"), 0);

        decide(&store, 1, 1, &Ev::default().decision());
        decide(
            &store,
            1,
            2,
            &Ev {
                change: EventChange::Updated,
                start: Some("2026-10-04T21:00:00-04:00"),
                refs: &["ABC123", "QXJZKM"],
                ..Default::default()
            }
            .decision(),
        );
        let id = listed(&store, 1)[0].id;
        assert_eq!(keys(&store, id), vec!["ref:abc123", "ref:qxjzkm"]);
        decide(&store, 1, 2, &MessageDecision::default());
        let ghosts = listed(&store, 1);
        assert_eq!(ghosts.len(), 1);
        assert_eq!(ghosts[0].id, id, "rebuilt in place");
        assert_eq!(ghosts[0].start, at("2026-10-03T20:00:00-04:00"));
        assert_eq!(roles(&ghosts[0]), vec![(1, EventChange::Created)]);
        assert_eq!(
            keys(&store, id),
            vec!["ref:abc123"],
            "the retracted mail's ref goes with it"
        );
    }

    #[test]
    fn a_message_bridging_two_ghosts_merges_them_into_the_lowest_id() {
        let store = fixture();
        for (id, ago) in [(1, 90), (2, 60), (3, 30)] {
            mail(&store, 1, id, &format!("t{id}"), ago);
        }
        decide(&store, 1, 1, &Ev::default().decision());
        decide(
            &store,
            1,
            2,
            &Ev {
                refs: &["QXJZKM"],
                ..Default::default()
            }
            .decision(),
        );
        let ids: Vec<i64> = listed(&store, 1).iter().map(|g| g.id).collect();
        assert_eq!(ids.len(), 2);
        let (low, high) = (ids[0].min(ids[1]), ids[0].max(ids[1]));
        let going = DateTime::parse_from_rfc3339("2026-09-30T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // The survivor has an end but no state; the other has a state.
        assert!(
            store
                .set_ghost_user_end(1, low, Some(at("2026-10-03T23:30:00-04:00")), going)
                .unwrap()
        );
        assert!(
            store
                .set_ghost_user_state(1, high, Some(GhostUserState::Going), going)
                .unwrap()
        );
        decide(
            &store,
            1,
            3,
            &Ev {
                refs: &["ABC123", "QXJZKM"],
                change: EventChange::Updated,
                ..Default::default()
            }
            .decision(),
        );
        let ghosts = listed(&store, 1);
        assert_eq!(ghosts.len(), 1);
        let g = &ghosts[0];
        assert_eq!(g.id, low);
        assert_eq!(
            roles(g),
            vec![
                (1, EventChange::Created),
                (2, EventChange::Created),
                (3, EventChange::Updated)
            ]
        );
        assert_eq!(keys(&store, low), vec!["ref:abc123", "ref:qxjzkm"]);
        assert_eq!(g.user_end, Some(at("2026-10-03T23:30:00-04:00")));
        assert_eq!(g.user_state, Some(GhostUserState::Going));
        assert_eq!(g.user_state_at, Some(going));
        assert_eq!(count(&store, "ghost_events"), 1);
    }

    #[test]
    fn user_columns_survive_a_rebuild() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        mail(&store, 1, 2, "t2", 30);
        decide(&store, 1, 1, &Ev::default().decision());
        let id = listed(&store, 1)[0].id;
        let now = Utc::now();
        store
            .set_ghost_user_state(1, id, Some(GhostUserState::Dismissed), now)
            .unwrap();
        store
            .set_ghost_user_end(1, id, Some(at("2026-10-03T23:00:00-04:00")), now)
            .unwrap();
        decide(
            &store,
            1,
            2,
            &Ev {
                change: EventChange::Updated,
                title: "Show (moved)",
                ..Default::default()
            }
            .decision(),
        );
        // And a re-decision of the first message rebuilds again.
        decide(&store, 1, 1, &Ev::default().decision());
        let g = listed(&store, 1).remove(0);
        assert_eq!(g.title, "Show (moved)");
        assert_eq!(g.user_state, Some(GhostUserState::Dismissed));
        assert!(g.user_state_at.is_some());
        assert_eq!(g.user_end, Some(at("2026-10-03T23:00:00-04:00")));
        assert_eq!(g.state(now.fixed_offset()), GhostState::Dismissed);
        // Clearing the state clears its stamp.
        store.set_ghost_user_state(1, id, None, now).unwrap();
        let g = listed(&store, 1).remove(0);
        assert_eq!((g.user_state, g.user_state_at), (None, None));
    }

    /// EVERY SOURCE'S STORED DECISION IS RE-READ. One written before the
    /// calendar fields existed must still deserialize and feed its ghost.
    #[test]
    fn a_stored_decision_without_the_new_fields_still_reconciles() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        mail(&store, 1, 2, "t1", 30);
        let old = r#"{"kinds":["event_reservation"],"destinations":[],"summary":"s","reason":"r",
            "auth":{"kinds":[],"evidence":[]},
            "external_access":{"restricted":false,"reason":"r","evidence":[]},
            "attention":{"show_in_fye":false,"state":"informational","actions":[],"summary":"",
                "factors":{"urgency":0,"action_need":0,"personal_relevance":0,"importance":0,"attention_at":null},
                "relevant_message_ids":[],"evidence":[]},
            "records":[{"kind":"event","title":"Dinner at Lilia",
                "start":{"value":"2026-10-03T19:30:00-04:00","timezone":null,"source_message_id":1,"interpreted_relative":false},
                "location":"Lilia","evidence":[]}],
            "related_updates":[],"rule_exceptions":[],
            "notification":{"importance":0,"title":"","body":"","reason":""},"revisit":null}"#;
        let decision: MessageDecision = serde_json::from_str(old).unwrap();
        {
            let mut conn = store.lock().unwrap();
            let tx = conn.transaction().unwrap();
            tx.execute(
                "INSERT INTO agent_message_decisions(account_id,message_id,revision,decision_json,decided_at)
                 VALUES(1,1,1,?1,?2)",
                params![old, Utc::now().to_rfc3339()],
            )
            .unwrap();
            reconcile(&tx, 1, 1, "t1", true, &decision).unwrap();
            tx.commit().unwrap();
        }
        let g = listed(&store, 1).remove(0);
        assert_eq!(g.kind, EventRecordKind::Other);
        assert_eq!(g.commitment, EventCommitment::Unknown);
        assert_eq!(keys(&store, g.id), vec!["thread:t1:dinner-at-lilia"]);
        // A NEW decision in the same thread re-reads the old one too.
        decide(
            &store,
            1,
            2,
            &Ev {
                title: "Dinner at Lilia",
                refs: &[],
                start: Some("2026-10-03T20:00:00-04:00"),
                change: EventChange::Updated,
                kind: EventRecordKind::Reservation,
                ..Default::default()
            }
            .decision(),
        );
        let ghosts = listed(&store, 1);
        assert_eq!(ghosts.len(), 1);
        assert_eq!(
            roles(&ghosts[0]),
            vec![(1, EventChange::Created), (2, EventChange::Updated)]
        );
        assert_eq!(ghosts[0].start, at("2026-10-03T20:00:00-04:00"));
    }

    /// A message whose status changes after its ghost exists is caught at
    /// LIST time, in SQL, without any rebuild.
    #[test]
    fn a_ghost_whose_only_source_became_spam_or_restricted_is_not_listed() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        decide(&store, 1, 1, &Ev::default().decision());
        assert_eq!(listed(&store, 1).len(), 1);
        let set = |sql: &str| store.lock().unwrap().execute(sql, []).unwrap();
        set("UPDATE messages SET is_spam=1 WHERE id=1");
        assert!(listed(&store, 1).is_empty(), "spam");
        set("UPDATE messages SET is_spam=0 WHERE id=1");
        set("UPDATE agent_message_state SET access='restricted' WHERE message_id=1");
        assert!(listed(&store, 1).is_empty(), "restricted");
        set("UPDATE agent_message_state SET access='allowed' WHERE message_id=1");
        set("UPDATE messages SET is_sent=1 WHERE id=1");
        assert!(listed(&store, 1).is_empty(), "sent");
        set("UPDATE messages SET is_sent=0 WHERE id=1");
        assert_eq!(listed(&store, 1).len(), 1);

        // With a second, eligible source the ghost lists, and the ineligible
        // source is not served.
        mail(&store, 1, 2, "t2", 30);
        decide(
            &store,
            1,
            2,
            &Ev {
                change: EventChange::Updated,
                ..Default::default()
            }
            .decision(),
        );
        set("UPDATE agent_message_state SET access='restricted' WHERE message_id=1");
        let g = listed(&store, 1).remove(0);
        assert_eq!(roles(&g), vec![(2, EventChange::Updated)]);
    }

    /// Enqueue, claim and commit through the real triage commit.
    fn commit(store: &SqliteStore, id: i64, trigger: &str, decision: &MessageDecision) {
        use crate::store::agent_triage::{AgentCommitOutcome, AgentTriageStore};
        store.enqueue_agent_triage(1, id, trigger, false).unwrap();
        let job = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert_eq!(job.message_id, id);
        let context = store.load_agent_context(&job).unwrap();
        let outcome = store
            .commit_agent_decision(
                &job,
                &context,
                decision,
                std::slice::from_ref(&context.message.source),
            )
            .unwrap();
        assert_eq!(outcome, AgentCommitOutcome::Applied);
    }

    #[test]
    fn sent_spam_and_restricted_mail_never_make_a_ghost() {
        let store = fixture();
        for id in 1..=4 {
            mail(&store, 1, id, &format!("t{id}"), 60 - id);
        }
        {
            let conn = store.lock().unwrap();
            conn.execute("UPDATE messages SET is_sent=1 WHERE id=1", [])
                .unwrap();
            conn.execute("UPDATE messages SET is_spam=1 WHERE id=2", [])
                .unwrap();
        }
        let ticket = Ev::default().decision();
        commit(&store, 1, "arrival", &ticket);
        commit(&store, 2, "arrival", &ticket);
        let mut restricted = ticket.clone();
        restricted.external_access.restricted = true;
        commit(&store, 3, "arrival", &restricted);
        assert_eq!(count(&store, "ghost_events"), 0);
        // The ordinary path through the same commit does make one.
        commit(&store, 4, "arrival", &ticket);
        assert_eq!(listed(&store, 1).len(), 1);
        // And a re-decision that restricts its only source removes it.
        commit(&store, 4, "record_retraction", &restricted);
        assert_eq!(count(&store, "ghost_events"), 0);
    }

    /// INVARIANT 5. A projection that fails partway (after its ghost row and
    /// keys are written, at the source insert) rolls back to its savepoint,
    /// and the triage commit lands anyway.
    #[test]
    fn a_failing_projection_does_not_fail_the_triage_commit() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        store
            .lock()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER ghost_sources_fail BEFORE INSERT ON ghost_event_sources
                 BEGIN SELECT RAISE(ABORT, 'forced'); END;",
            )
            .unwrap();
        commit(&store, 1, "arrival", &Ev::default().decision());
        let conn = store.lock().unwrap();
        let decided: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_message_decisions WHERE message_id=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(decided, 1, "the decision committed");
        let finished: String = conn
            .query_row(
                "SELECT state FROM agent_triage_jobs WHERE message_id=1 AND kind='triage'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(finished, "completed");
        for table in ["ghost_events", "ghost_event_keys", "ghost_event_sources"] {
            let n: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{table}: the partial projection rolled back");
        }
        // The control: the same commit with nothing failing does project, so
        // the zero above is the rollback and not a projection that never ran.
        conn.execute_batch("DROP TRIGGER ghost_sources_fail;")
            .unwrap();
        drop(conn);
        commit(&store, 1, "record_retraction", &Ev::default().decision());
        assert_eq!(listed(&store, 1).len(), 1);
        // A missing table fails earlier, at the first read; same outcome, and
        // the ghost already there is untouched.
        store
            .lock()
            .unwrap()
            .execute_batch("DROP TABLE ghost_event_keys;")
            .unwrap();
        mail(&store, 1, 2, "t2", 30);
        commit(&store, 2, "arrival", &Ev::default().decision());
        assert_eq!(count(&store, "ghost_events"), 1);
        assert_eq!(count(&store, "ghost_event_sources"), 1);
        let decided: i64 = store
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM agent_message_decisions WHERE message_id=2",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(decided, 1);
    }

    #[test]
    fn accounts_never_see_or_touch_each_others_ghosts() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        mail(&store, 2, 2, "t1", 30);
        decide(&store, 1, 1, &Ev::default().decision());
        // Same ref, same thread id, other account: its own ghost.
        decide(
            &store,
            2,
            2,
            &Ev {
                change: EventChange::Cancelled,
                ..Default::default()
            }
            .decision(),
        );
        let a = listed(&store, 1);
        let b = listed(&store, 2);
        assert_eq!((a.len(), b.len()), (1, 1));
        assert_ne!(a[0].id, b[0].id);
        assert_eq!(a[0].mail_state, GhostMailState::Live, "B's cancel is B's");
        assert_eq!(roles(&a[0]), vec![(1, EventChange::Created)]);
        let now = Utc::now();
        assert!(
            !store
                .set_ghost_user_state(2, a[0].id, Some(GhostUserState::Dismissed), now)
                .unwrap()
        );
        assert!(
            !store
                .set_ghost_user_end(2, a[0].id, Some(at("2026-10-03T23:00:00-04:00")), now)
                .unwrap()
        );
        let a = listed(&store, 1).remove(0);
        assert_eq!((a.user_state, a.user_end), (None, None));
        assert_eq!(
            count(&store, "event_duration_priors"),
            0,
            "a refused end writes no prior"
        );
        // Retracting B's mail leaves A alone.
        decide(&store, 2, 2, &MessageDecision::default());
        assert_eq!(listed(&store, 1).len(), 1);
        assert!(listed(&store, 2).is_empty());
    }

    #[test]
    fn one_committed_or_agreed_source_commits_the_ghost() {
        let store = fixture();
        for (id, ago) in [(1, 90), (2, 60), (3, 30)] {
            mail(&store, 1, id, &format!("t{id}"), ago);
        }
        let invited = |change| Ev {
            commitment: EventCommitment::Invited,
            change,
            kind: EventRecordKind::Invite,
            ..Default::default()
        };
        decide(&store, 1, 1, &invited(EventChange::Created).decision());
        let g = listed(&store, 1).remove(0);
        assert_eq!(g.commitment, EventCommitment::Invited);
        assert!(g.served_likelihood().is_some());
        decide(&store, 1, 2, &invited(EventChange::Agreed).decision());
        decide(&store, 1, 3, &invited(EventChange::Updated).decision());
        let g = listed(&store, 1).remove(0);
        assert_eq!(
            g.commitment,
            EventCommitment::Committed,
            "the newest says invited, an older one says the user agreed"
        );
        assert!(g.likelihood.is_some(), "stored");
        assert_eq!(g.served_likelihood(), None, "never served when committed");
        // Retract the agreement: back to the newest proposal's word.
        decide(&store, 1, 2, &MessageDecision::default());
        assert_eq!(listed(&store, 1)[0].commitment, EventCommitment::Invited);
    }

    #[test]
    fn a_user_end_becomes_the_prior_for_the_venue_and_kind() {
        let store = fixture();
        for (id, ago) in [(1, 90), (2, 60), (3, 30)] {
            mail(&store, 1, id, &format!("t{id}"), ago);
        }
        decide(&store, 1, 1, &Ev::default().decision());
        decide(
            &store,
            1,
            2,
            &Ev {
                refs: &["QXJZKM"],
                location: Some("BROOKLYN STEEL"),
                start: Some("2026-11-07T21:00:00-05:00"),
                ..Default::default()
            }
            .decision(),
        );
        decide(
            &store,
            1,
            3,
            &Ev {
                refs: &["ZZZ999"],
                kind: EventRecordKind::Other,
                ..Default::default()
            }
            .decision(),
        );
        let first = listed(&store, 1)
            .into_iter()
            .find(|g| g.last_message_id == Some(1))
            .unwrap();
        assert_eq!(first.history_minutes, None);
        // 8pm to 2am.
        assert!(
            store
                .set_ghost_user_end(
                    1,
                    first.id,
                    Some(at("2026-10-04T02:00:00-04:00")),
                    Utc::now()
                )
                .unwrap()
        );
        let ghosts = listed(&store, 1);
        let by_message = |m| {
            ghosts
                .iter()
                .find(|g| g.last_message_id == Some(m))
                .unwrap()
        };
        let first = by_message(1);
        let end = first.effective_end().unwrap();
        assert_eq!(
            (end.source, end.estimated),
            (crate::triage::ghost::EndSource::User, false)
        );
        let second = by_message(2);
        assert_eq!(second.history_minutes, Some(360));
        let end = second.effective_end().unwrap();
        assert_eq!(end.source, crate::triage::ghost::EndSource::History);
        assert!(end.estimated);
        assert_eq!(end.end, at("2026-11-08T03:00:00-05:00"));
        assert_eq!(by_message(3).history_minutes, None, "another kind");

        // Clearing the end keeps what was learned.
        store
            .set_ghost_user_end(1, first.id, None, Utc::now())
            .unwrap();
        assert_eq!(by_message_after(&store, 2).history_minutes, Some(360));
        assert_eq!(by_message_after(&store, 1).user_end, None);
    }
    fn by_message_after(store: &SqliteStore, message: i64) -> GhostEvent {
        listed(store, 1)
            .into_iter()
            .find(|g| g.last_message_id == Some(message))
            .unwrap()
    }

    #[test]
    fn a_user_end_must_follow_the_start_at_its_precision() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        mail(&store, 1, 2, "t2", 30);
        decide(&store, 1, 1, &Ev::default().decision());
        decide(
            &store,
            1,
            2,
            &Ev {
                refs: &["QXJZKM"],
                start: Some("2026-10-10"),
                ..Default::default()
            }
            .decision(),
        );
        let ghosts = listed(&store, 1);
        let (timed, day) = if ghosts[0].all_day() {
            (ghosts[1].id, ghosts[0].id)
        } else {
            (ghosts[0].id, ghosts[1].id)
        };
        let now = Utc::now();
        for (id, end) in [
            (timed, "2026-10-03T19:00:00-04:00"),
            (timed, "2026-10-03T20:00:00-04:00"),
            (timed, "2026-10-04"),
            (day, "2026-10-10T23:00:00-04:00"),
            (day, "2026-10-10"),
        ] {
            assert!(
                matches!(
                    store.set_ghost_user_end(1, id, Some(at(end)), now),
                    Err(CoreError::InvalidInput(_))
                ),
                "{end}"
            );
        }
        assert!(
            store
                .set_ghost_user_end(1, day, Some(at("2026-10-12")), now)
                .unwrap()
        );
        assert_eq!(
            count(&store, "event_duration_priors"),
            0,
            "a date-only span teaches no duration"
        );
        assert!(!store.set_ghost_user_end(1, 999, None, now).unwrap());
    }

    #[test]
    fn listing_compares_instants_across_offsets_and_dates_by_date() {
        let store = fixture();
        let starts = [
            (1, Some("2026-10-03T23:30:00-07:00")), // 06:30Z on the 4th
            (2, Some("2026-10-04T05:00:00+09:00")), // 20:00Z on the 3rd
            (3, Some("2026-10-04")),
            (4, Some("2026-10-05")),
            (5, None),
        ];
        for (id, start) in starts {
            mail(&store, 1, id, &format!("t{id}"), 60 - id);
            decide(
                &store,
                1,
                id,
                &Ev {
                    refs: &[],
                    title: &format!("Event {id}"),
                    start,
                    ..Default::default()
                }
                .decision(),
            );
        }
        assert_eq!(
            count(&store, "ghost_events"),
            5,
            "the startless one is stored"
        );
        let range = |from: &str, to: &str| -> Vec<i64> {
            store
                .list_ghost_events(
                    1,
                    DateTime::parse_from_rfc3339(from).unwrap(),
                    DateTime::parse_from_rfc3339(to).unwrap(),
                )
                .unwrap()
                .iter()
                .map(|g| g.last_message_id.unwrap())
                .collect()
        };
        // The 4th, UTC: message 2 is the 3rd in UTC, message 1 the 4th. The
        // date-only 4th sorts at the start of its day.
        assert_eq!(
            range("2026-10-04T00:00:00Z", "2026-10-05T00:00:00Z"),
            [3, 1]
        );
        // The 3rd in Tokyo ends at 15:00Z on the 3rd: neither instant.
        assert_eq!(
            range("2026-10-03T00:00:00+09:00", "2026-10-04T00:00:00+09:00"),
            Vec::<i64>::new()
        );
        // The 4th in Tokyo: message 2 at 05:00 local, message 1 at 15:30
        // local, and the date-only 4th.
        assert_eq!(
            range("2026-10-04T00:00:00+09:00", "2026-10-05T00:00:00+09:00"),
            [2, 3, 1]
        );
        // A range ending mid-day includes that day's date-only ghost.
        assert_eq!(
            range("2026-10-04T12:00:00-07:00", "2026-10-05T09:00:00-07:00"),
            [3, 4]
        );
        // Ordered by start; the startless ghost is never listed.
        assert_eq!(
            range("2000-01-01T00:00:00Z", "2100-01-01T00:00:00Z"),
            [2, 3, 1, 4]
        );
        assert!(range("2026-10-05T00:00:00Z", "2026-10-04T00:00:00Z").is_empty());
    }

    #[test]
    fn deleting_a_message_takes_its_source_rows_and_any_ghost_it_alone_fed() {
        let store = fixture();
        for (id, ago) in [(1, 90), (2, 60), (3, 30)] {
            mail(&store, 1, id, &format!("t{id}"), ago);
        }
        decide(&store, 1, 1, &Ev::default().decision());
        decide(
            &store,
            1,
            2,
            &Ev {
                change: EventChange::Updated,
                ..Default::default()
            }
            .decision(),
        );
        decide(
            &store,
            1,
            3,
            &Ev {
                refs: &["QXJZKM"],
                ..Default::default()
            }
            .decision(),
        );
        let conn = store.lock().unwrap();
        conn.execute("DELETE FROM messages WHERE id IN (2,3)", [])
            .unwrap();
        drop(conn);
        assert_eq!(count(&store, "ghost_events"), 1, "the shared one stays");
        assert_eq!(count(&store, "ghost_event_sources"), 1);
        assert_eq!(count(&store, "ghost_event_keys"), 1);
        assert_eq!(
            roles(&listed(&store, 1)[0]),
            vec![(1, EventChange::Created)]
        );
    }

    #[test]
    fn served_state_runs_user_then_mail_then_clock() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        decide(&store, 1, 1, &Ev::default().decision());
        let mut g = listed(&store, 1).remove(0);
        let at_ = |s| DateTime::parse_from_rfc3339(s).unwrap();
        // Starts 20:00 with no history: the model's 150 minutes end it 22:30.
        assert_eq!(g.state(at_("2026-10-03T22:00:00-04:00")), GhostState::Ghost);
        assert_eq!(
            g.state(at_("2026-10-03T22:30:00-04:00")),
            GhostState::Expired
        );
        g.mail_state = GhostMailState::Cancelled;
        assert_eq!(g.state(at_("2026-10-04T00:00:00Z")), GhostState::Cancelled);
        g.user_state = Some(GhostUserState::Going);
        assert_eq!(g.state(at_("2026-10-04T00:00:00Z")), GhostState::Going);
        // A date-only ghost is live through its whole day.
        g.user_state = None;
        g.mail_state = GhostMailState::Live;
        g.start = at("2026-10-03");
        g.estimate = None;
        assert!(g.all_day());
        assert_eq!(g.state(at_("2026-10-03T23:59:00-04:00")), GhostState::Ghost);
        assert_eq!(
            g.state(at_("2026-10-04T00:00:00-04:00")),
            GhostState::Expired
        );
    }
}
