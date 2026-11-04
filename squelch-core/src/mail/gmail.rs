//! Gmail as a [`MailSource`]: REST + polling. REST, not IMAP — the read-only
//! `gmail.readonly` scope works over REST and IMAP XOAUTH2 rejects it.
//!
//! Everything Gmail-shaped the sync engine used to say for itself lives here:
//! label ids, search operators, the `history.list` cursor, `format=raw`'s
//! base64url, and the quota-aware request layer (the shared
//! [`GmailBudget`], Google's 403-vs-429 reasons, `Retry-After`, the
//! catch-up's retry in place). The engine sees ids, bytes and an opaque
//! cursor.
//!
//! Tokens, headers and bodies are NEVER logged (SECURITY.md).

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use base64::Engine as _;
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use serde::Deserialize;

use super::{
    Changes, ChangesError, FetchedHeaders, FetchedMessage, Folder, Listing, MailProvider,
    MailSource, Placement, RawBody, SourceRefusals, SyncCursor, Urgency,
};
use crate::credentials::CredentialStore;
use crate::error::{CoreError, Result};
use crate::gmail_budget::{GmailBudget, gmail_path};
use crate::metrics::{GmailErrorKind, SyncMetrics};
use crate::types::AccountId;

/// Gmail REST base for the authenticated user. Fixed; not user-tunable. `pub`
/// so squelch-api's write path targets the same host from one definition.
pub const GMAIL_API_BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";

/// The INBOX system label. `pub` because the write path archives by removing
/// exactly this label.
pub const LABEL_INBOX: &str = "INBOX";
/// The SENT system label.
pub const LABEL_SENT: &str = "SENT";
/// The SPAM system label — Gmail's own verdict, which squelch surfaces on its
/// spam page and never re-litigates. `pub` because the write path's "not spam"
/// removes exactly this label (and adds [`LABEL_INBOX`] back).
pub const LABEL_SPAM: &str = "SPAM";

/// Decode a base64url (Gmail `format=raw`) payload into RFC822 bytes. Gmail
/// usually omits padding; both padded and unpadded input are accepted. Errors
/// are surfaced without content so one bad message can't poison the batch.
pub fn decode_raw_b64url(s: &str) -> Result<Vec<u8>> {
    // MEMORY GUARD: bound peak ingest memory ourselves rather than inheriting
    // Gmail's ~50MB limit. b64 length upper-bounds the decoded size.
    const MAX_RAW_BYTES: usize = 64 * 1024 * 1024;
    let t = s.trim();
    if t.len() / 4 * 3 > MAX_RAW_BYTES {
        return Err(CoreError::InvalidInput(
            "raw message exceeds the 64MB ingest bound".to_string(),
        ));
    }
    // Try no-pad first (Gmail's usual shape), then the padded variant.
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(t)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(t))
        .map_err(|e| CoreError::InvalidInput(format!("base64url decode failed: {e}")))
}

/// Decide whether the incremental poll can proceed or a fresh catch-up is
/// required. `expired` reflects an HTTP 404 from `history.list` (Gmail drops
/// history older than ~a week); a 0/absent `cursor` means first run. Pure, so
/// the 404-fallback path is unit-testable without a network.
pub fn history_poll_decision(cursor: Option<u64>, expired: bool) -> HistoryDecision {
    match cursor {
        Some(id) if id > 0 && !expired => HistoryDecision::Incremental(id),
        _ => HistoryDecision::FullCatchUp,
    }
}

/// The outcome of [`history_poll_decision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryDecision {
    /// Poll `history.list` starting from this historyId.
    Incremental(u64),
    /// historyId is absent or expired: do a fresh backfill-window catch-up.
    FullCatchUp,
}

/// Advance a historyId cursor to the max of itself and every `historyId`
/// observed in a page — never backwards. Pure, so it is unit-testable.
pub fn advance_history_cursor(current: u64, observed: impl IntoIterator<Item = u64>) -> u64 {
    observed.into_iter().fold(current, u64::max)
}

// ---- Gmail REST response shapes (only the fields we consume) ---------------
// These model the GMAIL API's own JSON, never squelch's client-facing wire
// contracts. squelch-api's write path deserializes the same Gmail resources, so
// the shared ones are `pub` here and defined exactly once.

#[derive(Debug, Deserialize)]
struct MessageRef {
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListMessagesResp {
    #[serde(default)]
    messages: Vec<MessageRef>,
    #[serde(default)]
    next_page_token: Option<String>,
}

/// A Gmail `users.messages.get` resource, across every format squelch asks for:
/// `format=raw` fills `raw`, `format=metadata` fills `payload.headers`, and a
/// field the requested format omits simply stays `None`/empty.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GmailMessage {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub thread_id: Option<String>,
    /// The message's labels as of this read; every format carries them.
    #[serde(default)]
    pub label_ids: Vec<String>,
    /// base64url of the full RFC822 message (present with `format=raw`).
    #[serde(default)]
    pub raw: Option<String>,
    /// Milliseconds since epoch as a decimal string (Gmail's `internalDate`).
    #[serde(default)]
    pub internal_date: Option<String>,
    /// MIME structure (present with `format=metadata`/`full`); squelch reads
    /// only its headers.
    #[serde(default)]
    pub payload: Option<MessagePayload>,
}

/// The `payload` object of a Gmail message; only `headers` is consumed.
#[derive(Debug, Default, Deserialize)]
pub struct MessagePayload {
    #[serde(default)]
    pub headers: Vec<MessageHeader>,
}

/// A single Gmail `payload.headers[]` entry.
#[derive(Debug, Clone, Deserialize)]
pub struct MessageHeader {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfileResp {
    history_id: String,
}

/// A Gmail `users.labels.get` resource; only the unread pair is read, and both
/// counters default to 0 because Gmail omits them on a label with nothing in it.
///
/// `id` is required for exactly that reason: with only defaulted fields, ANY
/// 200 body — `{}`, an error envelope, some proxy's interstitial — would decode
/// to a confident `(0, 0)` and overwrite real counts. Requiring the one field
/// every label resource carries turns those bodies into decode errors, which
/// the caller keeps its last known counts through.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LabelResp {
    /// Never read; its presence is the check.
    #[allow(dead_code)]
    id: String,
    #[serde(default)]
    messages_unread: i64,
    #[serde(default)]
    threads_unread: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryListResp {
    #[serde(default)]
    history: Vec<HistoryRecord>,
    #[serde(default)]
    next_page_token: Option<String>,
    /// The newest historyId as of this response.
    #[serde(default)]
    history_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryRecord {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    messages_added: Vec<HistoryMessageAdded>,
}

#[derive(Debug, Deserialize)]
struct HistoryMessageAdded {
    message: MessageRef,
}

/// Parse a decimal string historyId; malformed input yields 0 (treated as
/// "unknown", forcing a full catch-up rather than a panic).
pub(crate) fn parse_history_id(s: &str) -> u64 {
    s.trim().parse::<u64>().unwrap_or(0)
}

/// The label a [`Folder`] is in Gmail.
fn folder_label(folder: Folder) -> &'static str {
    match folder {
        Folder::Inbox => LABEL_INBOX,
        Folder::Sent => LABEL_SENT,
        Folder::Spam => LABEL_SPAM,
    }
}

/// The `labelIds` and `q` a [`Listing`] is in Gmail's `messages.list`.
///
/// THE ARCHIVE WINDOW in epoch seconds, which Gmail reads as exact instants (a
/// bare date is a day in the ACCOUNT's timezone, which this daemon does not
/// know). The lower bound is one second early so two neighbouring chunks
/// overlap by that second whichever way Gmail rounds the boundary; the overlap
/// costs nothing, because its ids are already stored. No `labelIds` there:
/// all mail, and Gmail itself leaves spam and trash out of such a listing.
/// Drafts and chats are excluded by the query; `filed_only` adds the two live
/// labels, so a message carrying either NOW is never listed.
pub(crate) fn listing_params(listing: Listing) -> (Option<&'static str>, Option<String>) {
    match listing {
        Listing::Folder {
            folder,
            within_days,
        } => (
            Some(folder_label(folder)),
            within_days.map(|days| format!("newer_than:{days}d")),
        ),
        Listing::Window {
            from,
            to,
            filed_only,
        } => (
            None,
            Some(format!(
                "after:{} before:{} {}-in:draft -in:chats",
                from.timestamp() - 1,
                to.timestamp(),
                if filed_only {
                    "-in:inbox -in:sent "
                } else {
                    ""
                }
            )),
        ),
    }
}

/// Everything a [`GmailSource`] is built from. The engine assembles it ON
/// FIRST USE, after its builders have run, so the budget, the metrics registry
/// and the test seams it carries are the ones the engine ended up with.
pub(crate) struct GmailSourceParts<C: CredentialStore + ?Sized> {
    pub http: reqwest::Client,
    pub creds: Arc<C>,
    pub account_id: AccountId,
    pub budget: Arc<GmailBudget>,
    pub metrics: Arc<SyncMetrics>,
    pub refusals: Arc<SourceRefusals>,
    /// Always [`GMAIL_API_BASE`] outside the tests. Not settable from outside
    /// the crate on purpose: a build that could be aimed at an arbitrary host
    /// would be a bearer-token exfiltration primitive.
    pub api_base: String,
    pub quota_backoff: Duration,
    pub quota_retry_cap: Duration,
}

/// Gmail's read side, on the READ credential.
pub struct GmailSource<C: CredentialStore + ?Sized> {
    http: reqwest::Client,
    creds: Arc<C>,
    account_id: AccountId,
    /// The mailbox's Gmail quota budget, shared with every other caller for
    /// the same account (see [`crate::gmail_budget`]).
    budget: Arc<GmailBudget>,
    metrics: Arc<SyncMetrics>,
    /// Where [`Urgency::Background`] refusals are counted for the archive walk.
    refusals: Arc<SourceRefusals>,
    api_base: String,
    quota_backoff: Duration,
    /// The longest a catch-up request sleeps on its own between quota retries
    /// ([`GMAIL_QUOTA_RETRY_CAP`]).
    quota_retry_cap: Duration,
}

impl<C: CredentialStore + ?Sized> GmailSource<C> {
    pub(crate) fn new(parts: GmailSourceParts<C>) -> Self {
        let GmailSourceParts {
            http,
            creds,
            account_id,
            budget,
            metrics,
            refusals,
            api_base,
            quota_backoff,
            quota_retry_cap,
        } = parts;
        Self {
            http,
            creds,
            account_id,
            budget,
            metrics,
            refusals,
            api_base,
            quota_backoff,
            quota_retry_cap,
        }
    }

    /// Authenticated GET returning parsed JSON. A 404 surfaces as
    /// [`CoreError::NotFound`] so callers can branch on it (the
    /// expired-historyId fallback). Header and body are NEVER logged.
    async fn get_json<T: for<'de> Deserialize<'de>>(&self, url: &str) -> Result<T> {
        self.get_json_with_retry(url, Urgency::Live).await
    }

    async fn get_json_with_retry<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
        retry: Urgency,
    ) -> Result<T> {
        let mut backoff = self.quota_backoff;
        for attempt in 0..=GMAIL_QUOTA_RETRIES {
            let quiet = retry == Urgency::Background;
            let resp = self.send_get(url, quiet).await?;
            let status = resp.status();
            if status.is_success() {
                self.budget.note_success();
                return resp
                    .json::<T>()
                    .await
                    .map_err(|e| CoreError::Other(anyhow::anyhow!("gmail json decode: {e}")));
            }
            if status == StatusCode::NOT_FOUND {
                return Err(CoreError::NotFound);
            }
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(gmail_retry_after);
            let body = resp.text().await.unwrap_or_default();
            let kind = classify_gmail_status(status.as_u16(), &body);
            // Google's refusal sets the pace for the whole mailbox: every
            // caller holding this budget (sweeps and write door included) is
            // held for the pause and comes back slower. See GmailBudget.
            let slowed = matches!(kind, GmailErrorKind::Quota)
                .then(|| self.budget.note_quota_refusal(retry_after));
            // When the budget took the refusal, IT does the waiting: every
            // admission is held until the hold ends. So a Retry-After past
            // this call's own sleep cap no longer has to end the request (and
            // restart a 799-message catch-up) unless the hold is hours long.
            let delay =
                gmail_quota_delay(backoff, retry_after, self.quota_retry_cap).or(match slowed {
                    Some((hold, _)) if hold <= CATCHUP_HOLD_CAP => Some(Duration::ZERO),
                    _ => None,
                });
            if matches!(kind, GmailErrorKind::Quota)
                && retry == Urgency::Window
                && attempt < GMAIL_QUOTA_RETRIES
                && let Some(delay) = delay
            {
                self.metrics.record_gmail_quota_retry();
                let (hold, unit) = slowed.unwrap_or_default();
                eprintln!(
                    "squelch: Gmail quota limit ({}{}); holding this mailbox's Gmail calls {}s, then retrying the same request at {:.0}% of the starting rate (catch-up position retained)",
                    gmail_error_reason(&body),
                    gmail_quota_limit_note(&body),
                    hold.max(delay).as_secs(),
                    self.budget.rate_percent(unit)
                );
                tokio::time::sleep(delay).await;
                backoff = (backoff * 2).min(self.quota_retry_cap);
                continue;
            }
            // The archive walk's refusals are counted and nothing more: its
            // lane waits and retries by itself, and "sync is failing" in front
            // of a current inbox would be a lie. A credential failure is the
            // mailbox's, whoever hit it first.
            if quiet {
                // Typed, for the walk's own bookkeeping: its lane backs off
                // differently on a quota refusal, and neither kind is ever a
                // message it may skip. See the engine's `ArchiveRun`.
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
                self.metrics
                    .record_gmail_error_reason(kind, gmail_error_reason(&body));
            }
            return Err(CoreError::Other(anyhow::anyhow!(
                "gmail api status {} (reason: {}{})",
                status.as_u16(),
                gmail_error_reason(&body),
                gmail_quota_limit_note(&body)
            )));
        }
        unreachable!("the final attempt returns")
    }

    /// [`Self::get_json`] for the engine's request-layer tests. Test-only: a
    /// crate-visible method that attaches the bearer token to any URL it is
    /// handed would be the exfiltration primitive `api_base` is guarded
    /// against.
    #[cfg(test)]
    pub(crate) async fn test_get_json<T: for<'de> Deserialize<'de>>(&self, url: &str) -> Result<T> {
        self.get_json(url).await
    }

    /// [`Self::get_json_with_retry`], test-only for the same reason.
    #[cfg(test)]
    pub(crate) async fn test_get_json_with_retry<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
        retry: Urgency,
    ) -> Result<T> {
        self.get_json_with_retry(url, retry).await
    }

    /// Send a GET with a Bearer token, retrying once on 401 with a fresh token.
    async fn send_get(&self, url: &str, quiet: bool) -> Result<reqwest::Response> {
        let token = self.token_for_request(false).await?;
        let resp = self.bearer_get(url, &token.access_token, quiet).await?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            // Redacted: the fact of a retry, never token/header content.
            eprintln!("squelch: gmail 401; refreshing token and retrying once");
            // FRESH: past the store's in-memory copy, which is the token Gmail
            // just refused; the backend may hold a newer one.
            let token = self.token_for_request(true).await?;
            return self.bearer_get(url, &token.access_token, quiet).await;
        }
        Ok(resp)
    }

    /// The access token for one Gmail call (`fresh`: past the credential
    /// store's in-memory copy, for the retry after a 401). A credential
    /// failure never reaches a status code — the refresh exchange is what
    /// failed, `invalid_grant` included — so it is counted as `auth` on the
    /// typed variant here rather than string-matched out of an error chain
    /// later.
    async fn token_for_request(&self, fresh: bool) -> Result<crate::credentials::OAuthToken> {
        let token = if fresh {
            self.creds.fresh_token(self.account_id).await
        } else {
            self.creds.token(self.account_id).await
        };
        token
            // The SUCCESS half, and it is not decoration: the failure below sets
            // a state a client renders as "your mailbox is disconnected", so
            // something has to clear it when the credential works again. Without
            // this the banner would latch on and send somebody who has already
            // reconnected round the same loop a second time.
            .inspect(|_| self.metrics.note_credential_ok())
            .inspect_err(|e| {
                if matches!(e, CoreError::Credential(_)) {
                    self.metrics.record_gmail_error(GmailErrorKind::Auth);
                }
            })
    }

    async fn bearer_get(
        &self,
        url: &str,
        access_token: &str,
        quiet: bool,
    ) -> Result<reqwest::Response> {
        // Admission is the mailbox's, not this engine's: every caller for the
        // account books against the same budget, 401 retries included.
        self.budget
            .admit("GET", gmail_path(url, &self.api_base))
            .await;
        self.http
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| {
                // No status ever arrived: DNS, TLS, connect or the client's own
                // timeout. Distinct from `http` because the fix is different.
                if quiet {
                    self.metrics.count_gmail_error(GmailErrorKind::Network);
                } else {
                    self.metrics.record_gmail_error(GmailErrorKind::Network);
                }
                CoreError::Other(anyhow::anyhow!("gmail request: {e}"))
            })
    }

    /// The one paging loop behind every `messages.list`. `label` is `None`
    /// only for the archive walk, which lists ALL MAIL.
    ///
    /// With a `cap`, pagination STOPS once that many ids are in hand. Gmail
    /// returns `messages.list` newest-first in practice, so a cap is a recent-N
    /// window. It is not a contract, which is why the only caller is the spam
    /// sync: there the cap is a bound on WORK ("do not fetch ten thousand
    /// bodies because somebody clicked a tab"), not a promise about which
    /// messages are in it. Nothing that has to be exact may use it.
    async fn list_message_ids_in(
        &self,
        label: Option<&str>,
        q: Option<&str>,
        cap: Option<usize>,
        retry: Urgency,
    ) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut params = Vec::new();
            if let Some(label) = label {
                params.push(format!("labelIds={label}"));
            }
            if let Some(q) = q {
                params.push(format!("q={}", urlencode(q)));
            }
            if let Some(tok) = &page_token {
                params.push(format!("pageToken={tok}"));
            }
            let mut url = format!("{}/messages", self.api_base);
            if !params.is_empty() {
                url.push('?');
                url.push_str(&params.join("&"));
            }
            let page: ListMessagesResp = self.get_json_with_retry(&url, retry).await?;
            ids.extend(page.messages.into_iter().map(|m| m.id));
            if let Some(cap) = cap
                && ids.len() >= cap
            {
                ids.truncate(cap);
                break;
            }
            match page.next_page_token {
                Some(tok) => page_token = Some(tok),
                None => break,
            }
        }
        Ok(ids)
    }
}

#[async_trait::async_trait]
impl<C: CredentialStore + ?Sized + 'static> MailSource for GmailSource<C> {
    fn provider(&self) -> MailProvider {
        MailProvider::Gmail
    }

    async fn list(
        &self,
        listing: Listing,
        cap: Option<usize>,
        urgency: Urgency,
    ) -> Result<Vec<String>> {
        let (label, q) = listing_params(listing);
        self.list_message_ids_in(label, q.as_deref(), cap, urgency)
            .await
    }

    /// `users.getProfile` -> the account's current historyId. Retried in place
    /// like a window: it closes one, and losing it re-lists the window.
    async fn current_cursor(&self) -> Result<SyncCursor> {
        let url = format!("{}/profile", self.api_base);
        let profile: ProfileResp = self.get_json_with_retry(&url, Urgency::Window).await?;
        Ok(SyncCursor::new(
            parse_history_id(&profile.history_id).to_string(),
        ))
    }

    fn cursor_is_usable(&self, cursor: &SyncCursor) -> bool {
        matches!(
            history_poll_decision(Some(parse_history_id(cursor.as_str())), false),
            HistoryDecision::Incremental(_)
        )
    }

    /// Walk `history.list` from `since` under ONE label filter, returning the
    /// deduped `messageAdded` ids and the max historyId observed. Every answer
    /// carries the mailbox's CURRENT historyId whatever label it was filtered
    /// by, so a current id equal to the start proves nothing anywhere changed.
    /// Persists nothing. A 404 (Gmail drops history older than ~a week) is
    /// [`ChangesError::Expired`].
    async fn changes(
        &self,
        since: &SyncCursor,
        folder: Folder,
    ) -> std::result::Result<Changes, ChangesError> {
        let start_history_id = parse_history_id(since.as_str());
        let label = folder_label(folder);
        let mut cursor = start_history_id;
        let mut current;
        let mut page_token: Option<String> = None;
        let mut new_ids: Vec<String> = Vec::new();

        loop {
            let mut url = format!(
                "{}/history?startHistoryId={start_history_id}\
                 &historyTypes=messageAdded&labelId={label}",
                self.api_base
            );
            if let Some(tok) = &page_token {
                url.push_str(&format!("&pageToken={tok}"));
            }
            let page: HistoryListResp = self.get_json(&url).await?;
            current = page.history_id.as_deref().map(parse_history_id);

            // Advance the cursor from every observed historyId (records + the
            // page-level newest id).
            let observed = page
                .history
                .iter()
                .filter_map(|r| r.id.as_deref().map(parse_history_id))
                .chain(page.history_id.as_deref().map(parse_history_id));
            cursor = advance_history_cursor(cursor, observed);

            for rec in &page.history {
                for added in &rec.messages_added {
                    new_ids.push(added.message.id.clone());
                }
            }

            match page.next_page_token {
                Some(tok) => page_token = Some(tok),
                None => break,
            }
        }

        // Dedup ids (a message can appear across pages); order is irrelevant —
        // dedup at the store keys on (account_id, gmail_msg_id).
        new_ids.sort_unstable();
        new_ids.dedup();
        let saw_changes = !new_ids.is_empty() || current != Some(start_history_id);
        Ok(Changes {
            ids: new_ids,
            cursor: SyncCursor::new(cursor.to_string()),
            saw_changes,
            mailbox_unchanged: current == Some(start_history_id),
        })
    }

    /// One historyId spans the whole mailbox and only grows, so the max of the
    /// two walks covers both; `since` is behind both by construction.
    /// The historyId must be taken before the window is listed: a backfill
    /// is minutes of fetches, and a historyId asked for afterwards is past
    /// every message that arrived during them. Replaying the overlap costs a
    /// few re-fetches the store collapses (`UNIQUE(account_id, gmail_msg_id)`).
    fn cursor_before_window(&self) -> bool {
        true
    }

    fn merge_cursors(&self, _since: &SyncCursor, a: &SyncCursor, b: &SyncCursor) -> SyncCursor {
        SyncCursor::new(
            advance_history_cursor(parse_history_id(a.as_str()), [parse_history_id(b.as_str())])
                .to_string(),
        )
    }

    /// One `messages.get?format=raw`, base64url-decoded.
    async fn fetch_raw(&self, id: &str, urgency: Urgency) -> Result<FetchedMessage> {
        let url = format!("{}/messages/{id}?format=raw", self.api_base);
        let msg: GmailMessage = self.get_json_with_retry(&url, urgency).await?;
        let has = |l: &str| msg.label_ids.iter().any(|x| x == l);
        let placement = Placement {
            in_inbox: has(LABEL_INBOX),
            in_sent: has(LABEL_SENT),
        };
        let raw = match msg.raw.as_deref().map(decode_raw_b64url) {
            Some(Ok(bytes)) => RawBody::Bytes(bytes),
            Some(Err(e)) => RawBody::Undecodable(e.to_string()),
            None => RawBody::Missing,
        };
        Ok(FetchedMessage {
            id: if msg.id.is_empty() {
                id.to_string()
            } else {
                msg.id
            },
            thread_id: msg.thread_id,
            raw,
            received_at: parse_internal_date(msg.internal_date.as_deref()),
            placement,
        })
    }

    /// `format=metadata` with only `names` requested: headers, never bodies.
    async fn fetch_headers(&self, id: &str, names: &[&str]) -> Result<FetchedHeaders> {
        let mut url = format!("{}/messages/{id}?format=metadata", self.api_base);
        for name in names {
            url.push_str("&metadataHeaders=");
            url.push_str(name);
        }
        let msg: GmailMessage = self.get_json(&url).await?;
        Ok(FetchedHeaders {
            received_at: parse_internal_date(msg.internal_date.as_deref()),
            headers: msg
                .payload
                .map(|p| p.headers)
                .unwrap_or_default()
                .into_iter()
                .map(|h| (h.name, h.value))
                .collect(),
        })
    }

    /// `users.labels.get(INBOX)` -> Gmail's own unread counts. The read scope
    /// cannot see read state through any other call.
    async fn inbox_unread(&self) -> Result<(i64, i64)> {
        let url = format!("{}/labels/{LABEL_INBOX}", self.api_base);
        let label: LabelResp = self.get_json(&url).await?;
        Ok((label.messages_unread, label.threads_unread))
    }
}

pub(crate) const GMAIL_QUOTA_RETRIES: usize = 2;
pub(crate) const GMAIL_QUOTA_RETRY_CAP: Duration = Duration::from_secs(30);

pub(crate) fn gmail_quota_delay(
    backoff: Duration,
    retry_after: Option<Duration>,
    cap: Duration,
) -> Option<Duration> {
    // A longer server instruction ends this call. Do not sleep unboundedly or
    // truncate the instruction and retry earlier than Gmail allows.
    if retry_after.is_some_and(|delay| delay > cap) {
        return None;
    }
    Some(backoff.min(cap).max(retry_after.unwrap_or_default()))
}

/// The longest budget hold a catch-up request waits out in place. Past it
/// (Google asking for hours: a daily limit, not a rolling minute) the request
/// ends and the lifecycle's own backoff takes over, as before.
pub(crate) const CATCHUP_HOLD_CAP: Duration = Duration::from_secs(300);

/// Retry-After accepts seconds or an HTTP date. Never retry before it elapses.
pub(crate) fn gmail_retry_after(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let at = DateTime::parse_from_rfc2822(value).ok()?;
    Some(
        (at.with_timezone(&Utc) - Utc::now())
            .to_std()
            .unwrap_or_default(),
    )
}

/// Which [`GmailErrorKind`] a non-2xx Gmail response is.
///
/// 429 is unambiguous. 403 is NOT: Google returns it both for "you are asking
/// too fast" and for "this credential may not do that", separated only by the
/// `reason` in the JSON body. Everything else is `http`, which is the bucket
/// that means "read the logs".
pub(crate) fn classify_gmail_status(status: u16, body: &str) -> GmailErrorKind {
    match status {
        401 => GmailErrorKind::Auth,
        // A scope/permission 403 stays in `http`: it is rare, it is not a
        // credential this daemon can refresh its way out of, and the stderr
        // line already names it. The quota test is shared with the write door.
        _ if crate::gmail_budget::is_quota_refusal(status, body) => GmailErrorKind::Quota,
        _ => GmailErrorKind::Http,
    }
}

/// Only emit known reason constants, never Google's free-form message or body.
/// Both legacy Gmail errors and google.rpc.ErrorInfo carry structured reasons.
pub(crate) fn gmail_error_reason(body: &str) -> &'static str {
    const KNOWN: &[&str] = &[
        "rateLimitExceeded",
        "userRateLimitExceeded",
        "quotaExceeded",
        "dailyLimitExceeded",
        "domainPolicy",
        "insufficientPermissions",
        "forbidden",
        "accessNotConfigured",
        "authError",
        "backendError",
        "SERVICE_DISABLED",
        "ACCESS_TOKEN_SCOPE_INSUFFICIENT",
        "RATE_LIMIT_EXCEEDED",
        "QUOTA_EXCEEDED",
    ];
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return "unavailable";
    };
    for field in ["details", "errors"] {
        if let Some(entries) = value["error"][field].as_array() {
            for entry in entries {
                if let Some(reason) = entry["reason"].as_str()
                    && let Some(known) = KNOWN.iter().find(|known| **known == reason)
                {
                    return known;
                }
            }
        }
    }
    "unavailable"
}

/// Which quota Google says was exceeded, as `, limit: <name>`, or empty.
///
/// A `RATE_LIMIT_EXCEEDED` alone does not say whether the per-user or the
/// per-project budget ran out, and the per-user one is the limit the Cloud
/// console cannot chart. `google.rpc.ErrorInfo` names it in
/// `metadata.quota_limit` (falling back to `quota_metric`). Only an identifier
/// is emitted: ASCII letters, digits and `_./-`, at most 96 bytes, or nothing.
pub(crate) fn gmail_quota_limit_note(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return String::new();
    };
    let Some(entries) = value["error"]["details"].as_array() else {
        return String::new();
    };
    for entry in entries {
        for key in ["quota_limit", "quota_metric"] {
            if let Some(name) = entry["metadata"][key].as_str()
                && !name.is_empty()
                && name.len() <= 96
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'/' | b'-'))
            {
                // What Google says it enforces, so a log line settles whether
                // this project really is on the 6,000-unit standard model.
                let value = entry["metadata"]["quota_limit_value"]
                    .as_str()
                    .filter(|v| {
                        !v.is_empty() && v.len() <= 12 && v.bytes().all(|b| b.is_ascii_digit())
                    })
                    .map(|v| format!(" = {v}"))
                    .unwrap_or_default();
                return format!(", limit: {name}{value}");
            }
        }
    }
    String::new()
}

/// Minimal percent-encoding for a Gmail `q` value. Enough for `newer_than:Nd`
/// and simple queries; arbitrary user queries are never built here.
pub(crate) fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' => {
                out.push(b as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Gmail `internalDate` is milliseconds-since-epoch as a decimal string. `pub`
/// so squelch-api's send-echo parses it from one definition.
pub fn parse_internal_date(s: Option<&str>) -> Option<DateTime<Utc>> {
    let ms: i64 = s?.trim().parse().ok()?;
    DateTime::from_timestamp_millis(ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// The listing strings are Gmail's search grammar and the archive walk's
    /// whole correctness story (see the engine's forward pass), so they are
    /// pinned character for character here as well as on the mock's wire.
    #[test]
    fn listings_are_the_labels_and_queries_the_engine_always_sent() {
        assert_eq!(
            listing_params(Listing::Folder {
                folder: Folder::Inbox,
                within_days: Some(30)
            }),
            (Some("INBOX"), Some("newer_than:30d".to_string()))
        );
        assert_eq!(
            listing_params(Listing::Folder {
                folder: Folder::Sent,
                within_days: None
            }),
            (Some("SENT"), None)
        );
        assert_eq!(
            listing_params(Listing::Folder {
                folder: Folder::Spam,
                within_days: Some(7)
            }),
            (Some("SPAM"), Some("newer_than:7d".to_string()))
        );
        let from = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let to = Utc.timestamp_opt(1_700_604_800, 0).unwrap();
        assert_eq!(
            listing_params(Listing::Window {
                from,
                to,
                filed_only: false
            }),
            (
                None,
                Some("after:1699999999 before:1700604800 -in:draft -in:chats".to_string())
            )
        );
        assert_eq!(
            listing_params(Listing::Window {
                from,
                to,
                filed_only: true
            }),
            (
                None,
                Some(
                    "after:1699999999 before:1700604800 -in:inbox -in:sent -in:draft -in:chats"
                        .to_string()
                )
            )
        );
    }
}
