//! Receipt -> open-bill matching: the pure merchant-identity, amount, and
//! recency rules for auto-resolving an open bill when its payment lands. The
//! store-side query and status transition live in [`crate::store::sqlite`].
//!
//! INVARIANT — precision over recall, because a false auto-close hides an unpaid
//! bill: merchant identity must match structurally (never fuzzy or substring),
//! both amounts must agree when both are present, and a receipt with no amount
//! never closes a bill that has one.

/// Max |receipt - bill| in USD for two parsed amounts to be "the same payment":
/// float noise and cent-level rounding only; anything larger is a different
/// transaction.
pub const AMOUNT_TOLERANCE_USD: f64 = 0.02;

/// Recency window for an AMOUNT-VERIFIED match. Billing cycles are monthly; 60
/// days covers a cycle plus a late payment without reaching into stale history.
pub const AMOUNT_MATCH_WINDOW_DAYS: i64 = 60;

/// Recency window for a MERCHANT-ONLY match (bill has no parsed amount).
/// Tighter, because no amount corroborates it.
pub const MERCHANT_ONLY_WINDOW_DAYS: i64 = 30;

/// Freemail domains that never establish MERCHANT identity by domain alone (two
/// strangers share `gmail.com`); for these only full from-address equality does.
/// Calling a domain freemail only ever makes a match stricter, so the list
/// errs long.
const FREEMAIL_DOMAINS: &[&str] = &[
    "gmail.com",
    "googlemail.com",
    "yahoo.com",
    "ymail.com",
    "rocketmail.com",
    "hotmail.com",
    "outlook.com",
    "live.com",
    "msn.com",
    "icloud.com",
    "me.com",
    "mac.com",
    "aol.com",
    "proton.me",
    "protonmail.com",
    "protonmail.ch",
    "pm.me",
    "web.de",
    "mail.ru",
    "inbox.ru",
    "list.ru",
    "bk.ru",
    "ya.ru",
    "qq.com",
    "163.com",
    "126.com",
    "naver.com",
    "zoho.com",
    "zohomail.com",
    "fastmail.com",
    "fastmail.fm",
    "hey.com",
    "tuta.io",
    "tutamail.com",
    // Consumer ISPs: every customer mails From the ISP's own domain.
    "comcast.net",
    "verizon.net",
    "att.net",
    "sbcglobal.net",
    "bellsouth.net",
    "cox.net",
    "charter.net",
    "spectrum.net",
    "earthlink.net",
    "optonline.net",
    "frontier.com",
    "windstream.net",
    "centurylink.net",
    "shaw.ca",
    "rogers.com",
    "telus.net",
    "sympatico.ca",
    "btinternet.com",
    "sky.com",
    "virginmedia.com",
    "talktalk.net",
    "orange.fr",
    "wanadoo.fr",
    "free.fr",
    "sfr.fr",
    "laposte.net",
    "t-online.de",
    "freenet.de",
    "libero.it",
    "bigpond.com",
    "optusnet.com.au",
    "aim.com",
    // The mail.com family: free sign-up under each of these.
    "mail.com",
    "email.com",
    "usa.com",
    "post.com",
    "consultant.com",
    "europe.com",
    "asia.com",
    "dr.com",
    "engineer.com",
    "duck.com",
];

/// Freemail HOSTS whose registrable domain is not itself freemail: Sign in
/// with Apple's relay (`privaterelay.appleid.com`) gives every user of every
/// app an address there, while `appleid.com` alone is Apple's own mail. A
/// host matches itself and any host under it.
const FREEMAIL_HOSTS: &[&str] = &["privaterelay.appleid.com"];

/// Freemail brands registered under many country suffixes (`hotmail.co.uk`,
/// `gmx.de`, `yandex.ru`): a registrable domain whose first label is one of
/// these is freemail whatever follows it.
const FREEMAIL_BRANDS: &[&str] = &[
    "hotmail", "live", "outlook", "yahoo", "gmx", "yandex", "tutanota",
];

/// Two-label PUBLIC (registry) suffixes, not registrable domains: `bar.co.uk`
/// and `baz.co.uk` must NOT match on "co.uk". Pragmatic list, not the full PSL.
const PUBLIC_TWO_LABEL_SUFFIXES: &[&str] = &[
    "co.uk", "org.uk", "ac.uk", "gov.uk", "com.au", "net.au", "org.au", "co.nz", "co.jp", "com.br",
    "co.in",
];

/// Trailing corporate-suffix tokens dropped during name normalization, so
/// "Comcast Inc." and "Comcast" are one merchant. Only TRAILING tokens go.
const CORP_SUFFIX_TOKENS: &[&str] = &[
    "inc",
    "llc",
    "llp",
    "ltd",
    "co",
    "corp",
    "corporation",
    "company",
    "gmbh",
    "sa",
    "plc",
];

/// Normalize a merchant DISPLAY NAME to a comparison key: lowercase, split on
/// every non-alphanumeric char, drop trailing corporate-suffix tokens, re-join
/// with nothing between ("PG&E" and "PGE" both -> "pge"). Empty = "no name".
pub fn normalize_merchant(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut tokens: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    while let Some(last) = tokens.last() {
        if tokens.len() > 1 && CORP_SUFFIX_TOKENS.contains(last) {
            tokens.pop();
        } else {
            break;
        }
    }
    tokens.concat()
}

/// The registrable-ish domain of an email address, lowercased: the last two
/// labels, or three when the last two are a known public suffix. `None` when
/// there is no `@` or too few labels to compare.
///
/// Public because merchant identity is needed beyond bill matching — the
/// shipments store namespaces order references by it — and two definitions of
/// "same merchant" would drift apart.
pub fn registrable_domain(addr: &str) -> Option<String> {
    let domain = addr
        .rsplit('@')
        .next()
        .filter(|d| *d != addr)?
        .to_lowercase();
    let labels: Vec<&str> = domain.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() < 2 {
        return None;
    }
    let last_two = labels[labels.len() - 2..].join(".");
    if PUBLIC_TWO_LABEL_SUFFIXES.contains(&last_two.as_str()) {
        if labels.len() < 3 {
            return None; // the address IS a bare public suffix — nothing to match
        }
        Some(labels[labels.len() - 3..].join("."))
    } else {
        Some(last_two)
    }
}

/// Is this registrable domain (from [`registrable_domain`]) a freemail
/// provider, where the domain says nothing about who sent the mail and only
/// the full address identifies a sender? Shared with the calendar's sender
/// scope (`triage::ghost::sender_scope`) so the two never disagree.
pub(crate) fn is_freemail_domain(registrable: &str) -> bool {
    FREEMAIL_DOMAINS.contains(&registrable)
        || registrable
            .split_once('.')
            .is_some_and(|(brand, _)| FREEMAIL_BRANDS.contains(&brand))
}

/// Is the HOST of this address (or this bare host) a freemail host that
/// [`is_freemail_domain`] cannot see, because its registrable domain is a
/// real organisation's (`x@privaterelay.appleid.com`)?
pub(crate) fn is_freemail_host(address: &str) -> bool {
    let host = address.rsplit('@').next().unwrap_or(address);
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    FREEMAIL_HOSTS
        .iter()
        .any(|h| host == *h || host.strip_suffix(h).is_some_and(|rest| rest.ends_with('.')))
}

/// Minimum normalized-name length for a NAME-ONLY match; shorter keys ("GE")
/// are too collision-prone to establish identity alone.
const MIN_NAME_KEY_LEN: usize = 3;

/// Do a receipt and a bill plausibly come from the SAME MERCHANT? Either a
/// shared registrable domain (on freemail, only the identical full address), or
/// both display names normalizing to the same non-trivial key. No substring or
/// fuzzy matching — a false merchant match risks hiding an unpaid bill.
pub fn merchant_matches(
    receipt_addr: &str,
    receipt_name: Option<&str>,
    bill_addr: &str,
    bill_name: Option<&str>,
) -> bool {
    // 1. Domain identity.
    if let (Some(rd), Some(bd)) = (
        registrable_domain(receipt_addr),
        registrable_domain(bill_addr),
    ) && rd == bd
    {
        if is_freemail_domain(&rd) || is_freemail_host(receipt_addr) || is_freemail_host(bill_addr)
        {
            if receipt_addr.eq_ignore_ascii_case(bill_addr) {
                return true;
            }
        } else {
            return true;
        }
    }
    // 2. Display-name identity.
    if let (Some(rn), Some(bn)) = (receipt_name, bill_name) {
        let (rk, bk) = (normalize_merchant(rn), normalize_merchant(bn));
        if rk.len() >= MIN_NAME_KEY_LEN && rk == bk {
            return true;
        }
    }
    false
}

/// Do two PARSED amounts agree to within [`AMOUNT_TOLERANCE_USD`]?
pub fn amount_matches(receipt: f64, bill: f64) -> bool {
    (receipt - bill).abs() <= AMOUNT_TOLERANCE_USD + f64::EPSILON
}

/// The amount rule plus the recency window it implies, or `None` when the
/// amounts forbid a close: both parsed and agreeing -> the wide window; both
/// parsed and disagreeing -> refuse; receipt missing an amount the bill has ->
/// refuse (the one verifiable number is absent); bill with no parsed amount ->
/// merchant identity carries it alone, inside the tight window.
pub fn amounts_permit_close(receipt: Option<f64>, bill: Option<f64>) -> Option<i64> {
    match (receipt, bill) {
        (Some(r), Some(b)) => amount_matches(r, b).then_some(AMOUNT_MATCH_WINDOW_DAYS),
        (None, Some(_)) => None,
        (_, None) => Some(MERCHANT_ONLY_WINDOW_DAYS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- name normalization ----------------------------------------------

    #[test]
    fn normalizes_case_punctuation_and_suffixes() {
        assert_eq!(normalize_merchant("PG&E"), "pge");
        assert_eq!(normalize_merchant("PGE"), "pge");
        assert_eq!(normalize_merchant("pg&e"), "pge");
        assert_eq!(normalize_merchant("Bay Wheels"), "baywheels");
        assert_eq!(normalize_merchant("Comcast, Inc."), "comcast");
        assert_eq!(normalize_merchant("Acme Co"), "acme");
        assert_eq!(normalize_merchant("  "), "");
    }

    #[test]
    fn suffix_only_name_is_not_emptied() {
        // A name that IS a suffix token keeps its last token (never normalize a
        // real name down to "").
        assert_eq!(normalize_merchant("Co"), "co");
    }

    // ---- domain identity --------------------------------------------------

    #[test]
    fn registrable_domain_strips_subdomains() {
        assert_eq!(
            registrable_domain("billing@pge.com").as_deref(),
            Some("pge.com")
        );
        assert_eq!(
            registrable_domain("receipts@mail.billing.pge.com").as_deref(),
            Some("pge.com")
        );
        assert_eq!(registrable_domain("no-at-sign"), None);
    }

    #[test]
    fn public_suffix_two_label_domains_take_three_labels() {
        // bar.co.uk vs baz.co.uk must NOT collapse to the shared "co.uk".
        assert_eq!(
            registrable_domain("a@billing.bar.co.uk").as_deref(),
            Some("bar.co.uk")
        );
        assert!(!merchant_matches("a@bar.co.uk", None, "b@baz.co.uk", None));
    }

    #[test]
    fn same_company_domain_matches_across_subdomains_and_mailboxes() {
        assert!(merchant_matches(
            "receipts@billing.pge.com",
            None,
            "no-reply@pge.com",
            None
        ));
    }

    #[test]
    fn freemail_domain_alone_never_matches() {
        // Two strangers on gmail are NOT the same merchant.
        assert!(!merchant_matches(
            "alice@gmail.com",
            Some("Alice"),
            "bob@gmail.com",
            Some("Bob")
        ));
        // The SAME gmail sender does match (a landlord billing from gmail).
        assert!(merchant_matches(
            "landlord@gmail.com",
            None,
            "Landlord@Gmail.com",
            None
        ));
    }

    /// Round 3 (N4): consumer ISPs, the mail.com family and Apple's relay
    /// are freemail too, so only the identical address is one merchant. The
    /// relay is matched by host: Apple's own `appleid.com` mail is still one
    /// organisation.
    #[test]
    fn isp_and_relay_addresses_match_only_themselves() {
        for (a, b) in [
            ("alice@comcast.net", "bob@comcast.net"),
            ("alice@verizon.net", "bob@verizon.net"),
            ("alice@aim.com", "bob@aim.com"),
            ("alice@mail.com", "bob@mail.com"),
            ("alice@optusnet.com.au", "bob@optusnet.com.au"),
            ("x1@privaterelay.appleid.com", "x2@privaterelay.appleid.com"),
            ("x1@privaterelay.appleid.com", "no_reply@appleid.com"),
        ] {
            assert!(!merchant_matches(a, None, b, None), "{a} {b}");
            assert!(merchant_matches(a, None, &a.to_uppercase(), None), "{a}");
        }
        assert!(is_freemail_host("x@PrivateRelay.AppleID.com."));
        assert!(!is_freemail_host("no_reply@appleid.com"));
        assert!(!is_freemail_host("x@notprivaterelay.appleid.com"));
        assert!(merchant_matches(
            "no_reply@appleid.com",
            None,
            "billing@id.appleid.com",
            None
        ));
    }

    #[test]
    fn normalized_names_match_across_different_domains() {
        // "PG&E" vs "PGE" with unrelated domains: name identity carries it.
        assert!(merchant_matches(
            "no-reply@pge.com",
            Some("PGE"),
            "billing@pacificgas.com",
            Some("PG&E")
        ));
    }

    #[test]
    fn short_name_keys_do_not_match_alone() {
        // Two-letter keys are too collision-prone for name-only identity.
        assert!(!merchant_matches(
            "a@one.com",
            Some("GE"),
            "b@two.com",
            Some("G.E.")
        ));
    }

    #[test]
    fn unrelated_merchants_do_not_match() {
        assert!(!merchant_matches(
            "receipts@comcast.com",
            Some("Comcast"),
            "billing@pge.com",
            Some("PG&E")
        ));
    }

    // ---- amount rules ------------------------------------------------------

    #[test]
    fn amounts_within_a_couple_cents_match() {
        assert!(amount_matches(84.20, 84.20));
        assert!(amount_matches(84.20, 84.21));
        assert!(amount_matches(84.20, 84.22));
        assert!(!amount_matches(84.20, 84.25));
        assert!(!amount_matches(84.20, 90.00));
    }

    #[test]
    fn amount_rule_picks_window_or_refuses() {
        // Both parsed + agree: wide window.
        assert_eq!(
            amounts_permit_close(Some(84.20), Some(84.20)),
            Some(AMOUNT_MATCH_WINDOW_DAYS)
        );
        // Both parsed + disagree: refuse.
        assert_eq!(amounts_permit_close(Some(12.00), Some(84.20)), None);
        // Bill has an amount, receipt doesn't: refuse (can't verify).
        assert_eq!(amounts_permit_close(None, Some(84.20)), None);
        // Bill has NO amount: merchant-only, tight window (receipt amount moot).
        assert_eq!(
            amounts_permit_close(Some(84.20), None),
            Some(MERCHANT_ONLY_WINDOW_DAYS)
        );
        assert_eq!(
            amounts_permit_close(None, None),
            Some(MERCHANT_ONLY_WINDOW_DAYS)
        );
    }
}
