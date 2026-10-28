//! WRITE operations against the user's mailbox — the ONLY write capability in
//! squelch. Lives here, not in squelch-core, so the sync/triage core stays
//! provably write-free.
//!
//! The provider-neutral half is this module and [`mime`]: the [`MailWriter`]
//! seam every action calls through, the types it speaks, and the RFC 822
//! composition every provider sends. [`gmail`] is the Gmail REST
//! implementation, and the only one today; which writer an account gets is
//! decided by its `accounts.provider` row (`handlers::write_client`).

pub mod gmail;
pub mod mime;

/// An error from a write operation: deliberately coarse and free of any
/// token/body content.
#[derive(Debug)]
pub enum WriteError {
    /// No write credential is configured/stored (run `squelchd auth --write`).
    MissingCredential(String),
    /// The Gmail API returned a non-success status.
    Api { status: u16, message: String },
    /// A local/transport failure (network, serialization, credential store).
    Transport(String),
    /// The caller passed invalid input (e.g. empty send body).
    Invalid(String),
    /// The mailbox's Gmail budget is holding every call after a quota refusal,
    /// longer than a human's request should wait. Nothing was sent; try again
    /// in about this many seconds.
    Paced { retry_in_secs: u64 },
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::MissingCredential(m) => write!(f, "{m}"),
            WriteError::Api { status, message } => {
                write!(f, "gmail api status {status}: {message}")
            }
            WriteError::Transport(m) => write!(f, "{m}"),
            WriteError::Invalid(m) => write!(f, "{m}"),
            WriteError::Paced { retry_in_secs } => write!(
                f,
                "Gmail is pacing this mailbox; try again in {retry_in_secs}s"
            ),
        }
    }
}

/// How long a `messages.send` may take: the client's ordinary minute, plus a
/// second for every 50 KB of the message — a 25 MB attachment is ~34 MB on the
/// wire, and 50 KB/s is a slow uplink, not a broken one. The Swift client
/// sizes its own wait the same way (`APIClient.actionSend`), so neither side
/// gives up on a send the other is still finishing.
pub fn send_budget(raw_len: usize) -> std::time::Duration {
    std::time::Duration::from_secs(60 + (raw_len / 50_000) as u64)
}

/// Gmail metadata headers we read (with the WRITE token) to thread a reply and
/// to derive who it goes to.
///
/// The address fields hold the RAW header values Gmail returned — display names,
/// groups, folding and all. NOTHING here reaches the wire: every recipient is
/// re-derived as a bare address by [`derive_reply_recipients`], which is what
/// keeps an attacker-authored display name from becoming a header.
#[derive(Debug, Default, Clone)]
pub struct ParentHeaders {
    pub message_id: Option<String>,
    pub references: Option<String>,
    pub reply_to: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub cc: Option<String>,
}

/// What `messages.send` echoed back about the message it created. Parsed
/// leniently — both fields `None` on a `{}` response — because the send has
/// ALREADY succeeded by the time this is built: a missing id costs us the local
/// echo, never the request.
#[derive(Debug, Default, Clone)]
pub struct SentRef {
    pub id: Option<String>,
    pub thread_id: Option<String>,
}

/// One `format=raw` message as Gmail returns it: the base64url RFC822 body plus
/// the ids/date it carries alongside. Decoding is the caller's job (core owns
/// `decode_raw_b64url`).
#[derive(Debug, Clone)]
pub struct FetchedRaw {
    pub raw_b64: String,
    pub thread_id: Option<String>,
    /// Milliseconds since epoch as a decimal string (Gmail's `internalDate`).
    pub internal_date: Option<String>,
}

/// What the write door asks of a mailbox, whichever provider holds it. Every
/// action route, the scheduled-send worker and the group fan-out reach the
/// mailbox through this and nothing else; [`gmail::GmailWriteClient`] is the
/// one implementation today.
///
/// Message ids are the provider's own (`messages.gmail_msg_id` for a Gmail
/// account), opaque to the caller.
///
/// WHAT IS STILL GMAIL-SHAPED, deliberately, until a second provider needs it
/// otherwise: [`Self::modify`] takes label ids, [`FetchedRaw`] carries the raw
/// message base64url-encoded the way Gmail returns it, and a send's
/// `thread_id` is a provider thread id. [`Self::find_scheduled_send`]'s window
/// is sized by the caller with Gmail's clock-skew slack
/// ([`gmail::SCAN_SLACK`]).
#[async_trait::async_trait]
pub trait MailWriter: Send + Sync {
    /// Add and remove labels (provider label ids) on one message.
    async fn modify(
        &self,
        msg_id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<(), WriteError>;

    /// Take a message out of the inbox.
    async fn archive(&self, msg_id: &str) -> Result<(), WriteError>;

    /// Move a message to the trash: recoverable, never a permanent delete.
    async fn trash(&self, msg_id: &str) -> Result<(), WriteError>;

    /// The threading and recipient headers of a message being replied to.
    async fn parent_headers(&self, msg_id: &str) -> Result<ParentHeaders, WriteError>;

    /// Send a raw RFC 822 message, threaded onto `thread_id` when there is one.
    async fn send(&self, raw: &[u8], thread_id: Option<&str>) -> Result<SentRef, WriteError>;

    /// One message's raw RFC 822 bytes (see [`FetchedRaw`] for the encoding).
    async fn fetch_raw(&self, msg_id: &str) -> Result<FetchedRaw, WriteError>;

    /// The id of the message whose `Message-ID` header is `msgid`, or `None`
    /// when the mailbox has none. A hit is proof; a miss is not.
    async fn find_by_rfc822_msgid(&self, msgid: &str) -> Result<Option<String>, WriteError>;

    /// The id of the fired scheduled send whose minted `Message-ID` is
    /// `message_id`, looking inside `window`, or `None` when the mailbox holds
    /// no such mail. Any doubt is `Err`, never `None`: a wrong `None` is a
    /// double send. `stopping` is the daemon's shutdown, checked between
    /// requests.
    async fn find_scheduled_send(
        &self,
        message_id: &str,
        window: (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>),
        stopping: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<Option<String>, WriteError>;
}
