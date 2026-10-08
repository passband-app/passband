//! The contact card behind a click on a sender in the reader
//! (`/client/person`): everything this store knows about ONE address, read in
//! one call. HUMAN-DOOR ONLY, like `contacts` and `senders`, and for the same
//! reason: who the user writes to, and who writes to them, is a graph the
//! agent door was never handed.
//!
//! Every part is an indexed probe on one address rather than a directory scan,
//! so the card costs about what opening a thread does:
//!
//! - RECEIVED is counted from `messages`, not read off the `senders`
//!   directory. The directory's `first_seen` is the first SIGHTING, and the
//!   backfill walks newest-first, so it would date a ten-year correspondent to
//!   last Tuesday. The count follows the listings' rule (inbound, not spam, not
//!   sealed), so the card never promises more mail than "their mail" finds.
//! - SENT is the `contacts` row, which is also the known-contact floor
//!   (`is_known_contact`'s own predicate), so the card and triage agree about
//!   whether this person is always signal.
//! - The RULE is the one Stage-1 would apply to this address today: the first
//!   match over `list_sender_rules`' order, through the same hand-rolled glob.
//!   Re-matching here in SQL would drift from it (see sql-predicate-drift).
//! - The TIMEZONE is the `sender_zones` row (`zones`): the user's override
//!   first, else what ingest inferred. It is learned from header metadata
//!   (a `Date` offset, an invitation's start zone) rather than content, and
//!   the account's own address never has one.

use super::*;
use crate::triage::rules::match_sender_rule;

/// How many of a person's recent conversations the card lists.
const RECENT_THREADS: u32 = 5;

/// Mail from this person, counted the way the listings count it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonReceived {
    pub count: i64,
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
}

/// Mail the user sent to this person, off the Sent-derived contacts row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonSent {
    pub count: i64,
    pub last_at: Option<DateTime<Utc>>,
}

/// A send group this address is a member of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonGroup {
    pub id: i64,
    pub name: String,
}

/// One recent conversation this person wrote into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonThread {
    pub thread_id: String,
    pub subject: String,
    pub last_at: DateTime<Utc>,
}

/// A person's timezone as the card shows it: zone, where it came from (`user`,
/// `invite`, `offsets`) and how sure the inference is.
pub type PersonTimezone = crate::timezone::InferredZone;

/// Everything the card shows about one address.
#[derive(Debug, Clone)]
pub struct PersonRecord {
    pub addr: String,
    /// The newest name they sign their mail with, else the name the user's own
    /// Sent mail addressed them by. `None` when neither ever named them.
    pub display_name: Option<String>,
    pub is_self: bool,
    /// Anyone the user has written to is guaranteed signal tier.
    pub known_contact: bool,
    pub received: Option<PersonReceived>,
    pub sent: Option<PersonSent>,
    pub rule: Option<SenderRule>,
    pub groups: Vec<PersonGroup>,
    pub recent_threads: Vec<PersonThread>,
    /// `None` when nothing is known and the user has not said.
    pub timezone: Option<PersonTimezone>,
}

impl SqliteStore {
    /// HUMAN-DOOR ONLY: the contact card for `addr`. Never errors on an address
    /// the store has never seen: that person gets a card with nothing on it,
    /// which is a true answer ("you have no mail from them").
    pub fn person_record(&self, account_id: AccountId, addr: &str) -> Result<PersonRecord> {
        let addr = addr.trim();
        // Read OUTSIDE the connection lock below: `list_sender_rules` takes it
        // itself, and the mutex is not reentrant.
        let rule = match_sender_rule(addr, &self.list_sender_rules(account_id)?).cloned();

        let conn = self.lock()?;

        let is_self: bool = conn
            .query_row(
                "SELECT lower(trim(email)) = lower(?2) FROM accounts WHERE id = ?1",
                params![account_id, addr],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false);

        // The sealed exclusion is the listings' own (`search_senders`'s EXISTS
        // probe): a card that counted sealed mail would advertise messages the
        // "their mail" search it links to structurally cannot return.
        let received = conn.query_row(
            "SELECT COUNT(*), MIN(m.received_at), MAX(m.received_at)
                 FROM messages m
                 LEFT JOIN triage t ON t.message_id = m.id
                 WHERE m.account_id = ?1 AND m.from_addr = ?2 COLLATE NOCASE
                   AND m.is_sent = 0 AND m.is_spam = 0
                   AND COALESCE(t.sensitivity, 'normal') != 'sealed'",
            params![account_id, addr],
            |r| {
                let count: i64 = r.get(0)?;
                if count == 0 {
                    return Ok(None);
                }
                Ok(Some(PersonReceived {
                    count,
                    first_at: dt(r, 1)?,
                    last_at: dt(r, 2)?,
                }))
            },
        )?;

        let contact: Option<(i64, Option<DateTime<Utc>>, Option<String>)> = conn
            .query_row(
                "SELECT sent_count, last_sent_at, display_name FROM contacts
                 WHERE account_id = ?1 AND addr = ?2 COLLATE NOCASE
                 ORDER BY sent_count DESC LIMIT 1",
                params![account_id, addr],
                |r| Ok((r.get(0)?, dt_opt(r, 1)?, r.get(2)?)),
            )
            .optional()?;
        // `is_known_contact`'s predicate exactly: a row with a zero count is a
        // stub, not somebody the user wrote to.
        let known_contact = contact.as_ref().is_some_and(|(n, _, _)| *n > 0);
        let sent = contact
            .as_ref()
            .filter(|(n, _, _)| *n > 0)
            .map(|(n, last, _)| PersonSent {
                count: *n,
                last_at: *last,
            });

        // Off the same mail `received` counts, not the `senders` directory:
        // the directory takes a name from sealed mail too (it registers before
        // triage decides sealed) and keeps one row per header SPELLING, so it
        // can disagree with the card's own count about who this is.
        let sender_name: Option<String> = conn
            .query_row(
                "SELECT m.from_name FROM messages m
                 LEFT JOIN triage t ON t.message_id = m.id
                 WHERE m.account_id = ?1 AND m.from_addr = ?2 COLLATE NOCASE
                   AND m.is_sent = 0 AND m.is_spam = 0
                   AND COALESCE(t.sensitivity, 'normal') != 'sealed'
                   AND trim(COALESCE(m.from_name, '')) != ''
                 ORDER BY m.received_at DESC LIMIT 1",
                params![account_id, addr],
                |r| r.get(0),
            )
            .optional()?;
        let display_name = [sender_name, contact.and_then(|(_, _, name)| name)]
            .into_iter()
            .flatten()
            .map(|n| n.trim().to_string())
            .find(|n| !n.is_empty());

        // Members are stored as the lowercased bare address.
        let mut stmt = conn.prepare(
            "SELECT g.id, g.name FROM group_members gm
             JOIN send_groups g ON g.id = gm.group_id
             WHERE gm.account_id = ?1 AND gm.addr = lower(?2)
             ORDER BY g.name COLLATE NOCASE",
        )?;
        let groups = stmt
            .query_map(params![account_id, addr], |r| {
                Ok(PersonGroup {
                    id: r.get(0)?,
                    name: r.get(1)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // The subject is the NEWEST message's in each thread: SQLite returns
        // the bare columns of the row that won a lone MAX().
        let mut stmt = conn.prepare(
            "SELECT m.thread_id, m.subject, MAX(m.received_at)
             FROM messages m
             LEFT JOIN triage t ON t.message_id = m.id
             WHERE m.account_id = ?1 AND m.from_addr = ?2 COLLATE NOCASE
               AND m.is_sent = 0 AND m.is_spam = 0
               AND COALESCE(t.sensitivity, 'normal') != 'sealed'
             GROUP BY m.thread_id
             ORDER BY MAX(m.received_at) DESC
             LIMIT ?3",
        )?;
        let recent_threads = stmt
            .query_map(params![account_id, addr, RECENT_THREADS], |r| {
                Ok(PersonThread {
                    thread_id: r.get(0)?,
                    subject: r.get(1)?,
                    last_at: dt(r, 2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let timezone = if is_self {
            None
        } else {
            super::zones::person_timezone_conn(&conn, account_id, addr)?
        };

        Ok(PersonRecord {
            addr: addr.to_string(),
            display_name,
            is_self,
            known_contact,
            received,
            sent,
            rule,
            groups,
            recent_threads,
            timezone,
        })
    }
}
