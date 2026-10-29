//! Outlook as a [`MailSource`]: Microsoft Graph's mail API, polled.
//!
//! What Graph changes about the conversation the engine has with a mailbox:
//!
//! - **IMMUTABLE IDS, ON EVERY REQUEST.** By default a Graph message id changes
//!   when the message moves folder, so an archive or a trash would orphan the
//!   row keyed on it (`UNIQUE(account_id, gmail_msg_id)`). Every request this
//!   source sends carries `Prefer: IdType="ImmutableId"`, and it is set in the
//!   one place a request is built ([`GraphSource::send_get`]), not at the call
//!   sites. The write side does the same.
//! - **THE CURSOR IS TWO DELTA LINKS**, one per folder (inbox, sent items),
//!   JSON-encoded. Graph's links are opaque URLs; [`MailSource::merge_cursors`]
//!   takes each folder's link from whichever walk moved it.
//! - **A NEW CURSOR COSTS NO REQUEST.** [`MailSource::current_cursor`] mints
//!   the INITIAL delta round for each folder, filtered to mail received in the
//!   last [`CURSOR_OVERLAP`]. The first walk runs it: mail that landed between
//!   the window's listing and now comes back in that round instead of falling
//!   into the gap, and what is already stored is skipped before any fetch.
//! - **DELTA NAMES CHANGED MAIL TOO** (read-state flips, flags), so this source
//!   says [`MailSource::changes_report_updates`] and the engine skips ids it
//!   already holds on the incremental path.
//! - **`410 Gone` on a delta link is the expired cursor**, like Gmail's 404 on
//!   `history.list`. The engine's catch-up mints a fresh cursor; the
//!   `Location` Graph sends would start a full-folder enumeration, which the
//!   catch-up's window listing already covers.
//! - **THROTTLING IS MICROSOFT'S**: `429` (or `503`) with `Retry-After`.
//!   There is no shared Gmail budget here; [`Urgency::Window`] waits a
//!   refusal out in place, the others return it.
//!
//! EVERY URL THIS SOURCE FOLLOWS STARTS WITH ITS OWN BASE. Delta and next
//! links come back from Graph and are stored in the database, and the bearer
//! token rides along with whatever URL is followed, so a link from a tampered
//! row or a confused proxy that points anywhere else is refused before a
//! request is built ([`GraphSource::own_url`]).
//!
//! Tokens, headers and bodies are NEVER logged (SECURITY.md).

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use super::{
    Changes, ChangesError, FetchedHeaders, FetchedMessage, Folder, Listing, MailProvider,
    MailSource, Placement, RawBody, SourceRefusals, SyncCursor, Urgency,
};
use crate::credentials::CredentialStore;
use crate::error::{CoreError, Result};
use crate::metrics::{GmailErrorKind, SyncMetrics};
use crate::types::AccountId;

/// Graph's v1.0 base. Fixed; not user-tunable, for the reason
/// [`super::gmail::GMAIL_API_BASE`] is not: a build that could be aimed at an
/// arbitrary host would be a bearer-token exfiltration primitive. `pub` so the
/// write side targets the same host from one definition.
pub const GRAPH_API_BASE: &str = "https://graph.microsoft.com/v1.0";

/// The preference every Graph mail request carries: ids that survive a move.
/// `pub` so the write side sends the identical header.
pub const PREFER_IMMUTABLE_IDS: &str = "IdType=\"ImmutableId\"";

/// How far back a freshly minted cursor's first delta round reaches. Covers
/// the gap between a window's listing and the cursor that follows it, with
/// room for a slow backfill's last page and some clock skew between this host
/// and Exchange. The overlap costs one cheap delta page and an indexed probe
/// per message already stored, never a body.
pub const CURSOR_OVERLAP: chrono::Duration = chrono::Duration::minutes(30);

/// Ids per page asked of Graph. Its own ceiling is 1000 for a list and the
/// delta default is 10; 100 keeps a page's JSON small with `$select=id`.
const PAGE_SIZE: usize = 100;

/// How many times a [`Urgency::Window`] request waits out a throttle in place.
const GRAPH_THROTTLE_RETRIES: usize = 3;

/// The largest raw message this source reads. The same bound the Gmail source
/// puts on a decoded body, so peak ingest memory is ours to set, not Graph's.
const MAX_RAW_BYTES: usize = 64 * 1024 * 1024;

/// A Graph well-known folder name, as Graph spells it in a path.
fn folder_name(folder: Folder) -> &'static str {
    match folder {
        Folder::Inbox => "inbox",
        Folder::Sent => "sentitems",
        Folder::Spam => "junkemail",
    }
}

/// Percent-encode one path segment. Graph ids are base64-shaped and may carry
/// `/`, `+` and `=`, any of which would change the URL if left bare.
fn path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Percent-encode a query value (OData expressions carry spaces and colons).
fn query_value(s: &str) -> String {
    path_segment(s)
}

/// An instant the way an OData `$filter` reads one.
fn odata_instant(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

// ---- The cursor ---------------------------------------------------------

/// The decoded form of an Outlook [`SyncCursor`]: where each folder's change
/// feed resumes. A link is either a delta link Graph handed back or the
/// initial round [`MailSource::current_cursor`] minted; the walk follows
/// either the same way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct GraphCursor {
    /// The encoding's version, so a later shape can refuse this one cleanly.
    v: u8,
    inbox: String,
    sent: String,
}

const CURSOR_VERSION: u8 = 1;

impl GraphCursor {
    fn decode(cursor: &SyncCursor) -> Option<Self> {
        let c: GraphCursor = serde_json::from_str(cursor.as_str()).ok()?;
        (c.v == CURSOR_VERSION && !c.inbox.is_empty() && !c.sent.is_empty()).then_some(c)
    }

    fn encode(&self) -> SyncCursor {
        SyncCursor::new(serde_json::to_string(self).expect("a cursor of two strings serializes"))
    }

    fn link(&self, folder: Folder) -> Option<&str> {
        match folder {
            Folder::Inbox => Some(&self.inbox),
            Folder::Sent => Some(&self.sent),
            Folder::Spam => None,
        }
    }

    fn with_link(&self, folder: Folder, link: String) -> Self {
        let mut next = self.clone();
        match folder {
            Folder::Inbox => next.inbox = link,
            Folder::Sent => next.sent = link,
            Folder::Spam => {}
        }
        next
    }
}

// ---- Graph response shapes (only the fields consumed) -------------------

#[derive(Debug, Deserialize)]
struct Page<T> {
    #[serde(default = "Vec::new")]
    value: Vec<T>,
    #[serde(rename = "@odata.nextLink", default)]
    next_link: Option<String>,
    #[serde(rename = "@odata.deltaLink", default)]
    delta_link: Option<String>,
}

/// One entry of a listing or a delta round.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MessageEntry {
    id: String,
    #[serde(default)]
    parent_folder_id: Option<String>,
    #[serde(default)]
    is_draft: Option<bool>,
    /// Present (as an object) on a delta entry for a message that left the
    /// folder or was deleted. Squelch does not process deletions.
    #[serde(rename = "@removed", default)]
    removed: Option<serde_json::Value>,
}

/// The fields of one message read beside its raw body.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MessageMeta {
    #[serde(default)]
    id: String,
    #[serde(default)]
    conversation_id: Option<String>,
    #[serde(default)]
    received_date_time: Option<String>,
    #[serde(default)]
    parent_folder_id: Option<String>,
    #[serde(default)]
    from: Option<Recipient>,
}

#[derive(Debug, Deserialize)]
struct EmailAddress {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    address: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Recipient {
    email_address: EmailAddress,
}

/// The structured address fields `fetch_headers` renders as header values.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecipientsMeta {
    #[serde(default)]
    to_recipients: Vec<Recipient>,
    #[serde(default)]
    cc_recipients: Vec<Recipient>,
    #[serde(default)]
    bcc_recipients: Vec<Recipient>,
    #[serde(default)]
    from: Option<Recipient>,
    #[serde(default)]
    reply_to: Vec<Recipient>,
    #[serde(default)]
    received_date_time: Option<String>,
}

/// `GET /me/mailFolders/{name}`. `id` is required so an empty or foreign 200
/// body is a decode error, not a confident zero (see Gmail's `LabelResp`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FolderResp {
    id: String,
    #[serde(default)]
    unread_item_count: i64,
}

#[derive(Debug, Deserialize)]
struct GraphErrorBody {
    error: GraphErrorInner,
}

#[derive(Debug, Deserialize)]
struct GraphErrorInner {
    #[serde(default)]
    code: String,
}

/// The folders whose ids a message's `parentFolderId` is compared against.
/// Folder ids are stable; looked up once per source.
#[derive(Debug, Clone)]
struct FolderIds {
    inbox: String,
    sent: String,
    /// Never part of the archive walk: drafts, deleted items, junk.
    excluded: HashSet<String>,
}

fn parse_instant(s: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// One address the way a header would carry it. The name is quoted when it
/// has anything a header parser would split on; the consumers strip names
/// anyway and keep the address.
fn render_address(r: &Recipient) -> Option<String> {
    let addr = r.email_address.address.as_deref()?.trim();
    if addr.is_empty() {
        return None;
    }
    match r.email_address.name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() && !name.eq_ignore_ascii_case(addr) => {
            Some(format!("\"{}\" <{addr}>", clean_name(name)))
        }
        _ => Some(addr.to_string()),
    }
}

/// A display name made safe to quote: quotes, backslashes and line breaks
/// DROPPED rather than escaped. The header parsers downstream (the reply
/// path's injected-header truncation) treat every `"` as a quote boundary,
/// so an escaped one would cut a recipient list short; and nothing reads
/// the name except a person.
fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '"' | '\\' | '\r' | '\n'))
        .collect()
}

fn render_list(list: &[Recipient]) -> Option<String> {
    let parts: Vec<String> = list.iter().filter_map(render_address).collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// The stable label a Graph refusal is recorded under. A closed set, so the
/// sync-error surface never carries Graph's own (free-text) message.
fn graph_reason(status: u16, code: &str) -> &'static str {
    match (status, code) {
        (401, _) => "graph_unauthorized",
        (403, _) => "graph_forbidden",
        (410, _) => "graph_sync_state_expired",
        (429, _) | (503, _) => "graph_throttled",
        (_, "MailboxNotEnabledForRESTAPI") => "graph_mailbox_not_enabled",
        (_, "ErrorItemNotFound") => "graph_item_not_found",
        _ => "graph_error",
    }
}

/// Which [`GmailErrorKind`] (the daemon's provider-blind error buckets, named
/// for the first provider) a Graph status is.
fn classify_graph_status(status: u16) -> GmailErrorKind {
    match status {
        401 => GmailErrorKind::Auth,
        429 | 503 => GmailErrorKind::Quota,
        _ => GmailErrorKind::Http,
    }
}

/// Everything a [`GraphSource`] is built from, assembled by the engine on
/// first use (see `GmailSourceParts` for why first use).
pub(crate) struct GraphSourceParts<C: CredentialStore + ?Sized> {
    pub http: reqwest::Client,
    pub creds: Arc<C>,
    pub account_id: AccountId,
    /// The mailbox's own address: mail From it is the user's own.
    pub account_email: String,
    pub metrics: Arc<SyncMetrics>,
    pub refusals: Arc<SourceRefusals>,
    /// Always [`GRAPH_API_BASE`] outside the tests.
    pub api_base: String,
    /// The longest a [`Urgency::Window`] request waits on one `Retry-After`.
    pub quota_retry_cap: Duration,
}

/// Outlook's read side, on the READ credential.
pub struct GraphSource<C: CredentialStore + ?Sized> {
    http: reqwest::Client,
    creds: Arc<C>,
    account_id: AccountId,
    account_email: String,
    metrics: Arc<SyncMetrics>,
    refusals: Arc<SourceRefusals>,
    api_base: String,
    quota_retry_cap: Duration,
    folders: tokio::sync::OnceCell<FolderIds>,
    /// Every address this account is (see [`Self::own_addresses`]).
    own: tokio::sync::OnceCell<Vec<String>>,
}

impl<C: CredentialStore + ?Sized> GraphSource<C> {
    pub(crate) fn new(parts: GraphSourceParts<C>) -> Self {
        Self {
            http: parts.http,
            creds: parts.creds,
            account_id: parts.account_id,
            account_email: parts.account_email,
            metrics: parts.metrics,
            refusals: parts.refusals,
            api_base: parts.api_base,
            quota_retry_cap: parts.quota_retry_cap,
            folders: tokio::sync::OnceCell::new(),
            own: tokio::sync::OnceCell::new(),
        }
    }

    /// The addresses mail FROM this account can carry: the configured one,
    /// plus what Graph's `/me` names (`mail`, the sign-in name). A personal
    /// account's configured alias, its primary and its sign-in can all
    /// differ, and Exchange sends as the primary, so matching only the
    /// configured address would read the owner's own mail as a stranger's.
    /// Asked once per source; a failed lookup is retried next time and falls
    /// back to the configured address alone.
    async fn own_addresses(&self) -> Vec<String> {
        #[derive(Deserialize)]
        struct Me {
            mail: Option<String>,
            #[serde(rename = "userPrincipalName")]
            upn: Option<String>,
        }
        let configured = self.account_email.trim().to_ascii_lowercase();
        let looked_up = self
            .own
            .get_or_try_init(|| async {
                #[derive(Deserialize)]
                struct Addr {
                    address: Option<String>,
                }
                #[derive(Deserialize)]
                struct From {
                    #[serde(rename = "emailAddress")]
                    email_address: Addr,
                }
                #[derive(Deserialize)]
                struct Sent {
                    from: Option<From>,
                }
                #[derive(Deserialize)]
                struct Page {
                    #[serde(default)]
                    value: Vec<Sent>,
                }
                let url = format!("{}/me?$select=mail,userPrincipalName", self.api_base);
                let me = self.get_json::<Me>(&url, Urgency::Window).await?;
                // What Exchange actually sends as, which `/me` can lag by a
                // day after the owner changes their primary alias.
                let sent_url = format!(
                    "{}/me/mailFolders/sentitems/messages?$top=1&$select=from&$orderby=sentDateTime desc",
                    self.api_base
                );
                let last_sent = self
                    .get_json::<Page>(&sent_url, Urgency::Window)
                    .await
                    .ok()
                    .and_then(|p| p.value.into_iter().next())
                    .and_then(|m| m.from)
                    .and_then(|f| f.email_address.address);
                let mut all = vec![configured.clone()];
                for a in [me.mail, me.upn, last_sent].into_iter().flatten() {
                    let a = a.trim().to_ascii_lowercase();
                    if a.contains('@') && !all.contains(&a) {
                        all.push(a);
                    }
                }
                Ok::<_, CoreError>(all)
            })
            .await;
        match looked_up {
            Ok(all) => all.clone(),
            Err(_) => vec![configured],
        }
    }

    /// Is `url` one of this source's own? The guard on every link that came
    /// from Graph or the database: the bearer token goes wherever it points.
    fn own_url(&self, url: &str) -> bool {
        url.strip_prefix(&self.api_base)
            .is_some_and(|rest| rest.starts_with('/'))
    }

    fn require_own(&self, url: &str) -> Result<()> {
        if self.own_url(url) {
            Ok(())
        } else {
            // The URL is never echoed: it may carry a delta token.
            Err(CoreError::InvalidInput(
                "graph link does not point at the Graph API base; refusing to follow it".into(),
            ))
        }
    }

    /// The INITIAL delta round for `folder`: ids only, mail received since
    /// `since`.
    fn initial_delta(&self, folder: Folder, since: DateTime<Utc>) -> String {
        format!(
            "{}/me/mailFolders/{}/messages/delta?$select=id&$filter={}",
            self.api_base,
            folder_name(folder),
            query_value(&format!("receivedDateTime ge {}", odata_instant(since)))
        )
    }

    async fn token(&self, fresh: bool) -> Result<crate::credentials::OAuthToken> {
        let token = if fresh {
            self.creds.fresh_token(self.account_id).await
        } else {
            self.creds.token(self.account_id).await
        };
        token
            .inspect(|_| self.metrics.note_credential_ok())
            .inspect_err(|e| {
                if matches!(e, CoreError::Credential(_)) {
                    self.metrics.record_gmail_error(GmailErrorKind::Auth);
                }
            })
    }

    async fn bearer_get(&self, url: &str, token: &str, quiet: bool) -> Result<reqwest::Response> {
        self.http
            .get(url)
            .bearer_auth(token)
            // Delta rounds page by this preference alone (their default is
            // 10); a list's `$top` already says the same.
            .header(
                "Prefer",
                format!("{PREFER_IMMUTABLE_IDS}, odata.maxpagesize={PAGE_SIZE}"),
            )
            .send()
            .await
            .map_err(|e| {
                if quiet {
                    self.metrics.count_gmail_error(GmailErrorKind::Network);
                } else {
                    self.metrics.record_gmail_error(GmailErrorKind::Network);
                }
                // reqwest's error names the URL, which may carry a delta
                // token; say what failed without it.
                CoreError::Other(anyhow::anyhow!(
                    "graph request failed ({})",
                    if e.is_timeout() {
                        "timeout"
                    } else if e.is_connect() {
                        "connect"
                    } else {
                        "transport"
                    }
                ))
            })
    }

    /// One authenticated GET to one of this source's own URLs, retrying once
    /// on 401 with a token from past the store's in-memory copy.
    async fn send_get(&self, url: &str, quiet: bool) -> Result<reqwest::Response> {
        self.require_own(url)?;
        let token = self.token(false).await?;
        let resp = self.bearer_get(url, &token.access_token, quiet).await?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            eprintln!("squelch: graph 401; refreshing token and retrying once");
            let token = self.token(true).await?;
            return self.bearer_get(url, &token.access_token, quiet).await;
        }
        Ok(resp)
    }

    /// GET `url` and hand back the successful response, waiting throttles out
    /// in place for [`Urgency::Window`]. `404` and `410` are
    /// [`CoreError::NotFound`]: a message that is gone, or a delta link Graph
    /// can no longer replay from.
    async fn get_ok(&self, url: &str, urgency: Urgency) -> Result<reqwest::Response> {
        let quiet = urgency == Urgency::Background;
        for attempt in 0..=GRAPH_THROTTLE_RETRIES {
            let resp = self.send_get(url, quiet).await?;
            let status = resp.status();
            if status.is_success() {
                return Ok(resp);
            }
            if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
                return Err(CoreError::NotFound);
            }
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(super::gmail::gmail_retry_after);
            let body = read_capped(resp, 16 * 1024).await.unwrap_or_default();
            let code = serde_json::from_slice::<GraphErrorBody>(&body)
                .map(|b| b.error.code)
                .unwrap_or_default();
            let kind = classify_graph_status(status.as_u16());
            let reason = graph_reason(status.as_u16(), &code);
            if kind == GmailErrorKind::Quota
                && urgency == Urgency::Window
                && attempt < GRAPH_THROTTLE_RETRIES
            {
                // Graph's `Retry-After` is the instruction; without one, a
                // short doubling wait. Past the cap the request ends and the
                // lifecycle's own backoff takes over.
                let wait = retry_after.unwrap_or(Duration::from_secs(2 << attempt));
                if wait <= self.quota_retry_cap {
                    self.metrics.record_gmail_quota_retry();
                    eprintln!(
                        "squelch: Microsoft Graph is throttling this mailbox; retrying the same \
                         request in {}s (catch-up position retained)",
                        wait.as_secs()
                    );
                    tokio::time::sleep(wait).await;
                    continue;
                }
            }
            if quiet {
                match kind {
                    GmailErrorKind::Quota => {
                        self.refusals.quota.fetch_add(1, Ordering::Relaxed);
                    }
                    GmailErrorKind::Auth => {
                        self.refusals.auth.fetch_add(1, Ordering::Relaxed);
                    }
                    GmailErrorKind::Http | GmailErrorKind::Network => {}
                }
            }
            if quiet && kind != GmailErrorKind::Auth {
                self.metrics.count_gmail_error(kind);
            } else {
                self.metrics.record_gmail_error_reason(kind, reason);
            }
            return Err(CoreError::Other(anyhow::anyhow!(
                "graph api status {} (reason: {reason})",
                status.as_u16()
            )));
        }
        unreachable!("the final attempt returns")
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
        urgency: Urgency,
    ) -> Result<T> {
        let resp = self.get_ok(url, urgency).await?;
        let body = read_capped(resp, 8 * 1024 * 1024).await?;
        serde_json::from_slice(&body).map_err(|e| {
            CoreError::Other(anyhow::anyhow!(
                "graph json decode: {:?} at line {} column {}",
                e.classify(),
                e.line(),
                e.column()
            ))
        })
    }

    /// The ids of the folders a message's `parentFolderId` is held against.
    async fn folder_ids(&self) -> Result<&FolderIds> {
        self.folders
            .get_or_try_init(|| async {
                let id = |name: &'static str| async move {
                    let url = format!("{}/me/mailFolders/{name}?$select=id", self.api_base);
                    self.get_json::<FolderResp>(&url, Urgency::Window)
                        .await
                        .map(|f| f.id)
                };
                let inbox = id("inbox").await?;
                let sent = id("sentitems").await?;
                let mut excluded = HashSet::new();
                for name in ["drafts", "deleteditems", "junkemail"] {
                    excluded.insert(id(name).await?);
                }
                Ok(FolderIds {
                    inbox,
                    sent,
                    excluded,
                })
            })
            .await
    }

    /// Page through a listing from `first`, keeping the entries `keep`
    /// accepts, until done or `cap` ids are in hand.
    async fn page_ids(
        &self,
        first: String,
        cap: Option<usize>,
        urgency: Urgency,
        keep: impl Fn(&MessageEntry) -> bool,
    ) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        let mut url = first;
        loop {
            let page: Page<MessageEntry> = self.get_json(&url, urgency).await?;
            ids.extend(page.value.into_iter().filter(|e| keep(e)).map(|e| e.id));
            if let Some(cap) = cap
                && ids.len() >= cap
            {
                ids.truncate(cap);
                break;
            }
            match page.next_link {
                Some(next) => url = next,
                None => break,
            }
        }
        Ok(ids)
    }
}

/// Read a body with a ceiling, refusing a declared length past it before a
/// byte is read.
async fn read_capped(mut resp: reqwest::Response, cap: usize) -> Result<Vec<u8>> {
    let too_big =
        || CoreError::InvalidInput(format!("graph response exceeds the {cap}-byte bound"));
    if resp.content_length().is_some_and(|n| n > cap as u64) {
        return Err(too_big());
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|_| CoreError::Other(anyhow::anyhow!("graph response body read failed")))?
    {
        if body.len() + chunk.len() > cap {
            return Err(too_big());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[async_trait::async_trait]
impl<C: CredentialStore + ?Sized + 'static> MailSource for GraphSource<C> {
    fn provider(&self) -> MailProvider {
        MailProvider::Outlook
    }

    async fn list(
        &self,
        listing: Listing,
        cap: Option<usize>,
        urgency: Urgency,
    ) -> Result<Vec<String>> {
        match listing {
            Listing::Folder {
                folder,
                within_days,
            } => {
                let mut url = format!(
                    "{}/me/mailFolders/{}/messages?$select=id&$top={PAGE_SIZE}&$orderby={}",
                    self.api_base,
                    folder_name(folder),
                    query_value("receivedDateTime desc"),
                );
                if let Some(days) = within_days {
                    let since = Utc::now() - chrono::Duration::days(i64::from(days));
                    url.push_str(&format!(
                        "&$filter={}",
                        query_value(&format!("receivedDateTime ge {}", odata_instant(since)))
                    ));
                }
                self.page_ids(url, cap, urgency, |_| true).await
            }
            Listing::Window {
                from,
                to,
                filed_only,
            } => {
                let folders = self.folder_ids().await?.clone();
                // One second early at the bottom, as Gmail's window is, so two
                // neighbouring chunks overlap rather than gap on a rounding.
                let filter = format!(
                    "receivedDateTime ge {} and receivedDateTime lt {}",
                    odata_instant(from - chrono::Duration::seconds(1)),
                    odata_instant(to)
                );
                let url = format!(
                    "{}/me/messages?$select=id,parentFolderId,isDraft&$top={PAGE_SIZE}&$filter={}",
                    self.api_base,
                    query_value(&filter)
                );
                self.page_ids(url, cap, urgency, move |e| {
                    if e.is_draft == Some(true) {
                        return false;
                    }
                    let Some(parent) = e.parent_folder_id.as_deref() else {
                        return true;
                    };
                    if folders.excluded.contains(parent) {
                        return false;
                    }
                    !(filed_only && (parent == folders.inbox || parent == folders.sent))
                })
                .await
            }
        }
    }

    /// No request: the initial delta round for each folder, reaching back
    /// [`CURSOR_OVERLAP`]. See the module doc.
    async fn current_cursor(&self) -> Result<SyncCursor> {
        let since = Utc::now() - CURSOR_OVERLAP;
        Ok(GraphCursor {
            v: CURSOR_VERSION,
            inbox: self.initial_delta(Folder::Inbox, since),
            sent: self.initial_delta(Folder::Sent, since),
        }
        .encode())
    }

    fn cursor_is_usable(&self, cursor: &SyncCursor) -> bool {
        GraphCursor::decode(cursor).is_some_and(|c| self.own_url(&c.inbox) && self.own_url(&c.sent))
    }

    async fn changes(
        &self,
        since: &SyncCursor,
        folder: Folder,
    ) -> std::result::Result<Changes, ChangesError> {
        let start = GraphCursor::decode(since).ok_or_else(|| {
            ChangesError::Other(CoreError::InvalidInput("not an Outlook sync cursor".into()))
        })?;
        let mut url = start
            .link(folder)
            .ok_or_else(|| {
                ChangesError::Other(CoreError::InvalidInput(
                    "the Outlook cursor has no change feed for this folder".into(),
                ))
            })?
            .to_string();
        let mut ids = Vec::new();
        let mut saw_changes = false;
        let delta = loop {
            let page: Page<MessageEntry> = self.get_json(&url, Urgency::Live).await?;
            saw_changes |= !page.value.is_empty();
            ids.extend(
                page.value
                    .into_iter()
                    .filter(|e| e.removed.is_none())
                    .map(|e| e.id),
            );
            match (page.next_link, page.delta_link) {
                (Some(next), _) => url = next,
                (None, Some(delta)) => break delta,
                // A final page with neither link is not a round Graph
                // finished; holding the cursor is the only safe answer.
                (None, None) => {
                    return Err(ChangesError::Other(CoreError::Other(anyhow::anyhow!(
                        "graph delta round ended without a delta link"
                    ))));
                }
            }
        };
        // A link we would refuse to follow next time is refused now, while
        // the old cursor still stands.
        self.require_own(&delta)?;
        ids.sort_unstable();
        ids.dedup();
        Ok(Changes {
            ids,
            cursor: start.with_link(folder, delta).encode(),
            // One folder's round says nothing about the other's.
            mailbox_unchanged: false,
            saw_changes,
        })
    }

    /// Each folder's link from whichever walk moved it relative to `since`.
    /// The walks are per folder, so at most one of them moved each link.
    fn merge_cursors(&self, since: &SyncCursor, a: &SyncCursor, b: &SyncCursor) -> SyncCursor {
        let (Some(s), Some(a), Some(b)) = (
            GraphCursor::decode(since),
            GraphCursor::decode(a),
            GraphCursor::decode(b),
        ) else {
            // Unreachable from the engine (all three came from this source);
            // `since` is the answer that never skips anything.
            return since.clone();
        };
        let pick = |s: &str, a: String, b: String| if a != s { a } else { b };
        GraphCursor {
            v: CURSOR_VERSION,
            inbox: pick(&s.inbox, a.inbox, b.inbox),
            sent: pick(&s.sent, a.sent, b.sent),
        }
        .encode()
    }

    /// The message's metadata, then its MIME (`$value`, raw bytes, not
    /// base64).
    async fn fetch_raw(&self, id: &str, urgency: Urgency) -> Result<FetchedMessage> {
        let base = format!("{}/me/messages/{}", self.api_base, path_segment(id));
        let meta: MessageMeta = self
            .get_json(
                &format!("{base}?$select=id,conversationId,receivedDateTime,parentFolderId,from"),
                urgency,
            )
            .await?;
        let resp = self.get_ok(&format!("{base}/$value"), urgency).await?;
        let raw = match read_capped(resp, MAX_RAW_BYTES).await {
            Ok(bytes) if bytes.is_empty() => RawBody::Missing,
            Ok(bytes) => RawBody::Bytes(bytes),
            Err(CoreError::InvalidInput(m)) => RawBody::Undecodable(m),
            Err(e) => return Err(e),
        };
        // Placement needs the folder ids; a failure to learn them reports
        // nothing, which hands the decision to the listing's folder.
        // SENT IS WHO WROTE IT, wherever it is filed now. Gmail's SENT label
        // follows a message into any folder; Exchange's Sent Items does not,
        // so the user's own mail filed in Archive or a folder of theirs is
        // recognised by its From.
        let from_addr = meta
            .from
            .as_ref()
            .and_then(|r| r.email_address.address.as_deref())
            .map(|a| a.trim().to_ascii_lowercase());
        let from_me = match from_addr {
            Some(a) if a == self.account_email.trim().to_ascii_lowercase() => true,
            Some(a) => self.own_addresses().await.contains(&a),
            None => false,
        };
        let placement = match (&meta.parent_folder_id, self.folder_ids().await) {
            (Some(parent), Ok(f)) => Placement {
                in_inbox: *parent == f.inbox,
                in_sent: *parent == f.sent || from_me,
            },
            _ => Placement {
                in_inbox: false,
                in_sent: from_me,
            },
        };
        Ok(FetchedMessage {
            id: if meta.id.is_empty() {
                id.to_string()
            } else {
                meta.id
            },
            thread_id: meta.conversation_id.filter(|c| !c.is_empty()),
            raw,
            received_at: parse_instant(meta.received_date_time.as_deref()),
            placement,
        })
    }

    /// Graph's structured address fields, rendered as the header values the
    /// engine asked for. Names it has no field for are left out.
    async fn fetch_headers(&self, id: &str, names: &[&str]) -> Result<FetchedHeaders> {
        let url = format!(
            "{}/me/messages/{}?$select=toRecipients,ccRecipients,bccRecipients,from,replyTo,receivedDateTime",
            self.api_base,
            path_segment(id)
        );
        let meta: RecipientsMeta = self.get_json(&url, Urgency::Live).await?;
        let mut headers = Vec::new();
        for name in names {
            let value = match name.to_ascii_lowercase().as_str() {
                "to" => render_list(&meta.to_recipients),
                "cc" => render_list(&meta.cc_recipients),
                "bcc" => render_list(&meta.bcc_recipients),
                "reply-to" => render_list(&meta.reply_to),
                "from" => meta.from.as_ref().and_then(render_address),
                _ => None,
            };
            if let Some(value) = value {
                headers.push((name.to_string(), value));
            }
        }
        Ok(FetchedHeaders {
            headers,
            received_at: parse_instant(meta.received_date_time.as_deref()),
        })
    }

    /// The inbox's unread item count. Graph has no thread count, so the
    /// message count stands for both.
    async fn inbox_unread(&self) -> Result<(i64, i64)> {
        let url = format!(
            "{}/me/mailFolders/inbox?$select=id,unreadItemCount",
            self.api_base
        );
        let f: FolderResp = self.get_json(&url, Urgency::Live).await?;
        Ok((f.unread_item_count, f.unread_item_count))
    }

    fn changes_report_updates(&self) -> bool {
        true
    }

    fn cursor_before_window(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests;
