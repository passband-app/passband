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

/// What the mock answers a parent-headers read with: a message whose
/// Reply-To and Cc name people its From does not. An agent is shown only the
/// From, so neither may end up on an agent's reply.
const PARENT_HEADERS: &str = r#"{"payload":{"headers":[
{"name":"From","value":"Alice <alice@example.com>"},
{"name":"Reply-To","value":"attacker@evil.test"},
{"name":"To","value":"me@example.com"},
{"name":"Cc","value":"carol@example.com"},
{"name":"Message-ID","value":"<parent@example.com>"}]}}"#;

/// Answer every request until dropped, recording what arrived: a send with
/// `{}`, anything else (the parent-headers read) with [`PARENT_HEADERS`]. A
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
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = if request.contains("/messages/send") {
                "{}"
            } else {
                PARENT_HEADERS
            };
            seen.lock().unwrap().push(request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.flush().await;
        }
    });
    (format!("http://{addr}"), log)
}

/// Give `id` a current access assessment, so the agent door may (or, when
/// `restricted`, may not) read it.
fn assess(store: &SqliteStore, acct: i64, id: i64, restricted: bool) {
    store.enqueue_agent_triage(acct, id, "test", false).unwrap();
    // Ordinary mail queues a `triage` job; provider spam, which is never
    // triaged, queues an `access` one.
    let job = ["triage", "access"]
        .into_iter()
        .find_map(|kind| store.claim_agent_job(acct, kind, Utc::now(), 60).unwrap())
        .expect("a queued assessment job");
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

/// The RFC822 message a recorded `messages.send` request carried.
fn sent_mail(request: &str) -> String {
    use base64::Engine as _;
    let json = &request[request.find("\r\n\r\n").expect("a body") + 4..];
    let value: serde_json::Value = serde_json::from_str(json).expect("json body");
    let raw = value["raw"].as_str().expect("raw");
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw.trim_end_matches('='))
        .expect("base64url");
    String::from_utf8_lossy(&bytes).to_string()
}

fn audit_details(f: &Fixture) -> Vec<(String, String, String)> {
    f.store
        .list_audit(f.acct, 50)
        .unwrap()
        .into_iter()
        .map(|r| (r.actor, r.action, r.detail.unwrap_or_default()))
        .collect()
}

// --- the setting -------------------------------------------------------------

/// A mailbox that never opened the setting: an agent may leave drafts, and
/// may not send. Sending is only ever something the owner chose.
#[tokio::test]
async fn a_mailbox_that_never_opened_the_setting_allows_drafts_and_no_sending() {
    let (state, store, acct) = state_with(|_, _| {});
    assert!(
        Store::get_app_setting(store.as_ref(), acct, AGENT_WRITE_ACCESS_KEY)
            .unwrap()
            .is_none()
    );
    let mailer = state.agent_mailer();

    let created = mailer.create_draft(compose("hi")).await.unwrap();
    assert!(
        store
            .draft_by_id(acct, created.draft_id)
            .unwrap()
            .unwrap()
            .from_agent
    );
    assert_eq!(
        mailer.send(compose("hi")).await,
        Err(AgentWriteError::NotAllowed(AgentWriteAccess::Send))
    );
    let audit = store.list_audit(acct, 10).unwrap();
    assert_eq!(audit[0].actor, "agent");
    assert_eq!(audit[0].action, "send");
    assert_eq!(
        audit[0].detail.as_deref(),
        Some("rejected:agent_not_allowed")
    );
}

#[tokio::test]
async fn read_only_allows_an_agent_nothing() {
    let (state, store, acct) = state_with(|_, _| {});
    AgentWriteAccess::None.save(store.as_ref(), acct).unwrap();
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

/// The agent's view of a message carries its From and nothing else, and the
/// human approves what the agent shows them. So a reply with no stated `to`
/// goes to the From: not to a Reply-To nobody saw, and with nobody copied
/// from the parent's headers. The owner's own reply honors both; this one
/// must not.
#[tokio::test]
async fn an_agent_reply_goes_to_the_from_it_was_shown_not_a_hidden_reply_to() {
    let f = fixture(AgentWriteAccess::Send, true).await;
    let sent = f
        .mailer
        .send(reply(f.readable, "Works for me."))
        .await
        .unwrap();
    assert_eq!(
        sent.to, "alice@example.com",
        "the result says where it went"
    );
    assert_eq!(sent.cc, "");

    let mail = sent_mail(&requests(&f)[1]);
    assert!(mail.contains("To: alice@example.com"), "{mail}");
    assert!(!mail.contains("evil.test"), "{mail}");
    assert!(!mail.contains("carol@example.com"), "{mail}");
    // Still threaded onto the parent.
    assert!(mail.contains("In-Reply-To: <parent@example.com>"), "{mail}");
}

/// Provider spam is assessed like other mail but the agent door never serves
/// it, so it is not something an agent can reply to. "Readable" here is the
/// door's own thread read, not a list of its rules.
#[tokio::test]
async fn spam_is_not_a_reply_target() {
    let f = fixture(AgentWriteAccess::Send, true).await;
    let spam = f
        .store
        .upsert_message(&squelch_core::types::NewMessage {
            is_spam: true,
            ..msg(f.acct, "g-spam", "t-spam", "WIN A PRIZE", "click here")
        })
        .unwrap();
    assess(&f.store, f.acct, spam, false);

    assert_eq!(
        f.mailer.create_draft(reply(spam, "hello")).await,
        Err(AgentWriteError::NotFound)
    );
    assert_eq!(
        f.mailer.send(reply(spam, "hello")).await,
        Err(AgentWriteError::NotFound)
    );
    assert!(f.store.list_drafts(f.acct).unwrap().is_empty());
    assert!(requests(&f).is_empty(), "nothing may reach Gmail");
}

#[tokio::test]
async fn every_refusal_leaves_a_ledger_row() {
    let f = fixture(AgentWriteAccess::Send, true).await;
    let _ = f.mailer.send(reply(f.restricted, "hello")).await;
    let _ = f.mailer.create_draft(reply(f.restricted, "hello")).await;
    let _ = f.mailer.send(compose("   ")).await;
    let _ = f.mailer.create_draft(compose("\u{200B}")).await;
    let _ = f
        .mailer
        .send(AgentCompose {
            body: "hello".into(),
            ..Default::default()
        })
        .await;
    let rows = audit_details(&f);
    assert_eq!(rows.len(), 5, "{rows:?}");
    assert!(
        rows.iter().all(|(actor, _, _)| actor == "agent"),
        "{rows:?}"
    );
    let details: Vec<&str> = rows.iter().map(|(_, _, d)| d.as_str()).collect();
    assert_eq!(details.iter().filter(|d| **d == "failed:target").count(), 2);
    assert_eq!(
        details.iter().filter(|d| **d == "rejected:invalid").count(),
        2
    );
    assert!(details.contains(&"rejected:no_recipient"));
}

#[tokio::test]
async fn the_guard_reads_everything_the_agent_wrote_not_only_the_body() {
    let f = fixture(AgentWriteAccess::Send, true).await;
    let key = "sk-abcDEF0123456789ghijKLMNopq";
    let in_subject = AgentCompose {
        subject: Some(format!("the key is {key}")),
        ..compose("as promised")
    };
    let in_display_name = AgentCompose {
        to: Some(format!("\"{key}\" <bob@example.com>")),
        ..compose("as promised")
    };
    let split_by_markdown = compose("here: sk-abc**DEF0123456789**ghijKLMNopq");
    for attempt in [in_subject, in_display_name, split_by_markdown] {
        let refused = f.mailer.send(attempt.clone()).await;
        assert!(
            matches!(refused, Err(AgentWriteError::Blocked(_))),
            "{attempt:?} -> {refused:?}"
        );
    }
    assert!(requests(&f).is_empty(), "nothing may reach Gmail");
}

#[tokio::test]
async fn a_new_message_needs_a_subject_to_send() {
    let f = fixture(AgentWriteAccess::Send, true).await;
    let no_subject = AgentCompose {
        subject: None,
        ..compose("hello")
    };
    assert!(matches!(
        f.mailer.send(no_subject).await,
        Err(AgentWriteError::Invalid(_))
    ));
    assert!(requests(&f).is_empty(), "nothing may reach Gmail");
}

#[tokio::test]
async fn header_fields_take_no_control_characters_and_a_bounded_audience() {
    let f = fixture(AgentWriteAccess::Send, true).await;
    for bad in [
        "Hello\u{2028}Bcc: eve@example.com",
        "Hello\u{0}",
        "Hello\u{85}x",
    ] {
        let smuggled = AgentCompose {
            subject: Some(bad.into()),
            ..compose("hi")
        };
        assert!(
            matches!(
                f.mailer.send(smuggled).await,
                Err(AgentWriteError::Invalid(_))
            ),
            "{bad:?}"
        );
    }
    let crowd = (0..60)
        .map(|i| format!("p{i}@example.com"))
        .collect::<Vec<_>>()
        .join(", ");
    let blast = AgentCompose {
        bcc: Some(crowd),
        ..compose("hi")
    };
    assert!(matches!(
        f.mailer.send(blast).await,
        Err(AgentWriteError::Invalid(_))
    ));
    assert!(requests(&f).is_empty(), "nothing may reach Gmail");
}

#[tokio::test]
async fn agent_drafts_are_capped_until_the_owner_clears_some() {
    let f = fixture(AgentWriteAccess::Drafts, false).await;
    let mut last = 0;
    for i in 0..25 {
        last = f
            .mailer
            .create_draft(compose(&format!("draft {i}")))
            .await
            .unwrap()
            .draft_id;
    }
    let refused = f.mailer.create_draft(compose("one too many")).await;
    assert!(
        matches!(refused, Err(AgentWriteError::Blocked(_))),
        "{refused:?}"
    );
    assert_eq!(f.store.list_drafts(f.acct).unwrap().len(), 25);
    // The owner's own drafts do not count against the agent, and clearing
    // one agent draft makes room for one.
    f.store.delete_draft(f.acct, last).unwrap();
    assert!(f.mailer.create_draft(compose("room again")).await.is_ok());
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
}

// --- opening a draft on the Mac ------------------------------------------------

/// Read SSE frames off a `/client/events?opens=1` response until one names a
/// draft to open, or the wait runs out.
async fn next_open_frame(resp: axum::response::Response) -> Option<String> {
    use http_body_util::BodyExt;
    let mut body = resp.into_body();
    let mut seen = String::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let frame = tokio::time::timeout_at(deadline, body.frame())
            .await
            .ok()??;
        let Ok(frame) = frame else { return None };
        if let Some(data) = frame.data_ref() {
            seen.push_str(&String::from_utf8_lossy(data));
            if seen.contains("open_draft") && seen.ends_with("\n\n") {
                return Some(seen);
            }
        }
    }
}

#[tokio::test]
async fn open_draft_reaches_a_listening_mac_and_says_so() {
    let f = fixture(AgentWriteAccess::Drafts, false).await;
    let created = f.mailer.create_draft(compose("hello")).await.unwrap();
    // The link names the draft, its mailbox, and the second it was created,
    // which is what ties it to this draft and not the next one to get the id.
    let stored = f
        .store
        .draft_by_id(f.acct, created.draft_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        created.link,
        format!(
            "passband://draft/{}?mailbox=me%40example.com&t={}",
            created.draft_id,
            stored.created_at.timestamp()
        )
    );

    // Nothing listening: saved, not shown, and the answer says which.
    let unheard = f.mailer.open_draft(created.draft_id).await.unwrap();
    assert!(!unheard.on_mac);
    assert_eq!(unheard.link, created.link);

    // A Mac app connects and asks for open requests.
    let resp = f
        .app
        .clone()
        .oneshot(authed("GET", "/client/events?opens=1"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let listening = tokio::spawn(next_open_frame(resp));
    // The 5s spacing is for the owner's screen, not for this test.
    tokio::time::sleep(std::time::Duration::from_millis(5100)).await;

    let heard = f.mailer.open_draft(created.draft_id).await.unwrap();
    assert!(heard.on_mac);
    let frame = listening.await.unwrap().expect("an open_draft frame");
    assert!(frame.contains("event: open_draft"), "{frame}");
    assert!(
        frame.contains(&format!("{{\"open_draft\":{},\"at\":", created.draft_id)),
        "{frame}"
    );
    // Not a row in the notification log: it carries no cursor.
    assert!(
        !frame.contains("\nid:") && !frame.starts_with("id:"),
        "{frame}"
    );

    let rows = audit_details(&f);
    assert!(rows.contains(&("agent".into(), "draft.open".into(), "ok:no_mac".into())));
    assert!(rows.contains(&("agent".into(), "draft.open".into(), "ok:mac".into())));
}

/// A connection that did not ask (an older Mac app, the phone) is not "a Mac
/// listening", and is never sent the frame.
#[tokio::test]
async fn a_stream_that_did_not_ask_is_not_a_listening_mac() {
    let f = fixture(AgentWriteAccess::Drafts, false).await;
    let created = f.mailer.create_draft(compose("hello")).await.unwrap();
    let resp = f
        .app
        .clone()
        .oneshot(authed("GET", "/client/events"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let opened = f.mailer.open_draft(created.draft_id).await.unwrap();
    assert!(!opened.on_mac);
    drop(resp);

    // Nor is a spelling of the opt-in this build does not know a broken feed.
    let resp = f
        .app
        .clone()
        .oneshot(authed("GET", "/client/events?opens=true"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn an_agent_can_only_open_a_draft_an_agent_wrote() {
    let f = fixture(AgentWriteAccess::Drafts, false).await;
    let own = f
        .store
        .upsert_draft(
            f.acct,
            DraftKey::NewCreate,
            DraftFields {
                body: "the owner's own",
                ..Default::default()
            },
            Utc::now(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        f.mailer.open_draft(own.id).await,
        Err(AgentWriteError::NotFound)
    );
    assert!(audit_details(&f).contains(&(
        "agent".into(),
        "draft.open".into(),
        "failed:target".into()
    )));
}

#[tokio::test]
async fn open_draft_follows_the_owners_setting() {
    let f = fixture(AgentWriteAccess::Drafts, false).await;
    let created = f.mailer.create_draft(compose("hello")).await.unwrap();
    AgentWriteAccess::None
        .save(f.store.as_ref(), f.acct)
        .unwrap();
    assert_eq!(
        f.mailer.open_draft(created.draft_id).await,
        Err(AgentWriteError::NotAllowed(AgentWriteAccess::Drafts))
    );
}

/// Inside the interval every request is turned away alike, whatever it
/// names: a live draft, the owner's, or nothing. Otherwise the refusal itself
/// would tell an agent whether the owner had sent or discarded a draft. And a
/// second mailer over the same state shares the clock.
#[tokio::test]
async fn open_requests_are_spaced_out_and_the_refusal_says_nothing() {
    let (state, store, acct) = state_with(|_, _| {});
    AgentWriteAccess::Drafts.save(store.as_ref(), acct).unwrap();
    let mailer = state.agent_mailer();
    let other = state.agent_mailer();
    let created = mailer.create_draft(compose("hello")).await.unwrap();
    assert!(mailer.open_draft(created.draft_id).await.is_ok());

    let before = store.list_audit(acct, 50).unwrap().len();
    for (who, id) in [
        (&mailer, created.draft_id),
        (&mailer, 999_999),
        (&other, created.draft_id),
    ] {
        let again = who.open_draft(id).await;
        assert!(
            matches!(again, Err(AgentWriteError::Blocked(_))),
            "{id}: {again:?}"
        );
    }
    assert_eq!(
        store.list_audit(acct, 50).unwrap().len(),
        before,
        "a request inside the interval writes no ledger row"
    );
}

// --- the archive tier (issue #422) -------------------------------------------

/// An archive row through the real archive write: old mail, stored unassessed.
fn old_mail(f: &Fixture, gmail: &str, thread: &str) -> i64 {
    use squelch_core::sync::ingest::{RawFetched, ingest};
    let when = Utc::now() - chrono::Duration::days(200);
    let fetched = RawFetched {
        account_id: f.acct,
        gmail_msg_id: gmail.into(),
        gmail_thread_id: Some(thread.into()),
        raw: b"From: Zed Old <zed@old.example>\r\nTo: me@example.com\r\nSubject: heron ledger\r\n\r\nheron ledger\r\n"
            .to_vec(),
        internal_date: Some(when),
        is_sent: false,
        is_spam: false,
        account_addr: "me@example.com".into(),
    };
    let triaged = ingest(&fetched, &Default::default(), Utc::now(), |_| false);
    f.store.ingest_archive_message(&triaged).unwrap().unwrap()
}

/// Every job on `id`, as (kind, trigger, state).
fn job_rows(f: &Fixture, id: i64) -> Vec<(String, String, String)> {
    f.store.agent_diagnostics(f.acct, id).unwrap()["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|job| {
            let field = |name: &str| job[name].as_str().unwrap().to_string();
            (field("kind"), field("trigger"), field("state"))
        })
        .collect()
}

/// S5. A REPLY-TARGETED WRITE BUYS NOTHING AND PROMOTES NOTHING. `create_draft`
/// and `send_email` check their reply target by performing the agent door's
/// own thread read (`readable_parent`), and that read used to queue an access
/// check on every unassessed archive sibling it passed, outside every cap: a
/// write tool pointed at old mail was a way to spend. Driven here through the
/// REAL mailer the daemon hands the agent door, not a recording stand-in.
#[tokio::test]
async fn a_reply_targeted_write_queues_no_work_on_archive_mail_and_promotes_none() {
    use squelch_core::triage::access::AccessDecision;
    let f = fixture(AgentWriteAccess::Send, true).await;
    // Unassessed and alone; unassessed beside a live readable message; and
    // one an access check has already cleared.
    let lone = old_mail(&f, "g-lone", "t-lone");
    let ancestor = old_mail(&f, "g-anc", "t-mixed");
    let live = f
        .store
        .upsert_message(&msg(f.acct, "g-live", "t-mixed", "Re: heron", "osprey"))
        .unwrap();
    assess(&f.store, f.acct, live, false);
    let cleared = old_mail(&f, "g-cleared", "t-cleared");
    assert_eq!(
        f.store
            .request_archive_access(f.acct, &[cleared], Default::default(), Utc::now())
            .unwrap(),
        vec![cleared]
    );
    let job = f
        .store
        .claim_agent_job(f.acct, "access", Utc::now(), 120)
        .unwrap()
        .unwrap();
    let original = f.store.load_agent_access_message(&job).unwrap();
    let allowed = AccessDecision {
        restricted: false,
        reason: "test".into(),
    };
    f.store
        .commit_agent_access(&job, &original, &allowed, &serde_json::json!({}))
        .unwrap();
    let cleared_jobs = vec![(
        "access".to_string(),
        "archive_access".to_string(),
        "completed".to_string(),
    )];
    assert_eq!(job_rows(&f, cleared), cleared_jobs);

    for round in 0..2 {
        // An archive row nobody has cleared is not a reply target, exactly
        // as mail that does not exist is not.
        for id in [lone, ancestor] {
            assert_eq!(
                f.mailer.create_draft(reply(id, "hello")).await,
                Err(AgentWriteError::NotFound),
                "round {round}"
            );
            assert_eq!(
                f.mailer.send(reply(id, "hello")).await,
                Err(AgentWriteError::NotFound),
                "round {round}"
            );
        }
        // THE READ REALLY RAN: the live message beside the unassessed
        // ancestor and the cleared archive row are both served by it.
        f.mailer.send(reply(live, "Works for me.")).await.unwrap();
        f.mailer.send(reply(cleared, "Found it.")).await.unwrap();
        if round == 0 {
            f.mailer
                .create_draft(reply(live, "A draft."))
                .await
                .unwrap();
            f.mailer
                .create_draft(reply(cleared, "A draft."))
                .await
                .unwrap();
        }

        for id in [lone, ancestor, cleared] {
            assert!(
                f.store.message_is_archive(f.acct, id).unwrap(),
                "round {round}: {id} was promoted from the agent door"
            );
        }
        // Not "no triage job": NO JOB AT ALL on the rows nobody cleared, and
        // on the cleared one the single check it already had.
        assert_eq!(job_rows(&f, lone), vec![], "round {round}");
        assert_eq!(job_rows(&f, ancestor), vec![], "round {round}");
        assert_eq!(job_rows(&f, cleared), cleared_jobs, "round {round}");
    }
    let sends = requests(&f)
        .iter()
        .filter(|request| request.contains("/messages/send"))
        .count();
    assert_eq!(sends, 4, "two readable targets, two rounds");
    assert_eq!(f.store.list_drafts(f.acct).unwrap().len(), 2);
}
