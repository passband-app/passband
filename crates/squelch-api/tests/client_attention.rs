//! Contract tests for reading attention (`POST
//! /client/v2/messages/{id}/attention`). What a report stored is read back
//! through the store's per-sender statistics, which smart clear scores from.
mod common;
use axum::http::StatusCode;
use chrono::{Duration, Utc};
use common::*;
use serde_json::{Value, json};
use squelch_core::store::{
    SqliteStore, Store,
    agent_triage::{AgentCommitOutcome, AgentListQuery, AgentTriageStore},
};
use squelch_core::triage::decision::*;
use squelch_core::triage::sender_fatigue::SenderSignals;
use squelch_core::types::{NewMessage, SealedKind, Sensitivity, Tier};
use tower::ServiceExt;

const NEWS: &str = "news@letters.example";

/// A message from `from`, `days_ago` old, in its own thread.
fn from(acct: i64, key: &str, from: &str, days_ago: i64) -> NewMessage {
    NewMessage {
        from_addr: from.to_string(),
        from_name: Some("The Letter".to_string()),
        received_at: Utc::now() - Duration::days(days_ago),
        ..msg(acct, key, key, "An issue", "Short and sweet.")
    }
}

/// Store a message with the lifecycle row ingest always writes beside it.
fn seed(store: &SqliteStore, message: &NewMessage, sealed: bool) -> i64 {
    let id = store.upsert_message(message).unwrap();
    store
        .set_triage(
            id,
            message.account_id,
            0,
            Tier::Noise,
            if sealed {
                Sensitivity::Sealed
            } else {
                Sensitivity::Normal
            },
            sealed.then_some(SealedKind::Otp),
            "",
            "",
            None,
        )
        .unwrap();
    id
}

/// Commit a model decision placing `id` in Reading, the way the worker does.
fn into_reading(store: &SqliteStore, account: i64, id: i64) {
    store
        .enqueue_agent_triage(account, id, "test", false)
        .unwrap();
    let job = store
        .claim_agent_job(account, "triage", Utc::now(), 60)
        .unwrap()
        .unwrap();
    assert_eq!(job.message_id, id);
    let context = store.load_agent_context(&job).unwrap();
    let decision = MessageDecision {
        kinds: vec![EmailKind::Promotional],
        destinations: vec![MessageDestination::Reading],
        summary: "An issue".into(),
        reason: "A newsletter".into(),
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

async fn post(h: &Harness, uri: &str, body: Value) -> StatusCode {
    h.app
        .clone()
        .oneshot(authed_json("POST", uri, body))
        .await
        .unwrap()
        .status()
}

/// One sender's statistics as the store reads them for an account, with the
/// Reading feed's defaults.
fn signals(store: &SqliteStore, account: i64, addr: &str) -> SenderSignals {
    store
        .reading_senders(account, Utc::now(), &AgentListQuery::default(), 200)
        .unwrap()
        .senders
        .into_iter()
        .find(|s| s.sender == addr)
        .unwrap_or_else(|| panic!("{addr} missing"))
        .signals
}

#[tokio::test]
async fn attention_reports_add_up_and_are_clamped() {
    let h = harness(|_, _| {});
    let open = seed(&h.store, &from(h.acct, "open", NEWS, 1), false);
    into_reading(&h.store, h.acct, open);
    let read = seed(&h.store, &from(h.acct, "read", NEWS, 2), false);
    let uri = format!("/client/v2/messages/{read}/attention");
    // A ledger older than the mail, so the totals are visible in the read.
    h.store
        .set_app_setting(
            h.acct,
            "opened_ledger_since",
            &(Utc::now() - Duration::days(400)).to_rfc3339(),
        )
        .unwrap();

    // Opened, then two reports. Either field may be left out.
    assert_eq!(
        post(&h, &format!("/client/v2/messages/{read}/opened"), json!({})).await,
        StatusCode::OK
    );
    let response = h
        .app
        .clone()
        .oneshot(authed_json(
            "POST",
            &uri,
            json!({"dwell_ms": 1500, "link_clicks": 1}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(body_json(response).await, Value::Null);
    assert_eq!(
        post(&h, &uri, json!({"dwell_ms": 2500})).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        post(&h, &uri, json!({"link_clicks": 2})).await,
        StatusCode::NO_CONTENT
    );
    // An empty report is accepted and changes nothing.
    assert_eq!(post(&h, &uri, json!({})).await, StatusCode::NO_CONTENT);
    // A malformed one is refused the ordinary way.
    assert_eq!(
        post(&h, &uri, json!({"dwell_ms": -5})).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let s = signals(&h.store, h.acct, NEWS);
    assert_eq!(s.dwell_sample, 1);
    assert_eq!(s.median_dwell_ms, Some(4000));
    assert_eq!(s.clicks, 3);

    // One absurd report adds ten minutes and twenty clicks, no more.
    assert_eq!(
        post(
            &h,
            &uri,
            json!({"dwell_ms": 86_400_000u64, "link_clicks": 4_000_000})
        )
        .await,
        StatusCode::NO_CONTENT
    );
    let s = signals(&h.store, h.acct, NEWS);
    assert_eq!(s.median_dwell_ms, Some(604_000));
    assert_eq!(s.clicks, 23);

    // And the total stops at an hour and a thousand clicks.
    for _ in 0..60 {
        post(&h, &uri, json!({"dwell_ms": 600_000, "link_clicks": 20})).await;
    }
    let s = signals(&h.store, h.acct, NEWS);
    assert_eq!(s.median_dwell_ms, Some(3_600_000));
    assert_eq!(s.clicks, 1000);
}

/// A client reads 404 from this route as "the daemon predates it" and stops
/// reporting for the session, so an id the daemon does not know, another
/// account's id and a sealed id all answer 204, and none of them is written.
#[tokio::test]
async fn attention_answers_204_for_unknown_and_sealed_ids_and_writes_nothing() {
    let h = harness(|_, _| {});
    let since = (Utc::now() - Duration::days(400)).to_rfc3339();
    h.store
        .set_app_setting(h.acct, "opened_ledger_since", &since)
        .unwrap();
    let other = h.store.ensure_account("other@example.com").unwrap();
    h.store
        .set_app_setting(other, "opened_ledger_since", &since)
        .unwrap();
    // Each account has a sender with one open Reading mail and one opened
    // mail, which is where a stored report would show.
    let open_theirs = seed(&h.store, &from(other, "t-open", NEWS, 1), false);
    into_reading(&h.store, other, open_theirs);
    let theirs = seed(&h.store, &from(other, "theirs", NEWS, 2), false);
    h.store
        .acknowledge_agent_message(other, theirs, Utc::now())
        .unwrap();
    let open = seed(&h.store, &from(h.acct, "open", NEWS, 1), false);
    into_reading(&h.store, h.acct, open);
    // A sealed row, opened (the human door may open sealed mail).
    let sealed = seed(&h.store, &from(h.acct, "code", NEWS, 2), true);
    h.store
        .acknowledge_agent_message(h.acct, sealed, Utc::now())
        .unwrap();
    let report = json!({"dwell_ms": 5000, "link_clicks": 1});

    for id in [9_999, theirs, sealed] {
        let response = h
            .app
            .clone()
            .oneshot(authed_json(
                "POST",
                &format!("/client/v2/messages/{id}/attention"),
                report.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "message {id}");
        assert_eq!(body_json(response).await, Value::Null, "message {id}");
    }

    // Nothing landed on the other account's message: its own read still sees
    // an open with no report behind it.
    let theirs_now = signals(&h.store, other, NEWS);
    assert_eq!(theirs_now.opened, 1);
    assert_eq!(theirs_now.dwell_sample, 0);
    // Unsealed afterwards, the sealed row shows that nothing was stored on it
    // while it was sealed.
    h.store
        .set_triage(
            sealed,
            h.acct,
            0,
            Tier::Noise,
            Sensitivity::Normal,
            None,
            "",
            "",
            None,
        )
        .unwrap();
    let s = signals(&h.store, h.acct, NEWS);
    assert_eq!(s.opened, 1, "the unsealed row is counted now");
    assert_eq!(s.dwell_sample, 0, "and carries no report");
    assert_eq!(s.clicks, 0);

    // Human door: no bearer, no write.
    let response = h
        .app
        .clone()
        .oneshot(json_request(
            "POST",
            &format!("/client/v2/messages/{sealed}/attention"),
            &report,
            false,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
