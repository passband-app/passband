//! The fact check (docs/TRANSLATION.md, invariant 2): every number, URL,
//! email address and code in the source must still be in the translation,
//! and the translation must not carry such facts the source never had.
//!
//! The prompt asks for this; the check is what makes it a guarantee rather
//! than a hope. It is a MULTISET comparison over a whole batch: each fact must
//! appear in the translation at least as many times as in the source. Over the
//! batch rather than per segment, because a translation into English may move
//! a word across an inline tag boundary (German puts the verb last), and a
//! number that moved to the neighbouring segment was not lost.
//!
//! Both sides are NORMALIZED first ([`normalize`]): fullwidth forms and every
//! script's decimal digits become ASCII, so `１２３` and `٣٤٥` compare equal to
//! the `123` and `345` an English translation writes. Every pattern is then
//! ASCII-only, with ASCII word boundaries checked by hand: CJK text glues
//! codes and URLs to the words around them with no space, and a Unicode `\b`
//! would see no boundary there at all.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

struct Patterns {
    url: Regex,
    email: Regex,
    digits: Regex,
    code: Regex,
    marker: Regex,
    month: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        // ASCII printable minus space, quotes and angle brackets: a URL ends
        // at the first CJK character or ideographic punctuation (`。`, `、`,
        // `「`), because none of those is in the class.
        url: Regex::new(r"(?i)(?:https?://|www\.)[!#-&(-;=?-~]+").expect("static regex"),
        email: Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}")
            .expect("static regex"),
        digits: Regex::new(r"[0-9]+").expect("static regex"),
        code: Regex::new(r"[A-Z0-9]{5,}").expect("static regex"),
        marker: Regex::new(r"⟦/?[0-9]+⟧").expect("static regex"),
        month: Regex::new(
            r"\b(?:January|February|March|April|May|June|July|August|September|October|November|December|Jan|Feb|Mar|Apr|Jun|Jul|Aug|Sept|Sep|Oct|Nov|Dec)\b",
        )
        .expect("static regex"),
    })
}

/// Zero of every decimal-digit block an email plausibly carries (the
/// Unicode `Nd` blocks of living scripts). A digit is `zero + n`.
const DIGIT_ZEROS: [u32; 24] = [
    0x0660, 0x06F0, 0x07C0, 0x0966, 0x09E6, 0x0A66, 0x0AE6, 0x0B66, 0x0BE6, 0x0C66, 0x0CE6, 0x0D66,
    0x0DE6, 0x0E50, 0x0ED0, 0x0F20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1B50, 0xFF10,
];

/// `text` with fullwidth ASCII forms, the ideographic space and every
/// decimal digit mapped to ASCII. Everything else is unchanged, so `。` and
/// `、` stay what they are and still end a URL.
pub fn normalize(text: &str) -> String {
    text.chars()
        .map(|c| {
            let u = c as u32;
            if let Some(zero) = DIGIT_ZEROS.iter().find(|&&z| u >= z && u < z + 10) {
                return char::from(b'0' + (u - zero) as u8);
            }
            match u {
                0x3000 => ' ',
                0xFF01..=0xFF5E => char::from_u32(u - 0xFEE0).unwrap_or(c),
                _ => c,
            }
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Url,
    Email,
    Code,
    Digits,
}

#[derive(Debug, Clone)]
struct Fact {
    text: String,
    kind: Kind,
    /// A CJK month number (`10月`, `10월`): a translation may write it as the
    /// English month name instead of the digits.
    month: Option<u8>,
}

/// Whether the char before `start` and the char at `end` leave `s[start..end]`
/// standing as its own ASCII token.
fn ascii_bounded(s: &str, start: usize, end: usize) -> bool {
    let before = s[..start].chars().next_back();
    let after = s[end..].chars().next();
    !before.is_some_and(|c| c.is_ascii_alphanumeric())
        && !after.is_some_and(|c| c.is_ascii_alphanumeric())
}

/// `s` with each byte range replaced by one space.
fn blank_out(s: &str, ranges: &[(usize, usize)]) -> String {
    let mut out = String::with_capacity(s.len());
    let mut at = 0;
    for &(a, b) in ranges {
        out.push_str(&s[at..a]);
        out.push(' ');
        at = b;
    }
    out.push_str(&s[at..]);
    out
}

/// The marker pattern [`super::segment`] parses merged segments with.
pub(crate) fn marker_regex() -> &'static Regex {
    &patterns().marker
}

/// The inline-tag markers (`⟦1⟧`, `⟦/1⟧`) a merged segment carries are not
/// facts, and their digits must not count as numbers.
pub fn strip_markers(text: &str) -> std::borrow::Cow<'_, str> {
    patterns().marker.replace_all(text, "")
}

fn extract(text: &str) -> Vec<Fact> {
    let p = patterns();
    let text = normalize(&strip_markers(text));
    let mut out = Vec::new();
    // URLs, emails and codes first, each removed once taken, so the digits
    // inside them are not counted twice (the token already carries them).
    let mut taken = Vec::new();
    for m in p.url.find_iter(&text) {
        // `xhttps://` is not a URL start; `詳細はhttps://` is.
        if text[..m.start()]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        {
            continue;
        }
        let url = m
            .as_str()
            .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '(', '[']);
        out.push(Fact {
            text: url.to_string(),
            kind: Kind::Url,
            month: None,
        });
        taken.push((m.start(), m.end()));
    }
    let rest = blank_out(&text, &taken);
    let mut taken = Vec::new();
    for m in p.email.find_iter(&rest) {
        out.push(Fact {
            text: m.as_str().to_string(),
            kind: Kind::Email,
            month: None,
        });
        taken.push((m.start(), m.end()));
    }
    let rest = blank_out(&rest, &taken);
    let is_code = |t: &str| {
        t.chars().any(|c| c.is_ascii_digit()) && t.chars().any(|c| c.is_ascii_uppercase())
    };
    let mut taken = Vec::new();
    for m in p.code.find_iter(&rest) {
        if is_code(m.as_str()) && ascii_bounded(&rest, m.start(), m.end()) {
            out.push(Fact {
                text: m.as_str().to_string(),
                kind: Kind::Code,
                month: None,
            });
            taken.push((m.start(), m.end()));
        }
    }
    let rest = blank_out(&rest, &taken);
    let joined = join_number_groups(&rest);
    for m in p.digits.find_iter(&joined) {
        let after = joined[m.end()..].trim_start_matches(' ');
        let month = (m.as_str().len() <= 2 && (after.starts_with('月') || after.starts_with('월')))
            .then(|| m.as_str().parse::<u8>().ok())
            .flatten()
            .filter(|n| (1..=12).contains(n));
        out.push(Fact {
            text: m.as_str().to_string(),
            kind: Kind::Digits,
            month,
        });
    }
    out
}

/// The facts one text carries, each as the exact string the translation must
/// also contain: digit runs (separators removed), URLs, email addresses, and
/// code-like tokens of 5 or more uppercase letters and digits with at least
/// one of each. Normalized as [`normalize`] does.
pub fn facts(text: &str) -> Vec<String> {
    extract(text).into_iter().map(|f| f.text).collect()
}

/// Characters that separate the groups of ONE number, dropped between two
/// digits on both sides alike so the conventions compare equal: `1.234,56`,
/// `1,234.56`, `1'234`, and the time forms `14:30`, `14h30`, `14.30`.
fn is_number_separator(c: char) -> bool {
    matches!(c, '.' | ',' | '\'' | ':' | 'h' | 'H')
}

/// Spaces that group thousands: `1 234`, with no-break, narrow and thin
/// spaces too. Only a thousands group: see [`join_number_groups`].
fn is_space_separator(c: char) -> bool {
    matches!(c, ' ' | '\u{A0}' | '\u{202F}' | '\u{2009}')
}

/// The CJK hour marks of a time (`14時30分`, `14点30分`, `14시 30분`), which an
/// English translation writes as `14:30`.
fn is_hour_mark(c: char) -> bool {
    matches!(c, '時' | '时' | '点' | '點' | '시')
}

/// `s` with every in-number separator removed. Punctuation separators go
/// whenever a digit sits on each side. A SPACE goes only when exactly three
/// digits follow it (a thousands group), so `Zimmer 12 2 Personen` stays two
/// numbers. An hour mark goes with one space after it.
fn join_number_groups(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let digit = |i: usize| chars.get(i).is_some_and(char::is_ascii_digit);
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let after_digit = i > 0 && digit(i - 1);
        if after_digit && digit(i + 1) && is_number_separator(c) {
            i += 1;
            continue;
        }
        if after_digit && is_space_separator(c) {
            let group = (i + 1..).take_while(|&j| digit(j)).count();
            if group == 3 {
                i += 1;
                continue;
            }
        }
        if after_digit && is_hour_mark(c) {
            if digit(i + 1) {
                i += 1;
                continue;
            }
            if chars.get(i + 1) == Some(&' ') && digit(i + 2) {
                i += 2;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// The months (1 to 12) a text names in English.
fn months_named(text: &str) -> Vec<u8> {
    const NAMES: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    patterns()
        .month
        .find_iter(text)
        .filter_map(|m| {
            NAMES
                .iter()
                .position(|n| m.as_str().starts_with(n))
                .map(|i| i as u8 + 1)
        })
        .collect()
}

fn multiset<'a>(items: impl Iterator<Item = &'a str>) -> HashMap<String, usize> {
    let mut have: HashMap<String, usize> = HashMap::new();
    for f in items {
        *have.entry(f.to_string()).or_default() += 1;
    }
    have
}

fn take(have: &mut HashMap<String, usize>, key: &str) -> bool {
    match have.get_mut(key) {
        Some(n) if *n > 0 => {
            *n -= 1;
            true
        }
        _ => false,
    }
}

/// The source facts the translation does not carry, in source order, each
/// listed once per missing occurrence. Empty means the check passed.
///
/// A CJK month number (`2026年10月21日`) is also satisfied by the English
/// month name (`October 21, 2026`): the month is the one fact a faithful
/// English date legitimately writes as a word. Digit-for-digit matches are
/// taken first, so a month never uses up a number another fact needed.
pub fn missing_facts(source: &[String], translated: &[String]) -> Vec<String> {
    let t_facts: Vec<Fact> = translated.iter().flat_map(|t| extract(t)).collect();
    let mut have = multiset(t_facts.iter().map(|f| f.text.as_str()));
    let mut months: HashMap<u8, usize> = HashMap::new();
    for t in translated {
        for m in months_named(&strip_markers(t)) {
            *months.entry(m).or_default() += 1;
        }
    }
    let s_facts: Vec<Fact> = source.iter().flat_map(|s| extract(s)).collect();
    let mut missing: Vec<(usize, String)> = Vec::new();
    let mut month_facts = Vec::new();
    for (i, f) in s_facts.iter().enumerate() {
        if f.month.is_some() {
            month_facts.push(i);
        } else if !take(&mut have, &f.text) {
            missing.push((i, f.text.clone()));
        }
    }
    for i in month_facts {
        let f = &s_facts[i];
        let by_name =
            |months: &mut HashMap<u8, usize>| match f.month.and_then(|m| months.get_mut(&m)) {
                Some(n) if *n > 0 => {
                    *n -= 1;
                    true
                }
                _ => false,
            };
        if !take(&mut have, &f.text) && !by_name(&mut months) {
            missing.push((i, f.text.clone()));
        }
    }
    missing.sort_by_key(|(i, _)| *i);
    missing.into_iter().map(|(_, f)| f).collect()
}

/// The facts the translation carries that the source does not: any URL,
/// email address or code, and any number of four or more digits, absent
/// from the source (as a multiset). A translation has no business inventing
/// any of these, and an injected "call +1 555 0100" is exactly this shape.
/// Shorter numbers are let through: a translation writes `3` for `trois`.
pub fn added_facts(source: &[String], translated: &[String]) -> Vec<String> {
    let s_facts: Vec<Fact> = source.iter().flat_map(|s| extract(s)).collect();
    let mut have = multiset(s_facts.iter().map(|f| f.text.as_str()));
    let mut added = Vec::new();
    for t in translated {
        for f in extract(t) {
            let checked = f.kind != Kind::Digits || f.text.len() >= 4;
            if checked && !take(&mut have, &f.text) {
                added.push(f.text);
            }
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn passes(src: &str, out: &str) -> bool {
        missing_facts(&v(&[src]), &v(&[out])).is_empty()
    }

    #[test]
    fn separators_inside_a_number_are_ignored_across_conventions() {
        assert!(passes("Total : 1.234,56 €", "Total: €1,234.56"));
        assert!(passes("Betrag 1 234,50", "Amount 1,234.50"));
        assert!(passes("Betrag 1\u{202F}234,50", "Amount 1,234.50"));
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
        assert!(added_facts(&src, &good).is_empty());
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
        assert!(passes("FACTURE URGENTE", "URGENT INVOICE"));
    }

    #[test]
    fn counts_matter_and_order_does_not() {
        assert!(passes("le 21/10/2026", "on 10/21/2026"));
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

    /// The probe cases from the review, each of which used to fail a faithful
    /// translation (and buy a retry).
    #[test]
    fn cjk_text_glued_to_codes_and_urls() {
        assert_eq!(facts("ご注文番号ABC12345です"), v(&["ABC12345"]));
        assert!(passes(
            "ご注文番号ABC12345です",
            "Your order number is ABC12345."
        ));
        assert!(passes(
            "詳細はhttps://example.jp/a/1をご覧ください",
            "See https://example.jp/a/1 for details."
        ));
        assert_eq!(
            facts("詳細：https://example.jp/a/1。よろしく"),
            v(&["https://example.jp/a/1"])
        );
        assert!(passes(
            "詳細：https://example.jp/a/1。よろしく",
            "Details: https://example.jp/a/1. Best regards."
        ));
        assert_eq!(
            facts("「https://example.jp/x」、または"),
            v(&["https://example.jp/x"])
        );
        assert_eq!(facts("連絡先：info@example.jp。"), v(&["info@example.jp"]));
        // An ASCII letter still glues: that is a word, not a code.
        assert!(facts("xABC12345").iter().all(|f| f != "ABC12345"));
    }

    #[test]
    fn fullwidth_and_other_script_digits_are_normalized() {
        assert!(passes("金額は１２３円", "The amount is 123 yen."));
        assert!(passes("المبلغ ٣٤٥ ريال", "The amount is 345 riyals."));
        assert!(passes("مبلغ ۱۲۳۴ تومان", "The amount is 1234 toman."));
        assert!(passes("राशि ९९ रुपये", "Amount 99 rupees."));
        assert!(passes("注文番号ＡＢＣ１２３４５", "Order number ABC12345"));
        assert!(!passes("金額は１２３円", "The amount is 124 yen."));
    }

    #[test]
    fn times_compare_across_separators() {
        assert!(passes("um 14.30 Uhr", "at 14:30"));
        assert!(passes("à 14h30", "at 14:30"));
        assert!(passes("14時30分に", "at 14:30"));
        assert!(passes("14点30分", "at 14:30"));
        assert!(passes("14시 30분에", "at 14:30"));
        assert!(!passes("um 14.30 Uhr", "at 14:40"));
    }

    #[test]
    fn a_space_joins_only_a_thousands_group() {
        assert!(passes("Zimmer 12 2 Personen", "Room 12, 2 people"));
        assert_eq!(
            facts("Zimmer 12 2 Personen"),
            v(&["12", "2"]),
            "two numbers, not 122"
        );
        assert_eq!(facts("1 234 567 €"), v(&["1234567"]));
        assert_eq!(facts("12 3456"), v(&["12", "3456"]));
    }

    #[test]
    fn a_cjk_month_may_become_its_english_name() {
        assert!(passes("2026年10月21日に発送", "Ships on October 21, 2026."));
        assert!(passes("2026年10月21日に発送", "Ships on Oct 21, 2026."));
        assert!(passes("2026年10月21日に発送", "Ships on 2026/10/21."));
        assert!(passes("2026년 3월 5일", "March 5, 2026"));
        // The wrong month, or a lost day, is still caught.
        assert_eq!(
            missing_facts(
                &v(&["2026年10月21日に発送"]),
                &v(&["Ships on November 21, 2026."])
            ),
            v(&["10"])
        );
        assert_eq!(
            missing_facts(&v(&["2026年10月21日"]), &v(&["October 2026"])),
            v(&["21"])
        );
        // Only a month number is lenient: "10 people" needs its 10.
        assert!(!passes("10人", "October people"));
    }

    #[test]
    fn markers_are_not_facts() {
        assert!(facts("⟦1⟧Bonjour⟦/1⟧ ⟦2⟧Marie⟦/2⟧").is_empty());
        assert_eq!(facts("Code ⟦1⟧ZX81Q9⟦/1⟧"), v(&["ZX81Q9"]));
    }

    #[test]
    fn additions_are_caught() {
        let src = v(&["Votre colis arrive demain."]);
        assert_eq!(
            added_facts(
                &src,
                &v(&["Your parcel arrives tomorrow. Call +1 555 0100."])
            ),
            v(&["1555", "0100"])
        );
        assert_eq!(
            added_facts(
                &src,
                &v(&[
                    "Your parcel arrives tomorrow, see https://evil.example/x or mail a@evil.example"
                ])
            ),
            v(&["https://evil.example/x", "a@evil.example"])
        );
        assert_eq!(
            added_facts(&src, &v(&["Your parcel AB12345 arrives tomorrow."])),
            v(&["AB12345"])
        );
        // Small numbers are translation: "trois" is 3.
        assert!(added_facts(&v(&["trois colis"]), &v(&["3 parcels"])).is_empty());
        // A date's year, a CJK month spelled out: nothing new.
        assert!(
            added_facts(
                &v(&["2026年10月21日に発送"]),
                &v(&["Ships on October 21, 2026."])
            )
            .is_empty()
        );
        assert!(added_facts(&v(&["14時30分に"]), &v(&["at 14:30"])).is_empty());
        // Repeating a source number more often than the source did is new.
        assert_eq!(
            added_facts(&v(&["Total 1234"]), &v(&["Total 1234, again 1234"])),
            v(&["1234"])
        );
    }
}
