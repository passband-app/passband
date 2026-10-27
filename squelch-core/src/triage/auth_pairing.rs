//! Explained alerts: a sign-in alert that landed beside the code the owner
//! used, or the identity provider's receipt of the sign-in they just made,
//! needs no question.
//!
//! Computed at read time over the Auth page's rows, never stored: a verdict,
//! a reveal, a re-triage or a newly arrived code changes the answer on the
//! next read.
//!
//! NARROW ON PURPOSE (security review, 2026-10-26). An attacker can make a
//! service send almost anything: a reset, a signup verification, an emailed
//! code, and a consent screen produces both a grant and a receipt. So:
//! - only a NON-critical `login_alert` is ever explained. An account change,
//!   an app grant or a security alert always keeps its question;
//! - a code or sign-in link explains only once the OWNER revealed it (an
//!   audited reveal row exists), which an attacker cannot do for them, and
//!   only a reveal around the sign-in (no later than a minute after the
//!   alert): opening an unsolicited code later, to see what it was, is not
//!   using it;
//! - a receipt explains only when it is Google's own sign-in receipt (exact
//!   sender address and subject template) and the app it names is the
//!   alert's service. Google also sends mail other people write (Docs
//!   shares, Calendar, Forms) from its domain, so the domain proves nothing;
//! - the explainer passed sender authentication, arrived before the alert
//!   (or at most a minute after), and was not ruled "Not me";
//! - an alert the owner already ruled on, either way, is never explained.
//!
//! Alerts never explain alerts. Critical changes are never explained.
//!
//! RESIDUAL, accepted: an attacker-triggered code the owner opens just
//! before the attacker's sign-in still explains it.
use super::decision::AuthKind;
use chrono::{DateTime, Duration, Utc};

/// How long before an alert its explanation may have arrived.
pub const EXPLAINED_BEFORE_SECONDS: i64 = 5 * 60;
/// How long after an alert its explanation may arrive: a code is asked for
/// before the sign-in it causes, but delivery order is not exact.
pub const EXPLAINED_AFTER_SECONDS: i64 = 60;

/// The one sender whose sign-in receipts may explain an alert. Apple and
/// Microsoft receipts are not used: none were in the survey, and their
/// templates are unverified.
pub const GOOGLE_RECEIPT_SENDER: &str = "noreply-accounts@google.com";
/// Google's receipt subject, up to the app's name.
pub const GOOGLE_RECEIPT_SUBJECT: &str = "You shared some Google Account data with ";

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
    /// `messages.auth_pass = 1`: DMARC or an aligned DKIM signature held.
    pub auth_pass: bool,
    /// When the owner first revealed it through the audited reveal door.
    pub revealed_at: Option<DateTime<Utc>>,
    /// The owner ruled it "Not me".
    pub not_mine: bool,
    /// The owner ruled it either way.
    pub ruled: bool,
}

impl AuthMailRow {
    /// The one kind the pairing judges a message by: the priority pick, so a
    /// message that lists a code kind beside an alert is a code, and one that
    /// lists a receipt beside an alert is an alert.
    fn kind(&self) -> Option<AuthKind> {
        AuthKind::priority_pick(&self.kinds)
    }

    /// May this message be explained at all?
    pub fn explainable(&self) -> bool {
        !self.critical && !self.ruled && self.kind() == Some(AuthKind::LoginAlert)
    }
}

/// The span of arrival times an explanation for an alert at `at` may have.
pub fn explainer_window(at: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    (
        at - Duration::seconds(EXPLAINED_BEFORE_SECONDS),
        at + Duration::seconds(EXPLAINED_AFTER_SECONDS),
    )
}

/// For each row, in order, the id of the message that explains it, if any.
/// The nearest qualifying explanation wins (ties to the lower id).
pub fn explained_by(rows: &[AuthMailRow]) -> Vec<Option<i64>> {
    let services: Vec<Option<String>> = rows.iter().map(|row| service(&row.from_addr)).collect();
    rows.iter()
        .enumerate()
        .map(|(m, alert)| {
            if !alert.explainable() {
                return None;
            }
            let (from, to) = explainer_window(alert.received_at);
            rows.iter()
                .enumerate()
                .filter(|&(e, explainer)| {
                    e != m
                        && explainer.auth_pass
                        && !explainer.not_mine
                        && (from..=to).contains(&explainer.received_at)
                        && vouches(explainer, &services[e], alert, &services[m])
                })
                .map(|(_, e)| ((e.received_at - alert.received_at).abs(), e.id))
                .min()
                .map(|(_, id)| id)
        })
        .collect()
}

/// Does `e` vouch for alert `m` (from `m_service`), by kind and service?
fn vouches(
    e: &AuthMailRow,
    e_service: &Option<String>,
    m: &AuthMailRow,
    m_service: &Option<String>,
) -> bool {
    match e.kind() {
        // A code or link the owner used, around the sign-in, from the
        // alert's own service.
        Some(AuthKind::Otp | AuthKind::SignInLink) => {
            e.revealed_at.is_some_and(|revealed| {
                revealed <= m.received_at + Duration::seconds(EXPLAINED_AFTER_SECONDS)
            }) && m_service.is_some()
                && e_service == m_service
        }
        // Google's receipt naming the alert's service as the app, never
        // Google itself: a receipt of signing in somewhere with Google says
        // nothing about a new sign-in to Google.
        Some(AuthKind::SigninReceipt) => {
            let (Some(app), Some(service)) = (google_receipt_app(e), m_service) else {
                return false;
            };
            e_service.as_deref() != Some(service.as_str())
                && service_label(service).is_some_and(|label| app_names(app, label))
        }
        _ => false,
    }
}

/// The app a genuine Google sign-in receipt names: sender exactly
/// [`GOOGLE_RECEIPT_SENDER`] (the parsed address, any case), subject exactly
/// [`GOOGLE_RECEIPT_SUBJECT`] then the app, surrounding whitespace aside.
fn google_receipt_app(e: &AuthMailRow) -> Option<&str> {
    let addr = e.from_addr.trim();
    let addr = match (addr.rfind('<'), addr.rfind('>')) {
        (Some(open), Some(close)) if open < close => &addr[open + 1..close],
        _ => addr,
    };
    if !addr.trim().eq_ignore_ascii_case(GOOGLE_RECEIPT_SENDER) {
        return None;
    }
    let app = e
        .subject
        .trim()
        .strip_prefix(GOOGLE_RECEIPT_SUBJECT)?
        .trim();
    (!app.is_empty()).then_some(app)
}

/// The receipt's app is the service with this label: the app's name with
/// everything but letters and digits removed equals it, or one whole word of
/// the name does (ASCII case ignored either way).
fn app_names(app: &str, label: &str) -> bool {
    let squashed: String = app.chars().filter(|c| c.is_alphanumeric()).collect();
    squashed.eq_ignore_ascii_case(label) || names_word(app, label)
}

/// The sender's service: its registrable domain under the full Public
/// Suffix List (so each `*.firebaseapp.com` or `*.myshopify.com` app is its
/// own service), `None` for relays, freemail and shared tenant hosts, where
/// the domain says nothing about who sent the mail. The same rule the
/// calendar's sender scope uses.
fn service(from_addr: &str) -> Option<String> {
    super::ghost::organisation_domain(from_addr.trim().trim_end_matches('>'))
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
            + Duration::seconds(seconds)
    }
    /// An authenticated, unruled, unrevealed message.
    fn row(id: i64, from: &str, subject: &str, seconds: i64, kinds: &[AuthKind]) -> AuthMailRow {
        AuthMailRow {
            id,
            from_addr: from.into(),
            subject: subject.into(),
            received_at: at(seconds),
            kinds: kinds.to_vec(),
            critical: false,
            auth_pass: true,
            revealed_at: None,
            not_mine: false,
            ruled: false,
        }
    }
    fn alert(id: i64, from: &str, seconds: i64) -> AuthMailRow {
        row(id, from, "New sign-in", seconds, &[AuthKind::LoginAlert])
    }
    /// A code the owner revealed as it arrived.
    fn used_code(id: i64, from: &str, seconds: i64) -> AuthMailRow {
        AuthMailRow {
            revealed_at: Some(at(seconds + 5)),
            ..row(id, from, "Your code", seconds, &[AuthKind::Otp])
        }
    }
    fn google_receipt(id: i64, app: &str, seconds: i64) -> AuthMailRow {
        row(
            id,
            GOOGLE_RECEIPT_SENDER,
            &format!("You shared some Google Account data with {app}"),
            seconds,
            &[AuthKind::SigninReceipt],
        )
    }
    fn explains(rows: &[AuthMailRow]) -> Option<i64> {
        explained_by(rows)[0]
    }

    #[test]
    fn a_code_the_owner_used_explains_the_sign_in_it_caused() {
        let rows = [
            alert(1, "no-reply@accounts.example.com", 30),
            used_code(2, "codes@example.com", 0),
        ];
        assert_eq!(explained_by(&rows), vec![Some(2), None]);
        // A sign-in link the owner opened counts the same way.
        let link = AuthMailRow {
            kinds: vec![AuthKind::SignInLink],
            ..used_code(2, "codes@example.com", 0)
        };
        assert_eq!(explains(&[alert(1, "a@example.com", 30), link]), Some(2));
    }

    #[test]
    fn an_unrevealed_code_explains_nothing() {
        // Anyone who knows the address can make a service email a code.
        let code = row(2, "codes@example.com", "Your code", 0, &[AuthKind::Otp]);
        assert_eq!(explains(&[alert(1, "a@example.com", 30), code]), None);
    }

    /// The owner opening an unsolicited code AFTER the attacker's sign-in,
    /// to see what it was, is not using it.
    #[test]
    fn a_reveal_counts_only_around_the_sign_in() {
        let revealed = |seconds| AuthMailRow {
            revealed_at: Some(at(seconds)),
            ..row(2, "codes@example.com", "Your code", 0, &[AuthKind::Otp])
        };
        let alert = || alert(1, "a@example.com", 30);
        assert_eq!(explains(&[alert(), revealed(30 + 60)]), Some(2));
        assert_eq!(explains(&[alert(), revealed(30 + 61)]), None);
        assert_eq!(explains(&[alert(), revealed(30 + 10 * 60)]), None);
        // Revealed before the sign-in: the accepted residual.
        assert_eq!(explains(&[alert(), revealed(10)]), Some(2));
    }

    #[test]
    fn a_reset_or_verification_explains_nothing_even_when_revealed() {
        // The attacker submits the reset form; the real reset arrives.
        for kind in [AuthKind::PasswordReset, AuthKind::Verification] {
            let triggered = AuthMailRow {
                kinds: vec![kind],
                ..used_code(2, "noreply@github.com", 0)
            };
            assert_eq!(
                explains(&[alert(1, "noreply@github.com", 30), triggered]),
                None
            );
        }
    }

    #[test]
    fn only_an_unruled_non_critical_sign_in_alert_is_ever_explained() {
        for kind in [
            AuthKind::AccountChange,
            AuthKind::AppGrant,
            AuthKind::SecurityAlert,
        ] {
            let change = row(1, "noreply@github.com", "SSH key added", 30, &[kind]);
            assert_eq!(
                explains(&[change, used_code(2, "noreply@github.com", 0)]),
                None
            );
        }
        let critical = AuthMailRow {
            critical: true,
            ..alert(1, "noreply@github.com", 30)
        };
        assert_eq!(
            explains(&[critical, used_code(2, "noreply@github.com", 0)]),
            None
        );
        // Ruled either way, the owner has answered: no "explained" note.
        for not_mine in [false, true] {
            let ruled = AuthMailRow {
                ruled: true,
                not_mine,
                ..alert(1, "noreply@github.com", 30)
            };
            assert_eq!(
                explains(&[ruled, used_code(2, "noreply@github.com", 0)]),
                None
            );
        }
    }

    #[test]
    fn alerts_never_explain_alerts() {
        // The takeover shape: a sign-in, then the recovery phone moves.
        let change = AuthMailRow {
            critical: true,
            ..row(
                1,
                "security@example.com",
                "Recovery phone changed",
                60,
                &[AuthKind::AccountChange],
            )
        };
        let rows = [change, alert(2, "security@example.com", 0)];
        assert_eq!(explained_by(&rows), vec![None, None]);
        let rows = [alert(1, "a@example.com", 0), alert(2, "b@example.com", 20)];
        assert_eq!(explained_by(&rows), vec![None, None]);
        // An alert that also lists a receipt kind is judged by its priority
        // pick: it is an alert, revealed or not.
        let receipt_ish = AuthMailRow {
            revealed_at: Some(at(10)),
            ..row(
                2,
                "a@example.com",
                "x",
                10,
                &[AuthKind::LoginAlert, AuthKind::SigninReceipt],
            )
        };
        assert_eq!(
            explains(&[alert(1, "a@example.com", 30), receipt_ish]),
            None
        );
    }

    #[test]
    fn the_explainer_must_have_passed_sender_authentication() {
        let spoofed = AuthMailRow {
            auth_pass: false,
            ..used_code(2, "codes@example.com", 0)
        };
        assert_eq!(explains(&[alert(1, "a@example.com", 30), spoofed]), None);
        let forged_receipt = AuthMailRow {
            auth_pass: false,
            ..google_receipt(2, "Luma", 0)
        };
        assert_eq!(
            explains(&[alert(1, "hello@luma.com", 30), forged_receipt]),
            None
        );
    }

    #[test]
    fn a_not_mine_explainer_explains_nothing() {
        let disowned = AuthMailRow {
            not_mine: true,
            ruled: true,
            ..used_code(2, "codes@example.com", 0)
        };
        assert_eq!(explains(&[alert(1, "a@example.com", 30), disowned]), None);
    }

    #[test]
    fn the_explainer_comes_first_or_within_a_minute_after() {
        let code = |seconds| used_code(2, "codes@example.com", seconds);
        let alert = || alert(1, "a@example.com", 0);
        assert_eq!(explains(&[alert(), code(-5 * 60)]), Some(2));
        assert_eq!(explains(&[alert(), code(-5 * 60 - 1)]), None);
        assert_eq!(explains(&[alert(), code(55)]), Some(2));
        assert_eq!(explains(&[alert(), code(61)]), None);
        assert_eq!(explains(&[alert(), code(4 * 60)]), None);
    }

    #[test]
    fn googles_real_receipt_explains_the_app_it_names() {
        let luma = |seconds| alert(1, "hello@luma.com", seconds);
        assert_eq!(
            explains(&[luma(4 * 60), google_receipt(2, "Luma", 0)]),
            Some(2)
        );
        assert_eq!(
            explains(&[luma(5 * 60), google_receipt(2, "Luma", 0)]),
            Some(2)
        );
        assert_eq!(
            explains(&[luma(6 * 60), google_receipt(2, "Luma", 0)]),
            None
        );
        // Any case of the address, a display-name form, padding around the
        // subject, and an app named in several words.
        let padded = AuthMailRow {
            from_addr: "Google <NoReply-Accounts@Google.com>".into(),
            subject: "  You shared some Google Account data with Luma Events \n".into(),
            ..google_receipt(2, "", 0)
        };
        assert_eq!(explains(&[luma(30), padded]), Some(2));
        // Receipts need no reveal: they are not credentials.
        assert!(google_receipt(2, "Luma", 0).revealed_at.is_none());
    }

    /// Google sends mail other people write from its own domain. A Docs
    /// share titled like a receipt is still a Docs share.
    #[test]
    fn a_docs_share_with_the_receipt_subject_explains_nothing() {
        let share = AuthMailRow {
            from_addr: "drive-shares-dm-noreply@google.com".into(),
            ..google_receipt(2, "Chase", 0)
        };
        assert_eq!(explains(&[alert(1, "alerts@chase.com", 30), share]), None);
        for from in [
            "no-reply@accounts.google.com",
            "calendar-notification@google.com",
            "no-reply@accounts-google.example",
            "appleid@id.apple.com",
        ] {
            let other = AuthMailRow {
                from_addr: from.into(),
                ..google_receipt(2, "Chase", 0)
            };
            assert_eq!(
                explains(&[alert(1, "alerts@chase.com", 30), other]),
                None,
                "{from}"
            );
        }
        // Nor does the alert's own service vouch for itself with a receipt.
        let own = row(
            2,
            "hello@luma.com",
            "You shared some Google Account data with Luma",
            0,
            &[AuthKind::SigninReceipt],
        );
        assert_eq!(explains(&[alert(1, "hello@luma.com", 30), own]), None);
    }

    #[test]
    fn only_the_app_name_counts_never_the_template() {
        // `you.com` is a word of every receipt subject, never its app.
        assert_eq!(
            explains(&[alert(1, "hi@you.com", 30), google_receipt(2, "Claude", 0)]),
            None
        );
        for template_word in ["shared", "google", "account", "data"] {
            let from = format!("hi@{template_word}.com");
            assert_eq!(
                explains(&[alert(1, &from, 30), google_receipt(2, "Claude", 0)]),
                None,
                "{template_word}"
            );
        }
        // A subject off the template names nothing.
        let off = AuthMailRow {
            subject: "Luma: You shared some Google Account data with Luma".into(),
            ..google_receipt(2, "", 0)
        };
        assert_eq!(explains(&[alert(1, "hello@luma.com", 30), off]), None);
    }

    #[test]
    fn a_providers_receipt_never_explains_the_providers_own_alert() {
        let google_alert = row(
            1,
            "no-reply@accounts.google.com",
            "Security alert: new sign-in on Windows",
            -60,
            &[AuthKind::LoginAlert],
        );
        assert_eq!(
            explains(&[google_alert.clone(), google_receipt(2, "Shady App", -90)]),
            None
        );
        assert_eq!(
            explains(&[google_alert, google_receipt(2, "Google", -90)]),
            None
        );
    }

    #[test]
    fn a_consent_screen_grant_is_not_explained_by_its_own_receipt() {
        let grant = row(
            1,
            "no-reply@accounts.google.com",
            "Luma was granted access to your Google Account",
            0,
            &[AuthKind::AppGrant],
        );
        assert_eq!(explains(&[grant, google_receipt(2, "Luma", 0)]), None);
    }

    #[test]
    fn a_receipt_names_a_service_only_as_a_whole_word() {
        // `lu` is under three characters.
        assert_eq!(
            explains(&[alert(1, "team@lu.ma", 30), google_receipt(2, "Luma", 0)]),
            None
        );
        assert_eq!(
            explains(&[
                alert(1, "hi@uber.com", 30),
                google_receipt(2, "Ubereats", 0)
            ]),
            None
        );
        // The app's name squashed: "Lu Ma" is `luma`.
        assert_eq!(
            explains(&[alert(1, "hi@luma.com", 30), google_receipt(2, "Lu Ma", 0)]),
            Some(2)
        );
    }

    #[test]
    fn shared_hosts_relays_and_freemail_are_never_one_service() {
        // Two Firebase apps, two Shopify stores: separate services.
        for (a, b) in [
            (
                "noreply@app-a.firebaseapp.com",
                "noreply@app-b.firebaseapp.com",
            ),
            ("shop@store-a.myshopify.com", "shop@store-b.myshopify.com"),
            (
                "noreply_at_luma_com_abc@privaterelay.appleid.com",
                "x_at_other_com@privaterelay.appleid.com",
            ),
            ("someone@gmail.com", "else@gmail.com"),
        ] {
            assert_eq!(
                explains(&[alert(1, a, 30), used_code(2, b, 0)]),
                None,
                "{a} / {b}"
            );
        }
        // One Firebase app is one service.
        assert_eq!(
            explains(&[
                alert(1, "noreply@app-a.firebaseapp.com", 30),
                used_code(2, "codes@app-a.firebaseapp.com", 0)
            ]),
            Some(2)
        );
    }

    #[test]
    fn the_service_is_the_public_suffix_list_registrable_domain() {
        assert_eq!(
            service("no-reply@accounts.google.com").as_deref(),
            Some("google.com")
        );
        assert_eq!(
            service("A@Mail.Example.COM").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            service("alerts@secure.bank.co.uk").as_deref(),
            Some("bank.co.uk")
        );
        assert_eq!(
            service("n@app-a.firebaseapp.com").as_deref(),
            Some("app-a.firebaseapp.com")
        );
        assert_eq!(
            service("n@store-a.myshopify.com").as_deref(),
            Some("store-a.myshopify.com")
        );
        assert_eq!(service("localhost"), None);
        assert_eq!(service("x@privaterelay.appleid.com"), None);
    }

    #[test]
    fn the_nearest_explanation_wins() {
        let rows = [
            alert(1, "a@example.com", 100),
            used_code(2, "a@example.com", 0),
            used_code(3, "a@example.com", 90),
        ];
        assert_eq!(explained_by(&rows), vec![Some(3), None, None]);
    }
}
