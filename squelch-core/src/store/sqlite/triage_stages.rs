//! The Stage-1 / Stage-2 / extract queues, their applies, the LLM usage
//! ledger and the per-account cap settings.

use super::messages::rewrite_deadline_conn;
use super::*;

// ---- LLM usage ledger helpers (shared by the stage-1 and stage-2 categories) --

/// Bump the `stage2_usage` ledger for `(account, day, category)`: +1 call and add
/// the token counts. Both triage stages share this table keyed by `category`.
#[allow(clippy::too_many_arguments)] // the parts of one usage ledger line
fn bump_usage_category(
    conn: &Connection,
    account_id: AccountId,
    day: &str,
    category: &str,
    tokens: UsageTokens,
) -> Result<()> {
    conn.execute(
        "INSERT INTO stage2_usage(account_id, day, category, calls, input_tokens, output_tokens,
                                  cache_creation_tokens, cache_read_tokens)
         VALUES(?1, ?2, ?3, 1, ?4, ?5, ?6, ?7)
         ON CONFLICT(account_id, day, category) DO UPDATE SET
             calls = calls + 1,
             input_tokens = input_tokens + excluded.input_tokens,
             output_tokens = output_tokens + excluded.output_tokens,
             cache_creation_tokens = cache_creation_tokens + excluded.cache_creation_tokens,
             cache_read_tokens = cache_read_tokens + excluded.cache_read_tokens",
        params![
            account_id,
            day,
            category,
            tokens.input as i64,
            tokens.output as i64,
            tokens.cache_creation as i64,
            tokens.cache_read as i64
        ],
    )?;
    Ok(())
}

/// Sum the ledger for `(account, category)` over every day `>= since_day`.
fn usage_since_category(
    conn: &Connection,
    account_id: AccountId,
    since_day: &str,
    category: &str,
) -> Result<Stage2Usage> {
    let row = conn.query_row(
        "SELECT COALESCE(SUM(calls), 0), COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(output_tokens), 0), COALESCE(SUM(cache_creation_tokens), 0),
                COALESCE(SUM(cache_read_tokens), 0)
         FROM stage2_usage
         WHERE account_id = ?1 AND day >= ?2 AND category = ?3",
        params![account_id, since_day, category],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        },
    )?;
    Ok(Stage2Usage {
        calls: row.0.max(0) as u64,
        input_tokens: row.1.max(0) as u64,
        output_tokens: row.2.max(0) as u64,
        cache_creation_tokens: row.3.max(0) as u64,
        cache_read_tokens: row.4.max(0) as u64,
    })
}

/// The most recent `days` rows for `(account, category)`, newest-first (sparse).
fn list_usage_category(
    conn: &Connection,
    account_id: AccountId,
    days: u32,
    category: &str,
) -> Result<Vec<Stage2UsageDay>> {
    let mut stmt = conn.prepare(
        "SELECT day, calls, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens
         FROM stage2_usage
         WHERE account_id = ?1 AND category = ?3
         ORDER BY day DESC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![account_id, days as i64, category], |r| {
            Ok(Stage2UsageDay {
                day: r.get::<_, String>(0)?,
                calls: r.get::<_, i64>(1)?.max(0) as u64,
                input_tokens: r.get::<_, i64>(2)?.max(0) as u64,
                output_tokens: r.get::<_, i64>(3)?.max(0) as u64,
                cache_creation_tokens: r.get::<_, i64>(4)?.max(0) as u64,
                cache_read_tokens: r.get::<_, i64>(5)?.max(0) as u64,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

// Keep the time-window range as the outer loop. Each matching request makes an
// indexed equality probe into jobs, including the computed trigger value.
// A job is PARKED when the worker deferred it past now because a daily budget
// ran out; ?3 is now.
pub(super) const RETRIAGE_PROGRESS_SQL: &str =
    "SELECT COUNT(*),COALESCE(SUM(CASE WHEN j.state IN ('completed','failed')
                 THEN 1 ELSE 0 END),0),MIN(t.retriage_at),
                 COALESCE(SUM(CASE WHEN j.state='queued' AND j.last_error='daily_budget_exhausted'
                     AND j.available_at>?3 THEN 1 ELSE 0 END),0),
                 MIN(CASE WHEN j.state='queued' AND j.last_error='daily_budget_exhausted'
                     AND j.available_at>?3 THEN j.available_at END)
             FROM triage t INDEXED BY idx_triage_retriage_window
             CROSS JOIN agent_triage_jobs j INDEXED BY idx_agent_jobs_retriage_progress
               ON j.account_id=t.account_id AND j.message_id=t.message_id
              AND j.trigger='manual:' || t.retriage_at
              AND j.kind IN ('triage','access')
             WHERE t.account_id=?1 AND t.retriage_at>=?2";

impl SqliteStore {
    pub(super) fn extract_queue(
        &self,
        account_id: AccountId,
        categories: &[&str],
        limit: usize,
    ) -> Result<Vec<ExtractQueued>> {
        if categories.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock()?;
        // A message that already produced a RECEIPT is excluded: a receipt and a
        // banking row must never double-create. The category IN (...) list is
        // built from bound params, never string-interpolated.
        let placeholders = (0..categories.len())
            .map(|i| format!("?{}", i + 3))
            .collect::<Vec<_>>()
            .join(",");
        // Hand-requested rows (`retriage_at` set) sort first.
        let sql = format!(
            "SELECT m.id, m.thread_id, m.from_addr, m.from_name, m.subject, m.body,
                    t.category, t.sensitivity, m.received_at, t.retriage_at
             FROM triage t
             JOIN messages m ON m.id = t.message_id
             WHERE t.account_id = ?1
               AND t.category IN ({placeholders})
               AND t.extractor_model_used IS NULL
               AND t.sensitivity = 'normal'
               AND m.is_sent = 0 AND m.is_spam = 0
               AND NOT EXISTS(
                   SELECT 1 FROM receipts r
                   WHERE r.account_id = t.account_id AND r.message_id = t.message_id
               )
             ORDER BY t.retriage_at IS NULL, t.retriage_at DESC, m.received_at DESC
             LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql)?;
        // Bind params: ?1 account_id, ?2 limit, ?3.. categories.
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(categories.len() + 2);
        binds.push(Box::new(account_id));
        binds.push(Box::new(limit as i64));
        for c in categories {
            binds.push(Box::new((*c).to_string()));
        }
        let bind_refs: Vec<&dyn rusqlite::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
        let out = stmt
            .query_map(bind_refs.as_slice(), |r| {
                Ok(ExtractQueued {
                    message_id: r.get(0)?,
                    account_id,
                    thread_id: r.get(1)?,
                    from_addr: r.get(2)?,
                    from_name: r.get(3)?,
                    subject: r.get(4)?,
                    body: r.get(5)?,
                    category: r.get(6)?,
                    sensitivity: Sensitivity::parse(&r.get::<_, String>(7)?),
                    received_at: dt(r, 8)?,
                    retriage_at: dt_opt(r, 9)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(out)
    }

    pub(super) fn ship_extract_queue(
        &self,
        account_id: AccountId,
        limit: usize,
    ) -> Result<Vec<ExtractQueued>> {
        let conn = self.lock()?;
        // Same projection as `extract_queue`, DIFFERENT predicate — and
        // deliberately not that method with another category list. `extract_queue`
        // routes on `triage.category` (no shipping category exists) and excludes
        // receipt-bearing messages, which most order confirmations are; both would
        // silently empty this queue.
        //
        // COALESCE on the category because the trigger is stamped at INGEST: a row
        // can queue here before Stage-1 ever assigns a category, and
        // `ExtractQueued.category` is a `String`.
        // Hand-requested rows (`retriage_at` set) sort first.
        let mut stmt = conn.prepare(
            "SELECT m.id, m.thread_id, m.from_addr, m.from_name, m.subject, m.body,
                    COALESCE(t.category, ''), t.sensitivity, m.received_at, t.retriage_at
             FROM triage t
             JOIN messages m ON m.id = t.message_id
             WHERE t.account_id = ?1
               AND t.ship_extract_model = 'pending'
               AND t.sensitivity = 'normal'
               AND m.is_sent = 0 AND m.is_spam = 0
             ORDER BY t.retriage_at IS NULL, t.retriage_at DESC, m.received_at DESC
             LIMIT ?2",
        )?;
        let out = stmt
            .query_map(params![account_id, limit as i64], |r| {
                Ok(ExtractQueued {
                    message_id: r.get(0)?,
                    account_id,
                    thread_id: r.get(1)?,
                    from_addr: r.get(2)?,
                    from_name: r.get(3)?,
                    subject: r.get(4)?,
                    body: r.get(5)?,
                    category: r.get(6)?,
                    sensitivity: Sensitivity::parse(&r.get::<_, String>(7)?),
                    received_at: dt(r, 8)?,
                    retriage_at: dt_opt(r, 9)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(out)
    }

    pub(super) fn ship_extract_mark(
        &self,
        account_id: AccountId,
        message_id: i64,
        marker: &str,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE triage SET ship_extract_model = ?3
             WHERE message_id = ?1 AND account_id = ?2 AND sensitivity = 'normal'",
            params![message_id, account_id, marker],
        )?;
        Ok(())
    }

    /// Schedule new agent work without erasing the last usable decision or any
    /// user state. Each request has its own identity, including repeated asks.
    pub(super) fn retriage_reset(
        &self,
        account_id: AccountId,
        message_id: Option<i64>,
        days: u32,
    ) -> Result<u64> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let now = Utc::now().to_rfc3339();
        let cutoff = (Utc::now() - chrono::Duration::days(days as i64)).to_rfc3339();
        // THE USER'S OWN MAIL HAS NO VERDICT TO REDO (see `OWN_MAIL`): it is
        // never triaged, only reviewed for access. A window therefore skips it,
        // and one named message that turns out to be theirs stands for the
        // conversation, so the request goes to the newest inbound message in
        // the thread. Enqueued as itself it would queue an access review and
        // answer the human with nothing.
        let ids = match message_id {
            Some(id) => {
                let named: Option<(String, bool, bool)> = tx
                    .query_row(
                        concat!(
                            "SELECT m.thread_id,",
                            own_mail_sql!(),
                            " AND m.is_spam=0,m.is_archive
                             FROM messages m WHERE m.account_id=?1 AND m.id=?2"
                        ),
                        params![account_id, id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?;
                let inbound: Option<i64> = match &named {
                    Some((thread, true, _)) if !thread.is_empty() => tx
                        .query_row(
                            concat!(
                                "SELECT m.id FROM messages m WHERE m.account_id=?1 AND m.thread_id=?2
                                 AND NOT ",
                                own_mail_sql!(),
                                // Never into the archive: the redirect is
                                // ours, not the owner's, and it must not
                                // promote a message they did not name.
                                " AND m.is_spam=0 AND m.is_archive=0
                                 ORDER BY m.received_at DESC,m.id DESC LIMIT 1"
                            ),
                            params![account_id, thread],
                            |row| row.get(0),
                        )
                        .optional()?,
                    _ => None,
                };
                // With no live inbound message to stand for it, own mail is
                // enqueued as itself, UNLESS IT IS IN THE ARCHIVE: the
                // owner's old sent mail is never promoted and never queued,
                // so naming it does nothing at all.
                named
                    .and_then(|(_, own, archived)| {
                        inbound.or_else(|| (!(own && archived)).then_some(id))
                    })
                    .into_iter()
                    .collect()
            }
            None => {
                let mut stmt = tx.prepare(concat!(
                    "SELECT m.id FROM messages m WHERE m.account_id=?1 AND m.received_at>=?2
                     AND NOT ",
                    own_mail_sql!(),
                    // A window never reaches into the archive, however many
                    // days it names: that mail was stored on the promise that
                    // no model reads it unasked. One named message still may.
                    " AND m.is_spam=0 AND m.is_archive=0"
                ))?;
                stmt.query_map(params![account_id, cutoff], |row| row.get::<_, i64>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            }
        };
        // One named message is a human waiting on an answer: foreground, so it
        // pays from the global budget only. A window stays background so one
        // click cannot spend the reserve that new arrivals rely on.
        let foreground = message_id.is_some();
        for id in &ids {
            // ONE NAMED ARCHIVE MESSAGE IS PROMOTED: the owner asked for this
            // one to be triaged, so it becomes live mail in this transaction
            // and the job below is an ordinary triage. A window holds no
            // archive row (the select above), and must never promote one.
            let promoted =
                foreground && super::agent_triage::promote_archive_conn(&tx, account_id, *id)?;
            super::agent_triage::enqueue_agent_triage_lane(
                &tx,
                account_id,
                *id,
                &format!("manual:{now}"),
                false,
                foreground,
            )?;
            if promoted {
                super::agent_triage::requeue_promoted_conn(&tx, account_id, *id, Utc::now())?;
            }
            tx.execute(
                "UPDATE triage SET retriage_at=?3 WHERE account_id=?1 AND message_id=?2",
                params![account_id, id, now],
            )?;
        }
        tx.commit()?;
        Ok(ids.len() as u64)
    }

    /// Progress follows durable jobs, including work reclaimed after restart.
    /// A failed job is terminal; diagnostics preserve its failure separately.
    pub(super) fn retriage_progress(&self, account_id: AccountId) -> Result<RetriageProgress> {
        let conn = self.lock()?;
        let now = Utc::now();
        let since = (now - chrono::Duration::hours(24)).to_rfc3339();
        let (total, done, started_at, parked, resumes_at): (
            i64,
            i64,
            Option<String>,
            i64,
            Option<String>,
        ) = conn.query_row(
            RETRIAGE_PROGRESS_SQL,
            params![account_id, since, now.to_rfc3339()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        let parse = |time: Option<String>| {
            time.as_deref()
                .and_then(|time| DateTime::parse_from_rfc3339(time).ok())
                .map(|time| time.with_timezone(&Utc))
        };
        Ok(RetriageProgress {
            total,
            done,
            started_at: parse(started_at),
            budget_parked: parked,
            budget_resumes_at: parse(resumes_at),
            // The store cannot see sync health; the HTTP door fills this in.
            blocked: None,
        })
    }

    pub(super) fn extract_mark_processed(
        &self,
        account_id: AccountId,
        message_id: i64,
        extractor_model_used: &str,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE triage SET extractor_model_used = ?3
             WHERE message_id = ?1 AND account_id = ?2 AND sensitivity = 'normal'",
            params![message_id, account_id, extractor_model_used],
        )?;
        Ok(())
    }

    // ---- STAGE-2 ----------------------------------------------------------

    pub(super) fn stage1_apply(&self, applied: &Stage1Applied) -> Result<bool> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let deadline_dt = applied.deadline.as_ref().map(|d| d.due_at.to_rfc3339());
        let field_reasons_json = if applied.field_reasons.is_empty() {
            None
        } else {
            serde_json::to_string(&applied.field_reasons).ok()
        };
        // Overwrite the seed values, stamp stage1_model_used (leaving the queue),
        // and set the escalation flag, leaving `model_used` (the Stage-2 marker)
        // untouched. Guarded by sensitivity='normal'.
        let n = tx.execute(
            "UPDATE triage SET
                 importance = ?3,
                 tier = ?4,
                 one_line = ?5,
                 reason = ?6,
                 deadline = ?7,
                 stage1_model_used = ?8,
                 needs_stage2 = ?9,
                 field_reasons = ?10,
                 category = COALESCE(?11, category),
                 escalation_reason = ?12
             WHERE message_id = ?1 AND account_id = ?2 AND sensitivity = 'normal'",
            params![
                applied.message_id,
                applied.account_id,
                applied.importance as i64,
                applied.tier.as_str(),
                applied.one_line,
                applied.reason,
                deadline_dt,
                applied.stage1_model_used,
                applied.needs_stage2 as i64,
                field_reasons_json,
                applied.category,
                applied.escalation_reason,
            ],
        )?;
        // TOCTOU: a row sealed by hand between the queue SELECT and this apply
        // makes the guard match nothing. The verdict did NOT land, so skip the
        // deadlines rewrite too — a sealed message must not grow a fresh deadline
        // row — and report `false` so the caller emits no event.
        if n > 0 {
            rewrite_deadline_conn(
                &tx,
                applied.account_id,
                applied.message_id,
                applied.deadline.as_ref(),
            )?;
        }
        tx.commit()?;
        Ok(n > 0)
    }

    // ---- REVISITS ---------------------------------------------------------

    /// For the sync engine's tests: a message stored under provider id
    /// `gmail`, as (`triage.first_load`, whether it was stamped to notify).
    #[cfg(test)]
    pub(crate) fn triage_by_provider_id(
        &self,
        account_id: AccountId,
        gmail: &str,
    ) -> Result<Option<(bool, bool)>> {
        let conn = self.lock()?;
        Ok(conn
            .query_row(
                "SELECT t.first_load, t.notify_eligible_at IS NOT NULL
                 FROM messages m JOIN triage t ON t.account_id=m.account_id AND t.message_id=m.id
                 WHERE m.account_id=?1 AND m.gmail_msg_id=?2",
                params![account_id, gmail],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// `triage.first_load` for one message, for the sync engine's tests.
    #[cfg(test)]
    pub(crate) fn triage_first_load(&self, account_id: AccountId, message_id: i64) -> Result<bool> {
        let conn = self.lock()?;
        Ok(conn.query_row(
            "SELECT first_load FROM triage WHERE account_id=?1 AND message_id=?2",
            params![account_id, message_id],
            |r| r.get(0),
        )?)
    }

    pub(super) fn notify_eligible_at(
        &self,
        account_id: AccountId,
        message_id: i64,
    ) -> Result<Option<DateTime<Utc>>> {
        let conn = self.lock()?;
        // NO `sensitivity` PREDICATE. See the trait doc: a sealed row carries a
        // stamp like any other.
        let at = conn
            .query_row(
                "SELECT notify_eligible_at FROM triage
                 WHERE message_id = ?1 AND account_id = ?2",
                params![message_id, account_id],
                |r| dt_opt(r, 0),
            )
            .optional()?;
        Ok(at.flatten())
    }

    pub(super) fn stage1_bump_usage(
        &self,
        account_id: AccountId,
        day: &str,
        tokens: UsageTokens,
    ) -> Result<()> {
        let conn = self.lock()?;
        bump_usage_category(&conn, account_id, day, "stage1", tokens)
    }

    pub(super) fn stage1_usage_since(
        &self,
        account_id: AccountId,
        since_day: &str,
    ) -> Result<Stage2Usage> {
        let conn = self.lock()?;
        usage_since_category(&conn, account_id, since_day, "stage1")
    }

    pub(super) fn category_usage_since(
        &self,
        account_id: AccountId,
        since_day: &str,
        category: &str,
    ) -> Result<Stage2Usage> {
        let conn = self.lock()?;
        usage_since_category(&conn, account_id, since_day, category)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn extract_bump_usage(
        &self,
        account_id: AccountId,
        day: &str,
        category: &str,
        tokens: UsageTokens,
    ) -> Result<()> {
        let conn = self.lock()?;
        bump_usage_category(&conn, account_id, day, category, tokens)
    }

    pub(super) fn list_usage_stage1(
        &self,
        account_id: AccountId,
        days: u32,
    ) -> Result<Vec<Stage2UsageDay>> {
        let conn = self.lock()?;
        list_usage_category(&conn, account_id, days, "stage1")
    }

    /// Distinct categories first, then each one's history through the same
    /// per-category helper the named readers use — rather than one windowed
    /// query, because `days` is a per-category ROW limit and reproducing that
    /// across categories in SQL buys nothing at this cardinality (a handful of
    /// stages and extractors).
    pub(super) fn list_usage_by_category(
        &self,
        account_id: AccountId,
        days: u32,
    ) -> Result<Vec<(String, Vec<Stage2UsageDay>)>> {
        let conn = self.lock()?;
        let categories: Vec<String> = conn
            .prepare(
                "SELECT DISTINCT category FROM stage2_usage
                 WHERE account_id = ?1
                 ORDER BY category",
            )?
            .query_map(params![account_id], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        categories
            .into_iter()
            .map(|c| {
                let rows = list_usage_category(&conn, account_id, days, &c)?;
                Ok((c, rows))
            })
            .collect()
    }

    pub(super) fn stage2_budget_used(
        &self,
        account_id: AccountId,
        thread_id: &str,
        day: &str,
    ) -> Result<u32> {
        let conn = self.lock()?;
        let n: i64 = conn
            .query_row(
                "SELECT model_calls FROM wake_budget
                 WHERE account_id=?1 AND thread_id=?2 AND day=?3",
                params![account_id, thread_id, day],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(n.max(0) as u32)
    }

    pub(super) fn stage2_increment_budget(
        &self,
        account_id: AccountId,
        thread_id: &str,
        day: &str,
    ) -> Result<u32> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO wake_budget(account_id, thread_id, day, model_calls)
             VALUES(?1, ?2, ?3, 1)
             ON CONFLICT(account_id, thread_id, day)
             DO UPDATE SET model_calls = model_calls + 1",
            params![account_id, thread_id, day],
        )?;
        let n: i64 = conn.query_row(
            "SELECT model_calls FROM wake_budget
             WHERE account_id=?1 AND thread_id=?2 AND day=?3",
            params![account_id, thread_id, day],
            |r| r.get(0),
        )?;
        Ok(n.max(0) as u32)
    }

    /// Give back one charge taken by [`Self::stage2_increment_budget`].
    ///
    /// FLOORED AT ZERO, and the floor is not paranoia: the counter is keyed by
    /// UTC day, so a call charged at 23:59:59 and refunded at 00:00:01 lands on
    /// the NEXT day's row, which has nothing in it. `MAX(0, ...)` makes that
    /// refund a no-op instead of a negative balance that would silently hand
    /// the new day a free call.
    pub(super) fn stage2_refund_budget(
        &self,
        account_id: AccountId,
        thread_id: &str,
        day: &str,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE wake_budget SET model_calls = MAX(0, model_calls - 1)
             WHERE account_id=?1 AND thread_id=?2 AND day=?3",
            params![account_id, thread_id, day],
        )?;
        Ok(())
    }

    pub(super) fn stage2_bump_usage(
        &self,
        account_id: AccountId,
        day: &str,
        tokens: UsageTokens,
    ) -> Result<()> {
        let conn = self.lock()?;
        bump_usage_category(&conn, account_id, day, "stage2", tokens)
    }

    pub(super) fn stage2_usage_today(
        &self,
        account_id: AccountId,
        day: &str,
    ) -> Result<Stage2Usage> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT calls, input_tokens, output_tokens, cache_creation_tokens,
                        cache_read_tokens
                 FROM stage2_usage
                 WHERE account_id = ?1 AND day = ?2 AND category = 'stage2'",
                params![account_id, day],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?;
        Ok(row
            .map(|(calls, in_tok, out_tok, cache_w, cache_r)| Stage2Usage {
                calls: calls.max(0) as u64,
                input_tokens: in_tok.max(0) as u64,
                output_tokens: out_tok.max(0) as u64,
                cache_creation_tokens: cache_w.max(0) as u64,
                cache_read_tokens: cache_r.max(0) as u64,
            })
            .unwrap_or_default())
    }

    pub(super) fn list_usage(
        &self,
        account_id: AccountId,
        days: u32,
    ) -> Result<Vec<Stage2UsageDay>> {
        let conn = self.lock()?;
        list_usage_category(&conn, account_id, days, "stage2")
    }

    pub(super) fn stage2_usage_since(
        &self,
        account_id: AccountId,
        since_day: &str,
    ) -> Result<Stage2Usage> {
        let conn = self.lock()?;
        usage_since_category(&conn, account_id, since_day, "stage2")
    }

    pub(super) fn get_app_setting(
        &self,
        account_id: AccountId,
        key: &str,
    ) -> Result<Option<String>> {
        let conn = self.lock()?;
        let v: Option<String> = conn
            .query_row(
                "SELECT value FROM app_settings WHERE account_id = ?1 AND key = ?2",
                params![account_id, key],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v)
    }

    pub(super) fn set_app_setting(
        &self,
        account_id: AccountId,
        key: &str,
        value: &str,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO app_settings(account_id, key, value)
             VALUES(?1, ?2, ?3)
             ON CONFLICT(account_id, key) DO UPDATE SET value = excluded.value",
            params![account_id, key, value],
        )?;
        Ok(())
    }

    pub(super) fn stage2_cap_overrides(&self, account_id: AccountId) -> Result<Stage2CapOverrides> {
        let conn = self.lock()?;
        // One SELECT pulls all four cap rows (only those that exist come back).
        let mut stmt = conn.prepare(
            "SELECT key, value FROM app_settings
             WHERE account_id = ?1 AND key IN (?2, ?3, ?4, ?5)",
        )?;
        // A stored value only counts if it parses as an integer in the valid
        // range; anything else is treated as absent (fall back to config/default).
        let valid = |s: String| -> Option<u32> {
            s.trim().parse::<u32>().ok().filter(|n| {
                (crate::config::STAGE2_CAP_MIN..=crate::config::STAGE2_CAP_MAX).contains(n)
            })
        };
        let mut out = Stage2CapOverrides::default();
        let rows = stmt.query_map(
            params![
                account_id,
                crate::config::APP_SETTING_THREAD_DAILY_CAP,
                crate::config::APP_SETTING_SENDER_DAILY_CAP,
                crate::config::APP_SETTING_GLOBAL_DAILY_CAP,
                crate::config::APP_SETTING_STAGE1_GLOBAL_DAILY_CAP,
            ],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )?;
        for row in rows {
            let (key, value) = row?;
            match key.as_str() {
                k if k == crate::config::APP_SETTING_THREAD_DAILY_CAP => {
                    out.thread_daily_cap = valid(value)
                }
                k if k == crate::config::APP_SETTING_SENDER_DAILY_CAP => {
                    out.sender_daily_cap = valid(value)
                }
                k if k == crate::config::APP_SETTING_GLOBAL_DAILY_CAP => {
                    out.global_daily_cap = valid(value)
                }
                k if k == crate::config::APP_SETTING_STAGE1_GLOBAL_DAILY_CAP => {
                    out.stage1_global_daily_cap = valid(value)
                }
                _ => {}
            }
        }
        Ok(out)
    }

    pub(super) fn count_inbound_since(
        &self,
        account_id: AccountId,
        since: DateTime<Utc>,
    ) -> Result<u64> {
        let conn = self.lock()?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages
             WHERE account_id = ?1 AND is_sent = 0 AND is_spam = 0 AND is_archive = 0
               AND received_at >= ?2",
            params![account_id, since.to_rfc3339()],
            |r| r.get(0),
        )?;
        Ok(n.max(0) as u64)
    }
}
