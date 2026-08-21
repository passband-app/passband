//! The Gmail write transport: `messages.modify`, `messages.send`,
//! `messages.get`, behind [`MailWriter`].
//!
//! Everything in this file that assembles RFC822 — replies, forwards,
//! recipient derivation, boundary selection — is PROVIDER-NEUTRAL and is
//! re-exported from [`crate::write`] for any transport that can post raw MIME.
//! Only the handful of request builders and [`GmailWriteClient`] itself speak
//! Gmail.
//!
//! Tokens, auth headers and bodies are NEVER logged.

use base64::Engine as _;
use serde_json::{Value, json};

use squelch_core::credentials::CredentialStore;
// The Gmail endpoint, the INBOX label and the Gmail-API response shapes are
// defined once in core's Gmail transport; the write path reuses them rather
// than re-deriving its own copy. Archiving == removing `LABEL_INBOX`.
use squelch_core::mail::gmail::{GMAIL_API_BASE, GmailMessage, LABEL_INBOX};
use squelch_core::types::AccountId;

use crate::write::mime::ParentHeaders;
use crate::write::{FetchedRaw, MailWriter, SentRef, WriteError};

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

/// Executes Gmail write ops with a WRITE-bound credential store. The token is
/// fetched per call and never retained.
pub struct GmailWriteClient {
    creds: std::sync::Arc<dyn CredentialStore>,
    account_id: AccountId,
    http: reqwest::Client,
    /// API base URL. Overridable in tests only; production uses Gmail's.
    base: String,
}

impl GmailWriteClient {
    pub fn new(creds: std::sync::Arc<dyn CredentialStore>, account_id: AccountId) -> Self {
        Self::with_base(creds, account_id, GMAIL_API_BASE.to_string())
    }

    /// Construct with an explicit API base (tests). Production uses [`Self::new`].
    pub fn with_base(
        creds: std::sync::Arc<dyn CredentialStore>,
        account_id: AccountId,
        base: String,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("reqwest client build");
        Self {
            creds,
            account_id,
            http,
            base,
        }
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
        let token = self.write_token().await?;
        let resp = self
            .http
            .post(url)
            .bearer_auth(&token)
            .json(body)
            .send()
            .await
            .map_err(|e| WriteError::Transport(format!("gmail request failed: {e}")))?;
        let status = resp.status();
        if status.is_success() {
            resp.json::<Value>()
                .await
                .map_err(|e| WriteError::Transport(format!("gmail json decode: {e}")))
        } else {
            // Never echo the upstream body (it may carry request context).
            Err(WriteError::Api {
                status: status.as_u16(),
                message: "request rejected".into(),
            })
        }
    }

    /// GET a JSON body with the write bearer token (gmail.modify grants read).
    /// Never logs the token or the response.
    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, WriteError> {
        let token = self.write_token().await?;
        let resp = self
            .http
            .get(url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| WriteError::Transport(format!("gmail request failed: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            // Never echo the upstream body (it may carry request context).
            return Err(WriteError::Api {
                status: status.as_u16(),
                message: "request rejected".into(),
            });
        }
        resp.json::<T>()
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
        let v = self.post_json(&url, &body).await?;
        Ok(SentRef {
            id: v["id"].as_str().map(str::to_string),
            thread_id: v["threadId"].as_str().map(str::to_string),
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
}

// --- forwarding --------------------------------------------------------------
//
// A forward is NOT a reply: it starts a new conversation (no In-Reply-To, no
// References, no threadId), it carries the original's body AND its attachments
// back out, and its note may be empty — "here, look at this" is the whole point.
// Everything below composes that message from the original's RAW RFC822 bytes,
// because what is forwarded must be what was received, not what our store's
// baked/sanitized copy remembers of it.

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
}
