//! TWO-DOOR INVARIANT for sender research (docs/SENDER-RESEARCH.md): nothing
//! from `sender_research` reaches any `/mcp` response.
//!
//! What research says about a stranger is untrusted model output, written for
//! the owner to read on the contact card. The agent door was never handed it,
//! for the reason it was never handed the relationship ledger or the owner
//! profile (docs/UNDERSTANDING.md §2.2). Two checks, as there:
//!
//! - AT THE WIRE: a mailbox whose research on a sender carries a canary word
//!   in every field a tool could serialize is read through every tool the
//!   door offers, and no response may carry one. The sender's own mail is the
//!   control, so the test cannot pass on a door that serves nothing.
//! - IN THE SOURCE: the agent door's crate names none of research's store
//!   calls or types.

use std::sync::Arc;

use chrono::Utc;
use serde_json::json;
use squelch_core::store::agent_triage::{AgentCommitOutcome, AgentTriageStore as _};
use squelch_core::store::{SqliteStore, Store as _};
use squelch_core::sync::ingest::{RawFetched, ingest_with_rules};
use squelch_core::triage::decision::MessageDecision;
use squelch_core::triage::research::{
    CheckStatus, ClaimCheck, Organization, ResearchResult, ResearchStatus, Verdict,
};
use squelch_mcp::testing::wire::Session;
use squelch_mcp::{AgentDoorAuth, agent_door_router};
use tokio_util::sync::CancellationToken;

const ACCOUNT: &str = "me@localhost";
const HOST: &str = "localhost";
const SENDER: &str = "pat@lumen.example";
const VISIBLE: &str = "kestrelvisible";
/// One per field of a stored result.
const WHO_CANARY: &str = "shrikewho";
const ORG_CANARY: &str = "bramblingorg";
const SITE_CANARY: &str = "siskinsite";
const WHAT_CANARY: &str = "linnetwhat";
const SIZE_CANARY: &str = "twitesize";
const CLAIM_CANARY: &str = "redpollclaim";
const SOURCE_CANARY: &str = "crossbillsource";

const CANARIES: [&str; 7] = [
    WHO_CANARY,
    ORG_CANARY,
    SITE_CANARY,
    WHAT_CANARY,
    SIZE_CANARY,
    CLAIM_CANARY,
    SOURCE_CANARY,
];

fn seeded_door() -> axum::Router {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let account = store.ensure_account(ACCOUNT).expect("account");
    let now = Utc::now();
    let raw = RawFetched {
        account_id: account,
        gmail_msg_id: "m-visible".into(),
        gmail_thread_id: Some("t-visible".into()),
        raw: format!(
            "From: Pat Doe <{SENDER}>\r\nTo: {ACCOUNT}\r\nSubject: Hello {VISIBLE}\r\nDate: {}\r\n\r\nI enjoyed your talk. {VISIBLE}",
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
            summary: "A note from a stranger".into(),
            ..Default::default()
        };
        decision.attention.show_in_fye = true;
        decision.attention.summary = "A note worth reading".into();
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
    // Research on the very sender the agent can see, in every field.
    assert_eq!(
        store
            .start_sender_research(account, SENDER, now, 90)
            .expect("start"),
        None
    );
    let result = ResearchResult {
        who: format!("Pat Doe, {WHO_CANARY}."),
        organization: Organization {
            name: Some(ORG_CANARY.into()),
            website: Some(format!("https://{SITE_CANARY}.example/")),
            what_it_does: Some(WHAT_CANARY.into()),
            size_hint: Some(SIZE_CANARY.into()),
        },
        checks: vec![ClaimCheck {
            claim: CLAIM_CANARY.into(),
            status: CheckStatus::Contradicted,
        }],
        verdict: Verdict::Suspicious,
        sources: vec![format!("https://lumen.example/{SOURCE_CANARY}")],
    };
    store
        .finish_sender_research(
            account,
            SENDER,
            ResearchStatus::Done,
            Some(&result),
            None,
            2,
            now,
        )
        .expect("finish");
    assert!(
        store
            .sender_research(account, SENDER)
            .expect("read")
            .and_then(|row| row.result)
            .is_some(),
        "control: the research is stored and decodes"
    );
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

fn assert_no_research(tool: &str, text: &str) {
    for word in CANARIES.iter().chain(["suspicious", "contradicted"].iter()) {
        assert!(
            !text.contains(word),
            "{tool} served sender research (`{word}`) to the agent door:\n{text}"
        );
    }
}

#[tokio::test]
async fn no_tool_response_carries_sender_research() {
    let door = seeded_door();
    let mut session = Session::open(door, HOST, None).await;
    let since = "2000-01-01T00:00:00Z";
    let thread = session
        .tool("get_thread", json!({"id": "t-visible"}))
        .await
        .0;
    assert!(thread.contains(VISIBLE), "control: {thread}");

    let mut called = Vec::new();
    for name in session.tool_names().await {
        let calls = match name.as_str() {
            "get_inbox_updates" | "get_reading" => vec![json!({"since": since})],
            "get_thread" => vec![json!({"id": "t-visible"})],
            "search_mail" => [
                VISIBLE,
                WHO_CANARY,
                ORG_CANARY,
                CLAIM_CANARY,
                &format!("from:{SENDER}"),
                "Pat Doe",
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
            assert_no_research(&name, &text);
        }
        called.push(name);
    }
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
fn the_agent_door_names_none_of_the_research_store_calls() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    // Assembled, so this file's own text is not what the walk finds.
    let forbidden = [
        format!("{}_research", "sender"),
        format!("{}::research", "triage"),
        format!("{}Result", "Research"),
        format!("{}_subject_fields", "research"),
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
                    "{} names `{word}`: sender research is human door only",
                    path.display()
                );
            }
        }
    }
}
