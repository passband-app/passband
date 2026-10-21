//! The fact check (docs/TRANSLATION.md, invariant 2): every number, URL,
//! email address and code in the source must still be in the translation,
//! and the translation must not carry such facts the source never had.
//!
//! The prompt asks for this; the check is what makes it a guarantee rather
//! than a hope. It runs over a whole batch rather than per segment, because a
//! translation into English may move a word across an inline tag boundary
//! (German puts the verb last), and a number that moved to the neighbouring
//! segment was not lost.
//!
//! EVERY FACT HAS A SET OF KEYS, the forms a faithful translation may write
//! it in, and two facts are the same fact when they share a key:
//!
//! - A plain number is its digits, separators removed (`1.234,56` and
//!   `1,234.56` are both `123456`), leading zeros dropped.
//! - A number with a magnitude unit is ALSO its value: `3万` is `3` and
//!   `30000`, `1,5 Mio.` is `15` and `1500000`, `30 thousand` is `30` and
//!   `30000`. English has no 万, so the faithful English of `3万円` is
//!   `30,000 yen`.
//! - A time is its (hour, minute): `18 Uhr`, `18h`, `14 h 30`, `14.30 Uhr`,
//!   `14時30分`, `午後3時` against `18:00`, `6 PM`, `2:30 p.m.`. An hour of 12
//!   or less with no a.m./p.m. may be either half of the day.
//! - A date's day, month and year are three numbers (`21.10.2026` is NOT the
//!   one number `21102026`), and a month number may be written as the English
//!   month name, as may a CJK `10月`.
//! - A code compares case-insensitively (`abc12345` against `ABC12345`).
//!
//! MISSING is a maximum matching between the source's facts and the
//! translation's over shared keys: each source fact needs its own partner.
//!
//! ADDED is a translation fact that shares no key with anything the source
//! says, the source's DERIVED values included: kanji numerals (`三千` is
//! 3000, `二〇二六` is 2026), Japanese era years (`令和8年` is 2026), every
//! magnitude's value, and the source's phone numbers. Additions come in two
//! weights ([`Check`]):
//!
//! - an added URL, email address, code, or number of seven or more digits
//!   (a phone number's length) is what an injected "call +1 555 0100" looks
//!   like, and REJECTS the translation if the retry still writes it;
//! - any other added number (4 to 6 digits, a magnitude, a time) is most
//!   likely a number word the source spelled out (`deux mille`, `dix-huit
//!   heures`), which this check does not try to parse in every language: it
//!   makes the translation `incomplete` with a note naming the numbers, and
//!   is served with the warning. Numbers of 1 to 3 digits are let through:
//!   a translation writes `3` for `trois`.
//!
//! Both sides are NORMALIZED first ([`normalize`]): fullwidth forms and every
//! script's decimal digits become ASCII, so `１２３` and `٣٤٥` compare equal to
//! the `123` and `345` an English translation writes. Every pattern is then
//! ASCII-only where it can be, with ASCII word boundaries checked by hand: CJK
//! text glues codes and URLs to the words around them with no space, and a
//! Unicode `\b` would see no boundary there at all.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use regex::Regex;

struct Patterns {
    url: Regex,
    email: Regex,
    token: Regex,
    marker: Regex,
    month: Regex,
    era: Regex,
    time_cjk: Regex,
    time_uhr: Regex,
    time_meridiem: Regex,
    time_h: Regex,
    time_word: Regex,
    time_colon: Regex,
    date_ymd: Regex,
    date_dmy: Regex,
    cjk_magnitude: Regex,
    word_magnitude: Regex,
    kanji: Regex,
}

/// Magnitude words after a number, with the power(s) of ten each may mean.
/// Two powers where languages disagree: `billion` is 10^9 in English and
/// 10^12 in German and French; `mil` is a thousand in Spanish and Portuguese
/// and an informal million in English.
const MAGNITUDE_WORDS: &[(&str, &[u32])] = &[
    ("thousand", &[3]),
    ("million", &[6]),
    ("millions", &[6]),
    ("billion", &[9, 12]),
    ("billions", &[9, 12]),
    ("trillion", &[12]),
    ("mio", &[6]),
    ("mrd", &[9]),
    ("tsd", &[3]),
    ("tausend", &[3]),
    ("millionen", &[6]),
    ("milliarde", &[9]),
    ("milliarden", &[9]),
    ("milliard", &[9]),
    ("milliards", &[9]),
    ("millón", &[6]),
    ("millon", &[6]),
    ("millones", &[6]),
    ("mil", &[3, 6]),
    ("mila", &[3]),
    ("milione", &[6]),
    ("milioni", &[6]),
    ("miliardo", &[9]),
    ("miliardi", &[9]),
    ("milhão", &[6]),
    ("milhões", &[6]),
    ("bilhão", &[9]),
    ("bilhões", &[9]),
    ("miljoen", &[6]),
    ("miljard", &[9]),
    ("тыс", &[3]),
    ("тысяча", &[3]),
    ("тысячи", &[3]),
    ("тысяч", &[3]),
    ("млн", &[6]),
    ("млрд", &[9]),
    ("миллион", &[6]),
    ("миллиона", &[6]),
    ("миллионов", &[6]),
    ("миллиард", &[9]),
    ("миллиарда", &[9]),
    ("миллиардов", &[9]),
    ("lakh", &[5]),
    ("lakhs", &[5]),
    ("crore", &[7]),
    ("crores", &[7]),
    ("लाख", &[5]),
    ("करोड़", &[7]),
    ("k", &[3]),
];

/// The CJK magnitude characters and their power of ten.
fn cjk_unit(c: char) -> Option<u32> {
    match c {
        '十' => Some(1),
        '百' | '백' => Some(2),
        '千' | '천' => Some(3),
        '万' | '萬' | '만' => Some(4),
        '億' | '亿' | '억' => Some(8),
        '兆' | '조' => Some(12),
        _ => None,
    }
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| {
        // Longest first: the regex engine takes the first alternative that
        // matches, and `mil` must not win over `milioni`.
        let mut words: Vec<&str> = MAGNITUDE_WORDS.iter().map(|(w, _)| *w).collect();
        words.sort_by_key(|w| std::cmp::Reverse(w.chars().count()));
        words.dedup();
        let words: Vec<String> = words.iter().map(|w| regex::escape(w)).collect();
        Patterns {
            // ASCII printable minus space, quotes and angle brackets: a URL
            // ends at the first CJK character or ideographic punctuation (`。`,
            // `、`, `「`), because none of those is in the class.
            url: Regex::new(r"(?i)(?:https?://|www\.)[!#-&(-;=?-~]+").expect("static regex"),
            email: Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}")
                .expect("static regex"),
            token: Regex::new(r"[A-Za-z0-9]{5,}").expect("static regex"),
            marker: Regex::new(r"⟦/?[0-9]+⟧").expect("static regex"),
            month: Regex::new(
                r"\b(?:January|February|March|April|May|June|July|August|September|October|November|December|Jan|Feb|Mar|Apr|Jun|Jul|Aug|Sept|Sep|Oct|Nov|Dec)\b",
            )
            .expect("static regex"),
            era: Regex::new(r"(令和|平成|昭和)\s*([0-9]+|元|[〇一二三四五六七八九十]+)\s*年")
                .expect("static regex"),
            time_cjk: Regex::new(
                r"(午前|午後|上午|下午|中午|早上|晚上|오전|오후)?\s*([0-9]{1,2})\s*[時时点點시](?:\s*([0-9]{1,2})\s*[分분]|(半))?",
            )
            .expect("static regex"),
            time_uhr: Regex::new(r"([0-9]{1,2})(?:[.:]([0-9]{2}))?\s?Uhr(?:\s([0-9]{2}))?")
                .expect("static regex"),
            time_meridiem: Regex::new(r"([0-9]{1,2})(?:[:.]([0-9]{2}))?\s?([AaPp])\.?\s?[Mm]\.?")
                .expect("static regex"),
            time_h: Regex::new(r"([0-9]{1,2})\s?[hH](?:\s?([0-9]{2}))?").expect("static regex"),
            time_word: Regex::new(r"([0-9]{1,2})\s?(?:heures?|o'clock|o’clock)")
                .expect("static regex"),
            time_colon: Regex::new(r"([0-9]{1,2}):([0-9]{2})(?::[0-9]{2})?").expect("static regex"),
            date_ymd: Regex::new(r"([0-9]{4})([-/.])([0-9]{1,2})([-/.])([0-9]{1,2})")
                .expect("static regex"),
            date_dmy: Regex::new(r"([0-9]{1,2})([-/.])([0-9]{1,2})([-/.])([0-9]{4}|[0-9]{2})")
                .expect("static regex"),
            cjk_magnitude: Regex::new(
                r"[0-9]+(?:[.,][0-9]+)?\s?[十百千万萬億亿兆백천만억조](?:\s?[0-9]+(?:[.,][0-9]+)?\s?[百千万萬億亿兆백천만억조])*[0-9]*",
            )
            .expect("static regex"),
            word_magnitude: Regex::new(&format!(
                r"([0-9]+(?:[.,'][0-9]+|[ \u{{A0}}\u{{202F}}][0-9]{{3}})*)(\s?)(?i:({}))\.?",
                words.join("|")
            ))
            .expect("static regex"),
            kanji: Regex::new(r"[〇零一二三四五六七八九十百千万萬億亿两兩]+").expect("static regex"),
        }
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
    /// Uppercase letters and digits, no lowercase: an order, booking or
    /// tracking code. Verbatim, case aside.
    Code,
    /// Letters and digits with lowercase in it (`abc12345`, `iPhone15`): a
    /// code to a translation that keeps it, its digits to one that does not.
    Token,
    Digits,
    /// A number with a magnitude unit (`3万`, `1,5 Mio.`, `30 thousand`).
    Magnitude,
    Time,
    /// An English month name. Only the translation's are extracted: they
    /// satisfy a source month number, and are never an addition.
    Month,
}

#[derive(Debug, Clone)]
struct Fact {
    /// What a "missing" list names: the digits for a number, the token
    /// itself otherwise.
    text: String,
    /// The text as written, for the "added numbers" note.
    raw: String,
    kind: Kind,
    keys: Vec<String>,
    /// Literal digits in it, for the weight of an addition.
    digits: usize,
}

/// A run of numbers separated only by spaces, dashes, dots or parentheses,
/// seven digits or more together: how a phone number is written.
#[derive(Debug, Clone)]
struct Run {
    raw: String,
    digits: String,
    /// Indices into the extraction's `facts`.
    members: Vec<usize>,
}

#[derive(Debug, Default)]
struct Extracted {
    facts: Vec<Fact>,
    /// Values the text implies without a digit for them (kanji numerals, era
    /// years written in kanji, the parts of a compound amount): they explain
    /// an addition, and are never required.
    implied: Vec<String>,
    runs: Vec<Run>,
}

/// Digits with leading zeros dropped (`0` stays `0`): `01` is the day `1`.
fn strip_zeros(d: &str) -> &str {
    let s = d.trim_start_matches('0');
    if s.is_empty() && !d.is_empty() {
        "0"
    } else {
        s
    }
}

fn num_key(d: &str) -> String {
    format!("n:{}", strip_zeros(d))
}

fn digits_of(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

fn is_ascii_alnum_at_end(s: &str) -> bool {
    s.chars()
        .next_back()
        .is_some_and(|c| c.is_ascii_alphanumeric())
}

/// Whether the char before `start` and the char at `end` leave `s[start..end]`
/// standing as its own ASCII token.
fn ascii_bounded(s: &str, start: usize, end: usize) -> bool {
    !is_ascii_alnum_at_end(&s[..start])
        && !s[end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
}

/// Whether a number starting at `start` is not the tail of a longer one:
/// no digit before it, and no `1.` / `1,` / `1/` / `1-` glued on.
fn number_starts(s: &str, start: usize) -> bool {
    let mut back = s[..start].chars().rev();
    match back.next() {
        Some(c) if c.is_ascii_digit() => false,
        Some('.' | ',' | '/' | '-' | ':') => !back.next().is_some_and(|c| c.is_ascii_digit()),
        _ => true,
    }
}

/// Whether a number ending at `end` is not continued: no digit after it, and
/// no `.5` / `,5` / `/5` / `-5` / `:5` following.
fn number_ends(s: &str, end: usize) -> bool {
    let mut fwd = s[end..].chars();
    match fwd.next() {
        Some(c) if c.is_ascii_digit() => false,
        Some('.' | ',' | '/' | '-' | ':') => !fwd.next().is_some_and(|c| c.is_ascii_digit()),
        _ => true,
    }
}

/// `s` with each byte range replaced by as many spaces as it had bytes, so
/// offsets into `s` stay valid. Ranges must be on char boundaries and not
/// overlap.
fn blank(s: &mut String, ranges: &[(usize, usize)]) {
    if ranges.is_empty() {
        return;
    }
    let mut out = String::with_capacity(s.len());
    let mut at = 0;
    let mut sorted = ranges.to_vec();
    sorted.sort_unstable();
    for (a, b) in sorted {
        if a < at {
            continue;
        }
        out.push_str(&s[at..a]);
        out.extend(std::iter::repeat_n(' ', b - a));
        at = b;
    }
    out.push_str(&s[at..]);
    *s = out;
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

/// The value of a kanji numeral string: positional digits (`二〇二六` is
/// 2026) or the unit form (`三千五百`, `一万`, `十月` is 10, `二十一` is 21).
fn kanji_value(run: &str) -> Option<u128> {
    let digit = |c: char| -> Option<u128> {
        Some(match c {
            '〇' | '零' => 0,
            '一' => 1,
            '二' | '两' | '兩' => 2,
            '三' => 3,
            '四' => 4,
            '五' => 5,
            '六' => 6,
            '七' => 7,
            '八' => 8,
            '九' => 9,
            _ => return None,
        })
    };
    if !run.chars().any(|c| cjk_unit(c).is_some()) {
        let mut v: u128 = 0;
        for c in run.chars() {
            v = v.checked_mul(10)?.checked_add(digit(c)?)?;
        }
        return Some(v);
    }
    let (mut total, mut section, mut cur): (u128, u128, Option<u128>) = (0, 0, None);
    for c in run.chars() {
        if let Some(d) = digit(c) {
            cur = Some(cur.map_or(Some(d), |v| v.checked_mul(10)?.checked_add(d))?);
            continue;
        }
        let pow = cjk_unit(c)?;
        let unit = 10u128.checked_pow(pow)?;
        if pow >= 4 {
            let part = section + cur.unwrap_or(0);
            total = total.checked_add(if part == 0 { 1 } else { part }.checked_mul(unit)?)?;
            section = 0;
        } else {
            section = section.checked_add(cur.unwrap_or(1).checked_mul(unit)?)?;
        }
        cur = None;
    }
    total.checked_add(section)?.checked_add(cur.unwrap_or(0))
}

/// The ways a written amount (`1,5`, `1.250`, `12 345`, `1.234,56`) may be
/// read, each as (digits, decimal places). A last separator followed by
/// exactly three digits is either a thousands mark or a decimal point, so
/// both readings come back.
fn mantissa_readings(s: &str) -> Vec<(u128, u32)> {
    let s: String = s
        .chars()
        .filter(|c| !matches!(c, ' ' | '\'' | '\u{A0}' | '\u{202F}'))
        .collect();
    let all: Option<u128> = digits_of(&s).parse().ok();
    let Some(all) = all else {
        return Vec::new();
    };
    let Some(last) = s.rfind(['.', ',']) else {
        return vec![(all, 0)];
    };
    let tail = s.len() - last - 1;
    if tail == 3 {
        let same = s.matches(&s[last..=last]).count();
        if same > 1 {
            return vec![(all, 0)];
        }
        return vec![(all, 0), (all, 3)];
    }
    vec![(all, tail as u32)]
}

fn scaled(mantissa: (u128, u32), pow: u32) -> Option<u128> {
    let (digits, dec) = mantissa;
    let v = digits.checked_mul(10u128.checked_pow(pow)?)?;
    let div = 10u128.checked_pow(dec)?;
    (v % div == 0).then(|| v / div)
}

/// A time's keys: `t:H:MM` (both halves of the day for an hour of 12 or
/// less with no a.m./p.m.), and its digits as a plain number, so a
/// translation that writes `18.00` or `18 o'clock` still carries it.
fn time_keys(hour: u32, minute: u32, meridiem: Option<bool>, literal_digits: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let h = match meridiem {
        Some(true) => hour % 12 + 12,
        Some(false) => hour % 12,
        None => hour,
    };
    keys.push(format!("t:{h}:{minute:02}"));
    if meridiem.is_none() && (1..=12).contains(&hour) {
        keys.push(format!("t:{}:{minute:02}", (hour + 12) % 24));
    }
    keys.push(num_key(literal_digits));
    keys.push(num_key(&format!("{hour}{minute:02}")));
    keys.dedup();
    keys
}

/// THE EXTRACTION, one text. `translated` turns on the English month names,
/// which only the translation's side has use for.
fn extract(text: &str, translated: bool) -> Extracted {
    let p = patterns();
    let t = normalize(&strip_markers(text));
    let mut s = t.clone();
    let mut out = Extracted::default();

    // URLs, emails and codes first, each blanked once taken, so the digits
    // inside them are not counted again (the token already carries them).
    let mut taken = Vec::new();
    for m in p.url.find_iter(&s) {
        // `xhttps://` is not a URL start; `詳細はhttps://` is.
        if is_ascii_alnum_at_end(&s[..m.start()]) {
            continue;
        }
        let url = m
            .as_str()
            .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '(', '[']);
        out.facts.push(Fact {
            text: url.to_string(),
            raw: url.to_string(),
            kind: Kind::Url,
            keys: vec![format!("u:{url}")],
            digits: 0,
        });
        taken.push((m.start(), m.end()));
    }
    blank(&mut s, &taken);
    let mut taken = Vec::new();
    for m in p.email.find_iter(&s) {
        out.facts.push(Fact {
            text: m.as_str().to_string(),
            raw: m.as_str().to_string(),
            kind: Kind::Email,
            keys: vec![format!("e:{}", m.as_str().to_lowercase())],
            digits: 0,
        });
        taken.push((m.start(), m.end()));
    }
    blank(&mut s, &taken);

    // Japanese era years: `令和8年` is 2026. The digits are required as
    // written, and the Gregorian year satisfies them.
    let mut taken = Vec::new();
    for c in p.era.captures_iter(&s) {
        let whole = c.get(0).expect("match");
        let base = match &c[1] {
            "令和" => 2018,
            "平成" => 1988,
            _ => 1925,
        };
        let n = &c[2];
        if n.chars().all(|ch| ch.is_ascii_digit()) {
            if let Ok(v) = n.parse::<u32>() {
                out.facts.push(Fact {
                    text: n.to_string(),
                    raw: whole.as_str().trim().to_string(),
                    kind: Kind::Digits,
                    keys: vec![num_key(n), format!("n:{}", base + v)],
                    digits: n.len(),
                });
            }
        } else {
            let v = if n == "元" { Some(1) } else { kanji_value(n) };
            if let Some(v) = v {
                out.implied.push(format!("n:{v}"));
                out.implied.push(format!("n:{}", base as u128 + v));
            }
        }
        taken.push((whole.start(), whole.end()));
    }
    blank(&mut s, &taken);

    // TIMES, most specific form first.
    let push_time =
        |out: &mut Extracted, raw: &str, hour: u32, minute: u32, meridiem: Option<bool>| -> bool {
            if hour > 24 || minute > 59 {
                return false;
            }
            let lit = digits_of(raw);
            out.facts.push(Fact {
                text: raw.trim().to_string(),
                raw: raw.trim().to_string(),
                kind: Kind::Time,
                keys: time_keys(hour, minute, meridiem, &lit),
                digits: lit.len(),
            });
            true
        };
    let mut taken = Vec::new();
    for c in p.time_cjk.captures_iter(&s) {
        let whole = c.get(0).expect("match");
        let num = c.get(2).expect("hour");
        if !number_starts(&s, num.start()) {
            continue;
        }
        let meridiem = c
            .get(1)
            .map(|m| matches!(m.as_str(), "午後" | "下午" | "中午" | "晚上" | "오후"));
        let hour: u32 = num.as_str().parse().unwrap_or(99);
        let minute = match (c.get(3), c.get(4)) {
            (Some(m), _) => m.as_str().parse().unwrap_or(99),
            (None, Some(_)) => 30,
            _ => 0,
        };
        let start = c.get(1).map_or(num.start(), |m| m.start());
        if push_time(&mut out, &s[start..whole.end()], hour, minute, meridiem) {
            taken.push((start, whole.end()));
        }
    }
    blank(&mut s, &taken);
    let mut taken = Vec::new();
    for c in p.time_uhr.captures_iter(&s) {
        let whole = c.get(0).expect("match");
        if !number_starts(&s, whole.start())
            || !ascii_bounded(&s, whole.start(), whole.end())
            || !number_ends(&s, whole.end())
        {
            continue;
        }
        let hour: u32 = c[1].parse().unwrap_or(99);
        let minute: u32 = c
            .get(2)
            .or(c.get(3))
            .map_or(0, |m| m.as_str().parse().unwrap_or(99));
        if push_time(&mut out, whole.as_str(), hour, minute, None) {
            taken.push((whole.start(), whole.end()));
        }
    }
    blank(&mut s, &taken);
    let mut taken = Vec::new();
    for c in p.time_meridiem.captures_iter(&s) {
        let whole = c.get(0).expect("match");
        if !number_starts(&s, whole.start())
            || is_ascii_alnum_at_end(&s[..whole.start()])
            || s[whole.end()..]
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_alphabetic())
        {
            continue;
        }
        let hour: u32 = c[1].parse().unwrap_or(99);
        if !(1..=12).contains(&hour) {
            continue;
        }
        let minute: u32 = c.get(2).map_or(0, |m| m.as_str().parse().unwrap_or(99));
        let pm = c[3].eq_ignore_ascii_case("p");
        if push_time(&mut out, whole.as_str(), hour, minute, Some(pm)) {
            taken.push((whole.start(), whole.end()));
        }
    }
    blank(&mut s, &taken);
    let mut taken = Vec::new();
    for re in [&p.time_h, &p.time_word] {
        for c in re.captures_iter(&s) {
            let whole = c.get(0).expect("match");
            if !number_starts(&s, whole.start())
                || !ascii_bounded(&s, whole.start(), whole.end())
                || !number_ends(&s, whole.end())
            {
                continue;
            }
            let hour: u32 = c[1].parse().unwrap_or(99);
            let minute: u32 = c.get(2).map_or(0, |m| m.as_str().parse().unwrap_or(99));
            if push_time(&mut out, whole.as_str(), hour, minute, None) {
                taken.push((whole.start(), whole.end()));
            }
        }
        blank(&mut s, &taken);
        taken.clear();
    }
    for c in p.time_colon.captures_iter(&s) {
        let whole = c.get(0).expect("match");
        if !number_starts(&s, whole.start()) || !number_ends(&s, whole.end()) {
            continue;
        }
        let hour: u32 = c[1].parse().unwrap_or(99);
        let minute: u32 = c[2].parse().unwrap_or(99);
        if hour <= 24 && push_time(&mut out, whole.as_str(), hour, minute, None) {
            taken.push((whole.start(), whole.end()));
        }
    }
    blank(&mut s, &taken);

    // DATES: three numbers each, never one joined run, and a month position
    // may be satisfied by the month's English name.
    let push_component = |out: &mut Extracted, d: &str, may_be_month: bool| {
        let mut keys = vec![num_key(d)];
        if may_be_month
            && let Ok(m) = d.parse::<u32>()
            && (1..=12).contains(&m)
        {
            keys.push(format!("m:{m}"));
        }
        out.facts.push(Fact {
            text: d.to_string(),
            raw: d.to_string(),
            kind: Kind::Digits,
            keys,
            digits: d.len(),
        });
    };
    let mut taken = Vec::new();
    for c in p.date_ymd.captures_iter(&s) {
        let whole = c.get(0).expect("match");
        if c[2] != c[4] || !number_starts(&s, whole.start()) || !number_ends(&s, whole.end()) {
            continue;
        }
        let (mo, d): (u32, u32) = (c[3].parse().unwrap_or(99), c[5].parse().unwrap_or(99));
        if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
            continue;
        }
        push_component(&mut out, &c[1], false);
        push_component(&mut out, &c[3], true);
        push_component(&mut out, &c[5], false);
        taken.push((whole.start(), whole.end()));
    }
    blank(&mut s, &taken);
    let mut taken = Vec::new();
    for c in p.date_dmy.captures_iter(&s) {
        let whole = c.get(0).expect("match");
        if c[2] != c[4] || !number_starts(&s, whole.start()) || !number_ends(&s, whole.end()) {
            continue;
        }
        let (a, b): (u32, u32) = (c[1].parse().unwrap_or(99), c[3].parse().unwrap_or(99));
        if !(1..=31).contains(&a) || !(1..=31).contains(&b) || (a > 12 && b > 12) {
            continue;
        }
        // d.m.y in most of the world, m/d/y in the US: either small number
        // may be the month.
        push_component(&mut out, &c[1], true);
        push_component(&mut out, &c[3], true);
        push_component(&mut out, &c[5], false);
        taken.push((whole.start(), whole.end()));
    }
    blank(&mut s, &taken);

    // CODES: ASCII letters and digits, five or more, standing alone.
    let mut taken = Vec::new();
    for m in p.token.find_iter(&s) {
        let tok = m.as_str();
        if !ascii_bounded(&s, m.start(), m.end())
            || !tok.chars().any(|c| c.is_ascii_digit())
            || !tok.chars().any(|c| c.is_ascii_alphabetic())
        {
            continue;
        }
        let upper = tok.to_ascii_uppercase();
        let digits = digits_of(tok);
        let lower = tok.chars().any(|c| c.is_ascii_lowercase());
        if !lower && !tok.chars().any(|c| c.is_ascii_uppercase()) {
            continue;
        }
        out.facts.push(Fact {
            text: tok.to_string(),
            raw: tok.to_string(),
            kind: if lower { Kind::Token } else { Kind::Code },
            keys: if lower {
                vec![format!("c:{upper}"), num_key(&digits)]
            } else {
                vec![format!("c:{upper}")]
            },
            digits: digits.len(),
        });
        taken.push((m.start(), m.end()));
    }
    blank(&mut s, &taken);

    // MAGNITUDES: the digits as written, and the value.
    let mut taken = Vec::new();
    for m in p.cjk_magnitude.find_iter(&s) {
        if !number_starts(&s, m.start()) {
            continue;
        }
        // Pairs of (mantissa, unit power), then glued trailing digits.
        let mut pairs: Vec<(String, u32)> = Vec::new();
        let mut num = String::new();
        for ch in m.as_str().chars() {
            if let Some(pow) = cjk_unit(ch) {
                pairs.push((std::mem::take(&mut num), pow));
            } else if !ch.is_whitespace() {
                num.push(ch);
            }
        }
        let trailing = num;
        let values: Vec<Vec<u128>> = pairs
            .iter()
            .map(|(n, pow)| {
                mantissa_readings(n)
                    .into_iter()
                    .filter_map(|r| scaled(r, *pow))
                    .collect()
            })
            .collect();
        let descending = pairs.windows(2).all(|w| w[0].1 > w[1].1);
        let raw = m.as_str().trim().to_string();
        if pairs.len() == 1 && trailing.is_empty() {
            let d = digits_of(&pairs[0].0);
            let mut keys = vec![num_key(&d)];
            keys.extend(values[0].iter().map(|v| format!("n:{v}")));
            out.facts.push(Fact {
                text: d.clone(),
                raw,
                kind: Kind::Magnitude,
                keys,
                digits: d.len(),
            });
        } else if descending && values.iter().all(|v| !v.is_empty()) {
            let mut total: u128 = values.iter().map(|v| v[0]).sum();
            let last_unit = 10u128.pow(pairs.last().map_or(0, |p| p.1));
            if let Ok(t) = trailing.parse::<u128>()
                && t < last_unit
            {
                total += t;
            }
            for ((n, _), v) in pairs.iter().zip(&values) {
                out.implied.push(num_key(&digits_of(n)));
                out.implied.extend(v.iter().map(|x| format!("n:{x}")));
            }
            if !trailing.is_empty() {
                out.implied.push(num_key(&trailing));
            }
            out.facts.push(Fact {
                text: total.to_string(),
                raw,
                kind: Kind::Magnitude,
                keys: vec![format!("n:{total}")],
                digits: total.to_string().len(),
            });
        } else {
            // Not one amount: leave it to the plain number scan.
            continue;
        }
        taken.push((m.start(), m.end()));
    }
    blank(&mut s, &taken);
    let mut taken = Vec::new();
    for c in p.word_magnitude.captures_iter(&s) {
        let whole = c.get(0).expect("match");
        let unit = c[3].to_lowercase();
        let glued = c[2].is_empty();
        if !number_starts(&s, whole.start())
            || is_ascii_alnum_at_end(&s[..whole.start()])
            || s[whole.end()..]
                .chars()
                .next()
                .is_some_and(char::is_alphanumeric)
            || (unit == "k" && !glued)
        {
            continue;
        }
        let Some(pows) = MAGNITUDE_WORDS
            .iter()
            .find(|(w, _)| *w == unit)
            .map(|(_, p)| *p)
        else {
            continue;
        };
        let d = digits_of(&c[1]);
        let mut keys = vec![num_key(&d)];
        for r in mantissa_readings(&c[1]) {
            for pow in pows {
                if let Some(v) = scaled(r, *pow) {
                    keys.push(format!("n:{v}"));
                }
            }
        }
        keys.dedup();
        out.facts.push(Fact {
            text: d.clone(),
            raw: whole.as_str().trim().to_string(),
            kind: Kind::Magnitude,
            keys,
            digits: d.len(),
        });
        taken.push((whole.start(), whole.end()));
    }
    blank(&mut s, &taken);

    // Kanji numerals: implied values only. `一緒` (together) and `一番` are
    // words, so a kanji number is never required, only an explanation.
    for m in p.kanji.find_iter(&s) {
        if s[..m.start()]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_digit())
        {
            continue;
        }
        if let Some(v) = kanji_value(m.as_str()) {
            out.implied.push(format!("n:{v}"));
        }
    }

    // PLAIN NUMBERS, the separators inside one number dropped.
    let first_plain = out.facts.len();
    for (start, end, joined) in scan_numbers(&s) {
        let mut keys = vec![num_key(&joined)];
        let after = s[end..].trim_start_matches(' ');
        if joined.len() <= 2
            && (after.starts_with('月') || after.starts_with('월'))
            && let Ok(m) = joined.parse::<u8>()
            && (1..=12).contains(&m)
        {
            keys.push(format!("m:{m}"));
        }
        out.facts.push(Fact {
            text: joined.clone(),
            raw: s[start..end].to_string(),
            kind: Kind::Digits,
            keys,
            digits: joined.len(),
        });
    }
    out.runs = phone_runs(&s, &out.facts[first_plain..], first_plain);

    if translated {
        const NAMES: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        for m in p.month.find_iter(&s) {
            if let Some(i) = NAMES.iter().position(|n| m.as_str().starts_with(n)) {
                out.facts.push(Fact {
                    text: m.as_str().to_string(),
                    raw: m.as_str().to_string(),
                    kind: Kind::Month,
                    keys: vec![format!("m:{}", i + 1)],
                    digits: 0,
                });
            }
        }
    }
    out
}

/// Characters that separate the groups of ONE number, dropped between two
/// digits on both sides alike so the conventions compare equal: `1.234,56`,
/// `1,234.56`, `1'234`. (Dates and times are taken before this runs.)
fn is_number_separator(c: char) -> bool {
    matches!(c, '.' | ',' | '\'' | ':' | 'h' | 'H')
}

/// Spaces that group thousands: `1 234`, with no-break, narrow and thin
/// spaces too. Only a thousands group: see [`scan_numbers`].
fn is_space_separator(c: char) -> bool {
    matches!(c, ' ' | '\u{A0}' | '\u{202F}' | '\u{2009}')
}

/// Every number in `s` as (byte start, byte end, digits). Punctuation
/// separators are skipped whenever a digit sits on each side. A SPACE is
/// skipped only when exactly three digits follow it (a thousands group), so
/// `Zimmer 12 2 Personen` stays two numbers.
fn scan_numbers(s: &str) -> Vec<(usize, usize, String)> {
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let digit = |i: usize| chars.get(i).is_some_and(|(_, c)| c.is_ascii_digit());
    let end_of = |i: usize| chars.get(i).map_or(s.len(), |(b, _)| *b);
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !digit(i) {
            i += 1;
            continue;
        }
        let start = chars[i].0;
        let mut joined = String::new();
        loop {
            while digit(i) {
                joined.push(chars[i].1);
                i += 1;
            }
            let Some(&(_, c)) = chars.get(i) else { break };
            if is_number_separator(c) && digit(i + 1) {
                i += 1;
                continue;
            }
            if is_space_separator(c) {
                let group = (i + 1..).take_while(|&j| digit(j)).count();
                if group == 3 {
                    i += 1;
                    continue;
                }
            }
            break;
        }
        out.push((start, end_of(i), joined));
    }
    out
}

/// The phone-number-shaped runs among `facts` (plain numbers, in text order,
/// `offset` their index in the extraction): two or more numbers separated
/// by at most three of space, dash, dot or parenthesis, with a `+` allowed
/// in front, seven digits or more together.
fn phone_runs(s: &str, facts: &[Fact], offset: usize) -> Vec<Run> {
    // Recover each fact's span by scanning again: `scan_numbers` is
    // deterministic, and the facts are exactly its output.
    let spans: Vec<(usize, usize)> = scan_numbers(s)
        .into_iter()
        .map(|(a, b, _)| (a, b))
        .collect();
    debug_assert_eq!(spans.len(), facts.len());
    let mut runs = Vec::new();
    let mut i = 0;
    while i < spans.len() {
        let mut j = i;
        while j + 1 < spans.len() {
            let gap = &s[spans[j].1..spans[j + 1].0];
            if gap.is_empty()
                || gap.chars().count() > 3
                || !gap
                    .chars()
                    .all(|c| matches!(c, ' ' | '-' | '.' | '(' | ')' | '\u{A0}'))
            {
                break;
            }
            j += 1;
        }
        if j > i {
            let digits: String = (i..=j).map(|k| facts[k].text.as_str()).collect();
            if digits.len() >= 7 {
                let mut start = spans[i].0;
                if s[..start].ends_with('+') {
                    start -= 1;
                }
                runs.push(Run {
                    raw: s[start..spans[j].1].to_string(),
                    digits,
                    members: (i..=j).map(|k| k + offset).collect(),
                });
            }
        }
        i = j + 1;
    }
    runs
}

/// The facts one text carries, each as the string a missing-facts list names
/// it by: digit runs (separators removed), URLs, email addresses, and
/// code-like tokens of 5 or more letters and digits with at least one of
/// each. Normalized as [`normalize`] does.
pub fn facts(text: &str) -> Vec<String> {
    extract(text, false)
        .facts
        .into_iter()
        .map(|f| f.text)
        .collect()
}

/// The fact check's verdict on one batch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Check {
    /// Source facts the translation does not carry, in source order, each
    /// listed once per missing occurrence.
    pub missing: Vec<String>,
    /// REJECT-CLASS ADDITIONS: a URL, email address, code, or number of
    /// seven or more digits (a phone number) the source does not say.
    pub added: Vec<String>,
    /// Other numbers the source does not say (4 to 6 digits, a magnitude, a
    /// time): most likely a number the source spelled out in words. The
    /// translation is served `incomplete` with a note naming them.
    pub added_numbers: Vec<String>,
}

impl Check {
    pub fn passed(&self) -> bool {
        self.missing.is_empty() && self.added.is_empty() && self.added_numbers.is_empty()
    }
}

/// A maximum matching of `left` facts to `right` facts over shared keys:
/// for each left fact, the right fact it took.
fn max_matching(left: &[&Fact], right: &[&Fact]) -> Vec<Option<usize>> {
    let mut by_key: HashMap<&str, Vec<usize>> = HashMap::new();
    for (j, f) in right.iter().enumerate() {
        for k in &f.keys {
            by_key.entry(k.as_str()).or_default().push(j);
        }
    }
    let adj: Vec<Vec<usize>> = left
        .iter()
        .map(|f| {
            let mut v: Vec<usize> = f
                .keys
                .iter()
                .filter_map(|k| by_key.get(k.as_str()))
                .flatten()
                .copied()
                .collect();
            v.sort_unstable();
            v.dedup();
            v
        })
        .collect();
    let mut owner: Vec<Option<usize>> = vec![None; right.len()];
    let mut taken: Vec<Option<usize>> = vec![None; left.len()];
    // Greedy first: almost every fact has one obvious partner.
    for (u, vs) in adj.iter().enumerate() {
        if let Some(&v) = vs.iter().find(|&&v| owner[v].is_none()) {
            owner[v] = Some(u);
            taken[u] = Some(v);
        }
    }
    fn augment(
        u: usize,
        adj: &[Vec<usize>],
        seen: &mut [bool],
        owner: &mut [Option<usize>],
        taken: &mut [Option<usize>],
    ) -> bool {
        for &v in &adj[u] {
            if seen[v] {
                continue;
            }
            seen[v] = true;
            if owner[v].is_none_or(|w| augment(w, adj, seen, owner, taken)) {
                owner[v] = Some(u);
                taken[u] = Some(v);
                return true;
            }
        }
        false
    }
    for u in 0..left.len() {
        if taken[u].is_none() && !adj[u].is_empty() {
            let mut seen = vec![false; right.len()];
            augment(u, &adj, &mut seen, &mut owner, &mut taken);
        }
    }
    taken
}

/// A translation's phone number is the source's when the digits agree, or
/// when the translation only put a country code in front of the national
/// number (`01 23 45 67 89` written `+33 1 23 45 67 89`).
fn same_phone(translated: &str, source: &str) -> bool {
    let (t, s) = (strip_zeros(translated), strip_zeros(source));
    t == s || (s.len() >= 7 && t.ends_with(s) && t.len() - s.len() <= 3)
}

/// THE CHECK: what the translation misses and what it adds, both ways over
/// the whole batch. See the module docs for what counts as the same fact.
pub fn check(source: &[String], translated: &[String]) -> Check {
    let src: Vec<Extracted> = source.iter().map(|s| extract(s, false)).collect();
    let tr: Vec<Extracted> = translated.iter().map(|t| extract(t, true)).collect();
    let s_facts: Vec<&Fact> = src.iter().flat_map(|e| &e.facts).collect();
    let t_facts: Vec<&Fact> = tr.iter().flat_map(|e| &e.facts).collect();

    let taken = max_matching(&s_facts, &t_facts);
    let missing = s_facts
        .iter()
        .zip(&taken)
        .filter(|(_, t)| t.is_none())
        .map(|(f, _)| f.text.clone())
        .collect();

    // Everything the source says or implies.
    let mut universe: HashSet<&str> = HashSet::new();
    for e in &src {
        for f in &e.facts {
            universe.extend(f.keys.iter().map(String::as_str));
        }
        universe.extend(e.implied.iter().map(String::as_str));
    }
    let s_runs: Vec<&str> = src
        .iter()
        .flat_map(|e| &e.runs)
        .map(|r| r.digits.as_str())
        .collect();
    let explained = |f: &Fact| f.keys.iter().any(|k| universe.contains(k.as_str()));

    let mut added = Vec::new();
    let mut added_numbers: Vec<String> = Vec::new();
    for e in &tr {
        let mut in_run = vec![false; e.facts.len()];
        for run in &e.runs {
            for &m in &run.members {
                in_run[m] = true;
            }
            let ok = run.members.iter().all(|&m| explained(&e.facts[m]))
                || universe.contains(num_key(&run.digits).as_str())
                || s_runs.iter().any(|s| same_phone(&run.digits, s));
            if !ok {
                added.push(run.raw.trim().to_string());
            }
        }
        for (i, f) in e.facts.iter().enumerate() {
            if in_run[i] || f.kind == Kind::Month || explained(f) {
                continue;
            }
            match f.kind {
                Kind::Url | Kind::Email | Kind::Code => added.push(f.text.clone()),
                Kind::Token | Kind::Digits | Kind::Magnitude | Kind::Time => {
                    if f.digits >= 7 && f.kind != Kind::Time {
                        added.push(f.raw.clone());
                    } else if (f.digits >= 4 || matches!(f.kind, Kind::Magnitude | Kind::Time))
                        && !added_numbers.contains(&f.raw)
                    {
                        added_numbers.push(f.raw.clone());
                    }
                }
                Kind::Month => {}
            }
        }
    }
    Check {
        missing,
        added,
        added_numbers,
    }
}

/// [`Check::missing`] alone.
pub fn missing_facts(source: &[String], translated: &[String]) -> Vec<String> {
    check(source, translated).missing
}

/// [`Check::added`] alone: the reject-class additions.
pub fn added_facts(source: &[String], translated: &[String]) -> Vec<String> {
    check(source, translated).added
}

#[cfg(test)]
mod tests;
