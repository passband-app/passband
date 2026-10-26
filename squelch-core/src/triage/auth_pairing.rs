//! Explained alerts: a sign-in or account alert that landed beside the code,
//! link or Google sign-in receipt that accounts for it needs no question.
//!
//! Computed at read time over the Auth page's rows, never stored: a verdict,
//! a re-triage or a newly arrived code changes the answer on the next read.
//!
//! THE ASYMMETRY IS THE POINT. Only mail the owner asked for (a code, a link,
//! a reset, a verification) or a receipt of their own sign-in explains
//! anything. An alert never explains an alert: an attacker who signs in and
//! then changes the recovery phone produces two same-service alerts a minute
//! apart, and neither vouches for the other. A critical change is never
//! explained at all.
use super::decision::AuthKind;
use chrono::{DateTime, Utc};

/// How far apart an alert and its explanation may arrive.
pub const PAIRING_WINDOW_SECONDS: i64 = 5 * 60;

/// One auth message, as the pairing reads it.
#[derive(Debug, Clone)]
pub struct AuthMailRow {
    pub id: i64,
    pub from_addr: String,
    pub subject: String,
    pub received_at: DateTime<Utc>,
    pub kinds: Vec<AuthKind>,
    /// Already normalized: true only beside `account_change`.
    pub critical: bool,
}

/// Kinds that may be explained: the alerts.
fn is_alert(kind: AuthKind) -> bool {
    matches!(
        kind,
        AuthKind::LoginAlert
            | AuthKind::AccountChange
            | AuthKind::AppGrant
            | AuthKind::SecurityAlert
    )
}

/// Kinds that may explain: what the owner asked for, and receipts of what
/// they just did.
fn is_explainer(kind: AuthKind) -> bool {
    matches!(
        kind,
        AuthKind::Otp
            | AuthKind::SignInLink
            | AuthKind::PasswordReset
            | AuthKind::Verification
            | AuthKind::SigninReceipt
    )
}

/// For each row, in order, the id of the message that explains it, if any.
///
/// M is explained by E when M's kind (the priority pick) is an alert and M is
/// not critical; E's kind is an explainer (judged by E's own priority pick,
/// so a message that is mostly an alert cannot vouch through a secondary
/// kind); they arrived within [`PAIRING_WINDOW_SECONDS`]; and they are the
/// same service: the same registrable sender domain, or E is a sign-in
/// receipt whose subject names M's service. The nearest such E wins.
pub fn explained_by(rows: &[AuthMailRow]) -> Vec<Option<i64>> {
    let picked: Vec<Option<AuthKind>> = rows
        .iter()
        .map(|row| AuthKind::priority_pick(&row.kinds))
        .collect();
    let domains: Vec<Option<String>> = rows
        .iter()
        .map(|row| registrable_domain(&row.from_addr))
        .collect();
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|&i| (rows[i].received_at, rows[i].id));
    let window = chrono::Duration::seconds(PAIRING_WINDOW_SECONDS);
    let mut out = vec![None; rows.len()];
    for (position, &m) in order.iter().enumerate() {
        if rows[m].critical || !picked[m].is_some_and(is_alert) {
            continue;
        }
        let label = domains[m].as_deref().and_then(service_label);
        let at = rows[m].received_at;
        let mut best: Option<(chrono::Duration, i64)> = None;
        let mut consider = |e: usize| {
            if e == m || !picked[e].is_some_and(is_explainer) {
                return;
            }
            let same_domain = domains[m].is_some() && domains[m] == domains[e];
            let named = picked[e] == Some(AuthKind::SigninReceipt)
                && label.is_some_and(|label| names_word(&rows[e].subject, label));
            if !(same_domain || named) {
                return;
            }
            let gap = (rows[e].received_at - at).abs();
            let candidate = (gap, rows[e].id);
            if best.is_none_or(|current| candidate < current) {
                best = Some(candidate);
            }
        };
        for &e in order[..position].iter().rev() {
            if at - rows[e].received_at > window {
                break;
            }
            consider(e);
        }
        for &e in &order[position + 1..] {
            if rows[e].received_at - at > window {
                break;
            }
            consider(e);
        }
        out[m] = best.map(|(_, id)| id);
    }
    out
}

/// Relay services hand every sender the same domain, so a shared domain says
/// nothing about a shared service.
fn is_relay(domain: &str) -> bool {
    domain == "privaterelay.appleid.com" || domain.ends_with(".privaterelay.appleid.com")
}

/// Second-level labels that sit under a two-letter country code as a
/// registry, not a service (`co.uk`, `com.au`): the service is one label in.
const COUNTRY_REGISTRIES: &[&str] = &["co", "com", "net", "org", "gov", "ac", "edu", "ne", "or"];

/// The sender's registrable domain: the last two labels of the address's
/// domain, lowercased (three under a country registry such as `co.uk`).
/// `None` for relay addresses and anything without a dotted domain.
fn registrable_domain(from_addr: &str) -> Option<String> {
    let domain = from_addr
        .trim()
        .trim_end_matches('>')
        .rsplit_once('@')?
        .1
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if is_relay(&domain) {
        return None;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|label| label.is_empty()) {
        return None;
    }
    let take = if labels.len() >= 3
        && labels[labels.len() - 1].len() == 2
        && COUNTRY_REGISTRIES.contains(&labels[labels.len() - 2])
    {
        3
    } else {
        2
    };
    Some(labels[labels.len() - take..].join("."))
}

/// The service's name as a subject would say it: the registrable domain's
/// first label, when it is long enough not to match by accident.
fn service_label(registrable: &str) -> Option<&str> {
    let label = registrable.split('.').next()?;
    (label.chars().count() >= 3).then_some(label)
}

/// `word` appears in `text` as a whole word, ignoring ASCII case.
fn names_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || c == '-'))
        .any(|token| token.eq_ignore_ascii_case(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-20T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + chrono::Duration::seconds(seconds)
    }
    fn row(id: i64, from: &str, subject: &str, seconds: i64, kinds: &[AuthKind]) -> AuthMailRow {
        AuthMailRow {
            id,
            from_addr: from.into(),
            subject: subject.into(),
            received_at: at(seconds),
            kinds: kinds.to_vec(),
            critical: false,
        }
    }

    #[test]
    fn a_code_explains_the_sign_in_it_caused() {
        let rows = [
            row(
                1,
                "no-reply@accounts.example.com",
                "New sign-in",
                30,
                &[AuthKind::LoginAlert],
            ),
            row(2, "codes@example.com", "Your code", 0, &[AuthKind::Otp]),
        ];
        assert_eq!(explained_by(&rows), vec![Some(2), None]);
    }

    #[test]
    fn alerts_never_explain_alerts() {
        // The takeover shape: a sign-in, then the recovery phone moves.
        let mut change = row(
            1,
            "security@example.com",
            "Recovery phone changed",
            60,
            &[AuthKind::AccountChange],
        );
        change.critical = true;
        let sign_in = row(
            2,
            "security@example.com",
            "New sign-in",
            0,
            &[AuthKind::LoginAlert],
        );
        assert_eq!(explained_by(&[change, sign_in]), vec![None, None]);
        // Nor does one sign-in alert vouch for another.
        let rows = [
            row(
                1,
                "a@example.com",
                "New sign-in",
                0,
                &[AuthKind::LoginAlert],
            ),
            row(
                2,
                "b@example.com",
                "New sign-in",
                20,
                &[AuthKind::LoginAlert],
            ),
        ];
        assert_eq!(explained_by(&rows), vec![None, None]);
        // A non-critical change is no explainer either, but it can be explained.
        let rows = [
            row(
                1,
                "a@example.com",
                "SSH key added",
                0,
                &[AuthKind::AccountChange],
            ),
            row(
                2,
                "a@example.com",
                "New sign-in",
                20,
                &[AuthKind::LoginAlert],
            ),
            row(3, "a@example.com", "Your code", 40, &[AuthKind::Otp]),
        ];
        assert_eq!(explained_by(&rows), vec![Some(3), Some(3), None]);
    }

    #[test]
    fn critical_changes_are_never_explained() {
        let mut change = row(
            1,
            "a@example.com",
            "Password changed",
            10,
            &[AuthKind::AccountChange],
        );
        change.critical = true;
        let reset = row(
            2,
            "a@example.com",
            "Reset your password",
            0,
            &[AuthKind::PasswordReset],
        );
        assert_eq!(explained_by(&[change, reset]), vec![None, None]);
    }

    #[test]
    fn a_google_receipt_explains_the_service_it_names() {
        let alert = |seconds| {
            row(
                1,
                "hello@luma.com",
                "New sign-in to Luma",
                seconds,
                &[AuthKind::LoginAlert],
            )
        };
        let receipt = row(
            2,
            "no-reply@accounts.google.com",
            "You shared some Google Account data with Luma",
            0,
            &[AuthKind::SigninReceipt],
        );
        assert_eq!(
            explained_by(&[alert(4 * 60), receipt.clone()]),
            vec![Some(2), None]
        );
        assert_eq!(
            explained_by(&[alert(5 * 60), receipt.clone()]),
            vec![Some(2), None]
        );
        assert_eq!(
            explained_by(&[alert(6 * 60), receipt.clone()]),
            vec![None, None]
        );
        assert_eq!(explained_by(&[alert(-6 * 60), receipt]), vec![None, None]);
    }

    #[test]
    fn a_receipt_names_a_service_only_as_a_whole_word() {
        let alert = row(1, "team@lu.ma", "New sign-in", 30, &[AuthKind::LoginAlert]);
        let receipt = row(
            2,
            "no-reply@accounts.google.com",
            "You shared some Google Account data with Luma",
            0,
            &[AuthKind::SigninReceipt],
        );
        // `lu` is under three characters, and `luma` is not the word `lu`.
        assert_eq!(explained_by(&[alert, receipt.clone()]), vec![None, None]);
        let alert = row(1, "hi@uber.com", "New sign-in", 30, &[AuthKind::LoginAlert]);
        let receipt = AuthMailRow {
            subject: "You shared some Google Account data with Ubereats".into(),
            ..receipt
        };
        assert_eq!(explained_by(&[alert, receipt]), vec![None, None]);
    }

    #[test]
    fn relay_senders_never_match_by_domain() {
        let rows = [
            row(
                1,
                "noreply_at_luma_com_abc@privaterelay.appleid.com",
                "New sign-in",
                30,
                &[AuthKind::LoginAlert],
            ),
            row(
                2,
                "codes_at_other_com_xyz@privaterelay.appleid.com",
                "Your code",
                0,
                &[AuthKind::Otp],
            ),
        ];
        assert_eq!(explained_by(&rows), vec![None, None]);
    }

    #[test]
    fn the_registrable_domain_is_the_last_two_labels() {
        assert_eq!(
            registrable_domain("no-reply@accounts.google.com").as_deref(),
            Some("google.com")
        );
        assert_eq!(
            registrable_domain("A@Mail.Example.COM").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            registrable_domain("alerts@secure.bank.co.uk").as_deref(),
            Some("bank.co.uk")
        );
        assert_eq!(registrable_domain("localhost"), None);
        assert_eq!(registrable_domain("x@privaterelay.appleid.com"), None);
    }

    #[test]
    fn the_nearest_explanation_wins_and_the_kind_is_the_priority_pick() {
        let rows = [
            row(
                1,
                "a@example.com",
                "New sign-in",
                100,
                &[AuthKind::LoginAlert],
            ),
            row(2, "a@example.com", "Your code", 0, &[AuthKind::Otp]),
            row(
                3,
                "a@example.com",
                "Your link",
                130,
                &[AuthKind::SignInLink],
            ),
            // Mostly an alert: its secondary receipt kind does not vouch.
            row(
                4,
                "a@example.com",
                "New sign-in",
                101,
                &[AuthKind::LoginAlert, AuthKind::SigninReceipt],
            ),
            // A code that also mentions a sign-in is a code: not explained.
            row(
                5,
                "a@example.com",
                "Code",
                102,
                &[AuthKind::LoginAlert, AuthKind::Otp],
            ),
        ];
        assert_eq!(
            explained_by(&rows),
            vec![Some(5), None, None, Some(5), None]
        );
    }
}
