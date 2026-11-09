//! TWO-DOOR INVARIANT for the owner profile (docs/UNDERSTANDING.md §2.2, §7):
//! nothing from `memory_entries` reaches any `/mcp` response.
//!
//! The profile is a set of sentences about the owner and the people they
//! answer. Triage (the human door's backend) reads it and `/client/memory`
//! shows it; the agent door was never handed it. Two checks, because either
//! alone can pass on a door that leaks:
//!
//! - AT THE WIRE: a mailbox whose profile carries a canary word in every
//!   column a tool could serialize (the line, the body, the scope) is read
//!   through every tool the door offers, and no response may carry it. One
//!   allowed message is the control, so the test cannot pass on a door that
//!   serves nothing.
//! - IN THE SOURCE: the agent door's crate names none of the profile's store
//!   calls, so a tool added later cannot start reading it without this file
//!   changing.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use squelch_core::store::agent_triage::{AgentCommitOutcome, AgentTriageStore as _};
use squelch_core::store::{SqliteStore, Store as _};
use squelch_core::sync::ingest::{RawFetched, ingest_with_rules};
use squelch_core::triage::decision::MessageDecision;
use squelch_core::triage::memory::{MemoryRecord, Origin, Provenance, new_ulid};
use squelch_mcp::testing::wire::Session;
use squelch_mcp::{AgentDoorAuth, agent_door_router};
use tokio_util::sync::CancellationToken;

const ACCOUNT: &str = "me@localhost";
const HOST: &str = "localhost";
const SENDER: &str = "alerts@example.com";
const VISIBLE: &str = "kestrelvisible";
/// In each entry's line, body and scope respectively.
const LINE_CANARY: &str = "ptarmiganline";
const BODY_CANARY: &str = "dotterelbody";
const SCOPE_CANARY: &str = "godwitscope";
/// In each entry's provenance addresses: who it rests on.
const PROVENANCE_CANARY: &str = "curlewprovenance@example.org";

/// The door, and the ids of the entries seeded behind it.
fn seeded_door() -> (axum::Router, Vec<String>) {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let account = store.ensure_account(ACCOUNT).expect("account");
    let now: DateTime<Utc> = Utc::now();
    let raw = RawFetched {
        account_id: account,
        gmail_msg_id: "m-visible".into(),
        gmail_thread_id: Some("t-visible".into()),
        raw: format!(
            "From: Alerts <{SENDER}>\r\nTo: {ACCOUNT}\r\nSubject: Update {VISIBLE}\r\nDate: {}\r\n\r\nThe {VISIBLE} report is attached.",
            now.to_rfc2822()
        )
        .into_bytes(),
        internal_date: Some(now),
        is_sent: false,
        is_spam: false,
        account_addr: ACCOUNT.into(),
    };
    let message = ingest_with_rules(&raw, &Default::default(), now, &[], |_| false);
    let id = store.ingest_message(&message).expect("ingest");
    while let Some(job) = ["triage", "access"].into_iter().find_map(|kind| {
        store
            .claim_agent_job(account, kind, Utc::now(), 120)
            .expect("claim")
    }) {
        let context = store.load_agent_context(&job).expect("context");
        let mut decision = MessageDecision {
            summary: "A meaningful update".into(),
            ..Default::default()
        };
        decision.attention.show_in_fye = true;
        decision.attention.summary = "An update worth reading".into();
        decision.attention.relevant_message_ids = vec![id];
        assert_eq!(
            store
                .commit_agent_decision(
                    &job,
                    &context,
                    &decision,
                    std::slice::from_ref(&context.message.source)
                )
                .expect("commit"),
            AgentCommitOutcome::Applied
        );
    }
    let mut ids = Vec::new();
    // A profile entry in every scope a run could be served, each carrying all
    // three canaries, both origins, citing the visible message.
    for (i, (scope, origin)) in [
        ("owner".to_string(), Origin::Stated),
        (format!("sender:{SENDER}"), Origin::Inferred),
        ("domain:example.com".to_string(), Origin::Inferred),
        (format!("kind:{SCOPE_CANARY}"), Origin::Inferred),
    ]
    .into_iter()
    .enumerate()
    {
        let at = now - Duration::days(1);
        let entry_id = new_ulid(now + Duration::milliseconds(i as i64));
        ids.push(entry_id.clone());
        store
            .insert_memory_entry_unchecked(
                account,
                &MemoryRecord {
                    id: entry_id,
                    scope,
                    origin,
                    kind: "role".into(),
                    body: json!({"what": format!("Works on {BODY_CANARY}")}),
                    line: format!("Works on {LINE_CANARY}."),
                    provenance: Provenance {
                        message_ids: vec![id],
                        addrs: vec![SENDER.into(), PROVENANCE_CANARY.into()],
                    },
                    confidence: 0.9,
                    revision: 1,
                    created_at: at,
                    updated_at: at,
                    affirmed_at: at,
                    expires_at: None,
                    retired_at: None,
                    deleted_at: None,
                },
            )
            .expect("memory entry");
    }
    let router = agent_door_router(
        store,
        ACCOUNT,
        Default::default(),
        Default::default(),
        CancellationToken::new(),
        &AgentDoorAuth::Open,
        None,
        Default::default(),
        Default::default(),
    )
    .expect("router");
    (router, ids)
}

fn assert_no_memory(tool: &str, text: &str, ids: &[String]) {
    let words = [LINE_CANARY, BODY_CANARY, SCOPE_CANARY, PROVENANCE_CANARY]
        .into_iter()
        .chain(ids.iter().map(String::as_str));
    for word in words {
        assert!(
            !text.contains(word),
            "{tool} served the owner profile (`{word}`) to the agent door:\n{text}"
        );
    }
}

#[tokio::test]
async fn no_tool_response_carries_the_owner_profile() {
    let (door, ids) = seeded_door();
    assert_eq!(ids.len(), 4);
    let mut session = Session::open(door, HOST, None).await;
    let since = "2000-01-01T00:00:00Z";
    let inbox = session
        .tool("get_inbox_updates", json!({"since": since}))
        .await
        .0;
    assert!(inbox.contains("t-visible"), "control: {inbox}");

    let mut called = Vec::new();
    for name in session.tool_names().await {
        let calls = match name.as_str() {
            "get_inbox_updates" | "get_reading" => vec![json!({"since": since})],
            "get_thread" => vec![json!({"id": "t-visible"})],
            "search_mail" => [
                VISIBLE,
                LINE_CANARY,
                BODY_CANARY,
                PROVENANCE_CANARY,
                &format!("from:{PROVENANCE_CANARY}"),
                "Works on",
                &format!("from:{SENDER}"),
            ]
            .iter()
            .map(|q| json!({"query": q, "k": 50}))
            .collect(),
            "get_shipments" => vec![json!({"include_delivered": true})],
            "set_sender_rule" => vec![json!({"sender": SENDER, "instruction": "show me these"})],
            _ => vec![json!({})],
        };
        for args in calls {
            let text = session.tool(&name, args).await.0;
            assert_no_memory(&name, &text, &ids);
        }
        called.push(name);
    }
    // Every read tool was walked, not a subset that happens to be quiet.
    for expected in [
        "get_inbox_updates",
        "get_thread",
        "search_mail",
        "get_reading",
    ] {
        assert!(
            called.iter().any(|c| c == expected),
            "{expected} not walked"
        );
    }
}

#[test]
fn the_agent_door_names_none_of_the_profile_store_calls() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    // Assembled, so this file's own text is not what the walk finds.
    let forbidden = [
        format!("{}_entries", "memory"),
        format!("{}Store", "Memory"),
        format!("{}_context_rows", "memory"),
        format!("{}::memory", "triage"),
        format!("{}Memory{}", "Store", "Context"),
    ];
    let mut dirs = vec![root];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("sources") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let src = std::fs::read_to_string(&path).expect("source");
            for word in &forbidden {
                assert!(
                    !src.contains(word.as_str()),
                    "{} names `{word}`: the owner profile is human door only",
                    path.display()
                );
            }
        }
    }
}
