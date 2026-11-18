//! What the lapsed-user note lists: the low-value mail from the last week
//! that triage already handled, the receipts and order and shipping
//! confirmations somebody would otherwise have opened and marked read by hand
//! in their old client. The note itself (who gets it, when, and the words) is
//! the api crate's `lapsed_note`; this is only the read.
//!
//! READ-ONLY, ONE STATEMENT, NO MODEL. Everything here is a fact triage
//! already stored: the decision's kinds and records, its placement, the
//! owner's own behaviour. Nothing is classified anew.
//!
//! EVERY EXCLUSION IS A DECISION, and each one errs towards leaving mail out:
//!
//! - NOT THE OWNER'S OWN MAIL, by authorship (`own_mail_sql!`), never the bare
//!   `is_sent` column.
//! - NOT SPAM, NOT THE ARCHIVE TIER, and not mail that FAILED authentication
//!   (`auth_pass IS NOT 0`): a forgery in a shop's name is not that shop's
//!   receipt, and its subject must not be repeated back to the owner.
//! - NOT SEALED MAIL, by the strict `sealed_mail_sql!`, so mail whose access
//!   check is still pending is out too: a login code from a shop arrives from
//!   the address its receipts do.
//! - ONLY A RECEIPT OR A DELIVERY, by the decision's kinds or records, and
//!   never one that is also a bill, correspondence or auth mail. A delivery
//!   whose status is `exception` is a problem, not a chore, and is left out.
//! - ONLY WHAT TRIAGE KEPT OUT OF THE WAY: no thread placed in For your eyes
//!   (by the model or pinned by a person), nothing in Reading, no reminder.
//! - NOTHING THE OWNER ALREADY OPENED in a Passband client.
//! - NOTHING FROM A KNOWN CONTACT (`sent_count > 0`): a friend's mail is never
//!   "mail you shouldn't have had to open", whatever triage called it.
//!
//! Whether the owner opened it in Gmail is not stored locally, so it is not a
//! test here.

use super::*;

/// The most rows one read returns. A week of receipts is a few dozen at most;
/// the cap bounds the lock hold if a mailbox is unusual.
pub const MAX_CANDIDATES: usize = 500;

/// The short label a listed message carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    Receipt,
    Order,
    Shipped,
    Delivered,
}

impl NoteKind {
    /// The label as the note prints it.
    pub fn label(self) -> &'static str {
        match self {
            NoteKind::Receipt => "Receipt",
            NoteKind::Order => "Order",
            NoteKind::Shipped => "Shipped",
            NoteKind::Delivered => "Delivered",
        }
    }
}

/// One message the note may list. Facts only, straight off the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteCandidate {
    pub from_addr: String,
    pub from_name: Option<String>,
    pub subject: String,
    /// The stored `messages.snippet` (never the body).
    pub snippet: String,
    pub received_at: DateTime<Utc>,
    pub kind: NoteKind,
}

const CANDIDATES_SQL: &str = concat!(
    "SELECT m.from_addr, m.from_name, m.subject, m.received_at, d.decision_json, m.snippet
     FROM messages m
     LEFT JOIN triage t ON t.message_id = m.id
     JOIN agent_message_decisions d ON d.account_id = m.account_id AND d.message_id = m.id
     WHERE m.account_id = ?1
       AND m.received_at >= ?2
       AND m.is_spam = 0 AND m.is_archive = 0
       AND m.auth_pass IS NOT 0
       AND NOT ",
    own_mail_sql!(),
    " AND NOT ",
    sealed_mail_sql!(),
    " AND CASE WHEN json_valid(d.decision_json) THEN
           (EXISTS(SELECT 1 FROM json_each(d.decision_json, '$.kinds') k
                   WHERE k.value IN ('receipt', 'delivery'))
            OR EXISTS(SELECT 1 FROM json_each(d.decision_json, '$.records') r
                      WHERE json_extract(r.value, '$.kind') IN ('receipt', 'delivery')))
           AND NOT EXISTS(SELECT 1 FROM json_each(d.decision_json, '$.kinds') k
                          WHERE k.value IN ('bill', 'correspondence', 'authentication_security'))
           AND COALESCE(json_extract(d.decision_json, '$.attention.show_in_fye'), 0) = 0
         ELSE 0 END
       AND NOT EXISTS(SELECT 1 FROM agent_thread_attention a
                      WHERE a.account_id = m.account_id AND a.thread_id = m.thread_id
                        AND a.show_in_fye = 1)
       AND NOT EXISTS(SELECT 1 FROM agent_thread_preferences p
                      WHERE p.account_id = m.account_id AND p.thread_id = m.thread_id
                        AND p.show_in_fye = 1)
       AND NOT EXISTS(SELECT 1 FROM agent_message_destinations dst
                      WHERE dst.account_id = m.account_id AND dst.message_id = m.id)
       AND t.opened_at IS NULL AND t.remind_at IS NULL AND t.reminded_at IS NULL
       AND NOT EXISTS(SELECT 1 FROM contacts c
                      WHERE c.account_id = m.account_id
                        AND c.addr = lower(trim(m.from_addr)) COLLATE NOCASE
                        AND c.sent_count > 0)
     ORDER BY m.received_at DESC, m.id DESC
     LIMIT ?3"
);

/// The label a stored decision earns, or `None` when it should not be listed
/// after all (a delivery gone wrong). Records win over kinds: a record says
/// what the mail is about, a kind only what it looks like.
fn kind_of(decision_json: &str) -> Option<NoteKind> {
    let v: serde_json::Value = serde_json::from_str(decision_json).ok()?;
    let records = v["records"].as_array().cloned().unwrap_or_default();
    let mut receipt = false;
    let mut delivery: Option<NoteKind> = None;
    for r in &records {
        match r["kind"].as_str() {
            Some("receipt") => receipt = true,
            Some("delivery") => {
                let status = r["status"].as_str().unwrap_or("unknown");
                let kind = match status {
                    "exception" => return None,
                    "ordered" => NoteKind::Order,
                    "delivered" => NoteKind::Delivered,
                    _ => NoteKind::Shipped,
                };
                delivery.get_or_insert(kind);
            }
            _ => {}
        }
    }
    if let Some(kind) = delivery {
        return Some(kind);
    }
    if receipt {
        return Some(NoteKind::Receipt);
    }
    let kinds = v["kinds"].as_array().cloned().unwrap_or_default();
    if kinds.iter().any(|k| k.as_str() == Some("delivery")) {
        return Some(NoteKind::Shipped);
    }
    if kinds.iter().any(|k| k.as_str() == Some("receipt")) {
        return Some(NoteKind::Receipt);
    }
    None
}

impl SqliteStore {
    /// The mail the lapsed-user note may list: received at or after `since`,
    /// newest first, at most [`MAX_CANDIDATES`]. See the module docs for what
    /// is left out and why.
    pub fn lapsed_note_candidates(
        &self,
        account_id: AccountId,
        since: DateTime<Utc>,
    ) -> Result<Vec<NoteCandidate>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare_cached(CANDIDATES_SQL)?;
        let rows = stmt
            .query_map(
                params![account_id, since.to_rfc3339(), MAX_CANDIDATES as i64],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                        dt(r, 3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter_map(
                |(from_addr, from_name, subject, received_at, decision, snippet)| {
                    Some(NoteCandidate {
                        kind: kind_of(&decision)?,
                        from_addr,
                        from_name,
                        subject,
                        snippet,
                        received_at,
                    })
                },
            )
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_name_the_kind_before_kinds_do() {
        let j = |s: &str| kind_of(s);
        assert_eq!(
            j(r#"{"kinds":["receipt"],"records":[{"kind":"delivery","status":"shipped"}]}"#),
            Some(NoteKind::Shipped)
        );
        assert_eq!(
            j(r#"{"kinds":["delivery"],"records":[{"kind":"delivery","status":"ordered"}]}"#),
            Some(NoteKind::Order)
        );
        assert_eq!(
            j(r#"{"records":[{"kind":"delivery","status":"delivered"}]}"#),
            Some(NoteKind::Delivered)
        );
        assert_eq!(
            j(r#"{"kinds":["delivery"],"records":[{"kind":"receipt"}]}"#),
            Some(NoteKind::Receipt)
        );
        assert_eq!(
            j(r#"{"kinds":["delivery"],"records":[]}"#),
            Some(NoteKind::Shipped)
        );
        assert_eq!(j(r#"{"kinds":["receipt"]}"#), Some(NoteKind::Receipt));
        assert_eq!(j(r#"{"kinds":["promotional"]}"#), None);
        assert_eq!(j("not json"), None);
    }

    #[test]
    fn a_delivery_gone_wrong_is_not_a_chore() {
        assert_eq!(
            kind_of(
                r#"{"kinds":["delivery"],"records":[{"kind":"delivery","status":"exception"}]}"#
            ),
            None
        );
    }
}
