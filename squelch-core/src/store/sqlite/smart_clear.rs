//! The store half of smart clear (`POST /client/v2/reading/smart_clear`): gather
//! what the judgement in [`crate::triage::smart_clear`] reads, write the batch
//! with its provenance, and undo it exactly.
//!
//! HUMAN-DOOR ONLY. It resolves mail on the owner's say-so, which no agent
//! door may do.
//!
//! THREE STEPS, AND THE LOCK IS HELD FOR NONE OF THE MODEL CALL:
//!
//! 1. [`SqliteStore::smart_clear_scan`], one lock: which open Reading rows
//!    have nothing stored that decides them, so the model can be asked.
//! 2. The model pass, in the caller, with no lock held.
//! 3. [`SqliteStore::smart_clear_apply`], one lock and one transaction: SCAN
//!    AGAIN and write. Nothing step 1 saw about a ROW is trusted here (only
//!    its sender scores are carried over; see that function). Seconds have
//!    passed and the owner has been using the app: a row may have been opened,
//!    reminded, resolved, or its sender kept. So every row is judged afresh
//!    from the store as it is now, and a model verdict is honoured only for a
//!    row that would still be asked about.
//!
//! WHICH ROWS are the Reading feed's own (`agent_triage::reading_items`) and
//! which senders are scored is the purge list's own (`reading_senders`'s
//! measurer and its cap), so smart clear can never sweep a row the feed does
//! not show or a sender for a score the list does not show. That also sets
//! its reach: open Reading mail from the feed's thirty-day window. Older mail
//! is never swept, on purpose; the owner cannot see it there to check.
//!
//! MESSAGE-SCOPED, unlike a human done, which resolves the whole thread. A
//! sweep that took a sibling it never judged could not say why, and could not
//! give it back. What it takes is recorded twice over: `triage.resolved_by`
//! on each row, and the batch's `detail` with the status each row had.

use super::agent_triage::{list_items, reading_items};
use super::attention::open_ledger_since;
use super::reading_senders::{SenderMeasurer, busiest_senders, sender_key};
use super::*;
use crate::store::agent_triage::AgentListQuery;
use crate::triage::smart_clear::{
    ClearReason, ModelRow, RowFacts, SMART_CLEAR_MODEL_ROWS, Verdict, model_question,
    stored_reason, summary_fingerprint,
};
use std::collections::{HashMap, HashSet};

/// `triage.resolved_by` for the rows of one batch.
fn batch_tag(batch_id: i64) -> String {
    format!("smart_clear:{batch_id}")
}

/// Each measured sender's fatigue score, by lowercased address.
pub type SenderScores = HashMap<String, Option<u8>>;

/// What step 1 hands to step 3 across the model call.
#[derive(Debug, Clone, Default)]
pub struct SmartClearScan {
    /// The rows worth asking the model about, newest first, capped.
    pub asks: Vec<ModelRow>,
    /// The sender scores step 1 measured, reused by step 3. See
    /// [`SqliteStore::smart_clear_apply`] for why that is safe.
    pub sender_scores: SenderScores,
    /// When step 1 read the store. A sender whose mail was OPENED after this
    /// has a score that is out of date in the one direction that matters.
    pub scanned_at: Option<DateTime<Utc>>,
}

/// How far back an undo is remembered. A row comes back into Reading's
/// thirty-day window at most thirty days after it arrived, so ninety covers
/// every row an undone batch could still offer, with room.
const UNDO_MEMORY_DAYS: i64 = 90;

/// The newest open on one sender's mail, for the stale-score check. Seeks the
/// sender index like every per-sender read here.
const SENDER_LAST_OPEN_SQL: &str = "SELECT MAX(t.opened_at)
     FROM messages m INDEXED BY idx_messages_from_nocase
     JOIN triage t ON t.message_id = m.id
     WHERE m.account_id = ?1 AND m.from_addr = ?2 COLLATE NOCASE";

/// Every row id an undo gave back, from this account's batches undone since
/// `since`. One indexed read of a handful of small rows.
fn given_back(
    conn: &Connection,
    account_id: AccountId,
    since: DateTime<Utc>,
) -> Result<HashSet<i64>> {
    let mut stmt = conn.prepare(
        "SELECT detail FROM smart_clear_batches
         WHERE account_id = ?1 AND undone_at IS NOT NULL AND undone_at >= ?2",
    )?;
    let details = stmt
        .query_map(params![account_id, since.to_rfc3339()], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(details
        .iter()
        .filter_map(|detail| serde_json::from_str::<serde_json::Value>(detail).ok())
        .flat_map(|detail| {
            detail["reopened"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|id| id.as_i64())
        })
        .collect())
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

/// One cleared message as the receipt's full list shows it: what it was, who
/// sent it, and why it went.
///
/// THIS IS MAIL TEXT, AND IT LIVES ONLY IN THE ANSWER TO THE PRESS. It is
/// read for the human door's response and is never written to the batch's
/// `detail`, which records ids, reasons and prior status and nothing a sender
/// wrote. A sealed row cannot be one of these because [`MARK_DONE_SQL`]
/// cannot write one.
#[derive(Debug, Clone, PartialEq)]
pub struct SmartClearRow {
    pub message_id: i64,
    pub reason: ClearReason,
    /// The bare address as stored.
    pub sender: String,
    /// The display name, or empty when the mail gave none.
    pub sender_name: String,
    pub subject: String,
    /// RFC 3339, as stored.
    pub received_at: String,
}

/// What the receipt shows of one message, by primary key.
const RECEIPT_ROW_SQL: &str = "SELECT from_addr, COALESCE(from_name, ''), subject, received_at
     FROM messages WHERE account_id = ?1 AND id = ?2";

/// What one press cleared.
#[derive(Debug, Clone, Default)]
pub struct SmartClearOutcome {
    /// `None` when nothing was cleared: no batch is recorded for an empty one.
    pub batch_id: Option<i64>,
    /// Each message taken and why, newest first.
    pub cleared: Vec<(i64, ClearReason)>,
    /// The same messages in the same order, with what the receipt's full
    /// list shows of each.
    pub rows: Vec<SmartClearRow>,
    /// How many distinct senders that mail came from.
    pub senders: usize,
}

/// One row's own state, by primary key: (kept close, status, opened, a
/// reminder pending or fired). "Kept close" is `sealed_mail_sql!`, the one
/// definition, so auth and restricted mail is protected by its stored
/// decision and not only by the retired `sensitivity` column.
const ROW_STATE_SQL: &str = concat!(
    "SELECT ",
    sealed_mail_sql!(),
    ", t.status, t.opened_at IS NOT NULL,
            t.remind_at IS NOT NULL OR t.reminded_at IS NOT NULL
     FROM triage t JOIN messages m ON m.account_id = t.account_id AND m.id = t.message_id
     WHERE t.account_id = ?1 AND t.message_id = ?2"
);

/// The write: ONE ROW, by primary key, this message and no sibling. The
/// predicates repeat what the scan just established, so that this statement
/// alone can never resolve a row that is already done or one that is kept
/// close.
pub(super) const MARK_DONE_SQL: &str = concat!(
    "UPDATE triage AS t
     SET status = 'done', resolved_at = ?1, resolved_by = ?2
     WHERE t.account_id = ?3 AND t.message_id = ?4
       AND t.status != 'done'
       AND EXISTS(SELECT 1 FROM messages m
                  WHERE m.account_id = t.account_id AND m.id = t.message_id
                    AND NOT ",
    sealed_mail_sql!(),
    ")"
);

/// Judge every open Reading row from the store as it is right now.
///
/// `earlier` is step 1's result when step 3 is calling; `None` measures the
/// senders. Everything ELSE is always read fresh: the feed, each row's own
/// state, FYE membership, what an undo gave back, and who the owner said to
/// leave alone.
fn scan(
    conn: &Connection,
    account_id: AccountId,
    now: DateTime<Utc>,
    inventory: &AgentListQuery,
    feed_limit: usize,
    earlier: Option<&SmartClearScan>,
) -> Result<(Vec<Scanned>, SenderScores)> {
    let items = reading_items(conn, account_id, feed_limit, inventory, now)?;
    let mut measurer = SenderMeasurer::new(conn, account_id, now)?;

    // SCORES for the senders the purge list would show, and no others: a
    // sender past its cap was never shown a score to be swept for.
    let mut scores = match earlier {
        Some(scan) => scan.sender_scores.clone(),
        None => {
            let (_, groups) = busiest_senders(&items);
            let mut scores = SenderScores::new();
            for (sender, _, _) in &groups {
                scores.insert(sender.clone(), measurer.measure(sender)?.score);
            }
            scores
        }
    };

    // WHAT IS ON THE FYE BOARD, by the FYE listing's own function: the
    // decision's attention, the owner's per-thread override, auth and own
    // mail left out, reminders in. Asked once; a row whose thread is here is
    // not smart clear's to take.
    let fye_threads: HashSet<String> = list_items(conn, account_id, "fye", now)?
        .into_iter()
        .map(|item| item.thread_id)
        .collect();

    // A SCORE FROM BEFORE AN OPEN IS NOT REUSED. Opening mail is the one
    // thing the owner can do during a model call that LOWERS a sender's
    // score, and a sender at 77 with one more open may be a sender at 72. So
    // any sender whose mail was opened after step 1 read the store is
    // unscored for this press: nothing of theirs goes as `ignored_sender`.
    // Asked only of senders whose reused score could sweep anything.
    if let Some(scanned_at) = earlier.and_then(|scan| scan.scanned_at) {
        let mut last_open = conn.prepare(SENDER_LAST_OPEN_SQL)?;
        for (sender, score) in scores.iter_mut() {
            if score.is_none_or(|s| s < crate::triage::smart_clear::SMART_CLEAR_SENDER_SCORE) {
                continue;
            }
            let opened: Option<String> =
                last_open.query_row(params![account_id, sender], |r| r.get(0))?;
            // A stamp that will not parse cannot be shown to be older.
            let stale = opened.is_some_and(|at| {
                DateTime::parse_from_rfc3339(&at)
                    .map_or(true, |at| at.with_timezone(&Utc) > scanned_at)
            });
            if stale {
                *score = None;
            }
        }
    }

    // WHAT AN UNDO GAVE BACK. The owner looked at a sweep and said no; the
    // same rows are not offered up again by the next press.
    let given_back = given_back(
        conn,
        account_id,
        now - chrono::Duration::days(UNDO_MEMORY_DAYS),
    )?;

    // WHEN OPENS BEGAN TO BE RECORDED. Mail from before it has no open on
    // record because none could have been, which `stale_promo` must not read
    // as the owner never opening it.
    let open_ledger = open_ledger_since(conn, account_id)?;

    let mut state = conn.prepare(ROW_STATE_SQL)?;
    let mut protected: HashMap<String, bool> = HashMap::new();
    let mut out = Vec::with_capacity(items.len());
    for item in &items {
        let sender = sender_key(&item.from_addr);
        // WHO THE OWNER SAID TO LEAVE ALONE is asked of every sender, scored
        // or not, and asked NOW: it is three small probes, and it is the
        // guard.
        let sender_protected = match protected.get(&sender) {
            Some(known) => *known,
            None => {
                let standing = measurer.standing(&sender)?.protected();
                protected.insert(sender.clone(), standing);
                standing
            }
        };
        // A message with no triage row has no status to write and is skipped.
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
            thread_attention: Some(&item.attention),
            received_at,
            opened,
            sealed,
            reminder,
            in_fye: fye_threads.contains(&item.thread_id),
            given_back: given_back.contains(&item.message_id),
            before_open_ledger: received_at < open_ledger,
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
    Ok((out, scores))
}

impl SqliteStore {
    /// Step 1: the rows worth asking the model about, newest first, at most
    /// [`SMART_CLEAR_MODEL_ROWS`], and the sender scores measured on the way.
    /// Read-only.
    pub fn smart_clear_scan(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
        inventory: &AgentListQuery,
        feed_limit: usize,
    ) -> Result<SmartClearScan> {
        let conn = self.lock()?;
        let (rows, sender_scores) = scan(&conn, account_id, now, inventory, feed_limit, None)?;
        Ok(SmartClearScan {
            scanned_at: Some(now),
            asks: rows
                .into_iter()
                .filter_map(|row| {
                    Some(ModelRow {
                        message_id: row.message_id,
                        summary: row.question?,
                        received_at: row.received_at,
                    })
                })
                .take(SMART_CLEAR_MODEL_ROWS)
                .collect(),
            sender_scores,
        })
    }

    /// Step 3: judge again and write, in one transaction.
    ///
    /// `verdicts` is what the model said about the rows step 1 offered it. A
    /// verdict is honoured only for a row that is STILL askable (not opened
    /// into a reminder, resolved, protected, or decided by a stored fact in
    /// the meantime) and whose summary is STILL the one the model was shown:
    /// a message re-triaged during the call keeps its id and changes its
    /// summary, and a yes about the old text says nothing about the new.
    ///
    /// `scan` is step 1's result, and its SENDER SCORES ARE REUSED rather
    /// than measured again. That is the one thing not re-read here, and it is
    /// safe to leave: a score is a count over ninety days of a sender's mail,
    /// so the seconds of a model call cannot move it across a threshold in
    /// any way that matters, and everything a score is allowed to DO is still
    /// gated by what is re-read. A sender kept, written to or put under a rule
    /// in the meantime is protected by a fresh probe. A row opened in the
    /// meantime is no longer the sender sweep's to take, and a sender ANY of
    /// whose mail was opened in the meantime is treated as unscored, since an
    /// open is the one thing that lowers a score. A sender that step 1 did
    /// not score has no score and is not swept. Measuring twice bought
    /// none of that and held the store mutex for a second sweep of up to a
    /// hundred senders' mail.
    pub fn smart_clear_apply(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
        inventory: &AgentListQuery,
        feed_limit: usize,
        scan_result: &SmartClearScan,
        verdicts: &HashMap<i64, Verdict>,
    ) -> Result<SmartClearOutcome> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let (rows, _) = scan(
            &tx,
            account_id,
            now,
            inventory,
            feed_limit,
            Some(scan_result),
        )?;
        let picks: Vec<(Scanned, ClearReason)> = rows
            .into_iter()
            .filter_map(|row| {
                // A stored fact first; the model only for a row that is still
                // one the model would be asked about, about the same text.
                let reason = row.reason.or_else(|| {
                    let question = row.question.as_ref()?;
                    let verdict = verdicts.get(&row.message_id)?;
                    (verdict.summary == summary_fingerprint(question)).then_some(verdict.reason)
                })?;
                Some((row, reason))
            })
            .collect();
        if picks.is_empty() {
            return Ok(SmartClearOutcome::default());
        }

        let stamp = now.to_rfc3339();
        tx.execute(
            "INSERT INTO smart_clear_batches(account_id, created_at, cleared, detail)
             VALUES(?1, ?2, 0, '{}')",
            params![account_id, stamp],
        )?;
        let batch_id = tx.last_insert_rowid();
        let tag = batch_tag(batch_id);

        let mut cleared = Vec::with_capacity(picks.len());
        let mut rows = Vec::with_capacity(picks.len());
        let mut receipt = Vec::with_capacity(picks.len());
        let mut senders = HashSet::new();
        {
            let mut done = tx.prepare(MARK_DONE_SQL)?;
            let mut shown = tx.prepare(RECEIPT_ROW_SQL)?;
            for (row, reason) in picks {
                if done.execute(params![stamp, tag, account_id, row.message_id])? == 0 {
                    continue;
                }
                // THE RECEIPT IS READ HERE, for the row just written and in
                // the transaction that wrote it, so the list the owner is
                // shown can never name a message this press did not take or
                // miss one it did. A later pass over the ids could do both.
                receipt.push(shown.query_row(params![account_id, row.message_id], |r| {
                    Ok(SmartClearRow {
                        message_id: row.message_id,
                        reason,
                        sender: r.get(0)?,
                        sender_name: r.get(1)?,
                        subject: r.get(2)?,
                        received_at: r.get(3)?,
                    })
                })?);
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
            return Ok(SmartClearOutcome::default());
        }
        tx.execute(
            "UPDATE smart_clear_batches SET cleared = ?1, detail = ?2 WHERE id = ?3",
            params![
                cleared.len() as i64,
                serde_json::json!({ "rows": rows }).to_string(),
                batch_id
            ],
        )?;
        tx.commit()?;
        Ok(SmartClearOutcome {
            batch_id: Some(batch_id),
            cleared,
            rows: receipt,
            senders: senders.len(),
        })
    }

    /// Give a batch back: every row it took that is STILL done and STILL its
    /// own goes back to the status it had. A row a person has since reopened
    /// or re-done carries no `resolved_by` any more and is left exactly as
    /// they put it.
    ///
    /// `None` for a batch this account does not have (unknown and another
    /// account's are the same answer). `Some(0)` for one already undone. An
    /// error, with nothing changed, for a batch whose record cannot be read.
    pub fn smart_clear_undo(
        &self,
        account_id: AccountId,
        batch_id: i64,
        now: DateTime<Utc>,
    ) -> Result<Option<usize>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some((undone_at, detail)) = tx
            .query_row(
                "SELECT undone_at, detail FROM smart_clear_batches
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
        // A RECORD THAT CANNOT BE READ IS NOT THROWN AWAY BY TRYING. The
        // list in `detail` is the only way to this batch's rows, so if it is
        // not there the answer is an error and NOTHING is touched: the batch
        // stays un-undone and its rows stay tagged, to be undone once
        // whatever is wrong with the row is put right. Stamping it undone
        // with nothing reopened would strand those rows as done for good.
        let unreadable = || {
            CoreError::Other(anyhow::anyhow!(
                "smart clear batch {batch_id} has no readable record of what it took"
            ))
        };
        let mut detail: serde_json::Value =
            serde_json::from_str(&detail).map_err(|_| unreadable())?;
        let rows = detail["rows"].as_array().cloned().ok_or_else(unreadable)?;

        let tag = batch_tag(batch_id);
        let mut given_back = Vec::new();
        {
            let mut restore = tx.prepare(
                "UPDATE triage
                 SET status = ?1, resolved_at = NULL, resolved_by = NULL
                 WHERE account_id = ?2 AND message_id = ?3
                   AND status = 'done' AND resolved_by = ?4",
            )?;
            // THE BATCH'S OWN LIST IS THE MEMBERSHIP, and every row on it is
            // reached by primary key. The tag in `resolved_by` is checked on
            // each, so a row a person has taken over is skipped, but the tag
            // is never SEARCHED for: that would walk every done row the
            // account has, under the store mutex, to find a few dozen.
            for row in &rows {
                let Some(id) = row["id"].as_i64() else {
                    continue;
                };
                // Only ever back to a status an open row can have.
                let was = match row["was"].as_str() {
                    Some("new") => "new",
                    _ => "open",
                };
                if restore.execute(params![was, account_id, id, tag])? > 0 {
                    given_back.push(id);
                }
            }
        }
        let reopened = given_back.len();
        // REMEMBERED: these are the rows the owner said no about. The next
        // press reads this list and leaves them alone (`given_back`). A row
        // the owner had already taken over is not on it; they dealt with
        // that one themselves.
        detail["reopened"] = serde_json::json!(given_back);
        tx.execute(
            "UPDATE smart_clear_batches SET undone_at = ?1, detail = ?2 WHERE id = ?3",
            params![now.to_rfc3339(), detail.to_string(), batch_id],
        )?;
        tx.commit()?;
        Ok(Some(reopened))
    }
}
