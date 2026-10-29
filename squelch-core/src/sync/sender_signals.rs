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
//! goes through. Every other signal is a header the sender wrote; that is
//! fine, because each one is shown as what the sender SAID (replies go to X,
//! sent by Y), and a sender who forges one only makes their own mail look
//! stranger.
//!
//! ORGANIZATIONS, NOT HOSTS. `news.example.com` and `example.com` are one
//! sender, so every comparison is between organizational domains
//! ([`org_domain`]): the last two labels, or three under a known two-label
//! public suffix (`example.co.uk`). No public-suffix list is consulted; a miss
//! only ever makes a line show where it need not, never hides one.

use super::ingest::{
    GMAIL_AUTHSERV_ID, addr_domain, dkim_signing_domain, dmarc_header_from, domains_aligned,
    tokenize_auth_results,
};
use crate::types::SenderSignals;

/// Two-label public suffixes common enough to matter. Under these the
/// organization is the last THREE labels.
const TWO_LABEL_SUFFIXES: &[&str] = &[
    "co.uk", "org.uk", "ac.uk", "gov.uk", "me.uk", "ltd.uk", "plc.uk", "com.au", "net.au",
    "org.au", "edu.au", "gov.au", "co.nz", "org.nz", "co.jp", "ne.jp", "or.jp", "co.in", "org.in",
    "com.br", "com.mx", "co.za", "com.cn", "com.sg", "com.tr", "co.kr", "com.ar", "com.hk",
    "com.tw", "co.il", "com.my", "co.id", "com.ph", "com.vn",
];

/// The organizational domain of `domain`, lower-cased.
pub fn org_domain(domain: &str) -> String {
    let d = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    let labels: Vec<&str> = d.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() <= 2 {
        return labels.join(".");
    }
    let last_two = labels[labels.len() - 2..].join(".");
    let keep = if TWO_LABEL_SUFFIXES.contains(&last_two.as_str()) {
        3
    } else {
        2
    };
    labels[labels.len() - keep..].join(".")
}

fn same_org(a: &str, b: &str) -> bool {
    let (a, b) = (org_domain(a), org_domain(b));
    !a.is_empty() && a == b
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
    /// The message carries a `List-Id:` header.
    pub list_id: bool,
    /// The raw value of the trusted `Authentication-Results` header.
    pub trusted_auth_results: Option<&'a str>,
}

/// Read the signals off the extracted headers.
pub fn signals(h: &SenderHeaders<'_>) -> SenderSignals {
    let from = h.from_addr.trim().to_ascii_lowercase();
    let Some(from_domain) = addr_domain(&from) else {
        // No From domain to compare against: nothing can disagree with it.
        return SenderSignals {
            name_spoof: h.from_name.and_then(|n| name_spoof(n, &from)),
            ..SenderSignals::default()
        };
    };

    let sender = h
        .sender
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| s.contains('@') && *s != from);

    let reply_to = if h.list_id {
        // A list's Reply-To pointing at the list is how lists work.
        None
    } else {
        h.reply_to
            .iter()
            .map(|r| r.trim().to_ascii_lowercase())
            .find(|r| addr_domain(r).is_some_and(|d| !same_org(&d, &from_domain)))
    };

    let envelope_domain = h
        .return_path
        .and_then(addr_domain)
        .filter(|d| !same_org(d, &from_domain));

    SenderSignals {
        sender,
        via: h
            .trusted_auth_results
            .and_then(|raw| via_domain(raw, &from_domain)),
        reply_to,
        envelope_domain,
        name_spoof: h.from_name.and_then(|n| name_spoof(n, &from)),
    }
}

/// Read the signals off a parsed message. `None` for mail the account sent:
/// those headers are the owner's own.
pub fn extract(
    msg: &mail_parser::Message,
    from_addr: &str,
    is_sent: bool,
) -> Option<SenderSignals> {
    if is_sent {
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
        list_id: msg.header_raw("List-Id").is_some(),
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
/// in the trusted verdict, but only when nothing aligned with the From
/// organization passed (no aligned DKIM pass, no aligned DMARC pass).
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

/// An address-shaped token inside the display name that is not the From
/// address itself. `"Support" <x@y>` is fine; `"support@paypal.com" <x@evil>`
/// is not.
fn name_spoof(name: &str, from: &str) -> Option<String> {
    name.split(|c: char| c.is_whitespace() || "<>\"'(),;:[]".contains(c))
        .map(|t| t.trim_matches('.').to_ascii_lowercase())
        .find(|t| {
            let Some((local, domain)) = t.rsplit_once('@') else {
                return false;
            };
            !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && t != from
        })
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
            sender: Some("Assistant@Example.com"),
            ..base("boss@example.com")
        });
        assert_eq!(s.sender.as_deref(), Some("assistant@example.com"));
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

        let s = signals(&SenderHeaders {
            reply_to: &rt,
            list_id: true,
            ..base("ceo@company.example")
        });
        assert!(s.reply_to.is_none(), "a list's Reply-To is how lists work");

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
            Sender: relay@evil.example\r\n\
            Subject: hi\r\n\r\nbody\r\n";
        let m = parse(genuine);
        let s = extract(&m, "x@evil.example", false).unwrap();
        assert_eq!(s.via.as_deref(), Some("sendgrid.net"));
        assert_eq!(s.reply_to.as_deref(), Some("pay@elsewhere.example"));
        assert_eq!(s.sender.as_deref(), Some("relay@evil.example"));
        assert_eq!(s.envelope_domain.as_deref(), Some("em.sendgrid.net"));
        assert_eq!(s.name_spoof.as_deref(), Some("support@bank.example"));
        assert_eq!(extract(&m, "x@evil.example", true), None);

        // The same verdict with no Google hop above it is the sender's text.
        let forged = "Authentication-Results: mx.google.com; dkim=pass header.i=@sendgrid.net\r\n\
            From: x@evil.example\r\nSubject: hi\r\n\r\nbody\r\n";
        let m = parse(forged);
        assert!(extract(&m, "x@evil.example", false).unwrap().via.is_none());
    }

    #[test]
    fn the_topmost_return_path_wins() {
        let raw = "Return-Path: <real@bounce.example>\r\n\
            Return-Path: <fake@shop.example>\r\n\
            From: news@shop.example\r\nSubject: hi\r\n\r\nbody\r\n";
        let m = parse(raw);
        let s = extract(&m, "news@shop.example", false).unwrap();
        assert_eq!(s.envelope_domain.as_deref(), Some("bounce.example"));
        let null = "Return-Path: <>\r\nFrom: news@shop.example\r\nSubject: hi\r\n\r\nbody\r\n";
        let m = parse(null);
        assert!(extract(&m, "news@shop.example", false).unwrap().is_empty());
    }
}
