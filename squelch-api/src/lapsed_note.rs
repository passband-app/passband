//! The lapsed-user note: ONCE EVER, for somebody who paired a Passband app
//! and then stopped using it, one message filed into their own inbox listing
//! the receipts and order and shipping mail from the past week that Passband
//! already handled for them. "You shouldn't have had to open these." The
//! point is to show the chore Passband takes away, in the client they went
//! back to.
//!
//! FILED, NEVER SENT. The daemon writes the message into the owner's own
//! mailbox with Gmail's `messages.insert` (labels INBOX and UNREAD,
//! [`crate::write::MailWriter::insert_into_inbox`]). Nothing goes over SMTP,
//! nothing has a recipient but the mailbox it is filed in, and no mail leaves
//! the tenant. Passband is never a mail server.
//!
//! WHO. Only a lapsed owner ([`is_lapsed`]): a client was paired at least
//! once, and no client has been present for [`IDLE_DAYS`]. Present means the
//! app was OPEN, not only that it acted: an owner who opens the app every day
//! and never taps anything is reading it, and a note telling them what they
//! missed would be wrong. A mailbox that never paired a client is never
//! written to.
//!
//! WHAT. Facts triage already stored ([`SqliteStore::lapsed_note_candidates`]),
//! never a model call. The words are a fixed template ([`compose`]); the list
//! is sender, kind, date and subject. Fewer than [`MIN_ITEMS`] and nothing is
//! filed and nothing is stamped, so a later check (while the owner is still
//! away) can try again.
//!
//! ONCE EVER, AT MOST ONCE. A stamp in `app_settings` ([`STAMP_KEY`]) is
//! written BEFORE the insert, so a crash between the two costs the note,
//! never a second copy. It is lifted again only when the request provably
//! never reached Google (no credential, or the mailbox's Gmail budget held
//! it); any other failure keeps it.
//!
//! WHICH MAILBOXES. Gmail only, for now. Graph can make a non-draft message
//! only through an undocumented extended property (`PR_MESSAGE_FLAGS`), and
//! IMAP has no insert of the same shape; both are skipped, silently, every
//! check. A broken or missing write credential is skipped the same way.
//!
//! IT NEVER SURFACES IN PASSBAND. The note is From "Passband" at the owner's
//! OWN address, and a filed message crosses no inbound edge, so it carries no
//! `Authentication-Results` and reads as the owner's own mail by authorship
//! (`own_mail_sql!`). Own mail is never triaged (its job is an access check),
//! never placed in For your eyes or Reading, never pushed, and never listed
//! by a later note. `X-Passband-Note` and `Auto-Submitted` name it for any
//! other reader.
//!
//! WHEN. Checked from the morning brief's worker ([`LapsedCheck`]), at most
//! every [`CHECK_EVERY`], and only in the owner's daytime when their zone is
//! known, so the note is new when they look rather than buried overnight.
//!
//! LOGS. Outcomes and a count, never a sender, a subject or an address.

use std::time::Duration;

use chrono::{DateTime, Datelike, Timelike, Utc};
use chrono_tz::Tz;
use squelch_core::mail::MailProvider;
use squelch_core::owner_clock::OwnerZoneSource;
use squelch_core::store::sqlite::lapsed_note::NoteCandidate;
use squelch_core::store::{ClientActivity, SqliteStore, Store};

use crate::error::ApiError;
use crate::handlers::{account_provider, store_call, write_client};
use crate::state::ApiState;
use crate::write::WriteError;
use crate::write::mime::{alternative_section, escape_html, multipart_boundary, qp_encode};

/// The once-ever stamp, in `app_settings`: when the note was attempted.
pub const STAMP_KEY: &str = "lapsed_note";
/// How long no client may have been present before the owner counts as gone.
pub const IDLE_DAYS: i64 = 10;
/// How far back the note looks for handled mail.
pub const WINDOW_DAYS: i64 = 7;
/// Fewer qualifying messages than this and there is no note.
pub const MIN_ITEMS: usize = 3;
/// The most messages the note lists by name.
pub const LIST_CAP: usize = 8;
/// The least time between two checks.
pub const CHECK_EVERY: Duration = Duration::from_secs(4 * 60 * 60);
/// The owner-local hours a note may be filed in, when their zone is known.
pub const LOCAL_HOURS: std::ops::Range<u32> = 9..18;

/// The header that names the note for any reader.
pub const NOTE_HEADER: &str = "X-Passband-Note";
pub const NOTE_HEADER_VALUE: &str = "lapsed-digest";

/// The note's subject.
pub const SUBJECT: &str = "You shouldn't have had to open these";
/// The one line above the list.
pub const LEAD: &str =
    "Passband already filed these for you this week, so there was nothing to open or mark read.";
/// The one link. No `passband://` link merely opens the app (the scheme's
/// actions are pairing and drafts), and webmail drops custom schemes anyway.
pub const LINK: &str = "https://passband.app/download";
/// The words before the link.
pub const LINK_LEAD: &str = "Pick up where you left off:";

/// Longest a sender or subject runs in the list before it is cut.
const SENDER_CAP: usize = 40;
const SUBJECT_CAP: usize = 90;

/// The newest moment any client was present: a pairing, an app open
/// (`last_seen_at`), an action (`last_action_at`), or a use by a client that
/// never names itself (`unnamed_used_at`, see
/// [`SqliteStore::unnamed_client_last_used_at`]).
pub fn last_presence(
    activity: &ClientActivity,
    unnamed_used_at: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    let platforms = activity.platforms.values().flat_map(|p| {
        [Some(p.first_paired_at), p.last_seen_at, p.last_action_at]
            .into_iter()
            .flatten()
    });
    activity
        .first_paired_at
        .into_iter()
        .chain(unnamed_used_at)
        .chain(platforms)
        .max()
}

/// THE DEAD RULE. True when a client has been paired at least once AND the
/// newest presence ([`last_presence`]) is at least [`IDLE_DAYS`] before
/// `now`. Never paired is never lapsed.
pub fn is_lapsed(
    activity: &ClientActivity,
    unnamed_used_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    if activity.first_paired_at.is_none() {
        return false;
    }
    last_presence(activity, unnamed_used_at)
        .is_some_and(|last| now - last >= chrono::Duration::days(IDLE_DAYS))
}

/// Whether `now` is in the owner's daytime. An unknown zone is any time: the
/// note waits for nobody's clock it cannot read.
pub fn in_local_hours(now: DateTime<Utc>, zone: Tz, known: bool) -> bool {
    !known || LOCAL_HOURS.contains(&now.with_timezone(&zone).hour())
}

/// One listed message, as the note prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteLine {
    pub kind: &'static str,
    pub sender: String,
    pub subject: String,
    /// `Nov 12`, in the owner's zone.
    pub date: String,
}

impl NoteLine {
    pub fn from_candidate(c: &NoteCandidate, zone: Tz) -> Self {
        let name = c
            .from_name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or(c.from_addr.as_str());
        let local = c.received_at.with_timezone(&zone);
        Self {
            kind: c.kind.label(),
            sender: clip(name, SENDER_CAP),
            subject: clip(&c.subject, SUBJECT_CAP),
            date: format!("{} {}", month(local.month()), local.day()),
        }
    }
}

fn month(m: u32) -> &'static str {
    [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][(m.clamp(1, 12) - 1) as usize]
}

/// One line of untrusted text (a sender's name, a subject), control
/// characters dropped, folded to single spaces, cut at `cap` characters with
/// an ellipsis.
fn clip(raw: &str, cap: usize) -> String {
    let folded = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if folded.chars().count() <= cap {
        return folded;
    }
    let cut: String = folded.chars().take(cap - 1).collect();
    format!("{}…", cut.trim_end())
}

/// The note's words, both alternatives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub subject: &'static str,
    pub text: String,
    pub html: String,
}

/// THE TEMPLATE. `lines` is every qualifying message, newest first; the
/// first [`LIST_CAP`] are listed and the rest counted. None below
/// [`MIN_ITEMS`]. Fixed copy and facts only: no sentence here is written by
/// anything but this function.
pub fn compose(lines: &[NoteLine]) -> Option<Note> {
    if lines.len() < MIN_ITEMS {
        return None;
    }
    let shown = &lines[..lines.len().min(LIST_CAP)];
    let more = lines.len() - shown.len();
    let more_line = (more > 0).then(|| format!("And {more} more."));

    let mut text = String::new();
    text.push_str(LEAD);
    text.push_str("\n\n");
    for l in shown {
        text.push_str(&format!(
            "{} from {}, {}\n  {}\n",
            l.kind, l.sender, l.date, l.subject
        ));
    }
    if let Some(m) = &more_line {
        text.push_str(&format!("\n{m}\n"));
    }
    text.push_str(&format!("\n{LINK_LEAD} {LINK}\n"));

    let mut html = String::new();
    html.push_str(
        "<!doctype html><html><body style=\"font-family:-apple-system,Helvetica,Arial,sans-serif;\
         font-size:15px;line-height:1.5;color:#1d1d1f\">\n",
    );
    html.push_str(&format!(
        "<p>{}</p>\n<ul style=\"padding-left:20px\">\n",
        escape_html(LEAD)
    ));
    for l in shown {
        html.push_str(&format!(
            "<li style=\"margin-bottom:8px\"><strong>{}</strong> from {}, {}<br>{}</li>\n",
            escape_html(l.kind),
            escape_html(&l.sender),
            escape_html(&l.date),
            escape_html(&l.subject)
        ));
    }
    html.push_str("</ul>\n");
    if let Some(m) = &more_line {
        html.push_str(&format!("<p>{}</p>\n", escape_html(m)));
    }
    html.push_str(&format!(
        "<p>{} <a href=\"{LINK}\">{LINK}</a></p>\n</body></html>\n",
        escape_html(LINK_LEAD)
    ));

    Some(Note {
        subject: SUBJECT,
        text,
        html,
    })
}

/// The note as RFC 822 bytes, From "Passband" at `mailbox` (the owner's own
/// address, see the module docs) and To the same, multipart/alternative with
/// a text/plain part first. Refused when `mailbox` could break a header.
pub fn build_rfc822(
    note: &Note,
    mailbox: &str,
    message_id: &str,
    date: DateTime<Utc>,
) -> Result<Vec<u8>, WriteError> {
    let addr = mailbox.trim();
    let header_safe = |s: &str| {
        !s.is_empty()
            && s.is_ascii()
            && !s
                .chars()
                .any(|c| c.is_control() || matches!(c, '<' | '>' | ',' | '"' | ' '))
    };
    if !header_safe(addr) || !addr.contains('@') {
        return Err(WriteError::Invalid("mailbox address unusable".into()));
    }
    if !message_id.is_ascii() || message_id.chars().any(|c| c.is_control()) {
        return Err(WriteError::Invalid("message id unusable".into()));
    }
    let text = qp_encode(&note.text.replace('\n', "\r\n"));
    let html = qp_encode(&note.html.replace('\n', "\r\n"));
    let boundary = multipart_boundary(&[&text, &html]);
    let mut out = String::new();
    out.push_str(&format!("From: Passband <{addr}>\r\n"));
    out.push_str(&format!("To: <{addr}>\r\n"));
    out.push_str(&format!("Subject: {}\r\n", note.subject));
    out.push_str(&format!("Date: {}\r\n", date.to_rfc2822()));
    out.push_str(&format!("Message-ID: {message_id}\r\n"));
    out.push_str(&format!("{NOTE_HEADER}: {NOTE_HEADER_VALUE}\r\n"));
    out.push_str("Auto-Submitted: auto-generated\r\n");
    out.push_str("MIME-Version: 1.0\r\n");
    out.push_str(&format!(
        "Content-Type: multipart/alternative; boundary=\"{boundary}\"\r\n\r\n"
    ));
    out.push_str(&alternative_section(&boundary, &text, &html));
    Ok(out.into_bytes())
}

/// What a pass did, for the log and for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LapsedOutcome {
    /// The note was filed (or attempted) before: never again.
    AlreadyDone,
    /// No client was ever paired.
    NeverPaired,
    /// A client was present within [`IDLE_DAYS`].
    Active,
    /// Outside the owner's daytime; the check does not count.
    NotNow,
    /// Not a Gmail mailbox (see the module docs).
    Unsupported,
    /// No write credential, or it would not yield a token.
    NoCredential,
    /// Fewer than [`MIN_ITEMS`] to list; nothing stamped.
    TooFew(usize),
    /// The request never reached Google; the stamp was lifted.
    Deferred,
    /// Filed: this many qualifying messages (listed or counted).
    Filed(usize),
    /// Google refused it, or the answer was lost. Stamped for good.
    Failed,
}

/// ONE PASS: file the note if this owner is lapsed, it has never been filed,
/// and there is enough to list. See the module docs for every gate.
pub async fn run_lapsed_pass(
    state: &ApiState,
    now: DateTime<Utc>,
) -> Result<LapsedOutcome, ApiError> {
    let gate = store_call(state, move |store, account| {
        if store.get_app_setting(account, STAMP_KEY)?.is_some() {
            return Ok(Err(LapsedOutcome::AlreadyDone));
        }
        let activity = store.client_activity(account)?;
        if activity.first_paired_at.is_none() {
            return Ok(Err(LapsedOutcome::NeverPaired));
        }
        let unnamed = store.unnamed_client_last_used_at(account)?;
        if !is_lapsed(&activity, unnamed, now) {
            return Ok(Err(LapsedOutcome::Active));
        }
        let zone = store.owner_zone(account)?;
        if !in_local_hours(now, zone.zone, zone.source != OwnerZoneSource::Default) {
            return Ok(Err(LapsedOutcome::NotNow));
        }
        Ok(Ok(zone.zone))
    })
    .await?;
    let zone = match gate {
        Ok(zone) => zone,
        Err(outcome) => return Ok(outcome),
    };

    match account_provider(state).await {
        Ok(MailProvider::Gmail) => {}
        Ok(_) => return Ok(LapsedOutcome::Unsupported),
        Err(_) => return Ok(LapsedOutcome::Unsupported),
    }
    let Some(creds) = state.write_creds().cloned() else {
        return Ok(LapsedOutcome::NoCredential);
    };

    let since = now - chrono::Duration::days(WINDOW_DAYS);
    let (candidates, mailbox) = store_call(state, move |store, account| {
        Ok((
            store.lapsed_note_candidates(account, since)?,
            store.account_email(account)?,
        ))
    })
    .await?;
    let lines: Vec<NoteLine> = candidates
        .iter()
        .map(|c| NoteLine::from_candidate(c, zone))
        .collect();
    let Some(note) = compose(&lines) else {
        return Ok(LapsedOutcome::TooFew(lines.len()));
    };

    // A disconnected account (a revoked grant, a refresh that fails) is
    // skipped before anything is stamped.
    if creds.token(state.account_id).await.is_err() {
        return Ok(LapsedOutcome::NoCredential);
    }
    let message_id = crate::scheduled_send::mint_message_id()?;
    let raw = match build_rfc822(&note, &mailbox, &message_id, now) {
        Ok(raw) => raw,
        Err(_) => return Ok(LapsedOutcome::Unsupported),
    };
    let writer = match write_client(state).await {
        Ok(w) => w,
        Err(_) => return Ok(LapsedOutcome::NoCredential),
    };

    // STAMP FIRST, under one store call that re-reads it: at most once.
    let stamped = store_call(state, move |store: &SqliteStore, account| {
        if store.get_app_setting(account, STAMP_KEY)?.is_some() {
            return Ok(false);
        }
        store.set_app_setting(account, STAMP_KEY, &now.to_rfc3339())?;
        Ok(true)
    })
    .await?;
    if !stamped {
        return Ok(LapsedOutcome::AlreadyDone);
    }

    match writer.insert_into_inbox(&raw).await {
        Ok(_) => Ok(LapsedOutcome::Filed(lines.len())),
        Err(WriteError::Paced { .. } | WriteError::MissingCredential(_)) => {
            store_call(state, |store, account| {
                store.delete_app_setting(account, STAMP_KEY)
            })
            .await?;
            Ok(LapsedOutcome::Deferred)
        }
        Err(_) => Ok(LapsedOutcome::Failed),
    }
}

fn log(outcome: &Result<LapsedOutcome, ApiError>) {
    // Outcomes and a count only: never a sender, a subject or an address.
    match outcome {
        Ok(LapsedOutcome::Filed(n)) => {
            eprintln!("squelchd: lapsed note filed ({n} handled messages)")
        }
        Ok(LapsedOutcome::Failed) => eprintln!("squelchd: lapsed note insert failed (not retried)"),
        Ok(LapsedOutcome::Deferred) => eprintln!("squelchd: lapsed note deferred"),
        Ok(_) => {}
        Err(_) => eprintln!("squelchd: a lapsed note pass failed"),
    }
}

/// The check's clock, kept by the worker that runs it: at most one pass per
/// [`CHECK_EVERY`], and a pass that found it was not the owner's daytime
/// does not count.
#[derive(Debug, Default)]
pub struct LapsedCheck {
    last: Option<tokio::time::Instant>,
}

impl LapsedCheck {
    /// Run a pass if one is due, and log what it did.
    pub async fn tick(&mut self, state: &ApiState) {
        let now = tokio::time::Instant::now();
        if self
            .last
            .is_some_and(|last| now.duration_since(last) < CHECK_EVERY)
        {
            return;
        }
        let outcome = run_lapsed_pass(state, Utc::now()).await;
        log(&outcome);
        if !matches!(outcome, Ok(LapsedOutcome::NotNow)) {
            self.last = Some(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use squelch_core::store::sqlite::lapsed_note::NoteKind;
    use squelch_core::store::{ClientPlatform, PlatformActivity};

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    const NOW: &str = "2026-11-18T17:00:00Z";

    fn activity(paired: &str, seen: Option<&str>, acted: Option<&str>) -> ClientActivity {
        let mut a = ClientActivity {
            first_paired_at: Some(at(paired)),
            ..Default::default()
        };
        a.platforms.insert(
            ClientPlatform::Macos,
            PlatformActivity {
                first_paired_at: at(paired),
                last_seen_at: seen.map(at),
                last_action_at: acted.map(at),
            },
        );
        a
    }

    // ---- the dead rule ---------------------------------------------------

    #[test]
    fn never_paired_is_never_lapsed() {
        assert!(!is_lapsed(&ClientActivity::default(), None, at(NOW)));
        // Even with an unnamed token's use on record, no pairing is no pairing.
        assert!(!is_lapsed(
            &ClientActivity::default(),
            Some(at("2026-01-01T00:00:00Z")),
            at(NOW)
        ));
    }

    #[test]
    fn paired_and_active_is_not_lapsed() {
        let a = activity("2026-10-01T00:00:00Z", Some("2026-11-17T00:00:00Z"), None);
        assert!(!is_lapsed(&a, None, at(NOW)));
    }

    /// Open every day and never acting is reading, not gone.
    #[test]
    fn seen_without_acting_counts_as_present() {
        let a = activity(
            "2026-10-01T00:00:00Z",
            Some("2026-11-16T00:00:00Z"),
            Some("2026-10-02T00:00:00Z"),
        );
        assert!(!is_lapsed(&a, None, at(NOW)));
    }

    #[test]
    fn paired_and_idle_ten_days_is_lapsed() {
        let a = activity(
            "2026-10-01T00:00:00Z",
            Some("2026-11-08T17:00:00Z"),
            Some("2026-11-07T00:00:00Z"),
        );
        assert!(is_lapsed(&a, None, at(NOW)), "exactly ten days");
        let a = activity("2026-10-01T00:00:00Z", Some("2026-11-08T17:00:01Z"), None);
        assert!(!is_lapsed(&a, None, at(NOW)), "a second short of ten days");
    }

    /// A fresh pairing nobody has used yet is not a lapse either.
    #[test]
    fn a_recent_pairing_is_presence() {
        let a = activity("2026-11-15T00:00:00Z", None, None);
        assert!(!is_lapsed(&a, None, at(NOW)));
    }

    /// A client that never names itself (the terminal) is seen through its
    /// token's last use.
    #[test]
    fn an_unnamed_clients_use_is_presence() {
        let a = ClientActivity {
            first_paired_at: Some(at("2026-09-01T00:00:00Z")),
            ..Default::default()
        };
        assert!(is_lapsed(&a, None, at(NOW)));
        assert!(!is_lapsed(&a, Some(at("2026-11-17T00:00:00Z")), at(NOW)));
    }

    #[test]
    fn local_hours_gate_only_a_known_zone() {
        let la: Tz = "America/Los_Angeles".parse().unwrap();
        // 09:00 PST is in, 08:59 and 18:00 are out.
        assert!(in_local_hours(at("2026-11-18T17:00:00Z"), la, true));
        assert!(!in_local_hours(at("2026-11-18T16:59:00Z"), la, true));
        assert!(!in_local_hours(at("2026-11-19T02:00:00Z"), la, true));
        assert!(in_local_hours(at("2026-11-19T02:00:00Z"), la, false));
    }

    // ---- the template ----------------------------------------------------

    fn line(n: usize) -> NoteLine {
        NoteLine {
            kind: "Receipt",
            sender: format!("Shop {n}"),
            subject: format!("Your order {n}"),
            date: "Nov 12".into(),
        }
    }

    #[test]
    fn fewer_than_three_is_no_note() {
        assert_eq!(compose(&[]), None);
        assert_eq!(compose(&[line(1), line(2)]), None);
        assert!(compose(&[line(1), line(2), line(3)]).is_some());
    }

    #[test]
    fn the_list_is_capped_and_the_rest_counted() {
        let lines: Vec<NoteLine> = (1..=11).map(line).collect();
        let note = compose(&lines).unwrap();
        for n in 1..=8 {
            assert!(note.text.contains(&format!("Your order {n}\n")), "{n}");
            assert!(note.html.contains(&format!("Your order {n}</li>")), "{n}");
        }
        for n in 9..=11 {
            assert!(!note.text.contains(&format!("Your order {n}\n")), "{n}");
        }
        assert!(note.text.contains("And 3 more."));
        assert!(note.html.contains("And 3 more."));
        // Exactly eight: no "and N more" line.
        let note = compose(&lines[..8]).unwrap();
        assert!(!note.text.contains("more."));
    }

    #[test]
    fn the_copy_is_fixed_plain_and_dashless() {
        let note = compose(&[line(1), line(2), line(3)]).unwrap();
        assert_eq!(note.subject, "You shouldn't have had to open these");
        assert!(note.text.starts_with(LEAD));
        assert!(
            note.text
                .contains("Receipt from Shop 1, Nov 12\n  Your order 1\n")
        );
        assert!(note.text.trim_end().ends_with(LINK));
        assert!(note.html.contains(&format!("<a href=\"{LINK}\">")));
        for s in [note.subject, &note.text, &note.html, LEAD, LINK_LEAD] {
            assert!(
                !s.contains('\u{2014}') && !s.contains('\u{2013}'),
                "no dashes: {s}"
            );
        }
        // The HTML's `<!doctype>` is markup, not copy.
        for s in [note.subject, &note.text, LEAD, LINK_LEAD] {
            assert!(!s.contains('!'), "no exclamation marks: {s}");
        }
    }

    /// Senders and subjects are strangers' text: escaped in the HTML, and
    /// folded to one line everywhere.
    #[test]
    fn untrusted_text_is_escaped_and_folded() {
        let c = NoteCandidate {
            from_addr: "x@shop.example".into(),
            from_name: Some("<b>Shop</b>\r\nBcc: someone@else.example".into()),
            subject: "Order <script>alert(1)</script>\nshipped".into(),
            received_at: at("2026-11-12T20:00:00Z"),
            kind: NoteKind::Shipped,
        };
        let l = NoteLine::from_candidate(&c, Tz::UTC);
        assert!(!l.sender.contains('\n') && !l.subject.contains('\n'));
        let note = compose(&[l.clone(), l.clone(), l]).unwrap();
        assert!(!note.html.contains("<script>") && !note.html.contains("<b>Shop"));
        assert!(note.html.contains("&lt;script&gt;"));
    }

    #[test]
    fn a_line_names_the_sender_kind_and_owner_local_date() {
        let la: Tz = "America/Los_Angeles".parse().unwrap();
        let c = NoteCandidate {
            from_addr: "orders@shop.example".into(),
            from_name: None,
            subject: "  Your   receipt ".into(),
            // 02:00 UTC on the 13th is still the 12th in California.
            received_at: at("2026-11-13T02:00:00Z"),
            kind: NoteKind::Receipt,
        };
        let l = NoteLine::from_candidate(&c, la);
        assert_eq!(
            l,
            NoteLine {
                kind: "Receipt",
                sender: "orders@shop.example".into(),
                subject: "Your receipt".into(),
                date: "Nov 12".into(),
            }
        );
    }

    // ---- the message -----------------------------------------------------

    fn note() -> Note {
        compose(&[line(1), line(2), line(3)]).unwrap()
    }

    #[test]
    fn the_message_has_both_parts_and_names_itself() {
        let raw = build_rfc822(&note(), "me@example.com", "<abc@passband.app>", at(NOW)).unwrap();
        let s = String::from_utf8(raw.clone()).unwrap();
        assert!(s.starts_with("From: Passband <me@example.com>\r\nTo: <me@example.com>\r\n"));
        assert!(s.contains("Subject: You shouldn't have had to open these\r\n"));
        assert!(s.contains("X-Passband-Note: lapsed-digest\r\n"));
        assert!(s.contains("Auto-Submitted: auto-generated\r\n"));
        assert!(s.contains("Message-ID: <abc@passband.app>\r\n"));
        assert!(s.contains("Content-Type: multipart/alternative;"));
        let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
        assert_eq!(parsed.subject(), Some(SUBJECT));
        assert!(parsed.body_text(0).unwrap().contains("Your order 2"));
        assert!(parsed.body_html(0).unwrap().contains("<li"));
        assert_eq!(
            parsed.from().unwrap().first().unwrap().name(),
            Some("Passband")
        );
    }

    #[test]
    fn a_mailbox_that_could_break_a_header_is_refused() {
        for bad in [
            "",
            "no-at-sign",
            "me@example.com\r\nBcc: x@y.example",
            "a b@c.example",
        ] {
            assert!(
                build_rfc822(&note(), bad, "<abc@passband.app>", at(NOW)).is_err(),
                "{bad:?}"
            );
        }
    }
}
