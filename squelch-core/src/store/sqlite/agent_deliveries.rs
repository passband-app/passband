//! Carrier polling projections of explicit agent delivery facts.
//!
//! Canonical decisions own these rows. Retraction rebuilds from remaining
//! proposals and removes an agent-created row when its last proposal disappears.
use super::*;
use crate::triage::ShipmentStatus;
use crate::triage::amazon::{self, AmazonPackage};
use crate::triage::decision::{MessageDecision, RecordProposal};
use crate::triage::extract::shipments::sanitize_tracking_number;
use std::collections::BTreeSet;

/// The key one delivery record files under: its tracking number, or, when it
/// has none and its mail is about one Amazon box, that box's key
/// ([`AmazonPackage::key`]). A model-written number can never collide with a
/// box key: the sanitizer upper-cases, and the prefix is lower-case.
fn delivery_key(number: Option<&str>, package: Option<&AmazonPackage>) -> Option<String> {
    match number.and_then(|n| sanitize_tracking_number(Some(n), None)) {
        Some(number) => Some(number),
        None => package.map(AmazonPackage::key),
    }
}

fn tracking_numbers<'a>(
    decision: &'a MessageDecision,
    package: Option<&'a AmazonPackage>,
) -> impl Iterator<Item = String> + 'a {
    decision
        .records
        .iter()
        .filter_map(move |record| match record {
            RecordProposal::Delivery {
                tracking_number, ..
            } => delivery_key(tracking_number.as_deref(), package),
            _ => None,
        })
}

/// Does this decision hold a delivery record with no usable number? Only then
/// is its mail worth reading for an Amazon box.
fn has_trackless_delivery(decision: &MessageDecision) -> bool {
    decision.records.iter().any(|record| {
        matches!(record, RecordProposal::Delivery { tracking_number, .. }
            if delivery_key(tracking_number.as_deref(), None).is_none())
    })
}

/// SQL columns that load a message's body only when it could be Amazon mail.
/// The exact test is `amazon::package_from_mail`; this only spares reading
/// every other body. The HTML part loads only when the plain-text part has no
/// tracker link: it is many times the size, and this runs on every rebuild.
const AMAZON_BODY_COLUMNS: &str = "m.from_addr,m.auth_pass,
    CASE WHEN lower(m.from_addr) LIKE '%amazon.com%' THEN m.body END,
    CASE WHEN lower(m.from_addr) LIKE '%amazon.com%'
          AND coalesce(m.body,'') NOT LIKE '%progress-tracker%' THEN m.body_html END";

fn package_from_row(
    from_addr: &str,
    auth_pass: Option<bool>,
    body: Option<String>,
    body_html: Option<String>,
) -> Option<AmazonPackage> {
    amazon::package_from_mail(
        from_addr,
        auth_pass,
        body.as_deref().unwrap_or(""),
        body_html.as_deref().unwrap_or(""),
    )
}

/// The Amazon box message `message` is about, if any.
fn message_package(
    conn: &Connection,
    account: AccountId,
    message: i64,
) -> Result<Option<AmazonPackage>> {
    let row = conn
        .query_row(
            &format!(
                "SELECT {AMAZON_BODY_COLUMNS} FROM messages m WHERE m.account_id=?1 AND m.id=?2"
            ),
            params![account, message],
            |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    Ok(row.and_then(|(from, auth, body, html)| package_from_row(&from, auth, body, html)))
}

/// Write what the newest retained mail about a package says onto its row. ONE
/// place, for owned and legacy rows alike, so there is one answer to "what does
/// newer mail do to a shipment":
///
/// * it becomes the row's click target and its carrier (the mail is the newer
///   witness; if it names FedEx for a number USPS rejected five times, the
///   un-retired poll must go to FedEx, not back to USPS);
/// * it moves `last_update` forward, which is what returns a row the listing
///   hid as silent; a re-decided OLD mail moves nothing;
/// * mail NEWER THAN THE ROW un-retires it. A carrier that said "never heard
///   of it" five times was answering about a label the shipper had not handed
///   over, and fresh mail is the evidence the number is real now. Nothing else
///   can reset the counter, because a retired row is not polled.
///
/// `status` is decided by the caller (see `reconcile`). `created_by_message_id`
/// moves only on an owned row: it is the projection's own provenance, and a
/// legacy row keeps the mail that minted it.
#[allow(clippy::too_many_arguments)]
fn record_newer_mail(
    conn: &Connection,
    account: AccountId,
    id: i64,
    managed: bool,
    message: i64,
    info: &crate::triage::ShipmentInfo,
    status: &str,
    received: DateTime<Utc>,
) -> Result<()> {
    conn.execute(
        "UPDATE shipments SET
             carrier=?3,
             tracking_url=?4,
             created_by_message_id=CASE WHEN ?9 THEN ?5 ELSE created_by_message_id END,
             last_message_id=?5,
             poll_failures=CASE WHEN last_update<?7 THEN 0 ELSE poll_failures END,
             last_update=MAX(last_update,?7),
             status=?6,
             delivered_at=CASE
                 WHEN ?6='delivered' THEN COALESCE(delivered_at,?7)
                 WHEN carrier_status_raw IS NULL AND ?9 THEN NULL
                 ELSE delivered_at END
         WHERE account_id=?1 AND id=?2 AND (?8 OR last_update<?7)",
        params![
            account,
            id,
            info.carrier,
            info.tracking_url,
            message,
            status,
            received.to_rfc3339(),
            managed,
            managed,
        ],
    )?;
    Ok(())
}

/// One retained delivery proposal, laundered. Newest first in `reconcile`.
struct Proposal {
    message: i64,
    received: DateTime<Utc>,
    /// `item_name` is the sanitized agent name, or empty.
    info: crate::triage::ShipmentInfo,
    merchant: Option<String>,
    order_refs: Vec<String>,
}

/// Rebuild the 'agent' order links of one shipment from its retained
/// proposals. 'legacy' links are left alone, and one already holding a key
/// wins.
///
/// ONE MERCHANT PER BOX. Every ref goes under the package's single `seller`
/// (the newest proposal that named one, with its message), not under whatever
/// each proposal said: one box is sold by one store, and letting two proposals
/// file its refs under two stores would let a single package chain two shops'
/// orders into one card. A ref with no seller anywhere is stored under
/// merchant_key '' and never groups.
///
/// Each link records the message that printed its ref (`message_id`) and the
/// one that named the seller (`merchant_msg`): the agent door serves a link
/// only when both are mail it may read.
fn rebuild_order_links(
    conn: &Connection,
    account: AccountId,
    id: i64,
    mine: &[&Proposal],
    seller: Option<(&str, i64)>,
) -> Result<()> {
    use crate::triage::order_link::{merchant_key, order_key};
    if !super::migrate::order_links_exist(conn)? {
        // The migration could not create it this open; it retries next open.
        return Ok(());
    }
    conn.execute(
        "DELETE FROM shipment_order_links WHERE account_id=?1 AND shipment_id=?2 AND source='agent'",
        params![account, id],
    )?;
    let (merchant, merchant_msg) = match seller {
        Some((name, message)) => (name, Some(message)),
        None => ("", None),
    };
    for proposal in mine {
        for order_ref in &proposal.order_refs {
            conn.execute(
                "INSERT OR IGNORE INTO shipment_order_links
                     (account_id,shipment_id,merchant_key,order_key,merchant,order_ref,source,
                      message_id,merchant_msg)
                 VALUES(?1,?2,?3,?4,?5,?6,'agent',?7,?8)",
                params![
                    account,
                    id,
                    merchant_key(merchant),
                    order_key(order_ref),
                    merchant,
                    order_ref,
                    proposal.message,
                    merchant_msg,
                ],
            )?;
        }
    }
    Ok(())
}

/// What the retained proposals say the package IS: its name and its seller.
///
/// An OWNED row is a projection, so both follow the newest proposal that
/// states them, and fall away when the last such proposal is retracted. A
/// proposal that states NO name never erases one: "newest with a name" skips
/// it.
///
/// A LEGACY row's name and merchant were written by the old subsystem, and the
/// agent's replace them only when mail NEWER than the row lands (`since`, the
/// row's `last_update` before this pass), the same gate `record_newer_mail`
/// uses. Nothing newer, or nothing newer that names it: untouched.
///
/// ONCE THE AGENT HAS RENAMED A LEGACY ROW the old name is gone, and the name
/// is the agent's like an owned row's: it follows the newest proposal naming
/// the package (the gate has nothing left to protect), and when the proposal
/// that named it is retracted and none remaining does, it is cleared to the
/// unnamed state. The legacy name cannot come back; showing a retracted
/// mail's words would be worse. The merchant likewise, judged by
/// `order_merchant_msg` (set only when the agent wrote it).
fn record_identity(
    conn: &Connection,
    account: AccountId,
    id: i64,
    managed: bool,
    mine: &[&Proposal],
    since: DateTime<Utc>,
) -> Result<()> {
    let (agent_named, agent_merchant): (bool, bool) = match conn
        .query_row(
            "SELECT item_name_source='agent', order_merchant_msg IS NOT NULL
             FROM shipments WHERE account_id=?1 AND id=?2",
            params![account, id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
    {
        Some(flags) => flags,
        // Retired by the caller: nothing to write.
        None => return Ok(()),
    };
    let named = mine
        .iter()
        .filter(|p| managed || agent_named || p.received > since)
        .find(|p| !p.info.item_name.is_empty());
    let merchant = mine
        .iter()
        .filter(|p| managed || agent_merchant || p.received > since)
        .find_map(|p| p.merchant.as_deref().map(|m| (m, p.message)));
    match named {
        Some(p) => {
            conn.execute(
                "UPDATE shipments SET item_name=?3, item_name_source='agent', item_name_msg=?4
                 WHERE account_id=?1 AND id=?2",
                params![account, id, p.info.item_name, p.message],
            )?;
        }
        // Reset to the column's resting provenance, as every scrub does.
        None if managed || agent_named => {
            conn.execute(
                "UPDATE shipments SET item_name='', item_name_source='regex', item_name_msg=NULL
                 WHERE account_id=?1 AND id=?2",
                params![account, id],
            )?;
        }
        None => {}
    }
    if merchant.is_some() || managed || agent_merchant {
        conn.execute(
            "UPDATE shipments SET order_merchant=?3, order_merchant_msg=?4
             WHERE account_id=?1 AND id=?2",
            params![
                account,
                id,
                merchant.map(|(m, _)| m),
                merchant.map(|(_, msg)| msg)
            ],
        )?;
    }
    Ok(())
}

/// Rebuild every shipment `message`'s decision touched, before (`previous`)
/// and now (`current`), from all retained decisions of the account.
pub(super) fn reconcile(
    conn: &Connection,
    account: AccountId,
    message: i64,
    previous: Option<&MessageDecision>,
    current: &MessageDecision,
) -> Result<()> {
    let package = if previous.is_some_and(has_trackless_delivery) || has_trackless_delivery(current)
    {
        message_package(conn, account, message)?
    } else {
        None
    };
    let affected: BTreeSet<String> = previous
        .into_iter()
        .flat_map(|decision| tracking_numbers(decision, package.as_ref()))
        .chain(tracking_numbers(current, package.as_ref()))
        .collect();
    rebuild(conn, account, &affected)
}

/// ONE-TIME BACKFILL (see `migrate::project_amazon_boxes`): project every
/// Amazon box the account's stored decisions already describe. The agent
/// always wrote these records; they only lacked a key, so no model call is
/// needed. Returns how many boxes were rebuilt.
pub(super) fn project_amazon_boxes(conn: &Connection, account: AccountId) -> Result<usize> {
    let mut statement = conn.prepare(&format!(
        "SELECT d.decision_json,{AMAZON_BODY_COLUMNS}
         FROM agent_message_decisions d JOIN messages m ON m.id=d.message_id AND m.account_id=d.account_id
         WHERE d.account_id=?1 AND m.is_sent=0 AND m.is_spam=0
           AND lower(m.from_addr) LIKE '%amazon.com%'"
    ))?;
    let rows = statement
        .query_map([account], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<bool>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut boxes = BTreeSet::new();
    for (encoded, from, auth, body, html) in rows {
        // One undecodable decision costs only its own box.
        let Ok(decision) = serde_json::from_str::<MessageDecision>(&encoded) else {
            continue;
        };
        if has_trackless_delivery(&decision)
            && let Some(package) = package_from_row(&from, auth, body, html)
        {
            boxes.extend(
                tracking_numbers(&decision, Some(&package)).filter(|k| amazon::is_package_key(k)),
            );
        }
    }
    rebuild(conn, account, &boxes)?;
    Ok(boxes.len())
}

/// Rebuild the rows keyed by `affected` from every retained decision.
fn rebuild(conn: &Connection, account: AccountId, affected: &BTreeSet<String>) -> Result<()> {
    use crate::triage::order_link::{
        sanitize_agent_item_name, sanitize_merchant, sanitize_order_refs,
    };
    if affected.is_empty() {
        return Ok(());
    }
    let mut statement = conn.prepare(&format!(
        "SELECT d.message_id,d.decision_json,m.received_at,m.subject,{AMAZON_BODY_COLUMNS}
         FROM agent_message_decisions d JOIN messages m ON m.id=d.message_id AND m.account_id=d.account_id
         WHERE d.account_id=?1 AND m.is_sent=0 AND m.is_spam=0 ORDER BY m.received_at DESC,m.id DESC",
    ))?;
    let decisions = statement
        .query_map([account], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                dt(row, 2)?,
                row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                (
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<bool>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ),
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut proposals = Vec::new();
    for (message, encoded, received, subject, (from, auth, body, html)) in decisions {
        let decision: MessageDecision =
            serde_json::from_str(&encoded).map_err(|e| CoreError::Other(e.into()))?;
        let package = if has_trackless_delivery(&decision) {
            package_from_row(&from, auth, body, html)
        } else {
            None
        };
        for record in decision.records {
            let RecordProposal::Delivery {
                carrier,
                tracking_number,
                status,
                item_name,
                merchant,
                order_refs,
                ..
            } = record
            else {
                continue;
            };
            let Some(number) = delivery_key(tracking_number.as_deref(), package.as_ref()) else {
                continue;
            };
            // A box key is Amazon's whatever the model called the carrier
            // (it often says null or "unknown" for Amazon Logistics).
            let boxed = package.as_ref().filter(|_| amazon::is_package_key(&number));
            let carrier = match boxed {
                Some(_) => "amazon".to_string(),
                None => match carrier {
                    Some(carrier)
                        if matches!(
                            carrier.as_str(),
                            "ups" | "usps" | "fedex" | "dhl" | "amazon"
                        ) =>
                    {
                        carrier
                    }
                    _ => continue,
                },
            };
            if affected.contains(&number)
                && let Some(status) = crate::triage::ShipmentStatus::parse(&status)
            {
                // Amazon is the seller of a box Amazon's own mail tracks, and
                // the box's order is the one in its tracker link.
                let merchant = sanitize_merchant(merchant.as_deref())
                    .or_else(|| boxed.map(|_| "Amazon".to_string()));
                let mut order_refs = sanitize_order_refs(&order_refs);
                if let Some(package) = boxed
                    && !order_refs.contains(&package.order_id)
                {
                    order_refs.push(package.order_id.clone());
                }
                let item_name = sanitize_agent_item_name(
                    item_name.as_deref(),
                    &subject,
                    merchant.as_deref(),
                    &carrier,
                    &order_refs,
                )
                .unwrap_or_default();
                proposals.push(Proposal {
                    message,
                    received,
                    info: crate::triage::ShipmentInfo {
                        tracking_url: match boxed {
                            Some(package) => Some(package.tracking_url()),
                            None => crate::triage::shipment::tracking_url(&carrier, &number),
                        },
                        carrier,
                        tracking_number: number,
                        status,
                        item_name,
                    },
                    merchant,
                    order_refs,
                });
            }
        }
    }
    for number in affected {
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM shipments WHERE account_id=?1 AND tracking_number=?2",
                params![account, number],
                |row| row.get(0),
            )
            .optional()?;
        let mine: Vec<&Proposal> = proposals
            .iter()
            .filter(|p| p.info.tracking_number == *number)
            .collect();
        let Some(latest) = mine.first() else {
            if let Some(id) = existing {
                // Imported legacy rows belong to their original subsystem and
                // remain intact; only rows created by this projection are retired.
                // Deleting a row takes its order links with it (the trigger in
                // migrate.rs); a kept legacy row loses only the agent's links.
                conn.execute("DELETE FROM shipments WHERE account_id=?1 AND id=?2 AND EXISTS(
                    SELECT 1 FROM agent_delivery_projections p WHERE p.account_id=?1 AND p.shipment_id=?2)", params![account,id])?;
                conn.execute(
                    "DELETE FROM agent_delivery_projections WHERE account_id=?1 AND shipment_id=?2",
                    params![account, id],
                )?;
                rebuild_order_links(conn, account, id, &[], None)?;
                // A kept legacy row the agent had renamed loses that name.
                record_identity(conn, account, id, false, &[], Utc::now())?;
            }
            continue;
        };
        let (message, received, info) = (&latest.message, &latest.received, &latest.info);
        let carrier_status: Option<String> = conn.query_row(
            "SELECT status FROM shipments WHERE account_id=?1 AND tracking_number=?2 AND carrier_status_raw IS NOT NULL",
            params![account,number], |row|row.get(0),
        ).optional()?;
        let id = if let Some(id) = existing {
            id
        } else {
            // Reuse insertion and provenance plumbing only. Existing rows do
            // not pass through the legacy name or monotonic-status merge.
            let id =
                super::specialists::upsert_shipment_conn(conn, account, *message, info, *received)?;
            conn.execute(
                "INSERT INTO agent_delivery_projections(account_id,shipment_id) VALUES(?1,?2)",
                params![account, id],
            )?;
            id
        };
        let managed: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM agent_delivery_projections WHERE account_id=?1 AND shipment_id=?2)",params![account,id],|r|r.get(0))?;
        let since: DateTime<Utc> = conn.query_row(
            "SELECT last_update FROM shipments WHERE account_id=?1 AND id=?2",
            params![account, id],
            |r| dt(r, 0),
        )?;
        // WHAT THE MAIL SAYS THE STATUS IS differs by ownership, and it is the
        // only thing that does. An OWNED row is a projection of the retained
        // proposals, so the newest one is authoritative and a retraction can
        // walk it back. A LEGACY row is a fact this projection does not own and
        // cannot rebuild, so it takes the no-regress merge, and a delivered
        // package is never walked back by a mail the model misread as "shipped"
        // (a re-ship notice, a survey). Either way the carrier's own word, once
        // it has one, outranks the mail.
        let status = if managed {
            carrier_status.unwrap_or_else(|| info.status.as_str().to_string())
        } else {
            let current: String = conn.query_row(
                "SELECT status FROM shipments WHERE account_id=?1 AND id=?2",
                params![account, id],
                |r| r.get(0),
            )?;
            let current = ShipmentStatus::parse(&current).unwrap_or(ShipmentStatus::Shipped);
            carrier_status.unwrap_or_else(|| {
                ShipmentStatus::merge(current, info.status)
                    .as_str()
                    .to_string()
            })
        };
        record_newer_mail(
            conn, account, id, managed, *message, info, &status, *received,
        )?;
        record_identity(conn, account, id, managed, &mine, since)?;
        let seller = mine
            .iter()
            .find_map(|p| p.merchant.as_deref().map(|m| (m, p.message)));
        rebuild_order_links(conn, account, id, &mine, seller)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> SqliteStore {
        let store = SqliteStore::open_in_memory().unwrap();
        let conn = store.lock().unwrap();
        conn.execute(
            "INSERT INTO accounts(id,email,created_at) VALUES(1,'me@test',?1)",
            [Utc::now().to_rfc3339()],
        )
        .unwrap();
        for id in [1, 2] {
            conn.execute(
                "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body)
                 VALUES(?1,1,?2,?2,'shipping@test','Package',?3,'','')",
                params![id,id.to_string(),(Utc::now()-chrono::Duration::hours(3-id)).to_rfc3339()],
            ).unwrap();
        }
        drop(conn);
        store
    }

    fn delivery(number: &str, status: &str) -> MessageDecision {
        named(number, status, None, None, &[])
    }

    fn named(
        number: &str,
        status: &str,
        item: Option<&str>,
        merchant: Option<&str>,
        refs: &[&str],
    ) -> MessageDecision {
        MessageDecision {
            records: vec![RecordProposal::Delivery {
                carrier: Some("ups".into()),
                tracking_number: Some(number.into()),
                status: status.into(),
                item_name: item.map(str::to_string),
                merchant: merchant.map(str::to_string),
                order_refs: refs.iter().map(|r| r.to_string()).collect(),
                evidence: vec![],
            }],
            ..Default::default()
        }
    }

    /// `(item_name, item_name_source, item_name_msg, order_merchant)` of a row.
    fn identity(
        store: &SqliteStore,
        number: &str,
    ) -> (String, String, Option<i64>, Option<String>) {
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT item_name,item_name_source,item_name_msg,order_merchant
                 FROM shipments WHERE tracking_number=?1",
                [number],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
    }

    /// Every order link, as `(tracking_number, merchant_key, order_key, source)`.
    fn links(store: &SqliteStore) -> Vec<(String, String, String, String)> {
        let conn = store.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT s.tracking_number,l.merchant_key,l.order_key,l.source
                 FROM shipment_order_links l JOIN shipments s ON s.id=l.shipment_id
                 ORDER BY s.tracking_number,l.order_key",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }

    fn link_count(store: &SqliteStore) -> i64 {
        store
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM shipment_order_links", [], |r| {
                r.get(0)
            })
            .unwrap()
    }

    fn add_message(store: &SqliteStore, id: i64, subject: &str, age: chrono::Duration) {
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body)
                 VALUES(?1,1,?2,?2,'orders@billsexhausts.test',?3,?4,'','')",
                params![id, id.to_string(), subject, (Utc::now() - age).to_rfc3339()],
            )
            .unwrap();
    }

    /// EVERY STORED DECISION IS RE-READ. One written before the three new
    /// fields existed must still deserialize, or reconcile fails the whole pass
    /// for every package the account has.
    #[test]
    fn a_stored_decision_without_the_new_fields_still_reconciles() {
        let store = fixture();
        let old = r#"{"kinds":["delivery"],"destinations":[],"summary":"s","reason":"r",
            "auth":{"kinds":[],"evidence":[]},
            "external_access":{"restricted":false,"reason":"r","evidence":[]},
            "attention":{"show_in_fye":false,"state":"informational","actions":[],"summary":"",
                "factors":{"urgency":0,"action_need":0,"personal_relevance":0,"importance":0,"attention_at":null},
                "relevant_message_ids":[],"evidence":[]},
            "records":[{"kind":"delivery","carrier":"ups","tracking_number":"1Z999AA10123456784",
                "status":"shipped","evidence":[]}],
            "related_updates":[],"rule_exceptions":[],
            "notification":{"importance":0,"title":"","body":"","reason":""},"revisit":null}"#;
        let decision: MessageDecision = serde_json::from_str(old).unwrap();
        {
            let mut conn = store.lock().unwrap();
            let tx = conn.transaction().unwrap();
            tx.execute(
                "INSERT INTO agent_message_decisions(account_id,message_id,revision,decision_json,decided_at)
                 VALUES(1,1,1,?1,?2)",
                params![old, Utc::now().to_rfc3339()],
            )
            .unwrap();
            reconcile(&tx, 1, 1, None, &decision).unwrap();
            tx.commit().unwrap();
        }
        // A NEW decision about the same package re-reads the old one too.
        write(
            &store,
            2,
            None,
            &named(
                "1Z999AA10123456784",
                "out_for_delivery",
                Some("Desk Lamp"),
                None,
                &[],
            ),
        );
        let rows = listed(&store);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].item_name, "Desk Lamp");
        assert_eq!(rows[0].status, "out_for_delivery");
    }

    /// THE REAL CASE. Two UPS numbers, one order: the supplier-to-merchant leg,
    /// whose mail never prints the order number (the model found it in the
    /// sender's July thread and cites that message), and the leg to the user.
    /// One card, named, with both rows carrying order 21470.
    #[test]
    fn two_numbers_on_one_order_are_one_named_card() {
        let store = fixture();
        add_message(
            &store,
            3,
            "Austin Racing shipment update",
            chrono::Duration::minutes(50),
        );
        add_message(
            &store,
            4,
            "A shipment from order #21470 is on the way",
            chrono::Duration::minutes(10),
        );
        let inbound = named(
            "1ZW061R3DG21045729",
            "shipped",
            None,
            Some("Bill's Exhausts"),
            &["21470"],
        );
        let outbound = named(
            "1ZB8B2560323528551",
            "shipped",
            Some("Austin Racing DB Killer AUR10"),
            Some("BILLS EXHAUSTS"),
            &["Order #21470"],
        );
        write(&store, 3, None, &inbound);
        write(&store, 4, None, &outbound);
        assert_eq!(
            links(&store),
            vec![
                (
                    "1ZB8B2560323528551".into(),
                    "billsexhausts".into(),
                    "21470".into(),
                    "agent".into()
                ),
                (
                    "1ZW061R3DG21045729".into(),
                    "billsexhausts".into(),
                    "21470".into(),
                    "agent".into()
                ),
            ]
        );
        let cards = listed(&store);
        assert_eq!(cards.len(), 1, "one order, one card");
        let card = &cards[0];
        assert_eq!(
            card.tracking_number, "1ZB8B2560323528551",
            "the newest leg represents"
        );
        assert_eq!(card.item_name, "Austin Racing DB Killer AUR10");
        assert_eq!(card.merchant.as_deref(), Some("BILLS EXHAUSTS"));
        assert_eq!(card.orders.len(), 1);
        assert_eq!(card.orders[0].order_ref, "21470");
        assert_eq!(card.legs.len(), 1);
        assert_eq!(card.legs[0].tracking_number, "1ZW061R3DG21045729");
        // The poller still sees two numbers.
        assert_eq!(
            store
                .list_pollable_shipments(1, Utc::now() - chrono::Duration::days(1), 5)
                .unwrap()
                .len(),
            2
        );

        // Retracting the outbound proposal retires its row AND its links, and
        // the inbound leg stands alone again, still carrying the order.
        write(&store, 4, Some(&outbound), &MessageDecision::default());
        assert_eq!(links(&store).len(), 1);
        let cards = listed(&store);
        assert_eq!(cards.len(), 1);
        assert!(cards[0].legs.is_empty());
        assert_eq!(cards[0].item_name, "", "the inbound mail never named it");
    }

    /// A ref with no merchant is kept and shown but never groups: "#1001" is
    /// a different purchase at every shop.
    #[test]
    fn merchantless_refs_are_stored_but_never_group() {
        let store = fixture();
        write(
            &store,
            1,
            None,
            &named("1Z999AA10123456784", "shipped", None, None, &["1001"]),
        );
        write(
            &store,
            2,
            None,
            &named("1Z999AA10123456785", "shipped", None, None, &["#1001"]),
        );
        assert_eq!(
            links(&store)[0].1,
            "",
            "stored under the empty merchant key"
        );
        let cards = listed(&store);
        assert_eq!(cards.len(), 2);
        assert!(
            cards
                .iter()
                .all(|c| c.orders.len() == 1 && c.legs.is_empty())
        );
    }

    /// A mail that names the order but not the seller, about a box another
    /// mail already attributed: same box, same seller, so the order groups.
    #[test]
    fn a_ref_borrows_the_packages_merchant() {
        let store = fixture();
        write(
            &store,
            1,
            None,
            &named("1Z999AA10123456784", "shipped", None, Some("Bean Co"), &[]),
        );
        write(
            &store,
            2,
            None,
            &named("1Z999AA10123456784", "shipped", None, None, &["7"]),
        );
        assert_eq!(links(&store)[0].1, "beanco");
    }

    /// Only visible rows group: a silent leg does not ride along on the card.
    #[test]
    fn a_hidden_row_never_groups() {
        let store = fixture();
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE messages SET received_at=?1 WHERE id=1",
                [(Utc::now() - chrono::Duration::days(30)).to_rfc3339()],
            )
            .unwrap();
        write(
            &store,
            1,
            None,
            &named(
                "1Z999AA10123456784",
                "shipped",
                None,
                Some("Bean Co"),
                &["7"],
            ),
        );
        write(
            &store,
            2,
            None,
            &named(
                "1Z999AA10123456785",
                "shipped",
                None,
                Some("Bean Co"),
                &["7"],
            ),
        );
        assert!(
            links(&store).iter().all(|l| l.1 == "beanco"),
            "both carry a groupable key, so only the hide keeps them apart"
        );
        let cards = listed(&store);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].tracking_number, "1Z999AA10123456785");
        assert!(cards[0].legs.is_empty(), "the silent leg stays hidden");
    }

    /// A retracted proposal takes its agent links with it, on a legacy row
    /// too, and a legacy link is never touched.
    #[test]
    fn retraction_removes_agent_links_and_keeps_legacy_ones() {
        let store = fixture();
        let id = legacy_row(&store, "1Z999AA10123456784", 3);
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO shipment_order_links
                     (account_id,shipment_id,merchant_key,order_key,merchant,order_ref,source)
                 VALUES(1,?1,'shopa','1042','Shop A','1042','legacy')",
                [id],
            )
            .unwrap();
        let update = named(
            "1Z999AA10123456784",
            "shipped",
            None,
            Some("Shop B"),
            &["55"],
        );
        write(&store, 2, None, &update);
        assert_eq!(link_count(&store), 2);
        write(&store, 2, Some(&update), &MessageDecision::default());
        assert_eq!(
            links(&store),
            vec![(
                "1Z999AA10123456784".into(),
                "shopa".into(),
                "1042".into(),
                "legacy".into()
            )]
        );
    }

    /// A legacy row's name is the old subsystem's; only mail NEWER than the
    /// row replaces it, and then it is the agent's.
    #[test]
    fn a_legacy_name_is_replaced_only_by_newer_mail() {
        let store = fixture();
        legacy_row(&store, "1Z999AA10123456784", 1);
        // The row is a day old; message 1 goes further back than that.
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE messages SET received_at=?1 WHERE id=1",
                [(Utc::now() - chrono::Duration::days(2)).to_rfc3339()],
            )
            .unwrap();
        write(
            &store,
            1,
            None,
            &named(
                "1Z999AA10123456784",
                "shipped",
                Some("Old Desk Lamp"),
                Some("Lamps Co"),
                &[],
            ),
        );
        let (name, source, _, merchant) = identity(&store, "1Z999AA10123456784");
        assert_eq!(
            (name.as_str(), source.as_str()),
            ("Lamp", "regex"),
            "older mail renames nothing"
        );
        assert_eq!(merchant, None);
        write(
            &store,
            2,
            None,
            &named(
                "1Z999AA10123456784",
                "shipped",
                Some("Brass Desk Lamp"),
                Some("Lamps Co"),
                &[],
            ),
        );
        let (name, source, msg, merchant) = identity(&store, "1Z999AA10123456784");
        assert_eq!(
            (name.as_str(), source.as_str(), msg),
            ("Brass Desk Lamp", "agent", Some(2))
        );
        assert_eq!(merchant.as_deref(), Some("Lamps Co"));
    }

    /// A newer proposal that names nothing never erases a name, on an owned row
    /// or a legacy one. Retracting the proposal that named it does.
    #[test]
    fn an_empty_name_never_erases_one() {
        let store = fixture();
        let named_mail = named(
            "1Z999AA10123456784",
            "shipped",
            Some("Espresso Machine"),
            Some("Bean Co"),
            &[],
        );
        write(&store, 1, None, &named_mail);
        write(
            &store,
            2,
            None,
            &delivery("1Z999AA10123456784", "out_for_delivery"),
        );
        let (name, source, msg, merchant) = identity(&store, "1Z999AA10123456784");
        assert_eq!(
            (name.as_str(), source.as_str(), msg),
            ("Espresso Machine", "agent", Some(1))
        );
        assert_eq!(merchant.as_deref(), Some("Bean Co"));
        // The owned row is a projection: the naming proposal gone, the name goes.
        write(&store, 1, Some(&named_mail), &MessageDecision::default());
        let (name, source, msg, merchant) = identity(&store, "1Z999AA10123456784");
        assert_eq!((name.as_str(), source.as_str(), msg), ("", "regex", None));
        assert_eq!(merchant, None);

        // Legacy: newer unnamed mail keeps the legacy name.
        legacy_row(&store, "1Z999AA10123456785", 1);
        write(
            &store,
            2,
            None,
            &delivery("1Z999AA10123456785", "out_for_delivery"),
        );
        assert_eq!(identity(&store, "1Z999AA10123456785").0, "Lamp");
    }

    /// The model's name is laundered before it lands: a status phrase, the
    /// store's own name or the order number is not what is in the box.
    #[test]
    fn a_junk_agent_name_is_not_stored() {
        let store = fixture();
        for (id, junk) in [(1, "Arriving soon"), (2, "Bill's Exhausts")] {
            write(
                &store,
                id,
                None,
                &named(
                    "1Z999AA10123456784",
                    "shipped",
                    Some(junk),
                    Some("Bill's Exhausts"),
                    &["21470"],
                ),
            );
            assert_eq!(identity(&store, "1Z999AA10123456784").0, "", "{junk}");
        }
    }

    /// ONE MERCHANT PER BOX. Two mails about one box, the older naming Shop A
    /// order 1 and the newer Shop B order 2: every ref lands under the box's
    /// one seller (the newest), so the box cannot be the bridge that chains a
    /// Shop A order into a Shop B card.
    #[test]
    fn one_box_files_every_ref_under_its_one_seller() {
        let store = fixture();
        add_message(&store, 3, "Order update", chrono::Duration::minutes(5));
        write(
            &store,
            1,
            None,
            &named(
                "1Z999AA10123456784",
                "shipped",
                None,
                Some("Shop A"),
                &["1"],
            ),
        );
        write(
            &store,
            2,
            None,
            &named(
                "1Z999AA10123456784",
                "shipped",
                None,
                Some("Shop B"),
                &["2"],
            ),
        );
        write(
            &store,
            3,
            None,
            &named(
                "1Z999AA10123456785",
                "shipped",
                None,
                Some("Shop A"),
                &["1"],
            ),
        );
        let box_links: Vec<_> = links(&store)
            .into_iter()
            .filter(|l| l.0 == "1Z999AA10123456784")
            .map(|l| (l.1, l.2))
            .collect();
        assert_eq!(
            box_links,
            vec![("shopb".into(), "1".into()), ("shopb".into(), "2".into())]
        );
        assert_eq!(
            listed(&store).len(),
            2,
            "Shop A's other box stays its own card"
        );
    }

    /// A LEGACY ROW THE AGENT RENAMED, THEN RETRACTED. The legacy name is gone
    /// for good once the agent replaced it, so when the only proposal naming
    /// the package is retracted the row goes back to unnamed, with no
    /// provenance pointing at the retracted mail and no agent merchant.
    #[test]
    fn a_retracted_rename_of_a_legacy_row_clears_the_agents_name() {
        let store = fixture();
        legacy_row(&store, "1Z999AA10123456784", 1);
        let rename = named(
            "1Z999AA10123456784",
            "shipped",
            Some("Brass Desk Lamp"),
            Some("Lamps Co"),
            &[],
        );
        write(&store, 2, None, &rename);
        assert_eq!(identity(&store, "1Z999AA10123456784").0, "Brass Desk Lamp");
        write(&store, 2, Some(&rename), &MessageDecision::default());
        let (name, source, msg, merchant) = identity(&store, "1Z999AA10123456784");
        assert_eq!((name.as_str(), source.as_str(), msg), ("", "regex", None));
        assert_eq!(merchant, None);
        let merchant_msg: Option<i64> = store
            .lock()
            .unwrap()
            .query_row(
                "SELECT order_merchant_msg FROM shipments WHERE tracking_number='1Z999AA10123456784'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(merchant_msg, None);
        assert!(
            listed(&store).len() == 1,
            "a legacy row is kept, only the agent's words go"
        );
    }

    /// The same, when an OLDER proposal still names it: once the agent owns
    /// the name, the newest remaining proposal naming the package names it,
    /// even though that mail is older than the row.
    #[test]
    fn a_retracted_rename_falls_back_to_a_remaining_agent_name() {
        let store = fixture();
        legacy_row(&store, "1Z999AA10123456784", 1);
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE messages SET received_at=?1 WHERE id=1",
                [(Utc::now() - chrono::Duration::days(2)).to_rfc3339()],
            )
            .unwrap();
        let old = named(
            "1Z999AA10123456784",
            "shipped",
            Some("Desk Lamp"),
            Some("Lamps Co"),
            &[],
        );
        let rename = named(
            "1Z999AA10123456784",
            "shipped",
            Some("Brass Desk Lamp"),
            Some("Lamp Shop"),
            &[],
        );
        write(&store, 1, None, &old);
        write(&store, 2, None, &rename);
        write(&store, 2, Some(&rename), &MessageDecision::default());
        let (name, source, msg, merchant) = identity(&store, "1Z999AA10123456784");
        assert_eq!(
            (name.as_str(), source.as_str(), msg),
            ("Desk Lamp", "agent", Some(1))
        );
        assert_eq!(merchant.as_deref(), Some("Lamps Co"));
    }

    /// THE REPRESENTATIVE IS THE BOX STILL COMING. The newer box was delivered
    /// two days ago and the older one is still in transit: one card, in
    /// transit, on the listing the client reads (delivered included) and on
    /// the one without delivered alike.
    #[test]
    fn a_delivered_leg_never_represents_an_order_still_in_transit() {
        let store = fixture();
        add_message(&store, 3, "Shipped", chrono::Duration::days(3));
        add_message(&store, 4, "Delivered", chrono::Duration::days(2));
        write(
            &store,
            3,
            None,
            &named(
                "1Z999AA10123456784",
                "shipped",
                None,
                Some("Bean Co"),
                &["5"],
            ),
        );
        write(
            &store,
            4,
            None,
            &named(
                "1Z999AA10123456785",
                "delivered",
                None,
                Some("Bean Co"),
                &["5"],
            ),
        );
        let policy = crate::config::ShipmentListPolicy::default();
        for include_delivered in [true, false] {
            let cards = store.list_shipments(1, include_delivered, policy).unwrap();
            assert_eq!(cards.len(), 1, "include_delivered={include_delivered}");
            assert_eq!(cards[0].tracking_number, "1Z999AA10123456784");
            assert_eq!(cards[0].status, "shipped");
            assert_eq!(
                cards[0].legs.len(),
                1,
                "the delivered box rides along (include_delivered={include_delivered})"
            );
        }
    }

    /// CLEARING A CARD CLEARS THE PURCHASE. The client posts the one id it
    /// shows; every package on that card is cleared, whichever id it posts.
    #[test]
    fn clearing_any_package_of_a_card_clears_the_card() {
        let policy = crate::config::ShipmentListPolicy::default();
        for tap_leg in [false, true] {
            let store = fixture();
            write(
                &store,
                1,
                None,
                &named(
                    "1Z999AA10123456784",
                    "shipped",
                    None,
                    Some("Bean Co"),
                    &["5"],
                ),
            );
            write(
                &store,
                2,
                None,
                &named(
                    "1Z999AA10123456785",
                    "shipped",
                    None,
                    Some("Bean Co"),
                    &["5"],
                ),
            );
            let card = listed(&store).remove(0);
            assert_eq!(card.legs.len(), 1);
            let tapped = if tap_leg { card.legs[0].id } else { card.id };
            assert!(store.clear_shipment(1, tapped, Utc::now(), policy).unwrap());
            assert!(
                store.list_shipments(1, true, policy).unwrap().is_empty(),
                "tap_leg={tap_leg}"
            );
        }
    }

    fn write(
        store: &SqliteStore,
        id: i64,
        previous: Option<&MessageDecision>,
        current: &MessageDecision,
    ) {
        let mut conn = store.lock().unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO agent_message_decisions(account_id,message_id,revision,decision_json,decided_at)
             VALUES(1,?1,1,?2,?3) ON CONFLICT(account_id,message_id) DO UPDATE SET decision_json=excluded.decision_json",
            params![id,serde_json::to_string(current).unwrap(),Utc::now().to_rfc3339()],
        ).unwrap();
        reconcile(&tx, 1, id, previous, current).unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn normalized_tracking_coalesces_polling_and_retracts_across_source_spellings() {
        let store = fixture();
        let spaced = delivery(" 1z999 aa10 1234 56784 ", "shipped");
        write(&store, 1, None, &spaced);
        let pollable = store
            .list_pollable_shipments(1, Utc::now() - chrono::Duration::days(1), 5)
            .unwrap();
        assert_eq!(pollable.len(), 1);
        assert_eq!(pollable[0].tracking_number, "1Z999AA10123456784");
        assert!(
            pollable[0]
                .tracking_url
                .as_deref()
                .unwrap()
                .ends_with("1Z999AA10123456784")
        );
        let compact = delivery("1Z999AA10123456784", "out_for_delivery");
        write(&store, 2, None, &compact);
        let rows = store
            .list_pollable_shipments(1, Utc::now() - chrono::Duration::days(1), 5)
            .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "equivalent spellings identify one polling row"
        );
        assert_eq!(rows[0].id, pollable[0].id);
        assert_eq!(rows[0].status, "out_for_delivery");
        write(&store, 2, Some(&compact), &MessageDecision::default());
        let remaining = store
            .list_pollable_shipments(1, Utc::now() - chrono::Duration::days(1), 5)
            .unwrap();
        assert_eq!(
            remaining.len(),
            1,
            "spaced source remains after compact source retracts"
        );
        assert_eq!(remaining[0].status, "shipped");
        write(&store, 1, Some(&spaced), &MessageDecision::default());
        assert!(
            store
                .list_pollable_shipments(1, Utc::now() - chrono::Duration::days(1), 5)
                .unwrap()
                .is_empty()
        );
        let marker_count: i64 = store
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM agent_delivery_projections", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            marker_count, 0,
            "normalized final retraction removes ownership marker"
        );
    }

    /// A LONG DELIVERED HISTORY IS NEVER LOADED. Two thousand packages that
    /// landed two months ago, every one linked to the same order as today's
    /// box, are hidden for certain by the silence rule, so the listing, the
    /// agent door's feed, its per-number check and a clear all read only the
    /// rows that could show: today's box, and an old one a carrier answered
    /// for yesterday (vouched, so still listed). Had the old rows been read
    /// and wrongly kept, they would be legs on today's card.
    #[test]
    fn a_long_delivered_history_is_never_loaded() {
        use super::super::specialists::{SHIPMENT_LINKS_READ, SHIPMENT_ROWS_READ};
        use crate::store::agent_triage::AgentTriageStore;
        let rows_read = || SHIPMENT_ROWS_READ.with(|n| n.replace(0));
        let links_read = || SHIPMENT_LINKS_READ.with(|n| n.replace(0));
        let store = fixture();
        let policy = crate::config::ShipmentListPolicy::default();
        let silence = policy.silence(Utc::now());
        assert!(silence.is_some(), "the default policy goes silent");
        write(
            &store,
            1,
            None,
            &named(
                "1Z999AA10123456784",
                "shipped",
                None,
                Some("Shop A"),
                &["1042"],
            ),
        );
        {
            let mut conn = store.lock().unwrap();
            let tx = conn.transaction().unwrap();
            let old = (Utc::now() - chrono::Duration::days(60)).to_rfc3339();
            for i in 0..2000 {
                tx.execute(
                    "INSERT INTO shipments(account_id,tracking_number,carrier,item_name,status,
                         first_seen,last_update,delivered_at)
                     VALUES(1,?1,'ups','','delivered',?2,?2,?2)",
                    params![format!("OLD{i:06}"), old],
                )
                .unwrap();
                tx.execute(
                    "INSERT INTO shipment_order_links(account_id,shipment_id,merchant_key,
                         order_key,merchant,order_ref,source)
                     VALUES(1,?1,'shopa','1042','Shop A','1042','legacy')",
                    [tx.last_insert_rowid()],
                )
                .unwrap();
            }
            tx.execute(
                "INSERT INTO shipments(account_id,tracking_number,carrier,item_name,status,
                     first_seen,last_update,delivered_at,last_answered_at)
                 VALUES(1,'VOUCHED','ups','','delivered',?1,?1,?1,?2)",
                params![old, (Utc::now() - chrono::Duration::days(1)).to_rfc3339()],
            )
            .unwrap();
            tx.commit().unwrap();
        }
        rows_read();
        links_read();

        let cards = store.list_shipments(1, true, policy).unwrap();
        let mut numbers: Vec<_> = cards.iter().map(|c| c.tracking_number.as_str()).collect();
        numbers.sort();
        assert_eq!(numbers, ["1Z999AA10123456784", "VOUCHED"]);
        assert!(
            cards.iter().all(|c| c.legs.is_empty()),
            "no old box is a leg"
        );
        assert_eq!(rows_read(), 2, "only the rows that could show are read");
        assert_eq!(links_read(), 1, "only the listed rows' links are read");

        store.external_shipments_within(1, silence).unwrap();
        assert_eq!(rows_read(), 2, "the agent door's feed is cut the same way");
        assert!(
            store
                .agent_shipment_is_hidden(1, "OLD000007", silence)
                .unwrap(),
            "a record whose row was cut is hidden with it"
        );
        assert!(
            !store
                .agent_shipment_is_hidden(1, "VOUCHED", silence)
                .unwrap()
        );
        assert!(
            !store
                .agent_shipment_is_hidden(1, "OLD000007", None)
                .unwrap(),
            "with the window off, nothing is silent"
        );

        let fresh = cards
            .iter()
            .find(|c| c.tracking_number == "1Z999AA10123456784")
            .unwrap();
        assert!(
            store
                .clear_shipment(1, fresh.id, Utc::now(), policy)
                .unwrap()
        );
        assert_eq!(rows_read(), 2, "a clear reads no history either");
        assert_eq!(store.list_shipments(1, true, policy).unwrap().len(), 1);
    }

    /// A row written before the agent owned deliveries, `age_days` silent.
    fn legacy_row(store: &SqliteStore, number: &str, age_days: i64) -> i64 {
        let at = (Utc::now() - chrono::Duration::days(age_days)).to_rfc3339();
        let conn = store.lock().unwrap();
        conn.execute(
            "INSERT INTO shipments(account_id,tracking_number,carrier,item_name,status,first_seen,last_update)
             VALUES(1,?1,'ups','Lamp','shipped',?2,?2)",
            params![number, at],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn listed(store: &SqliteStore) -> Vec<crate::types::Shipment> {
        let policy = crate::config::ShipmentListPolicy::default();
        assert!(
            policy.stale_after_days > 0,
            "the default policy goes silent"
        );
        store.list_shipments(1, false, policy).unwrap()
    }

    /// THE PRODUCTION REVIVAL PATH. `reconcile` is the only mail-driven writer
    /// left, so a row the listing hid as silent comes back through here or not
    /// at all, and a legacy row is in no projection to be updated through.
    #[test]
    fn newer_mail_returns_a_silent_legacy_row_without_adopting_it() {
        let store = fixture();
        let id = legacy_row(&store, "1Z999AA10123456784", 30);
        assert!(listed(&store).is_empty(), "silent for 30 days: hidden");

        let update = delivery("1Z999AA10123456784", "out_for_delivery");
        write(&store, 2, None, &update);
        let rows = listed(&store);
        assert_eq!(rows.len(), 1, "the update email brought it back");
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].status, "out_for_delivery");
        assert_eq!(rows[0].item_name, "Lamp", "the legacy name is untouched");

        // Retracting that mail must not delete a row this projection never owned.
        write(&store, 2, Some(&update), &MessageDecision::default());
        let kept: i64 = store
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM shipments WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(kept, 1, "legacy rows are never retired from here");
    }

    /// Re-triaging OLD mail is not news: it must not resurrect the row, or a
    /// re-triage pass would refill the list with every dead package at once.
    #[test]
    fn re_deciding_old_mail_does_not_return_a_silent_legacy_row() {
        let store = fixture();
        legacy_row(&store, "1Z999AA10123456784", 30);
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE messages SET received_at=?1 WHERE id=1",
                [(Utc::now() - chrono::Duration::days(40)).to_rfc3339()],
            )
            .unwrap();
        write(
            &store,
            1,
            None,
            &delivery("1Z999AA10123456784", "delivered"),
        );
        assert!(listed(&store).is_empty());
        let status: String = store
            .lock()
            .unwrap()
            .query_row("SELECT status FROM shipments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            status, "shipped",
            "mail older than the row rewrites nothing"
        );
    }

    fn poll_failures(store: &SqliteStore, id: i64) -> u32 {
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT poll_failures FROM shipments WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn retire(store: &SqliteStore, id: i64) {
        store
            .lock()
            .unwrap()
            .execute("UPDATE shipments SET poll_failures=5 WHERE id=?1", [id])
            .unwrap();
    }

    fn pollable(store: &SqliteStore, id: i64) -> bool {
        store
            .list_pollable_shipments(1, Utc::now() - chrono::Duration::days(45), 5)
            .unwrap()
            .iter()
            .any(|s| s.id == id)
    }

    /// A retired number is polled again once newer mail says it is real. Only a
    /// successful poll used to reset the counter, and a retired row never gets
    /// one, so retirement had quietly become permanent.
    #[test]
    fn newer_mail_un_retires_a_row_and_older_mail_does_not() {
        let store = fixture();
        // Owned row, minted from 30-day-old mail, then retired.
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE messages SET received_at=?1 WHERE id=1",
                [(Utc::now() - chrono::Duration::days(30)).to_rfc3339()],
            )
            .unwrap();
        write(&store, 1, None, &delivery("1Z999AA10123456784", "shipped"));
        let owned: i64 = store
            .lock()
            .unwrap()
            .query_row("SELECT id FROM shipments", [], |r| r.get(0))
            .unwrap();
        retire(&store, owned);
        assert!(!pollable(&store, owned), "retired: out of the poll queue");

        // Re-deciding the SAME mail is not news: still retired.
        write(&store, 1, None, &delivery("1Z999AA10123456784", "shipped"));
        assert_eq!(poll_failures(&store, owned), 5);

        // Newer mail is.
        write(
            &store,
            2,
            None,
            &delivery("1Z999AA10123456784", "out_for_delivery"),
        );
        assert_eq!(poll_failures(&store, owned), 0);
        assert!(pollable(&store, owned), "back in the poll queue");

        // The same for a legacy row.
        let legacy = legacy_row(&store, "1Z999AA10123456785", 20);
        retire(&store, legacy);
        write(
            &store,
            2,
            None,
            &delivery("1Z999AA10123456785", "out_for_delivery"),
        );
        assert_eq!(poll_failures(&store, legacy), 0);
        assert!(pollable(&store, legacy));
    }

    fn row_facts(store: &SqliteStore, id: i64) -> (String, String, Option<String>, Option<String>) {
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT status,carrier,delivered_at,tracking_url FROM shipments WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
    }

    /// A DELIVERED LEGACY ROW IS NEVER WALKED BACK. The model misreads a re-ship
    /// notice or a survey as "shipped": the row keeps its terminal status and
    /// its delivery time, and does not re-enter the poll queue. It is still
    /// news, so it still returns to the list and still un-retires.
    #[test]
    fn newer_mail_cannot_walk_a_delivered_legacy_row_back() {
        let store = fixture();
        let id = legacy_row(&store, "1Z999AA10123456784", 30);
        let landed = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE shipments SET status='delivered',delivered_at=?1 WHERE id=?2",
                params![landed, id],
            )
            .unwrap();
        write(&store, 2, None, &delivery("1Z999AA10123456784", "shipped"));
        let (status, _, delivered_at, _) = row_facts(&store, id);
        assert_eq!(
            status, "delivered",
            "terminal status survives a misread mail"
        );
        assert_eq!(delivered_at.as_deref(), Some(landed.as_str()));
        assert!(!pollable(&store, id), "a delivered package is not polled");
        assert_eq!(
            listed(&store).len(),
            0,
            "and en-route listings still exclude it"
        );
    }

    /// Newer mail that names a different carrier for the number re-routes the
    /// un-retired poll: five USPS rejections were USPS being asked about a
    /// FedEx label, and asking USPS five more times helps nobody.
    #[test]
    fn newer_mail_re_routes_a_legacy_row_to_the_carrier_it_names() {
        let store = fixture();
        let id = legacy_row(&store, "123456789012", 20);
        store
            .lock()
            .unwrap()
            .execute("UPDATE shipments SET carrier='usps' WHERE id=?1", [id])
            .unwrap();
        retire(&store, id);
        let mut mail = delivery("123456789012", "out_for_delivery");
        if let RecordProposal::Delivery { carrier, .. } = &mut mail.records[0] {
            *carrier = Some("fedex".into());
        }
        write(&store, 2, None, &mail);
        let (status, carrier, _, url) = row_facts(&store, id);
        assert_eq!(
            (status.as_str(), carrier.as_str()),
            ("out_for_delivery", "fedex")
        );
        assert!(
            url.unwrap().contains("fedex"),
            "the tracking link follows the carrier"
        );
        assert_eq!(poll_failures(&store, id), 0);
        assert!(pollable(&store, id));
    }

    /// The same revival for a row this projection DOES own.
    #[test]
    fn newer_mail_returns_a_silent_managed_row() {
        let store = fixture();
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE messages SET received_at=?1 WHERE id=1",
                [(Utc::now() - chrono::Duration::days(30)).to_rfc3339()],
            )
            .unwrap();
        write(&store, 1, None, &delivery("1Z999AA10123456784", "shipped"));
        assert!(
            listed(&store).is_empty(),
            "minted from 30-day-old mail: hidden"
        );
        write(
            &store,
            2,
            None,
            &delivery("1Z999AA10123456784", "out_for_delivery"),
        );
        assert_eq!(listed(&store).len(), 1);
    }

    #[test]
    fn invalid_tracking_stays_a_canonical_fact_without_entering_carrier_polling() {
        let store = fixture();
        for (id, number) in [(1, "https://example.test/order/1234567890"), (2, "1234")] {
            write(&store, id, None, &delivery(number, "shipped"));
        }
        let conn = store.lock().unwrap();
        let records: i64 = conn
            .query_row("SELECT COUNT(*) FROM agent_message_decisions", [], |r| {
                r.get(0)
            })
            .unwrap();
        let shipments: i64 = conn
            .query_row("SELECT COUNT(*) FROM shipments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(records, 2);
        assert_eq!(shipments, 0);
    }

    // ---- Amazon boxes: trackless records keyed by Amazon's shipment id ----

    fn tracker(order: &str, shipment: &str) -> String {
        format!(
            "Order #\n{order}\n\nTrack package\nhttps://www.amazon.com/progress-tracker/package?_encoding=UTF8&orderId={order}&packageIndex=0&shipmentId={shipment}&vt=NOTIFICATIONS\n"
        )
    }

    fn add_amazon(
        store: &SqliteStore,
        id: i64,
        from: &str,
        auth: Option<bool>,
        subject: &str,
        body: &str,
        age: chrono::Duration,
    ) {
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body,auth_pass)
                 VALUES(?1,1,?2,?2,?3,?4,?5,'',?6,?7)",
                params![id, id.to_string(), from, subject, (Utc::now() - age).to_rfc3339(), body, auth],
            )
            .unwrap();
    }

    /// What the agent writes for Amazon mail: no number, and as often as not
    /// no carrier either.
    fn trackless(
        status: &str,
        carrier: Option<&str>,
        item: &str,
        refs: &[&str],
    ) -> MessageDecision {
        MessageDecision {
            records: vec![RecordProposal::Delivery {
                carrier: carrier.map(str::to_string),
                tracking_number: None,
                status: status.into(),
                item_name: Some(item.into()),
                merchant: None,
                order_refs: refs.iter().map(|r| r.to_string()).collect(),
                evidence: vec![],
            }],
            ..Default::default()
        }
    }

    const ORDER: &str = "113-1810790-1724205";
    const KEY: &str = "amazon:113-1810790-1724205:N4RLGpMz1";

    /// THE BUG. Shipped, then Delivered, for one box, neither printing a
    /// tracking number: one Amazon row that ends delivered, named, sold by
    /// Amazon, carrying its order, and clickable through to Amazon's tracker.
    #[test]
    fn an_amazon_box_is_one_row_from_shipped_to_delivered() {
        let store = fixture();
        let body = tracker(ORDER, "N4RLGpMz1");
        add_amazon(
            &store,
            10,
            "shipment-tracking@amazon.com",
            Some(true),
            "Shipped: \"Wire Brushes\"",
            &body,
            chrono::Duration::hours(5),
        );
        add_amazon(
            &store,
            11,
            "order-update@amazon.com",
            Some(true),
            "Delivered: \"Wire Brushes\"",
            &body,
            chrono::Duration::hours(1),
        );
        write(
            &store,
            10,
            None,
            &trackless("shipped", None, "Wire Brushes", &[ORDER]),
        );
        let rows = store.list_shipments(1, true, Default::default()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tracking_number, KEY);
        assert_eq!(rows[0].carrier, "amazon", "a null carrier becomes Amazon's");
        assert_eq!(rows[0].status, "shipped");

        write(
            &store,
            11,
            None,
            &trackless("delivered", Some("unknown"), "Wire Brushes", &[]),
        );
        let rows = store.list_shipments(1, true, Default::default()).unwrap();
        assert_eq!(rows.len(), 1, "the delivered mail lands on the same box");
        let row = &rows[0];
        assert_eq!(row.status, "delivered");
        assert_eq!(row.item_name, "Wire Brushes");
        assert_eq!(row.merchant.as_deref(), Some("Amazon"));
        assert_eq!(
            row.tracking_url.as_deref(),
            Some(
                "https://www.amazon.com/progress-tracker/package?orderId=113-1810790-1724205&shipmentId=N4RLGpMz1"
            )
        );
        assert!(
            row.orders.iter().any(|o| o.order_ref == ORDER),
            "the order comes from the tracker link even when the record names none"
        );
        // Never polled: Amazon has no carrier API.
        let pollable = store
            .list_pollable_shipments(1, Utc::now() - chrono::Duration::days(30), 5)
            .unwrap();
        assert!(pollable.is_empty());
    }

    /// Two boxes of one order (the Charmin and the hair dye) are two rows
    /// with two ids, folded into one card by the order they share.
    #[test]
    fn two_amazon_boxes_of_one_order_are_two_rows_one_card() {
        let store = fixture();
        let order = "113-0712375-7928267";
        add_amazon(
            &store,
            10,
            "shipment-tracking@amazon.com",
            Some(true),
            "Shipped: Charmin",
            &tracker(order, "Nh06jx4Hr"),
            chrono::Duration::hours(5),
        );
        add_amazon(
            &store,
            11,
            "shipment-tracking@amazon.com",
            Some(true),
            "Shipped: Pulp Riot",
            &tracker(order, "NHlzjr4sr"),
            chrono::Duration::hours(4),
        );
        write(
            &store,
            10,
            None,
            &trackless("shipped", Some("amazon"), "Charmin", &[order]),
        );
        write(
            &store,
            11,
            None,
            &trackless("shipped", Some("amazon"), "Pulp Riot", &[order]),
        );
        let count: i64 = store
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM shipments WHERE carrier='amazon'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
        let cards = listed(&store);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].legs.len(), 1);
    }

    /// A look-alike, or mail that did not authenticate as amazon.com, mints
    /// nothing however Amazon-shaped its body is.
    #[test]
    fn unauthenticated_amazon_lookalikes_mint_no_box() {
        let store = fixture();
        let body = tracker(ORDER, "N4RLGpMz1");
        add_amazon(
            &store,
            10,
            "shipment-tracking@amazon.com",
            None,
            "Shipped",
            &body,
            chrono::Duration::hours(2),
        );
        add_amazon(
            &store,
            11,
            "shipment-tracking@amazon.com.evil.test",
            Some(true),
            "Shipped",
            &body,
            chrono::Duration::hours(1),
        );
        write(
            &store,
            10,
            None,
            &trackless("shipped", Some("amazon"), "Lure", &[]),
        );
        write(
            &store,
            11,
            None,
            &trackless("shipped", Some("amazon"), "Lure", &[]),
        );
        assert!(
            store
                .list_shipments(1, true, Default::default())
                .unwrap()
                .is_empty()
        );
    }

    /// Retracting the only decision about a box retires its row, with the
    /// box key worked out from the message, since the record holds none.
    #[test]
    fn retracting_an_amazon_decision_retires_its_box() {
        let store = fixture();
        add_amazon(
            &store,
            10,
            "shipment-tracking@amazon.com",
            Some(true),
            "Shipped",
            &tracker(ORDER, "N4RLGpMz1"),
            chrono::Duration::hours(1),
        );
        let decision = trackless("shipped", None, "Wire Brushes", &[]);
        write(&store, 10, None, &decision);
        assert_eq!(listed(&store).len(), 1);
        write(&store, 10, Some(&decision), &MessageDecision::default());
        assert!(
            store
                .list_shipments(1, true, Default::default())
                .unwrap()
                .is_empty()
        );
    }

    /// Mail whose tracker link is only in the HTML part (no plain-text part)
    /// still keys its box.
    #[test]
    fn a_tracker_only_in_the_html_part_still_keys_the_box() {
        let store = fixture();
        add_amazon(
            &store,
            10,
            "shipment-tracking@amazon.com",
            Some(true),
            "Shipped",
            "",
            chrono::Duration::hours(1),
        );
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE messages SET body_html=?1 WHERE id=10",
                [format!(
                    "<a href=\"https://www.amazon.com/progress-tracker/package?orderId={ORDER}&amp;shipmentId=N4RLGpMz1\">Track</a>"
                )],
            )
            .unwrap();
        write(
            &store,
            10,
            None,
            &trackless("shipped", None, "Wire Brushes", &[]),
        );
        let rows = listed(&store);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tracking_number, KEY);
    }

    /// THE BACKFILL. A decision stored before box keys existed has no row;
    /// the one-time pass projects it without a model call, and a second pass
    /// changes nothing.
    #[test]
    fn the_backfill_projects_stored_amazon_decisions() {
        let store = fixture();
        add_amazon(
            &store,
            10,
            "order-update@amazon.com",
            Some(true),
            "Delivered",
            &tracker(ORDER, "N4RLGpMz1"),
            chrono::Duration::hours(1),
        );
        let decision = trackless("delivered", Some("amazon"), "Wire Brushes", &[]);
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO agent_message_decisions(account_id,message_id,revision,decision_json,decided_at)
                 VALUES(1,10,1,?1,?2)",
                params![serde_json::to_string(&decision).unwrap(), Utc::now().to_rfc3339()],
            )
            .unwrap();
        assert!(
            store
                .list_shipments(1, true, Default::default())
                .unwrap()
                .is_empty()
        );
        for _ in 0..2 {
            let conn = store.lock().unwrap();
            assert_eq!(project_amazon_boxes(&conn, 1).unwrap(), 1);
        }
        let rows = store.list_shipments(1, true, Default::default()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tracking_number, KEY);
        assert_eq!(rows[0].status, "delivered");
    }

    /// A real carrier number on Amazon mail still wins: it can be polled.
    #[test]
    fn a_carrier_number_on_amazon_mail_keeps_its_number() {
        let store = fixture();
        add_amazon(
            &store,
            10,
            "shipment-tracking@amazon.com",
            Some(true),
            "Shipped",
            &tracker(ORDER, "N4RLGpMz1"),
            chrono::Duration::hours(1),
        );
        write(&store, 10, None, &delivery("1Z999AA10123456784", "shipped"));
        let rows = listed(&store);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tracking_number, "1Z999AA10123456784");
        assert_eq!(rows[0].carrier, "ups");
    }
}
