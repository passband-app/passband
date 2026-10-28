//! docs/SECURITY.md §4 and §4b, at the wire.
//!
//! The server tests (src/server.rs) prove each read tool's guard by calling
//! the tool method directly. This file proves the same guarantees the way an
//! agent meets them: JSON-RPC over the streamable HTTP transport, through
//! `agent_door_router`, the one construction path every host uses. A tool
//! wired to the wrong store call, a new tool that forgets the guard, or a
//! transport change that serializes something extra would pass the method
//! tests and fail here.
//!
//! One mailbox is seeded with a row of every kind the door must not show
//! (restricted by the access check, still pending, sealed by the owner, spam
//! that was assessed as allowed, and an allowed summary derived from a
//! restricted source), each carrying a unique canary word. Every read tool is
//! then called through the wire, and no response may contain any of those
//! words. One plainly allowed row is the control: it must appear, or the test
//! would pass on a door that serves nothing.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use squelch_core::store::agent_triage::{AgentCommitOutcome, AgentTriageStore as _};
use squelch_core::store::{SqliteStore, Store as _};
use squelch_core::sync::ingest::{RawFetched, ingest_with_rules};
use squelch_core::triage::decision::MessageDecision;
use squelch_core::types::AccountId;
use squelch_mcp::{AgentDoorAuth, agent_door_router};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;

const ACCOUNT: &str = "me@localhost";
/// Open mode keeps rmcp's loopback-only Host guard.
const HOST: &str = "localhost";

/// The control: allowed, and must be served.
const VISIBLE: &str = "kestrelvisible";
/// Rows the agent door must never serve, by the reason they are hidden.
const RESTRICTED: &str = "ospreyrestricted";
const PENDING: &str = "harrierpending";
const OWNER_SEALED: &str = "merlinownersealed";
const SPAM: &str = "shrikespam";
const DERIVED: &str = "plovederived";
const FORBIDDEN: [&str; 5] = [RESTRICTED, PENDING, OWNER_SEALED, SPAM, DERIVED];

/// The tools a door with no mail pipeline offers. Exact: a tool that archives,
/// labels, deletes, unsubscribes or touches the calendar must never appear
/// here without this list (and SECURITY.md §4c) changing with it.
const READ_ONLY_TOOLS: [&str; 8] = [
    "get_deadlines",
    "get_inbox_updates",
    "get_reading",
    "get_shipments",
    "get_thread",
    "list_sender_rules",
    "search_mail",
    "set_sender_rule",
];

fn ingest(store: &SqliteStore, account: AccountId, word: &str, is_spam: bool) -> i64 {
    let now: DateTime<Utc> = Utc::now();
    let raw = RawFetched {
        account_id: account,
        gmail_msg_id: format!("m-{word}"),
        gmail_thread_id: Some(format!("t-{word}")),
        raw: format!(
            "From: Alerts <alerts@example.com>\r\nTo: {ACCOUNT}\r\nSubject: Update {word}\r\nDate: {}\r\n\r\nThe {word} report is attached.",
            now.to_rfc2822()
        )
        .into_bytes(),
        internal_date: Some(now),
        is_sent: false,
        is_spam,
        account_addr: ACCOUNT.into(),
    };
    let message = ingest_with_rules(&raw, &Default::default(), now, &[], |_| false);
    store.ingest_message(&message).expect("ingest")
}

/// Run every queued triage/access job, deciding each message by `restricted`
/// and, for `derived`, citing `extra_source` as a second source.
fn assess_all(
    store: &SqliteStore,
    account: AccountId,
    restricted: &[i64],
    derived: Option<(i64, i64)>,
) {
    while let Some(job) = ["triage", "access"].into_iter().find_map(|kind| {
        store
            .claim_agent_job(account, kind, Utc::now(), 120)
            .expect("claim")
    }) {
        let id = job.message_id;
        let context = store.load_agent_context(&job).expect("context");
        let mut decision = MessageDecision {
            summary: "A meaningful update".into(),
            ..Default::default()
        };
        decision.external_access.restricted = restricted.contains(&id);
        decision.attention.show_in_fye = true;
        decision.attention.summary = "An update worth reading".into();
        decision.attention.relevant_message_ids = vec![id];
        let mut sources = vec![context.message.source.clone()];
        if let Some((derived_id, source)) = derived
            && derived_id == id
        {
            sources.push(
                store
                    .agent_read_message(account, source)
                    .expect("source")
                    .source,
            );
        }
        assert_eq!(
            store
                .commit_agent_decision(&job, &context, &decision, &sources)
                .expect("commit"),
            AgentCommitOutcome::Applied
        );
    }
}

/// The mailbox described in the module comment.
fn seeded_door() -> axum::Router {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let account = store.ensure_account(ACCOUNT).expect("account");

    let _visible = ingest(&store, account, VISIBLE, false);
    let restricted = ingest(&store, account, RESTRICTED, false);
    let owner_sealed = ingest(&store, account, OWNER_SEALED, false);
    let _spam = ingest(&store, account, SPAM, true);
    let derived = ingest(&store, account, DERIVED, false);
    assess_all(&store, account, &[restricted], Some((derived, restricted)));

    // Allowed by the check, then sealed by the owner by hand.
    store
        .correct_agent_triage(
            account,
            owner_sealed,
            "external_access",
            &json!(true),
            Utc::now(),
        )
        .expect("owner seal");

    // Ingested last, so its job stays queued: never assessed at all.
    ingest(&store, account, PENDING, false);

    agent_door_router(
        store,
        ACCOUNT,
        Default::default(),
        Default::default(),
        CancellationToken::new(),
        &AgentDoorAuth::Open,
        None,
        Default::default(),
    )
    .expect("router")
}

/// One MCP session over the real transport.
struct Session {
    app: axum::Router,
    id: String,
    next: u64,
}

impl Session {
    async fn open(app: axum::Router) -> Self {
        let resp = app
            .clone()
            .oneshot(post(
                None,
                json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
                    "protocolVersion":"2025-06-18","capabilities":{},
                    "clientInfo":{"name":"wire-test","version":"1"}}}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let id = resp
            .headers()
            .get("mcp-session-id")
            .expect("stateful transport issues a session id")
            .to_str()
            .unwrap()
            .to_owned();
        drop(body_text(resp).await);
        let resp = app
            .clone()
            .oneshot(post(
                Some(&id),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            ))
            .await
            .unwrap();
        assert!(resp.status().is_success(), "{}", resp.status());
        Self { app, id, next: 1 }
    }

    /// One request; returns the raw response text and the parsed JSON-RPC
    /// message. The raw text is what the canary check reads, so nothing the
    /// transport adds around the result escapes it.
    async fn call(&mut self, method: &str, params: Value) -> (String, Value) {
        let id = self.next;
        self.next += 1;
        let resp = self
            .app
            .clone()
            .oneshot(post(
                Some(&self.id),
                json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{method}");
        let text = body_text(resp).await;
        let message = text
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
            .find(|m| m["id"] == json!(id))
            .or_else(|| serde_json::from_str::<Value>(&text).ok())
            .unwrap_or_else(|| panic!("{method}: no JSON-RPC response in {text:?}"));
        (text, message)
    }

    async fn tool(&mut self, name: &str, arguments: Value) -> String {
        let (text, message) = self
            .call("tools/call", json!({"name": name, "arguments": arguments}))
            .await;
        assert!(
            message.get("result").is_some() || message.get("error").is_some(),
            "{name}: {message}"
        );
        text
    }
}

fn post(session: Option<&str>, body: Value) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::HOST, HOST)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if let Some(id) = session {
        b = b.header("mcp-session-id", id);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

async fn body_text(resp: axum::response::Response) -> String {
    let bytes = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        axum::body::to_bytes(resp.into_body(), usize::MAX),
    )
    .await
    .expect("response body completes")
    .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn assert_no_canary(tool: &str, text: &str) {
    for word in FORBIDDEN {
        assert!(
            !text.contains(word),
            "{tool} served `{word}` to the agent door:\n{text}"
        );
    }
}

#[tokio::test]
async fn the_door_offers_exactly_the_read_tools_without_a_mail_pipeline() {
    let mut session = Session::open(seeded_door()).await;
    let (_, listed) = session.call("tools/list", json!({})).await;
    let names: BTreeSet<&str> = listed["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, READ_ONLY_TOOLS.into_iter().collect::<BTreeSet<_>>());

    // A tool that is not offered is not callable either.
    for name in [
        "create_draft",
        "send_email",
        "open_draft",
        "archive",
        "delete",
    ] {
        let (text, message) = session
            .call("tools/call", json!({"name": name, "arguments": {}}))
            .await;
        let refused = message.get("error").is_some() || message["result"]["isError"] == json!(true);
        assert!(refused, "{name} was callable: {text}");
    }
}

#[tokio::test]
async fn no_read_tool_serves_hidden_mail_over_the_wire() {
    let mut session = Session::open(seeded_door()).await;
    let since = "2000-01-01T00:00:00Z";

    // Control first: the door does serve the allowed row.
    let inbox = session
        .tool("get_inbox_updates", json!({"since": since}))
        .await;
    assert!(
        inbox.contains(VISIBLE),
        "control: the allowed row must reach the agent:\n{inbox}"
    );
    assert_no_canary("get_inbox_updates", &inbox);

    let reading = session.tool("get_reading", json!({"since": since})).await;
    assert_no_canary("get_reading", &reading);

    let visible_thread = session
        .tool("get_thread", json!({"id": format!("t-{VISIBLE}")}))
        .await;
    assert!(
        visible_thread.contains(VISIBLE),
        "control:\n{visible_thread}"
    );

    for word in std::iter::once(VISIBLE).chain(FORBIDDEN) {
        // Search by the canary itself, by the shared words, and by sender:
        // each search leg must drop the hidden rows, not just the top hits.
        for query in [
            word.to_owned(),
            format!("Update {word}"),
            "report attached".to_owned(),
            "from:alerts@example.com".to_owned(),
        ] {
            for order in ["recent", "best_match"] {
                let found = session
                    .tool(
                        "search_mail",
                        json!({"query": query, "k": 50, "order": order}),
                    )
                    .await;
                assert_no_canary(&format!("search_mail({query:?}, {order})"), &found);
            }
        }
        let thread = session
            .tool("get_thread", json!({"id": format!("t-{word}")}))
            .await;
        if word != VISIBLE {
            assert_no_canary(&format!("get_thread(t-{word})"), &thread);
        }
    }

    for (name, args) in [
        ("get_deadlines", json!({})),
        ("get_shipments", json!({"include_delivered": true})),
        ("list_sender_rules", json!({})),
    ] {
        let text = session.tool(name, args).await;
        assert_no_canary(name, &text);
    }
}
