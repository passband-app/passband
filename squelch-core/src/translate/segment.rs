//! Segmenting a body into the strings the model translates, and putting the
//! translations back (docs/TRANSLATION.md, "Rendering").
//!
//! THE MODEL NEVER SEES OR WRITES MARKUP. The HTML path pulls the visible text
//! nodes out of `body_html`, which is ALREADY SANITIZED (`sanitize_email_html`
//! ran at ingest), and reassembles the document from its own tags with each
//! translation inserted as an ESCAPED text node. A translation that reads
//! `<script>` lands as the text `&lt;script&gt;`, and the result goes through
//! the sanitizer again anyway.
//!
//! A hand tokenizer is enough here, and safer to reason about than a second
//! parse: the input is html5ever's own serialization, in which every `<` in
//! text and attributes is escaped (the sanitizer's documented INVARIANT), so
//! every `<` starts a tag, and `<style>` is the only raw-text element left.

use crate::sync::html::sanitize_email_html;
use crate::sync::ingest::decode_entities;

/// One piece of a document: markup or untranslated text kept byte for byte,
/// or a text node whose core is segment `index`, between the whitespace it
/// was found with (as found: still entity-encoded on the HTML path).
#[derive(Debug, Clone)]
enum Piece {
    Raw(String),
    Text {
        lead: String,
        index: usize,
        trail: String,
    },
}

/// A body taken apart: its segments in document order, and the template that
/// rebuilds it from their translations.
#[derive(Debug, Clone)]
pub struct Segmented {
    pieces: Vec<Piece>,
    segments: Vec<String>,
    html: bool,
}

impl Segmented {
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    /// The body rebuilt with `translations[i]` in place of segment `i`.
    /// `None` when the count does not match: a misaligned array must never
    /// be put back, because every text node after the gap would be wrong.
    ///
    /// The HTML result has been through `sanitize_email_html` again.
    pub fn reassemble(&self, translations: &[String]) -> Option<String> {
        if translations.len() != self.segments.len() {
            return None;
        }
        let mut out = String::new();
        for piece in &self.pieces {
            match piece {
                Piece::Raw(raw) => out.push_str(raw),
                Piece::Text { lead, index, trail } => {
                    let text = &translations[*index];
                    out.push_str(lead);
                    if self.html {
                        out.push_str(&escape_text(text));
                    } else {
                        out.push_str(text);
                    }
                    out.push_str(trail);
                }
            }
        }
        Some(if self.html {
            sanitize_email_html(&out)
        } else {
            out
        })
    }
}

/// Segment sanitized HTML: one segment per visible text node worth
/// translating. Skipped and kept verbatim: the contents of `<style>` and
/// `<script>`, whitespace-only text, and text that is only numbers, URLs,
/// email addresses and punctuation.
pub fn segment_html(sanitized: &str) -> Segmented {
    let mut pieces = Vec::new();
    let mut segments = Vec::new();
    let bytes = sanitized.as_bytes();
    let mut i = 0;
    let mut raw = String::new();
    while i < sanitized.len() {
        if bytes[i] == b'<' {
            let end = tag_end(sanitized, i);
            let tag = &sanitized[i..end];
            raw.push_str(tag);
            i = end;
            // Raw-text elements: their content is code, carried through whole.
            if let Some(name) = raw_text_element(tag) {
                let close = find_ascii_ci(sanitized, i, &format!("</{name}"));
                let stop = close.unwrap_or(sanitized.len());
                raw.push_str(&sanitized[i..stop]);
                i = stop;
            }
            continue;
        }
        let next = sanitized[i..]
            .find('<')
            .map(|n| i + n)
            .unwrap_or(sanitized.len());
        let run = &sanitized[i..next];
        i = next;
        let (lead, core, trail) = split_html_run(run);
        let core = collapse_whitespace(&decode_entities(core));
        if worth_translating(&core) {
            if !raw.is_empty() {
                pieces.push(Piece::Raw(std::mem::take(&mut raw)));
            }
            pieces.push(Piece::Text {
                lead: lead.to_string(),
                index: segments.len(),
                trail: trail.to_string(),
            });
            segments.push(core);
        } else {
            raw.push_str(run);
        }
    }
    if !raw.is_empty() {
        pieces.push(Piece::Raw(raw));
    }
    Segmented {
        pieces,
        segments,
        html: true,
    }
}

/// Segment plain text: one segment per paragraph (blank-line separated), the
/// separators and each paragraph's surrounding whitespace kept as they were.
/// Line breaks inside a paragraph travel with it.
pub fn segment_plain(text: &str) -> Segmented {
    let mut pieces = Vec::new();
    let mut segments = Vec::new();
    let mut raw = String::new();
    for block in split_paragraphs(text) {
        match split_core(block) {
            Some((lead, core, trail)) if worth_translating(core) => {
                if !raw.is_empty() {
                    pieces.push(Piece::Raw(std::mem::take(&mut raw)));
                }
                pieces.push(Piece::Text {
                    lead: lead.to_string(),
                    index: segments.len(),
                    trail: trail.to_string(),
                });
                segments.push(core.to_string());
            }
            _ => raw.push_str(block),
        }
    }
    if !raw.is_empty() {
        pieces.push(Piece::Raw(raw));
    }
    Segmented {
        pieces,
        segments,
        html: false,
    }
}

/// `text` cut after every blank line, so the pieces concatenate back to it
/// exactly and each one holds a paragraph with its trailing separator.
fn split_paragraphs(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut line_start = 0;
    let mut seen_text = false;
    for (pos, c) in text.char_indices() {
        if c != '\n' {
            continue;
        }
        let line = &text[line_start..pos];
        let blank = line.trim().is_empty();
        if blank && seen_text {
            out.push(&text[start..pos + 1]);
            start = pos + 1;
            seen_text = false;
        } else if !blank {
            seen_text = true;
        }
        line_start = pos + 1;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// Whether a trimmed text holds anything a translation would change: some
/// letters once URLs, email addresses and digits are set aside.
fn worth_translating(core: &str) -> bool {
    core.split_whitespace().any(|word| {
        let lower = word.to_ascii_lowercase();
        if lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("www.")
        {
            return false;
        }
        if word.contains('@') && word.contains('.') {
            return false;
        }
        word.chars().any(char::is_alphabetic)
    })
}

/// A raw HTML text run split as `(lead, core, trail)`, where the lead and
/// trail are its whitespace AS WRITTEN, `&nbsp;` included, so a reassembled
/// node keeps the exact spacing it had. The core is still entity-encoded.
fn split_html_run(run: &str) -> (&str, &str, &str) {
    const SPACES: [&str; 3] = ["&nbsp;", "&#160;", "&#xa0;"];
    let mut start = 0;
    'lead: while start < run.len() {
        let rest = &run[start..];
        if let Some(c) = rest.chars().next().filter(|c| c.is_whitespace()) {
            start += c.len_utf8();
            continue;
        }
        for sp in SPACES {
            if rest.len() >= sp.len() && rest[..sp.len()].eq_ignore_ascii_case(sp) {
                start += sp.len();
                continue 'lead;
            }
        }
        break;
    }
    let mut end = run.len();
    'trail: while end > start {
        let rest = &run[start..end];
        if let Some(c) = rest.chars().next_back().filter(|c| c.is_whitespace()) {
            end -= c.len_utf8();
            continue;
        }
        for sp in SPACES {
            if rest.len() >= sp.len() && rest[rest.len() - sp.len()..].eq_ignore_ascii_case(sp) {
                end -= sp.len();
                continue 'trail;
            }
        }
        break;
    }
    (&run[..start], &run[start..end], &run[end..])
}

/// `(leading whitespace, core, trailing whitespace)`, `None` when blank.
fn split_core(s: &str) -> Option<(&str, &str, &str)> {
    let core = s.trim();
    if core.is_empty() {
        return None;
    }
    let start = s.len() - s.trim_start().len();
    let end = start + core.len();
    Some((&s[..start], core, &s[end..]))
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The byte after the `>` that closes the tag opening at `start`, honouring
/// quoted attribute values. An unclosed tag runs to the end.
fn tag_end(s: &str, start: usize) -> usize {
    let bytes = s.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = start + 1;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return i + 1,
            None => {}
        }
        i += 1;
    }
    bytes.len()
}

/// `style` or `script` when `tag` opens one.
fn raw_text_element(tag: &str) -> Option<&'static str> {
    let name: String = tag[1..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    match name.as_str() {
        "style" => Some("style"),
        "script" => Some("script"),
        _ => None,
    }
}

fn find_ascii_ci(haystack: &str, from: usize, needle: &str) -> Option<usize> {
    let hay = haystack.as_bytes();
    let n = needle.as_bytes();
    (from..hay.len().saturating_sub(n.len() - 1))
        .find(|&i| hay[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// Text made safe as an HTML text node.
fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\u{A0}' => out.push_str("&nbsp;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upper(seg: &Segmented) -> Vec<String> {
        seg.segments().iter().map(|s| s.to_uppercase()).collect()
    }

    #[test]
    fn html_text_nodes_are_segments_and_the_layout_survives() {
        let html = sanitize_email_html(
            "<style>p > b { color: red; }</style>\
             <table width=\"600\"><tr><td style=\"color:red\">Bonjour <b>Marie</b>,</td></tr></table>\
             <p>Votre   facture\n  est prête : <a href=\"https://x.example/f\">voir</a></p>\
             <p>12,50 €</p><p> </p><p>https://x.example/f</p>",
        );
        let seg = segment_html(&html);
        // The lone "," is punctuation only and stays where it was.
        assert_eq!(
            seg.segments(),
            ["Bonjour", "Marie", "Votre facture est prête :", "voir"].map(String::from)
        );
        let out = seg.reassemble(&upper(&seg)).unwrap();
        assert!(
            out.contains("<style>p > b { color: red; }</style>"),
            "{out}"
        );
        assert!(out.contains("<table width=\"600\">"), "{out}");
        assert!(
            out.contains("<td style=\"color:red\">BONJOUR <b>MARIE</b>,</td>"),
            "{out}"
        );
        assert!(
            out.contains("<a href=\"https://x.example/f\" rel=\"noopener noreferrer\">VOIR</a>"),
            "{out}"
        );
        // Untranslatable text is kept byte for byte.
        assert!(out.contains("<p>12,50 €</p>"), "{out}");
        assert!(out.contains("<p>https://x.example/f</p>"), "{out}");
    }

    #[test]
    fn identity_reassembly_reproduces_the_document() {
        let html = sanitize_email_html(
            "<div><p>Bonjour&nbsp;<i>tout</i> le monde &amp; merci</p><p>À bientôt</p></div>",
        );
        let seg = segment_html(&html);
        let out = seg.reassemble(seg.segments()).unwrap();
        assert_eq!(out, html);
    }

    #[test]
    fn a_translation_is_text_never_markup() {
        let html = sanitize_email_html("<p>Bonjour</p>");
        let seg = segment_html(&html);
        let out = seg
            .reassemble(&["<script>alert(1)</script><img src=x onerror=y>".to_string()])
            .unwrap();
        assert!(!out.contains("<script"), "{out}");
        assert!(!out.contains("<img"), "{out}");
        assert!(
            out.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "{out}"
        );
    }

    #[test]
    fn a_misaligned_array_is_refused() {
        let seg = segment_html("<p>Bonjour</p><p>Merci</p>");
        assert!(seg.reassemble(&["Hello".to_string()]).is_none());
    }

    #[test]
    fn script_contents_are_not_segments() {
        let seg = segment_html("<p>Bonjour</p><script>var texte = 'bonjour';</script>");
        assert_eq!(seg.segments(), &["Bonjour".to_string()]);
    }

    #[test]
    fn plain_text_paragraphs_are_segments_and_spacing_survives() {
        let text =
            "Bonjour Marie,\n\nVotre facture est prête.\nMerci.\n\n\n  12,50 €\n\nCordialement\n";
        let seg = segment_plain(text);
        assert_eq!(
            seg.segments(),
            &[
                "Bonjour Marie,".to_string(),
                "Votre facture est prête.\nMerci.".to_string(),
                "Cordialement".to_string()
            ]
        );
        assert_eq!(seg.reassemble(seg.segments()).unwrap(), text);
        let out = seg.reassemble(&upper(&seg)).unwrap();
        assert_eq!(
            out,
            "BONJOUR MARIE,\n\nVOTRE FACTURE EST PRÊTE.\nMERCI.\n\n\n  12,50 €\n\nCORDIALEMENT\n"
        );
    }
}
