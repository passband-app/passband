//! eBay packages, identified by eBay's own purchase line.
//!
//! eBay's order mail (Order confirmed, Order update, "Your package is now with
//! its carrier!", OUT FOR DELIVERY, PACKAGE DELIVERED) names the carrier but
//! never prints its tracking number, so the agent's delivery records for it
//! carry `tracking_number: null` and, like Amazon's, had nothing to key a
//! `shipments` row on. What every one of those mails DOES carry is an
//! order-details link with `itemId` and `transactionId`
//! (`ebay.com/vod/FetchOrderDetails?…` before delivery, `ebay.com/ord/show?…`
//! on it). The pair is one purchase line: the same across the whole order's
//! mail, and different for a second item. It keys `ebay:<itemId>:<transactionId>`.
//!
//! The same rules as [`super::amazon`]: read by code, not the model; only mail
//! whose From domain AUTHENTICATED as ebay.com, and only eBay's own address
//! (`members.ebay.com` relays what sellers write, links included); ids must be
//! all digits; the click target is built here; two purchase lines in one mail
//! yield nothing rather than a guess.
use crate::triage::text::rx;
use regex::Regex;
use std::collections::BTreeSet;
use std::sync::OnceLock;

/// The prefix every eBay package key carries in `shipments.tracking_number`.
pub const KEY_PREFIX: &str = "ebay:";

/// One eBay purchase line: the listing and eBay's id for the sale.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EbayPackage {
    pub item_id: String,
    pub transaction_id: String,
}

impl EbayPackage {
    /// The row key: `ebay:<itemId>:<transactionId>`.
    pub fn key(&self) -> String {
        format!("{KEY_PREFIX}{}:{}", self.item_id, self.transaction_id)
    }

    /// eBay's order details page, where its tracking lives. It needs the
    /// owner's eBay login, which is fine: the owner is the one who clicks it.
    pub fn tracking_url(&self) -> String {
        format!(
            "https://www.ebay.com/ord/show?itemId={}&transactionId={}",
            self.item_id, self.transaction_id
        )
    }
}

/// Is this a key [`EbayPackage::key`] minted?
pub fn is_package_key(number: &str) -> bool {
    number.starts_with(KEY_PREFIX)
}

/// The one eBay purchase this mail is about, or `None`: not authenticated
/// mail from eBay itself, no order-details link, or links to more than one.
pub fn package_from_mail(
    from_addr: &str,
    auth_pass: Option<bool>,
    body: &str,
    body_html: &str,
) -> Option<EbayPackage> {
    if super::ghost::sender_scope(from_addr, auth_pass).as_deref() != Some("ebay.com")
        || !from_addr.trim().to_ascii_lowercase().ends_with("@ebay.com")
    {
        return None;
    }
    let mut found = BTreeSet::new();
    for text in [body, body_html] {
        for raw in links(text) {
            if let Some(package) = order_link(&raw.replace("&amp;", "&")) {
                found.insert(package);
            }
        }
    }
    let mut found = found.into_iter();
    match (found.next(), found.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

fn links(text: &str) -> impl Iterator<Item = &str> {
    static LINK: OnceLock<Regex> = OnceLock::new();
    LINK.get_or_init(|| rx(r#"https?://[^\s"'<>()\[\]]+"#))
        .find_iter(text)
        .map(|m| m.as_str())
}

/// Read an order-details link. Item pages (`/itm/<id>`), feedback and survey
/// links carry no `transactionId` and are not the purchase.
fn order_link(raw: &str) -> Option<EbayPackage> {
    let url = url::Url::parse(raw).ok()?;
    if !matches!(url.host_str(), Some("www.ebay.com" | "ebay.com")) {
        return None;
    }
    if !matches!(url.path(), "/vod/FetchOrderDetails" | "/ord/show") {
        return None;
    }
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let get = |name: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    let item_id = get("itemId")?;
    let transaction_id = get("transactionId")?;
    (is_id(item_id, 9..=15) && is_id(transaction_id, 6..=20)).then(|| EbayPackage {
        item_id: item_id.to_string(),
        transaction_id: transaction_id.to_string(),
    })
}

fn is_id(s: &str, len: std::ops::RangeInclusive<usize>) -> bool {
    len.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FROM: &str = "ebay@ebay.com";

    /// Links from a real "Your package is now with its carrier!" mail, trimmed.
    const SHIPPED_HTML: &str = concat!(
        r#"<a href="https://www.ebay.com/itm/123456789012?mkevt=1&amp;plmtId=1764605637">TR/ST</a>"#,
        r#"<a href="https://www.ebay.com/vod/FetchOrderDetails?itemId=123456789012&amp;transactionId=10012345678901&amp;mkevt=1&amp;mkcid=7">Track package</a>"#,
        r#"<a href="https://www.ebay.com/srv/survey/a/crm.buyer_post_txn?_0=1&amp;mkevt=1">1</a>"#,
    );

    /// Links from the same order's real PACKAGE DELIVERED mail.
    const DELIVERED_HTML: &str = concat!(
        r#"<a href="https://www.ebay.com/fdbk/leave_feedback?mkevt=1&amp;mkpid=0">Give feedback</a>"#,
        r#"<a href="https://www.ebay.com/ord/show?itemId=123456789012&amp;transactionId=10012345678901&amp;mkevt=1">View order details</a>"#,
    );

    fn trst() -> EbayPackage {
        EbayPackage {
            item_id: "123456789012".into(),
            transaction_id: "10012345678901".into(),
        }
    }

    #[test]
    fn shipped_and_delivered_mail_name_the_same_purchase() {
        assert_eq!(
            package_from_mail(FROM, Some(true), "", SHIPPED_HTML),
            Some(trst())
        );
        assert_eq!(
            package_from_mail(FROM, Some(true), "", DELIVERED_HTML),
            Some(trst())
        );
        assert_eq!(trst().key(), "ebay:123456789012:10012345678901");
    }

    #[test]
    fn unauthenticated_relayed_or_foreign_senders_name_nothing() {
        for auth in [None, Some(false)] {
            assert_eq!(package_from_mail(FROM, auth, "", SHIPPED_HTML), None);
        }
        for from in [
            "seller_x1@members.ebay.com",
            "ebay@ebay.com.example.net",
            "someone@gmail.com",
        ] {
            assert_eq!(
                package_from_mail(from, Some(true), "", SHIPPED_HTML),
                None,
                "{from}"
            );
        }
    }

    #[test]
    fn an_order_link_on_another_host_is_not_ebays() {
        let lure = SHIPPED_HTML.replace("www.ebay.com/vod", "www.ebay.com.evil.test/vod");
        assert_eq!(package_from_mail(FROM, Some(true), "", &lure), None);
    }

    #[test]
    fn two_purchases_in_one_mail_is_no_guess() {
        let two = format!(
            "{SHIPPED_HTML}<a href=\"https://www.ebay.com/ord/show?itemId=198765432109&amp;transactionId=10095512345678\">x</a>"
        );
        assert_eq!(package_from_mail(FROM, Some(true), "", &two), None);
    }

    #[test]
    fn malformed_ids_are_refused() {
        for (item, txn) in [
            ("38760118650", "1008430463532x"),
            ("3876011865O1", "10012345678901"),
            ("123456789012", "123"),
            ("123456789012", "%3Cx%3E"),
        ] {
            let html = format!("https://www.ebay.com/ord/show?itemId={item}&transactionId={txn}");
            assert_eq!(
                package_from_mail(FROM, Some(true), "", &html),
                None,
                "{item} {txn}"
            );
        }
    }

    #[test]
    fn mail_without_an_order_link_names_nothing() {
        let offer = r#"<a href="https://www.ebay.com/itm/123456789012">your offer</a>"#;
        assert_eq!(package_from_mail(FROM, Some(true), "", offer), None);
    }

    #[test]
    fn the_click_target_is_built_from_the_ids() {
        assert_eq!(
            trst().tracking_url(),
            "https://www.ebay.com/ord/show?itemId=123456789012&transactionId=10012345678901"
        );
        assert!(is_package_key(&trst().key()));
        assert!(!is_package_key("amazon:113-0000000-0000001:N0FAKE001"));
    }
}
