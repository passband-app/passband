//! The fact check (docs/TRANSLATION.md, invariant 2): every number, URL,
//! email address and code in the source must still be in the translation.
//!
//! The prompt asks for this; the check is what makes it a guarantee rather
//! than a hope. It is a MULTISET comparison over a whole batch: each fact must
//! appear in the translation at least as many times as in the source. Over the
//! batch rather than per segment, because a translation into English may move
//! a word across an inline tag boundary (German puts the verb last), and a
//! number that moved to the neighbouring segment was not lost.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

struct Patterns {
    url: Regex,
    email: Regex,
    digits: Regex,
    code: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        url: Regex::new(r#"(?i)\b(?:https?://|www\.)[^\s<>"']+"#).expect("static regex"),
        email: Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}")
            .expect("static regex"),
        digits: Regex::new(r"\d+").expect("static regex"),
        code: Regex::new(r"\b[A-Z0-9]{5,}\b").expect("static regex"),
    })
}

/// Characters that separate the groups of ONE number: `1.234,56`,
/// `1,234.56`, `1'234`, `1 234` (no-break, narrow and thin spaces too). Dropped
/// only between two digits, on both sides alike, so the two conventions
/// compare equal.
fn is_number_separator(c: char) -> bool {
    matches!(
        c,
        '.' | ',' | '\'' | ' ' | '\u{A0}' | '\u{202F}' | '\u{2009}'
    )
}

/// The facts one text carries, each as the exact string the translation must
/// also contain: digit runs (separators removed), URLs, email addresses, and
/// code-like tokens of 5 or more uppercase letters and digits with at least
/// one of each.
pub fn facts(text: &str) -> Vec<String> {
    let p = patterns();
    let mut out = Vec::new();
    // URLs, emails and codes first, each removed once taken, so the digits
    // inside them are not counted twice (the token already carries them).
    let mut rest = text.to_string();
    for m in p.url.find_iter(text) {
        let url = m
            .as_str()
            .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}']);
        out.push(url.to_string());
    }
    rest = p.url.replace_all(&rest, " ").into_owned();
    for m in p.email.find_iter(&rest) {
        out.push(m.as_str().to_string());
    }
    rest = p.email.replace_all(&rest, " ").into_owned();
    let is_code = |t: &str| {
        t.chars().any(|c| c.is_ascii_digit()) && t.chars().any(|c| c.is_ascii_uppercase())
    };
    for m in p.code.find_iter(&rest) {
        if is_code(m.as_str()) {
            out.push(m.as_str().to_string());
        }
    }
    rest = p
        .code
        .replace_all(&rest, |c: &regex::Captures<'_>| {
            if is_code(&c[0]) {
                " ".to_string()
            } else {
                c[0].to_string()
            }
        })
        .into_owned();
    let joined = join_number_groups(&rest);
    for m in p.digits.find_iter(&joined) {
        out.push(m.as_str().to_string());
    }
    out
}

/// `rest` with every separator that sits between two digits removed.
fn join_number_groups(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        let between_digits = i > 0
            && i + 1 < chars.len()
            && chars[i - 1].is_ascii_digit()
            && chars[i + 1].is_ascii_digit();
        if between_digits && is_number_separator(c) {
            continue;
        }
        out.push(c);
    }
    out
}

/// The source facts the translation does not carry, in source order, each
/// listed once per missing occurrence. Empty means the check passed.
pub fn missing_facts(source: &[String], translated: &[String]) -> Vec<String> {
    let mut have: HashMap<String, usize> = HashMap::new();
    for t in translated {
        for f in facts(t) {
            *have.entry(f).or_default() += 1;
        }
    }
    let mut missing = Vec::new();
    for s in source {
        for f in facts(s) {
            match have.get_mut(&f) {
                Some(n) if *n > 0 => *n -= 1,
                _ => missing.push(f),
            }
        }
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn separators_inside_a_number_are_ignored_across_conventions() {
        assert!(missing_facts(&v(&["Total : 1.234,56 €"]), &v(&["Total: €1,234.56"])).is_empty());
        assert!(missing_facts(&v(&["Betrag 1 234,50"]), &v(&["Amount 1,234.50"])).is_empty());
        // A changed amount is caught.
        assert_eq!(
            missing_facts(&v(&["Total : 1.234,56 €"]), &v(&["Total: €1,234.65"])),
            v(&["123456"])
        );
    }

    #[test]
    fn urls_emails_and_codes_must_survive_verbatim() {
        let src = v(&[
            "Suivez votre colis sur https://track.example.fr/a?id=7, écrivez à aide@example.fr, code ZX81Q9.",
        ]);
        let good = v(&[
            "Track your parcel at https://track.example.fr/a?id=7, write to aide@example.fr, code ZX81Q9.",
        ]);
        assert!(missing_facts(&src, &good).is_empty());
        let bad = v(&["Track your parcel on the website, write to us, code ZX81Q9."]);
        assert_eq!(
            missing_facts(&src, &bad),
            v(&["https://track.example.fr/a?id=7", "aide@example.fr"])
        );
        let bad = v(&[
            "Track your parcel at https://track.example.fr/a?id=7, write to aide@example.fr, code ZX8IQ9.",
        ]);
        assert_eq!(missing_facts(&src, &bad), v(&["ZX81Q9"]));
    }

    #[test]
    fn an_uppercase_word_is_not_a_code() {
        // FACTURE becomes INVOICE; that is translation, not a lost fact.
        assert!(missing_facts(&v(&["FACTURE URGENTE"]), &v(&["URGENT INVOICE"])).is_empty());
    }

    #[test]
    fn counts_matter_and_order_does_not() {
        assert!(missing_facts(&v(&["le 21/10/2026"]), &v(&["on 10/21/2026"])).is_empty());
        assert_eq!(
            missing_facts(
                &v(&["2 adultes et 2 enfants"]),
                &v(&["2 adults and children"])
            ),
            v(&["2"])
        );
        // A number that moved to the neighbouring segment was not lost.
        assert!(
            missing_facts(
                &v(&["Vous avez ", "3 messages"]),
                &v(&["You have 3", "messages"])
            )
            .is_empty()
        );
    }

    #[test]
    fn plain_prose_has_no_facts() {
        assert!(facts("Bonjour, à bientôt !").is_empty());
    }
}
