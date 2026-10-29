//! Gmail WRITE operations — the ONLY write capability in squelch. Lives here,
//! not in squelch-core, so the sync/triage core stays provably write-free.
//! The write token is fetched PER REQUEST from a [`CredentialStore`] bound to
//! [`CredentialKind::Write`], dropped when the call returns, and never reachable
//! from a read path. Tokens, auth headers and bodies are NEVER logged.

use base64::Engine as _;
use serde_json::{Value, json};

use squelch_core::credentials::CredentialStore;
// The Gmail endpoint, the INBOX label and the Gmail-API response shapes are
// defined once in core sync; the write path reuses them rather than re-deriving
// its own copy. Archiving == removing `LABEL_INBOX`.
use squelch_core::sync::{GMAIL_API_BASE, GmailMessage, LABEL_INBOX, LABEL_SPAM};
use squelch_core::types::AccountId;

use super::mime::{OUTBOX_HEADER, bare_message_id, outbox_token};
use super::{FetchedRaw, MailWriter, ParentHeaders, SentRef, WriteError, send_budget};

/// The modify JSON body. Empty arrays are still sent (harmless no-ops) so the
/// shape is uniform.
pub fn modify_body(add: &[String], remove: &[String]) -> Value {
    json!({
        "addLabelIds": add,
        "removeLabelIds": remove,
    })
}

/// The `(path, json_body)` for a `messages.modify` call, relative to the API
/// base. `add`/`remove` are label ids.
pub fn modify_request(gmail_msg_id: &str, add: &[String], remove: &[String]) -> (String, Value) {
    let url = format!("{GMAIL_API_BASE}/messages/{gmail_msg_id}/modify");
    (url, modify_body(add, remove))
}

/// The archive request: remove `INBOX`, add nothing.
pub fn archive_request(gmail_msg_id: &str) -> (String, Value) {
    modify_request(gmail_msg_id, &[], &[LABEL_INBOX.to_string()])
}

/// The `messages.send` JSON body: `raw` base64url-encoded WITHOUT padding as
/// Gmail expects, `threadId` set only when threading a reply.
pub fn send_body(raw: &[u8], thread_id: Option<&str>) -> Value {
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
    let mut body = json!({ "raw": encoded });
    if let Some(tid) = thread_id {
        body["threadId"] = Value::String(tid.to_string());
    }
    body
}

/// The `(url, json_body)` for a `messages.send` call.
pub fn send_request_body(raw: &[u8], thread_id: Option<&str>) -> (String, Value) {
    let url = format!("{GMAIL_API_BASE}/messages/send");
    (url, send_body(raw, thread_id))
}

/// The longest a write-door request waits for the mailbox's Gmail budget.
/// Comfortably inside the app's 15 s idle timeout.
const WRITE_MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Executes Gmail write ops with a WRITE-bound credential store. The token is
/// fetched per call and never retained.
pub struct GmailWriteClient {
    creds: std::sync::Arc<dyn CredentialStore>,
    account_id: AccountId,
    http: reqwest::Client,
    /// API base URL. Overridable in tests only; production uses Gmail's.
    base: String,
    /// The mailbox's shared Gmail quota budget (see
    /// [`squelch_core::gmail_budget`]). `None` only where no daemon built one:
    /// tests, and a door with no sync engine beside it.
    budget: Option<std::sync::Arc<squelch_core::gmail_budget::GmailBudget>>,
}

impl GmailWriteClient {
    /// Construct with an explicit API base over a client of its own: the
    /// shape the tests use. Production goes through [`Self::with_http`] with
    /// the door's shared client (`ApiState::write_http`) and the real base.
    pub fn with_base(
        creds: std::sync::Arc<dyn CredentialStore>,
        account_id: AccountId,
        base: String,
    ) -> Self {
        Self::with_http(creds, account_id, base, Self::build_http())
    }

    /// Construct over an HTTP client the caller owns. The door builds ONE
    /// (`ApiState::write_http`) and hands it to every action, because a
    /// `reqwest::Client` is a rustls config and a connection pool, and
    /// building one per action meant every send, archive and label paid a
    /// fresh TCP + TLS handshake to Gmail and threw the warm connection of the
    /// action before it away. The client is an `Arc` underneath, so cloning
    /// it here is the cheap part.
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
            budget: None,
        }
    }

    /// The HTTP client a write client wants, now that one lives for the
    /// daemon's lifetime instead of one request:
    ///
    /// - a 60 s request ceiling and a 15 s connect ceiling, as before;
    /// - redirects REFUSED, as the sync engine's Gmail client refuses them
    ///   (`squelch_core::sync`): every request here carries the write bearer,
    ///   and Gmail never redirects, so a redirect is only ever somewhere the
    ///   token must not go;
    /// - a 30 s idle timeout on pooled connections and TCP keepalive at the
    ///   same interval. A per-action client dialled fresh every time; a shared
    ///   one would otherwise hand a user's one-shot send the connection a
    ///   laptop's sleep left half-open an hour ago. Idle connections are now
    ///   dropped before a NAT forgets them, and a peer that died is noticed by
    ///   the keepalive rather than by the user's 60 s wait.
    pub(crate) fn build_http() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .connect_timeout(std::time::Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .pool_idle_timeout(std::time::Duration::from_secs(30))
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .build()
            .expect("reqwest client build")
    }

    /// Book every request against the mailbox's shared Gmail budget, the one
    /// the sync engines draw from, so a send or an archive during a catch-up
    /// cannot push the user over Gmail's per-minute limit.
    pub fn with_budget(
        mut self,
        budget: Option<std::sync::Arc<squelch_core::gmail_budget::GmailBudget>>,
    ) -> Self {
        self.budget = budget;
        self
    }

    /// Wait for the mailbox's Gmail budget, but never longer than
    /// [`WRITE_MAX_WAIT`]: the app gives a request 15 s of silence before it
    /// calls it failed, and a send that waits out a one-minute refusal hold
    /// would be reported failed and then, possibly, go out anyway. Better to
    /// send nothing and say when to try again.
    async fn admit(&self, method: &str, url: &str) -> Result<(), WriteError> {
        let Some(budget) = &self.budget else {
            return Ok(());
        };
        budget
            .admit_within(
                method,
                squelch_core::gmail_budget::gmail_path(url, &self.base),
                WRITE_MAX_WAIT,
            )
            .await
            .map_err(|wait| WriteError::Paced {
                retry_in_secs: wait.as_secs().max(1),
            })
    }

    /// Tell the shared budget how Google answered, so a refusal on a send (the
    /// most expensive call) slows the sync loop too, and returns the
    /// non-success as an error without echoing Google's body.
    async fn settle(&self, resp: reqwest::Response) -> Result<reqwest::Response, WriteError> {
        let status = resp.status();
        if status.is_success() {
            if let Some(budget) = &self.budget {
                budget.note_success();
            }
            return Ok(resp);
        }
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(std::time::Duration::from_secs);
        let body = resp.text().await.unwrap_or_default();
        if let Some(budget) = &self.budget
            && squelch_core::gmail_budget::is_quota_refusal(status.as_u16(), &body)
        {
            budget.note_quota_refusal(retry_after);
        }
        // Never echo the upstream body (it may carry request context).
        Err(WriteError::Api {
            status: status.as_u16(),
            message: "request rejected".into(),
        })
    }

    /// Fetch a fresh write access token; an absent/failed credential is
    /// `MissingCredential`, which the handler maps to a 403.
    async fn write_token(&self) -> Result<String, WriteError> {
        match self.creds.token(self.account_id).await {
            Ok(t) => Ok(t.access_token),
            Err(e) => Err(WriteError::MissingCredential(format!(
                "no write credential available: {e}"
            ))),
        }
    }

    /// POST a JSON body with the write bearer token. Never logs the token or
    /// the body.
    async fn post_json(&self, url: &str, body: &Value) -> Result<Value, WriteError> {
        self.post_json_within(url, body, None).await
    }

    /// [`Self::post_json`] with its own budget, for a request whose body is
    /// megabytes rather than a few hundred bytes: the client's flat 60 s is
    /// sized for a reply, and a 30 MB message on a slow uplink would time out
    /// HERE while Gmail was still receiving it — and then be sent again.
    async fn post_json_within(
        &self,
        url: &str,
        body: &Value,
        timeout: Option<std::time::Duration>,
    ) -> Result<Value, WriteError> {
        let token = self.write_token().await?;
        self.admit("POST", url).await?;
        let mut req = self.http.post(url).bearer_auth(&token).json(body);
        if let Some(timeout) = timeout {
            req = req.timeout(timeout);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| WriteError::Transport(format!("gmail request failed: {e}")))?;
        self.settle(resp)
            .await?
            .json::<Value>()
            .await
            .map_err(|e| WriteError::Transport(format!("gmail json decode: {e}")))
    }

    /// GET a JSON body with the write bearer token (gmail.modify grants read).
    /// Never logs the token or the response.
    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, WriteError> {
        let token = self.write_token().await?;
        self.admit("GET", url).await?;
        let resp = self
            .http
            .get(url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| WriteError::Transport(format!("gmail request failed: {e}")))?;
        self.settle(resp)
            .await?
            .json::<T>()
            .await
            .map_err(|e| WriteError::Transport(format!("gmail json decode: {e}")))
    }
}

#[async_trait::async_trait]
impl MailWriter for GmailWriteClient {
    /// `messages.modify`: add/remove labels on a Gmail message.
    async fn modify(
        &self,
        gmail_msg_id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<(), WriteError> {
        let url = format!("{}/messages/{gmail_msg_id}/modify", self.base);
        let body = modify_body(add, remove);
        self.post_json(&url, &body).await.map(|_| ())
    }

    /// Archive: remove the INBOX label.
    async fn archive(&self, gmail_msg_id: &str) -> Result<(), WriteError> {
        self.modify(gmail_msg_id, &[], &[LABEL_INBOX.to_string()])
            .await
    }

    /// Not spam: add INBOX back and remove SPAM, in one modify.
    async fn not_spam(&self, gmail_msg_id: &str) -> Result<(), WriteError> {
        self.modify(
            gmail_msg_id,
            &[LABEL_INBOX.to_string()],
            &[LABEL_SPAM.to_string()],
        )
        .await
    }

    /// Move a message to Gmail's Trash — `/trash`, NEVER `messages.delete`.
    /// Trashed mail is recoverable for 30 days, and permanent delete requires
    /// the full `https://mail.google.com/` scope, which squelch never requests:
    /// the blast radius is capped by the OAuth scope, not by our restraint.
    async fn trash(&self, gmail_msg_id: &str) -> Result<(), WriteError> {
        let url = format!("{}/messages/{gmail_msg_id}/trash", self.base);
        self.post_json(&url, &serde_json::json!({}))
            .await
            .map(|_| ())
    }

    /// Read the parent's threading AND recipient headers with the WRITE token
    /// (gmail.modify grants read too). Metadata only — it never fetches a body.
    ///
    /// ONE fetch serves both uses: a reply-all send threads and derives its
    /// audience from the same response, so asking for the address headers costs
    /// no extra round trip even when nobody wants them.
    async fn parent_headers(&self, gmail_msg_id: &str) -> Result<ParentHeaders, WriteError> {
        let url = format!(
            "{}/messages/{gmail_msg_id}\
             ?format=metadata&metadataHeaders=Message-ID&metadataHeaders=References\
             &metadataHeaders=Reply-To&metadataHeaders=From&metadataHeaders=To&metadataHeaders=Cc",
            self.base
        );
        let meta: GmailMessage = self.get_json(&url).await?;
        let mut out = ParentHeaders::default();
        // A message may legitimately carry more than one Cc (or To) header; RFC
        // 5322 says a parser should treat them as one list, so append rather
        // than let the last one win and silently shrink the audience.
        fn append(slot: &mut Option<String>, value: String) {
            match slot {
                Some(existing) => {
                    existing.push_str(", ");
                    existing.push_str(&value);
                }
                None => *slot = Some(value),
            }
        }
        if let Some(p) = meta.payload {
            for h in p.headers {
                if h.name.eq_ignore_ascii_case("message-id") {
                    out.message_id = Some(h.value);
                } else if h.name.eq_ignore_ascii_case("references") {
                    out.references = Some(h.value);
                } else if h.name.eq_ignore_ascii_case("reply-to") {
                    append(&mut out.reply_to, h.value);
                } else if h.name.eq_ignore_ascii_case("from") {
                    append(&mut out.from, h.value);
                } else if h.name.eq_ignore_ascii_case("to") {
                    append(&mut out.to, h.value);
                } else if h.name.eq_ignore_ascii_case("cc") {
                    append(&mut out.cc, h.value);
                }
            }
        }
        Ok(out)
    }

    /// `messages.send`: send a raw RFC822 message, optionally threaded. Returns
    /// what Gmail echoed about the created message (see [`SentRef`]).
    async fn send(&self, raw: &[u8], thread_id: Option<&str>) -> Result<SentRef, WriteError> {
        let url = format!("{}/messages/send", self.base);
        let body = send_body(raw, thread_id);
        let v = self
            .post_json_within(&url, &body, Some(send_budget(raw.len())))
            .await?;
        Ok(SentRef {
            id: v["id"].as_str().map(str::to_string),
            thread_id: v["threadId"].as_str().map(str::to_string),
            warning: None,
            raw: None,
        })
    }

    /// Read a just-sent message with the WRITE token, to echo it into the local
    /// store: the read credential does not see Sent mail between polls.
    async fn fetch_raw(&self, gmail_msg_id: &str) -> Result<FetchedRaw, WriteError> {
        let url = format!("{}/messages/{gmail_msg_id}?format=raw", self.base);
        let msg: GmailMessage = self.get_json(&url).await?;
        Ok(FetchedRaw {
            raw_b64: msg.raw.unwrap_or_default(),
            thread_id: msg.thread_id,
            internal_date: msg.internal_date,
        })
    }

    /// The Gmail id of the message whose `Message-ID` header is `msgid`, or
    /// `None` when Gmail's search finds none. On the WRITE token, like every
    /// other call here (gmail.modify grants read).
    ///
    /// A HIT IS PROOF, A MISS IS NOT: a daemon-minted id was not findable this
    /// way minutes after its mail went out (2026-09-29, see
    /// [`Self::find_scheduled_send`], the question recovery actually asks).
    /// The search includes Spam and Trash; an error is an error, never "not
    /// found".
    async fn find_by_rfc822_msgid(&self, msgid: &str) -> Result<Option<String>, WriteError> {
        let url = rfc822msgid_search_url(&self.base, msgid);
        let list: MessageIdList = self.get_json(&url).await?;
        Ok(list.messages.into_iter().next().map(|m| m.id))
    }

    /// The Gmail id of the fired scheduled send whose `Message-ID` was minted
    /// as `message_id`, or `None` when the mailbox holds no such mail.
    ///
    /// THIS IS THE SCHEDULED SEND'S RECOVERY QUESTION (docs/SCHEDULED-SEND.md
    /// §4): a row left `sending` by a crash, or `maybe_sent` by an ambiguous
    /// failure, either went out or did not. A wrong `None` is a double send.
    ///
    /// Two looks, cheapest first:
    ///
    /// 1. `rfc822msgid:` for the minted id. A hit returns at once.
    /// 2. The Sent mail inside `window` (`after`, `before`), newest first,
    ///    [`SCAN_PAGE`] a page for at most [`SCAN_MAX_PAGES`] pages, each
    ///    candidate's [`OUTBOX_HEADER`] and `Message-ID` read by a metadata
    ///    get. The caller sizes the window to the attempt being asked about
    ///    (slack included, see `scan_window` in scheduled_send.rs): BOTH ends
    ///    matter, since a window open at the top covers every mail sent since,
    ///    and a busy mailbox would hit the page cap on every ask of an old row
    ///    and never let it send. The header is what the live finding demands: on 2026-09-29 a
    ///    minted id the daemon had just sent (and echoed) was NOT found by
    ///    step 1 at 9 and 28 minutes, so Gmail may rewrite a client-supplied
    ///    Message-ID; a custom header it has no reason to touch.
    ///
    /// Every request goes through the same budget and timeout as the rest of
    /// this client. Any error is `Err` ("could not ask"), never `None`. A scan
    /// that runs out of pages with more still listed is ALSO `Err`: it could
    /// not rule the mail out, and reading that as "not sent" is the double
    /// send this exists to prevent. So is a scan cut short by `stopping`
    /// (the daemon's shutdown), which is checked before every request after
    /// the first: a scan is up to 1 + 4 + 100 of them, too many for a
    /// shutdown to wait out.
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
        let (after, before) = window;
        let floor = after.timestamp().max(0);
        let ceiling = before.timestamp().max(floor + 1);
        let cut_short = || WriteError::Transport("sent scan cut short by shutdown".into());
        let mut page_token: Option<String> = None;
        for _ in 0..SCAN_MAX_PAGES {
            if stopping() {
                return Err(cut_short());
            }
            let url = sent_scan_url(&self.base, floor, ceiling, page_token.as_deref());
            let list: MessageIdList = self.get_json(&url).await?;
            for candidate in list.messages {
                if stopping() {
                    return Err(cut_short());
                }
                if self.carries_outbox(&candidate.id, &token, bare_mid).await? {
                    return Ok(Some(candidate.id));
                }
            }
            match list.next_page_token.filter(|t| !t.is_empty()) {
                Some(next) => page_token = Some(next),
                None => return Ok(None),
            }
        }
        Err(WriteError::Transport(
            "sent scan reached its page cap without ruling the mail out".into(),
        ))
    }
}

impl GmailWriteClient {
    /// Whether Gmail message `gmail_id` is the fired send: its
    /// [`OUTBOX_HEADER`] is `token`, or its `Message-ID` is the minted one
    /// (free, it rides the same metadata get). Exact after trimming.
    async fn carries_outbox(
        &self,
        gmail_id: &str,
        token: &str,
        bare_mid: &str,
    ) -> Result<bool, WriteError> {
        let url = format!(
            "{}/messages/{}?format=metadata&metadataHeaders={OUTBOX_HEADER}\
             &metadataHeaders=Message-ID",
            self.base,
            percent_encode(gmail_id)
        );
        let meta: GmailMessage = self.get_json(&url).await?;
        Ok(meta.payload.is_some_and(|p| {
            p.headers.iter().any(|h| {
                (h.name.eq_ignore_ascii_case(OUTBOX_HEADER) && h.value.trim() == token)
                    || (h.name.eq_ignore_ascii_case("message-id")
                        && bare_message_id(&h.value) == bare_mid)
            })
        }))
    }
}

/// How far past the attempt, either side, the Sent scan reaches: clock skew
/// between this host and Gmail's `internalDate`, with room to spare.
pub const SCAN_SLACK: chrono::Duration = chrono::Duration::hours(1);
/// Candidates per page of the Sent scan.
pub const SCAN_PAGE: u32 = 25;
/// Pages the Sent scan reads before it gives up on ruling a mail out: at most
/// `SCAN_PAGE * SCAN_MAX_PAGES` metadata gets per question.
pub const SCAN_MAX_PAGES: usize = 4;

/// `messages.list` over the Sent mail after `floor` and before `ceiling`
/// (epoch seconds, which Gmail's `after:` and `before:` take as instants
/// rather than local dates), Spam and Trash included for the same reason as
/// [`rfc822msgid_search_url`].
fn sent_scan_url(base: &str, floor: i64, ceiling: i64, page_token: Option<&str>) -> String {
    let q = percent_encode(&format!("in:sent after:{floor} before:{ceiling}"));
    let mut url = format!("{base}/messages?q={q}&maxResults={SCAN_PAGE}&includeSpamTrash=true");
    if let Some(t) = page_token {
        url.push_str("&pageToken=");
        url.push_str(&percent_encode(t));
    }
    url
}

/// `messages.list` narrowed to one `Message-ID`. THE BRACKETS COME OFF: the
/// header value is `<id@passband.app>`, but Gmail's `rfc822msgid:` operator
/// takes the bare `id@passband.app`, and searching with the brackets in finds NOTHING.
/// Proven live on 2026-09-29: a mail the daemon had just sent came back
/// "not found" with them and was requeued as maybe-sent. What is left still
/// carries `@` and the colon of the operator, so the whole term is
/// percent-encoded and nothing can end the value or open a parameter.
fn rfc822msgid_search_url(base: &str, msgid: &str) -> String {
    let bare = bare_message_id(msgid);
    let q = percent_encode(&format!("rfc822msgid:{bare}"));
    format!("{base}/messages?q={q}&maxResults=1&includeSpamTrash=true")
}

/// RFC 3986 percent-encoding for a query value: unreserved characters pass,
/// every other byte becomes `%XX`. Hand-rolled because it is eight lines and
/// the one query it serves is fully covered by tests.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `messages.list`'s answer, reduced to the ids. Gmail OMITS `messages`
/// entirely on no match (it does not send an empty list), hence the default.
#[derive(Debug, serde::Deserialize)]
struct MessageIdList {
    #[serde(default)]
    messages: Vec<MessageIdEntry>,
    /// Present while more pages follow.
    #[serde(default, rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct MessageIdEntry {
    id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write::mime::derive_reply_recipients;

    #[test]
    fn archive_removes_inbox_only() {
        let (url, body) = archive_request("abc123");
        assert!(url.ends_with("/messages/abc123/modify"));
        assert_eq!(body["removeLabelIds"], json!(["INBOX"]));
        assert_eq!(body["addLabelIds"], json!([] as [String; 0]));
    }

    #[test]
    fn modify_add_and_remove_shape() {
        let (url, body) = modify_request(
            "m1",
            &["Label_1".to_string(), "STARRED".to_string()],
            &["UNREAD".to_string()],
        );
        assert!(url.ends_with("/messages/m1/modify"));
        assert_eq!(body["addLabelIds"], json!(["Label_1", "STARRED"]));
        assert_eq!(body["removeLabelIds"], json!(["UNREAD"]));
    }

    #[test]
    fn send_body_is_base64url_nopad_and_threaded() {
        let raw = b"To: a@b.com\r\nSubject: hi\r\n\r\nbody";
        let (url, body) = send_request_body(raw, Some("thread-42"));
        assert!(url.ends_with("/messages/send"));
        assert_eq!(body["threadId"], "thread-42");
        let enc = body["raw"].as_str().unwrap();
        // No padding, web-safe.
        assert!(!enc.contains('='));
        assert!(!enc.contains('+') && !enc.contains('/'));
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(enc)
            .unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn send_body_without_thread_omits_threadid() {
        let (_url, body) = send_request_body(b"x", None);
        assert!(body.get("threadId").is_none());
    }

    // ---- network executor against a one-shot mock server ------------------

    use async_trait::async_trait;
    use squelch_core::credentials::OAuthToken;
    use squelch_core::error::Result as CoreResult;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A credential store that hands out a fixed write token (no keyring/file).
    struct StubCreds;
    #[async_trait]
    impl CredentialStore for StubCreds {
        async fn token(&self, _account: AccountId) -> CoreResult<OAuthToken> {
            Ok(OAuthToken {
                access_token: "WRITE-TOKEN".into(),
                refresh_token: None,
                expires_at: None,
            })
        }
    }

    /// Spawn a one-shot HTTP/1.1 server that captures the first request's raw
    /// bytes and replies with `status`/`resp_body`. Returns (base_url, join).
    async fn mock_once(
        status: u16,
        resp_body: &'static str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                resp_body.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            sock.flush().await.unwrap();
            req
        });
        (format!("http://{addr}"), handle)
    }

    fn client(base: String) -> GmailWriteClient {
        GmailWriteClient::with_base(Arc::new(StubCreds), 1_i64, base)
    }

    #[tokio::test]
    async fn archive_sends_modify_removing_inbox() {
        let (base, handle) = mock_once(200, "{}").await;
        let c = client(base);
        c.archive("gmail-123").await.unwrap();
        let req = handle.await.unwrap();
        assert!(req.starts_with("POST "), "must be a POST");
        assert!(req.contains("/messages/gmail-123/modify"), "modify path");
        assert!(
            req.contains("authorization: Bearer WRITE-TOKEN")
                || req.contains("Authorization: Bearer WRITE-TOKEN")
        );
        assert!(req.contains("\"removeLabelIds\":[\"INBOX\"]"));
        assert!(req.contains("\"addLabelIds\":[]"));
    }

    /// Not spam is the one modify the door used to spell out itself: INBOX
    /// added, SPAM removed, on the same endpoint.
    #[tokio::test]
    async fn not_spam_sends_modify_adding_inbox_and_removing_spam() {
        let (base, handle) = mock_once(200, "{}").await;
        let c = client(base);
        c.not_spam("gmail-9").await.unwrap();
        let req = handle.await.unwrap();
        assert!(req.starts_with("POST "), "must be a POST");
        assert!(req.contains("/messages/gmail-9/modify"), "modify path");
        assert!(req.contains("\"addLabelIds\":[\"INBOX\"]"));
        assert!(req.contains("\"removeLabelIds\":[\"SPAM\"]"));
    }

    #[tokio::test]
    async fn send_posts_raw_and_threadid_and_returns_ids() {
        let (base, handle) = mock_once(200, "{\"id\":\"sent-1\",\"threadId\":\"thread-9\"}").await;
        let c = client(base);
        let raw = b"To: a@b.com\r\nSubject: hi\r\n\r\nbody";
        let sent = c.send(raw, Some("thread-9")).await.unwrap();
        assert_eq!(sent.id.as_deref(), Some("sent-1"));
        assert_eq!(sent.thread_id.as_deref(), Some("thread-9"));
        let req = handle.await.unwrap();
        assert!(req.contains("/messages/send"), "send path");
        assert!(req.contains("\"threadId\":\"thread-9\""));
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        assert!(req.contains(&expected), "raw base64url payload present");
    }

    #[tokio::test]
    async fn send_tolerates_an_idless_response() {
        // The send SUCCEEDED; an echo-less body must not become an error.
        let (base, handle) = mock_once(200, "{}").await;
        let c = client(base);
        let sent = c.send(b"To: a@b.com\r\n\r\nx", None).await.unwrap();
        handle.await.unwrap();
        assert!(sent.id.is_none());
        assert!(sent.thread_id.is_none());
    }

    #[tokio::test]
    async fn fetch_raw_gets_format_raw_with_the_write_token() {
        let (base, handle) = mock_once(
            200,
            "{\"id\":\"sent-1\",\"threadId\":\"t9\",\"internalDate\":\"1783591200000\",\
             \"raw\":\"VG86IGFAYi5jb20\"}",
        )
        .await;
        let c = client(base);
        let got = c.fetch_raw("sent-1").await.unwrap();
        let req = handle.await.unwrap();
        assert!(req.starts_with("GET "), "raw fetch is a GET");
        assert!(req.contains("/messages/sent-1?format=raw"));
        assert!(
            req.contains("authorization: Bearer WRITE-TOKEN")
                || req.contains("Authorization: Bearer WRITE-TOKEN")
        );
        assert_eq!(got.raw_b64, "VG86IGFAYi5jb20");
        assert_eq!(got.thread_id.as_deref(), Some("t9"));
        assert_eq!(got.internal_date.as_deref(), Some("1783591200000"));
    }

    #[tokio::test]
    async fn parent_headers_asks_for_the_recipient_headers_and_joins_repeats() {
        let body = serde_json::json!({
            "id": "gmail-parent",
            "payload": { "headers": [
                { "name": "Message-ID", "value": "<parent@x>" },
                { "name": "From", "value": "Alice <alice@example.com>" },
                { "name": "Reply-To", "value": "list@example.com" },
                { "name": "To", "value": "me@example.com" },
                // Two Cc headers are one list, not the last one winning.
                { "name": "Cc", "value": "bob@example.com" },
                { "name": "Cc", "value": "carol@example.com" },
            ]}
        })
        .to_string();
        let (base, handle) = mock_once(200, Box::leak(body.into_boxed_str())).await;
        let c = client(base);
        let h = c.parent_headers("gmail-parent").await.unwrap();
        let req = handle.await.unwrap();
        assert!(req.starts_with("GET ") && req.contains("format=metadata"));
        for name in ["Message-ID", "References", "Reply-To", "From", "To", "Cc"] {
            assert!(
                req.contains(&format!("metadataHeaders={name}")),
                "asks for {name}"
            );
        }
        assert_eq!(h.message_id.as_deref(), Some("<parent@x>"));
        assert_eq!(h.reply_to.as_deref(), Some("list@example.com"));
        assert_eq!(h.from.as_deref(), Some("Alice <alice@example.com>"));
        assert_eq!(h.to.as_deref(), Some("me@example.com"));
        assert_eq!(h.cc.as_deref(), Some("bob@example.com, carol@example.com"));
        // ...and both Ccs survive into the derivation.
        let r = derive_reply_recipients(&h, "me@example.com", true);
        assert_eq!(r.to, "list@example.com");
        assert_eq!(r.cc, "bob@example.com, carol@example.com");
    }

    #[tokio::test]
    async fn api_error_status_is_surfaced_without_body() {
        let (base, handle) = mock_once(403, "{\"error\":\"insufficientPermissions\"}").await;
        let c = client(base);
        let err = c.archive("g1").await.unwrap_err();
        handle.await.unwrap();
        match err {
            WriteError::Api { status, message } => {
                assert_eq!(status, 403);
                // The upstream body is NOT echoed.
                assert!(!message.contains("insufficientPermissions"));
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    // ---- scheduled send: the minted Message-ID and its recovery search ------

    #[test]
    fn rfc822msgid_search_drops_the_brackets_and_encodes_the_at() {
        let url = rfc822msgid_search_url("https://g.test/me", "<a.b-c_1~@passband>");
        assert_eq!(
            url,
            "https://g.test/me/messages?q=rfc822msgid%3Aa.b-c_1~%40passband\
             &maxResults=1&includeSpamTrash=true"
        );
        // A bare id, or one with stray whitespace, searches the same.
        assert_eq!(
            rfc822msgid_search_url("https://g.test/me", " a.b-c_1~@passband "),
            url
        );
        // Nothing that could end the value or open another parameter survives.
        let q = url.split("?q=").nth(1).unwrap().split('&').next().unwrap();
        for raw in ['<', '>', '@', ':', ' ', '&', '='] {
            assert!(!q.contains(raw), "{raw:?} must be encoded");
        }
        assert_eq!(percent_encode("a b&c=d"), "a%20b%26c%3Dd");
    }

    #[tokio::test]
    async fn find_by_rfc822_msgid_returns_the_hit() {
        let (base, handle) = mock_once(
            200,
            "{\"messages\":[{\"id\":\"gm-77\",\"threadId\":\"t-1\"}],\"resultSizeEstimate\":1}",
        )
        .await;
        let got = client(base)
            .find_by_rfc822_msgid("<x@passband>")
            .await
            .unwrap();
        let req = handle.await.unwrap();
        assert_eq!(got.as_deref(), Some("gm-77"));
        assert!(req.starts_with("GET "), "a search is a GET");
        assert!(req.contains("/messages?q=rfc822msgid%3Ax%40passband&maxResults=1"));
        assert!(
            req.contains("authorization: Bearer WRITE-TOKEN")
                || req.contains("Authorization: Bearer WRITE-TOKEN")
        );
    }

    #[tokio::test]
    async fn find_by_rfc822_msgid_reads_an_absent_messages_key_as_none() {
        // Gmail's real no-match answer: no `messages` key at all.
        let (base, handle) = mock_once(200, "{\"resultSizeEstimate\":0}").await;
        let got = client(base)
            .find_by_rfc822_msgid("<x@passband>")
            .await
            .unwrap();
        handle.await.unwrap();
        assert_eq!(got, None);
    }

    #[tokio::test]
    async fn find_by_rfc822_msgid_reads_an_empty_list_as_none() {
        let (base, handle) = mock_once(200, "{\"messages\":[]}").await;
        let got = client(base)
            .find_by_rfc822_msgid("<x@passband>")
            .await
            .unwrap();
        handle.await.unwrap();
        assert_eq!(got, None);
    }

    #[tokio::test]
    async fn find_by_rfc822_msgid_surfaces_an_error_never_a_none() {
        // "Could not ask" must not read as "did not go out": that is a resend.
        let (base, handle) = mock_once(500, "{}").await;
        let got = client(base).find_by_rfc822_msgid("<x@passband>").await;
        handle.await.unwrap();
        assert!(matches!(got, Err(WriteError::Api { status: 500, .. })));
    }

    #[test]
    fn the_sent_scan_lists_sent_mail_between_two_instants() {
        assert_eq!(
            sent_scan_url("https://g.test/me", 1_790_000_000, 1_790_007_200, None),
            "https://g.test/me/messages?q=in%3Asent%20after%3A1790000000\
             %20before%3A1790007200&maxResults=25&includeSpamTrash=true"
        );
        assert_eq!(
            sent_scan_url("https://g.test/me", 5, 9, Some("t/1&x")),
            "https://g.test/me/messages?q=in%3Asent%20after%3A5%20before%3A9\
             &maxResults=25&includeSpamTrash=true&pageToken=t%2F1%26x"
        );
    }

    /// Serve one scripted `(status, body)` per connection, in order, and
    /// return the requests. Stops at the first connection that does not come
    /// within half a second, so a test can script MORE than it expects and
    /// prove the client stopped short.
    async fn mock_seq(
        responses: Vec<(u16, String)>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let mut reqs = Vec::new();
            for (status, body) in responses {
                let accepted =
                    tokio::time::timeout(std::time::Duration::from_millis(500), listener.accept())
                        .await;
                let Ok(Ok((mut sock, _))) = accepted else {
                    break;
                };
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap();
                reqs.push(String::from_utf8_lossy(&buf[..n]).to_string());
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
                sock.flush().await.unwrap();
            }
            reqs
        });
        (format!("http://{addr}"), handle)
    }

    const NO_MATCH: &str = "{\"resultSizeEstimate\":0}";

    fn meta(id: &str, outbox: &str, mid: &str) -> String {
        format!(
            "{{\"id\":\"{id}\",\"payload\":{{\"headers\":[\
               {{\"name\":\"Message-ID\",\"value\":\"{mid}\"}},\
               {{\"name\":\"X-Passband-Outbox\",\"value\":\"{outbox}\"}}]}}}}"
        )
    }

    fn window() -> (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) {
        (
            chrono::DateTime::from_timestamp(1_790_000_000, 0).unwrap(),
            chrono::DateTime::from_timestamp(1_790_007_200, 0).unwrap(),
        )
    }

    fn running() -> bool {
        false
    }

    #[tokio::test]
    async fn a_msgid_hit_short_circuits_the_scan() {
        let (base, handle) = mock_seq(vec![
            (200, "{\"messages\":[{\"id\":\"gm-1\"}]}".into()),
            (200, NO_MATCH.into()),
        ])
        .await;
        let got = client(base)
            .find_scheduled_send("<abc@passband>", window(), &running)
            .await
            .unwrap();
        assert_eq!(got.as_deref(), Some("gm-1"));
        let reqs = handle.await.unwrap();
        assert_eq!(reqs.len(), 1, "no scan after a hit");
        assert!(reqs[0].contains("rfc822msgid%3Aabc%40passband"));
    }

    #[tokio::test]
    async fn the_scan_finds_the_mail_by_its_outbox_header() {
        let (base, handle) = mock_seq(vec![
            (200, NO_MATCH.into()),
            (
                200,
                "{\"messages\":[{\"id\":\"gm-a\"},{\"id\":\"gm-b\"}]}".into(),
            ),
            (200, meta("gm-a", "other", "<x@mail.gmail.com>")),
            // Gmail's own Message-ID: the minted one did not survive.
            (200, meta("gm-b", " abc ", "<rewritten@mail.gmail.com>")),
        ])
        .await;
        let got = client(base)
            .find_scheduled_send("<abc@passband>", window(), &running)
            .await
            .unwrap();
        assert_eq!(got.as_deref(), Some("gm-b"));
        let reqs = handle.await.unwrap();
        assert_eq!(reqs.len(), 4);
        // Both ends of the window, in epoch seconds, as given.
        assert!(reqs[1].contains(
            "GET /messages?q=in%3Asent%20after%3A1790000000%20before%3A1790007200&maxResults=25"
        ));
        assert!(reqs[1].contains("includeSpamTrash=true"));
        for (req, id) in [(&reqs[2], "gm-a"), (&reqs[3], "gm-b")] {
            assert!(req.contains(&format!(
                "GET /messages/{id}?format=metadata&metadataHeaders=X-Passband-Outbox\
                 &metadataHeaders=Message-ID"
            )));
        }
    }

    #[tokio::test]
    async fn the_scan_also_matches_the_minted_message_id() {
        let (base, handle) = mock_seq(vec![
            (200, NO_MATCH.into()),
            (200, "{\"messages\":[{\"id\":\"gm-a\"}]}".into()),
            (
                200,
                "{\"id\":\"gm-a\",\"payload\":{\"headers\":[\
                  {\"name\":\"Message-Id\",\"value\":\"<abc@passband>\"}]}}"
                    .into(),
            ),
        ])
        .await;
        let got = client(base)
            .find_scheduled_send("<abc@passband>", window(), &running)
            .await
            .unwrap();
        handle.await.unwrap();
        assert_eq!(got.as_deref(), Some("gm-a"));
    }

    #[tokio::test]
    async fn a_clean_scan_pages_to_the_end_and_answers_none() {
        let (base, handle) = mock_seq(vec![
            (200, NO_MATCH.into()),
            (
                200,
                "{\"messages\":[{\"id\":\"p1\"}],\"nextPageToken\":\"tok1\"}".into(),
            ),
            (200, meta("p1", "nope", "<p1@x>")),
            (200, "{\"messages\":[{\"id\":\"p2\"}]}".into()),
            (200, meta("p2", "abcd", "<abc@passband.evil>")),
        ])
        .await;
        let got = client(base)
            .find_scheduled_send("<abc@passband>", window(), &running)
            .await
            .unwrap();
        let reqs = handle.await.unwrap();
        assert_eq!(got, None, "a prefix or a lookalike is not a match");
        assert_eq!(reqs.len(), 5);
        assert!(reqs[3].contains("&pageToken=tok1"));
    }

    #[tokio::test]
    async fn a_scan_that_reaches_its_page_cap_stops_and_cannot_rule_out() {
        let mut script = vec![(200, NO_MATCH.to_string())];
        for page in 0..=SCAN_MAX_PAGES {
            script.push((
                200,
                format!(
                    "{{\"messages\":[{{\"id\":\"p{page}\"}}],\"nextPageToken\":\"tok{page}\"}}"
                ),
            ));
            script.push((200, meta(&format!("p{page}"), "nope", "<n@x>")));
        }
        let (base, handle) = mock_seq(script).await;
        let got = client(base)
            .find_scheduled_send("<abc@passband>", window(), &running)
            .await;
        let reqs = handle.await.unwrap();
        assert!(matches!(got, Err(WriteError::Transport(_))), "{got:?}");
        assert_eq!(reqs.len(), 1 + SCAN_MAX_PAGES * 2, "stopped at the cap");
    }

    #[tokio::test]
    async fn a_5xx_mid_scan_is_an_error_never_a_none() {
        let (base, handle) = mock_seq(vec![
            (200, NO_MATCH.into()),
            (
                200,
                "{\"messages\":[{\"id\":\"gm-a\"},{\"id\":\"gm-b\"}]}".into(),
            ),
            (503, "{}".into()),
            (200, meta("gm-b", "abc", "<abc@passband>")),
        ])
        .await;
        let got = client(base)
            .find_scheduled_send("<abc@passband>", window(), &running)
            .await;
        let reqs = handle.await.unwrap();
        assert!(matches!(got, Err(WriteError::Api { status: 503, .. })));
        assert_eq!(reqs.len(), 3, "nothing asked after the failure");
    }

    #[tokio::test]
    async fn a_shutdown_mid_scan_is_an_error_never_a_none() {
        let (base, handle) = mock_seq(vec![
            (200, NO_MATCH.into()),
            (
                200,
                "{\"messages\":[{\"id\":\"gm-a\"},{\"id\":\"gm-b\"}]}".into(),
            ),
            (200, meta("gm-a", "other", "<x@mail.gmail.com>")),
            (200, meta("gm-b", "abc", "<abc@passband.app>")),
        ])
        .await;
        // Asked before the page and the first candidate; stopping by the
        // second.
        let asks = std::sync::atomic::AtomicUsize::new(0);
        let stopping = || asks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 2;
        let got = client(base)
            .find_scheduled_send("<abc@passband.app>", window(), &stopping)
            .await;
        let reqs = handle.await.unwrap();
        assert!(matches!(got, Err(WriteError::Transport(_))), "{got:?}");
        assert_eq!(reqs.len(), 3, "nothing asked after the shutdown");
        assert!(reqs[0].contains("rfc822msgid%3Aabc%40passband.app"));
    }
}
