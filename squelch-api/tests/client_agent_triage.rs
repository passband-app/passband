//! The embedded assistant uses a distinct read audience from the human reader.
mod common;
use axum::http::StatusCode;
use chrono::Utc;
use common::*;
use squelch_core::{
    store::{
        Store,
        agent_triage::{AgentCommitOutcome, AgentTriageStore},
    },
    triage::decision::*,
};
use tower::ServiceExt;

fn assess(h: &Harness, id: i64, restricted: bool) {
    h.store
        .enqueue_agent_triage(h.acct, id, "test", false)
        .unwrap();
    let job = h
        .store
        .claim_agent_job(h.acct, "triage", Utc::now(), 60)
        .unwrap()
        .unwrap();
    assert_eq!(job.message_id, id);
    let context = h.store.load_agent_context(&job).unwrap();
    let evidence = vec![EvidenceRef {
        message_id: id,
        location: "body".into(),
    }];
    let decision = MessageDecision {
        kinds: vec![EmailKind::Receipt],
        destinations: vec![MessageDestination::Reading],
        summary: "A receipt".into(),
        reason: "Keep the record".into(),
        external_access: AccessAssessment {
            restricted,
            reason: "Access assessment".into(),
            evidence: evidence.clone(),
        },
        attention: ThreadAttentionDecision {
            show_in_fye: true,
            summary: "Review receipt".into(),
            relevant_message_ids: vec![id],
            evidence: evidence.clone(),
            ..Default::default()
        },
        records: vec![RecordProposal::Receipt {
            merchant: "Example".into(),
            amount: Some(12.0),
            currency: Some("USD".into()),
            evidence,
        }],
        notification: NotificationAdvice {
            importance: 30,
            title: "Internal notification title".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(
        h.store
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
async fn human_can_open_pending_auth_but_agent_cannot_read_or_discover_it() {
    let h = harness(|_, _| {});
    let id = h
        .store
        .upsert_message(&msg(
            h.acct,
            "pending",
            "auth-thread",
            "secretcode",
            "Your code is 123456",
        ))
        .unwrap();
    let human = h
        .app
        .clone()
        .oneshot(authed("GET", &format!("/client/v2/messages/{id}")))
        .await
        .unwrap();
    assert_eq!(human.status(), StatusCode::OK);
    for url in [
        "/client/agent/thread/auth-thread".to_owned(),
        format!("/client/agent/triage/{id}"),
    ] {
        let response = h.app.clone().oneshot(authed("GET", &url)).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{url}");
    }
    let search = h
        .app
        .clone()
        .oneshot(authed("GET", "/client/agent/search?q=secretcode"))
        .await
        .unwrap();
    assert_eq!(body_json(search).await["items"], serde_json::json!([]));
    assess(&h, id, true);
    let response = h
        .app
        .clone()
        .oneshot(authed("GET", "/client/agent/thread/auth-thread"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    for destination in ["fye", "reading", "records"] {
        let response = h
            .app
            .clone()
            .oneshot(authed(
                "GET",
                &format!("/client/agent/feed?destination={destination}"),
            ))
            .await
            .unwrap();
        assert_eq!(body_json(response).await["items"], serde_json::json!([]));
    }
    let response = h
        .app
        .oneshot(authed("GET", "/client/agent/records?kind=receipts"))
        .await
        .unwrap();
    assert_eq!(body_json(response).await["receipts"], serde_json::json!([]));
}

#[tokio::test]
async fn allowed_decisions_surface_without_internal_notification_metadata() {
    let h = harness(|_, _| {});
    let id = h
        .store
        .upsert_message(&msg(
            h.acct,
            "receipt",
            "receipt-thread",
            "receipt",
            "Paid twelve dollars",
        ))
        .unwrap();
    assess(&h, id, false);
    let response = h
        .app
        .clone()
        .oneshot(authed("GET", "/client/agent/feed"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body = body_json(response).await;
    assert_eq!(body["items"][0]["message_id"], id);
    assert!(body["items"][0]["decision"].get("notification").is_none());
    let response = h
        .app
        .clone()
        .oneshot(authed("GET", &format!("/client/agent/triage/{id}")))
        .await
        .unwrap();
    let body = body_json(response).await;
    assert!(body["decision"].get("notification").is_none());
    let response = h
        .app
        .clone()
        .oneshot(authed("GET", "/client/agent/records?kind=receipts"))
        .await
        .unwrap();
    assert_eq!(
        body_json(response).await["receipts"][0]["record"]["amount"],
        12.0
    );
    // A new unassessed sibling invalidates the whole external thread context.
    h.store
        .upsert_message(&msg(h.acct, "new-auth", "receipt-thread", "Code", "123456"))
        .unwrap();
    let response = h
        .app
        .oneshot(authed("GET", "/client/agent/feed"))
        .await
        .unwrap();
    assert_eq!(body_json(response).await["items"], serde_json::json!([]));
}

#[tokio::test]
async fn agent_search_fills_results_after_pending_leading_matches() {
    let h = harness(|_, _| {});
    let mut readable = msg(h.acct, "readable", "readable-thread", "zebra", "zebra memo");
    readable.received_at = Utc::now() - chrono::Duration::days(10);
    let id = h.store.upsert_message(&readable).unwrap();
    assess(&h, id, false);
    for i in 0..24 {
        h.store
            .upsert_message(&msg(
                h.acct,
                &format!("pending-{i}"),
                &format!("pending-thread-{i}"),
                "zebra",
                "zebra memo",
            ))
            .unwrap();
    }
    let response = h
        .app
        .oneshot(authed(
            "GET",
            "/client/agent/search?q=zebra&limit=2&mode=keyword",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["items"][0]["id"], id);
}

/// A HEALTHY IDLE MAILBOX READS AS CLEAR. A verdict that asks to look again
/// in three days leaves a queued revisit job behind (revisits are on by
/// default), and `/client/stats` must not count it as an email waiting for
/// triage, or the app says "Sorting your mail." forever.
#[tokio::test]
async fn stats_do_not_count_a_scheduled_revisit_as_waiting() {
    let (state, store, acct) = state_with(|_, _| {});
    let id = store
        .upsert_message(&msg(acct, "g-rv", "t-rv", "Invoice", "Due next week"))
        .unwrap();
    store.enqueue_agent_triage(acct, id, "test", false).unwrap();
    let job = store
        .claim_agent_job(acct, "triage", Utc::now(), 60)
        .unwrap()
        .unwrap();
    let context = store.load_agent_context(&job).unwrap();
    let decision = MessageDecision {
        kinds: vec![EmailKind::Receipt],
        destinations: vec![MessageDestination::Reading],
        summary: "An invoice".into(),
        reason: "Look again when it is due".into(),
        revisit: Some(RevisitRequest {
            at: Utc::now() + chrono::Duration::days(3),
            reason: "due date".into(),
        }),
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
    assert_eq!(
        store.triage_job_counts(acct).unwrap(),
        [1, 0],
        "the fixture left a queued revisit behind"
    );

    let metrics = squelch_core::metrics::SyncMetrics::new();
    metrics.note_poll_ok();
    let app = squelch_api::router(state.with_sync_metrics(metrics));
    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["triage"]["queued"], 0, "{}", json["triage"]);
    assert_eq!(json["triage"]["leased"], 0);
}

/// An archive row through the real archive write: old mail, stored unassessed.
fn old_mail(h: &Harness, gmail: &str, thread: &str, subject: &str) -> i64 {
    use squelch_core::sync::ingest::{RawFetched, ingest};
    let when = Utc::now() - chrono::Duration::days(200);
    let fetched = RawFetched {
        account_id: h.acct,
        gmail_msg_id: gmail.into(),
        gmail_thread_id: Some(thread.into()),
        raw: format!(
            "From: Zed Old <zed@old.example>\r\nTo: me@example.com\r\nSubject: {subject}\r\n\r\n{subject} ledgerbody\r\n"
        )
        .into_bytes(),
        internal_date: Some(when),
        is_sent: false,
        is_spam: false,
        account_addr: "me@example.com".into(),
    };
    let triaged = ingest(&fetched, &Default::default(), Utc::now(), |_| false);
    h.store.ingest_archive_message(&triaged).unwrap().unwrap()
}

async fn get(h: &Harness, uri: &str) -> (StatusCode, serde_json::Value) {
    let response = h.app.clone().oneshot(authed("GET", uri)).await.unwrap();
    let status = response.status();
    (status, body_json(response).await)
}

/// THE ARCHIVE TIER AT THE EMBEDDED AGENT'S ROUTES. An unassessed old
/// ancestor is absent from the thread, the feed and the search of a live
/// conversation; a search that matches old mail has it checked, waits, and
/// returns what was allowed with the horizon it searched.
#[tokio::test]
async fn the_agent_routes_omit_unassessed_archive_mail_and_search_surfaces_it() {
    use squelch_core::store::archive_surface::SurfaceWait;
    use squelch_core::triage::access::AccessDecision;
    let (state, store, acct) = state_with(|_, _| {});
    let poke = std::sync::Arc::new(tokio::sync::Notify::new());
    let state = state
        .with_archive_days(365)
        .with_refine_poke(poke.clone())
        .with_surface_wait(SurfaceWait {
            deadline: std::time::Duration::from_secs(20),
            interval: std::time::Duration::from_millis(10),
        });
    let h = Harness {
        app: squelch_api::router(state),
        store,
        acct,
    };
    let ancestor = old_mail(&h, "g-anc", "mix", "heron ledger");
    let live = h
        .store
        .upsert_message(&msg(
            h.acct,
            "g-live",
            "mix",
            "osprey plans",
            "osprey meeting",
        ))
        .unwrap();
    assess(&h, live, false);
    let no_trace = |what: &str, body: &serde_json::Value| {
        let wire = body.to_string();
        for secret in ["heron", "ledger", "zed@old.example", "Zed Old"] {
            assert!(!wire.contains(secret), "{what} carries {secret}: {wire}");
        }
    };

    let (status, feed) = get(&h, "/client/agent/feed?destination=reading").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(feed["items"].as_array().unwrap().len(), 1, "{feed}");
    no_trace("feed", &feed);
    let (status, found) = get(&h, "/client/agent/search?q=osprey&mode=keyword").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(found["items"].as_array().unwrap().len(), 1);
    assert!(found.get("pending_older_matches").is_none());
    no_trace("search", &found);
    let (status, _) = get(&h, &format!("/client/agent/triage/{ancestor}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, thread) = get(&h, "/client/agent/thread/mix").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(thread["messages"].as_array().unwrap().len(), 1);
    assert_eq!(thread["subject"], "osprey plans");
    no_trace("thread", &thread);
    // The HUMAN reads both messages, so the thread is not one a client may
    // cache as agent-readable while the ancestor is unchecked.
    let (_, human) = get(&h, "/client/thread/mix").await;
    assert_eq!(human["messages"].as_array().unwrap().len(), 2);
    assert_eq!(human["cache_allowed"], false);
    let (_, opened) = get(&h, &format!("/client/v2/messages/{live}")).await;
    assert_eq!(opened["thread"]["messages"].as_array().unwrap().len(), 2);
    assert_eq!(opened["thread"]["cache_allowed"], false);

    // A stand-in refine lane: each poke runs the queued checks, allowing all
    // but the one whose subject says it is a secret.
    for i in 0..3 {
        old_mail(&h, &format!("g-{i}"), &format!("t-{i}"), "heron survey");
    }
    let secret = old_mail(&h, "g-secret", "t-secret", "heron password");
    let (worker_store, heard) = (h.store.clone(), poke.clone());
    tokio::spawn(async move {
        loop {
            heard.notified().await;
            while let Some(job) = worker_store
                .claim_agent_job(acct, "access", Utc::now(), 120)
                .unwrap()
            {
                let original = worker_store.load_agent_access_message(&job).unwrap();
                let decision = AccessDecision {
                    restricted: job.message_id == secret,
                    reason: "test".into(),
                };
                worker_store
                    .commit_agent_access(&job, &original, &decision, &serde_json::json!({}))
                    .unwrap();
            }
        }
    });

    for _ in 0..2 {
        let (status, found) = get(&h, "/client/agent/search?q=heron&mode=keyword").await;
        assert_eq!(status, StatusCode::OK);
        let items = found["items"].as_array().unwrap();
        // The three surveys and the ancestor (its check was queued by the
        // thread read above and ran with the rest); never the restricted one.
        assert_eq!(items.len(), 4, "{found}");
        assert!(items.iter().all(|item| item["id"] != secret));
        assert!(!found.to_string().contains("password"));
        assert!(found.get("pending_older_matches").is_none());
        assert!(found["indexed_since"].is_string());
        assert_eq!(found["archive"]["walk_complete"], false);
        assert_eq!(
            found["archive"]["target"],
            (Utc::now() - chrono::Duration::days(365))
                .format("%Y-%m-%d")
                .to_string()
        );
    }
    for id in [ancestor, secret] {
        assert_eq!(
            h.store.agent_diagnostics(h.acct, id).unwrap()["jobs"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "one check each, however often the search runs"
        );
    }
}

/// With no sync engine attached (no poke) the search queues the check and
/// reports the match as pending instead of waiting; with the archive off it
/// reports no archive horizon.
#[tokio::test]
async fn agent_search_without_a_refine_lane_reports_pending_matches_and_no_archive() {
    let h = harness(|_, _| {});
    let old = old_mail(&h, "g-old", "t-old", "heron ledger");
    for _ in 0..2 {
        let started = std::time::Instant::now();
        let (status, found) = get(&h, "/client/agent/search?q=heron&mode=keyword").await;
        assert_eq!(status, StatusCode::OK);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(found["items"], serde_json::json!([]));
        assert_eq!(found["pending_older_matches"], 1);
        assert!(found.get("archive").is_none(), "{found}");
        assert!(!found.to_string().contains("ledger"));
    }
    let jobs = h.store.agent_diagnostics(h.acct, old).unwrap()["jobs"].clone();
    assert_eq!(jobs.as_array().unwrap().len(), 1, "{jobs}");
}
