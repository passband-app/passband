//! THE IMAP ACCOUNT'S WRITER (issue #575 phase 3): send over SMTP, then make
//! sure Sent holds exactly one copy. Folder moves, flags and the lookups are
//! phase 3 step 2; until then they answer 501.
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

use futures::TryStreamExt;
use squelch_core::mail::imap::{
    ImapCredential, ImapSession, ImapSource, MessageLocation, header_values, normalize_message_id,
    stable_id_for_message_id,
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
        })
    }

    /// Shorter waits, for the tests.
    #[cfg(test)]
    pub(crate) fn with_look_schedule(mut self, schedule: Vec<Duration>) -> Self {
        self.look_schedule = schedule;
        self
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
        // It goes inside a quoted string: refuse anything that could end it.
        if message_id.contains(['"', '\\', '\r', '\n']) || message_id.is_empty() {
            return Ok(None);
        }
        let (session, sent) = self.session().await?;
        let sent = sent.clone();
        let out = tokio::time::timeout(IMAP_STEP, async {
            // EXAMINE afresh on every look: a server shows a session what
            // arrived in its open folder only when the view is re-synced.
            let mbox = session.examine(&sent).await.map_err(|e| e.to_string())?;
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
            // EXACT, NOT A SUBSTRING: `SEARCH HEADER` matches any header
            // containing the text, so each hit is read back.
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
                .collect();
            exact.sort_unstable();
            Ok(exact.first().map(|&uid| MessageLocation {
                folder: sent.clone(),
                uidvalidity,
                uid,
                message_id: Some(message_id.to_string()),
            }))
        })
        .await
        .unwrap_or_else(|_| Err("imap look in Sent timed out".to_string()));
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
    async fn modify(&self, _: &str, _: &[String], _: &[String]) -> Result<(), WriteError> {
        Err(Self::not_built("modify"))
    }

    async fn archive(&self, _: &str) -> Result<(), WriteError> {
        Err(Self::not_built("archive"))
    }

    async fn not_spam(&self, _: &str) -> Result<(), WriteError> {
        Err(Self::not_built("not_spam"))
    }

    async fn trash(&self, _: &str) -> Result<(), WriteError> {
        Err(Self::not_built("trash"))
    }

    async fn parent_headers(&self, _: &str) -> Result<ParentHeaders, WriteError> {
        Err(Self::not_built("parent_headers"))
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

    async fn fetch_raw(&self, _: &str) -> Result<FetchedRaw, WriteError> {
        Err(Self::not_built("fetch_raw"))
    }

    async fn find_by_rfc822_msgid(&self, _: &str) -> Result<Option<String>, WriteError> {
        Err(Self::not_built("find_by_rfc822_msgid"))
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
