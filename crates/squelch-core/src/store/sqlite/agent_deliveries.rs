//! Carrier polling projections of explicit agent delivery facts.
//!
//! Canonical decisions own these rows. Retraction rebuilds from remaining
//! proposals and removes an agent-created row when its last proposal disappears.
use super::*;
use crate::triage::ShipmentStatus;
use crate::triage::amazon::{self, AmazonPackage};
use crate::triage::decision::{MessageDecision, RecordProposal};
use crate::triage::ebay::{self, EbayPackage};
use crate::triage::extract::shipments::sanitize_tracking_number;
use crate::triage::order_fallback;
use crate::triage::order_link::{order_key, sanitize_order_refs};
use std::collections::{BTreeSet, HashMap};

/// A package a store's own mail identifies when it prints no carrier number:
/// an Amazon box ([`amazon`]) or an eBay purchase ([`ebay`]). Both are read
/// from authenticated mail by code and key rows the poller never sends to a
/// carrier (a key holds a `:`, which no sanitized tracking number can).
#[derive(Debug, Clone)]
enum StorePackage {
    Amazon(AmazonPackage),
    Ebay(EbayPackage),
}

impl StorePackage {
    fn key(&self) -> String {
        match self {
            Self::Amazon(package) => package.key(),
            Self::Ebay(package) => package.key(),
        }
    }

    fn tracking_url(&self) -> String {
        match self {
            Self::Amazon(package) => package.tracking_url(),
            Self::Ebay(package) => package.tracking_url(),
        }
    }

    /// Is `number` the key this package minted (and not a carrier number the
    /// same mail's record happened to carry)?
    fn keys(&self, number: &str) -> bool {
        match self {
            Self::Amazon(_) => amazon::is_package_key(number),
            Self::Ebay(_) => ebay::is_package_key(number),
        }
    }

    /// The seller of record, for a record the model left without one.
    fn merchant(&self) -> &'static str {
        match self {
            Self::Amazon(_) => "Amazon",
            Self::Ebay(_) => "eBay",
        }
    }

    /// The order this package is in, when its key carries one. eBay's order
    /// number is printed, not linked; the model's `order_refs` carry it.
    fn order_id(&self) -> Option<&str> {
        match self {
            Self::Amazon(package) => Some(&package.order_id),
            Self::Ebay(_) => None,
        }
    }

    /// The carrier the row shows. Amazon's box is Amazon's whatever the model
    /// said (it often says null or "unknown" for Amazon Logistics). An eBay
    /// seller ships with a real carrier, which the mail names, so the model's
    /// word stands; without one the row is "unknown", never dropped.
    fn carrier(&self, said: Option<String>) -> String {
        match self {
            Self::Amazon(_) => "amazon".to_string(),
            Self::Ebay(_) => said
                .filter(|c| matches!(c.as_str(), "ups" | "usps" | "fedex" | "dhl"))
                .unwrap_or_else(|| "unknown".to_string()),
        }
    }
}

/// The key one delivery record files under: its tracking number, or, when it
/// has none and its mail is about one store package, that package's key. A
/// model-written number can never collide with a package key: the sanitizer
/// upper-cases and strips everything but letters and digits, and every
/// package key is lower-case-prefixed with a `:`.
fn delivery_key(number: Option<&str>, package: Option<&StorePackage>) -> Option<String> {
    match number.and_then(|n| sanitize_tracking_number(Some(n), None)) {
        Some(number) => Some(number),
        None => package.map(StorePackage::key),
    }
}

/// What one message's mail says about who sent it and which store package
/// it is about. The package is read only for a decision with a delivery
/// record.
#[derive(Default)]
struct MailContext {
    package: Option<StorePackage>,
    /// [`order_fallback::order_scope`]: whose order numbers this mail's are.
    scope: Option<String>,
}

/// The keys one delivery record touches ([`record_keys`]).
struct RecordKeys {
    /// The carrier number or store package key it files under.
    tracked: Option<String>,
    /// Its orders, as `(order row key, the order number as the model wrote
    /// it)`, smallest key first. A TRACKED record COVERS each of them: the
    /// order's own card steps aside for the tracked one. A TRACKLESS record
    /// files under the first one its mail prints ([`order_fallback`]).
    orders: Vec<(String, String)>,
    /// A "tracking number" that was only this record's order number. It keys
    /// nothing now, but a row an older build keyed on it must be rebuilt (and
    /// so retired), or the order would show twice.
    dropped: Option<String>,
}

fn record_keys(
    tracking_number: Option<&str>,
    order_refs: &[String],
    ctx: &MailContext,
) -> RecordKeys {
    let mut refs = sanitize_order_refs(order_refs);
    // An Amazon mail's tracker link names its order whatever number the
    // record carries, so a TBA-numbered box still covers its Ordered card.
    if let Some(order) = ctx.package.as_ref().and_then(StorePackage::order_id)
        && !refs.iter().any(|r| order_key(r) == order_key(order))
    {
        refs.push(order.to_string());
    }
    let mut tracked = delivery_key(tracking_number, ctx.package.as_ref());
    // The model copying the order number into `tracking_number` is not a
    // tracked package: there is nothing to poll, and as a "tracked" record it
    // would cover, and hide, its own order. It is the order, or the box its
    // mail is about. Only a number no carrier's shape vouches for is demoted
    // (a real `1Z…` listed among the refs stays tracked), and only when a key
    // takes its place: the record never ends up keyed by nothing.
    let mut dropped = None;
    if let Some(number) = tracked.as_deref()
        && !number.contains(':')
        && crate::triage::shipment::is_ambiguous_tracking_shape(number)
        && refs.iter().any(|r| order_key(r) == number)
        && (ctx.package.is_some() || ctx.scope.is_some())
    {
        dropped = tracked.take();
        tracked = ctx.package.as_ref().map(StorePackage::key);
    }
    let mut orders: Vec<(String, String)> = match &ctx.scope {
        Some(scope) => refs
            .into_iter()
            .filter_map(|r| {
                let key = order_key(&r);
                order_fallback::usable(&key).then(|| (order_fallback::key(scope, &key), r))
            })
            .collect(),
        None => Vec::new(),
    };
    orders.sort();
    orders.dedup_by(|a, b| a.0 == b.0);
    RecordKeys {
        tracked,
        orders,
        dropped,
    }
}

/// [`record_keys`] for each delivery record of `decision`.
fn decision_keys<'a>(
    decision: &'a MessageDecision,
    ctx: &'a MailContext,
) -> impl Iterator<Item = RecordKeys> + 'a {
    decision
        .records
        .iter()
        .filter_map(move |record| match record {
            RecordProposal::Delivery {
                tracking_number,
                order_refs,
                ..
            } => Some(record_keys(tracking_number.as_deref(), order_refs, ctx)),
            _ => None,
        })
}

/// Every key `decision` touches: each record's tracked key, all its order
/// keys (a trackless record's candidates as well as a tracked one's covers, so
/// a rebuild sees whichever one the mail turns out to print), and a demoted
/// number's old row.
fn tracking_numbers<'a>(
    decision: &'a MessageDecision,
    ctx: &'a MailContext,
) -> impl Iterator<Item = String> + 'a {
    decision_keys(decision, ctx).flat_map(|keys| {
        keys.tracked
            .into_iter()
            .chain(keys.orders.into_iter().map(|(key, _)| key))
            .chain(keys.dropped)
            .collect::<Vec<_>>()
    })
}

fn has_delivery(decision: &MessageDecision) -> bool {
    decision
        .records
        .iter()
        .any(|record| matches!(record, RecordProposal::Delivery { .. }))
}

/// SQL columns that load a message's body only when it could be a store's
/// package mail. The exact tests are the `package_from_mail`s; this only
/// spares reading every other body. Amazon's HTML part loads only when the
/// plain-text part has no tracker link: it is many times the size, and this
/// runs on every rebuild. eBay's plain text carries no links at all, and the
/// same address sends its promotions, so its HTML loads only for mail whose
/// stored decision holds a delivery record.
const STORE_BODY_COLUMNS: &str = "m.from_addr,m.auth_pass,
    CASE WHEN lower(m.from_addr) LIKE '%amazon.com%' THEN m.body END,
    CASE WHEN lower(m.from_addr) LIKE '%amazon.com%'
          AND coalesce(m.body,'') NOT LIKE '%progress-tracker%' THEN m.body_html
         WHEN lower(m.from_addr) LIKE '%@ebay.com' AND EXISTS(
              SELECT 1 FROM agent_message_decisions dd
              WHERE dd.account_id=m.account_id AND dd.message_id=m.id
                AND dd.decision_json LIKE '%\"kind\":\"delivery\"%') THEN m.body_html END";

fn package_from_row(
    from_addr: &str,
    auth_pass: Option<bool>,
    body: Option<String>,
    body_html: Option<String>,
) -> Option<StorePackage> {
    let (body, html) = (
        body.as_deref().unwrap_or(""),
        body_html.as_deref().unwrap_or(""),
    );
    amazon::package_from_mail(from_addr, auth_pass, body, html)
        .map(StorePackage::Amazon)
        .or_else(|| {
            ebay::package_from_mail(from_addr, auth_pass, body, html).map(StorePackage::Ebay)
        })
}

/// What `message`'s mail says for `decision`'s records.
fn mail_context(
    decision: &MessageDecision,
    from_addr: &str,
    auth_pass: Option<bool>,
    body: Option<String>,
    body_html: Option<String>,
) -> MailContext {
    MailContext {
        package: if has_delivery(decision) {
            package_from_row(from_addr, auth_pass, body, body_html)
        } else {
            None
        },
        scope: order_fallback::order_scope(from_addr, auth_pass),
    }
}

/// [`mail_context`] for one stored message, or the empty context when it is
/// gone.
fn message_context(
    conn: &Connection,
    account: AccountId,
    message: i64,
    decision: &MessageDecision,
) -> Result<MailContext> {
    let row = conn
        .query_row(
            &format!(
                "SELECT {STORE_BODY_COLUMNS} FROM messages m WHERE m.account_id=?1 AND m.id=?2"
            ),
            params![account, message],
            |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    Ok(match row {
        Some((from, auth, body, html)) => mail_context(decision, &from, auth, body, html),
        None => MailContext::default(),
    })
}

/// The tokens of `message`'s subject and body ([`order_fallback::mail_tokens`]),
/// read once per rebuild and only for a message whose order is in question.
fn message_tokens<'c>(
    conn: &Connection,
    account: AccountId,
    message: i64,
    cache: &'c mut HashMap<i64, Vec<String>>,
) -> Result<&'c [String]> {
    if let std::collections::hash_map::Entry::Vacant(slot) = cache.entry(message) {
        let (text, html): (String, String) = conn
            .query_row(
                "SELECT coalesce(subject,'')||' '||coalesce(body,''),coalesce(body_html,'')
                 FROM messages WHERE account_id=?1 AND id=?2",
                params![account, message],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .unwrap_or_default();
        slot.insert(order_fallback::mail_tokens(&text, &html));
    }
    Ok(&cache[&message])
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

/// A package the owner dropped off at a shipping store is going AWAY from
/// them, and the store's receipt is the last mail it will ever get, so it is
/// never a delivery. Strips the delivery records from a decision about a
/// drop-off receipt, whatever the model proposed, before it is stored: both
/// doors read the stored decision, so neither shows the box. Returns whether
/// anything was dropped.
pub(super) fn drop_sent_packages(
    decision: &mut MessageDecision,
    subject: &str,
    body: &str,
) -> bool {
    let before = decision.records.len();
    if crate::triage::shipment::is_drop_off_receipt(subject, body) {
        decision
            .records
            .retain(|record| !matches!(record, RecordProposal::Delivery { .. }));
    }
    decision.records.len() != before
}

/// ONE-TIME BACKFILL (see `migrate::drop_sent_packages`): take the delivery
/// records out of every stored decision about a drop-off receipt, then
/// rebuild what they had put on the Shipments rail. Returns how many
/// decisions changed.
pub(super) fn retire_sent_packages(conn: &Connection, account: AccountId) -> Result<usize> {
    let rows = conn
        .prepare(
            "SELECT d.message_id,d.decision_json,coalesce(m.subject,''),coalesce(m.body,'')
             FROM agent_message_decisions d JOIN messages m ON m.id=d.message_id AND m.account_id=d.account_id
             WHERE d.account_id=?1 AND d.decision_json LIKE '%\"delivery\"%'",
        )?
        .query_map([account], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut changed = 0;
    for (message, encoded, subject, body) in rows {
        // One undecodable decision costs only its own box.
        let Ok(previous) = serde_json::from_str::<MessageDecision>(&encoded) else {
            continue;
        };
        let mut current = previous.clone();
        if !drop_sent_packages(&mut current, &subject, &body) {
            continue;
        }
        conn.execute(
            "UPDATE agent_message_decisions SET decision_json=?3 WHERE account_id=?1 AND message_id=?2",
            params![
                account,
                message,
                serde_json::to_string(&current).map_err(|e| CoreError::Other(e.into()))?
            ],
        )?;
        reconcile(conn, account, message, Some(&previous), &current)?;
        changed += 1;
    }
    Ok(changed)
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
    let mut affected = BTreeSet::new();
    for decision in previous.into_iter().chain([current]) {
        if has_delivery(decision) {
            let ctx = message_context(conn, account, message, decision)?;
            affected.extend(tracking_numbers(decision, &ctx));
        }
    }
    rebuild(conn, account, &affected)
}

/// ONE-TIME BACKFILL (see `migrate::project_amazon_boxes`): project every
/// Amazon box the account's stored decisions already describe. The agent
/// always wrote these records; they only lacked a key, so no model call is
/// needed. Returns how many boxes were rebuilt.
pub(super) fn project_amazon_boxes(conn: &Connection, account: AccountId) -> Result<usize> {
    project_keys(conn, account, "%amazon.com%", |keys| {
        keys.tracked
            .filter(|k| amazon::is_package_key(k))
            .into_iter()
            .collect()
    })
}

/// ONE-TIME BACKFILL (see `migrate::project_ebay_packages`): the same, for
/// eBay purchases.
pub(super) fn project_ebay_packages(conn: &Connection, account: AccountId) -> Result<usize> {
    project_keys(conn, account, "%@ebay.com", |keys| {
        keys.tracked
            .filter(|k| ebay::is_package_key(k))
            .into_iter()
            .collect()
    })
}

/// ONE-TIME BACKFILL (see `migrate::project_order_fallbacks`): the same, for
/// orders whose mail prints neither a carrier number nor a store package id.
/// Only records that stored their order numbers can key one; the many decided
/// before the agent recorded them stay off the rail.
pub(super) fn project_order_fallbacks(conn: &Connection, account: AccountId) -> Result<usize> {
    project_keys(conn, account, "%", |keys| {
        // A demoted number's old row is rebuilt too, which retires it.
        keys.orders
            .into_iter()
            .map(|(key, _)| key)
            .chain(keys.dropped)
            .collect()
    })
}

/// Rebuild every key of kind `wanted` that the stored decisions of mail from
/// `from_like` touch. Returns how many keys were rebuilt.
fn project_keys(
    conn: &Connection,
    account: AccountId,
    from_like: &str,
    wanted: fn(RecordKeys) -> Vec<String>,
) -> Result<usize> {
    let mut statement = conn.prepare(&format!(
        "SELECT d.decision_json,{STORE_BODY_COLUMNS}
         FROM agent_message_decisions d JOIN messages m ON m.id=d.message_id AND m.account_id=d.account_id
         WHERE d.account_id=?1 AND m.is_sent=0 AND m.is_spam=0
           AND lower(m.from_addr) LIKE ?2 AND d.decision_json LIKE '%\"kind\":\"delivery\"%'"
    ))?;
    let rows = statement
        .query_map(params![account, from_like], |r| {
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
        // One undecodable decision costs only its own package.
        let Ok(decision) = serde_json::from_str::<MessageDecision>(&encoded) else {
            continue;
        };
        let ctx = mail_context(&decision, &from, auth, body, html);
        boxes.extend(decision_keys(&decision, &ctx).flat_map(wanted));
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
        "SELECT d.message_id,d.decision_json,m.received_at,m.subject,{STORE_BODY_COLUMNS},m.from_name
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
                row.get::<_, Option<String>>(8)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut proposals = Vec::new();
    // Order keys in `affected` -> (message, order number) of every tracked or
    // boxed record that names the order and makes a card of its own.
    let mut covered_by: HashMap<String, Vec<(i64, String)>> = HashMap::new();
    let mut tokens: HashMap<i64, Vec<String>> = HashMap::new();
    for (message, encoded, received, subject, (from, auth, body, html), from_name) in decisions {
        let decision: MessageDecision =
            serde_json::from_str(&encoded).map_err(|e| CoreError::Other(e.into()))?;
        let ctx = mail_context(&decision, &from, auth, body, html);
        let package = ctx.package.as_ref();
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
            let Some(status) = crate::triage::ShipmentStatus::parse(&status) else {
                continue;
            };
            let keys = record_keys(tracking_number.as_deref(), &order_refs, &ctx);
            let (number, by_order) = match keys.tracked {
                Some(number) => (number, false),
                None => {
                    // A trackless record files under the smallest order its
                    // mail prints. Mail is read only when one is in question.
                    if !keys.orders.iter().any(|(key, _)| affected.contains(key)) {
                        continue;
                    }
                    let printed = message_tokens(conn, account, message, &mut tokens)?;
                    match keys
                        .orders
                        .iter()
                        .find(|(_, raw)| order_fallback::mail_prints(printed, raw))
                    {
                        Some((key, _)) => (key.clone(), true),
                        None => continue,
                    }
                }
            };
            let boxed = package.filter(|p| p.keys(&number));
            let carrier = match boxed {
                Some(package) => package.carrier(carrier),
                // An order card shows the carrier the mail named, or none:
                // trackless mail is exactly the mail that may not name one.
                None if by_order => carrier
                    .filter(|c| matches!(c.as_str(), "ups" | "usps" | "fedex" | "dhl" | "amazon"))
                    .unwrap_or_else(|| "unknown".to_string()),
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
            // Only a record that makes a card of its own covers its orders:
            // one dropped above (no usable carrier, no status) must not hide
            // the order card and leave nothing in its place.
            if !by_order {
                for (key, raw) in &keys.orders {
                    if affected.contains(key) {
                        covered_by
                            .entry(key.clone())
                            .or_default()
                            .push((message, raw.clone()));
                    }
                }
            }
            if affected.contains(&number) {
                // The store is the seller of a package its own mail tracks
                // when the model named none, and an Amazon box's order is the
                // one in its tracker link.
                let merchant = sanitize_merchant(merchant.as_deref())
                    .or_else(|| boxed.map(|p| p.merchant().to_string()))
                    .or_else(|| {
                        by_order
                            .then(|| sanitize_merchant(from_name.as_deref()))
                            .flatten()
                    });
                let mut order_refs = sanitize_order_refs(&order_refs);
                if let Some(order) = boxed.and_then(StorePackage::order_id)
                    && !order_refs.iter().any(|r| r == order)
                {
                    order_refs.push(order.to_string());
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
                            // The mail's own links are not trusted, and no
                            // carrier page is keyed by an order.
                            None if by_order => None,
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
        // An order card steps aside once a tracked or boxed package of the
        // same sender names its order, in mail that prints it: that package
        // is the order's card now, and two would be one purchase twice.
        let mut covered = false;
        for (message, raw) in covered_by.get(number).into_iter().flatten() {
            if order_fallback::mail_prints(
                message_tokens(conn, account, *message, &mut tokens)?,
                raw,
            ) {
                covered = true;
                break;
            }
        }
        let mine: Vec<&Proposal> = proposals
            .iter()
            .filter(|p| !covered && p.info.tracking_number == *number)
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
        let (message, received) = (&latest.message, &latest.received);
        // An eBay package keeps the last carrier its mail named: PACKAGE
        // DELIVERED names none, and the row should not forget it was FedEx.
        let mut info = latest.info.clone();
        if (ebay::is_package_key(number) || order_fallback::is_key(number))
            && info.carrier == "unknown"
            && let Some(named) = mine.iter().find(|p| p.info.carrier != "unknown")
        {
            info.carrier = named.info.carrier.clone();
        }
        let info = &info;
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
                 VALUES(?1,1,?2,?2,'orders@riveraexhaustworks.test',?3,?4,'','')",
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
    /// One card, named, with both rows carrying order 50417.
    #[test]
    fn two_numbers_on_one_order_are_one_named_card() {
        let store = fixture();
        add_message(
            &store,
            3,
            "Trackside shipment update",
            chrono::Duration::minutes(50),
        );
        add_message(
            &store,
            4,
            "A shipment from order #50417 is on the way",
            chrono::Duration::minutes(10),
        );
        let inbound = named(
            "1Z999AA10123456792",
            "shipped",
            None,
            Some("Rivera Exhaust Works"),
            &["50417"],
        );
        let outbound = named(
            "1Z999AA10123456793",
            "shipped",
            Some("Trackside Muffler Insert TM10"),
            Some("RIVERA EXHAUST WORKS"),
            &["Order #50417"],
        );
        write(&store, 3, None, &inbound);
        write(&store, 4, None, &outbound);
        assert_eq!(
            links(&store),
            vec![
                (
                    "1Z999AA10123456792".into(),
                    "riveraexhaustworks".into(),
                    "50417".into(),
                    "agent".into()
                ),
                (
                    "1Z999AA10123456793".into(),
                    "riveraexhaustworks".into(),
                    "50417".into(),
                    "agent".into()
                ),
            ]
        );
        let cards = listed(&store);
        assert_eq!(cards.len(), 1, "one order, one card");
        let card = &cards[0];
        assert_eq!(
            card.tracking_number, "1Z999AA10123456793",
            "the newest leg represents"
        );
        assert_eq!(card.item_name, "Trackside Muffler Insert TM10");
        assert_eq!(card.merchant.as_deref(), Some("RIVERA EXHAUST WORKS"));
        assert_eq!(card.orders.len(), 1);
        assert_eq!(card.orders[0].order_ref, "50417");
        assert_eq!(card.legs.len(), 1);
        assert_eq!(card.legs[0].tracking_number, "1Z999AA10123456792");
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
        for (id, junk) in [(1, "Arriving soon"), (2, "Rivera Exhaust Works")] {
            write(
                &store,
                id,
                None,
                &named(
                    "1Z999AA10123456784",
                    "shipped",
                    Some(junk),
                    Some("Rivera Exhaust Works"),
                    &["50417"],
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

    const ORDER: &str = "113-0000000-0000001";
    const KEY: &str = "amazon:113-0000000-0000001:N0FAKE001";
    const SHIPMENT: &str = "N0FAKE001";

    /// THE BUG. Shipped, then Delivered, for one box, neither printing a
    /// tracking number: one Amazon row that ends delivered, named, sold by
    /// Amazon, carrying its order, and clickable through to Amazon's tracker.
    #[test]
    fn an_amazon_box_is_one_row_from_shipped_to_delivered() {
        let store = fixture();
        let body = tracker(ORDER, "N0FAKE001");
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
                "https://www.amazon.com/progress-tracker/package?orderId=113-0000000-0000001&shipmentId=N0FAKE001"
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

    /// Two boxes of one order (the paper towels and the shampoo) are two rows
    /// with two ids, folded into one card by the order they share.
    #[test]
    fn two_amazon_boxes_of_one_order_are_two_rows_one_card() {
        let store = fixture();
        let order = "111-0000000-0000001";
        add_amazon(
            &store,
            10,
            "shipment-tracking@amazon.com",
            Some(true),
            "Shipped: Paper towels",
            &tracker(order, "N0FAKE002"),
            chrono::Duration::hours(5),
        );
        add_amazon(
            &store,
            11,
            "shipment-tracking@amazon.com",
            Some(true),
            "Shipped: Shampoo",
            &tracker(order, "N0FAKE003"),
            chrono::Duration::hours(4),
        );
        write(
            &store,
            10,
            None,
            &trackless("shipped", Some("amazon"), "Paper towels", &[order]),
        );
        write(
            &store,
            11,
            None,
            &trackless("shipped", Some("amazon"), "Shampoo", &[order]),
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
        let body = tracker(ORDER, "N0FAKE001");
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
            &tracker(ORDER, "N0FAKE001"),
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
                    "<a href=\"https://www.amazon.com/progress-tracker/package?orderId={ORDER}&amp;shipmentId=N0FAKE001\">Track</a>"
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
            &tracker(ORDER, "N0FAKE001"),
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

    /// eBay mail as it arrives: links only in the HTML part.
    fn add_ebay(store: &SqliteStore, id: i64, from: &str, subject: &str, path: &str, age_h: i64) {
        let html = format!(
            r#"<a href="https://www.ebay.com/itm/198765432109?mkevt=1">Lens</a>
               <a href="https://www.ebay.com/{path}?itemId=198765432109&amp;transactionId=10098765432101&amp;mkevt=1">Track</a>"#
        );
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body,body_html,auth_pass)
                 VALUES(?1,1,?2,?2,?3,?4,?5,'','Order number: 01-23456-78901',?6,1)",
                params![id, id.to_string(), from, subject, (Utc::now() - chrono::Duration::hours(age_h)).to_rfc3339(), html],
            )
            .unwrap();
    }

    const EBAY_KEY: &str = "ebay:198765432109:10098765432101";

    /// THE EBAY BUG. Five mails for one purchase,
    /// none printing a tracking number, the model naming FedEx on only two of
    /// them: one eBay row from Order confirmed to PACKAGE DELIVERED, shown as
    /// FedEx while it travels, clickable to eBay's order page, and never
    /// polled at FedEx with a key that is not FedEx's.
    #[test]
    fn an_ebay_purchase_is_one_row_from_ordered_to_delivered() {
        let store = fixture();
        let steps = [
            (
                20,
                "Order confirmed: Lens",
                "vod/FetchOrderDetails",
                "ordered",
                None,
            ),
            (
                21,
                "Order update: Lens",
                "vod/FetchOrderDetails",
                "shipped",
                None,
            ),
            (
                22,
                "Your package is now with its carrier!",
                "vod/FetchOrderDetails",
                "shipped",
                Some("fedex"),
            ),
            (
                23,
                "OUT FOR DELIVERY: Lens",
                "vod/FetchOrderDetails",
                "out_for_delivery",
                Some("fedex"),
            ),
            (24, "PACKAGE DELIVERED: Lens", "ord/show", "delivered", None),
        ];
        for (i, (id, subject, path, status, carrier)) in steps.into_iter().enumerate() {
            add_ebay(
                &store,
                id,
                "ebay@ebay.com",
                subject,
                path,
                50 - i as i64 * 10,
            );
            write(
                &store,
                id,
                None,
                &trackless(status, carrier, "Camera lens", &["01-23456-78901"]),
            );
            let rows = store.list_shipments(1, true, Default::default()).unwrap();
            assert_eq!(rows.len(), 1, "step {status} lands on the one row");
            let row = &rows[0];
            assert_eq!(row.tracking_number, EBAY_KEY);
            assert_eq!(row.status, status);
            assert_eq!(row.merchant.as_deref(), Some("eBay"));
            assert_eq!(
                row.tracking_url.as_deref(),
                Some(
                    "https://www.ebay.com/ord/show?itemId=198765432109&transactionId=10098765432101"
                )
            );
            let expected = if matches!(status, "ordered") || id == 21 {
                "unknown"
            } else {
                "fedex"
            };
            assert_eq!(row.carrier, expected, "step {status} (msg {id})");
            if status == "out_for_delivery" {
                assert!(
                    store
                        .list_pollable_shipments(1, Utc::now() - chrono::Duration::days(3), 5)
                        .unwrap()
                        .is_empty(),
                    "a package key is never polled at a carrier"
                );
            }
        }
    }

    /// THE EBAY BACKFILL: stored decisions project without a model call.
    #[test]
    fn the_backfill_projects_stored_ebay_decisions() {
        let store = fixture();
        add_ebay(
            &store,
            20,
            "ebay@ebay.com",
            "OUT FOR DELIVERY",
            "vod/FetchOrderDetails",
            1,
        );
        let decision = trackless("out_for_delivery", Some("fedex"), "Lens", &[]);
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO agent_message_decisions(account_id,message_id,revision,decision_json,decided_at)
                 VALUES(1,20,1,?1,?2)",
                params![serde_json::to_string(&decision).unwrap(), Utc::now().to_rfc3339()],
            )
            .unwrap();
        for _ in 0..2 {
            let conn = store.lock().unwrap();
            assert_eq!(project_amazon_boxes(&conn, 1).unwrap(), 0, "not Amazon's");
            assert_eq!(project_ebay_packages(&conn, 1).unwrap(), 1);
        }
        let rows = store.list_shipments(1, true, Default::default()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tracking_number, EBAY_KEY);
        assert_eq!(rows[0].status, "out_for_delivery");
    }

    /// Store mail: authenticated, with a display name, printing `body`.
    fn add_store_mail(
        store: &SqliteStore,
        id: i64,
        from: &str,
        name: &str,
        body: &str,
        age_h: i64,
    ) {
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,from_name,subject,received_at,snippet,body,auth_pass)
                 VALUES(?1,1,?2,?2,?3,?4,'Order update',?5,'',?6,1)",
                params![id, id.to_string(), from, name, (Utc::now() - chrono::Duration::hours(age_h)).to_rfc3339(), body],
            )
            .unwrap();
    }

    fn rail(store: &SqliteStore) -> Vec<crate::types::Shipment> {
        store.list_shipments(1, true, Default::default()).unwrap()
    }

    const SHOP: &str = "orders@pillows.example.com";
    const SHOP_KEY: &str = "order:example.com:4600000017";

    /// THE GENERAL CASE. A store that never prints a tracking number: its
    /// confirmation, shipping and delivery mail are one card, keyed by who
    /// sent it and the order it printed, sold by the sender's name when the
    /// model named no store, keeping the carrier one mail named, linking
    /// nowhere, and never polled.
    #[test]
    fn a_trackless_store_order_is_one_card_from_ordered_to_delivered() {
        let store = fixture();
        let steps = [
            (20, "ordered", None),
            (21, "shipped", Some("fedex")),
            (22, "delivered", None),
        ];
        for (i, (id, status, carrier)) in steps.into_iter().enumerate() {
            add_store_mail(
                &store,
                id,
                SHOP,
                "Pillow Shop",
                "Order #4600000017: Down pillow",
                30 - i as i64 * 10,
            );
            write(
                &store,
                id,
                None,
                &trackless(status, carrier, "Down pillow", &["#4600000017"]),
            );
            let rows = rail(&store);
            assert_eq!(rows.len(), 1, "{status}");
            let row = &rows[0];
            assert_eq!(row.tracking_number, SHOP_KEY);
            assert_eq!(row.status, status);
            assert_eq!(row.item_name, "Down pillow");
            assert_eq!(row.merchant.as_deref(), Some("Pillow Shop"));
            assert_eq!(
                row.carrier,
                if id == 20 { "unknown" } else { "fedex" },
                "{status}"
            );
            assert_eq!(row.tracking_url, None);
            assert!(
                store
                    .list_pollable_shipments(1, Utc::now() - chrono::Duration::days(3), 5)
                    .unwrap()
                    .is_empty()
            );
        }
    }

    /// The order number must be in the mail, whole: an invented one, or one
    /// that is only a prefix of what the mail prints, keys nothing. And a
    /// sender that did not authenticate opens no card.
    #[test]
    fn an_unprinted_order_or_an_unauthenticated_sender_keys_nothing() {
        let store = fixture();
        add_store_mail(
            &store,
            20,
            SHOP,
            "Pillow Shop",
            "Order #46000000179 is on its way",
            2,
        );
        write(
            &store,
            20,
            None,
            &trackless("shipped", None, "Pillow", &["4600000017"]),
        );
        assert!(rail(&store).is_empty(), "a prefix of the printed order");
        add_store_mail(
            &store,
            21,
            "orders@shop.test",
            "Shop",
            "Order #4600000017",
            1,
        );
        store
            .lock()
            .unwrap()
            .execute("UPDATE messages SET auth_pass=0 WHERE id=21", [])
            .unwrap();
        write(
            &store,
            21,
            None,
            &trackless("shipped", None, "Pillow", &["4600000017"]),
        );
        assert!(rail(&store).is_empty(), "unauthenticated");
    }

    /// Shopify sends every store's mail from one domain; two stores' order
    /// #1001 are two cards.
    #[test]
    fn two_shopify_stores_order_1001_are_two_cards() {
        let store = fixture();
        add_store_mail(
            &store,
            20,
            "store+111111@t.shopifyemail.com",
            "Tube Shop",
            "Order #1001 confirmed",
            2,
        );
        add_store_mail(
            &store,
            21,
            "store+222222@t.shopifyemail.com",
            "Stamp Shop",
            "Order #1001 confirmed",
            1,
        );
        write(
            &store,
            20,
            None,
            &trackless("ordered", None, "Tubes", &["#1001"]),
        );
        write(
            &store,
            21,
            None,
            &trackless("ordered", None, "Stamp", &["#1001"]),
        );
        let mut names: Vec<String> = rail(&store).into_iter().map(|r| r.item_name).collect();
        names.sort();
        assert_eq!(names, ["Stamp", "Tubes"]);
    }

    /// STEPPING ASIDE. The order card leaves when the same sender's tracked
    /// mail names its order, and comes back if that mail's record is
    /// retracted. Another sender naming the order does not move it.
    #[test]
    fn an_order_card_steps_aside_for_a_tracked_package_of_the_same_order() {
        let store = fixture();
        add_store_mail(
            &store,
            20,
            SHOP,
            "Pillow Shop",
            "Order #4600000017 confirmed",
            3,
        );
        write(
            &store,
            20,
            None,
            &trackless("ordered", None, "Pillow", &["4600000017"]),
        );
        add_store_mail(
            &store,
            22,
            "ups@ups.com",
            "UPS",
            "Order 4600000017 1Z999AA10123456784",
            2,
        );
        let stranger = named("1Z999AA10123456784", "shipped", None, None, &["4600000017"]);
        write(&store, 22, None, &stranger);
        assert_eq!(
            rail(&store).len(),
            2,
            "another sender does not retire the order"
        );

        add_store_mail(
            &store,
            21,
            SHOP,
            "Pillow Shop",
            "Order #4600000017 shipped: 1Z999AA10123456784",
            1,
        );
        let tracked = named(
            "1Z999AA10123456784",
            "shipped",
            Some("Pillow"),
            None,
            &["4600000017"],
        );
        write(&store, 21, None, &tracked);
        let rows = rail(&store);
        assert_eq!(rows.len(), 1, "the order card stepped aside");
        assert_eq!(rows[0].tracking_number, "1Z999AA10123456784");

        let retracted = MessageDecision::default();
        write(&store, 21, Some(&tracked), &retracted);
        let numbers: Vec<String> = rail(&store)
            .into_iter()
            .map(|r| r.tracking_number)
            .collect();
        assert!(
            numbers.iter().any(|n| n == SHOP_KEY),
            "retracting the covering record alone brings the order back: {numbers:?}"
        );
    }

    /// Amazon's Ordered mail has no tracker link, so it now opens an order
    /// card, which the box takes over once it ships.
    #[test]
    fn an_amazon_order_card_hands_over_to_its_box() {
        let store = fixture();
        add_amazon(
            &store,
            10,
            "auto-confirm@amazon.com",
            Some(true),
            "Ordered",
            &format!("Order #\n{ORDER}\n"),
            chrono::Duration::hours(5),
        );
        write(
            &store,
            10,
            None,
            &trackless("ordered", None, "Wire Brushes", &[ORDER]),
        );
        let rows = rail(&store);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].tracking_number,
            format!("order:amazon.com:{}", ORDER.replace('-', ""))
        );

        add_amazon(
            &store,
            11,
            "shipment-tracking@amazon.com",
            Some(true),
            "Shipped",
            &tracker(ORDER, "N0FAKE001"),
            chrono::Duration::hours(1),
        );
        write(
            &store,
            11,
            None,
            &trackless("shipped", None, "Wire Brushes", &[]),
        );
        let rows = rail(&store);
        assert_eq!(rows.len(), 1, "the box took the order over");
        assert_eq!(rows[0].tracking_number, KEY);
    }

    /// A tracked-looking record that makes no card (a carrier the rail does
    /// not know) does not hide the order: nothing would take its place.
    #[test]
    fn a_record_that_makes_no_card_covers_nothing() {
        let store = fixture();
        add_store_mail(
            &store,
            20,
            SHOP,
            "Pillow Shop",
            "Order #4600000017 confirmed",
            2,
        );
        write(
            &store,
            20,
            None,
            &trackless("ordered", None, "Pillow", &["4600000017"]),
        );
        add_store_mail(
            &store,
            21,
            SHOP,
            "Pillow Shop",
            "Order #4600000017 via OnTrac D10012345678901",
            1,
        );
        let mut ontrac = named("D10012345678901", "shipped", None, None, &["4600000017"]);
        if let RecordProposal::Delivery { carrier, .. } = &mut ontrac.records[0] {
            *carrier = Some("ontrac".into());
        }
        write(&store, 21, None, &ontrac);
        let rows = rail(&store);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tracking_number, SHOP_KEY);
        assert_eq!(rows[0].status, "ordered");
    }

    /// The model copying the order number into `tracking_number` files the
    /// record under its order, not as a polled package hiding that order.
    #[test]
    fn an_order_number_in_the_tracking_field_is_the_order() {
        let store = fixture();
        add_store_mail(
            &store,
            20,
            SHOP,
            "Pillow Shop",
            "Order #4600000017 is on its way",
            1,
        );
        write(
            &store,
            20,
            None,
            &named(
                "4600000017",
                "shipped",
                Some("Pillow"),
                None,
                &["#4600000017"],
            ),
        );
        let rows = rail(&store);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tracking_number, SHOP_KEY);
        assert!(
            store
                .list_pollable_shipments(1, Utc::now() - chrono::Duration::days(3), 5)
                .unwrap()
                .is_empty()
        );
    }

    /// Of a record's order numbers, the smallest one its mail PRINTS keys it;
    /// an unprinted smaller one does not cost the record its card. A number
    /// with separators matches however the mail spaces it.
    #[test]
    fn the_smallest_printed_order_keys_the_card() {
        let store = fixture();
        add_store_mail(
            &store,
            20,
            SHOP,
            "Pillow Shop",
            "Your order PO 2024/551 has shipped",
            1,
        );
        write(
            &store,
            20,
            None,
            &trackless("shipped", None, "Pillow", &["1000", "PO/2024/551"]),
        );
        let rows = rail(&store);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tracking_number, "order:example.com:PO2024551");
    }

    /// An Amazon box mail whose record carries a TBA number, and no order
    /// numbers, still covers its order through the tracker link.
    #[test]
    fn a_tba_numbered_box_covers_its_amazon_order() {
        let store = fixture();
        add_amazon(
            &store,
            10,
            "auto-confirm@amazon.com",
            Some(true),
            "Ordered",
            &format!("Order #\n{ORDER}\n"),
            chrono::Duration::hours(5),
        );
        write(
            &store,
            10,
            None,
            &trackless("ordered", None, "Wire Brushes", &[ORDER]),
        );
        add_amazon(
            &store,
            11,
            "shipment-tracking@amazon.com",
            Some(true),
            "Shipped",
            &tracker(ORDER, "N0FAKE001"),
            chrono::Duration::hours(1),
        );
        let mut tba = named(
            "TBA000123456789",
            "shipped",
            Some("Wire Brushes"),
            None,
            &[],
        );
        if let RecordProposal::Delivery { carrier, .. } = &mut tba.records[0] {
            *carrier = Some("amazon".into());
        }
        write(&store, 11, None, &tba);
        let numbers: Vec<String> = rail(&store)
            .into_iter()
            .map(|r| r.tracking_number)
            .collect();
        assert_eq!(numbers, ["TBA000123456789"]);
    }

    /// A real carrier number the model also listed among the order numbers
    /// stays a tracked, polled package, from any sender.
    #[test]
    fn a_carrier_number_among_the_refs_stays_tracked() {
        let store = fixture();
        let echoed = named(
            "1Z999AA10123456784",
            "shipped",
            None,
            None,
            &["1Z999AA10123456784"],
        );
        // An unauthenticated sender, which no order key could replace.
        write(&store, 1, None, &echoed);
        // A store whose mail prints it, whose order key could.
        add_store_mail(
            &store,
            20,
            SHOP,
            "Pillow Shop",
            "Order 1Z999AA10123456784 shipped",
            1,
        );
        write(&store, 20, None, &echoed);
        let rows = rail(&store);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tracking_number, "1Z999AA10123456784");
        let pollable = store
            .list_pollable_shipments(1, Utc::now() - chrono::Duration::days(3), 5)
            .unwrap();
        assert_eq!(pollable.len(), 1, "still a polled UPS package");
    }

    /// A row an older build keyed on an order number in the tracking field is
    /// retired by the backfill, leaving the order's own card.
    #[test]
    fn the_backfill_retires_a_row_keyed_on_an_order_number() {
        let store = fixture();
        add_store_mail(
            &store,
            20,
            SHOP,
            "Pillow Shop",
            "Order #4600000017 is on its way",
            1,
        );
        let decision = named(
            "4600000017",
            "shipped",
            Some("Pillow"),
            None,
            &["4600000017"],
        );
        {
            let conn = store.lock().unwrap();
            conn.execute(
                "INSERT INTO agent_message_decisions(account_id,message_id,revision,decision_json,decided_at)
                 VALUES(1,20,1,?1,?2)",
                params![serde_json::to_string(&decision).unwrap(), Utc::now().to_rfc3339()],
            )
            .unwrap();
            // What an older build projected: the order number as a UPS row.
            let info = crate::triage::ShipmentInfo {
                carrier: "ups".into(),
                tracking_number: "4600000017".into(),
                status: crate::triage::ShipmentStatus::Shipped,
                item_name: "Pillow".into(),
                tracking_url: None,
            };
            let id =
                super::super::specialists::upsert_shipment_conn(&conn, 1, 20, &info, Utc::now())
                    .unwrap();
            conn.execute(
                "INSERT INTO agent_delivery_projections(account_id,shipment_id) VALUES(1,?1)",
                [id],
            )
            .unwrap();
        }
        assert_eq!(rail(&store).len(), 1);
        project_order_fallbacks(&store.lock().unwrap(), 1).unwrap();
        let numbers: Vec<String> = rail(&store)
            .into_iter()
            .map(|r| r.tracking_number)
            .collect();
        assert_eq!(numbers, [SHOP_KEY]);
    }

    /// An Amazon box mail whose record carries the ORDER id as its tracking
    /// number is still its box.
    #[test]
    fn an_amazon_order_id_in_the_tracking_field_is_its_box() {
        let store = fixture();
        add_amazon(
            &store,
            11,
            "shipment-tracking@amazon.com",
            Some(true),
            "Shipped",
            &tracker(ORDER, SHIPMENT),
            chrono::Duration::hours(1),
        );
        let mut record = named(ORDER, "shipped", Some("Wire Brushes"), None, &[ORDER]);
        if let RecordProposal::Delivery { carrier, .. } = &mut record.records[0] {
            *carrier = None;
        }
        write(&store, 11, None, &record);
        let numbers: Vec<String> = rail(&store)
            .into_iter()
            .map(|r| r.tracking_number)
            .collect();
        assert_eq!(numbers, [KEY]);
    }

    /// Markup is not mail text: an attribute value is not a printed order.
    #[test]
    fn an_order_number_only_in_markup_is_not_printed() {
        let store = fixture();
        add_store_mail(&store, 20, SHOP, "Pillow Shop", "Your order shipped", 1);
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE messages SET body_html='<img width=\"10001\" src=\"x\">Your order shipped' WHERE id=20",
                [],
            )
            .unwrap();
        write(
            &store,
            20,
            None,
            &trackless("shipped", None, "Pillow", &["10001"]),
        );
        assert!(rail(&store).is_empty());
    }

    /// THE ORDER BACKFILL: stored trackless decisions project once, and a
    /// stored tracked package still covers its order.
    #[test]
    fn the_backfill_projects_stored_orders() {
        let store = fixture();
        add_store_mail(&store, 20, SHOP, "Pillow Shop", "Order #4600000017", 3);
        add_store_mail(
            &store,
            21,
            "orders@jeans.example.net",
            "Jeans Co",
            "Your order is confirmed (#50019)",
            2,
        );
        add_store_mail(
            &store,
            22,
            "orders@jeans.example.net",
            "Jeans Co",
            "Order #50019 shipped 1Z999AA10123456784",
            1,
        );
        let stored = [
            (20, trackless("shipped", None, "Pillow", &["4600000017"])),
            (21, trackless("ordered", None, "Jeans", &["50019"])),
            (
                22,
                named(
                    "1Z999AA10123456784",
                    "shipped",
                    Some("Jeans"),
                    None,
                    &["#50019"],
                ),
            ),
        ];
        {
            let conn = store.lock().unwrap();
            for (id, decision) in &stored {
                conn.execute(
                    "INSERT INTO agent_message_decisions(account_id,message_id,revision,decision_json,decided_at)
                     VALUES(1,?1,1,?2,?3)",
                    params![id, serde_json::to_string(decision).unwrap(), Utc::now().to_rfc3339()],
                )
                .unwrap();
            }
        }
        for _ in 0..2 {
            let conn = store.lock().unwrap();
            assert_eq!(
                project_order_fallbacks(&conn, 1).unwrap(),
                2,
                "the pillow shop's and the jeans shop's orders"
            );
        }
        let numbers: Vec<String> = rail(&store)
            .into_iter()
            .map(|r| r.tracking_number)
            .collect();
        assert_eq!(
            numbers,
            [SHOP_KEY],
            "the jeans shop's order is its tracked package's"
        );
    }

    /// THE DROP-OFF BACKFILL. A receipt stored with a delivery record had put
    /// the box on the rail; the one-time pass strips the record from the
    /// stored decision and retires the row. Ordinary shipping mail is left
    /// alone, and a second pass changes nothing.
    #[test]
    fn the_backfill_retires_stored_drop_off_receipts() {
        let store = fixture();
        {
            let conn = store.lock().unwrap();
            conn.execute(
                "UPDATE messages SET subject='Thanks for choosing The UPS Store',
                     body='Drop-Off Package Receipt. DROP-OFF TIME: 10:18 AM' WHERE id=1",
                [],
            )
            .unwrap();
        }
        write(&store, 1, None, &delivery("1Z999AA10123456794", "shipped"));
        write(&store, 2, None, &delivery("1Z999AA10123456784", "shipped"));
        assert_eq!(listed(&store).len(), 2);
        for expected in [1, 0] {
            let conn = store.lock().unwrap();
            assert_eq!(retire_sent_packages(&conn, 1).unwrap(), expected);
        }
        let rows = listed(&store);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tracking_number, "1Z999AA10123456784");
        let stored: String = store
            .lock()
            .unwrap()
            .query_row(
                "SELECT decision_json FROM agent_message_decisions WHERE message_id=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let stored: MessageDecision = serde_json::from_str(&stored).unwrap();
        assert!(stored.records.is_empty());
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
            &tracker(ORDER, "N0FAKE001"),
            chrono::Duration::hours(1),
        );
        write(&store, 10, None, &delivery("1Z999AA10123456784", "shipped"));
        let rows = listed(&store);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tracking_number, "1Z999AA10123456784");
        assert_eq!(rows[0].carrier, "ups");
    }
}
