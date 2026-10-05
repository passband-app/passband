//! The store half of smart done (`POST /client/v2/reading/smart_done`): gather
//! what the judgement in [`crate::triage::smart_done`] reads, write the batch
//! with its provenance, and undo it exactly.
//!
//! HUMAN-DOOR ONLY. It resolves mail on the owner's say-so, which no agent
//! door may do.
//!
//! THREE STEPS, AND THE LOCK IS HELD FOR NONE OF THE MODEL CALL:
//!
//! 1. [`SqliteStore::smart_done_scan`], one lock: which open Reading rows
//!    have nothing stored that decides them, so the model can be asked.
//! 2. The model pass, in the caller, with no lock held.
//! 3. [`SqliteStore::smart_done_apply`], one lock and one transaction: SCAN
//!    AGAIN and write. Nothing from step 1 is trusted here. Seconds have
//!    passed and the owner has been using the app: a row may have been opened,
//!    reminded, resolved, or its sender kept. So every row is judged afresh
//!    from the store as it is now, and a model verdict is honoured only for a
//!    row that would still be asked about.
//!
//! WHICH ROWS are the Reading feed's own (`agent_triage::reading_items`) and
//! which senders are scored is the purge list's own (`reading_senders`'s
//! measurer and its cap), so smart done can never sweep a row the feed does
//! not show or a sender for a score the list does not show.
//!
//! MESSAGE-SCOPED, unlike a human done, which resolves the whole thread. A
//! sweep that took a sibling it never judged could not say why, and could not
//! give it back. What it takes is recorded twice over: `triage.resolved_by`
//! on each row, and the batch's `detail` with the status each row had.

use super::agent_triage::reading_items;
use super::reading_senders::{SenderMeasurer, busiest_senders, sender_key};
use super::*;
use crate::store::agent_triage::AgentListQuery;
use crate::triage::smart_done::{
    ClearReason, ModelRow, RowFacts, SMART_DONE_MODEL_ROWS, model_question, stored_reason,
};
use std::collections::HashMap;

/// `triage.resolved_by` for the rows of one batch.
fn batch_tag(batch_id: i64) -> String {
    format!("smart_done:{batch_id}")
}

/// One open Reading row as the judgement left it.
struct Scanned {
    message_id: i64,
    sender: String,
    /// `triage.status` before anything is written, for the undo.
    status: String,
    received_at: DateTime<Utc>,
    /// What stored facts say, if they say to clear it.
    reason: Option<ClearReason>,
    /// The summary the model may be asked about, when nothing stored decides.
    question: Option<String>,
}

/// What one press cleared.
#[derive(Debug, Clone, Default)]
pub struct SmartDoneOutcome {
    /// `None` when nothing was cleared: no batch is recorded for an empty one.
    pub batch_id: Option<i64>,
    /// Each message taken and why, newest first.
    pub cleared: Vec<(i64, ClearReason)>,
    /// How many distinct senders that mail came from.
    pub senders: usize,
}

/// Judge every open Reading row from the store as it is right now.
fn scan(
    conn: &Connection,
    account_id: AccountId,
    now: DateTime<Utc>,
    inventory: &AgentListQuery,
    feed_limit: usize,
) -> Result<Vec<Scanned>> {
    let items = reading_items(conn, account_id, feed_limit, inventory, now)?;
    let mut measurer = SenderMeasurer::new(conn, account_id, now)?;

    // SCORES for the senders the purge list would show, and no others: a
    // sender past its cap was never shown a score to be swept for.
    let (_, groups) = busiest_senders(&items);
    let mut scores: HashMap<String, Option<u8>> = HashMap::new();
    let mut protected: HashMap<String, bool> = HashMap::new();
    for (sender, _, _) in &groups {
        let measured = measurer.measure(sender)?;
        protected.insert(
            sender.clone(),
            measured.signals.known_contact
                || measured.signals.surface_rule
                || measured.signals.kept_recently,
        );
        scores.insert(sender.clone(), measured.score);
    }

    // The row's own state, by primary key. A message with no triage row has
    // no status to write and is skipped.
    let mut state = conn.prepare(
        "SELECT sensitivity != 'normal', status, opened_at IS NOT NULL,
                remind_at IS NOT NULL OR reminded_at IS NOT NULL
         FROM triage WHERE account_id = ?1 AND message_id = ?2",
    )?;
    let mut out = Vec::with_capacity(items.len());
    for item in &items {
        let sender = sender_key(&item.from_addr);
        // WHO THE OWNER SAID TO LEAVE ALONE is asked of every sender, capped
        // or not: it is three small probes, and it is the guard.
        let sender_protected = match protected.get(&sender) {
            Some(known) => *known,
            None => {
                let standing = measurer.standing(&sender)?.protected();
                protected.insert(sender.clone(), standing);
                standing
            }
        };
        let Some((sealed, status, opened, reminder)) = state
            .query_row(params![account_id, item.message_id], |r| {
                Ok((
                    r.get::<_, bool>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, bool>(2)?,
                    r.get::<_, bool>(3)?,
                ))
            })
            .optional()?
        else {
            continue;
        };
        // A date that will not parse cannot prove the mail is a day old, so
        // the row is left alone.
        let Some(received_at) = DateTime::parse_from_rfc3339(&item.received_at)
            .ok()
            .map(|at| at.with_timezone(&Utc))
        else {
            continue;
        };
        let facts = RowFacts {
            decision: &item.decision,
            received_at,
            opened,
            sealed,
            reminder,
            sender_protected,
            sender_score: scores.get(&sender).copied().flatten(),
        };
        out.push(Scanned {
            message_id: item.message_id,
            status,
            received_at,
            reason: stored_reason(&facts, now),
            question: model_question(&facts, now),
            sender,
        });
    }
    Ok(out)
}

impl SqliteStore {
    /// Step 1: the rows worth asking the model about, newest first, at most
    /// [`SMART_DONE_MODEL_ROWS`]. Read-only.
    pub fn smart_done_scan(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
        inventory: &AgentListQuery,
        feed_limit: usize,
    ) -> Result<Vec<ModelRow>> {
        let conn = self.lock()?;
        Ok(scan(&conn, account_id, now, inventory, feed_limit)?
            .into_iter()
            .filter_map(|row| {
                Some(ModelRow {
                    message_id: row.message_id,
                    summary: row.question?,
                    received_at: row.received_at,
                })
            })
            .take(SMART_DONE_MODEL_ROWS)
            .collect())
    }

    /// Step 3: judge again and write, in one transaction. `verdicts` is what
    /// the model said about the rows step 1 offered it; a verdict for a row
    /// that is no longer askable (opened into a reminder, resolved, protected,
    /// or decided by a stored fact in the meantime) is dropped.
    pub fn smart_done_apply(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
        inventory: &AgentListQuery,
        feed_limit: usize,
        verdicts: &HashMap<i64, ClearReason>,
    ) -> Result<SmartDoneOutcome> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let picks: Vec<(Scanned, ClearReason)> = scan(&tx, account_id, now, inventory, feed_limit)?
            .into_iter()
            .filter_map(|row| {
                // A stored fact first; the model only for a row that is still
                // one the model would be asked about.
                let reason = row.reason.or_else(|| {
                    row.question
                        .as_ref()
                        .and_then(|_| verdicts.get(&row.message_id).copied())
                })?;
                Some((row, reason))
            })
            .collect();
        if picks.is_empty() {
            return Ok(SmartDoneOutcome::default());
        }

        let stamp = now.to_rfc3339();
        tx.execute(
            "INSERT INTO smart_done_batches(account_id, created_at, cleared, detail)
             VALUES(?1, ?2, 0, '{}')",
            params![account_id, stamp],
        )?;
        let batch_id = tx.last_insert_rowid();
        let tag = batch_tag(batch_id);

        let mut cleared = Vec::with_capacity(picks.len());
        let mut rows = Vec::with_capacity(picks.len());
        let mut senders = std::collections::HashSet::new();
        {
            // ONE ROW PER STATEMENT, by primary key: this message and no
            // sibling. The predicates repeat what the scan just established,
            // so that this statement alone can never resolve a row that is
            // already done or one that is sealed.
            let mut done = tx.prepare(
                "UPDATE triage
                 SET status = 'done', resolved_at = ?1, resolved_by = ?2
                 WHERE account_id = ?3 AND message_id = ?4
                   AND status != 'done' AND sensitivity = 'normal'",
            )?;
            for (row, reason) in picks {
                if done.execute(params![stamp, tag, account_id, row.message_id])? == 0 {
                    continue;
                }
                rows.push(serde_json::json!({
                    "id": row.message_id,
                    "reason": reason.as_str(),
                    "was": row.status,
                }));
                cleared.push((row.message_id, reason));
                senders.insert(row.sender);
            }
        }
        if cleared.is_empty() {
            // Dropping the transaction takes the empty batch row with it.
            return Ok(SmartDoneOutcome::default());
        }
        tx.execute(
            "UPDATE smart_done_batches SET cleared = ?1, detail = ?2 WHERE id = ?3",
            params![
                cleared.len() as i64,
                serde_json::json!({ "rows": rows }).to_string(),
                batch_id
            ],
        )?;
        tx.commit()?;
        Ok(SmartDoneOutcome {
            batch_id: Some(batch_id),
            cleared,
            senders: senders.len(),
        })
    }

    /// Give a batch back: every row it took that is STILL done and STILL its
    /// own goes back to the status it had. A row a person has since reopened
    /// or re-done carries no `resolved_by` any more and is left exactly as
    /// they put it.
    ///
    /// `None` for a batch this account does not have (unknown and another
    /// account's are the same answer). `Some(0)` for one already undone.
    pub fn smart_done_undo(
        &self,
        account_id: AccountId,
        batch_id: i64,
        now: DateTime<Utc>,
    ) -> Result<Option<usize>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some((undone_at, detail)) = tx
            .query_row(
                "SELECT undone_at, detail FROM smart_done_batches
                 WHERE account_id = ?1 AND id = ?2",
                params![account_id, batch_id],
                |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
        else {
            return Ok(None);
        };
        if undone_at.is_some() {
            return Ok(Some(0));
        }
        let tag = batch_tag(batch_id);
        let mut reopened = 0usize;
        {
            let mut restore = tx.prepare(
                "UPDATE triage
                 SET status = ?1, resolved_at = NULL, resolved_by = NULL
                 WHERE account_id = ?2 AND message_id = ?3
                   AND status = 'done' AND resolved_by = ?4",
            )?;
            let detail: serde_json::Value = serde_json::from_str(&detail).unwrap_or_default();
            for row in detail["rows"].as_array().into_iter().flatten() {
                let Some(id) = row["id"].as_i64() else {
                    continue;
                };
                // Only ever back to a status an open row can have.
                let was = match row["was"].as_str() {
                    Some("new") => "new",
                    _ => "open",
                };
                reopened += restore.execute(params![was, account_id, id, tag])?;
            }
        }
        // THE TAG IS THE MEMBERSHIP, the detail only remembers what each row
        // was. Anything still wearing the tag (a detail that would not parse)
        // is given back as open rather than stranded as done.
        reopened += tx.execute(
            "UPDATE triage
             SET status = 'open', resolved_at = NULL, resolved_by = NULL
             WHERE account_id = ?1 AND status = 'done' AND resolved_by = ?2",
            params![account_id, tag],
        )?;
        tx.execute(
            "UPDATE smart_done_batches SET undone_at = ?1 WHERE id = ?2",
            params![now.to_rfc3339(), batch_id],
        )?;
        tx.commit()?;
        Ok(Some(reopened))
    }
}
