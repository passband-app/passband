//! WRITE operations against the user's mailbox — the ONLY write capability in
//! squelch. Lives here, not in squelch-core, so the sync/triage core stays
//! provably write-free.
//!
//! The provider-neutral half is this module and [`mime`]: the types the write
//! path speaks and the RFC 822 composition every provider sends. [`gmail`] is
//! the Gmail REST implementation, and the only one today.

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
