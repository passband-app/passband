//! The `sender_research` table (docs/SENDER-RESEARCH.md): one row per sender,
//! the newest research on them, and the two reads research may make about a
//! sender. Every decision about whether a row STANDS is
//! `SenderResearchRow::blocks`, called here inside the lock that writes, so a
//! check and its write cannot be split by another request.
//!
//! HUMAN DOOR ONLY: nothing on `/mcp` reads this table, and triage never does.

use super::*;
use crate::triage::research::{
    ResearchResult, ResearchStatus, SenderResearchRow, SubjectFields, Trigger,
};

const ROW_SQL: &str = "SELECT sender_addr, status, trigger, result_json, failure, web_searches,
            requested_at, researched_at
       FROM sender_research WHERE account_id = ?1 AND sender_addr = ?2";

fn read_row(
    conn: &Connection,
    account_id: AccountId,
    addr: &str,
) -> Result<Option<SenderResearchRow>> {
    let raw = conn
        .query_row(ROW_SQL, params![account_id, addr], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        })
        .optional()?;
    let Some((sender_addr, status, trigger, result, failure, searches, requested, researched)) =
        raw
    else {
        return Ok(None);
    };
    let status = ResearchStatus::parse(&status)
        .ok_or_else(|| CoreError::InvalidInput(format!("bad research status {status:?}")))?;
    let trigger = Trigger::parse(&trigger)
        .ok_or_else(|| CoreError::InvalidInput(format!("bad research trigger {trigger:?}")))?;
    Ok(Some(SenderResearchRow {
        sender_addr,
        status,
        trigger,
        // A stored result that no longer decodes (an older shape) reads as
        // none, never as an error that costs the card.
        result: result.and_then(|json| serde_json::from_str::<ResearchResult>(&json).ok()),
        failure,
        web_searches: searches.max(0) as u64,
        requested_at: parse_dt(&requested)?,
        researched_at: researched.as_deref().map(parse_dt).transpose()?,
    }))
}

/// Write `addr` as a fresh `status` row for `trigger`, replacing whatever was
/// there (the caller has decided nothing stands).
fn begin_row(
    conn: &Connection,
    account_id: AccountId,
    addr: &str,
    status: ResearchStatus,
    trigger: Trigger,
    now: DateTime<Utc>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO sender_research
             (account_id, sender_addr, status, trigger, result_json, failure, web_searches,
              requested_at, researched_at)
         VALUES (?1, ?2, ?3, ?4, NULL, NULL, 0, ?5, NULL)
         ON CONFLICT(account_id, sender_addr) DO UPDATE SET
             status = excluded.status, trigger = excluded.trigger, result_json = NULL,
             failure = NULL, web_searches = 0, requested_at = excluded.requested_at,
             researched_at = NULL",
        params![
            account_id,
            addr,
            status.as_str(),
            trigger.as_str(),
            now.to_rfc3339()
        ],
    )?;
    Ok(())
}

impl SqliteStore {
    pub(super) fn sender_research_row(
        &self,
        account_id: AccountId,
        addr: &str,
    ) -> Result<Option<SenderResearchRow>> {
        let conn = self.lock()?;
        read_row(&conn, account_id, &addr.trim().to_lowercase())
    }

    pub(super) fn queue_sender_research_row(
        &self,
        account_id: AccountId,
        addr: &str,
        now: DateTime<Utc>,
        cache_days: i64,
    ) -> Result<bool> {
        let addr = addr.trim().to_lowercase();
        let conn = self.lock()?;
        if read_row(&conn, account_id, &addr)?
            .is_some_and(|row| row.blocks(Trigger::Auto, now, cache_days))
        {
            return Ok(false);
        }
        begin_row(
            &conn,
            account_id,
            &addr,
            ResearchStatus::Queued,
            Trigger::Auto,
            now,
        )?;
        Ok(true)
    }

    pub(super) fn start_sender_research_row(
        &self,
        account_id: AccountId,
        addr: &str,
        now: DateTime<Utc>,
        cache_days: i64,
    ) -> Result<Option<SenderResearchRow>> {
        let addr = addr.trim().to_lowercase();
        let conn = self.lock()?;
        if let Some(row) = read_row(&conn, account_id, &addr)?
            && row.blocks(Trigger::OnDemand, now, cache_days)
        {
            return Ok(Some(row));
        }
        begin_row(
            &conn,
            account_id,
            &addr,
            ResearchStatus::Running,
            Trigger::OnDemand,
            now,
        )?;
        Ok(None)
    }

    pub(super) fn claim_sender_research_row(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
        max_age: chrono::Duration,
    ) -> Result<Option<String>> {
        let conn = self.lock()?;
        conn.execute(
            "DELETE FROM sender_research
              WHERE account_id = ?1 AND status = 'queued' AND requested_at < ?2",
            params![account_id, (now - max_age).to_rfc3339()],
        )?;
        let addr: Option<String> = conn
            .query_row(
                "SELECT sender_addr FROM sender_research
                  WHERE account_id = ?1 AND status = 'queued'
                  ORDER BY requested_at, sender_addr LIMIT 1",
                params![account_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(addr) = addr else {
            return Ok(None);
        };
        // `requested_at` restarts with the run, so a run that never finishes
        // goes stale from when it began, not from when it was queued.
        conn.execute(
            "UPDATE sender_research SET status = 'running', requested_at = ?3
              WHERE account_id = ?1 AND sender_addr = ?2",
            params![account_id, addr, now.to_rfc3339()],
        )?;
        Ok(Some(addr))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn finish_sender_research_row(
        &self,
        account_id: AccountId,
        addr: &str,
        status: ResearchStatus,
        result: Option<&ResearchResult>,
        failure: Option<&str>,
        web_searches: u64,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let json = result
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| CoreError::InvalidInput(format!("research result: {e}")))?;
        let conn = self.lock()?;
        conn.execute(
            "UPDATE sender_research
                SET status = ?3, result_json = ?4, failure = ?5, web_searches = ?6,
                    researched_at = ?7
              WHERE account_id = ?1 AND sender_addr = ?2",
            params![
                account_id,
                addr.trim().to_lowercase(),
                status.as_str(),
                json,
                failure,
                i64::try_from(web_searches).unwrap_or(i64::MAX),
                now.to_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// The display name from the sender's newest message that carried one,
    /// and the claims from the newest decision on their mail that assessed
    /// outreach (`idx_decisions_outreach`'s expression, so the planner can
    /// use it). Bodies and subjects are never selected.
    pub(super) fn research_subject_row(
        &self,
        account_id: AccountId,
        addr: &str,
    ) -> Result<SubjectFields> {
        let addr = addr.trim().to_lowercase();
        let conn = self.lock()?;
        let is_self: bool = conn
            .query_row(
                "SELECT lower(trim(email)) = ?2 FROM accounts WHERE id = ?1",
                params![account_id, addr],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false);
        let display_name: Option<String> = conn
            .query_row(
                "SELECT from_name FROM messages
                  WHERE account_id = ?1 AND from_addr = ?2 COLLATE NOCASE
                    AND from_name IS NOT NULL AND trim(from_name) <> ''
                  ORDER BY received_at DESC LIMIT 1",
                params![account_id, addr],
                |r| r.get(0),
            )
            .optional()?;
        let decision: Option<String> = conn
            .query_row(
                "SELECT d.decision_json
                   FROM agent_message_decisions d
                   JOIN messages m ON m.account_id = d.account_id AND m.id = d.message_id
                  WHERE d.account_id = ?1 AND m.from_addr = ?2 COLLATE NOCASE
                    AND CASE WHEN json_valid(d.decision_json)
                             THEN json_type(d.decision_json,'$.outreach') END = 'object'
                  ORDER BY m.received_at DESC LIMIT 1",
                params![account_id, addr],
                |r| r.get(0),
            )
            .optional()?;
        let outreach = decision
            .and_then(|json| {
                serde_json::from_str::<crate::triage::decision::MessageDecision>(&json).ok()
            })
            .and_then(|d| d.outreach);
        Ok(SubjectFields {
            is_self,
            display_name,
            claimed_role: outreach.as_ref().and_then(|o| o.claimed_role.clone()),
            claimed_organization: outreach.and_then(|o| o.claimed_organization),
        })
    }
}
