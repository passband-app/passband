//! Contract tests for the cutover: model-owned destinations and exact human reads.
mod common;
use axum::http::StatusCode;
use chrono::Utc;
use common::*;
use squelch_core::store::{
    Store,
    agent_triage::{AgentCommitOutcome, AgentTriageStore},
};
use squelch_core::triage::decision::*;
use tower::ServiceExt;

fn assess(store: &squelch_core::store::SqliteStore, account: i64, id: i64, restricted: bool) {
    store
        .enqueue_agent_triage(account, id, "test", false)
        .unwrap();
    let job = store
        .claim_agent_job(account, "triage", Utc::now(), 60)
        .unwrap()
        .unwrap();
    assert_eq!(job.message_id, id);
    let context = store.load_agent_context(&job).unwrap();
    let evidence = vec![EvidenceRef {
        message_id: id,
        location: "body".into(),
    }];
    let decision = MessageDecision {
        kinds: vec![EmailKind::Promotional],
        destinations: vec![MessageDestination::Reading],
        summary: "An offer worth reading".into(),
        reason: "The user can curate promotions".into(),
        auth: if restricted {
            AuthAssessment {
                kinds: vec![AuthKind::Otp],
                evidence: evidence.clone(),
            }
        } else {
            AuthAssessment::default()
        },
        records: vec![squelch_core::triage::decision::RecordProposal::Receipt {
            merchant: "Shop".into(),
            amount: Some(10.0),
            currency: Some("USD".into()),
            evidence: evidence.clone(),
        }],
        external_access: AccessAssessment {
            restricted,
            reason: "Assessment".into(),
            evidence: evidence.clone(),
        },
        attention: ThreadAttentionDecision {
            show_in_fye: true,
            relevant_message_ids: vec![id],
            evidence,
            summary: "Review the offer".into(),
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

#[tokio::test]
async fn pending_exact_message_opens_and_acknowledges_without_waiting_for_triage() {
    let h = harness(|_, _| {});
    let id = h
        .store
        .upsert_message(&msg(
            h.acct,
            "pending",
            "thread",
            "Sign in",
            "Your code is 123456",
        ))
        .unwrap();
    // Ingest always creates the lifecycle row before exposing the message.
    h.store
        .set_triage(
            id,
            h.acct,
            0,
            squelch_core::types::Tier::Noise,
            squelch_core::types::Sensitivity::Normal,
            None,
            "",
            "pending agent triage",
            None,
        )
        .unwrap();
    let response = h
        .app
        .clone()
        .oneshot(authed("GET", &format!("/client/v2/messages/{id}")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body = body_json(response).await;
    assert_eq!(body["message_id"], id);
    assert_eq!(
        body["thread"]["messages"][0]["content"],
        "Your code is 123456"
    );
    assert_eq!(body["thread"]["cache_allowed"], false);
    assert!(!h.store.agent_access_allowed(h.acct, id).unwrap());
    let response = h
        .app
        .oneshot(authed("POST", &format!("/client/v2/messages/{id}/opened")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        h.store
            .agent_read_message(h.acct, id)
            .unwrap()
            .opened_at
            .is_some()
    );
}

#[tokio::test]
async fn destinations_overlap_and_correction_changes_only_requested_membership() {
    let h = harness(|_, _| {});
    let id = h
        .store
        .upsert_message(&msg(h.acct, "offer", "offers", "A sale", "An offer"))
        .unwrap();
    assess(&h.store, h.acct, id, false);
    for destination in ["fye", "reading", "records"] {
        let response = h
            .app
            .clone()
            .oneshot(authed(
                "GET",
                &format!("/client/v2/feed?destination={destination}"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["total_count"], 1, "{destination}");
        assert_eq!(body["items"][0]["message_id"], id);
    }
    let response = h
        .app
        .clone()
        .oneshot(authed_json(
            "POST",
            &format!("/client/v2/messages/{id}/corrections"),
            serde_json::json!({"field":"destinations", "value":[]}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = h
        .app
        .clone()
        .oneshot(authed("GET", "/client/v2/feed?destination=reading"))
        .await
        .unwrap();
    assert_eq!(body_json(response).await["total_count"], 0);
    let response = h
        .app
        .oneshot(authed("GET", "/client/v2/feed?destination=records"))
        .await
        .unwrap();
    assert_eq!(body_json(response).await["total_count"], 1);
}

#[tokio::test]
async fn exact_reads_are_account_scoped_even_before_assessment() {
    let h = harness(|_, _| {});
    let other = h.store.ensure_account("other@example.com").unwrap();
    let id = h
        .store
        .upsert_message(&msg(other, "other", "private", "Private", "Other account"))
        .unwrap();
    for suffix in ["", "/opened"] {
        let method = if suffix.is_empty() { "GET" } else { "POST" };
        let response = h
            .app
            .clone()
            .oneshot(authed(method, &format!("/client/v2/messages/{id}{suffix}")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn auth_lookup_uses_the_model_assessment_without_hiding_human_mail() {
    let h = harness(|_, _| {});
    let id = h
        .store
        .upsert_message(&msg(
            h.acct,
            "auth",
            "auth-thread",
            "Sign in",
            "Code 123456",
        ))
        .unwrap();
    assess(&h.store, h.acct, id, true);
    let response = h
        .app
        .clone()
        .oneshot(authed("GET", "/client/sealed"))
        .await
        .unwrap();
    let body = body_json(response).await;
    assert_eq!(body[0]["id"], id);
    assert_eq!(body[0]["kind"], "otp");
    let response = h
        .app
        .clone()
        .oneshot(authed("POST", &format!("/client/sealed/{id}/reveal")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(body_json(response).await["body"], "Code 123456");
    assert!(!h.store.agent_access_allowed(h.acct, id).unwrap());
    let response = h
        .app
        .oneshot(authed("GET", &format!("/client/v2/messages/{id}")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn correction_deltas_preserve_other_destinations_and_reject_ambiguous_payloads() {
    let h = harness(|_, _| {});
    let id = h
        .store
        .upsert_message(&msg(h.acct, "delta", "delta-thread", "Offer", "Details"))
        .unwrap();
    assess(&h.store, h.acct, id, false);
    let route = format!("/client/v2/messages/{id}/corrections");
    let response = h
        .app
        .clone()
        .oneshot(authed_json(
            "POST",
            &route,
            serde_json::json!({"field":"destinations","remove":["reading"]}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let current = h
        .store
        .agent_thread_context(h.acct, "delta-thread")
        .unwrap()
        .previous_decision
        .unwrap();
    assert!(current.destinations.is_empty());
    let response = h
        .app
        .clone()
        .oneshot(authed_json(
            "POST",
            &route,
            serde_json::json!({"field":"destinations","add":["reading"]}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        h.store
            .agent_thread_context(h.acct, "delta-thread")
            .unwrap()
            .previous_decision
            .unwrap()
            .destinations
            .len(),
        1
    );
    for invalid in [
        serde_json::json!({"field":"destinations","add":["records"]}),
        serde_json::json!({"field":"destinations","add":["reading"],"value":[]}),
        serde_json::json!({"field":"kinds","add":["not_a_kind"]}),
    ] {
        assert_eq!(
            h.app
                .clone()
                .oneshot(authed_json("POST", &route, invalid))
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn reading_and_records_support_explicit_history_and_done_filters() {
    let h = harness(|_, _| {});
    let recent = h
        .store
        .upsert_message(&msg(
            h.acct,
            "recent-inventory",
            "recent-inventory",
            "Recent",
            "Body",
        ))
        .unwrap();
    assess(&h.store, h.acct, recent, false);
    h.store
        .set_triage(
            recent,
            h.acct,
            0,
            squelch_core::types::Tier::Noise,
            squelch_core::types::Sensitivity::Normal,
            None,
            "",
            "",
            None,
        )
        .unwrap();
    h.store
        .set_attention_status(h.acct, recent, squelch_core::types::AttentionStatus::Done)
        .unwrap();
    let mut old = msg(h.acct, "old-inventory", "old-inventory", "Old", "Body");
    old.received_at = Utc::now() - chrono::Duration::days(40);
    let id = h.store.upsert_message(&old).unwrap();
    assess(&h.store, h.acct, id, false);
    for destination in ["reading", "records"] {
        for (suffix, count) in [
            ("", 0),
            ("&all_time=true", 1),
            ("&all_time=true&include_done=true", 2),
            ("&include_done=true", 1),
        ] {
            let response = h
                .app
                .clone()
                .oneshot(authed(
                    "GET",
                    &format!("/client/v2/feed?destination={destination}{suffix}"),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(body_json(response).await["total_count"], count);
        }
    }
    let response = h
        .app
        .oneshot(authed(
            "GET",
            "/client/v2/feed?destination=records&all_time=true&since=2020-01-01T00:00:00Z",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// A model mock that answers every request with `headline` and counts the
/// requests it saw, so a test can tell a cache hit from a second call.
async fn headline_mock(
    headline: &'static str,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = hits.clone();
    let body = format!(
        r#"{{"content":[{{"type":"text","text":"{{\"headline\":\"{headline}\"}}"}}],"stop_reason":"end_turn","usage":{{"input_tokens":300,"output_tokens":12}}}}"#
    );
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    (format!("http://{addr}"), hits)
}

fn with_headline(state: squelch_api::ApiState, url: String) -> axum::Router {
    use squelch_core::config::Stage2Provider;
    use squelch_core::triage::fye_headline::HeadlineClient;
    squelch_api::router(state.with_fye_headline(Some(HeadlineClient::new(
        reqwest::Client::new(),
        url,
        "sk-test",
        Stage2Provider::Anthropic,
        "claude-haiku-4-5",
        None,
    ))))
}

const HEADLINE_URI: &str = "/client/v2/fye/headline?today=2026-10-01%20(Thursday)";

#[tokio::test]
async fn fye_headline_is_written_once_per_board_and_billed_to_its_own_category() {
    let (url, hits) = headline_mock("Mostly one offer worth a look.").await;
    let (state, store, acct) = state_with(|_, _| {});
    let id = store
        .upsert_message(&msg(acct, "offer", "offers", "A sale", "An offer"))
        .unwrap();
    assess(&store, acct, id, false);
    let app = with_headline(state, url);

    for _ in 0..2 {
        let resp = app
            .clone()
            .oneshot(authed("GET", HEADLINE_URI))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["cache-control"], "no-store");
        assert_eq!(
            body_json(resp).await["headline"],
            "Mostly one offer worth a look."
        );
    }
    // The second ask was about the same board: served from the cache.
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    let categories: Vec<String> = store
        .list_usage_by_category(acct, 30)
        .unwrap()
        .into_iter()
        .map(|(c, _)| c)
        .collect();
    assert_eq!(categories, vec!["fye_headline".to_string()]);

    // A new day is a new board even with the same rows.
    let resp = app
        .oneshot(authed(
            "GET",
            "/client/v2/fye/headline?today=2026-10-02%20(Friday)",
        ))
        .await
        .unwrap();
    assert_eq!(
        body_json(resp).await["headline"],
        "Mostly one offer worth a look."
    );
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn fye_headline_is_null_without_a_model_or_on_an_empty_board() {
    // No model on the default harness, rows present.
    let h = harness(|_, _| {});
    let id = h
        .store
        .upsert_message(&msg(h.acct, "offer", "offers", "A sale", "An offer"))
        .unwrap();
    assess(&h.store, h.acct, id, false);
    let resp = h.app.oneshot(authed("GET", HEADLINE_URI)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["headline"], serde_json::Value::Null);

    // A model, but nothing in the band: no call, no spend.
    let (url, hits) = headline_mock("Something.").await;
    let (state, store, acct) = state_with(|_, _| {});
    let app = with_headline(state, url);
    let resp = app.oneshot(authed("GET", HEADLINE_URI)).await.unwrap();
    assert_eq!(body_json(resp).await["headline"], serde_json::Value::Null);
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(store.list_usage_by_category(acct, 30).unwrap().is_empty());
}
