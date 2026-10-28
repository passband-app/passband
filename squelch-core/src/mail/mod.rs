//! THE MAIL PROVIDER SEAM: what the sync engine asks of a mailbox, with no
//! provider's vocabulary in it.
//!
//! The engine keeps cadence, ingest, triage and cursor PERSISTENCE. A
//! [`MailSource`] owns listing, the change feed, fetching and cursor
//! ENCODING: the engine stores whatever [`SyncCursor`] a source hands it and
//! never looks inside. Gmail ([`gmail::GmailSource`]) is the only source
//! today, and it is what every account is unless its `accounts.provider` row
//! says otherwise ([`MailProvider`]).
//!
//! What stays Gmail-specific on purpose, and why, is in `docs/MAIL-PROVIDERS.md`.

pub mod gmail;
pub mod graph;

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::error::{CoreError, Result};

/// Which kind of mailbox an account is: the `accounts.provider` column, read
/// by the sync engine and the write door to pick a [`MailSource`] and a writer.
///
/// An enum and not the string, so an unknown value is an error at the one
/// place it is read rather than a default somewhere downstream: a database
/// written by a newer build (an `imap` account) and opened by this one must
/// refuse that account, never sync it as Gmail.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum MailProvider {
    /// Google's Gmail REST API, over OAuth.
    #[default]
    Gmail,
    /// Microsoft's Graph mail API (Outlook.com, Hotmail, Microsoft 365), over
    /// OAuth: [`graph::GraphSource`].
    Outlook,
}

impl MailProvider {
    /// The value stored in `accounts.provider`.
    pub fn as_str(self) -> &'static str {
        match self {
            MailProvider::Gmail => "gmail",
            MailProvider::Outlook => "outlook",
        }
    }

    /// The provider a stored `accounts.provider` value names. Exact match
    /// only: the column is written by this code, never by a person.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "gmail" => Ok(MailProvider::Gmail),
            "outlook" => Ok(MailProvider::Outlook),
            // The value is ours (a column default or a build's own write), so
            // echoing it is safe and is what makes the log line useful.
            other => Err(CoreError::InvalidInput(format!(
                "mail provider {other:?} is not supported by this build"
            ))),
        }
    }
}

impl std::fmt::Display for MailProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A source's position in its mailbox's change feed, OPAQUE to everything but
/// the source that minted it. Gmail's is a decimal `historyId`; an IMAP
/// source's would carry `UIDVALIDITY` and a `UID` per folder. The engine
/// persists it ([`crate::store::Store::set_sync_cursor`]) and hands it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncCursor(String);

impl SyncCursor {
    pub fn new(encoded: impl Into<String>) -> Self {
        Self(encoded.into())
    }

    /// The encoded form, for the store and for the source that decodes it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which mail a listing covers, in the provider's own terms once the source
/// has translated it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Folder {
    /// Received mail the provider files in the inbox.
    Inbox,
    /// The user's own sent mail.
    Sent,
    /// The provider's spam verdict. Never walked by the poll loop.
    Spam,
}

/// What one listing asks for. Built by the engine, translated by the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listing {
    /// Mail in `folder`, newer than `days` days if given, else all of it.
    Folder {
        folder: Folder,
        within_days: Option<u32>,
    },
    /// THE ARCHIVE WALK: all mail received in `[from, to)`, read or filed,
    /// received or sent, minus drafts and chats; with `filed_only`, minus
    /// anything still in the inbox or the sent folder too (the live paths'
    /// mail, see the engine's forward pass). The source owns any rounding the
    /// provider needs at the edges.
    Window {
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        filed_only: bool,
    },
}

/// How a request should behave under the provider's rate limits, which is a
/// question about who is waiting for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    /// The poll lane, the sweeps, an on-demand fetch: one attempt, and the
    /// caller's own loop schedules the retry.
    Live,
    /// A window somebody is waiting on (a backfill, a catch-up): a rate-limit
    /// refusal is waited out IN PLACE, so position in the window is kept.
    Window,
    /// The archive walk: one attempt like `Live`, and its refusals are counted
    /// for the walk ([`SourceRefusals`]) instead of being shown to the user as
    /// a sync error.
    Background,
}

/// Rate-limit and credential refusals a source has met on [`Urgency::
/// Background`] requests, ever. Shared between the source that counts and the
/// engine's archive walk that reads them before and after its own work.
#[derive(Debug, Default)]
pub struct SourceRefusals {
    pub quota: std::sync::atomic::AtomicU64,
    pub auth: std::sync::atomic::AtomicU64,
}

/// Where the provider says a fetched message is filed, as of the fetch. A
/// provider that reports nothing leaves both `false`, which hands the decision
/// back to the folder the batch was listed from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Placement {
    pub in_inbox: bool,
    pub in_sent: bool,
}

/// One message's raw RFC 822 bytes, or why there are none.
#[derive(Debug)]
pub enum RawBody {
    Bytes(Vec<u8>),
    /// The provider answered without a body.
    Missing,
    /// The provider's body could not be decoded. The text is the decoder's
    /// error, logged as it always was: a failure kind and an offset, and at
    /// most the one offending input byte, never the message.
    Undecodable(String),
}

/// One fetched message.
#[derive(Debug)]
pub struct FetchedMessage {
    /// The provider's id for the message, as listed. Stored in
    /// `messages.gmail_msg_id` whatever the provider.
    pub id: String,
    /// The provider's thread id, when it has one.
    pub thread_id: Option<String>,
    pub raw: RawBody,
    /// When the provider says it received the message.
    pub received_at: Option<DateTime<Utc>>,
    pub placement: Placement,
}

/// One message's headers, without its body: what the contacts harvest and the
/// recipients backfill read.
#[derive(Debug, Default)]
pub struct FetchedHeaders {
    /// `(name, value)` pairs, in the order the provider gave them. Values may
    /// repeat a name.
    pub headers: Vec<(String, String)>,
    pub received_at: Option<DateTime<Utc>>,
}

/// One folder's changes since a cursor ([`MailSource::changes`]).
#[derive(Debug)]
pub struct Changes {
    /// Ids added to the folder, deduplicated, in no particular order.
    pub ids: Vec<String>,
    /// The cursor after this walk: the start cursor with THIS folder's
    /// progress applied. Combine walks of several folders from one start with
    /// [`MailSource::merge_cursors`].
    pub cursor: SyncCursor,
    /// The source can prove NOTHING in the whole mailbox changed since the
    /// start cursor, in any folder: the engine may then skip its other walks.
    /// `false` is "changed, or cannot tell".
    pub mailbox_unchanged: bool,
    /// The round reported ANY change to the folder, removals included (mail
    /// deleted or moved out, which names no id to fetch but can move the
    /// unread count). Read only for a source whose feed names every change
    /// ([`MailSource::changes_report_updates`]): two rounds with this
    /// `false` are an idle mailbox.
    pub saw_changes: bool,
}

/// Why a [`MailSource::changes`] walk could not run.
#[derive(Debug)]
pub enum ChangesError {
    /// The cursor is too old for the provider to replay from: only a fresh
    /// catch-up over the window recovers.
    Expired,
    Other(CoreError),
}

impl From<CoreError> for ChangesError {
    fn from(e: CoreError) -> Self {
        // A provider's 404 on a change feed IS the expired cursor.
        match e {
            CoreError::NotFound => ChangesError::Expired,
            other => ChangesError::Other(other),
        }
    }
}

/// THE READ SIDE OF A MAILBOX: everything the sync engine asks of a provider.
///
/// Every method is one provider conversation the engine already has with
/// Gmail; none of them persists anything or decides policy (which folders,
/// in what order, what to ingest). Errors follow the crate's conventions:
/// [`CoreError::NotFound`] for a message the provider no longer has,
/// [`CoreError::Credential`] for a credential that cannot be refreshed.
/// Tokens, headers and bodies are never logged by an implementation.
#[async_trait::async_trait]
pub trait MailSource: Send + Sync {
    /// Which provider this is, for logs and dispatch checks.
    fn provider(&self) -> MailProvider;

    /// The ids `listing` covers, newest first where the provider orders them,
    /// paging until done or until `cap` ids are in hand.
    async fn list(
        &self,
        listing: Listing,
        cap: Option<usize>,
        urgency: Urgency,
    ) -> Result<Vec<String>>;

    /// The cursor for the mailbox as it stands now: where a fresh change feed
    /// starts after a window has been listed.
    async fn current_cursor(&self) -> Result<SyncCursor>;

    /// Is this a cursor the change feed can start from at all? `false` (absent
    /// position, a zero, a value the source cannot decode) sends the engine to
    /// a catch-up instead of a walk.
    fn cursor_is_usable(&self, cursor: &SyncCursor) -> bool;

    /// What was added to `folder` since `since`.
    async fn changes(
        &self,
        since: &SyncCursor,
        folder: Folder,
    ) -> std::result::Result<Changes, ChangesError>;

    /// The cursor covering two walks (`a`, `b`) that both started from
    /// `since`, never behind either.
    ///
    /// `since` is what makes this possible for a provider whose positions
    /// cannot be ordered: Outlook's per-folder delta links are opaque URLs, so
    /// the only way to tell which part of `a` is new is that it differs from
    /// `since`. Gmail's historyIds are ordered and need only the max.
    fn merge_cursors(&self, since: &SyncCursor, a: &SyncCursor, b: &SyncCursor) -> SyncCursor;

    /// One message's raw RFC 822 bytes.
    async fn fetch_raw(&self, id: &str, urgency: Urgency) -> Result<FetchedMessage>;

    /// The named headers of one message, without its body.
    async fn fetch_headers(&self, id: &str, names: &[&str]) -> Result<FetchedHeaders>;

    /// The provider's own (messages, threads) unread counts for the inbox.
    async fn inbox_unread(&self) -> Result<(i64, i64)>;

    /// Does [`Self::changes`] report mail that CHANGED, not only mail that
    /// arrived? Gmail's `messageAdded` history names a message once, when it
    /// lands. Graph's delta names it again on every read-state flip, flag or
    /// category edit, so on an account somebody reads elsewhere most of a
    /// poll's ids are mail already stored.
    ///
    /// `true` makes the engine skip ids it already holds
    /// ([`crate::store::Store::has_message`]) on the INCREMENTAL path only,
    /// before any body is fetched. Never on a backfill or a catch-up: those
    /// re-fetch stored mail on purpose, because that is how a sanitizer fix
    /// reaches `body_html`, which is baked at ingest.
    fn changes_report_updates(&self) -> bool {
        false
    }

    /// Does [`Self::current_cursor`] describe a position by TIME rather than
    /// by asking the server where its feed stands? Graph's mints the initial
    /// delta round from a timestamp, with no request; minted after a
    /// backfill that ran for an hour, it would start an hour late and the
    /// mail in between would never sync. `true` makes the engine mint the
    /// cursor BEFORE a backfill or catch-up lists its window and store it
    /// after, so the first walk replays everything since the listing began
    /// (stored ids are skipped). Gmail asks the server and keeps its order.
    fn cursor_before_window(&self) -> bool {
        false
    }

    /// PARK UNTIL THE MAILBOX MAY HAVE CHANGED, or `max_wait` passes, whichever
    /// is first. The poll lane calls this between ticks, inside the same
    /// `select!` as the manual refresh and shutdown, so a source that can be
    /// told about changes (IMAP IDLE) wakes the next tick early and one that
    /// cannot simply waits out the interval.
    ///
    /// `cursor` is the last one the engine committed, `None` before the first:
    /// a pushing source compares it with the server's state before it parks,
    /// so mail that landed between the last tick and the park is not missed
    /// for a whole `max_wait`.
    ///
    /// The default sleeps, which is exactly the polling the engine has always
    /// done. Returning early is always safe: the next tick asks
    /// [`Self::changes`] and finds out.
    ///
    /// IT MUST BE CANCEL-SAFE. The poll lane drops this future whenever a
    /// manual refresh, a spam-page request or shutdown wins the `select!`, so
    /// an implementation that holds a connection in IDLE has to leave it
    /// reusable (or cleanly closed) at every await point, never half-way
    /// through a command.
    async fn wait_for_change(&self, cursor: Option<&SyncCursor>, max_wait: Duration) {
        let _ = cursor;
        tokio::time::sleep(max_wait).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_round_trips_through_its_stored_value() {
        for p in [MailProvider::Gmail, MailProvider::Outlook] {
            assert_eq!(MailProvider::parse(p.as_str()).unwrap(), p);
        }
    }

    #[test]
    fn an_unknown_provider_is_refused_not_defaulted() {
        for stored in ["imap", "Gmail", "Outlook", "", " gmail", "graph"] {
            assert!(
                matches!(MailProvider::parse(stored), Err(CoreError::InvalidInput(_))),
                "{stored:?} must not read as a provider"
            );
        }
    }

    #[test]
    fn a_changes_404_is_an_expired_cursor() {
        assert!(matches!(
            ChangesError::from(CoreError::NotFound),
            ChangesError::Expired
        ));
        assert!(matches!(
            ChangesError::from(CoreError::InvalidInput("x".into())),
            ChangesError::Other(_)
        ));
    }

    struct Sleeper;

    #[async_trait::async_trait]
    impl MailSource for Sleeper {
        fn provider(&self) -> MailProvider {
            MailProvider::Gmail
        }
        async fn list(&self, _: Listing, _: Option<usize>, _: Urgency) -> Result<Vec<String>> {
            unimplemented!()
        }
        async fn current_cursor(&self) -> Result<SyncCursor> {
            unimplemented!()
        }
        fn cursor_is_usable(&self, _: &SyncCursor) -> bool {
            unimplemented!()
        }
        async fn changes(
            &self,
            _: &SyncCursor,
            _: Folder,
        ) -> std::result::Result<Changes, ChangesError> {
            unimplemented!()
        }
        fn merge_cursors(&self, _: &SyncCursor, a: &SyncCursor, _: &SyncCursor) -> SyncCursor {
            a.clone()
        }
        async fn fetch_raw(&self, _: &str, _: Urgency) -> Result<FetchedMessage> {
            unimplemented!()
        }
        async fn fetch_headers(&self, _: &str, _: &[&str]) -> Result<FetchedHeaders> {
            unimplemented!()
        }
        async fn inbox_unread(&self) -> Result<(i64, i64)> {
            unimplemented!()
        }
    }

    /// The default is the poll interval and nothing else: it neither returns
    /// early nor outlasts `max_wait`.
    #[tokio::test(start_paused = true)]
    async fn the_default_wait_is_a_sleep_of_exactly_max_wait() {
        let start = tokio::time::Instant::now();
        Sleeper.wait_for_change(None, Duration::from_secs(5)).await;
        assert_eq!(start.elapsed(), Duration::from_secs(5));
    }
}
