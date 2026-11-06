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

/// How much of a link a reading keeps: one longer than `over` characters is
/// shown as its first `keep` and an ellipsis, which is only a shortening
/// while `keep` is under `over`; with `path_only`, its query and fragment
/// go first, so the path (what says what a link is for) survives a long
/// host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Links {
    pub over: usize,
    pub keep: usize,
    pub path_only: bool,
}

impl Links {
    /// The triage context's: a mailer redirect to its origin.
    pub const TRIAGE: Links = Links {
        over: MAX_URL_CHARS,
        keep: URL_KEEP_CHARS,
        path_only: false,
    };
    /// The access check's: a link over 192 characters is shown up to its
    /// query, at most its first 128 characters, host and path, which is what
    /// tells a reset or sign-in link from a newsletter's tracking link, with
    /// room for a long host; a shorter link is shown whole, token and all
    /// (the check is told not to quote one).
    pub const ACCESS: Links = Links {
        over: 192,
        keep: 128,
        path_only: true,
    };
    /// No link shortened: a body read raw.
    pub const RAW: Links = Links {
        over: usize::MAX,
        keep: 0,
        path_only: false,
    };
}

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

/// Italian, Dutch and Portuguese attribution lines. FOR [`own_text`] ONLY,
/// like [`OWN_QUOTE_BANNERS`] and for the same reason: [`ATTRIBUTIONS`] feeds
/// triage's own quote cut ([`condense_body`], [`excerpt`]), and what triage
/// reads of a thread must not change with this feature, flag or no flag.
const OWN_ATTRIBUTIONS: &[(&str, &str)] = &[
    ("Il ", "ha scritto:"),
    ("Op ", "schreef"),
    ("Em ", "escreveu:"),
];

/// The verb of an attribution line in each language [`ATTRIBUTIONS`] knows,
/// for [`own_text`]'s looser line test.
const WROTE: &[&str] = &[
    "wrote",
    "a écrit",
    "schrieb",
    "escribió",
    "ha scritto",
    "schreef",
    "escreveu",
    // Swedish, Danish and Norwegian share it.
    "skrev",
];

/// More banners mail clients write over a quoted message: German, French,
/// and Thunderbird's spaced form. FOR [`own_text`] ONLY. Triage's
/// `outlook_cut` keeps the English banner alone, because its quote cut is
/// skipped only for a forward `forwarded()` recognises (English `FW:`), and a
/// German or French Outlook FORWARD carries the same banner: cut there,
/// triage would read "FYI" and lose the forwarded invoice.
const OWN_QUOTE_BANNERS: &[&str] = &[
    "-----Ursprüngliche Nachricht-----",
    "-----Message d'origine-----",
    "-------- Original Message --------",
];

/// Header keys of a quoted header block, by role, lowercased, in English,
/// German, French, Spanish, Italian, Dutch and Portuguese. Matched as the
/// whole text before a line's first colon (`De :` with French spacing too).
const HEADER_FROM: &[&str] = &["from", "von", "de", "da", "van"];
const HEADER_DATED: &[&str] = &[
    "sent",
    "date",
    "gesendet",
    "datum",
    "envoyé",
    "enviado",
    "enviada",
    "inviato",
    "data",
    "verzonden",
    "fecha",
];
const HEADER_ADDRESSED: &[&str] = &[
    "to",
    "subject",
    "cc",
    "an",
    "betreff",
    "à",
    "a",
    "objet",
    "para",
    "asunto",
    "oggetto",
    "aan",
    "onderwerp",
    "assunto",
];
/// Bytes of attribution allowed between lead and tail: a date, a time, a
/// name and an address, possibly wrapped once.
const ATTRIBUTION_WINDOW: usize = 240;

/// Drop the quoted earlier messages from the end of a body and shorten long
/// URLs throughout. A body with no new text before its quote (an empty reply,
/// or one the detector cannot place) is returned with only its URLs shortened,
/// and so is a forward: there the "quoted" part IS the message.
pub fn condense_body(body: &str) -> String {
    condense(body, quote_cut(body), usize::MAX, Links::TRIAGE).0
}

/// Long links shortened as `links` says, quoted text kept, cut to at most
/// `cap` characters, with the stored offset where the text left out begins
/// (`None` when nothing was): the access check's reading of a body, where a
/// quoted tail is evidence too, because the caller the check is for reads
/// the whole stored body, and a link's path (what tells a reset link from a
/// newsletter's) is worth more of it than the triage context keeps. The
/// walk is [`StoredBody::condense_within`]'s without the quote cut.
pub fn shorten_links_within(body: &str, cap: usize, links: Links) -> (String, Option<usize>) {
    condense(body, None, cap, links)
}

/// A stored body scanned once for its quote cut, to be condensed more than
/// once: the context bound condenses it again for each size it tries, and
/// the scan, which reads the whole body, is the expensive part; the walk
/// reads only the kept text.
pub struct StoredBody {
    text: String,
    quote: Option<usize>,
}

impl StoredBody {
    pub fn new(text: String) -> Self {
        let quote = quote_cut(&text);
        Self { text, quote }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// [`condense_body`], cut to at most `cap` characters, and where in the
    /// STORED body (a character offset, `read_message`'s unit) the text
    /// left out begins; `None` when nothing was left out.
    ///
    /// The condenser emits pieces that each stand for a run of the stored
    /// text: plain text (a short URL included) character for character, a
    /// shortened URL for the whole URL. Plain text is cut wherever the cap
    /// falls; a shortened URL is kept whole or left out, so the kept text
    /// never shows part of a stand-in. The offset is therefore exact: the
    /// stored text the kept pieces stand for ends there, and a page read
    /// from there begins with what the model has not seen (a URL left out
    /// begins the page, raw). Two things are never a cut: trailing
    /// whitespace, kept whole when it fits and dropped when it does not (it
    /// stands for nothing a page should fetch), and the quote marker, which
    /// rides over the cap (34 characters standing for a tail the model is
    /// told was removed; cut, it would only send the model paging a quote).
    /// The walk reads the kept text and little more: the cap bounds how far
    /// it looks for a link.
    pub fn condense_within(&self, cap: usize) -> (String, Option<usize>) {
        condense(&self.text, self.quote, cap, Links::TRIAGE)
    }
}

/// The walk behind [`StoredBody::condense_within`], the quote cut found and
/// the links' shortening chosen.
fn condense(body: &str, quote: Option<usize>, cap: usize, links: Links) -> (String, Option<usize>) {
    debug_assert!(links.keep < links.over, "a head no shorter than its link");
    let text = quote.map_or(body, |at| &body[..at]);
    let content = text.trim_end();
    let mut out = Condensed {
        text: String::with_capacity(text.len().min(cap.saturating_mul(4))),
        chars: 0,
        cap,
        stored: 0,
    };
    let mut rest = content;
    loop {
        // The cap reached with text to go: a cut, and no need to read on.
        let room = out.cap - out.chars;
        if room == 0 && !rest.is_empty() {
            return (out.text, Some(out.stored));
        }
        // A link that starts past the cap is never reached, so the search
        // for one stops where the cap must have fallen: `room` characters
        // are at most four bytes each, plus a scheme's worth so a link that
        // starts just under the cap is seen whole.
        let reach = rest.floor_char_boundary(room.saturating_mul(4).saturating_add(8));
        let Some(start) = url_start(&rest[..reach]) else {
            break;
        };
        if let Some(resume) = out.plain(&rest[..start]) {
            return (out.text, Some(resume));
        }
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
        let stands_for = url.chars().count();
        let left_out = if stands_for > links.over {
            let shown = if links.path_only {
                url.find(['?', '#']).map_or(url, |at| &url[..at])
            } else {
                url
            };
            let mut head = crate::text::truncate_chars(shown, links.keep);
            head.push('…');
            out.whole(&head, stands_for)
        } else {
            out.plain(url)
        };
        if let Some(resume) = left_out {
            return (out.text, Some(resume));
        }
        rest = &rest[end..];
    }
    if let Some(resume) = out.plain(rest) {
        return (out.text, Some(resume));
    }
    if quote.is_some() {
        out.text.push('\n');
        out.text.push_str(QUOTE_MARKER);
    } else {
        let trailing = &text[content.len()..];
        if trailing.chars().count() <= out.cap - out.chars {
            out.text.push_str(trailing);
        }
    }
    (out.text, None)
}

/// The condensed text being built, and how far into the stored body the
/// pieces emitted so far reach.
struct Condensed {
    text: String,
    /// Characters emitted.
    chars: usize,
    cap: usize,
    /// Characters of the stored body the emitted pieces stand for.
    stored: usize,
}

impl Condensed {
    /// Plain text, kept character for character as far as the cap allows.
    /// When the cap falls inside it, the stored offset to resume from. One
    /// pass, over no more of the run than the cap admits.
    fn plain(&mut self, run: &str) -> Option<usize> {
        let room = self.cap - self.chars;
        // Fewer bytes than room means fewer characters: no need to look.
        if room < run.len()
            && let Some((byte, _)) = run.char_indices().nth(room)
        {
            self.text.push_str(&run[..byte]);
            return Some(self.stored + room);
        }
        let length = run.chars().count();
        self.text.push_str(run);
        self.chars += length;
        self.stored += length;
        None
    }

    /// A stand-in for `stands_for` stored characters: kept whole, or left
    /// out with the resume at its start.
    fn whole(&mut self, piece: &str, stands_for: usize) -> Option<usize> {
        let length = piece.chars().count();
        if length > self.cap - self.chars {
            return Some(self.stored);
        }
        self.text.push_str(piece);
        self.chars += length;
        self.stored += stands_for;
        None
    }
}

/// The byte offset at which the body's quoted earlier messages begin, when
/// there is new text before them and quoted text after. An empty reply, one
/// the detector cannot place, and a forward (there the "quoted" part IS the
/// message) have none.
fn quote_cut(body: &str) -> Option<usize> {
    if forwarded(body) {
        return None;
    }
    [
        attribution_cut(body),
        outlook_cut(body),
        quoted_block_cut(body),
    ]
    .into_iter()
    .flatten()
    .min()
    .filter(|&at| {
        body[..at].chars().any(|c| !c.is_whitespace())
            && body[at..].chars().any(|c| !c.is_whitespace())
    })
}

/// The words a reply's AUTHOR wrote: everything before the first quoted
/// earlier message (an attribution line, an Outlook header block, a `>`
/// block), with any `>` line that survived that dropped. `None` for a
/// forward, where the part after the banner IS somebody else's message and
/// there is no telling the author's words from it.
///
/// Unlike [`quote_cut`] the cut is taken even when nothing precedes it: an
/// empty reply is an empty answer here, never the whole quoted chain. The
/// profile pass reads only this of the owner's sent mail
/// (`triage::profile::owner_words`), so an inbound body never reaches it.
///
/// IT CUTS MORE EAGERLY THAN TRIAGE'S CONDENSING DOES, on purpose: cutting
/// too early here loses a few of the owner's words, cutting too late hands an
/// inbound body to the profile pass. So beside the shared cuts it also takes
/// a bare Outlook header block (no rule above it: Outlook for Mac, OWA,
/// mobile, HTML flattened with the `<hr>` dropped, [`header_block_cut`]) and
/// any line ending in an attribution verb, with or without a date
/// ([`wrote_line_cut`]). Triage's own cuts keep their stricter tests, where
/// a false cut would hide the message being triaged.
pub fn own_text(body: &str) -> Option<String> {
    if forwarded(body) {
        return None;
    }
    let cut = [
        attribution_cut(body),
        attribution_cut_in(body, OWN_ATTRIBUTIONS),
        outlook_cut(body),
        quoted_block_cut(body),
        header_block_cut(body),
        wrote_line_cut(body),
        OWN_QUOTE_BANNERS.iter().filter_map(|b| body.find(b)).min(),
    ]
    .into_iter()
    .flatten()
    .min()
    .unwrap_or(body.len());
    Some(
        body[..cut]
            .lines()
            .filter(|line| !line.trim_start().starts_with('>'))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// A condensed body cut to `chars` scalar values, for the older siblings of
/// a thread whose gist matters but whose text rarely does. The flag says
/// whether the cut removed anything. The quote marker is whole or absent
/// here as everywhere: when it alone overflows the budget it goes, and the
/// `…` stands for the quote too. The walk reads only the kept text; the
/// quote scan still reads the body.
pub fn excerpt(body: &str, chars: usize) -> (String, bool) {
    let (mut kept, resume) = condense(body, quote_cut(body), chars, Links::TRIAGE);
    if resume.is_none() {
        if kept.chars().count() <= chars {
            return (kept, false);
        }
        let marker = format!("\n{QUOTE_MARKER}");
        if let Some(content) = kept.strip_suffix(&marker).map(str::len) {
            kept.truncate(content);
        }
    }
    (
        crate::text::truncate_ellipsis(&format!("{kept}…"), chars),
        true,
    )
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
    attribution_cut_in(body, ATTRIBUTIONS)
}

/// [`attribution_cut`] over a given list of (lead, tail) pairs.
fn attribution_cut_in(body: &str, attributions: &[(&str, &str)]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (lead, tail) in attributions {
        let mut from = 0;
        while let Some(pos) = body[from..].find(tail) {
            let tail_at = from + pos;
            let window_start = body.floor_char_boundary(tail_at.saturating_sub(ATTRIBUTION_WINDOW));
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

/// The byte offset of the first quoted HEADER BLOCK: a line-start from key
/// (`From:`, `Von:`, `De :`, ...) with, within three lines on EITHER side, a
/// dated key (`Sent:`, `Gesendet:`, `Envoyé :`, ...) AND an addressed one
/// (`To:`, `Betreff:`, `Objet :`, ...). Outlook writes it below the reply,
/// often with no rule over it; Thunderbird writes Subject and Date ABOVE
/// From. The cut is at the block's first header line, so the subject and date
/// of the quoted message go with it. For [`own_text`] only.
fn header_block_cut(body: &str) -> Option<usize> {
    let lines: Vec<(usize, &str)> = body
        .split_inclusive('\n')
        .scan(0usize, |at, line| {
            let start = *at;
            *at += line.len();
            Some((start, line))
        })
        .collect();
    // The text before a line's first colon, when it is short enough to be a
    // header key at all.
    let key = |line: &str| -> Option<String> {
        let (head, _) = line.trim_start().split_once(':')?;
        let head = head.trim_end();
        (!head.is_empty() && head.chars().count() <= 12).then(|| head.to_lowercase())
    };
    let is = |line: &str, keys: &[&str]| key(line).is_some_and(|k| keys.contains(&k.as_str()));
    let any_header =
        |line: &str| is(line, HEADER_FROM) || is(line, HEADER_DATED) || is(line, HEADER_ADDRESSED);
    lines.iter().enumerate().find_map(|(i, (_, line))| {
        if !is(line, HEADER_FROM) {
            return None;
        }
        let (mut dated, mut addressed) = (false, false);
        for (j, (_, l)) in lines
            .iter()
            .enumerate()
            .take((i + 4).min(lines.len()))
            .skip(i.saturating_sub(3))
        {
            if j != i {
                dated |= is(l, HEADER_DATED);
                addressed |= is(l, HEADER_ADDRESSED);
            }
        }
        if !(dated && addressed) {
            return None;
        }
        // Back up over header lines directly above From (Thunderbird's
        // Subject and Date), at most three.
        let mut first = i;
        while first > 0 && i - first < 3 && any_header(lines[first - 1].1) {
            first -= 1;
        }
        let (at, line) = lines[first];
        // Cut where the header's own text starts, not at the line's indent.
        Some(at + (line.len() - line.trim_start().len()))
    })
}

/// The byte offset of the first line that ENDS in an attribution verb and a
/// colon ("On Tue, Jo wrote:", "Il giorno lun Jo ha scritto:", "Op ma schreef
/// Jo <jo@x.nl>:"), with no date required. For [`own_text`] only: in prose a
/// line can end that way too, and there cutting early costs the owner a few
/// words, nothing more.
fn wrote_line_cut(body: &str) -> Option<usize> {
    let mut at = 0usize;
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some(head) = trimmed.strip_suffix(':') {
            let head = head.to_lowercase();
            if WROTE.iter().any(|w| head.contains(w)) {
                return Some(at + (line.len() - line.trim_start().len()));
            }
        }
        at += line.len();
    }
    None
}

/// Outlook's separators: the literal "Original Message" banner, or a rule of
/// underscores followed by a `From:` header block.
fn outlook_cut(body: &str) -> Option<usize> {
    const RULE: &str = "________________";
    let mut best = body.find("-----Original Message-----");
    let mut from = 0;
    while let Some(pos) = body[from..].find(RULE) {
        let at = from + pos;
        let end = body.floor_char_boundary(at + 300);
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

/// The byte offset of the next `http://` or `https://`, found in one pass:
/// a search for each scheme would read to the end of an https-only body at
/// every link.
fn url_start(text: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(found) = text[from..].find("http") {
        let at = from + found;
        if text[at..].starts_with("http://") || text[at..].starts_with("https://") {
            return Some(at);
        }
        from = at + "http".len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn within(body: &str, cap: usize) -> (String, Option<usize>) {
        StoredBody::new(body.into()).condense_within(cap)
    }

    const TRACKER: &str = "https://click.mailer.example.com/ls/click?upn=u001.AbCdEfGhIjKlMnOpQrStUvWxYz0123456789AbCdEfGhIjKlMnOpQrStUvWxYz0123456789AbCdEfGhIjKlMnOpQrStUvWxYz-2Fmore";

    /// Triage's condensing of an Italian, Dutch or Portuguese reply is what it
    /// is on main: none of those attributions is one triage cuts at, and with
    /// no `>` block under them there is no quote for triage to drop. So the
    /// body comes back whole from `condense_body`, and whole and unflagged
    /// from `excerpt`. `own_text` still stops at the attribution.
    #[test]
    fn italian_dutch_and_portuguese_replies_reach_triage_as_on_main() {
        for body in [
            "Va bene.\n\nIl giorno mar 3 nov 2026 alle ore 09:00 Jo <jo@x.it> ha scritto:\nIl testo di Jo.",
            "Prima.\n\nOp di 3 nov. 2026 om 09:00 schreef Jo <jo@x.nl>:\nDe tekst van Jo.",
            "Combinado.\n\nEm ter., 3 de nov. de 2026 às 09:00, Jo <jo@x.pt> escreveu:\nO texto do Jo.",
        ] {
            assert_eq!(condense_body(body), body, "{body:?}");
            assert_eq!(excerpt(body, 2_000), (body.to_string(), false), "{body:?}");
            let own = own_text(body).unwrap();
            assert!(!own.contains("Jo."), "{own:?}");
        }
    }

    /// Triage's condensing is unchanged by `own_text`'s extra banners: a
    /// German Outlook forward keeps the forwarded message.
    #[test]
    fn a_german_forward_still_reaches_triage() {
        let body = "FYI\n\n-----Ursprüngliche Nachricht-----\nVon: Rechnungen <billing@acme.de>\nGesendet: Dienstag, 3. November 2026 09:00\nAn: Me\nBetreff: WG: Rechnung 4711\n\nBitte zahlen Sie 120 EUR bis Freitag.";
        let condensed = condense_body(body);
        assert!(
            condensed.contains("Bitte zahlen Sie 120 EUR"),
            "{condensed}"
        );
        assert!(!condensed.contains(QUOTE_MARKER), "{condensed}");
        // The owner-words read, by contrast, stops at the banner.
        assert_eq!(own_text(body).unwrap().trim(), "FYI");
    }

    /// [`own_text`] must never hand an inbound body on, whatever shape the
    /// reply's quote takes.
    #[test]
    fn own_text_stops_at_every_quote_shape() {
        let inbound = "THEIR WORDS here";
        for body in [
            // Outlook for Mac, OWA, mobile: a header block with no rule.
            format!(
                "Sounds good.\n\nFrom: Jo <jo@x.com>\nSent: Tuesday, November 3, 2026 9:00 AM\nTo: Me\nSubject: RE: plan\n\n{inbound}"
            ),
            format!("Sounds good.\nFrom: Jo\nDate: Tue\nSubject: plan\n{inbound}"),
            // Attributions without a date, and in other languages.
            format!("Sounds good.\n\nOn Tue, Jo wrote:\n{inbound}"),
            format!(
                "Sounds good.\n\nIl giorno mar 3 nov 2026, Jo <jo@x.it> ha scritto:\n{inbound}"
            ),
            format!("Sounds good.\n\nOp di 3 nov. 2026 schreef Jo <jo@x.nl>:\n{inbound}"),
            format!(
                "Sounds good.\n\nEm ter., 3 de nov. de 2026, Jo <jo@x.pt> escreveu:\n{inbound}"
            ),
            format!("Sounds good.\n\nIl giorno mar, Jo ha scritto:\n{inbound}"),
            format!("Sounds good.\n\nDen tis 3 nov. 2026 skrev Jo <jo@x.se>:\n{inbound}"),
            // Localized Outlook header blocks, French spacing included.
            format!(
                "Sounds good.\n\nVon: Jo <jo@x.de>\nGesendet: Dienstag, 3. November 2026\nAn: Me\nBetreff: AW: Plan\n\n{inbound}"
            ),
            format!(
                "Sounds good.\n\nDe : Jo <jo@x.fr>\nEnvoyé : mardi 3 novembre 2026\nÀ : Moi\nObjet : RE: plan\n\n{inbound}"
            ),
            format!(
                "Sounds good.\n\nDe: Jo <jo@x.es>\nEnviado: martes, 3 de noviembre de 2026\nPara: Yo\nAsunto: RE: plan\n\n{inbound}"
            ),
            format!(
                "Sounds good.\n\nDa: Jo <jo@x.it>\nInviato: martedì 3 novembre 2026\nA: Me\nOggetto: R: piano\n\n{inbound}"
            ),
            format!(
                "Sounds good.\n\nVan: Jo <jo@x.nl>\nVerzonden: dinsdag 3 november 2026\nAan: Mij\nOnderwerp: RE: plan\n\n{inbound}"
            ),
            format!(
                "Sounds good.\n\nDe: Jo <jo@x.pt>\nEnviada: terça-feira, 3 de novembro de 2026\nPara: Eu\nAssunto: RE: plano\n\n{inbound}"
            ),
            // Banners: German, French, and Thunderbird's spaced one.
            format!("Sounds good.\n\n-----Ursprüngliche Nachricht-----\n{inbound}"),
            format!("Sounds good.\n\n-----Message d'origine-----\n{inbound}"),
            format!("Sounds good.\n\n-------- Original Message --------\n{inbound}"),
            // Thunderbird's block: Subject and Date ABOVE From.
            format!(
                "Sounds good.\n\nSubject: Re: plan\nDate: Tue, 3 Nov 2026 09:00\nFrom: Jo <jo@x.com>\nTo: Me\n\n{inbound}"
            ),
        ] {
            let own = own_text(&body).unwrap();
            assert_eq!(own.trim(), "Sounds good.", "{body:?}");
            assert!(!own.contains("THEIR"), "{body:?}");
        }
        // Prose that merely mentions a header word is kept.
        for prose in [
            "From: the team, thanks for everything.",
            "De: nada.\nA: thanks again.",
        ] {
            assert_eq!(own_text(prose).as_deref(), Some(prose), "{prose:?}");
        }
    }

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
        // The link did not fit the budget whole, so it is left out whole.
        assert_eq!(out, "Short note. …");
        // Plain text fills the budget, the `…` inside it.
        let (out, cut) = excerpt(&"word ".repeat(100), 40);
        assert!(cut);
        assert_eq!(out.chars().count(), 40);
        assert!(out.ends_with("word word…"), "{out}");
        assert_eq!(excerpt("anything", 0), (String::new(), true));
        // Under budget: the condensed body comes back whole, marker included.
        let reply = "Hi\n\nOn Mon, Sep 28, 2026 Alice <a@example.com> wrote:\n> q";
        let (out, cut) = excerpt(reply, 200);
        assert!(!cut);
        assert_eq!(out, format!("Hi\n{QUOTE_MARKER}"));
        // The marker alone over the budget: whole or absent, never a piece.
        assert_eq!(excerpt(reply, 20), ("Hi…".into(), true));
    }

    /// The capped condenser says exactly where the stored text resumes: a
    /// plain cut at the cap, a shortened link counted at its stored length,
    /// and a link the cap falls inside left out whole with the resume at its
    /// start, so the page from there begins with it, raw.
    #[test]
    fn a_capped_body_says_exactly_where_the_stored_text_resumes() {
        assert_eq!(within("one two three", 7), ("one two".into(), Some(7)));
        assert_eq!(within("one two", 7), ("one two".into(), None));
        assert_eq!(within("one two", 100), ("one two".into(), None));
        // A shortened link before the cut stands for its whole stored length.
        let body = format!("see {TRACKER} then more words");
        let (kept, resume) = within(&body, 60);
        assert_eq!(kept.chars().count(), 60);
        assert!(kept.ends_with("… then m"), "{kept}");
        let resume = resume.unwrap();
        assert_eq!(resume, 4 + TRACKER.chars().count() + 7);
        assert_eq!(body.chars().skip(resume).collect::<String>(), "ore words");
        // The cap inside a long link: the link is left out, the page from
        // the offset begins with it.
        assert_eq!(within(&body, 20), ("see ".into(), Some(4)));
        // A short link is plain text (nothing stands in for it): cut like it.
        assert_eq!(
            within("go https://x.example/y now", 10),
            ("go https:/".into(), Some(10))
        );
        // Trailing whitespace is kept whole or dropped, never a cut: nothing
        // a page should fetch, and nothing to fill the context with either.
        assert_eq!(within("Hello\n\n", 5), ("Hello".into(), None));
        assert_eq!(within("Hello\n\n", 6), ("Hello".into(), None));
        assert_eq!(within("Hello\n\n", 9), ("Hello\n\n".into(), None));
        assert_eq!(condense_body("Hello\n\n"), "Hello\n\n");
        let padded = format!("Hi{}", "\n".repeat(30_000));
        assert_eq!(within(&padded, 16_000), ("Hi".into(), None));
        assert_eq!(condense_body(&padded), padded);
        // Scanned once, condensed twice: the same answers.
        let scanned = StoredBody::new("Hello\n\n".into());
        assert_eq!(scanned.condense_within(5), ("Hello".into(), None));
        assert_eq!(scanned.condense_within(3), ("Hel".into(), Some(3)));
        assert_eq!(scanned.text(), "Hello\n\n");
        // The cap reached exactly at a link: a cut at the link, found
        // without reading past it.
        let body = format!("abcd {TRACKER} tail");
        assert_eq!(within(&body, 5), ("abcd ".into(), Some(5)));
        // "http" that is not a scheme is skipped, not stopped at.
        let body = format!("httpx http: {TRACKER} and https://x.example/y");
        let out = condense_body(&body);
        assert!(out.starts_with("httpx http: https://click.mailer.example.com/"));
        assert!(out.contains("… and https://x.example/y"), "{out}");
        // The quote marker rides over the cap; a cut in the new text before
        // it leaves it out, the page from the offset reaching the quote in
        // its turn.
        let body = "Thanks!  \n> one\n> two\n> three\n";
        let whole = format!("Thanks!\n{QUOTE_MARKER}");
        assert_eq!(within(body, 20), (whole.clone(), None));
        assert_eq!(within(body, 50), (whole, None));
        assert_eq!(within(body, 5), ("Thank".into(), Some(5)));
        // Nothing kept: resume at the start. Nothing stored: nothing to cut.
        assert_eq!(within("abc", 0), (String::new(), Some(0)));
        assert_eq!(within("", 0), (String::new(), None));
    }

    /// The access check's reading: links shortened, the quote kept whole,
    /// the cut (when there is one) placed exactly as the condenser's.
    #[test]
    fn links_shortened_and_quotes_kept_for_the_access_check() {
        let reply = format!(
            "Your code is 482913. {TRACKER}\n\nOn Mon, Sep 28, 2026 Alice <a@example.com> wrote:\n> earlier"
        );
        let (kept, cut) = shorten_links_within(&reply, 400, Links::TRIAGE);
        assert!(cut.is_none());
        assert!(
            kept.starts_with(
                "Your code is 482913. https://click.mailer.example.com/ls/click?upn=u0…"
            )
        );
        assert!(kept.ends_with("wrote:\n> earlier"), "{kept}");
        assert!(!kept.contains(QUOTE_MARKER));
        assert_eq!(
            shorten_links_within(&reply, 12, Links::TRIAGE),
            ("Your code is".into(), Some(12))
        );
        // The access reading keeps the path that says what the link is for
        // and drops the query, so a long host does not push the path out; a
        // link under the threshold is shown whole, never lengthened.
        let generous = Links::ACCESS;
        let reset = "https://notifications.accounts.someservice.example.com/password/reset?token=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert!(reset.chars().count() > 192);
        assert_eq!(
            shorten_links_within(reset, 400, generous).0,
            "https://notifications.accounts.someservice.example.com/password/reset…"
        );
        let long_host = "https://email-notifications.identity.security-verification.someverylongcompanyname-accounts.example.com/auth/signin?token=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let (kept, _) = shorten_links_within(long_host, 400, generous);
        assert!(kept.ends_with("example.com/auth/signin…"), "{kept}");
        let short = &reset[..120];
        assert_eq!(shorten_links_within(short, 400, generous).0, short);
        assert_eq!(shorten_links_within(reset, 400, Links::RAW).0, reset);
    }
}
