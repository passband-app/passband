//! `GraphWriteClient` against a scripted Graph on loopback. Each test scripts
//! the answers in order and asserts the requests that left, on the wire.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use super::*;
use squelch_core::credentials::OAuthToken;
use squelch_core::error::Result as CoreResult;

struct StubCreds;
#[async_trait]
impl CredentialStore for StubCreds {
    async fn token(&self, _: AccountId) -> CoreResult<OAuthToken> {
        Ok(OAuthToken {
            access_token: "WRITE-TOKEN".into(),
            refresh_token: None,
            expires_at: None,
        })
    }
}

/// One request as the mock saw it.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    /// Path and query, percent-decoded.
    target: String,
    content_type: String,
    prefer: String,
    auth: String,
    body: Vec<u8>,
}

#[derive(Default)]
struct St {
    answers: VecDeque<(u16, String, Option<&'static str>)>,
    seen: Vec<Seen>,
}

#[derive(Clone, Default)]
struct Mock(Arc<Mutex<St>>);

async fn any(
    State(m): State<Mock>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let h = |n: &str| {
        headers
            .get(n)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    let target = uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("")
        .to_string();
    let target = percent_decode(&target);
    let mut st = m.0.lock().unwrap();
    st.seen.push(Seen {
        method: method.to_string(),
        target,
        content_type: h("content-type"),
        prefer: h("prefer"),
        auth: h("authorization"),
        body: body.to_vec(),
    });
    let (status, body, retry_after) = st.answers.pop_front().unwrap_or((500, "{}".into(), None));
    let mut resp = (StatusCode::from_u16(status).unwrap(), body).into_response();
    resp.headers_mut()
        .insert("content-type", "application/json".parse().unwrap());
    if let Some(ra) = retry_after {
        resp.headers_mut()
            .insert("retry-after", ra.parse().unwrap());
    }
    resp
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 3 <= b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn mock(answers: Vec<(u16, &str)>) -> (Mock, GraphWriteClient) {
    mock_with(answers.into_iter().map(|(s, b)| (s, b, None)).collect()).await
}

async fn mock_with(answers: Vec<(u16, &str, Option<&'static str>)>) -> (Mock, GraphWriteClient) {
    let m = Mock::default();
    m.0.lock().unwrap().answers = answers
        .into_iter()
        .map(|(s, b, r)| (s, b.to_string(), r))
        .collect();
    let app = Router::new().fallback(any).with_state(m.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let c = GraphWriteClient::with_base(Arc::new(StubCreds), 1, format!("http://{addr}"));
    (m, c)
}

fn seen(m: &Mock) -> Vec<Seen> {
    let s = m.0.lock().unwrap().seen.clone();
    for r in &s {
        assert_eq!(r.prefer, PREFER_IMMUTABLE_IDS, "{} {}", r.method, r.target);
        assert_eq!(r.auth, "Bearer WRITE-TOKEN");
    }
    s
}

fn body_json(r: &Seen) -> Value {
    serde_json::from_slice(&r.body).unwrap()
}

#[tokio::test]
async fn archive_trash_and_not_spam_are_moves_to_well_known_folders() {
    let (m, c) = mock(vec![(201, "{}"), (201, "{}"), (201, "{}")]).await;
    c.archive("A/b+c=").await.unwrap();
    c.trash("t").await.unwrap();
    c.not_spam("s").await.unwrap();
    let s = seen(&m);
    assert_eq!(s.len(), 3);
    for (r, (id, dest)) in
        s.iter()
            .zip([("A/b+c=", "archive"), ("t", "deleteditems"), ("s", "inbox")])
    {
        assert_eq!(r.method, "POST");
        assert_eq!(r.target, format!("/me/messages/{id}/move"));
        assert_eq!(body_json(r), json!({ "destinationId": dest }));
    }
}

#[tokio::test]
async fn modify_maps_read_state_flags_and_inbox_removal() {
    let (m, c) = mock(vec![(200, "{}"), (201, "{}")]).await;
    c.modify("m", &["STARRED".into()], &["UNREAD".into(), "INBOX".into()])
        .await
        .unwrap();
    let s = seen(&m);
    assert_eq!(s[0].method, "PATCH");
    assert_eq!(s[0].target, "/me/messages/m");
    assert_eq!(
        body_json(&s[0]),
        json!({ "isRead": true, "flag": { "flagStatus": "flagged" } })
    );
    assert_eq!(s[1].target, "/me/messages/m/move");
    assert_eq!(body_json(&s[1]), json!({ "destinationId": "archive" }));
}

#[tokio::test]
async fn any_other_label_refuses_the_whole_request_before_a_call() {
    let (m, c) = mock(vec![]).await;
    let err = c
        .modify("m", &["UNREAD".into(), "Label_42".into()], &[])
        .await
        .unwrap_err();
    assert!(matches!(err, WriteError::Invalid(_)), "{err}");
    let err = c.modify("m", &["INBOX".into()], &[]).await.unwrap_err();
    assert!(matches!(err, WriteError::Invalid(_)));
    assert!(seen(&m).is_empty(), "a refused label edit reached Graph");
}

#[tokio::test]
async fn send_is_a_mime_draft_then_send_and_returns_the_drafts_id() {
    let (m, c) = mock(vec![
        (201, r#"{"id":"IMM-9","conversationId":"conv-9"}"#),
        (202, ""),
    ])
    .await;
    let raw = b"From: me@outlook.com\r\nTo: a@example.com\r\nSubject: x\r\n\r\nhi";
    let sent = c.send(raw, Some("ignored-thread")).await.unwrap();
    assert_eq!(sent.id.as_deref(), Some("IMM-9"));
    assert_eq!(sent.thread_id.as_deref(), Some("conv-9"));
    let s = seen(&m);
    assert_eq!(s[0].method, "POST");
    assert_eq!(s[0].target, "/me/messages");
    assert_eq!(s[0].content_type, "text/plain");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&s[0].body)
            .unwrap(),
        raw
    );
    assert_eq!(s[1].target, "/me/messages/IMM-9/send");
    assert_eq!(s.len(), 2);
}

#[tokio::test]
async fn a_refused_send_deletes_its_draft_and_says_so() {
    let (m, c) = mock(vec![(201, r#"{"id":"d1"}"#), (400, "{}"), (204, "")]).await;
    assert!(matches!(
        c.send(b"x", None).await,
        Err(WriteError::Api { status: 400, .. })
    ));
    let s = seen(&m);
    assert_eq!(s[2].method, "DELETE");
    assert_eq!(s[2].target, "/me/messages/d1");
}

#[tokio::test]
async fn a_throttled_send_is_paced_and_deletes_its_draft() {
    let (m, c) = mock_with(vec![
        (201, r#"{"id":"d1"}"#, None),
        (429, "{}", Some("17")),
        (204, "", None),
    ])
    .await;
    assert!(matches!(
        c.send(b"x", None).await,
        Err(WriteError::Paced { retry_in_secs: 17 })
    ));
    assert_eq!(seen(&m)[2].method, "DELETE");
}

#[tokio::test]
async fn an_oversized_message_is_refused_before_a_draft_exists() {
    let (m, c) = mock(vec![]).await;
    let big = vec![b'a'; GRAPH_MAX_REQUEST];
    assert!(matches!(
        c.send(&big, None).await,
        Err(WriteError::Invalid(_))
    ));
    assert!(seen(&m).is_empty());
}

#[tokio::test]
async fn upstream_text_is_never_echoed() {
    let (_m, c) = mock(vec![(403, r#"{"error":{"message":"secret upstream"}}"#)]).await;
    let err = c.archive("m").await.unwrap_err().to_string();
    assert!(!err.contains("secret upstream"), "{err}");
}

#[tokio::test]
async fn fetch_raw_is_the_mime_value_reencoded_base64url() {
    let (m, c) = mock(vec![
        (
            200,
            r#"{"id":"m","conversationId":"c","receivedDateTime":"2026-10-01T12:00:00Z"}"#,
        ),
        (200, "Subject: hi\r\n\r\nbody"),
    ])
    .await;
    let f = c.fetch_raw("m").await.unwrap();
    assert_eq!(
        squelch_core::sync::decode_raw_b64url(&f.raw_b64).unwrap(),
        b"Subject: hi\r\n\r\nbody"
    );
    assert_eq!(f.thread_id.as_deref(), Some("c"));
    assert_eq!(f.internal_date.as_deref(), Some("1790856000000"));
    assert_eq!(seen(&m)[1].target, "/me/messages/m/$value");
}

#[tokio::test]
async fn parent_headers_come_from_the_message_id_references_and_recipients() {
    let (_m, c) = mock(vec![(
        200,
        r#"{
            "internetMessageId": "<p@x>",
            "internetMessageHeaders": [
                {"name": "References", "value": "<a@x>"},
                {"name": "references", "value": "<b@x>"}
            ],
            "from": {"emailAddress": {"name": "Ann", "address": "ann@x.com"}},
            "replyTo": [],
            "toRecipients": [{"emailAddress": {"address": "me@outlook.com"}}],
            "ccRecipients": [{"emailAddress": {"name": "B", "address": "b@x.com"}}]
        }"#,
    )])
    .await;
    let p = c.parent_headers("m").await.unwrap();
    assert_eq!(p.message_id.as_deref(), Some("<p@x>"));
    assert_eq!(p.references.as_deref(), Some("<a@x> <b@x>"));
    assert_eq!(p.from.as_deref(), Some(r#""Ann" <ann@x.com>"#));
    assert_eq!(p.references.as_deref(), Some("<a@x> <b@x>"));
    assert_eq!(p.to.as_deref(), Some("me@outlook.com"));
    assert_eq!(p.cc.as_deref(), Some(r#""B" <b@x.com>"#));
    assert_eq!(p.reply_to, None);
}

#[tokio::test]
async fn the_message_id_lookup_quotes_its_literal() {
    let (m, c) = mock(vec![(200, r#"{"value":[{"id":"hit"}]}"#)]).await;
    assert_eq!(
        c.find_by_rfc822_msgid("<o'brien@x>")
            .await
            .unwrap()
            .as_deref(),
        Some("hit")
    );
    assert!(
        seen(&m)[0]
            .target
            .contains("$filter=internetMessageId eq '<o''brien@x>' and isDraft eq false"),
        "{}",
        seen(&m)[0].target
    );
}

fn window() -> (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) {
    let now = chrono::Utc::now();
    (now - chrono::Duration::hours(1), now)
}

#[tokio::test]
async fn the_scheduled_send_scan_finds_the_outbox_header() {
    let mid = "<sched-abc123@passband>";
    let token = outbox_token(mid);
    let page = json!({
        "value": [
            {"id": "other", "internetMessageId": "<z@x>", "internetMessageHeaders": []},
            {"id": "ours", "internetMessageId": "<rewritten@exchange>",
             "internetMessageHeaders": [{"name": OUTBOX_HEADER, "value": token}]}
        ]
    })
    .to_string();
    let (m, c) = mock(vec![(200, r#"{"value":[]}"#), (200, &page)]).await;
    let hit = c
        .find_scheduled_send(mid, window(), &|| false)
        .await
        .unwrap();
    assert_eq!(hit.as_deref(), Some("ours"));
    assert!(
        seen(&m)[1]
            .target
            .starts_with("/me/mailFolders/sentitems/messages?")
    );
}

#[tokio::test]
async fn a_scan_that_cannot_rule_the_mail_out_is_an_error_never_a_none() {
    let mid = "<sched-abc123@passband>";
    // Every page says there is more: the cap is reached.
    let base_page = |base: &str| {
        json!({"value": [], "@odata.nextLink": format!("{base}/me/mailFolders/sentitems/messages?$skiptoken=x")})
            .to_string()
    };
    let (m, c) = mock(vec![]).await;
    let base = c.base.clone();
    {
        let mut st = m.0.lock().unwrap();
        st.answers.push_back((200, r#"{"value":[]}"#.into(), None));
        for _ in 0..SCAN_MAX_PAGES {
            st.answers.push_back((200, base_page(&base), None));
        }
    }
    assert!(
        c.find_scheduled_send(mid, window(), &|| false)
            .await
            .is_err()
    );

    // A next link off the Graph base is refused, not followed.
    let (_m, c) = mock(vec![
        (200, r#"{"value":[]}"#),
        (200, &base_page("https://evil.example")),
    ])
    .await;
    assert!(
        c.find_scheduled_send(mid, window(), &|| false)
            .await
            .is_err()
    );

    // A shutdown mid-scan is an error.
    let (_m, c) = mock(vec![(200, r#"{"value":[]}"#)]).await;
    assert!(
        c.find_scheduled_send(mid, window(), &|| true)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_send_whose_outcome_is_unknown_keeps_its_draft() {
    for status in [500u16, 503, 504, 408] {
        let (m, c) = mock(vec![(201, r#"{"id":"d1"}"#), (status, "{}")]).await;
        let err = c.send(b"x", None).await.unwrap_err();
        // The caller's "maybe sent" is a Transport error or a 5xx.
        assert!(
            matches!(err, WriteError::Transport(_))
                || matches!(err, WriteError::Api { status, .. } if status >= 500),
            "{status}: {err}"
        );
        let s = seen(&m);
        assert_eq!(
            s.len(),
            2,
            "{status}: the draft (maybe the sent mail) was deleted"
        );
    }
}

#[tokio::test]
async fn folded_references_are_unfolded_and_names_lose_their_quotes() {
    let (_m, c) = mock(vec![(
        200,
        r#"{"internetMessageId":"<p@x>",
            "internetMessageHeaders":[{"name":"References","value":"<a@x>\r\n <b@x>"}],
            "ccRecipients":[
              {"emailAddress":{"name":"He said \"Re: hi","address":"one@x.com"}},
              {"emailAddress":{"address":"two@x.com"}}]}"#,
    )])
    .await;
    let p = c.parent_headers("m").await.unwrap();
    assert_eq!(p.references.as_deref(), Some("<a@x> <b@x>"));
    assert_eq!(
        p.cc.as_deref(),
        Some(r#""He said Re: hi" <one@x.com>, two@x.com"#)
    );
}

#[tokio::test]
async fn a_label_both_added_and_removed_is_refused() {
    let (m, c) = mock(vec![]).await;
    let err = c
        .modify("m", &["UNREAD".into()], &["UNREAD".into()])
        .await
        .unwrap_err();
    assert!(matches!(err, WriteError::Invalid(_)));
    assert!(seen(&m).is_empty());
}

#[test]
fn rewrite_from_swaps_only_the_address() {
    use super::rewrite_from;
    let raw = b"Subject: x\r\nFrom: \"Braelyn B\" <passband-qa@outlook.com>\r\nTo: a@example.com\r\n\r\nFrom: not a header\r\n";
    let out = rewrite_from(raw, "braelyn@passband.app").unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("From: \"Braelyn B\" <braelyn@passband.app>\r\nTo:"),
        "{out}"
    );
    assert!(
        out.ends_with("\r\n\r\nFrom: not a header\r\n"),
        "the body is untouched"
    );
    // Already right: nothing to do.
    assert!(rewrite_from(raw, "PASSBAND-QA@outlook.com").is_none());
    // A bare address, and an encoded-word name kept as written.
    let bare = rewrite_from(b"From: a@x.com\r\n\r\nb", "b@y.com").unwrap();
    assert_eq!(bare, b"From: b@y.com\r\n\r\nb");
    let enc = rewrite_from(
        b"From: =?utf-8?b?w6lsw6g=?=\r\n <a@x.com>\r\n\r\nb",
        "b@y.com",
    )
    .unwrap();
    assert_eq!(enc, b"From: =?utf-8?b?w6lsw6g=?=\r\n <b@y.com>\r\n\r\nb");
    // Two From headers: leave it alone.
    assert!(rewrite_from(b"From: a@x.com\r\nFrom: c@x.com\r\n\r\nb", "b@y.com").is_none());
}

#[tokio::test]
async fn a_send_goes_out_as_the_address_exchange_will_use() {
    let (m, c) = mock(vec![
        (
            200,
            r#"{"mail":"braelyn@passband.app","userPrincipalName":"braelyn@passband.app"}"#,
        ),
        (201, r#"{"id":"IMM-9","conversationId":"conv-9"}"#),
        (202, ""),
    ])
    .await;
    let c = c.resolving_send_as();
    let raw = b"From: Me <passband-qa@outlook.com>\r\nTo: a@example.com\r\nSubject: x\r\n\r\nhi";
    c.send(raw, None).await.unwrap();
    let s = seen(&m);
    assert_eq!(s[0].method, "GET");
    assert!(s[0].target.starts_with("/me?"), "{}", s[0].target);
    let draft = base64::engine::general_purpose::STANDARD
        .decode(&s[1].body)
        .unwrap();
    assert!(
        String::from_utf8(draft)
            .unwrap()
            .starts_with("From: Me <braelyn@passband.app>\r\n"),
    );

    // A failed lookup sends as composed.
    let (m, c) = mock(vec![(500, "{}"), (201, r#"{"id":"d"}"#), (202, "")]).await;
    let c = c.resolving_send_as();
    c.send(raw, None).await.unwrap();
    let s = seen(&m);
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&s[1].body)
            .unwrap(),
        raw
    );
}
