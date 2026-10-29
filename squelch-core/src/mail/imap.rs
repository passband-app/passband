//! IMAP as a [`MailSource`] (issue #575, phase 2): READ ONLY, self-host.
//!
//! What works: connecting (implicit TLS, STARTTLS, and plaintext to loopback
//! only), `LOGIN`, folder roles from `SPECIAL-USE` with a fallback by name,
//! both listings, the change feed with its per-folder cursor, `fetch_raw`,
//! `fetch_headers`, `current_cursor`, `inbox_unread` and `wait_for_change`
//! (IDLE on INBOX over a connection of its own).
//!
//! What a source here must never do:
//! - change server state: every folder is opened with `EXAMINE` (read-only)
//!   and every body or header fetch is `BODY.PEEK`, so `\Seen` and `\Recent`
//!   are left exactly as the user's other clients see them;
//! - log or echo the password: it lives in [`ImapPassword`], whose `Debug`
//!   is redacted, and a failed `LOGIN` is reported without the server's text
//!   (some servers echo the username back, a `BAD` may echo the command).
//!   async-imap `log::trace!`s the LOGIN line; this crate builds `log` with
//!   `max_level_debug` and `release_max_level_debug`, which compile every
//!   trace call out of the whole binary (a const assert below pins it);
//! - send the password in the clear to anything but this machine.
//!
//! THE STABLE ID is `imap:` + hex sha256 of the normalized `Message-ID`
//! ([`stable_id_for_message_id`]). Mail without one hashes a canonical set of
//! headers instead ([`canonical_header_id`]): never the raw block, which
//! delivery decorates differently per copy. It is what `messages.gmail_msg_id`
//! holds for an IMAP account, and it survives a `MOVE` between folders, which
//! a UID does not.
//!
//! AN ID IS BOUND TO ITS CONTENT (Amendment I). A Message-ID is chosen by the
//! sender, so anyone who has seen one can send a new message reusing it. Each
//! id therefore has a [`content_digest`] (canonical headers plus Reply-To, and
//! the exact body text), recorded the first time its bytes are read. A later
//! sighting of a known id at a new place must match it to share the id
//! ([`bind_identities`]); one that does not is another message with an id of
//! its own ([`distinct_stable_id`]), so a stored row is never overwritten with
//! a stranger's content. `fetch_raw` returns only bytes that match, and
//! placement counts only the copies that match.
//!
//! THE LOCATION TABLE ([`MessageLocations`], the store's `imap_locations`)
//! records every folder and UID each id was seen at. `fetch_raw` and
//! `fetch_headers` resolve through it, re-checking `UIDVALIDITY` and the UID,
//! then fall back to an EXACT `SEARCH HEADER Message-ID` (each hit's header
//! read back and compared, the lowest matching UID taken) in the watched
//! folders, the archive and all-mail folders, and as a last resort every
//! folder. A row that has gone stale is dropped as it is found.
//!
//! THE CURSOR is one `{mailbox, uidvalidity, last_uid, highestmodseq}` per
//! watched folder ([`ImapCursor`]). A `UIDVALIDITY` change, or a role that now
//! resolves to a different folder, is [`ChangesError::Expired`]: the engine's
//! catch-up handles it. A listing snapshots its folder BEFORE it searches, and
//! `current_cursor` hands those snapshots back, so mail that lands during a
//! backfill is the next walk's, never nobody's.
//!
//! THE CONNECTION is one command session behind a mutex. A command the server
//! refuses (`NO`/`BAD`) or a message that is not there leaves it in place;
//! only an I/O or protocol error, or a call dropped mid-command, retires it.
//! Reconnects back off exponentially, capped ([`backoff_delay`]).

use std::collections::{BTreeMap, HashSet};
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use async_imap::types::{Fetch, NameAttribute};
use async_imap::{Client, Session};
use chrono::{DateTime, Utc};
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName};
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

use super::{
    Changes, ChangesError, FetchedHeaders, FetchedMessage, Folder, Listing, MailProvider,
    MailSource, Placement, RawBody, SourceRefusals, SyncCursor, Urgency,
};
use crate::error::{CoreError, Result};
use crate::types::AccountId;

// THE PASSWORD NEVER REACHES A LOG LINE (security review 7): async-imap
// `log::trace!`s the LOGIN command. This fails the BUILD, in debug and release
// alike, if `log` is ever compiled with trace enabled (see squelch-core's
// Cargo.toml).
const _: () = assert!(
    log::STATIC_MAX_LEVEL as usize <= log::LevelFilter::Debug as usize,
    "log must be built with max_level_debug and release_max_level_debug"
);

/// The prefix of every IMAP stable id, so one can never be mistaken for a
/// Gmail id in the same column.
pub const STABLE_ID_PREFIX: &str = "imap:";

/// The same ingest bound as Gmail's `decode_raw_b64url`: one message larger
/// than this is not fetched into memory.
const MAX_RAW_BYTES: u32 = 64 * 1024 * 1024;

/// How the connection is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImapTls {
    /// TLS from the first byte (port 993).
    Implicit,
    /// Plain TCP upgraded with `STARTTLS` before anything else is said
    /// (port 143). A server that does not offer it is refused.
    StartTls,
    /// No TLS at all. Refused unless the host is loopback (contract §9): it
    /// exists for the local Dovecot test bed and nothing else.
    Plain,
}

/// Where the account's mailbox is. Everything here is safe to log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImapServer {
    pub host: String,
    pub port: u16,
    pub tls: ImapTls,
    pub username: String,
}

/// THE LEGACY HOME of an IMAP account's server config: an `app_settings` key
/// in plain SQLite, as the first phase-2 builds wrote it. Read once, to move
/// it into the credential slot ([`ImapCredential::from_legacy`]), then
/// deleted. Nothing writes it any more.
pub const IMAP_ACCOUNT_SETTING: &str = "imap_account";

/// Everything about an IMAP account but its password: where it connects and
/// whom it trusts there.
///
/// KEPT IN THE CREDENTIAL SLOT WITH THE PASSWORD (security review 5), never
/// in the database: whatever decides where the password is sent must be as
/// hard to change as the password is to read, or a database writer could
/// point `host` at its own server, add a CA for it, and collect the password
/// at the next start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImapAccountConfig {
    #[serde(flatten)]
    pub server: ImapServer,
    /// A PEM CA to trust IN ADDITION to the public web PKI, for a server
    /// whose certificate a private CA signed (the Dovecot test bed, a home
    /// server). The certificate itself, read once at setup, never a path a
    /// later write could swap. Verification is never off: this adds one
    /// trust anchor the owner named, it does not accept an unverified chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_pem: Option<String>,
}

impl ImapAccountConfig {
    /// The extra trust anchors `ca_pem` holds.
    fn extra_roots(&self) -> Result<Vec<CertificateDer<'static>>> {
        match &self.ca_pem {
            None => Ok(Vec::new()),
            Some(pem) => parse_ca_pem(pem.as_bytes()),
        }
    }
}

/// The certificates in a PEM CA file's bytes; at least one, or an error.
pub fn parse_ca_pem(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>> {
    use tokio_rustls::rustls::pki_types::pem::PemObject;
    let roots: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(pem)
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| CoreError::InvalidInput(format!("imap ca certificate: {e:?}")))?;
    if roots.is_empty() {
        return Err(CoreError::InvalidInput(
            "imap ca certificate: the file holds no certificate".into(),
        ));
    }
    Ok(roots)
}

/// WHAT AN IMAP ACCOUNT'S CREDENTIAL SLOT HOLDS: the server config and the
/// password, sealed together. `Debug` redacts the password.
#[derive(Debug, Clone)]
pub struct ImapCredential {
    pub config: ImapAccountConfig,
    pub password: ImapPassword,
}

#[derive(Serialize, Deserialize)]
struct SlotV1 {
    v: u32,
    #[serde(flatten)]
    config: ImapAccountConfig,
    password: String,
}

/// The phase-2-step-3 `app_settings` shape: a CA by PATH.
#[derive(Deserialize)]
struct LegacyConfig {
    #[serde(flatten)]
    server: ImapServer,
    #[serde(default)]
    ca_file: Option<std::path::PathBuf>,
}

/// A slot as read: the current shape, or a bare password from a build that
/// kept the server in `app_settings`.
#[derive(Debug)]
pub enum StoredImapSlot {
    Credential(ImapCredential),
    LegacyPassword(ImapPassword),
}

impl ImapCredential {
    /// The slot's text. Never logged: it holds the password.
    pub fn to_slot(&self) -> Result<String> {
        serde_json::to_string(&SlotV1 {
            v: 1,
            config: self.config.clone(),
            password: self.password.0.clone(),
        })
        .map_err(|_| CoreError::Other(anyhow::anyhow!("imap credential: could not encode")))
    }

    /// Read a slot's text. Errors never carry it.
    pub fn from_slot(text: &str) -> StoredImapSlot {
        match serde_json::from_str::<SlotV1>(text) {
            Ok(slot) if slot.v == 1 => StoredImapSlot::Credential(ImapCredential {
                config: slot.config,
                password: ImapPassword(slot.password),
            }),
            _ => StoredImapSlot::LegacyPassword(ImapPassword(text.to_string())),
        }
    }

    /// The one-time move from the legacy layout: the server from the old
    /// `app_settings` JSON, and the CA file it names read NOW, into a
    /// credential to seal. The caller stores it and deletes the setting.
    pub fn from_legacy(password: ImapPassword, setting_json: &str) -> Result<Self> {
        let legacy: LegacyConfig = serde_json::from_str(setting_json)
            .map_err(|e| CoreError::InvalidInput(format!("stored imap account config: {e}")))?;
        let ca_pem = match &legacy.ca_file {
            None => None,
            Some(path) => {
                let pem = std::fs::read(path).map_err(|e| {
                    CoreError::InvalidInput(format!("imap ca file {}: {e}", path.display()))
                })?;
                parse_ca_pem(&pem)?;
                Some(String::from_utf8(pem).map_err(|_| {
                    CoreError::InvalidInput(format!("imap ca file {} is not PEM", path.display()))
                })?)
            }
        };
        Ok(Self {
            config: ImapAccountConfig {
                server: legacy.server,
                ca_pem,
            },
            password,
        })
    }
}

/// The account's (app) password. Never in `Debug`, never in an error.
#[derive(Clone)]
pub struct ImapPassword(String);

impl ImapPassword {
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }
}

impl std::fmt::Debug for ImapPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ImapPassword(<redacted>)")
    }
}

// ---- the transport --------------------------------------------------------

/// The one stream type the session runs over, whichever way it was secured.
#[derive(Debug)]
enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

/// A [`Stream`] with a STALL TIMER (security review 3): while a read or a
/// write is waiting, the server must make progress within `stall`, or the
/// operation fails with `TimedOut`, which every command maps to a broken
/// session. Progress is any byte read, or a write accepted, so a large FETCH
/// over a slow link never trips it: only silence does. `None` for the IDLE
/// connection, which is silent by design and bounded by its own deadline.
struct Transport {
    stream: Stream,
    stall: Option<Duration>,
    /// When the stream last moved (or the timer was first needed).
    last_progress: tokio::time::Instant,
    timer: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transport")
            .field("stream", &self.stream)
            .field("stall", &self.stall)
            .finish_non_exhaustive()
    }
}

impl Transport {
    fn new(stream: Stream, stall: Option<Duration>) -> Self {
        Self {
            stream,
            stall,
            last_progress: tokio::time::Instant::now(),
            timer: None,
        }
    }

    fn progressed(&mut self) {
        self.last_progress = tokio::time::Instant::now();
        self.timer = None;
    }

    /// The inner poll came back `Pending`: has the server been silent too
    /// long? Arms the timer so this task is woken when it would be.
    fn check_stall(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Error> {
        let Some(stall) = self.stall else {
            return Poll::Pending;
        };
        let due = self.last_progress + stall;
        let timer = self
            .timer
            .get_or_insert_with(|| Box::pin(tokio::time::sleep_until(due)));
        if timer.deadline() != due {
            timer.as_mut().reset(due);
        }
        match timer.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the server stopped answering",
            )),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncRead for Transport {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let polled = match &mut this.stream {
            Stream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        };
        match polled {
            Poll::Pending => this.check_stall(cx).map(Err),
            ready => {
                this.progressed();
                ready
            }
        }
    }
}

impl AsyncWrite for Transport {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let polled = match &mut this.stream {
            Stream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        };
        match polled {
            Poll::Pending => this.check_stall(cx).map(Err),
            // A command going out starts the clock on its answer afresh.
            ready => {
                this.progressed();
                ready
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let polled = match &mut this.stream {
            Stream::Plain(s) => Pin::new(s).poll_flush(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        };
        match polled {
            Poll::Pending => this.check_stall(cx).map(Err),
            ready => {
                this.progressed();
                ready
            }
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut self.get_mut().stream {
            Stream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

/// How long each step of the IMAP conversation may take (security review
/// 3). Gmail's REST client has one 180 s timeout per request; IMAP has more
/// steps, so each gets its own bound.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timeouts {
    /// TCP connect, and separately the TLS handshake.
    pub connect: Duration,
    /// Everything from the greeting through STARTTLS, LOGIN and CAPABILITY.
    pub login: Duration,
    /// The longest the command session's server may say nothing while a
    /// command waits on it. A stall bound, not a total: a 64 MB FETCH at a
    /// few hundred KB/s keeps moving and is never cut off, and a server that
    /// has stopped answering is noticed in two minutes, not never.
    pub stall: Duration,
    /// The longest one IDLE round runs before `DONE` and a fresh `IDLE`
    /// (RFC 2177 says at most 29 minutes; contract §8 says 25).
    pub idle_reissue: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(30),
            login: Duration::from_secs(60),
            stall: Duration::from_secs(120),
            idle_reissue: IDLE_REISSUE,
        }
    }
}

/// TCP keepalive on the socket, so a peer that vanished (a laptop that slept
/// through a NAT timeout) is noticed by the kernel even under a long IDLE.
fn set_keepalive(tcp: &TcpStream) {
    let ka = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(60))
        .with_interval(Duration::from_secs(15));
    // Best effort: a platform that refuses it still has the stall timer.
    let _ = socket2::SockRef::from(tcp).set_tcp_keepalive(&ka);
}

type ImapSession = Session<Transport>;

/// Is `host` this machine, by its literal spelling? The connect also checks
/// the address it actually reached ([`ImapSource::connect_tcp`]), so a
/// `localhost` that resolves somewhere else still fails closed.
fn host_is_loopback(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost") || h.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// May an account on `host` skip TLS? Only when it is this machine (contract
/// §9). `squelchd auth --imap` asks before it prompts for the password; the
/// source refuses the rest on its own.
pub fn plaintext_allowed(host: &str) -> bool {
    host_is_loopback(host)
}

// ---- errors ---------------------------------------------------------------

/// The marker on an error that leaves the session unusable: the stream is
/// gone or mid-way through a response nobody will finish reading. Carried
/// inside `CoreError::Other` so the trait's error type does not change, and
/// read back by [`is_session_broken`].
#[derive(Debug)]
struct SessionBroken(String);

impl std::fmt::Display for SessionBroken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SessionBroken {}

/// An error about the connection, never carrying a secret: the context is
/// ours and `e` is an io/TLS/IMAP error about the transport. The session it
/// happened on (if any) is still good.
fn net_err(context: &'static str, e: impl std::fmt::Display) -> CoreError {
    CoreError::Other(anyhow::anyhow!("imap {context}: {e}"))
}

/// The same, for an error after which the session must not be reused.
fn broken_err(context: &'static str, e: impl std::fmt::Display) -> CoreError {
    CoreError::Other(anyhow::Error::new(SessionBroken(format!(
        "imap {context}: {e}"
    ))))
}

/// Does `e` mean the session it came from is finished (Amendment H)?
fn is_session_broken(e: &CoreError) -> bool {
    matches!(e, CoreError::Other(a) if a.is::<SessionBroken>())
}

/// An IMAP command's failure. A `NO` or `BAD` is the server answering this
/// one command (its text is about the command, never our password: `LOGIN`
/// is mapped separately, see [`ImapSource::connect`]), and a validation error
/// is caught before anything is sent, so the session survives both. I/O, a
/// lost connection or a response we could not parse leave the stream in an
/// unknown place: those retire it.
fn imap_err(context: &'static str) -> impl FnOnce(async_imap::error::Error) -> CoreError {
    use async_imap::error::Error as E;
    move |e| match e {
        E::No(_) | E::Bad(_) | E::Validate(_) => net_err(context, e),
        _ => broken_err(context, e),
    }
}

// ---- folder roles ---------------------------------------------------------

/// The folders a source watches or skips, resolved per connection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FolderRoles {
    pub sent: Option<String>,
    pub junk: Option<String>,
    pub trash: Option<String>,
    pub drafts: Option<String>,
    pub archive: Option<String>,
    /// The all-mail view (`\All`), where a server has one (Gmail, some
    /// Dovecot setups): every message whatever folder it is filed in.
    pub all: Option<String>,
    /// Every selectable folder, for the archive window's walk.
    pub selectable: Vec<String>,
}

impl FolderRoles {
    /// The folder a [`Folder`] is on this server, if it has one.
    pub fn folder(&self, folder: Folder) -> Option<&str> {
        match folder {
            Folder::Inbox => Some(INBOX),
            Folder::Sent => self.sent.as_deref(),
            Folder::Spam => self.junk.as_deref(),
        }
    }

    /// The watched folders, in walk order (contract §7).
    pub fn watched(&self) -> Vec<(Folder, &str)> {
        WATCHED
            .into_iter()
            .filter_map(|f| self.folder(f).map(|name| (f, name)))
            .collect()
    }

    /// Where a stale location's `SEARCH HEADER Message-ID` looks: the
    /// watched folders, then the archive and all-mail folders, since a
    /// message the user archived from another client is filed in one of
    /// those and must not read as gone (that would leave a blank body or
    /// an empty recipient list stuck on its row for good).
    pub fn search_folders(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self.watched().into_iter().map(|(_, n)| n).collect();
        for extra in [self.archive.as_deref(), self.all.as_deref()]
            .into_iter()
            .flatten()
        {
            if !out.contains(&extra) {
                out.push(extra);
            }
        }
        out
    }

    /// The folders the archive window walks: every selectable folder but
    /// junk, trash and drafts (Gmail's `-in:draft` plus its default exclusion
    /// of spam and trash), and with `filed_only` the inbox and sent folder too.
    /// Never the all-mail view (correctness M4): it holds INBOX and Sent mail
    /// as well, so it would break `filed_only` and list every message twice.
    pub fn window_folders(&self, filed_only: bool) -> Vec<&str> {
        let mut skip: Vec<&str> = [&self.junk, &self.trash, &self.drafts, &self.all]
            .into_iter()
            .filter_map(|f| f.as_deref())
            .collect();
        if filed_only {
            skip.push(INBOX);
            skip.extend(self.sent.as_deref());
        }
        self.selectable
            .iter()
            .map(String::as_str)
            .filter(|name| {
                // INBOX is case-insensitive by RFC 3501; the rest are exact.
                !skip.iter().any(|s| {
                    s == name || (s.eq_ignore_ascii_case(INBOX) && name.eq_ignore_ascii_case(INBOX))
                })
            })
            .collect()
    }
}

const INBOX: &str = "INBOX";
const WATCHED: [Folder; 3] = [Folder::Inbox, Folder::Sent, Folder::Spam];

/// Resolve roles from one `LIST` reply: `SPECIAL-USE` attributes (RFC 6154)
/// win; a folder named like the role is the fallback, first match in the
/// order given.
pub(crate) fn resolve_roles(names: &[(String, Vec<String>)]) -> FolderRoles {
    let by_attr = |attr: &str| {
        names
            .iter()
            .find(|(_, attrs)| attrs.iter().any(|a| a.eq_ignore_ascii_case(attr)))
            .map(|(n, _)| n.clone())
    };
    let by_name = |candidates: &[&str]| {
        candidates.iter().find_map(|c| {
            names
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(c))
                .map(|(n, _)| n.clone())
        })
    };
    let pick = |attr: &str, fallback: &[&str]| by_attr(attr).or_else(|| by_name(fallback));
    FolderRoles {
        sent: pick(
            "\\Sent",
            &["Sent", "Sent Messages", "Sent Items", "Sent Mail"],
        ),
        junk: pick("\\Junk", &["Junk", "Spam", "Junk E-mail", "Junk Email"]),
        trash: pick("\\Trash", &["Trash", "Deleted Messages", "Deleted Items"]),
        drafts: pick("\\Drafts", &["Drafts"]),
        archive: pick("\\Archive", &["Archive"]),
        all: pick("\\All", &["All Mail", "[Gmail]/All Mail"]),
        selectable: names
            .iter()
            .filter(|(_, attrs)| {
                !attrs.iter().any(|a| {
                    a.eq_ignore_ascii_case("\\Noselect") || a.eq_ignore_ascii_case("\\NonExistent")
                })
            })
            .map(|(n, _)| n.clone())
            .collect(),
    }
}

fn attr_text(a: &NameAttribute<'_>) -> String {
    match a {
        NameAttribute::NoInferiors => "\\Noinferiors".into(),
        NameAttribute::NoSelect => "\\Noselect".into(),
        NameAttribute::Marked => "\\Marked".into(),
        NameAttribute::Unmarked => "\\Unmarked".into(),
        NameAttribute::All => "\\All".into(),
        NameAttribute::Archive => "\\Archive".into(),
        NameAttribute::Drafts => "\\Drafts".into(),
        NameAttribute::Flagged => "\\Flagged".into(),
        NameAttribute::Junk => "\\Junk".into(),
        NameAttribute::Sent => "\\Sent".into(),
        NameAttribute::Trash => "\\Trash".into(),
        NameAttribute::Extension(s) => s.to_string(),
        other => format!("{other:?}"),
    }
}

// ---- stable ids -----------------------------------------------------------

/// Normalize a `Message-ID` value (contract §5): trim, take what is inside
/// the first `<...>` (or the whole value if unbracketed), and lowercase only
/// the domain part, which is case-insensitive where the local part is not.
/// `None` for a value with nothing in it.
pub fn normalize_message_id(value: &str) -> Option<String> {
    let v = value.trim();
    let inner = match (v.find('<'), v.find('>')) {
        (Some(a), Some(b)) if b > a => &v[a + 1..b],
        _ => v.trim_matches(|c| c == '<' || c == '>'),
    };
    let inner = inner.trim();
    if inner.is_empty() {
        return None;
    }
    Some(match inner.rsplit_once('@') {
        Some((local, domain)) => format!("{local}@{}", domain.to_ascii_lowercase()),
        None => inner.to_string(),
    })
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(STABLE_ID_PREFIX.len() + 64);
    out.push_str(STABLE_ID_PREFIX);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// The stable id of a message with this (already normalized) Message-ID.
pub fn stable_id_for_message_id(normalized: &str) -> String {
    hex_sha256(normalized.as_bytes())
}

/// The headers a message without a Message-ID is identified by
/// ([`canonical_header_id`]). Asked for in every id fetch so no second
/// round trip is needed.
const ID_HEADER_FIELDS: &str = "MESSAGE-ID FROM TO CC DATE SUBJECT";

/// THE ID OF MAIL WITHOUT A MESSAGE-ID (Amendment E): a hash of what the
/// author wrote, never of what delivery added. A self-sent copy in INBOX
/// carries `Received`, `Delivered-To` and `Return-Path` lines its Sent twin
/// does not, so the raw block would make them two messages. Hashed instead:
/// the lowercased addresses of From, To and Cc (sorted, deduplicated: a
/// server may reorder a list, never change it), the Date header as an
/// instant in UTC (a relay may rewrite the offset), and the trimmed Subject.
///
/// Two distinct messages with the same author, recipients, second and
/// subject share an id. Mail that careless about its identity is rare, and
/// for it one row is the honest outcome.
pub fn canonical_header_id(header: &[u8]) -> String {
    match canonical_header_text(header, false) {
        Some(canonical) => hex_sha256(canonical.as_bytes()),
        // Nothing parseable at all: the block is all there is.
        None => hex_sha256(header),
    }
}

/// The canonical header set as text (Amendment E), with Reply-To when
/// `reply_to` (the content digest's set, Amendment I). `None` when nothing in
/// the block parses.
fn canonical_header_text(header: &[u8], reply_to: bool) -> Option<String> {
    let m = mail_parser::MessageParser::default().parse_headers(header)?;
    let addrs = |a: Option<&mail_parser::Address<'_>>| {
        let mut v: Vec<String> = a
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.address())
                    .map(|x| x.trim().to_lowercase())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v.dedup();
        v.join(",")
    };
    let mut canonical = format!(
        "imap-canonical-v1\nfrom:{}\nto:{}\ncc:{}\ndate:{}\nsubject:{}\n",
        addrs(m.from()),
        addrs(m.to()),
        addrs(m.cc()),
        m.date()
            .map(|d| d.to_timestamp().to_string())
            .unwrap_or_default(),
        m.subject().map(str::trim).unwrap_or_default(),
    );
    if reply_to {
        canonical.push_str(&format!("reply-to:{}\n", addrs(m.reply_to())));
    }
    Some(canonical)
}

/// The headers the content digest is taken over, as `HEADER.FIELDS` names.
const DIGEST_HEADER_FIELDS: &str = "FROM TO CC DATE SUBJECT REPLY-TO";

/// THE CONTENT DIGEST (Amendment I): what a re-sighting of a known id must
/// match to share it. sha256 of the canonical header set plus Reply-To, then
/// the exact `BODY[TEXT]` bytes. Delivery trace headers are not in the set,
/// so a self-sent copy in INBOX and Sent and a message MOVEd between folders
/// keep one digest; a different message reusing a Message-ID does not.
pub fn content_digest(header: &[u8], text: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b"imap-content-v1\n");
    match canonical_header_text(header, true) {
        Some(c) => h.update(c.as_bytes()),
        None => h.update(header),
    }
    h.update(b"\n--text--\n");
    h.update(text);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// [`content_digest`] of a whole message, split where `BODY[TEXT]` starts:
/// after the first empty line.
pub fn content_digest_of_raw(raw: &[u8]) -> String {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .or_else(|| raw.windows(2).position(|w| w == b"\n\n").map(|i| i + 2));
    match split {
        Some(at) => content_digest(&raw[..at], &raw[at..]),
        None => content_digest(raw, b""),
    }
}

/// The id a message gets when it reuses a known id's Message-ID (or, with
/// none, its canonical headers) but is not that message: the Message-ID text
/// (or the known id) with its own digest. Never the known id itself.
pub fn distinct_stable_id(known_id: &str, message_id: Option<&str>, digest: &str) -> String {
    hex_sha256(
        format!(
            "imap-distinct-v1\n{}\n{digest}",
            message_id.unwrap_or(known_id)
        )
        .as_bytes(),
    )
}

/// The stable id of a message from its header block (all of it, or at least
/// [`ID_HEADER_FIELDS`]): the Message-ID's when it has one, else the
/// canonical header id. The normalized Message-ID comes back with it.
pub fn stable_id_for_header_block(header: &[u8]) -> (String, Option<String>) {
    match header_values(header, "Message-ID")
        .into_iter()
        .find_map(|v| normalize_message_id(&v))
    {
        Some(mid) => (stable_id_for_message_id(&mid), Some(mid)),
        None => (canonical_header_id(header), None),
    }
}

/// Every header in a raw block as `(name, value)`, unfolded, in order. The
/// values are the wire text (RFC 2047 words left encoded), as Gmail's
/// `format=metadata` gives them.
pub(crate) fn parse_header_block(header: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(header);
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.split("\r\n").flat_map(|l| l.split('\n')) {
        if line.is_empty() {
            continue;
        }
        if line.starts_with([' ', '\t']) {
            if let Some((_, v)) = out.last_mut() {
                v.push(' ');
                v.push_str(line.trim());
            }
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            out.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    out
}

fn header_values(header: &[u8], name: &str) -> Vec<String> {
    parse_header_block(header)
        .into_iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v)
        .collect()
}

// ---- the cursor -----------------------------------------------------------

/// One watched folder's position (contract §7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FolderPos {
    /// The folder's name on the server when the position was taken. A role
    /// that now resolves to a different folder is a reset, like a
    /// `UIDVALIDITY` change, not a position to carry across.
    pub mailbox: String,
    pub uidvalidity: u32,
    /// `UIDNEXT - 1` when the position was taken: every UID at or below it
    /// has been listed.
    pub last_uid: u32,
    /// `HIGHESTMODSEQ`, on a server with CONDSTORE (RFC 7162).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highestmodseq: Option<u64>,
}

impl FolderPos {
    /// Does `now` prove nothing in the folder changed since `self`? Without
    /// CONDSTORE only arrivals can be seen (a `UIDNEXT` that has not moved),
    /// which is what the walk needs: a flag change is not new mail.
    fn unchanged_at(&self, now: &FolderPos) -> bool {
        self.mailbox == now.mailbox
            && self.uidvalidity == now.uidvalidity
            && self.last_uid == now.last_uid
            && match (self.highestmodseq, now.highestmodseq) {
                (Some(a), Some(b)) => a == b,
                (None, None) => true,
                // The server gained or lost CONDSTORE: cannot tell.
                _ => false,
            }
    }
}

/// The whole cursor: one entry per watched folder, keyed by role
/// (`inbox`, `sent`, `spam`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ImapCursor {
    pub v: u32,
    pub folders: BTreeMap<String, FolderPos>,
}

pub(crate) fn role_key(folder: Folder) -> &'static str {
    match folder {
        Folder::Inbox => "inbox",
        Folder::Sent => "sent",
        Folder::Spam => "spam",
    }
}

impl ImapCursor {
    pub fn encode(&self) -> SyncCursor {
        // A struct of strings and integers: serialization cannot fail.
        SyncCursor::new(serde_json::to_string(self).unwrap_or_default())
    }

    pub fn decode(cursor: &SyncCursor) -> Option<Self> {
        serde_json::from_str::<Self>(cursor.as_str())
            .ok()
            .filter(|c| c.v == 1)
    }
}

/// Two walks from `since`, merged per folder: each folder takes whichever
/// side moved it off `since`, and if both did (they never should: a walk
/// moves only its own folder), the one further along. A folder only one
/// side has is kept. Never behind either side.
pub(crate) fn merge_cursor_values(
    since: Option<&ImapCursor>,
    a: &ImapCursor,
    b: &ImapCursor,
) -> ImapCursor {
    let mut out = ImapCursor {
        v: 1,
        folders: BTreeMap::new(),
    };
    let keys: std::collections::BTreeSet<&String> =
        a.folders.keys().chain(b.folders.keys()).collect();
    for key in keys {
        let base = since.and_then(|s| s.folders.get(key));
        let pick = match (a.folders.get(key), b.folders.get(key)) {
            (Some(x), None) | (None, Some(x)) => x,
            (Some(x), Some(y)) => match (Some(x) != base, Some(y) != base) {
                (true, false) => x,
                (false, true) => y,
                // Both moved, or neither did (then they are equal): a
                // folder reset on one side wins, else the further one.
                _ if x.uidvalidity != y.uidvalidity || x.mailbox != y.mailbox => {
                    if base
                        .is_some_and(|b| b.uidvalidity == x.uidvalidity && b.mailbox == x.mailbox)
                    {
                        y
                    } else {
                        x
                    }
                }
                _ => {
                    if (y.last_uid, y.highestmodseq) > (x.last_uid, x.highestmodseq) {
                        y
                    } else {
                        x
                    }
                }
            },
            (None, None) => unreachable!("key came from one side"),
        };
        out.folders.insert(key.clone(), pick.clone());
    }
    out
}

// ---- the location table ---------------------------------------------------

/// Where one message was seen: a folder, the folder's `UIDVALIDITY` then, and
/// the message's UID in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageLocation {
    pub folder: String,
    pub uidvalidity: u32,
    pub uid: u32,
    /// The normalized Message-ID TEXT, `None` for mail without one. The
    /// stable id is a hash of it, and the stale-location fallback has to
    /// `SEARCH` by the real header.
    pub message_id: Option<String>,
}

/// THE IMAP SOURCE'S PRIVATE LOOKUP STATE (Amendment A): every folder each
/// stable id has been seen in. Narrow on purpose, so the source holds no
/// handle on the rest of the store. `SqliteStore` implements it over the
/// `imap_locations` table. A row is a hint: the source re-checks it on use.
pub trait MessageLocations: Send + Sync {
    /// Every folder `msg_id` was last seen in, most recent sighting first.
    fn message_locations(
        &self,
        account_id: AccountId,
        msg_id: &str,
    ) -> Result<Vec<MessageLocation>>;

    /// Record sightings, one row per (id, folder), replacing that folder's
    /// previous row for the id. A sighting with no Message-ID text keeps the
    /// text already stored.
    fn record_message_locations(
        &self,
        account_id: AccountId,
        seen: &[(String, MessageLocation)],
    ) -> Result<()>;

    /// Drop one folder's row for `msg_id`: its UID is gone, or the folder's
    /// `UIDVALIDITY` moved on.
    fn forget_message_location(
        &self,
        account_id: AccountId,
        msg_id: &str,
        folder: &str,
    ) -> Result<()>;

    /// The [`content_digest`] recorded for `msg_id` (Amendment I), if any.
    fn content_digest(&self, account_id: AccountId, msg_id: &str) -> Result<Option<String>>;

    /// Record `msg_id`'s digest. The FIRST one recorded stands: a later call
    /// with a different digest changes nothing, so no sighting can rebind an
    /// id to other content.
    fn record_content_digest(
        &self,
        account_id: AccountId,
        msg_id: &str,
        digest: &str,
    ) -> Result<()>;
}

// ---- reconnect backoff ----------------------------------------------------

const BACKOFF_BASE: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(60);

/// How long to wait before the next connect after `failures` in a row:
/// nothing after a success, then 1s, 2s, 4s ... capped at a minute.
pub(crate) fn backoff_delay(failures: u32) -> Duration {
    match failures {
        0 => Duration::ZERO,
        n => BACKOFF_BASE
            .saturating_mul(1u32 << (n - 1).min(16))
            .min(BACKOFF_CAP),
    }
}

#[derive(Debug, Default)]
struct Backoff {
    failures: u32,
    last_failure: Option<tokio::time::Instant>,
}

impl Backoff {
    /// What is left of the wait before the next attempt.
    fn remaining(&self) -> Duration {
        self.last_failure
            .map(|t| backoff_delay(self.failures).saturating_sub(t.elapsed()))
            .unwrap_or_default()
    }
}

/// The IDLE connection's own failure record (correctness L3): its logins
/// back off like the command session's, and a refused password is counted
/// once per run of failures, not once per attempt.
#[derive(Debug, Default)]
struct IdleBackoff {
    backoff: Backoff,
    /// This run of failures has already counted its login refusal.
    auth_counted: bool,
}

// ---- the source -----------------------------------------------------------

/// One authenticated command connection and what was learned on it.
struct Conn {
    session: ImapSession,
    roles: FolderRoles,
    /// The server advertises CONDSTORE: `STATUS` may ask for HIGHESTMODSEQ.
    condstore: bool,
    /// The folder the server has open (`EXAMINE`d last) and its
    /// `UIDVALIDITY`, with the [`Conn::calls`] value it was opened in.
    ///
    /// Reused ONLY within the call that opened it: a server shows a session
    /// what changed in its open folder (arrivals, expunges) only when it
    /// re-syncs that view, so a later call opens it afresh rather than
    /// fetching or searching a stale one.
    selected: Option<(String, u32, u64)>,
    /// One per [`ImapSource::with_conn`] call.
    calls: u64,
    /// Set for the length of a command. Still set when the session is next
    /// locked means a caller was dropped mid-command and the stream is
    /// somewhere in a response: the session is retired, not reused.
    busy: bool,
}

impl Conn {
    /// `EXAMINE` (read-only) a folder and return its `UIDVALIDITY`, or `None`
    /// if the server refuses it (the folder is gone).
    async fn examine(&mut self, folder: &str) -> Result<Option<u32>> {
        if let Some((name, validity, call)) = &self.selected
            && name == folder
            && *call == self.calls
        {
            return Ok(Some(*validity));
        }
        self.selected = None;
        let mbox = match self.session.examine(folder).await {
            Ok(m) => m,
            Err(async_imap::error::Error::No(_)) => return Ok(None),
            Err(e) => return Err(imap_err("examine")(e)),
        };
        let validity = mbox
            .uid_validity
            .ok_or_else(|| net_err("examine", "server sent no UIDVALIDITY"))?;
        self.selected = Some((folder.to_string(), validity, self.calls));
        Ok(Some(validity))
    }

    /// `STATUS` on the open folder can answer from the session's stale view
    /// of it (RFC 3501 says not to ask), so every `STATUS` closes it first.
    /// `CLOSE` on a folder opened with `EXAMINE` expunges nothing.
    async fn close_if_open(&mut self, folder: &str) -> Result<()> {
        if self
            .selected
            .as_ref()
            .is_some_and(|(name, _, _)| name == folder)
        {
            self.selected = None;
            self.session.close().await.map_err(imap_err("close"))?;
        }
        Ok(())
    }

    /// List the folders again and take the roles from that (correctness H2):
    /// a role folder the user deleted or renamed in another client answers
    /// `NO`, and the roles this session resolved at login would keep naming
    /// it for as long as the session lives.
    async fn refresh_roles(&mut self) -> Result<()> {
        self.roles = list_roles(&mut self.session).await?;
        Ok(())
    }

    /// The folder a role is on now and its position, or `None` if the role
    /// has no folder. A role folder that answers `NO` sends the roles to be
    /// listed again and the role is asked once more, so a folder renamed or
    /// recreated is followed and one that is gone reads as no folder.
    async fn role_status(&mut self, folder: Folder) -> Result<Option<(String, FolderPos)>> {
        let Some(name) = self.roles.folder(folder).map(str::to_string) else {
            return Ok(None);
        };
        if let Some(pos) = self.status(&name).await? {
            return Ok(Some((name, pos)));
        }
        self.refresh_roles().await?;
        let Some(name) = self.roles.folder(folder).map(str::to_string) else {
            return Ok(None);
        };
        Ok(self.status(&name).await?.map(|pos| (name, pos)))
    }

    /// One folder's position as `STATUS` reports it now, or `None` if the
    /// server refuses it (the folder is gone).
    async fn status(&mut self, folder: &str) -> Result<Option<FolderPos>> {
        self.close_if_open(folder).await?;
        let items = if self.condstore {
            "(UIDVALIDITY UIDNEXT HIGHESTMODSEQ)"
        } else {
            "(UIDVALIDITY UIDNEXT)"
        };
        let st = match self.session.status(folder, items).await {
            Ok(st) => st,
            Err(async_imap::error::Error::No(_)) => return Ok(None),
            Err(e) => return Err(imap_err("status")(e)),
        };
        let (Some(uidvalidity), Some(uid_next)) = (st.uid_validity, st.uid_next) else {
            return Err(net_err("status", "server omitted UIDVALIDITY/UIDNEXT"));
        };
        Ok(Some(FolderPos {
            mailbox: folder.to_string(),
            uidvalidity,
            last_uid: uid_next.saturating_sub(1),
            highestmodseq: st.highest_modseq,
        }))
    }
}

/// What a server said it can do, read once per login.
#[derive(Debug, Clone, Copy)]
struct Caps {
    condstore: bool,
    idle: bool,
}

/// One message as a listing or a walk saw it.
#[derive(Debug, Clone)]
struct Seen {
    id: String,
    loc: MessageLocation,
    internal_date: Option<DateTime<Utc>>,
    /// The content digest, when binding this sighting had to read it
    /// ([`bind_identities`]): recorded with the location.
    digest: Option<String>,
}

/// An IMAP mailbox's read side.
pub struct ImapSource {
    account_id: AccountId,
    server: ImapServer,
    password: ImapPassword,
    tls: Arc<ClientConfig>,
    /// The one command connection (contract §11). `None` until first use and
    /// after an error that broke it, so the next call reconnects.
    conn: tokio::sync::Mutex<Option<Conn>>,
    backoff: std::sync::Mutex<Backoff>,
    locations: Arc<dyn MessageLocations>,
    /// Amendment G: each watched folder's position as of the START of its
    /// last listing, keyed by role, handed out by `current_cursor`.
    snapshots: std::sync::Mutex<BTreeMap<String, FolderPos>>,
    /// Logins that succeeded, ever: what the tests read to tell a kept
    /// session from a new one.
    connects: AtomicU64,
    refusals: Arc<SourceRefusals>,
    /// Does the server do IDLE? [`IDLE_UNKNOWN`] until a login says.
    idle_support: AtomicU8,
    /// The parked IDLE connection between `wait_for_change` calls (contract
    /// §8, §11): its own login, never the command session. TAKEN out for the
    /// length of a wait and put back only after a clean `DONE`, so a wait
    /// dropped mid-IDLE drops the connection with it and the next call
    /// starts over on a new one.
    idle_conn: tokio::sync::Mutex<Option<ImapSession>>,
    /// The cursor a wait last returned early for because the server was
    /// already ahead of it. The same cursor again means the engine's walk
    /// did not move it, so the next wait parks instead: never a spin.
    idle_ahead_of: std::sync::Mutex<Option<SyncCursor>>,
    /// IDLE logins that succeeded, for the tests.
    idle_connects: AtomicU64,
    idle_backoff: std::sync::Mutex<IdleBackoff>,
    /// When the last wait that returned before its deadline returned: the
    /// next early return is held to [`MIN_WAKE_GAP`] after it.
    last_wake: std::sync::Mutex<Option<tokio::time::Instant>>,
    timeouts: Timeouts,
}

/// The one thing a failed `LOGIN` ever says.
const LOGIN_REFUSED: &str = "imap login refused: check the username and app password";

const IDLE_UNKNOWN: u8 = 0;
const IDLE_YES: u8 = 1;
const IDLE_NO: u8 = 2;

/// The longest one IDLE runs before it is re-issued: RFC 2177 says servers
/// may drop a client idle for 30 minutes, so under that with room to spare.
const IDLE_REISSUE: Duration = Duration::from_secs(25 * 60);

/// How long past its deadline an IDLE wait may run to finish cleanly (the
/// `DONE` exchange that keeps the connection for the next wait). Past it the
/// wait gives up on the connection and returns: a server can hold a socket
/// open and say nothing (Dovecot does, after INBOX is deleted under an IDLE),
/// and nothing about that may hold the poll lane.
const IDLE_GRACE: Duration = Duration::from_secs(10);

/// The least time between two waits that return before their deadline
/// (security review 8): a server streaming untagged FETCH (hostile, or a
/// mailbox another client churns the flags of) cannot drive back-to-back
/// poll ticks.
const MIN_WAKE_GAP: Duration = Duration::from_secs(2);

impl std::fmt::Debug for ImapSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImapSource")
            .field("account_id", &self.account_id)
            .field("server", &self.server)
            .field("password", &self.password)
            .finish_non_exhaustive()
    }
}

fn tls_config(extra_roots: &[CertificateDer<'static>]) -> Result<Arc<ClientConfig>> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    for der in extra_roots {
        roots.add(der.clone()).map_err(|e| net_err("tls root", e))?;
    }
    let config = ClientConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| net_err("tls config", e))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(Arc::new(config))
}

impl ImapSource {
    /// A source for `account_id`'s mailbox on `server`, trusting the public
    /// web PKI only. `locations` is where it keeps its lookup rows (the
    /// store, in the daemon).
    pub fn new(
        account_id: AccountId,
        server: ImapServer,
        password: ImapPassword,
        locations: Arc<dyn MessageLocations>,
        refusals: Arc<SourceRefusals>,
    ) -> Result<Self> {
        Self::with_roots(account_id, server, password, locations, refusals, &[])
    }

    /// A source for a stored account ([`ImapAccountConfig`]), trusting its
    /// `ca_pem` on top of the web PKI.
    pub fn from_config(
        account_id: AccountId,
        config: &ImapAccountConfig,
        password: ImapPassword,
        locations: Arc<dyn MessageLocations>,
        refusals: Arc<SourceRefusals>,
    ) -> Result<Self> {
        let roots = config.extra_roots()?;
        Self::with_roots(
            account_id,
            config.server.clone(),
            password,
            locations,
            refusals,
            &roots,
        )
    }

    /// THE TEST LOGIN `squelchd auth --imap` runs before it stores anything:
    /// connect, `LOGIN`, `LIST`, and resolve the folder roles. Returns each
    /// role and the folder it resolved to (`None`: the server has none).
    pub async fn check_login(&self) -> Result<Vec<(&'static str, Option<String>)>> {
        let roles = self
            .with_conn(|c| Box::pin(async move { Ok(c.roles.clone()) }))
            .await?;
        Ok(vec![
            ("inbox", Some(INBOX.to_string())),
            ("sent", roles.sent),
            ("junk", roles.junk),
            ("archive", roles.archive),
            ("all mail", roles.all),
            ("trash", roles.trash),
            ("drafts", roles.drafts),
        ])
    }

    /// The same, also trusting `extra_roots`. Crate-private: the test bed's
    /// throwaway CA is the only caller. There is no way to turn verification
    /// off (contract §9).
    pub(crate) fn with_roots(
        account_id: AccountId,
        server: ImapServer,
        password: ImapPassword,
        locations: Arc<dyn MessageLocations>,
        refusals: Arc<SourceRefusals>,
        extra_roots: &[CertificateDer<'static>],
    ) -> Result<Self> {
        if server.tls == ImapTls::Plain && !host_is_loopback(&server.host) {
            return Err(CoreError::InvalidInput(format!(
                "imap: refusing a plaintext connection to {}: only loopback may skip TLS",
                server.host
            )));
        }
        Ok(Self {
            account_id,
            tls: tls_config(extra_roots)?,
            server,
            password,
            conn: tokio::sync::Mutex::new(None),
            backoff: std::sync::Mutex::new(Backoff::default()),
            locations,
            snapshots: std::sync::Mutex::new(BTreeMap::new()),
            connects: AtomicU64::new(0),
            refusals,
            idle_support: AtomicU8::new(IDLE_UNKNOWN),
            idle_conn: tokio::sync::Mutex::new(None),
            idle_ahead_of: std::sync::Mutex::new(None),
            idle_connects: AtomicU64::new(0),
            idle_backoff: std::sync::Mutex::new(IdleBackoff::default()),
            last_wake: std::sync::Mutex::new(None),
            timeouts: Timeouts::default(),
        })
    }

    /// Shorter bounds, for the tests that wait on them.
    #[cfg(test)]
    pub(crate) fn with_timeouts(mut self, timeouts: Timeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    async fn connect_tcp(&self) -> Result<TcpStream> {
        let tcp = tokio::time::timeout(
            self.timeouts.connect,
            TcpStream::connect((self.server.host.as_str(), self.server.port)),
        )
        .await
        .map_err(|_| net_err("tcp connect", "timed out"))?
        .map_err(|e| net_err("tcp connect", e))?;
        if self.server.tls == ImapTls::Plain {
            // Belt and braces for the one cleartext case: whatever the name
            // resolved to, the socket we are about to send a password over
            // must end on this machine.
            let peer = tcp.peer_addr().map_err(|e| net_err("peer address", e))?;
            if !peer.ip().is_loopback() {
                return Err(CoreError::InvalidInput(
                    "imap: plaintext host resolved off this machine; refusing".into(),
                ));
            }
        }
        set_keepalive(&tcp);
        Ok(tcp)
    }

    async fn tls_wrap(&self, tcp: TcpStream) -> Result<Stream> {
        let name = ServerName::try_from(
            self.server
                .host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_string(),
        )
        .map_err(|e| net_err("tls server name", e))?;
        let tls = tokio::time::timeout(
            self.timeouts.connect,
            TlsConnector::from(self.tls.clone()).connect(name, tcp),
        )
        .await
        .map_err(|_| net_err("tls handshake", "timed out"))?
        .map_err(|e| net_err("tls handshake", e))?;
        Ok(Stream::Tls(Box::new(tls)))
    }

    /// Open, secure and authenticate one connection, then resolve roles.
    async fn connect(&self) -> Result<Conn> {
        let (mut session, caps) = self.open_session(Some(self.timeouts.stall), true).await?;
        let roles = list_roles(&mut session).await?;
        self.connects.fetch_add(1, Ordering::Relaxed);
        Ok(Conn {
            session,
            roles,
            condstore: caps.condstore,
            selected: None,
            calls: 0,
            busy: false,
        })
    }

    /// Open, secure and authenticate one connection: the command session's
    /// and the IDLE connection's shared first half. `stall` is the session's
    /// stall timer ([`Transport`]); the login conversation is bounded by
    /// [`Timeouts::login`] either way. A refused login counts in
    /// [`SourceRefusals::auth`] when `count_refusal` says so.
    async fn open_session(
        &self,
        stall: Option<Duration>,
        count_refusal: bool,
    ) -> Result<(ImapSession, Caps)> {
        tokio::time::timeout(
            self.timeouts.login,
            self.open_session_unbounded(stall, count_refusal),
        )
        .await
        .map_err(|_| broken_err("login", "the server did not finish logging in in time"))?
    }

    async fn open_session_unbounded(
        &self,
        stall: Option<Duration>,
        count_refusal: bool,
    ) -> Result<(ImapSession, Caps)> {
        let tcp = self.connect_tcp().await?;
        let mut client = match self.server.tls {
            ImapTls::Implicit => Client::new(Transport::new(self.tls_wrap(tcp).await?, stall)),
            ImapTls::Plain => Client::new(Transport::new(Stream::Plain(tcp), stall)),
            ImapTls::StartTls => {
                let mut plain = Client::new(Transport::new(Stream::Plain(tcp), stall));
                read_greeting(&mut plain).await?;
                plain
                    .run_command_and_check_ok("STARTTLS", None)
                    .await
                    .map_err(imap_err("starttls"))?;
                // `into_inner` drops anything buffered after the tagged OK, so
                // a response injected before the handshake is never read as
                // if it came over TLS.
                let Stream::Plain(tcp) = plain.into_inner().stream else {
                    unreachable!("built over a plain transport above");
                };
                Client::new(Transport::new(self.tls_wrap(tcp).await?, stall))
            }
        };
        if self.server.tls != ImapTls::StartTls {
            read_greeting(&mut client).await?;
        }
        let mut session = match client.login(&self.server.username, &self.password.0).await {
            Ok(s) => s,
            Err(_) => {
                // EVERY failure, whatever its kind (security review 6): the
                // server's text is dropped (some echo the username, and a
                // `BAD` may echo the whole command, password included), and
                // it counts as a refusal. None of it helps the user more than
                // this line does.
                if count_refusal {
                    self.refusals.auth.fetch_add(1, Ordering::Relaxed);
                }
                return Err(CoreError::Credential(LOGIN_REFUSED.into()));
            }
        };
        let caps = session
            .capabilities()
            .await
            .map_err(imap_err("capability"))?;
        let caps = Caps {
            condstore: caps.has_str("CONDSTORE"),
            idle: caps.has_str("IDLE"),
        };
        // Whichever connection learns it first, the source remembers it.
        self.idle_support.store(
            if caps.idle { IDLE_YES } else { IDLE_NO },
            Ordering::Relaxed,
        );
        Ok((session, caps))
    }

    /// Sit out what is left of the backoff after the last failed connect.
    async fn wait_out_backoff(&self) {
        let wait = self
            .backoff
            .lock()
            .map(|b| b.remaining())
            .unwrap_or_default();
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }

    fn note_connect(&self, ok: bool) {
        if let Ok(mut b) = self.backoff.lock() {
            if ok {
                *b = Backoff::default();
            } else {
                b.failures = b.failures.saturating_add(1);
                b.last_failure = Some(tokio::time::Instant::now());
            }
        }
    }

    /// Run `f` on the command connection, connecting first if needed.
    ///
    /// The session is kept across errors that are about one command (a
    /// `NO`, a message that is not there, a store error) and retired on one
    /// that leaves the stream in an unknown place ([`is_session_broken`]), or
    /// when the previous caller was dropped mid-command (`busy`).
    async fn with_conn<T>(
        &self,
        f: impl for<'c> FnOnce(
            &'c mut Conn,
        )
            -> Pin<Box<dyn std::future::Future<Output = Result<T>> + Send + 'c>>,
    ) -> Result<T> {
        let mut guard = self.conn.lock().await;
        if guard.as_ref().is_some_and(|c| c.busy) {
            *guard = None;
        }
        if guard.is_none() {
            self.wait_out_backoff().await;
            let conn = self.connect().await;
            self.note_connect(conn.is_ok());
            *guard = Some(conn?);
        }
        let conn = guard.as_mut().expect("set above");
        conn.busy = true;
        conn.calls = conn.calls.wrapping_add(1);
        let out = f(conn).await;
        match &out {
            Err(e) if is_session_broken(e) => *guard = None,
            _ => {
                if let Some(c) = guard.as_mut() {
                    c.busy = false;
                }
            }
        }
        out
    }

    fn record(&self, seen: &[Seen]) -> Result<()> {
        for s in seen {
            if let Some(d) = &s.digest {
                self.locations
                    .record_content_digest(self.account_id, &s.id, d)?;
            }
        }
        let rows: Vec<(String, MessageLocation)> =
            seen.iter().map(|s| (s.id.clone(), s.loc.clone())).collect();
        self.locations
            .record_message_locations(self.account_id, &rows)
    }

    /// The roles as of the current connection (connecting if needed).
    #[cfg(test)]
    pub(crate) async fn roles(&self) -> Result<FolderRoles> {
        self.with_conn(|c| Box::pin(async move { Ok(c.roles.clone()) }))
            .await
    }

    /// Successful logins so far.
    #[cfg(test)]
    pub(crate) fn connects(&self) -> u64 {
        self.connects.load(Ordering::Relaxed)
    }

    /// One raw command on the command session, for the tests that need a
    /// command the server refuses.
    #[cfg(test)]
    pub(crate) async fn run_command_for_test(&self, cmd: &'static str) -> Result<()> {
        self.with_conn(|c| {
            Box::pin(async move {
                c.selected = None;
                c.session
                    .run_command_and_check_ok(cmd)
                    .await
                    .map_err(imap_err("test command"))
            })
        })
        .await
    }
}

async fn read_greeting(client: &mut Client<Transport>) -> Result<()> {
    match client.read_response().await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(broken_err("greeting", "connection closed")),
        Err(e) => Err(broken_err("greeting", e)),
    }
}

async fn list_roles(session: &mut ImapSession) -> Result<FolderRoles> {
    let names: Vec<(String, Vec<String>)> = session
        .list(Some(""), Some("*"))
        .await
        .map_err(imap_err("list"))?
        .map_ok(|n| {
            (
                n.name().to_string(),
                n.attributes().iter().map(attr_text).collect(),
            )
        })
        .try_collect()
        .await
        .map_err(imap_err("list"))?;
    Ok(resolve_roles(&names))
}

/// IMAP's `SEARCH` date (`DD-Mon-YYYY`, English month).
fn imap_date(t: DateTime<Utc>) -> String {
    t.format("%d-%b-%Y").to_string()
}

fn uid_set(uids: &[u32]) -> String {
    uids.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// `UID SEARCH` in the open folder, as a sorted list.
async fn search(session: &mut ImapSession, query: String) -> Result<Vec<u32>> {
    let mut uids: Vec<u32> = session
        .uid_search(query)
        .await
        .map_err(imap_err("search"))?
        .into_iter()
        .collect();
    uids.sort_unstable();
    Ok(uids)
}

/// THE ONLY WAY A FETCH'S RESPONSES ARE COLLECTED (security review 4): one
/// per UID asked for, at most. A response for a UID not asked for, or a
/// second one for the same UID, is read and dropped (a flag change another
/// client made can ride along), and a server that keeps sending past a
/// generous slack is cut off with the session, so memory is bounded by what
/// was asked for, not by what the server feels like sending.
async fn collect_fetches<S>(stream: S, wanted: &[u32], context: &'static str) -> Result<Vec<Fetch>>
where
    S: futures::Stream<Item = async_imap::error::Result<Fetch>>,
{
    use futures::StreamExt;
    let wanted_set: HashSet<u32> = wanted.iter().copied().collect();
    let limit = wanted.len().saturating_mul(2).saturating_add(16);
    let mut stream = std::pin::pin!(stream);
    let mut got: HashSet<u32> = HashSet::with_capacity(wanted.len());
    let mut out = Vec::with_capacity(wanted.len());
    let mut seen = 0usize;
    while let Some(f) = stream.next().await {
        let f = f.map_err(imap_err(context))?;
        seen += 1;
        if seen > limit {
            return Err(broken_err(
                context,
                "the server sent far more responses than were asked for",
            ));
        }
        if let Some(uid) = f.uid
            && wanted_set.contains(&uid)
            && got.insert(uid)
        {
            out.push(f);
        }
    }
    Ok(out)
}

/// The stable id of each UID in `uids` (in `folder`, open, at `uidvalidity`),
/// with the normalized Message-ID and the INTERNALDATE. Header-only fetches,
/// `BODY.PEEK`, one round trip per 500 UIDs.
async fn stable_ids(
    session: &mut ImapSession,
    folder: &str,
    uidvalidity: u32,
    uids: &[u32],
) -> Result<Vec<Seen>> {
    let mut out = Vec::with_capacity(uids.len());
    for chunk in uids.chunks(500) {
        let stream = session
            .uid_fetch(
                uid_set(chunk),
                format!("(UID INTERNALDATE BODY.PEEK[HEADER.FIELDS ({ID_HEADER_FIELDS})])"),
            )
            .await
            .map_err(imap_err("fetch ids"))?;
        let fetches = collect_fetches(stream, chunk, "fetch ids").await?;
        for f in fetches {
            let (Some(uid), Some(h)) = (f.uid, f.header()) else {
                continue;
            };
            let (id, message_id) = stable_id_for_header_block(h);
            out.push(Seen {
                id,
                loc: MessageLocation {
                    folder: folder.to_string(),
                    uidvalidity,
                    uid,
                    message_id,
                },
                internal_date: f.internal_date().map(|d| d.with_timezone(&Utc)),
                digest: None,
            });
        }
    }
    Ok(out)
}

/// One message's bytes by UID in the open folder, or `None` if the UID is
/// gone. `BODY.PEEK[]`: `\Seen` is not touched.
///
/// THE SIZE IS ASKED FIRST, on its own (security review 4): a message over
/// the 64 MB ingest bound is refused before a byte of it is downloaded, and
/// the bound is checked again on what actually arrived, so a server that
/// understates the size gains nothing.
async fn fetch_body(
    session: &mut ImapSession,
    uid: u32,
) -> Result<Option<(Vec<u8>, Option<DateTime<Utc>>)>> {
    let stream = session
        .uid_fetch(uid.to_string(), "(UID RFC822.SIZE)")
        .await
        .map_err(imap_err("fetch size"))?;
    let Some(sized) = collect_fetches(stream, &[uid], "fetch size")
        .await?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    if sized.size.is_some_and(|s| s > MAX_RAW_BYTES) {
        return Err(too_big());
    }
    // A partial fetch one byte past the bound: a server keeping to RFC 3501
    // cannot send more, and one byte over is how "too big" shows.
    let stream = session
        .uid_fetch(
            uid.to_string(),
            format!("(UID INTERNALDATE BODY.PEEK[]<0.{}>)", MAX_RAW_BYTES + 1),
        )
        .await
        .map_err(imap_err("fetch body"))?;
    let Some(f) = collect_fetches(stream, &[uid], "fetch body")
        .await?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let body = f.body().unwrap_or_default();
    if body.len() > MAX_RAW_BYTES as usize {
        return Err(too_big());
    }
    let received = f.internal_date().map(|d| d.with_timezone(&Utc));
    Ok(Some((body.to_vec(), received)))
}

fn too_big() -> CoreError {
    CoreError::InvalidInput("raw message exceeds the 64MB ingest bound".to_string())
}

/// Is `uid` still in the open folder?
async fn uid_exists(session: &mut ImapSession, uid: u32) -> Result<bool> {
    let stream = session
        .uid_fetch(uid.to_string(), "(UID)")
        .await
        .map_err(imap_err("fetch uid"))?;
    Ok(!collect_fetches(stream, &[uid], "fetch uid")
        .await?
        .is_empty())
}

/// The [`content_digest`] of the message at `uid` in `folder`, if the folder
/// is still at `uidvalidity` and the UID is there. Header fields and
/// `BODY.PEEK[TEXT]`, the text capped one byte past the ingest bound (the
/// digest of an oversized message covers its first bytes: deterministic, and
/// ingest refuses it anyway).
async fn location_digest(
    c: &mut Conn,
    folder: &str,
    uidvalidity: u32,
    uid: u32,
) -> Result<Option<String>> {
    if c.examine(folder).await? != Some(uidvalidity) {
        return Ok(None);
    }
    let stream = c
        .session
        .uid_fetch(
            uid.to_string(),
            format!(
                "(UID BODY.PEEK[HEADER.FIELDS ({DIGEST_HEADER_FIELDS})] BODY.PEEK[TEXT]<0.{}>)",
                MAX_RAW_BYTES + 1
            ),
        )
        .await
        .map_err(imap_err("fetch digest"))?;
    Ok(collect_fetches(stream, &[uid], "fetch digest")
        .await?
        .into_iter()
        .next()
        .map(|f| content_digest(f.header().unwrap_or_default(), text_section(&f))))
}

/// `BODY[TEXT]`, whole or partial (`BODY[TEXT]<0>`), from a fetch.
fn text_section(f: &Fetch) -> &[u8] {
    f.text()
        .or_else(|| {
            f.section(&async_imap::imap_proto::SectionPath::Full(
                async_imap::imap_proto::MessageSection::Text,
            ))
        })
        .unwrap_or_default()
}

/// Same folder, same `UIDVALIDITY`, same UID: the very copy already known.
fn same_place(a: &MessageLocation, b: &MessageLocation) -> bool {
    a.folder == b.folder && a.uidvalidity == b.uidvalidity && a.uid == b.uid
}

/// A RE-SIGHTING MUST PROVE IT IS THE SAME MESSAGE (Amendment I). `seen` was
/// just listed in its folder. A sighting of an id nobody has seen, or of a
/// place already on record, passes as it is: the common case costs nothing.
/// Any other sighting of a known id (a move, a self-sent twin, or a stranger
/// reusing the Message-ID) has its digest read and compared with the id's:
/// equal, it is that message at a new place; different, it is another
/// message and gets [`distinct_stable_id`], so the stored row is never
/// overwritten with its content.
///
/// The id's digest is the recorded one, or, before any body was fetched, the
/// digest of the earliest place still holding it (recorded then, so it
/// cannot drift).
async fn bind_identities(
    c: &mut Conn,
    locs: &dyn MessageLocations,
    account: AccountId,
    seen: Vec<Seen>,
) -> Result<Vec<Seen>> {
    // First sightings in this batch, so a duplicate WITHIN it is checked too.
    let mut first_here: std::collections::HashMap<String, MessageLocation> =
        std::collections::HashMap::new();
    let mut out = Vec::with_capacity(seen.len());
    for mut s in seen {
        let rows = locs.message_locations(account, &s.id)?;
        if rows.iter().any(|r| same_place(r, &s.loc)) {
            out.push(s);
            continue;
        }
        let earlier = first_here.get(&s.id).cloned();
        if rows.is_empty() && earlier.is_none() {
            first_here.insert(s.id.clone(), s.loc.clone());
            out.push(s);
            continue;
        }
        let mut reference = locs.content_digest(account, &s.id)?;
        if reference.is_none() {
            // Oldest first: the place the id was bound at.
            for r in earlier.iter().chain(rows.iter().rev()) {
                if let Some(d) = location_digest(c, &r.folder, r.uidvalidity, r.uid).await? {
                    locs.record_content_digest(account, &s.id, &d)?;
                    reference = Some(d);
                    break;
                }
            }
        }
        let Some(reference) = reference else {
            // Every earlier place is gone and nothing was ever read: this
            // copy is the only one there is, and it binds the id.
            first_here.insert(s.id.clone(), s.loc.clone());
            out.push(s);
            continue;
        };
        let Some(digest) =
            location_digest(c, &s.loc.folder.clone(), s.loc.uidvalidity, s.loc.uid).await?
        else {
            // Gone between the listing and now.
            continue;
        };
        if digest != reference {
            s.id = distinct_stable_id(&s.id, s.loc.message_id.as_deref(), &digest);
        }
        s.digest = Some(digest);
        out.push(s);
    }
    Ok(out)
}

/// Where a message with this Message-ID is, for a location that has gone
/// stale (contract §6), searching `folders` in order: at most one place per
/// folder.
///
/// EXACT, NOT A SUBSTRING (security review 2): `SEARCH HEADER` matches any
/// header CONTAINING the text, so the search is for `<id>` with its brackets,
/// each hit's Message-ID is read back and must normalize to exactly
/// `message_id`, and when the id has a recorded digest the hit's must match
/// it too. The LOWEST matching UID wins: a lookalike delivered later sits
/// above the real one, never below it.
async fn find_by_message_id(
    c: &mut Conn,
    folders: &[String],
    message_id: &str,
    expected: Option<&str>,
) -> Result<Vec<MessageLocation>> {
    // The value goes inside a quoted string: refuse anything that could end
    // it or the command early.
    if message_id.contains(['"', '\\', '\r', '\n', '<', '>']) {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for folder in folders {
        let Some(uidvalidity) = c.examine(folder).await? else {
            continue;
        };
        let hits = search(
            &mut c.session,
            format!("HEADER Message-ID \"<{message_id}>\""),
        )
        .await?;
        if hits.is_empty() {
            continue;
        }
        let stream = c
            .session
            .uid_fetch(
                uid_set(&hits),
                "(UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])",
            )
            .await
            .map_err(imap_err("fetch message-id"))?;
        let mut exact: Vec<u32> = collect_fetches(stream, &hits, "fetch message-id")
            .await?
            .into_iter()
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
        for uid in exact {
            if let Some(want) = expected
                && location_digest(c, folder, uidvalidity, uid)
                    .await?
                    .as_deref()
                    != Some(want)
            {
                continue;
            }
            out.push(MessageLocation {
                folder: folder.clone(),
                uidvalidity,
                uid,
                message_id: Some(message_id.to_string()),
            });
            break;
        }
    }
    Ok(out)
}

/// Does an untagged response seen during IDLE mean INBOX changed? An
/// arrival (`EXISTS`), a removal (`EXPUNGE`, or `VANISHED` under QRESYNC),
/// or a flag change (`FETCH`). `RECENT` always rides with an `EXISTS`.
fn is_mailbox_change(r: &async_imap::imap_proto::Response<'_>) -> bool {
    use async_imap::imap_proto::{MailboxDatum, Response};
    matches!(
        r,
        Response::MailboxData(MailboxDatum::Exists(_))
            | Response::Expunge(_)
            | Response::Vanished { .. }
            | Response::Fetch(..)
    )
}

/// Read everything queued on a session's unsolicited channel (what arrived
/// outside a command's own reply) and say whether any of it is a change to
/// the open folder.
fn drain_changes(mut next: impl FnMut() -> Option<async_imap::types::UnsolicitedResponse>) -> bool {
    use async_imap::types::UnsolicitedResponse as U;
    let mut changed = false;
    while let Some(u) = next() {
        changed |= match u {
            U::Exists(_) | U::Expunge(_) => true,
            U::Other(r) => is_mailbox_change(r.parsed()),
            _ => false,
        };
    }
    changed
}

/// Where the rows say a message is: in the inbox if any row is INBOX, sent
/// if any row is the sent folder.
fn placement_for(roles: &FolderRoles, rows: &[MessageLocation]) -> Placement {
    Placement {
        in_inbox: rows.iter().any(|r| r.folder.eq_ignore_ascii_case(INBOX)),
        in_sent: rows
            .iter()
            .any(|r| roles.sent.as_deref() == Some(r.folder.as_str())),
    }
}

/// What resolving one id's rows found.
struct Resolved<T> {
    /// The rows that checked out, plus any the search fallback found.
    live: Vec<MessageLocation>,
    /// The rows to drop: gone, or holding other content than the id's.
    stale: Vec<MessageLocation>,
    /// Rows the search fallback found, to record.
    found: Vec<MessageLocation>,
    got: Option<T>,
    /// The digest this resolve bound the id to, when none was recorded and
    /// it read the message's bytes: recorded by `settle`.
    bound: Option<String>,
}

/// Walk `rows` (then the Message-ID search) until `fetch` returns something
/// from one of them, checking EVERY row so placement is from live rows only.
/// `fetch` runs in the opened folder and returns `None` for a UID that is
/// gone.
///
/// ONLY THE ID'S OWN CONTENT IS ACCEPTED (Amendment I): with a digest
/// recorded (`expected`), a row whose message does not match it is stale, its
/// bytes are not returned, and it does not count toward placement.
/// `digest_of` reads the digest from what `fetch` returned when it can (the
/// whole message), else it is read from the server. With none recorded, the
/// first bytes read bind it.
///
/// Last resort (correctness M3): a message moved out of every folder the
/// search knows about (into a user folder, before its first fetch) is looked
/// for in every selectable folder, so it is not lost to the live path.
async fn resolve<T, F>(
    c: &mut Conn,
    rows: Vec<MessageLocation>,
    mut expected: Option<String>,
    digest_of: fn(&T) -> Option<String>,
    mut fetch: F,
) -> Result<Resolved<T>>
where
    T: Send,
    F: for<'s> FnMut(
            &'s mut ImapSession,
            u32,
        )
            -> Pin<Box<dyn std::future::Future<Output = Result<Option<T>>> + Send + 's>>
        + Send,
{
    let message_id = rows.iter().find_map(|r| r.message_id.clone());
    let mut out = Resolved {
        live: Vec::new(),
        stale: Vec::new(),
        found: Vec::new(),
        got: None,
        bound: None,
    };
    for row in rows {
        if c.examine(&row.folder).await? != Some(row.uidvalidity) {
            out.stale.push(row);
            continue;
        }
        let here = if out.got.is_none() {
            match fetch(&mut c.session, row.uid).await? {
                Some(v) => {
                    let d = match digest_of(&v) {
                        Some(d) => Some(d),
                        None if expected.is_some() => {
                            location_digest(c, &row.folder, row.uidvalidity, row.uid).await?
                        }
                        None => None,
                    };
                    match (&expected, d) {
                        (Some(want), Some(d)) if *want == d => {
                            out.got = Some(v);
                            true
                        }
                        (Some(_), _) => false,
                        (None, d) => {
                            if d.is_some() {
                                out.bound = d.clone();
                                expected = d;
                            }
                            out.got = Some(v);
                            true
                        }
                    }
                }
                None => false,
            }
        } else if let Some(want) = &expected {
            location_digest(c, &row.folder, row.uidvalidity, row.uid)
                .await?
                .as_deref()
                == Some(want.as_str())
        } else {
            uid_exists(&mut c.session, row.uid).await?
        };
        if here {
            out.live.push(row);
        } else {
            out.stale.push(row);
        }
    }
    if out.got.is_none()
        && let Some(mid) = message_id
    {
        let known: Vec<String> = c
            .roles
            .search_folders()
            .into_iter()
            .map(str::to_string)
            .collect();
        let mut hits = find_by_message_id(c, &known, &mid, expected.as_deref()).await?;
        if hits.is_empty() {
            // Listed afresh: a folder made since this session logged in is
            // exactly where a message may have just been filed. The roles
            // are left as they are.
            let rest: Vec<String> = list_roles(&mut c.session)
                .await?
                .selectable
                .iter()
                .filter(|f| !known.contains(f))
                .cloned()
                .collect();
            hits = find_by_message_id(c, &rest, &mid, expected.as_deref()).await?;
        }
        for loc in hits {
            if out.got.is_none() && c.examine(&loc.folder).await? == Some(loc.uidvalidity) {
                out.got = fetch(&mut c.session, loc.uid).await?;
                if expected.is_none() {
                    out.bound = out.got.as_ref().and_then(digest_of);
                    expected = out.bound.clone();
                }
            }
            out.live.push(loc.clone());
            out.found.push(loc);
        }
    }
    Ok(out)
}

/// What one `changes` walk found on the session, before the source records it.
enum Walk {
    /// The cursor cannot be replayed from (Amendment D / contract §7).
    Expired,
    /// The role has no folder on this server, now or at the cursor.
    NoFolder,
    Moved {
        pos: FolderPos,
        seen: Vec<Seen>,
        mailbox_unchanged: bool,
    },
}

/// Does every folder the poll lane walks, other than `skip`, stand exactly
/// where `start` left it? A role that gained, lost or changed its folder is a
/// change.
///
/// Not Junk (correctness M1): the poll lane never walks it, so its entry in
/// the cursor stays where the last catch-up left it, and the first junk mail
/// after that would read as a change on every tick for good.
async fn others_unchanged(c: &mut Conn, start: &ImapCursor, skip: Folder) -> Result<bool> {
    for folder in [Folder::Inbox, Folder::Sent] {
        if folder == skip {
            continue;
        }
        let name = c.roles.folder(folder).map(str::to_string);
        let pos = start.folders.get(role_key(folder));
        match (name, pos) {
            (None, None) => {}
            (Some(name), Some(pos)) if name == pos.mailbox => match c.status(&name).await? {
                Some(now) if pos.unchanged_at(&now) => {}
                _ => return Ok(false),
            },
            _ => return Ok(false),
        }
    }
    // An entry for a role this build does not know is ignored: it cannot be
    // walked, so it cannot be missed.
    Ok(true)
}

#[async_trait::async_trait]
impl MailSource for ImapSource {
    fn provider(&self) -> MailProvider {
        MailProvider::Imap
    }

    async fn list(
        &self,
        listing: Listing,
        cap: Option<usize>,
        _urgency: Urgency,
    ) -> Result<Vec<String>> {
        let (locs, account) = (self.locations.clone(), self.account_id);
        let (seen, snapshot) = match listing {
            Listing::Folder {
                folder,
                within_days,
            } => {
                self.with_conn(|c| {
                    Box::pin(async move {
                        // THE SNAPSHOT FIRST (Amendment G): anything that lands
                        // after it is above its `last_uid`, so the next walk
                        // finds it whether or not this search does.
                        let Some((name, snapshot)) = c.role_status(folder).await? else {
                            // No such folder on this server (no Junk, say, or
                            // a Sent folder deleted elsewhere): an empty
                            // listing, the same as an empty folder.
                            return Ok((Vec::new(), Some((folder, None))));
                        };
                        let snapshot = Some(snapshot);
                        let Some(validity) = c.examine(&name).await? else {
                            return Ok((Vec::new(), Some((folder, None))));
                        };
                        let query = match within_days {
                            Some(days) => format!(
                                "SINCE {}",
                                imap_date(Utc::now() - chrono::Duration::days(i64::from(days)))
                            ),
                            None => "ALL".to_string(),
                        };
                        let mut uids = search(&mut c.session, query).await?;
                        // Newest first: UIDs ascend with arrival in a folder.
                        uids.reverse();
                        if let Some(cap) = cap {
                            uids.truncate(cap);
                        }
                        let seen = stable_ids(&mut c.session, &name, validity, &uids).await?;
                        let mut seen = bind_identities(c, locs.as_ref(), account, seen).await?;
                        seen.sort_unstable_by(|a, b| b.loc.uid.cmp(&a.loc.uid));
                        Ok((seen, Some((folder, snapshot))))
                    })
                })
                .await?
            }
            Listing::Window {
                from,
                to,
                filed_only,
            } => {
                let seen = self
                    .with_conn(|c| {
                        Box::pin(async move {
                            // SINCE/BEFORE compare the date part of INTERNALDATE
                            // in its own zone (RFC 3501), so the search is a day
                            // wider than the window on each side and the exact
                            // edge is the INTERNALDATE filter below.
                            let query = format!(
                                "SINCE {} BEFORE {}",
                                imap_date(from - chrono::Duration::days(1)),
                                imap_date(to + chrono::Duration::days(2))
                            );
                            let folders: Vec<String> = c
                                .roles
                                .window_folders(filed_only)
                                .into_iter()
                                .map(str::to_string)
                                .collect();
                            let mut all = Vec::new();
                            for folder in folders {
                                let Some(validity) = c.examine(&folder).await? else {
                                    continue;
                                };
                                let uids = search(&mut c.session, query.clone()).await?;
                                let seen =
                                    stable_ids(&mut c.session, &folder, validity, &uids).await?;
                                let seen = bind_identities(c, locs.as_ref(), account, seen).await?;
                                all.extend(seen.into_iter().filter(|s| {
                                    s.internal_date.is_some_and(|d| d >= from && d < to)
                                }));
                            }
                            all.sort_by(|a, b| b.internal_date.cmp(&a.internal_date));
                            Ok(all)
                        })
                    })
                    .await?;
                (seen, None)
            }
        };
        self.record(&seen)?;
        if let Some((folder, snapshot)) = snapshot
            && let Ok(mut snaps) = self.snapshots.lock()
        {
            match snapshot {
                Some(pos) => snaps.insert(role_key(folder).to_string(), pos),
                None => snaps.remove(role_key(folder)),
            };
        }
        let mut ids_seen = HashSet::new();
        let mut out = Vec::new();
        for s in seen {
            // Two copies of one message (two folders, or twice in one) are
            // one id.
            if ids_seen.insert(s.id.clone()) {
                out.push(s.id);
            }
        }
        if let Some(cap) = cap {
            out.truncate(cap);
        }
        Ok(out)
    }

    /// The snapshots this pass's listings took, and live `STATUS` for any
    /// watched folder that was not listed (Amendment G).
    async fn current_cursor(&self) -> Result<SyncCursor> {
        let snaps = self.snapshots.lock().map(|s| s.clone()).unwrap_or_default();
        let used = snaps.clone();
        let folders = self
            .with_conn(|c| {
                Box::pin(async move {
                    let mut folders = BTreeMap::new();
                    for folder in WATCHED {
                        let key = role_key(folder);
                        let snap = snaps
                            .get(key)
                            .filter(|snap| c.roles.folder(folder) == Some(snap.mailbox.as_str()));
                        let pos = match snap {
                            Some(snap) => snap.clone(),
                            // A role folder gone since (deleted or renamed in
                            // another client, correctness H2) is re-resolved,
                            // or left out: `changes` reads a role with no
                            // entry and no folder as nothing to walk.
                            None => match c.role_status(folder).await? {
                                Some((_, pos)) => pos,
                                None => continue,
                            },
                        };
                        folders.insert(key.to_string(), pos);
                    }
                    Ok(folders)
                })
            })
            .await?;
        // Spent: drop exactly the snapshots used, so one a concurrent listing
        // took meanwhile is kept for its own pass.
        if let Ok(mut s) = self.snapshots.lock() {
            for (key, pos) in &used {
                if s.get(key) == Some(pos) {
                    s.remove(key);
                }
            }
        }
        Ok(ImapCursor { v: 1, folders }.encode())
    }

    fn cursor_is_usable(&self, cursor: &SyncCursor) -> bool {
        ImapCursor::decode(cursor).is_some_and(|c| c.folders.contains_key(role_key(Folder::Inbox)))
    }

    async fn changes(
        &self,
        since: &SyncCursor,
        folder: Folder,
    ) -> std::result::Result<Changes, ChangesError> {
        let Some(start) = ImapCursor::decode(since) else {
            return Err(ChangesError::Expired);
        };
        let key = role_key(folder);
        let start_for_walk = start.clone();
        let (locs, account) = (self.locations.clone(), self.account_id);
        let walk = self
            .with_conn(|c| {
                Box::pin(async move {
                    let start = start_for_walk;
                    let name = c.roles.folder(folder).map(str::to_string);
                    let (name, pos) = match (name, start.folders.get(key)) {
                        (None, None) => return Ok(Walk::NoFolder),
                        (Some(name), Some(pos)) if name == pos.mailbox => (name, pos.clone()),
                        // The role gained, lost or changed its folder: the
                        // position means nothing in the new one.
                        _ => return Ok(Walk::Expired),
                    };
                    let Some(now) = c.status(&name).await? else {
                        // Gone (correctness H2): list the roles again, so the
                        // catch-up this sends the engine to sees the folders
                        // as they are now, not as they were at login.
                        c.refresh_roles().await?;
                        return Ok(Walk::Expired);
                    };
                    if now.uidvalidity != pos.uidvalidity {
                        return Ok(Walk::Expired);
                    }
                    let mut last_uid = pos.last_uid.max(now.last_uid);
                    let mut seen = Vec::new();
                    if now.last_uid > pos.last_uid {
                        if c.examine(&name).await? != Some(pos.uidvalidity) {
                            return Ok(Walk::Expired);
                        }
                        // `n:*` always matches the highest UID even when it is
                        // below n, hence the filter.
                        let uids: Vec<u32> =
                            search(&mut c.session, format!("UID {}:*", pos.last_uid + 1))
                                .await?
                                .into_iter()
                                .filter(|u| *u > pos.last_uid)
                                .collect();
                        last_uid = last_uid.max(uids.last().copied().unwrap_or(0));
                        let ids = stable_ids(&mut c.session, &name, pos.uidvalidity, &uids).await?;
                        seen = bind_identities(c, locs.as_ref(), account, ids).await?;
                    }
                    let mailbox_unchanged = seen.is_empty()
                        && pos.unchanged_at(&now)
                        && others_unchanged(c, &start, folder).await?;
                    Ok(Walk::Moved {
                        pos: FolderPos {
                            mailbox: name,
                            uidvalidity: pos.uidvalidity,
                            last_uid,
                            highestmodseq: now.highestmodseq,
                        },
                        seen,
                        mailbox_unchanged,
                    })
                })
            })
            .await
            .map_err(ChangesError::Other)?;
        match walk {
            Walk::Expired => Err(ChangesError::Expired),
            Walk::NoFolder => Ok(Changes {
                ids: Vec::new(),
                cursor: since.clone(),
                mailbox_unchanged: false,
            }),
            Walk::Moved {
                pos,
                seen,
                mailbox_unchanged,
            } => {
                self.record(&seen).map_err(ChangesError::Other)?;
                let mut ids: Vec<String> = seen.into_iter().map(|s| s.id).collect();
                ids.sort_unstable();
                ids.dedup();
                let mut cursor = start;
                cursor.folders.insert(key.to_string(), pos);
                Ok(Changes {
                    ids,
                    cursor: cursor.encode(),
                    mailbox_unchanged,
                })
            }
        }
    }

    fn merge_cursors(&self, since: &SyncCursor, a: &SyncCursor, b: &SyncCursor) -> SyncCursor {
        match (ImapCursor::decode(a), ImapCursor::decode(b)) {
            (Some(x), Some(y)) => {
                merge_cursor_values(ImapCursor::decode(since).as_ref(), &x, &y).encode()
            }
            // A side this source cannot read cannot be merged: keep the one
            // it can (an unreadable cursor later sends the engine to a
            // catch-up, which is the safe direction).
            (Some(_), None) => a.clone(),
            (None, _) => b.clone(),
        }
    }

    async fn fetch_raw(&self, id: &str, _urgency: Urgency) -> Result<FetchedMessage> {
        let rows = self.locations.message_locations(self.account_id, id)?;
        if rows.is_empty() {
            // Never seen by this source: there is nowhere to look.
            return Err(CoreError::NotFound);
        }
        let expected = self.locations.content_digest(self.account_id, id)?;
        let (resolved, placement) = self
            .with_conn(|c| {
                Box::pin(async move {
                    let r = resolve(
                        c,
                        rows,
                        expected,
                        |(bytes, _): &(Vec<u8>, _)| {
                            (!bytes.is_empty()).then(|| content_digest_of_raw(bytes))
                        },
                        |s, uid| Box::pin(fetch_body(s, uid)),
                    )
                    .await?;
                    let placement = placement_for(&c.roles, &r.live);
                    Ok((r, placement))
                })
            })
            .await?;
        self.settle(id, &resolved)?;
        let (raw, received_at) = match resolved.got {
            Some((bytes, received)) if !bytes.is_empty() => (RawBody::Bytes(bytes), received),
            Some((_, received)) => (RawBody::Missing, received),
            // Seen once, gone from every watched folder now: nothing to
            // ingest, which is not an error for the batch it is in.
            None => (RawBody::Missing, None),
        };
        Ok(FetchedMessage {
            id: id.to_string(),
            thread_id: None,
            raw,
            received_at,
            placement,
        })
    }

    async fn fetch_headers(&self, id: &str, names: &[&str]) -> Result<FetchedHeaders> {
        // Header names go into the command; refuse anything that is not a
        // plain field name.
        if names
            .iter()
            .any(|n| n.is_empty() || !n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        {
            return Err(CoreError::InvalidInput("imap: bad header name".into()));
        }
        let rows = self.locations.message_locations(self.account_id, id)?;
        if rows.is_empty() {
            return Err(CoreError::NotFound);
        }
        let query = format!(
            "(UID INTERNALDATE BODY.PEEK[HEADER.FIELDS ({})])",
            names.join(" ")
        );
        let expected = self.locations.content_digest(self.account_id, id)?;
        let resolved = self
            .with_conn(|c| {
                Box::pin(async move {
                    resolve(
                        c,
                        rows,
                        expected,
                        |_| None,
                        move |s, uid| {
                            let query = query.clone();
                            Box::pin(async move {
                                let stream = s
                                    .uid_fetch(uid.to_string(), query)
                                    .await
                                    .map_err(imap_err("fetch headers"))?;
                                let fetches =
                                    collect_fetches(stream, &[uid], "fetch headers").await?;
                                Ok(fetches.into_iter().find(|f| f.uid == Some(uid)).map(|f| {
                                    FetchedHeaders {
                                        headers: f
                                            .header()
                                            .map(parse_header_block)
                                            .unwrap_or_default(),
                                        received_at: f
                                            .internal_date()
                                            .map(|d| d.with_timezone(&Utc)),
                                    }
                                }))
                            })
                        },
                    )
                    .await
                })
            })
            .await?;
        self.settle(id, &resolved)?;
        resolved.got.ok_or(CoreError::NotFound)
    }

    async fn inbox_unread(&self) -> Result<(i64, i64)> {
        self.with_conn(|c| {
            Box::pin(async move {
                // Not from a stale view of an open INBOX (correctness M5).
                c.close_if_open(INBOX).await?;
                let st = c
                    .session
                    .status(INBOX, "(MESSAGES UNSEEN)")
                    .await
                    .map_err(imap_err("status"))?;
                let unseen = i64::from(
                    st.unseen
                        .ok_or_else(|| net_err("status", "server omitted UNSEEN"))?,
                );
                // IMAP has no thread count: the message count stands in for
                // both, as the Graph mapping does.
                Ok((unseen, unseen))
            })
        })
        .await
    }

    /// IDLE on INBOX until it changes or `max_wait` passes (contract §8).
    ///
    /// An IDLE connection that fails (a hang-up, a refused login) is tried
    /// again within the same wait, after a backoff ([`IdleBackoff`]), so mail
    /// that lands after a server restart wakes the engine in seconds rather
    /// than at the next poll. Every retry waits at least a second and the
    /// waits double, so it never spins.
    async fn wait_for_change(&self, cursor: Option<&SyncCursor>, max_wait: Duration) {
        let deadline = tokio::time::Instant::now() + max_wait;
        loop {
            if self.idle_support.load(Ordering::Relaxed) == IDLE_NO {
                tokio::time::sleep_until(deadline).await;
                return;
            }
            let pause = self.idle_retry_pause();
            if !pause.is_zero() {
                if tokio::time::Instant::now() + pause >= deadline {
                    tokio::time::sleep_until(deadline).await;
                    return;
                }
                tokio::time::sleep(pause).await;
            }
            // BOUNDED HOWEVER THE SERVER BEHAVES: every step of the wait
            // (login, EXAMINE, IDLE, DONE) is a read that a silent server
            // never answers. Timing out drops the session with the future,
            // which is the cancel-safe path `idle_until` is built for; the
            // next wait logs in afresh.
            match tokio::time::timeout_at(deadline + IDLE_GRACE, self.idle_until(cursor, deadline))
                .await
            {
                Ok(Ok(early)) => {
                    self.note_idle(true);
                    if early {
                        self.space_out_wake(deadline).await;
                    }
                    return;
                }
                Err(_) => {
                    self.note_idle(false);
                    eprintln!(
                        "squelch: imap idle got no answer from the server by the end of the \
                         wait; reconnecting next tick"
                    );
                    return;
                }
                Ok(Err(e)) => {
                    // The error is about the transport, never the password.
                    if self.note_idle(false) {
                        eprintln!("squelch: imap idle unavailable ({e}); retrying with backoff");
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return;
                    }
                }
            }
        }
    }
}

impl ImapSource {
    /// How long the next IDLE login waits: its own backoff, or the command
    /// session's if that is longer (a server the command session cannot
    /// reach is not asked twice as often for IDLE).
    fn idle_retry_pause(&self) -> Duration {
        let idle = self
            .idle_backoff
            .lock()
            .map(|b| b.backoff.remaining())
            .unwrap_or_default();
        let command = self
            .backoff
            .lock()
            .map(|b| b.remaining())
            .unwrap_or_default();
        idle.max(command)
    }

    /// Book one IDLE outcome. A clean wait ends the run of failures; a
    /// failure extends it. True when the failure is worth a log line: the
    /// first of a run, then each time the backoff doubles past a power of
    /// two, so a server down for an hour is a handful of lines.
    fn note_idle(&self, ok: bool) -> bool {
        let Ok(mut b) = self.idle_backoff.lock() else {
            return true;
        };
        if ok {
            *b = IdleBackoff::default();
            return false;
        }
        b.backoff.failures = b.backoff.failures.saturating_add(1);
        b.backoff.last_failure = Some(tokio::time::Instant::now());
        b.backoff.failures.is_power_of_two()
    }

    /// An IDLE login, counting a refused password once per run of failures
    /// (correctness L3): the archive walk compares the refusal count before
    /// and after a fetch, and an IDLE retry must not read as its refusal.
    async fn open_idle_session(&self) -> Result<(ImapSession, Caps)> {
        let count = self.idle_backoff.lock().is_ok_and(|b| !b.auth_counted);
        let out = self.open_session(None, count).await;
        if let Ok(mut b) = self.idle_backoff.lock() {
            match &out {
                Ok(_) => b.auth_counted = false,
                Err(CoreError::Credential(_)) => b.auth_counted = true,
                Err(_) => {}
            }
        }
        out
    }

    /// Hold an early return to [`MIN_WAKE_GAP`] after the previous one
    /// (never past `deadline`).
    async fn space_out_wake(&self, deadline: tokio::time::Instant) {
        let now = tokio::time::Instant::now();
        let earliest = {
            let Ok(mut last) = self.last_wake.lock() else {
                return;
            };
            let earliest = last.map_or(now, |t| (t + MIN_WAKE_GAP).max(now));
            *last = Some(earliest.min(deadline));
            earliest
        };
        if earliest > now {
            tokio::time::sleep_until(earliest.min(deadline)).await;
        }
    }

    /// THE IDLE WAIT: on its own connection, `EXAMINE INBOX`, return at once
    /// if the server is already past `cursor`, else IDLE (re-issued every
    /// [`Timeouts::idle_reissue`]) until INBOX reports an arrival, an expunge
    /// or a flag change, or `deadline`. `Ok(true)` is a return before the
    /// deadline because something changed.
    ///
    /// THE ROUND ENDS ON ITS OWN CLOCK (correctness H1): async-imap's
    /// `wait_with_timeout` restarts its timer on every server line, keepalive
    /// `* OK Still here` included, so on its own a round never times out
    /// while keepalives flow. The round is cut at its end here, then `DONE`.
    /// Keepalives are never a wake.
    ///
    /// NOTHING SAID IN THE GAPS IS LOST (correctness L2): an `EXISTS` the
    /// server sends between `EXAMINE` and `+ idling`, or while `DONE` is
    /// answered, goes to the session's unsolicited channel, which is read
    /// after each.
    ///
    /// CANCEL-SAFE BY OWNERSHIP: the session is taken out of `idle_conn` for
    /// the whole wait and only put back after a clean `DONE`. A wait dropped
    /// at any await drops the session with it (the socket closes, the server
    /// ends the IDLE), and nothing shared is left half-way through a command:
    /// the next call logs in afresh. The command session is never touched.
    async fn idle_until(
        &self,
        cursor: Option<&SyncCursor>,
        deadline: tokio::time::Instant,
    ) -> Result<bool> {
        use async_imap::extensions::idle::IdleResponse;

        let parked = self.idle_conn.lock().await.take();
        let mut session = match parked {
            Some(s) => s,
            None => {
                let (s, caps) = self.open_idle_session().await?;
                if !caps.idle {
                    // No IDLE on this server: the default poll, for good.
                    drop(s);
                    tokio::time::sleep_until(deadline).await;
                    return Ok(false);
                }
                self.idle_connects.fetch_add(1, Ordering::Relaxed);
                s
            }
        };
        let unsolicited = session.unsolicited_responses.clone();
        // Left over from earlier waits: the EXAMINE below is the fresh view.
        drain_changes(|| unsolicited.try_recv().ok());
        // A fresh view of INBOX each wait: its UIDNEXT is what the engine's
        // cursor is compared with, and IDLE reports changes against it.
        let mbox = session
            .examine(INBOX)
            .await
            .map_err(imap_err("idle examine"))?;
        if self.already_ahead(cursor, mbox.uid_validity, mbox.uid_next) {
            *self.idle_conn.lock().await = Some(session);
            return Ok(true);
        }
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                *self.idle_conn.lock().await = Some(session);
                return Ok(false);
            }
            let round_end = deadline.min(now + self.timeouts.idle_reissue);
            let mut handle = session.idle();
            handle.init().await.map_err(imap_err("idle"))?;
            let mut changed = drain_changes(|| unsolicited.try_recv().ok());
            if !changed {
                // `_stop` must outlive the wait: dropping it interrupts it.
                let (wait, _stop) = handle.wait_with_timeout(round_end - now);
                match tokio::time::timeout_at(round_end, wait).await {
                    // The round is over, keepalives or not.
                    Err(_) => {}
                    Ok(r) => match r.map_err(imap_err("idle"))? {
                        IdleResponse::NewData(data) => changed = is_mailbox_change(data.parsed()),
                        IdleResponse::Timeout => {}
                        // The stream ended: the server hung up.
                        IdleResponse::ManualInterrupt => {
                            return Err(broken_err("idle", "connection closed"));
                        }
                    },
                }
            }
            session = handle.done().await.map_err(imap_err("idle done"))?;
            changed |= drain_changes(|| unsolicited.try_recv().ok());
            if changed {
                *self.idle_conn.lock().await = Some(session);
                return Ok(true);
            }
            // Something that is not a change (RECENT, an alert), or the
            // round's end: IDLE again for what is left.
        }
    }

    /// Is INBOX already past what `cursor` covers? Then the wait returns at
    /// once, but only once per cursor: if the engine hands the same cursor
    /// back, its walk did not move it, and returning early again would spin.
    fn already_ahead(
        &self,
        cursor: Option<&SyncCursor>,
        uidvalidity: Option<u32>,
        uid_next: Option<u32>,
    ) -> bool {
        let Some(cursor) = cursor else {
            return false;
        };
        let Some(pos) = ImapCursor::decode(cursor)
            .and_then(|c| c.folders.get(role_key(Folder::Inbox)).cloned())
        else {
            return false;
        };
        let ahead = uidvalidity.is_some_and(|v| v != pos.uidvalidity)
            || uid_next.is_some_and(|n| n.saturating_sub(1) > pos.last_uid);
        if !ahead {
            return false;
        }
        let Ok(mut last) = self.idle_ahead_of.lock() else {
            return false;
        };
        if last.as_ref() == Some(cursor) {
            return false;
        }
        *last = Some(cursor.clone());
        true
    }

    /// IDLE logins so far.
    #[cfg(test)]
    pub(crate) fn idle_connects(&self) -> u64 {
        self.idle_connects.load(Ordering::Relaxed)
    }

    /// Bring the location rows in line with what a resolve found: stale rows
    /// out, rows the search found in.
    fn settle<T>(&self, id: &str, r: &Resolved<T>) -> Result<()> {
        if let Some(d) = &r.bound {
            self.locations
                .record_content_digest(self.account_id, id, d)?;
        }
        for row in &r.stale {
            // A stale row the search found again at the same folder was just
            // re-recorded below; forgetting it first keeps that order safe.
            self.locations
                .forget_message_location(self.account_id, id, &row.folder)?;
        }
        let found: Vec<(String, MessageLocation)> = r
            .found
            .iter()
            .map(|l| (id.to_string(), l.clone()))
            .collect();
        self.locations
            .record_message_locations(self.account_id, &found)
    }
}

/// The in-memory [`MessageLocations`] the unit and live tests use.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct MemLocations(
    std::sync::Mutex<
        std::collections::HashMap<(AccountId, String), BTreeMap<String, MessageLocation>>,
    >,
    std::sync::Mutex<std::collections::HashMap<(AccountId, String), String>>,
);

#[cfg(test)]
impl MessageLocations for MemLocations {
    fn message_locations(&self, a: AccountId, id: &str) -> Result<Vec<MessageLocation>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .get(&(a, id.to_string()))
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default())
    }
    fn record_message_locations(
        &self,
        a: AccountId,
        seen: &[(String, MessageLocation)],
    ) -> Result<()> {
        let mut map = self.0.lock().unwrap();
        for (id, loc) in seen {
            let rows = map.entry((a, id.clone())).or_default();
            let mut loc = loc.clone();
            if loc.message_id.is_none() {
                loc.message_id = rows.get(&loc.folder).and_then(|r| r.message_id.clone());
            }
            rows.insert(loc.folder.clone(), loc);
        }
        Ok(())
    }
    fn forget_message_location(&self, a: AccountId, id: &str, folder: &str) -> Result<()> {
        if let Some(rows) = self.0.lock().unwrap().get_mut(&(a, id.to_string())) {
            rows.remove(folder);
        }
        Ok(())
    }
    fn content_digest(&self, a: AccountId, id: &str) -> Result<Option<String>> {
        Ok(self.1.lock().unwrap().get(&(a, id.to_string())).cloned())
    }
    fn record_content_digest(&self, a: AccountId, id: &str, digest: &str) -> Result<()> {
        self.1
            .lock()
            .unwrap()
            .entry((a, id.to_string()))
            .or_insert_with(|| digest.to_string());
        Ok(())
    }
}

#[cfg(test)]
mod tests;
