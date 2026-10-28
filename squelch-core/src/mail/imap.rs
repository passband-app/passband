//! IMAP as a [`MailSource`] (issue #575, phase 2): READ ONLY, self-host.
//!
//! STEP 1 SKELETON. What works: connecting (implicit TLS, STARTTLS, and
//! plaintext to loopback only), `LOGIN`, folder roles from `SPECIAL-USE`
//! with a fallback by name, [`Listing::Folder`], `fetch_raw`,
//! `fetch_headers`, `current_cursor` and `inbox_unread`. The change feed,
//! the archive window and IDLE return a clean error (or the default sleep)
//! until the later steps land.
//!
//! What a source here must never do:
//! - change server state: every folder is opened with `EXAMINE` (read-only)
//!   and every body or header fetch is `BODY.PEEK`, so `\Seen` and `\Recent`
//!   are left exactly as the user's other clients see them;
//! - log or echo the password: it lives in [`ImapPassword`], whose `Debug`
//!   is redacted, and a failed `LOGIN` is reported without the server's text
//!   (some servers echo the username back);
//! - send the password in the clear to anything but this machine.
//!
//! THE STABLE ID (contract §5) is `imap:` + hex sha256 of the normalized
//! `Message-ID` ([`stable_id_for_message_id`]), or of the raw header block
//! when there is none. It is what `messages.gmail_msg_id` holds for an IMAP
//! account, and it survives a `MOVE` between folders, which a UID does not.
//!
//! THE LOCATION TABLE (contract §6) is in memory in this step: every listing
//! records where each id was last seen, and `fetch_raw` resolves through it,
//! falling back to `SEARCH HEADER Message-ID` in the watched folders. Making
//! it persistent needs store access the trait does not give a source; see
//! the step 1 report.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};

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
enum Transport {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl AsyncRead for Transport {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Transport::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Transport::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Transport {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Transport::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Transport::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Transport::Plain(s) => Pin::new(s).poll_flush(cx),
            Transport::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Transport::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Transport::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

type ImapSession = Session<Transport>;

/// Is `host` this machine, by its literal spelling? The connect also checks
/// the address it actually reached ([`connect_tcp`]), so a `localhost` that
/// resolves somewhere else still fails closed.
fn host_is_loopback(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost") || h.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// An error about the connection, never carrying a secret: the context is
/// ours and `e` is an io/TLS/IMAP error about the transport.
fn net_err(context: &'static str, e: impl std::fmt::Display) -> CoreError {
    CoreError::Other(anyhow::anyhow!("imap {context}: {e}"))
}

/// An IMAP command's failure. async-imap's `No`/`Bad` carry the server's
/// response text, which is about the command (never our password: `LOGIN`
/// is mapped separately, see [`ImapSource::connect`]).
fn imap_err(context: &'static str) -> impl FnOnce(async_imap::error::Error) -> CoreError {
    move |e| net_err(context, e)
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
        [Folder::Inbox, Folder::Sent, Folder::Spam]
            .into_iter()
            .filter_map(|f| self.folder(f).map(|name| (f, name)))
            .collect()
    }
}

const INBOX: &str = "INBOX";

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

/// The stable id of a message from its raw header block: the Message-ID's
/// when it has one, else a hash of the block itself.
pub fn stable_id_for_header_block(header: &[u8]) -> (String, Option<String>) {
    match header_values(header, "Message-ID")
        .into_iter()
        .find_map(|v| normalize_message_id(&v))
    {
        Some(mid) => (stable_id_for_message_id(&mid), Some(mid)),
        None => (hex_sha256(header), None),
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
    pub last_uid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highestmodseq: Option<u64>,
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

// ---- the location table (in memory in step 1) -----------------------------

/// Where a stable id was last seen (contract §6).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Location {
    folder: String,
    uidvalidity: u32,
    uid: u32,
    /// The normalized Message-ID, kept because the stable id is a hash and
    /// the stale-location fallback has to `SEARCH` by the real header.
    message_id: Option<String>,
}

// ---- the source -----------------------------------------------------------

/// One authenticated command connection and what was learned on it.
struct Conn {
    session: ImapSession,
    roles: FolderRoles,
}

/// An IMAP mailbox's read side.
pub struct ImapSource {
    server: ImapServer,
    password: ImapPassword,
    tls: Arc<ClientConfig>,
    /// The one command connection (contract §11). `None` until first use and
    /// after any error, so the next call reconnects.
    conn: tokio::sync::Mutex<Option<Conn>>,
    locations: std::sync::Mutex<HashMap<String, Location>>,
    refusals: Arc<SourceRefusals>,
}

impl std::fmt::Debug for ImapSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImapSource")
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
    /// A source for `server`, trusting the public web PKI only.
    pub fn new(
        server: ImapServer,
        password: ImapPassword,
        refusals: Arc<SourceRefusals>,
    ) -> Result<Self> {
        Self::with_roots(server, password, refusals, &[])
    }

    /// The same, also trusting `extra_roots`. Crate-private: the test bed's
    /// throwaway CA is the only caller. There is no way to turn verification
    /// off (contract §9).
    pub(crate) fn with_roots(
        server: ImapServer,
        password: ImapPassword,
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
            tls: tls_config(extra_roots)?,
            server,
            password,
            conn: tokio::sync::Mutex::new(None),
            locations: std::sync::Mutex::new(HashMap::new()),
            refusals,
        })
    }

    async fn connect_tcp(&self) -> Result<TcpStream> {
        let tcp = TcpStream::connect((self.server.host.as_str(), self.server.port))
            .await
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
        Ok(tcp)
    }

    async fn tls_wrap(&self, tcp: TcpStream) -> Result<Transport> {
        let name = ServerName::try_from(
            self.server
                .host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_string(),
        )
        .map_err(|e| net_err("tls server name", e))?;
        let tls = TlsConnector::from(self.tls.clone())
            .connect(name, tcp)
            .await
            .map_err(|e| net_err("tls handshake", e))?;
        Ok(Transport::Tls(Box::new(tls)))
    }

    /// Open, secure and authenticate one connection, then resolve roles.
    async fn connect(&self) -> Result<Conn> {
        let tcp = self.connect_tcp().await?;
        let mut client = match self.server.tls {
            ImapTls::Implicit => Client::new(self.tls_wrap(tcp).await?),
            ImapTls::Plain => Client::new(Transport::Plain(tcp)),
            ImapTls::StartTls => {
                let mut plain = Client::new(Transport::Plain(tcp));
                read_greeting(&mut plain).await?;
                plain
                    .run_command_and_check_ok("STARTTLS", None)
                    .await
                    .map_err(imap_err("starttls"))?;
                // `into_inner` drops anything buffered after the tagged OK, so
                // a response injected before the handshake is never read as
                // if it came over TLS.
                let Transport::Plain(tcp) = plain.into_inner() else {
                    unreachable!("built over a plain transport above");
                };
                Client::new(self.tls_wrap(tcp).await?)
            }
        };
        if self.server.tls != ImapTls::StartTls {
            read_greeting(&mut client).await?;
        }
        let mut session = match client.login(&self.server.username, &self.password.0).await {
            Ok(s) => s,
            Err((async_imap::error::Error::No(_), _)) => {
                // The server's text is dropped: some servers echo the
                // username, and none of it helps the user more than this.
                self.refusals.auth.fetch_add(1, Ordering::Relaxed);
                return Err(CoreError::Credential(
                    "imap login refused: check the username and app password".into(),
                ));
            }
            Err((e, _)) => return Err(net_err("login", e)),
        };
        let roles = list_roles(&mut session).await?;
        Ok(Conn { session, roles })
    }

    /// Run `f` on the command connection, connecting first if needed. Any
    /// error drops the connection, so the next call starts clean rather than
    /// mid-way through a half-read response.
    async fn with_conn<T>(
        &self,
        f: impl for<'c> FnOnce(
            &'c mut Conn,
        )
            -> Pin<Box<dyn std::future::Future<Output = Result<T>> + Send + 'c>>,
    ) -> Result<T> {
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            *guard = Some(self.connect().await?);
        }
        let conn = guard.as_mut().expect("set above");
        let out = f(conn).await;
        if out.is_err() {
            *guard = None;
        }
        out
    }

    fn remember(&self, id: &str, loc: Location) {
        if let Ok(mut map) = self.locations.lock() {
            map.insert(id.to_string(), loc);
        }
    }

    fn location(&self, id: &str) -> Option<Location> {
        self.locations.lock().ok().and_then(|m| m.get(id).cloned())
    }

    /// The roles as of the current connection (connecting if needed).
    #[cfg(test)]
    pub(crate) async fn roles(&self) -> Result<FolderRoles> {
        self.with_conn(|c| Box::pin(async move { Ok(c.roles.clone()) }))
            .await
    }
}

async fn read_greeting(client: &mut Client<Transport>) -> Result<()> {
    match client.read_response().await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(net_err("greeting", "connection closed")),
        Err(e) => Err(net_err("greeting", e)),
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

/// `EXAMINE` (read-only) a folder and return its `UIDVALIDITY`.
async fn examine(session: &mut ImapSession, folder: &str) -> Result<u32> {
    let mbox = session.examine(folder).await.map_err(imap_err("examine"))?;
    mbox.uid_validity
        .ok_or_else(|| net_err("examine", "server sent no UIDVALIDITY"))
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

/// The stable id of each UID in `uids` (in the open folder), and the
/// normalized Message-ID behind it. Header-only fetches, `BODY.PEEK`.
async fn stable_ids(
    session: &mut ImapSession,
    uids: &[u32],
) -> Result<Vec<(u32, String, Option<String>)>> {
    let mut out = Vec::with_capacity(uids.len());
    let mut no_mid: Vec<u32> = Vec::new();
    for chunk in uids.chunks(500) {
        let fetches: Vec<Fetch> = session
            .uid_fetch(
                uid_set(chunk),
                "(UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])",
            )
            .await
            .map_err(imap_err("fetch message-id"))?
            .try_collect()
            .await
            .map_err(imap_err("fetch message-id"))?;
        for f in fetches {
            let Some(uid) = f.uid else { continue };
            let mid = f.header().and_then(|h| {
                header_values(h, "Message-ID")
                    .into_iter()
                    .find_map(|v| normalize_message_id(&v))
            });
            match mid {
                Some(m) => out.push((uid, stable_id_for_message_id(&m), Some(m))),
                None => no_mid.push(uid),
            }
        }
    }
    for chunk in no_mid.chunks(100) {
        let fetches: Vec<Fetch> = session
            .uid_fetch(uid_set(chunk), "(UID BODY.PEEK[HEADER])")
            .await
            .map_err(imap_err("fetch header"))?
            .try_collect()
            .await
            .map_err(imap_err("fetch header"))?;
        for f in fetches {
            let (Some(uid), Some(h)) = (f.uid, f.header()) else {
                continue;
            };
            let (id, mid) = stable_id_for_header_block(h);
            out.push((uid, id, mid));
        }
    }
    Ok(out)
}

/// One message's bytes by UID in the open folder, or `None` if the UID is
/// gone. `BODY.PEEK[]`: `\Seen` is not touched.
async fn fetch_body(
    session: &mut ImapSession,
    uid: u32,
) -> Result<Option<(Vec<u8>, Option<DateTime<Utc>>)>> {
    let fetches: Vec<Fetch> = session
        .uid_fetch(
            uid.to_string(),
            "(UID RFC822.SIZE INTERNALDATE BODY.PEEK[])",
        )
        .await
        .map_err(imap_err("fetch body"))?
        .try_collect()
        .await
        .map_err(imap_err("fetch body"))?;
    let Some(f) = fetches.into_iter().find(|f| f.uid == Some(uid)) else {
        return Ok(None);
    };
    if f.size.is_some_and(|s| s > MAX_RAW_BYTES) {
        return Err(CoreError::InvalidInput(
            "raw message exceeds the 64MB ingest bound".to_string(),
        ));
    }
    let received = f.internal_date().map(|d| d.with_timezone(&Utc));
    Ok(Some((
        f.body().map(<[u8]>::to_vec).unwrap_or_default(),
        received,
    )))
}

/// Search the watched folders for a message by its Message-ID, for a
/// location that has gone stale (contract §6).
async fn find_by_message_id(
    session: &mut ImapSession,
    roles: &FolderRoles,
    message_id: &str,
) -> Result<Option<(String, u32, u32)>> {
    // The value goes inside a quoted string: refuse anything that could end
    // it or the command early.
    if message_id.contains(['"', '\\', '\r', '\n']) {
        return Ok(None);
    }
    for (_, folder) in roles.watched() {
        let validity = examine(session, folder).await?;
        let hits = session
            .uid_search(format!("HEADER Message-ID \"{message_id}\""))
            .await
            .map_err(imap_err("search message-id"))?;
        if let Some(uid) = hits.into_iter().max() {
            return Ok(Some((folder.to_string(), validity, uid)));
        }
    }
    Ok(None)
}

fn placement_for(roles: &FolderRoles, folder: &str) -> Placement {
    Placement {
        in_inbox: folder.eq_ignore_ascii_case(INBOX),
        in_sent: roles.sent.as_deref() == Some(folder),
    }
}

fn not_yet(what: &str) -> CoreError {
    CoreError::Other(anyhow::anyhow!(
        "imap: {what} is not implemented in this build yet (issue #575)"
    ))
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
        let Listing::Folder {
            folder,
            within_days,
        } = listing
        else {
            return Err(not_yet("the archive window listing"));
        };
        let found = self
            .with_conn(|c| {
                Box::pin(async move {
                    let Some(name) = c.roles.folder(folder).map(str::to_string) else {
                        // No such folder on this server (no Junk, say): an
                        // empty listing, the same as an empty folder.
                        return Ok(Vec::new());
                    };
                    let validity = examine(&mut c.session, &name).await?;
                    let query = match within_days {
                        Some(days) => format!(
                            "SINCE {}",
                            imap_date(Utc::now() - chrono::Duration::days(i64::from(days)))
                        ),
                        None => "ALL".to_string(),
                    };
                    let mut uids: Vec<u32> = c
                        .session
                        .uid_search(query)
                        .await
                        .map_err(imap_err("search"))?
                        .into_iter()
                        .collect();
                    // Newest first: UIDs ascend with arrival in a folder.
                    uids.sort_unstable_by(|a, b| b.cmp(a));
                    if let Some(cap) = cap {
                        uids.truncate(cap);
                    }
                    let mut ids = stable_ids(&mut c.session, &uids).await?;
                    ids.sort_unstable_by(|a, b| b.0.cmp(&a.0));
                    Ok(ids
                        .into_iter()
                        .map(|(uid, id, mid)| (name.clone(), validity, uid, id, mid))
                        .collect::<Vec<_>>())
                })
            })
            .await?;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::with_capacity(found.len());
        for (folder, uidvalidity, uid, id, message_id) in found {
            self.remember(
                &id,
                Location {
                    folder,
                    uidvalidity,
                    uid,
                    message_id,
                },
            );
            // Two copies of one message in a folder are one id.
            if seen.insert(id.clone()) {
                out.push(id);
            }
        }
        Ok(out)
    }

    async fn current_cursor(&self) -> Result<SyncCursor> {
        let folders = self
            .with_conn(|c| {
                Box::pin(async move {
                    let mut folders = BTreeMap::new();
                    for (folder, name) in c.roles.watched() {
                        let st = c
                            .session
                            .status(name, "(UIDVALIDITY UIDNEXT HIGHESTMODSEQ)")
                            .await
                            .map_err(imap_err("status"))?;
                        let (Some(uidvalidity), Some(uid_next)) = (st.uid_validity, st.uid_next)
                        else {
                            return Err(net_err("status", "server omitted UIDVALIDITY/UIDNEXT"));
                        };
                        folders.insert(
                            role_key(folder).to_string(),
                            FolderPos {
                                mailbox: name.to_string(),
                                uidvalidity,
                                last_uid: uid_next.saturating_sub(1),
                                highestmodseq: st.highest_modseq,
                            },
                        );
                    }
                    Ok(folders)
                })
            })
            .await?;
        Ok(ImapCursor { v: 1, folders }.encode())
    }

    fn cursor_is_usable(&self, cursor: &SyncCursor) -> bool {
        ImapCursor::decode(cursor).is_some_and(|c| c.folders.contains_key(role_key(Folder::Inbox)))
    }

    async fn changes(
        &self,
        _since: &SyncCursor,
        _folder: Folder,
    ) -> std::result::Result<Changes, ChangesError> {
        Err(ChangesError::Other(not_yet("the change feed")))
    }

    fn merge_cursors(&self, since: &SyncCursor, a: &SyncCursor, _b: &SyncCursor) -> SyncCursor {
        // Placeholder until the change feed exists (step 2 owns the real
        // per-folder merge). Never behind `since`, which is all a caller of a
        // feed that only errors can observe.
        let _ = since;
        a.clone()
    }

    async fn fetch_raw(&self, id: &str, _urgency: Urgency) -> Result<FetchedMessage> {
        let loc = self.location(id);
        let id_owned = id.to_string();
        let got = self
            .with_conn(|c| {
                Box::pin(async move {
                    // The recorded location first, if its folder still has the
                    // same UIDVALIDITY.
                    if let Some(loc) = &loc
                        && examine(&mut c.session, &loc.folder).await? == loc.uidvalidity
                        && let Some(body) = fetch_body(&mut c.session, loc.uid).await?
                    {
                        let placement = placement_for(&c.roles, &loc.folder);
                        return Ok(Some((loc.clone(), placement, body)));
                    }
                    // Stale or unknown: search the watched folders by the real
                    // header. Without a Message-ID there is nothing to search.
                    let Some(mid) = loc.as_ref().and_then(|l| l.message_id.clone()) else {
                        return Ok(None);
                    };
                    let roles = c.roles.clone();
                    let Some((folder, uidvalidity, uid)) =
                        find_by_message_id(&mut c.session, &roles, &mid).await?
                    else {
                        return Ok(None);
                    };
                    let _ = examine(&mut c.session, &folder).await?;
                    let body = fetch_body(&mut c.session, uid).await?;
                    let placement = placement_for(&roles, &folder);
                    Ok(body.map(|b| {
                        (
                            Location {
                                folder,
                                uidvalidity,
                                uid,
                                message_id: Some(mid),
                            },
                            placement,
                            b,
                        )
                    }))
                })
            })
            .await?;
        match got {
            Some((loc, placement, (bytes, received_at))) => {
                self.remember(&id_owned, loc);
                Ok(FetchedMessage {
                    id: id_owned,
                    thread_id: None,
                    raw: if bytes.is_empty() {
                        RawBody::Missing
                    } else {
                        RawBody::Bytes(bytes)
                    },
                    received_at,
                    placement,
                })
            }
            // Never seen by this process (no location), or gone everywhere.
            None => Err(CoreError::NotFound),
        }
    }

    async fn fetch_headers(&self, id: &str, names: &[&str]) -> Result<FetchedHeaders> {
        let Some(loc) = self.location(id) else {
            return Err(CoreError::NotFound);
        };
        // Header names go into the command; refuse anything that is not a
        // plain field name.
        if names
            .iter()
            .any(|n| n.is_empty() || !n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        {
            return Err(CoreError::InvalidInput("imap: bad header name".into()));
        }
        let query = format!(
            "(UID INTERNALDATE BODY.PEEK[HEADER.FIELDS ({})])",
            names.join(" ")
        );
        self.with_conn(|c| {
            Box::pin(async move {
                if examine(&mut c.session, &loc.folder).await? != loc.uidvalidity {
                    return Err(CoreError::NotFound);
                }
                let fetches: Vec<Fetch> = c
                    .session
                    .uid_fetch(loc.uid.to_string(), query)
                    .await
                    .map_err(imap_err("fetch headers"))?
                    .try_collect()
                    .await
                    .map_err(imap_err("fetch headers"))?;
                let f = fetches
                    .into_iter()
                    .find(|f| f.uid == Some(loc.uid))
                    .ok_or(CoreError::NotFound)?;
                Ok(FetchedHeaders {
                    headers: f.header().map(parse_header_block).unwrap_or_default(),
                    received_at: f.internal_date().map(|d| d.with_timezone(&Utc)),
                })
            })
        })
        .await
    }

    async fn inbox_unread(&self) -> Result<(i64, i64)> {
        self.with_conn(|c| {
            Box::pin(async move {
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

    // `wait_for_change` keeps the default sleep until the IDLE step.
}

#[cfg(test)]
mod tests;
