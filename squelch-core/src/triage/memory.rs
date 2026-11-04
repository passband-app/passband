//! What Passband understands about the owner (docs/UNDERSTANDING.md §2.3): the
//! typed entries in `memory_entries`, the validator every model-written entry
//! passes before it is stored, and [`StoreMemoryContext`], which fills the
//! learned-memory seam the triage rewrite reserved ([`super::context`]).
//!
//! TYPED, NOT FREE TEXT. The pass may write only the five kinds in
//! [`MemoryKind`], each a few short fields with a cap, and the one sentence
//! the prompt sees is RENDERED FROM THOSE FIELDS here ([`MemoryBody::
//! render_line`]), never written by the model. Free prose is confined to a
//! sentence-sized field, which keeps the review page renderable and shrinks
//! what an email could smuggle into the owner's own profile.
//!
//! `origin` IS THE WHOLE AUTHORITY STORY. `stated` is the owner's word (the
//! review page's confirm or edit); `inferred` is the pass's guess. The pass may
//! revise or retire its own entries and may not touch a stated one, and
//! nothing it writes can come out stated: [`validate_ops`] has no path that
//! produces one.
//!
//! HUMAN DOOR ONLY. Triage is the human door's own backend and reads these;
//! `/mcp` never does (an invariant test in `squelch-mcp` pins it).

use super::context::{ContextScope, MemoryContextProvider, MemoryEntry};
use crate::features::{self, Features};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The most live entries (not deleted, retired or expired) one account holds,
/// stated ones included. The pass is told the cap and the validator enforces
/// it: a profile that grows without bound is a dossier, not an understanding.
pub const MAX_LIVE_ENTRIES: usize = 40;
/// The most lines one triage run is handed, and the bytes they may take
/// together. Small on purpose: these are context, and the message is the job.
pub const MAX_CONTEXT_LINES: usize = 20;
// Accepted: the byte cap counts line text only, not the JSON around each line.
pub const MAX_CONTEXT_BYTES: usize = 3_000;
/// An inferred entry lapses this long after the pass last stood by it. The
/// pass must re-affirm (keep or revise) or the entry stops being served:
/// jobs change and cofounders leave, and a guess nobody re-checks goes stale
/// without anyone noticing.
pub const INFERRED_TTL_DAYS: i64 = 90;
/// Provenance older than this reads as "old": confidence is discounted by
/// [`OLD_DISCOUNT`] at read time and the line says so.
pub const OLD_PROVENANCE_DAYS: i64 = 180;
pub const OLD_DISCOUNT: f64 = 0.5;
/// How close to `expires_at` a live entry must be for the pass to count as
/// having something to do (its change gate).
pub const EXPIRY_WARNING_DAYS: i64 = 14;

/// Field caps, in characters. `what` is the design's 120; the rest are sized
/// to one clause each.
pub const MAX_WHAT_CHARS: usize = 120;
pub const MAX_NAME_CHARS: usize = 60;
pub const MAX_NOTE_CHARS: usize = 160;
pub const MAX_BASIS_CHARS: usize = 120;
pub const MAX_ADDR_CHARS: usize = 254;
pub const MAX_SLUG_CHARS: usize = 40;
/// The owner's own edited line (the review page's free-text edit).
pub const MAX_LINE_CHARS: usize = 240;
/// Receipts per entry: enough to show "because" links, not a thread dump.
pub const MAX_PROVENANCE_IDS: usize = 12;
pub const MAX_PROVENANCE_ADDRS: usize = 12;
/// A `project`'s `until` may be at most this far out. A date years away is a
/// model writing a permanent fact into a field meant to expire.
pub const MAX_UNTIL_DAYS: i64 = 400;

/// Who an entry's words belong to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Stated,
    Inferred,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stated => "stated",
            Self::Inferred => "inferred",
        }
    }
    /// A stored value; anything unrecognised reads as `inferred`, the weaker
    /// authority, so a corrupt row can never be promoted by a typo.
    pub fn parse(s: &str) -> Self {
        if s == "stated" {
            Self::Stated
        } else {
            Self::Inferred
        }
    }
}

/// The five kinds. CLOSED: a kind the validator does not know is rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Role,
    Project,
    Person,
    Ignores,
    Prefers,
}

impl MemoryKind {
    pub const ALL: [MemoryKind; 5] = [
        Self::Role,
        Self::Project,
        Self::Person,
        Self::Ignores,
        Self::Prefers,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Role => "role",
            Self::Project => "project",
            Self::Person => "person",
            Self::Ignores => "ignores",
            Self::Prefers => "prefers",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// What a `person` is to the owner. CLOSED, like the kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    Cofounder,
    Teammate,
    Family,
    Friend,
    Client,
    Vendor,
    Recruiter,
    Investor,
    Assistant,
    Other,
}

impl Relation {
    pub const ALL: [Relation; 10] = [
        Self::Cofounder,
        Self::Teammate,
        Self::Family,
        Self::Friend,
        Self::Client,
        Self::Vendor,
        Self::Recruiter,
        Self::Investor,
        Self::Assistant,
        Self::Other,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cofounder => "cofounder",
            Self::Teammate => "teammate",
            Self::Family => "family",
            Self::Friend => "friend",
            Self::Client => "client",
            Self::Vendor => "vendor",
            Self::Recruiter => "recruiter",
            Self::Investor => "investor",
            Self::Assistant => "assistant",
            Self::Other => "other",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }
    /// "a cofounder", "family": the noun phrase the line uses.
    fn phrase(self) -> &'static str {
        match self {
            Self::Cofounder => "a cofounder",
            Self::Teammate => "a teammate",
            Self::Family => "family",
            Self::Friend => "a friend",
            Self::Client => "a client",
            Self::Vendor => "a vendor",
            Self::Recruiter => "a recruiter",
            Self::Investor => "an investor",
            Self::Assistant => "an assistant",
            Self::Other => "a contact",
        }
    }
}

/// One entry's typed fields. Stored as the fields alone (`body`), beside the
/// `kind` column, so the review page gets an object it can edit field by field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryBody {
    Role {
        what: String,
    },
    Project {
        name: String,
        note: String,
        until: Option<NaiveDate>,
    },
    Person {
        addr: String,
        relation: Relation,
        note: String,
    },
    Ignores {
        what: String,
        basis: String,
    },
    Prefers {
        what: String,
    },
}

/// Why an op or a field was refused. A snake_case code with no free text, so
/// a run's diagnostics can be counted and a client can say why in its own
/// words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Rejection {
    UnknownOp,
    UnknownKind,
    MissingEntry,
    MissingField {
        field: &'static str,
    },
    FieldOverCap {
        field: &'static str,
    },
    /// A link, a code-shaped run of digits, or a credential word.
    ForbiddenContent {
        field: &'static str,
    },
    BadRelation,
    BadDate,
    BadScope,
    /// A `person` entry not scoped to its own address.
    ScopeMismatch,
    UnknownAddr,
    /// A provenance id this account does not own, that the pass was not
    /// shown, or that is kept close.
    ForeignMessage,
    NoProvenance,
    TooMuchProvenance,
    BadConfidence,
    UnknownEntry,
    /// The pass tried to keep, revise or retire an entry the owner stated.
    TouchesStated,
    /// A revise that changes the entry's kind: a new kind is a new entry.
    KindChanged,
    /// Two ops in one document on the same entry.
    Duplicate,
    OverLiveCap,
}

/// Fold whitespace, drop control characters, and turn the dashes Passband's
/// copy never uses into the punctuation it does: these fields are shown to the
/// owner on the review page, and the model writes them.
pub fn clean_field(raw: &str) -> String {
    let folded: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .replace(" \u{2014} ", ", ")
        .replace(" \u{2013} ", ", ")
        .replace(['\u{2014}', '\u{2013}'], "-");
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Does `s` carry something no profile line may: a link, a code-shaped run of
/// six or more digits, or a credential word. The prompt forbids all three; this
/// is the floor under the prompt, because an email that talked the model into
/// one would otherwise ride every future triage run.
///
/// - A LINK is a scheme (`://`), a `www.`, or a bare domain with a path
///   (`acme.example/reset`): a host with a dot, a top-level label of two or
///   more letters that is not a file extension prose names (`Node.js/React`,
///   `notes.md/drafts`), and a path that starts with a letter or digit.
///   Accepted: a tech name shaped like that (`ASP.NET/C#`) is refused too.
/// - A CODE is six digits in one run. Digit GROUPS join across a single
///   space, hyphen or dot only when both are three digits or more, so
///   `123 456`, `123-456` and `1234-5678` are one run, while `9.30-17.30`
///   (a time range) is four short groups. Dates and year ranges starting
///   with a 19xx or 20xx year (`2026-11-30`, `2025-26`, `2025-2026`), a
///   standalone year (`ISO 27001 2022`), and thousands written with a one- or
///   two-digit lead (`1 000 000`, `1.000.000`) are left out of the count.
/// - A CREDENTIAL word is matched whole. `pin`, `token`, `one-time` and
///   `security code` count only with a digit within twenty characters ("my
///   pin is 4821", "token 48213"), because "Pin this", "token economics" and
///   "a one-time donor" are prose. The rest count anywhere.
pub fn forbidden_content(s: &str) -> bool {
    static NOT_CODES: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        // YEAR-SHAPED dates and ranges, standalone years, and thousands with a
        // short lead. A 4-4 code (`1234-5678`) matches none of them.
        regex::Regex::new(concat!(
            r"\b(?:19|20)\d{2}-(?:0[1-9]|1[0-2])(?:-(?:0[1-9]|[12]\d|3[01]))?\b",
            r"|\b(?:19|20)\d{2}-(?:(?:19|20)\d{2}|\d{2})\b",
            r"|\b\d{1,2}(?:[ .]\d{3})+\b",
            r"|\b(?:19|20)\d{2}\b",
        ))
        .expect("static date pattern")
    });
    static GATED: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\b(?:pins?|tokens?|one-time|security codes?)\b")
            .expect("static credential pattern")
    });
    const PROSE_EXTENSIONS: [&str; 6] = ["js", "ts", "py", "rs", "md", "txt"];
    let lower = s.to_lowercase();
    if lower.contains("://") || lower.contains("www.") {
        return true;
    }
    let bare_link = lower.split_whitespace().any(|word| {
        let word = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '/');
        let Some((host, path)) = word.split_once('/') else {
            return false;
        };
        let tld = host.rsplit_once('.').map(|(_, tld)| tld).unwrap_or("");
        host.contains('.')
            && tld.len() >= 2
            && tld.chars().all(|c| c.is_ascii_alphabetic())
            && !PROSE_EXTENSIONS.contains(&tld)
            && path.chars().next().is_some_and(|c| c.is_alphanumeric())
    });
    if bare_link {
        return true;
    }
    const PHRASES: [&str; 6] = [
        "password",
        "passcode",
        "passphrase",
        "api key",
        "login code",
        "verification code",
    ];
    if PHRASES.iter().any(|w| lower.contains(w)) {
        return true;
    }
    const WORDS: [&str; 2] = ["otp", "2fa"];
    if lower
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| WORDS.contains(&w))
    {
        return true;
    }
    let gated_with_digits = GATED.find_iter(&lower).any(|m| {
        let from = lower.floor_char_boundary(m.start().saturating_sub(20));
        let to = lower.ceil_char_boundary((m.end() + 20).min(lower.len()));
        lower[from..to].chars().any(|c| c.is_ascii_digit())
    });
    if gated_with_digits {
        return true;
    }
    longest_code_run(&NOT_CODES.replace_all(s, "#")) >= 6
}

/// The longest run of digits, joining two digit groups across ONE space,
/// hyphen or dot only when both groups are three digits or longer.
fn longest_code_run(s: &str) -> usize {
    let chars: Vec<char> = s.chars().collect();
    let mut best = 0usize;
    let mut run = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        // A non-digit does not reset the run by itself: whether the next
        // group joins it is decided below, from the one character between.
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        let group = i - start;
        // Did the previous group end just before one separator?
        let joined = start >= 2
            && matches!(chars[start - 1], ' ' | '-' | '.')
            && chars[start - 2].is_ascii_digit()
            && run >= 3
            && group >= 3;
        run = if joined { run + group } else { group };
        // A group of one or two digits ends any run: nothing joins it.
        let last_group_short = group < 3;
        best = best.max(run);
        if last_group_short {
            run = 0;
        }
    }
    best
}

fn bounded(
    fields: &Value,
    field: &'static str,
    cap: usize,
    required: bool,
) -> Result<String, Rejection> {
    let raw = fields.get(field).and_then(Value::as_str).unwrap_or("");
    let value = clean_field(raw);
    if value.is_empty() {
        return if required {
            Err(Rejection::MissingField { field })
        } else {
            Ok(value)
        };
    }
    if value.chars().count() > cap {
        return Err(Rejection::FieldOverCap { field });
    }
    if forbidden_content(&value) {
        return Err(Rejection::ForbiddenContent { field });
    }
    Ok(value)
}

/// A bare, lowercased address: `Sam <SAM@x.com>` and ` sam@x.com ` are one key,
/// the one `reading_senders::sender_key` and the scopes use.
pub fn normalize_addr(raw: &str) -> String {
    let raw = raw.trim();
    let inner = match (raw.rfind('<'), raw.rfind('>')) {
        (Some(open), Some(close)) if open < close => &raw[open + 1..close],
        _ => raw,
    };
    inner.trim().to_ascii_lowercase()
}

fn plausible_addr(addr: &str) -> bool {
    addr.len() <= MAX_ADDR_CHARS
        && addr.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && domain.contains('.') && !domain.contains('@')
        })
        && !addr.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// The domain an address is at, lowercased; `None` for something that is not
/// an address.
pub fn addr_domain(addr: &str) -> Option<String> {
    let addr = normalize_addr(addr);
    plausible_addr(&addr).then(|| addr.rsplit_once('@').map(|(_, d)| d.to_string()))?
}

impl MemoryBody {
    pub fn kind(&self) -> MemoryKind {
        match self {
            Self::Role { .. } => MemoryKind::Role,
            Self::Project { .. } => MemoryKind::Project,
            Self::Person { .. } => MemoryKind::Person,
            Self::Ignores { .. } => MemoryKind::Ignores,
            Self::Prefers { .. } => MemoryKind::Prefers,
        }
    }

    /// Parse and bound one kind's fields. Fields that kind does not have are
    /// ignored, so the flat shape the model fills (every field, most null)
    /// parses for every kind. Address MEMBERSHIP (is it in the ledger) is the
    /// validator's, not this: this sees one entry, not the store.
    pub fn from_fields(kind: &str, fields: &Value, today: NaiveDate) -> Result<Self, Rejection> {
        let kind = MemoryKind::parse(kind).ok_or(Rejection::UnknownKind)?;
        Ok(match kind {
            MemoryKind::Role => Self::Role {
                what: bounded(fields, "what", MAX_WHAT_CHARS, true)?,
            },
            MemoryKind::Project => {
                let until = match fields.get("until").and_then(Value::as_str) {
                    None | Some("") => None,
                    Some(raw) => {
                        let date = NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
                            .map_err(|_| Rejection::BadDate)?;
                        // Not in the past (a finished project is not live,
                        // so writing one is writing nothing) and not years
                        // out. `today` is the owner-local date.
                        if date < today || date > today + Duration::days(MAX_UNTIL_DAYS) {
                            return Err(Rejection::BadDate);
                        }
                        Some(date)
                    }
                };
                Self::Project {
                    name: bounded(fields, "name", MAX_NAME_CHARS, true)?,
                    note: bounded(fields, "note", MAX_NOTE_CHARS, false)?,
                    until,
                }
            }
            MemoryKind::Person => {
                let addr = normalize_addr(fields.get("addr").and_then(Value::as_str).unwrap_or(""));
                if addr.is_empty() {
                    return Err(Rejection::MissingField { field: "addr" });
                }
                if addr.chars().count() > MAX_ADDR_CHARS {
                    return Err(Rejection::FieldOverCap { field: "addr" });
                }
                if !plausible_addr(&addr) {
                    return Err(Rejection::UnknownAddr);
                }
                let relation = fields
                    .get("relation")
                    .and_then(Value::as_str)
                    .and_then(Relation::parse)
                    .ok_or(Rejection::BadRelation)?;
                Self::Person {
                    addr,
                    relation,
                    note: bounded(fields, "note", MAX_NOTE_CHARS, false)?,
                }
            }
            MemoryKind::Ignores => Self::Ignores {
                what: bounded(fields, "what", MAX_WHAT_CHARS, true)?,
                basis: bounded(fields, "basis", MAX_BASIS_CHARS, false)?,
            },
            MemoryKind::Prefers => Self::Prefers {
                what: bounded(fields, "what", MAX_WHAT_CHARS, true)?,
            },
        })
    }

    /// The fields alone, as stored in `body` and served to the review page.
    pub fn fields(&self) -> Value {
        match self {
            Self::Role { what } | Self::Prefers { what } => json!({ "what": what }),
            Self::Project { name, note, until } => json!({
                "name": name,
                "note": note,
                "until": until.map(|d| d.format("%Y-%m-%d").to_string()),
            }),
            Self::Person {
                addr,
                relation,
                note,
            } => json!({ "addr": addr, "relation": relation.as_str(), "note": note }),
            Self::Ignores { what, basis } => json!({ "what": what, "basis": basis }),
        }
    }

    /// The one sentence the prompt sees. Built from the bounded fields only,
    /// so a line can never carry more than its fields did.
    pub fn render_line(&self) -> String {
        let line = match self {
            Self::Role { what } => what.clone(),
            Self::Project { name, note, until } => {
                let mut line = if note.is_empty() {
                    format!("Working on {name}")
                } else {
                    format!("Working on {name}: {note}")
                };
                if let Some(until) = until {
                    line.push_str(&format!(" (through {})", until.format("%Y-%m-%d")));
                }
                line
            }
            Self::Person {
                addr,
                relation,
                note,
            } => {
                if note.is_empty() {
                    format!("{addr} is {}", relation.phrase())
                } else {
                    format!("{addr} is {}; {note}", relation.phrase())
                }
            }
            Self::Ignores { what, basis } => {
                if basis.is_empty() {
                    format!("Ignores {what}")
                } else {
                    format!("Ignores {what} ({basis})")
                }
            }
            Self::Prefers { what } => format!("Prefers {what}"),
        };
        // An address leads a person's line and stays as written.
        sentence(&line, !matches!(self, Self::Person { .. }))
    }
}

/// End on a full stop, capitalising the first letter when asked.
fn sentence(line: &str, capitalise: bool) -> String {
    let line = line.trim().trim_end_matches(['.', ';', ',']);
    let mut chars = line.chars();
    let mut out = match chars.next() {
        Some(first) if capitalise => first.to_uppercase().chain(chars).collect::<String>(),
        Some(_) => line.to_string(),
        None => String::new(),
    };
    if !out.is_empty() && !out.ends_with(['!', '?']) {
        out.push('.');
    }
    out
}

/// What an entry rests on. Every id was checked, when it was written, to be
/// this account's own, unsealed mail the pass was shown.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    #[serde(default)]
    pub message_ids: Vec<i64>,
    #[serde(default)]
    pub addrs: Vec<String>,
}

/// An entry's scope. `owner` is about the owner; `sender:`/`domain:` are
/// served only on mail from that address or domain; `kind:<slug>` is served on
/// every run (the triage question's "nothing like this", §4, lands there).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Owner,
    Sender(String),
    Domain(String),
    Kind(String),
}

impl Scope {
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw == "owner" {
            return Some(Self::Owner);
        }
        if let Some(addr) = raw.strip_prefix("sender:") {
            let addr = normalize_addr(addr);
            return plausible_addr(&addr).then_some(Self::Sender(addr));
        }
        if let Some(domain) = raw.strip_prefix("domain:") {
            let domain = domain.trim().to_ascii_lowercase();
            let ok = domain.len() <= MAX_ADDR_CHARS
                && domain.contains('.')
                && domain
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
            return ok.then_some(Self::Domain(domain));
        }
        if let Some(slug) = raw.strip_prefix("kind:") {
            let ok = !slug.is_empty()
                && slug.len() <= MAX_SLUG_CHARS
                && slug
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
            return ok.then(|| Self::Kind(slug.to_string()));
        }
        None
    }
    pub fn as_string(&self) -> String {
        match self {
            Self::Owner => "owner".into(),
            Self::Sender(addr) => format!("sender:{addr}"),
            Self::Domain(domain) => format!("domain:{domain}"),
            Self::Kind(slug) => format!("kind:{slug}"),
        }
    }
}

/// One stored row, as the store reads it back.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MemoryRecord {
    pub id: String,
    pub scope: String,
    pub origin: Origin,
    pub kind: String,
    /// The fields, as stored. Typed through [`MemoryBody::from_fields`] when
    /// it has to be (a revise, an edit); served as an object either way.
    pub body: Value,
    pub line: String,
    pub provenance: Provenance,
    pub confidence: f64,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub affirmed_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub retired_at: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub deleted_at: Option<DateTime<Utc>>,
}

impl MemoryRecord {
    /// Served to triage and counted against [`MAX_LIVE_ENTRIES`]: not deleted,
    /// not retired, not expired, and not a project whose `until` has passed.
    /// `today` is the OWNER-LOCAL date: "through 30 November" means the
    /// owner's 30 November, and for an American evening UTC is already the
    /// 1st.
    pub fn is_live(&self, now: DateTime<Utc>, today: NaiveDate) -> bool {
        self.deleted_at.is_none()
            && self.retired_at.is_none()
            && self.expires_at.is_none_or(|at| at > now)
            && self.until().is_none_or(|until| until >= today)
    }

    /// A project's `until`, when it has one.
    pub fn until(&self) -> Option<NaiveDate> {
        if self.kind != MemoryKind::Project.as_str() {
            return None;
        }
        self.body
            .get("until")
            .and_then(Value::as_str)
            .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
    }
}

/// When an inferred entry affirmed at `at` lapses.
pub fn inferred_expiry(at: DateTime<Utc>) -> DateTime<Utc> {
    at + Duration::days(INFERRED_TTL_DAYS)
}

// ---------------------------------------------------------------------------
// The pass's output and its validator.
// ---------------------------------------------------------------------------

/// One entry as the model writes it: the flat shape the output schema has,
/// every field present and most of them null. Loosely typed ON PURPOSE: a
/// typed enum here would turn one unknown kind into a parse failure of the
/// whole document, where the validator drops that op and keeps the rest.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawEntry {
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub what: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
    #[serde(default)]
    pub addr: Option<String>,
    #[serde(default)]
    pub relation: Option<String>,
    #[serde(default)]
    pub basis: Option<String>,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub message_ids: Vec<i64>,
    #[serde(default)]
    pub addrs: Vec<String>,
}

impl RawEntry {
    fn fields(&self) -> Value {
        json!({
            "what": self.what, "name": self.name, "note": self.note, "until": self.until,
            "addr": self.addr, "relation": self.relation, "basis": self.basis,
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawOp {
    #[serde(default)]
    pub op: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub entry: Option<RawEntry>,
}

/// The whole document the pass returns.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ProfileOps {
    #[serde(default)]
    pub ops: Vec<RawOp>,
}

/// An entry that passed every check.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidEntry {
    pub scope: String,
    pub body: MemoryBody,
    pub provenance: Provenance,
    pub confidence: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ValidOp {
    Keep { id: String },
    Revise { id: String, entry: ValidEntry },
    Retire { id: String },
    New { entry: ValidEntry },
}

/// What the validator knows about the store, read inside the transaction that
/// applies the result, so nothing it checked can move before the write.
pub struct Facts<'a> {
    /// Every non-deleted entry of the account, retired ones included.
    pub existing: &'a [MemoryRecord],
    /// The addresses, of those the document mentions, that are in `contacts`
    /// or `senders` for this account.
    pub known_addrs: &'a BTreeSet<String>,
    /// The message ids, of those the document mentions, this account owns and
    /// that are not kept close (`sealed_mail_sql!`).
    pub owned_ids: &'a BTreeSet<i64>,
    /// The message ids the pass was SHOWN this run. The mirror of decision
    /// evidence (`agent::validate_decision` checks citations against the
    /// sources the run observed): a model may cite only what it read.
    pub exposed_ids: &'a BTreeSet<i64>,
    pub now: DateTime<Utc>,
    /// The owner-local date, for `until`.
    pub today: NaiveDate,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Validated {
    pub ops: Vec<ValidOp>,
    /// `(index into the document's ops, why)`.
    pub rejected: Vec<(usize, Rejection)>,
}

/// Every address and message id a document mentions, so the store can answer
/// [`Facts::known_addrs`] and [`Facts::owned_ids`] with two bounded queries.
pub fn mentioned(ops: &[RawOp]) -> (BTreeSet<String>, BTreeSet<i64>) {
    let mut addrs = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for entry in ops.iter().filter_map(|op| op.entry.as_ref()) {
        addrs.extend(entry.addrs.iter().map(|a| normalize_addr(a)));
        if let Some(addr) = &entry.addr {
            addrs.insert(normalize_addr(addr));
        }
        if let Some(Scope::Sender(addr)) = Scope::parse(&entry.scope) {
            addrs.insert(addr);
        }
        ids.extend(entry.message_ids.iter().copied());
    }
    (addrs, ids)
}

fn check_entry(
    raw: &RawEntry,
    facts: &Facts<'_>,
    allowed_ids: &dyn Fn(i64) -> bool,
) -> Result<ValidEntry, Rejection> {
    let body = MemoryBody::from_fields(&raw.kind, &raw.fields(), facts.today)?;
    let scope = Scope::parse(&raw.scope).ok_or(Rejection::BadScope)?;
    match &scope {
        Scope::Sender(addr) if !facts.known_addrs.contains(addr) => {
            return Err(Rejection::UnknownAddr);
        }
        Scope::Domain(domain)
            if !facts
                .known_addrs
                .iter()
                .any(|a| addr_domain(a).as_deref() == Some(domain)) =>
        {
            return Err(Rejection::UnknownAddr);
        }
        _ => {}
    }
    if let MemoryBody::Person { addr, .. } = &body {
        if !facts.known_addrs.contains(addr) {
            return Err(Rejection::UnknownAddr);
        }
        // A person entry is ABOUT that person, so it is served on their mail
        // and nowhere else: scoped `owner`, a line naming one address would be
        // handed to every triage run in the mailbox.
        if scope != Scope::Sender(addr.clone()) {
            return Err(Rejection::ScopeMismatch);
        }
    }
    if !(0.0..=1.0).contains(&raw.confidence) || raw.confidence.is_nan() {
        return Err(Rejection::BadConfidence);
    }
    let mut message_ids: Vec<i64> = raw.message_ids.clone();
    message_ids.sort_unstable();
    message_ids.dedup();
    let mut addrs: Vec<String> = raw.addrs.iter().map(|a| normalize_addr(a)).collect();
    addrs.sort();
    addrs.dedup();
    if message_ids.is_empty() && addrs.is_empty() {
        return Err(Rejection::NoProvenance);
    }
    if message_ids.len() > MAX_PROVENANCE_IDS || addrs.len() > MAX_PROVENANCE_ADDRS {
        return Err(Rejection::TooMuchProvenance);
    }
    if message_ids
        .iter()
        .any(|id| !facts.owned_ids.contains(id) || !allowed_ids(*id))
    {
        return Err(Rejection::ForeignMessage);
    }
    if addrs.iter().any(|a| !facts.known_addrs.contains(a)) {
        return Err(Rejection::UnknownAddr);
    }
    Ok(ValidEntry {
        scope: scope.as_string(),
        body,
        provenance: Provenance { message_ids, addrs },
        confidence: raw.confidence,
    })
}

/// Validate a document of ops against the store's facts. Op by op: one bad op
/// is dropped with its reason and the rest stand, because a whole night's
/// re-affirmations lost to one unknown address would expire good entries.
///
/// THE CAP is checked last, over the result: the live entries that remain once
/// every accepted retire, revise and keep is applied, plus the accepted news
/// in document order. A `new` that would make it 41 is dropped.
pub fn validate_ops(ops: &[RawOp], facts: &Facts<'_>) -> Validated {
    let by_id: BTreeMap<&str, &MemoryRecord> =
        facts.existing.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut seen = BTreeSet::<String>::new();
    let mut out = Validated::default();
    let mut retired = BTreeSet::<String>::new();
    let mut news = Vec::new();
    for (index, op) in ops.iter().enumerate() {
        let target = |id: &Option<String>| -> Result<&MemoryRecord, Rejection> {
            let id = id.as_deref().ok_or(Rejection::UnknownEntry)?;
            let record = by_id.get(id).ok_or(Rejection::UnknownEntry)?;
            if record.origin == Origin::Stated {
                return Err(Rejection::TouchesStated);
            }
            // Only what is live can be kept or revised: a retired tombstone
            // stays a tombstone, and an expired guess comes back as a `new`
            // with fresh receipts or not at all.
            if !record.is_live(facts.now, facts.today) {
                return Err(Rejection::UnknownEntry);
            }
            Ok(record)
        };
        let result = match op.op.as_str() {
            // An id is SEEN only once an op on it has validated: a revise that
            // fails its checks must not turn the keep after it into a
            // duplicate, or the entry the model meant to stand by lapses.
            "keep" | "retire" | "revise" => target(&op.id).and_then(|record| {
                if seen.contains(&record.id) {
                    return Err(Rejection::Duplicate);
                }
                match op.op.as_str() {
                    "keep" => Ok(ValidOp::Keep {
                        id: record.id.clone(),
                    }),
                    "retire" => Ok(ValidOp::Retire {
                        id: record.id.clone(),
                    }),
                    _ => {
                        let raw = op.entry.as_ref().ok_or(Rejection::MissingEntry)?;
                        // The entry's own receipts stay citable on a revise:
                        // they were checked when it was written, and the
                        // ownership check above still runs over them now.
                        let prior = &record.provenance.message_ids;
                        let entry = check_entry(raw, facts, &|id| {
                            facts.exposed_ids.contains(&id) || prior.contains(&id)
                        })?;
                        if entry.body.kind().as_str() != record.kind {
                            return Err(Rejection::KindChanged);
                        }
                        Ok(ValidOp::Revise {
                            id: record.id.clone(),
                            entry,
                        })
                    }
                }
            }),
            "new" => op
                .entry
                .as_ref()
                .ok_or(Rejection::MissingEntry)
                .and_then(|raw| check_entry(raw, facts, &|id| facts.exposed_ids.contains(&id)))
                .map(|entry| ValidOp::New { entry }),
            _ => Err(Rejection::UnknownOp),
        };
        match result {
            Ok(ValidOp::New { entry }) => news.push((index, entry)),
            Ok(op) => {
                let id = match &op {
                    ValidOp::Keep { id } | ValidOp::Revise { id, .. } => id,
                    ValidOp::Retire { id } => {
                        retired.insert(id.clone());
                        id
                    }
                    ValidOp::New { .. } => unreachable!("matched above"),
                };
                seen.insert(id.clone());
                out.ops.push(op);
            }
            Err(why) => out.rejected.push((index, why)),
        }
    }
    let mut live = facts
        .existing
        .iter()
        .filter(|r| r.is_live(facts.now, facts.today) && !retired.contains(&r.id))
        .count();
    for (index, entry) in news {
        if live >= MAX_LIVE_ENTRIES {
            out.rejected.push((index, Rejection::OverLiveCap));
        } else {
            live += 1;
            out.ops.push(ValidOp::New { entry });
        }
    }
    out.rejected.sort_by_key(|(index, _)| *index);
    out
}

// ---------------------------------------------------------------------------
// Ids.
// ---------------------------------------------------------------------------

/// A ULID: 48 bits of milliseconds then 80 random bits, in Crockford base32.
/// Sorts by creation, so the review page's "newest first" is an `ORDER BY id`.
/// Written here rather than taken as a crate: it is twenty lines, and the OS
/// entropy it needs is already a dependency (`getrandom`, for tracking tokens).
pub fn new_ulid(now: DateTime<Utc>) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let ms = now.timestamp_millis().max(0) as u128 & ((1u128 << 48) - 1);
    let mut random = [0u8; 10];
    if getrandom::fill(&mut random).is_err() {
        // No entropy is not a reason to fail a write: fall back to the clock's
        // nanoseconds, which still separates two ids minted in one millisecond.
        random[..8].copy_from_slice(&now.timestamp_nanos_opt().unwrap_or(0).to_le_bytes());
    }
    let mut value = ms << 80;
    for (i, byte) in random.iter().enumerate() {
        value |= (*byte as u128) << (8 * (9 - i));
    }
    (0..26)
        .rev()
        .map(|i| ALPHABET[((value >> (5 * i)) & 0x1f) as usize] as char)
        .collect()
}

/// Whether `s` is shaped like an id [`new_ulid`] mints: 26 Crockford base32
/// characters. The human door checks a path id with this before it reaches
/// the store or the audit log's `target`, so neither ever holds arbitrary
/// client text.
pub fn is_ulid(s: &str) -> bool {
    s.len() == 26
        && s.bytes()
            .all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b))
}

// ---------------------------------------------------------------------------
// Serving.
// ---------------------------------------------------------------------------

/// One candidate line for a triage run, as the store reads it: the record and
/// the newest `received_at` among its provenance messages (`None` when it rests
/// on addresses only, or on messages no longer stored).
pub type ServedRow = (MemoryRecord, Option<DateTime<Utc>>);

/// Order, discount and cap what a run is handed. Pure, so the ordering rules
/// are tested without a store.
///
/// - Live ([`MemoryRecord::is_live`]): not deleted, retired or expired, and
///   not a project whose `until` passed.
/// - INFERRED confidence is halved and the line says "(old)" when every
///   message it rests on is older than [`OLD_PROVENANCE_DAYS`]. A STATED entry
///   is the owner's word and is not aged: "I am a cofounder of X" does not get
///   less true for resting on last year's mail.
/// - `stated` first, then by (discounted) confidence, then newest affirmation.
/// - At most [`MAX_CONTEXT_LINES`] lines and [`MAX_CONTEXT_BYTES`] of text; a
///   line that would overflow the bytes is skipped, not cut.
///
/// `today` is the owner-local date ([`MemoryRecord::is_live`]).
pub fn select_for_context(
    rows: Vec<ServedRow>,
    now: DateTime<Utc>,
    today: NaiveDate,
) -> Vec<MemoryEntry> {
    let mut candidates: Vec<(MemoryRecord, f64, bool)> = rows
        .into_iter()
        .filter(|(record, _)| record.is_live(now, today))
        .map(|(record, newest)| {
            let old = record.origin == Origin::Inferred
                && !record.provenance.message_ids.is_empty()
                && newest.is_some_and(|at| now - at > Duration::days(OLD_PROVENANCE_DAYS));
            let confidence = if old {
                record.confidence * OLD_DISCOUNT
            } else {
                record.confidence
            };
            (record, confidence, old)
        })
        .collect();
    candidates.sort_by(|a, b| {
        let stated = |r: &MemoryRecord| r.origin == Origin::Stated;
        stated(&b.0)
            .cmp(&stated(&a.0))
            .then(b.1.total_cmp(&a.1))
            .then(b.0.affirmed_at.cmp(&a.0.affirmed_at))
            .then(a.0.id.cmp(&b.0.id))
    });
    let mut out = Vec::new();
    let mut bytes = 0usize;
    for (record, confidence, old) in candidates {
        if out.len() >= MAX_CONTEXT_LINES {
            break;
        }
        let text = if old {
            format!("{} (old)", record.line)
        } else {
            record.line.clone()
        };
        if bytes + text.len() > MAX_CONTEXT_BYTES {
            continue;
        }
        bytes += text.len();
        out.push(MemoryEntry {
            id: record.id,
            scope: record.scope,
            provenance: record.provenance.message_ids,
            revision: record.revision,
            text,
            origin: record.origin.as_str().to_string(),
            confidence: (confidence * 100.0).round() / 100.0,
        });
    }
    out
}

/// The scopes one triage run may see: the owner, the message's sender and the
/// sender's domain. `kind:` scopes are served on every run and are matched by
/// prefix in the store, not listed here.
pub fn scopes_for(sender: &str) -> Vec<String> {
    let mut scopes = vec![Scope::Owner.as_string()];
    let addr = normalize_addr(sender);
    if plausible_addr(&addr) {
        if let Some(domain) = addr_domain(&addr) {
            scopes.push(Scope::Domain(domain).as_string());
        }
        scopes.push(Scope::Sender(addr).as_string());
    }
    scopes
}

/// The learned-memory seam, filled: entries from `memory_entries` for this run.
///
/// OFF IS AS IF IT DID NOT EXIST. With the `understanding` feature off this
/// returns nothing without reading the store, so a mailbox whose flag is
/// switched off stops seeing its profile on the very next job.
pub struct StoreMemoryContext<'a, S: crate::store::memory::MemoryStore + ?Sized> {
    pub store: &'a S,
    pub features: &'a Features,
    pub now: DateTime<Utc>,
    /// The owner-local date (`OwnerClock::today`).
    pub today: NaiveDate,
}

impl<S: crate::store::memory::MemoryStore + ?Sized> MemoryContextProvider
    for StoreMemoryContext<'_, S>
{
    fn relevant_memory(&self, scope: &ContextScope) -> Result<Vec<MemoryEntry>, String> {
        if !self.features.enabled(features::UNDERSTANDING) {
            return Ok(Vec::new());
        }
        let rows = self
            .store
            .memory_context_rows(scope.account_id, &scopes_for(&scope.sender), self.now)
            .map_err(|e| e.to_string())?;
        Ok(select_for_context(rows, self.now, self.today))
    }
}

#[cfg(test)]
mod tests;
