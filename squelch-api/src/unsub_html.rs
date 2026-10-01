//! Reading an unsubscribe page: its words, its forms, and which of them is safe
//! to submit for the user.
//!
//! HAND-ROLLED, BOUNDED, AND NEVER EXECUTED. Same posture as the footer-link
//! scan in `crate::unsubscribe`: the page is hostile input, nothing here runs
//! it, and every loop walks forward through a string capped by the fetcher
//! (`unsub_fetch::MAX_BODY_BYTES`). A page that needs JavaScript to show its
//! button reads as one with no button, and goes to the user.
//!
//! THE SAFETY LINE IS [`Action::submission`]. The model (or the no-model rule)
//! decides WHICH action finishes the unsubscribe; this decides whether that
//! action can be taken without the user at all. It can when submitting it is
//! exactly what clicking its one button would do: no password, no file, no
//! choice left unmade, and NOTHING TO TYPE. A form whose email field the page
//! left empty is sent with it empty (or not at all, when it is required): the
//! page's host is the sender's choice at every hop, so no "same site" test can
//! say who would receive an address we typed, and the account's primary
//! address is not necessarily the one this list knows (plus-addressing,
//! aliases, forwards).
//!
//! BOUNDED WORK. Every scan is linear in the page: [`MAX_TAGS`] caps the walk,
//! [`MAX_CONTROLS`] caps a form, and no inner scan runs past its parent.

use std::collections::HashSet;

use reqwest::Method;
use url::Url;

/// Phrases that mean the unsubscribe has ALREADY happened. Past tense and
/// unambiguous on purpose: "you will no longer receive" opens half the confirm
/// pages on the internet ("click below and you will no longer receive...").
const DONE_PHRASES: &[&str] = &[
    "you have been unsubscribed",
    "you've been unsubscribed",
    "you’ve been unsubscribed",
    "you have successfully unsubscribed",
    "you've successfully unsubscribed",
    "you’ve successfully unsubscribed",
    "you are now unsubscribed",
    "you're now unsubscribed",
    "you’re now unsubscribed",
    "you are unsubscribed",
    "you have unsubscribed",
    "you've unsubscribed",
    "you’ve unsubscribed",
    "you are no longer subscribed",
    "you're no longer subscribed",
    "you’re no longer subscribed",
    "you're unsubscribed",
    "you’re unsubscribed",
    "unsubscribed successfully",
    "unsubscribe successful",
    "unsubscription successful",
    "has been unsubscribed",
    "have been removed from",
    "has been removed from",
    "you've been removed",
    "you’ve been removed",
    "we've removed you",
    "we’ve removed you",
    "we have removed you",
    "you have been opted out",
    "you've been opted out",
    "you’ve been opted out",
    "successfully opted out",
];

/// Words on a control that say "this one unsubscribes". Used to pick between
/// several buttons, and as the whole of the no-model rule.
const UNSUB_LABELS: &[&str] = &[
    "unsubscribe",
    "opt out",
    "opt-out",
    "remove me",
    "stop receiving",
    "stop sending",
    "stop all",
];
/// Weaker: "confirm" only counts when nothing says unsubscribe outright.
const CONFIRM_LABELS: &[&str] = &["confirm", "yes"];
/// Controls that take an unsubscribe BACK. They say "unsubscribe" too ("Undo
/// unsubscribe"), so they are excluded by name before anything else looks at
/// a label: pressing one is the one action worse than doing nothing.
const UNDO_LABELS: &[&str] = &[
    "undo",
    "cancel",
    "resubscribe",
    "re-subscribe",
    "subscribe again",
    "changed your mind",
    "change your mind",
    "didn't mean",
    "didn’t mean",
    "keep me",
    "stay subscribed",
    "remain subscribed",
    "not you",
];

/// A label that says "this one unsubscribes", and does not say "undo".
fn is_unsub_label(label: &str) -> bool {
    has_any(label, UNSUB_LABELS) && !has_any(label, UNDO_LABELS)
}

const MAX_TEXT_CHARS: usize = 20_000;
/// Tags read per page. A real unsubscribe page has a few hundred; past this
/// the rest is ignored, which can only cost us an action, never add one.
const MAX_TAGS: usize = 20_000;
/// Controls per form. A form with more is not an unsubscribe confirmation, and
/// is refused outright rather than read in part.
const MAX_CONTROLS: usize = 500;
const MAX_ACTIONS: usize = 8;
const MAX_LINKS: usize = 4;

#[derive(Debug, Clone)]
pub(crate) struct Page {
    pub title: String,
    pub text: String,
    pub actions: Vec<Action>,
    /// `<script>` elements seen. A page with scripts and almost no text is an
    /// app shell that only a browser can render.
    pub scripts: usize,
    /// A `<meta http-equiv=refresh>` target: a redirect that needs no script.
    pub meta_refresh: Option<Url>,
}

#[derive(Debug, Clone)]
pub(crate) enum Action {
    Form(Form),
    /// An anchor whose own text says it unsubscribes or confirms.
    Link {
        url: Url,
        label: String,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct Form {
    pub action: Url,
    pub method: Method,
    pub controls: Vec<Control>,
    pub submits: Vec<Submit>,
}

#[derive(Debug, Clone)]
pub(crate) enum Control {
    Hidden {
        name: String,
        value: String,
    },
    Text {
        name: String,
        value: String,
        kind: String,
        required: bool,
    },
    Checkbox {
        name: String,
        value: String,
        checked: bool,
        required: bool,
    },
    Radio {
        name: String,
        value: String,
        checked: bool,
        required: bool,
    },
    Select {
        name: String,
        value: Option<String>,
        required: bool,
    },
    TextArea {
        name: String,
        value: String,
        required: bool,
    },
    /// Password or file: a form carrying either is never submitted for the user.
    Forbidden,
}

#[derive(Debug, Clone)]
pub(crate) struct Submit {
    pub name: Option<String>,
    pub value: String,
    pub label: String,
    /// `formaction` / `formmethod` on the button, which override the form's.
    pub action: Option<Url>,
    pub method: Option<Method>,
}

/// A ready-to-send submission.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Submission {
    pub method: Method,
    pub url: Url,
    pub pairs: Vec<(String, String)>,
}

impl Page {
    /// The page says, in so many words, that the unsubscribe already happened,
    /// and does not ALSO offer an unsubscribe button. Pages that ship their
    /// success message beside the form (shown by script after the click) are
    /// the reason: hidden text is skipped by the parser, and a live button next
    /// to the words means the words are not about this visit yet.
    pub fn says_done(&self) -> bool {
        (says_done(&self.text) || says_done(&self.title))
            && !self
                .actions
                .iter()
                .any(|a| a.labels().iter().any(|l| is_unsub_label(l)))
    }

    /// A plain-text body, read as a page with no actions.
    pub fn from_text(body: &str) -> Page {
        Page {
            title: String::new(),
            text: collapse(body).chars().take(MAX_TEXT_CHARS).collect(),
            actions: Vec::new(),
            scripts: 0,
            meta_refresh: None,
        }
    }

    /// An app shell: scripts and next to no readable text.
    pub fn needs_browser(&self) -> bool {
        self.scripts > 0 && self.text.chars().filter(|c| c.is_alphanumeric()).count() < 40
    }

    /// One line per action, numbered by position, for the model.
    pub fn describe_actions(&self) -> Vec<String> {
        self.actions.iter().map(Action::describe).collect()
    }

    /// The no-model rule: exactly one action whose own label says unsubscribe,
    /// on a page that talks about unsubscribing. Anything less certain is the
    /// user's call.
    pub fn obvious_action(&self) -> Option<usize> {
        let lower = self.text.to_lowercase();
        if !(lower.contains("unsubscribe") || lower.contains("opt out")) {
            return None;
        }
        let mut hits = self
            .actions
            .iter()
            .enumerate()
            .filter(|(_, a)| a.labels().iter().any(|l| is_unsub_label(l)));
        let first = hits.next()?;
        hits.next().is_none().then_some(first.0)
    }
}

pub(crate) fn says_done(text: &str) -> bool {
    let lower = text.to_lowercase();
    DONE_PHRASES.iter().any(|p| lower.contains(p))
}

fn has_any(text: &str, phrases: &[&str]) -> bool {
    let lower = text.to_lowercase();
    phrases.iter().any(|p| lower.contains(p))
}

impl Action {
    fn labels(&self) -> Vec<String> {
        match self {
            Action::Link { label, .. } => vec![label.clone()],
            Action::Form(f) => f.submits.iter().map(|s| s.label.clone()).collect(),
        }
    }

    /// A form whose action is not a web address (`javascript:void(0)`, the
    /// usual sign of a form a script submits). Only a browser can take it.
    pub fn needs_script(&self) -> bool {
        matches!(self, Action::Form(f) if !matches!(f.action.scheme(), "http" | "https"))
    }

    pub fn describe(&self) -> String {
        match self {
            Action::Link { url, label } => {
                format!("LINK \"{label}\" to {}", url.host_str().unwrap_or("?"))
            }
            Action::Form(f) => f.describe(),
        }
    }

    /// The request that taking this action would send, or `None` when it cannot
    /// be taken without the user. See the module header for the line.
    pub fn submission(&self) -> Option<Submission> {
        match self {
            Action::Link { url, .. } => Some(Submission {
                method: Method::GET,
                url: url.clone(),
                pairs: Vec::new(),
            }),
            Action::Form(f) => f.submission(),
        }
    }
}

impl Form {
    fn describe(&self) -> String {
        let mut parts = vec![format!(
            "{} form to {}",
            self.method.as_str(),
            self.action.host_str().unwrap_or("?")
        )];
        let buttons: Vec<String> = self
            .submits
            .iter()
            .map(|s| format!("\"{}\"", s.label))
            .collect();
        parts.push(if buttons.is_empty() {
            "no buttons".into()
        } else {
            format!("buttons {}", buttons.join(", "))
        });
        let (mut checked, mut unchecked) = (0, 0);
        let mut radios: Vec<(String, usize, bool)> = Vec::new();
        for c in &self.controls {
            match c {
                Control::Text {
                    name, value, kind, ..
                } => parts.push(format!(
                    "{kind} field \"{name}\"{}",
                    if value.is_empty() {
                        " (empty)"
                    } else {
                        " (filled)"
                    }
                )),
                Control::Checkbox { checked: true, .. } => checked += 1,
                Control::Checkbox { checked: false, .. } => unchecked += 1,
                Control::Radio {
                    name, checked: on, ..
                } => match radios.iter_mut().find(|(n, _, _)| n == name) {
                    Some(r) => {
                        r.1 += 1;
                        r.2 |= *on;
                    }
                    None => radios.push((name.clone(), 1, *on)),
                },
                Control::Select { name, .. } => parts.push(format!("dropdown \"{name}\"")),
                Control::TextArea { name, .. } => parts.push(format!("text box \"{name}\"")),
                Control::Forbidden => parts.push("password or file field".into()),
                Control::Hidden { .. } => {}
            }
        }
        if checked + unchecked > 0 {
            parts.push(format!(
                "{} checkboxes ({checked} checked)",
                checked + unchecked
            ));
        }
        for (name, n, any) in radios {
            parts.push(format!(
                "choice \"{name}\" ({n} options, {})",
                if any { "one chosen" } else { "none chosen" }
            ));
        }
        parts.join("; ")
    }

    fn submission(&self) -> Option<Submission> {
        let submit = self.pick_submit()?;
        let mut pairs = Vec::new();
        let mut required_radios: HashSet<&str> = HashSet::new();
        let mut chosen_radios: HashSet<&str> = HashSet::new();
        for c in &self.controls {
            match c {
                Control::Forbidden => return None,
                Control::Hidden { name, value } => pairs.push((name.clone(), value.clone())),
                Control::Text {
                    name,
                    value,
                    required,
                    ..
                } => {
                    if !value.is_empty() {
                        pairs.push((name.clone(), value.clone()));
                    } else if *required {
                        return None;
                    } else {
                        pairs.push((name.clone(), String::new()));
                    }
                }
                Control::Checkbox {
                    name,
                    value,
                    checked,
                    required,
                } => {
                    if *checked {
                        pairs.push((name.clone(), value.clone()));
                    } else if *required {
                        return None;
                    }
                }
                Control::Radio {
                    name,
                    value,
                    checked,
                    required,
                } => {
                    if *required {
                        required_radios.insert(name);
                    }
                    if *checked {
                        chosen_radios.insert(name);
                        pairs.push((name.clone(), value.clone()));
                    }
                }
                Control::Select {
                    name,
                    value,
                    required,
                } => match value {
                    // A browser sends the chosen option's value even when it is
                    // empty; `required` is what refuses an empty one.
                    Some(v) if v.is_empty() && *required => return None,
                    Some(v) => pairs.push((name.clone(), v.clone())),
                    None if *required => return None,
                    None => {}
                },
                Control::TextArea {
                    name,
                    value,
                    required,
                } => {
                    if value.is_empty() && *required {
                        return None;
                    }
                    pairs.push((name.clone(), value.clone()));
                }
            }
        }
        // A required choice nobody made is a question for the user (usually
        // "which lists?"), and guessing the answer is not ours to do.
        if required_radios.iter().any(|r| !chosen_radios.contains(r)) {
            return None;
        }
        if let Some(name) = &submit.name {
            pairs.push((name.clone(), submit.value.clone()));
        }
        Some(Submission {
            method: submit.method.clone().unwrap_or_else(|| self.method.clone()),
            url: submit.action.clone().unwrap_or_else(|| self.action.clone()),
            pairs,
        })
    }

    /// The one button a person would press. A lone button is it, whatever it
    /// says (the model already judged the form). Among several, exactly one must
    /// say unsubscribe, or failing that exactly one must say confirm.
    fn pick_submit(&self) -> Option<&Submit> {
        match self.submits.as_slice() {
            [] => None,
            [only] if has_any(&only.label, UNDO_LABELS) => None,
            [only] => Some(only),
            many => {
                for phrases in [UNSUB_LABELS, CONFIRM_LABELS] {
                    let hits: Vec<&Submit> = many
                        .iter()
                        .filter(|s| has_any(&s.label, phrases) && !has_any(&s.label, UNDO_LABELS))
                        .collect();
                    match hits.as_slice() {
                        [one] => return Some(one),
                        [] => continue,
                        _ => return None,
                    }
                }
                None
            }
        }
    }
}

// ===========================================================================
// Parsing
// ===========================================================================

/// One tag: its lowercased name, its attributes, whether it closes, and the
/// byte offset just past its `>`.
struct Tag {
    name: String,
    attrs: Vec<(String, String)>,
    closing: bool,
    end: usize,
}

impl Tag {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
    fn has(&self, name: &str) -> bool {
        self.attrs.iter().any(|(k, _)| k == name)
    }
}

/// The next tag at or after `from`, quote-aware so `title="a>b"` does not end
/// the tag early. Comments and doctypes are skipped whole.
fn next_tag(html: &str, from: usize) -> Option<(usize, Tag)> {
    let bytes = html.as_bytes();
    let mut i = from;
    loop {
        let rel = html.get(i..)?.find('<')?;
        let start = i + rel;
        if html[start..].starts_with("<!--") {
            i = start + 4 + html[start + 4..].find("-->").map(|e| e + 3)?;
            continue;
        }
        let mut j = start + 1;
        let closing = bytes.get(j) == Some(&b'/');
        if closing {
            j += 1;
        }
        let name_start = j;
        while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'-') {
            j += 1;
        }
        if j == name_start {
            // `<!doctype`, `< 3`, stray brackets: not a tag we read.
            i = start + 1;
            continue;
        }
        let name = html[name_start..j].to_ascii_lowercase();
        let attr_start = j;
        let mut quote: Option<u8> = None;
        while j < bytes.len() {
            match (quote, bytes[j]) {
                (None, b'"') | (None, b'\'') => quote = Some(bytes[j]),
                (Some(q), c) if c == q => quote = None,
                (None, b'>') => break,
                _ => {}
            }
            j += 1;
        }
        if j >= bytes.len() {
            return None;
        }
        let attrs = parse_attrs(&html[attr_start..j]);
        return Some((
            start,
            Tag {
                name,
                attrs,
                closing,
                end: j + 1,
            },
        ));
    }
}

fn parse_attrs(raw: &str) -> Vec<(String, String)> {
    let b = raw.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let ns = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'/' {
            i += 1;
        }
        if i == ns {
            i += 1;
            continue;
        }
        let name = raw[ns..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                let vs = i + 1;
                let ve = raw[vs..].find(q as char).map(|e| vs + e).unwrap_or(b.len());
                value = decode_entities(&raw[vs..ve]);
                i = (ve + 1).min(b.len());
            } else {
                let vs = i;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = decode_entities(&raw[vs..i]);
            }
        }
        out.push((name, value));
    }
    out
}

/// The entities a page's text and attributes actually lean on.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        // The `;` within 12 BYTES, found on character boundaries: slicing at a
        // fixed byte offset panics inside "Shop & Save 🎉".
        let Some(semi) = rest
            .char_indices()
            .take_while(|(i, _)| *i < 12)
            .find(|(_, c)| *c == ';')
            .map(|(i, _)| i)
        else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..semi];
        let decoded = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            "rsquo" => Some('’'),
            "lsquo" => Some('‘'),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                u32::from_str_radix(&ent[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Elements whose content is never shown as text.
const HIDDEN_CONTENT: &[&str] = &["script", "style", "noscript", "template", "svg", "head"];
/// Elements that start a new line of text.
const BLOCKS: &[&str] = &[
    "p", "div", "br", "li", "tr", "h1", "h2", "h3", "h4", "h5", "h6", "section", "form", "label",
    "td", "table", "button", "option",
];

fn collapse(s: &str) -> String {
    let lines: Vec<String> = s
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect();
    lines.join("\n")
}

/// The text inside `[from, to)`, tags dropped.
fn inner_text(html: &str, from: usize, to: usize) -> String {
    let mut out = String::new();
    let mut i = from;
    while i < to {
        match next_tag(html, i) {
            Some((s, t)) if s < to => {
                out.push_str(&html[i..s]);
                out.push(' ');
                i = t.end.min(to);
            }
            _ => {
                out.push_str(&html[i..to]);
                break;
            }
        }
    }
    decode_entities(&out)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Offset of the matching `</name>` at or after `from`, or end of input.
fn close_of(html: &str, name: &str, from: usize) -> (usize, usize) {
    let mut i = from;
    while let Some((s, t)) = next_tag(html, i) {
        if t.closing && t.name == name {
            return (s, t.end);
        }
        i = t.end;
    }
    (html.len(), html.len())
}

pub(crate) fn parse_page(html: &str, page_url: &Url) -> Page {
    let mut title = String::new();
    let mut text = String::new();
    let mut actions: Vec<Action> = Vec::new();
    let mut links = 0;
    let mut scripts = 0;
    let mut meta_refresh = None;
    let mut form: Option<Form> = None;

    let mut i = 0;
    let mut tags = 0;
    loop {
        tags += 1;
        if tags > MAX_TAGS {
            break;
        }
        let Some((start, tag)) = next_tag(html, i) else {
            text.push_str(&html[i..]);
            break;
        };
        text.push_str(&html[i..start]);
        i = tag.end;
        let name = tag.name.as_str();

        if tag.closing {
            if name == "form"
                && let Some(f) = form.take()
                && actions.len() < MAX_ACTIONS
            {
                actions.push(Action::Form(f));
            }
            if BLOCKS.contains(&name) {
                text.push('\n');
            }
            continue;
        }

        // Hidden content is skipped whole, form controls inside it included:
        // a form a script reveals is a form for a browser.
        if !VOID.contains(&name) && is_hidden(&tag) {
            i = skip_element(html, name, i);
            continue;
        }

        match name {
            "title" => {
                let (s, e) = close_of(html, "title", i);
                title = inner_text(html, i, s);
                i = e;
                continue;
            }
            "meta" => {
                if tag
                    .attr("http-equiv")
                    .is_some_and(|v| v.eq_ignore_ascii_case("refresh"))
                    && let Some(content) = tag.attr("content")
                    && let Some(pos) = content.to_ascii_lowercase().find("url=")
                {
                    let target = content[pos + 4..].trim().trim_matches(['\'', '"']);
                    meta_refresh = page_url.join(target).ok();
                }
                continue;
            }
            _ if HIDDEN_CONTENT.contains(&name) => {
                if name == "script" {
                    scripts += 1;
                }
                // `<head>` is skipped for its text but its tags still matter
                // (title, meta), so only the raw-text elements jump ahead.
                if name != "head" {
                    let (_, e) = close_of(html, name, i);
                    i = e;
                }
                continue;
            }
            "form" => {
                // A form that never closed before the next one opens is closed
                // here, the way a browser would.
                if let Some(f) = form.take()
                    && actions.len() < MAX_ACTIONS
                {
                    actions.push(Action::Form(f));
                }
                let action = tag
                    .attr("action")
                    .filter(|a| !a.trim().is_empty())
                    .and_then(|a| page_url.join(a.trim()).ok())
                    .unwrap_or_else(|| page_url.clone());
                let method = if tag
                    .attr("method")
                    .is_some_and(|m| m.eq_ignore_ascii_case("post"))
                {
                    Method::POST
                } else {
                    Method::GET
                };
                form = Some(Form {
                    action,
                    method,
                    controls: Vec::new(),
                    submits: Vec::new(),
                });
            }
            "input" => {
                if let Some(f) = form.as_mut() {
                    read_input(&tag, f, page_url);
                }
            }
            "button" => {
                let (s, e) = close_of(html, "button", i);
                let label = inner_text(html, i, s);
                text.push(' ');
                text.push_str(&label);
                text.push('\n');
                i = e;
                if let Some(f) = form.as_mut() {
                    let kind = tag.attr("type").unwrap_or("submit").to_ascii_lowercase();
                    if kind == "submit" && !tag.has("disabled") {
                        let (action, method) = overrides(&tag, page_url);
                        f.submits.push(Submit {
                            name: tag
                                .attr("name")
                                .filter(|n| !n.is_empty())
                                .map(str::to_string),
                            value: tag.attr("value").unwrap_or("").to_string(),
                            label,
                            action,
                            method,
                        });
                    }
                }
                continue;
            }
            "select" => {
                let (s, e) = close_of(html, "select", i);
                if let Some(f) = form.as_mut()
                    && !over_cap(f)
                    && let Some(name) = tag.attr("name").filter(|n| !n.is_empty())
                    && !tag.has("disabled")
                {
                    f.controls.push(Control::Select {
                        name: name.to_string(),
                        value: selected_option(html, i, s),
                        required: tag.has("required"),
                    });
                }
                i = e;
                continue;
            }
            "textarea" => {
                let (s, e) = close_of(html, "textarea", i);
                if let Some(f) = form.as_mut()
                    && !over_cap(f)
                    && let Some(name) = tag.attr("name").filter(|n| !n.is_empty())
                    && !tag.has("disabled")
                {
                    f.controls.push(Control::TextArea {
                        name: name.to_string(),
                        value: decode_entities(&html[i..s]),
                        required: tag.has("required"),
                    });
                }
                i = e;
                continue;
            }
            "a" => {
                let (s, e) = close_of(html, "a", i);
                let label = inner_text(html, i, s);
                text.push_str(&label);
                i = e;
                if form.is_none()
                    && links < MAX_LINKS
                    && actions.len() < MAX_ACTIONS
                    && (has_any(&label, UNSUB_LABELS) || has_any(&label, CONFIRM_LABELS))
                    && !has_any(&label, UNDO_LABELS)
                    && let Some(href) = tag.attr("href")
                    && let Ok(url) = page_url.join(href.trim())
                    && matches!(url.scheme(), "http" | "https")
                    // A link back to the page we are on is not an action.
                    && url != *page_url
                {
                    links += 1;
                    actions.push(Action::Link { url, label });
                }
                continue;
            }
            _ => {}
        }
        if BLOCKS.contains(&name) {
            text.push('\n');
        }
        if text.len() > MAX_TEXT_CHARS * 4 {
            break;
        }
    }
    if let Some(f) = form.take()
        && actions.len() < MAX_ACTIONS
    {
        actions.push(Action::Form(f));
    }
    let text = collapse(&decode_entities(&text));
    let text: String = text.chars().take(MAX_TEXT_CHARS).collect();
    Page {
        title,
        text,
        actions,
        scripts,
        meta_refresh,
    }
}

fn read_input(tag: &Tag, f: &mut Form, page_url: &Url) {
    if tag.has("disabled") || over_cap(f) {
        return;
    }
    let kind = tag.attr("type").unwrap_or("text").to_ascii_lowercase();
    let name = tag.attr("name").unwrap_or("").to_string();
    let value = tag.attr("value").unwrap_or("").to_string();
    let required = tag.has("required");
    match kind.as_str() {
        "submit" | "image" => {
            let (action, method) = overrides(tag, page_url);
            f.submits.push(Submit {
                name: (!name.is_empty()).then_some(name),
                label: if value.is_empty() {
                    tag.attr("alt").unwrap_or("Submit").to_string()
                } else {
                    value.clone()
                },
                value,
                action,
                method,
            })
        }
        "password" | "file" => f.controls.push(Control::Forbidden),
        "button" | "reset" => {}
        _ if name.is_empty() => {}
        "hidden" => f.controls.push(Control::Hidden { name, value }),
        "checkbox" => f.controls.push(Control::Checkbox {
            name,
            value: if value.is_empty() { "on".into() } else { value },
            checked: tag.has("checked"),
            required,
        }),
        "radio" => f.controls.push(Control::Radio {
            name,
            value: if value.is_empty() { "on".into() } else { value },
            checked: tag.has("checked"),
            required,
        }),
        _ => f.controls.push(Control::Text {
            name,
            value,
            kind,
            required,
        }),
    }
}

/// A submit control's own `formaction` / `formmethod`.
fn overrides(tag: &Tag, page_url: &Url) -> (Option<Url>, Option<Method>) {
    let action = tag
        .attr("formaction")
        .filter(|a| !a.trim().is_empty())
        .and_then(|a| page_url.join(a.trim()).ok());
    let method = tag.attr("formmethod").map(|m| {
        if m.eq_ignore_ascii_case("post") {
            Method::POST
        } else {
            Method::GET
        }
    });
    (action, method)
}

/// Content a person never sees: the `hidden` attribute, inline
/// `display:none` / `visibility:hidden`, `aria-hidden`, and the usual utility
/// classes. AJAX unsubscribe pages ship their "you're unsubscribed" message
/// this way, waiting for a click; reading it as the page's answer would report
/// done for a request that never went out.
fn is_hidden(tag: &Tag) -> bool {
    if tag.has("hidden")
        || tag
            .attr("aria-hidden")
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    {
        return true;
    }
    if let Some(style) = tag.attr("style") {
        let style: String = style
            .to_ascii_lowercase()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if style.contains("display:none") || style.contains("visibility:hidden") {
            return true;
        }
    }
    tag.attr("class").is_some_and(|c| {
        c.split_whitespace().any(|k| {
            matches!(
                k.to_ascii_lowercase().as_str(),
                "hidden" | "d-none" | "hide" | "is-hidden" | "invisible" | "display-none"
            )
        })
    })
}

/// Elements with no content and no closing tag.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

/// Offset just past the `</name>` that closes the element opened before
/// `from`, counting nested elements of the same name. End of input when it
/// never closes. Linear in what it skips, and the caller resumes after it.
fn skip_element(html: &str, name: &str, from: usize) -> usize {
    let mut depth = 1usize;
    let mut i = from;
    while let Some((_, t)) = next_tag(html, i) {
        i = t.end;
        if t.name == name {
            if t.closing {
                depth -= 1;
                if depth == 0 {
                    return t.end;
                }
            } else {
                depth += 1;
            }
        }
    }
    html.len()
}

/// At [`MAX_CONTROLS`], the form is marked [`Control::Forbidden`] once (so it
/// is never submitted) and nothing more is read into it.
fn over_cap(f: &mut Form) -> bool {
    if f.controls.len() + f.submits.len() < MAX_CONTROLS {
        return false;
    }
    if !matches!(f.controls.last(), Some(Control::Forbidden)) {
        f.controls.push(Control::Forbidden);
    }
    true
}

/// The value a `<select>` submits: its `selected` option, else its first.
/// LINEAR: an option without a `value` reads its text up to the next tag, and
/// never scans for a `</option>` that a hostile page can leave out.
fn selected_option(html: &str, from: usize, to: usize) -> Option<String> {
    let mut first = None;
    let mut i = from;
    while let Some((s, t)) = next_tag(html, i) {
        if s >= to {
            break;
        }
        i = t.end;
        if t.closing || t.name != "option" {
            continue;
        }
        let value = match t.attr("value") {
            Some(v) => v.to_string(),
            None => {
                let stop = html[t.end..to].find('<').map_or(to, |p| t.end + p);
                inner_text(html, t.end, stop)
            }
        };
        if t.has("selected") {
            return Some(value);
        }
        first.get_or_insert(value);
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url() -> Url {
        Url::parse("https://news.example.com/u/abc?t=1").unwrap()
    }

    fn page(html: &str) -> Page {
        parse_page(html, &url())
    }

    fn only_submission(p: &Page) -> Option<Submission> {
        assert_eq!(p.actions.len(), 1, "{:#?}", p.actions);
        p.actions[0].submission()
    }

    #[test]
    fn done_page_reads_done_and_a_confirm_page_does_not() {
        let done = page(
            "<html><head><title>Bye</title><script>x()</script></head>\
             <body><h1>You&rsquo;ve been unsubscribed</h1>\
             <p>Mind telling us why?</p><form method=post action=/survey>\
             <input type=radio name=why value=a> Too many\
             <button>Send feedback</button></form></body></html>",
        );
        assert!(done.says_done());
        assert_eq!(done.title, "Bye");

        let confirm = page(
            "<p>Click below and you will no longer receive our emails.</p>\
             <form method=post><button>Unsubscribe</button></form>",
        );
        assert!(
            !confirm.says_done(),
            "future tense is a question, not an answer"
        );
    }

    #[test]
    fn single_button_confirm_form_is_submitted_like_a_click() {
        let p = page(
            r#"<form method="POST" action="/unsub/confirm">
                 <input type="hidden" name="token" value="a&amp;b">
                 <input type="email" name="email" value="me+news@example.org">
                 <input type="text" name="note" value="">
                 <input type="checkbox" name="all" value="1" checked>
                 <input type="checkbox" name="partners" value="1">
                 <input type="radio" name="reason" value="many">
                 <textarea name="other"></textarea>
                 <button type="submit" name="go" value="yes">Unsubscribe</button>
               </form>"#,
        );
        let s = only_submission(&p).expect("submittable");
        assert_eq!(s.method, Method::POST);
        assert_eq!(s.url.as_str(), "https://news.example.com/unsub/confirm");
        assert_eq!(
            s.pairs,
            vec![
                ("token".into(), "a&b".into()),
                ("email".into(), "me+news@example.org".into()),
                ("note".into(), String::new()),
                ("all".into(), "1".into()),
                ("other".into(), String::new()),
                ("go".into(), "yes".into()),
            ],
            "defaults exactly as a click would send them; the optional survey left unanswered"
        );
    }

    #[test]
    fn nothing_is_typed_an_empty_email_goes_empty_and_a_required_one_refuses() {
        let p = page(
            r#"<form method=post action="https://news.example.com/u">
                 <input type=email name=email><button>Unsubscribe</button></form>"#,
        );
        let s = only_submission(&p).expect("still submittable, just empty");
        assert_eq!(s.pairs, vec![("email".into(), String::new())]);

        let p = page(
            r#"<form method=post action="https://news.example.com/u">
                 <input type=email name=email required><button>Unsubscribe</button></form>"#,
        );
        assert!(
            only_submission(&p).is_none(),
            "a required field we may not fill"
        );
    }

    #[test]
    fn forms_that_want_a_person_are_refused() {
        for html in [
            "<form><input type=password name=p><button>Unsubscribe</button></form>",
            "<form><input type=file name=f><button>Unsubscribe</button></form>",
            "<form><input type=text name=name required><button>Unsubscribe</button></form>",
            "<form><input type=radio name=lists value=a required>\
             <input type=radio name=lists value=b required><button>Save</button></form>",
            "<form><select name=freq required><option value=''>Choose</option>\
             <option value=w>Weekly</option></select><button>Save</button></form>",
            "<form><input type=hidden name=t value=1></form>",
            "<form><button>Save preferences</button><button>Cancel</button></form>",
        ] {
            assert!(only_submission(&page(html)).is_none(), "{html}");
        }
    }

    #[test]
    fn among_several_buttons_the_unsubscribe_one_wins() {
        let p = page(
            "<form method=post><button name=a value=save>Update preferences</button>\
             <button name=a value=all>Unsubscribe from all</button></form>",
        );
        let s = only_submission(&p).unwrap();
        assert_eq!(s.pairs, vec![("a".into(), "all".into())]);
    }

    #[test]
    fn confirm_links_are_actions_and_the_obvious_one_is_found() {
        let p = page(
            "<p>Are you sure you want to unsubscribe?</p>\
             <a href='/home'>Home</a>\
             <a href='/u/abc/confirm?x=1&amp;y=2'>Yes, unsubscribe me</a>",
        );
        assert_eq!(p.actions.len(), 1);
        assert_eq!(p.obvious_action(), Some(0));
        let s = p.actions[0].submission().unwrap();
        assert_eq!(
            s.url.as_str(),
            "https://news.example.com/u/abc/confirm?x=1&y=2"
        );
        assert_eq!(s.method, Method::GET);
    }

    #[test]
    fn a_preference_center_has_no_obvious_action() {
        let p = page(
            "<h1>Email preferences</h1><p>Choose what you get. Unsubscribe from topics below.</p>\
             <form method=post><input type=checkbox name=t value=news checked> News\
             <input type=checkbox name=t value=deals checked> Deals\
             <button>Save</button></form>",
        );
        assert_eq!(p.obvious_action(), None);
        assert!(p.describe_actions()[0].contains("2 checkboxes (2 checked)"));
    }

    #[test]
    fn app_shells_and_meta_refresh_are_noticed() {
        let shell =
            page("<html><body><div id=root></div><script src=/app.js></script></body></html>");
        assert!(shell.needs_browser());

        let hop = page("<meta http-equiv=\"refresh\" content=\"0; URL='/u/abc/done'\">");
        assert_eq!(
            hop.meta_refresh.unwrap().as_str(),
            "https://news.example.com/u/abc/done"
        );
    }

    #[test]
    fn hostile_markup_terminates() {
        for html in [
            "<",
            "<a",
            "<form",
            "<form><button>",
            "<!--",
            "<title>x",
            "<select><option",
            "<a href='x>unsubscribe",
            "<<<<>>>>",
            "<textarea>never closed",
        ] {
            let _ = page(html);
        }
        let big = "<div>".repeat(50_000);
        let _ = page(&big);
    }

    /// The security review's case: options with no value and no closing tag
    /// made every option scan to the end of the page (16s at 160 KB, about
    /// 700s at the body cap). Now linear, and the form is refused as oversized.
    #[test]
    fn a_select_full_of_unclosed_options_parses_in_linear_time() {
        let html = format!(
            "<form><select name=x>{}</select><button>Go</button></form>",
            "<option>a".repeat(100_000)
        );
        assert!(html.len() <= crate::unsub_fetch::MAX_BODY_BYTES);
        let started = std::time::Instant::now();
        let p = page(&html);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "took {:?}",
            started.elapsed()
        );
        let _ = p.describe_actions();
        let _ = p.actions.iter().map(Action::submission).count();
    }

    #[test]
    fn a_form_past_the_control_cap_is_refused() {
        let radios: String = (0..2000)
            .map(|i| format!("<input type=radio name=r{i} value=a>"))
            .collect();
        let p = page(&format!(
            "<form>{radios}<button>Unsubscribe</button></form>"
        ));
        assert!(only_submission(&p).is_none());
    }

    /// Correctness review, critical: a fixed 12-byte slice after `&` panicked
    /// inside multibyte text, and the panic stranded the job.
    #[test]
    fn ampersands_beside_multibyte_text_do_not_panic() {
        for html in [
            "<p>Tom & 日本語のニュース</p>",
            "<p>Shop & Save 🎉🎉🎉</p>",
            "<a href='/u?x=1&ü=2'>Unsubscribe</a>",
        ] {
            let _ = page(html);
        }
        assert_eq!(page("<p>a &amp; 🎉</p>").text, "a & 🎉");
    }

    /// AJAX pages ship the success message hidden beside the form. It is not
    /// the page's answer until a click reveals it.
    #[test]
    fn hidden_success_text_is_not_done() {
        for hidden in [
            "<div style=\"display: none\">You have been unsubscribed.</div>",
            "<div hidden>You have been unsubscribed.</div>",
            "<div class=\"msg d-none\"><p>You have been unsubscribed.</p></div>",
            "<div aria-hidden=true>You have been unsubscribed.</div>",
        ] {
            let p = page(&format!(
                "<p>Unsubscribe from Acme?</p><form method=post><button>Unsubscribe</button></form>{hidden}"
            ));
            assert!(!p.says_done(), "{hidden}");
            assert_eq!(p.obvious_action(), Some(0));
        }
        // With no button on the page at all, only the hidden-skip stands
        // between the hidden text and a false "done".
        let p = page(
            "<p>Open the link in our app to finish.</p>\
             <div style='display:none'><h2>You have been unsubscribed.</h2></div>",
        );
        assert!(!p.says_done(), "hidden text alone is not an answer");
        // Visible success text beside a live unsubscribe button is not trusted
        // either: the button says the work is not done yet.
        let p = page(
            "<p>You have been unsubscribed.</p><form method=post><button>Unsubscribe</button></form>",
        );
        assert!(!p.says_done());
    }

    /// Correctness review, major: "Undo unsubscribe" is the one action worse
    /// than doing nothing.
    #[test]
    fn undo_links_and_buttons_are_never_actions() {
        let p = page(
            "<h1>Unsubscribed</h1><p>You have unsubscribed from Acme News.</p>\
             <a href=/resubscribe>Undo unsubscribe</a>\
             <form method=post action=/resub><button>Didn't mean to unsubscribe?</button></form>",
        );
        assert!(p.says_done(), "past tense reads as done");
        assert_eq!(p.obvious_action(), None);
        assert!(p.actions.iter().all(|a| !matches!(a, Action::Link { .. })));
        assert!(p.actions.iter().all(|a| a.submission().is_none()));
    }

    #[test]
    fn a_buttons_formaction_and_formmethod_win() {
        let p = page(
            "<form method=post action=/prefs/save><input type=hidden name=t value=1>\
             <button name=a value=save>Save preferences</button>\
             <button formaction=/unsub-all formmethod=post>Unsubscribe from all</button></form>",
        );
        let s = only_submission(&p).unwrap();
        assert_eq!(s.url.as_str(), "https://news.example.com/unsub-all");
        assert_eq!(s.pairs, vec![("t".into(), "1".into())]);
    }

    #[test]
    fn required_checkboxes_and_empty_options_behave_like_a_browser() {
        let p = page(
            "<form><input type=checkbox name=agree required><button>Unsubscribe</button></form>",
        );
        assert!(
            only_submission(&p).is_none(),
            "a required box we will not tick"
        );
        let p = page(
            "<form><select name=why><option value=''>Pick one</option><option value=a>A</option></select>\
             <button>Unsubscribe</button></form>",
        );
        assert_eq!(
            only_submission(&p).unwrap().pairs,
            vec![("why".into(), String::new())]
        );
    }

    #[test]
    fn a_script_only_form_needs_a_browser() {
        let p = page("<form action='javascript:void(0)'><button>Unsubscribe</button></form>");
        assert!(p.actions[0].needs_script());
    }

    #[test]
    fn plain_text_bodies_are_read() {
        assert!(Page::from_text("OK\nYou have been unsubscribed.\n").says_done());
    }
}
