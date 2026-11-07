//! The last rung of package identity: the order itself.
//!
//! A delivery record files under, in order: a carrier tracking number (polled),
//! a store's own package id read from its mail ([`super::amazon`],
//! [`super::ebay`]), and, when the mail has neither, the ORDER: who sent it
//! and which order number it printed, keyed `order:<scope>:<order key>`. That
//! carries a purchase from "Order confirmed" to "Delivered" for any store that
//! never prints a tracking number, with no code per store.
//!
//! WHO SENT IT IS DECIDED BY CODE, not the model. The model names one store
//! four ways across one order's mail ("eBay (Acme Records, Inc.)", "acmerecords",
//! "acmerecords (eBay)"), so the scope is the authenticated sender
//! ([`super::ghost::sender_scope`]). A platform that sends every store's mail
//! from one domain (Shopify, `store+<id>@t.shopifyemail.com`) is scoped by the
//! full address instead, or every store's "#1001" would be one order.
//!
//! THE ORDER NUMBER IS THE MODEL'S, CHECKED BY CODE: it must appear in the
//! mail whole ([`mail_prints`]). A number the model invented, or read off a
//! different mail in the thread, keys nothing.
//!
//! One record keys one row: of its order numbers the mail prints, the one with
//! the smallest key, so the same set of numbers always picks the same order.
use crate::triage::text::rx;
use regex::Regex;
use std::sync::OnceLock;

/// The prefix every order key carries in `shipments.tracking_number`. Like the
/// store package keys it holds a `:`, which no sanitized carrier number can,
/// so the poller never sends it to a carrier.
pub const KEY_PREFIX: &str = "order:";

/// Sending domains that carry many unrelated stores' mail.
const RELAY_DOMAINS: &[&str] = &["shopifyemail.com"];

/// Is this a key [`key`] minted?
pub fn is_key(number: &str) -> bool {
    number.starts_with(KEY_PREFIX)
}

/// Whose order numbers a mail's are: its authenticated sender's domain, or
/// its full address when that domain is shared (freemail, a store platform).
/// `None` when the From domain did not authenticate: a stranger's mail never
/// opens or advances an order card.
pub fn order_scope(from_addr: &str, auth_pass: Option<bool>) -> Option<String> {
    let scope = super::ghost::sender_scope(from_addr, auth_pass)?;
    if RELAY_DOMAINS.contains(&scope.as_str()) {
        return Some(from_addr.trim().to_lowercase());
    }
    Some(scope)
}

/// The row key for one order of one sender.
pub fn key(scope: &str, order_key: &str) -> String {
    format!("{KEY_PREFIX}{scope}:{order_key}")
}

/// Can this order key identify an order? Four characters at least, one of
/// them a digit: "A1" or "ORDER" is not a purchase.
pub fn usable(order_key: &str) -> bool {
    order_key.len() >= 4 && order_key.bytes().any(|b| b.is_ascii_digit())
}

/// Letters-and-digits runs, upper-cased, in order: how both a mail and an
/// order number are compared.
fn chunks(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.chars().flat_map(char::to_uppercase).collect())
}

/// The mail's tokens, for [`mail_prints`]: its text, and its HTML with the
/// markup taken out, so `width="1000"` is not a printed order #1000.
pub fn mail_tokens(text: &str, html: &str) -> Vec<String> {
    static TAG: OnceLock<Regex> = OnceLock::new();
    let visible = TAG
        .get_or_init(|| rx(r"(?s)<[^>]*>"))
        .replace_all(html, " ");
    chunks(text).chain(chunks(&visible)).collect()
}

/// Does the mail print this order number whole? Its letter-and-digit runs
/// must appear as consecutive tokens of the mail, so "01-23456-78901" is found
/// in "Order #01-23456-78901" and "PO/2024/001" in "PO 2024/001", but "#5001"
/// is not in "#50019".
pub fn mail_prints(tokens: &[String], order_ref: &str) -> bool {
    let want: Vec<String> = chunks(order_ref).collect();
    !want.is_empty() && tokens.windows(want.len()).any(|w| w == want.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_is_the_authenticated_sender() {
        assert_eq!(
            order_scope("orders@pillows.example.com", Some(true)).as_deref(),
            Some("example.com")
        );
        assert_eq!(
            order_scope("orders@status.example.org", Some(true)).as_deref(),
            Some("example.org"),
            "subdomains of one store are one store"
        );
        for auth in [None, Some(false)] {
            assert_eq!(order_scope("orders@pillows.example.com", auth), None);
        }
    }

    #[test]
    fn two_shopify_stores_are_two_scopes() {
        let tubes = order_scope("store+111111@t.shopifyemail.com", Some(true));
        let stamps = order_scope("store+222222@t.shopifyemail.com", Some(true));
        assert_eq!(tubes.as_deref(), Some("store+111111@t.shopifyemail.com"));
        assert_ne!(tubes, stamps);
    }

    #[test]
    fn the_key_names_sender_and_order() {
        let k = key("example.com", "4600000017");
        assert_eq!(k, "order:example.com:4600000017");
        assert!(is_key(&k));
        assert!(!is_key("ebay:1:2"));
    }

    #[test]
    fn usable_orders_have_a_digit_and_some_length() {
        assert!(usable("5001"));
        assert!(!usable("A1"));
        assert!(!usable("ORDER"));
    }

    #[test]
    fn the_order_must_be_printed_whole() {
        let tokens = mail_tokens(
            "Order #50019 confirmed.",
            "<p>Order&nbsp;#01-23456-78901, $16.81.</p><td width=\"99999\">PO 2024/001</td>",
        );
        assert!(mail_prints(&tokens, "#50019"));
        assert!(mail_prints(&tokens, "01-23456-78901"));
        assert!(mail_prints(&tokens, "PO/2024/001"), "separators may differ");
        assert!(!mail_prints(&tokens, "5001"), "a prefix is not the order");
        assert!(!mail_prints(&tokens, "15386-01"), "nor a piece of one");
        assert!(!mail_prints(&tokens, "99999"), "markup is not printed");
        assert!(!mail_prints(&tokens, "77777"), "an invented order");
        assert!(!mail_prints(&tokens, "#"), "nothing to find");
    }
}
