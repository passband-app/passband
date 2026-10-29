//! THE IMAP ACCOUNT'S WRITER (issue #575 phase 3): send over SMTP, then make
//! sure Sent holds exactly one copy; folder moves and flags ([`ops`]) on the
//! copies the source verified as the id's own; and the lookups through the
//! source's guarded resolve.
//!
//! EVERY ERROR TEXT IS OURS. The door shows an IMAP account's write errors
//! as they are (amendment A4), so nothing here carries a server's reply
//! text or a credential: SMTP failures carry at most a reply code, IMAP
//! failures a fixed line, and the detail is logged without content.
//!
//! A MAYBE-SENT MESSAGE IS NEVER SENT AGAIN (amendment A3). Gmail files a
//! sent message in Sent as part of sending it, so the scheduled-send
//! recovery reads "not in Sent" as "not sent" and fires again. SMTP has no
//! such step: the copy in Sent is the writer's own APPEND, after the fact,
//! and a crash or an ambiguous answer between SMTP and that APPEND leaves
//! no copy of a message that went. So a fired scheduled send (the one kind
//! of send anything re-fires) is written to an [`OutboxLedger`] BEFORE it is
//! submitted, and [`ImapWriter::find_scheduled_send`] reads a marker it
//! cannot rule out as doubt, never as "not sent".
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

use super::mime::{OUTBOX_HEADER, outbox_token};
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

/// THE WHOLE OF ONE SEND, SMTP and the Sent copy together (security review
/// F3). The route it runs under is cut at
/// [`crate::limits::LONG_REQUEST_TIMEOUT`] (20 min), and a send cut there
/// after SMTP accepted it never reaches its audit or its echo. 17 min leaves
/// 3 for what the route does around the writer: the slot read, a reply's
/// parent headers ([`PARENT_HEADERS_BOUND`], 1 min), and the store writes and
/// echo after it (each store call can wait SQLite's 5 s busy handler; the
/// echo ingests the bytes in hand, no fetch). SMTP gets all but
/// [`SENT_RESERVE`] of it, which clears the largest message's own budget
/// (`send_budget(MAX_SEND_RAW_BYTES)`, about 13.7 min) with 2 min for the
/// connection, the login and the envelope (a test pins both).
pub(crate) const SEND_DEADLINE: Duration = Duration::from_secs(17 * 60);

/// What SMTP leaves of [`SEND_DEADLINE`] for the Sent copy. A copy that does
/// not fit is the warning, never a failed send: the mail went.
pub(crate) const SENT_RESERVE: Duration = Duration::from_secs(60);

/// The most a reply's parent headers may take (one login, one verified
/// copy read).
pub(crate) const PARENT_HEADERS_BOUND: Duration = Duration::from_secs(60);

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

/// Where a fired scheduled send stands with SMTP, kept durably by its outbox
/// token so a crash cannot lose it (amendment A3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboxMark {
    /// About to be, or being, submitted: SMTP may have taken it. Written
    /// BEFORE the connection opens; still here after a crash or an
    /// ambiguous answer, it is doubt.
    Submitting { message_id: String },
    /// SMTP accepted it: sent, whether or not Sent has a copy yet.
    Accepted { message_id: String },
}

impl OutboxMark {
    fn encode(&self) -> String {
        match self {
            OutboxMark::Submitting { message_id } => format!("submitting {message_id}"),
            OutboxMark::Accepted { message_id } => format!("accepted {message_id}"),
        }
    }

    /// An unreadable value is a `Submitting` with no id: still doubt.
    fn decode(text: &str) -> Self {
        match text.split_once(' ') {
            Some(("accepted", mid)) if !mid.is_empty() => OutboxMark::Accepted {
                message_id: mid.to_string(),
            },
            Some((_, mid)) => OutboxMark::Submitting {
                message_id: mid.to_string(),
            },
            None => OutboxMark::Submitting {
                message_id: String::new(),
            },
        }
    }
}

/// THE DURABLE MAYBE-SENT MARKERS, by outbox token. Any failure is `Err`,
/// which every caller reads as "could not rule it out".
pub trait OutboxLedger: Send + Sync {
    fn get(&self, token: &str) -> Result<Option<OutboxMark>, String>;
    fn set(&self, token: &str, mark: &OutboxMark) -> Result<(), String>;
    fn clear(&self, token: &str) -> Result<(), String>;
}

/// The app-settings key a token's marker is kept under.
const OUTBOX_KEY_PREFIX: &str = "imap_outbox:";

/// The daemon's [`OutboxLedger`]: one `app_settings` row per send in doubt,
/// in the account's own store, removed once the copy in Sent is seen.
pub struct StoreOutbox {
    pub store: Arc<squelch_core::store::SqliteStore>,
    pub account_id: squelch_core::types::AccountId,
}

impl OutboxLedger for StoreOutbox {
    fn get(&self, token: &str) -> Result<Option<OutboxMark>, String> {
        use squelch_core::store::Store as _;
        self.store
            .get_app_setting(self.account_id, &format!("{OUTBOX_KEY_PREFIX}{token}"))
            .map(|v| v.map(|t| OutboxMark::decode(&t)))
            .map_err(|_| "outbox ledger read failed".to_string())
    }

    fn set(&self, token: &str, mark: &OutboxMark) -> Result<(), String> {
        use squelch_core::store::Store as _;
        self.store
            .set_app_setting(
                self.account_id,
                &format!("{OUTBOX_KEY_PREFIX}{token}"),
                &mark.encode(),
            )
            .map_err(|_| "outbox ledger write failed".to_string())
    }

    fn clear(&self, token: &str) -> Result<(), String> {
        use squelch_core::store::Store as _;
        self.store
            .delete_app_setting(self.account_id, &format!("{OUTBOX_KEY_PREFIX}{token}"))
            .map_err(|_| "outbox ledger write failed".to_string())
    }
}

/// What a scheduled send in doubt is answered with: an `Err`, so the worker
/// defers and in the end fails the row, and never fires it again.
pub const MAY_HAVE_BEEN_SENT: &str = "this message may already have been sent (the send was \
     interrupted after the mail server took it), so Passband will not send it again; check Sent";

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
    /// The maybe-sent markers. `None` (a writer built without one) is safe,
    /// not lax: a scheduled send it cannot find in Sent is doubt.
    outbox: Option<Arc<dyn OutboxLedger>>,
    /// Tests only: stop right after SMTP accepted, before Sent, as a crash
    /// there would.
    #[cfg(test)]
    crash_after_smtp: bool,
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
            outbox: None,
            #[cfg(test)]
            crash_after_smtp: false,
        })
    }

    /// Keep scheduled sends' maybe-sent markers in `ledger` (the daemon's is
    /// [`StoreOutbox`]).
    pub fn with_outbox(mut self, ledger: Arc<dyn OutboxLedger>) -> Self {
        self.outbox = Some(ledger);
        self
    }

    /// Stop after SMTP accepted (and the marker said so), before Sent, as a
    /// crash there would, for the tests.
    #[cfg(test)]
    pub(crate) fn crash_after_smtp(mut self) -> Self {
        self.crash_after_smtp = true;
        self
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
        e => {
            // Core's text can quote the server; it is logged, not shown.
            eprintln!("squelch-api: imap write: {e}");
            WriteError::Transport(IMAP_UNREACHABLE.into())
        }
    }
}

/// What a write that could not reach or use the IMAP server is told.
pub const IMAP_UNREACHABLE: &str = "the mail server could not be reached; try again";

/// What a lookup that could not search the server is told.
const IMAP_SEARCH_FAILED: &str = "the mail server could not be searched; try again";

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
    exact_header_hit(session, folder, Exact::MessageId(message_id)).await
}

/// What [`exact_header_hit`] looks for.
#[derive(Clone, Copy)]
pub(crate) enum Exact<'a> {
    /// A `Message-ID`, normalized and bare.
    MessageId(&'a str),
    /// An [`OUTBOX_HEADER`] value, exact after trimming.
    Outbox(&'a str),
}

impl Exact<'_> {
    fn header(self) -> &'static str {
        match self {
            Exact::MessageId(_) => "Message-ID",
            Exact::Outbox(_) => OUTBOX_HEADER,
        }
    }

    fn text(&self) -> &str {
        match self {
            Exact::MessageId(v) | Exact::Outbox(v) => v,
        }
    }

    /// Is this one header block's field exactly the one looked for?
    fn matches(&self, head: &[u8]) -> bool {
        match self {
            Exact::MessageId(want) => header_values(head, "Message-ID")
                .into_iter()
                .find_map(|v| normalize_message_id(&v))
                .is_some_and(|m| m == *want),
            Exact::Outbox(want) => header_values(head, OUTBOX_HEADER)
                .into_iter()
                .any(|v| v.trim() == *want),
        }
    }
}

/// The lowest-UID copy in `folder` carrying `want` EXACTLY (see
/// [`exact_message_id_hit`]), with its own `Message-ID` as the location's.
pub(crate) async fn exact_header_hit(
    session: &mut ImapSession,
    folder: &str,
    want: Exact<'_>,
) -> Result<Option<MessageLocation>, String> {
    let text = want.text();
    // It goes inside a quoted string: refuse anything that could end it.
    if text.contains(['"', '\\', '\r', '\n']) || text.is_empty() {
        return Ok(None);
    }
    let header = want.header();
    tokio::time::timeout(IMAP_STEP, async {
        let mbox = match session.examine(folder).await {
            Ok(m) => m,
            Err(async_imap::error::Error::No(_)) => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let uidvalidity = mbox.uid_validity.ok_or("no UIDVALIDITY")?;
        let hits = session
            .uid_search(format!("HEADER {header} \"{text}\""))
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
        let fields = match want {
            Exact::MessageId(_) => "MESSAGE-ID".to_string(),
            Exact::Outbox(_) => format!("MESSAGE-ID {}", OUTBOX_HEADER.to_ascii_uppercase()),
        };
        let fetches: Vec<_> = session
            .uid_fetch(set, format!("(UID BODY.PEEK[HEADER.FIELDS ({fields})])"))
            .await
            .map_err(|e| e.to_string())?
            .try_collect()
            .await
            .map_err(|e| e.to_string())?;
        let mut exact: Vec<(u32, Option<String>)> = fetches
            .iter()
            .filter_map(|f| {
                let head = f.header()?;
                let uid = f.uid.filter(|u| uids.contains(u))?;
                want.matches(head).then(|| {
                    let mid = header_values(head, "Message-ID")
                        .into_iter()
                        .find_map(|v| normalize_message_id(&v));
                    (uid, mid)
                })
            })
            .collect();
        exact.sort_unstable();
        Ok(exact
            .into_iter()
            .next()
            .map(|(uid, message_id)| MessageLocation {
                folder: folder.to_string(),
                uidvalidity,
                uid,
                message_id,
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
    ///
    /// ONE copy is read (correctness review F2): the first that passes the
    /// guard, not every location, so a reply to a message with a large
    /// attachment in INBOX and Sent downloads it once, within
    /// [`PARENT_HEADERS_BOUND`].
    async fn parent_headers(&self, msg_id: &str) -> Result<ParentHeaders, WriteError> {
        let read = tokio::time::timeout(PARENT_HEADERS_BOUND, self.source.verified_copy(msg_id))
            .await
            .map_err(|_| WriteError::Transport(IMAP_UNREACHABLE.into()))?;
        match read {
            Ok(Some(raw)) => Ok(parent_headers_of(&raw)),
            Ok(None) | Err(CoreError::NotFound) => Err(not_found()),
            Err(e) => Err(core_write_error(e)),
        }
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
        let deadline = tokio::time::Instant::now() + SEND_DEADLINE;
        // ONE BUFFER is saved and (less its Bcc) transmitted: CRLF-only and
        // safe for any relay, so the delivered copy and the saved one are the
        // same bytes and print as one message.
        let saved = smtp::transfer_safe(&smtp::with_submission_headers(
            raw,
            &self.from,
            chrono::Utc::now(),
            &minted,
        ));
        let message_id = smtp::first_header(&saved, "Message-ID")
            .and_then(|v| normalize_message_id(&v))
            .ok_or_else(|| WriteError::Invalid("the message has an unusable Message-ID".into()))?;
        let rcpts = smtp::envelope_recipients(&saved);
        if rcpts.is_empty() {
            return Err(WriteError::Invalid("the message has no recipient".into()));
        }
        let wire = smtp::transmitted_copy(&saved);

        // A FIRED SCHEDULED SEND IS MARKED BEFORE IT GOES (amendment A3): a
        // crash from here on leaves a marker the recovery reads as doubt. No
        // marker, no send: a mark that cannot be written is a send refused
        // with nothing said to SMTP.
        let token = smtp::first_header(&saved, OUTBOX_HEADER)
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        if let (Some(token), Some(ledger)) = (&token, &self.outbox) {
            let mark = OutboxMark::Submitting {
                message_id: message_id.clone(),
            };
            if let Err(e) = ledger.set(token, &mark) {
                eprintln!("squelch-api: imap send: {e}; not sent");
                return Err(WriteError::Api {
                    status: 503,
                    message: "could not record the send before it went; nothing was sent".into(),
                });
            }
        }
        let submitted = smtp::submit_until(
            &self.smtp,
            self.timeouts,
            &self.from,
            &rcpts,
            &wire,
            send_budget(wire.len()),
            Some(deadline - SENT_RESERVE),
        )
        .await;
        if let (Some(token), Some(ledger)) = (&token, &self.outbox) {
            let after = match &submitted {
                Ok(()) => ledger.set(
                    token,
                    &OutboxMark::Accepted {
                        message_id: message_id.clone(),
                    },
                ),
                // Doubt stays doubt: the marker stands.
                Err(SmtpError::MaybeSent(_)) => Ok(()),
                // Nothing went: a later fire may send it.
                Err(_) => ledger.clear(token),
            };
            if let Err(e) = after {
                // The marker left behind is `Submitting`: doubt, the safe side.
                eprintln!("squelch-api: imap send: {e}");
            }
        }
        submitted.map_err(smtp_write_error)?;
        #[cfg(test)]
        if self.crash_after_smtp {
            return Err(WriteError::Transport("test: crashed after SMTP".into()));
        }

        // SENT. Nothing below can fail the send (contract §7).
        let id = stable_id_for_message_id(&message_id);
        let mut folder = LiveSent {
            source: &self.source,
            session: None,
        };
        let copy = tokio::time::timeout_at(
            deadline,
            look_then_append(&mut folder, &message_id, &saved, &self.look_schedule),
        )
        .await
        .unwrap_or_else(|_| Err("the Sent copy ran out of time".to_string()));
        folder.logout().await;
        let warning = match copy {
            Ok(SentCopy::ServerSaved(loc)) | Ok(SentCopy::Appended(Some(loc))) => {
                // Sent has it: the search answers from now on.
                if let (Some(token), Some(ledger)) = (&token, &self.outbox)
                    && ledger.clear(token).is_err()
                {
                    eprintln!("squelch-api: imap send: an outbox marker was not cleared");
                }
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
            // The echo ingests what was saved, Bcc and all (review 1b): the
            // copy a sync would read first is the delivered one, Bcc-less.
            raw: Some(saved),
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
                    eprintln!("squelch-api: imap lookup: {e}");
                    out = Err(WriteError::Transport(IMAP_SEARCH_FAILED.into()));
                    break;
                }
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), session.logout()).await;
        out
    }

    /// THE RECOVERY QUESTION (contract §8, amendment A3): did this fired
    /// scheduled send go? Sent is searched for its exact `Message-ID`, then
    /// for its [`OUTBOX_HEADER`] token, each hit read back. A hit is the
    /// answer, and `window` is not needed: the search is the server's, exact
    /// and whole-folder.
    ///
    /// NO HIT IS NOT YET "NOT SENT". The outbox marker the send wrote before
    /// SMTP decides: none means it never reached SMTP (or SMTP refused it),
    /// and only then is the answer `None`. `Accepted` is sent (the APPEND
    /// never happened): its id. `Submitting` is doubt, and so is a ledger
    /// that cannot be read or a writer with none: [`MAY_HAVE_BEEN_SENT`],
    /// an `Err`, which the worker defers and in the end fails, never fires.
    async fn find_scheduled_send(
        &self,
        message_id: &str,
        _window: (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>),
        stopping: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<Option<String>, WriteError> {
        let token = outbox_token(message_id);
        let Some(mid) = normalize_message_id(message_id).filter(|_| !token.is_empty()) else {
            return Err(WriteError::Invalid("no outbox token to search for".into()));
        };
        let (mut session, roles) = self.open_session().await?;
        let hit = match roles.sent.as_deref() {
            Some(sent) => {
                let mut out = Ok(None);
                for want in [Exact::MessageId(&mid), Exact::Outbox(&token)] {
                    if stopping() {
                        out = Err("cut short by shutdown".to_string());
                        break;
                    }
                    out = exact_header_hit(&mut session, sent, want).await;
                    if !matches!(out, Ok(None)) {
                        break;
                    }
                }
                out
            }
            // No Sent to look in: only the marker can answer.
            None => Ok(None),
        };
        let _ = tokio::time::timeout(Duration::from_secs(5), session.logout()).await;
        let hit = hit.map_err(|e| {
            eprintln!("squelch-api: imap scheduled-send lookup: {e}");
            WriteError::Transport(IMAP_SEARCH_FAILED.into())
        })?;
        outbox_answer(hit, &mid, &token, self.outbox.as_deref())
    }
}

/// THE DECISION after the Sent search (see
/// [`ImapWriter::find_scheduled_send`]): a hit is the answer (and its
/// marker is no longer needed); with none, only an absent marker is "not
/// sent". Every doubt is `Err`.
pub(crate) fn outbox_answer(
    hit: Option<MessageLocation>,
    mid: &str,
    token: &str,
    ledger: Option<&dyn OutboxLedger>,
) -> Result<Option<String>, WriteError> {
    if let Some(loc) = hit {
        if let Some(ledger) = ledger
            && ledger.clear(token).is_err()
        {
            eprintln!("squelch-api: imap: an outbox marker was not cleared");
        }
        let found = loc.message_id.unwrap_or_else(|| mid.to_string());
        return Ok(Some(stable_id_for_message_id(&found)));
    }
    let doubt = || WriteError::Transport(MAY_HAVE_BEEN_SENT.into());
    let Some(ledger) = ledger else {
        return Err(doubt());
    };
    match ledger.get(token) {
        Ok(None) => Ok(None),
        Ok(Some(OutboxMark::Accepted { message_id })) => {
            let id = normalize_message_id(&message_id).unwrap_or_else(|| mid.to_string());
            Ok(Some(stable_id_for_message_id(&id)))
        }
        Ok(Some(OutboxMark::Submitting { .. })) => Err(doubt()),
        Err(e) => {
            eprintln!("squelch-api: imap scheduled-send lookup: {e}");
            Err(doubt())
        }
    }
}

#[cfg(test)]
mod tests;
