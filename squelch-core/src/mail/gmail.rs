//! The Gmail read transport: REST + polling, behind [`MailSource`].
//!
//! REST, not IMAP — the read-only `gmail.readonly` scope works over REST and
//! IMAP XOAUTH2 rejects it. A first run lists INBOX+SENT over the backfill
//! window and records the account's `historyId`; after that `history.list`
//! polls `messageAdded` under BOTH labels — INBOX for incoming mail, SENT for
//! whatever the user wrote from another client — with a 404 (expired history)
//! surfacing as [`CoreError::NotFound`] so the engine can re-list.
//!
//! Tokens, headers and bodies are NEVER logged.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use serde::Deserialize;

use crate::credentials::CredentialStore;
use crate::error::{CoreError, Result};
use crate::mail::{Cursor, MailSource, NewMessageRef, PollOutcome, RawMessage, SentRecipients};
use crate::metrics::{GmailErrorKind, SyncMetrics};
use crate::sync::ingest::collect_mailboxes;
use crate::types::AccountId;

/// Gmail REST base for the authenticated user. Fixed; not user-tunable. `pub`
/// so squelch-api's write path targets the same host from one definition.
pub const GMAIL_API_BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";

/// The INBOX system label. `pub` because the write path archives by removing
/// exactly this label.
pub const LABEL_INBOX: &str = "INBOX";
/// The SENT system label.
pub const LABEL_SENT: &str = "SENT";

/// How many sent messages the contacts harvest scans between progress lines.
const HARVEST_PROGRESS_EVERY: usize = 1000;

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

/// Gmail `internalDate` is milliseconds-since-epoch as a decimal string. `pub`
/// so squelch-api's send-echo parses it from one definition.
pub fn parse_internal_date(s: Option<&str>) -> Option<DateTime<Utc>> {
    let ms: i64 = s?.trim().parse().ok()?;
    DateTime::from_timestamp_millis(ms)
}

/// Advance a historyId cursor to the max of itself and every `historyId`
/// observed in a page — never backwards. Pure, so it is unit-testable.
pub fn advance_history_cursor(current: u64, observed: impl IntoIterator<Item = u64>) -> u64 {
    observed.into_iter().fold(current, u64::max)
}

/// Parse a decimal string historyId; malformed input yields 0 (treated as
/// "unknown", forcing a full re-list rather than a panic).
pub fn parse_history_id(s: &str) -> u64 {
    s.trim().parse::<u64>().unwrap_or(0)
}

/// Decide whether an incremental poll can proceed or a fresh listing is
/// required. `expired` reflects an HTTP 404 from `history.list` (Gmail drops
/// history older than ~a week); a 0/absent `cursor` means the position is
/// unknown. Pure, so the fallback path is unit-testable without a network.
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
    /// The historyId is absent or expired: only a fresh listing recovers it.
    FullCatchUp,
}

/// Drop from `ids` every id also in `claimed`, preserving order. Used to hand the
/// SENT batch only what the INBOX batch did NOT already claim: a self-addressed
/// message carries both labels, and the store's message upsert writes
/// `is_sent = excluded.is_sent` on conflict, so re-ingesting it as sent would
/// hide the visible copy. Pure, so the precedence rule is unit-testable.
fn subtract_ids(ids: Vec<String>, claimed: &[String]) -> Vec<String> {
    if claimed.is_empty() {
        return ids;
    }
    let claimed: std::collections::HashSet<&str> = claimed.iter().map(|s| s.as_str()).collect();
    ids.into_iter()
        .filter(|id| !claimed.contains(id.as_str()))
        .collect()
}

/// Which [`GmailErrorKind`] a non-2xx Gmail response is.
///
/// 429 is unambiguous. 403 is NOT: Google returns it both for "you are asking
/// too fast" and for "this credential may not do that", separated only by the
/// `reason` in the JSON body. Everything else is `http`, which is the bucket
/// that means "read the logs".
fn classify_gmail_status(status: u16, body: &str) -> GmailErrorKind {
    const QUOTA_REASONS: [&str; 4] = [
        "ratelimitexceeded",
        "userratelimitexceeded",
        "quotaexceeded",
        "dailylimitexceeded",
    ];
    match status {
        401 => GmailErrorKind::Auth,
        429 => GmailErrorKind::Quota,
        // Bounded: an error body is a few hundred bytes, and the reason sits at
        // the front of it. Never logged, only matched.
        403 => {
            let head: String = body.chars().take(2048).collect::<String>().to_lowercase();
            if QUOTA_REASONS.iter().any(|r| head.contains(r)) {
                GmailErrorKind::Quota
            } else {
                // A scope/permission 403 stays in `http`: it is rare, it is not
                // a credential this daemon can refresh its way out of, and the
                // stderr line already names it.
                GmailErrorKind::Http
            }
        }
        _ => GmailErrorKind::Http,
    }
}

/// Minimal percent-encoding for a Gmail `q` value. Enough for `newer_than:Nd`
/// and simple queries; arbitrary user queries are never built here.
fn urlencode(s: &str) -> String {
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

/// Rebuild an RFC822 header block from Gmail's `payload.headers[]` so the same
/// mail-parser path that reads a real message reads these too — grouped address
/// lists, quoted display names and RFC2047 encoding all behave identically.
///
/// A header whose name or value carries CR/LF is DROPPED rather than emitted:
/// Gmail's own values are bare, so a line break here is header injection, not
/// data.
pub fn synthesize_rfc822_headers(headers: &[MessageHeader]) -> String {
    let mut out = String::new();
    for h in headers {
        if h.name.contains(['\r', '\n']) || h.value.contains(['\r', '\n']) {
            continue;
        }
        out.push_str(&h.name);
        out.push_str(": ");
        out.push_str(&h.value);
        out.push_str("\r\n");
    }
    out.push_str("\r\n");
    out
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

/// A single Gmail `payload.headers[]` entry. Also what the contacts-seeding
/// tests build to exercise header parsing via [`synthesize_rfc822_headers`].
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

/// The Gmail REST transport.
///
/// Generic over the credential store rather than boxing it, because the engine
/// that builds this is itself generic over one and an `Arc<C>` with `C: ?Sized`
/// cannot be coerced to a trait object without knowing it is sized.
pub struct GmailSource<C: CredentialStore + ?Sized> {
    http: reqwest::Client,
    creds: Arc<C>,
    account_id: AccountId,
    metrics: Arc<SyncMetrics>,
    /// Gmail REST base for every call this source makes. Always
    /// [`GMAIL_API_BASE`] in production; tests point it at a loopback mock so
    /// the walk/cursor rules are asserted on the wire rather than on a struct.
    api_base: String,
}

impl<C: CredentialStore + ?Sized> GmailSource<C> {
    pub fn new(
        http: reqwest::Client,
        creds: Arc<C>,
        account_id: AccountId,
        metrics: Arc<SyncMetrics>,
        api_base: String,
    ) -> Self {
        Self {
            http,
            creds,
            account_id,
            metrics,
            api_base,
        }
    }

    /// Authenticated GET returning parsed JSON. A 404 surfaces as
    /// [`CoreError::NotFound`] so callers can branch on it (the
    /// expired-historyId fallback). Header and body are NEVER logged.
    async fn get_json<T: for<'de> Deserialize<'de>>(&self, url: &str) -> Result<T> {
        let resp = self.send_get(url).await?;
        match resp.status() {
            s if s.is_success() => resp
                .json::<T>()
                .await
                .map_err(|e| CoreError::Other(anyhow::anyhow!("gmail json decode: {e}"))),
            // NOT an error metric: this is the expired-historyId signal the
            // caller recovers from with a re-list, and counting a routine
            // self-heal would make the errors family unalertable.
            StatusCode::NOT_FOUND => Err(CoreError::NotFound),
            s => {
                // Classified HERE, the last place the status is typed AND the
                // body is still readable: a 403 is a quota refusal or an
                // authorization one depending only on the reason string Google
                // puts in the body, and once this is an `anyhow` chain nothing
                // downstream can tell them apart. The body is read for that one
                // decision and never logged.
                let body = resp.text().await.unwrap_or_default();
                self.metrics
                    .record_gmail_error(classify_gmail_status(s.as_u16(), &body));
                Err(CoreError::Other(anyhow::anyhow!(
                    "gmail api status {}",
                    s.as_u16()
                )))
            }
        }
    }

    /// Send a GET with a Bearer token, retrying once on 401 with a fresh token.
    async fn send_get(&self, url: &str) -> Result<reqwest::Response> {
        let token = self.token_for_request().await?;
        let resp = self.bearer_get(url, &token.access_token).await?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            // Redacted: the fact of a retry, never token/header content.
            eprintln!("squelch: gmail 401; refreshing token and retrying once");
            let token = self.token_for_request().await?;
            return self.bearer_get(url, &token.access_token).await;
        }
        Ok(resp)
    }

    /// The access token for one Gmail call. A credential failure never reaches
    /// a status code — the refresh exchange is what failed, `invalid_grant`
    /// included — so it is counted as `auth` on the typed variant here rather
    /// than string-matched out of an error chain later.
    async fn token_for_request(&self) -> Result<crate::credentials::OAuthToken> {
        self.creds.token(self.account_id).await.inspect_err(|e| {
            if matches!(e, CoreError::Credential(_)) {
                self.metrics.record_gmail_error(GmailErrorKind::Auth);
            }
        })
    }

    async fn bearer_get(&self, url: &str, access_token: &str) -> Result<reqwest::Response> {
        self.http
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| {
                // No status ever arrived: DNS, TLS, connect or the client's own
                // timeout. Distinct from `http` because the fix is different.
                self.metrics.record_gmail_error(GmailErrorKind::Network);
                CoreError::Other(anyhow::anyhow!("gmail request: {e}"))
            })
    }

    /// List all message ids under `label`, optionally narrowed by a Gmail search
    /// `q`. Paginates fully.
    async fn list_message_ids(&self, label: &str, q: Option<&str>) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut url = format!("{}/messages?labelIds={label}", self.api_base);
            if let Some(q) = q {
                url.push_str(&format!("&q={}", urlencode(q)));
            }
            if let Some(tok) = &page_token {
                url.push_str(&format!("&pageToken={tok}"));
            }
            let page: ListMessagesResp = self.get_json(&url).await?;
            ids.extend(page.messages.into_iter().map(|m| m.id));
            match page.next_page_token {
                Some(tok) => page_token = Some(tok),
                None => break,
            }
        }
        Ok(ids)
    }

    /// Walk `history.list` from `start_history_id` under ONE label filter,
    /// returning the deduped `messageAdded` ids and the max historyId observed.
    /// Persists nothing: the caller owns the single cursor commit that covers
    /// every label. Propagates [`CoreError::NotFound`] on an expired historyId.
    async fn history_walk_label(
        &self,
        start_history_id: u64,
        label: &str,
    ) -> Result<(Vec<String>, u64)> {
        let mut cursor = start_history_id;
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
        Ok((new_ids, cursor))
    }

    /// `users.getProfile` -> the account's current historyId.
    async fn fetch_profile_history_id(&self) -> Result<u64> {
        let url = format!("{}/profile", self.api_base);
        let profile: ProfileResp = self.get_json(&url).await?;
        Ok(parse_history_id(&profile.history_id))
    }

    /// `format=metadata` with only To/Cc requested: headers, never bodies, so
    /// deep history costs one light call per sent message.
    async fn metadata_recipients(
        &self,
        id: &str,
    ) -> Result<(GmailMessage, Vec<(String, Option<String>)>)> {
        let url = format!(
            "{}/messages/{id}?format=metadata\
             &metadataHeaders=To&metadataHeaders=Cc",
            self.api_base
        );
        let msg: GmailMessage = self.get_json(&url).await?;
        let headers = msg
            .payload
            .as_ref()
            .map(|p| p.headers.clone())
            .unwrap_or_default();
        // Through mail-parser via header synthesis, so grouped lists, quoted
        // display names and RFC2047 encoding parse exactly as ingest does.
        let blob = synthesize_rfc822_headers(&headers);
        let mut mailboxes: Vec<(String, Option<String>)> = Vec::new();
        if let Some(parsed) = mail_parser::MessageParser::default().parse(blob.as_bytes()) {
            for list in [parsed.to(), parsed.cc()].into_iter().flatten() {
                collect_mailboxes(list, &mut mailboxes);
            }
        }
        Ok((msg, mailboxes))
    }

    fn refs(ids: Vec<String>, is_sent: bool) -> Vec<NewMessageRef> {
        ids.into_iter()
            .map(|id| NewMessageRef { id, is_sent })
            .collect()
    }
}

#[async_trait]
impl<C: CredentialStore + ?Sized + 'static> MailSource for GmailSource<C> {
    fn provider(&self) -> &'static str {
        "gmail"
    }

    async fn initial_listing(&self, window_days: u32) -> Result<PollOutcome> {
        let q = format!("newer_than:{window_days}d");

        // INBOX first, and its ids subtracted from the SENT batch, for the same
        // reason the history walk orders itself that way: a self-addressed
        // message is in both listings and its visible copy must win the
        // unique-key race.
        let inbox_ids = self.list_message_ids(LABEL_INBOX, Some(&q)).await?;
        let sent_ids = self.list_message_ids(LABEL_SENT, Some(&q)).await?;
        let sent_only = subtract_ids(sent_ids, &inbox_ids);

        // THE CURSOR IS TAKEN AFTER THE LISTING, and that leaves a gap: mail
        // arriving between the two is not in the listing and is older than the
        // historyId, so the next walk steps over it. Gmail offers no way to
        // close it (there is no "list as of this historyId"), and the cost of
        // taking the profile FIRST would be re-ingesting the whole window on
        // every recovery. The window is one round trip wide and the next
        // catch-up re-lists anyway.
        let history_id = self.fetch_profile_history_id().await?;

        let mut new_messages = Self::refs(inbox_ids, false);
        new_messages.extend(Self::refs(sent_only, true));
        Ok(PollOutcome {
            new_messages,
            cursor: history_id.to_string(),
        })
    }

    async fn poll_changes(&self, cursor: &Cursor) -> Result<PollOutcome> {
        // 0 is "unknown", not a position: a malformed cursor, or the sentinel a
        // pre-seam install could persist. Only a fresh listing recovers it, and
        // NotFound is how this transport says so.
        let start = match history_poll_decision(Some(parse_history_id(cursor)), false) {
            HistoryDecision::Incremental(id) => id,
            HistoryDecision::FullCatchUp => return Err(CoreError::NotFound),
        };

        // TWO label-filtered walks from the SAME start cursor, INBOX then SENT,
        // with one cursor covering both. The SENT walk is what makes mail the
        // user writes from Gmail web or their phone land here at all: such a
        // message never carries INBOX, so an INBOX-only walk is blind to it, and
        // the api's post-send echo covers only what was sent through the daemon
        // itself. Both walks start from the same cursor because one Gmail cursor
        // spans the whole mailbox.
        let (inbox_ids, inbox_cursor) = self.history_walk_label(start, LABEL_INBOX).await?;

        // The SENT half is best-effort AGAINST THE CURSOR: any failure returns
        // the cursor UNMOVED so the next poll re-walks both labels from the same
        // start rather than stepping over INBOX history the successful half
        // already covered. Re-walking costs a few fetches and changes nothing:
        // `UNIQUE(account_id, gmail_msg_id)` collapses the message rows and
        // `UNIQUE(message_id)` on `events` collapses the notifications.
        let (sent_ids, sent_cursor) = match self.history_walk_label(start, LABEL_SENT).await {
            Ok(v) => v,
            // An expired historyId is not a SENT-specific failure: the cursor
            // itself is gone and only a fresh listing can recover it.
            Err(CoreError::NotFound) => return Err(CoreError::NotFound),
            Err(e) => {
                eprintln!("squelch: sent history walk failed ({e}); holding the cursor");
                return Ok(PollOutcome {
                    new_messages: Self::refs(inbox_ids, false),
                    cursor: cursor.clone(),
                });
            }
        };

        let sent_only = subtract_ids(sent_ids, &inbox_ids);
        let advanced = advance_history_cursor(start, [inbox_cursor, sent_cursor]);

        let mut new_messages = Self::refs(inbox_ids, false);
        new_messages.extend(Self::refs(sent_only, true));
        Ok(PollOutcome {
            new_messages,
            cursor: advanced.to_string(),
        })
    }

    async fn fetch_raw(&self, id: &str) -> Result<RawMessage> {
        let url = format!("{}/messages/{id}?format=raw", self.api_base);
        let msg: GmailMessage = self.get_json(&url).await?;
        // A resource with no `raw` is nothing to ingest. Distinct from a
        // transport failure: the caller skips the message and keeps the batch.
        let raw_b64 = msg
            .raw
            .as_deref()
            .ok_or_else(|| CoreError::InvalidInput("gmail message carried no raw body".into()))?;
        Ok(RawMessage {
            id: if msg.id.is_empty() {
                id.to_string()
            } else {
                msg.id.clone()
            },
            raw: decode_raw_b64url(raw_b64)?,
            thread_id: msg.thread_id.clone(),
            internal_date: parse_internal_date(msg.internal_date.as_deref()),
        })
    }

    async fn fetch_recipients(&self, id: &str) -> Result<Vec<(String, Option<String>)>> {
        let (_, mailboxes) = self.metadata_recipients(id).await?;
        Ok(mailboxes)
    }

    async fn list_sent_recipients(&self) -> Result<Vec<SentRecipients>> {
        // The WHOLE Sent mailbox, no window: the harvest seeds contacts from
        // every address the account has ever written to.
        let ids = self.list_message_ids(LABEL_SENT, None).await?;
        eprintln!(
            "squelch: sent-contacts harvest scanning {} sent messages (headers only)",
            ids.len()
        );

        let mut out = Vec::with_capacity(ids.len());
        for (i, id) in ids.iter().enumerate() {
            let (msg, mailboxes) = self.metadata_recipients(id).await?;
            out.push(SentRecipients {
                sent_at: parse_internal_date(msg.internal_date.as_deref()),
                mailboxes,
            });
            if (i + 1) % HARVEST_PROGRESS_EVERY == 0 {
                eprintln!(
                    "squelch: sent-contacts harvest {}/{} messages scanned",
                    i + 1,
                    ids.len()
                );
            }
        }
        Ok(out)
    }

    async fn unread_counts(&self) -> Result<(i64, i64)> {
        let url = format!("{}/labels/{LABEL_INBOX}", self.api_base);
        let label: LabelResp = self.get_json(&url).await?;
        Ok((label.messages_unread, label.threads_unread))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 403 split is the whole reason this classifier reads the body: Google
    /// spends one status on "too fast" and on "not allowed", and only the reason
    /// string tells them apart.
    #[test]
    fn gmail_status_classification_splits_403_on_the_reason() {
        let quota_body = r#"{"error":{"errors":[{"reason":"userRateLimitExceeded"}]}}"#;
        assert_eq!(
            classify_gmail_status(403, quota_body),
            GmailErrorKind::Quota
        );
        assert_eq!(
            classify_gmail_status(403, r#"{"error":{"errors":[{"reason":"forbidden"}]}}"#),
            GmailErrorKind::Http
        );
        assert_eq!(classify_gmail_status(429, ""), GmailErrorKind::Quota);
        assert_eq!(classify_gmail_status(401, ""), GmailErrorKind::Auth);
        assert_eq!(classify_gmail_status(500, ""), GmailErrorKind::Http);
    }

    #[test]
    fn decode_raw_b64url_no_pad_round_trips() {
        let eml = "From: a@b.com\r\nSubject: hi\r\n\r\nbody\r\n";
        let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(eml);
        let out = decode_raw_b64url(&enc).unwrap();
        assert_eq!(out, eml.as_bytes());
    }

    #[test]
    fn decode_raw_b64url_accepts_padded_and_web_safe() {
        // 4 bytes => 6 base64 chars + '==' padding; values force '-'/'_' web-safe.
        let bytes: Vec<u8> = vec![0xfb, 0xff, 0xbf, 0xf0];
        let padded = base64::engine::general_purpose::URL_SAFE.encode(&bytes);
        assert!(padded.contains('='), "expected padding in this fixture");
        assert!(
            padded.contains('-') || padded.contains('_'),
            "expected web-safe chars in this fixture"
        );
        let out = decode_raw_b64url(&padded).unwrap();
        assert_eq!(out, bytes);
    }

    #[test]
    fn decode_raw_b64url_rejects_garbage() {
        assert!(decode_raw_b64url("!!!not base64!!!").is_err());
    }

    #[test]
    fn advance_history_cursor_takes_max_never_regresses() {
        assert_eq!(advance_history_cursor(100, [50, 75, 40]), 100);
        assert_eq!(advance_history_cursor(100, [150, 120, 200]), 200);
        assert_eq!(advance_history_cursor(0, std::iter::empty()), 0);
        assert_eq!(advance_history_cursor(10, [10]), 10);
    }

    #[test]
    fn history_decision_incremental_when_cursor_present_and_fresh() {
        assert_eq!(
            history_poll_decision(Some(4242), false),
            HistoryDecision::Incremental(4242)
        );
    }

    #[test]
    fn history_decision_full_catchup_on_expired() {
        assert_eq!(
            history_poll_decision(Some(4242), true),
            HistoryDecision::FullCatchUp
        );
    }

    #[test]
    fn history_decision_full_catchup_when_absent_or_zero() {
        assert_eq!(
            history_poll_decision(None, false),
            HistoryDecision::FullCatchUp
        );
        assert_eq!(
            history_poll_decision(Some(0), false),
            HistoryDecision::FullCatchUp
        );
    }

    #[test]
    fn synthesize_headers_drops_injected_newlines() {
        let headers = vec![MessageHeader {
            name: "From".into(),
            value: "x@y.com\r\nBcc: evil@z.com".into(),
        }];
        let raw = synthesize_rfc822_headers(&headers);
        assert!(!raw.contains("Bcc"), "CRLF-injected header must be dropped");
    }

    #[test]
    fn parse_internal_date_millis() {
        // 2026-07-07T10:00:00Z = 1783591200000 ms.
        let dt = parse_internal_date(Some("1783591200000")).unwrap();
        assert_eq!(dt.timestamp(), 1783591200);
        assert!(parse_internal_date(None).is_none());
        assert!(parse_internal_date(Some("garbage")).is_none());
    }

    #[test]
    fn parse_history_id_handles_bad_input() {
        assert_eq!(parse_history_id("12345"), 12345);
        assert_eq!(parse_history_id(""), 0);
        assert_eq!(parse_history_id("not-a-number"), 0);
    }

    #[test]
    fn urlencode_escapes_spaces_and_reserved() {
        assert_eq!(urlencode("newer_than:30d"), "newer_than:30d");
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("x&y"), "x%26y");
    }

    #[test]
    fn subtract_ids_drops_the_claimed_and_keeps_order() {
        let claimed = vec!["b".to_string(), "d".to_string()];
        let ids = vec!["a".into(), "b".into(), "c".into(), "d".into()];
        assert_eq!(
            subtract_ids(ids, &claimed),
            vec!["a".to_string(), "c".into()]
        );
        // Nothing claimed: the batch passes through untouched.
        assert_eq!(
            subtract_ids(vec!["a".to_string()], &[]),
            vec!["a".to_string()]
        );
    }
}
