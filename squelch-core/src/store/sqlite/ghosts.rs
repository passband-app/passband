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
//! A GHOST BELONGS TO ONE SENDER. The record that creates a ghost sets its
//! `owner_scope` (`triage::ghost::sender_scope`: the authenticated sender's
//! organisation domain, or the full address on a domain strangers share) and
//! its `owner_addr` (that record's From address). A record joins a ghost by
//! ref only from the owner's scope (a merchant confirms from one address and
//! cancels from another, and a ref is something the sender must know), and
//! by thread only from the owner's identical address: no list of domains
//! strangers share is ever complete, and a thread is open to whoever replies.
//! So another sender quoting the user's booking number, or replying "let's
//! do 8 instead" in the same thread, makes a ghost of its own rather than an
//! edit of this one. A record from mail that did not pass authentication has
//! no scope: its ghost has a NULL owner, takes nothing from any other
//! message, and is never committed. The owner changes in ONE case: the one
//! message that alone feeds a ghost is re-decided under another scope or
//! address (its authentication result arrived or went on a re-sync), and
//! then the ghost follows it, since no other mail is affected.
//!
//! The user_* columns are the user's. The one projection write to them: a
//! rebuild that changes a start's precision clears the end span stored at the
//! other precision.
//!
//! THE PROJECTION NEVER BLOCKS TRIAGE. [`reconcile_guarded`] runs it under a
//! savepoint inside the triage commit; an error rolls back the ghosts alone,
//! logs ids and an error kind (never a title, location or subject), and the
//! commit proceeds. The one exception is an error after which SQLite has
//! already rolled back the whole transaction: that fails the commit, as it
//! would have with no ghosts at all.
//!
//! SENT, SPAM AND RESTRICTED MAIL NEVER FEEDS A GHOST. Its assigned records
//! keep their rows and their keys (so un-spamming finds the same ghost again,
//! with the user's state) but contribute no facts to a rebuild, and
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

/// A message may feed a ghost: not sent, not spam, and NOT SEALED, judged by
/// the same test the Auth page's `sealed_messages` uses (docs/SECURITY.md):
/// its access review has ALLOWED it (not `restricted`, and not still
/// `pending`, which is what a changed message is until its next review), and
/// its stored decision names no auth kind (a sign-in alert, a login code).
/// `triage.sensitivity` is NOT the test: the agent pipeline no longer writes
/// it, so a guard on it would protect nothing. A decision that does not parse
/// is not known to be harmless, and feeds nothing. The SQL form, for messages
/// `m` joined by [`ELIGIBLE_JOINS`]; [`may_feed`] is its Rust half.
pub(super) const ELIGIBLE: &str = "m.is_sent=0 AND m.is_spam=0 AND COALESCE(a.access,'pending')='allowed' \
     AND (CASE WHEN ed.decision_json IS NULL THEN 1 \
               WHEN json_valid(ed.decision_json) \
               THEN COALESCE(json_array_length(ed.decision_json,'$.auth.kinds'),0)=0 \
               ELSE 0 END)";

/// The rows [`ELIGIBLE`] reads, LEFT JOINed to messages `m`: no access state
/// is a review not yet done, and no decision row names no auth kind.
pub(super) const ELIGIBLE_JOINS: &str = "LEFT JOIN agent_message_state a ON a.account_id=m.account_id AND a.message_id=m.id \
     LEFT JOIN agent_message_decisions ed ON ed.account_id=m.account_id AND ed.message_id=m.id";

/// What a decision itself says about whether its message may feed a ghost:
/// not restricted and not auth. The rest of [`ELIGIBLE`] (sent, spam, the
/// access review) is the message's, read from the store.
pub(super) fn may_feed(decision: &MessageDecision) -> bool {
    !decision.external_access.restricted && !decision.auth.is_auth()
}

/// Source rows one ghost may hold. A new record past this replaces the oldest,
/// so one long-running thread cannot grow a ghost (and every rebuild of it)
/// without bound. Only the ghost's own sender scope can add rows, so another
/// sender cannot use the cap to evict the owner's.
const SOURCES_PER_GHOST: i64 = 50;

/// Ghosts one listing returns at most.
const LIST_CAP: i64 = 2000;

/// How far apart two starts may be and still be one event, unless the record
/// says it moved the event (`updated`).
const SAME_EVENT_SECONDS: i64 = 24 * 60 * 60;

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
pub(super) fn sort_key(time: &EventTime) -> String {
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
pub(super) fn error_kind(error: &CoreError) -> String {
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

/// Run `project` under its own savepoint, inside the caller's transaction.
///
/// A projection error is SWALLOWED while that transaction is alive: the
/// savepoint rolls back, the log line carries ids and an error kind only, and
/// the caller's write proceeds, because a ghost is worth less than the write
/// it rides on. But some errors (SQLITE_FULL, IOERR, NOMEM, a trigger's
/// `RAISE(ROLLBACK)`) can make SQLite roll back the WHOLE transaction. Then
/// the connection is back in autocommit, the caller's earlier writes are
/// gone, and anything it wrote after a swallowed error would commit alone, a
/// statement at a time. So that case is returned, and the caller fails as it
/// would have without ghosts.
fn guarded(
    conn: &Connection,
    account: AccountId,
    message: i64,
    what: &str,
    project: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let error = match conn.execute_batch("SAVEPOINT ghost_projection") {
        Err(e) => CoreError::from(e),
        Ok(()) => match project() {
            Ok(()) => match conn.execute_batch("RELEASE ghost_projection") {
                Ok(()) => return Ok(()),
                Err(e) => e.into(),
            },
            Err(e) => e,
        },
    };
    if conn.is_autocommit() {
        eprintln!(
            "squelch: ghost {what} failed for account {account} message {message} ({}) and the transaction was rolled back; the write fails",
            error_kind(&error)
        );
        return Err(error);
    }
    eprintln!(
        "squelch: ghost {what} failed for account {account} message {message} ({}); the write proceeds without it",
        error_kind(&error)
    );
    // Fails harmlessly when SAVEPOINT itself was what failed.
    let _ = conn.execute_batch("ROLLBACK TO ghost_projection; RELEASE ghost_projection");
    Ok(())
}

/// [`reconcile`] under its own savepoint, for the triage commit. Err only
/// when the transaction itself is gone (see [`guarded`]); the caller
/// propagates it.
///
/// `eligible` is false for sent, spam, restricted and auth mail (see
/// [`ELIGIBLE`]), which then feeds nothing: its assigned records keep their
/// rows and stop contributing.
pub(super) fn reconcile_guarded(
    conn: &Connection,
    account: AccountId,
    message: i64,
    eligible: bool,
    current: &MessageDecision,
) -> Result<()> {
    guarded(conn, account, message, "projection", || {
        reconcile(conn, account, message, eligible, current)
    })
}

/// Re-run [`reconcile`] for `message` from its STORED decision, under the
/// same guard, for a path that changes whether a message may feed a ghost
/// (its spam flag, its agent access) WITHOUT a triage commit.
///
/// It matches afresh rather than rebuilding the ghosts by stored position: a
/// decision that changed while the message could not feed anything describes
/// other events now, and rebuilding by position would hand its new records
/// the old ghosts, the user's marks included. With no readable stored
/// decision there is nothing to match, so the message's ghosts are only
/// rebuilt. Err only when the transaction itself is gone, as for
/// [`reconcile_guarded`].
pub(super) fn refresh_message_guarded(
    conn: &Connection,
    account: AccountId,
    message: i64,
) -> Result<()> {
    guarded(conn, account, message, "refresh", || {
        let stored: Option<String> = conn
            .query_row(
                "SELECT decision_json FROM agent_message_decisions WHERE account_id=?1 AND message_id=?2",
                params![account, message],
                |r| r.get(0),
            )
            .optional()?;
        match stored.and_then(|json| serde_json::from_str::<MessageDecision>(&json).ok()) {
            Some(decision) => reconcile(conn, account, message, may_feed(&decision), &decision),
            None => {
                let now = Utc::now().to_rfc3339();
                for id in ghosts_fed_by(conn, account, message)? {
                    rebuild(conn, account, id, &now)?;
                }
                Ok(())
            }
        }
    })
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

/// Whether `message` may feed a ghost right now, by [`ELIGIBLE`].
fn message_eligible(conn: &Connection, account: AccountId, message: i64) -> Result<bool> {
    let mut statement = conn.prepare_cached(&format!(
        "SELECT EXISTS(SELECT 1 FROM messages m {ELIGIBLE_JOINS}
                       WHERE m.account_id=?1 AND m.id=?2 AND {ELIGIBLE})"
    ))?;
    Ok(statement.query_row(params![account, message], |r| r.get(0))?)
}

/// The message being decided, as matching sees it.
struct Matching<'a> {
    message: i64,
    /// Its sender scope; None when it did not pass authentication.
    scope: Option<&'a str>,
    /// Its From address, lowercased; None exactly when `scope` is.
    addr: Option<&'a str>,
    thread: &'a str,
    received: DateTime<Utc>,
    /// The ghosts it fed before this decision.
    fed_before: &'a BTreeSet<i64>,
}

/// What matching needs to know about one existing ghost.
struct Candidate {
    id: i64,
    start: Option<EventTime>,
    live: bool,
    /// The sender scope the ghost belongs to.
    owner: Option<String>,
    /// The From address of the record that created it (or last re-owned it).
    owner_addr: Option<String>,
    /// Some source row belongs to another message.
    other_sources: bool,
    /// The message being decided fed it before this decision.
    fed_by_this: bool,
}
impl Candidate {
    /// THE OWNERSHIP RULE for refs: a record joins a ghost by ref only from
    /// the ghost's own sender scope.
    fn owned_by(&self, scope: Option<&str>) -> bool {
        self.owner.as_deref() == scope
    }
    /// THE OWNERSHIP RULE for threads: the identical address as well. A scope
    /// is a domain whenever the domain is on no list of shared ones, and two
    /// strangers at an unlisted ISP share it; anyone can reply in a thread.
    fn sent_by(&self, scope: Option<&str>, addr: Option<&str>) -> bool {
        self.owned_by(scope) && addr.is_some() && self.owner_addr.as_deref() == addr
    }
}

fn candidate(
    conn: &Connection,
    account: AccountId,
    id: i64,
    m: &Matching,
) -> Result<Option<Candidate>> {
    let Some((starts_at, mail_state, owner, owner_addr, other_sources)) = conn
        .query_row(
            "SELECT g.starts_at,g.mail_state,g.owner_scope,g.owner_addr,
                    EXISTS(SELECT 1 FROM ghost_event_sources s
                           WHERE s.account_id=g.account_id AND s.ghost_id=g.id AND s.message_id<>?3)
             FROM ghost_events g WHERE g.account_id=?1 AND g.id=?2",
            params![account, id, m.message],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, bool>(4)?,
                ))
            },
        )
        .optional()?
    else {
        return Ok(None);
    };
    Ok(Some(Candidate {
        id,
        start: starts_at.as_deref().and_then(EventTime::parse),
        live: mail_state != "cancelled",
        owner,
        owner_addr,
        other_sources,
        fed_by_this: m.fed_before.contains(&id),
    }))
}

/// Seconds between two starts, or None when either is missing.
fn distance(a: Option<&EventTime>, b: Option<&EventTime>) -> Option<i64> {
    Some((instant(a?) - instant(b?)).num_seconds().abs())
}

/// Whether a record's start and a ghost's are too far apart to be one event.
/// A missing start is no evidence either way.
fn dates_apart(record: &GhostProposal, c: &Candidate) -> bool {
    distance(record.start.as_ref(), c.start.as_ref()).is_some_and(|d| d > SAME_EVENT_SECONDS)
}

/// A candidate's place in [`rank`]'s order: smaller first, the ghost id last.
type Rank = (bool, u8, i64, i64);

/// The order among equally good candidates, smallest first: a ghost this
/// message already fed, then the nearest start, then the lowest id. A record
/// with no start (a cancellation that repeats nothing) prefers the ghost next
/// to happen after its mail arrived, then the latest before it; a ghost with
/// no start comes last.
fn rank(record: &GhostProposal, m: &Matching, c: &Candidate) -> Rank {
    let (group, gap) = match (&record.start, &c.start) {
        (Some(_), Some(_)) => (
            0,
            distance(record.start.as_ref(), c.start.as_ref()).unwrap_or(i64::MAX),
        ),
        (None, Some(start)) => {
            let ahead = (instant(start) - m.received).num_seconds();
            if ahead >= 0 { (0, ahead) } else { (1, -ahead) }
        }
        (_, None) => (2, 0),
    };
    (!c.fed_by_this, group, gap, c.id)
}

/// (a) By ref: the ghost of this sender sharing the most of the record's
/// scoped ref keys. Only an `updated` record may bridge dates (a reschedule):
/// any other more than a day from a ghost's start is not that event, since
/// flight numbers, season-ticket seats and other low-entropy refs repeat.
fn match_by_ref(
    conn: &Connection,
    account: AccountId,
    m: &Matching,
    record: &GhostProposal,
    keys: &[String],
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
    let mut best: Option<(std::cmp::Reverse<usize>, Rank)> = None;
    for (id, count) in shared {
        if claimed.contains(&id) {
            continue;
        }
        let Some(c) = candidate(conn, account, id, m)? else {
            continue;
        };
        if !c.owned_by(m.scope)
            || (record.change != EventChange::Updated && dates_apart(record, &c))
        {
            continue;
        }
        // Most shared keys first, then the rank; smaller sorts first.
        let key = (std::cmp::Reverse(count), rank(record, m, &c));
        if best.as_ref().is_none_or(|b| key < *b) {
            best = Some(key);
        }
    }
    // The rank's last field is the ghost id.
    Ok(best.map(|(_, (_, _, _, id))| id))
}

/// (b) By thread, for a record with no usable refs: a ghost created from
/// this sender's identical address ([`Candidate::sent_by`]) fed from the
/// same Gmail thread (by any of its source rows, eligible or not, so a ghost
/// hidden by a seal is still found when the seal lifts). The one starting on
/// the record's date
/// wins; failing that, a record that is not `created` takes the thread's only
/// live candidate (a ref-less "moved to 9pm" follow-up). With several live
/// candidates and none on its date, it matches nothing: a ref-less
/// cancellation then cancels nothing rather than a guess.
/// The ghosts any message of one thread feeds. See [`match_by_thread`].
/// `CROSS JOIN` is SQLite's way to fix the join order: the thread's messages
/// first, so an `IN (subquery)` (which the planner still read as a filter over
/// the account's sources) is not left to its guess.
pub(super) const THREAD_GHOSTS_SQL: &str = "SELECT DISTINCT s.ghost_id FROM messages m
     CROSS JOIN ghost_event_sources s ON s.account_id=m.account_id AND s.message_id=m.id
     WHERE m.account_id=?1 AND m.thread_id=?2";

fn match_by_thread(
    conn: &Connection,
    account: AccountId,
    m: &Matching,
    record: &GhostProposal,
    claimed: &BTreeSet<i64>,
) -> Result<Option<i64>> {
    let ids: Vec<i64> = {
        // Driven from the THREAD: its messages off a thread index, then each
        // one's source rows off the primary key. Written as a join
        // on the sources, the planner (with no ANALYZE ever run) walked every
        // source row of the account instead, inside the triage commit.
        let mut statement = conn.prepare_cached(THREAD_GHOSTS_SQL)?;
        statement
            .query_map(params![account, m.thread], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?
    };
    let mut candidates = Vec::new();
    for id in ids {
        if claimed.contains(&id) {
            continue;
        }
        if let Some(c) = candidate(conn, account, id, m)?
            && c.sent_by(m.scope, m.addr)
        {
            candidates.push(c);
        }
    }
    let same_date = candidates
        .iter()
        .filter(|c| match (&record.start, &c.start) {
            (Some(r), Some(g)) => local_date(r) == local_date(g),
            _ => false,
        })
        .min_by_key(|c| rank(record, m, c));
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

/// (c) The ghost this message fed before: what keeps a ghost's id and the
/// user's mark when a re-triage rewords a title or a ref. Only a ghost this
/// message ALONE feeds (no source row of any other message, eligible or
/// not), within a day of the record's start (or with either start missing):
/// a ghost other mail still describes is that mail's, and a record now about
/// another date is another event. The ghost at the record's own position
/// first; when that one does not qualify (claimed by another record of this
/// message, as after a reorder), the nearest-starting other one that does.
///
/// Whatever its owner: no other message feeds it, so it is this message's
/// own ghost whoever the message now speaks for, and the caller re-owns it
/// ([`reown`]). Refusing it lost the user's mark whenever a re-sync filled
/// or cleared the message's authentication result.
fn match_previous(
    conn: &Connection,
    account: AccountId,
    m: &Matching,
    record: &GhostProposal,
    at_position: Option<i64>,
    claimed: &BTreeSet<i64>,
) -> Result<Option<i64>> {
    let usable =
        |c: &Candidate| !claimed.contains(&c.id) && !c.other_sources && !dates_apart(record, c);
    if let Some(id) = at_position
        && let Some(c) = candidate(conn, account, id, m)?
        && usable(&c)
    {
        return Ok(Some(id));
    }
    let mut best: Option<(Rank, i64)> = None;
    for &id in m.fed_before {
        if Some(id) == at_position {
            continue;
        }
        let Some(c) = candidate(conn, account, id, m)? else {
            continue;
        };
        if usable(&c) {
            let key = rank(record, m, &c);
            if best.as_ref().is_none_or(|(b, _)| key < *b) {
                best = Some((key, id));
            }
        }
    }
    Ok(best.map(|(_, id)| id))
}

/// THE ONE PLACE A GHOST'S OWNER CHANGES: a ghost [`match_previous`] gave
/// back, which only the message being decided feeds, takes that message's
/// current scope and address. Nothing else can be affected (the ghost has
/// no other source), and keeping the old owner would make the rebuild
/// ignore the ghost's only record ([`rebuild`] reads each source's current
/// scope), or keep a commitment the message no longer has authority for.
fn reown(conn: &Connection, account: AccountId, id: i64, m: &Matching, now: &str) -> Result<()> {
    conn.execute(
        "UPDATE ghost_events SET owner_scope=?3,owner_addr=?4,updated_at=?5
         WHERE account_id=?1 AND id=?2
           AND (owner_scope IS NOT ?3 OR owner_addr IS NOT ?4)
           AND NOT EXISTS(SELECT 1 FROM ghost_event_sources s
                          WHERE s.account_id=?1 AND s.ghost_id=?2 AND s.message_id<>?6)",
        params![account, id, m.scope, m.addr, now, m.message],
    )?;
    Ok(())
}

/// Project one message's current decision. See the module doc for the rules;
/// in order:
///
/// 1. An ineligible message (sent, spam, restricted, sealed) keeps whatever
///    it was assigned and contributes nothing: its ghosts are rebuilt
///    without it.
/// 2. Each laundered event record is matched against the state BEFORE this
///    message's old assignment is removed, so a re-decision finds the ghosts
///    it fed: (a) by scoped ref, among ghosts of its scope, else (b) with no
///    refs, by thread, among ghosts created from its identical address, else
///    (c) a ghost this message alone fed before, whatever its owner (which
///    then becomes this record's scope and address), else (d) a new ghost
///    owned by its scope and address. An unauthenticated record skips (a)
///    and (b). A `cancelled` record that matched nothing creates nothing:
///    there is no event to cancel. No two records of one message share a
///    ghost.
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
    let records = ghost::launder_events(&current.records);
    if records.is_empty() && previous.is_empty() {
        return Ok(());
    }
    // The caller's test plus the stored one: a seal is not the caller's to
    // know about.
    if !eligible || !message_eligible(conn, account, message)? {
        for &id in &fed_before {
            rebuild(conn, account, id, &now)?;
        }
        return Ok(());
    }
    let (thread, from_addr, auth_pass, received): (String, String, Option<bool>, String) = conn
        .query_row(
            "SELECT thread_id,from_addr,auth_pass,received_at FROM messages WHERE account_id=?1 AND id=?2",
            params![account, message],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
    let scope = ghost::sender_scope(&from_addr, auth_pass);
    let addr = scope.as_ref().map(|_| from_addr.trim().to_lowercase());
    let m = Matching {
        message,
        scope: scope.as_deref(),
        addr: addr.as_deref(),
        thread: &thread,
        received: DateTime::parse_from_rfc3339(&received)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        fed_before: &fed_before,
    };

    let mut claimed: BTreeSet<i64> = BTreeSet::new();
    let mut assigned: Vec<(usize, i64, String)> = Vec::new();
    for (position, record) in records.iter().enumerate() {
        let mut target = None;
        if m.scope.is_some() {
            let keys = ghost::identity_keys(record, m.scope);
            target = if keys.is_empty() {
                match_by_thread(conn, account, &m, record, &claimed)?
            } else {
                match_by_ref(conn, account, &m, record, &keys, &claimed)?
            };
        }
        if target.is_none() {
            let at_position = previous.get(&position).copied();
            target = match_previous(conn, account, &m, record, at_position, &claimed)?;
            if let Some(id) = target {
                reown(conn, account, id, &m, &now)?;
            }
        }
        let id = match target {
            Some(id) => id,
            None if record.change == EventChange::Cancelled => continue,
            None => {
                // A placeholder the rebuild below fills in. Its owner is
                // set here, and changes only through `reown`.
                conn.execute(
                    "INSERT INTO ghost_events(account_id,kind,title,commitment,owner_scope,owner_addr,created_at,updated_at)
                     VALUES(?1,?2,?3,'unknown',?4,?5,?6,?6)",
                    params![account, label(&record.kind), record.title, m.scope, m.addr, now],
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
            "INSERT INTO ghost_event_sources(account_id,ghost_id,message_id,position,role)
             VALUES(?1,?2,?3,?4,?5)",
            params![account, id, message, *position as i64, role],
        )?;
    }
    let affected: BTreeSet<i64> = fed_before
        .iter()
        .copied()
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
    proposal: GhostProposal,
}

/// What a source message yields for a rebuild.
enum Stored {
    /// Its laundered event records, in position order, and whether they may
    /// supply facts right now: false for a message that is spam, sent,
    /// restricted (by its access state or by the decision itself) or sealed.
    ///
    /// `in_scope` is false when its message's CURRENT sender scope (read
    /// now, from its `from_addr` and `auth_pass`) is not the ghost's owner:
    /// it was assigned under a scope it no longer has, so its rows stay and
    /// contribute nothing (no facts, no keys, no commitment) until the
    /// message is decided again.
    Events {
        events: Vec<GhostProposal>,
        eligible: bool,
        in_scope: bool,
    },
    /// It has no decision row. Eligible, its rows are stale; ineligible, they
    /// wait like any other ineligible row.
    Undecided { eligible: bool },
    /// Its decision does not deserialize. Not known to have stopped feeding
    /// the ghost, so its rows stay and one bad row rewrites nothing.
    Unreadable,
}

/// Rebuild one ghost from exactly its assigned records, loading only those
/// messages' decisions.
///
/// - A row whose message is unreadable, or whose message's current scope is
///   not the owner's, stays and contributes nothing: no facts, no keys, no
///   commitment. A row whose eligible message's decision no longer has a
///   record at that position (or has no decision) is stale and goes; an
///   ineligible message's rows wait for its next reconcile.
/// - Keys are the union of the in-scope readable records' refs under the
///   owner scope, ELIGIBLE OR NOT: keys are never served, and a record that
///   comes back from a seal or a restriction must find its own ghost by ref
///   even when every other source is still sealed.
/// - No rows left: the ghost is deleted. Rows but no eligible record: the
///   facts stay and `supplier_message_id` is cleared, so the listing hides
///   the ghost until a source is eligible again.
/// - Otherwise facts come from the newest eligible record that does not
///   cancel (or the newest, when all do), `mail_state` from the newest.
///   Every such record has the owner's scope now, so commitment is
///   `committed` when any is committed or agreed and the owner is
///   authenticated (a NULL-owner ghost is `unknown` whatever its mail claims).
pub(super) fn rebuild(conn: &Connection, account: AccountId, id: i64, now: &str) -> Result<()> {
    let Some(owner) = conn
        .query_row(
            "SELECT owner_scope FROM ghost_events WHERE account_id=?1 AND id=?2",
            params![account, id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
    else {
        return Ok(());
    };
    type SourceRow = (i64, i64, String, bool, Option<String>, String, Option<bool>);
    let rows: Vec<SourceRow> = {
        let mut statement = conn.prepare_cached(&format!(
            "SELECT s.message_id,s.position,m.received_at,{ELIGIBLE},d.decision_json,
                    m.from_addr,m.auth_pass
             FROM ghost_event_sources s
             JOIN messages m ON m.account_id=s.account_id AND m.id=s.message_id
             {ELIGIBLE_JOINS}
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
                    r.get(6)?,
                ))
            })?
            .collect::<std::result::Result<_, _>>()?
    };
    let mut stored: BTreeMap<i64, Stored> = BTreeMap::new();
    let mut feds: Vec<Fed> = Vec::new();
    let mut keyed: Vec<GhostProposal> = Vec::new();
    let mut stale: Vec<(i64, i64)> = Vec::new();
    let total = rows.len();
    for (message, position, received, eligible, encoded, from_addr, auth_pass) in rows {
        let entry = stored.entry(message).or_insert_with(|| {
            let Some(encoded) = encoded else {
                return Stored::Undecided { eligible };
            };
            match serde_json::from_str::<MessageDecision>(&encoded) {
                Ok(decision) => Stored::Events {
                    events: ghost::launder_events(&decision.records),
                    eligible: eligible && may_feed(&decision),
                    in_scope: ghost::sender_scope(&from_addr, auth_pass) == owner,
                },
                Err(_) => {
                    eprintln!(
                        "squelch: ghost rebuild skipped an unreadable decision (account {account} message {message})"
                    );
                    Stored::Unreadable
                }
            }
        });
        let (events, eligible, in_scope) = match entry {
            Stored::Events {
                events,
                eligible,
                in_scope,
            } => (events.as_slice(), *eligible, *in_scope),
            Stored::Undecided { eligible: true } => (&[][..], true, false),
            Stored::Undecided { eligible: false } | Stored::Unreadable => continue,
        };
        let Some(proposal) = usize::try_from(position).ok().and_then(|p| events.get(p)) else {
            if eligible {
                stale.push((message, position));
            }
            continue;
        };
        if !in_scope {
            continue;
        }
        keyed.push(proposal.clone());
        if eligible {
            feds.push(Fed {
                message,
                received: DateTime::parse_from_rfc3339(&received)
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or(DateTime::<Utc>::MIN_UTC),
                position: position as usize,
                proposal: proposal.clone(),
            });
        }
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
    let keys: BTreeSet<String> = keyed
        .iter()
        .flat_map(|p| ghost::identity_keys(p, owner.as_deref()))
        .collect();
    for key in keys {
        conn.execute(
            "INSERT INTO ghost_event_keys(account_id,ghost_id,key) VALUES(?1,?2,?3)",
            params![account, id, key],
        )?;
    }
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
    let commitment = if owner.is_none() {
        // Unauthenticated mail's word is no evidence of anything.
        EventCommitment::Unknown
    } else if feds.iter().any(|f| {
        f.proposal.commitment == EventCommitment::Committed
            || f.proposal.change == EventChange::Agreed
    }) {
        EventCommitment::Committed
    } else {
        newest.proposal.commitment
    };
    let p = &supplier.proposal;
    // THE ONE PROJECTION WRITE TO A USER COLUMN: a start that changed
    // precision clears the user's end span at the other precision, so a
    // correction cannot reappear when a later mail changes it back.
    let (all_day, timed) = match p.start {
        Some(EventTime::Date(_)) => (true, false),
        Some(EventTime::At(_)) => (false, true),
        None => (false, false),
    };
    conn.execute(
        "UPDATE ghost_events SET kind=?3,title=?4,starts_at=?5,start_sort=?6,start_timezone=?7,
             ends_at=?8,est_minutes=?9,est_basis=?10,location=?11,venue_key=?12,commitment=?13,
             mail_state=?14,likelihood_band=?15,likelihood_reason=?16,supplier_message_id=?17,
             last_message_id=?18,updated_at=?19,
             user_end_minutes=CASE WHEN ?20 THEN NULL ELSE user_end_minutes END,
             user_end_days=CASE WHEN ?21 THEN NULL ELSE user_end_days END
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
            all_day,
            timed,
        ],
    )?;
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
pub(super) fn in_range(
    start: &EventTime,
    from: DateTime<FixedOffset>,
    to: DateTime<FixedOffset>,
) -> bool {
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
pub(super) fn listing_order(start: &EventTime, offset: &FixedOffset) -> DateTime<Utc> {
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
/// as it is NOW. A rebuild clears a span at the other precision, so one here
/// is only ever a span no rebuild has seen yet, and it is ignored.
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
                    g.created_at,g.updated_at,g.owner_scope
             FROM ghost_events g
             LEFT JOIN event_duration_priors p
               ON p.account_id=g.account_id AND p.venue_key=g.venue_key AND p.kind=g.kind
             WHERE g.account_id=?1 AND g.start_sort IS NOT NULL
               AND g.start_sort>=?2 AND g.start_sort<=?3
               AND EXISTS(SELECT 1 FROM messages m {ELIGIBLE_JOINS}
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
                        owner_scope: r.get(22)?,
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
             {ELIGIBLE_JOINS}
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
            // Only eligible sources are served: a restricted or sealed
            // message's id is not the human door's to point at from here.
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

    /// The user's PATCH of a ghost, in ONE transaction. Each of `end` and
    /// `state` is `None` to leave alone, `Some(None)` to clear, `Some(Some(_))`
    /// to set. False (nothing written) for an unknown id or another account's;
    /// `InvalidInput` (nothing written, the state included) for an end that
    /// is not after the start at its precision, or a ghost with no start.
    ///
    /// The end arrives absolute and is stored as a span from the CURRENT
    /// start: minutes for a timed ghost, days for an all-day one. A timed
    /// span also becomes the duration prior for the ghost's venue and kind
    /// (never for a kind with no default duration, travel), remembering which
    /// ghost taught it; clearing the end takes that prior back while it
    /// still holds this ghost's number, so "undo" falls back to the estimate
    /// rather than to the user's own correction relabelled as history.
    pub(super) fn patch_ghost_user(
        &self,
        account: AccountId,
        id: i64,
        end: Option<Option<EventTime>>,
        state: Option<Option<GhostUserState>>,
        at: DateTime<Utc>,
    ) -> Result<bool> {
        let mut conn = self.lock()?;
        // Dropped uncommitted on any error below, so a refused end takes the
        // state written before it with it.
        let tx = conn.transaction()?;
        let Some((starts_at, location, kind, old_minutes)) = tx
            .query_row(
                "SELECT starts_at,location,kind,user_end_minutes FROM ghost_events
                 WHERE account_id=?1 AND id=?2",
                params![account, id],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<i64>>(3)?,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(false);
        };
        // The prior a span teaches, as it is stored: None for a kind with no
        // default (one long flight from an airport says nothing about the
        // next) or a ghost with no venue.
        let venue = location
            .as_deref()
            .and_then(ghost::venue_key)
            .filter(|_| ghost::default_minutes(unlabel::<EventRecordKind>(&kind)).is_some());
        let prior_minutes = |minutes: i64| {
            u32::try_from(minutes)
                .unwrap_or(u32::MAX)
                .clamp(*ESTIMATE_MINUTES.start(), *ESTIMATE_MINUTES.end())
        };
        if let Some(state) = state {
            tx.execute(
                "UPDATE ghost_events SET user_state=?3,user_state_at=?4 WHERE account_id=?1 AND id=?2",
                params![
                    account,
                    id,
                    state.map(GhostUserState::as_str),
                    state.map(|_| at.to_rfc3339())
                ],
            )?;
        }
        if let Some(end) = end {
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
                        if let Some(venue) = &venue {
                            tx.execute(
                                "INSERT INTO event_duration_priors(account_id,venue_key,kind,minutes,ghost_id,updated_at)
                                 VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(account_id,venue_key,kind) DO UPDATE SET
                                 minutes=excluded.minutes,ghost_id=excluded.ghost_id,updated_at=excluded.updated_at",
                                params![account, venue, kind, prior_minutes(minutes), id, at.to_rfc3339()],
                            )?;
                        }
                    }
                    (EventTime::Date(s), EventTime::Date(e)) => {
                        span.1 = Some((*e - s).num_days());
                    }
                    // Refused above: `is_after` is false across precisions.
                    _ => {}
                }
            } else if let Some(old) = old_minutes {
                // Whatever venue or kind it was taught under: the ghost and
                // the number say it is this correction's, and a later
                // correction elsewhere has replaced both.
                tx.execute(
                    "DELETE FROM event_duration_priors WHERE account_id=?1 AND ghost_id=?2 AND minutes=?3",
                    params![account, id, prior_minutes(old)],
                )?;
            }
            tx.execute(
                "UPDATE ghost_events SET user_end_minutes=?3,user_end_days=?4 WHERE account_id=?1 AND id=?2",
                params![account, id, span.0, span.1],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// The state half of [`Self::patch_ghost_user`], for tests.
    #[cfg(test)]
    pub(super) fn set_ghost_user_state(
        &self,
        account: AccountId,
        id: i64,
        state: Option<GhostUserState>,
        at: DateTime<Utc>,
    ) -> Result<bool> {
        self.patch_ghost_user(account, id, None, Some(state), at)
    }

    /// The end half of [`Self::patch_ghost_user`], for tests.
    #[cfg(test)]
    pub(super) fn set_ghost_user_end(
        &self,
        account: AccountId,
        id: i64,
        end: Option<EventTime>,
        at: DateTime<Utc>,
    ) -> Result<bool> {
        self.patch_ghost_user(account, id, Some(end), None, at)
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

    /// THE THREAD MATCH IS DRIVEN FROM THE THREAD, with no ANALYZE (the
    /// daemon never runs one): the thread's messages off a thread index,
    /// their source rows off the primary key, and
    /// never a walk of the account's sources by `idx_ghost_event_sources_ghost`.
    #[test]
    fn the_thread_match_reads_the_thread_index_not_every_source() {
        let store = fixture();
        let conn = store.lock().unwrap();
        let plan: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {THREAD_GHOSTS_SQL}"))
            .unwrap()
            .query_map(params![1, "t"], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        let plan = plan.join(" | ");
        eprintln!("thread ghosts plan: {plan}");
        // Messages first, by a (account_id, thread_id) index...
        assert!(
            plan.starts_with("SEARCH m USING COVERING INDEX idx_messages_thread"),
            "{plan}"
        );
        // ...then each message's sources off the primary key.
        assert!(
            plan.contains(
                "SEARCH s USING INDEX sqlite_autoindex_ghost_event_sources_1 (account_id=? AND message_id=?)"
            ),
            "{plan}"
        );
        assert!(!plan.contains("idx_ghost_event_sources_ghost"), "{plan}");
        assert!(!plan.contains("SCAN"), "{plan}");
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
        let eligible = !sent && !spam && may_feed(decision);
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
            tx.execute(
                "INSERT INTO agent_message_state(account_id,message_id,content_snapshot,access)
                 VALUES(1,1,'','allowed')",
                [],
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
        // keeps its assignment and its key (lifting the restriction finds the
        // ghost again, with the user's state) but supplies nothing. Round 3
        // (N1) changed the key count here from 0: keys are never served.
        commit(&store, 4, "record_retraction", &restricted);
        assert!(listed(&store, 1).is_empty());
        assert_eq!(count(&store, "ghost_event_sources"), 1);
        assert_eq!(count(&store, "ghost_event_keys"), 1, "its key stays");
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
        // Same ref, same thread id, other account: its own ghost. (An update
        // rather than a cancellation, which matching nothing makes nothing.)
        decide(
            &store,
            2,
            2,
            &Ev {
                title: "Moved",
                change: EventChange::Updated,
                ..Default::default()
            }
            .decision(),
        );
        let a = listed(&store, 1);
        let b = listed(&store, 2);
        assert_eq!((a.len(), b.len()), (1, 1));
        assert_ne!(a[0].id, b[0].id);
        assert_eq!(a[0].title, "Show", "B's update is B's");
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

        // Clearing the end takes back what that correction taught (F4): the
        // cleared ghost falls back to the model's estimate, not to its own
        // number relabelled "history".
        store
            .set_ghost_user_end(1, first.id, None, Utc::now())
            .unwrap();
        assert_eq!(by_message_after(&store, 2).history_minutes, None);
        let cleared = by_message_after(&store, 1);
        assert_eq!(cleared.user_end, None);
        assert_eq!(
            cleared.effective_end().map(|e| e.source),
            Some(crate::triage::ghost::EndSource::Model)
        );
        assert_eq!(count(&store, "event_duration_priors"), 0);
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
        // Its facts are still the deleted supplier's, and no SQL can rebuild
        // them, so it is hidden rather than served under message 1's name
        // (round 2, A9) until a reconcile of a message that feeds it.
        assert!(listed(&store, 1).is_empty());
        let supplier: Option<i64> = store
            .lock()
            .unwrap()
            .query_row("SELECT supplier_message_id FROM ghost_events", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(supplier, None);
        decide(&store, 1, 1, &Ev::default().decision());
        let g = listed(&store, 1).remove(0);
        assert_eq!(roles(&g), vec![(1, EventChange::Created)]);
        assert_eq!(g.supplier_message_id, Some(1));
        assert_eq!(
            g.start,
            at("2026-10-03T20:00:00-04:00"),
            "message 1's facts"
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

    /// RR1 on ONE day. Google's invite URL keeps its id in the query, so with
    /// the query stripped both invites shared one generic route, and the date
    /// test that kept RR1 apart cannot help on the same day. The route is
    /// dropped as a ref (fix pass B2), so neither has a key and each stays in
    /// its own thread.
    #[test]
    fn two_calendar_invites_on_the_same_day_in_different_threads_stay_two_ghosts() {
        let store = fixture();
        let google = "calendar-notification@google.com";
        mail_from(&store, 1, 1, "t1", 60, google, Some(true));
        mail_from(&store, 1, 2, "t2", 30, google, Some(true));
        for (id, title, start, eid) in [
            (
                1,
                "Standup with Ana",
                "2026-10-20T09:00:00-04:00",
                "AAAA111",
            ),
            (2, "Dentist", "2026-10-20T15:00:00-04:00", "BBBB222"),
        ] {
            let link = format!("https://calendar.google.com/calendar/event?action=VIEW&eid={eid}");
            decide(
                &store,
                1,
                id,
                &Ev {
                    title,
                    start: Some(start),
                    refs: &[link.as_str()],
                    kind: EventRecordKind::Invite,
                    ..Default::default()
                }
                .decision(),
            );
        }
        let ghosts = listed(&store, 1);
        let mut titles: Vec<&str> = ghosts.iter().map(|g| g.title.as_str()).collect();
        titles.sort();
        assert_eq!(titles, ["Dentist", "Standup with Ana"]);
        assert_eq!(count(&store, "ghost_event_keys"), 0, "the route is no key");
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
        // The spoofed cancellation matched nothing, so it made nothing; the
        // spoofed update made its own ghost.
        assert_eq!(count(&store, "ghost_events"), 2);
        // The spoof holds no key a later message could find it by.
        assert_eq!(count(&store, "ghost_event_keys"), 1);
        // A re-decision of a spoof keeps its own ghost.
        let spoof = listed(&store, 1)
            .into_iter()
            .find(|g| g.last_message_id == Some(3))
            .unwrap()
            .id;
        assert!(!ghost_by(&store, spoof).verified());
        assert!(ghost_by(&store, victim).verified());
        decide(
            &store,
            1,
            3,
            &Ev {
                title: "Reworded",
                refs: &["OTHER999"],
                start: Some("2026-10-03T23:00:00-04:00"),
                ..Default::default()
            }
            .decision(),
        );
        assert_eq!(ghost_by(&store, spoof).title, "Reworded");
        assert_eq!(count(&store, "ghost_events"), 2);
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
        assert_eq!(count(&store, "ghost_events"), 4);
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

    /// Another sender's ref-less mail in the thread never joins the ghost
    /// (round 2, 1.1: a ghost belongs to one sender scope), so its `agreed`
    /// cannot commit the user to the host's event. It makes a ghost of its
    /// own instead; only the host's own agreement commits the host's ghost.
    #[test]
    fn agreement_from_another_sender_makes_its_own_ghost_and_does_not_commit_the_hosts() {
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
        let host = listed(&store, 1)[0].id;
        decide(&store, 1, 2, &invite(EventChange::Agreed).decision());
        assert_eq!(listed(&store, 1).len(), 2, "the friend's is its own ghost");
        let g = ghost_by(&store, host);
        assert_eq!(roles(&g), vec![(1, EventChange::Created)]);
        assert_eq!(g.commitment, EventCommitment::Invited);
        // The host's own agreement, same thread and date, joins and commits.
        decide(&store, 1, 3, &invite(EventChange::Agreed).decision());
        let g = ghost_by(&store, host);
        assert_eq!(
            roles(&g),
            vec![(1, EventChange::Created), (3, EventChange::Agreed)]
        );
        assert_eq!(g.commitment, EventCommitment::Committed);
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
        // The not-spam action rebuilds what the message feeds (A6). Its
        // content snapshot changed (the spam flag is in it), so its access
        // review is pending again and the ghost waits for the re-review the
        // action queued, which lands here as a decision.
        assert!(store.clear_spam(1, 1).unwrap());
        assert!(listed(&store, 1).is_empty(), "pending until reviewed");
        decide(&store, 1, 1, &Ev::default().decision());
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

    // ---- The guard (fix pass B1) ----

    /// A record per ghost, with titles and locations at their cap, so the
    /// projection writes many pages of its own.
    fn heavy_decision() -> MessageDecision {
        let titles: Vec<String> = (0..40)
            .map(|i| format!("{i} {}", "t".repeat(190)))
            .collect();
        let refs: Vec<[String; 1]> = (0..40).map(|i| [format!("REF{i:05}")]).collect();
        let refs: Vec<Vec<&str>> = refs.iter().map(|r| vec![r[0].as_str()]).collect();
        let location = "L".repeat(199);
        decided(
            titles
                .iter()
                .zip(&refs)
                .map(|(title, refs)| {
                    Ev {
                        title,
                        refs,
                        location: Some(&location),
                        ..Default::default()
                    }
                    .record()
                })
                .collect(),
        )
    }

    /// Claim message 1's job, cap the database `headroom` pages above its
    /// current size, and commit `decision` through the real triage commit.
    fn commit_capped(
        sent: bool,
        headroom: i64,
        decision: &MessageDecision,
    ) -> (
        SqliteStore,
        crate::store::agent_triage::AgentJob,
        crate::store::agent_triage::AgentContext,
        Result<crate::store::agent_triage::AgentCommitOutcome>,
    ) {
        use crate::store::agent_triage::AgentTriageStore;
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        if sent {
            set_flag(&store, "UPDATE messages SET is_sent=1 WHERE id=1");
        }
        store.enqueue_agent_triage(1, 1, "arrival", false).unwrap();
        let job = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        let context = store.load_agent_context(&job).unwrap();
        {
            let conn = store.lock().unwrap();
            let pages: i64 = conn
                .query_row("PRAGMA page_count", [], |r| r.get(0))
                .unwrap();
            let _: i64 = conn
                .query_row(
                    &format!("PRAGMA max_page_count={}", pages + headroom),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
        }
        let outcome = store.commit_agent_decision(
            &job,
            &context,
            decision,
            std::slice::from_ref(&context.message.source),
        );
        (store, job, context, outcome)
    }
    fn set_flag(store: &SqliteStore, sql: &str) {
        store.lock().unwrap().execute(sql, []).unwrap();
    }
    fn job_state(store: &SqliteStore) -> String {
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT state FROM agent_triage_jobs WHERE message_id=1 AND kind='triage'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// INVARIANT 5's exception, forced honestly. A write past
    /// `max_page_count` fails with SQLITE_FULL, and SQLite answers that by
    /// rolling back the WHOLE transaction (observed here: the connection is
    /// back in autocommit), not just the statement. The guard must then fail
    /// the commit: swallowing it would let the job's `completed` mark commit
    /// on its own in autocommit, with no decision behind it.
    ///
    /// The headroom is found, not hardcoded: the same commit on SENT mail
    /// (whose projection writes nothing) succeeds with less room than the
    /// real one needs, and every headroom in between overflows inside the
    /// projection.
    #[test]
    fn a_projection_that_kills_the_transaction_fails_the_commit_and_leaves_the_job() {
        use crate::store::agent_triage::{AgentCommitOutcome, AgentTriageStore};
        let decision = heavy_decision();
        let fits = |sent: bool| {
            (0..64)
                .find(|&headroom| commit_capped(sent, headroom, &decision).3.is_ok())
                .expect("some headroom fits")
        };
        let (without_ghosts, with_ghosts) = (fits(true), fits(false));
        assert!(
            without_ghosts < with_ghosts,
            "the projection needs pages of its own ({without_ghosts} vs {with_ghosts})"
        );
        for headroom in without_ghosts..with_ghosts {
            let (store, job, context, outcome) = commit_capped(false, headroom, &decision);
            match outcome {
                Err(CoreError::Sqlite(rusqlite::Error::SqliteFailure(e, _))) => {
                    assert_eq!(e.code, rusqlite::ErrorCode::DiskFull, "{headroom}")
                }
                other => panic!("headroom {headroom}: {other:?}"),
            }
            assert_eq!(
                job_state(&store),
                "leased",
                "{headroom}: the job stays open"
            );
            for table in [
                "agent_message_decisions",
                "ghost_events",
                "ghost_event_sources",
            ] {
                assert_eq!(count(&store, table), 0, "{headroom}: {table}");
            }
            // Room again: the same job, retried, commits everything.
            store
                .lock()
                .unwrap()
                .execute_batch("PRAGMA max_page_count=1073741823")
                .unwrap();
            let retried = store
                .commit_agent_decision(
                    &job,
                    &context,
                    &decision,
                    std::slice::from_ref(&context.message.source),
                )
                .unwrap();
            assert_eq!(retried, AgentCommitOutcome::Applied);
            assert_eq!(job_state(&store), "completed");
            assert_eq!(count(&store, "agent_message_decisions"), 1);
            // 40 records in the decision, the first 20 projected.
            assert_eq!(listed(&store, 1).len(), ghost::EVENTS_PER_MESSAGE);
        }
    }

    /// The guard's two branches, directly: a projection error with the
    /// transaction alive is swallowed and its writes undone; one that ended
    /// the transaction is returned.
    #[test]
    fn the_guard_returns_an_error_only_when_the_transaction_is_gone() {
        let store = fixture();
        let mut conn = store.lock().unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO accounts(id,email,created_at) VALUES(3,'c@test','x')",
            [],
        )
        .unwrap();
        let swallowed = guarded(&tx, 3, 1, "test", || {
            tx.execute(
                "INSERT INTO accounts(id,email,created_at) VALUES(4,'d@test','x')",
                [],
            )?;
            Err(CoreError::InvalidInput("forced".into()))
        });
        assert!(swallowed.is_ok());
        assert!(!tx.is_autocommit());
        let ids: i64 = tx
            .query_row("SELECT COUNT(*) FROM accounts WHERE id IN (3,4)", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            ids, 1,
            "the caller's write stands, the projection's is undone"
        );

        let returned = guarded(&tx, 3, 1, "test", || {
            tx.execute_batch("ROLLBACK")?;
            Err(CoreError::InvalidInput("forced".into()))
        });
        assert!(matches!(returned, Err(CoreError::InvalidInput(_))));
        assert!(tx.is_autocommit());
        // A failed RELEASE with the transaction gone is the same case.
        let released = guarded(&tx, 3, 1, "test", || {
            tx.execute_batch("ROLLBACK").map_err(Into::into)
        });
        assert!(released.is_err(), "{released:?}");
    }

    // ---- Round 2 (final review): ownership, matching, rebuild ----

    /// 1.1. A friend's ref-less "dinner Sat 7pm": another authenticated
    /// sender replying in the same thread, on the same date, cannot rename,
    /// move or cancel it. Its update makes its own ghost, and its
    /// cancellation reaches only that.
    #[test]
    fn another_sender_in_the_thread_cannot_rename_move_or_cancel_a_refless_ghost() {
        let store = fixture();
        mail_from(&store, 1, 1, "t1", 90, "alice@gmail.com", Some(true));
        mail_from(&store, 1, 2, "t1", 60, "mallory@corp.example", Some(true));
        mail_from(&store, 1, 3, "t1", 30, "mallory@corp.example", Some(true));
        let dinner = |title, change, start| Ev {
            title,
            refs: &[],
            change,
            start: Some(start),
            commitment: EventCommitment::Invited,
            kind: EventRecordKind::Invite,
            location: None,
        };
        decide(
            &store,
            1,
            1,
            &dinner("Dinner", EventChange::Created, "2026-10-03T19:00:00-04:00").decision(),
        );
        let alice = listed(&store, 1)[0].clone();
        decide(
            &store,
            1,
            2,
            &dinner(
                "Dinner at 8",
                EventChange::Updated,
                "2026-10-03T20:00:00-04:00",
            )
            .decision(),
        );
        decide(
            &store,
            1,
            3,
            &dinner(
                "Dinner off",
                EventChange::Cancelled,
                "2026-10-03T19:00:00-04:00",
            )
            .decision(),
        );
        let after = ghost_by(&store, alice.id);
        assert_eq!(
            (after.title.as_str(), after.start, after.mail_state),
            ("Dinner", alice.start, GhostMailState::Live)
        );
        assert_eq!(roles(&after), vec![(1, EventChange::Created)]);
        assert_eq!(after.owner_scope.as_deref(), Some("alice@gmail.com"));
        // Mallory's update is Mallory's ghost, and Mallory's cancellation
        // cancelled that one.
        let ghosts = listed(&store, 1);
        assert_eq!(ghosts.len(), 2);
        let other = ghosts.iter().find(|g| g.id != alice.id).unwrap();
        assert_eq!(other.owner_scope.as_deref(), Some("corp.example"));
        assert_eq!(other.mail_state, GhostMailState::Cancelled);
    }

    /// 1.1. Fifty replies from another sender in the owner's thread go to
    /// their own ghost, so the source cap cannot evict the owner's record.
    #[test]
    fn fifty_replies_from_another_sender_evict_nothing_of_the_owners() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 1000);
        decide(
            &store,
            1,
            1,
            &Ev {
                refs: &[],
                ..Default::default()
            }
            .decision(),
        );
        let owner = listed(&store, 1)[0].id;
        for id in 2..=52 {
            mail_from(
                &store,
                1,
                id,
                "t1",
                1000 - id,
                "spam@other.example",
                Some(true),
            );
            decide(
                &store,
                1,
                id,
                &Ev {
                    refs: &[],
                    title: "Show (moved)",
                    change: EventChange::Updated,
                    ..Default::default()
                }
                .decision(),
            );
        }
        let g = ghost_by(&store, owner);
        assert_eq!(g.title, "Show");
        assert_eq!(roles(&g), vec![(1, EventChange::Created)]);
        assert_eq!(source_rows(&store, owner), vec![(1, 0)]);
        assert_eq!(count(&store, "ghost_events"), 2);
    }

    /// 1.2. `com.mx` is a public suffix: a hotel there and a stranger there
    /// are two scopes, so the stranger's update with the hotel's booking
    /// number is a ghost of its own.
    #[test]
    fn a_stranger_under_the_same_public_suffix_cannot_touch_the_ghost() {
        let store = fixture();
        mail_from(&store, 1, 1, "t1", 60, "reservas@hotel.com.mx", Some(true));
        mail_from(&store, 1, 2, "t2", 30, "reservas@evil.com.mx", Some(true));
        decide(&store, 1, 1, &Ev::default().decision());
        let hotel = listed(&store, 1)[0].id;
        decide(
            &store,
            1,
            2,
            &Ev {
                title: "Moved",
                change: EventChange::Updated,
                start: Some("2026-10-04T20:00:00-04:00"),
                ..Default::default()
            }
            .decision(),
        );
        let g = ghost_by(&store, hotel);
        assert_eq!(g.title, "Show");
        assert_eq!(roles(&g), vec![(1, EventChange::Created)]);
        assert_eq!(keys(&store, hotel), vec!["ref:hotel.com.mx:abc123"]);
        assert_eq!(listed(&store, 1).len(), 2);
    }

    /// 1.2. Two addresses at a provider the freemail list did not know
    /// before (a country domain of a listed brand) are two scopes.
    #[test]
    fn two_senders_at_a_newly_listed_freemail_provider_do_not_share_a_scope() {
        let store = fixture();
        mail_from(&store, 1, 1, "t1", 60, "alice@hotmail.co.uk", Some(true));
        mail_from(&store, 1, 2, "t2", 30, "bob@hotmail.co.uk", Some(true));
        decide(&store, 1, 1, &Ev::default().decision());
        let alice = listed(&store, 1)[0].id;
        decide(
            &store,
            1,
            2,
            &Ev {
                title: "Moved",
                change: EventChange::Updated,
                ..Default::default()
            }
            .decision(),
        );
        assert_eq!(ghost_by(&store, alice).title, "Show", "bob is not alice");
        assert_eq!(listed(&store, 1).len(), 2);
    }

    /// F1. A reminder re-triaged as an unrelated appointment: the ghost it
    /// fed is still the Show's (message 1 describes it), so the reminder's
    /// new record makes its own ghost and the Show keeps its title, Going
    /// mark, commitment and key.
    #[test]
    fn a_reminder_retriaged_as_another_event_leaves_the_show_ghost_alone() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 90);
        mail(&store, 1, 2, "t2", 30);
        decide(&store, 1, 1, &Ev::default().decision());
        decide(
            &store,
            1,
            2,
            &Ev {
                change: EventChange::Updated,
                title: "Show reminder",
                ..Default::default()
            }
            .decision(),
        );
        let show = listed(&store, 1)[0].id;
        store
            .set_ghost_user_state(1, show, Some(GhostUserState::Going), Utc::now())
            .unwrap();
        decide(
            &store,
            1,
            2,
            &Ev {
                title: "Dentist",
                refs: &["ZZZ999"],
                start: Some("2026-11-20T09:00:00-05:00"),
                location: Some("Dental office"),
                kind: EventRecordKind::Other,
                commitment: EventCommitment::Unknown,
                ..Default::default()
            }
            .decision(),
        );
        let g = ghost_by(&store, show);
        assert_eq!(
            (g.title.as_str(), g.start),
            ("Show", at("2026-10-03T20:00:00-04:00"))
        );
        assert_eq!(g.user_state, Some(GhostUserState::Going));
        assert_eq!(g.commitment, EventCommitment::Committed);
        assert_eq!(roles(&g), vec![(1, EventChange::Created)]);
        assert_eq!(keys(&store, show), vec!["ref:venue.example:abc123"]);
        let dentist = listed(&store, 1)
            .into_iter()
            .find(|g| g.id != show)
            .unwrap();
        assert_eq!(dentist.title, "Dentist");
        assert_eq!(dentist.user_state, None);
    }

    /// F1 on the SAME day: the re-triaged reminder now reads as another
    /// event that evening, so the date test alone cannot refuse the Show
    /// ghost. It is refused because message 1 still feeds it: a previous
    /// ghost is reused only when this message is its only feeder.
    #[test]
    fn a_reminder_retriaged_as_another_event_the_same_day_leaves_the_show_ghost_alone() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 90);
        mail(&store, 1, 2, "t2", 30);
        decide(&store, 1, 1, &Ev::default().decision());
        decide(
            &store,
            1,
            2,
            &Ev {
                change: EventChange::Updated,
                title: "Show reminder",
                ..Default::default()
            }
            .decision(),
        );
        let show = listed(&store, 1)[0].id;
        store
            .set_ghost_user_state(1, show, Some(GhostUserState::Going), Utc::now())
            .unwrap();
        decide(
            &store,
            1,
            2,
            &Ev {
                title: "Dinner before",
                refs: &["ZZZ999"],
                start: Some("2026-10-03T18:00:00-04:00"),
                kind: EventRecordKind::Reservation,
                commitment: EventCommitment::Unknown,
                ..Default::default()
            }
            .decision(),
        );
        let g = ghost_by(&store, show);
        assert_eq!(g.title, "Show");
        assert_eq!(roles(&g), vec![(1, EventChange::Created)]);
        assert_eq!(keys(&store, show), vec!["ref:venue.example:abc123"]);
        let dinner = listed(&store, 1)
            .into_iter()
            .find(|g| g.id != show)
            .unwrap();
        assert_eq!(
            (dinner.title.as_str(), dinner.user_state),
            ("Dinner before", None)
        );
    }

    /// F2. One message's two records re-triaged in the other order, one
    /// with a changed ref: the record whose ref matches takes its ghost by
    /// ref, and the other falls back to the remaining ghost this message
    /// alone fed, keeping the user's dismissal, instead of a new ghost.
    #[test]
    fn a_swap_plus_a_changed_ref_keeps_the_dismissed_ghost() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        let out = |refs| Ev {
            title: "Flight out",
            refs,
            start: Some("2026-10-03T08:00:00-04:00"),
            ..Default::default()
        };
        let back = Ev {
            title: "Flight back",
            refs: &["BBB222"],
            start: Some("2026-10-10T08:00:00-04:00"),
            ..Default::default()
        };
        decide(
            &store,
            1,
            1,
            &decided(vec![out(&["AAA111"]).record(), back.record()]),
        );
        let ghosts = listed(&store, 1);
        let (first, second) = (ghosts[0].id, ghosts[1].id);
        assert_eq!(ghost_by(&store, first).title, "Flight out");
        store
            .set_ghost_user_state(1, first, Some(GhostUserState::Dismissed), Utc::now())
            .unwrap();
        decide(
            &store,
            1,
            1,
            &decided(vec![back.record(), out(&["AAA119"]).record()]),
        );
        let g = ghost_by(&store, first);
        assert_eq!(g.title, "Flight out");
        assert_eq!(g.user_state, Some(GhostUserState::Dismissed));
        assert_eq!(ghost_by(&store, second).title, "Flight back");
        assert_eq!(count(&store, "ghost_events"), 2);
        assert_eq!(source_rows(&store, first), vec![(1, 1)]);
        assert_eq!(source_rows(&store, second), vec![(1, 0)]);
    }

    /// F3. A cancellation that matches nothing creates nothing.
    #[test]
    fn an_unmatched_cancellation_makes_no_ghost() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        decide(
            &store,
            1,
            1,
            &Ev {
                change: EventChange::Cancelled,
                ..Default::default()
            }
            .decision(),
        );
        assert_eq!(count(&store, "ghost_events"), 0);
        assert_eq!(count(&store, "ghost_event_sources"), 0);
        // An update that matches nothing still does: it carries facts.
        decide(
            &store,
            1,
            1,
            &Ev {
                change: EventChange::Updated,
                ..Default::default()
            }
            .decision(),
        );
        assert_eq!(listed(&store, 1).len(), 1);
    }

    /// F3. A ref-less cancellation with no start, in a thread where its
    /// sender has two live ghosts on other dates, cannot tell which it
    /// means: it cancels neither and makes no third.
    #[test]
    fn an_ambiguous_refless_cancellation_cancels_nothing() {
        let store = fixture();
        let alice = "alice@gmail.com";
        mail_from(&store, 1, 1, "t9", 90, alice, Some(true));
        mail_from(&store, 1, 2, "t9", 60, alice, Some(true));
        mail_from(&store, 1, 3, "t9", 30, alice, Some(true));
        let coffee = |start, change| Ev {
            title: "Coffee",
            refs: &[],
            start,
            change,
            commitment: EventCommitment::Proposed,
            kind: EventRecordKind::Proposal,
            location: None,
        };
        decide(
            &store,
            1,
            1,
            &coffee(Some("2026-10-05T10:00:00-04:00"), EventChange::Created).decision(),
        );
        decide(
            &store,
            1,
            2,
            &coffee(Some("2026-10-07T10:00:00-04:00"), EventChange::Created).decision(),
        );
        decide(
            &store,
            1,
            3,
            &coffee(None, EventChange::Cancelled).decision(),
        );
        let ghosts = listed(&store, 1);
        assert_eq!(ghosts.len(), 2);
        assert!(ghosts.iter().all(|g| g.mail_state == GhostMailState::Live));
        assert_eq!(count(&store, "ghost_events"), 2);
        // With only one live candidate the same cancellation is unambiguous.
        decide(&store, 1, 2, &MessageDecision::default());
        decide(
            &store,
            1,
            3,
            &coffee(None, EventChange::Cancelled).decision(),
        );
        assert_eq!(listed(&store, 1)[0].mail_state, GhostMailState::Cancelled);
    }

    /// 1.3. Only `updated` may bridge dates by ref. A cancellation or an
    /// agreement carrying a flight-number ref a month from every ghost is not
    /// about any of them.
    #[test]
    fn only_an_update_bridges_dates_by_ref() {
        let store = fixture();
        let airline = "noreply@united.example";
        for (id, ago) in [(1, 90), (2, 60), (3, 30), (4, 20)] {
            mail_from(&store, 1, id, &format!("t{id}"), ago, airline, Some(true));
        }
        let flight = |start, change| Ev {
            title: "UA 1234",
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
        let october = listed(&store, 1)[0].id;
        decide(
            &store,
            1,
            2,
            &flight("2026-11-20T08:00:00-05:00", EventChange::Cancelled).decision(),
        );
        decide(
            &store,
            1,
            3,
            &flight("2026-11-20T08:00:00-05:00", EventChange::Agreed).decision(),
        );
        let g = ghost_by(&store, october);
        assert_eq!(g.mail_state, GhostMailState::Live, "no cancellation");
        assert_eq!(roles(&g), vec![(1, EventChange::Created)]);
        // The agreement made its own ghost; the cancellation made nothing.
        assert_eq!(count(&store, "ghost_events"), 2);
        // An update a week later is a reschedule of the October flight (the
        // nearer of the two ghosts holding the ref).
        decide(
            &store,
            1,
            4,
            &flight("2026-10-10T08:00:00-04:00", EventChange::Updated).decision(),
        );
        let moved = ghost_by(&store, october);
        assert_eq!(moved.start, at("2026-10-10T08:00:00-04:00"));
        assert_eq!(
            roles(&moved),
            vec![(1, EventChange::Created), (4, EventChange::Updated)]
        );
    }

    /// 1.3. A start-less cancellation sharing its ref with several ghosts
    /// cancels the one next to happen after the mail arrived, not the
    /// lowest id.
    #[test]
    fn a_startless_cancellation_by_ref_takes_the_next_upcoming_ghost() {
        let store = fixture();
        let now = Utc::now();
        let iso = |days: i64| {
            (now + chrono::Duration::days(days))
                .fixed_offset()
                .to_rfc3339()
        };
        let (past, soon, later) = (iso(-30), iso(10), iso(40));
        for (id, ago) in [(1, 90), (2, 80), (3, 70), (4, 10)] {
            mail(&store, 1, id, &format!("t{id}"), ago);
        }
        for (id, start) in [(1, &past), (2, &later), (3, &soon)] {
            decide(
                &store,
                1,
                id,
                &Ev {
                    title: "Season ticket",
                    refs: &["SEASON2026A"],
                    start: Some(start.as_str()),
                    ..Default::default()
                }
                .decision(),
            );
        }
        let by_start = |start: &str| {
            listed(&store, 1)
                .into_iter()
                .find(|g| g.start == at(start))
                .unwrap()
        };
        let next = by_start(&soon).id;
        decide(
            &store,
            1,
            4,
            &Ev {
                title: "Cancelled",
                refs: &["SEASON2026A"],
                start: None,
                change: EventChange::Cancelled,
                ..Default::default()
            }
            .decision(),
        );
        assert_eq!(ghost_by(&store, next).mail_state, GhostMailState::Cancelled);
        assert_eq!(by_start(&past).mail_state, GhostMailState::Live);
        assert_eq!(by_start(&later).mail_state, GhostMailState::Live);
    }

    /// F6. Re-triaged while restricted into something else, then unrestricted
    /// by the user: the refresh matches the stored decision afresh, so the
    /// new record does not inherit the old ghost and its Going mark.
    #[test]
    fn a_decision_changed_while_restricted_does_not_take_over_the_old_ghost() {
        use crate::store::agent_triage::AgentTriageStore;
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        decide(&store, 1, 1, &Ev::default().decision());
        let show = listed(&store, 1)[0].id;
        store
            .set_ghost_user_state(1, show, Some(GhostUserState::Going), Utc::now())
            .unwrap();
        let mut other = Ev {
            title: "Password reset",
            refs: &["QQQ777"],
            start: Some("2026-12-01T09:00:00-05:00"),
            ..Default::default()
        }
        .decision();
        other.external_access.restricted = true;
        decide(&store, 1, 1, &other);
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
        let ghosts = listed(&store, 1);
        assert_eq!(ghosts.len(), 1);
        assert_eq!(ghosts[0].title, "Password reset");
        assert_ne!(ghosts[0].id, show, "a new ghost, not the Show's");
        assert_eq!(ghosts[0].user_state, None);
        assert_eq!(
            count(&store, "ghost_events"),
            1,
            "nothing describes the Show now"
        );
    }

    /// F5. A precision change clears the span at the other precision, so a
    /// later change back does not bring the old correction back.
    #[test]
    fn a_precision_change_and_back_does_not_bring_the_user_end_back() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        let at_start = |start| {
            Ev {
                location: None,
                start: Some(start),
                ..Default::default()
            }
            .decision()
        };
        decide(&store, 1, 1, &at_start("2026-10-03T20:00:00-04:00"));
        let id = listed(&store, 1)[0].id;
        store
            .set_ghost_user_end(1, id, Some(at("2026-10-03T23:00:00-04:00")), Utc::now())
            .unwrap();
        // Re-triages of the one message, all within a day, so the ghost and
        // its id are kept throughout.
        decide(&store, 1, 1, &at_start("2026-10-03"));
        assert_eq!(ghost_by(&store, id).user_end, None);
        decide(&store, 1, 1, &at_start("2026-10-03T10:00:00-04:00"));
        let g = ghost_by(&store, id);
        assert_eq!(g.user_end, None, "cleared, not ignored");
        assert_ne!(
            g.effective_end().map(|e| e.source),
            Some(crate::triage::ghost::EndSource::User)
        );
        // The same for an all-day span and a change to timed and back.
        decide(&store, 1, 1, &at_start("2026-10-03"));
        store
            .set_ghost_user_end(1, id, Some(at("2026-10-05")), Utc::now())
            .unwrap();
        assert_eq!(ghost_by(&store, id).user_end, Some(at("2026-10-05")));
        decide(&store, 1, 1, &at_start("2026-10-03T10:00:00-04:00"));
        decide(&store, 1, 1, &at_start("2026-10-03"));
        assert_eq!(ghost_by(&store, id).user_end, None);
    }

    /// F12. An end correction on a travel ghost teaches no prior, and a
    /// prior that exists anyway is not read for travel.
    #[test]
    fn a_travel_end_correction_teaches_no_prior() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        mail(&store, 1, 2, "t2", 30);
        let flight = |refs, start| Ev {
            title: "Flight",
            refs,
            start: Some(start),
            location: Some("JFK Terminal 4, Queens"),
            kind: EventRecordKind::Travel,
            ..Default::default()
        };
        decide(
            &store,
            1,
            1,
            &flight(&["UA0001X"], "2026-10-03T08:00:00-04:00").decision(),
        );
        let id = listed(&store, 1)[0].id;
        store
            .set_ghost_user_end(1, id, Some(at("2026-10-03T21:00:00-04:00")), Utc::now())
            .unwrap();
        assert_eq!(count(&store, "event_duration_priors"), 0);
        // Even a prior already stored (by an older build) is not read.
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO event_duration_priors(account_id,venue_key,kind,minutes,updated_at)
                 VALUES(1,'jfk terminal 4','travel',780,'x')",
                [],
            )
            .unwrap();
        decide(
            &store,
            1,
            2,
            &flight(&["DL0002X"], "2026-11-03T08:00:00-05:00").decision(),
        );
        let later = listed(&store, 1).into_iter().find(|g| g.id != id).unwrap();
        assert_eq!(later.history_minutes, Some(780));
        assert_ne!(
            later.effective_end().map(|e| e.source),
            Some(crate::triage::ghost::EndSource::History)
        );
    }

    /// F4. Clearing an end takes back only the prior THIS ghost's correction
    /// wrote, and only while it still holds that number: a later correction
    /// of another ghost at the venue is the user's newer word and stays.
    #[test]
    fn clearing_an_end_leaves_a_prior_another_ghost_rewrote() {
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
                start: Some("2026-11-07T20:00:00-05:00"),
                ..Default::default()
            }
            .decision(),
        );
        let (a, b) = {
            let ghosts = listed(&store, 1);
            (ghosts[0].id, ghosts[1].id)
        };
        let now = Utc::now();
        store
            .set_ghost_user_end(1, a, Some(at("2026-10-03T23:00:00-04:00")), now)
            .unwrap();
        store
            .set_ghost_user_end(1, b, Some(at("2026-11-08T00:00:00-05:00")), now)
            .unwrap();
        store.set_ghost_user_end(1, a, None, now).unwrap();
        let prior: (i64, i64) = store
            .lock()
            .unwrap()
            .query_row(
                "SELECT minutes,ghost_id FROM event_duration_priors",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(prior, (240, b));
    }

    /// F16. Sealed mail feeds no ghost and serves none, judged by what seals
    /// mail TODAY (the Auth page's `sealed_messages` test), never the retired
    /// `triage.sensitivity` column: an auth decision, an access review still
    /// pending, or a restriction. At LIST time in SQL, at reconcile, and back
    /// again when it clears.
    #[test]
    fn sealed_mail_feeds_no_ghost() {
        use crate::triage::decision::{AuthAssessment, AuthKind};
        use crate::types::TriageAxis;
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        mail(&store, 1, 2, "t2", 30);
        mail(&store, 1, 3, "t3", 20);
        decide(&store, 1, 1, &Ev::default().decision());
        let id = listed(&store, 1)[0].id;
        store
            .set_ghost_user_state(1, id, Some(GhostUserState::Going), Utc::now())
            .unwrap();
        // The legacy column seals nothing (the pipeline never writes it).
        set_flag(
            &store,
            "UPDATE triage SET sensitivity='sealed' WHERE message_id=1",
        );
        assert_eq!(
            listed(&store, 1).len(),
            1,
            "the retired column is not the test"
        );

        // AN ACCESS REVIEW PENDING (the message changed since its review): no
        // rebuild runs, the listing hides it at once, and it is back when the
        // review allows it.
        set_flag(
            &store,
            "UPDATE agent_message_state SET access='pending' WHERE message_id=1",
        );
        assert!(listed(&store, 1).is_empty(), "pending serves nothing");
        set_flag(
            &store,
            "UPDATE agent_message_state SET access='allowed' WHERE message_id=1",
        );
        assert_eq!(listed(&store, 1)[0].id, id);
        // A pending message that is decided makes nothing.
        {
            let mut conn = store.lock().unwrap();
            let tx = conn.transaction().unwrap();
            let decision = Ev {
                refs: &["PENDNG"],
                start: Some("2026-11-03T20:00:00-05:00"),
                ..Default::default()
            }
            .decision();
            tx.execute(
                "INSERT INTO agent_message_state(account_id,message_id,content_snapshot,access)
                 VALUES(1,3,'','pending')",
                [],
            )
            .unwrap();
            reconcile(&tx, 1, 3, true, &decision).unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(count(&store, "ghost_events"), 1, "pending fed nothing");

        // AN AUTH DECISION (a sign-in alert that also names an event, left
        // unrestricted by the model): it feeds no ghost...
        let auth = |ev: Ev<'_>| MessageDecision {
            auth: AuthAssessment {
                kinds: vec![AuthKind::LoginAlert],
                evidence: vec![],
                critical: false,
            },
            ..ev.decision()
        };
        let sign_in = auth(Ev {
            refs: &["QXJZKM"],
            start: Some("2026-12-03T20:00:00-05:00"),
            ..Default::default()
        });
        assert!(!sign_in.external_access.restricted);
        decide(&store, 1, 2, &sign_in);
        assert_eq!(count(&store, "ghost_events"), 1, "auth mail fed nothing");
        // ...and a ghost whose supplier is re-decided as auth serves nothing,
        // even before any rebuild: the listing reads the stored decision.
        set_flag(
            &store,
            &format!(
                "UPDATE agent_message_decisions SET decision_json='{}' WHERE message_id=1",
                serde_json::to_string(&auth(Ev::default())).unwrap()
            ),
        );
        assert!(listed(&store, 1).is_empty(), "auth serves nothing");
        decide(&store, 1, 1, &auth(Ev::default()));
        assert!(listed(&store, 1).is_empty());
        assert_eq!(count(&store, "ghost_events"), 1, "kept, with its mark");
        // Decided again without the auth kind, it is back with the mark.
        decide(&store, 1, 1, &Ev::default().decision());
        let g = listed(&store, 1).remove(0);
        assert_eq!((g.id, g.user_state), (id, Some(GhostUserState::Going)));

        // RESTRICTED by hand hides it at once, and lifting it brings it back.
        store
            .correct_triage(1, 1, TriageAxis::Sensitivity, "sealed", None, Utc::now())
            .unwrap()
            .unwrap();
        assert!(listed(&store, 1).is_empty());
        store
            .correct_triage(1, 1, TriageAxis::Sensitivity, "normal", None, Utc::now())
            .unwrap()
            .unwrap();
        let g = listed(&store, 1).remove(0);
        assert_eq!((g.id, g.user_state), (id, Some(GhostUserState::Going)));

        // A STORED DECISION THAT DOES NOT PARSE (verify2 V11) is not known to
        // be harmless: its message feeds nothing until it is decided again.
        set_flag(
            &store,
            "UPDATE agent_message_decisions SET decision_json='{not json' WHERE message_id=1",
        );
        assert!(listed(&store, 1).is_empty(), "unreadable serves nothing");
        decide(&store, 1, 1, &Ev::default().decision());
        let g = listed(&store, 1).remove(0);
        assert_eq!((g.id, g.user_state), (id, Some(GhostUserState::Going)));
    }

    // ---- Round 3 ----

    /// A ghost's row as stored, listed or not: owner scope, owner address,
    /// user state, commitment, title.
    type Stored3 = (
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        String,
    );
    fn stored(store: &SqliteStore, id: i64) -> Option<Stored3> {
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT owner_scope,owner_addr,user_state,commitment,title FROM ghost_events WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()
            .unwrap()
    }

    /// The invariants every reconcile keeps: no ghost without a source row,
    /// no record (message and position) on two ghosts, and a user mark only
    /// ever on the ghost it was set on (`marked`, when given).
    fn invariants(store: &SqliteStore, marked: Option<i64>) {
        let conn = store.lock().unwrap();
        let orphans: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ghost_events g WHERE NOT EXISTS(
                     SELECT 1 FROM ghost_event_sources s WHERE s.ghost_id=g.id)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0, "a ghost with no source row");
        let doubled: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM (SELECT 1 FROM ghost_event_sources
                 GROUP BY account_id,message_id,position HAVING COUNT(DISTINCT ghost_id)>1)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(doubled, 0, "a record on two ghosts");
        if let Some(marked) = marked {
            let elsewhere: Vec<i64> = conn
                .prepare("SELECT id FROM ghost_events WHERE user_state IS NOT NULL AND id<>?1")
                .unwrap()
                .query_map([marked], |r| r.get(0))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            assert!(elsewhere.is_empty(), "the mark moved to {elsewhere:?}");
        }
    }

    /// N1. Two sources of one Going ghost, both restricted, then lifted one
    /// at a time: the first to come back finds the ghost by the keys the
    /// still-restricted source keeps for it, so the ghost, its id and its
    /// mark survive (the reviewer's probe made a new, unmarked ghost and
    /// deleted this one).
    #[test]
    fn two_restricted_sources_lifted_one_at_a_time_keep_the_ghost_and_its_mark() {
        use crate::store::agent_triage::AgentTriageStore;
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        mail(&store, 1, 2, "t2", 30);
        decide(&store, 1, 1, &Ev::default().decision());
        decide(
            &store,
            1,
            2,
            &Ev {
                title: "Show reminder",
                change: EventChange::Updated,
                ..Default::default()
            }
            .decision(),
        );
        let g = listed(&store, 1)[0].id;
        store
            .set_ghost_user_state(1, g, Some(GhostUserState::Going), Utc::now())
            .unwrap();
        let access = |id: i64, restricted: bool| {
            store
                .correct_agent_triage(
                    1,
                    id,
                    "external_access",
                    &serde_json::json!(restricted),
                    Utc::now(),
                )
                .unwrap();
        };
        access(1, true);
        access(2, true);
        assert!(listed(&store, 1).is_empty());
        assert_eq!(
            keys(&store, g),
            vec!["ref:venue.example:abc123"],
            "an ineligible source keeps its keys"
        );
        invariants(&store, Some(g));
        access(1, false);
        invariants(&store, Some(g));
        assert_eq!(count(&store, "ghost_events"), 1);
        let after = ghost_by(&store, g);
        assert_eq!(after.user_state, Some(GhostUserState::Going));
        assert_eq!(after.title, "Show");
        access(2, false);
        invariants(&store, Some(g));
        let ghosts = listed(&store, 1);
        assert_eq!(ghosts.len(), 1);
        assert_eq!(
            (ghosts[0].id, ghosts[0].user_state, ghosts[0].title.as_str()),
            (g, Some(GhostUserState::Going), "Show reminder")
        );
        assert_eq!(
            roles(&ghosts[0]),
            vec![(1, EventChange::Created), (2, EventChange::Updated)]
        );
    }

    /// N1, ref-less: the same through sealing, where only the thread finds
    /// the ghost, which it does through the sealed sources' kept rows.
    #[test]
    fn two_sealed_refless_sources_unsealed_one_at_a_time_keep_the_ghost() {
        use crate::types::TriageAxis;
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        mail(&store, 1, 2, "t1", 30);
        decide(
            &store,
            1,
            1,
            &Ev {
                refs: &[],
                ..Default::default()
            }
            .decision(),
        );
        decide(
            &store,
            1,
            2,
            &Ev {
                refs: &[],
                title: "Show reminder",
                change: EventChange::Updated,
                ..Default::default()
            }
            .decision(),
        );
        let g = listed(&store, 1)[0].id;
        store
            .set_ghost_user_state(1, g, Some(GhostUserState::Dismissed), Utc::now())
            .unwrap();
        for (id, level) in [(1, "sealed"), (2, "sealed"), (2, "normal"), (1, "normal")] {
            store
                .correct_triage(1, id, TriageAxis::Sensitivity, level, None, Utc::now())
                .unwrap()
                .unwrap();
            invariants(&store, Some(g));
            assert_eq!(count(&store, "ghost_events"), 1, "after {id} {level}");
        }
        let ghosts = listed(&store, 1);
        assert_eq!(
            (ghosts[0].id, ghosts[0].user_state),
            (g, Some(GhostUserState::Dismissed))
        );
        assert_eq!(source_rows(&store, g), vec![(1, 0), (2, 0)]);
    }

    /// N2. A re-sync fills in the message's authentication result: its own
    /// ghost (which it alone feeds) is re-owned to its scope and address,
    /// keeps its id and the user's Going mark, and is now committed and keyed.
    #[test]
    fn an_authentication_result_that_arrives_later_keeps_the_ghost_and_its_mark() {
        let store = fixture();
        mail_from(&store, 1, 1, "t1", 60, "Tickets@Venue.example", None);
        decide(&store, 1, 1, &Ev::default().decision());
        let g = listed(&store, 1)[0].id;
        assert_eq!(
            stored(&store, g).unwrap(),
            (None, None, None, "unknown".into(), "Show".into())
        );
        assert!(keys(&store, g).is_empty());
        store
            .set_ghost_user_state(1, g, Some(GhostUserState::Going), Utc::now())
            .unwrap();
        set_flag(&store, "UPDATE messages SET auth_pass=1 WHERE id=1");
        decide(&store, 1, 1, &Ev::default().decision());
        invariants(&store, Some(g));
        assert_eq!(count(&store, "ghost_events"), 1);
        assert_eq!(
            stored(&store, g).unwrap(),
            (
                Some("venue.example".into()),
                Some("tickets@venue.example".into()),
                Some("going".into()),
                "committed".into(),
                "Show".into()
            )
        );
        assert_eq!(keys(&store, g), vec!["ref:venue.example:abc123"]);
        assert!(ghost_by(&store, g).verified());
    }

    /// N2, the reverse: the result is lost. The ghost keeps its id and the
    /// dismissal, and loses what only authenticated mail may claim: its
    /// owner, its keys and its committed state.
    #[test]
    fn an_authentication_result_that_is_lost_keeps_the_ghost_and_its_dismissal() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        decide(&store, 1, 1, &Ev::default().decision());
        let g = listed(&store, 1)[0].id;
        store
            .set_ghost_user_state(1, g, Some(GhostUserState::Dismissed), Utc::now())
            .unwrap();
        set_flag(&store, "UPDATE messages SET auth_pass=NULL WHERE id=1");
        decide(&store, 1, 1, &Ev::default().decision());
        invariants(&store, Some(g));
        assert_eq!(count(&store, "ghost_events"), 1);
        assert_eq!(
            stored(&store, g).unwrap(),
            (
                None,
                None,
                Some("dismissed".into()),
                "unknown".into(),
                "Show".into()
            )
        );
        assert!(keys(&store, g).is_empty());
        assert!(!ghost_by(&store, g).verified());
    }

    /// N2's limit: a ghost another message also feeds is never re-owned. The
    /// message whose authentication went makes a ghost of its own; the
    /// owner's ghost keeps its owner, its mark and its other source.
    #[test]
    fn a_ghost_another_message_feeds_is_never_reowned() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        mail(&store, 1, 2, "t2", 30);
        decide(&store, 1, 1, &Ev::default().decision());
        decide(
            &store,
            1,
            2,
            &Ev {
                title: "Show reminder",
                change: EventChange::Updated,
                ..Default::default()
            }
            .decision(),
        );
        let g = listed(&store, 1)[0].id;
        store
            .set_ghost_user_state(1, g, Some(GhostUserState::Going), Utc::now())
            .unwrap();
        set_flag(&store, "UPDATE messages SET auth_pass=NULL WHERE id=2");
        decide(
            &store,
            1,
            2,
            &Ev {
                title: "Show reminder",
                change: EventChange::Updated,
                ..Default::default()
            }
            .decision(),
        );
        invariants(&store, Some(g));
        let (owner, owner_addr, mark, commitment, title) = stored(&store, g).unwrap();
        assert_eq!(
            (owner.as_deref(), owner_addr.as_deref(), mark.as_deref()),
            (
                Some("venue.example"),
                Some("tickets@venue.example"),
                Some("going")
            )
        );
        assert_eq!((commitment.as_str(), title.as_str()), ("committed", "Show"));
        assert_eq!(source_rows(&store, g), vec![(1, 0)]);
        let other = listed(&store, 1).into_iter().find(|x| x.id != g).unwrap();
        assert_eq!(other.owner_scope, None);
        assert_eq!(roles(&other), vec![(2, EventChange::Updated)]);
    }

    /// N3. A source whose message lost its authentication after it was
    /// assigned contributes nothing to a rebuild another message triggers:
    /// not its commitment, not its keys, not its facts.
    #[test]
    fn a_source_that_lost_its_scope_contributes_nothing_to_a_rebuild() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        mail(&store, 1, 2, "t2", 30);
        decide(
            &store,
            1,
            1,
            &Ev {
                refs: &["ABC123", "XYZ789"],
                ..Default::default()
            }
            .decision(),
        );
        let g = listed(&store, 1)[0].id;
        set_flag(&store, "UPDATE messages SET auth_pass=0 WHERE id=1");
        decide(
            &store,
            1,
            2,
            &Ev {
                title: "Show reminder",
                change: EventChange::Updated,
                commitment: EventCommitment::Unknown,
                ..Default::default()
            }
            .decision(),
        );
        invariants(&store, None);
        let after = ghost_by(&store, g);
        assert_eq!(after.commitment, EventCommitment::Unknown);
        assert_eq!(after.supplier_message_id, Some(2));
        assert_eq!(keys(&store, g), vec!["ref:venue.example:abc123"]);
        // Its row stays, like an ineligible source's.
        assert_eq!(source_rows(&store, g), vec![(1, 0), (2, 0)]);
        // With message 2 gone too, nothing in scope is eligible: hidden.
        set_flag(&store, "UPDATE messages SET is_spam=1 WHERE id=2");
        decide(&store, 1, 2, &MessageDecision::default());
        assert!(listed(&store, 1).is_empty());
        assert_eq!(count(&store, "ghost_events"), 1);
    }

    /// N4. Two strangers at a shared domain no list knows (`isp.example`)
    /// have one scope, so only the identical-address test keeps the second
    /// from renaming, moving or cancelling the first's ref-less ghost in a
    /// thread they share. Each address owns its own ghost.
    #[test]
    fn a_stranger_on_an_unlisted_shared_domain_cannot_join_a_refless_ghost() {
        let store = fixture();
        mail_from(&store, 1, 1, "t1", 90, "Alice@ISP.example", Some(true));
        mail_from(&store, 1, 2, "t1", 60, "mallory@isp.example", Some(true));
        mail_from(&store, 1, 3, "t1", 30, "mallory@isp.example", Some(true));
        let dinner = |title, change, start| Ev {
            title,
            refs: &[],
            change,
            start: Some(start),
            commitment: EventCommitment::Invited,
            kind: EventRecordKind::Invite,
            location: None,
        };
        decide(
            &store,
            1,
            1,
            &dinner("Dinner", EventChange::Created, "2026-10-03T19:00:00-04:00").decision(),
        );
        let alice = listed(&store, 1)[0].id;
        assert_eq!(
            stored(&store, alice).unwrap().0.as_deref(),
            Some("isp.example"),
            "one scope for both: the list does not know this domain"
        );
        assert_eq!(
            stored(&store, alice).unwrap().1.as_deref(),
            Some("alice@isp.example")
        );
        decide(
            &store,
            1,
            2,
            &dinner(
                "Dinner at 8",
                EventChange::Updated,
                "2026-10-03T20:00:00-04:00",
            )
            .decision(),
        );
        decide(
            &store,
            1,
            3,
            &dinner(
                "Dinner off",
                EventChange::Cancelled,
                "2026-10-03T19:00:00-04:00",
            )
            .decision(),
        );
        invariants(&store, None);
        let after = ghost_by(&store, alice);
        assert_eq!(
            (after.title.as_str(), after.mail_state),
            ("Dinner", GhostMailState::Live)
        );
        assert_eq!(roles(&after), vec![(1, EventChange::Created)]);
        let ghosts = listed(&store, 1);
        assert_eq!(ghosts.len(), 2);
        let other = ghosts.iter().find(|g| g.id != alice).unwrap();
        assert_eq!(
            stored(&store, other.id).unwrap().1.as_deref(),
            Some("mallory@isp.example")
        );
        // Mallory's own cancellation found Mallory's own ghost.
        assert_eq!(other.mail_state, GhostMailState::Cancelled);
        // Alice herself still moves her ghost by thread.
        mail_from(&store, 1, 4, "t1", 10, "alice@isp.example", Some(true));
        decide(
            &store,
            1,
            4,
            &dinner(
                "Dinner at 7:30",
                EventChange::Updated,
                "2026-10-03T19:30:00-04:00",
            )
            .decision(),
        );
        assert_eq!(ghost_by(&store, alice).title, "Dinner at 7:30");
    }

    /// N4: the ref rule still takes the scope, not the address: a merchant
    /// confirms from one mailbox and cancels from another.
    #[test]
    fn one_organisation_still_matches_by_ref_across_its_addresses() {
        let store = fixture();
        mail_from(
            &store,
            1,
            1,
            "t1",
            60,
            "reservations@hotel.example",
            Some(true),
        );
        mail_from(
            &store,
            1,
            2,
            "t2",
            30,
            "noreply@mail.hotel.example",
            Some(true),
        );
        decide(&store, 1, 1, &Ev::default().decision());
        let g = listed(&store, 1)[0].id;
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
        let after = ghost_by(&store, g);
        assert_eq!(after.mail_state, GhostMailState::Cancelled);
        assert_eq!(
            roles(&after),
            vec![(1, EventChange::Created), (2, EventChange::Cancelled)]
        );
        assert_eq!(count(&store, "ghost_events"), 1);
    }

    /// The four invariants through a long run of the round's changes on one
    /// marked ghost: restrictions, an authentication flip on one source, a
    /// re-triage, a stranger's reply, spam and rescue.
    #[test]
    fn a_marked_ghost_keeps_its_invariants_through_every_change() {
        use crate::store::agent_triage::AgentTriageStore;
        let store = fixture();
        mail(&store, 1, 1, "t1", 90);
        mail(&store, 1, 2, "t1", 60);
        mail_from(&store, 1, 3, "t1", 30, "mallory@venue.example", Some(true));
        decide(&store, 1, 1, &Ev::default().decision());
        let update = Ev {
            title: "Show (late set)",
            change: EventChange::Updated,
            ..Default::default()
        };
        decide(&store, 1, 2, &update.decision());
        let g = listed(&store, 1)[0].id;
        store
            .set_ghost_user_state(1, g, Some(GhostUserState::Going), Utc::now())
            .unwrap();
        let owner = || stored(&store, g).map(|s| (s.0, s.1));
        let first_owner = owner();
        let step = |label: &str| {
            invariants(&store, Some(g));
            assert_eq!(
                stored(&store, g).and_then(|s| s.2).as_deref(),
                Some("going"),
                "{label}"
            );
        };
        for (id, restricted) in [(2, true), (1, true), (2, false), (1, false)] {
            store
                .correct_agent_triage(
                    1,
                    id,
                    "external_access",
                    &serde_json::json!(restricted),
                    Utc::now(),
                )
                .unwrap();
            step(&format!("access {id} {restricted}"));
            assert_eq!(owner(), first_owner, "another source feeds it");
        }
        set_flag(&store, "UPDATE messages SET auth_pass=NULL WHERE id=2");
        decide(&store, 1, 2, &update.decision());
        step("message 2 lost its authentication");
        assert_eq!(owner(), first_owner);
        decide(
            &store,
            1,
            3,
            &Ev {
                refs: &[],
                title: "Show cancelled",
                change: EventChange::Cancelled,
                ..Default::default()
            }
            .decision(),
        );
        step("a stranger at the same domain cancels by thread");
        assert_eq!(ghost_by(&store, g).mail_state, GhostMailState::Live);
        set_flag(&store, "UPDATE messages SET is_spam=1 WHERE id=1");
        decide(&store, 1, 1, &Ev::default().decision());
        step("spam");
        assert!(store.clear_spam(1, 1).unwrap());
        step("rescued");
        assert_eq!(owner(), first_owner);
        assert_eq!(ghost_by(&store, g).title, "Show");
    }

    // ---- Zoned local times (the flight with no times) ----

    /// A Tokyo-to-Los Angeles flight as the model writes it: naive local
    /// times, each with its own zone.
    fn zoned_flight(title: &str, refs: &[&str]) -> MessageDecision {
        let mut record = Ev {
            title,
            refs,
            kind: EventRecordKind::Travel,
            location: None,
            ..Default::default()
        }
        .record();
        let at = |value: &str, zone: &str| {
            Some(SupportedTime {
                value: value.into(),
                timezone: Some(zone.into()),
                source_message_id: 0,
                interpreted_relative: false,
            })
        };
        if let RecordProposal::Event { start, end, .. } = &mut record {
            *start = at("2026-10-19T21:30", "Asia/Tokyo");
            *end = at("2026-10-19T14:45", "America/Los_Angeles");
        }
        decided(vec![record])
    }

    /// The stored columns, read raw: what every reader of the table sees.
    fn stored_times(store: &SqliteStore, id: i64) -> (Option<String>, Option<String>) {
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT starts_at,ends_at FROM ghost_events WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }

    #[test]
    fn a_zoned_local_start_and_end_are_stored_with_their_zones_offsets() {
        let store = fixture();
        mail(&store, 1, 1, "t1", 60);
        decide(&store, 1, 1, &zoned_flight("Flight NH 106", &["PNR4XQ"]));
        let g = &listed(&store, 1)[0];
        assert_eq!(
            stored_times(&store, g.id),
            (
                Some("2026-10-19T21:30:00+09:00".into()),
                Some("2026-10-19T14:45:00-07:00".into())
            )
        );
        assert_eq!(g.start, at("2026-10-19T21:30:00+09:00"));
        assert_eq!(g.stated_end, Some(at("2026-10-19T14:45:00-07:00")));
        assert_eq!(g.start_timezone.as_deref(), Some("Asia/Tokyo"));
    }

    /// A file store whose ghosts were built before the resolver: the zoned
    /// times are in the stored decisions and missing from the ghost rows, as
    /// the old projection left them. Returns the store's path and the ghosts.
    fn store_with_timeless_ghosts(tag: &str) -> (std::path::PathBuf, Vec<i64>) {
        let dir =
            std::env::temp_dir().join(format!("squelch-ghost-times-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.sqlite3");
        let store = SqliteStore::open(&path).unwrap();
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO accounts(id,email,created_at) VALUES(1,'a@test',?1)",
                [Utc::now().to_rfc3339()],
            )
            .unwrap();
        mail(&store, 1, 1, "t1", 60);
        mail(&store, 1, 2, "t2", 30);
        decide(&store, 1, 1, &zoned_flight("Outbound", &["PNR4XQ"]));
        decide(&store, 1, 2, &zoned_flight("Another trip", &["QZ7KMB"]));
        let ids: Vec<i64> = listed(&store, 1).iter().map(|g| g.id).collect();
        assert_eq!(ids.len(), 2);
        store
            .lock()
            .unwrap()
            .execute_batch(
                "UPDATE ghost_events SET starts_at=NULL,start_sort=NULL,start_timezone=NULL,ends_at=NULL",
            )
            .unwrap();
        for id in &ids {
            assert_eq!(stored_times(&store, *id), (None, None));
        }
        (path, ids)
    }

    fn marker(store: &SqliteStore) -> i64 {
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM app_settings WHERE account_id=1 AND key=?1",
                [super::super::migrate::GHOST_ZONED_TIMES_KEY],
                |r| r.get(0),
            )
            .unwrap()
    }

    #[test]
    fn opening_the_store_rebuilds_timeless_ghosts_once() {
        let (path, ids) = store_with_timeless_ghosts("once");
        let store = SqliteStore::open(&path).unwrap();
        for id in &ids {
            assert_eq!(
                stored_times(&store, *id),
                (
                    Some("2026-10-19T21:30:00+09:00".into()),
                    Some("2026-10-19T14:45:00-07:00".into())
                ),
                "ghost {id}"
            );
        }
        assert_eq!(
            listed(&store, 1)[0].start_timezone.as_deref(),
            Some("Asia/Tokyo")
        );
        assert_eq!(marker(&store), 1);
        // Once: a later open leaves the ghosts as it finds them.
        store
            .lock()
            .unwrap()
            .execute_batch("UPDATE ghost_events SET starts_at=NULL,ends_at=NULL")
            .unwrap();
        drop(store);
        let store = SqliteStore::open(&path).unwrap();
        for id in &ids {
            assert_eq!(stored_times(&store, *id), (None, None), "ghost {id}");
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_ghost_that_fails_to_rebuild_is_skipped_and_the_store_still_opens() {
        let (path, ids) = store_with_timeless_ghosts("skip");
        let (bad, good) = (ids[0], ids[1]);
        SqliteStore::open(&path)
            .unwrap()
            .lock()
            .unwrap()
            .execute_batch(&format!(
                "DELETE FROM app_settings WHERE key='{}';
                 UPDATE ghost_events SET starts_at=NULL,start_sort=NULL,ends_at=NULL;
                 CREATE TRIGGER fail_one BEFORE UPDATE ON ghost_events WHEN NEW.id={bad}
                 BEGIN SELECT RAISE(ABORT,'no'); END;",
                super::super::migrate::GHOST_ZONED_TIMES_KEY
            ))
            .unwrap();
        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(stored_times(&store, bad), (None, None));
        assert_eq!(
            stored_times(&store, good),
            (
                Some("2026-10-19T21:30:00+09:00".into()),
                Some("2026-10-19T14:45:00-07:00".into())
            )
        );
        assert_eq!(marker(&store), 1, "one bad ghost does not retry forever");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
