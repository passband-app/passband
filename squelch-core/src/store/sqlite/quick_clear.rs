//! The store half of quick clear (docs/RENOTIFY.md, "Quick clear"): the open
//! backlog, item by item, with what [`crate::quick_clear`] needs to price
//! each one. The pricing, the budget and the floors are not here.
//!
//! THE BACKLOG IS TWO LISTS, read by the functions that build the screens
//! the owner sees, so the count cannot drift from them:
//!
//! - every card on the For your eyes board (`agent_triage::list_items`, the
//!   same rows the Dock badge counts), and
//! - the Reading rows smart clear would take on stored facts alone, from
//!   smart clear's own scan (`smart_clear::scan`, the very judgement a press
//!   runs), so the press the client turns them into clears at least those.
//!
//! READ-ONLY, one lock.

use super::agent_triage::list_items;
use super::attention::dwell_ledger_since;
use super::reading_senders::{SenderMeasurer, sender_key};
use super::smart_clear::scan;
use super::*;
use crate::quick_clear::{DwellHistory, OpenItem};
use crate::store::agent_triage::AgentListQuery;
use crate::triage::decision::{MessageDecision, ReadingKind, ThreadAttentionDecision};
use crate::triage::sender_fatigue::{median, word_count};
use std::collections::HashMap;

/// Newest reported looks read per sender. The median of twenty is a habit;
/// more is only a longer query.
const DWELL_SAMPLE: i64 = 20;

/// A message's own guards, by primary key: kept close (`sealed_mail_sql!`,
/// the one definition) and a reminder pending or fired.
const ROW_GUARDS_SQL: &str = concat!(
    "SELECT ",
    sealed_mail_sql!(),
    ", COALESCE(t.remind_at IS NOT NULL OR t.reminded_at IS NOT NULL, 0)
     FROM messages m
     LEFT JOIN triage t ON t.account_id = m.account_id AND t.message_id = m.id
     WHERE m.account_id = ?1 AND m.id = ?2"
);

/// A sender's reported looks, newest first. Dwell zero is an open no client
/// reported on, not a look of no length, so it is no sample.
const SENDER_DWELL_SQL: &str = "SELECT t.opened_at, t.dwell_ms
     FROM messages m INDEXED BY idx_messages_from_nocase
     JOIN triage t ON t.message_id = m.id
     WHERE m.account_id = ?1 AND m.from_addr = ?2 COLLATE NOCASE
       AND t.opened_at IS NOT NULL AND t.dwell_ms > 0
     ORDER BY m.id DESC LIMIT ?3";

const BODY_SQL: &str = "SELECT body FROM messages WHERE account_id = ?1 AND id = ?2";

impl SqliteStore {
    /// The open backlog quick clear prices: the FYE board, then the Reading
    /// rows smart clear would take on stored facts. `inventory` and
    /// `feed_limit` must be the Reading feed's own, as for a smart-clear
    /// press, so the rows are the ones that press would see.
    pub fn quick_clear_backlog(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
        inventory: &AgentListQuery,
        feed_limit: usize,
    ) -> Result<Vec<OpenItem>> {
        let conn = self.lock()?;
        let (reading, _) = scan(&conn, account_id, now, inventory, feed_limit, None)?;
        let fye = list_items(&conn, account_id, "fye", now)?;
        let dwell_floor = dwell_ledger_since(&conn, account_id)?;
        let mut measurer = SenderMeasurer::new(&conn, account_id, now)?;
        let mut guards = conn.prepare(ROW_GUARDS_SQL)?;
        let mut dwell = conn.prepare(SENDER_DWELL_SQL)?;
        let mut body = conn.prepare(BODY_SQL)?;
        let mut senders: HashMap<String, (bool, Option<DwellHistory>)> = HashMap::new();

        let mut row_guards = |id: i64| -> Result<(bool, bool)> {
            Ok(guards
                .query_row(params![account_id, id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                // No message row: nothing can be said about it, so it is
                // treated as kept close and never priced.
                .unwrap_or((true, false)))
        };

        let mut out = Vec::with_capacity(fye.len());
        for item in &fye {
            let sender = sender_key(&item.from_addr);
            let (protected, history) = match senders.get(&sender) {
                Some(known) => *known,
                None => {
                    let protected = measurer.standing(&sender)?.protected();
                    let looks = dwell
                        .query_map(params![account_id, sender, DWELL_SAMPLE], |r| {
                            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                        })?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    // Only looks where dwell could be reported count, and a
                    // stamp that will not parse is not evidence.
                    let mut ms: Vec<u64> = looks
                        .into_iter()
                        .filter(|(at, _)| {
                            DateTime::parse_from_rfc3339(at)
                                .is_ok_and(|at| at.with_timezone(&Utc) >= dwell_floor)
                        })
                        .map(|(_, ms)| ms.max(0) as u64)
                        .collect();
                    let history = median(&mut ms).map(|median_ms| DwellHistory {
                        samples: ms.len() as u32,
                        median_ms,
                    });
                    senders.insert(sender.clone(), (protected, history));
                    (protected, history)
                }
            };
            let (sealed, reminder) = row_guards(item.message_id)?;
            let words = if item.decision.reading_kind == Some(ReadingKind::Content) {
                body.query_row(params![account_id, item.message_id], |r| {
                    Ok(word_count(r.get_ref(0)?.as_str().unwrap_or("")))
                })
                .optional()?
                .unwrap_or(0)
            } else {
                0
            };
            let one_line = if item.decision.summary.trim().is_empty() {
                item.attention.summary.clone()
            } else {
                item.decision.summary.clone()
            };
            out.push(OpenItem {
                message_id: item.message_id,
                thread_id: item.thread_id.clone(),
                sender,
                one_line,
                decision: item.decision.clone(),
                attention: item.attention.clone(),
                smart_clear: false,
                sealed,
                reminder,
                sender_protected: protected,
                words,
                sender_dwell: history,
            });
        }

        for row in reading.into_iter().filter(|r| r.reason.is_some()) {
            let (sealed, reminder) = row_guards(row.message_id)?;
            out.push(OpenItem {
                message_id: row.message_id,
                thread_id: row.thread_id,
                sender: row.sender,
                one_line: row.summary,
                decision: MessageDecision::default(),
                attention: ThreadAttentionDecision::default(),
                smart_clear: true,
                sealed,
                reminder,
                sender_protected: false,
                words: 0,
                sender_dwell: None,
            });
        }
        Ok(out)
    }
}
