//! Mail WRITE operations — the ONLY write capability in squelch.
//!
//! Lives here, not in squelch-core, so the sync/triage core stays provably
//! write-free. The write token is fetched PER REQUEST from a
//! [`CredentialStore`](squelch_core::credentials::CredentialStore) bound to
//! [`CredentialKind::Write`](squelch_core::credentials::CredentialKind::Write),
//! dropped when the call returns, and never reachable from a read path. Tokens,
//! auth headers and bodies are NEVER logged.
//!
//! [`MailWriter`] is the provider seam. It is shaped around INTENT — archive
//! this, trash this, send this — never around one provider's mechanics: Gmail
//! archives by removing a label and Outlook by moving to a folder, and a trait
//! that spoke labels would force the second provider to pretend it has them.
//!
//! What crosses the seam is RFC822 bytes, which is why the MIME half of this
//! module is provider-neutral: every builder in [`gmail`] that assembles a
//! reply or a forward is reused verbatim by any transport that can post raw
//! MIME.

pub mod gmail;

use async_trait::async_trait;

pub use gmail::{
    ForwardAttachment, ForwardParts, ForwardedOriginal, GmailWriteClient, ParentHeaders,
    ReplyParts, ReplyRecipients, build_forward_rfc822, build_references, build_reply_rfc822,
    cc_excluding, default_recipient, derive_reply_recipients, forward_subject,
    parse_forwarded_original, reply_subject,
};

/// An error from a write operation: deliberately coarse and free of any
/// token/body content.
#[derive(Debug)]
pub enum WriteError {
    /// No write credential is configured/stored (run `squelchd auth --write`).
    MissingCredential(String),
    /// The provider's API returned a non-success status.
    Api { status: u16, message: String },
    /// A local/transport failure (network, serialization, credential store).
    Transport(String),
    /// The caller passed invalid input (e.g. empty send body), OR asked for
    /// something this provider cannot express.
    Invalid(String),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::MissingCredential(m) => write!(f, "{m}"),
            WriteError::Api { status, message } => {
                write!(f, "mail api status {status}: {message}")
            }
            WriteError::Transport(m) => write!(f, "{m}"),
            WriteError::Invalid(m) => write!(f, "{m}"),
        }
    }
}

/// What a successful send echoed back.
#[derive(Debug, Clone, Default)]
pub struct SentRef {
    /// The provider's id for the stored copy, when it echoed one. `None` means
    /// the send succeeded but nothing can be fetched back.
    pub id: Option<String>,
    /// The thread the copy landed in, when the provider names one.
    pub thread_id: Option<String>,
}

/// One message's raw bytes as the provider stores them.
#[derive(Debug, Clone, Default)]
pub struct FetchedRaw {
    pub raw_b64: String,
    pub thread_id: Option<String>,
    pub internal_date: Option<String>,
}

/// A provider's write transport.
///
/// Every method here CHANGES the mailbox, which is the whole reason this trait
/// is in squelch-api and not in core: it is reachable only from human-door
/// action handlers, and only with a `Write`-bound credential.
#[async_trait]
pub trait MailWriter: Send + Sync {
    /// Move a message out of the inbox, leaving it findable.
    ///
    /// Gmail removes the INBOX label; Outlook moves it to the Archive folder.
    /// Same intent, and the caller must not have to know which.
    async fn archive(&self, msg_id: &str) -> Result<(), WriteError>;

    /// Move a message to the provider's recoverable trash. NEVER a purge.
    async fn trash(&self, msg_id: &str) -> Result<(), WriteError>;

    /// Add/remove provider labels.
    ///
    /// A GMAIL CONCEPT, exposed because the human door does. A provider with no
    /// label model maps what it can (Outlook: `UNREAD` to the read flag) and
    /// returns [`WriteError::Invalid`] for the rest rather than silently
    /// dropping the request.
    async fn modify(
        &self,
        msg_id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<(), WriteError>;

    /// Send pre-built RFC822 bytes.
    ///
    /// `thread_id` is a HINT only: Gmail takes it to place the copy, while a
    /// provider that threads server-side ignores it and relies on the
    /// In-Reply-To/References headers already in `raw`.
    async fn send(&self, raw: &[u8], thread_id: Option<&str>) -> Result<SentRef, WriteError>;

    /// Fetch one message's raw bytes — the forward path re-reading an original.
    async fn fetch_raw(&self, msg_id: &str) -> Result<FetchedRaw, WriteError>;

    /// The recipient headers of a message being replied to.
    async fn parent_headers(&self, msg_id: &str) -> Result<ParentHeaders, WriteError>;
}
