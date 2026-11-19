//! The lapsed-user note end to end: the pass over a real store and a mock
//! Gmail (who gets it, once ever, never under three items, the insert's
//! shape), and the note coming back through ingest, where it is recognised
//! and never triaged. Every sender is invented, at `.example`.
mod common;
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use squelch_api::{ApiState, LapsedOutcome, run_lapsed_pass};
use squelch_core::credentials::{CredentialStore, OAuthToken};
use squelch_core::store::{
    ClientTag, SqliteStore, Store,
    agent_triage::{AgentCommitOutcome, AgentTriageStore},
};
use squelch_core::triage::decision::*;
use squelch_core::types::NewMessage;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct StubCreds;
#[async_trait]
impl CredentialStore for StubCreds {
    async fn token(&self, _account: i64) -> squelch_core::error::Result<OAuthToken> {
        Ok(OAuthToken {
            access_token: "WRITE-TOKEN".into(),
            refresh_token: None,
            expires_at: None,
        })
    }
}

/// A write credential that cannot produce a token: a disconnected account.
struct BrokenCreds;
#[async_trait]
impl CredentialStore for BrokenCreds {
    async fn token(&self, _account: i64) -> squelch_core::error::Result<OAuthToken> {
        Err(squelch_core::error::CoreError::Credential("revoked".into()))
    }
}

/// What the mock Gmail answers.
#[derive(Clone, Default)]
struct Mock {
    /// The status of every insert (POST).
    post: u16,
    /// The status of every labels read (GET) not otherwise configured.
    get: u16,
    /// Message ids the labels read reports in the trash.
    trashed: Vec<&'static str>,
    /// Message ids the labels read answers 404 for.
    missing: Vec<&'static str>,
}

/// A mock Gmail that answers every insert with `status`, reports every
/// message still in the inbox, and records the inserts.
async fn mock_gmail(
    status: u16,
) -> (
    String,
    Arc<std::sync::Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    mock_gmail_with(Mock {
        post: status,
        get: 200,
        ..Default::default()
    })
    .await
}

/// [`mock_gmail`], configured. Only inserts are recorded.
async fn mock_gmail_with(
    mock: Mock,
) -> (
    String,
    Arc<std::sync::Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0u8; 65536];
            let n = sock.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let line = req.lines().next().unwrap_or_default().to_string();
            let (status, body) = if line.starts_with("GET ") {
                let id = line
                    .split("/messages/")
                    .nth(1)
                    .and_then(|r| r.split(['?', ' ']).next())
                    .unwrap_or_default()
                    .to_string();
                if mock.missing.iter().any(|m| *m == id) {
                    (404, "{}".to_string())
                } else if mock.trashed.iter().any(|m| *m == id) {
                    (mock.get, format!(r#"{{"id":"{id}","labelIds":["TRASH"]}}"#))
                } else {
                    (mock.get, format!(r#"{{"id":"{id}","labelIds":["INBOX"]}}"#))
                }
            } else {
                log.lock().unwrap().push(req);
                (
                    mock.post,
                    r#"{"id":"filed-1","threadId":"t-filed"}"#.to_string(),
                )
            };
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            sock.flush().await.unwrap();
        }
    });
    (format!("http://{addr}"), seen, handle)
}

/// The owner's Mac, paired and opened NOW.
fn paired(store: &SqliteStore, acct: i64) {
    let token = store.issue_device_token(acct, "laptop").unwrap();
    store
        .verify_device_token_from(
            &token.token,
            Some(&ClientTag::parse("macos/0.0.21").unwrap()),
        )
        .unwrap()
        .unwrap();
}

/// One receipt from a shop, received at `at`, decided as a receipt that
/// stayed out of For your eyes and Reading.
fn receipt(store: &SqliteStore, acct: i64, n: usize, at: DateTime<Utc>) {
    let message = NewMessage {
        from_addr: format!("orders@shop{n}.example"),
        from_name: Some(format!("Shop {n}")),
        received_at: at,
        ..common::msg(
            acct,
            &format!("g-{n}"),
            &format!("t-{n}"),
            &format!("Your receipt {n}"),
            "Thanks for your order.",
        )
    };
    let id = store.upsert_message(&message).unwrap();
    store.enqueue_agent_triage(acct, id, "test", false).unwrap();
    let job = store
        .claim_agent_job(acct, "triage", Utc::now(), 60)
        .unwrap()
        .unwrap();
    let context = store.load_agent_context(&job).unwrap();
    let decision = MessageDecision {
        kinds: vec![EmailKind::Receipt],
        summary: "A receipt".into(),
        records: vec![RecordProposal::Receipt {
            merchant: format!("Shop {n}"),
            amount: Some(10.0),
            currency: Some("USD".into()),
            evidence: vec![],
        }],
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

/// Twenty days on: the Mac was last open twenty days before this.
fn later() -> DateTime<Utc> {
    Utc::now() + Duration::days(20)
}

fn stamp(store: &SqliteStore, acct: i64) -> Option<String> {
    store.get_app_setting(acct, "lapsed_note").unwrap()
}

fn setup(receipts: usize, gmail: &str) -> (ApiState, Arc<SqliteStore>, i64) {
    let (state, store, acct) = common::state_with(|store, acct| {
        paired(store, acct);
        for n in 0..receipts {
            receipt(store, acct, n, later() - Duration::hours(2 + n as i64));
        }
    });
    let state = state.with_write_test_harness(Arc::new(StubCreds), gmail.to_string());
    (state, store, acct)
}

#[tokio::test]
async fn a_lapsed_owner_gets_one_note_filed_into_their_inbox_once() {
    let (base, seen, _h) = mock_gmail(200).await;
    let (state, store, acct) = setup(4, &base);

    let now = later();
    assert_eq!(
        run_lapsed_pass(&state, now).await.unwrap(),
        LapsedOutcome::Filed(4)
    );
    assert!(stamp(&store, acct).is_some(), "stamped");
    let reqs = seen.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1, "one request: {reqs:?}");
    let req = &reqs[0];
    let line = req.lines().next().unwrap();
    assert!(
        line.starts_with("POST /messages "),
        "an insert, not a send: {line}"
    );
    assert!(req.contains(r#""labelIds":["INBOX","UNREAD"]"#), "{req}");

    // The raw message: the template, newest first, From Passband at the
    // owner's own address.
    let body = &req[req.find("\r\n\r\n").unwrap() + 4..];
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    use base64::Engine as _;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(v["raw"].as_str().unwrap())
        .unwrap();
    let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
    assert_eq!(
        parsed.subject(),
        Some("You shouldn't have had to open these")
    );
    let from = parsed.from().unwrap().first().unwrap();
    assert_eq!(from.name(), Some("Passband"));
    assert_eq!(from.address(), Some("no-reply@passband.app"));
    let to = parsed.to().unwrap().first().unwrap();
    assert_eq!(to.address(), Some("me@example.com"));
    let text = parsed.body_text(0).unwrap();
    let first = text.find("Your receipt 0").unwrap();
    let last = text.find("Your receipt 3").unwrap();
    assert!(first < last, "newest first: {text}");
    assert!(text.contains("Shop 0 \u{b7} Receipt, "), "{text}");
    assert!(text.contains("Open Passband: https://passband.app/download"));

    // Never again, whatever happens next.
    assert_eq!(
        run_lapsed_pass(&state, now + Duration::days(30))
            .await
            .unwrap(),
        LapsedOutcome::AlreadyDone
    );
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn an_owner_who_never_paired_gets_nothing() {
    let (base, seen, _h) = mock_gmail(200).await;
    let (state, store, acct) = common::state_with(|store, acct| {
        for n in 0..4 {
            receipt(store, acct, n, later() - Duration::hours(1));
        }
    });
    let state = state.with_write_test_harness(Arc::new(StubCreds), base);
    assert_eq!(
        run_lapsed_pass(&state, later()).await.unwrap(),
        LapsedOutcome::NeverPaired
    );
    assert!(stamp(&store, acct).is_none());
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_owner_still_using_the_app_gets_nothing() {
    let (base, seen, _h) = mock_gmail(200).await;
    let (state, store, acct) = setup(4, &base);
    // Two days after the Mac was last open.
    assert_eq!(
        run_lapsed_pass(&state, Utc::now() + Duration::days(2))
            .await
            .unwrap(),
        LapsedOutcome::Active
    );
    assert!(stamp(&store, acct).is_none());
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn under_three_is_skipped_and_not_stamped() {
    let (base, seen, _h) = mock_gmail(200).await;
    let (state, store, acct) = setup(2, &base);
    assert_eq!(
        run_lapsed_pass(&state, later()).await.unwrap(),
        LapsedOutcome::TooFew(2)
    );
    assert!(stamp(&store, acct).is_none(), "a later check may still try");
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_disconnected_account_is_skipped_and_not_stamped() {
    let (base, seen, _h) = mock_gmail(200).await;
    let (state, store, acct) = setup(4, &base);
    let state = state.with_write_test_harness(Arc::new(BrokenCreds), base);
    assert_eq!(
        run_lapsed_pass(&state, later()).await.unwrap(),
        LapsedOutcome::NoCredential
    );
    assert!(stamp(&store, acct).is_none());
    assert!(seen.lock().unwrap().is_empty());
}

/// Google refusing the insert spends the one chance: at most once.
#[tokio::test]
async fn a_refused_insert_stays_stamped() {
    let (base, seen, _h) = mock_gmail(400).await;
    let (state, store, acct) = setup(4, &base);
    assert_eq!(
        run_lapsed_pass(&state, later()).await.unwrap(),
        LapsedOutcome::Failed
    );
    assert!(stamp(&store, acct).is_some());
    assert_eq!(
        run_lapsed_pass(&state, later()).await.unwrap(),
        LapsedOutcome::AlreadyDone
    );
    assert_eq!(seen.lock().unwrap().len(), 1);
}

/// Mail deleted, trashed or in spam since triage saw it is never listed:
/// the store never hears of a deletion, so Gmail is asked first.
#[tokio::test]
async fn mail_gone_from_gmail_is_not_listed() {
    let (base, seen, _h) = mock_gmail_with(Mock {
        post: 200,
        get: 200,
        trashed: vec!["g-0"],
        missing: vec!["g-1"],
    })
    .await;
    let (state, _store, _acct) = setup(5, &base);
    assert_eq!(
        run_lapsed_pass(&state, later()).await.unwrap(),
        LapsedOutcome::Filed(3)
    );
    let req = seen.lock().unwrap()[0].clone();
    let body = &req[req.find("\r\n\r\n").unwrap() + 4..];
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    use base64::Engine as _;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(v["raw"].as_str().unwrap())
        .unwrap();
    let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
    let text = parsed.body_text(0).unwrap();
    assert!(!text.contains("Your receipt 0") && !text.contains("Your receipt 1"));
    for n in 2..5 {
        assert!(text.contains(&format!("Your receipt {n}")), "{text}");
    }
}

/// Too few left once the gone ones are dropped: nothing filed or stamped.
#[tokio::test]
async fn gone_mail_can_leave_too_few() {
    let (base, seen, _h) = mock_gmail_with(Mock {
        post: 200,
        get: 200,
        trashed: vec!["g-0"],
        missing: vec![],
    })
    .await;
    let (state, store, acct) = setup(3, &base);
    assert_eq!(
        run_lapsed_pass(&state, later()).await.unwrap(),
        LapsedOutcome::TooFew(2)
    );
    assert!(stamp(&store, acct).is_none());
    assert!(seen.lock().unwrap().is_empty());
}

/// A check that cannot finish files nothing and stamps nothing.
#[tokio::test]
async fn a_failed_mailbox_check_files_nothing() {
    let (base, seen, _h) = mock_gmail_with(Mock {
        post: 200,
        get: 500,
        ..Default::default()
    })
    .await;
    let (state, store, acct) = setup(4, &base);
    assert_eq!(
        run_lapsed_pass(&state, later()).await.unwrap(),
        LapsedOutcome::CheckFailed
    );
    assert!(stamp(&store, acct).is_none());
    assert!(seen.lock().unwrap().is_empty());
}

/// Refusals that prove nothing was filed lift the stamp; a 5xx keeps it.
#[tokio::test]
async fn an_insert_that_never_reached_google_is_retried_later() {
    for (status, outcome, stamped) in [
        (401, LapsedOutcome::Deferred, false),
        (429, LapsedOutcome::Deferred, false),
        (500, LapsedOutcome::Failed, true),
    ] {
        let (base, _seen, _h) = mock_gmail(status).await;
        let (state, store, acct) = setup(4, &base);
        assert_eq!(
            run_lapsed_pass(&state, later()).await.unwrap(),
            outcome,
            "{status}"
        );
        assert_eq!(stamp(&store, acct).is_some(), stamped, "{status}");
    }
}

/// Somebody who uses Passband only through an assistant: the Mac idle for
/// twenty days, the agent door used yesterday. Not lapsed.
#[tokio::test]
async fn an_owner_using_the_agent_door_gets_nothing() {
    let (base, seen, _h) = mock_gmail(200).await;
    let (state, store, acct) = setup(4, &base);
    store
        .note_agent_door_use(acct, later() - Duration::days(1))
        .unwrap();
    assert_eq!(
        run_lapsed_pass(&state, later()).await.unwrap(),
        LapsedOutcome::Active
    );
    assert!(stamp(&store, acct).is_none());
    assert!(seen.lock().unwrap().is_empty());
}

/// The note, synced back from Gmail as a fresh arrival, is recognised: stored
/// like an archive row with no triage or notification job, so no placement in
/// For your eyes or Reading, no push, and never a line in another note.
/// THE CONTROL: the same bytes after crossing a mail server (a `Received`
/// line and Gmail's `Authentication-Results` prepended, as any stranger's
/// mail carrying the header would have) are triaged like any other mail.
#[tokio::test]
async fn the_filed_note_is_never_triaged_but_a_delivered_copy_is() {
    let (base, seen, _h) = mock_gmail(200).await;
    let (state, store, acct) = setup(3, &base);
    assert_eq!(
        run_lapsed_pass(&state, later()).await.unwrap(),
        LapsedOutcome::Filed(3)
    );
    let req = seen.lock().unwrap()[0].clone();
    let body = &req[req.find("\r\n\r\n").unwrap() + 4..];
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    use base64::Engine as _;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(v["raw"].as_str().unwrap())
        .unwrap();

    let ingest = |gmail: &str, raw: Vec<u8>| {
        let fetched = squelch_core::sync::ingest::RawFetched {
            account_id: acct,
            gmail_msg_id: gmail.into(),
            gmail_thread_id: Some(format!("t-{gmail}")),
            raw,
            internal_date: Some(later()),
            is_sent: false,
            is_spam: false,
            account_addr: "me@example.com".into(),
        };
        let mut triaged = squelch_core::sync::ingest::ingest(
            &fetched,
            &squelch_core::config::Stage1Config::default(),
            later(),
            |_| false,
        );
        // As the poll lane stamps a fresh arrival.
        triaged.foreground_triage = true;
        triaged.notify_eligible_at = Some(Utc::now());
        let noted = triaged.passband_note;
        (store.ingest_message(&triaged).unwrap(), noted)
    };
    let due = Utc::now() + Duration::days(1);

    let (note, noted) = ingest("filed-1", raw.clone());
    assert!(noted, "the filed bytes are recognised");
    for kind in ["triage", "notification", "access"] {
        assert!(
            store
                .claim_agent_job(acct, kind, due, 60)
                .unwrap()
                .is_none(),
            "no {kind} job for the note"
        );
    }
    // A later sighting (a catch-up re-read) is the same row, still no job.
    assert_eq!(ingest("filed-1", raw.clone()).0, note);
    assert!(
        store
            .claim_agent_job(acct, "triage", due, 60)
            .unwrap()
            .is_none()
    );

    // THE CONTROL.
    let delivered = [
        "Received: by 2002:a05:6400:1:b0:1:1 with SMTP id x;\r\n        Tue, 17 Nov 2026 10:00:00 -0800\r\n",
        "Authentication-Results: mx.google.com;\r\n       spf=pass smtp.mailfrom=passband.app\r\n",
    ]
    .concat()
    .into_bytes()
    .into_iter()
    .chain(raw)
    .collect::<Vec<u8>>();
    let (other, noted) = ingest("delivered-1", delivered);
    assert!(!noted, "mail that crossed a server is never the filed note");
    let job = store
        .claim_agent_job(acct, "triage", due, 60)
        .unwrap()
        .expect("a delivered copy is triaged like any mail");
    assert_eq!(job.message_id, other);
}
