//! Calendar ghosts, the stored half: the projection of the agent's `event`
//! records into `ghost_events`, and the human door's reads and writes on it.
//!
//! Canonical decisions own these rows, as they own agent-created shipments
//! (`agent_deliveries`). IDENTITY IS AN EXPLICIT ASSIGNMENT: each laundered
//! event record (a message and its `position`) belongs to exactly one ghost,
//! chosen once when its message is decided ([`reconcile`]) and remembered in
//! `ghost_event_sources`. A rebuild reloads exactly the assigned records and
//! nothing else, so a ghost is never found or reshaped by key intersection,
//! and ghosts are NEVER merged: when a record could belong to several, it is
//! assigned to one and the rest are left alone. A duplicate ghost is an
//! acceptable outcome; losing or rewriting one is not.
//!
//! A REF ONLY MEANS SOMETHING WITHIN ITS SENDER. Keys are
//! `ref:<sender scope>:<ref>` (`triage::ghost::sender_scope`), so mail from
//! another domain quoting the user's booking number makes its own ghost. A
//! record from mail that did not pass authentication has no scope: it never
//! matches a ghost by ref or thread, never joins a ghost other mail feeds, and
//! never commits one.
//!
//! The user_* columns are the user's: no rebuild touches them.
//!
//! THE PROJECTION NEVER BLOCKS TRIAGE. [`reconcile_guarded`] runs it under a
//! savepoint inside the triage commit; an error rolls back the ghosts alone,
//! logs ids and an error kind (never a title, location or subject), and the
//! commit proceeds.
//!
//! SENT, SPAM AND RESTRICTED MAIL NEVER FEEDS A GHOST. Its assigned records
//! keep their rows (so un-spamming brings the ghost back with the user's
//! state) but contribute nothing to a rebuild, and
//! [`SqliteStore::list_ghost_events`] serves a ghost only while the message
//! that supplied its facts is eligible, judged in SQL, so a status that changed
//! after the last rebuild cannot serve facts from it.
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

/// Source rows one ghost may hold. A new record past this replaces the oldest,
/// so one long-running thread cannot grow a ghost (and every rebuild of it)
/// without bound.
const SOURCES_PER_GHOST: i64 = 50;

/// Ghosts one listing returns at most.
const LIST_CAP: i64 = 2000;

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
/// SQL range to cover any zone that date might be read in, and orders exactly
/// in Rust.
fn sort_key(time: &EventTime) -> String {
    match time {
        EventTime::Date(day) => format!("{}T00:00:00Z", day.format("%Y-%m-%d")),
        EventTime::At(at) => at
            .with_timezone(&Utc)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
    }
}

/// A start as one instant for distance comparisons: a date at 00:00Z.
fn instant(time: &EventTime) -> DateTime<Utc> {
    match time {
        EventTime::Date(day) => day.and_time(chrono::NaiveTime::MIN).and_utc(),
        EventTime::At(at) => at.with_timezone(&Utc),
    }
}

/// The calendar date a start names, in the mail's own offset.
fn local_date(time: &EventTime) -> chrono::NaiveDate {
    match time {
        EventTime::Date(day) => *day,
        EventTime::At(at) => at.date_naive(),
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

/// Run `project` under its own savepoint. Infallible on purpose: a ghost is
/// worth less than the write it rides on. `what` names the caller in the log
/// line, which carries ids and an error kind only.
fn guarded(
    conn: &Connection,
    account: AccountId,
    message: i64,
    what: &str,
    project: impl FnOnce() -> Result<()>,
) {
    if let Err(e) = conn.execute_batch("SAVEPOINT ghost_projection") {
        eprintln!(
            "squelch: ghost {what} skipped for account {account} message {message} ({})",
            error_kind(&e.into())
        );
        return;
    }
    match project() {
        Ok(()) => {
            if let Err(e) = conn.execute_batch("RELEASE ghost_projection") {
                eprintln!(
                    "squelch: ghost {what} release failed for account {account} message {message} ({})",
                    error_kind(&e.into())
                );
            }
        }
        Err(e) => {
            eprintln!(
                "squelch: ghost {what} failed for account {account} message {message} ({}); the write committed without it",
                error_kind(&e)
            );
            // If even this fails the outer transaction is already gone, and
            // its commit reports that on its own.
            let _ = conn.execute_batch("ROLLBACK TO ghost_projection; RELEASE ghost_projection");
        }
    }
}

/// [`reconcile`] under its own savepoint, for the triage commit.
///
/// `eligible` is false for sent, spam and restricted mail, which then feeds
/// nothing: its assigned records keep their rows and stop contributing.
pub(super) fn reconcile_guarded(
    conn: &Connection,
    account: AccountId,
    message: i64,
    eligible: bool,
    current: &MessageDecision,
) {
    guarded(conn, account, message, "projection", || {
        reconcile(conn, account, message, eligible, current)
    });
}

/// Rebuild every ghost `message` is assigned to, under the same guard. For a
/// path that changes whether a message may feed a ghost (its spam flag or its
/// agent access) WITHOUT a triage commit: a rebuild reads stored decisions and
/// current eligibility, so it needs no decision in hand.
pub(super) fn refresh_message_guarded(conn: &Connection, account: AccountId, message: i64) {
    guarded(conn, account, message, "refresh", || {
        let now = Utc::now().to_rfc3339();
        for id in ghosts_fed_by(conn, account, message)? {
            rebuild(conn, account, id, &now)?;
        }
        Ok(())
    });
}

/// The ghosts `message` is assigned to.
fn ghosts_fed_by(conn: &Connection, account: AccountId, message: i64) -> Result<BTreeSet<i64>> {
    let mut statement = conn.prepare_cached(
        "SELECT DISTINCT ghost_id FROM ghost_event_sources WHERE account_id=?1 AND message_id=?2",
    )?;
    let found = statement
        .query_map(params![account, message], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    Ok(found)
}

/// What matching needs to know about one existing ghost.
struct Candidate {
    id: i64,
    start: Option<EventTime>,
    live: bool,
    keys: BTreeSet<String>,
    /// Some source row belongs to another message.
    other_sources: bool,
    /// Some source row of another message is unauthenticated: such a ghost
    /// is that record's alone, and nothing else joins it.
    other_unauthenticated: bool,
    /// The message being decided fed it before this decision.
    fed_by_this: bool,
}

fn candidate(
    conn: &Connection,
    account: AccountId,
    id: i64,
    message: i64,
    fed_before: &BTreeSet<i64>,
) -> Result<Option<Candidate>> {
    let Some((starts_at, mail_state)) = conn
        .query_row(
            "SELECT starts_at,mail_state FROM ghost_events WHERE account_id=?1 AND id=?2",
            params![account, id],
            |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let keys: BTreeSet<String> = {
        let mut statement = conn.prepare_cached(
            "SELECT key FROM ghost_event_keys WHERE account_id=?1 AND ghost_id=?2",
        )?;
        statement
            .query_map(params![account, id], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?
    };
    let (other_sources, other_unauthenticated): (bool, bool) = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM ghost_event_sources
                       WHERE account_id=?1 AND ghost_id=?2 AND message_id<>?3),
                EXISTS(SELECT 1 FROM ghost_event_sources
                       WHERE account_id=?1 AND ghost_id=?2 AND message_id<>?3 AND scope IS NULL)",
        params![account, id, message],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(Some(Candidate {
        id,
        start: starts_at.as_deref().and_then(EventTime::parse),
        live: mail_state != "cancelled",
        keys,
        other_sources,
        other_unauthenticated,
        fed_by_this: fed_before.contains(&id),
    }))
}

/// Seconds between two starts, or None when either is missing.
fn distance(a: Option<&EventTime>, b: Option<&EventTime>) -> Option<i64> {
    Some((instant(a?) - instant(b?)).num_seconds().abs())
}

/// The order among equally good candidates: a ghost this message already fed,
/// then the nearest start (a missing one last), then the lowest id.
fn rank(record: &GhostProposal, c: &Candidate) -> (bool, i64, i64) {
    (
        !c.fed_by_this,
        distance(record.start.as_ref(), c.start.as_ref()).unwrap_or(i64::MAX),
        c.id,
    )
}

/// A3 (a): the ghost sharing the most of this record's scoped ref keys. A
/// `created` record is not the same event as a ghost whose start is more than
/// a day from its own: flight numbers, season-ticket seats and other
/// low-entropy refs repeat across dates.
fn match_by_ref(
    conn: &Connection,
    account: AccountId,
    message: i64,
    record: &GhostProposal,
    keys: &[String],
    fed_before: &BTreeSet<i64>,
    claimed: &BTreeSet<i64>,
) -> Result<Option<i64>> {
    let mut shared: BTreeMap<i64, usize> = BTreeMap::new();
    {
        let mut statement = conn.prepare_cached(
            "SELECT DISTINCT ghost_id FROM ghost_event_keys WHERE account_id=?1 AND key=?2",
        )?;
        for key in keys {
            for id in statement.query_map(params![account, key], |r| r.get::<_, i64>(0))? {
                *shared.entry(id?).or_default() += 1;
            }
        }
    }
    let mut best: Option<(std::cmp::Reverse<usize>, (bool, i64, i64))> = None;
    for (id, count) in shared {
        if claimed.contains(&id) {
            continue;
        }
        let Some(c) = candidate(conn, account, id, message, fed_before)? else {
            continue;
        };
        if c.other_unauthenticated {
            continue;
        }
        if record.change == EventChange::Created
            && distance(record.start.as_ref(), c.start.as_ref())
                .is_some_and(|d| d > chrono::Duration::days(1).num_seconds())
        {
            continue;
        }
        // Most shared keys first, then the rank; smaller sorts first.
        let key = (std::cmp::Reverse(count), rank(record, &c));
        if best.as_ref().is_none_or(|b| key < *b) {
            best = Some(key);
        }
    }
    // The rank's last field is the ghost id.
    Ok(best.map(|(_, (_, _, id))| id))
}

/// A3 (b), for a record with no usable refs: a ghost fed from the same Gmail
/// thread. A ghost holding ref keys of another sender is not a candidate. The
/// one starting on the record's date wins; failing that, a record that is not
/// `created` takes the thread's only live candidate (a ref-less "moved to 9pm"
/// follow-up).
#[allow(clippy::too_many_arguments)] // One record and the state it is matched against.
fn match_by_thread(
    conn: &Connection,
    account: AccountId,
    message: i64,
    thread: &str,
    scope: &str,
    record: &GhostProposal,
    fed_before: &BTreeSet<i64>,
    claimed: &BTreeSet<i64>,
) -> Result<Option<i64>> {
    let ids: Vec<i64> = {
        let mut statement = conn.prepare_cached(
            "SELECT DISTINCT s.ghost_id FROM ghost_event_sources s
             JOIN messages m ON m.account_id=s.account_id AND m.id=s.message_id
             WHERE s.account_id=?1 AND m.thread_id=?2",
        )?;
        statement
            .query_map(params![account, thread], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?
    };
    let own = format!("ref:{scope}:");
    let mut candidates = Vec::new();
    for id in ids {
        if claimed.contains(&id) {
            continue;
        }
        let Some(c) = candidate(conn, account, id, message, fed_before)? else {
            continue;
        };
        if c.other_unauthenticated || c.keys.iter().any(|k| !k.starts_with(&own)) {
            continue;
        }
        candidates.push(c);
    }
    let same_date = candidates
        .iter()
        .filter(|c| match (&record.start, &c.start) {
            (Some(r), Some(g)) => local_date(r) == local_date(g),
            _ => false,
        })
        .min_by_key(|c| rank(record, c));
    if let Some(c) = same_date {
        return Ok(Some(c.id));
    }
    if record.change != EventChange::Created {
        let live: Vec<&Candidate> = candidates.iter().filter(|c| c.live).collect();
        if let [only] = live.as_slice() {
            return Ok(Some(only.id));
        }
    }
    Ok(None)
}

/// Project one message's current decision. See the module doc for the rules;
/// in order:
///
/// 1. An ineligible message (sent, spam, restricted) keeps whatever it was
///    assigned and contributes nothing: its ghosts are rebuilt without it.
/// 2. Each laundered event record is matched against the state BEFORE this
///    message's old assignment is removed, so a re-decision finds the ghosts
///    it fed: by scoped ref, else (no refs) by thread, else the ghost this
///    message fed at that position (or its only ghost), else a new ghost. An
///    unauthenticated record skips the first two and reuses only a ghost no
///    other message feeds. No two records of one message share a ghost.
/// 3. The old assignment is replaced by the new one, and every ghost the
///    message fed before or feeds now is rebuilt. A ghost is deleted only when
///    no source row is left at all.
pub(super) fn reconcile(
    conn: &Connection,
    account: AccountId,
    message: i64,
    eligible: bool,
    current: &MessageDecision,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let previous: BTreeMap<usize, i64> = {
        let mut statement = conn.prepare_cached(
            "SELECT position,ghost_id FROM ghost_event_sources WHERE account_id=?1 AND message_id=?2",
        )?;
        statement
            .query_map(params![account, message], |r| {
                Ok((r.get::<_, i64>(0)? as usize, r.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<_, _>>()?
    };
    let fed_before: BTreeSet<i64> = previous.values().copied().collect();
    if !eligible {
        for &id in &fed_before {
            rebuild(conn, account, id, &now)?;
        }
        return Ok(());
    }
    let records = ghost::launder_events(&current.records);
    if records.is_empty() && previous.is_empty() {
        return Ok(());
    }
    let (thread, from_addr, auth_pass): (String, String, Option<bool>) = conn.query_row(
        "SELECT thread_id,from_addr,auth_pass FROM messages WHERE account_id=?1 AND id=?2",
        params![account, message],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let scope = ghost::sender_scope(&from_addr, auth_pass);

    let mut claimed: BTreeSet<i64> = BTreeSet::new();
    let mut assigned: Vec<(usize, i64, String)> = Vec::new();
    for (position, record) in records.iter().enumerate() {
        let mut target = None;
        if let Some(scope) = scope.as_deref() {
            let keys = ghost::identity_keys(record, Some(scope));
            target = if keys.is_empty() {
                match_by_thread(
                    conn,
                    account,
                    message,
                    &thread,
                    scope,
                    record,
                    &fed_before,
                    &claimed,
                )?
            } else {
                match_by_ref(conn, account, message, record, &keys, &fed_before, &claimed)?
            };
        }
        if target.is_none() {
            // (c) The ghost this message fed before: what keeps a ghost's id
            // and the user's mark when a re-triage rewords a title or a ref.
            let reusable = |id: i64| -> Result<bool> {
                if claimed.contains(&id) {
                    return Ok(false);
                }
                if scope.is_some() {
                    return Ok(true);
                }
                // An unauthenticated record keeps only a ghost of its own.
                Ok(candidate(conn, account, id, message, &fed_before)?
                    .is_some_and(|c| !c.other_sources))
            };
            if let Some(&id) = previous.get(&position)
                && reusable(id)?
            {
                target = Some(id);
            } else if let [only] = fed_before.iter().copied().collect::<Vec<_>>().as_slice()
                && reusable(*only)?
            {
                target = Some(*only);
            }
        }
        let id = match target {
            Some(id) => id,
            None => {
                // A placeholder the rebuild below fills in.
                conn.execute(
                    "INSERT INTO ghost_events(account_id,kind,title,commitment,created_at,updated_at)
                     VALUES(?1,?2,?3,'unknown',?4,?4)",
                    params![account, label(&record.kind), record.title, now],
                )?;
                conn.last_insert_rowid()
            }
        };
        claimed.insert(id);
        assigned.push((position, id, label(&record.change)));
    }

    conn.execute(
        "DELETE FROM ghost_event_sources WHERE account_id=?1 AND message_id=?2",
        params![account, message],
    )?;
    for (position, id, role) in &assigned {
        let held: i64 = conn.query_row(
            "SELECT COUNT(*) FROM ghost_event_sources WHERE account_id=?1 AND ghost_id=?2",
            params![account, id],
            |r| r.get(0),
        )?;
        if held >= SOURCES_PER_GHOST {
            conn.execute(
                "DELETE FROM ghost_event_sources WHERE rowid IN (
                     SELECT s.rowid FROM ghost_event_sources s
                     LEFT JOIN messages m ON m.account_id=s.account_id AND m.id=s.message_id
                     WHERE s.account_id=?1 AND s.ghost_id=?2
                     ORDER BY m.received_at,s.message_id,s.position LIMIT ?3)",
                params![account, id, held - SOURCES_PER_GHOST + 1],
            )?;
        }
        conn.execute(
            "INSERT INTO ghost_event_sources(account_id,ghost_id,message_id,position,role,scope)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![account, id, message, *position as i64, role, scope],
        )?;
    }
    let affected: BTreeSet<i64> = fed_before
        .into_iter()
        .chain(assigned.iter().map(|(_, id, _)| *id))
        .collect();
    for id in affected {
        rebuild(conn, account, id, &now)?;
    }
    Ok(())
}

/// One assigned record that feeds a ghost now.
struct Fed {
    message: i64,
    received: DateTime<Utc>,
    position: usize,
    scope: Option<String>,
    proposal: GhostProposal,
}

/// What a source message's stored decision yields for a rebuild.
enum Stored {
    /// Its laundered event records, in position order.
    Events(Vec<GhostProposal>),
    /// It may not feed a ghost right now: spam, sent or restricted (by its
    /// access state or by the decision itself).
    Ineligible,
    /// Its decision does not deserialize. Not known to have stopped feeding
    /// the ghost, so its rows stay and one bad row rewrites nothing.
    Unreadable,
}

/// Rebuild one ghost from exactly its assigned records, loading only those
/// messages' decisions.
///
/// - A row whose message is ineligible or unreadable stays and contributes
///   nothing. A row whose message's decision no longer has a record at that
///   position (or has no decision) is stale and goes.
/// - No rows left: the ghost is deleted. Rows but no eligible record: the
///   facts stay, `supplier_message_id` is cleared and the keys go, so the
///   listing hides the ghost until a source is eligible again.
/// - Otherwise facts come from the newest eligible record that does not
///   cancel (or the newest, when all do), `mail_state` from the newest, and
///   commitment is `committed` only through an authenticated record of the
///   ghost's own sender scope (the oldest authenticated record's) that is
///   committed or agreed. Keys are the union of the eligible records' keys.
pub(super) fn rebuild(conn: &Connection, account: AccountId, id: i64, now: &str) -> Result<()> {
    type SourceRow = (i64, i64, Option<String>, String, bool, Option<String>);
    let rows: Vec<SourceRow> = {
        let mut statement = conn.prepare_cached(&format!(
            "SELECT s.message_id,s.position,s.scope,m.received_at,{ELIGIBLE},d.decision_json
             FROM ghost_event_sources s
             JOIN messages m ON m.account_id=s.account_id AND m.id=s.message_id
             LEFT JOIN agent_message_state a ON a.account_id=s.account_id AND a.message_id=s.message_id
             LEFT JOIN agent_message_decisions d ON d.account_id=s.account_id AND d.message_id=s.message_id
             WHERE s.account_id=?1 AND s.ghost_id=?2"
        ))?;
        statement
            .query_map(params![account, id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<std::result::Result<_, _>>()?
    };
    let mut stored: BTreeMap<i64, Stored> = BTreeMap::new();
    let mut feds: Vec<Fed> = Vec::new();
    let mut stale: Vec<(i64, i64)> = Vec::new();
    let total = rows.len();
    for (message, position, scope, received, eligible, encoded) in rows {
        let entry = stored.entry(message).or_insert_with(|| {
            if !eligible {
                return Stored::Ineligible;
            }
            let Some(encoded) = encoded else {
                return Stored::Events(Vec::new());
            };
            match serde_json::from_str::<MessageDecision>(&encoded) {
                Ok(decision) if decision.external_access.restricted => Stored::Ineligible,
                Ok(decision) => Stored::Events(ghost::launder_events(&decision.records)),
                Err(_) => {
                    eprintln!(
                        "squelch: ghost rebuild skipped an unreadable decision (account {account} message {message})"
                    );
                    Stored::Unreadable
                }
            }
        });
        let Stored::Events(events) = entry else {
            continue;
        };
        let Some(proposal) = usize::try_from(position).ok().and_then(|p| events.get(p)) else {
            stale.push((message, position));
            continue;
        };
        feds.push(Fed {
            message,
            received: DateTime::parse_from_rfc3339(&received)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or(DateTime::<Utc>::MIN_UTC),
            position: position as usize,
            scope,
            proposal: proposal.clone(),
        });
    }
    for (message, position) in &stale {
        conn.execute(
            "DELETE FROM ghost_event_sources WHERE account_id=?1 AND message_id=?2 AND position=?3",
            params![account, message, position],
        )?;
    }
    if stale.len() == total {
        conn.execute(
            "DELETE FROM ghost_events WHERE account_id=?1 AND id=?2",
            params![account, id],
        )?;
        return Ok(());
    }
    conn.execute(
        "DELETE FROM ghost_event_keys WHERE account_id=?1 AND ghost_id=?2",
        params![account, id],
    )?;
    if feds.is_empty() {
        conn.execute(
            "UPDATE ghost_events SET supplier_message_id=NULL,updated_at=?3
             WHERE account_id=?1 AND id=?2",
            params![account, id, now],
        )?;
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
    // The ghost speaks for the sender that first described it. Thread
    // matching can attach another sender's ref-less mail; that mail may
    // update the facts but cannot commit the user.
    let owner = feds
        .iter()
        .filter(|f| f.scope.is_some())
        .min_by(|a, b| {
            a.received
                .cmp(&b.received)
                .then(a.message.cmp(&b.message))
                .then(a.position.cmp(&b.position))
        })
        .and_then(|f| f.scope.as_deref());
    let commitment = if feds.iter().any(|f| {
        f.scope.is_some()
            && f.scope.as_deref() == owner
            && (f.proposal.commitment == EventCommitment::Committed
                || f.proposal.change == EventChange::Agreed)
    }) {
        EventCommitment::Committed
    } else if newest.scope.is_none() || newest.proposal.commitment == EventCommitment::Committed {
        // Unauthenticated mail's word, or a committed claim from outside the
        // ghost's own sender, is no evidence of anything.
        EventCommitment::Unknown
    } else {
        newest.proposal.commitment
    };
    let p = &supplier.proposal;
    conn.execute(
        "UPDATE ghost_events SET kind=?3,title=?4,starts_at=?5,start_sort=?6,start_timezone=?7,
             ends_at=?8,est_minutes=?9,est_basis=?10,location=?11,venue_key=?12,commitment=?13,
             mail_state=?14,likelihood_band=?15,likelihood_reason=?16,supplier_message_id=?17,
             last_message_id=?18,updated_at=?19
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
            supplier.message,
            newest.message,
            now,
        ],
    )?;
    let keys: BTreeSet<String> = feds
        .iter()
        .flat_map(|f| ghost::identity_keys(&f.proposal, f.scope.as_deref()))
        .collect();
    for key in keys {
        conn.execute(
            "INSERT INTO ghost_event_keys(account_id,ghost_id,key) VALUES(?1,?2,?3)",
            params![account, id, key],
        )?;
    }
    for f in &feds {
        conn.execute(
            "UPDATE ghost_event_sources SET role=?4 WHERE account_id=?1 AND message_id=?2 AND position=?3",
            params![account, f.message, f.position as i64, label(&f.proposal.change)],
        )?;
    }
    Ok(())
}

/// Does a start fall in `[from, to)`? An instant compares as an instant. A
/// date compares as a date: on or after `from`'s date in `from`'s offset, and
/// before `to`'s date read in `to`'s OWN offset (or on it, when `to` is past
/// that day's midnight). Reading `to` in `from`'s offset listed one all-day
/// ghost in two adjacent weeks across a DST change.
fn in_range(start: &EventTime, from: DateTime<FixedOffset>, to: DateTime<FixedOffset>) -> bool {
    match start {
        EventTime::At(at) => *at >= from && *at < to,
        EventTime::Date(day) => {
            let first_day = from.date_naive();
            let end_day = to.date_naive();
            // `to` exactly at midnight excludes that day.
            let ends_inside = to.time() > chrono::NaiveTime::MIN;
            *day >= first_day && (*day < end_day || (ends_inside && *day == end_day))
        }
    }
}

/// Where a start sorts for a caller in `offset`: an instant as itself, a date
/// at 00:00 of that date in the caller's offset (not UTC, which put a western
/// caller's all-day Saturday before their Friday evening).
fn listing_order(start: &EventTime, offset: &FixedOffset) -> DateTime<Utc> {
    match start {
        EventTime::At(at) => at.with_timezone(&Utc),
        EventTime::Date(day) => day
            .and_time(chrono::NaiveTime::MIN)
            .and_local_timezone(*offset)
            .single()
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|| instant(start)),
    }
}

/// The user's end correction as an end, from its stored span and the start
/// as it is NOW. A span stored at the other precision is ignored.
fn user_end(start: &EventTime, minutes: Option<i64>, days: Option<i64>) -> Option<EventTime> {
    match (start, minutes, days) {
        (EventTime::At(at), Some(minutes), _) => at
            .checked_add_signed(chrono::Duration::minutes(minutes))
            .map(EventTime::At),
        (EventTime::Date(day), _, Some(days)) => day
            .checked_add_signed(chrono::Duration::days(days))
            .map(EventTime::Date),
        _ => None,
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
                    g.likelihood_reason,g.user_state,g.user_state_at,g.user_end_minutes,
                    g.user_end_days,p.minutes,g.supplier_message_id,g.last_message_id,
                    g.created_at,g.updated_at
             FROM ghost_events g
             LEFT JOIN event_duration_priors p
               ON p.account_id=g.account_id AND p.venue_key=g.venue_key AND p.kind=g.kind
             WHERE g.account_id=?1 AND g.start_sort IS NOT NULL
               AND g.start_sort>=?2 AND g.start_sort<=?3
               AND EXISTS(SELECT 1 FROM messages m
                   LEFT JOIN agent_message_state a ON a.account_id=m.account_id AND a.message_id=m.id
                   WHERE m.account_id=g.account_id AND m.id=g.supplier_message_id AND {ELIGIBLE})
             ORDER BY g.start_sort,g.id
             LIMIT {LIST_CAP}"
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
                let est_minutes: Option<u32> = r.get(6)?;
                let est_basis: Option<String> = r.get(7)?;
                let span: (Option<i64>, Option<i64>) = (r.get(15)?, r.get(16)?);
                Ok((
                    EventTime::parse(&starts_at),
                    span,
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
                        // Derived below from the span and the start.
                        user_end: None,
                        history_minutes: r.get(17)?,
                        supplier_message_id: r.get(18)?,
                        last_message_id: r.get(19)?,
                        created_at: dt(r, 20)?,
                        updated_at: dt(r, 21)?,
                        sources: Vec::new(),
                    },
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        // One entry per (message, role): a message with two records on one
        // ghost cannot (no two records of a message share a ghost), but the
        // wire shape is per message and role either way.
        let mut sources = conn.prepare(&format!(
            "SELECT DISTINCT s.message_id,m.thread_id,s.role,m.received_at FROM ghost_event_sources s
             JOIN messages m ON m.account_id=s.account_id AND m.id=s.message_id
             LEFT JOIN agent_message_state a ON a.account_id=s.account_id AND a.message_id=s.message_id
             WHERE s.account_id=?1 AND s.ghost_id=?2 AND {ELIGIBLE}
             ORDER BY m.received_at,s.message_id,s.role"
        ))?;
        let mut listed = Vec::new();
        for (start, (minutes, days), mut ghost) in rows {
            let Some(start) = start.filter(|s| in_range(s, from, to)) else {
                continue;
            };
            ghost.start = start;
            ghost.user_end = user_end(&start, minutes, days);
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
        let offset = *from.offset();
        listed.sort_by_key(|g| (listing_order(&g.start, &offset), g.id));
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

    /// The end arrives absolute and is stored as a span from the CURRENT
    /// start: minutes for a timed ghost, days for an all-day one.
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
        let mut span: (Option<i64>, Option<i64>) = (None, None);
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
            match (start, end) {
                (EventTime::At(s), EventTime::At(e)) => {
                    let minutes = (*e - s).num_minutes();
                    span.0 = Some(minutes);
                    if let Some(venue) = location.as_deref().and_then(ghost::venue_key) {
                        let minutes = u32::try_from(minutes)
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
                (EventTime::Date(s), EventTime::Date(e)) => {
                    span.1 = Some((*e - s).num_days());
                }
                // Refused above: `is_after` is false across precisions.
                _ => {}
            }
        }
        tx.execute(
            "UPDATE ghost_events SET user_end_minutes=?3,user_end_days=?4 WHERE account_id=?1 AND id=?2",
            params![account, id, span.0, span.1],
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

    /// The sender every fixture message has unless a test says otherwise:
    /// authenticated, so its scope is `venue.example`.
    const VENUE: &str = "tickets@venue.example";

    /// An authenticated message from [`VENUE`], `minutes_ago` old, in `thread`.
    fn mail(store: &SqliteStore, account: AccountId, id: i64, thread: &str, minutes_ago: i64) {
        mail_from(store, account, id, thread, minutes_ago, VENUE, Some(true));
    }

    /// A message from `from` whose authentication result is `auth_pass`.
    fn mail_from(
        store: &SqliteStore,
        account: AccountId,
        id: i64,
        thread: &str,
        minutes_ago: i64,
        from: &str,
        auth_pass: Option<bool>,
    ) {
        let conn = store.lock().unwrap();
        conn.execute(
            "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body,auth_pass)
             VALUES(?1,?2,?3,?4,?5,'Your tickets',?6,'','',?7)",
            params![
                id,
                account,
                id.to_string(),
                thread,
                from,
                (Utc::now() - chrono::Duration::minutes(minutes_ago)).to_rfc3339(),
                auth_pass
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
        let (sent, spam): (bool, bool) = tx
            .query_row(
                "SELECT is_sent,is_spam FROM messages WHERE account_id=?1 AND id=?2",
                params![account, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let eligible = !sent && !spam && !decision.external_access.restricted;
        reconcile(&tx, account, id, eligible, decision).unwrap();
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
        assert_eq!(keys(&store, g.id), vec!["ref:venue.example:abc123"]);
        assert_eq!(g.supplier_message_id, Some(1));
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
        assert_eq!(
            keys(&store, id),
            vec!["ref:venue.example:abc123", "ref:venue.example:qxjzkm"]
        );
        decide(&store, 1, 2, &MessageDecision::default());
        let ghosts = listed(&store, 1);
        assert_eq!(ghosts.len(), 1);
        assert_eq!(ghosts[0].id, id, "rebuilt in place");
        assert_eq!(ghosts[0].start, at("2026-10-03T20:00:00-04:00"));
        assert_eq!(roles(&ghosts[0]), vec![(1, EventChange::Created)]);
        assert_eq!(
            keys(&store, id),
            vec!["ref:venue.example:abc123"],
            "the retracted mail's ref goes with it"
        );
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
            reconcile(&tx, 1, 1, true, &decision).unwrap();
            tx.commit().unwrap();
        }
        let g = listed(&store, 1).remove(0);
        assert_eq!(g.kind, EventRecordKind::Other);
        assert_eq!(g.commitment, EventCommitment::Unknown);
        assert!(keys(&store, g.id).is_empty(), "no refs, so no keys");
        // A NEW ref-less decision in the same thread, same date, joins it by
        // thread and re-reads the old one too.
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
        // A re-decision that restricts its only source hides it. The record
        // keeps its assignment (lifting the restriction brings the ghost back
        // with the user's state) but supplies nothing.
        commit(&store, 4, "record_retraction", &restricted);
        assert!(listed(&store, 1).is_empty());
        assert_eq!(count(&store, "ghost_event_sources"), 1);
        assert_eq!(count(&store, "ghost_event_keys"), 0, "its keys went");
        let supplier: Option<i64> = store
            .lock()
            .unwrap()
            .query_row("SELECT supplier_message_id FROM ghost_events", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(supplier, None);
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
        // The 4th in Tokyo: the date-only 4th sorts at 00:00 Tokyo time,
        // before message 2 at 05:00 local and message 1 at 15:30 local.
        assert_eq!(
            range("2026-10-04T00:00:00+09:00", "2026-10-05T00:00:00+09:00"),
            [3, 2, 1]
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
        let g = listed(&store, 1).remove(0);
        assert_eq!(roles(&g), vec![(1, EventChange::Created)]);
        assert_eq!(
            g.supplier_message_id,
            Some(1),
            "the deleted supplier's stand-in keeps the ghost listed"
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

    // ---- Identity v2 regressions (review RR1..RR11 and the fix contract) ----

    fn ghost_by(store: &SqliteStore, id: i64) -> GhostEvent {
        listed(store, 1)
            .into_iter()
            .find(|g| g.id == id)
            .unwrap_or_else(|| panic!("ghost {id} not listed"))
    }
    fn source_rows(store: &SqliteStore, ghost: i64) -> Vec<(i64, i64)> {
        let conn = store.lock().unwrap();
        let mut statement = conn
            .prepare(
                "SELECT message_id,position FROM ghost_event_sources WHERE ghost_id=?1
                 ORDER BY message_id,position",
            )
            .unwrap();
        statement
            .query_map([ghost], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }

    /// RR1. Two invites from the same calendar service, in different threads,
    /// whose refs differ only in a query string the laundering strips.
    #[test]
    fn two_calendar_invites_in_different_threads_stay_two_ghosts() {
        let store = fixture();
        let google = "calendar-notification@google.com";
        mail_from(&store, 1, 1, "t1", 60, google, Some(true));
        mail_from(&store, 1, 2, "t2", 30, google, Some(true));
        decide(
            &store,
            1,
            1,
            &Ev {
                title: "Standup with Ana",
                refs: &["https://calendar.google.com/calendar/event?action=VIEW&eid=AAAA111"],
                kind: EventRecordKind::Invite,
                ..Default::default()
            }
            .decision(),
        );
        decide(
            &store,
            1,
            2,
            &Ev {
                title: "Dentist",
                start: Some("2026-10-20T09:00:00-04:00"),
                refs: &["https://calendar.google.com/calendar/event?action=VIEW&eid=BBBB222"],
                kind: EventRecordKind::Invite,
                ..Default::default()
            }
            .decision(),
        );
        let mut titles: Vec<String> = listed(&store, 1).into_iter().map(|g| g.title).collect();
        titles.sort();
        assert_eq!(titles, ["Dentist", "Standup with Ana"]);
    }

    /// RR2. No refs: the second mail in the thread is the same dinner even
    /// though the model titled it differently.
    #[test]
    fn a_title_that_drifts_in_one_thread_is_still_one_ghost() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        mail(&store, 1, 2, "t1", 30);
        let dinner = |title, change, start| Ev {
            title,
            refs: &[],
            change,
            start: Some(start),
            kind: EventRecordKind::Reservation,
            ..Default::default()
        };
        decide(
            &store,
            1,
            1,
            &dinner(
                "Dinner at Lilia",
                EventChange::Created,
                "2026-10-03T20:00:00-04:00",
            )
            .decision(),
        );
        decide(
            &store,
            1,
            2,
            &dinner(
                "Dinner at Lilia on Friday",
                EventChange::Updated,
                "2026-10-03T21:00:00-04:00",
            )
            .decision(),
        );
        let ghosts = listed(&store, 1);
        assert_eq!(ghosts.len(), 1);
        assert_eq!(ghosts[0].title, "Dinner at Lilia on Friday");
        assert_eq!(ghosts[0].start, at("2026-10-03T21:00:00-04:00"));
        assert_eq!(
            roles(&ghosts[0]),
            vec![(1, EventChange::Created), (2, EventChange::Updated)]
        );
    }

    /// RR9. The confirmation carries the booking number, the reminder in the
    /// same thread does not.
    #[test]
    fn a_ref_less_reminder_after_a_confirmation_in_its_thread_is_the_same_ghost() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        mail(&store, 1, 2, "t1", 30);
        decide(&store, 1, 1, &Ev::default().decision());
        decide(
            &store,
            1,
            2,
            &Ev {
                refs: &[],
                change: EventChange::Updated,
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
        assert_eq!(
            keys(&store, ghosts[0].id),
            vec!["ref:venue.example:abc123"],
            "the confirmation's key stays"
        );
    }

    /// RR4. A recurring meeting in one thread is one ghost per date.
    #[test]
    fn the_same_title_on_two_dates_in_one_thread_is_two_ghosts() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        mail(&store, 1, 2, "t1", 30);
        for (id, start) in [
            (1, "2026-10-03T19:00:00-04:00"),
            (2, "2026-11-07T19:00:00-05:00"),
        ] {
            decide(
                &store,
                1,
                id,
                &Ev {
                    title: "Book club",
                    refs: &[],
                    start: Some(start),
                    commitment: EventCommitment::Invited,
                    kind: EventRecordKind::Invite,
                    ..Default::default()
                }
                .decision(),
            );
        }
        let starts: Vec<EventTime> = listed(&store, 1).iter().map(|g| g.start).collect();
        assert_eq!(
            starts,
            [
                at("2026-10-03T19:00:00-04:00"),
                at("2026-11-07T19:00:00-05:00")
            ]
        );
    }

    /// RR3. A re-triage of the same message words the title differently.
    #[test]
    fn a_retriage_that_rewords_the_title_keeps_the_ghost_and_its_dismissal() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        decide(
            &store,
            1,
            1,
            &Ev {
                title: "Dinner at Lilia",
                refs: &[],
                ..Default::default()
            }
            .decision(),
        );
        let id = listed(&store, 1)[0].id;
        store
            .set_ghost_user_state(1, id, Some(GhostUserState::Dismissed), Utc::now())
            .unwrap();
        decide(
            &store,
            1,
            1,
            &Ev {
                title: "Dinner at Lilia (8pm)",
                refs: &[],
                ..Default::default()
            }
            .decision(),
        );
        let ghosts = listed(&store, 1);
        assert_eq!(ghosts.len(), 1);
        assert_eq!(ghosts[0].id, id);
        assert_eq!(ghosts[0].title, "Dinner at Lilia (8pm)");
        assert_eq!(ghosts[0].user_state, Some(GhostUserState::Dismissed));
    }

    /// RR3b. A re-triage of the same message reads the booking number
    /// differently: no ghost holds the new key, and the message's own ghost
    /// is reused by position.
    #[test]
    fn a_retriage_that_changes_the_refs_keeps_the_ghost_and_its_dismissal() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        decide(&store, 1, 1, &Ev::default().decision());
        let id = listed(&store, 1)[0].id;
        store
            .set_ghost_user_state(1, id, Some(GhostUserState::Dismissed), Utc::now())
            .unwrap();
        decide(
            &store,
            1,
            1,
            &Ev {
                refs: &["ABC123X"],
                ..Default::default()
            }
            .decision(),
        );
        let ghosts = listed(&store, 1);
        assert_eq!(ghosts.len(), 1);
        assert_eq!(ghosts[0].id, id);
        assert_eq!(ghosts[0].user_state, Some(GhostUserState::Dismissed));
        assert_eq!(
            keys(&store, id),
            vec!["ref:venue.example:abc123x"],
            "the old key left with the old reading"
        );
        assert_eq!(count(&store, "ghost_events"), 1);
    }

    /// Hijack: mail that passes authentication for ANOTHER domain, quoting
    /// the victim's booking number, cannot move or cancel the victim's ghost.
    #[test]
    fn another_domain_quoting_a_ref_cannot_move_or_cancel_the_ghost() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 90);
        mail_from(&store, 1, 2, "t2", 60, "box@evil.example", Some(true));
        mail_from(&store, 1, 3, "t3", 30, "box@evil.example", Some(true));
        decide(&store, 1, 1, &Ev::default().decision());
        let victim = listed(&store, 1)[0].clone();
        decide(
            &store,
            1,
            2,
            &Ev {
                title: "Moved",
                change: EventChange::Updated,
                start: Some("2026-10-04T21:00:00-04:00"),
                ..Default::default()
            }
            .decision(),
        );
        decide(
            &store,
            1,
            3,
            &Ev {
                change: EventChange::Cancelled,
                ..Default::default()
            }
            .decision(),
        );
        let after = ghost_by(&store, victim.id);
        assert_eq!(
            (after.title.as_str(), after.start, after.mail_state),
            ("Show", victim.start, GhostMailState::Live)
        );
        assert_eq!(roles(&after), vec![(1, EventChange::Created)]);
        assert_eq!(keys(&store, victim.id), vec!["ref:venue.example:abc123"]);
        // The stranger's mail made its own ghost instead.
        assert_eq!(listed(&store, 1).len(), 2);
    }

    /// Hijack: mail CLAIMING the victim's own domain that did not pass
    /// authentication (failed, or unknown) is nobody's word: it matches
    /// neither by ref nor, ref-less, by thread.
    #[test]
    fn unauthenticated_mail_spoofing_the_domain_makes_its_own_ghost() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 90);
        mail_from(&store, 1, 2, "t2", 60, VENUE, Some(false));
        mail_from(&store, 1, 3, "t1", 30, VENUE, None);
        decide(&store, 1, 1, &Ev::default().decision());
        let victim = listed(&store, 1)[0].id;
        decide(
            &store,
            1,
            2,
            &Ev {
                change: EventChange::Cancelled,
                ..Default::default()
            }
            .decision(),
        );
        // In the victim's own thread, same date, no refs.
        decide(
            &store,
            1,
            3,
            &Ev {
                title: "Moved",
                refs: &[],
                change: EventChange::Updated,
                start: Some("2026-10-03T23:00:00-04:00"),
                ..Default::default()
            }
            .decision(),
        );
        let g = ghost_by(&store, victim);
        assert_eq!(
            (g.title.as_str(), g.mail_state),
            ("Show", GhostMailState::Live)
        );
        assert_eq!(roles(&g), vec![(1, EventChange::Created)]);
        assert_eq!(count(&store, "ghost_events"), 3);
        // Neither spoof holds a key a later message could find it by.
        assert_eq!(count(&store, "ghost_event_keys"), 1);
        // A re-decision of a spoof keeps its own ghost.
        let spoof = listed(&store, 1)
            .into_iter()
            .find(|g| g.last_message_id == Some(2))
            .unwrap()
            .id;
        decide(
            &store,
            1,
            2,
            &Ev {
                title: "Reworded",
                refs: &["OTHER999"],
                ..Default::default()
            }
            .decision(),
        );
        assert_eq!(ghost_by(&store, spoof).title, "Reworded");
        assert_eq!(count(&store, "ghost_events"), 3);
        // And nothing joins a spoof's ghost: authenticated ref-less mail in
        // its thread, on its date, makes its own.
        mail_from(&store, 1, 4, "t9", 20, VENUE, None);
        mail(&store, 1, 5, "t9", 10);
        let refless = |change| Ev {
            refs: &[],
            change,
            ..Default::default()
        };
        decide(&store, 1, 4, &refless(EventChange::Created).decision());
        decide(&store, 1, 5, &refless(EventChange::Updated).decision());
        assert_eq!(count(&store, "ghost_events"), 5);
    }

    /// Freemail: the domain says nothing, so two senders on it never share a
    /// scope.
    #[test]
    fn two_freemail_senders_do_not_share_a_scope() {
        let store = fixture();
        mail_from(&store, 1, 1, "t1", 60, "alice@gmail.com", Some(true));
        mail_from(&store, 1, 2, "t2", 30, "bob@gmail.com", Some(true));
        mail_from(&store, 1, 3, "t3", 20, "Alice@Gmail.com", Some(true));
        decide(&store, 1, 1, &Ev::default().decision());
        let alice = listed(&store, 1)[0].id;
        let update = Ev {
            title: "Moved",
            change: EventChange::Updated,
            ..Default::default()
        }
        .decision();
        decide(&store, 1, 2, &update);
        assert_eq!(ghost_by(&store, alice).title, "Show", "bob is not alice");
        assert_eq!(listed(&store, 1).len(), 2);
        decide(&store, 1, 3, &update);
        assert_eq!(ghost_by(&store, alice).title, "Moved", "alice is alice");
        assert_eq!(listed(&store, 1).len(), 2);
    }

    /// Commitment is enforced at REBUILD, in the stored column, so every
    /// reader agrees: unauthenticated mail is never served as committed.
    #[test]
    fn a_committed_record_from_unauthenticated_mail_is_unknown() {
        let store = fixture();
        mail_from(&store, 1, 1, "t1", 60, VENUE, None);
        decide(&store, 1, 1, &Ev::default().decision());
        let g = listed(&store, 1).remove(0);
        assert_eq!(g.commitment, EventCommitment::Unknown);
        let stored: String = store
            .lock()
            .unwrap()
            .query_row("SELECT commitment FROM ghost_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, "unknown");
        // Whatever the model said: an invitation is no more its word.
        mail_from(&store, 1, 2, "t2", 30, VENUE, Some(false));
        decide(
            &store,
            1,
            2,
            &Ev {
                commitment: EventCommitment::Invited,
                refs: &["OTHER999"],
                ..Default::default()
            }
            .decision(),
        );
        assert!(
            listed(&store, 1)
                .iter()
                .all(|g| g.commitment == EventCommitment::Unknown)
        );
    }

    /// Thread matching may attach another sender's ref-less mail to a ghost,
    /// but only the ghost's own sender can commit the user.
    #[test]
    fn agreement_from_another_sender_does_not_commit_the_ghost() {
        let store = fixture();
        mail_from(&store, 1, 1, "t1", 90, "host@club.example", Some(true));
        mail_from(&store, 1, 2, "t1", 60, "friend@other.example", Some(true));
        mail_from(&store, 1, 3, "t1", 30, "host@club.example", Some(true));
        let invite = |change| Ev {
            title: "Book club",
            refs: &[],
            change,
            commitment: EventCommitment::Invited,
            kind: EventRecordKind::Invite,
            ..Default::default()
        };
        decide(&store, 1, 1, &invite(EventChange::Created).decision());
        decide(&store, 1, 2, &invite(EventChange::Agreed).decision());
        let g = listed(&store, 1);
        assert_eq!(g.len(), 1);
        assert_eq!(roles(&g[0]).len(), 2, "joined by thread and date");
        assert_eq!(g[0].commitment, EventCommitment::Invited);
        decide(&store, 1, 3, &invite(EventChange::Agreed).decision());
        assert_eq!(listed(&store, 1)[0].commitment, EventCommitment::Committed);
    }

    /// Storm: one mail carrying five ghosts' refs is assigned to one of them.
    /// Nothing merges, nothing is deleted, the other four do not change.
    #[test]
    fn one_mail_carrying_five_ghosts_refs_changes_only_one() {
        let store = fixture();
        let refs = ["AAA111", "BBB222", "CCC333", "DDD444", "EEE555"];
        let starts = [
            "2026-10-03T20:00:00-04:00",
            "2026-10-04T20:00:00-04:00",
            "2026-10-05T20:00:00-04:00",
            "2026-10-06T20:00:00-04:00",
            "2026-10-07T20:00:00-04:00",
        ];
        for (i, (r, start)) in refs.iter().zip(starts).enumerate() {
            let id = i as i64 + 1;
            mail(&store, 1, id, &format!("t{id}"), 100 - id);
            decide(
                &store,
                1,
                id,
                &Ev {
                    title: &format!("Show {id}"),
                    refs: std::slice::from_ref(r),
                    start: Some(start),
                    ..Default::default()
                }
                .decision(),
            );
        }
        let before = listed(&store, 1);
        assert_eq!(before.len(), 5);
        mail(&store, 1, 6, "t6", 10);
        decide(
            &store,
            1,
            6,
            &Ev {
                title: "Everything is cancelled",
                refs: &refs,
                start: None,
                change: EventChange::Cancelled,
                ..Default::default()
            }
            .decision(),
        );
        let after = listed(&store, 1);
        assert_eq!(after.len(), 5, "nothing merged or deleted");
        let touched: Vec<&GhostEvent> = after
            .iter()
            .filter(|g| g.sources.iter().any(|s| s.message_id == 6))
            .collect();
        assert_eq!(touched.len(), 1, "one ghost took the record");
        for (b, a) in before.iter().zip(&after) {
            assert_eq!(b.id, a.id);
            if a.id != touched[0].id {
                assert_eq!(
                    (&a.title, a.start, a.mail_state, roles(a)),
                    (&b.title, b.start, b.mail_state, roles(b))
                );
            }
        }
        assert_eq!(touched[0].mail_state, GhostMailState::Cancelled);
    }

    /// A flight number repeats across dates: same ref, a month apart, is two
    /// ghosts, and an update carrying it moves the one nearest its start.
    #[test]
    fn a_low_entropy_ref_on_two_dates_is_two_ghosts_and_an_update_moves_the_right_one() {
        let store = fixture();
        let airline = "noreply@united.example";
        mail_from(&store, 1, 1, "t1", 90, airline, Some(true));
        mail_from(&store, 1, 2, "t2", 60, airline, Some(true));
        mail_from(&store, 1, 3, "t3", 30, airline, Some(true));
        let flight = |start, change| Ev {
            title: "UA 1234 to SFO",
            refs: &["UA1234"],
            start: Some(start),
            change,
            kind: EventRecordKind::Travel,
            ..Default::default()
        };
        decide(
            &store,
            1,
            1,
            &flight("2026-10-03T08:00:00-04:00", EventChange::Created).decision(),
        );
        decide(
            &store,
            1,
            2,
            &flight("2026-11-03T08:00:00-05:00", EventChange::Created).decision(),
        );
        let ghosts = listed(&store, 1);
        assert_eq!(ghosts.len(), 2);
        let (october, november) = (ghosts[0].id, ghosts[1].id);
        decide(
            &store,
            1,
            3,
            &flight("2026-11-03T09:30:00-05:00", EventChange::Updated).decision(),
        );
        assert_eq!(
            ghost_by(&store, november).start,
            at("2026-11-03T09:30:00-05:00")
        );
        assert_eq!(
            ghost_by(&store, october).start,
            at("2026-10-03T08:00:00-04:00")
        );
        assert_eq!(
            roles(&ghost_by(&store, october)),
            vec![(1, EventChange::Created)]
        );
    }

    /// RR7. One source goes spam, the other is re-decided away, then the
    /// first is un-spammed. The ghost waits, hidden, and comes back with the
    /// user's mark.
    #[test]
    fn a_ghost_survives_its_sources_going_spam_and_away_and_keeps_its_mark() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        mail(&store, 1, 2, "t2", 30);
        decide(&store, 1, 1, &Ev::default().decision());
        decide(
            &store,
            1,
            2,
            &Ev {
                change: EventChange::Updated,
                title: "Show (late set)",
                ..Default::default()
            }
            .decision(),
        );
        let id = listed(&store, 1)[0].id;
        store
            .set_ghost_user_state(1, id, Some(GhostUserState::Going), Utc::now())
            .unwrap();
        store
            .lock()
            .unwrap()
            .execute("UPDATE messages SET is_spam=1 WHERE id=1", [])
            .unwrap();
        decide(&store, 1, 2, &MessageDecision::default());
        assert!(listed(&store, 1).is_empty(), "no eligible supplier");
        assert_eq!(count(&store, "ghost_events"), 1, "but not deleted");
        // The not-spam action rebuilds what the message feeds (A6).
        assert!(store.clear_spam(1, 1).unwrap());
        let g = listed(&store, 1).remove(0);
        assert_eq!(g.id, id);
        assert_eq!(g.title, "Show", "facts from the un-spammed source");
        assert_eq!(g.user_state, Some(GhostUserState::Going));
        assert_eq!(roles(&g), vec![(1, EventChange::Created)]);
    }

    /// A5 and A6. The supplier is the newer source. Restricted by a write that
    /// rebuilds nothing, the ghost is not listed; restricted by the access
    /// job, it is rebuilt at once from the other source.
    #[test]
    fn a_ghost_whose_supplier_is_restricted_is_hidden_until_rebuilt_from_its_other_source() {
        use crate::store::agent_triage::{AgentCommitOutcome, AgentTriageStore};
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        mail(&store, 1, 2, "t2", 30);
        decide(&store, 1, 1, &Ev::default().decision());
        decide(
            &store,
            1,
            2,
            &Ev {
                title: "Show (moved)",
                change: EventChange::Updated,
                start: Some("2026-10-04T20:00:00-04:00"),
                ..Default::default()
            }
            .decision(),
        );
        let g = listed(&store, 1).remove(0);
        assert_eq!(g.supplier_message_id, Some(2));
        let set = |sql: &str| store.lock().unwrap().execute(sql, []).unwrap();
        set("UPDATE agent_message_state SET access='restricted' WHERE message_id=2");
        assert!(
            listed(&store, 1).is_empty(),
            "facts from a restricted message are never served"
        );
        set("UPDATE agent_message_state SET access='allowed' WHERE message_id=2");
        assert_eq!(listed(&store, 1).len(), 1);

        store
            .enqueue_agent_triage(1, 2, "source_access", false)
            .unwrap();
        let job = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert_eq!((job.message_id, job.kind.as_str()), (2, "access"));
        let original = store.load_agent_access_message(&job).unwrap();
        let restrict = crate::triage::access::AccessDecision {
            restricted: true,
            reason: "A sign-in code".into(),
        };
        assert_eq!(
            store
                .commit_agent_access(&job, &original, &restrict, &serde_json::json!({}))
                .unwrap(),
            AgentCommitOutcome::Applied
        );
        let g = listed(&store, 1).remove(0);
        assert_eq!(g.supplier_message_id, Some(1));
        assert_eq!(
            (g.title.as_str(), g.start),
            ("Show", at("2026-10-03T20:00:00-04:00"))
        );
        assert_eq!(roles(&g), vec![(1, EventChange::Created)]);

        // The user's own restriction of the remaining source hides it at once
        // too, and lifting it brings it back.
        store
            .correct_agent_triage(
                1,
                1,
                "external_access",
                &serde_json::json!(true),
                Utc::now(),
            )
            .unwrap();
        assert!(listed(&store, 1).is_empty());
        store
            .correct_agent_triage(
                1,
                1,
                "external_access",
                &serde_json::json!(false),
                Utc::now(),
            )
            .unwrap();
        assert_eq!(listed(&store, 1)[0].supplier_message_id, Some(1));
    }

    /// RR5. An all-day ghost on the Sunday DST ends: two adjacent local weeks
    /// whose bounds carry different offsets list it once between them.
    #[test]
    fn a_date_only_ghost_lists_in_one_week_across_a_dst_change() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        decide(
            &store,
            1,
            1,
            &Ev {
                refs: &[],
                title: "Fair",
                start: Some("2026-11-08"),
                ..Default::default()
            }
            .decision(),
        );
        let at_ = |s| DateTime::parse_from_rfc3339(s).unwrap();
        let week = |from, to| {
            store
                .list_ghost_events(1, at_(from), at_(to))
                .unwrap()
                .len()
        };
        assert_eq!(
            week("2026-11-01T00:00:00-04:00", "2026-11-08T00:00:00-05:00"),
            0
        );
        assert_eq!(
            week("2026-11-08T00:00:00-05:00", "2026-11-15T00:00:00-05:00"),
            1
        );
    }

    /// RR10. A western caller's all-day Saturday sorts after their Friday
    /// evening, even though Friday 9pm New York is Saturday in UTC.
    #[test]
    fn an_all_day_ghost_sorts_after_the_previous_evening_for_the_caller() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        mail(&store, 1, 2, "t2", 30);
        decide(
            &store,
            1,
            1,
            &Ev {
                title: "Fri dinner",
                refs: &[],
                start: Some("2026-10-03T21:00:00-04:00"),
                ..Default::default()
            }
            .decision(),
        );
        decide(
            &store,
            1,
            2,
            &Ev {
                title: "Sat fair",
                refs: &[],
                start: Some("2026-10-04"),
                ..Default::default()
            }
            .decision(),
        );
        let at_ = |s| DateTime::parse_from_rfc3339(s).unwrap();
        let order: Vec<String> = store
            .list_ghost_events(
                1,
                at_("2026-10-03T00:00:00-04:00"),
                at_("2026-10-05T00:00:00-04:00"),
            )
            .unwrap()
            .into_iter()
            .map(|g| g.title)
            .collect();
        assert_eq!(order, ["Fri dinner", "Sat fair"]);
    }

    /// RR6. The user's end is a span: mail that moves the start a day
    /// earlier moves the end with it, and a change of precision drops it.
    #[test]
    fn a_user_end_follows_a_moved_start() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 90);
        mail(&store, 1, 2, "t2", 60);
        mail(&store, 1, 3, "t3", 30);
        decide(&store, 1, 1, &Ev::default().decision());
        let id = listed(&store, 1)[0].id;
        store
            .set_ghost_user_end(1, id, Some(at("2026-10-03T23:00:00-04:00")), Utc::now())
            .unwrap();
        decide(
            &store,
            1,
            2,
            &Ev {
                change: EventChange::Updated,
                start: Some("2026-10-02T20:00:00-04:00"),
                ..Default::default()
            }
            .decision(),
        );
        let g = ghost_by(&store, id);
        let end = g.effective_end().unwrap();
        assert_eq!(end.source, crate::triage::ghost::EndSource::User);
        assert_eq!(end.end, at("2026-10-02T23:00:00-04:00"));
        assert_eq!(end.minutes, Some(180));
        // Now all-day: a span in minutes means nothing for it.
        decide(
            &store,
            1,
            3,
            &Ev {
                change: EventChange::Updated,
                start: Some("2026-10-02"),
                ..Default::default()
            }
            .decision(),
        );
        assert_eq!(ghost_by(&store, id).user_end, None);
    }

    /// Mutation M6 survived the old suite. Rebuild itself must drop a source
    /// row whose message's decision no longer carries a record at that
    /// position (reconcile normally detaches it first, which hid the gap),
    /// and delete a ghost left with no rows at all.
    #[test]
    fn rebuild_drops_a_record_its_message_no_longer_carries() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        mail(&store, 1, 2, "t2", 30);
        decide(&store, 1, 1, &Ev::default().decision());
        decide(
            &store,
            1,
            2,
            &Ev {
                title: "Show (moved)",
                change: EventChange::Updated,
                ..Default::default()
            }
            .decision(),
        );
        let id = listed(&store, 1)[0].id;
        assert_eq!(source_rows(&store, id), vec![(1, 0), (2, 0)]);
        let empty = serde_json::to_string(&MessageDecision::default()).unwrap();
        let blank = |message: i64| {
            store
                .lock()
                .unwrap()
                .execute(
                    "UPDATE agent_message_decisions SET decision_json=?1 WHERE message_id=?2",
                    params![empty, message],
                )
                .unwrap();
        };
        let rebuild_now = || {
            let conn = store.lock().unwrap();
            rebuild(&conn, 1, id, &Utc::now().to_rfc3339()).unwrap();
        };
        blank(2);
        rebuild_now();
        assert_eq!(source_rows(&store, id), vec![(1, 0)]);
        let g = ghost_by(&store, id);
        assert_eq!((g.title.as_str(), g.supplier_message_id), ("Show", Some(1)));
        blank(1);
        rebuild_now();
        assert_eq!(count(&store, "ghost_events"), 0);
        assert_eq!(count(&store, "ghost_event_sources"), 0);
        assert_eq!(count(&store, "ghost_event_keys"), 0);
    }

    /// One message's two records (an outbound and a return flight sharing a
    /// booking reference) are two ghosts, and stay the same two when the
    /// message is re-decided.
    #[test]
    fn two_records_of_one_message_never_share_a_ghost() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        let both = decided(vec![
            Ev {
                title: "Outbound",
                refs: &["PNR4XQ"],
                ..Default::default()
            }
            .record(),
            // The same evening: within a day of the first, so only "one
            // record, one ghost per message" keeps them apart.
            Ev {
                title: "Return",
                refs: &["PNR4XQ"],
                start: Some("2026-10-03T23:30:00-04:00"),
                ..Default::default()
            }
            .record(),
        ]);
        // First read as one event, then re-read as two: the second record
        // shares the first's ref and evening, and still gets its own ghost.
        decide(&store, 1, 1, &decided(vec![both.records[0].clone()]));
        assert_eq!(listed(&store, 1).len(), 1);
        decide(&store, 1, 1, &both);
        let first: Vec<(i64, String)> = listed(&store, 1)
            .into_iter()
            .map(|g| (g.id, g.title))
            .collect();
        assert_eq!(first.len(), 2);
        decide(&store, 1, 1, &both);
        let again: Vec<(i64, String)> = listed(&store, 1)
            .into_iter()
            .map(|g| (g.id, g.title))
            .collect();
        assert_eq!(first, again);
    }

    /// A ghost holds at most 50 source rows; the 51st replaces the oldest.
    #[test]
    fn a_ghost_keeps_at_most_fifty_sources() {
        let store = fixture();
        for id in 1..=51 {
            mail(&store, 1, id, &format!("t{id}"), 200 - id);
            decide(
                &store,
                1,
                id,
                &Ev {
                    change: if id == 1 {
                        EventChange::Created
                    } else {
                        EventChange::Updated
                    },
                    ..Default::default()
                }
                .decision(),
            );
        }
        assert_eq!(count(&store, "ghost_events"), 1);
        assert_eq!(count(&store, "ghost_event_sources"), 50);
        let g = listed(&store, 1).remove(0);
        assert!(
            g.sources.iter().all(|s| s.message_id != 1),
            "the oldest went"
        );
        assert_eq!(g.last_message_id, Some(51));
    }
}
