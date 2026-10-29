//! Outlook WRITE operations over Microsoft Graph, behind [`MailWriter`].
//!
//! Same rules as the Gmail writer: the write token is fetched per request from
//! a store bound to the Write slot, never retained, never logged, and no
//! upstream body is ever echoed. What Graph changes:
//!
//! - **IMMUTABLE IDS ON EVERY REQUEST** (`Prefer: IdType="ImmutableId"`), as
//!   the read side sends them. Archive and trash are folder MOVES in Graph,
//!   and without the preference the moved message would come back under a
//!   new id the stored row does not know.
//! - **SEND IS DRAFT, THEN SEND.** `POST /me/sendMail` answers `202` with no
//!   id, which would cost the audit row its id and the send its echo. So the
//!   MIME is posted as a draft (`POST /me/messages`, base64 body,
//!   `text/plain`), which answers with the message, and then sent
//!   (`POST /me/messages/{id}/send`). With immutable ids the draft's id is the
//!   sent item's id, so [`SentRef::id`] is what the echo fetches. A draft
//!   whose send Graph REFUSED (a status, not a dropped connection) is deleted
//!   best-effort; one whose send outcome is unknown is left alone, because
//!   deleting it might delete mail that went out.
//! - **LABELS ARE GMAIL'S.** [`MailWriter::modify`] maps the ones Outlook has
//!   a meaning for (`UNREAD` is read state, `STARRED` is the flag, removing
//!   `INBOX` is an archive) and refuses the rest with a 400, never a guess.
//! - **THROTTLING** is a `429` with `Retry-After`, answered as
//!   [`WriteError::Paced`]: nothing was done, try again then.
//! - **A REQUEST IS AT MOST 4 MB.** Graph refuses larger bodies outright;
//!   a bigger message needs an upload session this writer does not open yet,
//!   so it is refused before anything is created.

use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};

use squelch_core::credentials::CredentialStore;
use squelch_core::mail::graph::PREFER_IMMUTABLE_IDS;
use squelch_core::types::AccountId;

use super::mime::{OUTBOX_HEADER, bare_message_id, outbox_token};
use super::{FetchedRaw, MailWriter, ParentHeaders, SentRef, WriteError, send_budget};

/// Graph's ceiling on one request body.
const GRAPH_MAX_REQUEST: usize = 4 * 1024 * 1024;

/// Candidates per page, and pages, of the scheduled-send Sent scan; the same
/// bounds the Gmail writer uses (see [`super::gmail::SCAN_PAGE`]).
use super::gmail::{SCAN_MAX_PAGES, SCAN_PAGE};

/// The Gmail labels [`MailWriter::modify`] can translate, and the refusal for
/// every other.
const LABELS_REFUSED: &str =
    "labels are a Gmail concept; an Outlook account can only mark read or unread, flag, or archive";

/// Percent-encode one path segment or query value.
fn enc(s: &str) -> String {
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

/// An OData string literal: single quotes doubled.
fn odata_string(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
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

#[derive(Debug, Deserialize)]
struct Header {
    name: String,
    value: String,
}

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
    internet_message_id: Option<String>,
    #[serde(default)]
    internet_message_headers: Vec<Header>,
    #[serde(default)]
    from: Option<Recipient>,
    #[serde(default)]
    reply_to: Vec<Recipient>,
    #[serde(default)]
    to_recipients: Vec<Recipient>,
    #[serde(default)]
    cc_recipients: Vec<Recipient>,
}

#[derive(Debug, Deserialize)]
struct Page {
    #[serde(default)]
    value: Vec<MessageMeta>,
    #[serde(rename = "@odata.nextLink", default)]
    next_link: Option<String>,
}

/// One address as a header would carry it; the reply path re-derives bare
/// addresses from these values, so a display name never reaches the wire.
fn render(r: &Recipient) -> Option<String> {
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
    let parts: Vec<String> = list.iter().filter_map(render).collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// `raw` with its `From:` address replaced by `addr`, keeping the display
/// name exactly as written (encoded words and all). `None` when the address
/// is already `addr`, when there is no single From header, or when the header
/// block is not UTF-8: the message then goes as composed.
pub(crate) fn rewrite_from(raw: &[u8], addr: &str) -> Option<Vec<u8>> {
    let end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 2)
        .or_else(|| raw.windows(2).position(|w| w == b"\n\n").map(|p| p + 1))?;
    let head = std::str::from_utf8(&raw[..end]).ok()?;
    // Unfolded header spans: (start, end) byte offsets of each header.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut pos = 0;
    for line in head.split_inclusive('\n') {
        let folded = line.starts_with(' ') || line.starts_with('\t');
        match spans.last_mut() {
            Some(last) if folded => last.1 = pos + line.len(),
            _ => spans.push((pos, pos + line.len())),
        }
        pos += line.len();
    }
    let froms: Vec<&(usize, usize)> = spans
        .iter()
        .filter(|(a, _)| {
            head[*a..]
                .get(..5)
                .is_some_and(|n| n.eq_ignore_ascii_case("from:"))
        })
        .collect();
    let [span] = froms.as_slice() else {
        return None;
    };
    let (start, stop) = **span;
    let header = &head[start..stop];
    let value = header[5..].trim_end_matches(['\r', '\n']);
    let eol = &header[5 + value.len()..];
    let current = match (value.rfind('<'), value.rfind('>')) {
        (Some(l), Some(r)) if l < r => value[l + 1..r].trim().to_string(),
        _ => value.trim().to_string(),
    };
    if current.eq_ignore_ascii_case(addr) {
        return None;
    }
    let new_value = match (value.rfind('<'), value.rfind('>')) {
        (Some(l), Some(r)) if l < r => format!("{}<{addr}>{}", &value[..l], &value[r + 1..]),
        _ => format!(" {addr}"),
    };
    let mut out = Vec::with_capacity(raw.len() + addr.len());
    out.extend_from_slice(&raw[..start]);
    out.extend_from_slice(b"From:");
    out.extend_from_slice(new_value.as_bytes());
    out.extend_from_slice(eol.as_bytes());
    out.extend_from_slice(&raw[stop..]);
    Some(out)
}

/// Executes Outlook write ops with a WRITE-bound credential store.
pub struct GraphWriteClient {
    creds: std::sync::Arc<dyn CredentialStore>,
    account_id: AccountId,
    http: reqwest::Client,
    /// API base. Graph's own in production; tests point it at a mock.
    base: String,
    /// Ask Graph which address the account sends as before each send, and
    /// put it in the From ([`rewrite_from`]). On in production; the mock
    /// tests turn it on only where they script the `/me` answer.
    resolve_send_as: bool,
}

impl GraphWriteClient {
    /// Over a client of its own, for tests.
    pub fn with_base(
        creds: std::sync::Arc<dyn CredentialStore>,
        account_id: AccountId,
        base: String,
    ) -> Self {
        let mut c = Self::with_http(
            creds,
            account_id,
            base,
            super::gmail::GmailWriteClient::build_http(),
        );
        c.resolve_send_as = false;
        c
    }

    /// Turn the send-as lookup on (tests that script `/me`).
    pub fn resolving_send_as(mut self) -> Self {
        self.resolve_send_as = true;
        self
    }

    /// Over the door's shared client (redirects refused; see
    /// `GmailWriteClient::build_http`).
    pub fn with_http(
        creds: std::sync::Arc<dyn CredentialStore>,
        account_id: AccountId,
        base: String,
        http: reqwest::Client,
    ) -> Self {
        Self {
            creds,
            account_id,
            http,
            base,
            resolve_send_as: true,
        }
    }

    /// The address Exchange sends this account's mail as: `/me`'s `mail`,
    /// else an address-shaped `userPrincipalName`. `None` on any failure, and
    /// the message then goes as composed.
    async fn send_as(&self) -> Option<String> {
        #[derive(serde::Deserialize)]
        struct Me {
            mail: Option<String>,
            #[serde(rename = "userPrincipalName")]
            upn: Option<String>,
        }
        let me: Me = self
            .call(
                self.http
                    .get(format!("{}/me?$select=mail,userPrincipalName", self.base))
                    .timeout(std::time::Duration::from_secs(15)),
            )
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        me.mail
            .filter(|m| m.contains('@'))
            .or(me.upn.filter(|u| u.contains('@')))
            .map(|a| a.trim().to_string())
    }

    fn msg_url(&self, id: &str) -> String {
        format!("{}/me/messages/{}", self.base, enc(id))
    }

    async fn write_token(&self) -> Result<String, WriteError> {
        self.creds
            .token(self.account_id)
            .await
            .map(|t| t.access_token)
            .map_err(|e| {
                WriteError::MissingCredential(format!("no write credential available: {e}"))
            })
    }

    /// Send one request with the write bearer and the immutable-id
    /// preference, and turn a non-success into an error that carries no
    /// upstream text. A `429` is [`WriteError::Paced`]: Graph did nothing.
    async fn call(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response, WriteError> {
        let token = self.write_token().await?;
        let resp = req
            .bearer_auth(&token)
            .header("Prefer", PREFER_IMMUTABLE_IDS)
            .send()
            .await
            .map_err(|e| {
                WriteError::Transport(format!(
                    "graph request failed ({})",
                    if e.is_timeout() {
                        "timeout"
                    } else {
                        "transport"
                    }
                ))
            })?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_in_secs = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(30)
                .max(1);
            return Err(WriteError::Paced { retry_in_secs });
        }
        Err(WriteError::Api {
            status: status.as_u16(),
            message: "request rejected".into(),
        })
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, WriteError> {
        self.call(self.http.get(url))
            .await?
            .json::<T>()
            .await
            .map_err(|_| WriteError::Transport("graph json decode failed".into()))
    }

    async fn post_json(&self, url: &str, body: &Value) -> Result<reqwest::Response, WriteError> {
        self.call(self.http.post(url).json(body)).await
    }

    async fn patch_json(&self, url: &str, body: &Value) -> Result<(), WriteError> {
        self.call(self.http.patch(url).json(body)).await.map(|_| ())
    }

    /// Move a message to a well-known folder.
    async fn move_to(&self, id: &str, folder: &str) -> Result<(), WriteError> {
        self.post_json(
            &format!("{}/move", self.msg_url(id)),
            &json!({ "destinationId": folder }),
        )
        .await
        .map(|_| ())
    }

    /// Whether message `id` is the fired send: its [`OUTBOX_HEADER`] is
    /// `token`, or its `internetMessageId` is the minted one.
    fn carries_outbox(m: &MessageMeta, token: &str, bare_mid: &str) -> bool {
        m.internet_message_id
            .as_deref()
            .is_some_and(|mid| bare_message_id(mid) == bare_mid)
            || m.internet_message_headers
                .iter()
                .any(|h| h.name.eq_ignore_ascii_case(OUTBOX_HEADER) && h.value.trim() == token)
    }
}

#[async_trait::async_trait]
impl MailWriter for GraphWriteClient {
    /// The Gmail labels Outlook has a meaning for, applied in one PATCH (plus
    /// a move for an archive); anything else refuses the whole request before
    /// a call is made, so a mixed request never half-applies.
    async fn modify(
        &self,
        msg_id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<(), WriteError> {
        if add.iter().any(|l| remove.contains(l)) {
            return Err(WriteError::Invalid(
                "a label cannot be added and removed in one request".into(),
            ));
        }
        let mut patch = serde_json::Map::new();
        let mut archive = false;
        for (label, adding) in add
            .iter()
            .map(|l| (l, true))
            .chain(remove.iter().map(|l| (l, false)))
        {
            match (label.as_str(), adding) {
                ("UNREAD", adding) => {
                    patch.insert("isRead".into(), json!(!adding));
                }
                ("STARRED", adding) => {
                    patch.insert(
                        "flag".into(),
                        json!({ "flagStatus": if adding { "flagged" } else { "notFlagged" } }),
                    );
                }
                ("INBOX", false) => archive = true,
                _ => return Err(WriteError::Invalid(LABELS_REFUSED.into())),
            }
        }
        if !patch.is_empty() {
            self.patch_json(&self.msg_url(msg_id), &Value::Object(patch))
                .await?;
        }
        if archive {
            self.archive(msg_id).await?;
        }
        Ok(())
    }

    async fn archive(&self, msg_id: &str) -> Result<(), WriteError> {
        self.move_to(msg_id, "archive").await
    }

    async fn not_spam(&self, msg_id: &str) -> Result<(), WriteError> {
        self.move_to(msg_id, "inbox").await
    }

    /// To Deleted Items: recoverable, never a permanent delete.
    async fn trash(&self, msg_id: &str) -> Result<(), WriteError> {
        self.move_to(msg_id, "deleteditems").await
    }

    /// The threading headers from `internetMessageId` and the transport
    /// headers (`References`), the audience from the structured fields.
    async fn parent_headers(&self, msg_id: &str) -> Result<ParentHeaders, WriteError> {
        let url = format!(
            "{}?$select=internetMessageId,internetMessageHeaders,from,replyTo,toRecipients,ccRecipients",
            self.msg_url(msg_id)
        );
        let m: MessageMeta = self.get_json(&url).await?;
        // Unfolded and single-spaced: a folded value carries CRLF, which the
        // reply builder refuses outright, and every reply in a long thread
        // would fail.
        let references = m
            .internet_message_headers
            .iter()
            .filter(|h| h.name.eq_ignore_ascii_case("references"))
            .flat_map(|h| h.value.split_whitespace().map(str::to_string))
            .reduce(|a, b| format!("{a} {b}"));
        Ok(ParentHeaders {
            message_id: m.internet_message_id.clone(),
            references,
            reply_to: render_list(&m.reply_to),
            from: m.from.as_ref().and_then(render),
            to: render_list(&m.to_recipients),
            cc: render_list(&m.cc_recipients),
        })
    }

    /// Draft, then send (see the module doc). `thread_id` is not needed: the
    /// MIME's `In-Reply-To` and `References` thread the reply in Exchange.
    async fn send(&self, raw: &[u8], _thread_id: Option<&str>) -> Result<SentRef, WriteError> {
        if raw.is_empty() {
            return Err(WriteError::Invalid("empty message".into()));
        }
        // SEND AS WHAT EXCHANGE WILL USE. A From that is not the account's
        // primary address (an alias, or the configured address before an
        // alias change has propagated) is not refused: Exchange silently
        // replaces it with the mailbox's hidden `outlook_<hex>@outlook.com`
        // address. Seen live 2026-10-28. Writing the real one keeps the From
        // the reader sees and the one the recipient gets the same.
        let rewritten = if self.resolve_send_as {
            match self.send_as().await {
                Some(addr) => rewrite_from(raw, &addr),
                None => None,
            }
        } else {
            None
        };
        let raw: &[u8] = rewritten.as_deref().unwrap_or(raw);
        let body = base64::engine::general_purpose::STANDARD.encode(raw);
        if body.len() > GRAPH_MAX_REQUEST {
            return Err(WriteError::Invalid(format!(
                "this message is {} MB; Outlook sends over about 3 MB are not supported yet",
                raw.len() / (1024 * 1024)
            )));
        }
        let draft = self
            .call(
                self.http
                    .post(format!("{}/me/messages", self.base))
                    .header(reqwest::header::CONTENT_TYPE, "text/plain")
                    .timeout(send_budget(raw.len()))
                    .body(body),
            )
            .await?
            .json::<MessageMeta>()
            .await
            .map_err(|_| WriteError::Transport("graph draft response unreadable".into()))?;
        if draft.id.is_empty() {
            return Err(WriteError::Transport(
                "graph created a draft without an id; nothing was sent".into(),
            ));
        }
        let sent = self
            .post_json(&format!("{}/send", self.msg_url(&draft.id)), &json!({}))
            .await
            // A 408 says the server gave up waiting, not that it sent
            // nothing: it is a "maybe sent" to every caller (the door and the
            // scheduled send decide that from a Transport error), never a
            // plain refusal that invites a second send.
            .map_err(|e| match e {
                WriteError::Api { status: 408, .. } => {
                    WriteError::Transport("graph send timed out (408); it may have gone".into())
                }
                other => other,
            });
        match sent {
            Ok(_) => Ok(SentRef {
                id: Some(draft.id),
                thread_id: draft.conversation_id,
            }),
            Err(e) => {
                // Refused outright (a 4xx other than a timeout, or pacing):
                // nothing went out, so the draft is ours to clean up. A 5xx,
                // a 408 or a dropped connection is not proof either way: the
                // mail may have gone, and with immutable ids the draft's id
                // now names the Sent Items copy. The draft stays.
                let refused = match &e {
                    WriteError::Api { status, .. } => (400..500).contains(status) && *status != 408,
                    WriteError::Paced { .. } => true,
                    _ => false,
                };
                if refused {
                    let _ = self.call(self.http.delete(self.msg_url(&draft.id))).await;
                }
                Err(e)
            }
        }
    }

    /// `$value` (raw MIME bytes), re-encoded base64url for [`FetchedRaw`],
    /// with the thread and received date from the message's metadata.
    async fn fetch_raw(&self, msg_id: &str) -> Result<FetchedRaw, WriteError> {
        let meta: MessageMeta = self
            .get_json(&format!(
                "{}?$select=id,conversationId,receivedDateTime",
                self.msg_url(msg_id)
            ))
            .await?;
        let mut resp = self
            .call(self.http.get(format!("{}/$value", self.msg_url(msg_id))))
            .await?;
        // The read side's bound, for the same reason: peak memory is ours.
        const MAX_RAW: usize = 64 * 1024 * 1024;
        let too_big = || WriteError::Invalid("message exceeds the 64MB bound".into());
        if resp.content_length().is_some_and(|n| n > MAX_RAW as u64) {
            return Err(too_big());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|_| WriteError::Transport("graph body read failed".into()))?
        {
            if bytes.len() + chunk.len() > MAX_RAW {
                return Err(too_big());
            }
            bytes.extend_from_slice(&chunk);
        }
        let internal_date = meta
            .received_date_time
            .as_deref()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.timestamp_millis().to_string());
        Ok(FetchedRaw {
            raw_b64: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes),
            thread_id: meta.conversation_id,
            internal_date,
        })
    }

    async fn find_by_rfc822_msgid(&self, msgid: &str) -> Result<Option<String>, WriteError> {
        // Never a draft: a send whose outcome was unknown leaves its draft,
        // carrying this Message-ID, and finding THAT would record as sent
        // mail that may never have gone.
        let filter = format!(
            "internetMessageId eq {} and isDraft eq false",
            odata_string(msgid.trim())
        );
        let url = format!(
            "{}/me/messages?$select=id&$top=1&$filter={}",
            self.base,
            enc(&filter)
        );
        let page: Page = self.get_json(&url).await?;
        Ok(page
            .value
            .into_iter()
            .map(|m| m.id)
            .find(|id| !id.is_empty()))
    }

    /// The scheduled send's recovery question, as the Gmail writer asks it: a
    /// `Message-ID` lookup, then a bounded scan of Sent Items inside `window`
    /// reading each candidate's outbox header. Any doubt is `Err`.
    async fn find_scheduled_send(
        &self,
        message_id: &str,
        window: (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>),
        stopping: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<Option<String>, WriteError> {
        let token = outbox_token(message_id);
        if token.is_empty() {
            return Err(WriteError::Invalid("no outbox token to search for".into()));
        }
        if let Some(id) = self.find_by_rfc822_msgid(message_id).await? {
            return Ok(Some(id));
        }
        let bare_mid = bare_message_id(message_id);
        let fmt = |t: chrono::DateTime<chrono::Utc>| t.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let filter = format!(
            "sentDateTime ge {} and sentDateTime lt {}",
            fmt(window.0),
            fmt(window.1.max(window.0 + chrono::Duration::seconds(1)))
        );
        let mut url = format!(
            "{}/me/mailFolders/sentitems/messages?$select=id,internetMessageId,internetMessageHeaders&$top={SCAN_PAGE}&$filter={}",
            self.base,
            enc(&filter)
        );
        let cut_short = || WriteError::Transport("sent scan cut short by shutdown".into());
        for _ in 0..SCAN_MAX_PAGES {
            if stopping() {
                return Err(cut_short());
            }
            let page: Page = self.get_json(&url).await?;
            if let Some(hit) = page
                .value
                .iter()
                .find(|m| Self::carries_outbox(m, &token, bare_mid))
            {
                return Ok(Some(hit.id.clone()));
            }
            match page.next_link {
                // A link off our own base is never followed with the token.
                Some(next) if next.starts_with(&format!("{}/", self.base)) => url = next,
                Some(_) => {
                    return Err(WriteError::Transport(
                        "sent scan got a next link off the Graph base".into(),
                    ));
                }
                None => return Ok(None),
            }
        }
        Err(WriteError::Transport(
            "sent scan reached its page cap without ruling the mail out".into(),
        ))
    }
}

#[cfg(test)]
mod tests;
