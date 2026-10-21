//! `message_translations` (docs/TRANSLATION.md, "Storage"): the English
//! version of mail written in another language, one row per message, BESIDE
//! the message and never in it. `messages.subject`/`body`/`body_html` are
//! hashed into the agent's content snapshot, so a translation written there
//! would re-triage the message and reset its external access.
//!
//! LIFECYCLE. Ingest inserts `pending` (live mail) or `deferred` (archive
//! tier, which spends nothing until the reader displaying its thread asks,
//! over `POST /client/thread/{id}/translate`; a plain read of the thread
//! serves it as `deferred` and promotes nothing) in its own transaction. The translate pass claims `pending` rows, and a claim is a
//! LEASE: it counts an attempt and stamps the row, so a pass that dies
//! mid-call leaves a row the next pass picks up after [`LEASE_SECS`], and a
//! row that keeps killing passes runs out of attempts like any other failure.
//! `failed` rows come back after an exponential backoff, and none is claimed
//! once it has had [`MAX_ATTEMPTS`].
//!
//! Spam, sent and the account's own mail never get a row at ingest, and the
//! claim re-checks all three, plus SEALED mail (an auth decision, or a legacy
//! `triage.sensitivity = 'sealed'` row with no agent state: [`SEALED_SQL`],
//! the predicate `sealed_messages` uses): a row can predate the verdict on
//! its message. Such a row is CLOSED at the next claim, and a translation it
//! was already serving is dropped with its index columns and vector.
//! Because translation runs ahead of triage, brand-new auth mail may be
//! translated before triage seals it; that is accepted (docs/TRANSLATION.md,
//! "Sealed mail").
//!
//! THE LEASE IS A TOKEN. A claim stamps `lease`, every write of a result
//! matches on it, and every terminal write clears it, so a heal that dropped
//! and re-queued the row (or a pass that re-took an expired lease) owns the
//! row from then on and a late result writes nothing.
//!
//! THE TRANSLATION IS PART OF THE AGENT'S SNAPSHOT. A served translation
//! landing, or being dropped, changes what the triage agent and the access
//! assessment read, so it goes through the agent bookkeeping like any other
//! content change ([`translation_changed_conn`]). Triage holds a message back
//! while its row is `pending`, for at most [`TRIAGE_WAIT_SECS`] after it was
//! queued, so foreign mail is normally triaged once, on the English.
//!
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
/// How long agent triage holds a live message back while its translation is
/// `pending`, from when the row was queued. Long enough for a normal
/// translation to land first, short enough that a translator that is down,
/// over its daily cap or cooling down delays triage by minutes and no more.
pub const TRIAGE_WAIT_SECS: i64 = 10 * 60;
/// The note an `incomplete` row carries when the engine translated only the
/// head of an over-long message. Shown with the other missing facts.
pub const TRUNCATED_NOTE: &str = "only the first part was translated";

/// SEALED MAIL over alias `m`: the auth decision the agent wrote, or a legacy
/// `triage.sensitivity = 'sealed'` row that no agent state has superseded.
/// The same predicate as `sealed_messages` (messages.rs), which is what the
/// Auth page lists and the human thread view refuses.
pub(super) const SEALED_SQL: &str = "(EXISTS(SELECT 1 FROM agent_message_decisions sd
        WHERE sd.account_id=m.account_id AND sd.message_id=m.id
          AND json_array_length(COALESCE(sd.decision_json,'{}'),'$.auth.kinds')>0)
     OR (NOT EXISTS(SELECT 1 FROM agent_message_state ss
             WHERE ss.account_id=m.account_id AND ss.message_id=m.id)
         AND EXISTS(SELECT 1 FROM triage st
             WHERE st.message_id=m.id AND st.sensitivity='sealed')))";

/// Mail that must never be translated, over alias `m`: spam, the account's
/// own mail (the same authorship test the human thread view uses), sealed.
fn untranslatable_sql() -> String {
    format!("(m.is_spam=1 OR {OWN_MAIL} OR {SEALED_SQL})")
}

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

/// Whether `message_id` must never be translated (see
/// [`untranslatable_sql`]). Ingest asks it of the row it just wrote.
pub(super) fn is_untranslatable_conn(conn: &Connection, message_id: i64) -> Result<bool> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {} FROM messages m WHERE m.id = ?1",
                untranslatable_sql()
            ),
            params![message_id],
            |r| r.get::<_, bool>(0),
        )
        .optional()?
        .unwrap_or(true))
}

/// Queue `message_id` for translation from `source_lang`, inside the caller's
/// transaction. `deferred` parks it until the reader displaying its thread
/// asks. A row that already exists is left as it is: a re-ingest of the same
/// message must not throw away a translation it already paid for. (An archive
/// row seen again live never gets here; ingest leaves it alone. The owner's
/// promotion is what makes it live: [`promote_live_translation_conn`].)
pub(super) fn queue_translation_conn(
    conn: &Connection,
    account_id: AccountId,
    message_id: i64,
    source_lang: &str,
    deferred: bool,
    now: DateTime<Utc>,
) -> Result<()> {
    let (status, wait_until) = if deferred {
        (TranslationStatus::Deferred, None)
    } else {
        (
            TranslationStatus::Pending,
            Some(now.timestamp() + TRIAGE_WAIT_SECS),
        )
    };
    conn.execute(
        "INSERT INTO message_translations
             (message_id, account_id, source_lang, status, attempts,
              triage_wait_until, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?6)
         ON CONFLICT(message_id) DO NOTHING",
        params![
            message_id,
            account_id,
            source_lang,
            status.as_str(),
            wait_until,
            now.timestamp()
        ],
    )?;
    Ok(())
}

/// An archive row just became live (the owner's promotion,
/// `promote_archive_conn`): its `deferred` translation is queued as live mail's
/// is, triage wait included. A plain live re-sighting of an archive row is NOT
/// this: ingest leaves such a row exactly as it is, translation included, for
/// the reason the archive tier exists.
pub(super) fn promote_live_translation_conn(
    conn: &Connection,
    message_id: i64,
    now: DateTime<Utc>,
) -> Result<()> {
    conn.execute(
        "UPDATE message_translations
         SET status = 'pending', updated_at = ?2, triage_wait_until = ?3
         WHERE message_id = ?1 AND status = 'deferred'",
        params![
            message_id,
            now.timestamp(),
            now.timestamp() + TRIAGE_WAIT_SECS
        ],
    )?;
    Ok(())
}

/// Drop `message_id`'s translation, inside the caller's transaction. What a
/// heal does before it re-queues: the text the row was translated from is
/// the text the heal just replaced. The heal's own agent bookkeeping follows
/// in the same transaction, so this does not re-enqueue.
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

/// A served translation landed on, or left, `message`: the triage agent's
/// content snapshot covers it, so the agent bookkeeping runs as for any
/// content change. Live mail queues one re-triage (trigger `translation`). An
/// archive row queues an access reassessment only if it had been assessed
/// as readable; otherwise its stored snapshot is brought up to date in place,
/// still `pending`, and spends nothing until somebody asks for it.
pub(super) fn translation_changed_conn(
    conn: &Connection,
    account: AccountId,
    message: i64,
) -> Result<()> {
    let state: Option<(bool, String)> = conn
        .query_row(
            "SELECT m.is_archive, COALESCE(s.access, '')
             FROM messages m
             LEFT JOIN agent_message_state s
               ON s.account_id = m.account_id AND s.message_id = m.id
             WHERE m.account_id = ?1 AND m.id = ?2",
            params![account, message],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((archive, access)) = state else {
        return Ok(());
    };
    if !archive {
        return super::agent_triage::enqueue_agent_triage_conn(
            conn,
            account,
            message,
            "translation",
            false,
        );
    }
    if access == "allowed" {
        return super::agent_triage::enqueue_agent_triage_conn(
            conn,
            account,
            message,
            "source_access",
            false,
        );
    }
    super::agent_triage::reseed_archive_snapshot_conn(conn, account, message)
}

/// Close a row its message no longer permits (spam, sealed, own mail):
/// terminal `failed` with its attempts spent and its text gone, and when it
/// was serving a translation, its vector and index columns with it.
fn close_translation_conn(
    conn: &Connection,
    account: AccountId,
    message_id: i64,
    served: bool,
    now: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE message_translations
         SET status = 'failed', attempts = MAX(attempts, ?2), lease = NULL,
             subject = NULL, body = NULL, body_html = NULL, snippet = NULL,
             missing_facts = NULL, triage_wait_until = NULL, updated_at = ?3
         WHERE message_id = ?1",
        params![message_id, MAX_ATTEMPTS, now],
    )?;
    if served {
        conn.execute(
            "DELETE FROM message_translation_vecs WHERE message_id = ?1",
            params![message_id],
        )?;
        refresh_fts_conn(conn, message_id)?;
        translation_changed_conn(conn, account, message_id)?;
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
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now_s = now.timestamp();
        // SETTLE WHAT NOTHING WILL EVER FINISH, first. A lease that expired
        // on the last attempt would otherwise sit `pending` for good, which
        // the reader shows as "Translating..." for good.
        tx.execute(
            "UPDATE message_translations
             SET status = 'failed', lease = NULL, triage_wait_until = NULL
             WHERE account_id = ?1 AND status = 'pending'
               AND attempts >= ?2 AND updated_at + ?3 <= ?4",
            params![account_id, MAX_ATTEMPTS, LEASE_SECS, now_s],
        )?;
        // And rows whose message became spam, sealed or own mail after it
        // was queued: closed, and a served translation dropped.
        let closing: Vec<(i64, bool)> = {
            let mut stmt = tx.prepare(&format!(
                "SELECT t.message_id, t.status IN ('done','incomplete')
                 FROM message_translations t
                 JOIN messages m ON m.id = t.message_id
                 WHERE t.account_id = ?1
                   AND t.status IN ('deferred','pending','failed','done','incomplete')
                   AND NOT (t.status = 'failed' AND t.attempts >= ?2)
                   AND {}",
                untranslatable_sql()
            ))?;
            stmt.query_map(params![account_id, MAX_ATTEMPTS], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        for (message_id, served) in closing {
            close_translation_conn(&tx, account_id, message_id, served, now_s)?;
        }
        let jobs: Vec<TranslationJob> = {
            let mut stmt = tx.prepare(&format!(
                "SELECT t.message_id, t.account_id, t.source_lang,
                        m.subject, m.body, m.body_html, t.attempts,
                        t.status, t.updated_at
                 FROM message_translations t
                 JOIN messages m ON m.id = t.message_id
                 WHERE t.account_id = ?1
                   AND NOT {}
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
                untranslatable_sql()
            ))?;
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
                    let prev: String = r.get(7)?;
                    Ok(TranslationJob {
                        message_id: r.get(0)?,
                        account_id: r.get(1)?,
                        source_lang: r.get(2)?,
                        subject: r.get(3)?,
                        body: r.get(4)?,
                        body_html: r.get(5)?,
                        attempts: r.get::<_, i64>(6)? + 1,
                        lease: String::new(),
                        prev_status: TranslationStatus::parse(&prev)
                            .unwrap_or(TranslationStatus::Pending),
                        prev_updated_at: r.get(8)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<_>>()?
        };
        // THE LEASE: the attempt is counted when it is taken, so a call that
        // never comes back still spends one, and the row carries a fresh
        // token that only this claim's result can match.
        let mut leased = Vec::with_capacity(jobs.len());
        for mut job in jobs {
            job.lease = tx.query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0))?;
            tx.execute(
                "UPDATE message_translations
                 SET status = 'pending', attempts = attempts + 1, updated_at = ?2,
                     lease = ?3
                 WHERE message_id = ?1",
                params![job.message_id, now_s, job.lease],
            )?;
            leased.push(job);
        }
        tx.commit()?;
        Ok(leased)
    }

    pub(super) fn record_translation(
        &self,
        job: &TranslationJob,
        write: &TranslationWrite<'_>,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let message_id = job.message_id;
        // EVERY ARM WRITES ONLY OVER ITS OWN LEASE: a row still `pending`
        // under the token this job's claim stamped. A heal that dropped and
        // re-queued it, or a pass that re-took an expired lease, owns it now.
        let n = match write {
            TranslationWrite::Translated {
                translated: t,
                model,
                prompt_version,
            } => {
                let mut missing = t.missing_facts.clone();
                if t.truncated {
                    missing.push(TRUNCATED_NOTE.to_string());
                }
                let status = if missing.is_empty() {
                    TranslationStatus::Done
                } else {
                    TranslationStatus::Incomplete
                };
                let n = tx.execute(
                    "UPDATE message_translations
                     SET status = ?2, subject = ?3, body = ?4, body_html = ?5,
                         snippet = ?6, missing_facts = ?7, model = ?8,
                         prompt_version = ?9, updated_at = ?10,
                         source_lang = COALESCE(?11, source_lang),
                         lease = NULL, triage_wait_until = NULL
                     WHERE message_id = ?1 AND status = 'pending' AND lease = ?12",
                    params![
                        message_id,
                        status.as_str(),
                        t.subject,
                        t.body,
                        t.body_html,
                        t.snippet,
                        strings_json(&missing),
                        model,
                        prompt_version,
                        now.timestamp(),
                        t.source_lang.map(|l| l.code()),
                        job.lease,
                    ],
                )?;
                // THE KEYWORD INDEX LEARNS THE ENGLISH in the same
                // transaction, and so does the agent's snapshot; the vector
                // is the caller's, embedded off the store lock.
                if n > 0 {
                    refresh_fts_conn(&tx, message_id)?;
                    translation_changed_conn(&tx, job.account_id, message_id)?;
                }
                n
            }
            TranslationWrite::Failed { model } => tx.execute(
                "UPDATE message_translations
                 SET status = 'failed', model = ?2, updated_at = ?3, lease = NULL,
                     triage_wait_until = NULL
                 WHERE message_id = ?1 AND status = 'pending' AND lease = ?4",
                params![message_id, model, now.timestamp(), job.lease],
            )?,
            TranslationWrite::NotNeeded {
                model,
                prompt_version,
            }
            | TranslationWrite::Rejected {
                model,
                prompt_version,
            } => {
                let status = if matches!(write, TranslationWrite::Rejected { .. }) {
                    TranslationStatus::Rejected
                } else {
                    TranslationStatus::NotNeeded
                };
                tx.execute(
                    "UPDATE message_translations
                     SET status = ?2, model = ?3, prompt_version = ?4,
                         subject = NULL, body = NULL, body_html = NULL,
                         snippet = NULL, missing_facts = NULL, updated_at = ?5,
                         lease = NULL, triage_wait_until = NULL
                     WHERE message_id = ?1 AND status = 'pending' AND lease = ?6",
                    params![
                        message_id,
                        status.as_str(),
                        model,
                        prompt_version,
                        now.timestamp(),
                        job.lease
                    ],
                )?
            }
            // Back to exactly what the claim found: the status, the
            // `updated_at` its backoff counts from, and the attempt.
            TranslationWrite::Released => tx.execute(
                "UPDATE message_translations
                 SET status = ?2, attempts = MAX(attempts - 1, 0), updated_at = ?3,
                     lease = NULL
                 WHERE message_id = ?1 AND status = 'pending' AND lease = ?4",
                params![
                    message_id,
                    job.prev_status.as_str(),
                    job.prev_updated_at,
                    job.lease
                ],
            )?,
        };
        tx.commit()?;
        Ok(n > 0)
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
