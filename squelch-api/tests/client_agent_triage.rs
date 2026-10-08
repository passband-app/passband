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
    // Not runnable yet: no lane has said it can spend, so nothing waits.
    let poke = squelch_core::store::archive_surface::RefinePoke::new();
    let state = state
        .with_archive_days(365)
        .with_refine_poke(poke.clone())
        .with_surface_wait(SurfaceWait {
            deadline: std::time::Duration::from_secs(20),
            idle: std::time::Duration::from_secs(20),
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
    poke.set_runnable(true);
    tokio::spawn(async move {
        loop {
            heard.poked().await;
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
        // THE WHOLE SHAPE: the items and a horizon. No count, no flag.
        let mut keys: Vec<&String> = found.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            ["archive", "indexed_since", "items", "match_kind", "sort"]
        );
        assert_eq!(found["indexed_since"].as_str().unwrap().len(), 10, "a date");
        assert_eq!(
            found["archive"],
            serde_json::json!({
                "target": (Utc::now() - chrono::Duration::days(365))
                    .format("%Y-%m-%d")
                    .to_string(),
                "walk_complete": false,
                "older_mail": "checked_on_first_match",
            })
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

/// B1. With no sync engine attached the search queues the check and SAYS
/// NOTHING: a query that matched unassessed archive mail gets the same bytes
/// back as one that matched nothing, in every mode, phrase and operator.
#[tokio::test]
async fn agent_search_says_nothing_about_unassessed_archive_matches() {
    let (state, store, acct) = state_with(|_, _| {});
    let h = Harness {
        app: squelch_api::router(state.with_archive_days(365)),
        store,
        acct,
    };
    let mut old = Vec::new();
    for i in 0..15 {
        old.push(old_mail(
            &h,
            &format!("g-{i}"),
            &format!("t-{i}"),
            "heron ledger",
        ));
    }
    let (_, nothing) = get(&h, "/client/agent/search?q=zzyzx&mode=keyword").await;
    assert_eq!(
        nothing,
        serde_json::json!({
            "items": [],
            "match_kind": "keyword",
            "sort": "recent",
            "archive": {
                "target": (Utc::now() - chrono::Duration::days(365))
                    .format("%Y-%m-%d")
                    .to_string(),
                "walk_complete": false,
                "older_mail": "checked_on_first_match",
            },
        })
    );
    // Word, phrase, sender operator, date operator, sender alone; twice each,
    // so the queued and the beyond-the-cap states are both covered.
    for q in [
        "heron",
        "%22heron%20ledger%22",
        "heron%20from:zed",
        "from:zed@old.example",
        "heron%20after:2020-01-01",
    ] {
        for _ in 0..2 {
            let started = std::time::Instant::now();
            let (status, found) =
                get(&h, &format!("/client/agent/search?q={q}&mode=keyword")).await;
            assert_eq!(status, StatusCode::OK);
            assert!(started.elapsed() < std::time::Duration::from_secs(5));
            assert_eq!(found, nothing, "q={q} tells the caller about unread mail");
        }
    }
    let queued: usize = old
        .iter()
        .map(|id| {
            h.store.agent_diagnostics(h.acct, *id).unwrap()["jobs"]
                .as_array()
                .unwrap()
                .len()
        })
        .sum();
    assert!(queued > 0, "the matches really were surfaced");
    assert!(queued <= 15, "and never twice");

    // Archive off: no horizon at all, and still nothing about the matches.
    let off = harness(|_, _| {});
    old_mail(&off, "g-old", "t-old", "heron ledger");
    let (_, found) = get(&off, "/client/agent/search?q=heron&mode=keyword").await;
    assert_eq!(
        found,
        serde_json::json!({"items": [], "match_kind": "keyword", "sort": "recent"})
    );
}

/// A lane that leases every check and never finishes one.
fn stalled_lane(
    store: std::sync::Arc<squelch_core::store::SqliteStore>,
    acct: i64,
) -> squelch_core::store::archive_surface::RefinePoke {
    let poke = squelch_core::store::archive_surface::RefinePoke::new();
    poke.set_runnable(true);
    let heard = poke.clone();
    tokio::spawn(async move {
        loop {
            heard.poked().await;
            while store
                .claim_agent_job(acct, "access", Utc::now(), 600)
                .unwrap()
                .is_some()
            {}
        }
    });
    poke
}

/// B4. THE HTTP DOOR'S OWN CEILING. The Mac client gives a request fifteen
/// seconds; whatever wait the host configured, a search here stops at six
/// with a check still in flight.
#[tokio::test]
async fn the_http_agent_search_never_waits_past_its_own_six_seconds() {
    use squelch_core::store::archive_surface::SurfaceWait;
    let (state, store, acct) = state_with(|_, _| {});
    let poke = stalled_lane(store.clone(), acct);
    let state = state
        .with_archive_days(365)
        .with_refine_poke(poke)
        .with_surface_wait(SurfaceWait {
            deadline: std::time::Duration::from_secs(60),
            idle: std::time::Duration::from_secs(60),
            interval: std::time::Duration::from_millis(50),
        });
    let h = Harness {
        app: squelch_api::router(state),
        store,
        acct,
    };
    old_mail(&h, "g-old", "t-old", "heron ledger");
    let started = std::time::Instant::now();
    let (status, found) = get(&h, "/client/agent/search?q=heron&mode=keyword").await;
    let waited = started.elapsed();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(found["items"], serde_json::json!([]));
    assert!(waited >= std::time::Duration::from_secs(5), "{waited:?}");
    assert!(waited < std::time::Duration::from_secs(9), "{waited:?}");
}

/// B4. The thread route waits for the sibling check its own open queued, so
/// the first answer is the final one: 404 when the ancestor is restricted,
/// the whole thread when it is not.
#[tokio::test]
async fn the_agent_thread_route_waits_for_its_sibling_checks() {
    use squelch_core::store::archive_surface::SurfaceWait;
    use squelch_core::triage::access::AccessDecision;
    for restricted in [true, false] {
        let (state, store, acct) = state_with(|_, _| {});
        let poke = squelch_core::store::archive_surface::RefinePoke::new();
        poke.set_runnable(true);
        let state = state
            .with_archive_days(365)
            .with_refine_poke(poke.clone())
            .with_surface_wait(SurfaceWait {
                deadline: std::time::Duration::from_secs(20),
                idle: std::time::Duration::from_secs(20),
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
            .upsert_message(&msg(h.acct, "g-live", "mix", "osprey plans", "osprey"))
            .unwrap();
        assess(&h, live, false);
        let worker_store = h.store.clone();
        tokio::spawn(async move {
            loop {
                poke.poked().await;
                while let Some(job) = worker_store
                    .claim_agent_job(acct, "access", Utc::now(), 120)
                    .unwrap()
                {
                    let original = worker_store.load_agent_access_message(&job).unwrap();
                    let decision = AccessDecision {
                        restricted,
                        reason: "test".into(),
                    };
                    worker_store
                        .commit_agent_access(&job, &original, &decision, &serde_json::json!({}))
                        .unwrap();
                }
            }
        });
        let (status, thread) = get(&h, "/client/agent/thread/mix").await;
        if restricted {
            assert_eq!(status, StatusCode::NOT_FOUND, "served before its verdict");
        } else {
            assert_eq!(status, StatusCode::OK);
            let ids: Vec<i64> = thread["messages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["id"].as_i64().unwrap())
                .collect();
            assert_eq!(ids, vec![ancestor, live]);
        }
    }
}

/// A request whose answer is not the point: the status alone.
async fn touch(h: &Harness, uri: &str) -> StatusCode {
    h.app
        .clone()
        .oneshot(authed("GET", uri))
        .await
        .unwrap()
        .status()
}

/// B10. PROMOTION IS THE OWNER'S, AT THE HUMAN DOOR. Every embedded-agent
/// route, at an archive row before and after it is cleared, leaves it in the
/// archive with nothing but access checks queued; the human's single-message
/// re-triage promotes it.
#[tokio::test]
async fn only_the_human_door_promotes_an_archive_row() {
    use squelch_core::triage::access::AccessDecision;
    let (state, store, acct) = state_with(|_, _| {});
    let h = Harness {
        app: squelch_api::router(state.with_archive_days(365)),
        store,
        acct,
    };
    let old = old_mail(&h, "g-old", "t-old", "heron ledger");
    let archived = || Store::message_is_archive(h.store.as_ref(), h.acct, old).unwrap();
    for round in 0..2 {
        for uri in [
            "/client/agent/search?q=heron&mode=keyword".to_string(),
            "/client/agent/search?q=heron&mode=hybrid".to_string(),
            "/client/agent/search?q=from:zed".to_string(),
            "/client/agent/thread/t-old".to_string(),
            "/client/agent/feed?destination=fye".to_string(),
            "/client/agent/feed?destination=reading".to_string(),
            "/client/agent/feed?destination=records".to_string(),
            "/client/agent/records".to_string(),
            format!("/client/agent/triage/{old}"),
        ] {
            let status = touch(&h, &uri).await;
            assert_ne!(status, StatusCode::UNAUTHORIZED, "{uri}");
            assert!(archived(), "round {round}: {uri}");
        }
        let jobs = h.store.agent_diagnostics(h.acct, old).unwrap()["jobs"].clone();
        for job in jobs.as_array().unwrap() {
            assert_eq!(job["kind"], "access", "round {round}: {job}");
        }
        // Second round: the same routes at a row the agent may now read.
        while let Some(job) = h
            .store
            .claim_agent_job(h.acct, "access", Utc::now(), 120)
            .unwrap()
        {
            let original = h.store.load_agent_access_message(&job).unwrap();
            let decision = AccessDecision {
                restricted: false,
                reason: "test".into(),
            };
            h.store
                .commit_agent_access(&job, &original, &decision, &serde_json::json!({}))
                .unwrap();
        }
    }
    // A human opening it is not a promotion either.
    touch(&h, &format!("/client/v2/messages/{old}")).await;
    touch(&h, "/client/thread/t-old").await;
    assert!(archived());

    // The owner asks for this one message to be triaged: now it is live.
    let response = h
        .app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/retriage",
            serde_json::json!({"message_id": old}),
        ))
        .await
        .unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    assert!(!archived(), "promoted by the owner's re-triage");
}
