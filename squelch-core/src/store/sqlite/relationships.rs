//! The relationship ledger's store half (docs/UNDERSTANDING.md §2.1): the
//! counting behind each `relationships` row, the dirty set that says which
//! rows are owed a recompute, and the reads the agent context and the human
//! door make of it. The arithmetic is [`crate::relationship`]; this module
//! only counts.
//!
//! HUMAN DOOR AND TRIAGE ONLY, like `people` and `reading_senders`, and for
//! their reason: who the owner answers, and how fast, is the correspondent
//! graph the agent door was never handed. Nothing here is reachable from a
//! `/mcp` tool.
//!
//! EVERY RECOMPUTE IS BOUNDED AND INDEXED, never a directory scan and never
//! a walk of one sender's whole history, because a sender is recounted every
//! round it is marked and a notifications firehose is marked every round. Off
//! `idx_messages_from_received` (account, address, arrival, and the three
//! flags the count filters on):
//!
//! - `received_count` and the first and last arrival are ONE index-only walk:
//!   no table row, no per-row probe.
//! - Everything that needs a row or a probe (threads and replies, the name,
//!   corrections) reads only the address's newest [`RECENT_WINDOW`] inbound
//!   messages, which are the index's own tail. The reply rate over the last
//!   500 messages is the better signal anyway: it is how the owner treats
//!   them now.
//! - A recount whose received, sent and initiated counts are UNCHANGED since
//!   the stored row skips those walks altogether and refreshes only what moves
//!   without new mail (attention, fatigue, the monthly rate), unless the mark
//!   asked for a full recount (`relationship_dirty.full`, set by the marks
//!   that change a row without changing its counts) or the nightly walk is
//!   running.
//! - Thread replies ride `idx_messages_thread_received`, initiations
//!   `idx_message_recipients_addr`, and the fatigue score is the Reading
//!   purge's own `SenderMeasurer`, whose bounds that module documents.
//!
//! Measured on a synthetic 240,000-message store with one 40,000-message
//! sender (`tests::relationships_bench`): see the numbers in PR #718.
//!
//! THE TRAPS, each a rule below:
//!
//! - DATES COME OFF `messages.received_at`, never `senders.first_seen`. The
//!   backfill walks newest first, so the directory's first sighting dates a
//!   ten-year correspondent to last Tuesday.
//! - INBOUND is not the owner's own mail, not provider spam, and not the
//!   archive tier, which is absent from every count. `received_count` stops
//!   there, so it is a bare index count and INCLUDES sealed mail: a count is
//!   all that leaves here, and excluding sealed mail costs a probe per row.
//!   The bounded window also leaves out mail KNOWN to be sealed
//!   (`known_sealed_mail_sql!`: a shop's login codes are not its newsletter),
//!   so threads, replies and the name never rest on one. "Not the owner's
//!   own" is `is_sent = 0` here, and not `own_mail_sql!`: the account's own
//!   address never has a row, and for any other address the two agree.
//! - PENDING MAIL COUNTS, the one place this module takes the looser of the
//!   two sealed macros, and the reason is the reply itself. Ingesting the
//!   owner's reply re-queues the thread it lands in, so at the moment the
//!   Sent mark asks for a recount every inbound message of the thread just
//!   answered is pending again. The strict macro would drop exactly the
//!   thread that made the owner a replier, and nothing would recount it until
//!   the nightly pass. A count is all that leaves here; a pending message
//!   that turns out to be a login code costs one inflated count until the
//!   next recount of that sender. The ATTENTION columns stay strict: they are
//!   the Reading purge's own statements, which sweep on what they find.
//! - ATTENTION IS THE PURGE'S OWN MEASUREMENT, the same `Measured` the
//!   fatigue score comes from, so "opened 2 of 38" in the stored reasons and
//!   the attention columns beside it agree by construction. `attention_rows`
//!   and `opened_count` are over mail received at or after the later of the
//!   Reading window and the open ledger floor; `median_read_fraction` and
//!   `link_clicks` are over the newest `DWELL_SAMPLE_CAP` opens a client
//!   reported on, OPENED at or after the dwell ledger floor, because a zero
//!   dwell before that floor is not a measurement
//!   (`migrate::stamp_dwell_ledger_start`).
//! - THE ACCOUNT'S OWN ADDRESS HAS NO ROW. Its "inbound" is notes to self.

use super::attention::open_ledger_since;
use super::reading_senders::{STATS_WINDOW_DAYS, SenderMeasurer, sender_key};
use super::*;
use crate::relationship::{self, AttentionCounts, Counts, Relationship, ThreadReply};
use crate::triage::sender_fatigue::Reason;

/// `sync_state.mailbox` of the one-time first-run backfill. Present means the
/// table has been filled once for the account; until then the agent context
/// carries no `relationship` key at all.
pub const RELATIONSHIPS_BACKFILL_KEY: &str = "relationships_backfill";

/// `sync_state.mailbox` of the nightly full recompute: `last_uid` holds the
/// owner-local DAY it last ran for, as `yyyymmdd`, so a restart on the same
/// day does not run it again.
pub const RELATIONSHIPS_DAILY_KEY: &str = "relationships_daily";

/// How many addresses one lock acquisition recomputes. Each is a handful of
/// seeks plus at most `DWELL_SAMPLE_CAP` body reads; twenty-five keeps one
/// chunk to a few milliseconds of the mutex both doors and sync wait on.
pub const RECOMPUTE_CHUNK: usize = 25;

/// The most dirty addresses one refine round drains. A burst past it (the
/// first backfill's flood) is carried by the next round, and by the nightly
/// recompute, rather than by one round that holds the lane for seconds.
pub const DIRTY_DRAIN_MAX: usize = 200;

/// How many of an address's newest inbound messages the row-reading walks
/// (threads, replies, name, corrections) look at.
pub const RECENT_WINDOW: i64 = 500;

/// One address's inbound mail, over `messages m`, through the covering index:
/// `?1` account, `?2` address. Everything here is a column of the index, so
/// a walk over it never reads a table row.
macro_rules! inbound_sql {
    () => {
        " FROM messages m INDEXED BY idx_messages_from_received
          WHERE m.account_id = ?1
            AND m.from_addr = ?2 COLLATE NOCASE
            AND m.is_sent = 0 AND m.is_spam = 0 AND m.is_archive = 0 "
    };
}

/// The address's newest [`RECENT_WINDOW`] inbound messages that are not
/// known to be sealed, newest first: the index's tail, a triage row and the
/// sealed probes for each of at most 500 rows. A subquery other statements
/// select from, aliased `w`, carrying `id`, `thread_id`, `received_at` and
/// `from_name`.
macro_rules! recent_sql {
    () => {
        concat!(
            "(SELECT m.id AS id, m.thread_id AS thread_id, m.received_at AS received_at,
                     m.from_name AS from_name
              FROM messages m INDEXED BY idx_messages_from_received
              LEFT JOIN triage t ON t.message_id = m.id
              WHERE m.account_id = ?1
                AND m.from_addr = ?2 COLLATE NOCASE
                AND m.is_sent = 0 AND m.is_spam = 0 AND m.is_archive = 0
                AND NOT ",
            known_sealed_mail_sql!(),
            " ORDER BY m.received_at DESC LIMIT 500) w"
        )
    };
}

/// Count, first and last arrival: one index-only walk.
pub(crate) const INBOUND_SQL: &str = concat!(
    "SELECT COUNT(*), MIN(m.received_at), MAX(m.received_at)",
    inbound_sql!()
);

/// One row per thread in the recent window: when the address first wrote
/// into it inside the window, and the owner's first SENT message in it after
/// that, if any. Each reply probe is one seek on
/// `idx_messages_thread_received`.
pub(crate) const THREADS_SQL: &str = concat!(
    "SELECT i.first_at,
            (SELECT MIN(s.received_at) FROM messages s
              WHERE s.account_id = ?1 AND s.thread_id = i.thread_id
                AND s.is_sent = 1 AND s.is_archive = 0
                AND s.received_at > i.first_at)
     FROM (SELECT w.thread_id AS thread_id, MIN(w.received_at) AS first_at FROM ",
    recent_sql!(),
    " GROUP BY w.thread_id) i"
);

/// The newest name in the recent window.
pub(crate) const NAME_SQL: &str = concat!(
    "SELECT w.from_name FROM ",
    recent_sql!(),
    " WHERE trim(COALESCE(w.from_name, '')) != '' LIMIT 1"
);

/// Threads the owner started by writing to the address: a sent message that
/// names them (`message_recipients`, lowercased, so its BINARY index serves
/// `?2`) and is the FIRST message of its thread.
///
/// PINNED BOTH WAYS. Left to choose, the planner walked the whole account's
/// messages by thread and probed each for a recipient row, because the
/// first-message subquery looks like a reason to go thread by thread.
/// `INDEXED BY` holds the recipients seek, and `CROSS JOIN` keeps it outer.
pub(crate) const INITIATED_SQL: &str = "SELECT COUNT(DISTINCT m.thread_id)
     FROM message_recipients r INDEXED BY idx_message_recipients_addr
     CROSS JOIN messages m ON m.account_id = r.account_id AND m.id = r.message_id
     WHERE r.account_id = ?1 AND r.addr = ?2
       AND m.is_sent = 1 AND m.is_archive = 0
       AND m.id = (SELECT f.id FROM messages f
                    WHERE f.account_id = ?1 AND f.thread_id = m.thread_id
                    ORDER BY f.received_at, f.id LIMIT 1)";

/// The owner's corrections on the address: the feedback table by its NOCASE
/// sender index (an index-only count), and field corrections on the recent
/// window's messages, each probed by key.
pub(crate) const CORRECTIONS_SQL: &str = concat!(
    "SELECT
       (SELECT COUNT(*) FROM triage_feedback
         WHERE account_id = ?1 AND sender = ?2 COLLATE NOCASE)
     + (SELECT COUNT(*) FROM ",
    recent_sql!(),
    " CROSS JOIN agent_triage_corrections c
            ON c.account_id = ?1 AND c.message_id = w.id)"
);

/// The Sent-derived contacts row, `is_known_contact`'s source.
const CONTACT_SQL: &str = "SELECT sent_count, last_sent_at, display_name FROM contacts
     WHERE account_id = ?1 AND addr = ?2 COLLATE NOCASE
     ORDER BY sent_count DESC LIMIT 1";

/// The next `?3` addresses after `?2` in the full walk's universe.
pub(crate) const CHUNK_SQL: &str = "SELECT a FROM (
         SELECT lower(trim(addr)) AS a FROM senders WHERE account_id = ?1
         UNION SELECT lower(trim(addr)) FROM contacts WHERE account_id = ?1
         UNION SELECT addr FROM message_recipients WHERE account_id = ?1
         UNION SELECT addr FROM relationships WHERE account_id = ?1)
     WHERE a > ?2 AND a != ''
     ORDER BY a LIMIT ?3";

/// Columns of a stored row, in [`row_from_sql`]'s order.
const ROW_COLUMNS: &str = "addr, display_name, received_count, received_first_at,
     received_last_at, received_per_month, sent_count, sent_last_at, initiated_count,
     inbound_threads, replied_threads, median_reply_ms, attention_rows, opened_count,
     median_read_fraction, link_clicks, corrections, fatigue_score, fatigue_reasons,
     computed_at";

/// How the list read orders its rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationshipSort {
    /// The people the owner answers most, then the most mail.
    Replies,
    /// The most inbound mail.
    Volume,
    /// The highest fatigue score; unscored last.
    Fatigue,
    /// The newest mail either way.
    Recent,
}

impl RelationshipSort {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "replies" => Some(Self::Replies),
            "volume" => Some(Self::Volume),
            "fatigue" => Some(Self::Fatigue),
            "recent" => Some(Self::Recent),
            _ => None,
        }
    }

    fn order_by(self) -> &'static str {
        match self {
            Self::Replies => "replied_threads DESC, received_count DESC, addr",
            Self::Volume => "received_count DESC, addr",
            Self::Fatigue => "fatigue_score IS NULL, fatigue_score DESC, received_count DESC, addr",
            Self::Recent => {
                "MAX(COALESCE(received_last_at, ''), COALESCE(sent_last_at, '')) DESC, addr"
            }
        }
    }
}

/// The list read's answer.
#[derive(Debug, Clone)]
pub struct RelationshipList {
    /// Whole days the attention columns cover ([`coverage_days_conn`]).
    pub coverage_days: i64,
    /// The newest `computed_at` among the account's rows, `None` when it has
    /// none (the first pass has not run, or there is no mail).
    pub computed_at: Option<DateTime<Utc>>,
    pub rows: Vec<Relationship>,
}

/// Mark `addr` owed a recompute. THE HOT-PATH WRITE: one `INSERT OR IGNORE`
/// of the lowercased address, nothing read, nothing computed.
pub(super) fn mark_dirty_conn(conn: &Connection, account_id: AccountId, addr: &str) -> Result<()> {
    let addr = sender_key(addr);
    if addr.is_empty() {
        return Ok(());
    }
    conn.prepare_cached(
        "INSERT OR IGNORE INTO relationship_dirty(account_id, addr) VALUES(?1, ?2)",
    )?
    .execute(params![account_id, addr])?;
    Ok(())
}

/// Mark the sender of message `id` owed a recompute, in ONE statement: the
/// attention POST knows the message, not the address, and must not pay a
/// round trip to learn it.
pub(super) fn mark_message_sender_dirty_conn(
    conn: &Connection,
    account_id: AccountId,
    id: i64,
) -> Result<()> {
    conn.prepare_cached(
        "INSERT OR IGNORE INTO relationship_dirty(account_id, addr)
         SELECT account_id, lower(trim(from_addr)) FROM messages
          WHERE account_id = ?1 AND id = ?2 AND is_sent = 0 AND trim(from_addr) != ''",
    )?
    .execute(params![account_id, id])?;
    Ok(())
}

/// The floor `attention_rows` counts from: `SenderMeasurer::new`'s own, the
/// later of the Reading window and the open ledger floor. Restated here only
/// to report its age; the counts themselves come from the measurer.
fn attention_floor(
    conn: &Connection,
    account_id: AccountId,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    let window = now - chrono::Duration::days(STATS_WINDOW_DAYS);
    Ok(window.max(open_ledger_since(conn, account_id)?))
}

/// Whole days the attention columns cover right now: the measurer's
/// `covered`, at most the Reading window, and under
/// `sender_fatigue::MIN_COVERED_DAYS` on a young ledger, which is what tells
/// the agent context to leave attention out (the fatigue score declines to
/// score under the same floor).
pub(super) fn coverage_days_conn(
    conn: &Connection,
    account_id: AccountId,
    now: DateTime<Utc>,
) -> Result<i64> {
    Ok((now - attention_floor(conn, account_id, now)?)
        .num_days()
        .clamp(0, STATS_WINDOW_DAYS))
}

/// Has the first-run backfill filled this account's table?
fn backfilled_conn(conn: &Connection, account_id: AccountId) -> Result<bool> {
    Ok(conn
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM sync_state WHERE account_id = ?1 AND mailbox = ?2)",
        )?
        .query_row(params![account_id, RELATIONSHIPS_BACKFILL_KEY], |r| {
            r.get(0)
        })?)
}

fn parse_at(s: Option<String>) -> Option<DateTime<Utc>> {
    s.and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|at| at.with_timezone(&Utc))
}

/// One recompute's shared state: built once per lock acquisition so a chunk
/// of addresses shares its prepared statements and floors.
struct Recomputer<'c> {
    conn: &'c Connection,
    account_id: AccountId,
    now: DateTime<Utc>,
    self_addr: String,
    measurer: SenderMeasurer<'c>,
}

impl<'c> Recomputer<'c> {
    fn new(conn: &'c Connection, account_id: AccountId, now: DateTime<Utc>) -> Result<Self> {
        let self_addr: String = conn
            .query_row(
                "SELECT lower(trim(email)) FROM accounts WHERE id = ?1",
                params![account_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_default();
        Ok(Self {
            conn,
            account_id,
            now,
            self_addr,
            measurer: SenderMeasurer::new(conn, account_id, now)?,
        })
    }

    /// Recount `addr` and write or remove its row. `full` refuses the
    /// unchanged-counts shortcut.
    fn recompute(&mut self, addr: &str, full: bool) -> Result<()> {
        let addr = sender_key(addr);
        if addr.is_empty() {
            return Ok(());
        }
        if addr == self.self_addr {
            return self.delete(&addr);
        }
        match self.count(&addr, full)? {
            Some(row) => write_row(self.conn, self.account_id, &row),
            None => self.delete(&addr),
        }
    }

    fn delete(&self, addr: &str) -> Result<()> {
        self.conn
            .prepare_cached("DELETE FROM relationships WHERE account_id = ?1 AND addr = ?2")?
            .execute(params![self.account_id, addr])?;
        Ok(())
    }

    /// The row for `addr`, or `None` when it has no live mail either way.
    fn count(&mut self, addr: &str, full: bool) -> Result<Option<Relationship>> {
        let conn = self.conn;
        let account = self.account_id;
        let (received_count, first, last): (i64, Option<String>, Option<String>) = conn
            .prepare_cached(INBOUND_SQL)?
            .query_row(params![account, addr], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
        let contact: Option<(i64, Option<String>, Option<String>)> = conn
            .prepare_cached(CONTACT_SQL)?
            .query_row(params![account, addr], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?;
        let initiated_count: i64 = conn
            .prepare_cached(INITIATED_SQL)?
            .query_row(params![account, addr], |r| r.get(0))?;
        let (sent_count, sent_last_at, contact_name) = match contact {
            Some((n, last, name)) => (n.max(0), parse_at(last), name),
            None => (0, None, None),
        };
        let mut counts = Counts {
            received_count,
            received_first_at: parse_at(first),
            received_last_at: parse_at(last),
            sent_count,
            sent_last_at,
            initiated_count,
            ..Default::default()
        };
        if !relationship::worth_a_row(&counts) {
            return Ok(None);
        }

        // THE PURGE FLOW'S OWN MEASUREMENT, never a second one: the score a
        // sender is shown and swept for is the score the ledger stores, and
        // the attention columns are the counts that score was computed from.
        // Always refreshed: opens, dwell and the window move without new mail.
        let measured = self.measurer.measure(addr)?;
        let signals = measured.signals;
        counts.attention = AttentionCounts {
            rows: signals.received,
            opened: signals.opened,
            median_read_fraction: signals.median_read_fraction,
            link_clicks: signals.clicks,
        };
        counts.fatigue = (measured.score, measured.reasons);

        // THE SHORTCUT: nothing arrived and nothing was sent either way since
        // the stored row, so the window walks would find what they found.
        if !full
            && let Some(stored) = relationship_conn(conn, account, addr)?
            && relationship::counts_unchanged(&stored, &counts)
        {
            return Ok(Some(relationship::refresh(stored, counts, self.now)));
        }

        counts.threads = conn
            .prepare_cached(THREADS_SQL)?
            .query_map(params![account, addr], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            // A stamp that will not parse is not a thread the ledger can date.
            .filter_map(|(first, reply)| {
                Some(ThreadReply {
                    first_inbound_at: parse_at(Some(first))?,
                    reply_at: parse_at(reply),
                })
            })
            .collect();
        counts.corrections = conn
            .prepare_cached(CORRECTIONS_SQL)?
            .query_row(params![account, addr], |r| r.get(0))?;
        let sender_name: Option<String> = conn
            .prepare_cached(NAME_SQL)?
            .query_row(params![account, addr], |r| r.get(0))
            .optional()?;
        let name = [sender_name, contact_name]
            .into_iter()
            .flatten()
            .map(|n| n.trim().to_string())
            .find(|n| !n.is_empty());
        Ok(Some(relationship::derive(addr, name, counts, self.now)))
    }
}

fn write_row(conn: &Connection, account_id: AccountId, row: &Relationship) -> Result<()> {
    let rfc = |at: Option<DateTime<Utc>>| at.map(|at| at.to_rfc3339());
    let reasons = serde_json::to_string(&row.fatigue_reasons)
        .map_err(|e| CoreError::InvalidInput(format!("fatigue reasons: {e}")))?;
    conn.prepare_cached(
        "INSERT INTO relationships(account_id, addr, display_name, received_count,
             received_first_at, received_last_at, received_per_month, sent_count,
             sent_last_at, initiated_count, inbound_threads, replied_threads,
             median_reply_ms, attention_rows, opened_count, median_read_fraction,
             link_clicks, corrections, fatigue_score, fatigue_reasons, computed_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)
         ON CONFLICT(account_id, addr) DO UPDATE SET
             display_name = excluded.display_name,
             received_count = excluded.received_count,
             received_first_at = excluded.received_first_at,
             received_last_at = excluded.received_last_at,
             received_per_month = excluded.received_per_month,
             sent_count = excluded.sent_count,
             sent_last_at = excluded.sent_last_at,
             initiated_count = excluded.initiated_count,
             inbound_threads = excluded.inbound_threads,
             replied_threads = excluded.replied_threads,
             median_reply_ms = excluded.median_reply_ms,
             attention_rows = excluded.attention_rows,
             opened_count = excluded.opened_count,
             median_read_fraction = excluded.median_read_fraction,
             link_clicks = excluded.link_clicks,
             corrections = excluded.corrections,
             fatigue_score = excluded.fatigue_score,
             fatigue_reasons = excluded.fatigue_reasons,
             computed_at = excluded.computed_at",
    )?
    .execute(params![
        account_id,
        row.addr,
        row.display_name,
        row.received_count,
        rfc(row.received_first_at),
        rfc(row.received_last_at),
        row.received_per_month.map(f64::from),
        row.sent_count,
        rfc(row.sent_last_at),
        row.initiated_count,
        row.inbound_threads,
        row.replied_threads,
        row.median_reply_ms,
        row.attention_rows,
        row.opened_count,
        row.median_read_fraction.map(f64::from),
        row.link_clicks,
        row.corrections,
        row.fatigue_score.map(f64::from),
        reasons,
        row.computed_at.to_rfc3339(),
    ])?;
    Ok(())
}

/// A stored row back, in [`ROW_COLUMNS`] order. A reasons blob that will not
/// parse reads as no reasons: the score beside it still stands, and a client
/// that shows "why" shows nothing rather than something invented.
fn row_from_sql(r: &rusqlite::Row<'_>) -> rusqlite::Result<Relationship> {
    let f32_opt = |i: usize| -> rusqlite::Result<Option<f32>> {
        Ok(r.get::<_, Option<f64>>(i)?.map(|v| v as f32))
    };
    let reasons: Option<String> = r.get(18)?;
    Ok(Relationship {
        addr: r.get(0)?,
        display_name: r.get(1)?,
        received_count: r.get(2)?,
        received_first_at: parse_at(r.get(3)?),
        received_last_at: parse_at(r.get(4)?),
        received_per_month: f32_opt(5)?,
        sent_count: r.get(6)?,
        sent_last_at: parse_at(r.get(7)?),
        initiated_count: r.get(8)?,
        inbound_threads: r.get(9)?,
        replied_threads: r.get(10)?,
        median_reply_ms: r.get(11)?,
        attention_rows: r.get(12)?,
        opened_count: r.get(13)?,
        median_read_fraction: f32_opt(14)?,
        link_clicks: r.get(15)?,
        corrections: r.get(16)?,
        fatigue_score: f32_opt(17)?,
        fatigue_reasons: reasons
            .and_then(|json| serde_json::from_str::<Vec<Reason>>(&json).ok())
            .unwrap_or_default(),
        computed_at: dt(r, 19)?,
    })
}

pub(super) fn relationship_conn(
    conn: &Connection,
    account_id: AccountId,
    addr: &str,
) -> Result<Option<Relationship>> {
    Ok(conn
        .prepare_cached(&format!(
            "SELECT {ROW_COLUMNS} FROM relationships
             WHERE account_id = ?1 AND addr = ?2 COLLATE NOCASE LIMIT 1"
        ))?
        .query_row(params![account_id, addr.trim()], row_from_sql)
        .optional()?)
}

/// The `relationship` block of the triage agent's context for `addr`, or
/// `None`, which the caller serves as an ABSENT key: before the first-run
/// backfill has filled the table (a missing row then means "not counted
/// yet", not "no history"), and for an address with no row.
pub(super) fn relationship_context_conn(
    conn: &Connection,
    account_id: AccountId,
    addr: &str,
    now: DateTime<Utc>,
) -> Result<Option<serde_json::Value>> {
    if !backfilled_conn(conn, account_id)? {
        return Ok(None);
    }
    let Some(row) = relationship_conn(conn, account_id, addr)? else {
        return Ok(None);
    };
    let coverage = coverage_days_conn(conn, account_id, now)?;
    Ok(Some(relationship::agent_json(&row, coverage)))
}

impl SqliteStore {
    /// Recount one address now, whatever the dirty set says.
    pub fn recompute_relationship(&self, account_id: AccountId, addr: &str) -> Result<()> {
        let conn = self.lock()?;
        Recomputer::new(&conn, account_id, Utc::now())?.recompute(addr, true)
    }

    /// Recompute up to `max` dirty addresses, [`RECOMPUTE_CHUNK`] per lock
    /// acquisition, deleting each mark under the lock it was recomputed in.
    /// Returns how many were drained.
    pub fn drain_relationship_dirty(&self, account_id: AccountId, max: usize) -> Result<usize> {
        let mut drained = 0;
        while drained < max {
            let conn = self.lock()?;
            let take = RECOMPUTE_CHUNK.min(max - drained);
            let addrs: Vec<(String, bool)> = conn
                .prepare_cached(
                    "SELECT addr, full FROM relationship_dirty WHERE account_id = ?1
                     ORDER BY addr LIMIT ?2",
                )?
                .query_map(params![account_id, take as i64], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?
                .collect::<std::result::Result<_, _>>()?;
            if addrs.is_empty() {
                break;
            }
            let mut recomputer = Recomputer::new(&conn, account_id, Utc::now())?;
            for (addr, full) in &addrs {
                recomputer.recompute(addr, *full)?;
                conn.prepare_cached(
                    "DELETE FROM relationship_dirty WHERE account_id = ?1 AND addr = ?2",
                )?
                .execute(params![account_id, addr])?;
            }
            drained += addrs.len();
            if addrs.len() < take {
                break;
            }
        }
        Ok(drained)
    }

    /// Recompute the next [`RECOMPUTE_CHUNK`]-or-fewer addresses after
    /// `after` (exclusive, in address order) under ONE lock acquisition.
    /// `Some(last)` to resume from, `None` once the walk is done.
    ///
    /// THE UNIVERSE is every address with a sender-directory row, a contacts
    /// row, a Sent recipient row, or a ledger row already: the first three
    /// between them hold every address with mail either way (contacts alone
    /// would miss a recipient the contact seeding filtered out), and the last
    /// is how a row whose mail has gone is found and removed. One `UNION`
    /// over per-account tables per chunk, which is the full recompute's one
    /// walk and its whole point; a single address is never found this way.
    pub fn recompute_relationship_chunk(
        &self,
        account_id: AccountId,
        after: Option<&str>,
        max: usize,
    ) -> Result<Option<String>> {
        let conn = self.lock()?;
        let addrs: Vec<String> = conn
            .prepare_cached(CHUNK_SQL)?
            .query_map(
                params![account_id, after.unwrap_or(""), max.max(1) as i64],
                |r| r.get(0),
            )?
            .collect::<std::result::Result<_, _>>()?;
        let mut recomputer = Recomputer::new(&conn, account_id, Utc::now())?;
        // The nightly walk always counts in full: it is what catches every
        // change no mark saw.
        for addr in &addrs {
            recomputer.recompute(addr, true)?;
        }
        Ok(if addrs.len() < max.max(1) {
            None
        } else {
            addrs.last().cloned()
        })
    }

    /// The whole account, chunk by chunk, releasing the lock between chunks.
    /// The sync pass drives the chunks itself (it yields between them); this
    /// is the same walk for callers that need not.
    pub fn recompute_all_relationships(&self, account_id: AccountId) -> Result<()> {
        let mut after: Option<String> = None;
        while let Some(last) =
            self.recompute_relationship_chunk(account_id, after.as_deref(), RECOMPUTE_CHUNK)?
        {
            after = Some(last);
        }
        Ok(())
    }

    /// HUMAN-DOOR ONLY: one address's row, `None` when it has none.
    pub fn relationship(&self, account_id: AccountId, addr: &str) -> Result<Option<Relationship>> {
        relationship_conn(&*self.lock()?, account_id, addr)
    }

    /// HUMAN-DOOR ONLY: the account's rows, ordered by `sort`, at most
    /// `limit` (clamped to 1..=500).
    pub fn list_relationships(
        &self,
        account_id: AccountId,
        sort: RelationshipSort,
        limit: usize,
    ) -> Result<RelationshipList> {
        let conn = self.lock()?;
        let now = Utc::now();
        let rows = conn
            .prepare(&format!(
                "SELECT {ROW_COLUMNS} FROM relationships WHERE account_id = ?1
                 ORDER BY {} LIMIT ?2",
                sort.order_by()
            ))?
            .query_map(
                params![account_id, limit.clamp(1, 500) as i64],
                row_from_sql,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let computed_at: Option<String> = conn.query_row(
            "SELECT MAX(computed_at) FROM relationships WHERE account_id = ?1",
            params![account_id],
            |r| r.get(0),
        )?;
        Ok(RelationshipList {
            coverage_days: coverage_days_conn(&conn, account_id, now)?,
            computed_at: parse_at(computed_at),
            rows,
        })
    }

    /// The agent-context block for `addr` (see [`relationship_context_conn`]).
    pub fn relationship_context(
        &self,
        account_id: AccountId,
        addr: &str,
    ) -> Result<Option<serde_json::Value>> {
        relationship_context_conn(&*self.lock()?, account_id, addr, Utc::now())
    }
}
