//! Where the From line and the real sender disagree (issue #639).
//!
//! The From line is whatever the sender typed. Who actually sent a message is
//! spread over other headers, and when they disagree with From that is one of
//! the best scam tells there is. This module reads them at ingest into a
//! [`SenderSignals`]: facts, never a verdict. The reader shows a quiet line,
//! triage weighs them, and the client alone decides what looks like a scam.
//!
//! TRUST. Only `via` claims anything about authentication, and it is read
//! ONLY from the `Authentication-Results` header that passes
//! [`super::ingest::trusted_auth_results`], the same gate the stored verdict
//! goes through. Every other signal is a header the sender wrote, shown as
//! what the sender SAID (replies go to X, sent by Y). A sender-written header
//! may ADD a signal but never SILENCES one: the mailing-list mute only holds
//! when the Reply-To points at the list itself, so a forged `List-Id` buys
//! nothing.
//!
//! ORGANIZATIONS, NOT HOSTS. `news.example.com` and `example.com` are one
//! sender, so comparisons are between registrable domains from the public
//! suffix list ([`org_domain`]), with IDNs folded to punycode and IP literals
//! compared whole. Two exceptions, because one domain there is many
//! unrelated people: a freemail From (`gmail.com`, `outlook.com`, ...) is
//! compared by full ADDRESS, and a shared-tenant parent (`onmicrosoft.com`)
//! keeps the tenant label.
//!
//! BOUNDS. Every value is an address or a domain, so anything longer than
//! the RFC 5321 limits, or holding whitespace or control characters, is
//! dropped rather than stored: these strings reach the triage prompt and
//! both doors. Format characters (bidi overrides, zero-width) are stripped
//! from display-name text before it is read.

use super::ingest::{
    GMAIL_AUTHSERV_ID, addr_domain, dkim_signing_domain, dmarc_header_from, domains_aligned,
    tokenize_auth_results,
};
use crate::types::SenderSignals;

/// Mailbox providers where one domain is millions of unrelated people, so a
/// shared domain says nothing about a shared sender.
const FREEMAIL: &[&str] = &[
    "gmail.com",
    "googlemail.com",
    "outlook.com",
    "hotmail.com",
    "live.com",
    "msn.com",
    "yahoo.com",
    "ymail.com",
    "icloud.com",
    "me.com",
    "mac.com",
    "aol.com",
    "proton.me",
    "protonmail.com",
    "gmx.com",
    "gmx.de",
    "gmx.net",
    "mail.com",
    "yandex.ru",
    "yandex.com",
    "zoho.com",
    "fastmail.com",
    "hey.com",
    "qq.com",
    "163.com",
];

/// Parents whose every tenant is a separate organization: the tenant label
/// is part of the organization (`contoso.onmicrosoft.com`).
const SHARED_TENANT_PARENTS: &[&str] = &["onmicrosoft.com", "googlegroups.com"];

/// RFC 5321: 254 octets for a path, 253 for a domain.
const MAX_ADDR: usize = 254;
const MAX_DOMAIN: usize = 253;

/// The domain in ASCII (punycode) lower case, without a trailing dot.
fn ascii_domain(domain: &str) -> String {
    let d = domain.trim().trim_end_matches('.');
    if d.starts_with('[') {
        return d.to_ascii_lowercase();
    }
    match url::Host::parse(d) {
        Ok(url::Host::Domain(a)) => a,
        _ => d.to_ascii_lowercase(),
    }
}

/// The organizational domain of `domain`: its registrable domain from the
/// public suffix list, lower-cased ASCII. IP literals are their own
/// organization, compared whole.
pub fn org_domain(domain: &str) -> String {
    let d = ascii_domain(domain);
    if d.starts_with('[') || d.parse::<std::net::IpAddr>().is_ok() {
        return d;
    }
    let reg = psl::domain_str(&d).unwrap_or(&d).to_string();
    if SHARED_TENANT_PARENTS.contains(&reg.as_str()) && d.len() > reg.len() {
        // Keep the tenant: the label just left of the parent.
        let head = &d[..d.len() - reg.len() - 1];
        let tenant = head.rsplit('.').next().unwrap_or(head);
        return format!("{tenant}.{reg}");
    }
    reg
}

fn same_org(a: &str, b: &str) -> bool {
    let (a, b) = (org_domain(a), org_domain(b));
    !a.is_empty() && a == b
}

fn is_freemail(domain: &str) -> bool {
    FREEMAIL.contains(&org_domain(domain).as_str())
}

/// Do these two addresses belong to the same sender? Same organization, or,
/// under a freemail domain, the same address.
fn same_sender(addr: &str, from: &str, from_domain: &str) -> bool {
    let Some(d) = addr_domain(addr) else {
        return false;
    };
    if is_freemail(from_domain) {
        return addr.eq_ignore_ascii_case(from);
    }
    same_org(&d, from_domain)
}

/// An address fit to store: bounded, no whitespace or control characters, a
/// domain present.
fn clean_addr(raw: &str) -> Option<String> {
    let a = raw.trim().to_ascii_lowercase();
    (a.len() <= MAX_ADDR
        && !a.chars().any(|c| c.is_whitespace() || c.is_control())
        && addr_domain(&a).is_some())
    .then_some(a)
}

fn clean_domain(raw: &str) -> Option<String> {
    let d = raw.trim().to_ascii_lowercase();
    (!d.is_empty()
        && d.len() <= MAX_DOMAIN
        && !d.chars().any(|c| c.is_whitespace() || c.is_control()))
    .then_some(d)
}

/// Unicode format characters a display name can use to reorder or hide text:
/// bidi embeddings, overrides and isolates, zero-width joiners and spaces,
/// the soft hyphen and the byte-order mark.
pub fn is_format_char(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
}

/// Display-name text with format characters removed and the lookalikes of
/// `@` and `.` folded to ASCII, so `security＠paypal。com` reads as the
/// address it is drawn as.
fn fold_lookalikes(name: &str) -> String {
    name.chars()
        .filter(|c| !is_format_char(*c))
        .map(|c| match c {
            '\u{FF20}' | '\u{FE6B}' => '@',
            '\u{3002}' | '\u{FF0E}' | '\u{FF61}' | '\u{2024}' | '\u{FE52}' => '.',
            other => other,
        })
        .collect()
}

/// The headers [`signals`] reads, already pulled off the message. Split out so
/// the whole decision is testable on strings.
#[derive(Debug, Default, Clone, Copy)]
pub struct SenderHeaders<'a> {
    pub from_addr: &'a str,
    pub from_name: Option<&'a str>,
    /// The `Sender:` mailbox.
    pub sender: Option<&'a str>,
    /// Every `Reply-To:` mailbox, in order.
    pub reply_to: &'a [String],
    /// The address in the TOPMOST `Return-Path:` (the one the delivering
    /// server wrote). `None` for a null path `<>`.
    pub return_path: Option<&'a str>,
    /// The raw `List-Id:` value, if any (`Eng <eng.lists.example.com>`).
    pub list_id: Option<&'a str>,
    /// The raw `List-Post:` value, if any (`<mailto:eng@lists.example.com>`).
    pub list_post: Option<&'a str>,
    /// The raw value of the trusted `Authentication-Results` header.
    pub trusted_auth_results: Option<&'a str>,
}

/// Read the signals off the extracted headers.
pub fn signals(h: &SenderHeaders<'_>) -> SenderSignals {
    let from = h.from_addr.trim().to_ascii_lowercase();
    let Some(from_domain) = addr_domain(&from) else {
        // No From domain to compare against: nothing can disagree with it.
        return SenderSignals {
            name_spoof: h
                .from_name
                .and_then(|n| name_spoof(n, &from, "", h.list_id.is_some())),
            ..SenderSignals::default()
        };
    };

    let is_list = h.list_id.is_some();
    let list_domains: Vec<String> = h
        .list_id
        .and_then(list_id_domain)
        .into_iter()
        .chain(h.list_post.and_then(list_post_domain))
        .collect();

    // A list's own bounce or relay address, or the sender's own organization,
    // is how mail is delivered, not who sent it.
    let sender = h
        .sender
        .and_then(clean_addr)
        .filter(|s| *s != from && !is_list && !same_sender(s, &from, &from_domain));

    let reply_to = h.reply_to.iter().filter_map(|r| clean_addr(r)).find(|r| {
        // A Reply-To pointing at the list itself is how lists work; a
        // List-Id alone mutes nothing, since the sender writes it.
        let to_list = addr_domain(r).is_some_and(|d| list_domains.iter().any(|l| same_org(&d, l)));
        !same_sender(r, &from, &from_domain) && !to_list
    });

    let envelope_domain = h
        .return_path
        .and_then(addr_domain)
        .and_then(|d| clean_domain(&d))
        .filter(|d| !same_org(d, &from_domain));

    SenderSignals {
        sender,
        via: h
            .trusted_auth_results
            .and_then(|raw| via_domain(raw, &from_domain))
            .and_then(|d| clean_domain(&d)),
        reply_to,
        envelope_domain,
        name_spoof: h
            .from_name
            .and_then(|n| name_spoof(n, &from, &from_domain, is_list)),
    }
}

/// The domain a `List-Id` names: the part inside `<...>`, else the whole
/// value (`Eng <eng.lists.example.com>` -> `eng.lists.example.com`).
fn list_id_domain(raw: &str) -> Option<String> {
    let inner = raw.rsplit_once('<').map_or(raw, |(_, r)| r);
    let inner = inner.split('>').next().unwrap_or(inner).trim();
    (inner.contains('.')).then(|| inner.to_ascii_lowercase())
}

/// The domain of a `List-Post` mailto (`<mailto:eng@lists.example.com>`).
fn list_post_domain(raw: &str) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    let at = lower.find("mailto:")? + "mailto:".len();
    let addr = lower[at..].split(['>', '?', ',']).next()?.trim();
    addr_domain(addr)
}

/// Read the signals off a parsed message. `None` for mail the account sent,
/// and for mail FROM the account's own address arriving in the inbox (a
/// self-addressed note, a send-as alias's echo): those headers are the
/// owner's own.
pub fn extract(
    msg: &mail_parser::Message,
    from_addr: &str,
    is_sent: bool,
    account_addr: &str,
) -> Option<SenderSignals> {
    if is_sent || from_addr.trim().eq_ignore_ascii_case(account_addr.trim()) {
        return None;
    }
    let from_name = msg
        .from()
        .and_then(|a| a.first())
        .and_then(|a| a.name())
        .map(str::to_string);
    let sender = msg
        .sender()
        .and_then(|a| a.first())
        .and_then(|a| a.address())
        .map(str::to_string);
    let mut boxes = Vec::new();
    if let Some(addrs) = msg.reply_to() {
        super::ingest::collect_mailboxes(addrs, &mut boxes);
    }
    let reply_to: Vec<String> = boxes.into_iter().map(|(a, _)| a).collect();
    let return_path = topmost_return_path(msg);
    let headers = SenderHeaders {
        from_addr,
        from_name: from_name.as_deref(),
        sender: sender.as_deref(),
        reply_to: &reply_to,
        return_path: return_path.as_deref(),
        list_id: msg.header_raw("List-Id"),
        list_post: msg.header_raw("List-Post"),
        trusted_auth_results: super::ingest::trusted_auth_results(msg),
    };
    Some(signals(&headers))
}

/// The address in the FIRST `Return-Path:` header in document order: the
/// delivering server prepends it, and copies below it are the sender's text.
fn topmost_return_path(msg: &mail_parser::Message) -> Option<String> {
    let header = msg
        .headers()
        .iter()
        .find(|h| h.name.as_str().eq_ignore_ascii_case("Return-Path"))?;
    let raw = msg
        .raw_message
        .get(header.offset_start as usize..header.offset_end as usize)?;
    let raw = std::str::from_utf8(raw).ok()?.trim();
    let inner = raw
        .trim_start_matches('<')
        .split('>')
        .next()
        .unwrap_or("")
        .trim();
    (inner.contains('@')).then(|| inner.to_ascii_lowercase())
}

/// The DKIM signing domain to name as "via": the first signature that PASSED
/// in the trusted verdict, but only when nothing aligned with the From domain
/// passed: no aligned DKIM pass, no aligned DMARC pass, and no SPF pass whose
/// `smtp.mailfrom` is the From organization (a Workspace domain without its
/// own DKIM key signs as `*.gappssmtp.com` but passes SPF as itself).
/// Alignment is DMARC's relaxed kind, by organization: `mail.brand.com`
/// speaks for `brand.com`. The public suffix list and the shared-tenant rule
/// in [`org_domain`] keep one tenant of a shared parent from speaking for
/// another.
fn via_domain(raw: &str, from_domain: &str) -> Option<String> {
    let fields = tokenize_auth_results(raw)?;
    let mut fields = fields.iter();
    let authserv = fields.next()?.first()?;
    if !authserv.eq_ignore_ascii_case(GMAIL_AUTHSERV_ID) {
        return None;
    }
    let mut unaligned: Option<String> = None;
    for field in fields {
        let Some((method, result)) = field[0].split_once('=') else {
            continue;
        };
        if !result.eq_ignore_ascii_case("pass") {
            continue;
        }
        let props = || field[1..].iter().map(String::as_str);
        if method.eq_ignore_ascii_case("spf") {
            let mailfrom = props()
                .find_map(|p| p.strip_prefix("smtp.mailfrom="))
                .and_then(|v| addr_domain(v).or_else(|| Some(v.to_ascii_lowercase())));
            if mailfrom.is_some_and(|d| same_org(&d, from_domain)) {
                return None;
            }
            continue;
        }
        if method.eq_ignore_ascii_case("dmarc") {
            if dmarc_header_from(props()).is_some_and(|d| domains_aligned(&d, from_domain)) {
                return None;
            }
            continue;
        }
        if method.eq_ignore_ascii_case("dkim")
            && let Some(signing) = dkim_signing_domain(props())
        {
            if same_org(&signing, from_domain) {
                return None;
            }
            unaligned.get_or_insert(signing);
        }
    }
    unaligned
}

/// An address-shaped token inside the display name that belongs to someone
/// other than the sender. `"Support" <x@y>` is fine, and so is the sender's
/// own organization (`"billing@co.com" <no-reply@mail.co.com>`);
/// `"support@paypal.com" <x@evil>` is not. Lookalike `@` and `.` are folded
/// first. A list's rewritten From (`"'ada@x.com' via Eng" <eng@lists...>`)
/// names the real author on purpose and is left alone.
fn name_spoof(name: &str, from: &str, from_domain: &str, is_list: bool) -> Option<String> {
    let folded = fold_lookalikes(name);
    if is_list && folded.to_ascii_lowercase().contains(" via ") {
        return None;
    }
    folded
        .split(|c: char| c.is_whitespace() || "<>\"'(),;:[]".contains(c))
        .map(|t| t.trim_matches('.').to_ascii_lowercase())
        .find(|t| {
            let Some((local, domain)) = t.rsplit_once('@') else {
                return false;
            };
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && t != from
                && !same_sender(t, from, from_domain)
        })
        .and_then(|t| clean_addr(&t))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GMAIL_AR: &str = "mx.google.com;";

    fn base(from: &'static str) -> SenderHeaders<'static> {
        SenderHeaders {
            from_addr: from,
            ..SenderHeaders::default()
        }
    }

    #[test]
    fn organizations_fold_subdomains_and_two_label_suffixes() {
        assert_eq!(org_domain("news.example.com"), "example.com");
        assert_eq!(org_domain("Example.COM."), "example.com");
        assert_eq!(org_domain("mail.shop.example.co.uk"), "example.co.uk");
        assert_eq!(org_domain("localhost"), "localhost");
        // The public suffix list, not a hand table.
        assert_eq!(org_domain("faktury.bank.com.pl"), "bank.com.pl");
        assert_ne!(org_domain("bank.com.pl"), org_domain("evil.com.pl"));
        assert_ne!(org_domain("a.co.th"), org_domain("b.co.th"));
        // Shared-tenant parents keep the tenant.
        assert_eq!(
            org_domain("mail.contoso.onmicrosoft.com"),
            "contoso.onmicrosoft.com"
        );
        assert_ne!(
            org_domain("contoso.onmicrosoft.com"),
            org_domain("evil.onmicrosoft.com")
        );
        // IP literals are compared whole, IDNs as punycode.
        assert_ne!(org_domain("[10.0.1.1]"), org_domain("[192.168.1.1]"));
        assert_eq!(org_domain("münchen.de"), org_domain("xn--mnchen-3ya.de"));
    }

    #[test]
    fn a_freemail_reply_to_another_address_is_flagged() {
        let rt: Vec<String> = vec!["wire.desk22@gmail.com".into()];
        let s = signals(&SenderHeaders {
            reply_to: &rt,
            ..base("jane.cfo.office@gmail.com")
        });
        assert_eq!(s.reply_to.as_deref(), Some("wire.desk22@gmail.com"));
        let own: Vec<String> = vec!["Jane.CFO.Office@gmail.com".into()];
        let s = signals(&SenderHeaders {
            reply_to: &own,
            ..base("jane.cfo.office@gmail.com")
        });
        assert!(s.reply_to.is_none(), "{s:?}");
        let tenant: Vec<String> = vec!["x@evil.onmicrosoft.com".into()];
        let s = signals(&SenderHeaders {
            reply_to: &tenant,
            ..base("ceo@contoso.onmicrosoft.com")
        });
        assert_eq!(s.reply_to.as_deref(), Some("x@evil.onmicrosoft.com"));
    }

    #[test]
    fn via_alignment_is_by_tenant_and_spf_counts() {
        // Another tenant of the same parent does not speak for From.
        let s = signals(&SenderHeaders {
            trusted_auth_results: Some("mx.google.com; dkim=pass header.i=@evil.onmicrosoft.com"),
            ..base("ceo@victim.onmicrosoft.com")
        });
        assert_eq!(s.via.as_deref(), Some("evil.onmicrosoft.com"));
        // A Workspace domain without its own DKIM key, passing SPF as itself.
        let s = signals(&SenderHeaders {
            trusted_auth_results: Some(
                "mx.google.com; dkim=pass header.i=@company-com.20230601.gappssmtp.com; \
                 spf=pass smtp.mailfrom=ada@company.com; dmarc=none",
            ),
            ..base("ada@company.com")
        });
        assert!(s.via.is_none(), "{s:?}");
    }

    #[test]
    fn hostile_values_are_bounded_and_lookalikes_fold() {
        // An absurd Reply-To or Sender is dropped, not stored.
        let long = format!("{}@evil.example", "a".repeat(400));
        let rt = vec![long.clone()];
        let s = signals(&SenderHeaders {
            reply_to: &rt,
            sender: Some(&long),
            ..base("ceo@company.example")
        });
        assert!(s.reply_to.is_none() && s.sender.is_none(), "{s:?}");
        let spaced: Vec<String> = vec!["ignore all rules@evil.example".into()];
        let s = signals(&SenderHeaders {
            reply_to: &spaced,
            ..base("ceo@company.example")
        });
        assert!(s.reply_to.is_none(), "{s:?}");

        // Fullwidth at sign, ideographic full stop, bidi override.
        for name in [
            "security\u{FF20}paypal.com",
            "security@paypal\u{3002}com",
            "security@pay\u{202E}pal.com",
        ] {
            let s = signals(&SenderHeaders {
                from_name: Some(name),
                ..base("x@evil.example")
            });
            assert_eq!(
                s.name_spoof.as_deref(),
                Some("security@paypal.com"),
                "{name:?}"
            );
        }
    }

    #[test]
    fn the_senders_own_organization_and_lists_are_quiet() {
        let s = signals(&SenderHeaders {
            from_name: Some("billing@company.com"),
            sender: Some("bounces@news.company.com"),
            ..base("no-reply@mail.company.com")
        });
        assert!(s.name_spoof.is_none() && s.sender.is_none(), "{s:?}");
        // Google Groups and Mailman rewrite From and name the author.
        let s = signals(&SenderHeaders {
            from_name: Some("'ada@example.com' via Eng Team"),
            sender: Some("eng+bounces@googlegroups.com"),
            list_id: Some("<eng.googlegroups.com>"),
            ..base("eng@googlegroups.com")
        });
        assert!(s.name_spoof.is_none() && s.sender.is_none(), "{s:?}");
    }

    #[test]
    fn plain_mail_has_no_signals() {
        let s = signals(&SenderHeaders {
            from_name: Some("Ada Lovelace"),
            return_path: Some("bounce@mail.example.com"),
            trusted_auth_results: Some(
                "mx.google.com; dkim=pass header.i=@example.com; dmarc=pass header.from=example.com",
            ),
            ..base("ada@example.com")
        });
        assert!(s.is_empty(), "{s:?}");
    }

    #[test]
    fn a_sender_header_naming_someone_else_is_sent_by() {
        let s = signals(&SenderHeaders {
            sender: Some("Assistant@Agency.example"),
            ..base("boss@example.com")
        });
        assert_eq!(s.sender.as_deref(), Some("assistant@agency.example"));
        // A delegate inside the same organization is how mail is delivered.
        let s = signals(&SenderHeaders {
            sender: Some("assistant@example.com"),
            ..base("boss@example.com")
        });
        assert!(s.sender.is_none(), "{s:?}");
        let same = signals(&SenderHeaders {
            sender: Some("BOSS@example.com"),
            ..base("boss@example.com")
        });
        assert!(same.sender.is_none());
    }

    #[test]
    fn an_esp_newsletter_is_via_its_signer_unless_an_aligned_signature_passed() {
        let esp = format!("{GMAIL_AR} dkim=pass header.i=@sendgrid.net; spf=pass");
        let s = signals(&SenderHeaders {
            trusted_auth_results: Some(&esp),
            ..base("news@shop.example")
        });
        assert_eq!(s.via.as_deref(), Some("sendgrid.net"));

        // The same mail also signed by the brand: nothing to say.
        let both = format!(
            "{GMAIL_AR} dkim=pass header.i=@sendgrid.net; dkim=pass header.i=@mail.shop.example"
        );
        let s = signals(&SenderHeaders {
            trusted_auth_results: Some(&both),
            ..base("news@shop.example")
        });
        assert!(s.via.is_none(), "{s:?}");

        // DMARC passing for the From domain also settles it.
        let dmarc = format!(
            "{GMAIL_AR} dkim=pass header.i=@sendgrid.net; dmarc=pass header.from=shop.example"
        );
        let s = signals(&SenderHeaders {
            trusted_auth_results: Some(&dmarc),
            ..base("news@shop.example")
        });
        assert!(s.via.is_none(), "{s:?}");

        // A failed signature names nothing.
        let failed = format!("{GMAIL_AR} dkim=fail header.i=@sendgrid.net");
        let s = signals(&SenderHeaders {
            trusted_auth_results: Some(&failed),
            ..base("news@shop.example")
        });
        assert!(s.via.is_none());
    }

    #[test]
    fn a_verdict_from_anyone_but_the_provider_names_no_via() {
        let s = signals(&SenderHeaders {
            trusted_auth_results: Some("evil.example; dkim=pass header.i=@sendgrid.net"),
            ..base("news@shop.example")
        });
        assert!(s.via.is_none());
    }

    #[test]
    fn reply_to_another_organization_is_flagged_but_not_for_lists() {
        let rt: Vec<String> = vec!["pay@invoices-secure.example".into()];
        let s = signals(&SenderHeaders {
            reply_to: &rt,
            ..base("ceo@company.example")
        });
        assert_eq!(s.reply_to.as_deref(), Some("pay@invoices-secure.example"));

        // A list's Reply-To pointing at the list is how lists work.
        let to_list: Vec<String> = vec!["eng@lists.example.org".into()];
        let s = signals(&SenderHeaders {
            reply_to: &to_list,
            list_id: Some("Eng <eng.lists.example.org>"),
            ..base("ceo@company.example")
        });
        assert!(s.reply_to.is_none(), "{s:?}");
        let s = signals(&SenderHeaders {
            reply_to: &to_list,
            list_post: Some("<mailto:eng@lists.example.org>"),
            ..base("ceo@company.example")
        });
        assert!(s.reply_to.is_none(), "{s:?}");

        // A forged List-Id does not mute a Reply-To somewhere else.
        let s = signals(&SenderHeaders {
            reply_to: &rt,
            list_id: Some("<x.evil.example>"),
            ..base("ceo@company.example")
        });
        assert_eq!(s.reply_to.as_deref(), Some("pay@invoices-secure.example"));

        let own: Vec<String> = vec!["support@help.company.example".into()];
        let s = signals(&SenderHeaders {
            reply_to: &own,
            ..base("ceo@company.example")
        });
        assert!(s.reply_to.is_none(), "same organization");
    }

    #[test]
    fn a_foreign_bounce_domain_is_kept_for_weight() {
        let s = signals(&SenderHeaders {
            return_path: Some("bounces+123@em.sendgrid.net"),
            ..base("news@shop.example")
        });
        assert_eq!(s.envelope_domain.as_deref(), Some("em.sendgrid.net"));
    }

    #[test]
    fn an_address_inside_the_display_name_that_is_not_from_is_a_spoof() {
        let s = signals(&SenderHeaders {
            from_name: Some("support@paypal.com"),
            ..base("x@evil.example")
        });
        assert_eq!(s.name_spoof.as_deref(), Some("support@paypal.com"));

        let s = signals(&SenderHeaders {
            from_name: Some("PayPal Service (security@paypal.com)"),
            ..base("x@evil.example")
        });
        assert_eq!(s.name_spoof.as_deref(), Some("security@paypal.com"));

        // Echoing the real address, or no address at all, is fine.
        for name in [
            "X@Evil.example",
            "Ada Lovelace",
            "Bob @ Work",
            "me@localhost",
        ] {
            let s = signals(&SenderHeaders {
                from_name: Some(name),
                ..base("x@evil.example")
            });
            assert!(s.name_spoof.is_none(), "{name}: {s:?}");
        }
    }

    fn parse(raw: &str) -> mail_parser::Message<'_> {
        mail_parser::MessageParser::default()
            .parse(raw.as_bytes())
            .unwrap()
    }

    /// Whole messages: the trust gate decides `via`, and sent mail reads
    /// nothing.
    #[test]
    fn extract_reads_the_gate_and_skips_sent_mail() {
        let genuine = "Return-Path: <bounce@em.sendgrid.net>\r\n\
            Received: by mx.google.com with SMTP id x; Mon, 1 Jan 2024 00:00:00 -0800\r\n\
            Authentication-Results: mx.google.com; dkim=pass header.i=@sendgrid.net\r\n\
            From: \"support@bank.example\" <x@evil.example>\r\n\
            Reply-To: pay@elsewhere.example\r\n\
            Sender: relay@mailer.example\r\n\
            Subject: hi\r\n\r\nbody\r\n";
        let m = parse(genuine);
        let s = extract(&m, "x@evil.example", false, "me@example.com").unwrap();
        assert_eq!(s.via.as_deref(), Some("sendgrid.net"));
        assert_eq!(s.reply_to.as_deref(), Some("pay@elsewhere.example"));
        assert_eq!(s.sender.as_deref(), Some("relay@mailer.example"));
        assert_eq!(s.envelope_domain.as_deref(), Some("em.sendgrid.net"));
        assert_eq!(s.name_spoof.as_deref(), Some("support@bank.example"));
        assert_eq!(extract(&m, "x@evil.example", true, "me@example.com"), None);
        // Mail from the account's own address is the owner's own headers.
        assert_eq!(extract(&m, "x@evil.example", false, "X@evil.example"), None);

        // The same verdict with no Google hop above it is the sender's text.
        let forged = "Authentication-Results: mx.google.com; dkim=pass header.i=@sendgrid.net\r\n\
            From: x@evil.example\r\nSubject: hi\r\n\r\nbody\r\n";
        let m = parse(forged);
        assert!(
            extract(&m, "x@evil.example", false, "me@example.com")
                .unwrap()
                .via
                .is_none()
        );
    }

    #[test]
    fn the_topmost_return_path_wins() {
        let raw = "Return-Path: <real@bounce.example>\r\n\
            Return-Path: <fake@shop.example>\r\n\
            From: news@shop.example\r\nSubject: hi\r\n\r\nbody\r\n";
        let m = parse(raw);
        let s = extract(&m, "news@shop.example", false, "me@example.com").unwrap();
        assert_eq!(s.envelope_domain.as_deref(), Some("bounce.example"));
        let null = "Return-Path: <>\r\nFrom: news@shop.example\r\nSubject: hi\r\n\r\nbody\r\n";
        let m = parse(null);
        assert!(
            extract(&m, "news@shop.example", false, "me@example.com")
                .unwrap()
                .is_empty()
        );
    }
}
