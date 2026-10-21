//! `message_translations` (docs/TRANSLATION.md, "Storage"): the English
//! version of mail written in another language, one row per message, BESIDE
//! the message and never in it. `messages.subject`/`body`/`body_html` are
//! hashed into the agent's content snapshot, so a translation written there
//! would re-triage the message and reset its external access.
//!
//! LIFECYCLE. Ingest inserts `pending` (live mail) or `deferred` (archive
//! tier, which spends nothing until the thread is opened) in its own
//! transaction. The translate pass claims `pending` rows, and a claim is a
//! LEASE: it counts an attempt and stamps the row, so a pass that dies
//! mid-call leaves a row the next pass picks up after [`LEASE_SECS`], and a
//! row that keeps killing passes runs out of attempts like any other failure.
//! `failed` rows come back after an exponential backoff, and none is claimed
//! once it has had [`MAX_ATTEMPTS`].
//!
//! Spam and sent mail never get a row at ingest, and the claim re-checks
//! both: a row can predate the spam walk's verdict on its message. Neither is
//! mail a PERSON SEALED (`triage.sensitivity = 'sealed'`, which only
//! `correct_triage` and legacy rows write): the extractors refuse
//! `Sensitivity::Sealed` and so does this.
//!
//! THE SEARCH INDEX FOLLOWS THE ROW. `messages_fts.tsubject`/`tbody` and
//! `message_translation_vecs` hold a translation exactly while it is served
//! (`done` or `incomplete`): a translation landing rewrites the FTS row, and a
//! translation dropped deletes its vector and rewrites the FTS row again. A
//! vec0 table has no foreign keys, so nothing else would.

use super::*;
use crate::store::{MessageTranslation, TranslationJob, TranslationStatus, TranslationWrite};

/// Attempts a row gets, claims that died mid-call included.
pub const MAX_ATTEMPTS: i64 = 5;
/// How long a claimed row is left alone before another pass may take it.
pub const LEASE_SECS: i64 = 15 * 60;
/// The first retry wait after a failure, doubled per attempt.
pub const BACKOFF_BASE_SECS: i64 = 10 * 60;
/// The longest retry wait.
pub const BACKOFF_CAP_SECS: i64 = 12 * 60 * 60;

/// THE ONE SELECT that produces an FTS row: `(rowid, subject, body, tsubject,
/// tbody)` for every message, the translation columns NULL unless a served
/// translation exists. The rebuild refills from it, the test fixtures copy
/// it, and [`refresh_fts_conn`] narrows it to one row, so the index can only
/// ever hold what this says.
pub(crate) const FTS_ROW_SELECT: &str = "SELECT m.id, m.subject, m.body, tr.subject, tr.body
     FROM messages m
     LEFT JOIN message_translations tr
       ON tr.message_id = m.id AND tr.status IN ('done','incomplete')";

/// Rewrite one message's FTS row from the stored message and its served
/// translation, inside the caller's transaction.
pub(super) fn refresh_fts_conn(conn: &Connection, message_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM messages_fts WHERE rowid = ?1",
        params![message_id],
    )?;
    conn.execute(
        &format!(
            "INSERT INTO messages_fts(rowid, subject, body, tsubject, tbody)
             {FTS_ROW_SELECT} WHERE m.id = ?1"
        ),
        params![message_id],
    )?;
    Ok(())
}

/// Queue `message_id` for translation from `source_lang`, inside the caller's
/// transaction. `deferred` parks it until its thread is opened. A row that
/// already exists is left as it is: a re-ingest of the same message must not
/// throw away a translation it already paid for.
pub(super) fn queue_translation_conn(
    conn: &Connection,
    account_id: AccountId,
    message_id: i64,
    source_lang: &str,
    deferred: bool,
    now: DateTime<Utc>,
) -> Result<()> {
    let status = if deferred {
        TranslationStatus::Deferred
    } else {
        TranslationStatus::Pending
    };
    conn.execute(
        "INSERT INTO message_translations
             (message_id, account_id, source_lang, status, attempts, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?5)
         ON CONFLICT(message_id) DO NOTHING",
        params![
            message_id,
            account_id,
            source_lang,
            status.as_str(),
            now.timestamp()
        ],
    )?;
    Ok(())
}

/// Drop `message_id`'s translation, inside the caller's transaction. What a
/// heal does before it re-queues: the text the row was translated from is
/// the text the heal just replaced.
pub(super) fn drop_translation_conn(conn: &Connection, message_id: i64) -> Result<()> {
    let dropped = conn.execute(
        "DELETE FROM message_translations WHERE message_id = ?1",
        params![message_id],
    )?;
    if dropped > 0 {
        // vec0 has no foreign key to cascade on: the vector goes by hand, and
        // the FTS row is rewritten so the dropped text stops matching.
        conn.execute(
            "DELETE FROM message_translation_vecs WHERE message_id = ?1",
            params![message_id],
        )?;
        refresh_fts_conn(conn, message_id)?;
    }
    Ok(())
}

fn strings_json(items: &[String]) -> Option<String> {
    (!items.is_empty()).then(|| serde_json::to_string(items).unwrap_or_else(|_| "[]".into()))
}

impl SqliteStore {
    pub(super) fn queue_translation(
        &self,
        account_id: AccountId,
        message_id: i64,
        source_lang: &str,
        deferred: bool,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let conn = self.lock()?;
        queue_translation_conn(&conn, account_id, message_id, source_lang, deferred, now)
    }

    pub(super) fn claim_translations(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<TranslationJob>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let now_s = now.timestamp();
        let jobs: Vec<TranslationJob> = {
            let mut stmt = tx.prepare(
                "SELECT t.message_id, t.account_id, t.source_lang,
                        m.subject, m.body, m.body_html, t.attempts
                 FROM message_translations t
                 JOIN messages m ON m.id = t.message_id
                 LEFT JOIN triage tg ON tg.message_id = m.id
                 WHERE t.account_id = ?1
                   AND m.is_spam = 0 AND m.is_sent = 0
                   AND COALESCE(tg.sensitivity, 'normal') != 'sealed'
                   AND t.attempts < ?3
                   AND (
                        (t.status = 'pending'
                         AND (t.attempts = 0 OR t.updated_at + ?4 <= ?2))
                     OR (t.status = 'failed'
                         AND t.updated_at
                             + MIN(?5 << MAX(t.attempts - 1, 0), ?6) <= ?2)
                   )
                 ORDER BY m.received_at DESC, t.message_id DESC
                 LIMIT ?7",
            )?;
            stmt.query_map(
                params![
                    account_id,
                    now_s,
                    MAX_ATTEMPTS,
                    LEASE_SECS,
                    BACKOFF_BASE_SECS,
                    BACKOFF_CAP_SECS,
                    limit as i64
                ],
                |r| {
                    Ok(TranslationJob {
                        message_id: r.get(0)?,
                        account_id: r.get(1)?,
                        source_lang: r.get(2)?,
                        subject: r.get(3)?,
                        body: r.get(4)?,
                        body_html: r.get(5)?,
                        attempts: r.get::<_, i64>(6)? + 1,
                    })
                },
            )?
            .collect::<rusqlite::Result<_>>()?
        };
        // THE LEASE: the attempt is counted when it is taken, so a call that
        // never comes back still spends one.
        for job in &jobs {
            tx.execute(
                "UPDATE message_translations
                 SET status = 'pending', attempts = attempts + 1, updated_at = ?2
                 WHERE message_id = ?1",
                params![job.message_id, now_s],
            )?;
        }
        tx.commit()?;
        Ok(jobs)
    }

    pub(super) fn record_translation(
        &self,
        message_id: i64,
        write: &TranslationWrite<'_>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        // Every arm writes only over a row still in flight (`pending`): a
        // heal that dropped and re-queued it meanwhile owns it now.
        match write {
            TranslationWrite::Translated {
                translated: t,
                model,
                prompt_version,
            } => {
                let status = if t.missing_facts.is_empty() {
                    TranslationStatus::Done
                } else {
                    TranslationStatus::Incomplete
                };
                let n = tx.execute(
                    "UPDATE message_translations
                     SET status = ?2, subject = ?3, body = ?4, body_html = ?5,
                         snippet = ?6, missing_facts = ?7, model = ?8,
                         prompt_version = ?9, updated_at = ?10,
                         source_lang = COALESCE(?11, source_lang)
                     WHERE message_id = ?1 AND status = 'pending'",
                    params![
                        message_id,
                        status.as_str(),
                        t.subject,
                        t.body,
                        t.body_html,
                        t.snippet,
                        strings_json(&t.missing_facts),
                        model,
                        prompt_version,
                        now.timestamp(),
                        t.source_lang.map(|l| l.code()),
                    ],
                )?;
                // THE KEYWORD INDEX LEARNS THE ENGLISH in the same
                // transaction; the vector is the caller's, embedded off the
                // store lock.
                if n > 0 {
                    refresh_fts_conn(&tx, message_id)?;
                }
            }
            TranslationWrite::Failed { model } => {
                tx.execute(
                    "UPDATE message_translations
                     SET status = 'failed', model = ?2, updated_at = ?3
                     WHERE message_id = ?1 AND status = 'pending'",
                    params![message_id, model, now.timestamp()],
                )?;
            }
            TranslationWrite::NotNeeded {
                model,
                prompt_version,
            } => {
                tx.execute(
                    "UPDATE message_translations
                     SET status = 'not_needed', model = ?2, prompt_version = ?3,
                         subject = NULL, body = NULL, body_html = NULL,
                         snippet = NULL, missing_facts = NULL, updated_at = ?4
                     WHERE message_id = ?1 AND status = 'pending'",
                    params![message_id, model, prompt_version, now.timestamp()],
                )?;
            }
            TranslationWrite::Released => {
                tx.execute(
                    "UPDATE message_translations
                     SET attempts = MAX(attempts - 1, 0), updated_at = ?2
                     WHERE message_id = ?1 AND status = 'pending'",
                    params![message_id, now.timestamp()],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub(super) fn promote_deferred_translations(
        &self,
        account_id: AccountId,
        thread_id: &str,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE message_translations
             SET status = 'pending', updated_at = ?3
             WHERE status = 'deferred' AND account_id = ?1
               AND message_id IN (
                   SELECT id FROM messages WHERE account_id = ?1 AND thread_id = ?2
               )",
            params![account_id, thread_id, now.timestamp()],
        )?;
        Ok(n)
    }

    pub(super) fn message_translations(
        &self,
        account_id: AccountId,
        message_ids: &[i64],
    ) -> Result<Vec<MessageTranslation>> {
        if message_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock()?;
        let ids = serde_json::to_string(message_ids).unwrap_or_else(|_| "[]".into());
        let mut stmt = conn.prepare(
            "SELECT message_id, source_lang, status, subject, body, body_html, snippet,
                    missing_facts, model, prompt_version, attempts
             FROM message_translations
             WHERE account_id = ?1
               AND message_id IN (SELECT value FROM json_each(?2))
             ORDER BY message_id",
        )?;
        let rows = stmt
            .query_map(params![account_id, ids], |r| {
                let status: String = r.get(2)?;
                let missing: Option<String> = r.get(7)?;
                Ok(MessageTranslation {
                    message_id: r.get(0)?,
                    source_lang: r.get(1)?,
                    status: TranslationStatus::parse(&status).unwrap_or(TranslationStatus::Failed),
                    subject: r.get(3)?,
                    body: r.get(4)?,
                    body_html: r.get(5)?,
                    snippet: r.get(6)?,
                    missing_facts: missing
                        .and_then(|m| serde_json::from_str(&m).ok())
                        .unwrap_or_default(),
                    model: r.get(8)?,
                    prompt_version: r.get(9)?,
                    attempts: r.get(10)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
}
