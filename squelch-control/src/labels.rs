//! Tenant labels: the subdomain a hosted daemon answers on, and therefore the
//! most load-bearing string a stranger gets to choose in this whole service.
//!
//! It becomes a DNS label, a URL path segment on the warden's API, and on the
//! far side the name of a Deployment, a Service, an Ingress host, a
//! NetworkPolicy, a PVC, and four Secrets. Every one of those has its own way of
//! being escaped out of, so validation is a strict ALLOWLIST applied once, here,
//! before the label reaches any of them: lowercase ASCII letters, digits, and
//! interior hyphens, and nothing else, ever. (That set is a subset of DNS-1123,
//! so a label that passes here is also a legal Kubernetes object name; this is
//! the stricter of the two gates and it is the outer one.)

use std::sync::LazyLock;

use regex::Regex;

/// Shortest and longest label. The floor is a product decision (a two-character
/// hosted subdomain is not something to hand out first-come) and the ceiling
/// keeps the whole `<label>.<base domain>` inside DNS's 63-byte label limit
/// with room to spare.
pub const MIN_LABEL_LEN: usize = 3;
pub const MAX_LABEL_LEN: usize = 30;

/// The contract's regex, verbatim. It pins the character set and the "no
/// leading or trailing hyphen" rule; the 3-character floor is enforced
/// separately below because this pattern also admits a single character.
static LABEL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-z0-9](?:[a-z0-9-]{1,28}[a-z0-9])?$").expect("label regex is a literal")
});

/// Labels nobody gets to claim.
///
/// Two different reasons, deliberately in one list: some are names this
/// deployment already serves or will serve (`signup`, `warden`, `auth`, `api`,
/// `mcp`, `relay`, `track`), and handing one out would let a tenant answer on a
/// hostname the product means something specific by. The rest are the names a
/// person assumes are ours (`www`, `mail`, `admin`, `status`) — a tenant
/// serving `mail.<base>` is a phishing surface wearing our domain.
pub const RESERVED: &[&str] = &[
    "www", "mail", "smtp", "imap", "auth", "api", "admin", "signup", "warden", "mcp", "status",
    "help", "docs", "app", "relay", "track",
];

/// Why a label was refused. The variants exist because these ARE safe to
/// distinguish: a label is public, chosen by the person reading the message,
/// and nothing about which rule it broke is a secret. (Invite codes are the
/// opposite, and [`crate::invites`] treats them that way.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelError {
    Empty,
    TooShort,
    TooLong,
    Charset,
    HyphenEdge,
    Reserved,
}

impl LabelError {
    /// The sentence a person reads on the signup page. No em dashes: this is
    /// user-facing copy.
    pub fn message(self) -> String {
        match self {
            LabelError::Empty => "Choose a username.".to_string(),
            LabelError::TooShort => {
                format!("That username is too short. Use at least {MIN_LABEL_LEN} characters.")
            }
            LabelError::TooLong => {
                format!("That username is too long. Use at most {MAX_LABEL_LEN} characters.")
            }
            LabelError::Charset => "Use lowercase letters, numbers, and hyphens only.".to_string(),
            LabelError::HyphenEdge => "A username cannot start or end with a hyphen.".to_string(),
            LabelError::Reserved => "That username is reserved. Pick another one.".to_string(),
        }
    }
}

/// Case-fold and trim the raw form field into the form everything downstream
/// uses. Separate from [`validate`] so that the value stored, the value sent to
/// the warden, and the value validated are provably the same string.
///
/// `to_lowercase` (not `to_ascii_lowercase`) so that a label typed in another
/// script folds by Unicode's rules and is then REFUSED by the ASCII allowlist,
/// rather than sneaking through some byte-level comparison unchanged.
pub fn normalize(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// Validate a NORMALIZED label. Returns the label itself on success so callers
/// cannot accidentally keep using the raw one.
pub fn validate(label: &str) -> Result<&str, LabelError> {
    if label.is_empty() {
        return Err(LabelError::Empty);
    }
    // Length in CHARACTERS, so a multi-byte label is not refused for the wrong
    // reason before the charset check gets to say what is actually wrong.
    let chars = label.chars().count();
    if chars < MIN_LABEL_LEN {
        return Err(LabelError::TooShort);
    }
    if chars > MAX_LABEL_LEN {
        return Err(LabelError::TooLong);
    }
    if !LABEL_RE.is_match(label) {
        // Distinguish the two failures the regex folds together, because
        // "no hyphen at the edge" is a rule people do not guess.
        return Err(if label.starts_with('-') || label.ends_with('-') {
            LabelError::HyphenEdge
        } else {
            LabelError::Charset
        });
    }
    if RESERVED.contains(&label) {
        return Err(LabelError::Reserved);
    }
    Ok(label)
}

/// Normalize and validate in one step, handing back an owned label.
pub fn parse(raw: &str) -> Result<String, LabelError> {
    let normalized = normalize(raw);
    validate(&normalized)?;
    Ok(normalized)
}

/// Usernames to suggest for `email`, best first: the part before the `@`, then
/// that part and the domain's first label joined by a hyphen (`ada@acme.com`
/// offers `ada`, then `ada-acme`). The caller takes the first one that is free.
///
/// Each candidate is folded into the label alphabet (a `+tag` dropped, every
/// other character outside it becoming a hyphen) and then has to pass
/// [`validate`] like anything typed, so a suggestion can never be a label the
/// form would refuse. Too short, reserved, or empty: left out, not padded.
pub fn suggestions(email: &str) -> Vec<String> {
    let Some((local, domain)) = email.trim().rsplit_once('@') else {
        return Vec::new();
    };
    let local = local.split('+').next().unwrap_or_default();
    // A letter outside ASCII would fold away and leave a mangled name behind
    // (`jürgen` to `j-rgen`): suggest nothing rather than that.
    if local.chars().any(|c| c.is_alphanumeric() && !c.is_ascii()) {
        return Vec::new();
    }
    let local = fold(local);
    let stem = fold(domain.split('.').next().unwrap_or_default());
    let mut out: Vec<String> = Vec::new();
    for candidate in [local.clone(), format!("{local}-{stem}")] {
        let candidate = clip(&candidate);
        if !local.is_empty() && validate(&candidate).is_ok() && !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

/// Lowercase, everything outside `[a-z0-9]` to one hyphen, no hyphen at an end.
fn fold(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Cut to [`MAX_LABEL_LEN`] without leaving a hyphen at the cut.
fn clip(label: &str) -> String {
    let cut: String = label.chars().take(MAX_LABEL_LEN).collect();
    cut.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_the_local_part_then_the_local_part_and_the_domain() {
        assert_eq!(suggestions("ada@acme.com"), ["ada", "ada-acme"]);
        assert_eq!(
            suggestions("Ada.Lovelace+news@gmail.com"),
            ["ada-lovelace", "ada-lovelace-gmail"]
        );
        // Too short alone, still offered with its domain.
        assert_eq!(suggestions("al@acme.com"), ["al-acme"]);
        // Reserved alone is the same.
        assert_eq!(suggestions("admin@acme.com"), ["admin-acme"]);
        // Long locals are clipped to a legal label, never past one.
        let long = suggestions("abcdefghijklmnopqrstuvwxyz-0123456789@acme.com");
        assert!(long.iter().all(|l| validate(l).is_ok()), "{long:?}");
        assert_eq!(long.len(), 1, "both clip to the same label: {long:?}");
        assert!(suggestions("not an address").is_empty());
        assert!(suggestions("+tag@acme.com").is_empty());
        assert!(suggestions("jürgen@acme.com").is_empty(), "no j-rgen");
    }

    #[test]
    fn accepts_ordinary_labels() {
        for good in [
            "ada",
            "ada-lovelace",
            "a1b",
            "000",
            "a-b-c",
            "abcdefghijklmnopqrstuvwxyz1234", // exactly 30
        ] {
            assert_eq!(parse(good).as_deref(), Ok(good), "{good}");
        }
    }

    #[test]
    fn folds_case_and_trims_before_validating() {
        assert_eq!(parse("  AdA  ").unwrap(), "ada");
        assert_eq!(parse("ADA-LOVELACE").unwrap(), "ada-lovelace");
        // A reserved name typed in caps is still reserved.
        assert_eq!(parse("WWW"), Err(LabelError::Reserved));
    }

    #[test]
    fn refuses_short_and_long() {
        assert_eq!(parse(""), Err(LabelError::Empty));
        assert_eq!(parse("   "), Err(LabelError::Empty));
        assert_eq!(parse("a"), Err(LabelError::TooShort));
        assert_eq!(parse("ab"), Err(LabelError::TooShort));
        assert_eq!(parse(&"a".repeat(31)), Err(LabelError::TooLong));
        assert!(parse(&"a".repeat(30)).is_ok());
    }

    #[test]
    fn refuses_hyphen_edges() {
        for bad in ["-ada", "ada-", "-ada-", "---"] {
            assert_eq!(parse(bad), Err(LabelError::HyphenEdge), "{bad}");
        }
    }

    /// Every one of these is a way out of a directory name, a unit name, a
    /// config file, or a hostname.
    #[test]
    fn refuses_everything_outside_the_allowlist() {
        for bad in [
            "ada_lovelace",
            "ada.lovelace",
            "ada lovelace",
            "ada/../root",
            "ada@host",
            "ada$(id)",
            "ada\nbcd",
            "ada%2e",
            "ada:1",
            "***",
        ] {
            assert_eq!(parse(bad), Err(LabelError::Charset), "{bad:?}");
        }
        // Length is checked before the character set, so a short one is refused
        // for being short. Both are refusals; only the sentence differs.
        assert_eq!(parse("*"), Err(LabelError::TooShort));
    }

    /// Unicode is refused rather than transliterated: a homograph of a reserved
    /// name is exactly the trick this allowlist exists to stop.
    #[test]
    fn refuses_unicode() {
        for bad in ["adá", "ａｄａ", "аdа", "ada\u{200b}", "İstanbul"] {
            assert!(matches!(parse(bad), Err(LabelError::Charset)), "{bad:?}");
        }
    }

    #[test]
    fn refuses_every_reserved_name() {
        for r in RESERVED {
            assert_eq!(parse(r), Err(LabelError::Reserved), "{r}");
        }
    }

    /// The messages are user-facing copy, and the house rule is no em dashes.
    #[test]
    fn messages_carry_no_em_dashes() {
        for e in [
            LabelError::Empty,
            LabelError::TooShort,
            LabelError::TooLong,
            LabelError::Charset,
            LabelError::HyphenEdge,
            LabelError::Reserved,
        ] {
            assert!(!e.message().contains('\u{2014}'), "{e:?}");
        }
    }
}
