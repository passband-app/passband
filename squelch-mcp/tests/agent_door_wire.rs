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

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::json;
use squelch_core::store::agent_triage::{AgentCommitOutcome, AgentTriageStore as _};
use squelch_core::store::{SqliteStore, Store as _};
use squelch_core::sync::ingest::{RawFetched, ingest_with_rules};
use squelch_core::triage::decision::MessageDecision;
use squelch_core::types::AccountId;
use squelch_mcp::testing::wire::{Session, refused};
use squelch_mcp::{AgentDoorAuth, agent_door_router};
use tokio_util::sync::CancellationToken;

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
        Default::default(),
    )
    .expect("router")
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
    let mut session = Session::open(seeded_door(), HOST, None).await;
    assert_eq!(session.tool_names().await, READ_ONLY_TOOLS);

    // A tool that is not offered is not callable either.
    for name in [
        "create_draft",
        "send_email",
        "open_draft",
        "archive",
        "delete",
    ] {
        let (text, reply) = session.tool(name, json!({})).await;
        assert!(refused(&reply), "{name} was callable: {text}");
    }
}

#[tokio::test]
async fn no_read_tool_serves_hidden_mail_over_the_wire() {
    let mut session = Session::open(seeded_door(), HOST, None).await;
    let since = "2000-01-01T00:00:00Z";

    // Control first: the door does serve the allowed row.
    let inbox = session
        .tool("get_inbox_updates", json!({"since": since}))
        .await
        .0;
    assert!(
        inbox.contains(VISIBLE),
        "control: the allowed row must reach the agent:\n{inbox}"
    );
    assert_no_canary("get_inbox_updates", &inbox);

    let reading = session.tool("get_reading", json!({"since": since})).await.0;
    assert_no_canary("get_reading", &reading);

    let visible_thread = session
        .tool("get_thread", json!({"id": format!("t-{VISIBLE}")}))
        .await
        .0;
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
                    .await
                    .0;
                assert_no_canary(&format!("search_mail({query:?}, {order})"), &found);
            }
        }
        let thread = session
            .tool("get_thread", json!({"id": format!("t-{word}")}))
            .await
            .0;
        if word != VISIBLE {
            assert_no_canary(&format!("get_thread(t-{word})"), &thread);
        }
    }

    for (name, args) in [
        ("get_deadlines", json!({})),
        ("get_shipments", json!({"include_delivered": true})),
        ("list_sender_rules", json!({})),
    ] {
        let text = session.tool(name, args).await.0;
        assert_no_canary(name, &text);
    }
}

/// Every field the relationship ledger has (docs/UNDERSTANDING.md §2.1), and
/// the key the triage context carries it under. None of these words appears
/// anywhere in the agent door's code today, so any of them in a response is
/// the ledger leaking.
const LEDGER_FIELDS: [&str; 15] = [
    "relationship",
    "received_per_month",
    "initiated_count",
    "inbound_threads",
    "replied_threads",
    "median_reply",
    "attention_rows",
    "opened_count",
    "median_read_fraction",
    "link_clicks",
    "fatigue",
    "coverage_days",
    "\"replied\"",
    "\"initiated\"",
    "computed_at",
];

/// HUMAN DOOR ONLY (docs/UNDERSTANDING.md §2.2): who the owner answers and
/// how fast is a graph the agent door was never handed. Triage, the human
/// door's own backend, reads the ledger; no `/mcp` tool response may carry
/// a ledger field, for the sender the agent can see most plainly.
#[tokio::test]
async fn no_read_tool_serves_the_relationship_ledger() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let account = store.ensure_account(ACCOUNT).expect("account");
    ingest(&store, account, VISIBLE, false);
    // The owner answered, so the ledger has replies to leak.
    let now: DateTime<Utc> = Utc::now() + chrono::Duration::minutes(5);
    let reply = RawFetched {
        account_id: account,
        gmail_msg_id: "m-reply".into(),
        gmail_thread_id: Some(format!("t-{VISIBLE}")),
        raw: format!(
            "From: {ACCOUNT}\r\nTo: Alerts <alerts@example.com>\r\nSubject: Re: Update {VISIBLE}\r\nDate: {}\r\n\r\nThanks.",
            now.to_rfc2822()
        )
        .into_bytes(),
        internal_date: Some(now),
        is_sent: true,
        is_spam: false,
        account_addr: ACCOUNT.into(),
    };
    store
        .ingest_message(&ingest_with_rules(
            &reply,
            &Default::default(),
            now,
            &[],
            |_| false,
        ))
        .expect("reply");
    // After the reply, which re-queues the thread it lands in.
    assess_all(&store, account, &[], None);
    store
        .recompute_all_relationships(account)
        .expect("recompute");
    store
        .set_sync_state(
            account,
            squelch_core::store::sqlite::relationships::RELATIONSHIPS_BACKFILL_KEY,
            &squelch_core::store::SyncState {
                uidvalidity: 0,
                last_uid: 1,
            },
        )
        .expect("backfilled");
    // Control: the ledger has a row for this sender, and triage is handed it.
    let row = store
        .relationship(account, "alerts@example.com")
        .expect("read")
        .expect("a ledger row");
    assert_eq!(row.replied_threads, 1, "control: {row:?}");
    assert!(
        store
            .relationship_context(account, "alerts@example.com")
            .expect("context")
            .is_some()
    );

    let door = agent_door_router(
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
    let mut session = Session::open(door, HOST, None).await;
    let since = "2000-01-01T00:00:00Z";
    let calls = [
        ("get_inbox_updates", json!({"since": since})),
        ("get_reading", json!({"since": since})),
        ("get_thread", json!({"id": format!("t-{VISIBLE}")})),
        (
            "search_mail",
            json!({"query": "from:alerts@example.com", "k": 50}),
        ),
        ("search_mail", json!({"query": VISIBLE, "k": 50})),
        ("get_deadlines", json!({})),
        ("get_shipments", json!({"include_delivered": true})),
        ("list_sender_rules", json!({})),
    ];
    let mut served_the_sender = false;
    for (name, args) in calls {
        let text = session.tool(name, args).await.0;
        served_the_sender |= text.contains(VISIBLE);
        for field in LEDGER_FIELDS {
            assert!(
                !text.contains(field),
                "{name} served the ledger's `{field}` to the agent door:\n{text}"
            );
        }
    }
    assert!(
        served_the_sender,
        "control: the door must serve the mail itself"
    );
}
