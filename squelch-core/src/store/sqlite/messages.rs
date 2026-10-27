//! Message ingest, thread views, attachments, deadlines, sync cursors
//! and the sealed-mail reads.

use super::specialists::{upsert_calendar_conn, upsert_receipt_conn, upsert_shipment_conn};
use super::*;

/// Apply the unsubscribe VIOLATION bump for a just-stored inbound message, in
/// the caller's transaction: an unresolved `unsubscribes` row for
/// `(account_id, lower(from_addr))` whose request is more than 72h older than
/// this `received_at` gets `violation_count + 1` and `last_violation_at`. No
/// row, already resolved, or still within grace is a silent no-op.
fn bump_unsub_violation_conn(
    conn: &Connection,
    account_id: AccountId,
    from_addr: &str,
    received_at: DateTime<Utc>,
) -> Result<()> {
    let sender = from_addr.trim().to_ascii_lowercase();
    if sender.is_empty() {
        return Ok(());
    }
    // Read the outstanding request so the grace comparison runs on real
    // timestamps in Rust rather than as lexical string math in SQL.
    let row: Option<String> = conn
        .query_row(
            "SELECT requested_at FROM unsubscribes
             WHERE account_id = ?1 AND sender_addr = ?2 AND resolution IS NULL
               AND (status IS NULL OR status IN ('done', 'opened'))",
            params![account_id, sender],
            |r| r.get(0),
        )
        .optional()?;
    let Some(requested_s) = row else {
        return Ok(());
    };
    let requested_at = parse_dt(&requested_s)?;
    if received_at > requested_at + chrono::Duration::hours(72) {
        conn.execute(
            "UPDATE unsubscribes
             SET violation_count = violation_count + 1, last_violation_at = ?3
             WHERE account_id = ?1 AND sender_addr = ?2 AND resolution IS NULL
               AND (status IS NULL OR status IN ('done', 'opened'))",
            params![account_id, sender, received_at.to_rfc3339()],
        )?;
    }
    Ok(())
}

/// A stored ISO 639-1 code as the reader names it ("French"), the code
/// itself for one no detection would have written.
fn language_name(code: &str) -> String {
    crate::translate::Lang::from_code(code)
        .map(|l| l.english_name().to_string())
        .unwrap_or_else(|| code.to_string())
}

/// The human door's translation off a thread row whose columns from `at` are
/// `source_lang, status, subject, body, body_html, missing_facts`. The join
/// admits `deferred`, `pending`, `done` and `incomplete` only; `deferred` and
/// `pending` carry no text (the reader shows the original and says it is
/// translating), and `missing_facts` rides only on `incomplete`.
///
/// `deferred` IS SERVED AS ITSELF, never as `pending`: reading a thread buys
/// nothing (the client prefetches through this read), so a deferred row tells
/// the reader a translation is available, and only the reader DISPLAYING the
/// thread asks for it, over `POST /client/thread/{id}/translate`.
fn client_translation(
    r: &rusqlite::Row<'_>,
    at: usize,
) -> rusqlite::Result<Option<crate::types::ClientTranslation>> {
    let Some(code) = r.get::<_, Option<String>>(at)? else {
        return Ok(None);
    };
    let status: String = r.get(at + 1)?;
    let ready = status == "done" || status == "incomplete";
    let text = |i: usize| -> rusqlite::Result<String> {
        Ok(if ready {
            r.get::<_, Option<String>>(i)?.unwrap_or_default()
        } else {
            String::new()
        })
    };
    let missing_facts = if status == "incomplete" {
        r.get::<_, Option<String>>(at + 5)?
            .and_then(|m| serde_json::from_str::<Vec<String>>(&m).ok())
            .filter(|m| !m.is_empty())
    } else {
        None
    };
    Ok(Some(crate::types::ClientTranslation {
        source_language: language_name(&code),
        source_lang: code,
        subject: text(at + 2)?,
        content: text(at + 3)?,
        html: if ready {
            r.get::<_, Option<String>>(at + 4)?
                .filter(|h| !h.is_empty())
        } else {
            None
        },
        missing_facts,
        status,
    }))
}

/// Upsert a message + FTS + Sent-derived contacts against an explicit
/// connection/transaction handle. Shared by [`SqliteStore::upsert_message`] and
/// the transactional [`Store::ingest_message`] path so both stay in sync.
///
/// `is_sent` is STICKY TO 0 on conflict (`MIN`): a row ever seen as received
/// can never flip to sent. `is_sent=1` removes a message from every listing
/// surface, so the flip would vanish inbound mail on the strength of a single
/// mislabeled sighting — and Gmail's label-filtered history has served inbound
/// mail through a SENT walk in production. The sync engine's walk ordering is
/// the first defense; this clause is the backstop that holds regardless of
/// ingest order or upstream filter quality. The reverse flip (1 -> 0) stays
/// allowed: a message can only gain visibility, never lose it.
///
/// `is_spam` is STICKY TO 0 the same way and for the same shape of reason: a
/// message ever seen outside the spam label must not be hidden by a later
/// sighting inside it. The SPAM walk runs last and subtracts what the other two
/// already returned, so the clause is a backstop rather than the mechanism.
///
/// The three RECIPIENT columns are the ones that PREFER THE STORED VALUE over a
/// NULL (`COALESCE(excluded, messages)`): not every writer parses headers, so a
/// re-fetch that skips them — or an old row the backfill already filled — must
/// not be blanked by the next writer that has no opinion. That is also what
/// keeps a sent message's Bcc list through the INBOX sighting of the same
/// message (a self-Cc, a note to self, a group echo): the received copy has no
/// Bcc to offer and says `None` rather than `Some("")`.
///
/// `bcc_addrs` is additionally DROPPED FOR ANYTHING NOT SENT, in the VALUES
/// clause so the gate covers the update too (`excluded` is the gated row). The
/// ingest path already refuses to read a `Bcc:` header off received mail, where
/// it is the sender's leak or a forgery — this is the store holding the same line
/// against every other caller, present and future, the way
/// `set_message_recipients` does for the backfill sweep. Gating on the INCOMING
/// `is_sent` and not the stored one is what keeps a real Bcc through the sticky
/// `is_sent` flip: the inbound sighting offers nothing here, so the COALESCE
/// keeps what the sent copy wrote.
/// `date_offset_minutes` is the Date header's offset (the timezone
/// inference's observation), written HERE with the rest of the row rather
/// than by a follow-up UPDATE: it is the last column, after the bodies, and
/// changing a record's size afterwards makes SQLite rewrite the whole record
/// and its overflow pages a second time. `None` is "no opinion" and keeps
/// what is stored, the recipients' rule.
fn upsert_message_conn(
    conn: &Connection,
    msg: &NewMessage,
    date_offset_minutes: Option<i32>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO messages(account_id, gmail_msg_id, thread_id, from_addr, from_name,
             subject, received_at, snippet, body, body_html, is_sent, to_addrs,
             cc_addrs, bcc_addrs,
             list_unsubscribe, list_unsub_one_click, auth_pass, is_spam,
             date_offset_minutes)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,
             CASE WHEN ?11 = 1 THEN ?14 ELSE NULL END,
             ?15,?16,?17,?18,?19)
         ON CONFLICT(account_id, gmail_msg_id) DO UPDATE SET
             thread_id=excluded.thread_id, from_addr=excluded.from_addr,
             from_name=excluded.from_name, subject=excluded.subject,
             received_at=excluded.received_at, snippet=excluded.snippet,
             body=excluded.body, body_html=excluded.body_html,
             is_sent=MIN(messages.is_sent, excluded.is_sent),
             to_addrs=COALESCE(excluded.to_addrs, messages.to_addrs),
             cc_addrs=COALESCE(excluded.cc_addrs, messages.cc_addrs),
             bcc_addrs=COALESCE(excluded.bcc_addrs, messages.bcc_addrs),
             list_unsubscribe=excluded.list_unsubscribe,
             list_unsub_one_click=excluded.list_unsub_one_click,
             auth_pass=excluded.auth_pass,
             is_spam=MIN(messages.is_spam, excluded.is_spam),
             date_offset_minutes=COALESCE(excluded.date_offset_minutes,
                                          messages.date_offset_minutes)",
        params![
            msg.account_id,
            msg.gmail_msg_id,
            msg.thread_id,
            msg.from_addr,
            msg.from_name,
            msg.subject,
            msg.received_at.to_rfc3339(),
            msg.snippet,
            msg.body,
            msg.body_html,
            msg.is_sent as i64,
            msg.to_addrs,
            msg.cc_addrs,
            msg.bcc_addrs,
            msg.list_unsubscribe,
            msg.list_unsub_one_click as i64,
            msg.auth_pass.map(|p| p as i64),
            msg.is_spam as i64,
            date_offset_minutes,
        ],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM messages WHERE account_id=?1 AND gmail_msg_id=?2",
        params![msg.account_id, msg.gmail_msg_id],
        |r| r.get(0),
    )?;

    // Keep the FTS index in sync: the row just written, plus its served
    // translation when one exists (a re-sighting of translated mail must not
    // drop the English from the index).
    super::translations::refresh_fts_conn(conn, id)?;

    // The sender directory, for the search field's `from:` menu. Inbound and
    // non-spam only: sent mail's From is the user, and spam is a structural
    // exclusion everywhere else too. Judged on THIS sighting's flags rather
    // than the row's sticky ones on purpose: a message first seen as spam and
    // now seen outside the label has just become visible, and this is the
    // sighting that should register its sender (the count is recomputed from
    // the row, so the sticky flags still decide what it says).
    if !msg.is_sent && !msg.is_spam {
        super::senders::bump_sender_conn(
            conn,
            msg.account_id,
            &msg.from_addr,
            msg.from_name.as_deref(),
            &msg.received_at.to_rfc3339(),
        )?;
    }

    // Contacts are NOT seeded here: Sent mail's From header is the user's own
    // address. They come from the To/Cc recipients in `ingest_message`.
    Ok(id)
}

/// Seed the contacts table from a Sent message's recipients, each bumping its
/// `sent_count`. Addresses arrive de-duplicated and stripped of the account's own
/// address, so only empties are skipped. Received mail passes an empty list.
fn seed_contacts_conn(
    conn: &Connection,
    account_id: AccountId,
    recipients: &[String],
    first_seen: &str,
) -> Result<()> {
    for addr in recipients {
        if addr.trim().is_empty() {
            continue;
        }
        conn.execute(
            "INSERT INTO contacts(account_id, addr, sent_count, first_seen, last_sent_at)
             VALUES(?1,?2,1,?3,?3)
             ON CONFLICT(account_id, addr) DO UPDATE SET
                 sent_count = sent_count + 1,
                 last_sent_at = MAX(COALESCE(last_sent_at,''), ?3)",
            params![account_id, addr, first_seen],
        )?;
    }
    Ok(())
}

/// `statvfs(2)` on `path`: the blocks an unprivileged writer may still take,
/// and the filesystem's whole size, both in bytes. The one platform call
/// behind the archive walk's disk floor.
#[cfg(unix)]
#[allow(clippy::unnecessary_cast, clippy::useless_conversion)]
fn volume_bytes_at(path: &std::path::Path) -> Result<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| CoreError::InvalidInput("database path holds a NUL byte".to_string()))?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c_path` is a NUL-terminated string that outlives the call, and
    // `stat` is writable storage of exactly the type the call fills in.
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if rc != 0 {
        return Err(CoreError::Other(anyhow::anyhow!(
            "statvfs: {}",
            std::io::Error::last_os_error().kind()
        )));
    }
    // SAFETY: a zero return means the call initialised the struct.
    let stat = unsafe { stat.assume_init() };
    // Widths differ by platform (32-bit block counts on macOS), hence the
    // casts and the allow above.
    let block = stat.f_frsize as u64;
    Ok((
        (stat.f_bavail as u64).saturating_mul(block),
        (stat.f_blocks as u64).saturating_mul(block),
    ))
}

/// Nowhere else is there a `statvfs`, so nowhere else is the number known.
#[cfg(not(unix))]
fn volume_bytes_at(_path: &std::path::Path) -> Result<(u64, u64)> {
    Err(CoreError::Other(anyhow::anyhow!(
        "free disk space is not readable on this platform"
    )))
}

/// Replace this message's attachment rows, in the caller's transaction.
/// DELETE-then-INSERT keeps re-ingest idempotent; `data == None` writes a NULL
/// blob (over-cap, metadata only) while `size_bytes` stays the real decoded size.
/// Written for sealed mail too — the byte-serving path guards sealed parents.
/// `keep_bytes == false` writes every blob NULL, whatever the part's size: the
/// archive tier keeps a year of attachments as names and sizes only.
///
/// INSERT OR IGNORE, not plain INSERT: a sender can attach the same file twice,
/// and a UNIQUE(account,message,filename,size) violation would roll back the
/// ENTIRE message ingest — a remote ingest DoS. Collapsing identical duplicates
/// to one row is the wanted outcome.
fn insert_attachments_conn(
    conn: &Connection,
    account_id: AccountId,
    message_id: i64,
    attachments: &[AttachmentInfo],
    keep_bytes: bool,
) -> Result<()> {
    conn.execute(
        "DELETE FROM attachments WHERE account_id=?1 AND message_id=?2",
        params![account_id, message_id],
    )?;
    for a in attachments {
        conn.execute(
            "INSERT OR IGNORE INTO attachments(account_id, message_id, filename, mime, size_bytes, data, content_id)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                account_id,
                message_id,
                a.filename,
                a.mime,
                a.size_bytes,
                a.data.as_deref().filter(|_| keep_bytes),
                a.content_id,
            ],
        )?;
    }
    Ok(())
}

/// [`external_messages_allowed_conn`]: whether EVERY message of account `?1`
/// named in the JSON array `?2` may go out the agent door. Each message's
/// current assessment is checked, and so is every source its decision
/// consumed, transitively. The check fails closed: on a message that does not
/// exist, an assessment that is unknown, pending or restricted, an owner's
/// seal (an `external_access` correction), or a source whose revision has
/// moved since the decision read it.
///
/// ONE WALK FOR THE WHOLE SET. Each condition is "no bad message (or stale
/// edge) anywhere in what this message depends on", so "all of these are
/// allowed" is the same question asked of the union of their dependencies.
/// A thread's siblings mostly depend on each other, so the guard over a
/// ten-message thread walks the thread once instead of ten times.
///
/// PINNED ORDER. `CROSS JOIN` keeps the walk's row outside each join onto
/// `agent_decision_sources`, which is then a seek on its primary key. Without
/// `sqlite_stat1` the planner is free to reach the table first by `account_id`
/// alone and probe the walk from it (`queue_dependent_refreshes` measured what
/// that costs). `agent_triage::tests::set_access_check_seeks_every_table`
/// pins the plan.
pub(super) const EXTERNAL_MESSAGES_ALLOWED_SQL: &str =
    "WITH RECURSIVE dependencies(message_id) AS (
         SELECT value FROM json_each(?2)
         UNION
         SELECT d.source_message_id FROM dependencies p
         CROSS JOIN agent_decision_sources d ON d.account_id=?1 AND d.message_id=p.message_id
     )
     SELECT EXISTS(SELECT 1 FROM json_each(?2))
       AND NOT EXISTS(
         SELECT 1 FROM dependencies p
         LEFT JOIN messages m ON m.id=p.message_id AND m.account_id=?1
         LEFT JOIN agent_message_state a ON a.message_id=p.message_id AND a.account_id=?1
         WHERE m.id IS NULL OR a.access IS NULL OR a.access!='allowed'
           OR EXISTS(SELECT 1 FROM agent_triage_corrections c WHERE c.account_id=?1
               AND c.message_id=p.message_id AND c.field='external_access' AND c.value_json='true')
       )
       AND NOT EXISTS(
         SELECT 1 FROM dependencies p
         CROSS JOIN agent_decision_sources d ON d.account_id=?1 AND d.message_id=p.message_id
         LEFT JOIN agent_message_state a ON a.message_id=d.source_message_id AND a.account_id=?1
         WHERE a.revision IS NULL OR a.revision!=d.source_revision
       )";

/// May every one of `message_ids` go out the agent door? See
/// [`EXTERNAL_MESSAGES_ALLOWED_SQL`]. An empty set is NOT allowed, here and in
/// the statement itself: a caller that ends up asking about nothing has lost
/// track of what it is serving.
pub(super) fn external_messages_allowed_conn(
    conn: &Connection,
    account_id: AccountId,
    message_ids: &[i64],
) -> Result<bool> {
    if message_ids.is_empty() {
        return Ok(false);
    }
    let seeds = serde_json::to_string(message_ids).map_err(|e| CoreError::Other(e.into()))?;
    Ok(conn
        .prepare_cached(EXTERNAL_MESSAGES_ALLOWED_SQL)?
        .query_row(params![account_id, seeds], |row| row.get(0))?)
}

/// Check the current assessment and every consumed source. Unknown assessments
/// and changed sources fail closed, including dependencies of derived summaries.
pub(super) fn external_message_allowed_conn(
    conn: &Connection,
    account_id: AccountId,
    message_id: i64,
) -> Result<bool> {
    external_messages_allowed_conn(conn, account_id, &[message_id])
}

/// The thread's messages as an external reader may count them: everything but
/// unassessed archive rows. `?1` account, `?2` thread.
const EXTERNAL_THREAD_IDS_SQL: &str = concat!(
    "SELECT m.id FROM messages m
     LEFT JOIN agent_message_state a ON a.account_id=m.account_id AND a.message_id=m.id
     WHERE m.account_id=?1 AND m.thread_id=?2 AND NOT ",
    archive_unassessed_sql!()
);

/// [`THREAD_SUBJECT_SQL`] for an external reader: the oldest message that
/// reader can see titles the thread, never an unassessed archive ancestor.
const EXTERNAL_THREAD_SUBJECT_SQL: &str = concat!(
    "SELECT m.subject FROM messages m
     LEFT JOIN agent_message_state a ON a.account_id=m.account_id AND a.message_id=m.id
     WHERE m.account_id=?1 AND m.thread_id=?2 AND NOT ",
    archive_unassessed_sql!(),
    " ORDER BY m.received_at ASC LIMIT 1"
);

/// [`EXTERNAL_THREAD_IDS_SQL`]: the siblings the thread guard asks about.
pub(super) fn external_thread_ids_conn(
    conn: &Connection,
    account_id: AccountId,
    thread_id: &str,
) -> Result<Vec<i64>> {
    Ok(conn
        .prepare_cached(EXTERNAL_THREAD_IDS_SQL)?
        .query_map(params![account_id, thread_id], |row| row.get::<_, i64>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?)
}

/// THE THREAD GUARD, as one walk: may every sibling of `thread_id` that an
/// external reader counts ([`EXTERNAL_THREAD_IDS_SQL`]), and every one of
/// `also`, go out the agent door? A thread with no such sibling is not found.
/// [`thread_guard_and_subject`] is this with nothing else to ask about; the
/// listings and the per-message reads that also guard the message's thread
/// pass their own messages in `also`, so the guard has one definition however
/// it is asked.
pub(super) fn thread_allowed_with_conn(
    conn: &Connection,
    account_id: AccountId,
    thread_id: &str,
    also: &[i64],
) -> Result<bool> {
    let mut ids = external_thread_ids_conn(conn, account_id, thread_id)?;
    if ids.is_empty() {
        return Ok(false);
    }
    ids.extend_from_slice(also);
    external_messages_allowed_conn(conn, account_id, &ids)
}

/// Judges the rows of one listing the way the agent door serves them, under
/// the lock the caller holds: a row passes when its thread passes the guard
/// together with the ids `half` names for that thread (`None` refuses the
/// thread outright: the listings use it for attention read at a revision that
/// has moved), and the row's own messages are allowed.
///
/// A thread with one row is one walk. A thread with more is judged once and
/// each of its rows then walks its own messages alone: "all of A and B" is
/// "all of A" and "all of B". A judge is for one lock hold; a verdict kept
/// past it would answer for a store that may have changed since.
pub(super) struct ListingJudge {
    repeated: std::collections::HashMap<String, Option<bool>>,
}

impl ListingJudge {
    /// A judge for rows in these threads, one entry per row.
    pub(super) fn new<'a>(threads: impl IntoIterator<Item = &'a str>) -> Self {
        let mut rows: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for thread in threads {
            *rows.entry(thread).or_default() += 1;
        }
        Self {
            repeated: rows
                .into_iter()
                .filter(|&(_, rows)| rows > 1)
                .map(|(thread, _)| (thread.to_string(), None))
                .collect(),
        }
    }

    pub(super) fn allowed(
        &mut self,
        conn: &Connection,
        account_id: AccountId,
        thread: &str,
        own: &[i64],
        half: impl Fn(&str) -> Result<Option<Vec<i64>>>,
    ) -> Result<bool> {
        let Some(verdict) = self.repeated.get_mut(thread) else {
            let Some(mut ids) = half(thread)? else {
                return Ok(false);
            };
            ids.extend_from_slice(own);
            return thread_allowed_with_conn(conn, account_id, thread, &ids);
        };
        let thread_ok = match *verdict {
            Some(known) => known,
            None => *verdict.insert(match half(thread)? {
                None => false,
                Some(ids) => thread_allowed_with_conn(conn, account_id, thread, &ids)?,
            }),
        };
        Ok(thread_ok && external_messages_allowed_conn(conn, account_id, own)?)
    }
}

/// External full-thread reads require every sibling to be allowed, except an
/// unassessed archive sibling, which is omitted instead (see
/// `archive_unassessed_sql!`). A thread of nothing but those is not found.
/// The human reader bypasses this guard and may open pending or restricted mail.
pub(super) fn thread_guard_and_subject(
    conn: &Connection,
    account_id: AccountId,
    thread_id: &str,
) -> Result<String> {
    if !thread_allowed_with_conn(conn, account_id, thread_id, &[])? {
        return Err(CoreError::NotFound);
    }
    conn.query_row(
        EXTERNAL_THREAD_SUBJECT_SQL,
        params![account_id, thread_id],
        |row| row.get(0),
    )
    .optional()?
    .ok_or(CoreError::NotFound)
}

/// Rows per store lock in [`SqliteStore::external_listing_allowed`]: about
/// 8 ms of checks on 8-11 message threads, with or without statistics.
const LISTING_CHECK_CHUNK: usize = 64;

/// The most access checks one thread open asks for, live and archive together.
const THREAD_ACCESS_BATCH: usize = 8;

/// Only an explicit external thread-open request may schedule source assessments.
/// Permission probes for human cache policy, listings and search remain pure.
///
/// `archive_caps` is what the caller may spend on the thread's ARCHIVE
/// siblings. `None` queues nothing for them: that is every read that is not a
/// door's own "open this thread" step (`thread_view` itself, and so the reply
/// target check behind create_draft and send_email), because an archive check
/// is only ever bought inside the surfacing ceilings.
pub(super) fn request_external_thread_access_conn(
    conn: &Connection,
    account_id: AccountId,
    thread_id: &str,
    archive_caps: Option<crate::store::ArchiveAccessCaps>,
    now: DateTime<Utc>,
) -> Result<Vec<i64>> {
    // Reading a legacy thread is demand for assessing its unseen sources. Queue
    // a small batch, never every ancestor at arrival. Existing assessments and
    // active or terminal work remain untouched; access stays closed until every source passes.
    let mut missing: Vec<i64> = conn
        .prepare(
            "SELECT m.id FROM messages m
         LEFT JOIN agent_message_state a ON a.account_id=m.account_id AND a.message_id=m.id
         WHERE m.account_id=?1 AND m.thread_id=?2 AND m.is_archive=0
           AND (a.access IS NULL OR a.access='pending')
           AND NOT EXISTS(SELECT 1 FROM agent_triage_jobs j
             WHERE j.account_id=m.account_id AND j.message_id=m.id
               AND j.kind IN ('triage','access') AND j.input_revision=a.revision)
         ORDER BY m.received_at DESC,m.id DESC LIMIT ?3",
        )?
        .query_map(
            params![account_id, thread_id, THREAD_ACCESS_BATCH as i64],
            |row| row.get(0),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    super::agent_triage::ensure_agent_source_ids_conn(conn, account_id, &missing)?;
    let room = THREAD_ACCESS_BATCH.saturating_sub(missing.len());
    let Some(caps) = archive_caps.filter(|_| room > 0) else {
        return Ok(missing);
    };
    // ARCHIVE SIBLINGS GO THROUGH THE SURFACING CEILINGS, never the path
    // above: opening a thread is the same purchase as a search matching it,
    // and thread ids are cheap to come by. Candidates are the siblings a
    // check can still be bought for (`archive_askable_sql!`), a FAILED one
    // included: asked for again once a day old, or the row would be omitted
    // from this thread for good.
    let archive: Vec<i64> = conn
        .prepare(concat!(
            "SELECT m.id FROM messages m
             LEFT JOIN agent_message_state a ON a.account_id=m.account_id AND a.message_id=m.id
             WHERE m.account_id=?1 AND m.thread_id=?2 AND ",
            archive_askable_sql!(),
            " ORDER BY m.received_at DESC,m.id DESC LIMIT 64"
        ))?
        .query_map(params![account_id, thread_id], |row| row.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    missing.extend(super::agent_triage::request_archive_access_conn(
        conn, account_id, &archive, room, caps, now,
    )?);
    Ok(missing)
}

// Shared with the query-plan regression test: LIMIT 1 must seek the thread,
// not walk the account's received_at index looking for its first message.
pub(super) const THREAD_SUBJECT_SQL: &str = "SELECT subject FROM messages
    WHERE account_id=?1 AND thread_id=?2
    ORDER BY received_at ASC LIMIT 1";

/// Replace this message's `deadlines` row, in the caller's transaction:
/// DELETE-then-INSERT so a re-apply/re-ingest is idempotent, and `None` simply
/// leaves the message dateless. Shared by ingest and both stage applies, whose
/// callers own the sealed guard (a sealed message must never grow a deadline).
pub(super) fn rewrite_deadline_conn(
    conn: &Connection,
    account_id: AccountId,
    message_id: i64,
    deadline: Option<&crate::triage::DeadlineHit>,
) -> Result<()> {
    conn.execute(
        "DELETE FROM deadlines WHERE message_id=?1",
        params![message_id],
    )?;
    if let Some(d) = deadline {
        conn.execute(
            "INSERT INTO deadlines(account_id, message_id, kind, amount, currency,
                 due_at, past_due, source)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                account_id,
                message_id,
                d.kind,
                d.amount,
                d.currency,
                d.due_at.to_rfc3339(),
                d.past_due as i64,
                d.source,
            ],
        )?;
    }
    Ok(())
}

/// Load one message's attachment metadata (NO bytes) as the human-door wire
/// shape, ordered by row id; `downloadable` is `data IS NOT NULL`. The CALLER
/// owns the sealed guard.
fn load_client_attachments_conn(
    conn: &Connection,
    account_id: AccountId,
    message_id: i64,
) -> Result<Vec<ClientAttachment>> {
    let mut stmt = conn.prepare(
        "SELECT id, filename, mime, size_bytes, data IS NOT NULL, content_id
         FROM attachments
         WHERE account_id=?1 AND message_id=?2
         ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map(params![account_id, message_id], |r| {
            Ok(ClientAttachment {
                id: r.get(0)?,
                filename: r.get(1)?,
                mime: r.get(2)?,
                size: r.get(3)?,
                downloadable: r.get(4)?,
                content_id: r.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(super) fn external_shipment_allowed_conn(
    conn: &Connection,
    account_id: AccountId,
    shipment_id: i64,
) -> Result<bool> {
    let row: Option<(super::specialists::Provenance, String)> = conn
        .query_row(
            "SELECT created_by_message_id,last_message_id,order_merchant_msg,item_name_msg,
                    item_name
             FROM shipments WHERE account_id=?1 AND id=?2",
            params![account_id, shipment_id],
            |row| {
                Ok((
                    super::specialists::Provenance {
                        created: row.get(0)?,
                        last: row.get(1)?,
                        merchant_msg: row.get(2)?,
                        name_msg: row.get(3)?,
                    },
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((prov, name)) = row else {
        return Ok(false);
    };
    super::specialists::agent_row_allowed(conn, account_id, &name, &prov, &mut Default::default())
}

/// May the agent door carry text that `message_id` contributed to a derived
/// row? The message's own current assessment (and every source it consumed)
/// must be allowed, and so must its thread. The per-field half of
/// [`external_shipment_allowed_conn`]: the merchant and each order link are
/// judged by the message that supplied them, one at a time, so a restricted
/// mail's merchant drops off an otherwise servable package instead of riding
/// along on it.
pub(super) fn external_contributor_allowed_conn(
    conn: &Connection,
    account_id: AccountId,
    message_id: i64,
) -> Result<bool> {
    let thread: Option<String> = conn
        .prepare_cached("SELECT thread_id FROM messages WHERE account_id=?1 AND id=?2")?
        .query_row(params![account_id, message_id], |r| r.get(0))
        .optional()?;
    let Some(thread) = thread else {
        return Ok(false);
    };
    thread_allowed_with_conn(conn, account_id, &thread, &[message_id])
}

impl SqliteStore {
    pub(super) fn upsert_message(&self, msg: &NewMessage) -> Result<i64> {
        let conn = self.lock()?;
        upsert_message_conn(&conn, msg, None)
    }

    /// Clear provider spam after the caller restores the message in Gmail.
    /// Reset human attention state and enqueue canonical triage in the same
    /// transaction. Rescue is push-silent, including for older messages. Legacy
    /// pass markers are cleared for compatibility with older diagnostic readers.
    /// Returns false for an unknown, foreign-account, or already rescued message.
    pub(super) fn clear_spam(&self, account_id: AccountId, message_id: i64) -> Result<bool> {
        let mut conn = self.lock()?;
        let now = Utc::now().to_rfc3339();
        let tx = conn.transaction()?;
        let cleared = tx.execute(
            "UPDATE messages SET is_spam = 0
             WHERE account_id = ?1 AND id = ?2 AND is_spam = 1",
            params![account_id, message_id],
        )?;
        if cleared == 0 {
            return Ok(false);
        }
        // The message may feed ghosts again: project its stored decision now,
        // so the ghost rows and the user's marks on them are kept. A ghost it
        // alone supplied stays HIDDEN until the re-triage queued below lands:
        // leaving spam changes the access snapshot, so the stored decision's
        // access reads `pending` and feeds nothing until it is re-decided.
        super::ghosts::refresh_message_guarded(&tx, account_id, message_id)?;
        tx.execute(
            "UPDATE triage
                SET stage1_model_used = NULL, model_used = NULL, needs_stage2 = 0,
                    extractor_model_used = NULL, retriage_at = ?3,
                    status = 'new', surfaced_at = NULL, resolved_at = NULL,
                    resolved_by = NULL
              WHERE account_id = ?1 AND message_id = ?2",
            params![account_id, message_id, now],
        )?;
        // The row just became visible WITHOUT passing through the upsert, so
        // its sender registers here or not at all: a sender whose only mail
        // was misfiled as spam has no directory row until this moment.
        let (from_addr, from_name, received_at): (String, Option<String>, String) = tx.query_row(
            "SELECT from_addr, from_name, received_at FROM messages
                 WHERE account_id = ?1 AND id = ?2",
            params![account_id, message_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        super::senders::bump_sender_conn(
            &tx,
            account_id,
            &from_addr,
            from_name.as_deref(),
            &received_at,
        )?;
        // Its offset and any invitation it holds count from now on.
        super::zones::refresh_for_message_guarded(&tx, account_id, message_id)?;
        super::agent_triage::enqueue_agent_triage_conn(
            &tx, account_id, message_id, "not_spam", false,
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Pure permission probe: human cache checks must never buy model work.
    pub fn external_thread_allowed(&self, account_id: AccountId, thread_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        match thread_guard_and_subject(&conn, account_id, thread_id) {
            Ok(_) => Ok(true),
            Err(CoreError::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// An agent door's own second look at a listing it is about to serve: for
    /// each `(message, decision source, thread)`, may the message and the
    /// message whose decision the row shows go out (what `agent_access_allowed`
    /// asks), and may their thread (what [`Self::external_thread_allowed`]
    /// asks)? One answer per row, in order, judged by [`ListingJudge`].
    ///
    /// One lock per [`LISTING_CHECK_CHUNK`] rows rather than two lock round
    /// trips per row, so the store is held for one chunk at a time, and the
    /// thread yields between chunks so a writer waiting on the mutex (which
    /// is not fair) usually gets in.
    pub fn external_listing_allowed(
        &self,
        account_id: AccountId,
        items: &[(i64, i64, &str)],
    ) -> Result<Vec<bool>> {
        let mut allowed = Vec::with_capacity(items.len());
        for (at, chunk) in items.chunks(LISTING_CHECK_CHUNK).enumerate() {
            if at > 0 {
                std::thread::yield_now();
            }
            let conn = self.lock()?;
            let mut judge = ListingJudge::new(chunk.iter().map(|row| row.2));
            for &(message_id, decision_source, thread_id) in chunk {
                allowed.push(judge.allowed(
                    &conn,
                    account_id,
                    thread_id,
                    &[message_id, decision_source],
                    |_| Ok(Some(Vec::new())),
                )?);
            }
        }
        Ok(allowed)
    }

    /// May a human client keep this thread in its on-disk cache? Stricter than
    /// [`Self::external_thread_allowed`] on purpose: the human reader is served
    /// EVERY message, unassessed archive rows included, so a thread carrying
    /// one is not cacheable until that row has been checked. Pure, like the
    /// probe above.
    pub fn human_cache_allowed(&self, account_id: AccountId, thread_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let unassessed: bool = conn.query_row(
            concat!(
                "SELECT EXISTS(SELECT 1 FROM messages m
                 LEFT JOIN agent_message_state a
                   ON a.account_id=m.account_id AND a.message_id=m.id
                 WHERE m.account_id=?1 AND m.thread_id=?2 AND ",
                archive_unassessed_sql!(),
                ")"
            ),
            params![account_id, thread_id],
            |row| row.get(0),
        )?;
        if unassessed {
            return Ok(false);
        }
        match thread_guard_and_subject(&conn, account_id, thread_id) {
            Ok(_) => Ok(true),
            Err(CoreError::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// A shipment combines tracking, status, and name fields from different
    /// messages. Every recorded contributor must have a current allowed decision.
    /// Legacy records without provenance remain available only to the human.
    pub fn external_shipment_allowed(
        &self,
        account_id: AccountId,
        shipment_id: i64,
    ) -> Result<bool> {
        let conn = self.lock()?;
        external_shipment_allowed_conn(&conn, account_id, shipment_id)
    }

    pub(super) fn thread_view(&self, account_id: AccountId, thread_id: &str) -> Result<ThreadView> {
        let conn = self.lock()?;
        // No caps: this read queues nothing for archive siblings. The doors
        // ask for those first, inside the surfacing ceilings.
        request_external_thread_access_conn(&conn, account_id, thread_id, None, Utc::now())?;
        let subject = thread_guard_and_subject(&conn, account_id, thread_id)?;

        // THE AGENT DOOR GETS NO SPAM AT ALL, not spam it is told to distrust.
        // Everything the agent reads is text it may act on, and provider spam is
        // text written to make a reader act; the human door can afford to show
        // it because a human is looking at a page that says who filed it. A
        // thread of nothing but spam therefore comes back empty here and 404s
        // below, which is the same shape sealed mail gets.
        //
        // AND NO UNASSESSED ARCHIVE ROW, for the reason on
        // `archive_unassessed_sql`: the guard above let the thread through
        // without looking at those rows, so this select is what keeps them in.
        //
        // THE TRANSLATION IS JOINED HERE, in the one query that reads the
        // body under the guard above, so it can reach no reader the body
        // could not (docs/TRANSLATION.md, invariant 5). Served statuses only.
        let mut stmt = conn.prepare(concat!(
            "SELECT m.id, m.from_addr, m.from_name, m.received_at, m.body,
                    tr.source_lang, tr.subject, tr.body, tr.status
             FROM messages m
             LEFT JOIN agent_message_state a
               ON a.account_id=m.account_id AND a.message_id=m.id
             LEFT JOIN message_translations tr
               ON tr.message_id=m.id AND tr.account_id=m.account_id
              AND tr.status IN ('done','incomplete')
             WHERE m.account_id=?1 AND m.thread_id=?2 AND m.is_spam = 0 AND NOT ",
            archive_unassessed_sql!(),
            " ORDER BY m.received_at ASC"
        ))?;
        let messages = stmt
            .query_map(params![account_id, thread_id], |r| {
                let translation = match r.get::<_, Option<String>>(5)? {
                    Some(code) => Some(crate::types::ExternalTranslation {
                        source_language: language_name(&code),
                        subject: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                        content: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
                        status: r.get(8)?,
                    }),
                    None => None,
                };
                Ok(SanitizedMessage {
                    id: r.get(0)?,
                    from_addr: r.get(1)?,
                    from_name: r.get(2)?,
                    received_at: dt(r, 3)?,
                    content: r.get(4)?,
                    translation,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if messages.is_empty() {
            return Err(CoreError::NotFound);
        }

        Ok(ThreadView {
            thread_id: thread_id.to_string(),
            subject,
            messages,
        })
    }

    pub(super) fn thread_id_for_message(
        &self,
        account_id: AccountId,
        message_id: i64,
    ) -> Result<Option<String>> {
        let conn = self.lock()?;
        if !external_message_allowed_conn(&conn, account_id, message_id)? {
            return Ok(None);
        }
        let thread_id: Option<String> = conn
            .query_row(
                "SELECT m.thread_id
                 FROM messages m
                 WHERE m.account_id = ?1 AND m.id = ?2",
                params![account_id, message_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(thread_id)
    }

    pub(super) fn thread_view_with_html(
        &self,
        account_id: AccountId,
        thread_id: &str,
    ) -> Result<ClientThreadView> {
        let conn = self.lock()?;
        // The human can read pending and restricted mail without waiting for triage.
        let subject = conn
            .query_row(THREAD_SUBJECT_SQL, params![account_id, thread_id], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or(CoreError::NotFound)?;

        // Per-message triage rides along for in-thread attention highlighting.
        // LEFT JOIN: a message somehow missing its triage row still renders,
        // just unhighlighted.
        //
        // `m.subject` rides along per message. The view's own `subject` is the
        // OLDEST message's (see `thread_guard_and_subject`), which titles the
        // conversation correctly and titles one message inside it WRONGLY the
        // moment somebody renames the thread — and a forward is composed from
        // one message, not from the conversation.
        //
        // The served `is_sent` is AUTHORSHIP, not the stored column: stored
        // `messages.is_sent` is a VISIBILITY flag that is sticky to 0 (see
        // `upsert_message_conn`) and the sync engine deliberately lets the INBOX
        // copy win for self-addressed mail, so a message the user wrote with
        // themselves on Cc — or mailed to themselves, or echoed back by a group
        // — stays pinned at 0. OR'ing the From address against the account's own
        // gives the reader the bit it actually aligns bubbles on. The accounts
        // LEFT JOIN is one row (`accounts.id` is the PK); LOWER on both sides
        // matches the ASCII case-folding every other address compare here uses,
        // and the empty-From guard keeps a blank sender from matching a blank
        // email, so a missing/NULL address falls back to the stored bit.
        let mut stmt = conn.prepare(
            "SELECT m.id, m.from_addr, m.from_name, m.received_at, m.body, m.body_html,
                    t.tier, t.deadline, t.status, t.one_line, m.auth_pass, m.subject,
                    m.is_spam, m.to_addrs, m.cc_addrs, m.bcc_addrs,
                    (m.is_sent = 1
                     OR (TRIM(COALESCE(m.from_addr, '')) != ''
                         AND LOWER(TRIM(COALESCE(m.from_addr, ''))) =
                             LOWER(TRIM(COALESCE(a.email, ''))))) AS authored_by_account,
                    tr.source_lang, tr.status, tr.subject, tr.body, tr.body_html,
                    tr.missing_facts
             FROM messages m
             LEFT JOIN triage t ON t.message_id = m.id
             LEFT JOIN accounts a ON a.id = m.account_id
             LEFT JOIN message_translations tr
               ON tr.message_id = m.id AND tr.account_id = m.account_id
              AND tr.status IN ('deferred','pending','done','incomplete')
             WHERE m.account_id=?1 AND m.thread_id=?2
             ORDER BY m.received_at ASC",
        )?;
        // Collect first, releasing `stmt`'s borrow of `conn`, so the per-message
        // attachment query below can run on the same connection.
        let mut messages = stmt
            .query_map(params![account_id, thread_id], |r| {
                Ok(ClientMessage {
                    id: r.get(0)?,
                    from_addr: r.get(1)?,
                    from_name: r.get(2)?,
                    received_at: dt(r, 3)?,
                    subject: r.get(11)?,
                    content: r.get(4)?,
                    html: r.get(5)?,
                    attachments: Vec::new(), // filled below, once `stmt` is gone
                    // Per-header recipient lists, straight off the row. NULL
                    // (never read) and "" (read, named nobody) stay distinct all
                    // the way to the client, which draws neither but must not
                    // turn one into the other.
                    //
                    // `bcc_addrs` IS SERVED ONLY ON THE ACCOUNT'S OWN MAIL, by
                    // the same authorship bit the bubbles align on. The writers
                    // gate on the INCOMING `is_sent`, and Gmail has served
                    // inbound mail through a SENT walk: that sighting stores the
                    // sender's leaked or forged `Bcc:` before the INBOX sighting
                    // pins `is_sent` to 0. Refusing it here holds whatever the
                    // row says.
                    to_addrs: r.get(13)?,
                    cc_addrs: r.get(14)?,
                    bcc_addrs: if r.get::<_, i64>(16)? != 0 {
                        r.get(15)?
                    } else {
                        None
                    },
                    // The computed authorship bit above: a boolean expression
                    // guarded against NULL on both sides, so every row answers.
                    is_sent: r.get::<_, i64>(16)? != 0,
                    is_spam: r.get::<_, i64>(12)? != 0,
                    tier: r
                        .get::<_, Option<String>>(6)?
                        .as_deref()
                        .and_then(Tier::parse),
                    deadline: dt_opt(r, 7)?,
                    attention_open: r.get::<_, Option<String>>(8)?.map(|s| s != "done"),
                    one_line: r.get::<_, Option<String>>(9)?.filter(|s| !s.is_empty()),
                    auth_pass: r.get::<_, Option<bool>>(10)?,
                    translation: client_translation(r, 17)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);

        for m in &mut messages {
            // ALWAYS present on the wire ([] when none). No sealed guard: this
            // is the HUMAN thread view, which serves sealed and restricted
            // threads to their owner by design
            // (`human_can_read_restricted_thread_with_html`). The audited
            // reveal is the Auth page's door, not this view's gate.
            m.attachments = load_client_attachments_conn(&conn, account_id, m.id)?;
        }
        if messages.is_empty() {
            return Err(CoreError::NotFound);
        }

        Ok(ClientThreadView {
            thread_id: thread_id.to_string(),
            subject,
            messages,
        })
    }

    pub(super) fn attachment_bytes(
        &self,
        account_id: AccountId,
        attachment_id: i64,
    ) -> Result<Option<AttachmentBytes>> {
        let conn = self.lock()?;
        // SECURITY: the parent message must be non-sealed (a missing triage row
        // COALESCEs to 'normal'), so a sealed parent yields no row and the caller
        // 404s, indistinguishable from an unknown id. An existing row with NULL
        // `data` (over the ingest cap) flows out as `Some((.., None))` => 410.
        let row = conn
            .query_row(
                "SELECT a.filename, a.mime, a.data
                 FROM attachments a
                 JOIN messages m ON m.id = a.message_id AND m.account_id = a.account_id
                 LEFT JOIN triage t ON t.message_id = a.message_id
                 WHERE a.account_id = ?1 AND a.id = ?2",
                params![account_id, attachment_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<Vec<u8>>>(2)?,
                    ))
                },
            )
            .optional()?;
        Ok(row)
    }

    pub(super) fn deadlines(
        &self,
        account_id: AccountId,
        within_days: Option<u32>,
    ) -> Result<Vec<Deadline>> {
        let conn = self.lock()?;
        // Human deadline reads include restricted source mail; external callers check access.
        // within_days = None means "all".
        let cutoff =
            within_days.map(|d| (Utc::now() + chrono::Duration::days(d as i64)).to_rfc3339());
        let cutoff_ref: &dyn rusqlite::ToSql = match &cutoff {
            Some(s) => s,
            None => &"9999-12-31T23:59:59+00:00",
        };

        let mut stmt = conn.prepare(
            "SELECT d.id, d.account_id, d.message_id, d.kind, d.amount, d.currency,
                    d.due_at, d.past_due, d.source
             FROM deadlines d
             WHERE d.account_id = ?1
               AND d.due_at <= ?2
             ORDER BY d.due_at ASC",
        )?;
        let out = stmt
            .query_map(params![account_id, cutoff_ref], |r| {
                Ok(Deadline {
                    id: r.get(0)?,
                    account_id: r.get(1)?,
                    message_id: r.get(2)?,
                    kind: r.get(3)?,
                    amount: r.get(4)?,
                    currency: r.get(5)?,
                    due_at: dt(r, 6)?,
                    past_due: r.get::<_, i64>(7)? != 0,
                    source: r.get(8)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(out)
    }

    pub(super) fn ingest_message(&self, triaged: &TriagedMessage) -> Result<i64> {
        self.ingest_message_inner(triaged, None, false)
            .map(|id| id.expect("a plain ingest never refuses"))
    }

    /// See [`crate::store::Store::ingest_archive_message`].
    pub(super) fn ingest_archive_message(&self, triaged: &TriagedMessage) -> Result<Option<i64>> {
        self.ingest_message_inner(triaged, None, true)
    }

    /// See [`crate::store::Store::ingest_message_fresh`].
    pub(super) fn ingest_message_fresh(
        &self,
        triaged: &TriagedMessage,
        scope: HealScope,
    ) -> Result<Option<i64>> {
        self.ingest_message_inner(triaged, Some(scope), false)
    }

    /// The one ingest write. `heal` is the blank-body heal's flavour: refuse
    /// anything but a live, normal, non-spam row the store already holds, drop
    /// the stale vector, leave the unsubscribe ledger alone, and queue the
    /// agent job the scope asks for rather than the one the sync origin would.
    /// Everything else — the body replacement, the content-revision bump and
    /// the access reset that follows it — is what a re-ingest already does.
    ///
    /// `archive` is the archive walk's flavour: insert-only, a neutral triage
    /// row like sent mail's, an agent state row with no job behind it, and none
    /// of the side effects an arrival has (contacts, the unsubscribe ledger,
    /// timezone observations).
    fn ingest_message_inner(
        &self,
        triaged: &TriagedMessage,
        heal: Option<HealScope>,
        archive: bool,
    ) -> Result<Option<i64>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;

        // 00. ARCHIVE ROWS ARE WRITE-ONCE, in both directions, and this is the
        //     one place that enforces it. An archive ingest of an id the store
        //     already holds (live or archive) writes nothing: the walk re-lists
        //     a chunk after a crash, and a live row must never be demoted. A
        //     live sighting of an archive row (a catch-up or history overlap at
        //     the window's edge) writes nothing either, and hands back the row
        //     it found: promoting it here would bump its revision and queue a
        //     model call nobody asked for, which is the one thing the tier
        //     exists to prevent. A heal refuses it like any row it may not
        //     touch.
        let stored: Option<(i64, bool)> = tx
            .query_row(
                "SELECT id, is_archive FROM messages
                 WHERE account_id = ?1 AND gmail_msg_id = ?2",
                params![triaged.message.account_id, triaged.message.gmail_msg_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match stored {
            Some(_) if archive => return Ok(None),
            Some((_, true)) if heal.is_some() => return Ok(None),
            Some((id, true)) => return Ok(Some(id)),
            _ => {}
        }

        // 0. HEAL PRE-CHECK, on the LIVE row inside the transaction. The sweep
        //    snapshotted its candidates minutes or hours ago; the on-demand spam
        //    walk or a human seal may have ruled on the row since, and either
        //    outranks a re-read. `is_spam` is sticky-to-zero in the upsert
        //    below (a sighting outside SPAM clears it), so a fabricated
        //    `is_spam: false` from the sweep would otherwise flip a real
        //    verdict back — and put a spam body in front of a model.
        if heal.is_some() {
            let live: Option<(i64, i64, Option<String>)> = tx
                .query_row(
                    "SELECT m.id, m.is_spam, t.sensitivity
                     FROM messages m
                     LEFT JOIN triage t ON t.message_id = m.id
                     WHERE m.account_id = ?1 AND m.gmail_msg_id = ?2",
                    params![triaged.message.account_id, triaged.message.gmail_msg_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            match live {
                Some((_, 0, Some(sens))) if sens == "normal" => {}
                _ => return Ok(None),
            }
        }

        // 1. Upsert the message row (+ FTS). An archive row keeps no Date
        //    offset: that column IS the timezone observation (`sqlite::zones`
        //    counts it by query), and a year of old mail must not vote on where
        //    a correspondent is now. The flag is set here, inside the insert's
        //    own transaction, and nowhere else; the only thing that ever
        //    clears it is the owner's promotion (`promote_archive_conn`).
        //
        //    NOR DOES IT RENAME ITS SENDER. The directory keeps the newest
        //    sighting's display name, on the understanding that sightings
        //    arrive oldest first; the walk runs the other way, so every older
        //    message would overwrite the name the sender uses now with one
        //    they used a year ago. A name already on file is put back; a
        //    sender with none yet takes the first (newest) one the walk meets.
        let kept_name: Option<String> = if archive {
            tx.query_row(
                "SELECT display_name FROM senders WHERE account_id = ?1 AND addr = ?2",
                params![triaged.message.account_id, triaged.message.from_addr],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
        } else {
            None
        };
        let id = upsert_message_conn(
            &tx,
            &triaged.message,
            triaged.date_offset_minutes.filter(|_| !archive),
        )?;
        if archive {
            tx.execute(
                "UPDATE messages SET is_archive = 1 WHERE account_id = ?1 AND id = ?2",
                params![triaged.message.account_id, id],
            )?;
            if let Some(name) = kept_name {
                tx.execute(
                    "UPDATE senders SET display_name = ?3 WHERE account_id = ?1 AND addr = ?2",
                    params![triaged.message.account_id, triaged.message.from_addr, name],
                )?;
            }
        }

        // 1a. HEAL: the vector was computed from the text the row had, which
        //     was the subject alone; drop it so the vector backfill re-embeds
        //     from the real text, batched and throttled, off this path.
        if heal.is_some() {
            tx.execute(
                "DELETE FROM message_vecs WHERE message_id = ?1",
                params![id],
            )?;
        }

        // 1b. Contacts from Sent-mail To/Cc, in the SAME transaction. Not from
        //     the archive: a contact is "someone I write to", read by triage as
        //     a live signal, and the walk arriving at 2025 must not re-rank
        //     this week's mail.
        if !archive {
            seed_contacts_conn(
                &tx,
                triaged.message.account_id,
                &triaged.recipients,
                &triaged.message.received_at.to_rfc3339(),
            )?;
        }

        // 1b'. THE NORMALIZED RECIPIENT INDEX, from the FAITHFUL address set
        //      rather than the contact-filtered one beside it, and in the same
        //      transaction for the same reason: `message_recipients` is what
        //      send-group history joins against, so it must never describe a
        //      message the transaction rolled back. Received mail carries an
        //      empty set, which writes nothing and clears nothing (the row has
        //      none to clear).
        if triaged.message.is_sent {
            SqliteStore::sync_message_recipients_conn(
                &tx,
                triaged.message.account_id,
                id,
                &triaged.recipient_addrs,
            )?;
        }

        // 1b''. TIMEZONE OBSERVATIONS: the Date header's offset is already
        //       on the row (step 1); the invitation's organizer and zone go
        //       beside it, and the zone of whoever they concern is recomputed,
        //       when there is anything to learn. Under its own savepoint, so a
        //       failure costs the zone and not the message (see
        //       `zones::guarded` for the one exception).
        if !archive {
            super::zones::observe_guarded(
                &tx,
                triaged.message.account_id,
                id,
                triaged.invite_zone.as_ref(),
            )?;
        }

        // 1c. UNSUBSCRIBE VIOLATION LEDGER: inbound mail from a sender the user
        //     unsubscribed from, past the 72h grace, bumps that sender's
        //     violation_count — in the SAME transaction as the message insert, so
        //     the ledger cannot drift from the mail that drives it.
        //     NOT on a heal: the message was counted when it arrived, and the
        //     bump is a blind `+ 1` with no idempotency key.
        //     NOT for the archive: it predates whatever the user has since
        //     unsubscribed from, or was counted in its own day.
        if !triaged.message.is_sent && heal.is_none() && !archive {
            bump_unsub_violation_conn(
                &tx,
                triaged.message.account_id,
                &triaged.message.from_addr,
                triaged.message.received_at,
            )?;
        }

        // 2. Write the triage row IN THE SAME TRANSACTION. That is the whole
        //    point for sealed mail: sensitivity='sealed' commits atomically with
        //    the message, so there is no window in which it is queryable as
        //    normal mail. `model_used` stays NULL, which with
        //    sensitivity='normal' is the Stage-2 queue predicate.
        let deadline_dt = triaged.deadline.as_ref().map(|d| d.due_at.to_rfc3339());
        // Record kinds never decide attention state. Preserve explicit lifecycle.
        let now_s = Utc::now().to_rfc3339();
        let (status, resolved_at): (&str, Option<String>) = ("new", None);
        // Re-ingest PRESERVES the existing attention lifecycle: a re-sync must not
        // reopen an item the user dismissed. Receipt/calendar rows are the
        // exception, force-resolved on every ingest — the CASE keys off
        // `excluded.status`, and only auto-resolved rows pass 'done' in.
        //
        // Per-property Stage-1 reasons as JSON (NULL when empty, as for sealed /
        // sent mail). HUMAN-DOOR ONLY on read.
        let field_reasons_json = if triaged.field_reasons.is_empty() {
            None
        } else {
            serde_json::to_string(&triaged.field_reasons).ok()
        };
        // STAGE-1/STAGE-2 QUEUE MARKERS: `stage1_model_used` decides whether the
        // Stage-1 pass looks at this row, `needs_stage2` is the escalation seed.
        //   * Sealed / Sent / provider-spam: never queued for any LLM ('n/a').
        //     Sealed mail reaching a model is the one thing this system must
        //     never do; spam reaching one is the second, because spam bodies are
        //     attacker-written text and a Stage-1 prompt is a reader of them.
        //   * Filtered rule: skip Stage-1 and go straight to Stage-2, which is
        //     the only stage that evaluates `want_text` ('rule', needs_stage2=1).
        //   * EVERYTHING ELSE, rule-decided included: enter the Stage-1 queue
        //     (NULL), seeding `needs_stage2` from heuristic confidence.
        //
        // A Squelch/Surface rule USED TO stop here with 'rule' and never see a
        // model. It no longer does. The rule still wins — the user's own ruling
        // on a sender is not something a classifier gets to overturn — but it
        // decides ONE axis, and a row that skipped the model had no category, no
        // extraction, no deadline, and no revisit schedule either. Honoring an
        // instruction about visibility was quietly costing every other thing
        // triage knows how to produce.
        //
        // Filtered is told apart from Squelch/Surface by `confident`: the Filtered
        // rung parks NOT-confident precisely because its verdict is pending an
        // LLM read of `want_text` (see `triage::stage1`).
        let (stage1_model_used, needs_stage2): (Option<&str>, i64) = if triaged.sensitivity
            != Sensitivity::Normal
            || triaged.message.is_sent
            || triaged.message.is_spam
            || archive
        {
            (Some("n/a"), 0)
        } else if triaged.matched_rule.is_some() && !triaged.confident {
            (Some("rule"), 1)
        } else {
            (None, if triaged.confident { 0 } else { 1 })
        };
        // RE-INGEST CLASSIFICATION GUARD. A re-ingest carries only HEURISTIC SEED
        // values, so for a row an LLM already classified (`model_used` set, or a
        // `stage1_model_used` other than the 'rule'/'n/a' sentinels) writing the
        // seed back would discard paid classification while the model markers
        // stay put — the row would never re-queue to recover it. This predicate
        // keeps those columns on conflict; still-seed rows refresh normally.
        const PROCESSED: &str = "(triage.model_used IS NOT NULL \
             OR (triage.stage1_model_used IS NOT NULL \
                 AND triage.stage1_model_used NOT IN ('rule', 'n/a')))";
        // DID A PERSON RULE ON *SENSITIVITY* FOR THIS ROW? Not "did a person
        // touch this row at all", which is what `triage.model_used = 'human'`
        // asks: `correct_triage` stamps that column for a TIER or a CATEGORY
        // correction too, so keying the seal freeze on it would pin the
        // sensitivity of every human-corrected row forever and take the
        // detector-fix direction below down with it — a better `detect_sealed`
        // could never newly seal a message whose tier somebody once fixed.
        //
        // `triage_feedback` is the exact record instead: `correct_triage`
        // writes one row per correction, naming the axis, in the SAME
        // transaction as the UPDATE, so a `dimension = 'sensitivity'` row is
        // "a person decided whether this is auth mail" and nothing else. It
        // rides `idx_triage_feedback_msg`, so this is an index probe on a table
        // that holds one row per human correction.
        //
        // BOTH DIRECTIONS, which is the point of asking about the axis rather
        // than the value: a person's seal outranks the seed that would revert
        // it (docs/SECURITY.md §4), and a person's un-seal outranks the same
        // recall-biased detector that over-sealed in the first place. An
        // un-seal a re-walk quietly undid would be a correction the user has to
        // make again every poll.
        const HUMAN_SENSITIVITY: &str = "EXISTS (SELECT 1 FROM triage_feedback f \
             WHERE f.account_id = triage.account_id \
               AND f.message_id = triage.message_id \
               AND f.dimension = 'sensitivity')";
        // SHIPMENTS-EXTRACTOR TRIGGER. 'pending' queues the row for the shipments
        // specialist; NULL means no shipping signal at ingest. Sealed, sent and
        // spam mail never queue — the detector does not even run for them.
        let ship_extract_model: Option<&str> = if triaged.ship_extract
            && triaged.sensitivity == Sensitivity::Normal
            && !triaged.message.is_sent
            && !triaged.message.is_spam
            && !archive
        {
            Some("pending")
        } else {
            None
        };
        let triage_upsert = format!(
            "INSERT INTO triage(message_id, account_id, importance, tier, sensitivity,
                 sealed_kind, one_line, reason, deadline, matched_rule_id,
                 stage1_model_used, needs_stage2, model_used,
                 status, resolved_at, created_at, field_reasons, ship_extract_model,
                 notify_eligible_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,NULL,?13,?14,?15,?16,?17,?18)
             ON CONFLICT(message_id) DO UPDATE SET
                 importance=CASE WHEN {PROCESSED} THEN triage.importance ELSE excluded.importance END,
                 tier=CASE WHEN {PROCESSED} THEN triage.tier ELSE excluded.tier END,
                 -- A HUMAN SEAL OUTRANKS A RE-INGEST, which every other verdict
                 -- column has always said via the PROCESSED predicate above and
                 -- this one did not.
                 -- A re-ingest carries only heuristic SEED values, and the seed
                 -- detector never saw whatever made a person call this mail
                 -- auth, so `excluded.sensitivity` here reverts the seal — on a
                 -- catch-up, or on the routine INBOX re-walk a held cursor
                 -- causes. That is not a bookkeeping loss: an unsealed row is a
                 -- row the fast lane will send to a model and turn into an
                 -- events row, minted AFTER `correct_triage` ran its redaction
                 -- and so with nothing left that can redact it (docs/SECURITY.md
                 -- §4). HUMAN_SENSITIVITY rather than PROCESSED because only a
                 -- PERSON outranks a person: an LLM-classified row refreshes
                 -- from the fresh detection as before, which is what lets a
                 -- detector fix newly seal an old message.
                 sensitivity=CASE WHEN {HUMAN_SENSITIVITY}
                     THEN triage.sensitivity ELSE excluded.sensitivity END,
                 -- SEALED_KIND IS NOT FROZEN WITH IT, deliberately. It looks
                 -- like it should move with the column above, and freezing it
                 -- only loses: `correct_triage` never writes a kind, so a row
                 -- a PERSON sealed carries NULL until a later detection
                 -- supplies one, and that kind is what routes the tap to the
                 -- reveal flow. Nothing reads it ungated either — every query
                 -- that selects it (`sealed_messages`, `sealed_body`, the
                 -- shred sweep) joins on `sensitivity = 'sealed'` — so a stale
                 -- kind left on a row a person called normal is inert.
                 sealed_kind=excluded.sealed_kind,
                 one_line=CASE WHEN {PROCESSED} THEN triage.one_line ELSE excluded.one_line END,
                 reason=CASE WHEN {PROCESSED} THEN triage.reason ELSE excluded.reason END,
                 field_reasons=CASE WHEN {PROCESSED} THEN triage.field_reasons ELSE excluded.field_reasons END,
                 deadline=CASE WHEN {PROCESSED} THEN triage.deadline ELSE excluded.deadline END,
                 matched_rule_id=excluded.matched_rule_id,
                 -- A PROCESSED shipments marker survives re-ingest (re-running a
                 -- paid extractor on the same mail buys nothing); a NULL or a
                 -- still-'pending' one refreshes from the fresh detection, so a
                 -- detector fix can newly queue — or newly un-queue — a row.
                 ship_extract_model = CASE
                     WHEN triage.ship_extract_model IS NOT NULL
                          AND triage.ship_extract_model != 'pending'
                     THEN triage.ship_extract_model ELSE excluded.ship_extract_model END,
                 status=CASE WHEN excluded.status='done' THEN 'done' ELSE triage.status END,
                 resolved_at=CASE WHEN excluded.status='done'
                     THEN excluded.resolved_at ELSE triage.resolved_at END,
                 -- A re-ingest that resolves the row is its own resolution and
                 -- replaces `resolved_at`, so whatever sweep had resolved it
                 -- before no longer owns it. One that leaves the status alone
                 -- leaves the provenance alone too.
                 resolved_by=CASE WHEN excluded.status='done'
                     THEN NULL ELSE triage.resolved_by END,
                 -- WRITTEN ONCE, ON FIRST INSERT, AND NEVER AGAIN — not even to
                 -- a fresher stamp, and not conditioned on `PROCESSED` like the
                 -- verdict columns. Notify eligibility is a fact about when we
                 -- FIRST saw a message, so a re-ingest (history overlap, a
                 -- catch-up re-scan, a sealed row being rewritten) must not be
                 -- able to move it forward: that would hand the whole re-scanned
                 -- window a fresh hour of notify eligibility, which is the storm
                 -- the guard exists to prevent. A NULL stays NULL for the same
                 -- reason, from the other direction.
                 notify_eligible_at = triage.notify_eligible_at"
        );
        tx.execute(
            &triage_upsert,
            params![
                id,
                triaged.message.account_id,
                triaged.importance as i64,
                triaged.tier.as_str(),
                triaged.sensitivity.as_str(),
                triaged.sealed_kind.map(|k| k.as_str()),
                triaged.one_line,
                triaged.reason,
                deadline_dt,
                triaged.matched_rule,
                stage1_model_used,
                needs_stage2,
                status,
                resolved_at,
                now_s,
                field_reasons_json,
                ship_extract_model,
                // Never for the archive, whatever the caller stamped.
                triaged
                    .notify_eligible_at
                    .filter(|_| !archive)
                    .map(|t| t.to_rfc3339()),
            ],
        )?;

        // 3. Deadlines: non-sealed mail only (Stage-1 never runs on sealed
        //    content), so a sealed re-ingest passes None and only clears.
        let ingest_deadline = if triaged.sensitivity == Sensitivity::Sealed {
            None
        } else {
            triaged.deadline.as_ref()
        };
        rewrite_deadline_conn(&tx, triaged.message.account_id, id, ingest_deadline)?;

        // 4. Shipment: NON-SEALED mail only, so `shipments` is sealed-free by
        //    construction. Upserted in the SAME transaction so a package's state
        //    and its source message land atomically.
        if triaged.sensitivity != Sensitivity::Sealed
            && let Some(s) = &triaged.shipment
        {
            upsert_shipment_conn(
                &tx,
                triaged.message.account_id,
                id,
                s,
                triaged.message.received_at,
            )?;
        }

        // 5. Receipt: NON-SEALED mail only, so `receipts` is sealed-free by
        //    construction. Independent of shipment detection — an order
        //    confirmation with a total AND tracking lands in BOTH tables.
        if triaged.sensitivity != Sensitivity::Sealed
            && let Some(r) = &triaged.receipt
        {
            upsert_receipt_conn(
                &tx,
                triaged.message.account_id,
                id,
                &triaged.message.from_addr,
                triaged.message.from_name.as_deref(),
                r,
                triaged.message.received_at,
            )?;
        }

        // 6. Calendar update: NON-SEALED mail only, so `calendar_updates` is
        //    sealed-free by construction. Independent of the other detectors,
        //    exactly like receipts. Nothing is written back to Gmail — "resolved"
        //    is squelch-internal.
        if triaged.sensitivity != Sensitivity::Sealed
            && let Some(c) = &triaged.calendar
        {
            upsert_calendar_conn(
                &tx,
                triaged.message.account_id,
                id,
                c,
                triaged.message.received_at,
            )?;
        }

        // 7. Attachments: written for sealed mail too — the byte-serving endpoint
        //    guards sealed parents. Replaces prior rows so re-ingest is
        //    idempotent; over-cap parts store a NULL blob.
        //
        //    AN ARCHIVE ROW KEEPS METADATA ONLY, every part stored the way an
        //    over-cap one is: the reader lists the file as not downloadable and
        //    the byte endpoint answers 410. A year of blobs is what fills a
        //    tenant's volume, and a full volume fails the LIVE ingest.
        insert_attachments_conn(
            &tx,
            triaged.message.account_id,
            id,
            &triaged.attachments,
            !archive,
        )?;
        // 7b. TRANSLATION: a message detected as not English is queued in this
        //     same transaction (docs/TRANSLATION.md), `deferred` on the archive
        //     tier, which spends nothing until its thread is opened. Never for
        //     spam, sent, the account's own or sealed mail, whatever the caller
        //     carried: the stored row is asked, with the same authorship test
        //     the human thread view uses and the same sealed predicate as the
        //     Auth page. A row that exists is kept (a re-ingest must not throw
        //     away a translation it paid for), unless THIS ingest made the
        //     message spam or own mail: then the row is closed here, at the
        //     event, its served text dropped. A heal replaced the text the row
        //     was translated from, so it starts over.
        //
        //     Only NEWLY ARRIVED mail holds its triage for the translation; a
        //     backfill is triaged on the original at once and re-triaged when
        //     the English lands. Over the day's translate budget, nothing
        //     holds, and a backfilled message waits `deferred` for its reader.
        if heal.is_some() {
            super::translations::drop_translation_conn(&tx, id)?;
        }
        if super::translations::is_untranslatable_conn(&tx, id)? {
            super::translations::close_if_untranslatable_conn(
                &tx,
                triaged.message.account_id,
                id,
                Utc::now(),
            )?;
        } else if let Some(lang) = triaged.translate_lang
            && !triaged.message.is_spam
            && !triaged.message.is_sent
        {
            let arrival = triaged.foreground_triage && heal.is_none();
            super::translations::queue_translation_conn(
                &tx,
                triaged.message.account_id,
                id,
                lang.code(),
                archive || (!arrival && triaged.translate_over_budget),
                arrival && !triaged.translate_over_budget,
                Utc::now(),
            )?;
        }
        // 8. THE AGENT'S BOOKKEEPING, last, over the row as it now stands: it
        //    re-reads the message, and a changed content snapshot is what
        //    advances the revision and queues the work. The trigger names the
        //    reason. A heal is neither an arrival nor a backfill: `heal` queues
        //    a background investigation of the re-read row, and the text-only
        //    scope queues an access reassessment alone (`source_access` is the
        //    trigger the store already resolves to an `access` job).
        //
        //    THE ARCHIVE GETS THE STATE ROW AND NO JOB: the same snapshot at
        //    revision 1, access pending, so the agent door fails closed on it
        //    and the first explicit surfacing has a revision to assess.
        if archive {
            super::agent_triage::seed_archive_state_conn(&tx, triaged.message.account_id, id)?;
            tx.commit()?;
            return Ok(Some(id));
        }
        let trigger = match heal {
            None if triaged.foreground_triage => "ingest",
            None => "backfill",
            Some(HealScope::TextAndTriage) => "heal",
            Some(HealScope::Text) => "source_access",
        };
        super::agent_triage::enqueue_agent_triage_conn(
            &tx,
            triaged.message.account_id,
            id,
            trigger,
            triaged.notify_eligible_at.is_some(),
        )?;

        tx.commit()?;
        Ok(Some(id))
    }

    pub(super) fn is_known_contact(&self, account_id: AccountId, addr: &str) -> Result<bool> {
        is_known_contact_conn(&*self.lock()?, account_id, addr)
    }

    pub(super) fn sync_state(
        &self,
        account_id: AccountId,
        mailbox: &str,
    ) -> Result<Option<SyncState>> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT uidvalidity, last_uid FROM sync_state
                 WHERE account_id=?1 AND mailbox=?2",
                params![account_id, mailbox],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()?;
        Ok(row.map(|(uv, lu)| SyncState {
            uidvalidity: uv as u32,
            last_uid: lu as u64,
        }))
    }

    pub(super) fn archive_walked_to(&self, account_id: AccountId) -> Result<Option<DateTime<Utc>>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT walked_to FROM archive_walk WHERE account_id=?1",
            params![account_id],
            |r| dt(r, 0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub(super) fn message_is_archive(
        &self,
        account_id: AccountId,
        message_id: i64,
    ) -> Result<bool> {
        let conn = self.lock()?;
        let is_archive: Option<bool> = conn
            .query_row(
                "SELECT is_archive FROM messages WHERE account_id=?1 AND id=?2",
                params![account_id, message_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(is_archive.unwrap_or(false))
    }

    pub(super) fn set_archive_walked_to(
        &self,
        account_id: AccountId,
        walked_to: DateTime<Utc>,
    ) -> Result<()> {
        let conn = self.lock()?;
        // `covered_to` is written by the INSERT alone: the row is created at
        // the walk's start, which is where forward coverage starts too, and
        // every later call here moves the backward edge and nothing else.
        conn.execute(
            "INSERT INTO archive_walk(account_id, walked_to, covered_to, updated_at)
             VALUES(?1,?2,?2,?3)
             ON CONFLICT(account_id) DO UPDATE SET
                 walked_to=excluded.walked_to, updated_at=excluded.updated_at",
            params![account_id, walked_to.to_rfc3339(), Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    /// See [`crate::store::Store::archive_disk`]. The database's own size is
    /// the main file plus its `-wal`: a walk writes there first, and a large
    /// log is as much disk as a large file. A file that cannot be sized
    /// counts as empty; the volume's numbers are the ones that must be read.
    pub(super) fn archive_disk(&self) -> Result<crate::store::ArchiveDisk> {
        let file: String = {
            let conn = self.lock()?;
            conn.query_row(
                "SELECT file FROM pragma_database_list WHERE name='main'",
                [],
                |r| r.get::<_, Option<String>>(0),
            )?
            .unwrap_or_default()
        };
        if file.is_empty() {
            return Ok(crate::store::ArchiveDisk::UNBOUNDED);
        }
        let (free_bytes, total_bytes) = volume_bytes_at(std::path::Path::new(&file))?;
        let size = |path: &str| std::fs::metadata(path).map_or(0, |meta| meta.len());
        Ok(crate::store::ArchiveDisk {
            free_bytes,
            total_bytes,
            db_bytes: size(&file).saturating_add(size(&format!("{file}-wal"))),
        })
    }

    /// See [`crate::store::Store::archive_covered_to`]. A row from before the
    /// column existed reads NULL and answers its `walked_to`: everything
    /// between the two was stored by the backward walk, so starting there
    /// re-lists ids the store already holds and fetches nothing twice.
    pub(super) fn archive_covered_to(
        &self,
        account_id: AccountId,
    ) -> Result<Option<DateTime<Utc>>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT COALESCE(covered_to, walked_to) FROM archive_walk WHERE account_id=?1",
            params![account_id],
            |r| dt(r, 0),
        )
        .optional()
        .map_err(Into::into)
    }

    /// See [`crate::store::Store::set_archive_covered_to`].
    pub(super) fn set_archive_covered_to(
        &self,
        account_id: AccountId,
        covered_to: DateTime<Utc>,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE archive_walk SET covered_to=?2, updated_at=?3 WHERE account_id=?1",
            params![account_id, covered_to.to_rfc3339(), Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub(super) fn stored_gmail_msg_ids(
        &self,
        account_id: AccountId,
        gmail_msg_ids: &[String],
    ) -> Result<std::collections::HashSet<String>> {
        // Chunked well under SQLite's bound-variable limit (999 on old builds),
        // with one slot kept for the account.
        const CHUNK: usize = 500;
        let conn = self.lock()?;
        let mut stored = std::collections::HashSet::new();
        for chunk in gmail_msg_ids.chunks(CHUNK) {
            let placeholders = (2..chunk.len() + 2)
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(",");
            let mut stmt = conn.prepare(&format!(
                "SELECT gmail_msg_id FROM messages
                 WHERE account_id=?1 AND gmail_msg_id IN ({placeholders})"
            ))?;
            let mut binds: Vec<&dyn rusqlite::ToSql> = vec![&account_id];
            binds.extend(chunk.iter().map(|id| id as &dyn rusqlite::ToSql));
            for id in stmt.query_map(binds.as_slice(), |r| r.get::<_, String>(0))? {
                stored.insert(id?);
            }
        }
        Ok(stored)
    }

    pub(super) fn set_sync_state(
        &self,
        account_id: AccountId,
        mailbox: &str,
        state: &SyncState,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO sync_state(account_id, mailbox, uidvalidity, last_uid)
             VALUES(?1,?2,?3,?4)
             ON CONFLICT(account_id, mailbox) DO UPDATE SET
                 uidvalidity=excluded.uidvalidity, last_uid=excluded.last_uid",
            params![
                account_id,
                mailbox,
                state.uidvalidity as i64,
                state.last_uid as i64,
            ],
        )?;
        Ok(())
    }

    pub(super) fn inbox_unread(&self, account_id: AccountId) -> Result<Option<InboxUnread>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT messages, threads, fetched_at FROM inbox_unread WHERE account_id=?1",
            params![account_id],
            |r| {
                Ok(InboxUnread {
                    messages: r.get(0)?,
                    threads: r.get(1)?,
                    fetched_at: dt(r, 2)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub(super) fn set_inbox_unread(
        &self,
        account_id: AccountId,
        messages: i64,
        threads: i64,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO inbox_unread(account_id, messages, threads, fetched_at)
             VALUES(?1,?2,?3,?4)
             ON CONFLICT(account_id) DO UPDATE SET
                 messages=excluded.messages,
                 threads=excluded.threads,
                 fetched_at=excluded.fetched_at",
            params![account_id, messages, threads, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub(super) fn account_identity(
        &self,
        account_id: AccountId,
    ) -> Result<Option<AccountIdentity>> {
        let conn = self.lock()?;
        // The blob is deliberately not selected: this rides on every stats call.
        conn.query_row(
            "SELECT name, given_name, photo_tag FROM account_profile WHERE account_id=?1",
            params![account_id],
            |r| {
                Ok(AccountIdentity {
                    name: r.get(0)?,
                    given_name: r.get(1)?,
                    photo_tag: r.get(2)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub(super) fn account_photo(&self, account_id: AccountId) -> Result<Option<AccountPhoto>> {
        let conn = self.lock()?;
        // The three columns are written together, so the filter is belt: a row
        // with a name and no picture simply has all three NULL.
        conn.query_row(
            "SELECT photo, photo_mime, photo_tag FROM account_profile
             WHERE account_id=?1
               AND photo IS NOT NULL AND photo_mime IS NOT NULL AND photo_tag IS NOT NULL",
            params![account_id],
            |r| {
                Ok(AccountPhoto {
                    bytes: r.get(0)?,
                    mime: r.get(1)?,
                    tag: r.get(2)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub(super) fn set_account_profile(
        &self,
        account_id: AccountId,
        name: Option<&str>,
        given_name: Option<&str>,
        photo: Option<&AccountPhoto>,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO account_profile(
                 account_id, name, given_name, photo, photo_mime, photo_tag, fetched_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(account_id) DO UPDATE SET
                 name=excluded.name,
                 given_name=excluded.given_name,
                 photo=excluded.photo,
                 photo_mime=excluded.photo_mime,
                 photo_tag=excluded.photo_tag,
                 fetched_at=excluded.fetched_at",
            params![
                account_id,
                name,
                given_name,
                photo.map(|p| p.bytes.as_slice()),
                photo.map(|p| p.mime.as_str()),
                photo.map(|p| p.tag.as_str()),
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub(super) fn sealed_messages(&self, account_id: AccountId) -> Result<Vec<SealedMessage>> {
        // Human Auth lookup: every message the agent classified as auth, plus
        // unmigrated legacy rows. `access` is deliberately NOT consulted: it
        // answers whether the agent door may read the mail, not whether it is
        // auth. An informational sign-in alert is readable (`allowed`) and is
        // still exactly what the Auth page's decision rail exists for.
        // Done (archived from the Auth page, or marked done anywhere) leaves the
        // list: it is how the page and its rail badge let go of dealt-with mail.
        use crate::triage::auth_pairing::{AuthMailRow, explained_by, explainer_window};
        let conn = self.lock()?;
        let open = sealed_rows(&conn, account_id, "COALESCE(t.status,'new')!='done'", &[])?;
        // Done rows are read for pairing only, and only inside the window of
        // an open alert that could be explained: the code the owner archived
        // a minute after using it still explains the sign-in beside it. The
        // windows are merged so a burst costs one query, not one per alert.
        let mut windows: Vec<(DateTime<Utc>, DateTime<Utc>)> = open
            .iter()
            .filter(|(_, row)| row.explainable())
            .map(|(_, row)| explainer_window(row.received_at))
            .collect();
        windows.sort();
        let mut merged: Vec<(DateTime<Utc>, DateTime<Utc>)> = Vec::new();
        for (from, to) in windows {
            match merged.last_mut() {
                Some(last) if from <= last.1 => last.1 = last.1.max(to),
                _ => merged.push((from, to)),
            }
        }
        let mut done: Vec<AuthMailRow> = Vec::new();
        for (from, to) in merged {
            done.extend(
                sealed_rows(
                    &conn,
                    account_id,
                    "COALESCE(t.status,'new')='done' AND m.received_at BETWEEN ?2 AND ?3",
                    &[from.to_rfc3339(), to.to_rfc3339()],
                )?
                .into_iter()
                .map(|(_, row)| row),
            );
        }
        let pairing: Vec<AuthMailRow> = open
            .iter()
            .map(|(_, row)| row.clone())
            .chain(done)
            .collect();
        let explained = explained_by(&pairing);
        Ok(open
            .into_iter()
            .zip(explained)
            .map(|((mut m, _), explained_by)| {
                m.explained_by = explained_by;
                m
            })
            .collect())
    }

    pub(super) fn sealed_body(&self, account_id: AccountId, message_id: i64) -> Result<SealedBody> {
        // Human Auth lookup detail: same membership as `sealed_messages`.
        let conn = self.lock()?;
        let row = conn
            .query_row(
                concat!(
                    "SELECT m.id, m.account_id, m.thread_id, m.from_addr, m.from_name,
                        m.subject, m.received_at, s.message_id IS NOT NULL,
                        json_extract(d.decision_json,'$.auth'), t.sealed_kind, m.body, m.body_html
                 FROM messages m
                 LEFT JOIN triage t ON t.message_id = m.id AND t.account_id=m.account_id
                 LEFT JOIN agent_message_state s ON s.message_id=m.id AND s.account_id=m.account_id
                 LEFT JOIN agent_message_decisions d ON d.message_id=m.id AND d.account_id=m.account_id
                 WHERE m.account_id = ?1 AND m.id = ?2 AND ",
                    sealed_membership_sql!()
                ),
                params![account_id, message_id],
                |r| {
                    Ok(SealedBody {
                        id: r.get(0)?,
                        account_id: r.get(1)?,
                        thread_id: r.get(2)?,
                        from_addr: r.get(3)?,
                        from_name: r.get(4)?,
                        subject: r.get(5)?,
                        received_at: dt(r, 6)?,
                        sealed_kind: SealedAuth::read(
                            r.get(7)?,
                            r.get::<_, Option<String>>(8)?.as_deref(),
                            r.get(9)?,
                        )
                        .wire_kind,
                        body: r.get(10)?,
                        body_html: r.get(11)?,
                    })
                },
            )
            .optional()?;
        row.ok_or(CoreError::NotFound)
    }

    pub(super) fn set_auth_verdict(
        &self,
        account_id: AccountId,
        message_id: i64,
        verdict: Option<crate::types::AuthVerdict>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let conn = self.lock()?;
        // Same membership as the Auth page: a ruling on mail that is not auth
        // would move FYE for a message the page never offered a question on.
        let member: bool = conn.query_row(
            concat!(
                "SELECT EXISTS(SELECT 1 FROM messages m
                 LEFT JOIN triage t ON t.message_id = m.id AND t.account_id=m.account_id
                 LEFT JOIN agent_message_state s ON s.message_id=m.id AND s.account_id=m.account_id
                 LEFT JOIN agent_message_decisions d ON d.message_id=m.id AND d.account_id=m.account_id
                 WHERE m.account_id = ?1 AND m.id = ?2 AND ",
                sealed_membership_sql!(),
                ")"
            ),
            params![account_id, message_id],
            |r| r.get(0),
        )?;
        if !member {
            return Err(CoreError::NotFound);
        }
        match verdict {
            Some(verdict) => conn.execute(
                "INSERT INTO auth_verdicts(account_id, message_id, verdict, decided_at)
                 VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(account_id, message_id) DO UPDATE SET
                 verdict=excluded.verdict, decided_at=excluded.decided_at",
                params![account_id, message_id, verdict.as_str(), now.to_rfc3339()],
            )?,
            None => conn.execute(
                "DELETE FROM auth_verdicts WHERE account_id=?1 AND message_id=?2",
                params![account_id, message_id],
            )?,
        };
        Ok(())
    }

    /// The STORED `is_sent` column, which every served surface reinterprets as
    /// authorship. Only the sync tests need the raw flag: it is what decides
    /// whether a row was ingested as sent or as received mail.
    #[cfg(test)]
    pub(crate) fn stored_is_sent(&self, message_id: i64) -> Result<bool> {
        Ok(self.lock()?.query_row(
            "SELECT is_sent FROM messages WHERE id = ?1",
            params![message_id],
            |r| r.get::<_, i64>(0),
        )? == 1)
    }

    pub(super) fn sent_listing(
        &self,
        account_id: AccountId,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SentMessage>> {
        let conn = self.lock()?;
        // Human-only outbox: pending and restricted mail remain readable.
        // Recipients and read receipts are scoped to the owning account.
        // To, Cc and Bcc come back as ONE list here, unlike the reader: a sent
        // row is one line answering "who did this go to", and the thread it
        // opens onto says which header carried whom.
        //
        // AUTHORSHIP, NOT THE STORED FLAG. `is_sent` is a visibility flag that
        // is sticky to 0, and Gmail's label-filtered INBOX history has served
        // the user's own sends: the send echo writes 1, the INBOX sighting
        // MINs it to 0, and the mail vanished from this page for good. Every
        // message one tenant ever sent from the composer was missing here
        // while the mail it sent from Gmail listed. `OWN_MAIL` is the same
        // answer the attention surfaces use for "the user wrote this", and
        // like every other use of it, it keeps the spam folder out: a From-me
        // spam row with no auth verdict is not something the user sent.
        let mut stmt = conn.prepare(concat!(
            "SELECT m.id, m.thread_id, COALESCE(m.to_addrs, ''), m.subject, m.snippet,
                    m.received_at,
                    (SELECT COUNT(*) FROM message_opens o
                     JOIN send_trackers st ON st.token = o.token
                     WHERE st.account_id = m.account_id AND st.message_id = m.id) AS opens,
                    COALESCE(m.cc_addrs, ''), COALESCE(m.bcc_addrs, '')
             FROM messages m
             WHERE m.account_id = ?1
               AND m.is_spam = 0
               AND ",
            own_mail_sql!(),
            "
             ORDER BY m.received_at DESC, m.id DESC
             LIMIT ?2 OFFSET ?3"
        ))?;
        let out = stmt
            .query_map(params![account_id, limit as i64, offset as i64], |r| {
                Ok(SentMessage {
                    id: r.get(0)?,
                    thread_id: r.get(1)?,
                    to: crate::sync::ingest::join_recipient_lists(&[
                        &r.get::<_, String>(2)?,
                        &r.get::<_, String>(7)?,
                        &r.get::<_, String>(8)?,
                    ]),
                    subject: r.get(3)?,
                    snippet: r.get(4)?,
                    // The stored string verbatim — this is the same RFC3339 the
                    // rest of the door serves, and re-formatting it would only
                    // invent a second spelling of one timestamp.
                    sent_at: r.get(5)?,
                    opens: r.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(out)
    }

    pub(super) fn messages_missing_recipients(
        &self,
        account_id: AccountId,
        limit: u32,
    ) -> Result<Vec<MessageMissingRecipients>> {
        let conn = self.lock()?;
        // The recipients-backfill queue: every row nobody has read headers for
        // under the per-header split. Newest first, so an interrupted pass has
        // already covered the mail the user is most likely to look at.
        //
        // `cc_addrs IS NULL` IS THE PREDICATE, and `to_addrs IS NULL` would be
        // the wrong one: sent rows have carried a `to_addrs` since before the
        // split — one string with To, Cc and Bcc mashed together — and those
        // rows are exactly the ones that need re-reading, or every address the
        // user copied in renders as though they had been written to directly.
        // `cc_addrs` arrives NULL on all of them and on every received row too.
        let mut stmt = conn.prepare(
            "SELECT id, gmail_msg_id FROM messages
             WHERE account_id = ?1 AND cc_addrs IS NULL
             ORDER BY received_at DESC, id DESC
             LIMIT ?2",
        )?;
        let out = stmt
            .query_map(params![account_id, limit as i64], |r| {
                Ok(MessageMissingRecipients {
                    message_id: r.get(0)?,
                    gmail_msg_id: r.get(1)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(out)
    }

    /// See [`crate::store::Store::blank_body_messages`].
    pub(super) fn blank_body_messages(
        &self,
        account_id: AccountId,
        before_id: i64,
        limit: u32,
    ) -> Result<crate::store::BlankBodyScan> {
        let conn = self.lock()?;
        // `body_html IS NOT NULL` is the only shape this heal can recover — a
        // plain-text-only message with nothing in it has nothing to flatten —
        // and it is a column test, so it bounds the read to HTML mail without
        // re-expressing the blankness predicate. Bodies are streamed one row
        // at a time and judged in Rust; only the blank ones are kept.
        let mut stmt = conn.prepare(
            "SELECT m.id, m.gmail_msg_id, m.received_at, m.body
             FROM messages m
             JOIN triage t ON t.message_id = m.id
             WHERE m.account_id = ?1
               AND m.id < ?2
               AND m.body_html IS NOT NULL
               AND m.is_sent = 0
               AND m.is_spam = 0
               AND m.is_archive = 0
               AND t.sensitivity = 'normal'
             ORDER BY m.id DESC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![account_id, before_id, limit as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                dt(r, 2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut scan = crate::store::BlankBodyScan::default();
        let mut seen = 0u32;
        let mut last_id = None;
        for row in rows {
            let (message_id, gmail_msg_id, received_at, body) = row?;
            seen += 1;
            last_id = Some(message_id);
            // A NULL body is nothing to read, the same as a blank one.
            if crate::triage::text::is_blank(body.as_deref().unwrap_or("")) {
                scan.candidates.push(crate::store::BlankBodyMessage {
                    message_id,
                    gmail_msg_id,
                    received_at,
                });
            }
        }
        // A short chunk means the scan reached the oldest row; a full one may
        // have more below it, and the next chunk starts under the last id seen.
        scan.next_before_id = if seen < limit { None } else { last_id };
        Ok(scan)
    }

    pub(super) fn set_message_recipients(
        &self,
        account_id: AccountId,
        message_id: i64,
        to_addrs: &str,
        cc_addrs: &str,
        bcc_addrs: &str,
    ) -> Result<bool> {
        let mut conn = self.lock()?;
        // ONE TRANSACTION for the columns and the index rows derived from them.
        // In autocommit, a process killed partway left new columns over old or
        // partial index rows, which the open-time backfill cannot see: it
        // heals a sent row with NO index rows, never one with some.
        let tx = conn.transaction()?;
        // BCC IS DROPPED FOR ANYTHING NOT SENT, here in SQL rather than only in
        // the caller: a `Bcc:` header on received mail is the sender's leak or a
        // forgery, and this is the one writer a network sweep drives. To and Cc
        // are written on every row — the reader shows them for received mail
        // too, which is the whole point of the sweep.
        //
        // Writing "" is meaningful: it takes the row out of the backfill queue
        // as "looked, and the headers named nobody".
        let n = tx.execute(
            "UPDATE messages
                SET to_addrs = ?3,
                    cc_addrs = ?4,
                    bcc_addrs = CASE WHEN is_sent = 1 THEN ?5 ELSE NULL END
             WHERE account_id = ?1 AND id = ?2",
            params![account_id, message_id, to_addrs, cc_addrs, bcc_addrs],
        )?;
        if n == 0 {
            return Ok(false);
        }
        // Keep the normalized index in step with the columns it is derived from,
        // and ONLY FOR SENT MAIL: `message_recipients` is the sent index send
        // group history joins against, and an inbound row in it would credit a
        // group with mail it never received. Re-read from the row rather than
        // trusted from the caller, for the same reason the Bcc gate is in SQL.
        let is_sent: bool = tx.query_row(
            "SELECT is_sent = 1 FROM messages WHERE account_id = ?1 AND id = ?2",
            params![account_id, message_id],
            |r| r.get(0),
        )?;
        if is_sent {
            // Parsed back OUT of the display strings because that is all this
            // path ever has — the sweep hands over rendered headers, not the
            // mailboxes. The ingest path, which does have them, never comes
            // through here.
            let joined =
                crate::sync::ingest::join_recipient_lists(&[to_addrs, cc_addrs, bcc_addrs]);
            let addrs = crate::sync::ingest::parse_stored_recipients(&joined);
            Self::sync_message_recipients_conn(&tx, account_id, message_id, &addrs)?;
        }
        tx.commit()?;
        Ok(true)
    }
}

/// [`SqliteStore::is_known_contact`] for a caller already holding the
/// connection: the one definition of "the owner has written to this address",
/// so a read that asks it for many senders under one lock cannot drift from
/// the floor triage applies.
pub(super) fn is_known_contact_conn(
    conn: &Connection,
    account_id: AccountId,
    addr: &str,
) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM contacts
             WHERE account_id=?1 AND addr=?2 COLLATE NOCASE AND sent_count > 0",
        params![account_id, addr],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// What the Auth page reads off one sealed row's decision: the kinds (for
/// pairing), whether it is critical, and the one kind a client sees.
pub(super) struct SealedAuth {
    pub kinds: Vec<crate::triage::decision::AuthKind>,
    pub critical: bool,
    pub wire_kind: Option<String>,
}

impl SealedAuth {
    /// `assessed` is whether the agent pipeline owns the row (it has an
    /// `agent_message_state`); a legacy row keeps its stored `sealed_kind`.
    /// Kinds this binary does not know are skipped rather than failing the
    /// page: a newer daemon's decision is still auth mail.
    pub fn read(assessed: bool, auth_json: Option<&str>, legacy: Option<String>) -> Self {
        use crate::triage::decision::AuthKind;
        if !assessed {
            let kinds = legacy
                .as_deref()
                .and_then(crate::types::SealedKind::parse)
                .map(AuthKind::from)
                .into_iter()
                .collect();
            return Self {
                kinds,
                critical: false,
                wire_kind: legacy,
            };
        }
        #[derive(serde::Deserialize, Default)]
        struct Raw {
            #[serde(default)]
            kinds: Vec<serde_json::Value>,
            #[serde(default)]
            critical: bool,
        }
        let raw: Raw = auth_json
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default();
        let kinds: Vec<AuthKind> = raw
            .kinds
            .into_iter()
            .filter_map(|kind| serde_json::from_value(kind).ok())
            .collect();
        let critical = crate::triage::decision::AuthAssessment {
            kinds: kinds.clone(),
            evidence: Vec::new(),
            critical: raw.critical,
        }
        .is_critical();
        let wire_kind = AuthKind::priority_pick(&kinds)
            .map(|kind| crate::types::SealedKind::from(kind).as_str().to_owned());
        Self {
            kinds,
            critical,
            wire_kind,
        }
    }
}

/// The Auth page's rows (`sealed_membership_sql!`) matching `filter`, newest
/// first, each with what the pairing reads off it. `filter` is SQL over the
/// same aliases; `extra` binds from `?2`.
fn sealed_rows(
    conn: &rusqlite::Connection,
    account_id: AccountId,
    filter: &str,
    extra: &[String],
) -> Result<Vec<(SealedMessage, crate::triage::auth_pairing::AuthMailRow)>> {
    let sql = format!(
        "{} AND {filter} ORDER BY m.received_at DESC, m.id DESC",
        concat!(
            "SELECT m.id, m.account_id, m.thread_id, m.from_addr, m.subject, m.received_at,
                    s.message_id IS NOT NULL, json_extract(d.decision_json,'$.auth'), t.sealed_kind,
                    v.verdict, m.auth_pass,
                    (SELECT MIN(al.ts) FROM audit_log al WHERE al.account_id=m.account_id
                           AND al.action='reveal_sealed' AND al.target=CAST(m.id AS TEXT)
                           AND al.detail IS NOT 'notification')
             FROM messages m
             LEFT JOIN triage t ON t.message_id = m.id AND t.account_id=m.account_id
             LEFT JOIN agent_message_state s ON s.message_id=m.id AND s.account_id=m.account_id
             LEFT JOIN agent_message_decisions d ON d.message_id=m.id AND d.account_id=m.account_id",
            auth_verdict_join_sql!(),
            "WHERE m.account_id = ?1 AND ",
            sealed_membership_sql!()
        )
    );
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&account_id];
    params.extend(extra.iter().map(|value| value as &dyn rusqlite::ToSql));
    // Cached: the done-row read runs once per merged alert window.
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt
        .query_map(params.as_slice(), |r| {
            let auth = SealedAuth::read(
                r.get(6)?,
                r.get::<_, Option<String>>(7)?.as_deref(),
                r.get(8)?,
            );
            let verdict = r
                .get::<_, Option<String>>(9)?
                .as_deref()
                .and_then(crate::types::AuthVerdict::parse);
            let auth_pass: Option<bool> = r.get(10)?;
            // Effective, as FYE reads it (`auth_critical_sql!`): a sender
            // that explicitly failed authentication makes no critical change.
            let critical = auth.critical && auth_pass != Some(false);
            let message = SealedMessage {
                id: r.get(0)?,
                account_id: r.get(1)?,
                thread_id: r.get(2)?,
                from_addr: r.get(3)?,
                subject: r.get(4)?,
                received_at: dt(r, 5)?,
                sealed_kind: auth.wire_kind,
                critical,
                verdict,
                explained_by: None,
            };
            let pairing = crate::triage::auth_pairing::AuthMailRow {
                id: message.id,
                from_addr: message.from_addr.clone(),
                subject: message.subject.clone(),
                received_at: message.received_at,
                kinds: auth.kinds,
                critical,
                auth_pass: auth_pass == Some(true),
                revealed_at: dt_opt(r, 11)?,
                not_mine: verdict == Some(crate::types::AuthVerdict::NotMine),
                ruled: verdict.is_some(),
            };
            Ok((message, pairing))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}
