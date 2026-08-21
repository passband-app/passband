//! The mail TRANSPORT seam: everything that speaks a provider's wire protocol,
//! and nothing that decides what a message means.
//!
//! [`SyncEngine`](crate::sync::SyncEngine) owns the cadence, the triage passes,
//! the budgets and the cursor's PERSISTENCE. A [`MailSource`] owns listing,
//! delta walking, raw fetching and the cursor's ENCODING. The line sits there
//! rather than at "build a URL" because cursor semantics are the thing that
//! actually differs between providers: Gmail hands out one mailbox-wide
//! historyId, Graph a deltaLink per folder.
//!
//! Ingest below this seam is already provider-blind — it takes RFC822 bytes
//! (see [`crate::sync::ingest`]) — so a source's whole job is to produce those
//! bytes and say which mailbox they came from.

pub mod gmail;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::Result;

/// An OPAQUE, provider-encoded sync cursor.
///
/// Gmail encodes a decimal historyId; Graph encodes its per-folder deltaLinks.
/// The engine round-trips this verbatim through
/// [`Store::set_sync_cursor`](crate::store::Store::set_sync_cursor) and never
/// inspects it — a cursor that means something to the engine would be a cursor
/// the next provider has to lie about.
pub type Cursor = String;

/// One message the provider says is new to us, as cheaply as the provider can
/// say it. Deliberately NOT the message: the engine decides whether to spend a
/// full MIME fetch (see [`crate::store::Store::has_message`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMessageRef {
    /// The provider-native message id, stored as `messages.gmail_msg_id`.
    pub id: String,
    /// Which mailbox surfaced it. Drives the `is_sent` ingest branch, and with
    /// it the neutral/silent treatment the user's own writing gets.
    pub is_sent: bool,
}

/// The result of a listing or a delta walk.
///
/// ORDERING IS PART OF THE CONTRACT: inbox refs come before sent refs. A
/// message that is in both mailboxes must land as its VISIBLE copy, because the
/// store's upsert writes `is_sent = excluded.is_sent` on conflict and a sent
/// copy arriving second would drop the row out of every band.
pub struct PollOutcome {
    /// Refs to ingest, inbox first. May be empty; that is the quiet case, not
    /// an error.
    pub new_messages: Vec<NewMessageRef>,
    /// The cursor to commit AFTER the ingest succeeds. A source that could not
    /// complete its walk returns the cursor it was given, which holds position
    /// so the next tick re-walks the same ground.
    pub cursor: Cursor,
}

/// One message's RFC822 bytes plus the identity the transport knows and the
/// headers do not.
pub struct RawMessage {
    /// The provider's OWN id for this message, which is not always the id it
    /// was asked for: Graph hands back an immutable id when fetched by a
    /// mutable one. Stored as `messages.gmail_msg_id`, so the authoritative
    /// spelling has to be the one that lands.
    pub id: String,
    /// Full RFC822 bytes, already decoded from whatever the wire used.
    pub raw: Vec<u8>,
    /// The provider's own thread key (Gmail `threadId`, Graph
    /// `conversationId`). `None` falls back to a header-derived key.
    pub thread_id: Option<String>,
    /// Date fallback for a message whose `Date` header is missing or unparseable.
    pub internal_date: Option<DateTime<Utc>>,
}

/// One sent message's recipients, for the one-time contacts harvest.
pub struct SentRecipients {
    /// When it was sent, for the contact's recency ranking.
    pub sent_at: Option<DateTime<Utc>>,
    /// `(address, display name)` for every To and Cc mailbox, lower-casing left
    /// to the caller so a source never has to know the contact rules.
    pub mailboxes: Vec<(String, Option<String>)>,
}

/// A provider's read transport. Read-only BY CONSTRUCTION: there is no method
/// here that changes anything in the mailbox, which is what keeps the sync and
/// triage core provably write-free (docs/SECURITY.md §4). Writes live behind
/// squelch-api's own trait, loaded only by human-door action handlers.
#[async_trait]
pub trait MailSource: Send + Sync {
    /// A human-readable provider label for log lines and error text.
    fn provider(&self) -> &'static str;

    /// FIRST RUN, or recovery from a cursor the provider no longer honours:
    /// enumerate `window_days` of both mailboxes AND establish a cursor, in one
    /// motion.
    ///
    /// Both halves together, deliberately. Listing and cursor-taking as
    /// separate steps leaves a gap in which arriving mail belongs to neither,
    /// and the provider is the only party that knows whether its API closes
    /// that gap (Graph's opening delta round does; Gmail's list + profile pair
    /// does not, and its implementation says so).
    async fn initial_listing(&self, window_days: u32) -> Result<PollOutcome>;

    /// One incremental step from `cursor`.
    ///
    /// [`CoreError::NotFound`](crate::error::CoreError::NotFound) means the
    /// CURSOR ITSELF is gone (Gmail 404s an expired historyId; Graph 410s with
    /// `resyncRequired`) and the engine recovers with [`Self::initial_listing`].
    /// Any other failure that leaves the walk incomplete should return the
    /// refs it did gather with the ORIGINAL cursor, holding position.
    async fn poll_changes(&self, cursor: &Cursor) -> Result<PollOutcome>;

    /// Fetch one message's RFC822 bytes.
    async fn fetch_raw(&self, id: &str) -> Result<RawMessage>;

    /// The To/Cc mailboxes of one sent message, WITHOUT its body — the
    /// sent-recipients backfill's per-row read.
    ///
    /// [`CoreError::NotFound`](crate::error::CoreError::NotFound) for a message
    /// the provider no longer has: the caller records the absence rather than
    /// re-queueing it forever.
    async fn fetch_recipients(&self, id: &str) -> Result<Vec<(String, Option<String>)>>;

    /// Every sent message's recipients, over the WHOLE mailbox with no window —
    /// the one-time contacts harvest. Headers only; this walks deep history and
    /// must never pull bodies.
    async fn list_sent_recipients(&self) -> Result<Vec<SentRecipients>>;

    /// Provider-side inbox unread counts as `(messages, threads)`.
    ///
    /// A provider that counts only messages returns the same number twice: the
    /// alternative is inventing a thread count, and a wrong number here is
    /// worse than a coarse one.
    async fn unread_counts(&self) -> Result<(i64, i64)>;
}
