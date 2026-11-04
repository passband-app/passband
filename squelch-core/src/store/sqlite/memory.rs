//! `memory_entries`: the owner profile (docs/UNDERSTANDING.md §2.3). The
//! [`MemoryStore`] reads and the pass's write, the profile pass's input source,
//! and the review page's inherent list/edit/delete.
//!
//! HUMAN DOOR ONLY. Triage (the human door's backend) and `/client/memory`
//! read this table; `/mcp` never does. The inherent methods are not on the
//! trait for the reason `groups.rs` states.
//!
//! EVERY TIMESTAMP IS `to_rfc3339()` OF A UTC INSTANT, the format `messages`
//! uses, so the lexical comparisons below (`expires_at > ?now`,
//! `received_at > ?since`) compare instants.

use super::attention::{dwell_ledger_since, open_ledger_since};
use super::messages::is_known_contact_conn;
use super::reading_senders::SENDER_COUNTS_SQL;
use super::*;
use crate::store::memory::MemoryStore;
use crate::triage::memory::{
    self, Facts, MAX_LINE_CHARS, MemoryBody, MemoryRecord, Origin, RawOp, Rejection, ServedRow,
    ValidEntry, ValidOp,
};
use crate::triage::profile::{
    self, ApplyReport, Attendee, ContactRow, GroupRow, LowAttentionSender, ProfileInput,
    ProfileInputOptions, ProfileInputSource, RepliedThread,
};
use crate::triage::sender_fatigue::{self, SenderSignals};
use chrono::Duration;
use std::collections::BTreeSet;

const COLUMNS: &str = "id, scope, origin, kind, body, line, provenance, confidence, revision, \
                       created_at, updated_at, affirmed_at, expires_at, retired_at, deleted_at";

/// Rows one triage read may consider before `select_for_context` caps them.
/// The live cap is 40, so this only bites on a store that broke it.
const CONTEXT_ROW_LIMIT: usize = 200;
/// How far back the low-attention read looks, before the ledger floor shortens
/// it: the Reading purge's own window.
const ATTENTION_WINDOW_DAYS: i64 = 90;
/// Senders the low-attention read measures, busiest first. Each is three
/// indexed seeks, the purge read's own bound (`reading_senders::MAX_SENDERS`).
const ATTENTION_CANDIDATES: usize = 100;
/// Owner replies the thread read considers before it has its thirty threads.
const REPLY_CANDIDATES: usize = 120;

fn instant(raw: Option<String>) -> Option<DateTime<Utc>> {
    raw.and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|d| d.with_timezone(&Utc))
}

fn record_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryRecord> {
    let body: String = row.get(4)?;
    let provenance: String = row.get(6)?;
    // A timestamp that will not parse reads as the epoch for the three that
    // are always set (the row stays visible and ages out), and as absent for
    // the optional ones EXCEPT `deleted_at`, which is checked in SQL anyway.
    let epoch = DateTime::<Utc>::UNIX_EPOCH;
    Ok(MemoryRecord {
        id: row.get(0)?,
        scope: row.get(1)?,
        origin: Origin::parse(&row.get::<_, String>(2)?),
        kind: row.get(3)?,
        body: serde_json::from_str(&body).unwrap_or(serde_json::Value::Null),
        line: row.get(5)?,
        provenance: serde_json::from_str(&provenance).unwrap_or_default(),
        confidence: row.get(7)?,
        revision: row.get(8)?,
        created_at: instant(row.get(9)?).unwrap_or(epoch),
        updated_at: instant(row.get(10)?).unwrap_or(epoch),
        affirmed_at: instant(row.get(11)?).unwrap_or(epoch),
        expires_at: instant(row.get(12)?),
        retired_at: instant(row.get(13)?),
        deleted_at: instant(row.get(14)?),
    })
}

fn json_text<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| CoreError::Other(e.into()))
}

fn entries_conn(conn: &Connection, account_id: AccountId) -> Result<Vec<MemoryRecord>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM memory_entries
         WHERE account_id = ?1 AND deleted_at IS NULL
         ORDER BY id DESC"
    ))?;
    let rows = stmt
        .query_map(params![account_id], record_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn entry_conn(conn: &Connection, account_id: AccountId, id: &str) -> Result<Option<MemoryRecord>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM memory_entries
                 WHERE account_id = ?1 AND id = ?2 AND deleted_at IS NULL"
            ),
            params![account_id, id],
            record_row,
        )
        .optional()?)
}

/// Which of `addrs` the owner has written to or heard from: in `contacts` or
/// `senders` for this account. The ledger the design names; the relationship
/// table joins this check when it lands.
fn known_addrs_conn(
    conn: &Connection,
    account_id: AccountId,
    addrs: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT EXISTS(SELECT 1 FROM contacts WHERE account_id = ?1 AND addr = ?2 COLLATE NOCASE)
             OR EXISTS(SELECT 1 FROM senders WHERE account_id = ?1 AND addr = ?2 COLLATE NOCASE)",
    )?;
    let mut known = BTreeSet::new();
    for addr in addrs {
        if stmt.query_row(params![account_id, addr], |r| r.get::<_, bool>(0))? {
            known.insert(addr.clone());
        }
    }
    Ok(known)
}

/// Which of `ids` are this account's own, not provider spam, and not kept
/// close by the strict reader answer (`sealed_mail_sql!`: auth, restricted,
/// or still pending its access check). The evidence-ownership check decision
/// citations get (`agent::validate_decision`), done in SQL because a profile
/// outlives the run that wrote it.
fn owned_ids_conn(
    conn: &Connection,
    account_id: AccountId,
    ids: &BTreeSet<i64>,
) -> Result<BTreeSet<i64>> {
    let mut stmt = conn.prepare(concat!(
        "SELECT EXISTS(SELECT 1 FROM messages m LEFT JOIN triage t ON t.message_id = m.id
          WHERE m.account_id = ?1 AND m.id = ?2 AND m.is_spam = 0 AND NOT ",
        sealed_mail_sql!(),
        ")"
    ))?;
    let mut owned = BTreeSet::new();
    for id in ids {
        if stmt.query_row(params![account_id, id], |r| r.get::<_, bool>(0))? {
            owned.insert(*id);
        }
    }
    Ok(owned)
}

fn rfc(at: DateTime<Utc>) -> String {
    at.to_rfc3339()
}

fn insert_entry(
    tx: &rusqlite::Transaction<'_>,
    account_id: AccountId,
    entry: &ValidEntry,
    now: DateTime<Utc>,
) -> Result<String> {
    let id = memory::new_ulid(now);
    tx.execute(
        "INSERT INTO memory_entries(account_id, id, scope, origin, kind, body, line, provenance,
             confidence, revision, created_at, updated_at, affirmed_at, expires_at)
         VALUES(?1, ?2, ?3, 'inferred', ?4, ?5, ?6, ?7, ?8, 1, ?9, ?9, ?9, ?10)",
        params![
            account_id,
            id,
            entry.scope,
            entry.body.kind().as_str(),
            json_text(&entry.body.fields())?,
            entry.body.render_line(),
            json_text(&entry.provenance)?,
            entry.confidence,
            rfc(now),
            rfc(memory::inferred_expiry(now)),
        ],
    )?;
    Ok(id)
}

impl MemoryStore for SqliteStore {
    fn memory_context_rows(
        &self,
        account_id: AccountId,
        scopes: &[String],
        now: DateTime<Utc>,
    ) -> Result<Vec<ServedRow>> {
        let conn = self.lock()?;
        let placeholders = (0..scopes.len())
            .map(|i| format!("?{}", i + 3))
            .collect::<Vec<_>>()
            .join(",");
        let scoped = if scopes.is_empty() {
            "0".to_string()
        } else {
            format!("scope IN ({placeholders})")
        };
        let sql = format!(
            "SELECT {COLUMNS} FROM memory_entries
             WHERE account_id = ?1 AND deleted_at IS NULL AND retired_at IS NULL
               AND (expires_at IS NULL OR expires_at > ?2)
               AND ({scoped} OR scope LIKE 'kind:%')
             ORDER BY id DESC LIMIT {CONTEXT_ROW_LIMIT}"
        );
        let now_text = rfc(now);
        let mut binds: Vec<&dyn rusqlite::ToSql> = vec![&account_id, &now_text];
        binds.extend(scopes.iter().map(|s| s as &dyn rusqlite::ToSql));
        let records = conn
            .prepare(&sql)?
            .query_map(binds.as_slice(), record_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        // The newest receipt per entry, by primary key: what "old" is
        // measured against. Messages no longer stored drop out of the MAX.
        let mut newest = conn.prepare(
            "SELECT MAX(received_at) FROM messages
             WHERE account_id = ?1 AND id IN (SELECT value FROM json_each(?2))",
        )?;
        let mut out = Vec::with_capacity(records.len());
        for record in records {
            let at = if record.provenance.message_ids.is_empty() {
                None
            } else {
                let ids = json_text(&record.provenance.message_ids)?;
                instant(newest.query_row(params![account_id, ids], |r| r.get(0))?)
            };
            out.push((record, at));
        }
        Ok(out)
    }

    fn memory_entries(&self, account_id: AccountId) -> Result<Vec<MemoryRecord>> {
        entries_conn(&*self.lock()?, account_id)
    }

    fn memory_coverage_days(&self, account_id: AccountId, now: DateTime<Utc>) -> Result<f32> {
        let since = dwell_ledger_since(&*self.lock()?, account_id)?;
        Ok(((now - since).num_seconds().max(0) as f64 / 86_400.0) as f32)
    }

    fn profile_changed_since(
        &self,
        account_id: AccountId,
        since: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let Some(since) = since else {
            return Ok(true);
        };
        let conn = self.lock()?;
        let since_text = rfc(since);
        // A REPLY: the owner's own mail since then, on a thread that carries
        // somebody else's earlier message. Walks the owner's recent mail by
        // the received index; a quiet day is a short walk.
        let replied: bool = conn.query_row(
            concat!(
                "SELECT EXISTS(SELECT 1 FROM messages m INDEXED BY idx_messages_received
                  WHERE m.account_id = ?1 AND m.received_at > ?2 AND ",
                own_mail_sql!(),
                " AND EXISTS(SELECT 1 FROM messages i INDEXED BY idx_messages_thread
                       WHERE i.account_id = m.account_id AND i.thread_id = m.thread_id
                         AND i.id != m.id AND i.is_sent = 0
                         AND lower(trim(i.from_addr)) != COALESCE(
                             (SELECT lower(trim(email)) FROM accounts WHERE id = m.account_id), '')))"
            ),
            params![account_id, since_text],
            |r| r.get(0),
        )?;
        if replied {
            return Ok(true);
        }
        let corrected: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM triage_feedback WHERE account_id = ?1 AND corrected_at > ?2)
                 OR EXISTS(SELECT 1 FROM agent_triage_corrections
                           WHERE account_id = ?1 AND updated_at > ?2)",
            params![account_id, since_text],
            |r| r.get(0),
        )?;
        if corrected {
            return Ok(true);
        }
        Ok(profile::any_expiring(
            &entries_conn(&conn, account_id)?,
            now,
        ))
    }

    fn apply_profile_ops(
        &self,
        account_id: AccountId,
        ops: &[RawOp],
        exposed: &BTreeSet<i64>,
        now: DateTime<Utc>,
    ) -> Result<ApplyReport> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let existing = entries_conn(&tx, account_id)?;
        let (addrs, mut ids) = memory::mentioned(ops);
        // A revise may re-cite its entry's own receipts; their ownership is
        // re-checked here with the rest.
        ids.extend(
            existing
                .iter()
                .flat_map(|r| r.provenance.message_ids.iter().copied()),
        );
        let known = known_addrs_conn(&tx, account_id, &addrs)?;
        let owned = owned_ids_conn(&tx, account_id, &ids)?;
        let validated = memory::validate_ops(
            ops,
            &Facts {
                existing: &existing,
                known_addrs: &known,
                owned_ids: &owned,
                exposed_ids: exposed,
                now,
            },
        );
        let mut report = ApplyReport {
            rejected: validated.rejected,
            ..Default::default()
        };
        let affirmed = rfc(now);
        let expires = rfc(memory::inferred_expiry(now));
        for op in &validated.ops {
            // `origin = 'inferred'` ON EVERY WRITE, though the validator has
            // already refused stated targets: the one rule this table exists
            // to keep is held by the SQL too.
            match op {
                ValidOp::Keep { id } => {
                    tx.execute(
                        "UPDATE memory_entries SET affirmed_at = ?3, expires_at = ?4
                         WHERE account_id = ?1 AND id = ?2 AND origin = 'inferred'
                           AND deleted_at IS NULL",
                        params![account_id, id, affirmed, expires],
                    )?;
                    report.kept += 1;
                }
                ValidOp::Revise { id, entry } => {
                    tx.execute(
                        "UPDATE memory_entries SET scope = ?3, body = ?4, line = ?5,
                             provenance = ?6, confidence = ?7, revision = revision + 1,
                             updated_at = ?8, affirmed_at = ?8, expires_at = ?9
                         WHERE account_id = ?1 AND id = ?2 AND origin = 'inferred'
                           AND deleted_at IS NULL",
                        params![
                            account_id,
                            id,
                            entry.scope,
                            json_text(&entry.body.fields())?,
                            entry.body.render_line(),
                            json_text(&entry.provenance)?,
                            entry.confidence,
                            affirmed,
                            expires,
                        ],
                    )?;
                    report.revised += 1;
                }
                ValidOp::Retire { id } => {
                    tx.execute(
                        "UPDATE memory_entries SET retired_at = ?3, updated_at = ?3
                         WHERE account_id = ?1 AND id = ?2 AND origin = 'inferred'
                           AND deleted_at IS NULL",
                        params![account_id, id, affirmed],
                    )?;
                    report.retired += 1;
                }
                ValidOp::New { entry } => {
                    insert_entry(&tx, account_id, entry, now)?;
                    report.added += 1;
                }
            }
        }
        tx.commit()?;
        Ok(report)
    }
}

impl ProfileInputSource for SqliteStore {
    /// Today's source: contacts, senders, groups, calendar guests and the
    /// owner's replies, each bounded and each an indexed read. ONE LOCK for
    /// the whole read, like the purge read: it runs once a day.
    fn profile_input(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
        options: ProfileInputOptions,
    ) -> Result<ProfileInput> {
        let conn = self.lock()?;
        let me: String = conn
            .query_row(
                "SELECT lower(trim(email)) FROM accounts WHERE id = ?1",
                params![account_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_default();
        Ok(ProfileInput {
            contacts: top_contacts(&conn, account_id, &me)?,
            low_attention_senders: low_attention_senders(&conn, account_id, &me, now)?,
            send_groups: groups(&conn, account_id)?,
            meeting_attendees: if options.calendar {
                attendees(&conn, account_id, now)?
            } else {
                Vec::new()
            },
            replied_threads: replied_threads(&conn, account_id, &me, now)?,
            ..Default::default()
        })
    }
}

fn top_contacts(conn: &Connection, account_id: AccountId, me: &str) -> Result<Vec<ContactRow>> {
    let mut stmt = conn.prepare(
        "SELECT lower(addr), display_name, sent_count, last_sent_at FROM contacts
         WHERE account_id = ?1 AND lower(addr) != ?2 AND sent_count > 0
         ORDER BY sent_count DESC, last_sent_at DESC LIMIT ?3",
    )?;
    let rows = stmt
        .query_map(params![account_id, me, profile::MAX_CONTACTS as i64], |r| {
            Ok(ContactRow {
                addr: r.get(0)?,
                name: r.get(1)?,
                sent_count: r.get(2)?,
                last_sent_at: r.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The senders the owner reads least, by the purge read's own counts
/// (`SENDER_COUNTS_SQL`: inbound, not spam, not sealed, not forged, inside the
/// open ledger) and scored by `sender_fatigue::score` over them. Only senders
/// with enough mail to score; sorted by the share opened, least first.
fn low_attention_senders(
    conn: &Connection,
    account_id: AccountId,
    me: &str,
    now: DateTime<Utc>,
) -> Result<Vec<LowAttentionSender>> {
    let floor =
        (now - Duration::days(ATTENTION_WINDOW_DAYS)).max(open_ledger_since(conn, account_id)?);
    let covered_days = ((now - floor).num_seconds().max(0) as f64 / 86_400.0) as f32;
    let floor_text = floor.to_rfc3339();
    let candidates: Vec<String> = conn
        .prepare(
            "SELECT lower(addr) FROM senders WHERE account_id = ?1 AND lower(addr) != ?2
             ORDER BY msg_count DESC LIMIT ?3",
        )?
        .query_map(params![account_id, me, ATTENTION_CANDIDATES as i64], |r| {
            r.get(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut counts = conn.prepare(SENDER_COUNTS_SQL)?;
    let mut out = Vec::new();
    for addr in candidates {
        let (received, opened, cleared): (u32, u32, u32) = counts
            .query_row(params![account_id, addr, floor_text], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
        if received < sender_fatigue::MIN_SAMPLE {
            continue;
        }
        let signals = SenderSignals {
            received,
            opened,
            cleared_unopened: cleared,
            dwell_sample: 0,
            median_dwell_ms: None,
            clicks: 0,
            read_sample: 0,
            median_read_fraction: None,
            median_words: None,
            per_month: sender_fatigue::per_month(received, covered_days),
            covered_days,
            known_contact: is_known_contact_conn(conn, account_id, &addr)?,
            surface_rule: false,
            kept_recently: false,
        };
        let (score, reasons) = sender_fatigue::score(&signals);
        out.push(LowAttentionSender {
            addr,
            received,
            opened,
            cleared_unopened: cleared,
            fatigue_score: score,
            reasons,
        });
    }
    out.sort_by(|a, b| {
        let share = |s: &LowAttentionSender| s.opened as f64 / s.received.max(1) as f64;
        share(a)
            .total_cmp(&share(b))
            .then(b.fatigue_score.cmp(&a.fatigue_score))
            .then(a.addr.cmp(&b.addr))
    });
    out.truncate(profile::MAX_LOW_ATTENTION);
    Ok(out)
}

fn groups(conn: &Connection, account_id: AccountId) -> Result<Vec<GroupRow>> {
    let mut stmt = conn.prepare(
        "SELECT g.id, g.name, (SELECT COUNT(*) FROM group_members gm WHERE gm.group_id = g.id)
         FROM send_groups g WHERE g.account_id = ?1 ORDER BY g.updated_at DESC LIMIT ?2",
    )?;
    let heads = stmt
        .query_map(params![account_id, profile::MAX_GROUPS as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut members = conn.prepare(
        "SELECT lower(addr) FROM group_members WHERE group_id = ?1 AND account_id = ?2
         ORDER BY added_at, addr LIMIT ?3",
    )?;
    let mut out = Vec::new();
    for (id, name, count) in heads {
        out.push(GroupRow {
            name,
            member_count: count.max(0) as usize,
            members: members
                .query_map(
                    params![id, account_id, profile::MAX_GROUP_MEMBERS_SHOWN as i64],
                    |r| r.get(0),
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?,
        });
    }
    Ok(out)
}

/// Who the owner met in the last thirty days, by events counted, the owner
/// themselves left out.
fn attendees(
    conn: &Connection,
    account_id: AccountId,
    now: DateTime<Utc>,
) -> Result<Vec<Attendee>> {
    let from = (now - Duration::days(profile::LOOKBACK_DAYS))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let to = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut stmt = conn.prepare(
        "SELECT lower(g.email), COUNT(DISTINCT e.calendar_id || '/' || e.event_id)
         FROM calendar_events e
         JOIN calendar_event_guests g ON g.account_id = e.account_id
              AND g.calendar_id = e.calendar_id AND g.event_id = e.event_id
         WHERE e.account_id = ?1 AND e.start_sort >= ?2 AND e.start_sort <= ?3
           AND g.is_self = 0 AND g.email != ''
         GROUP BY lower(g.email) ORDER BY 2 DESC, 1 LIMIT ?4",
    )?;
    let rows = stmt
        .query_map(
            params![account_id, from, to, profile::MAX_ATTENDEES as i64],
            |r| {
                Ok(Attendee {
                    addr: r.get(0)?,
                    meetings: r.get(1)?,
                })
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The threads the owner answered in the last thirty days, newest reply
/// first, one per thread: subject, participants, and the owner's own words.
///
/// SEALED AND AUTH MAIL ARE STRUCTURALLY ABSENT: a thread holding ANY message
/// the strict reader answer keeps close (`sealed_mail_sql!`: auth, restricted,
/// pending its access check) is skipped whole, the reply included, so neither
/// a login code's subject nor a reply that quotes one can reach the prompt.
/// Provider spam and archive-tier rows are skipped too.
fn replied_threads(
    conn: &Connection,
    account_id: AccountId,
    me: &str,
    now: DateTime<Utc>,
) -> Result<Vec<RepliedThread>> {
    let since = (now - Duration::days(profile::LOOKBACK_DAYS)).to_rfc3339();
    let mut replies = conn.prepare(concat!(
        "SELECT m.id, m.thread_id, m.subject, m.received_at, m.body
         FROM messages m INDEXED BY idx_messages_received
         WHERE m.account_id = ?1 AND m.received_at >= ?2 AND m.is_spam = 0 AND m.is_archive = 0
           AND ",
        own_mail_sql!(),
        " ORDER BY m.received_at DESC LIMIT ?3"
    ))?;
    let candidates = replies
        .query_map(params![account_id, since, REPLY_CANDIDATES as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    // One indexed probe per candidate thread: who else wrote on it, and
    // whether anything on it is kept close.
    let mut thread = conn.prepare(concat!(
        "SELECT lower(trim(m.from_addr)), ",
        own_mail_sql!(),
        ", ",
        sealed_mail_sql!(),
        " OR m.is_spam = 1
         FROM messages m INDEXED BY idx_messages_thread
         LEFT JOIN triage t ON t.message_id = m.id
         WHERE m.account_id = ?1 AND m.thread_id = ?2"
    ))?;
    let mut recipients = conn.prepare(
        "SELECT addr FROM message_recipients WHERE account_id = ?1 AND message_id = ?2
         ORDER BY addr",
    )?;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for (id, thread_id, subject, received_at, body) in candidates {
        if out.len() >= profile::MAX_REPLIED_THREADS {
            break;
        }
        if !seen.insert(thread_id.clone()) {
            continue;
        }
        let rows = thread
            .query_map(params![account_id, thread_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, bool>(1)?,
                    r.get::<_, bool>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if rows.iter().any(|(_, _, sealed)| *sealed) {
            continue;
        }
        let mut participants: Vec<String> = rows
            .iter()
            .filter(|(_, own, _)| !own)
            .map(|(from, _, _)| memory::normalize_addr(from))
            .collect();
        // A reply is to somebody: a thread with no one else's mail on it is
        // the owner writing first, which is not an answer.
        if participants.is_empty() {
            continue;
        }
        participants.extend(
            recipients
                .query_map(params![account_id, id], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?,
        );
        participants.retain(|p| !p.is_empty() && p != me);
        participants.sort();
        participants.dedup();
        participants.truncate(profile::MAX_PARTICIPANTS);
        let Some(owner_reply) = profile::owner_words(&body) else {
            continue;
        };
        out.push(RepliedThread {
            message_id: id,
            subject: crate::text::truncate_chars(
                &memory::clean_field(&subject),
                profile::MAX_SUBJECT_CHARS,
            ),
            participants,
            replied_at: received_at,
            owner_reply,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// The review page (`/client/memory`). Inherent, human door only.
// ---------------------------------------------------------------------------

/// The owner's edit of one entry. Every part optional; an empty edit is a
/// read.
#[derive(Debug, Clone, Default)]
pub struct MemoryEdit {
    /// The owner's own sentence, replacing the rendered line.
    pub line: Option<String>,
    /// New fields for the entry's kind (the kind itself cannot change).
    pub body: Option<serde_json::Value>,
    /// "Yes, that's right": the entry becomes the owner's word.
    pub confirm: bool,
}

impl SqliteStore {
    /// Every entry the owner has not deleted, newest first, retired ones
    /// included so the page can say what Passband no longer thinks.
    pub fn list_memory_entries(&self, account_id: AccountId) -> Result<Vec<MemoryRecord>> {
        entries_conn(&*self.lock()?, account_id)
    }

    /// Apply the owner's edit or confirmation. `None` when there is no such
    /// entry (or it was deleted: a deleted entry is gone to every reader).
    ///
    /// EITHER ONE MAKES THE ENTRY STATED. A confirm says so in as many words;
    /// an edit is the owner writing the entry themselves, and left `inferred`
    /// the pass's next revise would overwrite their words with its guess and
    /// the prompt would weigh their sentence as the daemon's. Stated entries
    /// never expire, so `expires_at` is cleared, and a retired entry the owner
    /// confirms or edits comes back. An edit bumps `revision`; a bare confirm
    /// does not (nothing the line says changed).
    pub fn edit_memory_entry(
        &self,
        account_id: AccountId,
        id: &str,
        edit: &MemoryEdit,
        now: DateTime<Utc>,
    ) -> Result<Option<MemoryRecord>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(current) = entry_conn(&tx, account_id, id)? else {
            return Ok(None);
        };
        let invalid = |why: Rejection| CoreError::InvalidInput(format!("{why:?}"));
        let mut scope = current.scope.clone();
        let mut body = current.body.clone();
        let mut line = current.line.clone();
        let edited = edit.body.is_some() || edit.line.is_some();
        if let Some(fields) = &edit.body {
            let typed = MemoryBody::from_fields(&current.kind, fields, now.date_naive())
                .map_err(invalid)?;
            if let MemoryBody::Person { addr, .. } = &typed {
                // A person entry is scoped to its address, so moving the
                // address moves the scope with it.
                scope = memory::Scope::Sender(addr.clone()).as_string();
            }
            line = typed.render_line();
            body = typed.fields();
        }
        if let Some(raw) = &edit.line {
            let cleaned = memory::clean_field(raw);
            if cleaned.is_empty() {
                return Err(invalid(Rejection::MissingField { field: "line" }));
            }
            if cleaned.chars().count() > MAX_LINE_CHARS {
                return Err(invalid(Rejection::FieldOverCap { field: "line" }));
            }
            // The owner's own words still never carry a link or a code into
            // every triage prompt from here on.
            if memory::forbidden_content(&cleaned) {
                return Err(invalid(Rejection::ForbiddenContent { field: "line" }));
            }
            line = cleaned;
        }
        if edited || edit.confirm {
            tx.execute(
                "UPDATE memory_entries SET origin = 'stated', scope = ?3, body = ?4, line = ?5,
                     revision = revision + ?6, updated_at = ?7, affirmed_at = ?7,
                     expires_at = NULL, retired_at = NULL
                 WHERE account_id = ?1 AND id = ?2 AND deleted_at IS NULL",
                params![
                    account_id,
                    id,
                    scope,
                    json_text(&body)?,
                    line,
                    edited as i64,
                    rfc(now),
                ],
            )?;
        }
        let updated = entry_conn(&tx, account_id, id)?;
        tx.commit()?;
        Ok(updated)
    }

    /// The owner removes an entry, FOR REAL (docs/UNDERSTANDING.md §3): the
    /// sentence, the fields and the receipts are blanked in the same write
    /// that stamps `deleted_at`, so neither the store nor a Litestream backup
    /// taken after it still holds what Passband thought about a person or
    /// which of their mail it rested on. The row itself stays, empty, with its
    /// id, scope, kind and dates, so the audit row has something to point at.
    /// `false` when there was nothing to delete.
    pub fn delete_memory_entry(
        &self,
        account_id: AccountId,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let n = self.lock()?.execute(
            "UPDATE memory_entries SET deleted_at = ?3, updated_at = ?3,
                 line = '', body = '{}', provenance = '{}'
             WHERE account_id = ?1 AND id = ?2 AND deleted_at IS NULL",
            params![account_id, id, rfc(now)],
        )?;
        Ok(n > 0)
    }

    /// Insert one entry as written, for tests and the eval harness: no
    /// validation, any origin.
    #[doc(hidden)]
    pub fn insert_memory_entry_unchecked(
        &self,
        account_id: AccountId,
        record: &MemoryRecord,
    ) -> Result<()> {
        self.lock()?.execute(
            "INSERT INTO memory_entries(account_id, id, scope, origin, kind, body, line, provenance,
                 confidence, revision, created_at, updated_at, affirmed_at, expires_at,
                 retired_at, deleted_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                account_id,
                record.id,
                record.scope,
                record.origin.as_str(),
                record.kind,
                json_text(&record.body)?,
                record.line,
                json_text(&record.provenance)?,
                record.confidence,
                record.revision,
                rfc(record.created_at),
                rfc(record.updated_at),
                rfc(record.affirmed_at),
                record.expires_at.map(rfc),
                record.retired_at.map(rfc),
                record.deleted_at.map(rfc),
            ],
        )?;
        Ok(())
    }
}
