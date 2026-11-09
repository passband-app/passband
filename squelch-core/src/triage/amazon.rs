//! Amazon packages, identified by Amazon's own per-box id.
//!
//! Amazon's Shipped and Delivered mail prints no carrier tracking number, so
//! the agent's delivery records for it carry `tracking_number: null`, and a
//! record without a number has nothing to key a `shipments` row on. What the
//! mail DOES carry is a "Track package" link to
//! `amazon.com/progress-tracker/package?orderId=…&shipmentId=…`. The
//! `shipmentId` is per box: the same in a box's Shipped and Delivered mail, and
//! different for two boxes of one order. It is the package's identity in all
//! but name, so a box keys on `amazon:<orderId>:<shipmentId>`.
//!
//! THIS IS READ BY CODE, NOT BY THE MODEL. Copying an opaque id out of a
//! redirect URL is exactly what a model gets subtly wrong, and decisions
//! stored before this existed heal without a second model call.
//!
//! TRUST. Only mail whose From domain AUTHENTICATED as amazon.com counts
//! (`ghost::sender_scope`, the same test the calendar uses), and both ids must
//! match Amazon's shapes. The click target is built here from the two ids,
//! never taken from the mail, so a look-alike cannot plant a link. Mail naming
//! more than one box yields nothing rather than a guess.
use crate::triage::text::rx;
use regex::Regex;
use std::collections::BTreeSet;
use std::sync::OnceLock;

/// The prefix every Amazon box key carries in `shipments.tracking_number`.
pub const KEY_PREFIX: &str = "amazon:";

/// One Amazon box: its order and Amazon's id for the shipment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AmazonPackage {
    pub order_id: String,
    pub shipment_id: String,
}

impl AmazonPackage {
    /// The row key: `amazon:<orderId>:<shipmentId>`. Case is kept, since
    /// Amazon's shipment ids are case-sensitive (`NH2K8N5lL`).
    pub fn key(&self) -> String {
        format!("{KEY_PREFIX}{}:{}", self.order_id, self.shipment_id)
    }

    /// Amazon's own package tracker page. It needs the owner's Amazon login,
    /// which is fine: the owner is the only one who clicks it.
    pub fn tracking_url(&self) -> String {
        format!(
            "https://www.amazon.com/progress-tracker/package?orderId={}&shipmentId={}",
            self.order_id, self.shipment_id
        )
    }
}

/// Is this a key [`AmazonPackage::key`] minted?
pub fn is_package_key(number: &str) -> bool {
    number.starts_with(KEY_PREFIX)
}

/// The one Amazon box this mail is about, or `None`: not authenticated
/// amazon.com mail, no tracker link, or links to more than one box.
pub fn package_from_mail(
    from_addr: &str,
    auth_pass: Option<bool>,
    body: &str,
    body_html: &str,
) -> Option<AmazonPackage> {
    if super::ghost::sender_scope(from_addr, auth_pass).as_deref() != Some("amazon.com") {
        return None;
    }
    let mut found = BTreeSet::new();
    for text in [body, body_html] {
        for raw in links(text) {
            if let Some(package) = tracker_link(&raw.replace("&amp;", "&"), 0) {
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

/// Read a progress-tracker link, looking through Amazon's `gp/r.html` click
/// redirect (its `U` parameter) once. A link carrying `itemId` is a per-item
/// feedback link ("How was your delivery?"), whose `shipmentId` is a
/// different, numeric id, so it is skipped.
fn tracker_link(raw: &str, depth: u8) -> Option<AmazonPackage> {
    let url = url::Url::parse(raw).ok()?;
    if !matches!(url.host_str(), Some("www.amazon.com" | "amazon.com")) {
        return None;
    }
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let get = |name: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    match url.path() {
        "/gp/r.html" if depth == 0 => tracker_link(get("U")?, depth + 1),
        "/progress-tracker/package" => {
            if get("itemId").is_some() {
                return None;
            }
            let order_id = get("orderId")?;
            let shipment_id = get("shipmentId")?;
            (is_order_id(order_id) && is_shipment_id(shipment_id)).then(|| AmazonPackage {
                order_id: order_id.to_string(),
                shipment_id: shipment_id.to_string(),
            })
        }
        _ => None,
    }
}

/// Amazon's order number: `113-0000000-0000001`.
fn is_order_id(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 3
        && [3, 7, 7]
            .iter()
            .zip(&parts)
            .all(|(n, p)| p.len() == *n && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Amazon's shipment id: a short ASCII alphanumeric token (`N0FAKE001`).
fn is_shipment_id(s: &str) -> bool {
    (6..=24).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FROM: &str = "shipment-tracking@amazon.com";

    /// The plain-text part of a real Shipped mail, trimmed.
    const SHIPPED_TEXT: &str = "Your package was shipped!\n\nOrder #\n113-0000000-0000001\n\n\
        Track package\nhttps://www.amazon.com/progress-tracker/package?_encoding=UTF8&orderId=113-0000000-0000001&packageIndex=0&shipmentId=N0FAKE001&vt=NOTIFICATIONS&ref_=p_btn_fed_track_package\n";

    /// The HTML part of a real Delivered mail: the tracker behind the click
    /// redirect, plus the two feedback links whose `shipmentId` is another id.
    const DELIVERED_HTML: &str = concat!(
        r#"<a href="https://www.amazon.com/gp/r.html?C=1KB&amp;K=2NG&amp;R=QDG&amp;T=C&amp;U=https%3A%2F%2Fwww.amazon.com%2Fprogress-tracker%2Fpackage%3F_encoding%3DUTF8%26orderId%3D113-0000000-0000001%26packageIndex%3D0%26shipmentId%3DN0FAKE001%26vt%3DNOTIFICATIONS&amp;H=BYA">Track package</a>"#,
        r#"<a href="https://www.amazon.com/gp/r.html?C=1KB&amp;U=https%3A%2F%2Fwww.amazon.com%2Fprogress-tracker%2Fpackage%3F_encoding%3DUTF8%26itemId%3Dfakeitemabcdefg%26orderId%3D113-0000000-0000001%26packageIndex%3D0%26shipmentId%3D500000000000001%26intent%3Dfeedback_happy">Great</a>"#,
        r#"<a href="https://www.amazon.com/gp/r.html?C=1KB&amp;U=https%3A%2F%2Fwww.amazon.com%2Fprogress-tracker%2Fpackage%3F_encoding%3DUTF8%26itemId%3Dfakeitemabcdefg%26orderId%3D113-0000000-0000001%26packageIndex%3D0%26shipmentId%3D500000000000001%26intent%3Dfeedback_unhappy">Not so great</a>"#,
    );

    fn box1() -> AmazonPackage {
        AmazonPackage {
            order_id: "113-0000000-0000001".into(),
            shipment_id: "N0FAKE001".into(),
        }
    }

    #[test]
    fn shipped_and_delivered_mail_name_the_same_box() {
        let shipped = package_from_mail(FROM, Some(true), SHIPPED_TEXT, "");
        let delivered =
            package_from_mail("order-update@amazon.com", Some(true), "", DELIVERED_HTML);
        assert_eq!(shipped, Some(box1()));
        assert_eq!(delivered, Some(box1()), "feedback links are not the box");
        assert_eq!(box1().key(), "amazon:113-0000000-0000001:N0FAKE001");
    }

    #[test]
    fn text_and_html_parts_agreeing_is_one_box() {
        let both = package_from_mail(FROM, Some(true), SHIPPED_TEXT, DELIVERED_HTML);
        assert_eq!(both, Some(box1()));
    }

    #[test]
    fn unauthenticated_or_foreign_senders_name_nothing() {
        for auth in [None, Some(false)] {
            assert_eq!(package_from_mail(FROM, auth, SHIPPED_TEXT, ""), None);
        }
        for from in [
            "shipment-tracking@amazon.com.example.net",
            "deals@notamazon.com",
            "someone@gmail.com",
        ] {
            assert_eq!(
                package_from_mail(from, Some(true), SHIPPED_TEXT, ""),
                None,
                "{from}"
            );
        }
    }

    #[test]
    fn a_tracker_link_on_another_host_is_not_amazons() {
        let lure = SHIPPED_TEXT.replace("www.amazon.com", "www.amazon.com.evil.test");
        assert_eq!(package_from_mail(FROM, Some(true), &lure, ""), None);
    }

    #[test]
    fn two_boxes_in_one_mail_is_no_guess() {
        let two = format!(
            "{SHIPPED_TEXT}\nhttps://www.amazon.com/progress-tracker/package?orderId=113-0000000-0000001&shipmentId=N0FAKE002\n"
        );
        assert_eq!(package_from_mail(FROM, Some(true), &two, ""), None);
    }

    #[test]
    fn malformed_ids_are_refused() {
        for (order, shipment) in [
            ("113-0000000", "N0FAKE001"),
            ("113-0000000-00000O1", "N0FAKE001"),
            ("113-0000000-0000001", "N0F"),
            ("113-0000000-0000001", "N0FA%3Cx%3E"),
            ("113-0000000-0000001", "N0FAKE001N0FAKE001N0FAKE001"),
        ] {
            let text = format!(
                "https://www.amazon.com/progress-tracker/package?orderId={order}&shipmentId={shipment}"
            );
            assert_eq!(
                package_from_mail(FROM, Some(true), &text, ""),
                None,
                "{order} {shipment}"
            );
        }
    }

    #[test]
    fn ordered_mail_without_a_tracker_names_nothing() {
        let ordered = "Ordered: \"ENTTEC DMX USB Pro\"\nOrder #\n113-0000000-0000002\n\
            https://www.amazon.com/gp/your-account/order-details?orderID=113-0000000-0000002\n";
        assert_eq!(
            package_from_mail("auto-confirm@amazon.com", Some(true), ordered, ""),
            None
        );
    }

    #[test]
    fn the_click_target_is_built_from_the_ids() {
        assert_eq!(
            box1().tracking_url(),
            "https://www.amazon.com/progress-tracker/package?orderId=113-0000000-0000001&shipmentId=N0FAKE001"
        );
        assert!(is_package_key(&box1().key()));
        assert!(!is_package_key("TBA303392911000"));
    }
}
