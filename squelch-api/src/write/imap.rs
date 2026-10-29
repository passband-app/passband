//! THE IMAP ACCOUNT'S WRITER (issue #575 phase 3): send over SMTP, then make
//! sure Sent holds exactly one copy; folder moves and flags ([`ops`]) on the
//! copies the source verified as the id's own; and the lookups through the
//! source's guarded resolve. `find_scheduled_send` is still 501 (step 3).
//!
//! IMAP work happens on a session of the writer's own, opened by
//! squelch-core ([`ImapSource::open_writer_session`]) under the source's TLS
//! policy and timeouts; core itself never issues a write command. The sync
//! source's own session is never borrowed, and only ever `EXAMINE`s.
//!
//! LOOK, THEN APPEND (contract §2). Some servers file a copy of everything
//! submitted into Sent on their own (Gmail, Fastmail, iCloud); most leave it
//! to the client. After SMTP accepts the message the writer looks in Sent for
//! its exact `Message-ID` for about 1.5 seconds (amendment A2), and only if it never shows
//! APPENDs the saved copy, `\Seen`. The saved copy keeps its `Bcc` header;
//! the transmitted one never had it ([`super::smtp::transmitted_copy`]).
//! Either way the copy's location is recorded, so the echo finds it under
//! the stable id `SentRef::id` carries.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use futures::TryStreamExt;
use squelch_core::CoreError;
use squelch_core::mail::imap::{
    FolderRoles, ImapCredential, ImapSession, ImapSource, MessageLocation, header_block,
    header_values, normalize_message_id, stable_id_for_message_id,
};
use squelch_core::mail::{MailSource, RawBody, Urgency};

mod ops;
use ops::{
    LabelPlan, LiveMailbox, Move, WriteMailbox, apply_flags, move_strategy, plan_labels, plan_move,
    run_move,
};

use super::smtp::{self, SmtpEndpoint, SmtpError, SmtpTimeouts};
use super::{FetchedRaw, MailWriter, ParentHeaders, SentRef, WriteError, send_budget};

/// What a write on an IMAP account set up before sending existed is told
/// (contract §1): its slot has no SMTP config. The door answers it 403,
/// audited `rejected:no_write_credential`, as for any missing credential.
pub const IMAP_NO_SMTP: &str = "run `squelchd auth --imap` again to enable sending";

/// The largest raw message a send takes: the most any route can build (25
/// MiB of attachments in base64's 78-per-57 lines, plus the 2 MiB a body
/// adds), the figure the Gmail path's own budget is sized by
/// (`limits::the_long_ceiling_clears_the_largest_send`). Past it the send is
/// refused before anything is said to a server.
pub const MAX_SEND_RAW_BYTES: usize =
    crate::handlers::MAX_OUTBOUND_ATTACHMENT_BYTES * 78 / 57 + 2 * 1024 * 1024;

/// How long the writer waits before each look in Sent for a server's own
/// copy: looks at 0, 0.25, 0.5, 1.0 and 1.5 s after SMTP accepted it
/// (amendment A2), then the APPEND. A server that files its copy later than
/// that ends up with two in Sent; Passband still shows one row, since both
/// have the same stable id. Accepted: the send's answer waits on this.
const LOOK_SCHEDULE: [Duration; 5] = [
    Duration::ZERO,
    Duration::from_millis(250),
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_millis(500),
];

/// The bound on one IMAP step of the Sent copy (EXAMINE, SEARCH, FETCH); the
/// APPEND itself gets [`send_budget`] for its size.
const IMAP_STEP: Duration = Duration::from_secs(60);

/// An IMAP account's [`MailWriter`].
pub struct ImapWriter {
    source: Arc<ImapSource>,
    smtp: SmtpEndpoint,
    /// The account's address: `From` when the composer left it out, and
    /// always the envelope's MAIL FROM (contract §7).
    from: String,
    timeouts: SmtpTimeouts,
    look_schedule: Vec<Duration>,
    /// Tests only: act as if the server had no MOVE, so the COPY + UID
    /// EXPUNGE path runs against a real one.
    without_move: bool,
}

impl ImapWriter {
    /// The writer for an account whose sealed slot is `credential`, reading
    /// and appending through `source`. A slot with no SMTP config is
    /// [`WriteError::MissingCredential`] with [`IMAP_NO_SMTP`].
    pub fn new(
        source: Arc<ImapSource>,
        credential: &ImapCredential,
        from: impl Into<String>,
    ) -> Result<Self, WriteError> {
        let smtp = SmtpEndpoint::from_config(&credential.config, credential.smtp_login_password())
            .map_err(|e| WriteError::Transport(e.to_string()))?
            .ok_or_else(|| WriteError::MissingCredential(IMAP_NO_SMTP.into()))?;
        Ok(Self {
            source,
            smtp,
            from: from.into(),
            timeouts: SmtpTimeouts::default(),
            look_schedule: LOOK_SCHEDULE.to_vec(),
            without_move: false,
        })
    }

    /// Pretend the server lacks MOVE, for the tests.
    #[cfg(test)]
    pub(crate) fn without_move(mut self) -> Self {
        self.without_move = true;
        self
    }

    /// Shorter waits, for the tests.
    #[cfg(test)]
    pub(crate) fn with_look_schedule(mut self, schedule: Vec<Duration>) -> Self {
        self.look_schedule = schedule;
        self
    }

    async fn open_session(&self) -> Result<(ImapSession, FolderRoles), WriteError> {
        tokio::time::timeout(IMAP_STEP, self.source.open_writer_session())
            .await
            .map_err(|_| WriteError::Transport("imap login timed out".into()))?
            .map_err(core_write_error)
    }

    /// EVERY COPY A WRITE MAY TOUCH (contract §5): the id's locations whose
    /// headers and body are the id's own, by the source's guarded resolve.
    /// None at all is the 404 Gmail gives a message it does not have.
    async fn targets(&self, msg_id: &str) -> Result<Vec<MessageLocation>, WriteError> {
        match self.source.verified_locations(msg_id).await {
            Ok(locs) if !locs.is_empty() => Ok(locs),
            Ok(_) | Err(CoreError::NotFound) => Err(not_found()),
            Err(e) => Err(core_write_error(e)),
        }
    }

    /// Verify, log in, carry the plan out, record where moved copies went,
    /// log out.
    async fn apply(&self, msg_id: &str, plan: LabelPlan) -> Result<(), WriteError> {
        let locs = self.targets(msg_id).await?;
        let (mut session, mut roles) = self.open_session().await?;
        let caps = LiveMailbox::read_caps(&mut session).await;
        let (moved, out) = match caps {
            Ok(mut caps) => {
                caps.mv &= !self.without_move;
                let mut mb = LiveMailbox { session, caps };
                let r = apply_plan(&mut mb, &mut roles, &locs, &plan).await;
                session = mb.session;
                r
            }
            Err(e) => (Vec::new(), Err(e)),
        };
        let _ = tokio::time::timeout(Duration::from_secs(5), session.logout()).await;
        for (from, to) in moved {
            // A miss here is the next sighting's to put right.
            if self.source.note_moved(msg_id, &from, to).is_err() {
                eprintln!("squelch-api: imap: could not record where a moved message went");
            }
        }
        out
    }

    /// The id's bytes as the source's guarded fetch serves them, and when the
    /// server received them.
    async fn verified_raw(
        &self,
        msg_id: &str,
    ) -> Result<(Vec<u8>, Option<chrono::DateTime<chrono::Utc>>), WriteError> {
        match self.source.fetch_raw(msg_id, Urgency::Live).await {
            Ok(m) => match m.raw {
                RawBody::Bytes(b) => Ok((b, m.received_at)),
                _ => Err(not_found()),
            },
            Err(CoreError::NotFound) => Err(not_found()),
            Err(e) => Err(core_write_error(e)),
        }
    }

    fn not_built(op: &str) -> WriteError {
        WriteError::Api {
            status: 501,
            message: format!("imap {op} is not built yet"),
        }
    }
}

/// Map a submission failure onto the write door's error, keeping its one
/// distinction that matters: `Transport` is "may have been sent" to every
/// caller (`SendFailure::after_post`), a 4xx `Api` is "refused, not sent",
/// and a 5xx `Api` is "not sent, but look before retrying".
fn smtp_write_error(e: SmtpError) -> WriteError {
    match e {
        SmtpError::MaybeSent(_) => WriteError::Transport(e.to_string()),
        SmtpError::Refused(m) => WriteError::Api {
            status: 400,
            message: m,
        },
        SmtpError::Auth => WriteError::Api {
            status: 401,
            message: smtp::SMTP_LOGIN_REFUSED.into(),
        },
        SmtpError::NotSent(m) => WriteError::Api {
            status: 503,
            message: m,
        },
    }
}

/// What a write on a message with no verified copy gets: Gmail's 404.
fn not_found() -> WriteError {
    WriteError::Api {
        status: 404,
        message: "no copy of this message on the mail server".into(),
    }
}

/// A core error from the writer's IMAP side. A refused login is one fixed
/// line (core's own, which never carries the server's text).
fn core_write_error(e: CoreError) -> WriteError {
    match e {
        CoreError::Credential(m) => WriteError::Api {
            status: 401,
            message: m,
        },
        CoreError::NotFound => not_found(),
        e => WriteError::Transport(e.to_string()),
    }
}

/// THE ORDER (contract §4-§6): the move planned and the server's ability to
/// move checked FIRST, so a refusal (no Junk, no Trash, no MOVE nor UIDPLUS)
/// sends no command at all; then the flags on every verified copy; then the
/// move. Each copy moved is handed back, with any failure after it.
pub(crate) async fn apply_plan(
    mb: &mut dyn WriteMailbox,
    roles: &mut FolderRoles,
    locs: &[MessageLocation],
    plan: &LabelPlan,
) -> (
    Vec<(MessageLocation, Option<MessageLocation>)>,
    Result<(), WriteError>,
) {
    let mv = match plan.mv.map(|m| plan_move(m, roles, locs)).transpose() {
        Ok(mv) => mv.flatten(),
        Err(e) => return (Vec::new(), Err(e)),
    };
    let strategy = match mv.as_ref().map(|_| move_strategy(mb.caps())).transpose() {
        Ok(s) => s,
        Err(e) => return (Vec::new(), Err(e)),
    };
    if let Err(e) = apply_flags(mb, locs, &plan.add_flags, &plan.remove_flags).await {
        return (Vec::new(), Err(e));
    }
    match (mv, strategy) {
        (Some(mv), Some(strategy)) => run_move(mb, roles, mv, strategy).await,
        _ => (Vec::new(), Ok(())),
    }
}

/// A parent's threading and recipient headers from its raw bytes, as the
/// wire has them (unfolded), the way Gmail's metadata gives them.
fn parent_headers_of(raw: &[u8]) -> ParentHeaders {
    let head = header_block(raw);
    let one = |name: &str| header_values(head, name).into_iter().next();
    ParentHeaders {
        message_id: one("Message-ID"),
        references: one("References"),
        reply_to: one("Reply-To"),
        from: one("From"),
        to: one("To"),
        cc: one("Cc"),
    }
}

/// The two things the Sent copy needs of a mailbox, so the decision can be
/// tested against a scripted one.
#[async_trait::async_trait]
pub(crate) trait SentFolder: Send {
    /// The location of the message in Sent whose `Message-ID` is EXACTLY
    /// `message_id` (normalized), lowest UID first; `None` if there is none.
    async fn look(&mut self, message_id: &str) -> Result<Option<MessageLocation>, String>;
    /// APPEND `bytes` to Sent with `\Seen`.
    async fn append_seen(&mut self, bytes: &[u8]) -> Result<(), String>;
}

/// Where the sent message's copy in Sent came from.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SentCopy {
    /// The server filed one itself; nothing was appended.
    ServerSaved(MessageLocation),
    /// The writer appended it (and found it at this location afterwards, when
    /// it could look).
    Appended(Option<MessageLocation>),
}

/// LOOK, THEN APPEND: look for `message_id` at each of `schedule`'s waits,
/// and APPEND `saved` only if no look found it. A look that fails counts as
/// not found: a second copy in Sent is a nuisance, no copy at all is what
/// leaves a crashed scheduled send unable to tell it already went.
pub(crate) async fn look_then_append(
    folder: &mut dyn SentFolder,
    message_id: &str,
    saved: &[u8],
    schedule: &[Duration],
) -> Result<SentCopy, String> {
    for wait in schedule {
        if !wait.is_zero() {
            tokio::time::sleep(*wait).await;
        }
        if let Ok(Some(loc)) = folder.look(message_id).await {
            return Ok(SentCopy::ServerSaved(loc));
        }
    }
    folder.append_seen(saved).await?;
    Ok(SentCopy::Appended(
        folder.look(message_id).await.ok().flatten(),
    ))
}

/// The lowest-UID copy in `folder` whose `Message-ID` is EXACTLY
/// `message_id` (normalized, bare), or `None`. The folder is `EXAMINE`d
/// afresh (a server shows a session what arrived in its open folder only
/// when the view is re-synced), searched, and every hit read back: `SEARCH
/// HEADER` matches any header CONTAINING the text.
pub(crate) async fn exact_message_id_hit(
    session: &mut ImapSession,
    folder: &str,
    message_id: &str,
) -> Result<Option<MessageLocation>, String> {
    // It goes inside a quoted string: refuse anything that could end it.
    if message_id.contains(['"', '\\', '\r', '\n']) || message_id.is_empty() {
        return Ok(None);
    }
    tokio::time::timeout(IMAP_STEP, async {
        let mbox = match session.examine(folder).await {
            Ok(m) => m,
            Err(async_imap::error::Error::No(_)) => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let uidvalidity = mbox.uid_validity.ok_or("no UIDVALIDITY")?;
        let hits = session
            .uid_search(format!("HEADER Message-ID \"{message_id}\""))
            .await
            .map_err(|e| e.to_string())?;
        if hits.is_empty() {
            return Ok(None);
        }
        let mut uids: Vec<u32> = hits.into_iter().collect();
        uids.sort_unstable();
        let set = uids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let fetches: Vec<_> = session
            .uid_fetch(set, "(UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])")
            .await
            .map_err(|e| e.to_string())?
            .try_collect()
            .await
            .map_err(|e| e.to_string())?;
        let mut exact: Vec<u32> = fetches
            .iter()
            .filter(|f| {
                f.header().is_some_and(|h| {
                    header_values(h, "Message-ID")
                        .into_iter()
                        .find_map(|v| normalize_message_id(&v))
                        .is_some_and(|m| m == message_id)
                })
            })
            .filter_map(|f| f.uid)
            .filter(|u| uids.contains(u))
            .collect();
        exact.sort_unstable();
        Ok(exact.first().map(|&uid| MessageLocation {
            folder: folder.to_string(),
            uidvalidity,
            uid,
            message_id: Some(message_id.to_string()),
        }))
    })
    .await
    .unwrap_or_else(|_| Err(format!("imap search in {folder:?} timed out")))
}

/// The live [`SentFolder`]: the writer's own session, opened on first use
/// and again after an error broke it.
struct LiveSent<'a> {
    source: &'a ImapSource,
    session: Option<(ImapSession, String)>,
}

impl LiveSent<'_> {
    async fn session(&mut self) -> Result<&mut (ImapSession, String), String> {
        if self.session.is_none() {
            let (session, roles) =
                tokio::time::timeout(IMAP_STEP, self.source.open_writer_session())
                    .await
                    .map_err(|_| "imap login timed out".to_string())?
                    .map_err(|e| e.to_string())?;
            let sent = roles
                .sent
                .clone()
                .ok_or_else(|| "the server has no Sent folder".to_string())?;
            self.session = Some((session, sent));
        }
        Ok(self.session.as_mut().expect("set above"))
    }

    async fn logout(&mut self) {
        if let Some((mut s, _)) = self.session.take() {
            let _ = tokio::time::timeout(Duration::from_secs(5), s.logout()).await;
        }
    }
}

#[async_trait::async_trait]
impl SentFolder for LiveSent<'_> {
    async fn look(&mut self, message_id: &str) -> Result<Option<MessageLocation>, String> {
        let (session, sent) = self.session().await?;
        let sent = sent.clone();
        let out = exact_message_id_hit(session, &sent, message_id).await;
        if out.is_err() {
            self.session = None;
        }
        out
    }

    async fn append_seen(&mut self, bytes: &[u8]) -> Result<(), String> {
        let budget = send_budget(bytes.len());
        let (session, sent) = self.session().await?;
        let sent = sent.clone();
        let out =
            tokio::time::timeout(budget, session.append(&sent, Some("(\\Seen)"), None, bytes))
                .await
                .map_err(|_| "imap APPEND to Sent timed out".to_string())
                .and_then(|r| r.map_err(|e| e.to_string()));
        if out.is_err() {
            self.session = None;
        }
        out
    }
}

#[async_trait::async_trait]
impl MailWriter for ImapWriter {
    /// The label table ([`ops::plan_labels`]), checked whole before any
    /// command is sent.
    async fn modify(
        &self,
        msg_id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<(), WriteError> {
        let plan = plan_labels(add, remove)?;
        self.apply(msg_id, plan).await
    }

    async fn archive(&self, msg_id: &str) -> Result<(), WriteError> {
        self.apply(msg_id, LabelPlan::moving(Move::Archive)).await
    }

    async fn not_spam(&self, msg_id: &str) -> Result<(), WriteError> {
        self.apply(msg_id, LabelPlan::moving(Move::NotSpam)).await
    }

    /// To `\Trash`; refused where there is none. Never a delete.
    async fn trash(&self, msg_id: &str) -> Result<(), WriteError> {
        self.apply(msg_id, LabelPlan::moving(Move::Trash)).await
    }

    /// From the verified copy's own bytes ([`Self::verified_raw`]): a
    /// lookalike's `Reply-To` never picks a reply's audience.
    async fn parent_headers(&self, msg_id: &str) -> Result<ParentHeaders, WriteError> {
        let (raw, _) = self.verified_raw(msg_id).await?;
        Ok(parent_headers_of(&raw))
    }

    /// SEND (contract §7): add what Gmail used to (From, Date, Message-ID),
    /// take the envelope from To/Cc/Bcc, strip Bcc from the transmitted
    /// bytes, submit, then make sure Sent holds one copy. `thread_id` is
    /// ignored: an IMAP thread is its headers, and ingest threads the echo.
    async fn send(&self, raw: &[u8], _thread_id: Option<&str>) -> Result<SentRef, WriteError> {
        if raw.len() > MAX_SEND_RAW_BYTES {
            return Err(WriteError::Invalid(
                "the message is too large to send".into(),
            ));
        }
        let minted = smtp::mint_message_id(&self.from)
            .ok_or_else(|| WriteError::Transport("could not mint a Message-ID".into()))?;
        let saved = smtp::with_submission_headers(raw, &self.from, chrono::Utc::now(), &minted);
        let message_id = smtp::first_header(&saved, "Message-ID")
            .and_then(|v| normalize_message_id(&v))
            .ok_or_else(|| WriteError::Invalid("the message has an unusable Message-ID".into()))?;
        let rcpts = smtp::envelope_recipients(&saved);
        if rcpts.is_empty() {
            return Err(WriteError::Invalid("the message has no recipient".into()));
        }
        let wire = smtp::transmitted_copy(&saved);
        smtp::submit(
            &self.smtp,
            self.timeouts,
            &self.from,
            &rcpts,
            &wire,
            send_budget(wire.len()),
        )
        .await
        .map_err(smtp_write_error)?;

        // SENT. Nothing below can fail the send (contract §7).
        let id = stable_id_for_message_id(&message_id);
        let mut folder = LiveSent {
            source: &self.source,
            session: None,
        };
        let copy = look_then_append(&mut folder, &message_id, &saved, &self.look_schedule).await;
        folder.logout().await;
        let warning = match copy {
            Ok(SentCopy::ServerSaved(loc)) | Ok(SentCopy::Appended(Some(loc))) => {
                // The location is a convenience for the echo: a miss here
                // is the next sync's to fill in.
                if self.source.record_own_copy(&id, &saved, loc).is_err() {
                    eprintln!("squelch-api: imap send: could not record the Sent copy's location");
                }
                None
            }
            Ok(SentCopy::Appended(None)) => None,
            Err(e) => {
                // Logged without content: the error is the server's or ours
                // about the APPEND, never the message.
                eprintln!("squelch-api: imap send: sent, but saving it to Sent failed: {e}");
                Some("sent, but the copy in Sent could not be saved".to_string())
            }
        };
        Ok(SentRef {
            id: Some(id),
            thread_id: None,
            warning,
        })
    }

    async fn fetch_raw(&self, msg_id: &str) -> Result<FetchedRaw, WriteError> {
        let (raw, received) = self.verified_raw(msg_id).await?;
        Ok(FetchedRaw {
            raw_b64: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw),
            thread_id: None,
            internal_date: received.map(|t| t.timestamp_millis().to_string()),
        })
    }

    /// An EXACT `Message-ID` hit in Sent, then `\All` (contract §8), as the
    /// stable id ingest gives that Message-ID. A miss is `None`; a folder
    /// that could not be searched is `Err`, never a miss.
    async fn find_by_rfc822_msgid(&self, msgid: &str) -> Result<Option<String>, WriteError> {
        let Some(mid) = normalize_message_id(msgid) else {
            return Err(WriteError::Invalid("not a Message-ID".into()));
        };
        let (mut session, roles) = self.open_session().await?;
        let mut out = Ok(None);
        for folder in [roles.sent.as_deref(), roles.all.as_deref()]
            .into_iter()
            .flatten()
        {
            match exact_message_id_hit(&mut session, folder, &mid).await {
                Ok(Some(_)) => {
                    out = Ok(Some(stable_id_for_message_id(&mid)));
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    out = Err(WriteError::Transport(e));
                    break;
                }
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), session.logout()).await;
        out
    }

    async fn find_scheduled_send(
        &self,
        _: &str,
        _: (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>),
        _: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<Option<String>, WriteError> {
        Err(Self::not_built("find_scheduled_send"))
    }
}

#[cfg(test)]
mod tests;
