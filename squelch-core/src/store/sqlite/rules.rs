//! Sender rules and the unsubscribe ledger.

use super::*;

/// A sender's recent non-sealed inbound mail with what became of it.
///
/// `INDEXED BY` ON PURPOSE. Without `sqlite_stat1` (which a daemon never
/// writes) the planner reads `ORDER BY received_at DESC LIMIT` as a reason to
/// walk `idx_messages_received` newest-first across the WHOLE account, under the
/// store mutex, until it happens on enough of this sender's rows. The seek on
/// the NOCASE sender index plus a sort of one sender's mail is the cheap plan,
/// and pinning it means a missing index is a loud prepare error, not a silent
/// walk. `tests::attention::rule_digest_seeks_the_nocase_sender_index` pins it.
pub(crate) const SENDER_RULE_DIGEST_SQL: &str =
    "SELECT m.subject, t.tier, t.status, t.opened_at IS NOT NULL
     FROM messages m INDEXED BY idx_messages_from_nocase
     JOIN triage t ON t.message_id = m.id
     WHERE m.account_id = ?1
       AND m.from_addr = ?2 COLLATE NOCASE
       AND m.is_sent = 0 AND m.is_spam = 0
       AND t.sensitivity = 'normal'
     ORDER BY m.received_at DESC
     LIMIT ?3";

/// Validate a sender-rule write, on the path of every door. A FILTERED rule with
/// an empty `want_text` is a contradiction (there is no standing instruction to
/// filter by): Stage-2 would get no instruction to evaluate, so the rule would
/// silently degrade while still reading as a rule in the UI. The message is
/// client-visible, so it stays polarity-blind: `want_text` may name what the
/// owner wants OR what they do not care about.
fn validate_sender_rule(want_text: &str, disposition: Disposition) -> Result<()> {
    if disposition == Disposition::Filtered && want_text.trim().is_empty() {
        return Err(CoreError::InvalidInput(
            "a filtered rule needs a want_text saying what you do, or do not, want from this sender"
                .into(),
        ));
    }
    Ok(())
}

/// Map a UNIQUE-constraint failure into a client-legible [`CoreError::InvalidInput`].
/// `sender_rules` carries `UNIQUE(account_id, match_pattern)`, so editing one
/// rule onto another's pattern is a user mistake, not a server fault: without
/// this it surfaces as a raw sqlite error that squelch-api collapses to a 500.
fn map_pattern_conflict(e: rusqlite::Error) -> CoreError {
    match &e {
        rusqlite::Error::SqliteFailure(err, _)
            if err.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            CoreError::InvalidInput("a rule for that pattern already exists".into())
        }
        _ => CoreError::from(e),
    }
}

// A sender-rule change never re-triages stored mail. Rules apply to mail that
// arrives after they are written: every re-triage is a full model
// investigation, and a catch-all `*` rule (a content filter, which is a real
// thing to want) used to queue the whole 30-day window, ~1,000 frontier calls
// for one click. A decision in flight when the rule lands still goes stale on
// its preference fingerprint and re-runs under the new rule; that is bounded
// by the worker's concurrency, not by the mailbox.

impl SqliteStore {
    pub(super) fn set_sender_rule(
        &self,
        account_id: AccountId,
        match_pattern: &str,
        want_text: &str,
        disposition: Disposition,
    ) -> Result<i64> {
        validate_sender_rule(want_text, disposition)?;
        let mut connection = self.lock()?;
        let conn = connection.transaction()?;
        conn.execute(
            "INSERT INTO sender_rules(account_id, match_pattern, want_text, disposition, updated_at)
             VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(account_id, match_pattern) DO UPDATE SET
                 want_text=excluded.want_text, disposition=excluded.disposition,
                 updated_at=excluded.updated_at",
            params![
                account_id,
                match_pattern,
                want_text,
                disposition.as_str(),
                Utc::now().to_rfc3339(),
            ],
        )?;
        let id: i64 = conn.query_row(
            "SELECT id FROM sender_rules WHERE account_id=?1 AND match_pattern=?2",
            params![account_id, match_pattern],
            |r| r.get(0),
        )?;
        conn.commit()?;
        Ok(id)
    }

    pub(super) fn set_sender_rule_audited(
        &self,
        account_id: AccountId,
        match_pattern: &str,
        want_text: &str,
        disposition: Disposition,
        audit: &NewAuditEntry,
    ) -> Result<i64> {
        validate_sender_rule(want_text, disposition)?;
        // FAIL-CLOSED: the rule write and its audit row share ONE transaction, so
        // a failed audit INSERT rolls the rule back and an agent-door write can
        // never land untraced.
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO sender_rules(account_id, match_pattern, want_text, disposition, updated_at)
             VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(account_id, match_pattern) DO UPDATE SET
                 want_text=excluded.want_text, disposition=excluded.disposition,
                 updated_at=excluded.updated_at",
            params![
                account_id,
                match_pattern,
                want_text,
                disposition.as_str(),
                Utc::now().to_rfc3339(),
            ],
        )?;
        let id: i64 = tx.query_row(
            "SELECT id FROM sender_rules WHERE account_id=?1 AND match_pattern=?2",
            params![account_id, match_pattern],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO audit_log(account_id, ts, actor, action, target, detail)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                account_id,
                Utc::now().to_rfc3339(),
                audit.actor,
                audit.action,
                audit.target,
                audit.detail,
            ],
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub(super) fn update_sender_rule(
        &self,
        account_id: AccountId,
        id: i64,
        match_pattern: &str,
        want_text: &str,
        disposition: Disposition,
    ) -> Result<bool> {
        validate_sender_rule(want_text, disposition)?;
        let conn = self.lock()?;
        // Retargeting this rule onto a pattern another rule already owns trips
        // UNIQUE(account_id, match_pattern); map it to InvalidInput so the door
        // returns a 4xx the user can act on instead of a 500.
        let n = conn
            .execute(
                "UPDATE sender_rules SET
                 match_pattern = ?3, want_text = ?4, disposition = ?5, updated_at = ?6
             WHERE account_id = ?1 AND id = ?2",
                params![
                    account_id,
                    id,
                    match_pattern,
                    want_text,
                    disposition.as_str(),
                    Utc::now().to_rfc3339(),
                ],
            )
            .map_err(map_pattern_conflict)?;
        Ok(n > 0)
    }

    pub(super) fn list_sender_rules(&self, account_id: AccountId) -> Result<Vec<SenderRule>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, account_id, match_pattern, want_text, disposition, updated_at
             FROM sender_rules WHERE account_id=?1 ORDER BY updated_at DESC",
        )?;
        let out = stmt
            .query_map(params![account_id], |r| {
                Ok(SenderRule {
                    id: r.get(0)?,
                    account_id: r.get(1)?,
                    match_pattern: r.get(2)?,
                    want_text: r.get(3)?,
                    disposition: Disposition::parse(&r.get::<_, String>(4)?)
                        .unwrap_or(Disposition::Surface),
                    updated_at: dt(r, 5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(out)
    }

    pub(super) fn sender_rule_digest(
        &self,
        account_id: AccountId,
        from_addr: &str,
        limit: usize,
    ) -> Result<Vec<SenderDigestRow>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(SENDER_RULE_DIGEST_SQL)?;
        let out = stmt
            .query_map(params![account_id, from_addr, limit as i64], |r| {
                Ok(SenderDigestRow {
                    subject: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    tier: r.get(1)?,
                    status: r.get(2)?,
                    opened: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(out)
    }

    pub(super) fn delete_sender_rule(&self, account_id: AccountId, id: i64) -> Result<bool> {
        let conn = self.lock()?;
        let n = conn.execute(
            "DELETE FROM sender_rules WHERE account_id=?1 AND id=?2",
            params![account_id, id],
        )?;
        Ok(n > 0)
    }

    pub(super) fn message_unsub_fields(
        &self,
        account_id: AccountId,
        message_id: i64,
    ) -> Result<Option<MessageUnsub>> {
        let conn = self.lock()?;
        // This is a human action. Agent restrictions do not hide unsubscribe metadata.
        let row = conn
            .query_row(
                "SELECT m.from_addr, m.list_unsubscribe, m.list_unsub_one_click, m.body_html,
                        m.auth_pass, m.is_spam
                 FROM messages m
                 WHERE m.account_id = ?1 AND m.id = ?2",
                params![account_id, message_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<i64>>(4)?,
                        r.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()?;
        Ok(row.map(
            |(from_addr, list_unsubscribe, one_click, body_html, auth_pass, is_spam)| {
                MessageUnsub {
                    from_addr,
                    list_unsubscribe,
                    list_unsub_one_click: one_click != 0,
                    body_html,
                    auth_pass: auth_pass.map(|v| v != 0),
                    is_spam: is_spam != 0,
                }
            },
        ))
    }

    pub(super) fn upsert_unsubscribe(
        &self,
        account_id: AccountId,
        sender: &str,
        method: &str,
        source_message_id: Option<i64>,
        requested_at: DateTime<Utc>,
    ) -> Result<()> {
        let conn = self.lock()?;
        // A fresh request RESETS the ledger — the user re-asked, so the 72h grace
        // clock restarts from this `requested_at`.
        conn.execute(
            "INSERT INTO unsubscribes(account_id, sender_addr, requested_at, method,
                 source_message_id, violation_count, last_violation_at, resolution)
             VALUES(?1,?2,?3,?4,?5,0,NULL,NULL)
             ON CONFLICT(account_id, sender_addr) DO UPDATE SET
                 requested_at=excluded.requested_at, method=excluded.method,
                 source_message_id=excluded.source_message_id,
                 violation_count=0, last_violation_at=NULL, resolution=NULL,
                 status=NULL, status_url=NULL, status_reason=NULL, status_at=NULL",
            params![
                account_id,
                sender,
                requested_at.to_rfc3339(),
                method,
                source_message_id,
            ],
        )?;
        Ok(())
    }

    pub(super) fn begin_unsubscribe_job(
        &self,
        account_id: AccountId,
        sender: &str,
        source_message_id: Option<i64>,
        url: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let conn = self.lock()?;
        let now = now.to_rfc3339();
        conn.execute(
            "INSERT INTO unsubscribes(account_id, sender_addr, requested_at, method,
                 source_message_id, violation_count, last_violation_at, resolution,
                 status, status_url, status_reason, status_at)
             VALUES(?1,?2,?3,'auto',?4,0,NULL,NULL,'running',?5,NULL,?3)
             ON CONFLICT(account_id, sender_addr) DO UPDATE SET
                 requested_at=excluded.requested_at, method='auto',
                 source_message_id=excluded.source_message_id,
                 violation_count=0, last_violation_at=NULL, resolution=NULL,
                 status='running', status_url=excluded.status_url,
                 status_reason=NULL, status_at=excluded.status_at",
            params![account_id, sender, now, source_message_id, url],
        )?;
        Ok(())
    }

    pub(super) fn finish_unsubscribe_job(
        &self,
        account_id: AccountId,
        sender: &str,
        status: &str,
        method: &str,
        reason: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let conn = self.lock()?;
        let now = now.to_rfc3339();
        // `done` restarts the grace clock from the moment the sender was asked;
        // `needs_you` leaves it, since nothing went out and the detector is not
        // armed for that status anyway. The link (it carries the recipient's
        // token) is kept only while the user still needs it.
        conn.execute(
            "UPDATE unsubscribes SET
                 status = ?3, method = ?4, status_reason = ?5, status_at = ?6,
                 requested_at = CASE WHEN ?3 = 'done' THEN ?6 ELSE requested_at END,
                 status_url = CASE WHEN ?3 = 'needs_you' THEN status_url ELSE NULL END
             WHERE account_id = ?1 AND sender_addr = ?2 AND status = 'running'",
            params![account_id, sender, status, method, reason, now],
        )?;
        Ok(())
    }

    pub(super) fn mark_unsubscribe_opened(
        &self,
        account_id: AccountId,
        sender: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let conn = self.lock()?;
        let now = now.to_rfc3339();
        let n = conn.execute(
            "UPDATE unsubscribes SET status = 'opened', status_at = ?3, requested_at = ?3,
                 status_url = NULL
             WHERE account_id = ?1 AND sender_addr = ?2 AND status = 'needs_you'",
            params![account_id, sender, now],
        )?;
        Ok(n > 0)
    }

    pub(super) fn reap_unsubscribe_jobs(
        &self,
        account_id: AccountId,
        live: &[String],
        stale_before: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT sender_addr, status_at FROM unsubscribes
             WHERE account_id = ?1 AND status = 'running'",
        )?;
        let running = stmt
            .query_map(params![account_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        // Ages compared as timestamps in Rust, not as strings in SQL. A row with
        // no readable `status_at` cannot prove it is fresh, so it is stale.
        let stale = |at: &Option<String>| {
            at.as_deref()
                .and_then(|a| parse_dt(a).ok())
                .is_none_or(|t| t < stale_before)
        };
        let now = now.to_rfc3339();
        let mut reaped = 0;
        for (sender, _) in running
            .iter()
            .filter(|(s, at)| !live.contains(s) && stale(at))
        {
            reaped += conn.execute(
                "UPDATE unsubscribes SET status = 'needs_you', status_reason = 'unclear',
                     status_at = ?3
                 WHERE account_id = ?1 AND sender_addr = ?2 AND status = 'running'",
                params![account_id, sender, now],
            )?;
        }
        Ok(reaped)
    }

    pub(super) fn list_unsubscribes(
        &self,
        account_id: AccountId,
    ) -> Result<Vec<UnsubscribeRecord>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT sender_addr, requested_at, method, violation_count,
                    last_violation_at, resolution, status, status_url,
                    status_reason, status_at
             FROM unsubscribes
             WHERE account_id = ?1
             ORDER BY requested_at DESC",
        )?;
        let out = stmt
            .query_map(params![account_id], |r| {
                Ok(UnsubscribeRecord {
                    sender: r.get(0)?,
                    requested_at: dt(r, 1)?,
                    method: r.get(2)?,
                    violation_count: r.get(3)?,
                    last_violation_at: dt_opt(r, 4)?,
                    resolution: r.get(5)?,
                    status: r.get(6)?,
                    status_url: r.get(7)?,
                    status_reason: r.get(8)?,
                    status_at: dt_opt(r, 9)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(out)
    }

    pub(super) fn set_unsubscribe_resolution(
        &self,
        account_id: AccountId,
        sender: &str,
        resolution: &str,
    ) -> Result<bool> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE unsubscribes SET resolution = ?3
             WHERE account_id = ?1 AND sender_addr = ?2",
            params![account_id, sender, resolution],
        )?;
        Ok(n > 0)
    }
}
