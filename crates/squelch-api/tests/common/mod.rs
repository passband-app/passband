//! Shared scaffolding for the human-door integration tests: the bearer every
//! test authenticates with, the store + account + state bootstrap, the request
//! builders, and the one inbound-message fixture.
//!
//! Each test binary compiles its own copy of this module and uses only part of
//! it, so unused helpers here are expected rather than rot.
#![allow(dead_code)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, header};
use http_body_util::BodyExt;
use serde_json::Value;
use squelch_api::{ApiState, router};
use squelch_core::store::SqliteStore;
use squelch_core::types::NewMessage;

/// The bearer every test authenticates with.
pub const TOKEN: &str = "test-secret-token";

/// A mounted router plus the store and account it was built over.
pub struct Harness {
    pub app: axum::Router,
    pub store: Arc<SqliteStore>,
    pub acct: i64,
}

/// An in-memory store, one account, `seed` applied, and state over the pair —
/// stopping short of the router so a caller can decorate the state first.
pub fn state_with(seed: impl FnOnce(&SqliteStore, i64)) -> (ApiState, Arc<SqliteStore>, i64) {
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let acct = store.ensure_account("me@example.com").unwrap();
    seed(&store, acct);
    let state = ApiState::new(store.clone(), acct, TOKEN);
    (state, store, acct)
}

/// [`state_with`] with the router mounted: the default bootstrap.
pub fn harness(seed: impl FnOnce(&SqliteStore, i64)) -> Harness {
    let (state, store, acct) = state_with(seed);
    Harness {
        app: router(state),
        store,
        acct,
    }
}

/// The `calendar` hidden feature, pinned on. Everything behind the Calendar
/// page 404s without it (`squelch_core::features::CALENDAR`).
pub fn calendar_on() -> squelch_core::features::Features {
    squelch_core::features::Features::pinned([squelch_core::features::CALENDAR])
}

/// [`harness`] for a mailbox with the `calendar` hidden feature on.
pub fn calendar_harness(seed: impl FnOnce(&SqliteStore, i64)) -> Harness {
    let (state, store, acct) = state_with(seed);
    Harness {
        app: router(state.with_features(calendar_on())),
        store,
        acct,
    }
}

/// An authed request with no body.
pub fn authed(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap()
}

/// A JSON-bodied request; `bearer` decides whether it carries the token, so the
/// unauthed case is the same request minus one header.
pub fn json_request(method: &str, uri: &str, body: &Value, bearer: bool) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if bearer {
        b = b.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    }
    b.body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

/// The authed form of [`json_request`].
pub fn authed_json(method: &str, uri: &str, body: Value) -> Request<Body> {
    json_request(method, uri, &body, true)
}

/// Collect a response body as JSON; an empty body (204s and friends) is `Null`.
pub async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    if bytes.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(&bytes).unwrap()
}

/// The one inbound-message fixture: an ordinary note from Alice.
pub fn msg(account_id: i64, gmail: &str, thread: &str, subject: &str, body: &str) -> NewMessage {
    NewMessage {
        account_id,
        gmail_msg_id: gmail.to_string(),
        thread_id: thread.to_string(),
        from_addr: "alice@example.com".to_string(),
        from_name: Some("Alice".to_string()),
        subject: subject.to_string(),
        received_at: chrono::Utc::now(),
        snippet: subject.to_string(),
        body: body.to_string(),
        body_html: None,
        is_sent: false,
        is_spam: false,
        to_addrs: None,
        cc_addrs: None,
        bcc_addrs: None,
        list_unsubscribe: None,
        list_unsub_one_click: false,
        auth_pass: None,
        sender_signals: None,
    }
}

/// The outbound counterpart of [`msg`]: a message the account SENT, carrying the
/// display recipients the sent listing reads.
pub fn sent_msg(account_id: i64, gmail: &str, thread: &str, subject: &str, to: &str) -> NewMessage {
    NewMessage {
        from_addr: "me@example.com".to_string(),
        from_name: Some("Me".to_string()),
        is_sent: true,
        to_addrs: Some(to.to_string()),
        ..msg(account_id, gmail, thread, subject, "what I wrote")
    }
}

/// Store `message` the way the FIRST-RUN BACKFILL does: message and triage row
/// in one ingest, stamped `triage.first_load` (only the backfill origin sets
/// it). Returns the id; the triage decision is the caller's.
pub fn ingest_first_load(store: &SqliteStore, message: &NewMessage) -> i64 {
    use squelch_core::config::Stage1Config;
    use squelch_core::store::Store;
    use squelch_core::sync::ingest::{RawFetched, ingest};
    let fetched = RawFetched {
        account_id: message.account_id,
        gmail_msg_id: message.gmail_msg_id.clone(),
        gmail_thread_id: Some(message.thread_id.clone()),
        raw: format!(
            "From: {}\r\nTo: me@example.com\r\nSubject: {}\r\n\r\n{}\r\n",
            message.from_addr, message.subject, message.body
        )
        .into_bytes(),
        internal_date: Some(message.received_at),
        is_sent: false,
        is_spam: false,
        account_addr: "me@example.com".to_string(),
    };
    let mut triaged = ingest(
        &fetched,
        &Stage1Config::default(),
        chrono::Utc::now(),
        |_| false,
    );
    triaged.message = message.clone();
    triaged.first_load = true;
    store.ingest_message(&triaged).unwrap()
}
