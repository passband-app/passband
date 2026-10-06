//! Agent writes (issue #394): the owner's setting, and the mailer the agent
//! door is handed. Driven through `ApiState::agent_mailer`, which is exactly
//! what `squelchd serve` gives the agent door.
mod common;

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::StatusCode;
use chrono::Utc;
use common::*;
use squelch_api::router;
use squelch_core::agent_write::{
    AGENT_WRITE_ACCESS_KEY, AgentCompose, AgentMailer, AgentWriteAccess, AgentWriteError,
};
use squelch_core::credentials::{CredentialStore, OAuthToken};
use squelch_core::store::agent_triage::{AgentCommitOutcome, AgentTriageStore};
use squelch_core::store::{DraftFields, DraftKey, SqliteStore, Store};
use squelch_core::triage::decision::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tower::ServiceExt;

struct StubCreds;
#[async_trait]
impl CredentialStore for StubCreds {
    async fn token(&self, _a: i64) -> squelch_core::Result<OAuthToken> {
        Ok(OAuthToken {
            access_token: "WRITE-TOKEN".into(),
            refresh_token: None,
            expires_at: None,
        })
    }
}

/// Answer every request `200 {}` until dropped, recording what arrived. A
/// test that expects NO request reads an empty log rather than hanging on one
/// that never comes.
async fn mock_gmail() -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0u8; 16384];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            seen.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buf[..n]).to_string());
            let _ = sock
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                      Content-Length: 2\r\nConnection: close\r\n\r\n{}",
                )
                .await;
            let _ = sock.flush().await;
        }
    });
    (format!("http://{addr}"), log)
}

/// Give `id` a current access assessment, so the agent door may (or, when
/// `restricted`, may not) read it.
fn assess(store: &SqliteStore, acct: i64, id: i64, restricted: bool) {
    store.enqueue_agent_triage(acct, id, "test", false).unwrap();
    let job = store
        .claim_agent_job(acct, "triage", Utc::now(), 60)
        .unwrap()
        .unwrap();
    assert_eq!(job.message_id, id);
    let context = store.load_agent_context(&job).unwrap();
    let evidence = vec![EvidenceRef {
        message_id: id,
        location: "body".into(),
    }];
    let decision = MessageDecision {
        kinds: vec![EmailKind::Receipt],
        destinations: vec![MessageDestination::Reading],
        summary: "A note".into(),
        reason: "Keep".into(),
        external_access: AccessAssessment {
            restricted,
            reason: "Access assessment".into(),
            evidence: evidence.clone(),
        },
        attention: ThreadAttentionDecision {
            summary: "Note".into(),
            relevant_message_ids: vec![id],
            evidence,
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(
        store
            .commit_agent_decision(
                &job,
                &context,
                &decision,
                std::slice::from_ref(&context.message.source)
            )
            .unwrap(),
        AgentCommitOutcome::Applied
    );
}

struct Fixture {
    store: Arc<SqliteStore>,
    acct: i64,
    mailer: Arc<dyn AgentMailer>,
    app: axum::Router,
    gmail: Arc<std::sync::Mutex<Vec<String>>>,
    /// A message from Alice the agent may read.
    readable: i64,
    /// A message the agent may NOT read.
    restricted: i64,
}

async fn fixture(access: AgentWriteAccess, with_write_credential: bool) -> Fixture {
    let (base, gmail) = mock_gmail().await;
    let (state, store, acct) = state_with(|_, _| {});
    let readable = store
        .upsert_message(&msg(acct, "g-open", "t-open", "Lunch?", "Tuesday at noon?"))
        .unwrap();
    assess(&store, acct, readable, false);
    let restricted = store
        .upsert_message(&msg(acct, "g-code", "t-code", "Your code", "123456"))
        .unwrap();
    assess(&store, acct, restricted, true);
    access.save(store.as_ref(), acct).unwrap();
    let state = if with_write_credential {
        state.with_write_test_harness(Arc::new(StubCreds), base)
    } else {
        state
    };
    Fixture {
        mailer: state.agent_mailer(),
        app: router(state),
        store,
        acct,
        gmail,
        readable,
        restricted,
    }
}

fn compose(body: &str) -> AgentCompose {
    AgentCompose {
        to: Some("bob@example.com".into()),
        subject: Some("Hello".into()),
        body: body.into(),
        ..Default::default()
    }
}

fn reply(to_message: i64, body: &str) -> AgentCompose {
    AgentCompose {
        reply_to_message_id: Some(to_message),
        body: body.into(),
        ..Default::default()
    }
}

fn requests(f: &Fixture) -> Vec<String> {
    f.gmail.lock().unwrap().clone()
}

// --- the setting -------------------------------------------------------------

#[tokio::test]
async fn a_mailbox_that_never_opened_the_setting_allows_an_agent_nothing() {
    let (state, store, acct) = state_with(|_, _| {});
    assert!(
        Store::get_app_setting(store.as_ref(), acct, AGENT_WRITE_ACCESS_KEY)
            .unwrap()
            .is_none()
    );
    let mailer = state.agent_mailer();

    assert_eq!(
        mailer.create_draft(compose("hi")).await,
        Err(AgentWriteError::NotAllowed(AgentWriteAccess::Drafts))
    );
    assert_eq!(
        mailer.send(compose("hi")).await,
        Err(AgentWriteError::NotAllowed(AgentWriteAccess::Send))
    );
    assert!(store.list_drafts(acct).unwrap().is_empty());

    // Both refusals are in the ledger, attributed to the agent.
    let audit = store.list_audit(acct, 10).unwrap();
    assert_eq!(audit.len(), 2);
    for row in &audit {
        assert_eq!(row.actor, "agent");
        assert_eq!(row.detail.as_deref(), Some("rejected:agent_not_allowed"));
    }
}

#[tokio::test]
async fn the_owner_reads_and_sets_the_level_on_the_human_door() {
    let f = fixture(AgentWriteAccess::None, true).await;

    let resp = f
        .app
        .clone()
        .oneshot(authed("GET", "/client/settings/agent-access"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["write_access"], "none");
    assert_eq!(body["sending_available"], true);

    let resp = f
        .app
        .clone()
        .oneshot(authed_json(
            "PUT",
            "/client/settings/agent-access",
            serde_json::json!({ "write_access": "send" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["write_access"], "send");
    assert_eq!(
        AgentWriteAccess::load(f.store.as_ref(), f.acct).unwrap(),
        AgentWriteAccess::Send
    );
    // The change is in the ledger as the owner's act, with both ends.
    let audit = f.store.list_audit(f.acct, 10).unwrap();
    assert_eq!(audit[0].action, "agent.write_access");
    assert_eq!(audit[0].actor, "client-api");
    assert_eq!(audit[0].detail.as_deref(), Some("none->send"));

    // A value this build does not know is refused, and changes nothing.
    let resp = f
        .app
        .clone()
        .oneshot(authed_json(
            "PUT",
            "/client/settings/agent-access",
            serde_json::json!({ "write_access": "everything" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        AgentWriteAccess::load(f.store.as_ref(), f.acct).unwrap(),
        AgentWriteAccess::Send
    );
}

#[tokio::test]
async fn the_setting_route_needs_the_bearer() {
    let f = fixture(AgentWriteAccess::None, false).await;
    let resp = f
        .app
        .clone()
        .oneshot(json_request(
            "PUT",
            "/client/settings/agent-access",
            &serde_json::json!({ "write_access": "send" }),
            false,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        AgentWriteAccess::load(f.store.as_ref(), f.acct).unwrap(),
        AgentWriteAccess::None
    );
}

#[tokio::test]
async fn turning_the_setting_off_stops_the_next_call() {
    let f = fixture(AgentWriteAccess::Drafts, false).await;
    assert!(f.mailer.create_draft(compose("first")).await.is_ok());
    AgentWriteAccess::None
        .save(f.store.as_ref(), f.acct)
        .unwrap();
    assert_eq!(
        f.mailer.create_draft(compose("second")).await,
        Err(AgentWriteError::NotAllowed(AgentWriteAccess::Drafts))
    );
    assert_eq!(f.store.list_drafts(f.acct).unwrap().len(), 1);
}

// --- drafts ------------------------------------------------------------------

#[tokio::test]
async fn drafts_level_leaves_a_marked_draft_and_cannot_send() {
    let f = fixture(AgentWriteAccess::Drafts, true).await;

    let created = f
        .mailer
        .create_draft(compose("See you **there**."))
        .await
        .unwrap();
    let draft = f
        .store
        .draft_by_id(f.acct, created.draft_id)
        .unwrap()
        .unwrap();
    assert!(draft.from_agent);
    assert_eq!(draft.to_addr, "bob@example.com");
    assert_eq!(draft.subject, "Hello");
    assert_eq!(draft.body, "See you **there**.");

    // The owner sees it among their drafts, marked.
    let resp = f
        .app
        .clone()
        .oneshot(authed("GET", "/client/drafts"))
        .await
        .unwrap();
    let listed = body_json(resp).await;
    let rows = listed
        .as_array()
        .or_else(|| listed["drafts"].as_array())
        .expect("a draft list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["from_agent"], true);

    // Ledger: ids only, never the text.
    let audit = f.store.list_audit(f.acct, 10).unwrap();
    assert_eq!(audit[0].actor, "agent");
    assert_eq!(audit[0].action, "draft.create");
    assert_eq!(
        audit[0].detail.as_deref(),
        Some(format!("ok:{}", created.draft_id).as_str())
    );

    // Drafts is not send.
    assert_eq!(
        f.mailer.send(compose("now really")).await,
        Err(AgentWriteError::NotAllowed(AgentWriteAccess::Send))
    );
    assert!(requests(&f).is_empty(), "nothing may reach Gmail");
}

#[tokio::test]
async fn a_reply_draft_is_addressed_like_the_composer_would() {
    let f = fixture(AgentWriteAccess::Drafts, false).await;
    let created = f
        .mailer
        .create_draft(reply(f.readable, "Works for me."))
        .await
        .unwrap();
    let draft = f
        .store
        .draft_by_id(f.acct, created.draft_id)
        .unwrap()
        .unwrap();
    assert_eq!(draft.reply_to_message_id, Some(f.readable));
    assert_eq!(draft.to_addr, "alice@example.com");
    assert_eq!(draft.subject, "Re: Lunch?");
}

#[tokio::test]
async fn an_agent_never_overwrites_the_owners_own_reply_draft() {
    let f = fixture(AgentWriteAccess::Send, false).await;
    f.store
        .upsert_draft(
            f.acct,
            DraftKey::Reply(f.readable),
            DraftFields {
                to_addr: "alice@example.com",
                body: "my own half-written thought",
                ..Default::default()
            },
            Utc::now(),
        )
        .unwrap();

    let refused = f
        .mailer
        .create_draft(reply(f.readable, "agent words"))
        .await;
    assert!(
        matches!(refused, Err(AgentWriteError::Blocked(_))),
        "{refused:?}"
    );
    let drafts = f.store.list_drafts(f.acct).unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].body, "my own half-written thought");
    assert!(!drafts[0].from_agent);
}

#[tokio::test]
async fn replying_to_mail_the_agent_cannot_read_is_the_same_not_found_as_no_mail() {
    let f = fixture(AgentWriteAccess::Send, true).await;
    for id in [f.restricted, 999_999] {
        assert_eq!(
            f.mailer.create_draft(reply(id, "hello")).await,
            Err(AgentWriteError::NotFound)
        );
        assert_eq!(
            f.mailer.send(reply(id, "hello")).await,
            Err(AgentWriteError::NotFound)
        );
    }
    assert!(f.store.list_drafts(f.acct).unwrap().is_empty());
    assert!(requests(&f).is_empty(), "nothing may reach Gmail");
}

// --- sending -----------------------------------------------------------------

#[tokio::test]
async fn send_level_sends_through_the_pipeline_as_the_agent() {
    let f = fixture(AgentWriteAccess::Send, true).await;
    f.mailer.send(compose("See you there.")).await.unwrap();

    let reqs = requests(&f);
    assert_eq!(reqs.len(), 1, "one Gmail call: the send");
    assert!(reqs[0].contains("/messages/send"), "{}", reqs[0]);
    assert!(reqs[0].contains("Bearer WRITE-TOKEN"));

    let audit = f.store.list_audit(f.acct, 10).unwrap();
    let sent = audit
        .iter()
        .find(|row| row.action == "send")
        .expect("a send row");
    assert_eq!(sent.actor, "agent", "the ledger names who sent it");
    assert_eq!(sent.detail.as_deref(), Some("ok"));
    assert!(
        audit.iter().all(|row| row.actor == "agent"),
        "every row the pipeline wrote for this send is the agent's: {audit:?}"
    );
}

#[tokio::test]
async fn a_reply_send_reads_the_parent_then_sends() {
    let f = fixture(AgentWriteAccess::Send, true).await;
    f.mailer
        .send(reply(f.readable, "Works for me."))
        .await
        .unwrap();
    let reqs = requests(&f);
    assert_eq!(reqs.len(), 2, "parent headers, then the send");
    assert!(reqs[0].contains("/messages/g-open"));
    assert!(reqs[1].contains("/messages/send"));
}

#[tokio::test]
async fn an_agent_cannot_get_a_secret_past_the_outbound_guard() {
    let f = fixture(AgentWriteAccess::Send, true).await;
    let refused = f
        .mailer
        .send(compose(
            "here is the key sk-abcDEF0123456789ghijKLMNopq you asked for",
        ))
        .await;
    let Err(AgentWriteError::Blocked(message)) = refused else {
        panic!("expected the guard to block: {refused:?}");
    };
    assert!(message.contains("api_key"));
    assert!(
        !message.contains("sk-abc"),
        "the refusal never echoes the secret"
    );
    assert!(requests(&f).is_empty(), "nothing may reach Gmail");

    let audit = f.store.list_audit(f.acct, 10).unwrap();
    assert_eq!(audit[0].actor, "agent");
    assert_eq!(audit[0].detail.as_deref(), Some("blocked:guard"));
}

#[tokio::test]
async fn a_new_message_needs_a_recipient_and_a_body() {
    let f = fixture(AgentWriteAccess::Send, true).await;
    let no_to = AgentCompose {
        body: "hello".into(),
        ..Default::default()
    };
    assert!(matches!(
        f.mailer.send(no_to).await,
        Err(AgentWriteError::Invalid(_))
    ));
    assert!(matches!(
        f.mailer.send(compose("   ")).await,
        Err(AgentWriteError::Invalid(_))
    ));
    let smuggled = AgentCompose {
        subject: Some("Hello\r\nBcc: eve@example.com".into()),
        ..compose("hi")
    };
    assert!(matches!(
        f.mailer.send(smuggled).await,
        Err(AgentWriteError::Invalid(_))
    ));
    assert!(requests(&f).is_empty(), "nothing may reach Gmail");
}

#[tokio::test]
async fn send_level_without_a_write_credential_says_so() {
    let f = fixture(AgentWriteAccess::Send, false).await;
    let refused = f.mailer.send(compose("hello")).await;
    assert!(
        matches!(refused, Err(AgentWriteError::Unavailable(_))),
        "{refused:?}"
    );
    let resp = f
        .app
        .clone()
        .oneshot(authed("GET", "/client/settings/agent-access"))
        .await
        .unwrap();
    assert_eq!(body_json(resp).await["sending_available"], false);
}
