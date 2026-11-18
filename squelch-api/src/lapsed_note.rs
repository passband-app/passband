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
//! IT NEVER SURFACES IN PASSBAND. The note is From "Passband"
//! <[`FROM_ADDR`]> and carries `X-Passband-Note: lapsed-digest`. A filed
//! message crosses no mail server, so it has no `Received` and no
//! `Authentication-Results`, and with the header that is what ingest
//! recognises (`squelch_core::sync::ingest::is_filed_passband_note`): it is
//! stored like an archive row, never triaged, never placed in For your eyes
//! or Reading, never pushed, and never listed by a later note. Mail that
//! arrived carrying the same header is triaged like any other, so a stranger
//! cannot use it to hide mail. `Auto-Submitted` names it for other readers.
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

/// The header that names the note, and that ingest reads it by.
pub use squelch_core::sync::ingest::{
    PASSBAND_NOTE_HEADER as NOTE_HEADER, PASSBAND_NOTE_VALUE as NOTE_HEADER_VALUE,
};
/// Who the note is From, with the display name "Passband".
pub const FROM_ADDR: &str = "no-reply@passband.app";

/// The note's subject.
pub const SUBJECT: &str = "You shouldn't have had to open these";
/// The headline, first and large.
pub const HEADLINE: &str = "Your email didn't need to be that much work.";
/// The Passband mark above the headline: the same public asset the invite
/// email uses (`squelch-control`'s `resend::MARK_URL`), in its variant for a
/// light background, as the invite's is for a dark one. A fixed URL with no
/// per-user parameters, so fetching it tells us nothing about who opened the
/// note. The wordmark beside it is live text, so a client that blocks remote
/// images still shows the name.
pub const MARK_URL: &str = "https://passband.app/mark.png";
/// The muted line under it.
pub const LEAD: &str = "Passband handled these this week. No opening, no marking read.";
/// The one link. No `passband://` link merely opens the app (the scheme's
/// actions are pairing and drafts), and webmail drops custom schemes anyway.
pub const LINK: &str = "https://passband.app/download";
/// The button's words.
pub const BUTTON: &str = "Open Passband";

/// Longest a sender, subject or snippet runs in the list before it is cut.
const SENDER_CAP: usize = 40;
const SUBJECT_CAP: usize = 90;
const SNIPPET_CAP: usize = 110;

/// The avatar colours, picked per sender by [`avatar_colour`].
pub const PALETTE: [&str; 8] = [
    "#5b8def", "#c27c3e", "#3a9d7a", "#9b6bd6", "#d0587e", "#4a9fb5", "#b5893a", "#6b7bd6",
];

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
    /// The stored `messages.snippet`, never the body. Only ever from mail the
    /// selection admitted, which is never sealed or auth mail.
    pub snippet: String,
    /// `Nov 12`, in the owner's zone.
    pub date: String,
    /// The avatar's letter and colour ([`avatar_initial`], [`avatar_colour`]).
    pub initial: char,
    pub colour: &'static str,
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
            snippet: clip(&c.snippet, SNIPPET_CAP),
            date: format!("{} {}", month(local.month()), local.day()),
            initial: avatar_initial(name),
            colour: avatar_colour(&c.from_addr),
        }
    }
}

/// The first letter or digit of a sender's name, uppercased; `?` for none.
pub fn avatar_initial(name: &str) -> char {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .and_then(|c| c.to_uppercase().next())
        .unwrap_or('?')
}

/// A sender's avatar colour: [`PALETTE`] indexed by an FNV-1a hash of the
/// trimmed, lowercased address, so a sender keeps its colour across rows and
/// builds (std's hasher promises neither).
pub fn avatar_colour(addr: &str) -> &'static str {
    let mut h: u32 = 0x811c_9dc5;
    for b in addr.trim().to_lowercase().bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    PALETTE[(h as usize) % PALETTE.len()]
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
    text.push_str(&format!("{HEADLINE}\n{LEAD}\n\n"));
    for l in shown {
        text.push_str(&format!(
            "{} \u{b7} {}, {}\n  {}\n",
            l.sender, l.kind, l.date, l.subject
        ));
        if !l.snippet.is_empty() {
            text.push_str(&format!("  {}\n", l.snippet));
        }
        text.push('\n');
    }
    if let Some(m) = &more_line {
        text.push_str(&format!("{m}\n\n"));
    }
    text.push_str(&format!("{BUTTON}: {LINK}\n"));

    // Tables and inline styles only, one image (the mark, with live text
    // beside it): what every mail client renders the same.
    const TABLE: &str =
        "role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\"";
    let mut html = String::new();
    html.push_str(
        "<!doctype html><html><body style=\"margin:0;padding:16px;background:#ffffff\">\n",
    );
    html.push_str(&format!(
        "<table {TABLE} style=\"max-width:560px;font-family:-apple-system,Helvetica,Arial,sans-serif;color:#1d1d1f\">\n\
         <tr><td style=\"padding:0 0 28px 0\">\n\
         <table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\"><tr>\n\
         <td style=\"padding-right:10px\"><img src=\"{MARK_URL}\" alt=\"\" width=\"45\" height=\"24\" style=\"display:block;border:0\"></td>\n\
         <td style=\"font:500 22px/1 Newsreader,Georgia,'Times New Roman',serif;color:#1d1d1f;vertical-align:middle\">Passband</td>\n\
         </tr></table></td></tr>\n\
         <tr><td style=\"padding:0 0 6px 0;font:500 28px/1.15 Newsreader,Georgia,'Times New Roman',serif;letter-spacing:-0.01em;color:#1d1d1f\">{}</td></tr>\n\
         <tr><td style=\"padding:0 0 20px 0;font-size:15px;line-height:1.5;color:#6e6e73\">{}</td></tr>\n\
         <tr><td>\n<table {TABLE} style=\"border:1px solid #e5e5ea;border-radius:12px;border-collapse:separate\">\n",
        escape_html(HEADLINE),
        escape_html(LEAD)
    ));
    for (i, l) in shown.iter().enumerate() {
        let separator = if i + 1 < shown.len() {
            ";border-bottom:1px solid #f0f0f2"
        } else {
            ""
        };
        let snippet = if l.snippet.is_empty() {
            String::new()
        } else {
            format!(
                "<div style=\"color:#8e8e93\">{}</div>\n",
                escape_html(&l.snippet)
            )
        };
        html.push_str(&format!(
            "<tr><td style=\"padding:12px 14px{separator}\">\n\
             <table {TABLE}><tr>\n\
             <td width=\"36\" valign=\"top\" style=\"padding-right:12px\"><div style=\"width:36px;height:36px;border-radius:18px;background:{colour};color:#fff;font-size:15px;font-weight:600;line-height:36px;text-align:center\">{initial}</div></td>\n\
             <td valign=\"top\" style=\"font-size:14px;line-height:1.35\">\n\
             <table {TABLE}><tr>\n\
             <td style=\"font-weight:600;color:#1d1d1f\">{sender}</td>\n\
             <td align=\"right\" style=\"font-size:12px;color:#8e8e93;white-space:nowrap\">{kind} &middot; {date}</td></tr></table>\n\
             <div style=\"color:#1d1d1f\">{subject}</div>\n\
             {snippet}</td></tr></table></td></tr>\n",
            colour = l.colour,
            initial = escape_html(&l.initial.to_string()),
            sender = escape_html(&l.sender),
            kind = escape_html(l.kind),
            date = escape_html(&l.date),
            subject = escape_html(&l.subject),
        ));
    }
    html.push_str("</table>\n</td></tr>\n");
    if let Some(m) = &more_line {
        html.push_str(&format!(
            "<tr><td style=\"padding:12px 0 0 0;font-size:14px;color:#8e8e93\">{}</td></tr>\n",
            escape_html(m)
        ));
    }
    html.push_str(&format!(
        "<tr><td style=\"padding:24px 0 0 0\">\n\
         <a href=\"{LINK}\" style=\"display:inline-block;background:#1d1d1f;color:#ffffff;text-decoration:none;font-size:15px;font-weight:600;padding:11px 20px;border-radius:10px\">{}</a>\n\
         </td></tr>\n</table>\n</body></html>\n",
        escape_html(BUTTON)
    ));

    Some(Note {
        subject: SUBJECT,
        text,
        html,
    })
}

/// The note as RFC 822 bytes, From "Passband" <[`FROM_ADDR`]> and To
/// `mailbox` (the owner's own address), multipart/alternative with a
/// text/plain part first. No `Received` and no `Authentication-Results`, which
/// with [`NOTE_HEADER`] is how ingest knows it. Refused when `mailbox` could
/// break a header.
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
    out.push_str(&format!("From: Passband <{FROM_ADDR}>\r\n"));
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
            snippet: format!("Thanks for shopping {n}"),
            date: "Nov 12".into(),
            initial: 'S',
            colour: PALETTE[0],
        }
    }

    #[test]
    fn fewer_than_three_is_no_note() {
        assert_eq!(compose(&[]), None);
        assert_eq!(compose(&[line(1), line(2)]), None);
        assert!(compose(&[line(1), line(2), line(3)]).is_some());
    }

    #[test]
    fn the_list_is_capped_at_eight_rows_and_the_rest_counted() {
        let lines: Vec<NoteLine> = (1..=11).map(line).collect();
        let note = compose(&lines).unwrap();
        for n in 1..=8 {
            assert!(note.text.contains(&format!("  Your order {n}\n")), "{n}");
            assert!(note.html.contains(&format!(">Your order {n}</div>")), "{n}");
        }
        for n in 9..=11 {
            assert!(!note.text.contains(&format!("Your order {n}\n")), "{n}");
            assert!(!note.html.contains(&format!(">Your order {n}<")), "{n}");
        }
        assert_eq!(
            note.html.matches("border-radius:18px").count(),
            8,
            "8 avatars"
        );
        // Hairlines between rows, none under the last.
        assert_eq!(
            note.html.matches("border-bottom:1px solid #f0f0f2").count(),
            7
        );
        assert!(note.text.contains("And 3 more."));
        assert!(note.html.contains(">And 3 more.</td>"));
        // Exactly eight: no "and N more" line.
        let note = compose(&lines[..8]).unwrap();
        assert!(!note.text.contains("more.") && !note.html.contains("more."));
    }

    #[test]
    fn the_copy_is_fixed_plain_and_dashless() {
        let note = compose(&[line(1), line(2), line(3)]).unwrap();
        assert_eq!(note.subject, "You shouldn't have had to open these");
        assert!(note.text.starts_with(
            "Your email didn't need to be that much work.\n\
             Passband handled these this week. No opening, no marking read.\n\n"
        ));
        assert!(
            note.html
                .contains(">Your email didn&#39;t need to be that much work.</td>")
        );
        assert!(note.html.contains(&format!(">{LEAD}</td>")));
        for s in [note.subject, &note.text, &note.html, HEADLINE, LEAD, BUTTON] {
            assert!(
                !s.contains('\u{2014}') && !s.contains('\u{2013}'),
                "no dashes: {s}"
            );
        }
        assert!(note.html.contains("Receipt &middot; Nov 12"));
        // The HTML's `<!doctype>` is markup, not copy.
        for s in [note.subject, &note.text, HEADLINE, LEAD, BUTTON] {
            assert!(!s.contains('!'), "no exclamation marks: {s}");
        }
    }

    /// The plain part mirrors the rows: sender, kind and date, then subject
    /// and snippet indented, then the count and the link.
    #[test]
    fn the_plain_part_mirrors_the_rows() {
        let lines: Vec<NoteLine> = (1..=9).map(line).collect();
        let note = compose(&lines).unwrap();
        assert!(
            note.text.contains(
                "Shop 1 \u{b7} Receipt, Nov 12\n  Your order 1\n  Thanks for shopping 1\n"
            )
        );
        assert!(note.text.contains("\nAnd 1 more.\n"));
        assert!(
            note.text
                .trim_end()
                .ends_with("Open Passband: https://passband.app/download")
        );
        assert!(!note.text.contains('<'), "no markup in the plain part");
    }

    /// The ending is one dark button to the download page, not a bare URL.
    #[test]
    fn the_ending_is_the_open_passband_button() {
        let note = compose(&[line(1), line(2), line(3)]).unwrap();
        assert!(note.html.contains(
            "<a href=\"https://passband.app/download\" style=\"display:inline-block;background:#1d1d1f;"
        ));
        assert!(note.html.contains(">Open Passband</a>"));
        assert_eq!(note.html.matches("<a ").count(), 1, "one link");
    }

    /// The mark is the one image: the fixed public asset, no parameters, and
    /// the name beside it is live text.
    #[test]
    fn the_header_is_the_mark_and_the_live_wordmark() {
        let note = compose(&[line(1), line(2), line(3)]).unwrap();
        assert!(note.html.contains(
            "<img src=\"https://passband.app/mark.png\" alt=\"\" width=\"45\" height=\"24\" style=\"display:block;border:0\">"
        ));
        assert_eq!(note.html.matches("<img ").count(), 1, "one image");
        assert!(!MARK_URL.contains('?') && !MARK_URL.contains('#'));
        assert!(
            note.html
                .contains("serif;color:#1d1d1f;vertical-align:middle\">Passband</td>")
        );
        let mark = note.html.find("mark.png").unwrap();
        let headline = note.html.find("need to be that much work").unwrap();
        assert!(mark < headline, "the mark sits above the headline");
    }

    #[test]
    fn the_initial_is_the_first_letter_or_digit_uppercased() {
        assert_eq!(avatar_initial("northwind outfitters"), 'N');
        assert_eq!(avatar_initial("  \"7-Eleven\""), '7');
        assert_eq!(avatar_initial("\u{e9}lan"), '\u{c9}');
        assert_eq!(avatar_initial("---"), '?');
        assert_eq!(avatar_initial(""), '?');
    }

    /// One sender, one colour: across rows, case and whitespace, and pinned
    /// to fixed values so a toolchain change cannot reshuffle them.
    #[test]
    fn a_senders_colour_is_stable() {
        let a = avatar_colour("orders@shop.example");
        assert_eq!(a, avatar_colour(" Orders@Shop.example "));
        assert!(PALETTE.contains(&a));
        let colours: std::collections::HashSet<_> = (0..40)
            .map(|n| avatar_colour(&format!("s{n}@shop.example")))
            .collect();
        assert!(colours.len() > 3, "senders spread over the palette");
        // FNV-1a is fixed by definition: pin one value.
        assert_eq!(
            avatar_colour(""),
            PALETTE[0x811c_9dc5_usize % PALETTE.len()]
        );
    }

    /// Senders, subjects and snippets are strangers' text: escaped in the
    /// HTML, and folded to one line everywhere.
    #[test]
    fn untrusted_text_is_escaped_and_folded() {
        let c = NoteCandidate {
            from_addr: "x@shop.example".into(),
            from_name: Some("<b>Shop</b>\r\nBcc: someone@else.example".into()),
            subject: "Order <script>alert(1)</script>\nshipped".into(),
            snippet: "Click <a href=\"https://evil.example\">here</a>\r\nnow & \"save\"".into(),
            received_at: at("2026-11-12T20:00:00Z"),
            kind: NoteKind::Shipped,
        };
        let l = NoteLine::from_candidate(&c, Tz::UTC);
        assert!(!l.sender.contains('\n') && !l.subject.contains('\n') && !l.snippet.contains('\n'));
        assert_eq!(l.initial, 'B', "the first letter of the name as written");
        let note = compose(&[l.clone(), l.clone(), l]).unwrap();
        assert!(!note.html.contains("<script>") && !note.html.contains("<b>Shop"));
        assert!(note.html.contains("&lt;script&gt;"));
        assert!(!note.html.contains("evil.example\">"), "no stranger's link");
        assert!(
            note.html
                .contains("Click &lt;a href=&quot;https://evil.example&quot;&gt;")
        );
        assert!(note.html.contains("now &amp; &quot;save&quot;"));
        assert_eq!(note.html.matches("<a ").count(), 1, "only our button links");
    }

    #[test]
    fn a_long_snippet_is_clipped() {
        let c = NoteCandidate {
            from_addr: "orders@shop.example".into(),
            from_name: Some("Shop".into()),
            subject: "s".into(),
            snippet: "word ".repeat(60),
            received_at: at("2026-11-12T20:00:00Z"),
            kind: NoteKind::Receipt,
        };
        let l = NoteLine::from_candidate(&c, Tz::UTC);
        assert!(l.snippet.chars().count() <= 110, "{}", l.snippet);
        assert!(l.snippet.ends_with('\u{2026}'));
    }

    #[test]
    fn a_line_names_the_sender_kind_and_owner_local_date() {
        let la: Tz = "America/Los_Angeles".parse().unwrap();
        let c = NoteCandidate {
            from_addr: "orders@shop.example".into(),
            from_name: None,
            subject: "  Your   receipt ".into(),
            snippet: "Thanks  for your order".into(),
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
                snippet: "Thanks for your order".into(),
                date: "Nov 12".into(),
                initial: 'O',
                colour: avatar_colour("orders@shop.example"),
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
        assert!(
            s.starts_with("From: Passband <no-reply@passband.app>\r\nTo: <me@example.com>\r\n")
        );
        assert!(!s.contains("Received:") && !s.contains("Authentication-Results"));
        // Ingest knows it for what it is.
        let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
        assert!(squelch_core::sync::ingest::is_filed_passband_note(&parsed));
        assert!(s.contains("Subject: You shouldn't have had to open these\r\n"));
        assert!(s.contains("X-Passband-Note: lapsed-digest\r\n"));
        assert!(s.contains("Auto-Submitted: auto-generated\r\n"));
        assert!(s.contains("Message-ID: <abc@passband.app>\r\n"));
        assert!(s.contains("Content-Type: multipart/alternative;"));
        let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
        assert_eq!(parsed.subject(), Some(SUBJECT));
        assert!(parsed.body_text(0).unwrap().contains("Your order 2"));
        assert!(parsed.body_html(0).unwrap().contains("Open Passband</a>"));
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
