//! Body condensing for the model's context: the parts of an email that cost
//! input tokens and carry nothing the triage decision needs.
//!
//! Measured on the local ledger (2026-09-28): the top tenth of triage calls by
//! input carried half of all uncached input tokens, and the largest were single
//! plain-text newsletters of ~55k characters holding ~135 click-tracking URLs
//! each. Threads with five or more messages averaged 2.5x the input of a
//! single message because every sibling body arrived in full, each re-quoting
//! the ones before it. Neither a redirect token nor the third copy of a reply
//! changes what the mail IS or where it belongs, and the model keeps
//! `read_message` (raw pages) and `read_thread` for when it does.
//!
//! Everything here is a pure function of the text and cuts on char
//! boundaries. Nothing is written back to the store: the stored body stays
//! whole for search, the reader, and the raw-page tool.

/// A URL longer than this is shortened to its first [`URL_KEEP_CHARS`]
/// characters. Carrier tracking links (`fedex.com/fedextrack/?trknbr=…`) and
/// order links are well under it; newsletter click-tracking redirects run 200
/// to 400 characters of opaque token and are the whole input tail.
const MAX_URL_CHARS: usize = 96;
/// Enough to keep scheme, host and the head of the path, which is what tells a
/// reader "this is a fedex.com link" versus "this is a mailer redirect".
const URL_KEEP_CHARS: usize = 48;

/// Stands in for a removed quoted tail, so the model knows text was there.
pub const QUOTE_MARKER: &str = "[earlier quoted messages removed]";

/// Attribution lines that open a quoted earlier message, as (lead, tail).
/// The lead must sit at a word boundary within a short window before the
/// tail, which is what keeps "Onboarding team wrote:" from matching.
const ATTRIBUTIONS: &[(&str, &str)] = &[
    ("On ", "wrote:"),
    ("Le ", "a écrit :"),
    ("Am ", "schrieb "),
    ("El ", "escribió:"),
];
/// Bytes of attribution allowed between lead and tail: a date, a time, a
/// name and an address, possibly wrapped once.
const ATTRIBUTION_WINDOW: usize = 240;

/// Drop the quoted earlier messages from the end of a body and shorten long
/// URLs throughout. A body with no new text before its quote (an empty reply,
/// or one the detector cannot place) is returned with only its URLs shortened,
/// and so is a forward: there the "quoted" part IS the message.
pub fn condense_body(body: &str) -> String {
    let cut = if forwarded(body) {
        None
    } else {
        [
            attribution_cut(body),
            outlook_cut(body),
            quoted_block_cut(body),
        ]
        .into_iter()
        .flatten()
        .min()
    };
    match cut {
        Some(at)
            if body[..at].chars().any(|c| !c.is_whitespace())
                && body[at..].chars().any(|c| !c.is_whitespace()) =>
        {
            let mut kept = shorten_urls(body[..at].trim_end());
            kept.push('\n');
            kept.push_str(QUOTE_MARKER);
            kept
        }
        _ => shorten_urls(body),
    }
}

/// A condensed body cut to `chars` scalar values (marker included), for the
/// older siblings of a thread whose gist matters but whose text rarely does.
/// The flag says whether the cut removed anything.
pub fn excerpt(body: &str, chars: usize) -> (String, bool) {
    let condensed = condense_body(body);
    let cut = condensed.chars().count() > chars;
    (crate::text::truncate_ellipsis(&condensed, chars), cut)
}

/// Whether the body's UNQUOTED text announces a forward: Gmail's and Apple
/// Mail's banners, or an Outlook header block whose subject starts with a
/// forward prefix. A marker inside a `>`-quoted line does not count, or a
/// reply to a forward would keep its whole quoted chain.
fn forwarded(body: &str) -> bool {
    body.lines().any(|line| {
        let line = line.trim_start();
        if line.starts_with('>') {
            return false;
        }
        if line.contains("Forwarded message") || line.contains("Begin forwarded message") {
            return true;
        }
        line.strip_prefix("Subject:").is_some_and(|subject| {
            let subject = subject.trim_start().to_ascii_lowercase();
            ["fw:", "fwd:", "tr:", "wg:"]
                .iter()
                .any(|prefix| subject.starts_with(prefix))
        })
    })
}

/// The byte offset of the earliest attribution line ("On …, X wrote:") whose
/// lead sits at a word boundary shortly before its tail. Works on text/plain
/// bodies and on HTML flattened to one line alike, because it never needs a
/// line start.
fn attribution_cut(body: &str) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (lead, tail) in ATTRIBUTIONS {
        let mut from = 0;
        while let Some(pos) = body[from..].find(tail) {
            let tail_at = from + pos;
            let mut window_start = tail_at.saturating_sub(ATTRIBUTION_WINDOW);
            while !body.is_char_boundary(window_start) {
                window_start -= 1;
            }
            let window = &body[window_start..tail_at];
            let lead_at = window.rmatch_indices(lead).find_map(|(idx, _)| {
                let at_boundary = if idx == 0 {
                    window_start == 0
                } else {
                    window[..idx].ends_with(char::is_whitespace)
                };
                at_boundary.then_some(window_start + idx)
            });
            // A real attribution carries a date or time and usually an
            // address; prose ("On Tuesday the vendor wrote:") carries neither.
            let looks_like_attribution = |between: &str| {
                between.chars().any(|c| c.is_ascii_digit())
                    && between.chars().any(|c| matches!(c, '@' | '<' | ':'))
            };
            if let Some(cut) = lead_at
                && body[cut..tail_at].matches('\n').count() <= 2
                && looks_like_attribution(&body[cut..tail_at])
            {
                best = Some(best.map_or(cut, |b| b.min(cut)));
                break;
            }
            from = tail_at + tail.len();
        }
    }
    best
}

/// Outlook's separators: the literal "Original Message" banner, or a rule of
/// underscores followed by a `From:` header block.
fn outlook_cut(body: &str) -> Option<usize> {
    const RULE: &str = "________________";
    let mut best = body.find("-----Original Message-----");
    let mut from = 0;
    while let Some(pos) = body[from..].find(RULE) {
        let at = from + pos;
        let mut end = (at + 300).min(body.len());
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        if body[at..end].contains("From:") {
            best = Some(best.map_or(at, |b| b.min(at)));
            break;
        }
        from = at + RULE.len();
    }
    best
}

/// A trailing block of `>`-prefixed lines, three or more, that nothing
/// unquoted follows. ([`forwarded`] has already excused the forwards, where
/// the quoted block IS the message the reader wants triaged.)
fn quoted_block_cut(body: &str) -> Option<usize> {
    let mut cut = None;
    let mut quoted_lines = 0usize;
    let mut offset = 0usize;
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if !trimmed.is_empty() && trimmed != "\n" {
            if trimmed.starts_with('>') {
                if cut.is_none() {
                    cut = Some(offset);
                    quoted_lines = 0;
                }
                quoted_lines += 1;
            } else {
                cut = None;
            }
        }
        offset += line.len();
    }
    cut.filter(|_| quoted_lines >= 3)
}

/// Replace each URL longer than [`MAX_URL_CHARS`] with its head plus `…`.
fn shorten_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = url_start(rest) {
        out.push_str(&rest[..start]);
        let end = rest[start..]
            .find(|c: char| {
                c.is_whitespace()
                    || matches!(
                        c,
                        '<' | '>' | '"' | '\'' | ')' | ']' | '}' | '|' | '»' | '”' | '’'
                    )
            })
            .map_or(rest.len(), |e| start + e);
        let url = &rest[start..end];
        if url.chars().count() > MAX_URL_CHARS {
            out.push_str(&crate::text::truncate_chars(url, URL_KEEP_CHARS));
            out.push('…');
        } else {
            out.push_str(url);
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn url_start(text: &str) -> Option<usize> {
    match (text.find("http://"), text.find("https://")) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACKER: &str = "https://click.mailer.example.com/ls/click?upn=u001.AbCdEfGhIjKlMnOpQrStUvWxYz0123456789AbCdEfGhIjKlMnOpQrStUvWxYz0123456789AbCdEfGhIjKlMnOpQrStUvWxYz-2Fmore";

    #[test]
    fn gmail_plain_text_reply_keeps_the_new_text_and_marks_the_quote() {
        let body = "Sounds good, see you then.\n\nOn Mon, Sep 28, 2026 at 3:14 PM Alice Example <alice@example.com> wrote:\n> Are we still on for Tuesday?\n>\n> Alice\n";
        assert_eq!(
            condense_body(body),
            format!("Sounds good, see you then.\n{QUOTE_MARKER}")
        );
    }

    #[test]
    fn flattened_html_reply_is_cut_at_the_inline_attribution() {
        // html_to_text collapses whitespace, so the attribution has no line of
        // its own and the blockquote has no `>` markers.
        let body = "Yes please, one for me. On Mon, Sep 28, 2026 at 3:14 PM Alice <alice@example.com> wrote: Who wants lunch? On Mon Alice wrote: earlier";
        assert_eq!(
            condense_body(body),
            format!("Yes please, one for me.\n{QUOTE_MARKER}")
        );
    }

    #[test]
    fn outlook_separators_cut_before_the_header_block() {
        let body = "Approved.\n\n________________________________\nFrom: Bob <bob@example.com>\nSent: Monday\nTo: me\nSubject: Re: budget\n\nPlease approve.";
        assert_eq!(condense_body(body), format!("Approved.\n{QUOTE_MARKER}"));
        let body = "Approved.\n-----Original Message-----\nFrom: Bob";
        assert_eq!(condense_body(body), format!("Approved.\n{QUOTE_MARKER}"));
    }

    #[test]
    fn a_bare_quoted_tail_is_cut_but_a_forward_is_not() {
        let body = "Thoughts?\n> line one\n> line two\n> line three\n";
        assert_eq!(condense_body(body), format!("Thoughts?\n{QUOTE_MARKER}"));
        let fwd =
            "FYI\n---------- Forwarded message ---------\n> line one\n> line two\n> line three\n";
        assert_eq!(condense_body(fwd), fwd);
        // An Outlook forward: the header block is the message, not a quote.
        let fw = "FYI, please handle.\n\n________________________________\nFrom: Vendor <billing@vendor.example>\nSent: Mon\nTo: me\nSubject: FW: Invoice 4471 due Oct 5\n\nPlease pay the attached invoice by October 5.";
        assert_eq!(condense_body(fw), fw);
        // A forward banner INSIDE the quoted tail does not excuse the tail.
        let reply = "Thanks!\n> ---------- Forwarded message ---------\n> From: someone\n> body\n";
        assert_eq!(condense_body(reply), format!("Thanks!\n{QUOTE_MARKER}"));
        // Two quoted lines are not a tail worth cutting.
        let short = "Thoughts?\n> one\n> two\n";
        assert_eq!(condense_body(short), short);
    }

    #[test]
    fn attribution_needs_a_word_boundary_and_new_text_before_it() {
        // "Onboarding" is not "On ".
        let body = "Onboarding team wrote: welcome aboard, here is what happens next.";
        assert_eq!(condense_body(body), body);
        // An empty reply is left whole rather than reduced to a marker.
        let body = "On Mon, Sep 28, 2026 Alice <a@example.com> wrote:\n> the only text";
        assert_eq!(condense_body(body), body);
        // A stray "wrote:" with no lead within the window is not an attribution.
        let body = "The reviewer wrote: ship it. Then everyone went home.";
        assert_eq!(condense_body(body), body);
        // Prose that happens to open with "On <day>": no date, time or address.
        let body = "Thanks for the update. On Tuesday the vendor wrote: delivery slips to October 20, please replan.";
        assert_eq!(condense_body(body), body);
        let body = "Am Montag schrieb ich noch, dass alles passt.";
        assert_eq!(condense_body(body), body);
    }

    #[test]
    fn long_urls_are_shortened_and_short_ones_kept() {
        let body = format!(
            "Your package: https://www.fedex.com/fedextrack/?trknbr=449044304137821 arrives Tuesday. Unsubscribe: {TRACKER} — thanks!"
        );
        let out = condense_body(&body);
        assert!(out.contains("https://www.fedex.com/fedextrack/?trknbr=449044304137821 arrives"));
        assert!(!out.contains("2Fmore"));
        assert!(
            out.contains("https://click.mailer.example.com/ls/click?upn=u0…"),
            "{out}"
        );
        assert!(out.ends_with("— thanks!"));
        assert!(out.chars().count() < body.chars().count() - 100);
    }

    #[test]
    fn url_terminators_and_multibyte_neighbours_survive() {
        let body = format!("«{TRACKER}» und (https://x.example/y) fertig ✓");
        let out = condense_body(&body);
        assert!(out.starts_with("«https://click.mailer"));
        assert!(out.contains("…» und (https://x.example/y) fertig ✓"));
    }

    #[test]
    fn excerpt_condenses_first_then_cuts_to_the_budget() {
        let body = format!(
            "Short note. {TRACKER}\n\nOn Mon, Sep 28, 2026 Alice <a@example.com> wrote:\n> long quote"
        );
        let (out, cut) = excerpt(&body, 40);
        assert!(cut);
        assert_eq!(out.chars().count(), 40);
        assert!(out.ends_with('…'));
        assert!(out.starts_with("Short note. https://"));
        // Under budget: the condensed body comes back whole, marker included.
        let (out, cut) = excerpt(
            "Hi\n\nOn Mon, Sep 28, 2026 Alice <a@example.com> wrote:\n> q",
            200,
        );
        assert!(!cut);
        assert_eq!(out, format!("Hi\n{QUOTE_MARKER}"));
    }
}
