//! Integration tests for the human-door router: bearer auth, sealed exclusion
//! from search, the audited `no-store` reveal, actions against a mock Gmail, and
//! the wire shapes the desktop client decodes.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
use squelch_api::{ApiState, router};
use squelch_core::store::{SmartBlockMemo, SpamScope, SqliteStore, Store, UsageTokens};
use squelch_core::types::{SealedKind, Sensitivity, Tier};
use tower::ServiceExt;

mod common;
use common::{
    Harness, TOKEN, authed, authed_json, body_json, calendar_on, harness, msg, sent_msg, state_with,
};

/// The mail list exactly as `/client/updates` reads it (`human_agent_updates`),
/// with the same filters the handler passes. What these tests mean by "what the
/// client sees".
#[allow(clippy::too_many_arguments)] // the handler's filters, one per axis
fn mail_list(
    store: &SqliteStore,
    acct: squelch_core::types::AccountId,
    since: chrono::DateTime<chrono::Utc>,
    min_importance: Option<u8>,
    status: Option<squelch_core::types::AttentionStatus>,
    band: Option<squelch_core::store::SitrepBand>,
    pending_reminders: bool,
    spam: SpamScope,
) -> squelch_core::error::Result<Vec<squelch_core::types::AttentionUpdate>> {
    use squelch_core::store::agent_triage::{AgentInventoryQuery, AgentTriageStore};
    store.human_agent_updates(
        acct,
        &AgentInventoryQuery {
            since,
            min_importance,
            status,
            band,
            pending_reminders,
            spam,
            ranking: Default::default(),
            before: None,
            limit: None,
        },
    )
}

#[tokio::test]
async fn missing_token_is_401() {
    let Harness { app, .. } = harness(|_, _| {});
    let req = Request::builder()
        .uri("/client/stats")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrong_token_is_401() {
    let Harness { app, .. } = harness(|_, _| {});
    let req = Request::builder()
        .uri("/client/stats")
        .header(header::AUTHORIZATION, "Bearer nope")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn good_token_is_200() {
    let Harness { app, .. } = harness(|_, _| {});
    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// A blank master token no longer stops the door coming up — pairing needs a
/// door to talk to — but it must not become a credential either. The door serves
/// and refuses everything until a device token exists.
#[tokio::test]
async fn an_empty_master_token_serves_and_refuses_everything() {
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let acct = store.ensure_account("me@example.com").unwrap();

    for blank in ["", "   "] {
        let app = router(ApiState::new(store.clone(), acct, blank));
        for header_value in [None, Some("Bearer "), Some("Bearer    ")] {
            let mut req = Request::builder().uri("/client/stats");
            if let Some(h) = header_value {
                req = req.header(header::AUTHORIZATION, h);
            }
            let resp = app
                .clone()
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{blank:?}");
        }
    }
}

#[tokio::test]
async fn human_search_includes_restricted_mail() {
    let Harness { app, .. } = harness(|store, acct| {
        // Normal message mentioning "verification"...
        let n = store
            .upsert_message(&msg(
                acct,
                "g1",
                "t1",
                "Your account verification steps",
                "hello",
            ))
            .unwrap();
        store
            .set_triage(
                n,
                acct,
                60,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
        // ...and a sealed OTP mentioning it too.
        let s = store
            .upsert_message(&msg(acct, "g2", "t2", "verification code inside", "123456"))
            .unwrap();
        store
            .set_triage(
                s,
                acct,
                90,
                Tier::Noise,
                Sensitivity::Sealed,
                Some(SealedKind::Otp),
                "",
                "",
                None,
            )
            .unwrap();
    });

    let resp = app
        .oneshot(authed("GET", "/client/search?q=verification"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "human search includes restricted mail");
    assert!(
        items.iter().any(|item| item["thread_id"] == "t1")
            && items.iter().any(|item| item["thread_id"] == "t2")
    );
    // No embedder attached => default mode resolves to keyword.
    assert_eq!(json["match_kind"], "keyword");
}

#[tokio::test]
async fn shipments_returns_en_route_by_default_and_delivered_with_flag() {
    use squelch_core::triage::{ShipmentInfo, ShipmentStatus};
    let Harness { app, store, acct } = harness(|store, acct| {
        let mid = store
            .upsert_message(&msg(acct, "g1", "t1", "shipped", "b"))
            .unwrap();
        // One en-route (shipped) and one delivered.
        store
            .upsert_shipment(
                acct,
                mid,
                &ShipmentInfo {
                    carrier: "ups".into(),
                    tracking_number: "1Z999AA10123456784".into(),
                    item_name: "Headphones".into(),
                    status: ShipmentStatus::Shipped,
                    tracking_url: Some(
                        "https://www.ups.com/track?tracknum=1Z999AA10123456784".into(),
                    ),
                },
                chrono::Utc::now(),
            )
            .unwrap();
        store
            .upsert_shipment(
                acct,
                mid,
                &ShipmentInfo {
                    carrier: "usps".into(),
                    tracking_number: "9400111899223817428490".into(),
                    item_name: "Book".into(),
                    status: ShipmentStatus::Delivered,
                    tracking_url: None,
                },
                chrono::Utc::now(),
            )
            .unwrap();
    });
    let _ = (&store, acct);

    // Default: en-route only (the shipped one, not the delivered one).
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/shipments"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let items = json.as_array().unwrap();
    assert_eq!(items.len(), 1, "delivered excluded by default");
    assert_eq!(items[0]["tracking_number"], "1Z999AA10123456784");
    assert_eq!(items[0]["status"], "shipped");

    // include_delivered=true: both.
    let resp = app
        .oneshot(authed("GET", "/client/shipments?include_delivered=true"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(
        json.as_array().unwrap().len(),
        2,
        "delivered included with flag"
    );
}

/// WIRE CONTRACT: the four carrier-poll fields ride out on `/client/shipments`
/// verbatim, so the client can show an ETA and the carrier's own words. A row
/// that has never been polled still carries all four KEYS, as nulls — a client
/// decoding them must never have to distinguish "absent" from "not yet polled".
#[tokio::test]
async fn shipments_carry_the_carrier_poll_fields() {
    use squelch_core::triage::{CarrierTrack, ShipmentInfo, ShipmentStatus};
    let t0 = chrono::Utc::now();
    let eta = t0 + chrono::Duration::hours(6);
    let polled = t0 + chrono::Duration::minutes(3);

    let Harness { app, .. } = harness(|store, acct| {
        let mid = store
            .upsert_message(&msg(acct, "g1", "t1", "shipped", "b"))
            .unwrap();
        let polled_id = store
            .upsert_shipment(
                acct,
                mid,
                &ShipmentInfo {
                    carrier: "ups".into(),
                    tracking_number: "1Z999AA10123456784".into(),
                    item_name: "Headphones".into(),
                    status: ShipmentStatus::Shipped,
                    tracking_url: None,
                },
                t0,
            )
            .unwrap();
        // A second row that no poll has ever touched, for the null case.
        store
            .upsert_shipment(
                acct,
                mid,
                &ShipmentInfo {
                    carrier: "usps".into(),
                    tracking_number: "9400111899223817428490".into(),
                    item_name: "Book".into(),
                    status: ShipmentStatus::Shipped,
                    tracking_url: None,
                },
                t0,
            )
            .unwrap();
        store
            .apply_carrier_track(
                acct,
                polled_id,
                &CarrierTrack {
                    status: Some(ShipmentStatus::OutForDelivery),
                    carrier_status_raw: "Out For Delivery".into(),
                    eta: Some(eta),
                    delivered_at: None,
                },
                polled,
            )
            .unwrap();
    });

    let resp = app
        .oneshot(authed("GET", "/client/shipments"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let items = json.as_array().unwrap();
    assert_eq!(items.len(), 2);

    let polled_row = items
        .iter()
        .find(|i| i["tracking_number"] == "1Z999AA10123456784")
        .expect("the polled shipment");
    assert_eq!(polled_row["status"], "out_for_delivery");
    assert_eq!(polled_row["carrier_status_raw"], "Out For Delivery");
    // Timestamps go out as strings the client parses back to the same instant.
    let parse = |v: &Value| {
        v.as_str()
            .unwrap()
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap()
    };
    assert_eq!(parse(&polled_row["eta"]), eta);
    assert_eq!(parse(&polled_row["last_polled_at"]), polled);
    assert!(
        polled_row["delivered_at"].is_null(),
        "out for delivery is not delivered"
    );

    let unpolled = items
        .iter()
        .find(|i| i["tracking_number"] == "9400111899223817428490")
        .expect("the never-polled shipment");
    for field in [
        "carrier_status_raw",
        "eta",
        "delivered_at",
        "last_polled_at",
    ] {
        assert!(
            unpolled.get(field).is_some_and(Value::is_null),
            "{field} must be present and null before the first poll"
        );
    }
}

/// A configured kick reaches the poller's `Notify` and reports the carriers it
/// will hit. `notify_one` parks a permit when nobody is waiting yet, so awaiting
/// AFTER the request still completes — the timeout is what makes a missed poke
/// a failure rather than a hang.
#[tokio::test]
async fn shipment_poll_kicks_the_poller_and_lists_its_carriers() {
    let kick = Arc::new(tokio::sync::Notify::new());
    let (state, _store, _acct) = state_with(|_, _| {});
    // Deliberately unsorted + duplicated: the response must not depend on how
    // the caller assembled the list.
    let app = router(
        state.with_shipment_poll_kick(kick.clone(), vec!["ups".into(), "dhl".into(), "ups".into()]),
    );

    let resp = app
        .oneshot(authed("POST", "/client/shipments/poll"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["kicked"], true);
    assert_eq!(json["carriers"], serde_json::json!(["dhl", "ups"]));

    tokio::time::timeout(std::time::Duration::from_secs(1), kick.notified())
        .await
        .expect("the poller's kick must have been notified");
}

/// Carrier polling is BYOK, so no poller is the RESTING STATE, not an error: the
/// route serves 200 with `kicked: false` and an empty list, which is how the
/// client knows to hide the button. Still behind the bearer, like everything in
/// the `/client` tree.
#[tokio::test]
async fn shipment_poll_without_a_poller_is_a_200_that_kicked_nothing() {
    let Harness { app, .. } = harness(|_, _| {});

    let unauthed = Request::builder()
        .method("POST")
        .uri("/client/shipments/poll")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(unauthed).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let resp = app
        .oneshot(authed("POST", "/client/shipments/poll"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["kicked"], false);
    assert_eq!(json["carriers"], serde_json::json!([]));
}

/// The clear endpoint end to end: bearer-gated, idempotent, 404 on an unknown
/// id, and — the design's whole point — REVERSED BY AN UPDATE with no un-clear
/// call anywhere. The Passband client builds against this contract.
#[tokio::test]
async fn clearing_a_shipment_hides_it_until_an_update_brings_it_back() {
    use squelch_core::triage::{CarrierTrack, ShipmentInfo, ShipmentStatus};
    let Harness { app, store, acct } = harness(|store, acct| {
        let mid = store
            .upsert_message(&msg(acct, "g1", "t1", "shipped", "b"))
            .unwrap();
        store
            .upsert_shipment(
                acct,
                mid,
                &ShipmentInfo {
                    carrier: "ups".into(),
                    tracking_number: "1Z999AA10123456784".into(),
                    item_name: "Headphones".into(),
                    status: ShipmentStatus::Shipped,
                    tracking_url: None,
                },
                chrono::Utc::now() - chrono::Duration::hours(1),
            )
            .unwrap();
    });
    let listed = store
        .list_shipments(acct, false, Default::default())
        .unwrap();
    let sid = listed[0].id;

    // Behind the bearer, like everything in the `/client` tree.
    let unauthed = Request::builder()
        .method("POST")
        .uri(format!("/client/shipments/{sid}/clear"))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(unauthed).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let resp = app
        .clone()
        .oneshot(authed("POST", &format!("/client/shipments/{sid}/clear")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["cleared"], true);
    assert_eq!(json["shipment_id"], sid);
    let cleared_at = json["cleared_at"]
        .as_str()
        .expect("cleared_at rides out as an RFC3339 string")
        .parse::<chrono::DateTime<chrono::Utc>>()
        .expect("and parses back");

    // Gone from the listing...
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/shipments"))
        .await
        .unwrap();
    assert!(
        body_json(resp).await.as_array().unwrap().is_empty(),
        "a cleared package leaves the listing"
    );

    // ...idempotently: a double-tap restamps and still answers 200.
    let resp = app
        .clone()
        .oneshot(authed("POST", &format!("/client/shipments/{sid}/clear")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // An unknown id is a 404, not a 500 and not a `false`.
    let resp = app
        .clone()
        .oneshot(authed("POST", "/client/shipments/999999/clear"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // REVIVAL: one carrier poll that actually moves the package, and the row is
    // back. No un-clear endpoint was called, because none exists.
    store
        .apply_carrier_track(
            acct,
            sid,
            &CarrierTrack {
                status: Some(ShipmentStatus::OutForDelivery),
                carrier_status_raw: "Out For Delivery".into(),
                eta: None,
                delivered_at: None,
            },
            cleared_at + chrono::Duration::minutes(5),
        )
        .unwrap();
    let resp = app
        .oneshot(authed("GET", "/client/shipments"))
        .await
        .unwrap();
    let items = body_json(resp).await;
    let items = items.as_array().unwrap();
    assert_eq!(items.len(), 1, "an update revives the cleared row");
    assert_eq!(items[0]["status"], "out_for_delivery");
}

#[tokio::test]
async fn receipts_returns_rows_newest_first_and_is_bearer_gated() {
    use squelch_core::triage::ReceiptInfo;
    let Harness { app, .. } = harness(|store, acct| {
        let m1 = store
            .upsert_message(&msg(acct, "g1", "t1", "receipt a", "b"))
            .unwrap();
        let m2 = store
            .upsert_message(&msg(acct, "g2", "t2", "receipt b", "b"))
            .unwrap();
        // Older, then newer — expect newest-first ordering on read.
        store
            .upsert_receipt(
                acct,
                m1,
                "no-reply@baywheels.com",
                Some("Bay Wheels"),
                &ReceiptInfo {
                    amount: Some(3.49),
                    currency: Some("USD".into()),
                },
                chrono::Utc::now() - chrono::Duration::hours(2),
            )
            .unwrap();
        store
            .upsert_receipt(
                acct,
                m2,
                "orders@shop.com",
                None,
                &ReceiptInfo {
                    amount: None,
                    currency: None,
                },
                chrono::Utc::now(),
            )
            .unwrap();
    });

    // Bearer-gated: no token => 401.
    let unauth = Request::builder()
        .uri("/client/receipts")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(unauth).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Authed: rows, newest-first, with clean sender + null amount preserved.
    let resp = app
        .oneshot(authed("GET", "/client/receipts"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let items = json.as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["from_addr"], "orders@shop.com", "newest first");
    assert_eq!(items[0]["amount"], Value::Null, "null amount preserved");
    assert_eq!(items[1]["from_addr"], "no-reply@baywheels.com");
    assert_eq!(items[1]["from_name"], "Bay Wheels");
    assert_eq!(items[1]["amount"], 3.49);
    assert_eq!(items[1]["currency"], "USD");
}

#[tokio::test]
async fn retriage_route_exists_resets_and_audits() {
    let Harness { app, store, acct } = harness(|store, acct| {
        let m = store
            .upsert_message(&msg(acct, "g1", "t1", "s", "b"))
            .unwrap();
        store
            .set_triage(
                m,
                acct,
                60,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
        // An LLM-classified row, so there is something to reset.
        store
            .stage1_apply(&squelch_core::store::Stage1Applied {
                message_id: m,
                account_id: acct,
                importance: 50,
                tier: squelch_core::types::Tier::Signal,
                one_line: "x".into(),
                reason: "x".into(),
                field_reasons: Default::default(),
                stage1_model_used: "claude-x".into(),
                needs_stage2: false,
                escalation_reason: None,
                deadline: None,
                category: Some("general".into()),
            })
            .unwrap();
    });

    // The route is mounted (the 404 class of regression) and resets the window.
    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/retriage",
            serde_json::json!({ "days": 7 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["reset"], 1);

    // Audited.
    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit.iter().any(|e| e.action == "retriage"),
        "retriage must land an audit row"
    );
}

/// The counter the blocking modal polls. GET and POST share the path, so the
/// GET half is exactly the "is it mounted" regression the POST test above
/// guards against — and the client BLOCKS THE APP on this route, so a 404 here
/// is a modal nobody can watch.
#[tokio::test]
async fn retriage_progress_route_reports_the_run_it_kicked() {
    let Harness { app, store, acct } = harness(|store, acct| {
        let m = store
            .upsert_message(&msg(acct, "g1", "t1", "s", "b"))
            .unwrap();
        store
            .set_triage(
                m,
                acct,
                60,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
        store
            .stage1_apply(&squelch_core::store::Stage1Applied {
                message_id: m,
                account_id: acct,
                importance: 50,
                tier: squelch_core::types::Tier::Signal,
                one_line: "x".into(),
                reason: "x".into(),
                field_reasons: Default::default(),
                stage1_model_used: "claude-x".into(),
                needs_stage2: false,
                escalation_reason: None,
                deadline: None,
                category: Some("general".into()),
            })
            .unwrap();
    });

    // Before anything is asked for: no run, and NO `started_at` — the client
    // reads a zero total as "nothing in flight", so it must not be confusable
    // with a finished one.
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/retriage"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["total"], 0);
    assert_eq!(json["done"], 0);
    assert!(json["started_at"].is_null());

    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/retriage",
            serde_json::json!({ "days": 7 }),
        ))
        .await
        .unwrap();
    assert_eq!(body_json(resp).await["reset"], 1);

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/retriage"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["total"], 1, "the kicked row is the run");
    assert_eq!(json["done"], 0, "and it has not been re-run yet");
    assert!(
        json["started_at"].is_string(),
        "a live run says when it began"
    );

    // Durable jobs, rather than old stage sentinels, determine completion.
    use squelch_core::store::agent_triage::AgentTriageStore;
    while let Some(job) = store
        .claim_agent_job(acct, "triage", chrono::Utc::now(), 60)
        .unwrap()
    {
        store.complete_agent_job(&job).unwrap();
    }

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/retriage"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(
        (json["total"].as_i64(), json["done"].as_i64()),
        (Some(1), Some(1)),
        "the modal unblocks only when the queues are actually empty"
    );
}

#[tokio::test]
async fn banking_returns_rows_newest_first_and_is_bearer_gated() {
    use squelch_core::store::BankingApplied;
    let Harness { app, .. } = harness(|store, acct| {
        let m1 = store
            .upsert_message(&msg(acct, "g1", "t1", "statement", "b"))
            .unwrap();
        let m2 = store
            .upsert_message(&msg(acct, "g2", "t2", "alert", "b"))
            .unwrap();
        // Older statement, then newer alert -> expect newest-first ordering.
        store
            .banking_apply(&BankingApplied {
                message_id: m1,
                account_id: acct,
                kind: "statement".into(),
                institution: Some("Chase".into()),
                amount: Some(1234.56),
                currency: Some("USD".into()),
                account_hint: Some("…1234".into()),
                received_at: chrono::Utc::now() - chrono::Duration::hours(2),
                extractor_model_used: "claude-haiku-4-5".into(),
                auto_resolve: true,
            })
            .unwrap();
        store
            .banking_apply(&BankingApplied {
                message_id: m2,
                account_id: acct,
                kind: "transaction_alert".into(),
                institution: None,
                amount: Some(42.10),
                currency: Some("USD".into()),
                account_hint: None,
                received_at: chrono::Utc::now(),
                extractor_model_used: "claude-haiku-4-5".into(),
                auto_resolve: true,
            })
            .unwrap();
    });

    // Bearer-gated: no token => 401.
    let unauth = Request::builder()
        .uri("/client/banking")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(unauth).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Authed: newest-first, exact wire shape (no account_id), nulls preserved.
    let resp = app.oneshot(authed("GET", "/client/banking")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let items = json.as_array().unwrap();
    assert_eq!(items.len(), 2);
    // Newest first: the transaction alert.
    assert_eq!(items[0]["kind"], "transaction_alert");
    assert_eq!(items[0]["institution"], Value::Null);
    assert_eq!(items[0]["account_hint"], Value::Null);
    assert_eq!(items[0]["amount"], 42.10);
    assert!(
        items[0].get("account_id").is_none(),
        "account_id off the wire"
    );
    // Then the statement.
    assert_eq!(items[1]["kind"], "statement");
    assert_eq!(items[1]["institution"], "Chase");
    assert_eq!(items[1]["amount"], 1234.56);
    assert_eq!(items[1]["account_hint"], "…1234");
    assert_eq!(items[1]["currency"], "USD");
}

#[tokio::test]
async fn calendar_returns_windowed_rows_newest_first_and_is_bearer_gated() {
    use squelch_core::triage::{CalendarInfo, CalendarKind};
    let Harness { app, .. } = harness(|store, acct| {
        let m1 = store
            .upsert_message(&msg(acct, "g1", "t1", "invite", "b"))
            .unwrap();
        let m2 = store
            .upsert_message(&msg(acct, "g2", "t2", "cancel", "b"))
            .unwrap();
        let m3 = store
            .upsert_message(&msg(acct, "g3", "t3", "old", "b"))
            .unwrap();
        // 2h old invite, 1h old cancellation, 30h old update (outside the
        // default 24h received_at window).
        store
            .upsert_calendar_update(
                acct,
                m1,
                &CalendarInfo {
                    kind: CalendarKind::Invite,
                    event_title: Some("Design review".into()),
                    starts_at: Some(chrono::Utc::now() + chrono::Duration::days(2)),
                    organizer: Some("Sam Doe".into()),
                },
                chrono::Utc::now() - chrono::Duration::hours(2),
            )
            .unwrap();
        store
            .upsert_calendar_update(
                acct,
                m2,
                &CalendarInfo {
                    kind: CalendarKind::Cancellation,
                    event_title: None,
                    starts_at: None,
                    organizer: None,
                },
                chrono::Utc::now() - chrono::Duration::hours(1),
            )
            .unwrap();
        store
            .upsert_calendar_update(
                acct,
                m3,
                &CalendarInfo {
                    kind: CalendarKind::Update,
                    event_title: Some("Old thing".into()),
                    starts_at: None,
                    organizer: None,
                },
                chrono::Utc::now() - chrono::Duration::hours(30),
            )
            .unwrap();
    });

    // Bearer-gated like every /client route: no token => 401.
    let unauth = Request::builder()
        .uri("/client/calendar")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(unauth).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Default 24h window: two rows, newest-RECEIVED first, exact wire shape.
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/calendar"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let items = json.as_array().unwrap();
    assert_eq!(items.len(), 2, "30h-old row outside the default 24h window");
    assert_eq!(items[0]["kind"], "cancellation", "newest-received first");
    assert_eq!(
        items[0]["event_title"],
        Value::Null,
        "nullable fields serialize as null"
    );
    assert_eq!(items[0]["starts_at"], Value::Null);
    assert_eq!(items[0]["organizer"], Value::Null);
    assert_eq!(items[1]["kind"], "invite");
    assert_eq!(items[1]["event_title"], "Design review");
    assert_eq!(items[1]["organizer"], "Sam Doe");
    assert!(items[1]["starts_at"].is_string(), "iso8601 start");
    // Contract shape: exactly these keys (account_id deliberately absent).
    let keys: std::collections::BTreeSet<_> =
        items[0].as_object().unwrap().keys().cloned().collect();
    let expect: std::collections::BTreeSet<String> = [
        "id",
        "message_id",
        "thread_id",
        "kind",
        "event_title",
        "starts_at",
        "organizer",
        "received_at",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(keys, expect, "wire contract keys");
    assert!(items[0]["id"].is_i64());
    assert!(items[0]["message_id"].is_i64());
    assert!(items[0]["received_at"].is_string());

    // Wider window picks up the 30h row.
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/calendar?hours=48"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json.as_array().unwrap().len(), 3);

    // Out-of-range hours are CLAMPED, not rejected. The 1h-old row sits exactly
    // on the hours=1 boundary, so assert a subset rather than an exact count.
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/calendar?hours=0"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "clamped, not 400");
    let json = body_json(resp).await;
    assert!(json.as_array().unwrap().len() <= 1, "hours=0 clamps to 1h");

    // Absurdly large hours clamp to a week (still 200, still all three rows).
    let resp = app
        .oneshot(authed("GET", "/client/calendar?hours=999999"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(
        json.as_array().unwrap().len(),
        3,
        "clamped to 168h, rows within remain"
    );
}

/// A garbage `mode` value is a 400.
#[tokio::test]
async fn search_bad_mode_is_400() {
    let Harness { app, .. } = harness(|store, acct| {
        let n = store
            .upsert_message(&msg(acct, "g1", "t1", "hello world", "body"))
            .unwrap();
        store
            .set_triage(
                n,
                acct,
                60,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
    });
    let resp = app
        .oneshot(authed("GET", "/client/search?q=hello&mode=bogus"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// The sort is a first-class request parameter: it is honoured, it is echoed
/// back, and an unreadable value is refused rather than quietly defaulted.
#[tokio::test]
async fn search_sort_is_honoured_echoed_and_validated() {
    // A stronger old match and a weaker fresh one, so the two sorts genuinely
    // disagree about which comes first. The gap is deliberately MODEST — a real
    // sentence about a contract, not the bare word twice — because recency is a
    // tilt with a ceiling (~1/FLOOR, see `store::recency`) and a pair further
    // apart than that would rank the same way under both sorts and prove
    // nothing. The same pair the store's own recency tests use.
    let Harness { app, .. } = harness(|store, acct| {
        let old = msg(
            acct,
            "g-old",
            "t-old",
            "contract",
            "The signed contract with the vendor is attached for your records.",
        );
        let mut old = old;
        old.received_at = chrono::Utc::now() - chrono::Duration::days(500);
        let n = store.upsert_message(&old).unwrap();
        store
            .set_triage(
                n,
                acct,
                60,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();

        let fresh = msg(
            acct,
            "g-new",
            "t-new",
            "weekly digest",
            "A stray mention of a contract sits far down this roundup of newsletter \
             items, among gardening tips, local events, recipes, and a reader letter \
             about compost.",
        );
        let n = store.upsert_message(&fresh).unwrap();
        store
            .set_triage(
                n,
                acct,
                60,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
    });

    let threads = |json: &Value| -> Vec<String> {
        json["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["thread_id"].as_str().unwrap().to_string())
            .collect()
    };

    // Default: recency considered, and the response says so.
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/search?q=contract"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["sort"], "recent", "the default is echoed, not omitted");
    assert_eq!(threads(&json), vec!["t-new", "t-old"]);

    // Opted out: relevance alone, and the order inverts.
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/search?q=contract&sort=best_match"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["sort"], "best_match");
    assert_eq!(
        threads(&json),
        vec!["t-old", "t-new"],
        "best_match ranks the stronger match first however old it is"
    );

    // A value nobody serves is a 400, not a silent fallback to the default.
    let resp = app
        .oneshot(authed("GET", "/client/search?q=contract&sort=newest"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Explicit `mode=semantic` with NO vector index degrades to keyword and reports
/// the kind actually run — never erroring the caller.
#[tokio::test]
async fn search_semantic_without_vectors_falls_back_to_keyword() {
    let Harness { app, .. } = harness(|store, acct| {
        let n = store
            .upsert_message(&msg(acct, "g1", "t1", "quarterly report attached", "body"))
            .unwrap();
        store
            .set_triage(
                n,
                acct,
                60,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
    });
    let resp = app
        .oneshot(authed("GET", "/client/search?q=quarterly&mode=semantic"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(
        json["match_kind"], "keyword",
        "no vectors => keyword fallback"
    );
    assert_eq!(json["items"].as_array().unwrap().len(), 1);
}

/// With an embedder attached, the default mode is hybrid, semantic/hybrid run,
/// and sealed mail is STILL excluded from every mode.
#[tokio::test]
async fn human_search_modes_include_restricted_mail() {
    use squelch_core::embed::StubEmbedder;

    // 384-dim to match the vec0 table.
    let store = SqliteStore::open_in_memory()
        .unwrap()
        .with_embedder(Arc::new(StubEmbedder::new(384)))
        .unwrap();
    let acct = store.ensure_account("me@example.com").unwrap();
    let store = Arc::new(store);

    // Normal message + its vector.
    let n = store
        .upsert_message(&msg(
            acct,
            "g1",
            "t1",
            "acme invoice for services",
            "please pay",
        ))
        .unwrap();
    store
        .set_triage(
            n,
            acct,
            60,
            Tier::Signal,
            Sensitivity::Normal,
            None,
            "",
            "",
            None,
        )
        .unwrap();
    let v = store
        .embedder()
        .unwrap()
        .embed("acme invoice for services please pay")
        .unwrap();
    store.upsert_message_vector(acct, n, &v).unwrap();

    // Sealed OTP + (defensively) a leaked vector — must never surface.
    let s = store
        .upsert_message(&msg(acct, "g2", "t2", "acme verification code", "123456"))
        .unwrap();
    store
        .set_triage(
            s,
            acct,
            90,
            Tier::Noise,
            Sensitivity::Sealed,
            Some(SealedKind::Otp),
            "",
            "",
            None,
        )
        .unwrap();
    let sv = store
        .embedder()
        .unwrap()
        .embed("acme verification code 123456")
        .unwrap();
    store.upsert_message_vector(acct, s, &sv).unwrap();

    let state = ApiState::new(store.clone(), acct, TOKEN);
    let app = router(state);

    // Default (no mode) => hybrid; sealed absent.
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/search?q=acme"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["match_kind"], "hybrid");
    let items = json["items"].as_array().unwrap();
    assert!(
        items.iter().any(|i| i["thread_id"] == "t2"),
        "human hybrid search includes restricted mail"
    );
    assert!(items.iter().any(|i| i["thread_id"] == "t1"));

    // Explicit semantic => runs semantic, sealed still absent.
    let resp = app
        .oneshot(authed("GET", "/client/search?q=acme&mode=semantic"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["match_kind"], "semantic");
    let items = json["items"].as_array().unwrap();
    assert!(
        items.iter().any(|i| i["thread_id"] == "t2"),
        "human semantic search includes restricted mail"
    );
}

// --- search: match-window snippets and query operators ----------------------

/// A body whose only occurrence of "pangolin" sits well past anything the
/// stored head-of-message snippet would ever cover.
const DEEP_BODY: &str = "Thanks for subscribing to the weekly digest. This week \
    we cover garden tools, the spring planting calendar, a reader letter about \
    compost, and our usual roundup of local events. Finally, a note on the \
    pangolin conservation fundraiser at the community hall.";

/// Midday UTC on the given day, so a `after:`/`before:` bound at midnight lands
/// unambiguously on one side of it.
fn at(y: i32, m: u32, d: u32) -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    chrono::Utc
        .with_ymd_and_hms(y, m, d, 12, 0, 0)
        .single()
        .unwrap()
}

#[tokio::test]
async fn search_snippet_is_the_body_match_window() {
    let Harness { app, .. } = harness(|store, acct| {
        let mut m = msg(acct, "g1", "t1", "weekly digest", DEEP_BODY);
        m.snippet = "Thanks for subscribing to the weekly digest.".into();
        let id = store.upsert_message(&m).unwrap();
        store
            .set_triage(
                id,
                acct,
                40,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
    });

    let resp = app
        .oneshot(authed("GET", "/client/search?q=pangolin"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    let snippet = items[0]["snippet"].as_str().unwrap();
    assert!(
        snippet.contains("pangolin"),
        "a body-deep hit must show the matched term, got {snippet:?}"
    );
    assert!(
        !snippet.starts_with("Thanks for subscribing"),
        "not the stored intro paragraph"
    );
}

/// Two dated invoices from Jane, one from Bob, one sealed and one sent — the
/// corpus every operator test below filters over.
fn seed_operator_corpus(store: &SqliteStore, acct: i64) {
    // The shared fixture is from Alice; these four are from Jane, so `from:jane`
    // means something.
    let from_jane = |m: &mut squelch_core::types::NewMessage| {
        m.from_addr = "jane@example.com".into();
        m.from_name = Some("Jane Doe".into());
    };

    let mut jan = msg(acct, "g-jan", "t-jan", "invoice january", "january invoice");
    from_jane(&mut jan);
    jan.received_at = at(2026, 1, 15);
    let mut feb = msg(
        acct,
        "g-feb",
        "t-feb",
        "invoice february",
        "february invoice",
    );
    from_jane(&mut feb);
    feb.received_at = at(2026, 2, 15);

    let mut bob = msg(acct, "g-bob", "t-bob", "invoice from bob", "bob's invoice");
    bob.from_addr = "bob@other.test".into();
    bob.from_name = Some("Bob".into());
    bob.received_at = at(2026, 2, 20);

    for m in [&jan, &feb, &bob] {
        let id = store.upsert_message(m).unwrap();
        store
            .set_triage(
                id,
                acct,
                50,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
    }

    // Sealed: same sender, same word, must never appear on any leg.
    let mut sealed = msg(
        acct,
        "g-seal",
        "t-seal",
        "invoice code inside",
        "code 123456",
    );
    from_jane(&mut sealed);
    sealed.received_at = at(2026, 2, 16);
    let sid = store.upsert_message(&sealed).unwrap();
    store
        .set_triage(
            sid,
            acct,
            90,
            Tier::Noise,
            Sensitivity::Sealed,
            Some(SealedKind::Otp),
            "",
            "",
            None,
        )
        .unwrap();

    // Sent: search is the human door's INBOX view, so this stays out too.
    let mut sent = msg(
        acct,
        "g-sent",
        "t-sent",
        "re: invoice",
        "sending the invoice back",
    );
    from_jane(&mut sent);
    sent.is_sent = true;
    sent.received_at = at(2026, 2, 17);
    store.upsert_message(&sent).unwrap();
}

#[tokio::test]
async fn search_operators_filter_by_sender_and_date() {
    let Harness { app, .. } = harness(seed_operator_corpus);

    let threads = |json: &Value| -> Vec<String> {
        json["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["thread_id"].as_str().unwrap().to_string())
            .collect()
    };

    // from: matches the address or the display name; the sealed row carrying the
    // same sender and the same word stays absent.
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/search?q=invoice%20from:jane"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["match_kind"], "keyword");
    let t = threads(&json);
    assert_eq!(t.len(), 3, "all of Jane's received mail: {t:?}");
    assert!(
        t.contains(&"t-seal".to_string()),
        "restricted mail is human-readable"
    );
    assert!(!t.contains(&"t-sent".to_string()), "sent stays excluded");

    // after: is inclusive at midnight UTC of the named day.
    let json = body_json(
        app.clone()
            .oneshot(authed("GET", "/client/search?q=invoice%20after:2026-02-01"))
            .await
            .unwrap(),
    )
    .await;
    let t = threads(&json);
    assert_eq!(t.len(), 3, "february onward: {t:?}");
    assert!(!t.contains(&"t-jan".to_string()));

    // before: is exclusive at midnight UTC of the named day.
    let json = body_json(
        app.clone()
            .oneshot(authed(
                "GET",
                "/client/search?q=invoice%20before:2026-02-16",
            ))
            .await
            .unwrap(),
    )
    .await;
    // Keyword hits come back in FTS rank order, so assert the SET, not the order.
    let t = threads(&json);
    assert_eq!(t.len(), 2, "january and february only: {t:?}");
    assert!(!t.contains(&"t-bob".to_string()), "bob is past the bound");

    // All three together narrow to exactly one message.
    let json = body_json(
        app.clone()
            .oneshot(authed(
                "GET",
                "/client/search?q=invoice%20from:jane%20after:2026-02-01%20before:2026-03-01",
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(threads(&json).len(), 2);
    assert!(
        threads(&json).contains(&"t-feb".to_string())
            && threads(&json).contains(&"t-seal".to_string())
    );

    // An unparseable date is not an operator: the token stays in the search
    // text, and the request still succeeds.
    let resp = app
        .oneshot(authed("GET", "/client/search?q=invoice%20after:soon"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn search_with_only_operators_lists_and_keeps_exclusions() {
    let Harness { app, .. } = harness(seed_operator_corpus);

    // No words left after parsing => a plain newest-first listing, no FTS MATCH,
    // reported as the keyword kind.
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/search?q=from:jane"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["match_kind"], "keyword");
    let t: Vec<&str> = json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["thread_id"].as_str().unwrap())
        .collect();
    assert_eq!(t, vec!["t-seal", "t-feb", "t-jan"], "newest first");
    assert!(
        t.contains(&"t-seal"),
        "restricted mail remains human-readable"
    );
    assert!(!t.contains(&"t-sent"), "sent absent from the listing");

    // NOTHING RETRIEVED THESE ROWS. `legs` is provenance, and a listing has
    // none: no MATCH ran, nothing was ranked, and the diagnostics beside it are
    // zeroes. Claiming the keyword leg here would say bm25 chose this order and
    // that a term in the diagnostics found these rows, and neither is true.
    for item in json["items"].as_array().unwrap() {
        assert_eq!(
            item["legs"],
            serde_json::json!([]),
            "a filter-only listing claims no leg"
        );
    }
    assert_eq!(json["diagnostics"]["terms"], serde_json::json!([]));

    // A date-only listing spans every non-sealed, non-sent row.
    let json = body_json(
        app.clone()
            .oneshot(authed("GET", "/client/search?q=after:2026-01-01"))
            .await
            .unwrap(),
    )
    .await;
    let t: Vec<&str> = json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["thread_id"].as_str().unwrap())
        .collect();
    assert_eq!(t, vec!["t-bob", "t-seal", "t-feb", "t-jan"]);

    // Operators that constrain nothing leave nothing to search: still a 400.
    let resp = app
        .oneshot(authed("GET", "/client/search?q=from:"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// The strict count is taken ONCE per request when the two questions are the
/// same one, and TWICE when they are not.
///
/// The keyword page counts its strict set to place the seam between its two
/// passes, and `diagnostics.strict_hits` reports the same number — but only
/// while there are no operators. Under a `from:` the page's count carries that
/// predicate and the diagnostics' does not, so sharing the page's number would
/// make the wire report a mailbox-wide count that had been narrowed to one
/// sender: "no message says all of this" for a mailbox where one does.
#[tokio::test]
async fn diagnostics_keep_their_own_strict_count_under_an_operator() {
    let Harness { app, .. } = harness(seed_operator_corpus);

    // "invoice bob": only Bob's message carries both words, and it is not
    // Jane's, so the FILTERED strict set is empty and the page is Jane's three
    // invoices off the any-only pass.
    let json = body_json(
        app.clone()
            .oneshot(authed("GET", "/client/search?q=invoice%20bob%20from:jane"))
            .await
            .unwrap(),
    )
    .await;
    let t: Vec<&str> = json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["thread_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        t.len(),
        3,
        "jane's invoices, including restricted mail: {t:?}"
    );
    assert!(!t.contains(&"t-bob"), "the operator still filters the page");
    assert_eq!(
        json["diagnostics"]["strict_hits"], 1,
        "one message in the mailbox says both words, and the count says so"
    );

    // Without the operator the two questions coincide, and the answer is the
    // same number the page seams on.
    let json = body_json(
        app.oneshot(authed("GET", "/client/search?q=invoice%20bob"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(json["diagnostics"]["strict_hits"], 1);
    let t: Vec<&str> = json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["thread_id"].as_str().unwrap())
        .collect();
    assert_eq!(t.first(), Some(&"t-bob"), "the strict hit leads: {t:?}");
}

#[tokio::test]
async fn reveal_writes_audit_and_returns_body() {
    let Harness { app, store, acct } = harness(|store, acct| {
        let s = store
            .upsert_message(&msg(acct, "g1", "t1", "code", "your code is 987654"))
            .unwrap();
        store
            .set_triage(
                s,
                acct,
                90,
                Tier::Noise,
                Sensitivity::Sealed,
                Some(SealedKind::Otp),
                "",
                "",
                None,
            )
            .unwrap();
    });

    let sealed_id = store.sealed_messages(acct).unwrap()[0].id;

    let resp = app
        .oneshot(authed(
            "POST",
            &format!("/client/sealed/{sealed_id}/reveal"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let json = body_json(resp).await;
    assert_eq!(json["body"], "your code is 987654");

    let audit = store.list_audit(acct, 10).unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].action, "reveal_sealed");
    assert_eq!(
        audit[0].target.as_deref(),
        Some(sealed_id.to_string().as_str())
    );
}

#[tokio::test]
async fn sealed_list_has_no_bodies() {
    let Harness { app, .. } = harness(|store, acct| {
        let s = store
            .upsert_message(&msg(acct, "g1", "t1", "code", "secret body 111"))
            .unwrap();
        store
            .set_triage(
                s,
                acct,
                90,
                Tier::Noise,
                Sensitivity::Sealed,
                Some(SealedKind::Otp),
                "",
                "",
                None,
            )
            .unwrap();
    });

    let resp = app.oneshot(authed("GET", "/client/sealed")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let items = json.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert!(
        items[0].get("body").is_none(),
        "no body field in sealed list"
    );
    assert_eq!(items[0]["kind"], "otp");
}

#[tokio::test]
async fn human_sent_listing_includes_restricted_mail_and_excludes_inbound() {
    let Harness { app, .. } = harness(|store, acct| {
        let seed = |gmail: &str, thread: &str, subject: &str, to: &str, sensitivity| {
            let id = store
                .upsert_message(&sent_msg(acct, gmail, thread, subject, to))
                .unwrap();
            store
                .set_triage(id, acct, 0, Tier::Noise, sensitivity, None, "", "", None)
                .unwrap();
            id
        };
        seed(
            "s1",
            "ts1",
            "first",
            "Alice <alice@friends.com>",
            Sensitivity::Normal,
        );
        seed(
            "s2",
            "ts2",
            "second",
            "bob@friends.com",
            Sensitivity::Normal,
        );
        seed(
            "s3",
            "ts3",
            "sealed",
            "support@bank.com",
            Sensitivity::Sealed,
        );
        store
            .upsert_message(&msg(acct, "g-in", "t-in", "inbound", "hi"))
            .unwrap();
    });

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/sent?limit=1"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["subject"], "sealed");
    assert_eq!(items[0]["to"], "support@bank.com");
    assert_eq!(items[0]["opens"], 0);
    assert!(items[0]["sent_at"].as_str().unwrap().contains('T'));
    assert!(items[0]["thread_id"].as_str().is_some());

    let cursor = json["next_cursor"]
        .as_str()
        .expect("next_cursor")
        .to_string();
    let resp2 = app
        .oneshot(authed(
            "GET",
            &format!("/client/sent?limit=1&cursor={cursor}"),
        ))
        .await
        .unwrap();
    let json2 = body_json(resp2).await;
    let items2 = json2["items"].as_array().unwrap();
    assert_eq!(items2.len(), 1);
    assert_eq!(items2[0]["subject"], "second");
    assert_eq!(items2[0]["to"], "bob@friends.com");
}

#[tokio::test]
async fn sent_listing_needs_the_bearer() {
    let Harness { app, .. } = harness(|_, _| {});
    let req = Request::builder()
        .uri("/client/sent")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn pagination_cursor_round_trip() {
    let Harness { app, .. } = harness(|store, acct| {
        // 3 normal signal messages so limit=2 yields a next_cursor.
        for i in 0..3 {
            let g = format!("g{i}");
            let t = format!("t{i}");
            let m = store
                .upsert_message(&msg(acct, &g, &t, "update", "body"))
                .unwrap();
            store
                .set_triage(
                    m,
                    acct,
                    80,
                    Tier::Signal,
                    Sensitivity::Normal,
                    None,
                    "",
                    "",
                    None,
                )
                .unwrap();
        }
    });

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/updates?limit=2"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["items"].as_array().unwrap().len(), 2);
    let cursor = json["next_cursor"]
        .as_str()
        .expect("next_cursor present")
        .to_string();

    // Second page via the cursor.
    let resp2 = app
        .oneshot(authed(
            "GET",
            &format!("/client/updates?limit=2&cursor={cursor}"),
        ))
        .await
        .unwrap();
    let json2 = body_json(resp2).await;
    assert_eq!(json2["items"].as_array().unwrap().len(), 1);
    assert!(json2.get("next_cursor").is_none() || json2["next_cursor"].is_null());
}

#[tokio::test]
async fn keyset_pages_by_arrival_and_refuses_mixed_paging() {
    let Harness { app, .. } = harness(|store, acct| {
        for i in 0..3 {
            let g = format!("k{i}");
            let t = format!("kt{i}");
            let m = store
                .upsert_message(&msg(acct, &g, &t, "update", "body"))
                .unwrap();
            store
                .set_triage(
                    m,
                    acct,
                    80,
                    Tier::Signal,
                    Sensitivity::Normal,
                    None,
                    "",
                    "",
                    None,
                )
                .unwrap();
        }
    });
    // Walk the listing one row at a time by (received_at, id): every row
    // once, newest first, and a keyset page never carries an offset cursor.
    let mut seen = Vec::new();
    let mut before: Option<(String, i64)> = None;
    loop {
        let uri = match &before {
            None => "/client/updates?limit=1&peek=true".to_string(),
            Some((at, id)) => format!(
                "/client/updates?limit=1&peek=true&before={}&before_id={id}",
                at.replace('+', "%2B")
            ),
        };
        let json = body_json(app.clone().oneshot(authed("GET", &uri)).await.unwrap()).await;
        if before.is_some() {
            assert!(json.get("next_cursor").is_none() || json["next_cursor"].is_null());
        }
        let items = json["items"].as_array().unwrap();
        let Some(row) = items.first() else { break };
        let id = row["id"].as_i64().unwrap();
        assert!(!seen.contains(&id), "keyset paging repeated {id}");
        seen.push(id);
        before = Some((row["received_at"].as_str().unwrap().to_string(), id));
        assert!(seen.len() <= 3);
    }
    assert_eq!(seen.len(), 3);

    for bad in [
        "/client/updates?before=2026-01-01T00:00:00Z&cursor=abc",
        "/client/updates?before=2026-01-01T00:00:00Z&band=standing",
        "/client/updates?before=2026-01-01T00:00:00Z&reminders=pending",
    ] {
        let resp = app.clone().oneshot(authed("GET", bad)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{bad}");
    }
}

#[tokio::test]
async fn bad_cursor_is_400() {
    let Harness { app, .. } = harness(|_, _| {});
    let resp = app
        .oneshot(authed("GET", "/client/updates?cursor=@@notbase64@@"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn action_requires_confirm() {
    // The confirm gate fires before anything else.
    let Harness { app, .. } = harness(|_, _| {});
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/archive",
            serde_json::json!({ "message_id": 1 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let json = body_json(resp).await;
    assert!(
        json["error"].as_str().unwrap().contains("confirm"),
        "400 must explain the confirm contract"
    );
}

#[tokio::test]
async fn action_without_write_credential_is_403() {
    // Confirmed, but no write credential configured => 403 with a hint.
    let Harness { app, .. } = harness(|store, acct| {
        let m = store
            .upsert_message(&msg(acct, "g1", "t1", "hi", "body"))
            .unwrap();
        store
            .set_triage(
                m,
                acct,
                80,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
    });
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/archive",
            serde_json::json!({ "message_id": 1, "confirm": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let json = body_json(resp).await;
    assert!(json["error"].as_str().unwrap().contains("squelchd auth"));
}

#[tokio::test]
async fn send_outbound_guard_blocks_and_audits() {
    let Harness { app, store, acct } = harness(|_, _| {});
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "body": "your verification code is 483920",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let json = body_json(resp).await;
    let err = json["error"].as_str().unwrap();
    assert!(err.contains("otp_code"), "422 lists redacted match kinds");
    assert!(
        !err.contains("483920"),
        "must NEVER echo the matched secret"
    );

    // A blocked send still audits.
    let audit = store.list_audit(acct, 10).unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].action, "send");
    assert_eq!(audit[0].actor, "client-api");
    assert_eq!(audit[0].detail.as_deref(), Some("blocked:guard"));
}

#[tokio::test]
async fn send_guard_override_passes_guard_then_403_no_creds() {
    // override_guard bypasses the guard, then the 403 gate stops it. Two audit
    // rows: the override note and the no-credential rejection.
    let Harness { app, store, acct } = harness(|_, _| {});
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "body": "your verification code is 483920",
                "confirm": true,
                "override_guard": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.detail.as_deref() == Some("rejected:no_write_credential"))
    );
    assert!(audit.iter().any(|a| {
        a.detail
            .as_deref()
            .is_some_and(|d| d.starts_with("guard_override:"))
    }));
}

#[tokio::test]
async fn clean_send_passes_guard() {
    // A clean body clears the guard; the 403 for missing creds proves it did
    // not block.
    let Harness { app, .. } = harness(|_, _| {});
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "body": "Hi Alice, sounds great, see you Tuesday. Bob",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "clean body clears the guard; only the missing write credential blocks it"
    );
}

// --- end-to-end action success (through the handler, mock Gmail) ------------

use async_trait::async_trait;
use squelch_core::credentials::{CredentialStore, OAuthToken};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

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

/// Serve `n` sequential requests, each answered `200 {}`, returning the raw
/// request bytes.
async fn mock_gmail(n: usize) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let mut reqs = Vec::new();
        for _ in 0..n {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let m = sock.read(&mut buf).await.unwrap();
            reqs.push(String::from_utf8_lossy(&buf[..m]).to_string());
            let body = "{}";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            sock.flush().await.unwrap();
        }
        reqs
    });
    (format!("http://{addr}"), handle)
}

/// [`mock_gmail`]'s scripted sibling: serve one `(status, body)` per entry, in
/// order. The send path's calls differ in what they must return (ids, then a raw
/// message), and the echo's failure mode is a status, so both are scripted.
async fn mock_gmail_seq(
    responses: Vec<(u16, String)>,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let mut reqs = Vec::new();
        for (status, body) in responses {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let m = sock.read(&mut buf).await.unwrap();
            reqs.push(String::from_utf8_lossy(&buf[..m]).to_string());
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            sock.flush().await.unwrap();
        }
        reqs
    });
    (format!("http://{addr}"), handle)
}

/// The default harness plus a write credential pointed at a mock Gmail `base`.
fn app_with_writes(base: String, seed: impl FnOnce(&SqliteStore, i64)) -> Harness {
    let (state, store, acct) = state_with(seed);
    let state = state.with_write_test_harness(Arc::new(StubCreds), base);
    Harness {
        app: router(state),
        store,
        acct,
    }
}

#[tokio::test]
async fn archive_success_audits_ok_and_hits_gmail() {
    let (base, handle) = mock_gmail(1).await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        let m = store
            .upsert_message(&msg(acct, "gmail-abc", "t1", "hi", "body"))
            .unwrap();
        store
            .set_triage(
                m,
                acct,
                80,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
    });
    let message_id = store.search(acct, "hi", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/archive",
            serde_json::json!({ "message_id": message_id, "confirm": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let reqs = handle.await.unwrap();
    assert_eq!(reqs.len(), 1);
    assert!(reqs[0].contains("/messages/gmail-abc/modify"));
    assert!(reqs[0].contains("\"removeLabelIds\":[\"INBOX\"]"));

    let audit = store.list_audit(acct, 10).unwrap();
    assert_eq!(audit[0].action, "archive");
    assert_eq!(audit[0].actor, "client-api");
    assert_eq!(audit[0].detail.as_deref(), Some("ok"));
}

/// THE WIRING, not the pacer: an archive through the real handler books its
/// Gmail cost against the budget the daemon handed ApiState, the same one the
/// sync engines draw from. Two budgets per mailbox is how a tenant asked Gmail
/// for 160% of its per-user limit (#263); drop `.with_budget` in
/// `write_client` or the `admit` in the write client and this fails.
#[tokio::test]
async fn an_archive_books_against_the_mailbox_gmail_budget() {
    use squelch_core::config::GmailQuotaProfile;
    use squelch_core::gmail_budget::GmailBudget;
    use std::time::{Duration, Instant};

    let (base, handle) = mock_gmail(1).await;
    // 10 ms a unit: modify costs 5, so the archive books 50 ms.
    let budget = Arc::new(GmailBudget::with_unit(
        GmailQuotaProfile::Standard,
        Duration::from_millis(10),
    ));
    let (state, store, acct) = state_with(|store, acct| {
        let m = store
            .upsert_message(&msg(acct, "gmail-budget", "t1", "hi", "body"))
            .unwrap();
        store
            .set_triage(
                m,
                acct,
                80,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
    });
    let app = router(
        state
            .with_write_test_harness(Arc::new(StubCreds), base)
            .with_gmail_budget(budget.clone()),
    );
    let message_id = store.search(acct, "hi", 10, 0).unwrap()[0].id;
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/archive",
            serde_json::json!({ "message_id": message_id, "confirm": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    handle.await.unwrap();

    // The sync side's next request has to wait out what the archive booked.
    let started = Instant::now();
    budget.admit("GET", "/profile").await;
    assert!(
        started.elapsed() >= Duration::from_millis(25),
        "the archive did not book against the shared budget"
    );
}

fn archive_app(
    base: String,
    budget: Arc<squelch_core::gmail_budget::GmailBudget>,
) -> (axum::Router, i64) {
    let (state, store, acct) = state_with(|store, acct| {
        let m = store
            .upsert_message(&msg(acct, "gmail-paced", "t1", "hi", "body"))
            .unwrap();
        store
            .set_triage(
                m,
                acct,
                80,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
    });
    let message_id = store.search(acct, "hi", 10, 0).unwrap()[0].id;
    let app = router(
        state
            .with_write_test_harness(Arc::new(StubCreds), base)
            .with_gmail_budget(budget),
    );
    (app, message_id)
}

/// A quota refusal on a WRITE (a send is the most expensive call there is)
/// slows the mailbox's shared budget, so the sync loop backs off too.
#[tokio::test]
async fn a_quota_refused_archive_slows_the_mailbox_budget() {
    use squelch_core::config::GmailQuotaProfile;
    use squelch_core::gmail_budget::GmailBudget;
    use std::time::Duration;

    let (base, handle) = mock_gmail_seq(vec![(
        403,
        r#"{"error":{"details":[{"reason":"RATE_LIMIT_EXCEEDED"}]}}"#.to_string(),
    )])
    .await;
    let budget = Arc::new(
        GmailBudget::with_unit(GmailQuotaProfile::Standard, Duration::from_micros(10))
            .with_adaptation(Duration::ZERO, Duration::from_secs(3600)),
    );
    let (app, message_id) = archive_app(base, budget.clone());
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/archive",
            serde_json::json!({ "message_id": message_id, "confirm": true }),
        ))
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::OK);
    handle.await.unwrap();
    assert_eq!(budget.unit(), Duration::from_micros(20));
}

/// During a refusal hold a human's archive is answered at once with 503 and
/// when to retry, and nothing is sent to Gmail: it must not sit out a
/// one-minute hold while the app's 15 s request timeout calls it failed.
#[tokio::test]
async fn an_archive_during_a_quota_hold_is_told_to_retry_without_sending() {
    use squelch_core::config::GmailQuotaProfile;
    use squelch_core::gmail_budget::GmailBudget;
    use std::time::{Duration, Instant};

    let (base, _handle) = mock_gmail(1).await;
    let budget = Arc::new(
        GmailBudget::with_unit(GmailQuotaProfile::Standard, Duration::ZERO)
            .with_adaptation(Duration::from_secs(60), Duration::from_secs(3600)),
    );
    budget.note_quota_refusal(None);
    let (app, message_id) = archive_app(base, budget);
    let started = Instant::now();
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/archive",
            serde_json::json!({ "message_id": message_id, "confirm": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "must answer at once, not wait out the hold"
    );
    let body = body_json(resp).await;
    assert!(
        body.to_string().contains("Try again in"),
        "the app should be told when to retry: {body}"
    );
}

#[tokio::test]
async fn human_can_archive_restricted_mail() {
    let (base, handle) = mock_gmail(1).await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        let s = store
            .upsert_message(&msg(acct, "gmail-sealed", "t9", "code", "123456"))
            .unwrap();
        store
            .set_triage(
                s,
                acct,
                90,
                Tier::Noise,
                Sensitivity::Sealed,
                Some(SealedKind::Otp),
                "",
                "",
                None,
            )
            .unwrap();
    });
    let sealed_id = store.sealed_messages(acct).unwrap()[0].id;
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/archive",
            serde_json::json!({ "message_id": sealed_id, "confirm": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(handle.await.unwrap().len(), 1);
    let audit = store.list_audit(acct, 10).unwrap();
    assert_eq!(audit[0].detail.as_deref(), Some("ok"));
}

#[tokio::test]
async fn reply_send_success_threads_and_audits_ok() {
    // Reply flow makes two Gmail calls: parent_headers GET, then send POST.
    let (base, handle) = mock_gmail(2).await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        let m = store
            .upsert_message(&msg(
                acct,
                "gmail-parent",
                "thread-77",
                "Lunch?",
                "want lunch?",
            ))
            .unwrap();
        store
            .set_triage(
                m,
                acct,
                80,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
    });
    let message_id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "reply_to_message_id": message_id,
                "body": "yes, noon works",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let reqs = handle.await.unwrap();
    assert_eq!(reqs.len(), 2);
    assert!(
        reqs[0].starts_with("GET "),
        "first call reads parent headers"
    );
    assert!(reqs[0].contains("/messages/gmail-parent"));
    assert!(reqs[1].contains("/messages/send"));
    // Threaded onto the parent's Gmail thread.
    assert!(reqs[1].contains("\"threadId\":\"thread-77\""));

    // `mock_gmail` answers `{}`, so there is no sent id to echo back.
    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send" && a.detail.as_deref() == Some("ok"))
    );
}

/// The RFC822 Gmail hands back for the echoed reply, base64url-encoded the way
/// `format=raw` returns it.
fn sent_reply_raw_b64() -> String {
    use base64::Engine as _;
    let eml = "From: Me <me@example.com>\r\n\
               To: Alice <alice@example.com>\r\n\
               Subject: Re: Lunch?\r\n\
               Date: Mon, 7 Jul 2026 10:00:00 +0000\r\n\
               Content-Type: text/plain; charset=\"UTF-8\"\r\n\
               \r\n\
               yes, noon works\r\n";
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(eml)
}

#[tokio::test]
async fn reply_send_echoes_the_sent_message_into_the_thread() {
    // Three Gmail calls: parent headers, the send, then the echo's raw read-back.
    let (base, handle) = mock_gmail_seq(vec![
        (200, "{}".to_string()),
        (
            200,
            "{\"id\":\"sent-1\",\"threadId\":\"thread-77\"}".to_string(),
        ),
        (
            200,
            format!(
                "{{\"id\":\"sent-1\",\"threadId\":\"thread-77\",\
                   \"internalDate\":\"1783591200000\",\"raw\":\"{}\"}}",
                sent_reply_raw_b64()
            ),
        ),
    ])
    .await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        let m = store
            .upsert_message(&msg(
                acct,
                "gmail-parent",
                "thread-77",
                "Lunch?",
                "want lunch?",
            ))
            .unwrap();
        store
            .set_triage(
                m,
                acct,
                80,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
    });
    let message_id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;

    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "reply_to_message_id": message_id,
                "body": "yes, noon works",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["status"], "sent");
    assert_eq!(json["thread_id"], "thread-77");
    let echo_id = json["echo_message_id"].as_i64().expect("echoed local id");

    let reqs = handle.await.unwrap();
    assert_eq!(reqs.len(), 3);
    assert!(
        reqs[2].starts_with("GET "),
        "the echo reads the sent message back"
    );
    assert!(reqs[2].contains("/messages/sent-1?format=raw"));

    // The thread view carries the reply immediately, no Sent poll needed.
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/thread/thread-77"))
        .await
        .unwrap();
    let thread = body_json(resp).await;
    let msgs = thread["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2, "parent + echoed reply");
    assert!(
        msgs.iter().any(|m| m["id"].as_i64() == Some(echo_id)
            && m["content"].as_str().unwrap().contains("noon works")),
        "the echoed reply is in the thread"
    );

    // ...and creates no attention noise: sent mail never reaches /client/updates.
    let resp = app.oneshot(authed("GET", "/client/updates")).await.unwrap();
    let updates = body_json(resp).await;
    assert!(
        updates["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|u| u["id"].as_i64() != Some(echo_id)),
        "the echo is not an update"
    );

    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send" && a.detail.as_deref() == Some("ok"))
    );
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send.echo"
                && a.detail.as_deref() == Some(&*format!("ok:{echo_id}"))),
        "the echo audits its local id"
    );
}

#[tokio::test]
async fn send_with_no_echoed_id_skips_the_echo_and_still_succeeds() {
    // Cold send whose response carries no id: nothing to fetch back, so exactly
    // one Gmail call and a null echo id.
    let (base, handle) = mock_gmail_seq(vec![(200, "{}".to_string())]).await;
    let Harness { app, store, acct } = app_with_writes(base, |_, _| {});

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "subject": "Hi",
                "body": "see you Tuesday",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["status"], "sent");
    assert!(json["echo_message_id"].is_null());
    assert!(
        json["thread_id"].is_null(),
        "no parent and no echoed thread"
    );
    assert_eq!(handle.await.unwrap().len(), 1);

    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send.echo" && a.detail.as_deref() == Some("skipped:no_id"))
    );
}

#[tokio::test]
async fn echo_fetch_failure_never_fails_the_send() {
    // The mail is away; a 500 on the read-back costs the local echo, nothing else.
    let (base, handle) = mock_gmail_seq(vec![
        (
            200,
            "{\"id\":\"sent-1\",\"threadId\":\"thread-new\"}".to_string(),
        ),
        (500, "{\"error\":\"backendError\"}".to_string()),
    ])
    .await;
    let Harness { app, store, acct } = app_with_writes(base, |_, _| {});

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "subject": "Hi",
                "body": "see you Tuesday",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a failed echo is not a failed send"
    );
    let json = body_json(resp).await;
    assert!(json["echo_message_id"].is_null());
    // A cold send still reports the thread Gmail created, so the client can open it.
    assert_eq!(json["thread_id"], "thread-new");
    assert_eq!(handle.await.unwrap().len(), 2);

    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send" && a.detail.as_deref() == Some("ok"))
    );
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send.echo" && a.detail.as_deref() == Some("failed:fetch"))
    );
}

/// The RFC822 for a sent reply whose body trips seal detection (a quoted OTP),
/// base64url-encoded the way `format=raw` returns it. The SEND body is innocuous —
/// what seals is the copy Gmail hands back, not what the client typed.
fn sealed_reply_raw_b64() -> String {
    use base64::Engine as _;
    let eml = "From: Me <me@example.com>\r\n\
               To: Support <support@bank.com>\r\n\
               Subject: Re: Your verification code\r\n\
               Date: Mon, 7 Jul 2026 10:00:00 +0000\r\n\
               Content-Type: text/plain; charset=\"UTF-8\"\r\n\
               \r\n\
               I never asked for this. Your one-time passcode is 483920.\r\n";
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(eml)
}

#[tokio::test]
async fn auth_sent_echo_is_stored_and_the_human_thread_still_opens() {
    let (base, handle) = mock_gmail_seq(vec![
        (200, "{}".to_string()),
        (
            200,
            "{\"id\":\"sent-1\",\"threadId\":\"thread-77\"}".to_string(),
        ),
        (
            200,
            format!(
                "{{\"id\":\"sent-1\",\"threadId\":\"thread-77\",\
                   \"internalDate\":\"1783591200000\",\"raw\":\"{}\"}}",
                sealed_reply_raw_b64()
            ),
        ),
    ])
    .await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        let m = store
            .upsert_message(&msg(
                acct,
                "gmail-parent",
                "thread-77",
                "Lunch?",
                "want lunch?",
            ))
            .unwrap();
        store
            .set_triage(
                m,
                acct,
                80,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
    });
    let message_id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;

    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "reply_to_message_id": message_id,
                "body": "please look into this",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a skipped echo is not a failed send"
    );
    let json = body_json(resp).await;
    assert_eq!(json["status"], "sent");
    assert!(
        json["echo_message_id"].is_number(),
        "the sent copy is available to the human before access assessment"
    );
    assert_eq!(handle.await.unwrap().len(), 3);

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/thread/thread-77"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "the thread must not 404");
    let thread = body_json(resp).await;
    assert_eq!(thread["messages"].as_array().map(Vec::len), Some(2));
    assert!(store.sealed_messages(acct).unwrap().is_empty());

    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send" && a.detail.as_deref() == Some("ok"))
    );
    assert!(audit.iter().any(|a| {
        a.action == "send.echo"
            && a.detail
                .as_deref()
                .is_some_and(|detail| detail.starts_with("ok:"))
    }));
}

#[tokio::test]
async fn echo_of_an_empty_raw_read_never_ingests() {
    // A `format=raw` 200 with no `raw` field decodes to zero bytes, which would
    // ingest as a message with neither headers nor body. It is a failed fetch.
    let (base, handle) = mock_gmail_seq(vec![
        (
            200,
            "{\"id\":\"sent-1\",\"threadId\":\"thread-new\"}".to_string(),
        ),
        (
            200,
            "{\"id\":\"sent-1\",\"threadId\":\"thread-new\"}".to_string(),
        ),
    ])
    .await;
    let Harness { app, store, acct } = app_with_writes(base, |_, _| {});

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "subject": "Hi",
                "body": "see you Tuesday",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert!(json["echo_message_id"].is_null());
    assert_eq!(handle.await.unwrap().len(), 2);

    // Nothing was written: no local message, sent or otherwise.
    let stats = store
        .stats(acct, chrono::Utc::now() - chrono::Duration::days(30))
        .unwrap();
    assert_eq!(
        (stats.total, stats.sealed),
        (0, 0),
        "empty bytes must not become a message row"
    );
    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send.echo" && a.detail.as_deref() == Some("failed:fetch"))
    );
}

// --- read tracking on the send path -----------------------------------------

/// A public base URL for the pixel; nothing is ever fetched from it in tests.
const TRACK_BASE: &str = "https://track.example.test";

/// [`app_with_writes`] plus a configured `[tracking] base_url`.
fn app_with_tracking(
    base: String,
    track_base: Option<&str>,
    seed: impl FnOnce(&SqliteStore, i64),
) -> Harness {
    let (state, store, acct) = state_with(seed);
    let state = state
        .with_write_test_harness(Arc::new(StubCreds), base)
        .with_tracking_base_url(track_base.map(str::to_string));
    Harness {
        app: router(state),
        store,
        acct,
    }
}

/// The RFC822 a captured `messages/send` request carried, decoded out of its
/// `raw` field.
fn sent_mime(req: &str) -> String {
    use base64::Engine as _;
    let start = req.find("\"raw\":\"").expect("send body carries raw") + 7;
    let end = start + req[start..].find('"').expect("raw is terminated");
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&req[start..end])
        .expect("raw is base64url");
    String::from_utf8(bytes).expect("mime is utf-8")
}

/// The token out of the one pixel URL in a built message.
fn pixel_token(mime: &str) -> String {
    let marker = format!("<img src=\"{TRACK_BASE}/t/");
    let start = mime.find(&marker).expect("the html part carries a pixel") + marker.len();
    let end = start + mime[start..].find('"').expect("the src is terminated");
    mime[start..end].to_string()
}

#[tokio::test]
async fn include_tracker_mints_a_pixel_the_store_can_resolve() {
    // Cold send: the send POST, then the echo's raw read-back.
    let (base, handle) = mock_gmail_seq(vec![
        (
            200,
            "{\"id\":\"sent-1\",\"threadId\":\"thread-new\"}".to_string(),
        ),
        (
            200,
            format!(
                "{{\"id\":\"sent-1\",\"threadId\":\"thread-new\",\
                   \"internalDate\":\"1783591200000\",\"raw\":\"{}\"}}",
                sent_reply_raw_b64()
            ),
        ),
    ])
    .await;
    let Harness { app, store, acct } = app_with_tracking(base, Some(TRACK_BASE), |_, _| {});

    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "subject": "Hi",
                "body": "see you Tuesday",
                "confirm": true,
                "include_tracker": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let echo_id = body_json(resp).await["echo_message_id"]
        .as_i64()
        .expect("echoed local id");

    let reqs = handle.await.unwrap();
    let mime = sent_mime(&reqs[0]);
    // An interactive send is not a scheduled one: Gmail mints its id, and it
    // carries no outbox token.
    assert!(!mime.contains("X-Passband-Outbox") && !mime.contains("Message-ID:"));
    // The pixel rides the HTML alternative only; the plain part stays clean.
    assert!(mime.contains("multipart/alternative"));
    let token = pixel_token(&mime);
    assert_eq!(token.len(), 32, "32 url-safe chars");
    assert_eq!(
        mime.matches("<img").count(),
        1,
        "exactly one pixel, in the html part"
    );

    // The tracker is REAL: an open against its token records, and the echo
    // backfill makes it readable per message.
    assert!(
        store
            .record_open(acct, &token, 1_700, Some("Apple Mail/16.0"), "unknown")
            .unwrap(),
        "the minted token names a stored tracker"
    );
    let opens = store.message_opens(acct, echo_id).unwrap();
    assert_eq!(opens.len(), 1);
    assert_eq!(opens[0].opened_at, 1_700);

    let audit = store.list_audit(acct, 20).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send.tracker" && a.detail.as_deref() == Some("minted"))
    );
    // The audit trail must never carry the capability itself.
    assert!(
        !audit
            .iter()
            .any(|a| a.detail.as_deref().is_some_and(|d| d.contains(&token))),
        "the token is never audited"
    );
}

#[tokio::test]
async fn include_tracker_without_a_configured_base_url_sends_untracked() {
    let (base, handle) = mock_gmail_seq(vec![
        (
            200,
            "{\"id\":\"sent-1\",\"threadId\":\"thread-new\"}".to_string(),
        ),
        (200, "{}".to_string()),
    ])
    .await;
    let Harness { app, store, acct } = app_with_tracking(base, None, |_, _| {});

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "subject": "Hi",
                "body": "see you Tuesday",
                "confirm": true,
                "include_tracker": true
            }),
        ))
        .await
        .unwrap();
    // Unconfigured is not an error: the mail goes out, just unwatched.
    assert_eq!(resp.status(), StatusCode::OK);

    let reqs = handle.await.unwrap();
    let mime = sent_mime(&reqs[0]);
    assert!(!mime.contains("<img"), "no pixel without a base url");
    assert!(
        mime.contains("Content-Type: text/plain"),
        "and no html part was invented to carry one"
    );
    assert!(!mime.contains("multipart/alternative"));
    assert!(
        !store
            .list_audit(acct, 20)
            .unwrap()
            .iter()
            .any(|a| a.action == "send.tracker"),
        "nothing was minted, so nothing is audited"
    );
}

/// The tracker row is written BEFORE the send, so a token can never reach the
/// wire unrecorded. The cost of that ordering is an orphan whenever the send
/// fails — a row nothing will ever link to a message, still accepting opens for
/// the life of the database. The failure path has to take it back.
#[tokio::test]
async fn a_failed_send_leaves_no_tracker_behind() {
    let (base, handle) = mock_gmail_seq(vec![(500, "{\"error\":\"backend\"}".to_string())]).await;
    let Harness { app, store, acct } = app_with_tracking(base, Some(TRACK_BASE), |_, _| {});

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "subject": "Hi",
                "body": "see you Tuesday",
                "confirm": true,
                "include_tracker": true
            }),
        ))
        .await
        .unwrap();
    assert!(!resp.status().is_success());

    // The token that WAS minted, read back off the wire the recipient never saw.
    let reqs = handle.await.unwrap();
    let token = pixel_token(&sent_mime(&reqs[0]));
    assert!(
        !store
            .record_open(
                acct,
                &token,
                chrono::Utc::now().timestamp(),
                None,
                "unknown"
            )
            .unwrap(),
        "the tracker outlived its failed send"
    );

    let audit = store.list_audit(acct, 20).unwrap();
    let details = |action: &str| -> Vec<String> {
        audit
            .iter()
            .filter(|a| a.action == action)
            .filter_map(|a| a.detail.clone())
            .collect()
    };
    assert!(details("send.tracker").contains(&"minted".to_string()));
    assert!(details("send.tracker").contains(&"discarded".to_string()));
    assert!(details("send").contains(&"failed:gmail".to_string()));
}

#[tokio::test]
async fn a_send_that_does_not_ask_for_tracking_never_gets_a_pixel() {
    let (base, handle) = mock_gmail_seq(vec![
        (
            200,
            "{\"id\":\"sent-1\",\"threadId\":\"thread-new\"}".to_string(),
        ),
        (200, "{}".to_string()),
    ])
    .await;
    // Tracking IS configured; the send simply does not ask for it.
    let Harness { app, store, acct } = app_with_tracking(base, Some(TRACK_BASE), |_, _| {});

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "subject": "Hi",
                "body": "see you Tuesday",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let reqs = handle.await.unwrap();
    let mime = sent_mime(&reqs[0]);
    assert!(!mime.contains("<img"));
    assert!(!mime.contains(TRACK_BASE));
    assert!(!mime.contains("multipart/alternative"));
    assert!(
        !store
            .list_audit(acct, 20)
            .unwrap()
            .iter()
            .any(|a| a.action == "send.tracker")
    );
}

// --- reply-all: recipient preview + the Cc on the wire ----------------------

/// A `format=metadata` response whose headers are a group thread: Alice wrote
/// to the account, Bob and Carol, and Cc'd Dave. Display names carry commas and
/// a CRLF injection attempt, because that is what the derivation has to survive.
fn group_metadata_body() -> String {
    serde_json::json!({
        "id": "gmail-parent",
        "threadId": "thread-77",
        "payload": { "headers": [
            { "name": "Message-ID", "value": "<parent@x>" },
            { "name": "References", "value": "<root@x>" },
            { "name": "From", "value": "\"Doe, Alice\" <alice@example.com>" },
            // The account is listed in a DIFFERENT case than the accounts row,
            // and one display name tries to smuggle a header through a CRLF.
            { "name": "To", "value":
                "Me <ME@Example.com>, \"evil\r\nBcc: attacker@evil.test\" <bob@example.com>" },
            { "name": "Cc", "value": "Carol <carol@example.com>, Dave <dave@example.com>" },
        ]}
    })
    .to_string()
}

/// A parent whose Reply-To routes replies somewhere other than its From — a
/// mailing list, the common case Reply-To exists for at all.
fn reply_to_metadata_body() -> String {
    serde_json::json!({
        "id": "gmail-parent",
        "threadId": "thread-77",
        "payload": { "headers": [
            { "name": "Message-ID", "value": "<parent@x>" },
            { "name": "From", "value": "Alice <alice@example.com>" },
            { "name": "Reply-To", "value": "list@example.com" },
            { "name": "To", "value": "me@example.com" },
        ]}
    })
    .to_string()
}

#[tokio::test]
async fn a_plain_reply_previews_and_sends_the_same_reply_to_address() {
    // The bug this pins: the preview honored Reply-To while the send used the
    // stored From, so a user reviewed a list address and mailed an individual.
    // Both legs now derive, so they agree. One fetch each.
    let (pbase, phandle) = mock_gmail_seq(vec![(200, reply_to_metadata_body())]).await;
    let Harness { app, store, acct } = app_with_writes(pbase, |store, acct| {
        seed_one_signal(store, acct, "gmail-parent", "thread-77", "Lunch?");
    });
    let id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;

    let preview = app
        .oneshot(authed(
            "GET",
            &format!("/client/messages/{id}/reply_recipients"),
        ))
        .await
        .unwrap();
    let json = body_json(preview).await;
    assert_eq!(json["to"], "list@example.com", "preview honors Reply-To");
    phandle.await.unwrap();

    let (sbase, shandle) = mock_gmail_seq(vec![
        (200, reply_to_metadata_body()),
        (200, "{}".to_string()),
    ])
    .await;
    let Harness { app, store, acct } = app_with_writes(sbase, |store, acct| {
        seed_one_signal(store, acct, "gmail-parent", "thread-77", "Lunch?");
    });
    let id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "reply_to_message_id": id,
                "body": "noon works",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mime = sent_mime(&shandle.await.unwrap()[1]);
    assert!(
        mime.contains("To: list@example.com\r\n"),
        "the send honors Reply-To too: {mime}"
    );
}

#[tokio::test]
async fn reply_recipients_preview_answers_sender_only_by_default() {
    let (base, handle) = mock_gmail_seq(vec![(200, group_metadata_body())]).await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(store, acct, "gmail-parent", "thread-77", "Lunch?");
    });
    let id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed(
            "GET",
            &format!("/client/messages/{id}/reply_recipients"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["to"], "alice@example.com");
    // No cc at all on a plain reply — the field is omitted, not empty.
    assert!(json.get("cc").is_none(), "{json}");

    let reqs = handle.await.unwrap();
    assert_eq!(
        reqs.len(),
        1,
        "the preview costs exactly one metadata fetch"
    );
    assert!(reqs[0].starts_with("GET "));
    assert!(reqs[0].contains("format=metadata"));
}

#[tokio::test]
async fn reply_recipients_preview_with_all_lists_the_room_minus_the_account() {
    let (base, handle) = mock_gmail_seq(vec![(200, group_metadata_body())]).await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(store, acct, "gmail-parent", "thread-77", "Lunch?");
    });
    let id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed(
            "GET",
            &format!("/client/messages/{id}/reply_recipients?all=true"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    handle.await.unwrap();
    assert_eq!(json["to"], "alice@example.com");
    // Bare addresses only, the account itself dropped, To before Cc.
    assert_eq!(
        json["cc"],
        "bob@example.com, carol@example.com, dave@example.com"
    );
    let wire = json.to_string();
    assert!(!wire.contains("evil.test"), "{wire}");
    assert!(!wire.contains("Doe"), "display names never survive: {wire}");
}

#[tokio::test]
async fn human_can_preview_recipients_for_restricted_mail() {
    let (base, handle) = mock_gmail(2).await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        let s = store
            .upsert_message(&msg(acct, "gmail-sealed", "t9", "code", "123456"))
            .unwrap();
        store
            .set_triage(
                s,
                acct,
                90,
                Tier::Noise,
                Sensitivity::Sealed,
                Some(SealedKind::Otp),
                "",
                "",
                None,
            )
            .unwrap();
    });
    let sealed_id = store.sealed_messages(acct).unwrap()[0].id;

    for uri in [
        format!("/client/messages/{sealed_id}/reply_recipients"),
        format!("/client/messages/{sealed_id}/reply_recipients?all=true"),
        "/client/messages/999999/reply_recipients".to_string(),
    ] {
        let resp = app.clone().oneshot(authed("GET", &uri)).await.unwrap();
        assert_eq!(
            resp.status(),
            if uri.contains("999999") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::OK
            },
            "{uri}"
        );
    }
    assert_eq!(handle.await.unwrap().len(), 2);
}

#[tokio::test]
async fn reply_recipients_preview_needs_the_bearer_and_a_write_credential() {
    // No write credential: a 403 the client can show, not a 500.
    let Harness { app, store, acct } = harness(|store, acct| {
        seed_one_signal(store, acct, "gmail-parent", "thread-77", "Lunch?");
    });
    let id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;
    let resp = app
        .clone()
        .oneshot(authed(
            "GET",
            &format!("/client/messages/{id}/reply_recipients"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // And the route is behind the bearer like everything else in the tree.
    let req = Request::builder()
        .uri(format!("/client/messages/{id}/reply_recipients"))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn reply_recipients_preview_surfaces_an_upstream_failure_as_502() {
    let (base, handle) = mock_gmail_seq(vec![(500, "{\"error\":\"backend\"}".to_string())]).await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(store, acct, "gmail-parent", "thread-77", "Lunch?");
    });
    let id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;
    let resp = app
        .oneshot(authed(
            "GET",
            &format!("/client/messages/{id}/reply_recipients"),
        ))
        .await
        .unwrap();
    handle.await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn reply_all_send_addresses_the_room_off_one_metadata_fetch() {
    let (base, handle) =
        mock_gmail_seq(vec![(200, group_metadata_body()), (200, "{}".to_string())]).await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(store, acct, "gmail-parent", "thread-77", "Lunch?");
    });
    let id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "reply_to_message_id": id,
                "body": "noon works",
                "reply_all": true,
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let reqs = handle.await.unwrap();
    // TWO calls, not three: the headers fetch that threads the reply is the same
    // one that derived its audience.
    assert_eq!(reqs.len(), 2);
    assert!(reqs[0].starts_with("GET ") && reqs[0].contains("format=metadata"));
    let mime = sent_mime(&reqs[1]);
    assert!(mime.contains("To: alice@example.com\r\n"), "{mime}");
    assert!(
        mime.contains("Cc: bob@example.com, carol@example.com, dave@example.com\r\n"),
        "{mime}"
    );
    // Threading still comes off the same fetch.
    assert!(mime.contains("In-Reply-To: <parent@x>\r\n"));
    assert!(mime.contains("References: <root@x> <parent@x>\r\n"));
    // Nothing the crafted display name carried reached the headers.
    let headers = mime.split("\r\n\r\n").next().unwrap();
    assert!(!headers.contains("Bcc"), "{headers}");
    assert!(!headers.contains("evil.test"), "{headers}");
    assert!(
        !headers.to_lowercase().contains("me@example.com"),
        "the account never copies itself: {headers}"
    );

    // The ledger records a reply-all AS one, with its recipient count (to +
    // cc = 1 + 3), so a wide send is never filed as a plain "ok".
    assert!(
        store
            .list_audit(acct, 20)
            .unwrap()
            .iter()
            .any(|a| a.action == "send" && a.detail.as_deref() == Some("ok:reply_all:4"))
    );
}

#[tokio::test]
async fn an_ordinary_reply_still_carries_no_cc() {
    let (base, handle) =
        mock_gmail_seq(vec![(200, group_metadata_body()), (200, "{}".to_string())]).await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(store, acct, "gmail-parent", "thread-77", "Lunch?");
    });
    let id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "reply_to_message_id": id,
                "body": "noon works",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let reqs = handle.await.unwrap();
    let mime = sent_mime(&reqs[1]);
    assert!(!mime.contains("Cc:"), "{mime}");
    // A plain reply now DERIVES too (so the preview route and the send agree),
    // but with no Reply-To in the fixture the derived To is just the From
    // address — same result, one recipient, no Cc.
    assert!(mime.contains("To: alice@example.com\r\n"), "{mime}");
}

/// A BCC-ONLY COLD SEND: the shape of a blind blast. It is not a send with no
/// recipient, so it must not be refused — the `To` falls back to the sender.
#[tokio::test]
async fn a_bcc_only_send_addresses_the_sender_and_blind_copies_the_rest() {
    let (base, handle) = mock_gmail_seq(vec![(200, "{}".to_string())]).await;
    let Harness { app, .. } = app_with_writes(base, |_, _| {});

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "bcc": "ann@fund.com, bo@fund.com",
                "subject": "Update #3",
                "body": "we closed the round",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let mime = sent_mime(&handle.await.unwrap()[0]);
    assert!(
        mime.contains("To: me@example.com\r\n"),
        "the account's own address is the visible recipient: {mime}"
    );
    assert!(
        mime.contains("Bcc: ann@fund.com, bo@fund.com\r\n"),
        "{mime}"
    );
    assert!(!mime.contains("Cc:"), "a bcc send invents no Cc: {mime}");
}

/// Nobody visibly on the mail may also receive a blind copy of it — that is two
/// deliveries of one message, the second apparently secret.
#[tokio::test]
async fn a_bcc_never_duplicates_someone_already_visible() {
    let (base, handle) = mock_gmail_seq(vec![(200, "{}".to_string())]).await;
    let Harness { app, .. } = app_with_writes(base, |_, _| {});

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "Ann <ann@fund.com>",
                "bcc": "ANN@fund.com, cy@fund.com",
                "subject": "Update #3",
                "body": "we closed the round",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let mime = sent_mime(&handle.await.unwrap()[0]);
    assert!(mime.contains("To: Ann <ann@fund.com>\r\n"), "{mime}");
    assert!(
        mime.contains("Bcc: cy@fund.com\r\n"),
        "Ann was already visible, so her blind copy is dropped: {mime}"
    );
}

/// A blind audience is invisible in the mail itself, so the ledger is the only
/// place it is written down — as a COUNT, never as addresses.
#[tokio::test]
async fn a_bcc_send_audits_its_reach_without_naming_anyone() {
    let (base, handle) = mock_gmail_seq(vec![(200, "{}".to_string())]).await;
    let Harness { app, store, acct } = app_with_writes(base, |_, _| {});

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "bcc": "ann@fund.com, bo@fund.com, cy@fund.com",
                "subject": "Update #3",
                "body": "we closed the round",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    handle.await.unwrap();

    let entries = store.list_audit(acct, 20).unwrap();
    assert!(
        entries
            .iter()
            .any(|a| a.action == "send" && a.detail.as_deref() == Some("ok:bcc:3")),
        "expected the reach in the ledger: {entries:?}"
    );
    let rendered = serde_json::to_string(&entries).unwrap();
    assert!(
        !rendered.contains("fund.com"),
        "the ledger must not name a blind audience: {rendered}"
    );
}

/// An empty `bcc` changes nothing: the send is byte-identical to what it was
/// before the field existed.
#[tokio::test]
async fn an_empty_bcc_writes_no_header() {
    let (base, handle) = mock_gmail_seq(vec![(200, "{}".to_string())]).await;
    let Harness { app, .. } = app_with_writes(base, |_, _| {});

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "ann@fund.com",
                "bcc": "   ",
                "subject": "hi",
                "body": "hello",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mime = sent_mime(&handle.await.unwrap()[0]);
    assert!(!mime.contains("Bcc:"), "{mime}");
}

/// A send with neither `to` nor `bcc` nor a parent is still refused: the bcc
/// fallback is a fallback for a blind audience, not a way to compose mail
/// addressed to nobody.
#[tokio::test]
async fn a_send_with_no_audience_at_all_is_still_refused() {
    let (base, _handle) = mock_gmail_seq(vec![]).await;
    let Harness { app, .. } = app_with_writes(base, |_, _| {});

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({"subject": "hi", "body": "hello", "confirm": true}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// A `to`/`bcc` group went out as ONE message the composer addressed itself, so
/// the daemon's only job is attribution — and that is what turns the history
/// entry from inferred into exact.
#[tokio::test]
async fn a_bcc_group_send_is_recorded_against_the_group() {
    let (base, handle) = mock_gmail_seq(vec![(200, "{}".to_string())]).await;
    let Harness { app, store, acct } = app_with_writes(base, |_, _| {});
    let group = store
        .create_send_group(
            acct,
            "Preseed Investors",
            squelch_core::types::GroupMode::Bcc,
            "",
            &[
                squelch_core::store::sqlite::groups::NewGroupMember {
                    addr: "ann@fund.com".into(),
                    display_name: None,
                },
                squelch_core::store::sqlite::groups::NewGroupMember {
                    addr: "bo@fund.com".into(),
                    display_name: None,
                },
            ],
        )
        .unwrap();

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "group_id": group,
                "bcc": "ann@fund.com, bo@fund.com",
                "subject": "Update #3",
                "body": "we closed the round",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    handle.await.unwrap();

    let history = store.group_history(acct, group, 50, 0).unwrap();
    assert_eq!(history.len(), 1, "the send should be recorded: {history:?}");
    assert!(history[0].group_send_id.is_some());
    assert_eq!(history[0].reached, 2);
    assert_eq!(history[0].group_size, 2);
    assert_eq!(history[0].subject, "Update #3");
}

/// An UNRESOLVED group token must be loud. `parse_addr_list` would drop it as
/// unemittable and send to everyone else on the line, silently missing the
/// audience the sender believed they had addressed.
#[tokio::test]
async fn an_unresolved_group_token_is_refused_rather_than_dropped() {
    let (base, _handle) = mock_gmail_seq(vec![]).await;
    let Harness { app, .. } = app_with_writes(base, |_, _| {});

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "#preseed investors, ann@fund.com",
                "subject": "Update #3",
                "body": "we closed the round",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("could not be resolved"),
        "{body}"
    );
}

/// Blasting a list into somebody else's thread is not a thing the composer can
/// express, so it is not a thing the daemon invents a meaning for.
#[tokio::test]
async fn a_group_cannot_be_addressed_by_a_reply() {
    let (base, _handle) = mock_gmail_seq(vec![]).await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(store, acct, "gmail-parent", "thread-77", "Lunch?");
    });
    let id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "reply_to_message_id": id,
                "group_id": 1,
                "body": "hi",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Another account's group id and an unknown one are the same 404, and neither
/// costs a Gmail call.
#[tokio::test]
async fn an_unknown_group_id_is_404_before_any_send() {
    let (base, _handle) = mock_gmail_seq(vec![]).await;
    let Harness { app, .. } = app_with_writes(base, |_, _| {});

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "group_id": 9999,
                "to": "ann@fund.com",
                "subject": "hi",
                "body": "hello",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// A FAN-OUT answers 202-shaped: the batch id, not a sent message. Every
/// recipient is recorded PENDING before any mail moves, so a daemon that dies
/// mid-batch leaves a record of an audience it had not reached rather than no
/// record at all.
#[tokio::test]
async fn a_fan_out_records_every_recipient_pending_before_it_sends() {
    // The job's sends race this assertion, so the mock is scripted generously
    // and the assertion is about what the ROUTE wrote, synchronously, before
    // returning.
    let (base, _handle) = mock_gmail_seq(vec![
        (200, "{}".to_string()),
        (200, "{}".to_string()),
        (200, "{}".to_string()),
    ])
    .await;
    let Harness { app, store, acct } = app_with_writes(base, |_, _| {});
    let group = store
        .create_send_group(
            acct,
            "Preseed Investors",
            squelch_core::types::GroupMode::Individual,
            "",
            &["ann@fund.com", "bo@fund.com", "cy@fund.com"].map(|addr| {
                squelch_core::store::sqlite::groups::NewGroupMember {
                    addr: addr.into(),
                    display_name: None,
                }
            }),
        )
        .unwrap();

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "group_id": group,
                "subject": "Update #3",
                "body": "we closed the round",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["status"], "sending");
    assert_eq!(body["recipients"], 3);
    assert!(body["group_send_id"].as_i64().is_some(), "{body}");

    // The batch exists the moment the route returns, with the whole audience on
    // it — whatever the job has or has not got to by now.
    let history = store.group_history(acct, group, 50, 0).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].group_size, 3);
    assert_eq!(history[0].mode, squelch_core::types::GroupMode::Individual);
    assert_eq!(
        history[0].reached + history[0].pending + history[0].failed,
        3,
        "every recipient is accounted for in some state: {history:?}"
    );
}

/// A fan-out is one-to-one BY CONSTRUCTION — that is the whole reason to pick it
/// over bcc — so no message it produces may carry a second recipient.
#[tokio::test]
async fn every_message_in_a_fan_out_names_exactly_one_person() {
    let (base, handle) =
        mock_gmail_seq(vec![(200, "{}".to_string()), (200, "{}".to_string())]).await;
    let Harness { app, store, acct } = app_with_writes(base, |_, _| {});
    let group = store
        .create_send_group(
            acct,
            "Pair",
            squelch_core::types::GroupMode::Individual,
            "",
            &["ann@fund.com", "bo@fund.com"].map(|addr| {
                squelch_core::store::sqlite::groups::NewGroupMember {
                    addr: addr.into(),
                    display_name: None,
                }
            }),
        )
        .unwrap();

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "group_id": group,
                "subject": "Update #3",
                "body": "we closed the round",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let reqs = handle.await.unwrap();
    assert_eq!(reqs.len(), 2, "one message per member");
    let mut addressed: Vec<String> = Vec::new();
    for req in &reqs {
        let mime = sent_mime(req);
        assert!(!mime.contains("Cc:"), "a fan-out invents no Cc: {mime}");
        assert!(!mime.contains("Bcc:"), "a fan-out invents no Bcc: {mime}");
        let to = mime
            .lines()
            .find(|l| l.starts_with("To: "))
            .expect("every message names a recipient")
            .trim_start_matches("To: ")
            .to_string();
        assert!(!to.contains(','), "one recipient per message, got {to}");
        addressed.push(to);
    }
    addressed.sort();
    assert_eq!(addressed, vec!["ann@fund.com", "bo@fund.com"]);
}

/// An empty group cannot be addressed: a "send" that reaches nobody is a
/// mistake, and it costs nothing to say so before a token is minted.
#[tokio::test]
async fn an_empty_group_is_refused() {
    let (base, _handle) = mock_gmail_seq(vec![]).await;
    let Harness { app, store, acct } = app_with_writes(base, |_, _| {});
    let group = store
        .create_send_group(
            acct,
            "Nobody",
            squelch_core::types::GroupMode::Individual,
            "",
            &[],
        )
        .unwrap();

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "group_id": group,
                "subject": "hi",
                "body": "hello",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert!(
        body["error"].as_str().unwrap().contains("nobody in it"),
        "{body}"
    );
}

#[tokio::test]
async fn an_explicit_to_wins_but_keeps_the_derived_copies() {
    let (base, handle) =
        mock_gmail_seq(vec![(200, group_metadata_body()), (200, "{}".to_string())]).await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(store, acct, "gmail-parent", "thread-77", "Lunch?");
    });
    let id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "reply_to_message_id": id,
                "to": "bob@example.com",
                "body": "noon works",
                "reply_all": true,
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mime = sent_mime(&handle.await.unwrap()[1]);
    assert!(mime.contains("To: bob@example.com\r\n"), "{mime}");
    // Bob was promoted out of the Cc rather than being written twice.
    assert!(
        mime.contains("Cc: carol@example.com, dave@example.com\r\n"),
        "{mime}"
    );
}

#[tokio::test]
async fn a_stated_cc_replaces_the_derived_one_and_an_empty_one_clears_it() {
    // The composer that SHOWS a Cc field has to be able to empty it. Absent
    // means derive (the pre-field wire); present — `""` included — means the
    // caller is stating the whole copy list.
    for (stated, expected) in [
        (
            serde_json::json!("erin@example.com"),
            Some("Cc: erin@example.com\r\n"),
        ),
        (serde_json::json!(""), None),
    ] {
        let (base, handle) =
            mock_gmail_seq(vec![(200, group_metadata_body()), (200, "{}".to_string())]).await;
        let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
            seed_one_signal(store, acct, "gmail-parent", "thread-77", "Lunch?");
        });
        let id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;
        let resp = app
            .oneshot(authed_json(
                "POST",
                "/client/actions/send",
                serde_json::json!({
                    "reply_to_message_id": id,
                    "body": "noon works",
                    "reply_all": true,
                    "cc": stated,
                    "confirm": true
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let mime = sent_mime(&handle.await.unwrap()[1]);
        match expected {
            Some(line) => assert!(mime.contains(line), "{mime}"),
            None => assert!(
                !mime.contains("Cc:"),
                "an emptied Cc must stay empty: {mime}"
            ),
        }
        // The parent's own copies are never put back either way.
        assert!(!mime.contains("carol@example.com"), "{mime}");
        assert!(!mime.contains("dave@example.com"), "{mime}");
    }
}

#[tokio::test]
async fn a_stated_cc_keeps_the_display_names_the_sender_typed() {
    // Verbatim, not through `cc_excluding`: that parses to bare addresses, and
    // running a STATED list through it would quietly delete the names.
    let (base, handle) =
        mock_gmail_seq(vec![(200, group_metadata_body()), (200, "{}".to_string())]).await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(store, acct, "gmail-parent", "thread-77", "Lunch?");
    });
    let id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "reply_to_message_id": id,
                "body": "noon works",
                "cc": "Bob Smith <bob@example.com>",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mime = sent_mime(&handle.await.unwrap()[1]);
    assert!(
        mime.contains("Cc: Bob Smith <bob@example.com>\r\n"),
        "{mime}"
    );
}

#[tokio::test]
async fn a_bcc_reaches_the_header_and_the_ledger_counts_it() {
    let (base, handle) =
        mock_gmail_seq(vec![(200, group_metadata_body()), (200, "{}".to_string())]).await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(store, acct, "gmail-parent", "thread-77", "Lunch?");
    });
    let id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "reply_to_message_id": id,
                "body": "noon works",
                "cc": "bob@example.com",
                "bcc": "erin@example.com, frank@example.com",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mime = sent_mime(&handle.await.unwrap()[1]);
    assert!(
        mime.contains("Bcc: erin@example.com, frank@example.com\r\n"),
        "{mime}"
    );
    // THE LEDGER IS THE ONLY PLACE A BCC IS WRITTEN DOWN: the copies that reach
    // the visible recipients carry no trace of it, so the audit line has to.
    // Counts, never addresses — and a stated `cc` gets no line of its own,
    // because it is legible in the delivered mail.
    let detail = store
        .list_audit(acct, 20)
        .unwrap()
        .into_iter()
        .find(|a| a.action == "send")
        .and_then(|a| a.detail);
    assert_eq!(detail.as_deref(), Some("ok:bcc:2"));
    assert!(
        !detail.unwrap().contains("erin"),
        "the ledger counts recipients, it does not name them"
    );
}

#[tokio::test]
async fn a_smuggled_header_never_reaches_the_wire_from_either_copy_list() {
    // TWO DIFFERENT GUARDS, because the two lists take different paths.
    //
    // A `bcc` is filtered through `addrs_excluding` first, which parses to bare
    // addresses and CUTS the value at an embedded header token — so the honest
    // prefix survives and the smuggled line is simply gone. Fewer recipients,
    // never a forged one.
    let (base, handle) =
        mock_gmail_seq(vec![(200, group_metadata_body()), (200, "{}".to_string())]).await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(store, acct, "gmail-parent", "thread-77", "Lunch?");
    });
    let id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "reply_to_message_id": id,
                "body": "noon works",
                "bcc": "erin@example.com\r\nBcc: attacker@evil.test",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mime = sent_mime(&handle.await.unwrap()[1]);
    let headers = mime.split("\r\n\r\n").next().unwrap();
    assert!(headers.contains("Bcc: erin@example.com\r\n"), "{headers}");
    assert!(
        !mime.contains("evil.test"),
        "the smuggled recipient must not exist anywhere: {mime}"
    );

    // A STATED `cc` is taken verbatim — that is what preserves the display names
    // the sender typed — so the MIME builder's CR/LF refusal is the whole of the
    // guard on that path, and it refuses rather than repairs.
    let (base, handle) = mock_gmail_seq(vec![(200, group_metadata_body())]).await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(store, acct, "gmail-parent", "thread-77", "Lunch?");
    });
    let id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "reply_to_message_id": id,
                "body": "noon works",
                "cc": "bob@example.com\r\nBcc: attacker@evil.test",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    // Only the metadata fetch happened; nothing was sent.
    assert_eq!(handle.await.unwrap().len(), 1);
    assert!(
        store
            .list_audit(acct, 20)
            .unwrap()
            .iter()
            .any(|a| a.action == "send" && a.detail.as_deref() == Some("rejected:compose"))
    );
}

#[tokio::test]
async fn reply_all_without_a_parent_is_a_400() {
    let (base, handle) = mock_gmail(0).await;
    let Harness { app, store, acct } = app_with_writes(base, |_, _| {});
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "subject": "Hi",
                "body": "hello",
                "reply_all": true,
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    handle.abort();
    let audit = store.list_audit(acct, 10).unwrap();
    assert_eq!(audit[0].action, "send");
    assert_eq!(
        audit[0].detail.as_deref(),
        Some("rejected:reply_all_no_parent")
    );
}

/// Reply-all's whole point is the audience. If the headers it comes from cannot
/// be read, quietly sending to the sender alone would deliver to fewer people
/// than the user reviewed — so the send fails instead. (A plain reply keeps its
/// best-effort threading: see `reply_send_survives_a_headers_failure`.)
#[tokio::test]
async fn reply_all_refuses_to_send_when_the_parent_headers_cannot_be_read() {
    let (base, handle) = mock_gmail_seq(vec![(500, "{\"error\":\"backend\"}".to_string())]).await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(store, acct, "gmail-parent", "thread-77", "Lunch?");
    });
    let id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "reply_to_message_id": id,
                "body": "noon works",
                "reply_all": true,
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    // The mock was scripted for ONE request: nothing was sent.
    let reqs = handle.await.unwrap();
    assert_eq!(reqs.len(), 1);
    assert!(reqs[0].starts_with("GET "));

    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send" && a.detail.as_deref() == Some("failed:gmail"))
    );
    assert!(
        !audit
            .iter()
            .any(|a| a.action == "send" && a.detail.as_deref() == Some("ok"))
    );
}

/// The other half of the rule above: a PLAIN reply whose headers fetch fails
/// still goes out, unthreaded, to the sender the store already knows.
#[tokio::test]
async fn reply_send_survives_a_headers_failure() {
    let (base, handle) = mock_gmail_seq(vec![
        (500, "{\"error\":\"backend\"}".to_string()),
        (200, "{}".to_string()),
    ])
    .await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(store, acct, "gmail-parent", "thread-77", "Lunch?");
    });
    let id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "reply_to_message_id": id,
                "body": "noon works",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mime = sent_mime(&handle.await.unwrap()[1]);
    assert!(mime.contains("To: alice@example.com\r\n"), "{mime}");
    assert!(!mime.contains("In-Reply-To:"), "{mime}");
    assert!(!mime.contains("Cc:"), "{mime}");
}

// --- sitrep: seen-ledger + bands + resolution over HTTP ---------------------

use squelch_core::types::AttentionStatus;

/// Seed one signal message and return its local id via search.
fn seed_one_signal(store: &SqliteStore, acct: i64, gmail: &str, thread: &str, subj: &str) -> i64 {
    let m = store
        .upsert_message(&msg(acct, gmail, thread, subj, "body"))
        .unwrap();
    store
        .set_triage(
            m,
            acct,
            80,
            Tier::Signal,
            Sensitivity::Normal,
            None,
            "",
            "",
            None,
        )
        .unwrap();
    m
}

#[tokio::test]
async fn updates_stamp_once_and_carry_prestamp_surfaced_at() {
    let Harness { app, store, acct } = harness(|store, acct| {
        seed_one_signal(store, acct, "g1", "t1", "hi");
    });

    // First fetch: pre-stamp surfaced_at is null (this row was never surfaced).
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/updates"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert!(
        items[0]["surfaced_at"].is_null(),
        "response carries PRE-stamp value (null on first surface)"
    );
    assert_eq!(items[0]["status"], "new", "pre-stamp status is new");

    // Stamped as a side effect of the fetch.
    let after = mail_list(
        &store,
        acct,
        chrono::Utc::now() - chrono::Duration::days(1),
        None,
        None,
        None,
        false,
        SpamScope::Exclude,
    )
    .unwrap();
    let first_stamp = after[0].surfaced_at.expect("surfaced_at now set");
    assert_eq!(after[0].status, AttentionStatus::Open);

    // Second fetch: surfaced_at is now present and unchanged (stamp-once).
    let resp2 = app.oneshot(authed("GET", "/client/updates")).await.unwrap();
    let json2 = body_json(resp2).await;
    assert!(!json2["items"][0]["surfaced_at"].is_null());
    let after2 = mail_list(
        &store,
        acct,
        chrono::Utc::now() - chrono::Duration::days(1),
        None,
        None,
        None,
        false,
        SpamScope::Exclude,
    )
    .unwrap();
    assert_eq!(
        after2[0].surfaced_at,
        Some(first_stamp),
        "stamp did not move"
    );
}

#[tokio::test]
async fn peek_returns_the_same_rows_without_stamping_the_ledger() {
    let Harness { app, store, acct } = harness(|store, acct| {
        seed_one_signal(store, acct, "g1", "t1", "hi");
        seed_one_signal(store, acct, "g2", "t2", "there");
    });
    let window = || chrono::Utc::now() - chrono::Duration::days(1);

    // peek=true: the agent reads on the user's behalf. Same page...
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/updates?peek=true"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["items"].as_array().unwrap().len(), 2);

    // ...and the ledger is untouched: nothing was actually shown to anyone.
    let after_peek = mail_list(
        &store,
        acct,
        window(),
        None,
        None,
        None,
        false,
        SpamScope::Exclude,
    )
    .unwrap();
    assert_eq!(after_peek.len(), 2);
    for u in &after_peek {
        assert!(u.surfaced_at.is_none(), "peek must not stamp surfaced_at");
        assert_eq!(
            u.status,
            AttentionStatus::New,
            "peek must not promote status"
        );
    }

    // Control: the same request without peek stamps, so the skip is the param
    // and not a broken fixture.
    let resp2 = app.oneshot(authed("GET", "/client/updates")).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    let after_normal = mail_list(
        &store,
        acct,
        window(),
        None,
        None,
        None,
        false,
        SpamScope::Exclude,
    )
    .unwrap();
    for u in &after_normal {
        assert!(u.surfaced_at.is_some(), "normal fetch stamps surfaced_at");
        assert_eq!(u.status, AttentionStatus::Open);
    }
}

#[tokio::test]
async fn updates_do_not_reuse_obsolete_field_reasons() {
    use squelch_core::types::FieldReasons;
    let Harness { app, .. } = harness(|store, acct| {
        let id = seed_one_signal(store, acct, "g1", "t1", "hi");
        store
            .set_field_reasons(
                id,
                acct,
                &FieldReasons {
                    importance: Some("known contact -> signal importance 80".into()),
                    deadline: None,
                    tier: Some("known contact -> signal".into()),
                },
            )
            .unwrap();
    });

    let resp = app.oneshot(authed("GET", "/client/updates")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let item = &json["items"][0];
    assert!(
        item.get("field_reasons").is_none(),
        "obsolete deterministic reasons must not explain pending triage"
    );
    assert_eq!(item["reason"], "Triage pending");
}

#[tokio::test]
async fn updates_without_reasons_omit_the_field_reasons_key() {
    // A row with no recorded reasons omits the key entirely — the desktop
    // treats absent the same as null.
    let Harness { app, .. } = harness(|store, acct| {
        seed_one_signal(store, acct, "g1", "t1", "hi");
    });
    let resp = app.oneshot(authed("GET", "/client/updates")).await.unwrap();
    let json = body_json(resp).await;
    assert!(
        json["items"][0].get("field_reasons").is_none(),
        "no reasons recorded -> no field_reasons key: {}",
        json["items"][0]
    );
}

#[tokio::test]
async fn band_query_filters_server_side() {
    use squelch_core::store::agent_triage::{AgentCommitOutcome, AgentTriageStore};
    use squelch_core::triage::decision::{MessageDecision, ThreadAttentionDecision};
    let Harness { app, .. } = harness(|store, acct| {
        // Legacy labels have no placement authority; only the committed agent decision does.
        let bill = store
            .upsert_message(&msg(acct, "g1", "t1", "PG&E past due", "pay"))
            .unwrap();
        store
            .set_triage(
                bill,
                acct,
                95,
                Tier::PastDue,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
        store
            .enqueue_agent_triage(acct, bill, "test", false)
            .unwrap();
        let job = store
            .claim_agent_job(acct, "triage", chrono::Utc::now(), 60)
            .unwrap()
            .unwrap();
        let context = store.load_agent_context(&job).unwrap();
        let decision = MessageDecision {
            summary: "Bill needs review".into(),
            attention: ThreadAttentionDecision {
                show_in_fye: true,
                relevant_message_ids: vec![bill],
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
        seed_one_signal(store, acct, "g2", "t2", "hello");
    });

    let resp = app
        .oneshot(authed("GET", "/client/updates?band=standing"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "standing uses agent attention membership");
    assert_eq!(items[0]["thread_id"], "t1");
    assert_eq!(items[0]["tier"], "signal");
}

#[tokio::test]
async fn bad_band_and_status_are_400() {
    let Harness { app, .. } = harness(|_, _| {});
    let r1 = app
        .clone()
        .oneshot(authed("GET", "/client/updates?band=bogus"))
        .await
        .unwrap();
    assert_eq!(r1.status(), StatusCode::BAD_REQUEST);
    let r2 = app
        .oneshot(authed("GET", "/client/updates?status=bogus"))
        .await
        .unwrap();
    assert_eq!(r2.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn dismiss_and_reopen_endpoint() {
    let Harness { app, store, acct } = harness(|store, acct| {
        seed_one_signal(store, acct, "g1", "t1", "hi");
    });
    let id = store.search(acct, "hi", 10, 0).unwrap()[0].id;

    // Dismiss -> done.
    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            &format!("/client/updates/{id}/status"),
            serde_json::json!({ "status": "done" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let done = mail_list(
        &store,
        acct,
        chrono::Utc::now() - chrono::Duration::days(1),
        None,
        Some(AttentionStatus::Done),
        None,
        false,
        SpamScope::Exclude,
    )
    .unwrap();
    assert_eq!(done.len(), 1);
    assert!(done[0].resolved_at.is_some());

    // The dismiss is audited.
    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "set_status" && a.detail.as_deref() == Some("done"))
    );

    // Reopen -> open.
    let resp2 = app
        .oneshot(authed_json(
            "POST",
            &format!("/client/updates/{id}/status"),
            serde_json::json!({ "status": "open" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
}

#[tokio::test]
async fn dismiss_unknown_message_is_404() {
    let Harness { app, .. } = harness(|_, _| {});
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/updates/999/status",
            serde_json::json!({ "status": "done" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn human_can_dismiss_restricted_mail() {
    // Restriction governs external agents; human lifecycle controls remain available.
    let Harness { app, store, acct } = harness(|store, acct| {
        let s = store
            .upsert_message(&msg(acct, "g1", "t1", "code", "123456"))
            .unwrap();
        store
            .set_triage(
                s,
                acct,
                90,
                Tier::Noise,
                Sensitivity::Sealed,
                Some(SealedKind::Otp),
                "",
                "",
                None,
            )
            .unwrap();
    });
    let sealed_id = store.sealed_messages(acct).unwrap()[0].id;
    let resp = app
        .oneshot(authed_json(
            "POST",
            &format!("/client/updates/{sealed_id}/status"),
            serde_json::json!({ "status": "done" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// --- reminders: "remind me about this later" over HTTP ----------------------

/// The listing window the reminder tests read over.
fn reminder_window() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() - chrono::Duration::days(30)
}

#[tokio::test]
async fn reminder_endpoint_defers_the_thread_and_schedules_its_return() {
    let Harness { app, store, acct } = harness(|store, acct| {
        seed_one_signal(store, acct, "g1", "t1", "hi");
    });
    let id = store.search(acct, "hi", 10, 0).unwrap()[0].id;
    let due = chrono::Utc::now() + chrono::Duration::days(2);

    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            &format!("/client/updates/{id}/reminder"),
            serde_json::json!({ "remind_at": due.to_rfc3339() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["message_id"], id);
    assert_eq!(body["remind_at"], due.to_rfc3339());

    // DEFERRING IS RESOLVING: one round trip both hides the mail and schedules
    // it, and the listing carries the pending stamp back.
    let rows = mail_list(
        &store,
        acct,
        reminder_window(),
        None,
        None,
        None,
        false,
        SpamScope::Exclude,
    )
    .unwrap();
    assert_eq!(rows[0].status, AttentionStatus::Done);
    assert!(rows[0].resolved_at.is_some());
    assert_eq!(rows[0].remind_at.unwrap().timestamp(), due.timestamp());
    assert!(rows[0].reminded_at.is_none());

    let audit = store.list_audit(acct, 10).unwrap();
    assert!(audit.iter().any(|a| a.action == "set_reminder"));
}

#[tokio::test]
async fn reminder_rejects_a_past_or_unparseable_date() {
    // A past date is a client bug, not a reminder that fires immediately: the
    // sweep would fire it on the next tick, turning a timezone mistake into mail
    // reappearing for no visible reason.
    let Harness { app, store, acct } = harness(|store, acct| {
        seed_one_signal(store, acct, "g1", "t1", "hi");
    });
    let id = store.search(acct, "hi", 10, 0).unwrap()[0].id;

    for bad in [
        (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339(),
        "next tuesday".to_string(),
    ] {
        let resp = app
            .clone()
            .oneshot(authed_json(
                "POST",
                &format!("/client/updates/{id}/reminder"),
                serde_json::json!({ "remind_at": bad }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "rejected: {bad}");
    }
    // And nothing was scheduled.
    assert!(
        mail_list(
            &store,
            acct,
            reminder_window(),
            None,
            None,
            None,
            true,
            SpamScope::Exclude
        )
        .unwrap()
        .is_empty()
    );
}

#[tokio::test]
async fn human_reminders_allow_restricted_mail_and_reject_unknown() {
    // A sealed row must be invisible to the reminder endpoints, exactly as it is
    // to the status one: missing and sealed are the same answer.
    let Harness { app, store, acct } = harness(|store, acct| {
        let s = store
            .upsert_message(&msg(acct, "g1", "t1", "code", "123456"))
            .unwrap();
        store
            .set_triage(
                s,
                acct,
                90,
                Tier::Noise,
                Sensitivity::Sealed,
                Some(SealedKind::Otp),
                "",
                "",
                None,
            )
            .unwrap();
    });
    let sealed_id = store.sealed_messages(acct).unwrap()[0].id;
    let due = (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339();

    for id in [sealed_id, 999] {
        let set = app
            .clone()
            .oneshot(authed_json(
                "POST",
                &format!("/client/updates/{id}/reminder"),
                serde_json::json!({ "remind_at": due }),
            ))
            .await
            .unwrap();
        assert_eq!(
            set.status(),
            if id == sealed_id {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            }
        );
        let clear = app
            .clone()
            .oneshot(authed("DELETE", &format!("/client/updates/{id}/reminder")))
            .await
            .unwrap();
        assert_eq!(
            clear.status(),
            if id == sealed_id {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            }
        );
    }
}

#[tokio::test]
async fn reminder_on_the_users_own_sent_mail_is_404() {
    // Sent mail carries a triage row, so the endpoint would happily stamp one —
    // onto a message every listing filters out, where the reminder could never
    // be seen or cancelled and firing it would surface nothing. Same answer as
    // sealed, for the same reason.
    let Harness { app, store, acct } = harness(|store, acct| {
        store
            .upsert_message(&sent_msg(acct, "g-sent", "t1", "what I wrote", "a@b.com"))
            .unwrap();
    });
    let sent_id = store.thread_view_with_html(acct, "t1").unwrap().messages[0].id;
    store
        .set_triage(
            sent_id,
            acct,
            0,
            Tier::Noise,
            Sensitivity::Normal,
            None,
            "",
            "",
            None,
        )
        .unwrap();

    let resp = app
        .oneshot(authed_json(
            "POST",
            &format!("/client/updates/{sent_id}/reminder"),
            serde_json::json!({
                "remind_at": (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339()
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(
        mail_list(
            &store,
            acct,
            reminder_window(),
            None,
            None,
            None,
            true,
            SpamScope::Exclude
        )
        .unwrap()
        .is_empty(),
        "nothing was scheduled"
    );
}

#[tokio::test]
async fn a_reminder_on_old_mail_survives_the_default_updates_window() {
    // THE DEFAULT WINDOW IS 30 DAYS and the pick is "next month": the schedule,
    // the band and the flat listing must all keep showing a two-month-old row
    // the user personally asked to see again, over the handler's own default
    // `since` (no query parameter here — that is the point).
    let Harness { app, store, acct } = harness(|store, acct| {
        let old = store
            .upsert_message(&squelch_core::types::NewMessage {
                received_at: chrono::Utc::now() - chrono::Duration::days(60),
                ..msg(acct, "g-old", "t-old", "the thing you parked", "body")
            })
            .unwrap();
        store
            .set_triage(
                old,
                acct,
                0,
                Tier::Noise,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
    });
    let id = store.thread_view_with_html(acct, "t-old").unwrap().messages[0].id;

    let set = app
        .clone()
        .oneshot(authed_json(
            "POST",
            &format!("/client/updates/{id}/reminder"),
            serde_json::json!({
                "remind_at": (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339()
            }),
        ))
        .await
        .unwrap();
    assert_eq!(set.status(), StatusCode::OK);

    let ids_of = |body: Value| -> Vec<i64> {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u["id"].as_i64().unwrap())
            .collect()
    };
    let schedule = app
        .clone()
        .oneshot(authed("GET", "/client/updates?reminders=pending"))
        .await
        .unwrap();
    assert_eq!(ids_of(body_json(schedule).await), vec![id]);

    // Fired, it is in the standing band, in the header count, and in the flat
    // listing — all three read over the same default window.
    store
        .fire_due_reminders(acct, chrono::Utc::now() + chrono::Duration::days(2))
        .unwrap();
    let standing = app
        .clone()
        .oneshot(authed("GET", "/client/updates?band=standing"))
        .await
        .unwrap();
    assert_eq!(ids_of(body_json(standing).await), vec![id]);
    let flat = app
        .clone()
        .oneshot(authed("GET", "/client/updates"))
        .await
        .unwrap();
    assert_eq!(ids_of(body_json(flat).await), vec![id]);
    let stats = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    assert_eq!(body_json(stats).await["bands"]["standing"], 1);
}

#[tokio::test]
async fn clear_reminder_endpoint_is_idempotent() {
    let Harness { app, store, acct } = harness(|store, acct| {
        seed_one_signal(store, acct, "g1", "t1", "hi");
    });
    let id = store.search(acct, "hi", 10, 0).unwrap()[0].id;
    app.clone()
        .oneshot(authed_json(
            "POST",
            &format!("/client/updates/{id}/reminder"),
            serde_json::json!({ "remind_at": (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339() }),
        ))
        .await
        .unwrap();

    // Twice: the second call is a no-op the caller cannot distinguish, because
    // its intent ("no reminder on this") is already true.
    for _ in 0..2 {
        let resp = app
            .clone()
            .oneshot(authed("DELETE", &format!("/client/updates/{id}/reminder")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["message_id"], id);
        assert_eq!(body["cleared"], true);
    }

    let rows = mail_list(
        &store,
        acct,
        reminder_window(),
        None,
        None,
        None,
        false,
        SpamScope::Exclude,
    )
    .unwrap();
    assert!(rows[0].remind_at.is_none(), "un-scheduled");
    assert_eq!(
        rows[0].status,
        AttentionStatus::Done,
        "clearing a reminder is not an undo"
    );
}

#[tokio::test]
async fn pending_reminder_listing_is_soonest_first_and_never_stamps() {
    let Harness { app, store, acct } = harness(|store, acct| {
        seed_one_signal(store, acct, "g1", "t1", "soon");
        seed_one_signal(store, acct, "g2", "t2", "later");
        seed_one_signal(store, acct, "g3", "t3", "unscheduled");
    });
    let id_of = |subj: &str| store.search(acct, subj, 10, 0).unwrap()[0].id;
    let (soon, later) = (id_of("soon"), id_of("later"));

    for (id, days) in [(later, 9), (soon, 1)] {
        let resp = app
            .clone()
            .oneshot(authed_json(
                "POST",
                &format!("/client/updates/{id}/reminder"),
                serde_json::json!({
                    "remind_at": (chrono::Utc::now() + chrono::Duration::days(days)).to_rfc3339()
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/updates?reminders=pending"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let items = body_json(resp).await["items"].clone();
    let ids: Vec<i64> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, vec![soon, later], "the schedule is soonest-first");
    // DONE ROWS INCLUDED: every row here is done by construction, so this is the
    // one listing where done must not mean gone.
    assert!(
        items
            .as_array()
            .unwrap()
            .iter()
            .all(|u| u["status"] == "done" && !u["remind_at"].is_null())
    );

    // AND IT NEVER STAMPS: these are deferred rows the user is reviewing the
    // schedule of, not mail being shown to them.
    let rows = mail_list(
        &store,
        acct,
        reminder_window(),
        None,
        None,
        None,
        true,
        SpamScope::Exclude,
    )
    .unwrap();
    assert!(
        rows.iter().all(|u| u.surfaced_at.is_none()),
        "the schedule read leaves the seen-ledger alone"
    );

    // An unknown value is a 400, never a silent full listing.
    let bogus = app
        .oneshot(authed("GET", "/client/updates?reminders=bogus"))
        .await
        .unwrap();
    assert_eq!(bogus.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn archive_success_resolves_target_to_done() {
    let (base, handle) = mock_gmail(1).await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(store, acct, "gmail-abc", "t1", "hi");
    });
    let message_id = store.search(acct, "hi", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/archive",
            serde_json::json!({ "message_id": message_id, "confirm": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let _ = handle.await.unwrap();

    // RESOLUTION: the target row is now done + resolved_at set.
    let done = mail_list(
        &store,
        acct,
        chrono::Utc::now() - chrono::Duration::days(1),
        None,
        Some(AttentionStatus::Done),
        None,
        false,
        SpamScope::Exclude,
    )
    .unwrap();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].update.id, message_id);
    assert!(done[0].resolved_at.is_some());
}

#[tokio::test]
async fn update_rule_edits_in_place_and_404s_bogus() {
    let Harness { app, .. } = harness(|_, _| {});

    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/rules",
            serde_json::json!({
                "match_pattern": "*@old.com",
                "want": "old want",
                "disposition": "squelch"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created = body_json(resp).await;
    let rule_id = created["rule_id"].as_i64().unwrap();

    // PUT updates it in place.
    let resp = app
        .clone()
        .oneshot(authed_json(
            "PUT",
            &format!("/client/rules/{rule_id}"),
            serde_json::json!({
                "match_pattern": "*@new.com",
                "want": "new want",
                "disposition": "surface"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // GET reflects the update (same id, new fields).
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/rules"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    let rules = json.as_array().unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0]["id"].as_i64().unwrap(), rule_id);
    assert_eq!(rules[0]["match_pattern"], "*@new.com");
    assert_eq!(rules[0]["want_text"], "new want");
    assert_eq!(rules[0]["disposition"], "surface");

    // PUT a bogus id => 404.
    let resp = app
        .oneshot(authed_json(
            "PUT",
            "/client/rules/999999",
            serde_json::json!({
                "match_pattern": "*@x.com",
                "want": "",
                "disposition": "squelch"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// A mock provider serving the same `status`/`body` to every request, returning
/// its URL. Serves repeatedly, not once: a single request can bill more than one
/// classifier call (create, then edit) and a spent one-shot would look like a
/// transport failure rather than a verdict.
async fn spawn_classifier_mock(status: u16, body: String) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16384];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            });
        }
    });
    format!("http://{addr}")
}

/// A state whose rule inference points at `url`. The classifier is the only thing
/// this state can talk to.
fn harness_pointing_inference_at(url: String, seed: impl FnOnce(&SqliteStore, i64)) -> Harness {
    use squelch_core::config::Stage2Provider;
    use squelch_core::triage::rule_infer::RuleInferClient;

    let (state, store, acct) = state_with(seed);
    let state = state.with_rule_inference(Some(RuleInferClient::new(
        reqwest::Client::new(),
        url,
        "sk-test",
        Stage2Provider::Anthropic,
        "claude-haiku-4-5",
    )));
    Harness {
        app: router(state),
        store,
        acct,
    }
}

/// A one-shot mock of the Anthropic Messages API answering with `verdict`, plus
/// the state decorated to point rule inference at it. Usage on the mocked
/// response is 50 in / 5 out, which is what the ledger assertions expect.
async fn app_with_rule_inference(
    verdict: &'static str,
    seed: impl FnOnce(&SqliteStore, i64),
) -> Harness {
    let body = format!(
        r#"{{"content":[{{"type":"text","text":"{{\"disposition\":\"{verdict}\"}}"}}],"stop_reason":"end_turn","usage":{{"input_tokens":50,"output_tokens":5}}}}"#
    );
    let url = spawn_classifier_mock(200, body).await;
    harness_pointing_inference_at(url, seed)
}

/// The same wiring against a classifier that FAILS permanently (a 400).
async fn app_with_failing_rule_inference(seed: impl FnOnce(&SqliteStore, i64)) -> Harness {
    let body =
        r#"{"type":"error","error":{"type":"invalid_request_error","message":"nope"}}"#.to_string();
    let url = spawn_classifier_mock(400, body).await;
    harness_pointing_inference_at(url, seed)
}

#[tokio::test]
async fn omitted_disposition_is_inferred_and_returned() {
    // No LLM configured on the default harness, so inference resolves to the
    // safe answer: filtered, stored and echoed back so the client can say so.
    let Harness { app, store, acct } = harness(|_, _| {});

    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/rules",
            serde_json::json!({
                "match_pattern": "billing@acme.com",
                "want": "i only care about the invoices"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created = body_json(resp).await;
    assert_eq!(
        created["disposition"], "filtered",
        "the resolved disposition rides the response"
    );
    let rule_id = created["rule_id"].as_i64().unwrap();

    let rules = store.list_sender_rules(acct).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].id, rule_id);
    assert_eq!(
        rules[0].disposition,
        squelch_core::types::Disposition::Filtered
    );
    assert_eq!(rules[0].want_text, "i only care about the invoices");
}

#[tokio::test]
async fn literal_auto_disposition_infers_like_an_absent_one() {
    let Harness { app, store, acct } = harness(|_, _| {});

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/rules",
            serde_json::json!({
                "match_pattern": "news@acme.com",
                "want": "the weekly digest is noise",
                "disposition": "auto"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(body_json(resp).await["disposition"], "filtered");
    assert_eq!(
        store.list_sender_rules(acct).unwrap()[0].disposition,
        squelch_core::types::Disposition::Filtered
    );
}

#[tokio::test]
async fn only_an_absent_key_or_exact_auto_asks_for_inference() {
    // INFERENCE IS OPT-IN, EXACTLY. A present-but-unparseable disposition is a
    // client bug, and folding it into "infer it then" hands a model a decision
    // the client thought it had made. Empty string, wrong case, and garbage all
    // 400 for the same reason "SURFACE" does.
    let Harness { app, store, acct } = harness(|_, _| {});

    for bad in ["", "   ", "AUTO", "Auto", "SURFACE", " squelch", "block"] {
        let resp = app
            .clone()
            .oneshot(authed_json(
                "POST",
                "/client/rules",
                serde_json::json!({
                    "match_pattern": "x@acme.com",
                    "want": "never show me anything from them",
                    "disposition": bad
                }),
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "disposition {bad:?} must be a 400, not a silent inference"
        );
    }
    assert!(
        store.list_sender_rules(acct).unwrap().is_empty(),
        "nothing stored"
    );

    // The exact lowercase literal is the one spelling that infers.
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/rules",
            serde_json::json!({
                "match_pattern": "x@acme.com",
                "want": "never show me anything from them",
                "disposition": "auto"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(body_json(resp).await["disposition"], "filtered");
}

#[tokio::test]
async fn a_configured_classifier_that_fails_still_saves_the_rule() {
    // A CONFIGURED but broken classifier is the interesting case: "no LLM at all"
    // short-circuits before the network. The save must still land, filtered.
    let Harness { app, store, acct } = app_with_failing_rule_inference(|_, _| {}).await;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/rules",
            serde_json::json!({
                "match_pattern": "billing@acme.com",
                "want": "never show me anything from them"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "inference never fails a save"
    );
    assert_eq!(body_json(resp).await["disposition"], "filtered");
    assert_eq!(
        store.list_sender_rules(acct).unwrap()[0].disposition,
        squelch_core::types::Disposition::Filtered
    );
    // A call that produced no verdict produced no billable usage either.
    assert!(
        !store
            .list_usage_by_category(acct, 30)
            .unwrap()
            .iter()
            .any(|(c, _)| c == "rule_infer"),
        "a failed call writes no ledger row"
    );
}

#[tokio::test]
async fn rule_inference_spend_lands_in_its_own_ledger_category() {
    // UNMETERED SPEND IS THE BUG: this is a model call on an interactive path, so
    // it has to show up in the ledger like every other one. Create and update
    // both bill it.
    let Harness { app, store, acct } = app_with_rule_inference("filtered", |_, _| {}).await;

    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/rules",
            serde_json::json!({
                "match_pattern": "news@acme.com",
                "want": "the weekly digest is noise"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let rule_id = body_json(resp).await["rule_id"].as_i64().unwrap();

    let row = |store: &SqliteStore| {
        store
            .list_usage_by_category(acct, 30)
            .unwrap()
            .into_iter()
            .find(|(c, _)| c == "rule_infer")
            .map(|(_, days)| days)
    };
    let days = row(&store).expect("the create call is billed under rule_infer");
    assert_eq!(days.len(), 1, "one day of spend");
    assert_eq!(days[0].calls, 1);
    assert_eq!(days[0].input_tokens, 50);
    assert_eq!(days[0].output_tokens, 5);

    // The edit re-infers, so it bills a second call onto the same line.
    let resp = app
        .oneshot(authed_json(
            "PUT",
            &format!("/client/rules/{rule_id}"),
            serde_json::json!({
                "match_pattern": "news@acme.com",
                "want": "actually just the billing ones"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let days = row(&store).expect("the update path bills too");
    assert_eq!(
        days[0].calls, 2,
        "create and update each bill their own call"
    );
    assert_eq!(days[0].input_tokens, 100);
    assert_eq!(days[0].output_tokens, 10);

    // And it stays its OWN category: never folded into stage1/stage2 spend.
    let cats: Vec<String> = store
        .list_usage_by_category(acct, 30)
        .unwrap()
        .into_iter()
        .map(|(c, _)| c)
        .collect();
    assert_eq!(cats, vec!["rule_infer".to_string()]);
}

#[tokio::test]
async fn explicit_disposition_is_honored_verbatim_and_echoed() {
    // Inference never runs when the client names a disposition: "surface" with a
    // conditional-sounding want stays surface.
    let Harness { app, store, acct } = harness(|_, _| {});

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/rules",
            serde_json::json!({
                "match_pattern": "boss@acme.com",
                "want": "only the ones about the launch",
                "disposition": "surface"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(body_json(resp).await["disposition"], "surface");
    assert_eq!(
        store.list_sender_rules(acct).unwrap()[0].disposition,
        squelch_core::types::Disposition::Surface
    );
}

#[tokio::test]
async fn omitted_disposition_with_empty_want_is_400() {
    // There is nothing to infer FROM, and a rule that says nothing is not a rule.
    let Harness { app, store, acct } = harness(|_, _| {});

    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/rules",
            serde_json::json!({ "match_pattern": "x@acme.com", "want": "   " }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(
        store.list_sender_rules(acct).unwrap().is_empty(),
        "nothing stored"
    );

    // Same on the edit path.
    let resp = app
        .oneshot(authed_json(
            "PUT",
            "/client/rules/1",
            serde_json::json!({ "match_pattern": "x@acme.com", "want": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn update_reinfers_from_the_submitted_want_when_disposition_is_omitted() {
    let Harness { app, store, acct } = harness(|_, _| {});

    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/rules",
            serde_json::json!({
                "match_pattern": "a@acme.com",
                "want": "always show me these",
                "disposition": "surface"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let rule_id = body_json(resp).await["rule_id"].as_i64().unwrap();

    // The edit omits the disposition: it is re-read from the new sentence, and
    // with no LLM wired in that is filtered.
    let resp = app
        .oneshot(authed_json(
            "PUT",
            &format!("/client/rules/{rule_id}"),
            serde_json::json!({
                "match_pattern": "a@acme.com",
                "want": "actually just the billing ones"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let updated = body_json(resp).await;
    assert_eq!(updated["rule_id"].as_i64().unwrap(), rule_id);
    assert_eq!(updated["disposition"], "filtered");
    assert_eq!(
        store.list_sender_rules(acct).unwrap()[0].disposition,
        squelch_core::types::Disposition::Filtered
    );
}

#[tokio::test]
async fn an_inferred_squelch_never_resolves_the_senders_open_mail() {
    // SIDE-EFFECT GUARD: the model says squelch, so the RULE is squelch — but a
    // misclassification must never mass-resolve a sender's mail, so the resolve
    // side effect stays behind an EXPLICIT squelch. Asking to sweep does not buy
    // past that: sweep is permission, not authority.
    let seeded = std::cell::RefCell::new(Vec::<i64>::new());
    let Harness { app, store, acct } = app_with_rule_inference("squelch", |store, acct| {
        seeded.borrow_mut().push(seed_unsub_msg(
            store,
            acct,
            "g1",
            "spam@evil.com",
            None,
            false,
        ));
    })
    .await;
    let spam_id = seeded.borrow()[0];

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/rules",
            serde_json::json!({
                "match_pattern": "spam@evil.com",
                "want": "never show me anything from them",
                "source_message_id": spam_id,
                "sweep": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(
        body_json(resp).await["disposition"],
        "squelch",
        "the rule IS squelch"
    );
    assert_eq!(
        store.list_sender_rules(acct).unwrap()[0].disposition,
        squelch_core::types::Disposition::Squelch
    );

    let done = mail_list(
        &store,
        acct,
        chrono::Utc::now() - chrono::Duration::days(1),
        None,
        Some(AttentionStatus::Done),
        None,
        false,
        SpamScope::Exclude,
    )
    .unwrap();
    assert!(
        !done.iter().any(|u| u.update.id == spam_id),
        "an INFERRED squelch resolves nothing, sweep or no sweep"
    );
}

#[tokio::test]
async fn a_literal_auto_squelch_is_inferred_and_never_resolves_open_mail() {
    // The literal "auto" asks for inference; it is NOT the client making the
    // decision, so the verdict it produces is exactly as inferred as an absent
    // key's. Nothing else distinguishes the two spellings — the no-LLM tests
    // can't, both paths land on filtered — so this is the one test standing
    // between "auto" and the explicit-squelch mass-resolve gate.
    let seeded = std::cell::RefCell::new(Vec::<i64>::new());
    let Harness { app, store, acct } = app_with_rule_inference("squelch", |store, acct| {
        seeded.borrow_mut().push(seed_unsub_msg(
            store,
            acct,
            "g1",
            "spam@evil.com",
            None,
            false,
        ));
    })
    .await;
    let spam_id = seeded.borrow()[0];

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/rules",
            serde_json::json!({
                "match_pattern": "spam@evil.com",
                "want": "never show me anything from them",
                "disposition": "auto",
                "source_message_id": spam_id,
                "sweep": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(
        body_json(resp).await["disposition"],
        "squelch",
        "the rule IS squelch"
    );
    assert_eq!(
        store.list_sender_rules(acct).unwrap()[0].disposition,
        squelch_core::types::Disposition::Squelch
    );

    let done = mail_list(
        &store,
        acct,
        chrono::Utc::now() - chrono::Duration::days(1),
        None,
        Some(AttentionStatus::Done),
        None,
        false,
        SpamScope::Exclude,
    )
    .unwrap();
    assert!(
        !done.iter().any(|u| u.update.id == spam_id),
        "an \"auto\" squelch is an inferred squelch: it resolves nothing"
    );
}

#[tokio::test]
async fn rule_mutations_write_audit_rows() {
    let Harness { app, store, acct } = harness(|_, _| {});

    // POST => rule.create, target = match_pattern.
    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/rules",
            serde_json::json!({
                "match_pattern": "*@old.com",
                "want": "old want",
                "disposition": "squelch"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let rule_id = body_json(resp).await["rule_id"].as_i64().unwrap();

    // PUT => rule.update.
    let resp = app
        .clone()
        .oneshot(authed_json(
            "PUT",
            &format!("/client/rules/{rule_id}"),
            serde_json::json!({
                "match_pattern": "*@new.com",
                "want": "new want",
                "disposition": "surface"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // DELETE => rule.delete, target = rule id.
    let resp = app
        .clone()
        .oneshot(authed("DELETE", &format!("/client/rules/{rule_id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let audit = store.list_audit(acct, 20).unwrap();
    assert!(audit.iter().all(|a| a.actor == "client-api"));
    let create = audit.iter().find(|a| a.action == "rule.create").unwrap();
    assert_eq!(create.target.as_deref(), Some("*@old.com"));
    // The detail carries the id AND the resolved disposition: an inferred
    // verdict is nowhere in the request body, so the log is the only record.
    assert_eq!(
        create.detail.as_deref(),
        Some(format!("{rule_id} disposition=squelch").as_str())
    );
    let update = audit.iter().find(|a| a.action == "rule.update").unwrap();
    assert_eq!(update.target.as_deref(), Some("*@new.com"));
    assert_eq!(
        update.detail.as_deref(),
        Some(format!("{rule_id} disposition=surface").as_str())
    );
    let delete = audit.iter().find(|a| a.action == "rule.delete").unwrap();
    assert_eq!(delete.target.as_deref(), Some(rule_id.to_string().as_str()));
}

#[tokio::test]
async fn failed_rule_mutations_write_no_audit_row() {
    // A 404 (unknown id) on PUT/DELETE changed nothing, so it writes no row.
    let Harness { app, store, acct } = harness(|_, _| {});

    let resp = app
        .clone()
        .oneshot(authed_json(
            "PUT",
            "/client/rules/999999",
            serde_json::json!({
                "match_pattern": "*@x.com",
                "want": "",
                "disposition": "squelch"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let resp = app
        .oneshot(authed("DELETE", "/client/rules/999999"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    assert_eq!(store.list_audit(acct, 20).unwrap().len(), 0);
}

#[tokio::test]
async fn stats_expose_stage2_usage_and_cost() {
    // Cost comes from the default Stage-2 per-MTok prices (5.0 in / 25.0 out),
    // with cache writes at 1.25x and reads at 0.1x of the input price.
    let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let Harness { app, .. } = harness(move |store, acct| {
        // 2 calls: 1_000_000 in, 200_000 out, 200_000 cache-write, 1_000_000
        // cache-read.
        store
            .stage2_bump_usage(
                acct,
                &day,
                UsageTokens {
                    input: 600_000,
                    output: 100_000,
                    cache_creation: 200_000,
                    cache_read: 1_000_000,
                },
            )
            .unwrap();
        store
            .stage2_bump_usage(
                acct,
                &day,
                UsageTokens {
                    input: 400_000,
                    output: 100_000,
                    ..Default::default()
                },
            )
            .unwrap();
    });

    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let s2 = &json["stage2"];
    assert_eq!(s2["calls_today"], 2);
    assert_eq!(s2["input_tokens_today"], 1_000_000);
    assert_eq!(s2["output_tokens_today"], 200_000);
    assert_eq!(s2["cache_creation_tokens_today"], 200_000);
    assert_eq!(s2["cache_read_tokens_today"], 1_000_000);
    // cost = 4.0*(1e6/1e6) + 20.0*(0.2e6/1e6)
    //      + 4.0*2.0*(0.2e6/1e6) + 4.0*0.05*(1e6/1e6) = 4.0 + 4.0 + 1.6 + 0.2
    // (Opus 5.5 prices; 1-hour cache writes at 2x, reads at 0.05x)
    let cost = s2["est_cost_usd_today"].as_f64().unwrap();
    assert!((cost - 9.8).abs() < 1e-9, "expected 9.8, got {cost}");
}

#[tokio::test]
async fn stats_carry_the_capability_flags_the_client_gates_on() {
    // `forward_of_message_id` is `#[serde(default)]`, so a daemon predating
    // forwarding IGNORES it rather than rejecting it: the request degrades into
    // a bare cold send of the note, with the original nowhere in it and nothing
    // in the response saying so. The flag is what lets the client refuse to
    // offer the composer instead of sending that quietly.
    let Harness { app, .. } = harness(|_, _| {});
    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["forwarding"], serde_json::json!(true));
    // Its neighbour, present for the same reason: an answer, never absence.
    assert!(json.get("assistant_relay").is_some());
}

#[tokio::test]
async fn stats_name_the_mailbox_this_daemon_serves() {
    // The app has no other way to learn its own address, and it seeds the
    // greeting's display name from the local part on a first run. Absence is
    // reserved for a daemon too old to say, so this key is ALWAYS present.
    let Harness { app, .. } = harness(|_, _| {});
    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(
        json["account_email"],
        Value::String("me@example.com".into())
    );
}

#[tokio::test]
async fn stats_name_the_person_and_the_photo_door_serves_their_picture() {
    // Never fetched (a grant without the profile scope): every key is ABSENT
    // and the photo door is a 404, so the client keeps its initial.
    let Harness { app, .. } = harness(|_, _| {});
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/stats"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    for key in ["account_name", "account_given_name", "account_photo"] {
        assert!(json.get(key).is_none(), "{key} invented");
    }
    let resp = app
        .oneshot(authed("GET", "/client/account/photo"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Fetched: the name rides on stats, and the tag there is the photo's.
    let jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3];
    let Harness { app, .. } = harness(|store, acct| {
        let photo = squelch_core::store::AccountPhoto {
            bytes: vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3],
            mime: "image/jpeg".into(),
            tag: "abc123".into(),
        };
        store
            .set_account_profile(acct, Some("Ada Lovelace"), Some("Ada"), Some(&photo))
            .unwrap();
    });
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/stats"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["account_name"], Value::String("Ada Lovelace".into()));
    assert_eq!(json["account_given_name"], Value::String("Ada".into()));
    assert_eq!(json["account_photo"], Value::String("abc123".into()));

    // The photo door is the human door: no token, no picture.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/client/account/photo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let resp = app
        .oneshot(authed("GET", "/client/account/photo"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "image/jpeg");
    assert_eq!(resp.headers()["x-content-type-options"], "nosniff");
    // Unversioned URL, so it must never be served stale past a tag change.
    assert_eq!(resp.headers()["cache-control"], "private, no-cache");
    assert_eq!(resp.headers()["etag"], "\"abc123\"");
    assert_eq!(body_bytes(resp).await, jpeg);
}

#[tokio::test]
async fn stats_carry_gmail_inbox_unread_only_once_the_sync_loop_has_fetched_it() {
    // Never fetched (old DB, or every fetch so far failed): the key is ABSENT.
    // A zeroed object would read as "your inbox is clear", which is a claim this
    // daemon cannot make without having asked Gmail.
    let Harness { app, .. } = harness(|_, _| {});
    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    let json = body_json(resp).await;
    assert!(
        json.get("inbox_unread").is_none(),
        "unfetched counts must be absent, not zero"
    );

    // Once the sync loop has stored a pair, it rides along with the stats.
    let Harness { app, store, acct } = harness(|store, acct| {
        store.set_inbox_unread(acct, 214, 190).unwrap();
    });
    let stamped = store.inbox_unread(acct).unwrap().unwrap().fetched_at;
    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["inbox_unread"]["messages"], 214);
    assert_eq!(json["inbox_unread"]["threads"], 190);
    // The counts keep being served while a fetch is failing, so the stamp is
    // what separates a live pair from one frozen since the scope was revoked.
    assert_eq!(
        json["inbox_unread"]["fetched_at"],
        Value::String(stamped.to_rfc3339()),
        "the stored stamp, verbatim RFC3339"
    );
    // Still the same body it always was.
    assert!(json["stage2"].is_object());

    // A real zero IS reported: nothing unread is a different answer from not
    // knowing, and the client renders them differently.
    let Harness { app, .. } = harness(|store, acct| {
        store.set_inbox_unread(acct, 0, 0).unwrap();
    });
    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["inbox_unread"]["messages"], 0);
    assert_eq!(json["inbox_unread"]["threads"], 0);
}

#[tokio::test]
async fn usage_returns_rows_totals_and_is_bearer_gated() {
    // Newest-first daily rows, totals costed at the default 3.0 in / 15.0 out
    // (cache writes at 1.25x, reads at 0.1x of the input price).
    let Harness { app, .. } = harness(|store, acct| {
        store
            .stage2_bump_usage(
                acct,
                "2026-07-08",
                UsageTokens {
                    input: 400_000,
                    output: 100_000,
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .stage2_bump_usage(
                acct,
                "2026-07-09",
                UsageTokens {
                    input: 600_000,
                    output: 100_000,
                    cache_creation: 400_000,
                    cache_read: 2_000_000,
                },
            )
            .unwrap();
    });

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/usage"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;

    let rows = json["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    // Newest-first.
    assert_eq!(rows[0]["day"], "2026-07-09");
    assert_eq!(rows[0]["input_tokens"], 600_000);
    assert_eq!(rows[0]["cache_creation_tokens"], 400_000);
    assert_eq!(rows[0]["cache_read_tokens"], 2_000_000);
    assert_eq!(rows[1]["day"], "2026-07-08");

    let totals = &json["totals"];
    assert_eq!(totals["calls"], 2);
    assert_eq!(totals["input_tokens"], 1_000_000);
    assert_eq!(totals["output_tokens"], 200_000);
    assert_eq!(totals["cache_creation_tokens"], 400_000);
    assert_eq!(totals["cache_read_tokens"], 2_000_000);
    // cost = 4.0*(1e6/1e6) + 20.0*(0.2e6/1e6)
    //      + 4.0*2.0*(0.4e6/1e6) + 4.0*0.05*(2e6/1e6) = 4.0 + 4.0 + 3.2 + 0.4
    // (Opus 5.5 prices; cache writes at the 1-hour TTL's 2x, reads at 0.05x)
    let cost = totals["est_cost_usd"].as_f64().unwrap();
    assert!((cost - 11.6).abs() < 1e-9, "expected 11.6, got {cost}");

    // Stage-1 and Stage-2 appear as separate usage categories.
    assert_eq!(json["model"], "claude-opus-5-5");
    assert_eq!(json["categories"]["stage2"]["model"], "claude-opus-5-5");
    assert_eq!(json["categories"]["stage1"]["model"], "claude-opus-5-5");

    // Bearer-gated: no token => 401.
    let req = Request::builder()
        .uri("/client/usage")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn stats_expose_bands_and_last_surfaced_at() {
    let Harness { app, .. } = harness(|store, acct| {
        let bill = store
            .upsert_message(&msg(acct, "g1", "t1", "bill due", "pay"))
            .unwrap();
        store
            .set_triage(
                bill,
                acct,
                95,
                Tier::Deadline,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
        seed_one_signal(store, acct, "g2", "t2", "hello");
    });

    // Before any surface: bands.new = 2, last_surfaced_at null.
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/stats"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["bands"]["standing"], 1);
    assert_eq!(json["bands"]["new"], 2);
    assert!(json["last_surfaced_at"].is_null());

    // Surface via /client/updates, then last_surfaced_at is set and new drops.
    let _ = app
        .clone()
        .oneshot(authed("GET", "/client/updates"))
        .await
        .unwrap();
    let resp2 = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    let json2 = body_json(resp2).await;
    assert_eq!(json2["bands"]["new"], 0);
    assert_eq!(json2["bands"]["open"], 2);
    assert!(!json2["last_surfaced_at"].is_null());
}

// --- GET /client/thread/{id} carries per-message sanitized html -------------

#[tokio::test]
async fn thread_response_carries_html_field() {
    let Harness { app, .. } = harness(|store, acct| {
        // One HTML message and one plain-text message in the same thread.
        let mut html_msg = msg(acct, "g-html", "t-html", "Newsletter", "flattened text");
        html_msg.body_html = Some("<p>Hello <strong>world</strong></p>".to_string());
        let h = store.upsert_message(&html_msg).unwrap();
        store
            .set_triage(
                h,
                acct,
                60,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();

        // RENAMED mid-thread, which is what makes the per-message subject differ
        // from the thread's (the thread's is the OLDEST message's).
        let plain = msg(
            acct,
            "g-plain",
            "t-html",
            "Re: Newsletter - unsubscribe?",
            "just text",
        );
        let p = store.upsert_message(&plain).unwrap();
        store
            .set_triage(
                p,
                acct,
                55,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
    });

    let resp = app
        .oneshot(authed("GET", "/client/thread/t-html"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let msgs = json["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2);
    // The HTML message carries sanitized html; the plain one carries null.
    assert_eq!(msgs[0]["html"], "<p>Hello <strong>world</strong></p>");
    assert_eq!(msgs[1]["html"], Value::Null);
    // Text content is always present (client fallback).
    assert_eq!(msgs[0]["content"], "flattened text");
    // Per-message triage rides along for in-thread attention highlighting.
    assert_eq!(msgs[0]["tier"], "signal");
    assert_eq!(msgs[0]["attention_open"], true, "unresolved row reads open");
    // ...and so does each message's OWN subject. The thread's is the oldest
    // message's, which titles the conversation and mis-titles a forward of any
    // message inside it the moment somebody renames the thread.
    assert_eq!(json["subject"], "Newsletter");
    assert_eq!(msgs[0]["subject"], "Newsletter");
    assert_eq!(msgs[1]["subject"], "Re: Newsletter - unsubscribe?");
}

/// The recipients the reader draws its To/Cc/Bcc block from, end to end: parsed
/// at ingest into three columns, served per message on the human door.
///
/// THE ABSENCES ARE THE ASSERTIONS. A row whose headers nobody has read yet must
/// arrive with the keys MISSING rather than empty, because the client draws the
/// two states the same and the daemon does not: absent is "the recipients sweep
/// has not reached this row", empty is "read, and the header named nobody", and
/// a client that saw "" for both would claim the second on evidence of the first.
#[tokio::test]
async fn thread_response_carries_recipients_per_header() {
    let Harness { app, .. } = harness(|store, acct| {
        let mut inbound = msg(acct, "g-1", "t-r", "Pricing", "text");
        inbound.to_addrs = Some("Me <me@example.com>, \"Doe, Jane\" <jane@x.test>".into());
        inbound.cc_addrs = Some("team@x.test".into());
        // Present on the wire from a caller that had no business setting it: the
        // store drops a Bcc on anything not sent, so it must not come back.
        inbound.bcc_addrs = Some("leaked@x.test".into());
        let a = store.upsert_message(&inbound).unwrap();

        let mut outbound = msg(acct, "g-2", "t-r", "Re: Pricing", "reply");
        outbound.is_sent = true;
        outbound.from_addr = "me@example.com".into();
        outbound.to_addrs = Some("jane@x.test".into());
        outbound.cc_addrs = Some(String::new());
        outbound.bcc_addrs = Some("Ana <ana@x.test>".into());
        let b = store.upsert_message(&outbound).unwrap();

        // A third message nobody has read the headers of yet.
        let c = store
            .upsert_message(&msg(acct, "g-3", "t-r", "Re: Pricing", "later"))
            .unwrap();

        for id in [a, b, c] {
            store
                .set_triage(
                    id,
                    acct,
                    0,
                    Tier::Noise,
                    Sensitivity::Normal,
                    None,
                    "",
                    "",
                    None,
                )
                .unwrap();
        }
    });

    let resp = app
        .oneshot(authed("GET", "/client/thread/t-r"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let msgs = json["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);

    assert_eq!(
        msgs[0]["to_addrs"], "Me <me@example.com>, \"Doe, Jane\" <jane@x.test>",
        "the quoting that keeps a lastname-first name one person survives the wire"
    );
    assert_eq!(msgs[0]["cc_addrs"], "team@x.test");
    assert!(
        msgs[0].get("bcc_addrs").is_none(),
        "a Bcc on received mail is the sender's leak or a forgery, and is never stored"
    );

    assert_eq!(msgs[1]["to_addrs"], "jane@x.test");
    assert_eq!(msgs[1]["cc_addrs"], "", "read, and the header named nobody");
    assert_eq!(
        msgs[1]["bcc_addrs"], "Ana <ana@x.test>",
        "the user's own blind copy"
    );

    for key in ["to_addrs", "cc_addrs", "bcc_addrs"] {
        assert!(
            msgs[2].get(key).is_none(),
            "an unread row carries no {key} at all, the shape an older daemon sends"
        );
    }
}

// --- UNSUBSCRIBE endpoints ---------------------------------------------------

/// Seed a message carrying explicit List-Unsubscribe fields; return its id. The
/// one-click flag is still stored (ingest keeps it) but no longer affects
/// selection — the handler only extracts the first http(s) URL.
fn seed_unsub_msg(
    store: &SqliteStore,
    acct: i64,
    gmail: &str,
    from: &str,
    header: Option<&str>,
    one_click: bool,
) -> i64 {
    let mut m = msg(acct, gmail, "t-unsub", "Newsletter", "body");
    m.from_addr = from.to_string();
    m.list_unsubscribe = header.map(|h| h.to_string());
    m.list_unsub_one_click = one_click;
    let id = store.upsert_message(&m).unwrap();
    store
        .set_triage(
            id,
            acct,
            10,
            Tier::Noise,
            Sensitivity::Normal,
            None,
            "",
            "",
            None,
        )
        .unwrap();
    id
}

#[tokio::test]
async fn unsubscribe_returns_first_http_url_and_records_and_audits() {
    // The server returns the first http(s) URL for the client to open; it makes
    // no outbound request. A mailto ahead of the URL is skipped.
    let Harness { app, store, acct } = harness(|store, acct| {
        seed_unsub_msg(
            store,
            acct,
            "g1",
            "News@Sub.com",
            Some("<mailto:u@sub.com>, <https://sub.com/u/1?x=2>"),
            true,
        );
    });
    let mid = store.search(acct, "Newsletter", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/unsubscribe",
            serde_json::json!({ "message_id": mid }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["method"], "browser");
    assert_eq!(json["sender"], "news@sub.com", "sender is lowercased");
    assert_eq!(json["url"], "https://sub.com/u/1?x=2");

    // A ledger row exists for the lowercased sender, method 'browser'.
    let recs = store.list_unsubscribes(acct).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].sender, "news@sub.com");
    assert_eq!(recs[0].method, "browser");
    assert_eq!(recs[0].violation_count, 0);

    // Audited (method + sender), never the URL.
    let audit = store.list_audit(acct, 10).unwrap();
    let row = audit.iter().find(|a| a.action == "unsubscribe").unwrap();
    assert_eq!(row.detail.as_deref(), Some("browser:news@sub.com"));
    assert!(audit.iter().all(|a| {
        a.detail
            .as_deref()
            .map(|d| !d.contains("sub.com/u"))
            .unwrap_or(true)
    }));
}

#[tokio::test]
async fn unsubscribe_browser_returns_url_and_does_not_fetch() {
    let Harness { app, store, acct } = harness(|store, acct| {
        seed_unsub_msg(
            store,
            acct,
            "g1",
            "news@sub.com",
            Some("<https://sub.com/u/9>"),
            false,
        );
    });
    let mid = store.search(acct, "Newsletter", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/unsubscribe",
            serde_json::json!({ "message_id": mid }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["method"], "browser");
    assert_eq!(json["sender"], "news@sub.com");
    assert_eq!(json["url"], "https://sub.com/u/9");
    assert_eq!(store.list_unsubscribes(acct).unwrap()[0].method, "browser");
}

#[tokio::test]
async fn unsubscribe_resolves_the_source_email_to_done() {
    // Unsubscribing IS the disposition of that email: the triage row must not
    // stay open demanding a second act.
    let Harness { app, store, acct } = harness(|store, acct| {
        seed_unsub_msg(
            store,
            acct,
            "g1",
            "news@sub.com",
            Some("<https://sub.com/u/9>"),
            false,
        );
    });
    let mid = store.search(acct, "Newsletter", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/unsubscribe",
            serde_json::json!({ "message_id": mid }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let done = mail_list(
        &store,
        acct,
        chrono::Utc::now() - chrono::Duration::days(1),
        None,
        Some(AttentionStatus::Done),
        None,
        false,
        SpamScope::Exclude,
    )
    .unwrap();
    assert!(
        done.iter().any(|u| u.update.id == mid),
        "unsubscribed email is done"
    );
}

#[tokio::test]
async fn squelch_rule_with_source_message_resolves_it_surface_does_not() {
    // Blocking a sender from an email marks that email done; a surface rule is
    // tuning, not a verdict, and must leave its source open.
    // Ids captured from the seeder, not inferred from a search ordering: spam
    // is seeded first and therefore holds the LOWER id, so max/min would have
    // them backwards.
    let seeded = std::cell::RefCell::new(Vec::<i64>::new());
    let Harness { app, store, acct } = harness(|store, acct| {
        seeded.borrow_mut().push(seed_unsub_msg(
            store,
            acct,
            "g1",
            "spam@evil.com",
            None,
            false,
        ));
        seeded.borrow_mut().push(seed_unsub_msg(
            store,
            acct,
            "g2",
            "friend@nice.com",
            None,
            false,
        ));
    });
    let (spam_id, nice_id) = (seeded.borrow()[0], seeded.borrow()[1]);

    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/rules",
            serde_json::json!({
                "match_pattern": "spam@evil.com",
                "want": "",
                "disposition": "squelch",
                "source_message_id": spam_id,
                "sweep": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/rules",
            serde_json::json!({
                "match_pattern": "friend@nice.com",
                "want": "always surface",
                "disposition": "surface",
                "source_message_id": nice_id,
                "sweep": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let done = mail_list(
        &store,
        acct,
        chrono::Utc::now() - chrono::Duration::days(1),
        None,
        Some(AttentionStatus::Done),
        None,
        false,
        SpamScope::Exclude,
    )
    .unwrap();
    assert!(
        done.iter().any(|u| u.update.id == spam_id),
        "squelch-rule source is done"
    );
    assert!(
        !done.iter().any(|u| u.update.id == nice_id),
        "surface-rule source stays open"
    );
}

#[tokio::test]
async fn an_explicit_squelch_without_sweep_resolves_nothing() {
    // THE UNDO CASE. Deleting a squelch rule and undoing the delete recreates it
    // with the same explicit disposition, and that recreate must not re-sweep an
    // inbox the user has since worked through. Same for the rule editor's save.
    // `sweep` is the client saying "and clear what they already sent"; without it
    // the rule is written and nothing else happens.
    let seeded = std::cell::RefCell::new(Vec::<i64>::new());
    let Harness { app, store, acct } = harness(|store, acct| {
        seeded.borrow_mut().push(seed_unsub_msg(
            store,
            acct,
            "g1",
            "spam@evil.com",
            None,
            false,
        ));
    });
    let spam_id = seeded.borrow()[0];

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/rules",
            serde_json::json!({
                "match_pattern": "spam@evil.com",
                "want": "never show me anything from them",
                "disposition": "squelch",
                "source_message_id": spam_id
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(
        store.list_sender_rules(acct).unwrap()[0].disposition,
        squelch_core::types::Disposition::Squelch,
        "the rule is still a squelch"
    );

    let done = mail_list(
        &store,
        acct,
        chrono::Utc::now() - chrono::Duration::days(1),
        None,
        Some(AttentionStatus::Done),
        None,
        false,
        SpamScope::Exclude,
    )
    .unwrap();
    assert!(
        !done.iter().any(|u| u.update.id == spam_id),
        "no sweep was asked for, so the sender's open mail stays open"
    );
}

/// Seed one triaged message from `from` on its OWN thread. Distinct threads
/// matter here: the bands collapse a thread to one representative row, so
/// same-thread seeds would hide exactly the sibling a sweep is supposed to clear.
fn seed_from_on_thread(
    store: &SqliteStore,
    acct: i64,
    gmail: &str,
    thread: &str,
    from: &str,
) -> i64 {
    let mut m = msg(acct, gmail, thread, "Newsletter", "body");
    m.from_addr = from.to_string();
    let id = store.upsert_message(&m).unwrap();
    store
        .set_triage(
            id,
            acct,
            10,
            Tier::Noise,
            Sensitivity::Normal,
            None,
            "",
            "",
            None,
        )
        .unwrap();
    id
}

#[tokio::test]
async fn sweep_resolves_by_sender_and_a_wildcard_resolves_only_the_source() {
    // WITH sweep, both resolve paths run: a plain address clears every open row
    // from that sender, and a wildcard clears ONLY the message the click came
    // from, because "*@evil.com" is a domain verdict and mass-resolving a whole
    // domain is a bigger claim than the click made.
    let seeded = std::cell::RefCell::new(Vec::<i64>::new());
    let Harness { app, store, acct } = harness(|store, acct| {
        let mut s = seeded.borrow_mut();
        s.push(seed_from_on_thread(
            store,
            acct,
            "g1",
            "t1",
            "spam@evil.com",
        ));
        s.push(seed_from_on_thread(
            store,
            acct,
            "g2",
            "t2",
            "spam@evil.com",
        ));
        s.push(seed_from_on_thread(
            store,
            acct,
            "g3",
            "t3",
            "other@junk.com",
        ));
        s.push(seed_from_on_thread(
            store,
            acct,
            "g4",
            "t4",
            "someone@junk.com",
        ));
    });
    let (spam_a, spam_b, junk_src, junk_other) = {
        let s = seeded.borrow();
        (s[0], s[1], s[2], s[3])
    };

    // Non-wildcard + sweep => resolve_sender_done: BOTH of that sender's rows.
    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/rules",
            serde_json::json!({
                "match_pattern": "spam@evil.com",
                "want": "never show me anything from them",
                "disposition": "squelch",
                "sweep": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Wildcard + sweep => resolve_done on the source message only.
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/rules",
            serde_json::json!({
                "match_pattern": "*@junk.com",
                "want": "never show me anything from them",
                "disposition": "squelch",
                "source_message_id": junk_src,
                "sweep": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let done = mail_list(
        &store,
        acct,
        chrono::Utc::now() - chrono::Duration::days(1),
        None,
        Some(AttentionStatus::Done),
        None,
        false,
        SpamScope::Exclude,
    )
    .unwrap();
    let is_done = |id: i64| done.iter().any(|u| u.update.id == id);
    assert!(
        is_done(spam_a) && is_done(spam_b),
        "every row from the swept sender is done"
    );
    assert!(is_done(junk_src), "the wildcard's source message is done");
    assert!(!is_done(junk_other), "the rest of the domain is untouched");
}

#[tokio::test]
async fn unsubscribe_mailto_only_is_422() {
    // A mailto-only List-Unsubscribe has no http(s) link => 422; the server never
    // sends anything.
    let Harness { app, store, acct } = harness(|store, acct| {
        seed_unsub_msg(
            store,
            acct,
            "g1",
            "news@sub.com",
            Some("<mailto:unsub@sub.com?subject=Bye>"),
            false,
        );
    });
    let mid = store.search(acct, "Newsletter", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/unsubscribe",
            serde_json::json!({ "message_id": mid }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        store.list_unsubscribes(acct).unwrap().len(),
        0,
        "no ledger row on 422"
    );
}

#[tokio::test]
async fn unsubscribe_no_info_is_422() {
    let Harness { app, store, acct } = harness(|store, acct| {
        seed_unsub_msg(store, acct, "g1", "news@sub.com", None, false);
    });
    let mid = store.search(acct, "Newsletter", 10, 0).unwrap()[0].id;
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/unsubscribe",
            serde_json::json!({ "message_id": mid }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn human_can_inspect_unsubscribe_for_restricted_mail_but_unknown_is_404() {
    let Harness { app, store, acct } = harness(|store, acct| {
        let s = store
            .upsert_message(&{
                let mut m = msg(acct, "g-otp", "t-otp", "verification code", "123456");
                m.list_unsubscribe = Some("<https://sub.com/u/1>".into());
                m
            })
            .unwrap();
        store
            .set_triage(
                s,
                acct,
                90,
                Tier::Noise,
                Sensitivity::Sealed,
                Some(SealedKind::Otp),
                "",
                "",
                None,
            )
            .unwrap();
    });
    let sealed_id = store.sealed_messages(acct).unwrap()[0].id;

    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/unsubscribe",
            serde_json::json!({ "message_id": sealed_id }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/unsubscribe",
            serde_json::json!({ "message_id": 999999 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_unsubscribes_newest_first_and_bearer_gated() {
    let Harness { app, .. } = harness(|store, acct| {
        store
            .upsert_unsubscribe(
                acct,
                "old@x.com",
                "browser",
                None,
                chrono::Utc::now() - chrono::Duration::hours(3),
            )
            .unwrap();
        store
            .upsert_unsubscribe(acct, "new@x.com", "one_click", None, chrono::Utc::now())
            .unwrap();
    });

    // Bearer-gated.
    let unauth = Request::builder()
        .uri("/client/unsubscribes")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(unauth).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );

    let resp = app
        .oneshot(authed("GET", "/client/unsubscribes"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let items = json.as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["sender"], "new@x.com", "newest requested_at first");
    assert_eq!(items[0]["method"], "one_click");
    assert_eq!(items[0]["violation_count"], 0);
    assert_eq!(items[0]["last_violation_at"], Value::Null);
    assert_eq!(items[0]["resolution"], Value::Null);
    assert_eq!(items[1]["sender"], "old@x.com");
}

#[tokio::test]
async fn unsubscribe_resolution_sets_blocked_and_404s_unknown_and_400s_bad_value() {
    let Harness { app, store, acct } = harness(|store, acct| {
        store
            .upsert_unsubscribe(acct, "news@x.com", "browser", None, chrono::Utc::now())
            .unwrap();
    });

    // blocked => 200, echoes sender + resolution.
    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/unsubscribes/resolution",
            serde_json::json!({ "sender": "News@X.com", "resolution": "blocked" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["sender"], "news@x.com");
    assert_eq!(json["resolution"], "blocked");
    assert_eq!(
        store.list_unsubscribes(acct).unwrap()[0]
            .resolution
            .as_deref(),
        Some("blocked")
    );
    let audit = store.list_audit(acct, 10).unwrap();
    assert!(audit.iter().any(
        |a| a.action == "unsub_resolution" && a.detail.as_deref() == Some("news@x.com:blocked")
    ));

    // filtered (a smart block) is a resolution too.
    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/unsubscribes/resolution",
            serde_json::json!({ "sender": "news@x.com", "resolution": "filtered" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        store.list_unsubscribes(acct).unwrap()[0]
            .resolution
            .as_deref(),
        Some("filtered")
    );

    // Unknown sender => 404.
    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/unsubscribes/resolution",
            serde_json::json!({ "sender": "nobody@x.com", "resolution": "dismissed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Bad resolution value => 400.
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/unsubscribes/resolution",
            serde_json::json!({ "sender": "news@x.com", "resolution": "nuke" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn human_can_read_restricted_thread_with_html() {
    let Harness { app, .. } = harness(|store, acct| {
        let mut sealed = msg(acct, "g-otp", "t-sealed", "verification code", "123456");
        sealed.body_html = Some("<p>code 123456</p>".to_string());
        let s = store.upsert_message(&sealed).unwrap();
        store
            .set_triage(
                s,
                acct,
                90,
                Tier::Noise,
                Sensitivity::Sealed,
                Some(SealedKind::Otp),
                "",
                "",
                None,
            )
            .unwrap();
    });

    let resp = app
        .oneshot(authed("GET", "/client/thread/t-sealed"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let thread = body_json(resp).await;
    assert_eq!(thread["messages"][0]["content"], "123456");
    assert_eq!(thread["messages"][0]["html"], "<p>code 123456</p>");
}

#[tokio::test]
async fn triage_config_get_default_shape() {
    // With no override rows and a default state, GET reports the built-in default
    // caps, all sources "default", the price fields, and null tokens/call (the
    // usage ledger is empty).
    let Harness { app, .. } = harness(|_, _| {});
    let resp = app
        .oneshot(authed("GET", "/client/triage-config"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;

    assert_eq!(json["thread_daily_cap"], 3);
    assert_eq!(json["sender_daily_cap"], 5);
    assert_eq!(json["global_daily_cap"], 120);
    assert_eq!(json["agent"]["effective_daily_run_cap"], 10_000);
    assert_eq!(json["agent"]["effective_background_daily_run_cap"], 200);
    assert_eq!(json["agent"]["access_daily_run_cap"], 500);
    assert_eq!(json["agent"]["effective_access_daily_run_cap"], 500);
    // What is left after both lanes.
    assert_eq!(json["agent"]["reserved_arrival_runs"], 9_300);
    assert_eq!(
        json["agent"]["legacy_stage_caps_are_active_ceilings"],
        false
    );
    assert_eq!(json["sources"]["thread_daily_cap"], "default");
    assert_eq!(json["sources"]["sender_daily_cap"], "default");
    assert_eq!(json["sources"]["global_daily_cap"], "default");
    // Empty ledger => tokens/call are null; calls/inbound averages are 0.
    assert!(json["avg_tokens_in_per_call"].is_null());
    assert!(json["avg_tokens_out_per_call"].is_null());
    assert_eq!(json["avg_stage2_calls_per_day"], 0.0);
    assert_eq!(json["avg_inbound_per_day"], 0.0);
    // Prices are present (default Stage2Config values on ApiState::new).
    assert!(json["price_in_per_mtok"].is_number());
    assert!(json["price_out_per_mtok"].is_number());

    // Stage-2 escalation model label. SAME model as Stage-1: escalation buys
    // context and reasoning depth, not a different reader.
    assert_eq!(json["stage2_model"], "claude-opus-5-5");

    // Stage-1 block: GLOBAL-only cap (default 500), "default" source, null
    // tokens/call (empty ledger), and prices.
    let s1 = &json["stage1"];
    assert_eq!(s1["model"], "claude-opus-5-5");
    assert_eq!(s1["global_daily_cap"], 500);
    assert_eq!(s1["source"], "default");
    assert_eq!(s1["avg_calls_per_day"], 0.0);
    assert!(s1["avg_tokens_in_per_call"].is_null());
    assert!(s1["avg_tokens_out_per_call"].is_null());
    assert!(s1["price_in_per_mtok"].is_number());
    assert!(s1["price_out_per_mtok"].is_number());
}

#[tokio::test]
async fn triage_config_post_persists_stage1_global_cap_override() {
    let Harness { app, store, acct } = harness(|_, _| {});
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/triage-config",
            serde_json::json!({ "stage1_global_daily_cap": 750 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["stage1"]["global_daily_cap"], 750);
    assert_eq!(json["stage1"]["source"], "override");

    // Persisted where the Stage-1 pass re-reads it each cycle.
    let o = store.stage2_cap_overrides(acct).unwrap();
    assert_eq!(o.stage1_global_daily_cap, Some(750));
}

#[tokio::test]
async fn triage_config_post_rejects_out_of_range_stage1_cap() {
    let Harness { app, store, acct } = harness(|_, _| {});
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/triage-config",
            serde_json::json!({ "stage1_global_daily_cap": 0 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        store
            .stage2_cap_overrides(acct)
            .unwrap()
            .stage1_global_daily_cap,
        None
    );
}

#[tokio::test]
async fn triage_config_get_computes_trailing_averages() {
    // Seed 4 recent inbound messages + a usage ledger day, and confirm the
    // trailing-14d averages and per-call token means.
    let Harness { app, .. } = harness(|store, acct| {
        for i in 0..4 {
            let m = msg(acct, &format!("g{i}"), &format!("t{i}"), "hi", "body");
            store.upsert_message(&m).unwrap();
        }
        // One day with 2 calls, 1000 in / 200 out tokens.
        let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
        store
            .stage2_bump_usage(
                acct,
                &day,
                UsageTokens {
                    input: 600,
                    output: 120,
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .stage2_bump_usage(
                acct,
                &day,
                UsageTokens {
                    input: 400,
                    output: 80,
                    ..Default::default()
                },
            )
            .unwrap();
    });

    let resp = app
        .oneshot(authed("GET", "/client/triage-config"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    // 4 inbound / 14 days.
    assert!((json["avg_inbound_per_day"].as_f64().unwrap() - 4.0 / 14.0).abs() < 1e-9);
    // 2 calls / 14 days.
    assert!((json["avg_stage2_calls_per_day"].as_f64().unwrap() - 2.0 / 14.0).abs() < 1e-9);
    // 1000 in / 2 calls = 500; 200 out / 2 calls = 100.
    assert!((json["avg_tokens_in_per_call"].as_f64().unwrap() - 500.0).abs() < 1e-9);
    assert!((json["avg_tokens_out_per_call"].as_f64().unwrap() - 100.0).abs() < 1e-9);
}

#[tokio::test]
async fn triage_config_post_persists_override_and_reports_it() {
    let Harness { app, store, acct } = harness(|_, _| {});
    // Set thread + global (omit sender — subset allowed).
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/triage-config",
            serde_json::json!({ "thread_daily_cap": 5, "global_daily_cap": 300 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;

    // Response reflects the fresh effective values + "override" sources for the
    // two set caps; the untouched sender cap stays default.
    assert_eq!(json["thread_daily_cap"], 5);
    assert_eq!(json["global_daily_cap"], 300);
    assert_eq!(json["sender_daily_cap"], 5);
    assert_eq!(json["sources"]["thread_daily_cap"], "override");
    assert_eq!(json["sources"]["global_daily_cap"], "override");
    assert_eq!(json["sources"]["sender_daily_cap"], "default");

    // Persisted to the store (what the Stage-2 pass re-reads each cycle).
    let o = store.stage2_cap_overrides(acct).unwrap();
    assert_eq!(o.thread_daily_cap, Some(5));
    assert_eq!(o.global_daily_cap, Some(300));
    assert_eq!(o.sender_daily_cap, None);
}

#[tokio::test]
async fn triage_config_post_rejects_out_of_range_and_non_integer() {
    let Harness { app, store, acct } = harness(|_, _| {});

    // Zero is below the min.
    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/triage-config",
            serde_json::json!({ "thread_daily_cap": 0 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Above the max.
    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/triage-config",
            serde_json::json!({ "global_daily_cap": 100001 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // A non-integer (float) is rejected at deserialization.
    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/triage-config",
            serde_json::json!({ "thread_daily_cap": 5.5 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Nothing was persisted by any rejected request.
    assert_eq!(
        store.stage2_cap_overrides(acct).unwrap(),
        Default::default()
    );
}

// --- attachments ------------------------------------------------------------

/// Collect a response body into raw bytes.
async fn body_bytes(resp: axum::response::Response) -> Vec<u8> {
    resp.into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

fn header_str(resp: &axum::response::Response, name: header::HeaderName) -> String {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

#[tokio::test]
async fn thread_carries_attachment_metadata_and_empty_when_none() {
    let Harness { app, store, acct } = harness(|store, acct| {
        // Thread t1: one message with a stored pdf + an over-cap (NULL data) part.
        let m1 = store
            .upsert_message(&msg(acct, "g1", "t1", "s1", "b1"))
            .unwrap();
        store
            .set_triage(
                m1,
                acct,
                60,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
        store
            .insert_attachment(acct, m1, "doc.pdf", "application/pdf", 5, Some(b"Hello"))
            .unwrap();
        store
            .insert_attachment(
                acct,
                m1,
                "big.bin",
                "application/octet-stream",
                11_000_000,
                None,
            )
            .unwrap();
        // ...and a cid-inline image, the part the body's <img src="cid:..."> names.
        store
            .insert_inline_attachment(
                acct,
                m1,
                "attachment-3",
                "image/png",
                3,
                Some(b"png"),
                Some("logo@squelch"),
            )
            .unwrap();
        // Thread t2: a message with NO attachments -> [] on the wire.
        let m2 = store
            .upsert_message(&msg(acct, "g2", "t2", "s2", "b2"))
            .unwrap();
        store
            .set_triage(
                m2,
                acct,
                60,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
    });
    let _ = (&store, acct);

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/thread/t1"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let atts = &json["messages"][0]["attachments"];
    assert_eq!(atts.as_array().unwrap().len(), 3);
    // Ordered by id: pdf first (downloadable), big second (not downloadable),
    // the cid-inline image last.
    assert_eq!(atts[0]["filename"], "doc.pdf");
    assert_eq!(atts[0]["mime"], "application/pdf");
    assert_eq!(atts[0]["size"], 5);
    assert_eq!(atts[0]["downloadable"], true);
    assert_eq!(atts[1]["filename"], "big.bin");
    assert_eq!(atts[1]["downloadable"], false);
    // Only the inline part carries a content_id; on the others the key is absent
    // entirely, exactly as a daemon predating the field would send it.
    assert_eq!(atts[2]["content_id"], "logo@squelch");
    assert!(atts[0].get("content_id").is_none());
    assert!(atts[1].get("content_id").is_none());

    let resp = app
        .oneshot(authed("GET", "/client/thread/t2"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(
        json["messages"][0]["attachments"],
        serde_json::json!([]),
        "attachments key is always present, [] when none"
    );
}

#[tokio::test]
async fn attachment_bytes_headers_apply_render_safety_whitelist() {
    // Seed one attachment of each interesting mime; ids come back in insert order.
    let ids = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(i64, &'static str)>::new()));
    let ids_seed = ids.clone();
    let Harness { app, .. } = harness(move |store, acct| {
        let m = store
            .upsert_message(&msg(acct, "g1", "t1", "s", "b"))
            .unwrap();
        store
            .set_triage(
                m,
                acct,
                60,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
        let mut v = ids_seed.lock().unwrap();
        v.push((
            store
                .insert_attachment(acct, m, "doc.pdf", "application/pdf", 3, Some(b"pdf"))
                .unwrap(),
            "pdf",
        ));
        v.push((
            store
                .insert_attachment(acct, m, "pic.png", "image/png", 3, Some(b"png"))
                .unwrap(),
            "png",
        ));
        v.push((
            store
                .insert_attachment(acct, m, "vec.svg", "image/svg+xml", 3, Some(b"svg"))
                .unwrap(),
            "svg",
        ));
        v.push((
            store
                .insert_attachment(acct, m, "page.html", "text/html", 3, Some(b"htm"))
                .unwrap(),
            "html",
        ));
        // Case/parameter tricks + the xml family: all must force octet-stream.
        v.push((
            store
                .insert_attachment(
                    acct,
                    m,
                    "shout.svg",
                    "IMAGE/SVG+XML; charset=x",
                    3,
                    Some(b"svg"),
                )
                .unwrap(),
            "svg-shout",
        ));
        v.push((
            store
                .insert_attachment(acct, m, "bare.svg", "image/svg", 3, Some(b"svg"))
                .unwrap(),
            "svg-bare",
        ));
        v.push((
            store
                .insert_attachment(acct, m, "x.xhtml", "application/xhtml+xml", 3, Some(b"xht"))
                .unwrap(),
            "xhtml",
        ));
        v.push((
            store
                .insert_attachment(acct, m, "x.xml", "text/xml", 3, Some(b"xml"))
                .unwrap(),
            "txml",
        ));
        v.push((
            store
                .insert_attachment(acct, m, "y.xml", "application/xml", 3, Some(b"xml"))
                .unwrap(),
            "axml",
        ));
    });

    let ids = ids.lock().unwrap().clone();
    for (id, kind) in ids {
        let resp = app
            .clone()
            .oneshot(authed("GET", &format!("/client/attachments/{id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{kind} should 200");
        let ct = header_str(&resp, header::CONTENT_TYPE);
        match kind {
            "pdf" => assert_eq!(ct, "application/pdf"),
            "png" => assert_eq!(ct, "image/png"),
            // Scriptable types must NEVER be renderable from our origin.
            "svg" => assert_eq!(ct, "application/octet-stream", "svg must be octet-stream"),
            "html" => assert_eq!(ct, "application/octet-stream", "html must be octet-stream"),
            "svg-shout" | "svg-bare" | "xhtml" | "txml" | "axml" => assert_eq!(
                ct, "application/octet-stream",
                "{kind}: scriptable/xml-family mime must be octet-stream"
            ),
            _ => unreachable!(),
        }
        // Common security headers on every served attachment.
        assert_eq!(header_str(&resp, header::X_CONTENT_TYPE_OPTIONS), "nosniff");
        assert_eq!(
            header_str(&resp, header::CACHE_CONTROL),
            "private, max-age=3600"
        );
        assert!(
            header_str(&resp, header::CONTENT_DISPOSITION).starts_with("attachment;"),
            "must be an attachment disposition"
        );
        let bytes = body_bytes(resp).await;
        assert_eq!(bytes.len(), 3, "{kind} bytes served");
    }
}

#[tokio::test]
async fn attachment_filename_is_sanitized_in_disposition() {
    let id = std::sync::Arc::new(std::sync::Mutex::new(0i64));
    let id_seed = id.clone();
    let Harness { app, .. } = harness(move |store, acct| {
        let m = store
            .upsert_message(&msg(acct, "g1", "t1", "s", "b"))
            .unwrap();
        store
            .set_triage(
                m,
                acct,
                60,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
        *id_seed.lock().unwrap() = store
            .insert_attachment(
                acct,
                m,
                "../../evil\"name.pdf",
                "application/pdf",
                3,
                Some(b"pdf"),
            )
            .unwrap();
    });
    let id = *id.lock().unwrap();
    let resp = app
        .oneshot(authed("GET", &format!("/client/attachments/{id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let disp = header_str(&resp, header::CONTENT_DISPOSITION);
    // Path separators and quotes stripped; the header stays well-formed.
    assert!(!disp.contains('/'), "no path separators: {disp}");
    assert!(!disp.contains('\\'), "no backslashes: {disp}");
    assert_eq!(disp, "attachment; filename=\"....evilname.pdf\"");
}

#[tokio::test]
async fn attachment_over_cap_is_410() {
    let id = std::sync::Arc::new(std::sync::Mutex::new(0i64));
    let id_seed = id.clone();
    let Harness { app, .. } = harness(move |store, acct| {
        let m = store
            .upsert_message(&msg(acct, "g1", "t1", "s", "b"))
            .unwrap();
        store
            .set_triage(
                m,
                acct,
                60,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
        // Metadata only (data == None): over the ingest cap.
        *id_seed.lock().unwrap() = store
            .insert_attachment(
                acct,
                m,
                "big.bin",
                "application/octet-stream",
                11_000_000,
                None,
            )
            .unwrap();
    });
    let id = *id.lock().unwrap();
    let resp = app
        .oneshot(authed("GET", &format!("/client/attachments/{id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::GONE);
}

#[tokio::test]
async fn human_can_read_restricted_attachment_but_unknown_is_404() {
    let id = std::sync::Arc::new(std::sync::Mutex::new(0i64));
    let id_seed = id.clone();
    let Harness { app, .. } = harness(move |store, acct| {
        let m = store
            .upsert_message(&msg(acct, "g1", "t1", "code", "secret"))
            .unwrap();
        store
            .set_triage(
                m,
                acct,
                0,
                Tier::Noise,
                Sensitivity::Sealed,
                Some(SealedKind::Otp),
                "",
                "",
                None,
            )
            .unwrap();
        *id_seed.lock().unwrap() = store
            .insert_attachment(acct, m, "secret.pdf", "application/pdf", 6, Some(b"secret"))
            .unwrap();
    });
    let id = *id.lock().unwrap();
    let resp = app
        .clone()
        .oneshot(authed("GET", &format!("/client/attachments/{id}")))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "human can read the attachment"
    );

    let resp = app
        .oneshot(authed("GET", "/client/attachments/999999"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn attachment_requires_bearer_auth() {
    let Harness { app, .. } = harness(|_, _| {});
    let req = Request::builder()
        .uri("/client/attachments/1")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "byte endpoint is behind auth"
    );
}

#[tokio::test]
async fn triage_feedback_round_trip_records_and_audits() {
    let mut mid = 0;
    let Harness { app, store, acct } = harness(|store, acct| {
        let m = store
            .upsert_message(&msg(acct, "g1", "t1", "s", "b"))
            .unwrap();
        store
            .set_triage(
                m,
                acct,
                60,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
        mid = m;
    });

    // Correct the tier; the response carries the recorded row.
    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/triage-feedback",
            serde_json::json!({
                "message_id": mid,
                "dimension": "tier",
                "to_value": "noise",
                "note": "newsletter, not signal"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "route must be mounted and accept the correction"
    );
    let json = body_json(resp).await;
    assert_eq!(json["dimension"], "tier");
    assert_eq!(json["from_value"], "signal");
    assert_eq!(json["to_value"], "noise");

    // The correction is the dataset: GET returns it.
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/triage-feedback"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json.as_array().map(Vec::len), Some(1));
    assert_eq!(json[0]["message_id"], mid);

    // Audited.
    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit.iter().any(|e| e.action == "triage_correction"),
        "correction must land an audit row"
    );

    // Invalid axis / invalid value are 400, not recorded.
    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/triage-feedback",
            serde_json::json!({ "message_id": mid, "dimension": "vibes", "to_value": "noise" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/triage-feedback",
            serde_json::json!({ "message_id": mid, "dimension": "tier", "to_value": "spam" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// --- local drafts: human-door CRUD + send cleanup ---------------------------

/// Seed one ordinary (non-sealed) message and return its local id.
fn seed_draft_parent(store: &SqliteStore, acct: i64) -> i64 {
    let m = store
        .upsert_message(&msg(
            acct,
            "gmail-parent",
            "t-draft",
            "Lunch?",
            "want lunch?",
        ))
        .unwrap();
    store
        .set_triage(
            m,
            acct,
            80,
            Tier::Signal,
            Sensitivity::Normal,
            None,
            "",
            "",
            None,
        )
        .unwrap();
    m
}

/// PUT one draft and hand back its wire view.
async fn put_draft(app: &axum::Router, body: Value) -> Value {
    let resp = app
        .clone()
        .oneshot(authed_json("PUT", "/client/drafts", body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    body_json(resp).await
}

/// The current draft list as a JSON array, as the current client asks for it
/// (forward drafts included).
async fn list_drafts(app: &axum::Router) -> Value {
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/drafts?forwards=1"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    body_json(resp).await
}

#[tokio::test]
async fn put_draft_creates_then_edits_the_same_key_in_place() {
    let mut parent = 0;
    let Harness { app, .. } = harness(|store, acct| parent = seed_draft_parent(store, acct));

    let first = put_draft(
        &app,
        serde_json::json!({
            "reply_to_message_id": parent,
            "to": "alice@example.com",
            "subject": "Re: Lunch?",
            "body": "sure"
        }),
    )
    .await;
    assert_eq!(first["reply_to_message_id"], parent);
    assert_eq!(first["to"], "alice@example.com");
    assert_eq!(first["body"], "sure");

    // Same key => the same composition, edited: one row, same id.
    let second = put_draft(
        &app,
        serde_json::json!({
            "reply_to_message_id": parent,
            "to": "alice@example.com",
            "subject": "Re: Lunch?",
            "body": "sure, 1pm"
        }),
    )
    .await;
    assert_eq!(
        second["id"], first["id"],
        "an edit must not mint a new draft"
    );
    assert_eq!(second["body"], "sure, 1pm");
    assert_eq!(
        second["created_at"], first["created_at"],
        "created_at survives an edit"
    );

    let all = list_drafts(&app).await;
    assert_eq!(all.as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn put_draft_defaults_missing_text_fields_and_keys_new_mail_on_null() {
    let Harness { app, .. } = harness(|_, _| {});
    let draft = put_draft(&app, serde_json::json!({ "to": "bob@example.com" })).await;
    assert!(
        draft["reply_to_message_id"].is_null(),
        "no parent => a new-message draft (the newest, created when none)"
    );
    assert_eq!(
        draft["subject"], "",
        "a missing text field stores empty, not 400"
    );
    assert_eq!(draft["body"], "");
}

#[tokio::test]
async fn human_can_save_draft_for_restricted_parent_but_unknown_still_fails() {
    let Harness { app, store, acct } = harness(|store, acct| {
        let s = store
            .upsert_message(&msg(acct, "gmail-sealed", "t9", "code", "123456"))
            .unwrap();
        store
            .set_triage(
                s,
                acct,
                90,
                Tier::Noise,
                Sensitivity::Sealed,
                Some(SealedKind::Otp),
                "",
                "",
                None,
            )
            .unwrap();
    });
    let sealed_id = store.sealed_messages(acct).unwrap()[0].id;

    for parent in [sealed_id, 999_999] {
        let resp = app
            .clone()
            .oneshot(authed_json(
                "PUT",
                "/client/drafts",
                serde_json::json!({ "reply_to_message_id": parent, "body": "hi" }),
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            if parent == sealed_id {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            },
            "human access and missing parent are distinct"
        );
    }
    assert_eq!(store.list_drafts(acct).unwrap().len(), 1);
    assert_eq!(list_drafts(&app).await.as_array().map(Vec::len), Some(1));
}

/// The forward key resolves its parent through the same unfiltered lookup as
/// the reply key: a sealed parent is the human's to forward-draft, an unknown
/// one is still a 404.
#[tokio::test]
async fn human_can_save_forward_draft_for_restricted_parent_but_unknown_still_fails() {
    let Harness { app, store, acct } = harness(|store, acct| {
        let s = store
            .upsert_message(&msg(acct, "gmail-sealed", "t9", "code", "123456"))
            .unwrap();
        store
            .set_triage(
                s,
                acct,
                90,
                Tier::Noise,
                Sensitivity::Sealed,
                Some(SealedKind::Otp),
                "",
                "",
                None,
            )
            .unwrap();
    });
    let sealed_id = store.sealed_messages(acct).unwrap()[0].id;

    for parent in [sealed_id, 999_999] {
        let resp = app
            .clone()
            .oneshot(authed_json(
                "PUT",
                "/client/drafts",
                serde_json::json!({ "forward_of_message_id": parent, "body": "fwd" }),
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            if parent == sealed_id {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            },
            "human access and missing parent are distinct"
        );
    }
    let drafts = store.list_drafts(acct).unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].forward_of_message_id, Some(sealed_id));
    assert_eq!(list_drafts(&app).await.as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn drafts_list_carries_the_to_field_name_on_the_wire() {
    let Harness { app, .. } = harness(|_, _| {});
    put_draft(
        &app,
        serde_json::json!({ "to": "bob@example.com", "subject": "Hello", "body": "hi" }),
    )
    .await;

    let all = list_drafts(&app).await;
    assert_eq!(all.as_array().map(Vec::len), Some(1));
    assert_eq!(
        all[0]["to"], "bob@example.com",
        "the column is to_addr; the wire says `to`"
    );
    assert!(
        all[0].get("to_addr").is_none(),
        "no store column name leaks"
    );
    assert!(
        all[0].get("account_id").is_none(),
        "no account id on the wire"
    );
    assert_eq!(all[0]["subject"], "Hello");
    assert_eq!(all[0]["body"], "hi");
    // RFC3339 timestamps, same as every other wire view.
    assert!(all[0]["created_at"].as_str().unwrap().contains('T'));
    assert!(all[0]["updated_at"].as_str().unwrap().contains('T'));
}

#[tokio::test]
async fn delete_draft_then_deleting_it_again_is_404() {
    let Harness { app, .. } = harness(|_, _| {});
    let draft = put_draft(
        &app,
        serde_json::json!({ "to": "bob@example.com", "body": "hi" }),
    )
    .await;
    let id = draft["id"].as_i64().unwrap();

    let resp = app
        .clone()
        .oneshot(authed("DELETE", &format!("/client/drafts/{id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["status"], "deleted");
    assert_eq!(json["id"], id);
    assert_eq!(list_drafts(&app).await.as_array().map(Vec::len), Some(0));

    let resp = app
        .oneshot(authed("DELETE", &format!("/client/drafts/{id}")))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "a second delete has nothing to remove"
    );
}

#[tokio::test]
async fn draft_reads_and_writes_are_no_store() {
    // A draft is unsent thinking: like the revealed sealed body, it must not be
    // retained by anything on the path to the client.
    let Harness { app, .. } = harness(|_, _| {});
    let resp = app
        .clone()
        .oneshot(authed_json(
            "PUT",
            "/client/drafts",
            serde_json::json!({ "to": "bob@example.com", "body": "hi" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );

    let resp = app.oneshot(authed("GET", "/client/drafts")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
}

#[tokio::test]
async fn draft_crud_writes_no_audit_rows() {
    // The audit log is the ledger of reveals and Gmail writes. A draft never
    // leaves the machine, so it must leave no trace there.
    let Harness { app, store, acct } = harness(|_, _| {});
    let draft = put_draft(
        &app,
        serde_json::json!({ "to": "bob@example.com", "body": "hi" }),
    )
    .await;
    let id = draft["id"].as_i64().unwrap();
    put_draft(
        &app,
        serde_json::json!({ "to": "bob@example.com", "body": "hi again" }),
    )
    .await;
    let _ = list_drafts(&app).await;
    app.oneshot(authed("DELETE", &format!("/client/drafts/{id}")))
        .await
        .unwrap();

    assert!(
        store.list_audit(acct, 10).unwrap().is_empty(),
        "draft CRUD is not audited"
    );
}

#[tokio::test]
async fn successful_send_discards_the_draft_it_consumed() {
    // Cold send: one Gmail call (no parent headers to fetch).
    let (base, handle) = mock_gmail(1).await;
    let Harness { app, store, acct } = app_with_writes(base, |_, _| {});

    let draft = put_draft(
        &app,
        serde_json::json!({ "to": "alice@example.com", "subject": "Hi", "body": "see you Tuesday" }),
    )
    .await;
    let draft_id = draft["id"].as_i64().unwrap();

    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "subject": "Hi",
                "body": "see you Tuesday",
                "confirm": true,
                "draft_id": draft_id
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let reqs = handle.await.unwrap();
    assert_eq!(reqs.len(), 1);
    assert!(reqs[0].contains("/messages/send"));

    assert_eq!(
        list_drafts(&app).await.as_array().map(Vec::len),
        Some(0),
        "the send consumed the draft"
    );
    // The send itself is still audited; the draft cleanup is not. `mock_gmail`
    // answers `{}`, so the echo has no id to fetch back and audits the skip.
    let audit = store.list_audit(acct, 10).unwrap();
    assert_eq!(audit.len(), 2);
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send" && a.detail.as_deref() == Some("ok"))
    );
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send.echo" && a.detail.as_deref() == Some("skipped:no_id"))
    );
}

#[tokio::test]
async fn send_with_an_unknown_draft_id_still_succeeds() {
    // A stale draft id is bookkeeping, not a send failure.
    let (base, handle) = mock_gmail(1).await;
    let Harness { app, .. } = app_with_writes(base, |_, _| {});
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "body": "see you Tuesday",
                "confirm": true,
                "draft_id": 4242
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(handle.await.unwrap().len(), 1);
}

#[tokio::test]
async fn guard_blocked_send_leaves_the_draft_intact() {
    let Harness { app, .. } = harness(|_, _| {});
    let draft = put_draft(
        &app,
        serde_json::json!({
            "to": "alice@example.com",
            "body": "your verification code is 483920"
        }),
    )
    .await;
    let draft_id = draft["id"].as_i64().unwrap();

    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "body": "your verification code is 483920",
                "confirm": true,
                "draft_id": draft_id
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // A blocked send is recoverable: the composition is exactly where it was.
    let all = list_drafts(&app).await;
    assert_eq!(all.as_array().map(Vec::len), Some(1));
    assert_eq!(all[0]["id"], draft_id);
    assert_eq!(all[0]["body"], "your verification code is 483920");
}

#[tokio::test]
async fn draft_routes_require_bearer_auth() {
    let Harness { app, .. } = harness(|_, _| {});
    let unauthed = [
        Request::builder()
            .uri("/client/drafts")
            .body(Body::empty())
            .unwrap(),
        common::json_request(
            "PUT",
            "/client/drafts",
            &serde_json::json!({ "to": "a@example.com" }),
            false,
        ),
        Request::builder()
            .method("DELETE")
            .uri("/client/drafts/1")
            .body(Body::empty())
            .unwrap(),
        common::json_request(
            "POST",
            "/client/drafts",
            &serde_json::json!({ "to": "a@example.com" }),
            false,
        ),
        common::json_request(
            "PUT",
            "/client/drafts/1",
            &serde_json::json!({ "to": "a@example.com" }),
            false,
        ),
    ];
    for req in unauthed {
        let uri = req.uri().clone();
        let method = req.method().clone();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {uri} must be behind auth"
        );
    }
}

/// Send one draft request and hand back `(status, json)`.
async fn draft_call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(authed_json(method, uri, body))
        .await
        .unwrap();
    let status = resp.status();
    if status == StatusCode::OK {
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store",
            "{method} {uri} must be no-store"
        );
    }
    (status, body_json(resp).await)
}

/// Every key a `DraftView` must carry, null or not, so a client never has to
/// tell a missing field from a daemon too old to have it.
const DRAFT_VIEW_KEYS: [&str; 15] = [
    "id",
    "reply_to_message_id",
    "forward_of_message_id",
    "to",
    "cc",
    "bcc",
    "subject",
    "body",
    "attachments",
    "created_at",
    "updated_at",
    "thread_id",
    "parent_subject",
    "parent_from",
    "parent_from_addr",
];

#[tokio::test]
async fn post_drafts_creates_distinct_rows_and_put_by_id_edits_one() {
    let Harness { app, store, acct } = harness(|_, _| {});
    let (s1, a) = draft_call(
        &app,
        "POST",
        "/client/drafts",
        serde_json::json!({ "to": "bob@example.com", "subject": "One", "body": "1" }),
    )
    .await;
    let (s2, b) = draft_call(
        &app,
        "POST",
        "/client/drafts",
        serde_json::json!({ "to": "carol@example.com", "subject": "Two", "body": "2" }),
    )
    .await;
    assert_eq!((s1, s2), (StatusCode::OK, StatusCode::OK));
    assert_ne!(a["id"], b["id"], "a second new mail is a second draft");
    let obj = a.as_object().unwrap();
    for key in DRAFT_VIEW_KEYS {
        assert!(obj.contains_key(key), "DraftView lacks `{key}`: {a}");
    }
    for key in [
        "reply_to_message_id",
        "forward_of_message_id",
        "thread_id",
        "parent_subject",
        "parent_from",
        "parent_from_addr",
    ] {
        assert!(a[key].is_null(), "plain new mail has no `{key}`");
    }
    assert_eq!(a["attachments"], serde_json::json!([]));

    let a_id = a["id"].as_i64().unwrap();
    let (s, edited) = draft_call(
        &app,
        "PUT",
        &format!("/client/drafts/{a_id}"),
        serde_json::json!({ "to": "bob@example.com", "subject": "One", "body": "1 again", "cc": "x@example.com" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(edited["id"], a["id"]);
    assert_eq!(edited["created_at"], a["created_at"]);
    assert_eq!(edited["body"], "1 again");
    assert_eq!(edited["cc"], "x@example.com");

    let all = list_drafts(&app).await;
    let all = all.as_array().unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0]["id"], a["id"], "the edited draft is newest");
    assert_eq!(store.list_drafts(acct).unwrap().len(), 2);
    assert!(
        store.list_audit(acct, 10).unwrap().is_empty(),
        "never audited"
    );
}

#[tokio::test]
async fn put_draft_by_id_is_the_same_404_for_unknown_and_another_accounts_id() {
    let Harness { app, store, .. } = harness(|_, _| {});
    let other = store.ensure_account("other@example.com").unwrap();
    let theirs = store
        .upsert_draft(
            other,
            squelch_core::store::DraftKey::NewCreate,
            squelch_core::store::DraftFields {
                body: "theirs",
                ..Default::default()
            },
            chrono::Utc::now(),
        )
        .unwrap()
        .unwrap();
    let (s_theirs, j_theirs) = draft_call(
        &app,
        "PUT",
        &format!("/client/drafts/{}", theirs.id),
        serde_json::json!({ "body": "mine now" }),
    )
    .await;
    let (s_unknown, j_unknown) = draft_call(
        &app,
        "PUT",
        "/client/drafts/999999",
        serde_json::json!({ "body": "x" }),
    )
    .await;
    assert_eq!(s_theirs, StatusCode::NOT_FOUND);
    assert_eq!(s_unknown, StatusCode::NOT_FOUND);
    assert_eq!(j_theirs, j_unknown, "indistinguishable");
    assert!(j_theirs["error"].is_string());
    // The miss wrote nothing, on either account.
    assert_eq!(
        store.draft_by_id(other, theirs.id).unwrap().unwrap().body,
        "theirs"
    );
    assert_eq!(list_drafts(&app).await.as_array().map(Vec::len), Some(0));
}

#[tokio::test]
async fn keyless_put_edits_the_newest_new_message_draft_for_old_apps() {
    let mut parent = 0;
    let Harness { app, .. } = harness(|store, acct| parent = seed_draft_parent(store, acct));
    // A reply draft never counts as the new-message slot.
    put_draft(
        &app,
        serde_json::json!({ "reply_to_message_id": parent, "body": "r" }),
    )
    .await;
    let (_, older) = draft_call(
        &app,
        "POST",
        "/client/drafts",
        serde_json::json!({ "body": "older" }),
    )
    .await;
    let (_, newer) = draft_call(
        &app,
        "POST",
        "/client/drafts",
        serde_json::json!({ "body": "newer" }),
    )
    .await;
    // A forward saved AFTER both: the newest row, and never the keyless target.
    let forward = put_draft(
        &app,
        serde_json::json!({ "forward_of_message_id": parent, "body": "fwd" }),
    )
    .await;
    let edited = put_draft(&app, serde_json::json!({ "body": "old app typed" })).await;
    assert_eq!(
        edited["id"], newer["id"],
        "the NEWEST PLAIN new-message row"
    );
    assert!(edited["forward_of_message_id"].is_null());
    let all = list_drafts(&app).await;
    let all = all.as_array().unwrap();
    assert_eq!(all.len(), 4);
    let old_row = all.iter().find(|d| d["id"] == older["id"]).unwrap();
    assert_eq!(old_row["body"], "older", "the older draft is untouched");
    let fwd_row = all.iter().find(|d| d["id"] == forward["id"]).unwrap();
    assert_eq!(fwd_row["body"], "fwd", "the forward draft is untouched");
    let new_row = all.iter().find(|d| d["id"] == newer["id"]).unwrap();
    assert_eq!(new_row["body"], "old app typed");
}

#[tokio::test]
async fn forward_drafts_are_keyed_per_message_and_carry_parent_context() {
    let mut parent = 0;
    let Harness { app, .. } = harness(|store, acct| parent = seed_draft_parent(store, acct));
    let first = put_draft(
        &app,
        serde_json::json!({ "forward_of_message_id": parent, "to": "carol@example.com", "body": "fyi" }),
    )
    .await;
    assert_eq!(first["forward_of_message_id"], parent);
    assert!(first["reply_to_message_id"].is_null());
    assert_eq!(first["thread_id"], "t-draft");
    assert_eq!(first["parent_subject"], "Lunch?");
    assert!(first["parent_from"].is_string());
    assert!(first["parent_from_addr"].as_str().unwrap().contains('@'));
    let second = put_draft(
        &app,
        serde_json::json!({ "forward_of_message_id": parent, "to": "carol@example.com", "body": "fyi!" }),
    )
    .await;
    assert_eq!(second["id"], first["id"], "one forward draft per message");
    // A reply to the same message is its own row, with the same context.
    let reply = put_draft(
        &app,
        serde_json::json!({ "reply_to_message_id": parent, "body": "r" }),
    )
    .await;
    assert_ne!(reply["id"], first["id"]);
    assert_eq!(reply["parent_subject"], "Lunch?");
    assert_eq!(list_drafts(&app).await.as_array().map(Vec::len), Some(2));

    // Both keys at once is a 400; an unknown forward parent is a 404.
    let (s, j) = draft_call(
        &app,
        "PUT",
        "/client/drafts",
        serde_json::json!({ "reply_to_message_id": parent, "forward_of_message_id": parent }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(j["error"].is_string());
    let (s, _) = draft_call(
        &app,
        "PUT",
        "/client/drafts",
        serde_json::json!({ "forward_of_message_id": 999_999 }),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(list_drafts(&app).await.as_array().map(Vec::len), Some(2));
}

/// An app predating forward drafts restores the first row with no reply key
/// as its new message, so a forward row must not reach it: listed only to a
/// client that opts in with `forwards=1`.
#[tokio::test]
async fn forward_drafts_are_listed_only_with_forwards_opt_in() {
    let mut parent = 0;
    let Harness { app, .. } = harness(|store, acct| parent = seed_draft_parent(store, acct));
    let fwd = put_draft(
        &app,
        serde_json::json!({ "forward_of_message_id": parent, "body": "fyi" }),
    )
    .await;
    let reply = put_draft(
        &app,
        serde_json::json!({ "reply_to_message_id": parent, "body": "r" }),
    )
    .await;

    let with = list_drafts(&app).await;
    let ids: Vec<&Value> = with.as_array().unwrap().iter().map(|d| &d["id"]).collect();
    assert!(ids.contains(&&fwd["id"]), "opted in: the forward is listed");
    assert!(ids.contains(&&reply["id"]));

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/drafts"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let without = body_json(resp).await;
    let ids: Vec<&Value> = without
        .as_array()
        .unwrap()
        .iter()
        .map(|d| &d["id"])
        .collect();
    assert!(
        !ids.contains(&&fwd["id"]),
        "an old app never sees the forward"
    );
    assert!(ids.contains(&&reply["id"]), "only forwards are hidden");
}

#[tokio::test]
async fn stats_advertise_drafts_v2() {
    let Harness { app, .. } = harness(|_, _| {});
    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["drafts_v2"], true);
}

#[tokio::test]
async fn stats_advertise_search_plain_order() {
    let Harness { app, .. } = harness(|_, _| {});
    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["search_plain_order"], true);
}

#[tokio::test]
async fn a_forward_send_discards_the_forward_draft_it_consumed() {
    let (base, handle) = mock_gmail_seq(vec![
        (200, original_raw_response("<p>chart</p>")),
        (200, "{}".to_string()),
    ])
    .await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(
            store,
            acct,
            "gmail-original",
            "thread-77",
            "Quarterly numbers",
        );
    });
    let id = store.search(acct, "quarterly", 10, 0).unwrap()[0].id;
    let draft = put_draft(
        &app,
        serde_json::json!({ "forward_of_message_id": id, "to": "carol@example.com", "body": "worth a read" }),
    )
    .await;
    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "forward_of_message_id": id,
                "to": "carol@example.com",
                "body": "worth a read",
                "confirm": true,
                "draft_id": draft["id"]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(handle.await.unwrap().len(), 2);
    assert_eq!(
        list_drafts(&app).await.as_array().map(Vec::len),
        Some(0),
        "the forward send consumed its draft"
    );
}

#[tokio::test]
async fn thread_view_marks_sent_to_senders_known_and_strangers_not() {
    use squelch_core::store::ContactEntry;
    let Harness { app, .. } = harness(|store, acct| {
        // Both authenticated at ingest, so the split under test is purely the
        // contact half.
        let mut known = msg(acct, "g1", "t1", "lunch?", "from a known contact");
        known.auth_pass = Some(true);
        store.upsert_message(&known).unwrap();
        let mut stranger = msg(acct, "g2", "t1", "lunch?", "from a stranger");
        stranger.from_addr = "promo@nowhere.io".into();
        stranger.from_name = None;
        stranger.auth_pass = Some(true);
        store.upsert_message(&stranger).unwrap();
        // Seeded through the Sent-derived contacts table — the same signal
        // triage's known-contact floor reads. Cased differently from the
        // message's `from_addr` on purpose: the column is NOCASE.
        store
            .merge_harvested_contacts(
                acct,
                &[ContactEntry {
                    addr: "Alice@Example.com".into(),
                    display_name: None,
                    sent_count: 1,
                    last_sent_at: None,
                }],
            )
            .unwrap();
    });

    let resp = app
        .oneshot(authed("GET", "/client/thread/t1"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let msgs = json["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2);
    let known = |addr: &str| -> Value {
        msgs.iter()
            .find(|m| m["from_addr"] == addr)
            .unwrap_or_else(|| panic!("{addr} missing from the thread"))["sender_known"]
            .clone()
    };
    assert_eq!(known("alice@example.com"), Value::Bool(true));
    assert_eq!(known("promo@nowhere.io"), Value::Bool(false));
    // The flatten must not have disturbed the fields the reader already decodes.
    assert!(msgs[0]["id"].is_i64());
    assert!(msgs[0].get("html").is_some());
    assert!(msgs[0]["attachments"].is_array());
}

#[tokio::test]
async fn thread_view_flags_the_users_own_sent_copies() {
    // WIRE CONTRACT: `is_sent` is a plain bool on EVERY message of the thread,
    // true when THIS ACCOUNT AUTHORED the message — its stored outbound copy, or
    // a received copy whose From is the account's own address — and false for
    // mail from anybody else. The reader right-aligns the user's bubbles on it,
    // so a missing key is not an acceptable "false".
    //
    // The stored `is_sent` column alone would NOT satisfy this: it is a
    // visibility flag, sticky to 0, and self-addressed mail (self-Cc, mail to
    // oneself, a group echo) is deliberately kept at 0 so the visible copy wins.
    // Hence the From-address arm, exercised by the third message here.
    let Harness { app, .. } = harness(|store, acct| {
        store
            .upsert_message(&msg(acct, "g-in", "t1", "lunch?", "what they wrote"))
            .unwrap();
        store
            .upsert_message(&sent_msg(
                acct,
                "g-out",
                "t1",
                "Re: lunch?",
                "Alice <alice@example.com>",
            ))
            .unwrap();
        // Written by the user with themselves on Cc: it landed through the INBOX
        // walk, so it is stored is_sent = 0 forever. Cased unlike the accounts
        // row on purpose — the match is case-insensitive.
        let mut self_cc = msg(acct, "g-cc", "t1", "Re: lunch?", "what I wrote to us both");
        self_cc.from_addr = "Me@Example.COM".into();
        self_cc.from_name = Some("Me".into());
        store.upsert_message(&self_cc).unwrap();
    });

    let resp = app
        .oneshot(authed("GET", "/client/thread/t1"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let msgs = json["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    for m in msgs {
        assert!(
            m["is_sent"].is_boolean(),
            "every message answers is_sent: {m}"
        );
    }
    let is_sent = |gmail_subject: &str| -> Value {
        msgs.iter()
            .find(|m| m["content"] == gmail_subject)
            .unwrap_or_else(|| panic!("{gmail_subject} missing from the thread"))["is_sent"]
            .clone()
    };
    assert_eq!(is_sent("what they wrote"), Value::Bool(false));
    assert_eq!(is_sent("what I wrote"), Value::Bool(true));
    assert_eq!(is_sent("what I wrote to us both"), Value::Bool(true));
}

#[tokio::test]
async fn thread_view_sender_known_needs_a_sent_message_not_merely_contact_row() {
    use squelch_core::store::ContactEntry;
    let Harness { app, .. } = harness(|store, acct| {
        store
            .upsert_message(&msg(acct, "g1", "t1", "hi", "body"))
            .unwrap();
        // sent_count 0: harvested but never actually written to. `is_known_contact`
        // requires sent_count > 0, and the read-time bit must not soften that.
        store
            .merge_harvested_contacts(
                acct,
                &[ContactEntry {
                    addr: "alice@example.com".into(),
                    display_name: None,
                    sent_count: 0,
                    last_sent_at: None,
                }],
            )
            .unwrap();
    });

    let resp = app
        .oneshot(authed("GET", "/client/thread/t1"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["messages"][0]["sender_known"], Value::Bool(false));
}

#[tokio::test]
async fn thread_view_sender_known_needs_the_mail_to_pass_email_authentication() {
    use squelch_core::store::ContactEntry;
    // Three copies of the same known contact's address, differing only in the
    // stored email-authentication verdict. A From header is free to write, so
    // only the authenticated one may claim the reader's pixel bypass — and the
    // NULL row (pre-existing mail, never re-synced) must read as "no".
    let Harness { app, .. } = harness(|store, acct| {
        for (label, auth_pass) in [
            ("authenticated", Some(true)),
            ("failed-auth", Some(false)),
            ("never-evaluated", None),
        ] {
            let mut m = msg(acct, label, "t1", "hi", label);
            m.auth_pass = auth_pass;
            store.upsert_message(&m).unwrap();
        }
        store
            .merge_harvested_contacts(
                acct,
                &[ContactEntry {
                    addr: "alice@example.com".into(),
                    display_name: None,
                    sent_count: 3,
                    last_sent_at: None,
                }],
            )
            .unwrap();
    });

    let resp = app
        .oneshot(authed("GET", "/client/thread/t1"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let msgs = json["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    let known = |label: &str| -> Value {
        msgs.iter()
            .find(|m| m["content"] == label)
            .unwrap_or_else(|| panic!("{label} missing from the thread"))["sender_known"]
            .clone()
    };
    assert_eq!(known("authenticated"), Value::Bool(true));
    assert_eq!(
        known("failed-auth"),
        Value::Bool(false),
        "a known contact still needs an authentication pass"
    );
    assert_eq!(
        known("never-evaluated"),
        Value::Bool(false),
        "NULL auth_pass grants no bypass"
    );
    // WIRE CONTRACT: sender_known stays a plain bool, and the verdict that
    // gates it is not itself exposed.
    assert!(msgs[0]["sender_known"].is_boolean());
    assert!(msgs[0].get("auth_pass").is_none());
}

// --- forwarding (POST /client/actions/send, forward_of_message_id) -----------

/// The raw RFC822 Gmail hands back for the message being forwarded: an html body
/// with a `cid:` reference and the inline part that reference resolves against.
/// Deliberately small — the mock reads one 8KB request.
fn original_raw_b64(body_html: &str) -> String {
    use base64::Engine as _;
    let eml = format!(
        "From: Alice <alice@example.com>\r\n\
         To: Me <me@example.com>\r\n\
         Subject: Quarterly numbers\r\n\
         Date: Tue, 7 Jul 2026 10:00:00 +0000\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/related; boundary=\"orig\"\r\n\
         \r\n\
         --orig\r\n\
         Content-Type: text/html; charset=\"UTF-8\"\r\n\
         \r\n\
         {body_html}\r\n\
         --orig\r\n\
         Content-Type: image/png; name=\"logo.png\"\r\n\
         Content-Transfer-Encoding: base64\r\n\
         Content-Disposition: inline; filename=\"logo.png\"\r\n\
         Content-ID: <logo@corp>\r\n\
         \r\n\
         aGVsbG8gcG5n\r\n\
         --orig--\r\n"
    );
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(eml)
}

/// The `(text, html)` a recipient's client would SEE in a sent forward, decoded
/// out of the quoted-printable transfer encoding both body parts carry. Header
/// and structure assertions read the raw mime; CONTENT assertions come through
/// here.
fn forward_bodies(mime: &str) -> (String, String) {
    let parsed = mail_parser::MessageParser::default()
        .parse(mime.as_bytes())
        .expect("the sent forward parses");
    (
        parsed
            .body_text(0)
            .expect("a text alternative")
            .into_owned(),
        parsed
            .body_html(0)
            .expect("an html alternative")
            .into_owned(),
    )
}

/// The `format=raw` response body carrying [`original_raw_b64`].
fn original_raw_response(body_html: &str) -> String {
    format!(
        "{{\"id\":\"gmail-original\",\"threadId\":\"thread-77\",\
          \"internalDate\":\"1783591200000\",\"raw\":\"{}\"}}",
        original_raw_b64(body_html)
    )
}

/// A `format=raw` response for a multipart/ALTERNATIVE original: a text/plain
/// half and a text/html half saying different things. The shape that proves what
/// the guard reads, because mail-parser hands back the text part verbatim and
/// never converts the html half down when a text half exists.
fn alternative_raw_response(text: &str, html: &str) -> String {
    use base64::Engine as _;
    let eml = format!(
        "From: Alice <alice@example.com>\r\n\
         To: Me <me@example.com>\r\n\
         Subject: Quarterly numbers\r\n\
         Date: Tue, 7 Jul 2026 10:00:00 +0000\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/alternative; boundary=\"orig\"\r\n\
         \r\n\
         --orig\r\n\
         Content-Type: text/plain; charset=\"UTF-8\"\r\n\
         \r\n\
         {text}\r\n\
         --orig\r\n\
         Content-Type: text/html; charset=\"UTF-8\"\r\n\
         \r\n\
         {html}\r\n\
         --orig--\r\n"
    );
    format!(
        "{{\"id\":\"gmail-original\",\"threadId\":\"thread-77\",\
          \"internalDate\":\"1783591200000\",\"raw\":\"{}\"}}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(eml)
    )
}

#[tokio::test]
async fn forwarding_and_replying_at_once_is_refused() {
    // The two disagree about threading and about the subject; there is no
    // sensible message to compose out of both.
    let Harness { app, store, acct } = harness(|store, acct| {
        seed_one_signal(store, acct, "gmail-original", "thread-77", "Lunch?");
    });
    let id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "forward_of_message_id": id,
                "reply_to_message_id": id,
                "to": "carol@example.com",
                "body": "look at this",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let audit = store.list_audit(acct, 10).unwrap();
    assert_eq!(audit[0].action, "send");
    assert_eq!(
        audit[0].detail.as_deref(),
        Some("rejected:forward_and_reply")
    );
    // The target is the message the send was ABOUT, even on a rejection.
    assert_eq!(audit[0].target.as_deref(), Some(&*id.to_string()));
}

#[tokio::test]
async fn a_forward_without_a_recipient_is_refused() {
    // Nobody on the original is who the user means to forward TO, so there is
    // nothing to derive and the send must not compose recipient-less.
    let Harness { app, store, acct } = harness(|store, acct| {
        seed_one_signal(store, acct, "gmail-original", "thread-77", "Lunch?");
    });
    let id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;
    for to in [serde_json::Value::Null, serde_json::json!("   ")] {
        let resp = app
            .clone()
            .oneshot(authed_json(
                "POST",
                "/client/actions/send",
                serde_json::json!({
                    "forward_of_message_id": id,
                    "to": to,
                    "body": "look at this",
                    "confirm": true
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "to = {to:?}");
        let json = body_json(resp).await;
        assert!(json["error"].as_str().unwrap().contains("forward requires"));
    }
    let audit = store.list_audit(acct, 10).unwrap();
    assert_eq!(audit.len(), 2);
    assert!(
        audit
            .iter()
            .all(|a| a.detail.as_deref() == Some("rejected:no_recipient"))
    );
}

#[tokio::test]
async fn an_empty_note_is_a_legitimate_forward_but_not_a_legitimate_send() {
    // The empty-body rejection is skipped for forwards and kept for everything
    // else. No write credential here, so the forward stops at the 403 gate —
    // which is exactly the proof that it cleared the body validation.
    let Harness { app, store, acct } = harness(|store, acct| {
        seed_one_signal(store, acct, "gmail-original", "thread-77", "Lunch?");
    });
    let id = store.search(acct, "lunch", 10, 0).unwrap()[0].id;
    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "forward_of_message_id": id,
                "to": "carol@example.com",
                "body": "",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({ "to": "carol@example.com", "body": "  ", "confirm": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.detail.as_deref() == Some("rejected:empty_body")),
        "a note-less send that is not a forward is still refused"
    );
}

#[tokio::test]
async fn forward_success_sends_a_new_conversation_carrying_the_original() {
    // Two Gmail calls: the original's raw read, then the send. The send answers
    // `{}` (no id), so no echo read-back follows.
    let (base, handle) = mock_gmail_seq(vec![
        (
            200,
            original_raw_response("<p>chart</p><img src=\"cid:logo@corp\">"),
        ),
        (200, "{}".to_string()),
    ])
    .await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(
            store,
            acct,
            "gmail-original",
            "thread-77",
            "Quarterly numbers",
        );
    });
    let id = store.search(acct, "quarterly", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "forward_of_message_id": id,
                "to": "carol@example.com",
                "body": "worth a read",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["status"], "sent");

    let reqs = handle.await.unwrap();
    assert_eq!(reqs.len(), 2);
    assert!(
        reqs[0].starts_with("GET "),
        "the original is read raw first"
    );
    assert!(reqs[0].contains("/messages/gmail-original?format=raw"));
    assert!(reqs[1].contains("/messages/send"));
    // A FORWARD STARTS A NEW CONVERSATION: no threadId on the wire.
    assert!(
        !reqs[1].contains("threadId"),
        "a forward must not be threaded onto the original"
    );

    let mime = sent_mime(&reqs[1]);
    assert!(mime.contains("To: carol@example.com\r\n"));
    assert!(mime.contains("Subject: Fwd: Quarterly numbers\r\n"));
    assert!(!mime.contains("In-Reply-To"), "{mime}");
    assert!(!mime.contains("References"), "{mime}");
    // The note, then the quoted block, in both alternatives. Read DECODED: a
    // forward's body parts ride quoted-printable, because what they carry is a
    // stranger's mail (see write::mime::qp_encode).
    let (text, html) = forward_bodies(&mime);
    assert!(text.contains("worth a read"));
    assert_eq!(
        format!("{text}{html}")
            .matches("---------- Forwarded message ---------")
            .count(),
        2
    );
    assert!(text.contains("From: Alice <alice@example.com>\r\n"));
    // The original's html rides verbatim, and the part its cid names rides with it.
    assert!(html.contains("<p>chart</p><img src=\"cid:logo@corp\">"));
    assert!(mime.contains("filename=\"logo.png\""));
    assert!(mime.contains("Content-ID: <logo@corp>\r\n"));
    assert!(mime.contains("Content-Transfer-Encoding: base64\r\n"));

    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send" && a.detail.as_deref() == Some("ok:forward")),
        "the ledger says a forward went out, not a plain send"
    );
    // FORWARDING IS NOT HANDLING: the attention row is left where it was, unlike
    // a reply, which resolves it.
    let done = mail_list(
        &store,
        acct,
        chrono::Utc::now() - chrono::Duration::days(1),
        None,
        Some(squelch_core::types::AttentionStatus::Done),
        None,
        false,
        SpamScope::Exclude,
    )
    .unwrap();
    assert!(
        done.is_empty(),
        "passing a message on can leave the user's own action outstanding"
    );
}

#[tokio::test]
async fn the_guard_scans_the_original_a_forward_carries() {
    // The classic exfil shape: a clean note wrapped around someone else's
    // secret. The note alone would sail through; what is SENT is what is
    // scanned, so the block comes from the original's own bytes.
    let secret = "-----BEGIN RSA PRIVATE KEY-----";
    let (base, handle) = mock_gmail_seq(vec![(
        200,
        original_raw_response(&format!("<pre>{secret}</pre>")),
    )])
    .await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(
            store,
            acct,
            "gmail-original",
            "thread-77",
            "Quarterly numbers",
        );
    });
    let id = store.search(acct, "quarterly", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "forward_of_message_id": id,
                "to": "carol@example.com",
                "body": "thought you should see this",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let json = body_json(resp).await;
    let err = json["error"].as_str().unwrap();
    assert!(err.contains("pem_block"), "422 lists redacted kinds: {err}");
    assert!(!err.contains("PRIVATE KEY"), "must NEVER echo the match");
    assert_eq!(
        handle.await.unwrap().len(),
        1,
        "read the original, sent nothing"
    );

    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send" && a.detail.as_deref() == Some("blocked:guard"))
    );

    // ...and the override is the ceremony's second act: same request, one flag,
    // now it sends and the bypass is on the record.
    let (base, handle) = mock_gmail_seq(vec![
        (200, original_raw_response(&format!("<pre>{secret}</pre>"))),
        (200, "{}".to_string()),
    ])
    .await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(
            store,
            acct,
            "gmail-original",
            "thread-77",
            "Quarterly numbers",
        );
    });
    let id = store.search(acct, "quarterly", 10, 0).unwrap()[0].id;
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "forward_of_message_id": id,
                "to": "carol@example.com",
                "body": "thought you should see this",
                "confirm": true,
                "override_guard": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(handle.await.unwrap().len(), 2);

    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit.iter().any(|a| {
            a.action == "send"
                && a.detail
                    .as_deref()
                    .is_some_and(|d| d.starts_with("guard_override:") && d.contains("pem_block"))
        }),
        "the override names the kinds it bypassed"
    );
    assert!(
        audit
            .iter()
            .any(|a| a.detail.as_deref() == Some("ok:forward"))
    );
}

#[tokio::test]
async fn a_forward_whose_original_cannot_be_read_is_a_loud_502() {
    // What is forwarded IS the request, so there is nothing to degrade to: no
    // send happens and the client hears about it.
    let (base, handle) = mock_gmail_seq(vec![(500, "{\"error\":\"backend\"}".to_string())]).await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(
            store,
            acct,
            "gmail-original",
            "thread-77",
            "Quarterly numbers",
        );
    });
    let id = store.search(acct, "quarterly", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "forward_of_message_id": id,
                "to": "carol@example.com",
                "body": "worth a read",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    let json = body_json(resp).await;
    assert!(
        json["error"]
            .as_str()
            .unwrap()
            .contains("forward was not sent")
    );
    assert_eq!(handle.await.unwrap().len(), 1, "nothing was sent");

    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send" && a.detail.as_deref() == Some("failed:fetch_original"))
    );
}

#[tokio::test]
async fn forwarding_restricted_mail_reaches_provider_and_reports_upstream_errors() {
    let (base, handle) = mock_gmail(0).await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        let s = store
            .upsert_message(&msg(acct, "gmail-sealed", "t9", "code", "123456"))
            .unwrap();
        store
            .set_triage(
                s,
                acct,
                90,
                Tier::Noise,
                Sensitivity::Sealed,
                Some(SealedKind::Otp),
                "",
                "",
                None,
            )
            .unwrap();
    });
    let sealed_id = store.sealed_messages(acct).unwrap()[0].id;
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "forward_of_message_id": sealed_id,
                "to": "carol@example.com",
                "body": "look",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    handle.abort();

    let audit = store.list_audit(acct, 10).unwrap();
    assert!(audit.iter().any(|a| {
        a.action == "send"
            && a.detail
                .as_deref()
                .is_some_and(|detail| detail.starts_with("failed:") && detail != "failed:target")
    }));
}

#[tokio::test]
async fn the_guard_reads_the_html_half_a_clean_text_half_would_hide() {
    // mail-parser hands back the text/plain part VERBATIM when one exists and
    // never converts the html alternative down, so a scan of the text view alone
    // sees "nothing to see here" and ships the key sitting in the html half.
    // What is scanned has to be what is SENT.
    let secret = "-----BEGIN RSA PRIVATE KEY-----";
    let (base, handle) = mock_gmail_seq(vec![(
        200,
        alternative_raw_response("nothing to see here", &format!("<pre>{secret}</pre>")),
    )])
    .await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(
            store,
            acct,
            "gmail-original",
            "thread-77",
            "Quarterly numbers",
        );
    });
    let id = store.search(acct, "quarterly", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "forward_of_message_id": id,
                "to": "carol@example.com",
                "body": "thought you should see this",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let json = body_json(resp).await;
    let err = json["error"].as_str().unwrap();
    assert!(err.contains("pem_block"), "422 lists redacted kinds: {err}");
    assert!(!err.contains("PRIVATE KEY"), "must NEVER echo the match");
    assert_eq!(
        handle.await.unwrap().len(),
        1,
        "read the original, sent nothing"
    );

    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send" && a.detail.as_deref() == Some("blocked:guard"))
    );
}

/// A `format=raw` response for an original whose BODIES are clean and whose
/// `text/plain` attachment is not: a key pasted into `notes.txt` and dragged
/// onto a mail, which is at least as natural an accident as pasting it inline.
fn text_attachment_raw_response(note: &str) -> String {
    use base64::Engine as _;
    let eml = format!(
        "From: Alice <alice@example.com>\r\n\
         To: Me <me@example.com>\r\n\
         Subject: Quarterly numbers\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"m\"\r\n\
         \r\n\
         --m\r\n\
         Content-Type: text/plain; charset=\"UTF-8\"\r\n\
         \r\n\
         nothing to see here\r\n\
         --m\r\n\
         Content-Type: text/plain; charset=\"UTF-8\"\r\n\
         Content-Disposition: attachment; filename=\"notes.txt\"\r\n\
         \r\n\
         {note}\r\n\
         --m--\r\n"
    );
    format!(
        "{{\"id\":\"gmail-original\",\"threadId\":\"thread-77\",\
          \"internalDate\":\"1783591200000\",\"raw\":\"{}\"}}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(eml)
    )
}

#[tokio::test]
async fn the_guard_reads_a_text_attachment_the_bodies_would_hide() {
    // A clean note, a clean body, and the key sitting in an attached notes.txt.
    // Scanning only the bodies shipped it while this code called forwarding the
    // classic exfil shape.
    let secret = "-----BEGIN RSA PRIVATE KEY-----";
    let (base, handle) = mock_gmail_seq(vec![(200, text_attachment_raw_response(secret))]).await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(
            store,
            acct,
            "gmail-original",
            "thread-77",
            "Quarterly numbers",
        );
    });
    let id = store.search(acct, "quarterly", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "forward_of_message_id": id,
                "to": "carol@example.com",
                "body": "thought you should see this",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let json = body_json(resp).await;
    let err = json["error"].as_str().unwrap();
    assert!(err.contains("pem_block"), "422 lists redacted kinds: {err}");
    assert!(!err.contains("PRIVATE KEY"), "must NEVER echo the match");
    assert_eq!(
        handle.await.unwrap().len(),
        1,
        "read the original, sent nothing"
    );
    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send" && a.detail.as_deref() == Some("blocked:guard"))
    );

    // ...and the override is still the ceremony's second act: same request, one
    // flag, and the attachment goes out with the bypass on the record.
    let (base, handle) = mock_gmail_seq(vec![
        (200, text_attachment_raw_response(secret)),
        (200, "{}".to_string()),
    ])
    .await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(
            store,
            acct,
            "gmail-original",
            "thread-77",
            "Quarterly numbers",
        );
    });
    let id = store.search(acct, "quarterly", 10, 0).unwrap()[0].id;
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "forward_of_message_id": id,
                "to": "carol@example.com",
                "body": "thought you should see this",
                "confirm": true,
                "override_guard": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let reqs = handle.await.unwrap();
    assert_eq!(reqs.len(), 2);
    assert!(sent_mime(&reqs[1]).contains("filename=\"notes.txt\""));
    let audit = store.list_audit(acct, 10).unwrap();
    assert!(audit.iter().any(|a| {
        a.detail
            .as_deref()
            .is_some_and(|d| d.starts_with("guard_override:") && d.contains("pem_block"))
    }));
}

#[tokio::test]
async fn a_forward_of_an_oversized_original_is_refused_not_attempted() {
    // A forward re-encodes everything it reads — decode, base64 back out, JSON,
    // reqwest's buffer — so a 25 MB original is four to five copies of itself in
    // memory and then an opaque 502 pointing at Gmail. The ceiling turns that
    // into a 413 that says which side the problem is on, before the parse
    // allocates a copy of every part.
    let oversized = {
        use base64::Engine as _;
        let mut eml = String::from(
            "From: Alice <alice@example.com>\r\n\
             Subject: Quarterly numbers\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: text/plain; charset=\"UTF-8\"\r\n\
             \r\n",
        );
        eml.push_str(&"x".repeat(20 * 1024 * 1024 + 1));
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(eml)
    };
    let (base, handle) = mock_gmail_seq(vec![(
        200,
        format!("{{\"id\":\"gmail-original\",\"threadId\":\"thread-77\",\"raw\":\"{oversized}\"}}"),
    )])
    .await;
    let Harness { app, store, acct } = app_with_writes(base, |store, acct| {
        seed_one_signal(
            store,
            acct,
            "gmail-original",
            "thread-77",
            "Quarterly numbers",
        );
    });
    let id = store.search(acct, "quarterly", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "forward_of_message_id": id,
                "to": "carol@example.com",
                "body": "worth a read",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let json = body_json(resp).await;
    assert!(
        json["error"]
            .as_str()
            .unwrap()
            .contains("too large to forward"),
        "{json}"
    );
    assert_eq!(
        handle.await.unwrap().len(),
        1,
        "the original was read, nothing was sent"
    );

    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send" && a.detail.as_deref() == Some("rejected:too_large"))
    );
}

/// A write credential store that has nothing left to hand out: the shape of an
/// expired or revoked write token whose refresh has stopped working.
struct DeadCreds;
#[async_trait]
impl CredentialStore for DeadCreds {
    async fn token(&self, _a: i64) -> squelch_core::Result<OAuthToken> {
        Err(squelch_core::error::CoreError::Credential(
            "invalid_grant".into(),
        ))
    }
}

#[tokio::test]
async fn a_forward_whose_write_credential_is_dead_says_so_instead_of_blaming_gmail() {
    // The raw fetch runs on the WRITE token too, so a dead credential fails
    // inside it — before any send. Collapsing that into the fetch's 502 told the
    // composer to blame Gmail when the fix is `squelchd auth`, which the
    // client already knows how to say when it sees the 403.
    let (base, handle) = mock_gmail(0).await;
    let (state, store, acct) = state_with(|store, acct| {
        seed_one_signal(
            store,
            acct,
            "gmail-original",
            "thread-77",
            "Quarterly numbers",
        );
    });
    let state = state.with_write_test_harness(Arc::new(DeadCreds), base);
    let app = router(state);
    let id = store.search(acct, "quarterly", 10, 0).unwrap()[0].id;

    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "forward_of_message_id": id,
                "to": "carol@example.com",
                "body": "worth a read",
                "confirm": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let json = body_json(resp).await;
    assert!(
        json["error"].as_str().unwrap().contains("squelchd auth"),
        "the 403 names the fix: {json}"
    );
    handle.abort();

    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send"
                && a.detail.as_deref() == Some("rejected:no_write_credential")),
        "a dead credential is not a fetch failure: {audit:?}"
    );
    assert!(
        !audit
            .iter()
            .any(|a| a.detail.as_deref() == Some("failed:fetch_original")),
        "and it must not be audited as one"
    );
}

/// The daemon says whether Gmail still opens, and OMITS the answer when it
/// cannot see. A door wired without a metrics registry must not report a
/// cheerful "connected" on behalf of a process that has no idea.
#[tokio::test]
async fn stats_omit_the_gmail_answer_when_this_door_cannot_see_the_credential() {
    let Harness { app, .. } = harness(|_, _| {});
    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    let json = body_json(resp).await;
    assert!(
        json.get("gmail").is_none(),
        "absence means 'no answer'; a client must not render it as good news"
    );
}

/// A live credential reads as connected, and carries NO reconnect link: an
/// invitation to re-consent for no reason is worse than no invitation.
#[tokio::test]
async fn stats_report_a_working_mailbox_as_connected_with_nothing_to_do() {
    let (state, _store, _acct) = state_with(|_, _| {});
    let metrics = squelch_core::metrics::SyncMetrics::new();
    let app = router(
        state
            .with_sync_metrics(metrics.clone())
            .with_console_sso_url(Some("https://signup.example".into())),
    );

    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["gmail"]["connected"], serde_json::json!(true));
    assert!(json["gmail"].get("reconnect_url").is_none());
    assert!(json["gmail"].get("disconnected_since").is_none());
}

/// A dead credential reads as disconnected, says since when, and on HOSTED
/// hands back the link that repairs it.
#[tokio::test]
async fn stats_report_a_dead_credential_with_the_link_that_repairs_it() {
    let (state, _store, _acct) = state_with(|_, _| {});
    let metrics = squelch_core::metrics::SyncMetrics::new();
    metrics.record_gmail_error(squelch_core::metrics::GmailErrorKind::Auth);
    let app = router(
        state
            .with_sync_metrics(metrics.clone())
            .with_console_sso_url(Some("https://signup.example".into())),
    );

    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["gmail"]["connected"], serde_json::json!(false));
    // The mailbox rides along as `hint`, URL-encoded, so the control plane
    // pre-selects it at Google and refuses a consent from any other account.
    assert_eq!(
        json["gmail"]["reconnect_url"],
        serde_json::json!("https://signup.example/reconnect?hint=me%40example.com")
    );
    assert!(
        json["gmail"]["disconnected_since"].as_str().is_some(),
        "since when is the half a person actually reads"
    );
}

/// SELF-HOST: the same dead credential, no link. `squelchd auth` at a shell is
/// not something a button can do, and offering one that goes nowhere is worse
/// than the sentence that tells the truth.
#[tokio::test]
async fn a_self_host_mailbox_is_told_it_is_disconnected_and_offered_no_link() {
    let (state, _store, _acct) = state_with(|_, _| {});
    let metrics = squelch_core::metrics::SyncMetrics::new();
    metrics.record_gmail_error(squelch_core::metrics::GmailErrorKind::Auth);
    // No console SSO origin configured: that IS the self-host branch.
    let app = router(state.with_sync_metrics(metrics.clone()));

    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["gmail"]["connected"], serde_json::json!(false));
    assert!(json["gmail"].get("reconnect_url").is_none());
}

// --- calendar access on /client/stats ---------------------------------------

/// A credentials file in the daemon's own on-disk shape with one Read token
/// whose recorded scopes are `scopes`, and the resolver over it.
fn calendar_resolver(
    tag: &str,
    account_id: i64,
    scopes: Option<&[&str]>,
) -> (
    squelch_core::credentials::CalendarCredentials,
    std::path::PathBuf,
) {
    use squelch_core::credentials::{CalendarCredentials, CredentialKind, StoredToken};
    let path = std::env::temp_dir().join(format!(
        "squelch-api-calendar-{tag}-{}-{}.json",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    let token = StoredToken {
        access_token: "a".into(),
        refresh_token: Some("r".into()),
        expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
        scopes: scopes.map(|s| s.iter().map(|s| s.to_string()).collect()),
    };
    squelch_core::credentials::store_tokens_backend(
        squelch_core::config::CredentialBackend::File,
        &path,
        "me@example.com",
        &[
            (CredentialKind::Read, token.clone()),
            (CredentialKind::Write, token),
        ],
    )
    .unwrap();
    let resolver = CalendarCredentials::new(
        account_id,
        "me@example.com".into(),
        squelch_core::config::CredentialBackend::File,
        path.clone(),
        squelch_core::config::OAuthClientConfig {
            client_id: "id".into(),
            client_secret: "secret".into(),
            issuer: Default::default(),
        },
    )
    .unwrap();
    (resolver, path)
}

const CAL_EVENTS: &str = squelch_core::config::CALENDAR_EVENTS_SCOPE;
const READONLY_SCOPE: &str = squelch_core::config::GMAIL_READONLY_SCOPE;

/// HOSTED, NOT GRANTED: the hosted tenant's one token carries only Gmail, so
/// `granted` is false and the link that grants it names this mailbox.
#[tokio::test]
async fn stats_offer_a_hosted_mailbox_the_calendar_link() {
    let (state, _store, acct) = state_with(|_, _| {});
    let (resolver, path) = calendar_resolver("hosted-off", acct, Some(&[READONLY_SCOPE]));
    let app = router(
        state
            .with_calendar_credentials(resolver)
            .with_console_sso_url(Some("https://signup.example/".into()))
            .with_features(calendar_on()),
    );
    let json = body_json(app.oneshot(authed("GET", "/client/stats")).await.unwrap()).await;
    eprintln!("hosted, not granted: {}", json["calendar"]);
    assert_eq!(
        json["calendar"],
        serde_json::json!({
            "granted": false,
            "connect_url": "https://signup.example/reconnect?calendar=1&hint=me%40example.com",
            "connect_command": null,
            "self_connect": false,
        })
    );
    std::fs::remove_file(&path).ok();
}

/// HOSTED, GRANTED: the same token with calendar on its recorded scopes.
#[tokio::test]
async fn stats_report_a_hosted_calendar_grant() {
    let (state, _store, acct) = state_with(|_, _| {});
    let (resolver, path) =
        calendar_resolver("hosted-on", acct, Some(&[READONLY_SCOPE, CAL_EVENTS]));
    let app = router(
        state
            .with_calendar_credentials(resolver)
            .with_console_sso_url(Some("https://signup.example".into()))
            .with_features(calendar_on()),
    );
    let json = body_json(app.oneshot(authed("GET", "/client/stats")).await.unwrap()).await;
    eprintln!("hosted, granted: {}", json["calendar"]);
    assert_eq!(
        json["calendar"],
        serde_json::json!({
            "granted": true,
            "connect_url": "https://signup.example/reconnect?calendar=1&hint=me%40example.com",
            "connect_command": null,
            "self_connect": false,
        })
    );
    std::fs::remove_file(&path).ok();
}

/// SELF-HOST: no console origin, so the answer is the command, never a link.
/// A token stored before scopes were recorded reads as not granted.
#[tokio::test]
async fn stats_tell_a_self_host_the_calendar_command() {
    let (state, _store, acct) = state_with(|_, _| {});
    let (resolver, path) = calendar_resolver("selfhost", acct, None);
    let app = router(
        state
            .with_calendar_credentials(resolver)
            .with_features(calendar_on()),
    );
    let json = body_json(app.oneshot(authed("GET", "/client/stats")).await.unwrap()).await;
    eprintln!("self-host: {}", json["calendar"]);
    assert_eq!(
        json["calendar"],
        serde_json::json!({
            "granted": false,
            "connect_url": null,
            "connect_command": "squelchd auth --calendar",
            "self_connect": true,
        })
    );
    std::fs::remove_file(&path).ok();

    // And a door with no resolver at all answers the same shape, not absence.
    let Harness { app, .. } = common::calendar_harness(|_, _| {});
    let json = body_json(app.oneshot(authed("GET", "/client/stats")).await.unwrap()).await;
    assert_eq!(json["calendar"]["granted"], serde_json::json!(false));
    assert_eq!(
        json["calendar"]["connect_command"],
        serde_json::json!("squelchd auth --calendar")
    );
    // With no OAuth client there is nothing to run a consent with.
    assert_eq!(json["calendar"]["self_connect"], serde_json::json!(false));
}

/// An address with characters a query string would misread is encoded, and
/// so cannot add a parameter to the link.
#[tokio::test]
async fn the_calendar_link_encodes_the_address() {
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let acct = store.ensure_account("a+b&calendar=0@example.com").unwrap();
    let state = ApiState::new(store, acct, TOKEN)
        .with_console_sso_url(Some("https://signup.example".into()))
        .with_features(calendar_on());
    let json = body_json(
        router(state)
            .oneshot(authed("GET", "/client/stats"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        json["calendar"]["connect_url"],
        serde_json::json!(
            "https://signup.example/reconnect?calendar=1&hint=a%2Bb%26calendar%3D0%40example.com"
        )
    );
}

/// SYNC AND TRIAGE HEALTH ARE ABSENT FROM A DOOR THAT CANNOT SEE THEM, like
/// `gmail`: an old or unwired daemon must read as "no answer", not "healthy".
#[tokio::test]
async fn stats_omit_sync_and_triage_when_this_door_cannot_see_them() {
    let Harness { app, .. } = harness(|_, _| {});
    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    let json = body_json(resp).await;
    assert!(json.get("sync").is_none());
    assert!(json.get("triage").is_none());
}

/// THE HOSTED OUTAGE: Gmail is rate-limiting a mailbox whose credential is
/// fine. `gmail.connected` stays true (it means the credential), and `sync`
/// says what is actually wrong, when the next try is, and that triage is
/// waiting on it with work queued.
#[tokio::test]
async fn stats_report_a_throttled_mailbox_and_the_triage_waiting_on_it() {
    use squelch_core::metrics::{GmailErrorKind, SyncMetrics};
    let (state, store, acct) = state_with(|store, acct| {
        let m = seed_one_signal(store, acct, "g-q", "t-q", "waiting");
        squelch_core::store::agent_triage::AgentTriageStore::enqueue_agent_triage(
            store, acct, m, "test", false,
        )
        .unwrap();
    });
    let metrics = SyncMetrics::new();
    metrics.record_gmail_error_reason(GmailErrorKind::Quota, "RATE_LIMIT_EXCEEDED");
    metrics.record_sync_error();
    let retry = chrono::Utc::now().timestamp() + 120;
    metrics.note_sync_retry_at(retry);
    let app = router(state.with_sync_metrics(metrics.clone()));

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/stats"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["gmail"]["connected"], serde_json::json!(true));
    let sync = &json["sync"];
    assert_eq!(sync["ok"], serde_json::json!(false));
    assert_eq!(sync["consecutive_failures"], 1);
    assert_eq!(sync["error"]["kind"], "quota");
    assert_eq!(sync["error"]["reason"], "RATE_LIMIT_EXCEEDED");
    assert!(sync["error"]["at"].is_string());
    let next =
        chrono::DateTime::parse_from_rfc3339(sync["next_retry_at"].as_str().unwrap()).unwrap();
    assert_eq!(next.timestamp(), retry);
    assert!(sync.get("last_success_at").is_none(), "it never synced");

    let triage = &json["triage"];
    assert_eq!(triage["waiting_for_sync"], serde_json::json!(true));
    let [queued, leased] = store
        .triage_waiting_counts(acct, chrono::Utc::now())
        .unwrap();
    assert!(queued > 0, "the fixture queued work");
    assert_eq!(triage["queued"], queued);
    assert_eq!(triage["leased"], leased);
    assert_eq!(triage["budget_parked"], 0);
    assert!(triage.get("cooldown_until").is_none());
    assert!(triage.get("budget_resumes_at").is_none());

    // Gmail answers: the outage is over, the error and retry go with it.
    metrics.note_poll_ok();
    let later = chrono::Utc::now().timestamp() + 600;
    metrics.note_triage_cooldown(later);
    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    let json = body_json(resp).await;
    let sync = &json["sync"];
    assert_eq!(sync["ok"], serde_json::json!(true));
    assert_eq!(sync["consecutive_failures"], 0);
    assert!(sync.get("error").is_none());
    assert!(sync.get("next_retry_at").is_none());
    assert!(sync["last_success_at"].is_string());
    assert_eq!(json["triage"]["waiting_for_sync"], serde_json::json!(false));
    assert!(json["triage"]["cooldown_until"].is_string());
}

/// A FRESH DAEMON IS NOT AN OUTAGE. Before the first poll (onboarding's
/// backfill, any restart) nothing has failed, so `sync.ok` is true and triage
/// is not "waiting for sync"; the backfill shows up as `catch_up` progress.
#[tokio::test]
async fn stats_do_not_report_an_outage_before_anything_failed() {
    use squelch_core::metrics::SyncMetrics;
    let (state, _store, _acct) = state_with(|_, _| {});
    let metrics = SyncMetrics::new();
    metrics.catchup_begin(40);
    metrics.catchup_step();
    let app = router(state.with_sync_metrics(metrics.clone()));
    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    let json = body_json(resp).await;
    assert!(!metrics.poll_ok(), "no poll has run");
    assert_eq!(json["sync"]["ok"], serde_json::json!(true));
    assert!(json["sync"].get("error").is_none());
    assert_eq!(json["triage"]["waiting_for_sync"], serde_json::json!(false));
    assert_eq!(
        json["catch_up"],
        serde_json::json!({"done": 1, "total": 40})
    );
}

/// A SIDE-CALL ERROR IS NOT A SYNC OUTAGE. With sync healthy, an error the
/// spam tab or unread count recorded (and any stray retry stamp) stays out
/// of `sync`: `error` and `next_retry_at` describe a current outage only.
#[tokio::test]
async fn stats_hide_a_side_call_error_while_sync_is_healthy() {
    use squelch_core::metrics::{GmailErrorKind, SyncMetrics};
    let (state, _store, _acct) = state_with(|_, _| {});
    let metrics = SyncMetrics::new();
    metrics.record_gmail_error_reason(GmailErrorKind::Quota, "RATE_LIMIT_EXCEEDED");
    metrics.record_sync_error();
    metrics.note_poll_ok();
    // After the good poll: a side call fails and a retry time is left over.
    metrics.record_gmail_error_reason(GmailErrorKind::Http, "backendError");
    metrics.note_sync_retry_at(chrono::Utc::now().timestamp() + 60);
    assert!(metrics.sync_last_error().is_some());
    let app = router(state.with_sync_metrics(metrics));
    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    let json = body_json(resp).await;
    let sync = &json["sync"];
    assert_eq!(sync["ok"], serde_json::json!(true));
    assert!(sync.get("error").is_none(), "{sync}");
    assert!(sync.get("next_retry_at").is_none(), "{sync}");
}

/// Seed one Stage-1-decided row a re-triage will pick up.
fn seed_retriageable(store: &SqliteStore, acct: i64) {
    let m = seed_one_signal(store, acct, "g-r", "t-r", "again");
    store
        .stage1_apply(&squelch_core::store::Stage1Applied {
            message_id: m,
            account_id: acct,
            importance: 50,
            tier: Tier::Signal,
            one_line: "x".into(),
            reason: "x".into(),
            field_reasons: Default::default(),
            stage1_model_used: "claude-x".into(),
            needs_stage2: false,
            escalation_reason: None,
            deadline: None,
            category: Some("general".into()),
        })
        .unwrap();
}

/// A STALLED RE-TRIAGE SAYS WHY. Gmail down outranks a provider cooldown, and
/// a door with no metrics says nothing rather than guessing.
#[tokio::test]
async fn retriage_progress_names_what_is_holding_the_run() {
    use squelch_core::metrics::SyncMetrics;
    let (state, _store, _acct) = state_with(seed_retriageable);
    let metrics = SyncMetrics::new();
    let blind = router(state.clone());
    let app = router(state.with_sync_metrics(metrics.clone()));

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/retriage"))
        .await
        .unwrap();
    assert!(
        body_json(resp).await.get("blocked").is_none(),
        "no run in flight, nothing to block"
    );

    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/retriage",
            serde_json::json!({ "days": 7 }),
        ))
        .await
        .unwrap();
    assert_eq!(body_json(resp).await["reset"], 1);

    // Nothing has polled yet, and nothing has failed: a fresh daemon is not
    // blocked on Gmail, whatever `poll_ok` says.
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/retriage"))
        .await
        .unwrap();
    assert!(
        body_json(resp).await.get("blocked").is_none(),
        "not polled yet is not waiting for sync"
    );

    // A lifecycle fails: triage is waiting for Gmail.
    metrics.record_sync_error();
    let retry = chrono::Utc::now().timestamp() + 60;
    metrics.note_sync_retry_at(retry);
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/retriage"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["blocked"]["reason"], "waiting_for_sync");
    let until =
        chrono::DateTime::parse_from_rfc3339(json["blocked"]["until"].as_str().unwrap()).unwrap();
    assert_eq!(until.timestamp(), retry);

    // Gmail answers, but the model provider is cooling off.
    metrics.note_poll_ok();
    metrics.note_triage_cooldown(chrono::Utc::now().timestamp() + 300);
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/retriage"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["blocked"]["reason"], "provider_cooldown");
    assert!(json["blocked"]["until"].is_string());

    let resp = blind
        .oneshot(authed("GET", "/client/retriage"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["total"], 1);
    assert!(json.get("blocked").is_none(), "a blind door does not guess");
}

// --- the spam page over HTTP ------------------------------------------------

/// Seed one ordinary signal message and one the provider filtered, and return
/// `(good, spam)`. Both carry a triage row, because a spam row gets one at
/// ingest like everything else — a neutral one nothing ever looked at.
fn seed_inbox_and_spam(store: &SqliteStore, acct: i64) -> (i64, i64) {
    let good = seed_one_signal(store, acct, "g-good", "t-good", "lunch tomorrow");
    let mut m = msg(acct, "g-spam", "t-spam", "you have won", "claim your prize");
    m.is_spam = true;
    // Distinct from the subject, so the row-text test can tell the two fields
    // apart — the shared fixture spells the snippet as the subject.
    m.snippet = "claim your prize".to_string();
    let spam = store.upsert_message(&m).unwrap();
    store
        .set_triage(
            spam,
            acct,
            0,
            Tier::Noise,
            Sensitivity::Normal,
            None,
            "",
            "",
            None,
        )
        .unwrap();
    (good, spam)
}

/// THE DEFAULT IS THE MAILBOX. Every existing client sends no `spam` parameter
/// and must keep getting exactly what it got before this route learned the word.
#[tokio::test]
async fn updates_exclude_spam_when_the_parameter_is_absent() {
    let mut spam_id = 0;
    let Harness { app, .. } = harness(|store, acct| {
        let (_, spam) = seed_inbox_and_spam(store, acct);
        spam_id = spam;
    });

    let resp = app.oneshot(authed("GET", "/client/updates")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "only the ordinary message");
    assert_ne!(items[0]["id"].as_i64().unwrap(), spam_id);
}

/// And `spam=only` is the page: the other side of the verdict, nothing else.
#[tokio::test]
async fn updates_serve_only_spam_when_asked_for_it() {
    let mut spam_id = 0;
    let Harness { app, .. } = harness(|store, acct| {
        let (_, spam) = seed_inbox_and_spam(store, acct);
        spam_id = spam;
    });

    let resp = app
        .oneshot(authed("GET", "/client/updates?spam=only"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "only the filtered message");
    assert_eq!(items[0]["id"].as_i64().unwrap(), spam_id);
}

/// Pending inbox and spam rows remain readable before model classification.
#[tokio::test]
async fn the_spam_page_serves_the_subject_and_the_opening_line() {
    let Harness { app, .. } = harness(|store, acct| {
        seed_inbox_and_spam(store, acct);
    });

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/updates?spam=only"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    let row = &json["items"].as_array().unwrap()[0];
    assert_eq!(
        row["one_line"],
        serde_json::json!("you have won"),
        "pending inventory uses the subject"
    );
    assert_eq!(row["subject"], serde_json::json!("you have won"));
    assert_eq!(row["preview"], serde_json::json!("claim your prize"));

    let resp = app.oneshot(authed("GET", "/client/updates")).await.unwrap();
    let json = body_json(resp).await;
    let row = &json["items"].as_array().unwrap()[0];
    assert_eq!(row["subject"], "lunch tomorrow");
    assert_eq!(row["preview"], "lunch tomorrow");
    assert_eq!(row["reason"], "Triage pending");
}

/// AN UNKNOWN VALUE IS A 400, never a silent full listing. A client asking for
/// the spam folder and quietly getting the inbox back — or the reverse — is the
/// worst failure this parameter has, because both answers look like real data.
#[tokio::test]
async fn updates_reject_an_unknown_spam_value() {
    let Harness { app, .. } = harness(|store, acct| {
        seed_inbox_and_spam(store, acct);
    });
    for bad in ["yes", "true", "1", "all", "exclude", ""] {
        let resp = app
            .clone()
            .oneshot(authed("GET", &format!("/client/updates?spam={bad}")))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "spam={bad:?} must be refused, not guessed at"
        );
    }
}

/// The spam count rides on /client/stats as its own number, and the tier counts
/// it used to inflate do not see it.
#[tokio::test]
async fn stats_carry_the_spam_count_apart_from_the_tiers() {
    let Harness { app, .. } = harness(|store, acct| {
        seed_inbox_and_spam(store, acct);
    });
    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["spam"], serde_json::json!(1));
    assert!(
        json["tier_counts"].get("noise").is_none()
            || json["tier_counts"]["noise"] == serde_json::json!(0),
        "the filtered row must not count as noise: {}",
        json["tier_counts"]
    );
}

/// NOT SPAM NEEDS THE CONFIRM GATE and a write credential, like every other
/// message-scoped action. Without a write credential the harness has none, so
/// the confirmed call stops at 403 rather than reaching Gmail — which is the
/// assertion that matters here: the route is wired, guarded, and ordered.
#[tokio::test]
async fn not_spam_is_confirm_gated_and_needs_a_write_credential() {
    let mut spam_id = 0;
    let Harness { app, store, acct } = harness(|store, acct| {
        let (_, spam) = seed_inbox_and_spam(store, acct);
        spam_id = spam;
    });

    // No confirm: refused before anything is touched.
    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/actions/not_spam",
            serde_json::json!({ "message_id": spam_id }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Confirmed, but this harness has no write credential.
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/not_spam",
            serde_json::json!({ "message_id": spam_id, "confirm": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // AND THE LOCAL FLAG IS UNTOUCHED, which is the ordering guarantee: the
    // Gmail write runs first, so a refused one leaves the message exactly where
    // Gmail still has it rather than visible here and filtered everywhere else.
    let rows = mail_list(
        &store,
        acct,
        chrono::Utc::now() - chrono::Duration::days(1),
        None,
        None,
        None,
        false,
        SpamScope::Only,
    )
    .unwrap();
    assert_eq!(rows.len(), 1, "the row is still spam");
}

/// THE SHAPE OF THE ACKNOWLEDGEMENT, pinned because the client decodes it and
/// nothing else checks that seam.
///
/// This route answers `{"triggered": bool}` — the same shape `POST
/// /client/refresh` has always used, because it is the same kind of poke at the
/// same sync loop. It is asserted by KEY rather than by "some JSON came back":
/// the client briefly decoded this into a type requiring a `status` field, the
/// POST arrived and the spam folder synced perfectly, and the DECODE of the
/// reply threw — which the client reported as being unable to reach the
/// provider. `passband/Tests/SpamWireTests.swift` holds the other half.
#[tokio::test]
async fn spam_refresh_answers_the_triggered_shape() {
    let Harness { app, .. } = harness(|_, _| {});
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/spam/refresh",
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert!(
        json.get("triggered").and_then(|v| v.as_bool()).is_some(),
        "the acknowledgement must carry a boolean `triggered`: {json}"
    );
    // No sync loop is wired into the test harness, so it reports that honestly
    // rather than claiming to have started something.
    assert_eq!(json["triggered"], serde_json::json!(false));
    assert!(
        json.get("status").is_none(),
        "and NOT a `status` field — the client type that wanted one was the bug"
    );
}

#[tokio::test]
async fn senders_autocomplete_ranks_prefix_then_volume_and_hides_spam_and_sent() {
    // WIRE CONTRACT: GET /client/senders?q= returns a flat array of
    // {addr, display_name, msg_count, last_received_at}, ranked prefix-match
    // first, then by volume; an empty fragment lists the senders with the most
    // mail (the reader has just typed `from:`), the limit is capped, and the
    // response is no-store. Sent mail and spam register nobody.
    let Harness { app, .. } = harness(|store, acct| {
        let seed = |gmail: &str, from: &str, name: Option<&str>| {
            let mut m = msg(acct, gmail, gmail, "hello", "body");
            m.from_addr = from.to_string();
            m.from_name = name.map(String::from);
            m
        };
        store
            .upsert_message(&seed("g1", "dan@example.com", Some("Dan Smith")))
            .unwrap();
        store
            .upsert_message(&seed("g2", "dan@example.com", Some("Dan Smith")))
            .unwrap();
        store
            .upsert_message(&seed("g3", "joanne@example.com", None))
            .unwrap();
        store
            .upsert_message(&seed("g4", "ann@example.com", None))
            .unwrap();
        let mut spam = seed("g5", "winner@lottery.example", Some("Ann Lottery"));
        spam.is_spam = true;
        store.upsert_message(&spam).unwrap();
        let mut sent = seed("g6", "me@example.com", Some("Me"));
        sent.is_sent = true;
        store.upsert_message(&sent).unwrap();
    });

    // "an" is a substring of dan@ and joanne@ (2 vs 1 messages) and the start
    // of ann@: prefix first, then volume.
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/senders?q=an"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let items = body_json(resp).await;
    let addrs: Vec<&str> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["addr"].as_str().unwrap())
        .collect();
    assert_eq!(
        addrs,
        vec!["ann@example.com", "dan@example.com", "joanne@example.com"]
    );
    assert_eq!(items[1]["display_name"], "Dan Smith");
    assert_eq!(items[1]["msg_count"], 2);
    assert!(items[1]["last_received_at"].is_string());
    assert!(items[2]["display_name"].is_null());

    // Neither the spam sender (matched on her display name) nor the user's own
    // sent copy is in the directory.
    for q in ["lottery", "Ann%20L", "me@"] {
        let resp = app
            .clone()
            .oneshot(authed("GET", &format!("/client/senders?q={q}")))
            .await
            .unwrap();
        let items = body_json(resp).await;
        let offered: Vec<String> = items
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["addr"].as_str().unwrap().to_string())
            .collect();
        assert!(
            !offered
                .iter()
                .any(|a| a.contains("lottery") || a == "me@example.com"),
            "{q}: {offered:?}"
        );
    }

    // An empty fragment is the volume listing, spam and sent still absent; and
    // the limit is honoured and capped. Asked THREE ways, because the Mac
    // client drops empty query values when it builds a URL: a blank `q`, a
    // whitespace `q`, and no `q` at all must all be the same question.
    for uri in [
        "/client/senders?q=%20%20",
        "/client/senders?q=",
        "/client/senders",
    ] {
        let resp = app.clone().oneshot(authed("GET", uri)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{uri}");
        let items = body_json(resp).await;
        let addrs: Vec<&str> = items
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["addr"].as_str().unwrap())
            .collect();
        assert_eq!(
            addrs,
            vec!["dan@example.com", "ann@example.com", "joanne@example.com"],
            "{uri}: volume first, then recency, then address"
        );
    }
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/senders?q=example&limit=1"))
        .await
        .unwrap();
    assert_eq!(body_json(resp).await.as_array().unwrap().len(), 1);

    // No token, no directory.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/client/senders?q=dan")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sender_domains_roll_up_by_host_for_the_rule_editor() {
    // WIRE CONTRACT: GET /client/sender-domains?q= returns a flat array of
    // {domain, sender_count, msg_count, last_received_at}, a host that starts
    // with the fragment first, then by volume; an empty or absent fragment
    // lists the busiest hosts, the limit is capped, the response is no-store.
    let Harness { app, .. } = harness(|store, acct| {
        let seed = |gmail: &str, from: &str| {
            let mut m = msg(acct, gmail, gmail, "hello", "body");
            m.from_addr = from.to_string();
            m
        };
        for (gmail, from) in [
            ("g1", "receipts@stripe.com"),
            ("g2", "support@stripe.com"),
            ("g3", "news@e.stripe.com"),
            ("g4", "dan@example.com"),
        ] {
            store.upsert_message(&seed(gmail, from)).unwrap();
        }
        let mut spam = seed("g5", "winner@stripe-lottery.example");
        spam.is_spam = true;
        store.upsert_message(&spam).unwrap();
    });

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/sender-domains?q=stripe"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let items = body_json(resp).await;
    let domains: Vec<&str> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["domain"].as_str().unwrap())
        .collect();
    assert_eq!(domains, vec!["stripe.com", "e.stripe.com"]);
    assert_eq!(items[0]["sender_count"], 2);
    assert_eq!(items[0]["msg_count"], 2);
    assert!(items[0]["last_received_at"].is_string());

    for uri in ["/client/sender-domains?q=", "/client/sender-domains"] {
        let resp = app.clone().oneshot(authed("GET", uri)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{uri}");
        let items = body_json(resp).await;
        assert_eq!(items.as_array().unwrap().len(), 3, "{uri}");
        assert_eq!(items[0]["domain"], "stripe.com", "{uri}");
    }
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/sender-domains?limit=1"))
        .await
        .unwrap();
    assert_eq!(body_json(resp).await.as_array().unwrap().len(), 1);

    // No token, no directory.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/client/sender-domains?q=stripe")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn person_card_reports_one_address_without_its_sealed_mail() {
    // WIRE CONTRACT: GET /client/person?addr= returns {addr, display_name?,
    // is_self, known_contact, received?, sent?, rule?, groups, recent_threads}.
    // Sealed mail is in no count and no list; optional parts are ABSENT when
    // there is nothing to say; the response is no-store; no token, no card.
    let Harness { app, .. } = harness(|store, acct| {
        let seed = |gmail: &str, thread: &str, subject: &str| {
            let mut m = msg(acct, gmail, thread, subject, "body");
            m.from_addr = "dan@example.com".to_string();
            m.from_name = Some("Dan Smith".to_string());
            m
        };
        store.upsert_message(&seed("g1", "t1", "lunch?")).unwrap();
        store.upsert_message(&seed("g2", "t2", "the deck")).unwrap();
        let sealed = store
            .upsert_message(&seed("g3", "t3", "your pangolin code"))
            .unwrap();
        store
            .set_triage(
                sealed,
                acct,
                50,
                Tier::Signal,
                Sensitivity::Sealed,
                Some(SealedKind::Otp),
                "",
                "",
                None,
            )
            .unwrap();
    });

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/person?addr=Dan%40Example.com"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let card = body_json(resp).await;
    assert_eq!(card["display_name"], "Dan Smith");
    assert_eq!(card["is_self"], false);
    assert_eq!(
        card["received"]["count"], 2,
        "the sealed message is not counted"
    );
    assert!(card["received"]["first_at"].is_string());
    let subjects: Vec<&str> = card["recent_threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["subject"].as_str().unwrap())
        .collect();
    assert!(!subjects.contains(&"your pangolin code"), "{subjects:?}");
    assert_eq!(subjects.len(), 2);
    // Never written to and no rule: absent, not null.
    let obj = card.as_object().unwrap();
    assert!(
        !obj.contains_key("sent") && !obj.contains_key("rule"),
        "{card}"
    );

    // A stranger is an empty card, not an error; your own address says so.
    let resp = app
        .clone()
        .oneshot(authed(
            "GET",
            "/client/person?addr=nobody%40nowhere.example",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let card = body_json(resp).await;
    assert!(!card.as_object().unwrap().contains_key("received"));
    assert_eq!(card["recent_threads"].as_array().unwrap().len(), 0);

    for uri in [
        "/client/person?addr=",
        "/client/person?addr=dan",
        "/client/person",
    ] {
        let resp = app.clone().oneshot(authed("GET", uri)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
    }

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/client/person?addr=dan%40example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Ingest one inbound message from `from` dated `days_ago`, stamped with a
/// `Date` offset of `offset` (`+0530` and so on), through the real pipeline.
/// Delivered the way Gmail delivers authenticated mail (its own `Received`
/// above a passing verdict for the From's domain): only authenticated mail
/// teaches a correspondent's timezone.
fn ingest_dated(
    store: &SqliteStore,
    acct: i64,
    gmail: &str,
    from: &str,
    days_ago: i64,
    offset: &str,
) {
    use squelch_core::config::Stage1Config;
    use squelch_core::sync::ingest::{RawFetched, ingest};
    let when = chrono::Utc::now() - chrono::Duration::days(days_ago);
    let domain = from
        .rsplit_once('@')
        .map(|(_, d)| d.trim_end_matches('>'))
        .unwrap_or_default();
    let eml = format!(
        "Received: from mail.{domain} (mail.{domain}. [192.0.2.1]) by mx.google.com with SMTPS id a1\r\n\
         Authentication-Results: mx.google.com; dkim=pass header.i=@{domain}; dmarc=pass header.from={domain}\r\n\
         From: {from}\r\nTo: me@example.com\r\nSubject: hi\r\nDate: {} {offset}\r\n\r\nhello\r\n",
        when.format("%a, %d %b %Y %H:%M:%S")
    );
    let fetched = RawFetched {
        account_id: acct,
        gmail_msg_id: gmail.to_string(),
        gmail_thread_id: Some(gmail.to_string()),
        raw: eml.into_bytes(),
        internal_date: Some(when),
        is_sent: false,
        is_spam: false,
        account_addr: "me@example.com".to_string(),
    };
    let triaged = ingest(
        &fetched,
        &Stage1Config::default(),
        chrono::Utc::now(),
        |_| false,
    );
    store.ingest_message(&triaged).unwrap();
}

#[tokio::test]
async fn person_card_carries_the_inferred_timezone_and_put_overrides_it() {
    // WIRE CONTRACT: the card gains `timezone: {zone, source, confidence,
    // utc_offset_minutes}`, ABSENT (like the card's other optional parts) when
    // nothing is known. PUT /client/person/timezone {addr, zone} sets the
    // user's override, `zone: null` clears it, and both answer
    // {"timezone": ...|null}: what the card now shows. Both are part of the
    // `calendar` hidden feature (the flag-off half is the next test).
    let Harness { app, .. } = common::calendar_harness(|store, acct| {
        for (i, gmail) in ["g1", "g2", "g3"].into_iter().enumerate() {
            ingest_dated(
                store,
                acct,
                gmail,
                "Priya <priya@example.in>",
                i as i64 + 1,
                "+0530",
            );
        }
    });

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/person?addr=priya%40example.in"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let card = body_json(resp).await;
    assert_eq!(
        card["timezone"],
        serde_json::json!({
            "zone": "Asia/Kolkata",
            "source": "offsets",
            "confidence": "high",
            "utc_offset_minutes": 330
        }),
        "{card}"
    );
    // Somebody nothing is known about: absent, not null.
    let resp = app
        .clone()
        .oneshot(authed(
            "GET",
            "/client/person?addr=nobody%40nowhere.example",
        ))
        .await
        .unwrap();
    let card = body_json(resp).await;
    assert!(
        !card.as_object().unwrap().contains_key("timezone"),
        "{card}"
    );

    // The override outranks the inference.
    let resp = app
        .clone()
        .oneshot(authed_json(
            "PUT",
            "/client/person/timezone",
            serde_json::json!({"addr": "Priya@Example.in", "zone": "America/Chicago"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let put = body_json(resp).await;
    assert_eq!(put["timezone"]["zone"], "America/Chicago");
    assert_eq!(put["timezone"]["source"], "user");
    assert_eq!(put["timezone"]["confidence"], "high");
    let chicago = put["timezone"]["utc_offset_minutes"].as_i64().unwrap();
    assert!(chicago == -300 || chicago == -360, "{put}");
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/person?addr=priya%40example.in"))
        .await
        .unwrap();
    assert_eq!(body_json(resp).await["timezone"]["source"], "user");

    // Clearing hands the card back to inference.
    let resp = app
        .clone()
        .oneshot(authed_json(
            "PUT",
            "/client/person/timezone",
            serde_json::json!({"addr": "priya@example.in", "zone": null}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let cleared = body_json(resp).await;
    assert_eq!(cleared["timezone"]["zone"], "Asia/Kolkata", "{cleared}");
    assert_eq!(cleared["timezone"]["source"], "offsets");
    // Clearing for somebody with nothing inferred answers null.
    let resp = app
        .clone()
        .oneshot(authed_json(
            "PUT",
            "/client/person/timezone",
            serde_json::json!({"addr": "ann@example.com", "zone": null}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await, serde_json::json!({"timezone": null}));

    // An unknown zone, a non-address, and the account's own address are 400s;
    // a body without the zone key is not read as "clear".
    for body in [
        serde_json::json!({"addr": "priya@example.in", "zone": "Eastern Standard Time"}),
        serde_json::json!({"addr": "priya@example.in", "zone": "PDT"}),
        serde_json::json!({"addr": "priya@example.in", "zone": ""}),
        serde_json::json!({"addr": "priya", "zone": "Asia/Tokyo"}),
        serde_json::json!({"addr": "Me@Example.com", "zone": "Asia/Tokyo"}),
    ] {
        let resp = app
            .clone()
            .oneshot(authed_json("PUT", "/client/person/timezone", body.clone()))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{body}");
    }
    let resp = app
        .clone()
        .oneshot(authed_json(
            "PUT",
            "/client/person/timezone",
            serde_json::json!({"addr": "priya@example.in"}),
        ))
        .await
        .unwrap();
    assert!(resp.status().is_client_error(), "{}", resp.status());
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/person?addr=priya%40example.in"))
        .await
        .unwrap();
    assert_eq!(
        body_json(resp).await["timezone"]["source"],
        "offsets",
        "no rejected PUT changed anything"
    );

    // No token, no write.
    let resp = app
        .oneshot(common::json_request(
            "PUT",
            "/client/person/timezone",
            &serde_json::json!({"addr": "priya@example.in", "zone": "Asia/Tokyo"}),
            false,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// THE TIMEZONE IS BEHIND THE `calendar` FLAG. Off, the card carries no
/// `timezone` key and the override route is 404 (as on main: no such route),
/// while the inference behind them keeps running at ingest, so turning the
/// flag on shows a zone at once.
#[tokio::test]
async fn the_person_timezone_is_served_and_set_only_with_the_calendar_feature() {
    let seed = |store: &SqliteStore, acct| {
        for (i, gmail) in ["g1", "g2", "g3"].into_iter().enumerate() {
            ingest_dated(
                store,
                acct,
                gmail,
                "Priya <priya@example.in>",
                i as i64 + 1,
                "+0530",
            );
        }
    };
    let (state, store, acct) = common::state_with(seed);
    let features = squelch_core::features::Features::none();
    let app = squelch_api::router(state.with_features(features.clone()));
    let card = |app: axum::Router| async move {
        let resp = app
            .oneshot(authed("GET", "/client/person?addr=priya%40example.in"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        body_json(resp).await
    };
    let off = card(app.clone()).await;
    assert!(
        !off.as_object().unwrap().contains_key("timezone"),
        "flag off: no timezone key: {off}"
    );
    let put = || {
        authed_json(
            "PUT",
            "/client/person/timezone",
            serde_json::json!({"addr": "priya@example.in", "zone": "America/Chicago"}),
        )
    };
    let resp = app.clone().oneshot(put()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "flag off: no route");
    // Inference ran regardless, and nothing was overridden.
    let zone = store
        .person_record(acct, "priya@example.in")
        .unwrap()
        .timezone;
    assert_eq!(
        zone.as_ref().map(|z| (z.zone.as_str(), z.source)),
        Some(("Asia/Kolkata", squelch_core::timezone::ZoneSource::Offsets)),
        "{zone:?}"
    );
    // On, the same daemon serves and sets it.
    features.replace_granted([squelch_core::features::CALENDAR]);
    assert_eq!(card(app.clone()).await["timezone"]["zone"], "Asia/Kolkata");
    let resp = app.clone().oneshot(put()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(card(app).await["timezone"]["source"], "user");
}

// --- search: diagnostics, legs and the as-you-type prefix -------------------
//
// The additions of docs/SEARCH.md §4.8. They are what lets a client tell "no
// mail says this" from "no mail says all of this at once", so they are asserted
// on the wire rather than in the store alone.

/// Seed for the diagnostics tests: one message with every term, one with some,
/// one SEALED carrying a word that appears nowhere else, and one SPAM likewise.
/// The last two are the security half — a term frequency is an oracle over
/// message text, so neither may contribute to any count.
fn seed_diagnostics_corpus(store: &SqliteStore, acct: i64) {
    let triage = |id: i64, sensitivity, kind| {
        store
            .set_triage(id, acct, 50, Tier::Signal, sensitivity, kind, "", "", None)
            .unwrap();
    };

    let all = store
        .upsert_message(&msg(
            acct,
            "g-all",
            "t-all",
            "vendor contract",
            "the signed vendor contract is attached",
        ))
        .unwrap();
    triage(all, Sensitivity::Normal, None);

    let some = store
        .upsert_message(&msg(
            acct,
            "g-some",
            "t-some",
            "contract",
            "a contract, with nobody named",
        ))
        .unwrap();
    triage(some, Sensitivity::Normal, None);

    let sealed = store
        .upsert_message(&msg(
            acct,
            "g-sealed",
            "t-sealed",
            "verification code",
            "your pangolin passcode is 123456",
        ))
        .unwrap();
    triage(sealed, Sensitivity::Sealed, Some(SealedKind::Otp));

    let mut spam = msg(
        acct,
        "g-spam",
        "t-spam",
        "you have won",
        "claim your aardvark prize now",
    );
    spam.is_spam = true;
    let spam = store.upsert_message(&spam).unwrap();
    triage(spam, Sensitivity::Normal, None);
}

#[tokio::test]
async fn search_reports_diagnostics_and_legs() {
    let Harness { app, .. } = harness(seed_diagnostics_corpus);

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/search?q=vendor%20contract"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;

    let d = &json["diagnostics"];
    assert_eq!(d["strict_hits"], 1, "one message carries both words");
    assert_eq!(d["any_hits"], 2, "two carry at least one");
    let terms = d["terms"].as_array().unwrap();
    assert_eq!(terms.len(), 2, "one entry per term, in the order typed");
    assert_eq!(terms[0]["text"], "vendor");
    assert_eq!(terms[0]["df"], 1);
    assert_eq!(terms[1]["text"], "contract");
    assert_eq!(terms[1]["df"], 2);

    // Every item says which leg produced it. Without an embedder the door runs
    // keyword, so that is the only leg there is.
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    for item in items {
        assert_eq!(item["legs"], serde_json::json!(["keyword"]));
        // The existing fields are still flat beside it: this is additive.
        assert!(item["thread_id"].is_string());
        assert!(item["snippet"].is_string());
    }

    // A query that looked NOTHING up still reports the object, zeroed. A field
    // that only sometimes exists is two code paths in every client.
    let resp = app
        .oneshot(authed("GET", "/client/search?q=from:alice"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["diagnostics"]["strict_hits"], 0);
    assert_eq!(json["diagnostics"]["any_hits"], 0);
    assert_eq!(json["diagnostics"]["terms"], serde_json::json!([]));
}

/// SECURITY: term frequencies must never see sealed or spam mail. A df is a
/// yes/no oracle over message text — "does any sealed message contain this
/// word" answered one word at a time is a read of sealed mail without a single
/// row ever being returned.
#[tokio::test]
async fn human_search_diagnostics_include_restricted_mail_and_exclude_spam() {
    let Harness { app, .. } = harness(seed_diagnostics_corpus);

    // "pangolin" appears ONLY in the sealed message; "aardvark" only in spam.
    for word in ["pangolin", "aardvark"] {
        let resp = app
            .clone()
            .oneshot(authed("GET", &format!("/client/search?q={word}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let d = &json["diagnostics"];
        let expected = if word == "pangolin" { 1 } else { 0 };
        assert_eq!(
            d["strict_hits"], expected,
            "{word}: matches human inventory"
        );
        assert_eq!(d["any_hits"], expected, "{word}");
        assert_eq!(d["terms"][0]["text"], word);
        assert_eq!(
            d["terms"][0]["df"], expected,
            "{word}: the count must not read hidden mail"
        );
        assert_eq!(json["items"].as_array().unwrap().len(), expected as usize);
    }
}

/// The diagnostics follow the leg that ran: keyword search excludes the
/// reader's own sent mail, so its counts do too. Counts taken under the other
/// rule would read as a missing page rather than a different question.
#[tokio::test]
async fn keyword_diagnostics_exclude_sent_mail_exactly_as_the_leg_does() {
    let Harness { app, .. } = harness(|store, acct| {
        let n = store
            .upsert_message(&sent_msg(
                acct,
                "g-sent",
                "t-sent",
                "quarterly",
                "bob@x.test",
            ))
            .unwrap();
        store
            .set_triage(
                n,
                acct,
                50,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
    });

    let resp = app
        .oneshot(authed("GET", "/client/search?q=quarterly"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["match_kind"], "keyword");
    assert!(
        json["items"].as_array().unwrap().is_empty(),
        "sent excluded"
    );
    assert_eq!(
        json["diagnostics"]["any_hits"], 0,
        "the count agrees with the list beside it"
    );
    assert_eq!(json["diagnostics"]["terms"][0]["df"], 0);
}

/// `partial` is the panel's as-you-type flag: the LAST word matches as a
/// prefix, and only then. An unreadable value is refused rather than guessed.
#[tokio::test]
async fn search_partial_prefixes_the_trailing_word_and_validates_its_value() {
    let Harness { app, .. } = harness(|store, acct| {
        let n = store
            .upsert_message(&msg(
                acct,
                "g1",
                "t1",
                "venue details",
                "wifi and the FAQ are on the same page",
            ))
            .unwrap();
        store
            .set_triage(
                n,
                acct,
                50,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
    });

    // Settled: "wif" is not a word anybody wrote.
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/search?q=wif"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert!(json["items"].as_array().unwrap().is_empty());
    assert_eq!(json["diagnostics"]["any_hits"], 0);

    // Still typing: the prefix finds it, and the diagnostics count the search
    // that actually ran rather than a bare-word one nobody asked for.
    for value in ["1", "true"] {
        let resp = app
            .clone()
            .oneshot(authed(
                "GET",
                &format!("/client/search?q=wif&partial={value}"),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(
            json["items"].as_array().unwrap().len(),
            1,
            "partial={value} must prefix-match"
        );
        assert_eq!(json["diagnostics"]["any_hits"], 1);
        assert_eq!(json["diagnostics"]["terms"][0]["text"], "wif");
        assert_eq!(json["diagnostics"]["terms"][0]["df"], 1);
    }

    // Off explicitly, and a value nobody serves.
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/search?q=wif&partial=0"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert!(json["items"].as_array().unwrap().is_empty());

    let resp = app
        .oneshot(authed("GET", "/client/search?q=wif&partial=yes"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// A word the right mail simply does not contain used to mean zero hits. The
/// door now serves the strict matches first and the partial ones below them,
/// and says so in the diagnostics.
#[tokio::test]
async fn search_serves_partial_matches_below_the_strict_ones() {
    let Harness { app, .. } = harness(seed_diagnostics_corpus);

    let resp = app
        .oneshot(authed(
            "GET",
            "/client/search?q=vendor%20contract%20invoice",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(
        json["diagnostics"]["strict_hits"], 0,
        "no message has all three words"
    );
    assert_eq!(json["diagnostics"]["any_hits"], 2);
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "and yet the mail is findable");
    assert_eq!(items[0]["thread_id"], "t-all", "two terms beats one");
}

/// In hybrid mode `legs` reports which recall list a hit came from, and a hit
/// both legs found says both.
#[tokio::test]
async fn hybrid_items_report_the_legs_that_found_them() {
    use squelch_core::embed::StubEmbedder;

    let store = SqliteStore::open_in_memory()
        .unwrap()
        .with_embedder(Arc::new(StubEmbedder::new(384)))
        .unwrap();
    let acct = store.ensure_account("me@example.com").unwrap();
    let store = Arc::new(store);

    let n = store
        .upsert_message(&msg(
            acct,
            "g1",
            "t1",
            "acme invoice for services",
            "please pay the acme invoice",
        ))
        .unwrap();
    store
        .set_triage(
            n,
            acct,
            60,
            Tier::Signal,
            Sensitivity::Normal,
            None,
            "",
            "",
            None,
        )
        .unwrap();
    let v = store
        .embedder()
        .unwrap()
        .embed("acme invoice for services please pay the acme invoice")
        .unwrap();
    store.upsert_message_vector(acct, n, &v).unwrap();

    let app = router(ApiState::new(store.clone(), acct, TOKEN));

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/search?q=acme%20invoice"))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["match_kind"], "hybrid");
    let legs = json["items"][0]["legs"].as_array().unwrap().clone();
    assert!(
        legs.contains(&Value::from("keyword")) && legs.contains(&Value::from("vector")),
        "both legs found it: {legs:?}"
    );

    // Semantic mode is vector by construction.
    let resp = app
        .oneshot(authed(
            "GET",
            "/client/search?q=acme%20invoice&mode=semantic",
        ))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["match_kind"], "semantic");
    assert_eq!(json["items"][0]["legs"], serde_json::json!(["vector"]));
    // Recall legs INCLUDE sent mail, so their diagnostics do too — the counts
    // follow whichever leg ran.
    assert_eq!(json["diagnostics"]["terms"][0]["text"], "acme");
}

/// PUNCTUATION IS NOT AN INVITATION TO LIST THE MAILBOX, and the guard has to
/// hold in every mode.
///
/// `q=***` passes the door's emptiness checks (non-empty string, non-empty
/// term, no operators) and the builder then makes no expression of it. The
/// keyword leg has always read that as an empty result. The recall legs never
/// consulted the builder: they embedded "***" and served a page of essentially
/// arbitrary mail, while the diagnostics beside it reported that nothing had
/// been looked up. A response cannot say both.
#[tokio::test]
async fn a_query_of_pure_punctuation_lists_nothing_in_any_mode() {
    use squelch_core::embed::StubEmbedder;

    let store = SqliteStore::open_in_memory()
        .unwrap()
        .with_embedder(Arc::new(StubEmbedder::new(384)))
        .unwrap();
    let acct = store.ensure_account("me@example.com").unwrap();
    let store = Arc::new(store);

    for (i, subject) in ["lunch on friday", "the quarterly numbers"]
        .iter()
        .enumerate()
    {
        let n = store
            .upsert_message(&msg(
                acct,
                &format!("g{i}"),
                &format!("t{i}"),
                subject,
                "a body with words in it",
            ))
            .unwrap();
        store
            .set_triage(
                n,
                acct,
                50,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
        let v = store
            .embedder()
            .unwrap()
            .embed(&format!("{subject} a body with words in it"))
            .unwrap();
        store.upsert_message_vector(acct, n, &v).unwrap();
    }

    let app = router(ApiState::new(store.clone(), acct, TOKEN));

    // Default (hybrid, since an embedder is attached) and each mode by name.
    for query in [
        "q=***",
        "q=***&mode=keyword",
        "q=***&mode=semantic",
        "q=***&mode=hybrid",
        "q=%3A%3A%3A&mode=hybrid",
        "q=***&mode=hybrid&partial=1",
    ] {
        let resp = app
            .clone()
            .oneshot(authed("GET", &format!("/client/search?{query}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{query}");
        let json = body_json(resp).await;
        assert!(
            json["items"].as_array().unwrap().is_empty(),
            "{query} listed mail: {}",
            json["items"]
        );
        assert!(json["next_cursor"].is_null(), "{query} offered a next page");
        assert_eq!(json["diagnostics"]["any_hits"], 0, "{query}");
        assert_eq!(
            json["diagnostics"]["terms"],
            serde_json::json!([]),
            "{query}"
        );
    }

    // An operator beside it is a real constraint and still selects.
    let resp = app
        .oneshot(authed(
            "GET",
            "/client/search?q=***%20from:alice%40example.com",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert!(
        !json["items"].as_array().unwrap().is_empty(),
        "the from: half still lists that sender's mail"
    );
}

#[tokio::test]
async fn search_unfinished_first_is_global_across_keyword_pages() {
    use squelch_core::types::AttentionStatus;
    let Harness { app, .. } = harness(|store, acct| {
        for (id, subject, done) in [
            ("done-strict", "anjuna tickets", true),
            ("open-partial", "tickets", false),
            ("open-strict", "anjuna tickets", false),
            ("done-partial", "tickets", true),
        ] {
            let id = store
                .upsert_message(&msg(acct, id, id, subject, "Save your tickets below"))
                .unwrap();
            store
                .set_triage(
                    id,
                    acct,
                    50,
                    Tier::Signal,
                    Sensitivity::Normal,
                    None,
                    "",
                    "",
                    None,
                )
                .unwrap();
            if done {
                store
                    .set_attention_status(acct, id, AttentionStatus::Done)
                    .unwrap();
            }
        }
    });
    for sort in ["recent", "best_match"] {
        let mut cursor = String::new();
        let mut threads = Vec::new();
        loop {
            let uri = format!(
                "/client/search?q=anjuna%20tickets&mode=keyword&unfinished_first=true&sort={sort}&limit=1{cursor}"
            );
            let response = app.clone().oneshot(authed("GET", &uri)).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let json = body_json(response).await;
            for hit in json["items"].as_array().unwrap() {
                let thread = hit["thread_id"].as_str().unwrap();
                assert_eq!(hit["is_done"], thread.starts_with("done"));
                assert_eq!(hit["snippet_matches"], serde_json::json!(["tickets"]));
                threads.push(thread.to_string());
            }
            match json["next_cursor"].as_str() {
                Some(c) => cursor = format!("&cursor={c}"),
                None => break,
            }
            assert!(threads.len() <= 4);
        }
        assert_eq!(
            threads,
            ["open-strict", "open-partial", "done-strict", "done-partial"]
        );
    }
}

#[tokio::test]
async fn search_fast_path_keeps_sent_mail_and_matching_diagnostics() {
    let Harness { app, .. } = harness(|store, acct| {
        let mut sent = msg(
            acct,
            "sent-ticket",
            "sent-ticket",
            "tickets",
            "Here are the tickets",
        );
        sent.is_sent = true;
        store.upsert_message(&sent).unwrap();
    });
    for q in ["tickets", "from:alice"] {
        let response = app
            .clone()
            .oneshot(authed(
                "GET",
                &format!("/client/search?q={q}&mode=keyword&unfinished_first=true"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["items"][0]["thread_id"], "sent-ticket");
        assert_eq!(json["items"][0]["is_done"], false);
        if q == "tickets" {
            assert_eq!(json["diagnostics"]["strict_hits"], 1);
        }
    }
    let response = app
        .oneshot(authed("GET", "/client/search?q=tickets&mode=keyword"))
        .await
        .unwrap();
    assert!(
        body_json(response).await["items"]
            .as_array()
            .unwrap()
            .is_empty(),
        "other keyword callers retain their existing inbound-only scope"
    );
}

/// Plain ranked order: `include_sent` without `unfinished_first`. The owner's
/// sent mail stays in the keyword corpus, done mail is NOT pushed behind
/// unfinished mail, and offset paging walks every hit exactly once.
#[tokio::test]
async fn search_plain_order_keeps_sent_mail_without_status_groups() {
    use squelch_core::types::AttentionStatus;
    let Harness { app, .. } = harness(|store, acct| {
        for (id, subject, done) in [
            ("done-strict", "anjuna tickets", true),
            ("open-partial", "tickets", false),
            ("open-strict", "anjuna tickets", false),
            ("done-partial", "tickets", true),
        ] {
            let id = store
                .upsert_message(&msg(acct, id, id, subject, "Save your tickets below"))
                .unwrap();
            store
                .set_triage(
                    id,
                    acct,
                    50,
                    Tier::Signal,
                    Sensitivity::Normal,
                    None,
                    "",
                    "",
                    None,
                )
                .unwrap();
            if done {
                store
                    .set_attention_status(acct, id, AttentionStatus::Done)
                    .unwrap();
            }
        }
        let mut sent = msg(
            acct,
            "sent-partial",
            "sent-partial",
            "tickets",
            "Here are the tickets",
        );
        sent.is_sent = true;
        store.upsert_message(&sent).unwrap();
    });
    let mut cursor = String::new();
    let mut threads = Vec::new();
    loop {
        let uri = format!(
            "/client/search?q=anjuna%20tickets&mode=keyword&include_sent=true&sort=best_match&limit=1{cursor}"
        );
        let response = app.clone().oneshot(authed("GET", &uri)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        for hit in json["items"].as_array().unwrap() {
            threads.push(hit["thread_id"].as_str().unwrap().to_string());
        }
        match json["next_cursor"].as_str() {
            Some(c) => {
                assert!(!c.starts_with("recall:"), "keyword pages by offset");
                cursor = format!("&cursor={c}");
            }
            None => break,
        }
        assert!(threads.len() <= 5);
    }
    assert_eq!(threads.len(), 5, "every hit served once: {threads:?}");
    // Best match puts both strict hits first, done or not: no status groups.
    let mut strict = threads[..2].to_vec();
    strict.sort();
    assert_eq!(strict, ["done-strict", "open-strict"], "{threads:?}");
    assert!(
        threads.contains(&"sent-partial".to_string()),
        "the owner's sent mail stays in the plain-order corpus: {threads:?}"
    );

    // A filter-only listing in plain order keeps sent mail too.
    let response = app
        .clone()
        .oneshot(authed(
            "GET",
            "/client/search?q=from:alice&mode=keyword&include_sent=true",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert!(
        json["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["thread_id"] == "sent-partial"),
        "{json}"
    );
}

// ---- compose attachments ----------------------------------------------------

/// An authed upload: the bytes as the body, the metadata in the query and the
/// `Content-Type` header — the shape the composer sends.
fn upload(filename: &str, mime: Option<&str>, cid: Option<&str>, bytes: &[u8]) -> Request<Body> {
    let mut uri = format!("/client/compose/attachments?filename={filename}");
    if let Some(cid) = cid {
        uri.push_str(&format!("&content_id={cid}"));
    }
    let mut b = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    if let Some(mime) = mime {
        b = b.header(header::CONTENT_TYPE, mime);
    }
    b.body(Body::from(bytes.to_vec())).unwrap()
}

async fn stage(app: &axum::Router, filename: &str, mime: &str, cid: &str, bytes: &[u8]) -> Value {
    let resp = app
        .clone()
        .oneshot(upload(filename, Some(mime), Some(cid), bytes))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    body_json(resp).await
}

#[tokio::test]
async fn staging_a_file_answers_its_metadata_and_serves_it_back() {
    let Harness { app, .. } = harness(|_, _| {});
    let meta = stage(
        &app,
        "shot.png",
        "image/png; charset=x",
        "shot-1@passband",
        b"PNGBYTES",
    )
    .await;
    let id = meta["id"].as_i64().expect("an id");
    assert_eq!(meta["filename"], "shot.png");
    assert_eq!(meta["mime"], "image/png", "parameters are dropped");
    assert_eq!(meta["size"], 8);
    assert_eq!(meta["content_id"], "shot-1@passband");

    // The bytes come back under the byte door's discipline.
    let resp = app
        .clone()
        .oneshot(authed("GET", &format!("/client/compose/attachments/{id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[header::CONTENT_TYPE], "image/png");
    assert_eq!(resp.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL],
        "no-store",
        "an unsent composition's bytes are not something to leave in a cache"
    );
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], b"PNGBYTES");

    // Removing it from the tray: gone, and a second delete is a 404.
    let resp = app
        .clone()
        .oneshot(authed(
            "DELETE",
            &format!("/client/compose/attachments/{id}"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(authed(
            "DELETE",
            &format!("/client/compose/attachments/{id}"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let resp = app
        .oneshot(authed("GET", &format!("/client/compose/attachments/{id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn staging_polices_what_it_stores() {
    let Harness { app, .. } = harness(|_, _| {});

    // No content type: a blob. A multipart claim: a blob. A bad token: 400.
    let meta = body_json(
        app.clone()
            .oneshot(upload("x", None, None, b"1"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(meta["mime"], "application/octet-stream");
    assert!(
        meta["content_id"].as_str().unwrap().ends_with("@passband"),
        "minted when the client sent none: {}",
        meta["content_id"]
    );
    let meta = body_json(
        app.clone()
            .oneshot(upload("x", Some("multipart/mixed; boundary=z"), None, b"1"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(meta["mime"], "application/octet-stream");
    let resp = app
        .clone()
        .oneshot(upload("x", Some("text/plain"), Some("a%20b"), b"1"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // A filename is a name, never a path.
    let meta = stage(&app, "..%2F..%2Fevil.txt", "text/plain", "t1", b"1").await;
    assert_eq!(meta["filename"], "....evil.txt");

    // A text file whose bytes are not UTF-8 is a blob: the MIME writer would
    // otherwise declare a charset the bytes do not have.
    let latin1 = stage(&app, "caf.txt", "text/plain", "t2", b"caf\xe9").await;
    assert_eq!(latin1["mime"], "application/octet-stream");
    let utf8 = stage(&app, "cafe.txt", "text/plain", "t3", "café".as_bytes()).await;
    assert_eq!(utf8["mime"], "text/plain");

    // Empty is refused; the bearer is required.
    let resp = app
        .clone()
        .oneshot(upload("x", Some("text/plain"), None, b""))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/client/compose/attachments?filename=x")
                .body(Body::from("1"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_draft_claims_exactly_the_files_it_names() {
    let Harness { app, .. } = harness(|_, _| {});
    let a = stage(&app, "a.pdf", "application/pdf", "a1", b"A").await["id"]
        .as_i64()
        .unwrap();
    let b = stage(&app, "b.pdf", "application/pdf", "b1", b"B").await["id"]
        .as_i64()
        .unwrap();

    let draft = put_draft(
        &app,
        serde_json::json!({ "to": "alice@example.com", "body": "files", "attachment_ids": [a, b] }),
    )
    .await;
    let names: Vec<&str> = draft["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["filename"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["a.pdf", "b.pdf"],
        "the PUT's answer already lists them"
    );

    // The listing restores them, metadata only.
    let drafts = list_drafts(&app).await;
    let d = &drafts.as_array().unwrap()[0];
    assert_eq!(d["attachments"].as_array().unwrap().len(), 2);
    assert!(d["attachments"][0]["data"].is_null());
    assert_eq!(d["attachments"][0]["content_id"], "a1");

    // Removing one from the tray: the next save names only the other.
    let draft = put_draft(
        &app,
        serde_json::json!({ "to": "alice@example.com", "body": "files", "attachment_ids": [b] }),
    )
    .await;
    assert_eq!(draft["attachments"].as_array().unwrap().len(), 1);
    assert_eq!(draft["attachments"][0]["id"], b);

    // A save that says nothing about files leaves the claims alone.
    let draft = put_draft(
        &app,
        serde_json::json!({ "to": "alice@example.com", "body": "files, edited" }),
    )
    .await;
    assert_eq!(draft["attachments"].as_array().unwrap().len(), 1);

    // `[]` releases every file, and the released file is still staged.
    let draft = put_draft(
        &app,
        serde_json::json!({ "to": "alice@example.com", "body": "files", "attachment_ids": [] }),
    )
    .await;
    assert_eq!(draft["attachments"].as_array().unwrap().len(), 0);
    let resp = app
        .clone()
        .oneshot(authed("GET", &format!("/client/compose/attachments/{b}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "released, not deleted");
    // Claimed again for the rest of the test.
    let draft = put_draft(
        &app,
        serde_json::json!({ "to": "alice@example.com", "body": "files", "attachment_ids": [b] }),
    )
    .await;
    assert_eq!(draft["attachments"].as_array().unwrap().len(), 1);

    // Deleting the draft takes its file with it; the released one survives
    // (until the sweep).
    let draft_id = draft["id"].as_i64().unwrap();
    let resp = app
        .clone()
        .oneshot(authed("DELETE", &format!("/client/drafts/{draft_id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(authed("GET", &format!("/client/compose/attachments/{b}")))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "the draft's file went with it"
    );
    let resp = app
        .oneshot(authed("GET", &format!("/client/compose/attachments/{a}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "released, not deleted");
}

#[tokio::test]
async fn a_send_carries_its_files_inline_where_the_body_points_and_consumes_them() {
    let (base, handle) = mock_gmail(1).await;
    let Harness { app, store, acct } = app_with_writes(base, |_, _| {});
    let shot = stage(&app, "shot.png", "image/png", "shot-1@passband", b"PNG").await["id"]
        .as_i64()
        .unwrap();
    let deck = stage(
        &app,
        "deck.pdf",
        "application/pdf",
        "deck-1@passband",
        b"PDF",
    )
    .await["id"]
        .as_i64()
        .unwrap();
    let draft = put_draft(
        &app,
        serde_json::json!({ "to": "alice@example.com", "body": "x", "attachment_ids": [shot, deck] }),
    )
    .await;

    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "subject": "Hi",
                "body": "the shot ![shot](cid:shot-1@passband) and the deck",
                "body_format": "markdown",
                "confirm": true,
                "draft_id": draft["id"],
                "attachment_ids": [shot, deck]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let reqs = handle.await.unwrap();
    let mime = sent_mime(&reqs[0]);
    // mixed[ related[ alternative, png inline ], pdf attachment ]
    assert!(mime.contains("Content-Type: multipart/mixed;"));
    assert!(mime.contains("Content-Type: multipart/related;"));
    assert!(mime.contains("Content-Type: multipart/alternative;"));
    assert!(mime.contains("Content-Disposition: inline; filename=\"shot.png\""));
    assert!(mime.contains("Content-ID: <shot-1@passband>"));
    assert!(mime.contains("<img src=\"cid:shot-1@passband\" alt=\"shot\""));
    assert!(mime.contains("Content-Disposition: attachment; filename=\"deck.pdf\""));
    use base64::Engine as _;
    assert!(mime.contains(&base64::engine::general_purpose::STANDARD.encode(b"PDF")));

    // Consumed: the draft, and both files.
    assert_eq!(list_drafts(&app).await.as_array().map(Vec::len), Some(0));
    assert!(store.outbound_attachment(acct, shot).unwrap().is_none());
    assert!(store.outbound_attachment(acct, deck).unwrap().is_none());
}

#[tokio::test]
async fn a_send_naming_a_file_that_is_gone_is_refused_before_gmail() {
    let (base, handle) = mock_gmail(0).await;
    let Harness { app, store, acct } = app_with_writes(base, |_, _| {});
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "body": "see attached",
                "confirm": true,
                "attachment_ids": [4242]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let err = body_json(resp).await["error"].as_str().unwrap().to_string();
    assert!(err.contains("no longer staged"), "{err}");
    assert_eq!(handle.await.unwrap().len(), 0, "nothing reached Gmail");
    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send"
                && a.detail.as_deref() == Some("rejected:attachment_missing"))
    );
}

/// A send may carry only files that are unclaimed or claimed by the draft it
/// consumes. Another draft's file is "no longer staged" as far as this send
/// is concerned: refused before Gmail, and the file stays with its draft
/// (a send that carried it would have consumed it on success).
#[tokio::test]
async fn a_send_naming_another_drafts_file_is_refused_and_the_file_survives() {
    let (base, handle) = mock_gmail(0).await;
    let Harness { app, store, acct } = app_with_writes(base, |_, _| {});
    let theirs = stage(&app, "theirs.pdf", "application/pdf", "t1", b"T").await["id"]
        .as_i64()
        .unwrap();
    let (status, holder) = draft_call(
        &app,
        "POST",
        "/client/drafts",
        serde_json::json!({ "body": "holder", "attachment_ids": [theirs] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(holder["attachments"].as_array().map(Vec::len), Some(1));
    let (status, other) = draft_call(
        &app,
        "POST",
        "/client/drafts",
        serde_json::json!({ "body": "other" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // From another draft, and from no draft at all: both refused.
    for draft_id in [other["id"].clone(), Value::Null] {
        let resp = app
            .clone()
            .oneshot(authed_json(
                "POST",
                "/client/actions/send",
                serde_json::json!({
                    "to": "alice@example.com",
                    "body": "see attached",
                    "confirm": true,
                    "draft_id": draft_id,
                    "attachment_ids": [theirs]
                }),
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "draft_id {draft_id}"
        );
        let err = body_json(resp).await["error"].as_str().unwrap().to_string();
        assert!(err.contains("no longer staged"), "{err}");
    }
    assert_eq!(handle.await.unwrap().len(), 0, "nothing reached Gmail");
    let audit = store.list_audit(acct, 10).unwrap();
    assert_eq!(
        audit
            .iter()
            .filter(|a| a.action == "send"
                && a.detail.as_deref() == Some("rejected:attachment_missing"))
            .count(),
        2
    );
    assert!(
        store.outbound_attachment(acct, theirs).unwrap().is_some(),
        "the file survives"
    );
    let holder_now = store
        .draft_by_id(acct, holder["id"].as_i64().unwrap())
        .unwrap()
        .expect("the holder draft survives");
    assert_eq!(
        holder_now.attachments.len(),
        1,
        "still attached to its draft"
    );
    assert_eq!(list_drafts(&app).await.as_array().map(Vec::len), Some(2));
}

#[tokio::test]
async fn a_send_carries_its_own_drafts_file_and_consumes_it() {
    let (base, handle) = mock_gmail(1).await;
    let Harness { app, store, acct } = app_with_writes(base, |_, _| {});
    let mine = stage(&app, "mine.pdf", "application/pdf", "m1", b"MINE").await["id"]
        .as_i64()
        .unwrap();
    let (_, draft) = draft_call(
        &app,
        "POST",
        "/client/drafts",
        serde_json::json!({ "body": "mine", "attachment_ids": [mine] }),
    )
    .await;
    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "body": "see attached",
                "confirm": true,
                "draft_id": draft["id"],
                "attachment_ids": [mine]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let reqs = handle.await.unwrap();
    assert!(sent_mime(&reqs[0]).contains("filename=\"mine.pdf\""));
    assert!(store.outbound_attachment(acct, mine).unwrap().is_none());
    assert_eq!(list_drafts(&app).await.as_array().map(Vec::len), Some(0));
}

#[tokio::test]
async fn a_draftless_send_carries_an_unclaimed_file_and_consumes_it() {
    let (base, handle) = mock_gmail(1).await;
    let Harness { app, store, acct } = app_with_writes(base, |_, _| {});
    let loose = stage(&app, "loose.pdf", "application/pdf", "l1", b"LOOSE").await["id"]
        .as_i64()
        .unwrap();
    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "body": "see attached",
                "confirm": true,
                "attachment_ids": [loose]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let reqs = handle.await.unwrap();
    assert!(sent_mime(&reqs[0]).contains("filename=\"loose.pdf\""));
    assert!(store.outbound_attachment(acct, loose).unwrap().is_none());
}

#[tokio::test]
async fn the_guard_reads_a_staged_text_file() {
    let (base, handle) = mock_gmail(0).await;
    let Harness { app, .. } = app_with_writes(base, |_, _| {});
    let key = "-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----\n";
    let notes = stage(&app, "notes.txt", "text/plain", "n1", key.as_bytes()).await["id"]
        .as_i64()
        .unwrap();
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "body": "the notes you asked for",
                "confirm": true,
                "attachment_ids": [notes]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "a key inside an attached text file is the body's secret by another door"
    );
    assert_eq!(handle.await.unwrap().len(), 0);
}

#[tokio::test]
async fn stats_advertise_compose_attachments() {
    let Harness { app, .. } = harness(|_, _| {});
    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["compose_attachments"], true);
}

#[tokio::test]
async fn a_failed_send_keeps_its_staged_files_and_its_draft() {
    // Gmail refuses the message: the files are still on the tray, so the
    // sender can fix whatever it was and send again without re-attaching.
    let (base, handle) = mock_gmail_seq(vec![(500, "{}".to_string())]).await;
    let Harness { app, store, acct } = app_with_writes(base, |_, _| {});
    let deck = stage(&app, "deck.pdf", "application/pdf", "deck-1", b"PDF").await["id"]
        .as_i64()
        .unwrap();
    let draft = put_draft(
        &app,
        serde_json::json!({ "to": "alice@example.com", "body": "x", "attachment_ids": [deck] }),
    )
    .await;
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "body": "see attached",
                "confirm": true,
                "draft_id": draft["id"],
                "attachment_ids": [deck]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(handle.await.unwrap().len(), 1);
    assert!(
        store.outbound_attachment(acct, deck).unwrap().is_some(),
        "a failed send consumes nothing"
    );
    assert_eq!(
        store.list_drafts(acct).unwrap().len(),
        1,
        "and the draft still holds it"
    );
    assert_eq!(store.list_drafts(acct).unwrap()[0].attachments.len(), 1);
}

#[tokio::test]
async fn a_guard_blocked_send_keeps_its_staged_files() {
    let (base, handle) = mock_gmail(0).await;
    let Harness { app, store, acct } = app_with_writes(base, |_, _| {});
    let deck = stage(&app, "deck.pdf", "application/pdf", "deck-1", b"PDF").await["id"]
        .as_i64()
        .unwrap();
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "body": "-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----",
                "confirm": true,
                "attachment_ids": [deck]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(handle.await.unwrap().len(), 0);
    assert!(
        store.outbound_attachment(acct, deck).unwrap().is_some(),
        "the verdict is the first act; the file waits for the second"
    );
}

#[tokio::test]
async fn duplicate_attachment_ids_send_one_part() {
    let (base, handle) = mock_gmail(1).await;
    let Harness { app, .. } = app_with_writes(base, |_, _| {});
    let deck = stage(&app, "deck.pdf", "application/pdf", "deck-1", b"PDF").await["id"]
        .as_i64()
        .unwrap();
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "body": "see attached",
                "confirm": true,
                "attachment_ids": [deck, deck]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mime = sent_mime(&handle.await.unwrap()[0]);
    assert_eq!(mime.matches("filename=\"deck.pdf\"").count(), 1);
}

#[tokio::test]
async fn an_upload_past_the_router_default_body_limit_is_accepted_and_the_cap_is_not() {
    // The `/client/*` router's default body limit is 2 MB; the upload route
    // carries its own. 3 MB proves the layer is really on the route; 26 MB
    // proves the cap still holds.
    let Harness { app, .. } = harness(|_, _| {});
    let three = vec![0x42u8; 3 * 1024 * 1024];
    let resp = app
        .clone()
        .oneshot(upload(
            "big.bin",
            Some("application/octet-stream"),
            Some("big1"),
            &three,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["size"], 3 * 1024 * 1024);

    let huge = vec![0x42u8; 26 * 1024 * 1024];
    let resp = app
        .oneshot(upload(
            "huge.bin",
            Some("application/octet-stream"),
            Some("huge1"),
            &huge,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn a_fan_out_carries_the_files_to_every_member_and_consumes_them() {
    let (base, handle) =
        mock_gmail_seq(vec![(200, "{}".to_string()), (200, "{}".to_string())]).await;
    let Harness { app, store, acct } = app_with_writes(base, |_, _| {});
    let group = store
        .create_send_group(
            acct,
            "Pair",
            squelch_core::types::GroupMode::Individual,
            "",
            &["ann@fund.com", "bo@fund.com"].map(|addr| {
                squelch_core::store::sqlite::groups::NewGroupMember {
                    addr: addr.into(),
                    display_name: None,
                }
            }),
        )
        .unwrap();
    let shot = stage(&app, "shot.png", "image/png", "shot-1", b"PNG").await["id"]
        .as_i64()
        .unwrap();
    let deck = stage(&app, "deck.pdf", "application/pdf", "deck-1", b"PDF").await["id"]
        .as_i64()
        .unwrap();
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "group_id": group,
                "subject": "Update #3",
                "body": "the chart ![shot](cid:shot-1) and the deck",
                "body_format": "markdown",
                "confirm": true,
                "attachment_ids": [shot, deck]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let reqs = handle.await.unwrap();
    assert_eq!(reqs.len(), 2);
    for req in &reqs {
        let mime = sent_mime(req);
        assert!(
            mime.contains("Content-Disposition: inline; filename=\"shot.png\""),
            "{mime}"
        );
        assert!(mime.contains("Content-ID: <shot-1>"));
        assert!(mime.contains("Content-Disposition: attachment; filename=\"deck.pdf\""));
    }
    assert!(store.outbound_attachment(acct, shot).unwrap().is_none());
    assert!(store.outbound_attachment(acct, deck).unwrap().is_none());
}

#[tokio::test]
async fn attachments_over_the_message_ceiling_are_refused_before_their_bytes_are_read() {
    let (base, handle) = mock_gmail(0).await;
    let Harness { app, store, acct } = app_with_writes(base, |_, _| {});
    let thirteen = vec![0x41u8; 13 * 1024 * 1024];
    let a = stage(&app, "a.bin", "application/octet-stream", "a1", &thirteen).await["id"]
        .as_i64()
        .unwrap();
    let b = stage(&app, "b.bin", "application/octet-stream", "b1", &thirteen).await["id"]
        .as_i64()
        .unwrap();
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "body": "two big ones",
                "confirm": true,
                "attachment_ids": [a, b]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(handle.await.unwrap().len(), 0);
    let audit = store.list_audit(acct, 10).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "send" && a.detail.as_deref() == Some("rejected:too_large"))
    );
    // Refused, not consumed.
    assert!(store.outbound_attachment(acct, a).unwrap().is_some());
}

#[tokio::test]
async fn two_files_sharing_a_content_id_are_refused() {
    let (base, handle) = mock_gmail(0).await;
    let Harness { app, .. } = app_with_writes(base, |_, _| {});
    let a = stage(&app, "a.png", "image/png", "same", b"A").await["id"]
        .as_i64()
        .unwrap();
    let b = stage(&app, "b.png", "image/png", "same", b"B").await["id"]
        .as_i64()
        .unwrap();
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "body": "![a](cid:same)",
                "body_format": "markdown",
                "confirm": true,
                "attachment_ids": [a, b]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_json(resp).await["error"]
            .as_str()
            .unwrap()
            .contains("content id")
    );
    assert_eq!(handle.await.unwrap().len(), 0);
}

#[tokio::test]
async fn the_guard_reads_an_attached_message_too() {
    // An attached .eml is RFC822 text carrying whatever its bodies carried.
    let (base, handle) = mock_gmail(0).await;
    let Harness { app, .. } = app_with_writes(base, |_, _| {});
    let eml = "From: a@x.com\r\nSubject: keys\r\n\r\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----\n";
    let id = stage(&app, "thread.eml", "message/rfc822", "e1", eml.as_bytes()).await["id"]
        .as_i64()
        .unwrap();
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "to": "alice@example.com",
                "body": "see the thread below",
                "confirm": true,
                "attachment_ids": [id]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(handle.await.unwrap().len(), 0);
}

#[tokio::test]
async fn grouped_recall_expands_without_repeating_hits_or_returning_done_early() {
    use squelch_core::embed::StubEmbedder;
    use squelch_core::types::AttentionStatus;
    let store = SqliteStore::open_in_memory()
        .unwrap()
        .with_embedder(Arc::new(StubEmbedder::new(384)))
        .unwrap();
    let acct = store.ensure_account("me@example.com").unwrap();
    for i in 0..27 {
        let key = format!("recall-{i}");
        let id = store
            .upsert_message(&msg(
                acct,
                &key,
                &key,
                "anjuna tickets",
                "save tickets below",
            ))
            .unwrap();
        store
            .set_triage(
                id,
                acct,
                50,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
        if i % 3 == 0 {
            store
                .set_attention_status(acct, id, AttentionStatus::Done)
                .unwrap();
        }
        let vector = store
            .embedder()
            .unwrap()
            .embed("anjuna tickets save tickets below")
            .unwrap();
        store.upsert_message_vector(acct, id, &vector).unwrap();
    }
    let app = router(ApiState::new(Arc::new(store), acct, TOKEN));
    for mode in ["hybrid", "semantic"] {
        let mut cursor = String::new();
        let mut seen = std::collections::HashSet::new();
        let mut saw_done = false;
        for _ in 0..12 {
            let uri = format!(
                "/client/search?q=anjuna%20tickets&mode={mode}&unfinished_first=true&limit=5{cursor}"
            );
            let response = app.clone().oneshot(authed("GET", &uri)).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let page = body_json(response).await;
            if seen.is_empty() {
                use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
                let cursor = page["next_cursor"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("recall:")
                    .unwrap();
                let resume: Value =
                    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(cursor).unwrap()).unwrap();
                assert!(
                    resume["k"].as_u64().unwrap() < 600,
                    "small searches must not pin recall to 600"
                );
            }
            for item in page["items"].as_array().unwrap() {
                assert!(
                    seen.insert(item["id"].as_i64().unwrap()),
                    "repeated hit: {page}"
                );
                let done = item["is_done"].as_bool().unwrap();
                assert!(!saw_done || done, "unfinished hit after done group: {page}");
                saw_done |= done;
            }
            match page["next_cursor"].as_str() {
                Some(next) => cursor = format!("&cursor={next}"),
                None => break,
            }
        }
        assert_eq!(
            seen.len(),
            27,
            "every recalled hit should be served in {mode}"
        );
    }
}

/// Plain-order recall (`include_sent`, no `unfinished_first`) pages by the
/// recall cursor, as grouped recall does: an offset into a pool that grows
/// with the offset repeats and skips hits. Every hit is served once, the
/// owner's sent mail among them, and done mail is NOT held back behind
/// unfinished mail. A recall cursor without either flag is still refused.
#[tokio::test]
async fn plain_order_recall_pages_by_cursor_without_repeats_or_groups() {
    use squelch_core::embed::StubEmbedder;
    use squelch_core::types::AttentionStatus;
    let store = SqliteStore::open_in_memory()
        .unwrap()
        .with_embedder(Arc::new(StubEmbedder::new(384)))
        .unwrap();
    let acct = store.ensure_account("me@example.com").unwrap();
    let mut sent_id = 0;
    for i in 0..27 {
        let key = format!("recall-{i}");
        let mut m = msg(acct, &key, &key, "anjuna tickets", "save tickets below");
        m.is_sent = i == 4;
        let id = store.upsert_message(&m).unwrap();
        if i == 4 {
            sent_id = id;
        }
        store
            .set_triage(
                id,
                acct,
                50,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
        if i % 3 == 0 {
            store
                .set_attention_status(acct, id, AttentionStatus::Done)
                .unwrap();
        }
        let vector = store
            .embedder()
            .unwrap()
            .embed("anjuna tickets save tickets below")
            .unwrap();
        store.upsert_message_vector(acct, id, &vector).unwrap();
    }
    let app = router(ApiState::new(Arc::new(store), acct, TOKEN));
    for mode in ["hybrid", "semantic"] {
        let mut cursor = String::new();
        let mut seen = std::collections::HashSet::new();
        let mut order = Vec::new();
        let mut last_cursor = None;
        for _ in 0..12 {
            let uri = format!(
                "/client/search?q=anjuna%20tickets&mode={mode}&include_sent=true&limit=5{cursor}"
            );
            let response = app.clone().oneshot(authed("GET", &uri)).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let page = body_json(response).await;
            for item in page["items"].as_array().unwrap() {
                assert!(
                    seen.insert(item["id"].as_i64().unwrap()),
                    "repeated hit in {mode}: {page}"
                );
                order.push(item["is_done"].as_bool().unwrap());
            }
            match page["next_cursor"].as_str() {
                Some(next) => {
                    assert!(next.starts_with("recall:"), "{mode}: {next}");
                    last_cursor = Some(next.to_string());
                    cursor = format!("&cursor={next}");
                }
                None => break,
            }
        }
        assert_eq!(seen.len(), 27, "every recalled hit served in {mode}");
        assert!(seen.contains(&sent_id), "sent mail is in the {mode} corpus");
        let first_done = order.iter().position(|d| *d).unwrap();
        assert!(
            order[first_done..].iter().any(|d| !*d),
            "{mode} held unfinished mail ahead of done mail: {order:?}"
        );

        // A caller sending neither flag still pages by offset, and is refused
        // a recall cursor.
        let c = last_cursor.expect("a 27-hit walk at 5 a page has a cursor");
        let response = app
            .clone()
            .oneshot(authed(
                "GET",
                &format!("/client/search?q=anjuna%20tickets&mode={mode}&limit=5&cursor={c}"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{mode}");
    }
}

// --- GET /client/rules/suggest ----------------------------------------------

/// One inbound message from `from` with `subject`, triaged at `sensitivity`.
fn seed_suggest_msg(
    store: &SqliteStore,
    acct: i64,
    gmail: &str,
    from: &str,
    subject: &str,
    sensitivity: Sensitivity,
) {
    let mut m = msg(acct, gmail, gmail, subject, "body");
    m.from_addr = from.to_string();
    let id = store.upsert_message(&m).unwrap();
    let kind = (sensitivity == Sensitivity::Sealed).then_some(SealedKind::Otp);
    store
        .set_triage(id, acct, 10, Tier::Noise, sensitivity, kind, "", "", None)
        .unwrap();
}

/// A state whose rule suggester points at a mock answering `suggestion`.
async fn app_with_rule_suggestion(
    suggestion: &'static str,
    seed: impl FnOnce(&SqliteStore, i64),
) -> Harness {
    let body = format!(
        r#"{{"content":[{{"type":"text","text":"{{\"suggestion\":\"{suggestion}\"}}"}}],"stop_reason":"end_turn","usage":{{"input_tokens":40,"output_tokens":6}}}}"#
    );
    app_with_suggest_body(body, seed).await
}

/// The suggester wired to a mock that answers every request with `body`.
async fn app_with_suggest_body(body: String, seed: impl FnOnce(&SqliteStore, i64)) -> Harness {
    use squelch_core::config::Stage2Provider;
    use squelch_core::triage::rule_suggest::RuleSuggestClient;

    let url = spawn_classifier_mock(200, body).await;
    let (state, store, acct) = state_with(seed);
    let state = state.with_rule_suggestion(Some(RuleSuggestClient::new(
        reqwest::Client::new(),
        url,
        "sk-test",
        Stage2Provider::Anthropic,
        "claude-haiku-4-5",
        None,
    )));
    Harness {
        app: router(state),
        store,
        acct,
    }
}

fn usage_categories(store: &SqliteStore, acct: i64) -> Vec<String> {
    store
        .list_usage_by_category(acct, 30)
        .unwrap()
        .into_iter()
        .map(|(c, _)| c)
        .collect()
}

#[tokio::test]
async fn rule_suggestion_is_cleaned_and_billed_to_its_own_category() {
    let Harness { app, store, acct } =
        app_with_rule_suggestion("only order confirmations.", |store, acct| {
            for (g, s) in [("g1", "Your order shipped"), ("g2", "New in the store")] {
                seed_suggest_msg(store, acct, g, "news@shop.com", s, Sensitivity::Normal);
            }
        })
        .await;

    let resp = app
        .oneshot(authed("GET", "/client/rules/suggest?sender=News@Shop.com"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["suggestion"], "only order confirmations");
    assert_eq!(
        usage_categories(&store, acct),
        vec!["rule_suggest".to_string()]
    );
}

/// The suggester answering a smart-block call with these three fields.
async fn app_with_smart_block(
    rule: &str,
    keeps: &str,
    drops: &str,
    seed: impl FnOnce(&SqliteStore, i64),
) -> Harness {
    let answer = serde_json::json!({ "rule": rule, "keeps": keeps, "drops": drops }).to_string();
    let body = serde_json::json!({
        "content": [{ "type": "text", "text": answer }],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 90, "output_tokens": 20 }
    })
    .to_string();
    app_with_suggest_body(body, seed).await
}

fn seed_mixed_sender(store: &SqliteStore, acct: i64) {
    for (g, s) in [
        ("g1", "New state filing requirement"),
        ("g2", "50% off your next LLC"),
        ("g3", "Webinar: grow your business"),
        ("g4", "Annual report deadline changed"),
    ] {
        seed_suggest_msg(store, acct, g, "info@corp.com", s, Sensitivity::Normal);
    }
}

const SMART_BLOCK: &str = "/client/unsubscribes/smart_block?sender=Info@Corp.com";

/// A sender with both kinds of mail, and an unsubscribe on file for them.
fn seed_ignored_unsubscribe(store: &SqliteStore, acct: i64) {
    seed_mixed_sender(store, acct);
    store
        .upsert_unsubscribe(acct, "info@corp.com", "one_click", None, chrono::Utc::now())
        .unwrap();
}

fn suggest_calls(store: &SqliteStore, acct: i64) -> u64 {
    store
        .list_usage_by_category(acct, 1)
        .unwrap()
        .into_iter()
        .flat_map(|(_, days)| days)
        .map(|d| d.calls)
        .sum()
}

#[tokio::test]
async fn a_smart_block_is_cleaned_billed_and_rides_the_unsubscribe_list() {
    let Harness { app, store, acct } = app_with_smart_block(
        "only state regulation alerts, no marketing.",
        "regulation alerts",
        "marketing and webinars",
        seed_ignored_unsubscribe,
    )
    .await;
    let proposal = serde_json::json!({
        "rule": "only state regulation alerts, no marketing",
        "keeps": "regulation alerts",
        "drops": "marketing and webinars",
    });
    let resp = app
        .clone()
        .oneshot(authed("GET", SMART_BLOCK))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["smart_block"], proposal);
    assert_eq!(
        usage_categories(&store, acct),
        vec!["rule_suggest".to_string()]
    );

    let list = body_json(
        app.oneshot(authed("GET", "/client/unsubscribes"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(list[0]["smart_block"], proposal);
    assert_eq!(list[0]["smart_block_asked"], true);
}

#[tokio::test]
async fn a_smart_block_is_asked_of_the_model_once() {
    let Harness { app, store, acct } = app_with_smart_block(
        "only regulation alerts",
        "regulation alerts",
        "marketing",
        seed_ignored_unsubscribe,
    )
    .await;
    let first = body_json(
        app.clone()
            .oneshot(authed("GET", SMART_BLOCK))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(first["smart_block"]["drops"], "marketing");

    // Swap what is remembered for something the model would not say: the
    // second answer can then only have come from the row.
    let mut kept = match store
        .unsubscribe_smart_block(acct, "info@corp.com")
        .unwrap()
    {
        Some(SmartBlockMemo::Asked(Some(block))) => block,
        other => panic!("the proposal was not kept on the row: {other:?}"),
    };
    kept.drops = "webinars".into();
    store
        .set_unsubscribe_smart_block(acct, "info@corp.com", Some(&kept))
        .unwrap();
    let second = body_json(
        app.clone()
            .oneshot(authed("GET", SMART_BLOCK))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(second["smart_block"]["drops"], "webinars");
    assert_eq!(suggest_calls(&store, acct), 1, "paid for a second call");

    // A proposal survives the owner asking again, which resets the rest.
    store
        .upsert_unsubscribe(acct, "info@corp.com", "one_click", None, chrono::Utc::now())
        .unwrap();
    assert_eq!(
        store
            .unsubscribe_smart_block(acct, "info@corp.com")
            .unwrap(),
        Some(SmartBlockMemo::Asked(Some(kept)))
    );
}

#[tokio::test]
async fn a_no_is_remembered_until_the_owner_unsubscribes_again() {
    // The model's "all promotion" answer.
    let Harness { app, store, acct } =
        app_with_smart_block("", "", "", seed_ignored_unsubscribe).await;
    for _ in 0..2 {
        let json = body_json(
            app.clone()
                .oneshot(authed("GET", SMART_BLOCK))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(json["smart_block"], Value::Null);
    }
    assert_eq!(
        suggest_calls(&store, acct),
        1,
        "asked again about a known no"
    );
    let list = body_json(
        app.oneshot(authed("GET", "/client/unsubscribes"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(list[0]["smart_block"], Value::Null);
    assert_eq!(list[0]["smart_block_asked"], true);

    store
        .upsert_unsubscribe(acct, "info@corp.com", "one_click", None, chrono::Utc::now())
        .unwrap();
    assert_eq!(
        store
            .unsubscribe_smart_block(acct, "info@corp.com")
            .unwrap(),
        Some(SmartBlockMemo::NotAsked)
    );
}

#[tokio::test]
async fn too_little_mail_is_neither_asked_nor_remembered_and_no_unsubscribe_is_404() {
    let Harness { app, store, acct } =
        app_with_smart_block("only alerts", "alerts", "marketing", |store, acct| {
            for g in ["g1", "g2", "g3"] {
                seed_suggest_msg(store, acct, g, "a@b.com", "hi", Sensitivity::Normal);
            }
            store
                .upsert_unsubscribe(acct, "a@b.com", "one_click", None, chrono::Utc::now())
                .unwrap();
            seed_mixed_sender(store, acct);
        })
        .await;
    // Three messages clear the sentence's floor but not this one.
    let resp = app
        .clone()
        .oneshot(authed(
            "GET",
            "/client/unsubscribes/smart_block?sender=a@b.com",
        ))
        .await
        .unwrap();
    assert_eq!(body_json(resp).await["smart_block"], Value::Null);
    assert_eq!(suggest_calls(&store, acct), 0);
    assert_eq!(
        store.unsubscribe_smart_block(acct, "a@b.com").unwrap(),
        Some(SmartBlockMemo::NotAsked)
    );

    // Plenty of mail, but nobody ever unsubscribed from them.
    let resp = app.oneshot(authed("GET", SMART_BLOCK)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(suggest_calls(&store, acct), 0);
}

#[tokio::test]
async fn rule_suggestion_is_null_without_a_model_or_without_history() {
    // No suggester on the default harness.
    let Harness { app, .. } = harness(|store, acct| {
        for g in ["g1", "g2", "g3"] {
            seed_suggest_msg(store, acct, g, "a@b.com", "hi", Sensitivity::Normal);
        }
    });
    let resp = app
        .oneshot(authed("GET", "/client/rules/suggest?sender=a@b.com"))
        .await
        .unwrap();
    assert_eq!(body_json(resp).await["suggestion"], Value::Null);

    // A suggester, but one message is not a pattern: no call, no spend.
    let Harness { app, store, acct } = app_with_rule_suggestion("mute them", |store, acct| {
        seed_suggest_msg(store, acct, "g1", "a@b.com", "hi", Sensitivity::Normal);
    })
    .await;
    let resp = app
        .oneshot(authed("GET", "/client/rules/suggest?sender=a@b.com"))
        .await
        .unwrap();
    assert_eq!(body_json(resp).await["suggestion"], Value::Null);
    assert!(usage_categories(&store, acct).is_empty());
}

#[tokio::test]
async fn sealed_mail_never_reaches_the_rule_suggester() {
    // The digest IS the suggester's input, so a sealed subject absent from it
    // cannot reach the model. Two sealed rows alone must not even clear the
    // history floor.
    let Harness { app, store, acct } = app_with_rule_suggestion("mute them", |store, acct| {
        seed_suggest_msg(
            store,
            acct,
            "g1",
            "bank@b.com",
            "code 111",
            Sensitivity::Sealed,
        );
        seed_suggest_msg(
            store,
            acct,
            "g2",
            "bank@b.com",
            "code 222",
            Sensitivity::Sealed,
        );
        seed_suggest_msg(
            store,
            acct,
            "g3",
            "bank@b.com",
            "statement",
            Sensitivity::Normal,
        );
    })
    .await;
    let digest = store.sender_rule_digest(acct, "bank@b.com", 15).unwrap();
    assert_eq!(
        digest
            .iter()
            .map(|r| r.subject.as_str())
            .collect::<Vec<_>>(),
        vec!["statement"]
    );
    let resp = app
        .oneshot(authed("GET", "/client/rules/suggest?sender=bank@b.com"))
        .await
        .unwrap();
    assert_eq!(body_json(resp).await["suggestion"], Value::Null);
    assert!(usage_categories(&store, acct).is_empty());
}

#[tokio::test]
async fn rule_suggestion_rejects_an_empty_sender_with_or_without_a_model() {
    let Harness { app, .. } = harness(|_, _| {});
    let resp = app
        .oneshot(authed("GET", "/client/rules/suggest?sender=%20"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "no model");

    let Harness { app, .. } = app_with_rule_suggestion("mute them", |_, _| {}).await;
    let resp = app
        .oneshot(authed("GET", "/client/rules/suggest?sender=%20"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "with a model");
}

#[tokio::test]
async fn rule_suggestion_stops_spending_at_the_daily_cap() {
    use squelch_core::store::UsageTokens;
    use squelch_core::triage::rule_suggest::DAILY_CAP;

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let Harness { app, store, acct } = app_with_rule_suggestion("mute them", |store, acct| {
        for g in ["g1", "g2", "g3"] {
            seed_suggest_msg(store, acct, g, "a@b.com", "hi", Sensitivity::Normal);
        }
        for _ in 0..DAILY_CAP {
            store
                .extract_bump_usage(acct, &today, "rule_suggest", UsageTokens::default())
                .unwrap();
        }
    })
    .await;
    let resp = app
        .oneshot(authed("GET", "/client/rules/suggest?sender=a@b.com"))
        .await
        .unwrap();
    assert_eq!(body_json(resp).await["suggestion"], Value::Null);
    let calls: u64 = store
        .list_usage_by_category(acct, 1)
        .unwrap()
        .into_iter()
        .flat_map(|(_, days)| days)
        .map(|d| d.calls)
        .sum();
    assert_eq!(calls, u64::from(DAILY_CAP), "no call past the cap");
}

#[tokio::test]
async fn a_garbled_suggestion_is_null_but_still_billed() {
    // The provider answered and charged for it; the answer just does not parse.
    let body = r#"{"content":[{"type":"text","text":"not json"}],"stop_reason":"end_turn","usage":{"input_tokens":40,"output_tokens":6}}"#.to_string();
    let Harness { app, store, acct } = app_with_suggest_body(body, |store, acct| {
        for g in ["g1", "g2"] {
            seed_suggest_msg(store, acct, g, "a@b.com", "hi", Sensitivity::Normal);
        }
    })
    .await;
    let resp = app
        .oneshot(authed("GET", "/client/rules/suggest?sender=a@b.com"))
        .await
        .unwrap();
    assert_eq!(body_json(resp).await["suggestion"], Value::Null);
    assert_eq!(
        usage_categories(&store, acct),
        vec!["rule_suggest".to_string()]
    );
}

// --- scheduled send: /client/scheduled and the worker -------------------------

use squelch_core::store::ScheduledSendState;

/// A harness plus the state under it, so a test can drive the router AND run
/// the worker's pass over the same store. `base` wires a write credential at a
/// mock Gmail; `None` is a daemon with none.
fn scheduled_rig(
    base: Option<String>,
    seed: impl FnOnce(&SqliteStore, i64),
) -> (Harness, ApiState) {
    let (state, store, acct) = state_with(seed);
    let state = match base {
        Some(base) => state.with_write_test_harness(Arc::new(StubCreds), base),
        None => state,
    };
    (
        Harness {
            app: router(state.clone()),
            store,
            acct,
        },
        state,
    )
}

fn in_minutes(minutes: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::minutes(minutes)).to_rfc3339()
}

fn rfc3339(v: &Value) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(v.as_str().expect("an RFC3339 string"))
        .unwrap()
        .with_timezone(&chrono::Utc)
}

/// One call on the outbox door. EVERY response there is `no-store`, the
/// refusals too, so every call asserts it.
async fn outbox(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = match body {
        Some(b) => authed_json(method, uri, b),
        None => authed(method, uri),
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    assert_eq!(
        resp.headers()
            .get(header::CACHE_CONTROL)
            .map(|v| v.to_str().unwrap()),
        Some("no-store"),
        "{method} {uri} ({status}) must be no-store"
    );
    (status, body_json(resp).await)
}

/// Audit details under `action`, newest first.
fn audited(store: &SqliteStore, acct: i64, action: &str) -> Vec<String> {
    store
        .list_audit(acct, 200)
        .unwrap()
        .into_iter()
        .filter(|a| a.action == action)
        .filter_map(|a| a.detail)
        .collect()
}

/// A due row straight into the store: a cold send to Alice, `send_at` a
/// minute ago, so the next pass claims it.
fn due_row(store: &SqliteStore, acct: i64, request: &str, msgid: &str) -> i64 {
    let now = chrono::Utc::now();
    store
        .insert_scheduled_send(
            acct,
            request,
            now - chrono::Duration::minutes(1),
            msgid,
            now,
        )
        .unwrap()
        .id
}

const COLD_REQUEST: &str =
    r#"{"to":"alice@example.com","subject":"Hi","body":"see you Tuesday","confirm":true}"#;

/// Gmail's answer to the echo's `format=raw` read-back of a sent message.
fn echo_response(id: &str) -> String {
    format!(
        "{{\"id\":\"{id}\",\"threadId\":\"thread-new\",\
           \"internalDate\":\"1783591200000\",\"raw\":\"{}\"}}",
        sent_reply_raw_b64()
    )
}

#[tokio::test]
async fn scheduling_freezes_the_request_and_consumes_the_draft() {
    let (base, _gmail) = mock_gmail(0).await;
    let (Harness { app, store, acct }, _) = scheduled_rig(Some(base), |_, _| {});
    let file = stage(&app, "plan.pdf", "application/pdf", "p1", b"PLAN").await["id"]
        .as_i64()
        .unwrap();
    let (_, draft) = draft_call(
        &app,
        "POST",
        "/client/drafts",
        serde_json::json!({ "to": "bob@example.com", "body": "later", "attachment_ids": [file] }),
    )
    .await;
    let draft_id = draft["id"].as_i64().unwrap();

    let send_at = in_minutes(90);
    let (status, created) = outbox(
        &app,
        "POST",
        "/client/scheduled",
        Some(serde_json::json!({
            "to": "bob@example.com",
            "cc": "",
            "subject": "The plan",
            "body": "see **attached**",
            "body_format": "markdown",
            "include_tracker": true,
            "attachment_ids": [file],
            "draft_id": draft_id,
            "confirm": true,
            "send_at": send_at,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_i64().unwrap();
    assert_eq!(rfc3339(&created["send_at"]), rfc3339(&Value::from(send_at)));
    assert!(
        created["send_at"].as_str().unwrap().ends_with('Z'),
        "spelled as the listing spells it: {}",
        created["send_at"]
    );

    // The draft is CONSUMED by scheduling; its file now belongs to the row.
    assert!(store.draft_by_id(acct, draft_id).unwrap().is_none());
    assert!(store.outbound_attachment(acct, file).unwrap().is_some());
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(
        (row.state, row.attempts),
        (ScheduledSendState::Scheduled, 0)
    );
    assert_eq!(
        row.attachments.iter().map(|a| a.id).collect::<Vec<_>>(),
        vec![file]
    );
    assert!(row.message_id_header.starts_with('<'));
    assert!(row.message_id_header.ends_with("@passband.app>"));
    // Another draft cannot take the file off the row.
    let (_, other) = draft_call(
        &app,
        "POST",
        "/client/drafts",
        serde_json::json!({ "body": "steal", "attachment_ids": [file] }),
    )
    .await;
    assert_eq!(other["attachments"], serde_json::json!([]));

    // The FROZEN REQUEST: the send body as posted, `confirm` true, `draft_id`
    // and `send_at` gone, a stated-empty `cc` kept (it clears a derivation).
    let stored: Value = serde_json::from_str(&row.request).unwrap();
    assert_eq!(
        stored,
        serde_json::json!({
            "to": "bob@example.com",
            "cc": "",
            "subject": "The plan",
            "body": "see **attached**",
            "body_format": "markdown",
            "include_tracker": true,
            "attachment_ids": [file],
            "confirm": true,
        })
    );
    assert_eq!(audited(&store, acct, "schedule"), vec![format!("ok:{id}")]);
}

#[tokio::test]
async fn scheduling_refuses_what_a_send_would_and_a_bad_date() {
    let (base, _gmail) = mock_gmail(0).await;
    let (Harness { app, store, acct }, _) = scheduled_rig(Some(base), |_, _| {});
    let post = |body: Value| {
        let app = app.clone();
        async move { outbox(&app, "POST", "/client/scheduled", Some(body)).await }
    };
    let cold = |send_at: Option<String>| {
        let mut v = serde_json::json!({
            "to": "bob@example.com",
            "body": "hi",
            "confirm": true,
        });
        if let Some(at) = send_at {
            v["send_at"] = Value::from(at);
        }
        v
    };

    // A past date is a client bug; so is a date past a year; and none at all.
    for body in [
        cold(Some(in_minutes(-5))),
        cold(Some(in_minutes(60 * 24 * 367))),
        cold(Some("tomorrow at 9".into())),
        cold(None),
    ] {
        let (status, _) = post(body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    // The send route's own ordered refusals, audited under `schedule`.
    let mut unconfirmed = cold(Some(in_minutes(60)));
    unconfirmed["confirm"] = Value::from(false);
    assert_eq!(post(unconfirmed).await.0, StatusCode::BAD_REQUEST);
    let mut missing = cold(Some(in_minutes(60)));
    missing["attachment_ids"] = serde_json::json!([987_654]);
    assert_eq!(post(missing).await.0, StatusCode::BAD_REQUEST);

    // An `individual` group fans out, and one Message-ID cannot name N mails.
    let member = |addr: &str| squelch_core::store::sqlite::groups::NewGroupMember {
        addr: addr.into(),
        display_name: None,
    };
    let fanout = store
        .create_send_group(
            acct,
            "Each",
            squelch_core::types::GroupMode::Individual,
            "",
            &[member("ann@fund.com"), member("bo@fund.com")],
        )
        .unwrap();
    let mut body = cold(Some(in_minutes(60)));
    body["group_id"] = Value::from(fanout);
    assert_eq!(post(body).await.0, StatusCode::BAD_REQUEST);
    // A bcc group is one message the composer addressed: fine, and the id
    // rides along for attribution at fire time.
    let bcc = store
        .create_send_group(
            acct,
            "Blind",
            squelch_core::types::GroupMode::Bcc,
            "",
            &[member("ann@fund.com"), member("bo@fund.com")],
        )
        .unwrap();
    let body = serde_json::json!({
        "group_id": bcc,
        "bcc": "ann@fund.com, bo@fund.com",
        "body": "hi all",
        "confirm": true,
        "send_at": in_minutes(60),
    });
    let (status, created) = post(body).await;
    assert_eq!(status, StatusCode::CREATED);

    let rows = store.list_scheduled_sends(acct).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![created["id"].as_i64().unwrap()],
        "no refusal left a row"
    );
    let details = audited(&store, acct, "schedule");
    for want in [
        "rejected:send_at",
        "rejected:confirm",
        "rejected:attachment_missing",
        "rejected:group_fanout",
    ] {
        assert!(details.iter().any(|d| d == want), "{want} in {details:?}");
    }
    assert_eq!(
        details.iter().filter(|d| *d == "rejected:send_at").count(),
        4
    );
}

#[tokio::test]
async fn scheduling_without_a_write_credential_is_403() {
    let (Harness { app, store, acct }, _) = scheduled_rig(None, |_, _| {});
    let (status, _) = outbox(
        &app,
        "POST",
        "/client/scheduled",
        Some(serde_json::json!({
            "to": "bob@example.com",
            "body": "hi",
            "confirm": true,
            "send_at": in_minutes(60),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(store.list_scheduled_sends(acct).unwrap().is_empty());
    assert_eq!(
        audited(&store, acct, "schedule"),
        vec!["rejected:no_write_credential".to_string()]
    );
}

#[tokio::test]
async fn the_guard_judges_a_scheduled_send_when_it_is_scheduled() {
    let (base, _gmail) = mock_gmail(0).await;
    let (Harness { app, store, acct }, _) = scheduled_rig(Some(base), |_, _| {});
    let secret = "-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----";
    let mut body = serde_json::json!({
        "to": "bob@example.com",
        "body": format!("here it is\n{secret}"),
        "confirm": true,
        "send_at": in_minutes(60),
    });
    let (status, err) = outbox(&app, "POST", "/client/scheduled", Some(body.clone())).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(err["error"].as_str().unwrap().contains("pem_block"));
    assert!(store.list_scheduled_sends(acct).unwrap().is_empty());

    // The override is given NOW and persists: the fire path never re-asks.
    body["override_guard"] = Value::from(true);
    let (status, created) = outbox(&app, "POST", "/client/scheduled", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED);
    let row = store
        .scheduled_send_by_id(acct, created["id"].as_i64().unwrap())
        .unwrap()
        .unwrap();
    let stored: Value = serde_json::from_str(&row.request).unwrap();
    assert_eq!(stored["override_guard"], true);

    let details = audited(&store, acct, "schedule");
    assert!(details.iter().any(|d| d == "blocked:guard"));
    assert!(
        details
            .iter()
            .any(|d| d.starts_with("guard_override:") && d.contains("pem_block"))
    );
    assert!(
        audited(&store, acct, "send").is_empty(),
        "nothing was sent, so nothing is on the send ledger"
    );
}

#[tokio::test]
async fn a_scheduled_forward_reads_and_judges_its_original_now() {
    let secret = "-----BEGIN RSA PRIVATE KEY-----";
    let (base, gmail) = mock_gmail_seq(vec![
        (200, original_raw_response(&format!("<pre>{secret}</pre>"))),
        (200, original_raw_response(&format!("<pre>{secret}</pre>"))),
    ])
    .await;
    let (Harness { app, store, acct }, _) = scheduled_rig(Some(base), |store, acct| {
        seed_one_signal(
            store,
            acct,
            "gmail-original",
            "thread-77",
            "Quarterly numbers",
        );
    });
    let original = store.search(acct, "quarterly", 10, 0).unwrap()[0].id;
    let mut body = serde_json::json!({
        "forward_of_message_id": original,
        "to": "carol@example.com",
        "body": "thought you should see this",
        "confirm": true,
        "send_at": in_minutes(60),
    });
    // A clean note around someone else's secret: blocked on the ORIGINAL.
    let (status, _) = outbox(&app, "POST", "/client/scheduled", Some(body.clone())).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    body["override_guard"] = Value::from(true);
    let (status, _) = outbox(&app, "POST", "/client/scheduled", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED);
    let reqs = gmail.await.unwrap();
    assert_eq!(reqs.len(), 2, "the original was read at each schedule");
    assert!(reqs.iter().all(|r| r.contains("/messages/gmail-original")));
    assert!(!reqs.iter().any(|r| r.contains("/messages/send")));
    assert!(
        audited(&store, acct, "schedule")
            .iter()
            .any(|d| d == "blocked:guard")
    );
}

#[tokio::test]
async fn the_outbox_lists_what_is_owed_an_outcome_with_its_parent() {
    let (base, _gmail) = mock_gmail(0).await;
    let (Harness { app, store, acct }, _) = scheduled_rig(Some(base), |store, acct| {
        seed_one_signal(store, acct, "gmail-parent", "thread-77", "Lunch?");
    });
    let parent = store.search(acct, "lunch", 10, 0).unwrap()[0].id;
    let (status, reply) = outbox(
        &app,
        "POST",
        "/client/scheduled",
        Some(serde_json::json!({
            "reply_to_message_id": parent,
            "cc": "carol@example.com",
            "bcc": "dan@example.com",
            "body": "yes, noon works",
            "confirm": true,
            "send_at": in_minutes(30),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let reply_id = reply["id"].as_i64().unwrap();

    let now = chrono::Utc::now();
    let row_at = |minutes: i64, tag: &str| {
        store
            .insert_scheduled_send(
                acct,
                &format!(
                    r#"{{"to":"{tag}@example.com","subject":"{tag}","body":"b","confirm":true}}"#
                ),
                now + chrono::Duration::minutes(minutes),
                &format!("<{tag}@passband>"),
                now,
            )
            .unwrap()
            .id
    };
    // A failed row (out of retries) still lists: it waits for the user.
    let failed = row_at(-10, "failed");
    store
        .claim_due_scheduled_send(acct, now, chrono::Duration::seconds(120))
        .unwrap()
        .unwrap();
    store
        .settle_scheduled_retry(
            acct,
            failed,
            1,
            now - chrono::Duration::minutes(20),
            "failed:gmail",
            false,
            now,
        )
        .unwrap();
    store
        .claim_due_scheduled_send(acct, now, chrono::Duration::seconds(120))
        .unwrap()
        .unwrap();
    store
        // Its last post was ambiguous: the listing says it may have gone.
        .settle_scheduled_failed(acct, failed, 2, "failed:gmail", true, None, now)
        .unwrap();
    // Sent and cancelled rows do not.
    let sent = row_at(-5, "sent");
    store
        .claim_due_scheduled_send(acct, now, chrono::Duration::seconds(120))
        .unwrap()
        .unwrap();
    store
        .settle_scheduled_sent(acct, sent, 1, Some(1), Some("t"), now)
        .unwrap();
    // A row IN GMAIL'S HANDS lists, as `sending`: the client shows it going.
    let sending = row_at(-4, "sending");
    store
        .claim_due_scheduled_send(acct, now, chrono::Duration::seconds(120))
        .unwrap()
        .unwrap();
    let cancelled = row_at(40, "cancelled");
    store.cancel_scheduled_send(acct, cancelled, now).unwrap();
    let later = row_at(60, "later");

    let (status, list) = outbox(&app, "GET", "/client/scheduled", None).await;
    assert_eq!(status, StatusCode::OK);
    let items = list.as_array().unwrap();
    assert_eq!(
        items
            .iter()
            .map(|i| i["id"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![failed, sending, reply_id, later],
        "oldest send_at first; sent and cancelled hidden"
    );
    for item in items {
        for key in [
            "id",
            "send_at",
            "state",
            "attempts",
            "last_error",
            "maybe_sent",
            "to",
            "cc",
            "bcc",
            "subject",
            "body",
            "reply_to_message_id",
            "forward_of_message_id",
            "attachments",
            "thread_id",
            "parent_subject",
            "parent_from",
            "parent_from_addr",
            "created_at",
            "updated_at",
        ] {
            assert!(item.get(key).is_some(), "{key} missing from {item}");
        }
    }
    assert_eq!(items[0]["state"], "failed");
    assert_eq!(items[0]["attempts"], 2);
    assert_eq!(items[0]["last_error"], "failed:gmail");
    assert_eq!(
        items[0]["maybe_sent"], true,
        "keyed on the flag, not the reason"
    );
    assert_eq!(items[0]["to"], "failed@example.com");
    assert_eq!(items[0]["subject"], "failed");
    assert_eq!(items[1]["state"], "sending");
    assert_eq!(items[1]["attempts"], 1);
    assert_eq!(items[1]["to"], "sending@example.com");
    assert_eq!(items[1]["maybe_sent"], false);
    assert_eq!(
        (&items[1]["cc"], &items[1]["bcc"]),
        (&Value::from(""), &Value::from("")),
        "absent copy lists read as \"\", like `to`"
    );

    let r = &items[2];
    assert_eq!(r["state"], "scheduled");
    assert_eq!(r["reply_to_message_id"], parent);
    assert_eq!(r["to"], "", "a reply's audience derives at fire time");
    assert_eq!(r["cc"], "carol@example.com");
    assert_eq!(r["bcc"], "dan@example.com");
    assert_eq!(r["body"], "yes, noon works");
    assert_eq!(r["thread_id"], "thread-77");
    assert_eq!(r["parent_subject"], "Lunch?");
    assert_eq!(r["parent_from"], "Alice");
    assert_eq!(r["parent_from_addr"], "alice@example.com");
    assert_eq!(r["attachments"], serde_json::json!([]));
    assert_eq!(items[3]["thread_id"], Value::Null);
}

#[tokio::test]
async fn cancelling_hands_the_request_and_its_files_back() {
    let (base, _gmail) = mock_gmail(0).await;
    let (Harness { app, store, acct }, _) = scheduled_rig(Some(base), |_, _| {});
    let file = stage(&app, "plan.pdf", "application/pdf", "p1", b"PLAN").await["id"]
        .as_i64()
        .unwrap();
    let (_, created) = outbox(
        &app,
        "POST",
        "/client/scheduled",
        Some(serde_json::json!({
            "to": "bob@example.com",
            "body": "later",
            "attachment_ids": [file],
            "confirm": true,
            "send_at": in_minutes(60),
        })),
    )
    .await;
    let id = created["id"].as_i64().unwrap();

    let (status, cancelled) =
        outbox(&app, "DELETE", &format!("/client/scheduled/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(cancelled["id"], id);
    assert_eq!(
        cancelled["request"],
        serde_json::json!({
            "to": "bob@example.com",
            "body": "later",
            "attachment_ids": [file],
        }),
        "the request as a JSON OBJECT, ready for the composer, `confirm` left to it"
    );
    assert_eq!(cancelled["attachments"][0]["id"], file);
    assert_eq!(cancelled["attachments"][0]["filename"], "plan.pdf");
    // UNCLAIMED again: the composer's next autosave can take it.
    let (_, draft) = draft_call(
        &app,
        "POST",
        "/client/drafts",
        serde_json::json!({ "body": "later", "attachment_ids": [file] }),
    )
    .await;
    assert_eq!(draft["attachments"][0]["id"], file);
    assert_eq!(
        audited(&store, acct, "schedule.cancel"),
        vec!["ok".to_string()]
    );

    // Twice is the same 404 as an unknown id and as another account's row.
    let (status, again) = outbox(&app, "DELETE", &format!("/client/scheduled/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, unknown) = outbox(&app, "DELETE", "/client/scheduled/987654", None).await;
    assert_eq!(again, unknown);
    let other = store.ensure_account("other@example.com").unwrap();
    let theirs = due_row(&store, other, COLD_REQUEST, "<theirs@passband>");
    let (status, foreign) =
        outbox(&app, "DELETE", &format!("/client/scheduled/{theirs}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(foreign, unknown);
    assert_eq!(
        store
            .scheduled_send_by_id(other, theirs)
            .unwrap()
            .unwrap()
            .state,
        ScheduledSendState::Scheduled
    );

    // A row in Gmail's hands cannot be pulled back.
    let busy = due_row(&store, acct, COLD_REQUEST, "<busy@passband>");
    store
        .claim_due_scheduled_send(acct, chrono::Utc::now(), chrono::Duration::seconds(120))
        .unwrap()
        .unwrap();
    let (status, err) = outbox(&app, "DELETE", &format!("/client/scheduled/{busy}"), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .contains("already being sent")
    );
}

#[tokio::test]
async fn send_now_queues_the_row_and_gives_a_failed_one_a_fresh_budget() {
    let (base, _gmail) = mock_gmail(0).await;
    let (Harness { app, store, acct }, _) = scheduled_rig(Some(base), |_, _| {});
    let (_, created) = outbox(
        &app,
        "POST",
        "/client/scheduled",
        Some(serde_json::json!({
            "to": "bob@example.com",
            "body": "later",
            "confirm": true,
            "send_at": in_minutes(90),
        })),
    )
    .await;
    let id = created["id"].as_i64().unwrap();
    let (status, queued) = outbox(&app, "POST", &format!("/client/scheduled/{id}/now"), None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(queued, serde_json::json!({ "status": "queued", "id": id }));
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert!(row.send_at <= chrono::Utc::now());

    // A failed row starts over: the user asked again.
    let failed = due_row(&store, acct, COLD_REQUEST, "<failed@passband>");
    let now = chrono::Utc::now();
    // The row above was pulled forward to now, so it is due too: claim both,
    // put that one back, fail this one.
    let mut claimed = Vec::new();
    for _ in 0..2 {
        claimed.push(
            store
                .claim_due_scheduled_send(acct, now, chrono::Duration::seconds(120))
                .unwrap()
                .unwrap()
                .id,
        );
    }
    assert!(claimed.contains(&id) && claimed.contains(&failed));
    store
        .settle_scheduled_retry(acct, id, 1, now, "failed:gmail", false, now)
        .unwrap();
    store
        .settle_scheduled_failed(acct, failed, 1, "failed:gmail", false, None, now)
        .unwrap();
    let row = store.scheduled_send_by_id(acct, failed).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Failed);
    let (status, _) = outbox(
        &app,
        "POST",
        &format!("/client/scheduled/{failed}/now"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let row = store.scheduled_send_by_id(acct, failed).unwrap().unwrap();
    assert_eq!(
        (row.state, row.attempts, row.last_error),
        (ScheduledSendState::Scheduled, 0, None)
    );

    let (status, _) = outbox(&app, "POST", "/client/scheduled/987654/now", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // IN GMAIL'S HANDS: the same 409 a cancel answers, not a 404.
    let busy = store
        .claim_due_scheduled_send(acct, chrono::Utc::now(), chrono::Duration::seconds(120))
        .unwrap()
        .unwrap()
        .id;
    let (status, err) = outbox(&app, "POST", &format!("/client/scheduled/{busy}/now"), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .contains("already being sent")
    );
    // Sent and cancelled rows have nothing left to queue: 404.
    let attempts = store
        .scheduled_send_by_id(acct, busy)
        .unwrap()
        .unwrap()
        .attempts;
    store
        .settle_scheduled_sent(acct, busy, attempts, None, None, chrono::Utc::now())
        .unwrap();
    let (status, _) = outbox(&app, "POST", &format!("/client/scheduled/{busy}/now"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let gone = due_row(&store, acct, COLD_REQUEST, "<gone@passband>");
    store
        .cancel_scheduled_send(acct, gone, chrono::Utc::now())
        .unwrap();
    let (status, _) = outbox(&app, "POST", &format!("/client/scheduled/{gone}/now"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        audited(&store, acct, "schedule.now"),
        vec![
            "failed:not_found".to_string(),
            "failed:not_found".to_string(),
            "rejected:sending".to_string(),
            "failed:not_found".to_string(),
            "ok".to_string(),
            "ok".to_string()
        ]
    );
}

#[tokio::test]
async fn stats_advertise_scheduled_send() {
    let Harness { app, .. } = harness(|_, _| {});
    let resp = app.oneshot(authed("GET", "/client/stats")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["scheduled_send"], true);
}

#[tokio::test]
async fn the_outbox_needs_the_bearer() {
    let Harness { app, .. } = harness(|_, _| {});
    for (method, uri) in [
        ("GET", "/client/scheduled"),
        ("POST", "/client/scheduled"),
        ("DELETE", "/client/scheduled/1"),
        ("POST", "/client/scheduled/1/now"),
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}

/// TWO DOORS: the outbox is human-door only. The agent door is another crate,
/// so it is checked the way its own drafts test checks it: its source must
/// not reach the outbox table or its store surface at all.
#[test]
fn the_agent_door_never_reaches_the_outbox() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../squelch-mcp/src");
    let mut scanned = 0;
    for entry in std::fs::read_dir(&src).expect("read squelch-mcp/src") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .expect("read source")
            .to_lowercase();
        // A test module may spell the word (its own two-door test names the
        // outbox's needles); the code above it may not.
        let text = text.split("#[cfg(test)]").next().unwrap_or_default();
        assert!(
            !text.contains("scheduled"),
            "{} mentions `scheduled`: the agent door must not reach the outbox",
            path.display()
        );
        scanned += 1;
    }
    assert!(scanned >= 3, "scanned only {scanned} source files");
}

#[tokio::test]
async fn the_worker_sends_a_due_row_with_its_minted_message_id() {
    let (base, gmail) = mock_gmail_seq(vec![
        (
            200,
            "{\"id\":\"sent-1\",\"threadId\":\"thread-new\"}".to_string(),
        ),
        (200, echo_response("sent-1")),
    ])
    .await;
    let (Harness { app, store, acct }, state) = scheduled_rig(Some(base), |_, _| {});
    let file = stage(&app, "plan.pdf", "application/pdf", "p1", b"PLAN").await["id"]
        .as_i64()
        .unwrap();
    let schedule = |minutes: i64, attachment_ids: Value| {
        let app = app.clone();
        async move {
            outbox(
                &app,
                "POST",
                "/client/scheduled",
                Some(serde_json::json!({
                    "to": "alice@example.com",
                    "subject": "Hi",
                    "body": "see you Tuesday",
                    "attachment_ids": attachment_ids,
                    "confirm": true,
                    "send_at": in_minutes(minutes),
                })),
            )
            .await
            .1["id"]
                .as_i64()
                .unwrap()
        }
    };
    let poked =
        || tokio::time::timeout(std::time::Duration::ZERO, state.scheduled_wake().notified());
    assert!(poked().await.is_err(), "a fresh state holds no permit");
    let due = schedule(60, serde_json::json!([file])).await;
    assert!(
        poked().await.is_ok(),
        "scheduling a row wakes the worker, which may be asleep until a later one"
    );
    let not_due = schedule(120, serde_json::json!([])).await;
    assert!(poked().await.is_ok());
    let (status, _) = outbox(&app, "POST", &format!("/client/scheduled/{due}/now"), None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(
        poked().await.is_ok(),
        "pulling a row forward wakes the worker for it"
    );
    let msgid = store
        .scheduled_send_by_id(acct, due)
        .unwrap()
        .unwrap()
        .message_id_header;

    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(report.sent, 1, "{report:?}");
    assert_eq!((report.retried, report.failed), (0, 0));

    let reqs = gmail.await.unwrap();
    assert_eq!(reqs.len(), 2, "one send, one echo read-back");
    assert!(reqs[0].contains("/messages/send"));
    let mime = sent_mime(&reqs[0]);
    assert!(
        mime.contains(&format!("Message-ID: {msgid}\r\n")),
        "the minted id rides the wire"
    );
    assert!(mime.contains("filename=\"plan.pdf\""));

    let row = store.scheduled_send_by_id(acct, due).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Sent);
    assert_eq!(row.attempts, 1);
    assert!(row.sent_message_id.is_some(), "the echo's local id");
    assert_eq!(row.sent_thread_id.as_deref(), Some("thread-new"));
    assert!(row.attachments.is_empty());
    assert!(
        store.outbound_attachment(acct, file).unwrap().is_none(),
        "the file went with the mail"
    );
    let untouched = store.scheduled_send_by_id(acct, not_due).unwrap().unwrap();
    assert_eq!(
        (untouched.state, untouched.attempts),
        (ScheduledSendState::Scheduled, 0)
    );

    // ONE ledger row for the send, carrying the marker, and so does its echo.
    let sends = audited(&store, acct, "send");
    assert_eq!(sends, vec![format!("scheduled:{due}:ok")]);
    assert_eq!(
        audited(&store, acct, "send.echo"),
        vec![format!(
            "scheduled:{due}:ok:{}",
            row.sent_message_id.unwrap()
        )]
    );

    let (_, list) = outbox(&app, "GET", "/client/scheduled", None).await;
    assert_eq!(
        list.as_array().unwrap().len(),
        1,
        "only the not-yet-due row"
    );
}

#[tokio::test]
async fn a_refused_send_backs_off_twice_then_fails_without_ever_searching() {
    // A 4xx is Gmail refusing the mail before sending it: a definitely-not-sent
    // failure, so every retry sends again WITHOUT a Message-ID search first.
    let (base, gmail) = mock_gmail_seq(vec![
        (400, "{\"error\":\"bad\"}".to_string()),
        (400, "{\"error\":\"bad\"}".to_string()),
        (400, "{\"error\":\"bad\"}".to_string()),
    ])
    .await;
    let (Harness { store, acct, .. }, state) = scheduled_rig(Some(base), |_, _| {});
    let id = due_row(&store, acct, COLD_REQUEST, "<retry@passband>");

    let before = chrono::Utc::now();
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!((report.sent, report.retried, report.failed), (0, 1, 0));
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Scheduled);
    assert_eq!(row.attempts, 1);
    assert_eq!(row.last_error.as_deref(), Some("failed:gmail"));
    assert!(!row.maybe_sent, "a 4xx never reached a mailbox");
    let wait = row.send_at - before;
    assert!(
        wait >= chrono::Duration::seconds(115) && wait <= chrono::Duration::seconds(130),
        "2^1 minutes: {wait}"
    );

    // Pulled forward (keeping its budget), it fails again: 2^2 minutes.
    let before = chrono::Utc::now();
    assert!(store.send_scheduled_now(acct, id, before).unwrap());
    squelch_api::run_scheduled_send_pass(&state).await;
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(
        (row.state, row.attempts),
        (ScheduledSendState::Scheduled, 2)
    );
    let wait = row.send_at - before;
    assert!(
        wait >= chrono::Duration::seconds(235) && wait <= chrono::Duration::seconds(250),
        "2^2 minutes: {wait}"
    );

    // The third failure is terminal and waits for the user.
    assert!(
        store
            .send_scheduled_now(acct, id, chrono::Utc::now())
            .unwrap()
    );
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(report.failed, 1);
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Failed);
    assert_eq!(row.attempts, 3);
    assert_eq!(row.last_error.as_deref(), Some("failed:gmail"));
    let reqs = gmail.await.unwrap();
    assert_eq!(reqs.len(), 3);
    assert!(
        reqs.iter().all(|r| r.contains("/messages/send")),
        "three sends, no search"
    );
    // `perform_send` audited each refusal once, MARKED with the row; the
    // worker added none.
    assert_eq!(
        audited(&store, acct, "send"),
        vec![format!("scheduled:{id}:failed:gmail"); 3]
    );
}

#[tokio::test]
async fn a_send_that_may_have_gone_is_searched_for_before_it_is_sent_again() {
    // Gmail took the send and answered 500: the mail may be in the mailbox.
    // The retry asks by Message-ID first, finds it, and sends NOTHING.
    let (base, gmail) = mock_gmail_seq(vec![
        (500, "{\"error\":\"backend\"}".to_string()),
        (
            200,
            "{\"messages\":[{\"id\":\"gm-5\",\"threadId\":\"thread-new\"}]}".to_string(),
        ),
        (200, echo_response("gm-5")),
    ])
    .await;
    let (Harness { store, acct, .. }, state) = scheduled_rig(Some(base), |_, _| {});
    let id = due_row(&store, acct, COLD_REQUEST, "<ambiguous@passband>");

    let before = chrono::Utc::now();
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(report.retried, 1, "{report:?}");
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert!(row.maybe_sent);
    assert!(
        row.send_at - before >= chrono::Duration::minutes(5),
        "the retry waits out the indexing grace, not two minutes"
    );

    // Send-now moves `send_at`, not the grace: the fire hands the claim back
    // (asking nothing) until the attempt is five minutes behind it.
    assert!(
        store
            .send_scheduled_now(acct, id, chrono::Utc::now())
            .unwrap()
    );
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(
        (report.deferred, report.recovered_sent),
        (1, 0),
        "{report:?}"
    );
    // Past the grace (the worker's clock stepped on), it asks, and finds it.
    let later = state
        .clone()
        .with_worker_clock_skew(chrono::Duration::minutes(6));
    let report = squelch_api::run_scheduled_send_pass(&later).await;
    assert_eq!((report.recovered_sent, report.sent), (1, 0), "{report:?}");
    let reqs = gmail.await.unwrap();
    assert_eq!(
        reqs.iter().filter(|r| r.contains("/messages/send")).count(),
        1,
        "EXACTLY ONE send hit Gmail"
    );
    assert!(reqs[1].contains("rfc822msgid%3Aambiguous%40passband"));
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Sent);
    assert_eq!(row.attempts, 2);
    assert_eq!(
        audited(&store, acct, "send"),
        vec![
            format!("scheduled:{id}:recovered"),
            format!("scheduled:{id}:failed:gmail"),
        ]
    );
}

#[tokio::test]
async fn a_re_fire_that_cannot_search_sends_nothing_and_spends_no_attempt() {
    let (base, gmail) = mock_gmail_seq(vec![(503, "{}".to_string())]).await;
    let (Harness { store, acct, .. }, state) = scheduled_rig(Some(base), |_, _| {});
    // A lapsed attempt that recovery did not find: `maybe_sent`, attempts 1,
    // its lease long enough ago that the grace is out.
    let id = lapsed_row(&store, acct, "<unsure@passband>", 10);
    store
        .requeue_scheduled_send(
            acct,
            id,
            1,
            chrono::Utc::now() - chrono::Duration::seconds(1),
            chrono::Utc::now(),
        )
        .unwrap();

    let before = chrono::Utc::now();
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!((report.deferred, report.sent), (1, 0), "{report:?}");
    let reqs = gmail.await.unwrap();
    assert_eq!(reqs.len(), 1);
    assert!(reqs[0].contains("rfc822msgid"), "a search, and no send");
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Scheduled);
    assert_eq!(row.attempts, 1, "the attempt was handed back");
    assert_eq!(row.last_error.as_deref(), Some("recovery:unreachable"));
    let wait = row.send_at - before;
    assert!(wait >= chrono::Duration::seconds(115), "{wait}");
}

#[tokio::test]
async fn a_paced_mailbox_spends_no_attempt() {
    use squelch_core::config::GmailQuotaProfile;
    use squelch_core::gmail_budget::GmailBudget;
    let (base, _gmail) = mock_gmail(0).await;
    let budget = Arc::new(
        GmailBudget::with_unit(GmailQuotaProfile::Standard, std::time::Duration::ZERO)
            .with_adaptation(
                std::time::Duration::from_secs(600),
                std::time::Duration::from_secs(600),
            ),
    );
    budget.note_quota_refusal(None);
    let (Harness { store, acct, .. }, state) = scheduled_rig(Some(base), |_, _| {});
    let state = state.with_gmail_budget(budget);
    let id = due_row(&store, acct, COLD_REQUEST, "<paced@passband>");

    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!((report.deferred, report.retried), (1, 0), "{report:?}");
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(
        (row.state, row.attempts, row.last_error.as_deref()),
        (ScheduledSendState::Scheduled, 0, Some("paced"))
    );
    assert!(!row.maybe_sent, "nothing was posted");
    // The ledger says what the row says: paced, not a Gmail failure.
    assert_eq!(
        audited(&store, acct, "send"),
        vec![format!("scheduled:{id}:paced")]
    );
}

/// A `sending` row whose lease lapsed `ago` minutes before now: the shape a
/// worker that died mid-send leaves.
fn lapsed_row(store: &SqliteStore, acct: i64, msgid: &str, ago: i64) -> i64 {
    let now = chrono::Utc::now();
    let id = store
        .insert_scheduled_send(
            acct,
            COLD_REQUEST,
            now - chrono::Duration::minutes(ago + 30),
            msgid,
            now - chrono::Duration::minutes(ago + 30),
        )
        .unwrap()
        .id;
    store
        .claim_due_scheduled_send(
            acct,
            now - chrono::Duration::minutes(ago) - chrono::Duration::seconds(1),
            chrono::Duration::seconds(1),
        )
        .unwrap()
        .expect("claimed");
    id
}

#[tokio::test]
async fn recovery_settles_a_mail_gmail_already_has_without_resending_it() {
    let (base, gmail) = mock_gmail_seq(vec![
        (
            200,
            "{\"messages\":[{\"id\":\"gm-9\",\"threadId\":\"thread-new\"}]}".to_string(),
        ),
        (200, echo_response("gm-9")),
    ])
    .await;
    let (Harness { store, acct, .. }, state) = scheduled_rig(Some(base), |_, _| {});
    let id = lapsed_row(&store, acct, "<crashed@passband>", 10);

    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!((report.recovered_sent, report.sent), (1, 0), "{report:?}");
    let reqs = gmail.await.unwrap();
    assert_eq!(reqs.len(), 2);
    assert!(reqs[0].contains("rfc822msgid%3Acrashed%40passband"));
    assert!(reqs[1].contains("/messages/gm-9"));
    assert!(
        !reqs.iter().any(|r| r.contains("/messages/send")),
        "EXACTLY ONCE: no second send"
    );
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Sent);
    assert!(row.sent_message_id.is_some());
    assert_eq!(row.sent_thread_id.as_deref(), Some("thread-new"));
    assert_eq!(
        audited(&store, acct, "send"),
        vec![format!("scheduled:{id}:recovered")]
    );
    assert_eq!(
        audited(&store, acct, "send.echo"),
        vec![format!(
            "scheduled:{id}:ok:{}",
            row.sent_message_id.unwrap()
        )]
    );
}

/// Gmail's `format=metadata` answer for a Sent candidate of the scan.
fn scan_meta(id: &str, outbox: &str, msgid: &str) -> String {
    format!(
        "{{\"id\":\"{id}\",\"payload\":{{\"headers\":[\
           {{\"name\":\"Message-ID\",\"value\":\"{msgid}\"}},\
           {{\"name\":\"X-Passband-Outbox\",\"value\":\"{outbox}\"}}]}}}}"
    )
}

#[tokio::test]
async fn recovery_finds_a_mail_whose_message_id_gmail_rewrote_by_its_outbox_header() {
    // THE LIVE FINDING (2026-09-29): the mail went out, but `rfc822msgid:`
    // for the minted id finds nothing. The Sent scan reads the outbox header
    // and settles it: recovered, never sent again.
    let (base, gmail) = mock_gmail_seq(vec![
        (200, "{\"resultSizeEstimate\":0}".to_string()),
        (
            200,
            "{\"messages\":[{\"id\":\"gm-other\"},{\"id\":\"gm-9\"}]}".to_string(),
        ),
        (200, scan_meta("gm-other", "", "<CAx@mail.gmail.com>")),
        (200, scan_meta("gm-9", "crashed", "<CAy@mail.gmail.com>")),
        (200, echo_response("gm-9")),
    ])
    .await;
    let (Harness { store, acct, .. }, state) = scheduled_rig(Some(base), |_, _| {});
    let id = lapsed_row(&store, acct, "<crashed@passband>", 10);

    let before = chrono::Utc::now();
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!((report.recovered_sent, report.sent), (1, 0), "{report:?}");
    let reqs = gmail.await.unwrap();
    assert_eq!(reqs.len(), 5);
    assert!(reqs[0].contains("rfc822msgid%3Acrashed%40passband"));
    assert!(reqs[1].contains("q=in%3Asent%20after%3A"));
    assert!(reqs[1].contains("includeSpamTrash=true"));
    // The window: from `send_at` (40 min ago) less one whole lease (at least
    // the 3 min base) and the hour of slack, to where the lapsed lease ended
    // (10 min ago) plus the slack. Tight enough that dropping the lease from
    // the floor, or the ceiling altogether, fails here.
    let bound = |op: &str, end: char| -> i64 {
        reqs[1]
            .split(op)
            .nth(1)
            .unwrap()
            .split(end)
            .next()
            .unwrap()
            .parse()
            .unwrap()
    };
    let (floor, ceiling) = (bound("after%3A", '%'), bound("before%3A", '&'));
    let ago = |m: i64| (before - chrono::Duration::minutes(m)).timestamp();
    assert!(
        (ago(40 + 60 + 10)..=ago(40 + 60 + 3)).contains(&floor),
        "floor {floor} vs now {}",
        before.timestamp()
    );
    assert!(
        (ago(10 - 60 + 1)..=ago(10 - 60 - 1)).contains(&ceiling),
        "ceiling {ceiling} vs now {}",
        before.timestamp()
    );
    assert!(reqs[2].contains("/messages/gm-other?format=metadata"));
    assert!(reqs[3].contains("/messages/gm-9?format=metadata&metadataHeaders=X-Passband-Outbox"));
    assert!(reqs[4].contains("/messages/gm-9?format=raw"), "the echo");
    assert!(
        !reqs.iter().any(|r| r.contains("/messages/send")),
        "EXACTLY ONCE: no second send"
    );
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Sent);
    assert!(row.sent_message_id.is_some());
    assert_eq!(
        audited(&store, acct, "send"),
        vec![format!("scheduled:{id}:recovered")]
    );
}

#[tokio::test]
async fn a_scan_that_fails_midway_defers_recovery_and_requeues_nothing() {
    let (base, gmail) = mock_gmail_seq(vec![
        (200, "{\"resultSizeEstimate\":0}".to_string()),
        (200, "{\"messages\":[{\"id\":\"gm-a\"}]}".to_string()),
        (503, "{}".to_string()),
    ])
    .await;
    let (Harness { store, acct, .. }, state) = scheduled_rig(Some(base), |_, _| {});
    let id = lapsed_row(&store, acct, "<midway@passband>", 10);

    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(
        (report.deferred, report.requeued, report.sent),
        (1, 0, 0),
        "{report:?}"
    );
    assert_eq!(gmail.await.unwrap().len(), 3, "no send");
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(
        row.state,
        ScheduledSendState::Sending,
        "a half-read scan is could-not-ask, never did-not-go-out"
    );
    assert_eq!(row.last_error.as_deref(), Some("recovery:unreachable"));
}

#[tokio::test]
async fn the_pre_fire_search_reads_the_outbox_header_too() {
    // Gmail took the send and answered 500. The re-fire's msgid search
    // misses (the id was rewritten); the Sent scan finds the header the fired
    // send carried, and nothing is sent twice.
    let (base, gmail) = mock_gmail_seq(vec![
        (500, "{\"error\":\"backend\"}".to_string()),
        (200, "{\"resultSizeEstimate\":0}".to_string()),
        (200, "{\"messages\":[{\"id\":\"gm-5\"}]}".to_string()),
        (200, scan_meta("gm-5", "prefire", "<CAz@mail.gmail.com>")),
        (200, echo_response("gm-5")),
    ])
    .await;
    let (Harness { store, acct, .. }, state) = scheduled_rig(Some(base), |_, _| {});
    let id = due_row(&store, acct, COLD_REQUEST, "<prefire@passband>");

    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(report.retried, 1, "{report:?}");
    let later = state
        .clone()
        .with_worker_clock_skew(chrono::Duration::minutes(6));
    assert!(
        store
            .send_scheduled_now(acct, id, chrono::Utc::now())
            .unwrap()
    );
    let report = squelch_api::run_scheduled_send_pass(&later).await;
    assert_eq!((report.recovered_sent, report.sent), (1, 0), "{report:?}");
    let reqs = gmail.await.unwrap();
    assert_eq!(reqs.len(), 5);
    // The fired send carried both fingerprints.
    let mime = sent_mime(&reqs[0]);
    assert!(mime.contains("Message-ID: <prefire@passband>\r\n"));
    assert!(mime.contains("X-Passband-Outbox: prefire\r\n"));
    assert!(reqs[1].contains("rfc822msgid%3Aprefire%40passband"));
    assert!(reqs[2].contains("q=in%3Asent%20after%3A"));
    assert!(reqs[3].contains("/messages/gm-5?format=metadata"));
    assert_eq!(
        reqs.iter().filter(|r| r.contains("/messages/send")).count(),
        1,
        "EXACTLY ONE send hit Gmail"
    );
    assert_eq!(
        store.scheduled_send_by_id(acct, id).unwrap().unwrap().state,
        ScheduledSendState::Sent
    );
}

#[tokio::test]
async fn a_lease_that_lapsed_inside_the_grace_is_not_asked_about_yet() {
    let (base, gmail) = mock_gmail(0).await;
    let (Harness { store, acct, .. }, state) = scheduled_rig(Some(base), |_, _| {});
    let id = lapsed_row(&store, acct, "<fresh@passband>", 1);
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert!(report.is_idle(), "{report:?}");
    assert!(gmail.await.unwrap().is_empty(), "Gmail was not asked");
    assert_eq!(
        store.scheduled_send_by_id(acct, id).unwrap().unwrap().state,
        ScheduledSendState::Sending
    );
}

#[tokio::test]
async fn recovery_requeues_a_mail_gmail_never_saw_and_does_not_send_it_in_the_same_pass() {
    let (base, gmail) = mock_gmail_seq(vec![
        // Recovery: the msgid search misses, and so does the Sent scan.
        (200, "{\"resultSizeEstimate\":0}".to_string()),
        (200, "{\"resultSizeEstimate\":0}".to_string()),
        // The re-fire, a grace later: asked AGAIN (both looks), then sent.
        (200, "{\"resultSizeEstimate\":0}".to_string()),
        (200, "{\"resultSizeEstimate\":0}".to_string()),
        (
            200,
            "{\"id\":\"sent-2\",\"threadId\":\"thread-new\"}".to_string(),
        ),
        (200, echo_response("sent-2")),
    ])
    .await;
    let (Harness { store, acct, .. }, state) = scheduled_rig(Some(base), |_, _| {});
    let id = lapsed_row(&store, acct, "<never@passband>", 10);

    let before = chrono::Utc::now();
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!((report.requeued, report.sent), (1, 0), "{report:?}");
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Scheduled);
    assert!(row.maybe_sent);
    assert!(row.send_at - before >= chrono::Duration::minutes(5));

    assert!(
        store
            .send_scheduled_now(acct, id, chrono::Utc::now())
            .unwrap()
    );
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(report.sent, 1, "{report:?}");
    let reqs = gmail.await.unwrap();
    assert_eq!(reqs.len(), 6);
    assert!(reqs[0].contains("rfc822msgid") && reqs[2].contains("rfc822msgid"));
    assert!(reqs[1].contains("q=in%3Asent%20after%3A") && reqs[3].contains("q=in%3Asent"));
    assert!(reqs[4].contains("/messages/send"));
    let mime = sent_mime(&reqs[4]);
    assert!(mime.contains("Message-ID: <never@passband>\r\n"));
    assert!(mime.contains("X-Passband-Outbox: never\r\n"));
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Sent);
    assert_eq!(row.attempts, 2, "the lapsed attempt counted");
}

#[tokio::test]
async fn a_row_that_keeps_dying_mid_send_stops_at_its_last_attempt() {
    let (base, gmail) = mock_gmail_seq(vec![
        (200, "{\"resultSizeEstimate\":0}".to_string()),
        (200, "{\"resultSizeEstimate\":0}".to_string()),
    ])
    .await;
    let (Harness { store, acct, .. }, state) = scheduled_rig(Some(base), |_, _| {});
    let id = lapsed_row(&store, acct, "<loop@passband>", 10);
    let now = chrono::Utc::now();
    // Two earlier lapses: this is its third claim.
    for attempts in 1..3 {
        store
            .requeue_scheduled_send(acct, id, attempts, now - chrono::Duration::minutes(20), now)
            .unwrap();
        store
            .claim_due_scheduled_send(
                acct,
                now - chrono::Duration::minutes(10) - chrono::Duration::seconds(1),
                chrono::Duration::seconds(1),
            )
            .unwrap()
            .unwrap();
    }
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!((report.failed, report.requeued), (1, 0), "{report:?}");
    assert_eq!(
        gmail.await.unwrap().len(),
        2,
        "the msgid search and the scan"
    );
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Failed);
    assert_eq!(row.last_error.as_deref(), Some("failed:crash_loop"));
    assert!(row.maybe_sent, "one miss in a trailing index is not proof");
    assert!(
        row.last_attempt_at.expect("stamped") <= now - chrono::Duration::minutes(5),
        "the attempt ended when its lease lapsed, not at the give-up"
    );
    // The worker decided this one without `perform_send`: it audits it.
    assert_eq!(
        audited(&store, acct, "send"),
        vec![format!("scheduled:{id}:failed:crash_loop")]
    );
}

#[tokio::test]
async fn recovery_that_cannot_ask_gmail_decides_nothing_and_asks_again_later() {
    let (base, gmail) = mock_gmail_seq(vec![(500, "{}".to_string())]).await;
    let (Harness { store, acct, .. }, state) = scheduled_rig(Some(base), |_, _| {});
    let id = lapsed_row(&store, acct, "<unknown@passband>", 10);

    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(report.deferred, 1, "{report:?}");
    // A second pass inside the minute asks nothing (one call per row per
    // minute, not per tick).
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert!(report.is_idle(), "{report:?}");
    assert_eq!(gmail.await.unwrap().len(), 1, "one search, no send");
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(
        row.state,
        ScheduledSendState::Sending,
        "could not ask is never did not go out"
    );
    assert_eq!(row.last_error.as_deref(), Some("recovery:unreachable"));
}

#[tokio::test]
async fn a_row_recovery_can_never_ask_about_goes_to_the_user() {
    // No write credential at all: the listing says why while the row waits,
    // and past the give-up it is `failed:recovery` for the user to decide.
    let (Harness { store, acct, .. }, state) = scheduled_rig(None, |_, _| {});
    let waiting = lapsed_row(&store, acct, "<waiting@passband>", 10);
    let stuck = lapsed_row(&store, acct, "<stuck@passband>", 31);
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!((report.deferred, report.failed), (1, 1), "{report:?}");
    let row = store.scheduled_send_by_id(acct, waiting).unwrap().unwrap();
    assert_eq!(
        (row.state, row.last_error.as_deref()),
        (
            ScheduledSendState::Sending,
            Some("rejected:no_write_credential")
        )
    );
    let row = store.scheduled_send_by_id(acct, stuck).unwrap().unwrap();
    assert_eq!(
        (row.state, row.last_error.as_deref()),
        (ScheduledSendState::Failed, Some("failed:recovery"))
    );
}

#[tokio::test]
async fn a_corrupt_request_fails_without_a_send() {
    let (Harness { store, acct, .. }, state) = scheduled_rig(None, |_, _| {});
    let id = due_row(&store, acct, "not json {", "<corrupt@passband>");
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(report.failed, 1);
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Failed);
    assert_eq!(row.last_error.as_deref(), Some("failed:request"));
    // No send was attempted, and the ledger still says why the row stopped.
    assert_eq!(
        audited(&store, acct, "send"),
        vec![format!("scheduled:{id}:failed:request")]
    );
}

#[tokio::test]
async fn a_missing_write_credential_is_a_retry_not_a_drop() {
    let (Harness { store, acct, .. }, state) = scheduled_rig(None, |_, _| {});
    let id = due_row(&store, acct, COLD_REQUEST, "<nocred@passband>");
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(report.retried, 1);
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Scheduled);
    assert_eq!(
        row.last_error.as_deref(),
        Some("rejected:no_write_credential")
    );
    assert_eq!(
        audited(&store, acct, "send"),
        vec![format!("scheduled:{id}:rejected:no_write_credential")]
    );
}

#[tokio::test]
async fn a_pass_claims_at_most_its_share_and_stops_on_shutdown() {
    let (Harness { store, acct, .. }, state) = scheduled_rig(None, |_, _| {});
    let ids: Vec<i64> = (0..21)
        .map(|i| due_row(&store, acct, COLD_REQUEST, &format!("<n{i}@passband>")))
        .collect();
    // No credential: every fire is a quick retryable failure.
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(report.retried, 20, "{report:?}");
    let untouched = ids
        .iter()
        .filter(|id| {
            store
                .scheduled_send_by_id(acct, **id)
                .unwrap()
                .unwrap()
                .attempts
                == 0
        })
        .count();
    assert_eq!(untouched, 1, "the twenty-first waits for the next tick");

    // A daemon that has been asked to stop claims nothing.
    let (tx, rx) = tokio::sync::watch::channel(true);
    let stopping = state.with_shutdown(rx);
    let before: Vec<u32> = ids
        .iter()
        .map(|id| {
            store
                .scheduled_send_by_id(acct, *id)
                .unwrap()
                .unwrap()
                .attempts
        })
        .collect();
    for id in &ids {
        store
            .send_scheduled_now(acct, *id, chrono::Utc::now())
            .unwrap();
    }
    let report = squelch_api::run_scheduled_send_pass(&stopping).await;
    assert_eq!(report.retried, 0, "{report:?}");
    let after: Vec<u32> = ids
        .iter()
        .map(|id| {
            store
                .scheduled_send_by_id(acct, *id)
                .unwrap()
                .unwrap()
                .attempts
        })
        .collect();
    assert_eq!(before, after);
    drop(tx);
}

#[tokio::test]
async fn a_fired_row_never_sends_a_file_it_does_not_hold() {
    let (base, gmail) = mock_gmail(0).await;
    let (Harness { app, store, acct }, state) = scheduled_rig(Some(base), |_, _| {});
    let file = stage(&app, "plan.pdf", "application/pdf", "p1", b"PLAN").await["id"]
        .as_i64()
        .unwrap();
    let (_, created) = outbox(
        &app,
        "POST",
        "/client/scheduled",
        Some(serde_json::json!({
            "to": "bob@example.com",
            "body": "attached",
            "attachment_ids": [file],
            "confirm": true,
            "send_at": in_minutes(60),
        })),
    )
    .await;
    let id = created["id"].as_i64().unwrap();

    // The compose door cannot delete a file the outbox holds: 404, as for an
    // unknown id. Cancel is the only release.
    let resp = app
        .clone()
        .oneshot(authed(
            "DELETE",
            &format!("/client/compose/attachments/{file}"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(store.outbound_attachment(acct, file).unwrap().is_some());

    // Were its id ever to name somebody else's fresh, unclaimed upload (the
    // row's file gone, the rowid reused), the fire refuses rather than send
    // a file nobody scanned for this mail.
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    store
        .release_scheduled_send_attachments(acct, id, chrono::Utc::now())
        .unwrap();
    assert!(row.attachments.len() == 1);
    store
        .send_scheduled_now(acct, id, chrono::Utc::now())
        .unwrap();
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(report.failed, 1, "{report:?}");
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(row.last_error.as_deref(), Some("failed:attachment_missing"));
    assert!(gmail.await.unwrap().is_empty(), "nothing was sent");
    assert!(
        store.outbound_attachment(acct, file).unwrap().is_some(),
        "and the unclaimed file was not consumed"
    );
}

#[tokio::test]
async fn a_fired_group_send_is_attribution_only() {
    let (base, gmail) = mock_gmail_seq(vec![
        (
            200,
            "{\"id\":\"sent-g\",\"threadId\":\"thread-new\"}".to_string(),
        ),
        (200, echo_response("sent-g")),
        (
            200,
            "{\"id\":\"sent-h\",\"threadId\":\"thread-new-2\"}".to_string(),
        ),
        (200, echo_response("sent-h")),
    ])
    .await;
    let (Harness { app, store, acct }, state) = scheduled_rig(Some(base), |_, _| {});
    let member = |addr: &str| squelch_core::store::sqlite::groups::NewGroupMember {
        addr: addr.into(),
        display_name: None,
    };
    let members = [member("ann@fund.com"), member("bo@fund.com")];
    let flipped = store
        .create_send_group(
            acct,
            "Blind",
            squelch_core::types::GroupMode::Bcc,
            "",
            &members,
        )
        .unwrap();
    let doomed = store
        .create_send_group(
            acct,
            "Doomed",
            squelch_core::types::GroupMode::Bcc,
            "",
            &members,
        )
        .unwrap();
    let schedule = |group: i64| {
        let app = app.clone();
        async move {
            outbox(
                &app,
                "POST",
                "/client/scheduled",
                Some(serde_json::json!({
                    "group_id": group,
                    "bcc": "ann@fund.com, bo@fund.com",
                    "subject": "Update",
                    "body": "hi all",
                    "confirm": true,
                    "send_at": in_minutes(60),
                })),
            )
            .await
            .1["id"]
                .as_i64()
                .unwrap()
        }
    };
    let a = schedule(flipped).await;
    let b = schedule(doomed).await;

    // After scheduling: one group flipped to one-per-person, one deleted.
    store
        .update_send_group(
            acct,
            flipped,
            "Blind",
            squelch_core::types::GroupMode::Individual,
            "",
            &members,
        )
        .unwrap();
    store.delete_send_group(acct, doomed).unwrap();

    for id in [a, b] {
        store
            .send_scheduled_now(acct, id, chrono::Utc::now())
            .unwrap();
    }
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(
        (report.sent, report.failed, report.retried),
        (2, 0, 0),
        "{report:?}"
    );

    let reqs = gmail.await.unwrap();
    let sends: Vec<&String> = reqs
        .iter()
        .filter(|r| r.contains("/messages/send"))
        .collect();
    assert_eq!(sends.len(), 2, "ONE message each, never a fan-out");
    for (send, id) in sends.iter().zip([a, b]) {
        let msgid = store
            .scheduled_send_by_id(acct, id)
            .unwrap()
            .unwrap()
            .message_id_header;
        let mime = sent_mime(send);
        assert!(mime.contains(&format!("Message-ID: {msgid}\r\n")));
        assert!(
            mime.contains("Bcc: ann@fund.com, bo@fund.com\r\n"),
            "{mime}"
        );
    }
    for id in [a, b] {
        assert_eq!(
            store.scheduled_send_by_id(acct, id).unwrap().unwrap().state,
            ScheduledSendState::Sent
        );
    }
    // The flipped group records the send the way it WENT; the deleted one
    // records nothing, and cost nothing.
    let history = store.group_history(acct, flipped, 10, 0).unwrap();
    assert_eq!(history.len(), 1, "{history:?}");
    assert_eq!(history[0].mode, squelch_core::types::GroupMode::Bcc);
    assert!(history[0].group_send_id.is_some(), "a recorded send");
    assert!(
        store
            .group_history(acct, doomed, 10, 0)
            .unwrap()
            .iter()
            .all(|h| h.group_send_id.is_none()),
        "no attribution row for a group that is gone"
    );
}

#[tokio::test]
async fn the_worker_fires_a_reply_threaded_under_its_minted_id() {
    let (base, gmail) = mock_gmail_seq(vec![
        (200, reply_to_metadata_body()),
        (
            200,
            "{\"id\":\"sent-r\",\"threadId\":\"thread-77\"}".to_string(),
        ),
        (200, echo_response("sent-r")),
    ])
    .await;
    let (Harness { app, store, acct }, state) = scheduled_rig(Some(base), |store, acct| {
        seed_one_signal(store, acct, "gmail-parent", "thread-77", "Lunch?");
    });
    let parent = store.search(acct, "lunch", 10, 0).unwrap()[0].id;
    let (status, created) = outbox(
        &app,
        "POST",
        "/client/scheduled",
        Some(serde_json::json!({
            "reply_to_message_id": parent,
            "body": "yes, noon works",
            "confirm": true,
            "send_at": in_minutes(30),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_i64().unwrap();
    store
        .send_scheduled_now(acct, id, chrono::Utc::now())
        .unwrap();

    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(report.sent, 1, "{report:?}");
    let reqs = gmail.await.unwrap();
    assert!(
        reqs[0].contains("/messages/gmail-parent"),
        "the parent's headers"
    );
    assert!(reqs[1].contains("/messages/send"));
    assert!(reqs[1].contains("\"threadId\":\"thread-77\""));
    let msgid = store
        .scheduled_send_by_id(acct, id)
        .unwrap()
        .unwrap()
        .message_id_header;
    let mime = sent_mime(&reqs[1]);
    assert!(mime.contains("In-Reply-To: <parent@x>\r\n"), "{mime}");
    assert!(mime.contains(&format!("Message-ID: {msgid}\r\n")), "{mime}");
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Sent);
    assert_eq!(row.sent_thread_id.as_deref(), Some("thread-77"));
    // `resolve_done`: a fired reply handles the message it answers, as a
    // reply sent now does.
    let done = mail_list(
        &store,
        acct,
        chrono::Utc::now() - chrono::Duration::days(365),
        None,
        Some(AttentionStatus::Done),
        None,
        false,
        SpamScope::Exclude,
    )
    .unwrap();
    assert!(
        done.iter().any(|u| u.update.id == parent),
        "the parent is handled"
    );
    assert_eq!(
        audited(&store, acct, "send"),
        vec![format!("scheduled:{id}:ok")]
    );
}

#[tokio::test]
async fn the_worker_fires_a_forward_without_judging_it_again() {
    let secret = "-----BEGIN RSA PRIVATE KEY-----";
    let (base, gmail) = mock_gmail_seq(vec![
        // Schedule time: the original is read and judged (overridden).
        (200, original_raw_response(&format!("<pre>{secret}</pre>"))),
        // Fire time: read again, and NOT judged again.
        (200, original_raw_response(&format!("<pre>{secret}</pre>"))),
        (
            200,
            "{\"id\":\"sent-f\",\"threadId\":\"thread-fwd\"}".to_string(),
        ),
        (200, echo_response("sent-f")),
    ])
    .await;
    let (Harness { app, store, acct }, state) = scheduled_rig(Some(base), |store, acct| {
        seed_one_signal(
            store,
            acct,
            "gmail-original",
            "thread-77",
            "Quarterly numbers",
        );
    });
    let original = store.search(acct, "quarterly", 10, 0).unwrap()[0].id;
    let (status, created) = outbox(
        &app,
        "POST",
        "/client/scheduled",
        Some(serde_json::json!({
            "forward_of_message_id": original,
            "to": "carol@example.com",
            "body": "fyi",
            "override_guard": true,
            "confirm": true,
            "send_at": in_minutes(60),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_i64().unwrap();
    store
        .send_scheduled_now(acct, id, chrono::Utc::now())
        .unwrap();
    let guard_rows = audited(&store, acct, "schedule").len();

    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(report.sent, 1, "{report:?}");
    let reqs = gmail.await.unwrap();
    assert!(reqs[1].contains("/messages/gmail-original"));
    assert!(reqs[2].contains("/messages/send"));
    assert!(
        !reqs[2].contains("threadId"),
        "a forward opens a new thread"
    );
    let msgid = store
        .scheduled_send_by_id(acct, id)
        .unwrap()
        .unwrap()
        .message_id_header;
    assert!(
        sent_mime(&reqs[2]).contains(&format!("Message-ID: {msgid}\r\n")),
        "the minted id rides a forward too"
    );
    // SETTLED: the fire wrote no verdict of its own.
    assert_eq!(audited(&store, acct, "schedule").len(), guard_rows);
    assert!(
        !audited(&store, acct, "send")
            .iter()
            .any(|d| d.contains("guard")),
        "no re-scan at fire time"
    );
    assert_eq!(
        audited(&store, acct, "send"),
        vec![format!("scheduled:{id}:ok:forward")]
    );
    assert_eq!(
        store.scheduled_send_by_id(acct, id).unwrap().unwrap().state,
        ScheduledSendState::Sent
    );
}

#[tokio::test]
async fn scheduling_asks_in_the_send_routes_order() {
    // Plain mail with a secret and no credential: the guard's 422 before the
    // credential's 403, exactly as a send answers.
    let (Harness { app, .. }, _) = scheduled_rig(None, |_, _| {});
    let (status, _) = outbox(
        &app,
        "POST",
        "/client/scheduled",
        Some(serde_json::json!({
            "to": "bob@example.com",
            "body": "-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----",
            "confirm": true,
            "send_at": in_minutes(60),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // A reply to a parent that is not stored, WITH a credential: 404, and
    // only after everything else passed.
    let (base, _gmail) = mock_gmail(0).await;
    let (Harness { app, store, acct }, _) = scheduled_rig(Some(base), |_, _| {});
    let (status, _) = outbox(
        &app,
        "POST",
        "/client/scheduled",
        Some(serde_json::json!({
            "reply_to_message_id": 987_654,
            "body": "yes",
            "confirm": true,
            "send_at": in_minutes(60),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        audited(&store, acct, "schedule"),
        vec!["failed:target".to_string()]
    );
    // The same reply without a credential is the 403: the parent is asked last.
    let (Harness { app, store, acct }, _) = scheduled_rig(None, |_, _| {});
    let (status, _) = outbox(
        &app,
        "POST",
        "/client/scheduled",
        Some(serde_json::json!({
            "reply_to_message_id": 987_654,
            "body": "yes",
            "confirm": true,
            "send_at": in_minutes(60),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        audited(&store, acct, "schedule"),
        vec!["rejected:no_write_credential".to_string()]
    );
}

#[tokio::test]
async fn every_outbox_response_is_no_store_even_an_extractors_rejection() {
    let (Harness { app, .. }, _) = scheduled_rig(None, |_, _| {});
    for req in [
        Request::builder()
            .method("POST")
            .uri("/client/scheduled")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{not json"))
            .unwrap(),
        authed("DELETE", "/client/scheduled/not-a-number"),
        authed("POST", "/client/scheduled/not-a-number/now"),
    ] {
        let resp = app.clone().oneshot(req).await.unwrap();
        assert!(resp.status().is_client_error(), "{}", resp.status());
        assert_eq!(
            resp.headers()
                .get(header::CACHE_CONTROL)
                .map(|v| v.to_str().unwrap()),
            Some("no-store"),
            "{}",
            resp.status()
        );
    }
}

#[tokio::test]
async fn a_cancelled_request_comes_back_without_its_answers() {
    let (base, _gmail) = mock_gmail(0).await;
    let (Harness { app, .. }, _) = scheduled_rig(Some(base), |_, _| {});
    let (_, created) = outbox(
        &app,
        "POST",
        "/client/scheduled",
        Some(serde_json::json!({
            "to": "bob@example.com",
            "body": "-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----",
            "override_guard": true,
            "confirm": true,
            "send_at": in_minutes(60),
        })),
    )
    .await;
    let id = created["id"].as_i64().unwrap();
    let (status, cancelled) =
        outbox(&app, "DELETE", &format!("/client/scheduled/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    let req = cancelled["request"].as_object().unwrap();
    assert!(!req.contains_key("override_guard"), "the guard asks again");
    assert!(!req.contains_key("confirm"), "the client confirms itself");
    assert_eq!(req["to"], "bob@example.com");
}

#[tokio::test]
async fn the_outbox_is_capped_per_account() {
    let (base, _gmail) = mock_gmail(0).await;
    let (Harness { app, store, acct }, _) = scheduled_rig(Some(base), |_, _| {});
    for i in 0..squelch_core::store::MAX_SCHEDULED_PER_ACCOUNT {
        store
            .insert_scheduled_send(
                acct,
                COLD_REQUEST,
                chrono::Utc::now() + chrono::Duration::hours(1),
                &format!("<cap{i}@passband>"),
                chrono::Utc::now(),
            )
            .unwrap();
    }
    let (status, err) = outbox(
        &app,
        "POST",
        "/client/scheduled",
        Some(serde_json::json!({
            "to": "bob@example.com",
            "body": "one more",
            "confirm": true,
            "send_at": in_minutes(60),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].as_str().unwrap().contains("too many"));
    assert_eq!(
        audited(&store, acct, "schedule"),
        vec!["rejected:too_many_scheduled".to_string()]
    );
}

#[tokio::test]
async fn scheduling_consumes_a_reply_draft_and_a_forward_draft_by_id() {
    let (base, _gmail) = mock_gmail_seq(vec![(200, original_raw_response("<p>numbers</p>"))]).await;
    let (Harness { app, store, acct }, _) = scheduled_rig(Some(base), |store, acct| {
        seed_one_signal(
            store,
            acct,
            "gmail-original",
            "thread-77",
            "Quarterly numbers",
        );
    });
    let parent = store.search(acct, "quarterly", 10, 0).unwrap()[0].id;
    let (status, reply_draft) = draft_call(
        &app,
        "PUT",
        "/client/drafts",
        serde_json::json!({ "reply_to_message_id": parent, "body": "yes" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply_draft}");
    let (status, fwd_draft) = draft_call(
        &app,
        "PUT",
        "/client/drafts",
        serde_json::json!({ "forward_of_message_id": parent, "to": "c@example.com", "body": "fyi" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{fwd_draft}");
    let (reply_id, fwd_id) = (
        reply_draft["id"].as_i64().unwrap(),
        fwd_draft["id"].as_i64().unwrap(),
    );
    assert_ne!(reply_id, fwd_id, "two keyed drafts, one per key");

    let (status, _) = outbox(
        &app,
        "POST",
        "/client/scheduled",
        Some(serde_json::json!({
            "reply_to_message_id": parent,
            "body": "yes",
            "draft_id": reply_id,
            "confirm": true,
            "send_at": in_minutes(60),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(store.draft_by_id(acct, reply_id).unwrap().is_none());
    assert!(
        store.draft_by_id(acct, fwd_id).unwrap().is_some(),
        "only the named draft goes"
    );
    let (status, _) = outbox(
        &app,
        "POST",
        "/client/scheduled",
        Some(serde_json::json!({
            "forward_of_message_id": parent,
            "to": "c@example.com",
            "body": "fyi",
            "draft_id": fwd_id,
            "confirm": true,
            "send_at": in_minutes(60),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(store.draft_by_id(acct, fwd_id).unwrap().is_none());
}

/// A due row whose earlier attempt MAY have gone (`maybe_sent`), its lapsed
/// lease ten minutes behind it so the fire-time grace is long out: the shape
/// recovery leaves after a "not found".
fn maybe_sent_row(store: &SqliteStore, acct: i64, msgid: &str) -> i64 {
    let id = lapsed_row(store, acct, msgid, 10);
    let now = chrono::Utc::now();
    store
        .requeue_scheduled_send(acct, id, 1, now - chrono::Duration::seconds(1), now)
        .unwrap();
    id
}

#[tokio::test]
async fn a_send_now_inside_the_grace_neither_searches_nor_sends() {
    // The ambiguous 500 lands; the user hits "send now" straight away.
    let (base, gmail) = mock_gmail_seq(vec![(500, "{\"error\":\"backend\"}".to_string())]).await;
    let (Harness { store, acct, .. }, state) = scheduled_rig(Some(base), |_, _| {});
    let id = due_row(&store, acct, COLD_REQUEST, "<hasty@passband>");
    squelch_api::run_scheduled_send_pass(&state).await;
    let ended = store
        .scheduled_send_by_id(acct, id)
        .unwrap()
        .unwrap()
        .last_attempt_at
        .expect("the attempt's end is stamped");

    assert!(
        store
            .send_scheduled_now(acct, id, chrono::Utc::now())
            .unwrap()
    );
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(
        (report.deferred, report.sent, report.recovered_sent),
        (1, 0, 0),
        "{report:?}"
    );
    let reqs = gmail.await.unwrap();
    assert_eq!(reqs.len(), 1, "the one send; no search, no second send");
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(row.state, ScheduledSendState::Scheduled);
    assert_eq!(row.attempts, 1, "the held-back claim spent nothing");
    assert_eq!(row.last_error.as_deref(), Some("recovery:grace"));
    assert_eq!(
        row.send_at,
        ended + chrono::Duration::minutes(5),
        "it comes back exactly when the grace is out"
    );
    assert!(row.maybe_sent);
}

#[tokio::test]
async fn a_row_that_failed_on_an_ambiguous_last_attempt_is_searched_before_send_now() {
    let (base, gmail) = mock_gmail_seq(vec![
        // Its third and last attempt: Gmail took it and answered 500.
        (500, "{\"error\":\"backend\"}".to_string()),
        // Send-now, past the grace: asked first, and it was there.
        (
            200,
            "{\"messages\":[{\"id\":\"gm-3\",\"threadId\":\"thread-new\"}]}".to_string(),
        ),
        (200, echo_response("gm-3")),
    ])
    .await;
    let (Harness { store, acct, .. }, state) = scheduled_rig(Some(base), |_, _| {});
    let id = due_row(&store, acct, COLD_REQUEST, "<last@passband>");
    // Two definitely-not-sent attempts before it (a 4xx each).
    for attempts in 1..3 {
        let now = chrono::Utc::now();
        store
            .claim_due_scheduled_send(acct, now, chrono::Duration::seconds(120))
            .unwrap()
            .unwrap();
        store
            .settle_scheduled_retry(
                acct,
                id,
                attempts,
                now - chrono::Duration::seconds(1),
                "failed:gmail",
                false,
                now,
            )
            .unwrap();
    }
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(report.failed, 1, "{report:?}");
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(
        (row.state, row.attempts, row.last_error.as_deref()),
        (ScheduledSendState::Failed, 3, Some("failed:gmail"))
    );
    assert!(row.maybe_sent, "the last attempt may have gone");

    assert!(
        store
            .send_scheduled_now(acct, id, chrono::Utc::now())
            .unwrap()
    );
    let later = state
        .clone()
        .with_worker_clock_skew(chrono::Duration::minutes(6));
    let report = squelch_api::run_scheduled_send_pass(&later).await;
    assert_eq!((report.recovered_sent, report.sent), (1, 0), "{report:?}");
    let reqs = gmail.await.unwrap();
    assert!(reqs[0].contains("/messages/send"));
    assert!(
        reqs[1].contains("rfc822msgid%3Alast%40passband"),
        "send-now's FIRST request is the search"
    );
    assert_eq!(
        reqs.iter().filter(|r| r.contains("/messages/send")).count(),
        1,
        "EXACTLY ONE send"
    );
    assert_eq!(
        store.scheduled_send_by_id(acct, id).unwrap().unwrap().state,
        ScheduledSendState::Sent
    );
}

#[tokio::test]
async fn a_row_recovery_gave_up_on_is_searched_before_send_now() {
    let (Harness { store, acct, .. }, state) = scheduled_rig(None, |_, _| {});
    let id = lapsed_row(&store, acct, "<gaveup@passband>", 31);
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(report.failed, 1, "{report:?}");
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(row.last_error.as_deref(), Some("failed:recovery"));
    assert!(
        row.maybe_sent,
        "recovery never got to ask: it may have gone"
    );
    assert_eq!(
        audited(&store, acct, "send"),
        vec![format!("scheduled:{id}:failed:recovery")]
    );

    // The credential comes back; the user hits send now.
    let (base, gmail) = mock_gmail_seq(vec![
        (
            200,
            "{\"messages\":[{\"id\":\"gm-r\",\"threadId\":\"thread-new\"}]}".to_string(),
        ),
        (200, echo_response("gm-r")),
    ])
    .await;
    let state = state
        .with_write_test_harness(Arc::new(StubCreds), base)
        .with_worker_clock_skew(chrono::Duration::minutes(6));
    assert!(
        store
            .send_scheduled_now(acct, id, chrono::Utc::now())
            .unwrap()
    );
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!((report.recovered_sent, report.sent), (1, 0), "{report:?}");
    let reqs = gmail.await.unwrap();
    assert!(
        reqs[0].contains("rfc822msgid"),
        "the first request is the search"
    );
    assert!(!reqs.iter().any(|r| r.contains("/messages/send")));
}

#[tokio::test]
async fn a_search_the_credential_refuses_spends_attempts_and_fails() {
    // A revoked write grant surfaces only when a token is fetched, which the
    // pre-fire search is the first thing to do: an attempt, not a "not now".
    let (state, store, acct) = state_with(|_, _| {});
    let state = state.with_write_test_harness(Arc::new(DeadCreds), "http://127.0.0.1:1".into());
    let id = maybe_sent_row(&store, acct, "<revoked@passband>");

    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!((report.retried, report.deferred), (1, 0), "{report:?}");
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(
        (row.state, row.attempts, row.last_error.as_deref()),
        (
            ScheduledSendState::Scheduled,
            2,
            Some("rejected:no_write_credential")
        )
    );
    // The third attempt, past the grace its retry waits out, is the last.
    let later = state.with_worker_clock_skew(chrono::Duration::minutes(6));
    let report = squelch_api::run_scheduled_send_pass(&later).await;
    assert_eq!(report.failed, 1, "{report:?}");
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(
        (row.state, row.attempts, row.last_error.as_deref()),
        (
            ScheduledSendState::Failed,
            3,
            Some("rejected:no_write_credential")
        )
    );
    assert!(row.maybe_sent, "still owed a search");
    assert_eq!(
        audited(&store, acct, "send"),
        vec![format!("scheduled:{id}:rejected:no_write_credential"); 2]
    );
}

#[tokio::test]
async fn a_search_that_can_never_be_made_gives_up_instead_of_deferring_forever() {
    // Nothing listens: every search is a transport error, "not now".
    let (state, store, acct) = state_with(|_, _| {});
    let state = state.with_write_test_harness(Arc::new(StubCreds), "http://127.0.0.1:1".into());
    let id = maybe_sent_row(&store, acct, "<offline@passband>");

    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(report.deferred, 1, "{report:?}");
    let first = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(first.attempts, 1, "no attempt spent");
    assert_eq!(first.last_error.as_deref(), Some("recovery:unreachable"));
    let since = first.search_deferred_since.expect("the clock started");

    // Ten minutes on, still deferring, and the clock keeps its start.
    let report = squelch_api::run_scheduled_send_pass(
        &state
            .clone()
            .with_worker_clock_skew(chrono::Duration::minutes(10)),
    )
    .await;
    assert_eq!(report.deferred, 1, "{report:?}");
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(row.search_deferred_since, Some(since));

    // Past the give-up: the user decides.
    let report = squelch_api::run_scheduled_send_pass(
        &state.with_worker_clock_skew(chrono::Duration::minutes(31)),
    )
    .await;
    assert_eq!(report.failed, 1, "{report:?}");
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(
        (row.state, row.last_error.as_deref(), row.maybe_sent),
        (ScheduledSendState::Failed, Some("failed:recovery"), true)
    );
    assert_eq!(
        audited(&store, acct, "send"),
        vec![format!("scheduled:{id}:failed:recovery")]
    );
}

#[tokio::test]
async fn a_re_fire_with_no_credential_is_audited_as_the_retry_it_is() {
    let (Harness { store, acct, .. }, state) = scheduled_rig(None, |_, _| {});
    let id = maybe_sent_row(&store, acct, "<nokey@passband>");
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(report.retried, 1, "{report:?}");
    assert_eq!(
        audited(&store, acct, "send"),
        vec![format!("scheduled:{id}:rejected:no_write_credential")]
    );
}

#[tokio::test]
async fn a_paced_read_of_a_forwards_original_hands_the_attempt_back() {
    use squelch_core::config::GmailQuotaProfile;
    use squelch_core::gmail_budget::GmailBudget;
    let (base, _gmail) = mock_gmail(0).await;
    let budget = Arc::new(
        GmailBudget::with_unit(GmailQuotaProfile::Standard, std::time::Duration::ZERO)
            .with_adaptation(
                std::time::Duration::from_secs(600),
                std::time::Duration::from_secs(600),
            ),
    );
    budget.note_quota_refusal(None);
    let (Harness { store, acct, .. }, state) = scheduled_rig(Some(base), |store, acct| {
        seed_one_signal(store, acct, "gmail-original", "thread-77", "Quarterly");
    });
    let state = state.with_gmail_budget(budget);
    let original = store.search(acct, "quarterly", 10, 0).unwrap()[0].id;
    let request = format!(
        r#"{{"forward_of_message_id":{original},"to":"c@example.com","body":"fyi","confirm":true}}"#
    );
    let id = due_row(&store, acct, &request, "<fwdpaced@passband>");

    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!((report.deferred, report.retried), (1, 0), "{report:?}");
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(
        (row.state, row.attempts, row.last_error.as_deref()),
        (ScheduledSendState::Scheduled, 0, Some("paced")),
        "not `failed:fetch_original`, and no attempt spent"
    );
    assert_eq!(
        audited(&store, acct, "send"),
        vec![format!("scheduled:{id}:paced")]
    );
}

#[tokio::test]
async fn a_fired_forward_whose_original_is_gone_is_marked_in_the_ledger() {
    let (base, _gmail) = mock_gmail_seq(vec![(404, "{}".to_string())]).await;
    let (Harness { store, acct, .. }, state) = scheduled_rig(Some(base), |store, acct| {
        seed_one_signal(store, acct, "gmail-original", "thread-77", "Quarterly");
    });
    let original = store.search(acct, "quarterly", 10, 0).unwrap()[0].id;
    let request = format!(
        r#"{{"forward_of_message_id":{original},"to":"c@example.com","body":"fyi","confirm":true}}"#
    );
    let id = due_row(&store, acct, &request, "<fwdgone@passband>");
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(report.retried, 1, "{report:?}");
    assert_eq!(
        audited(&store, acct, "send"),
        vec![format!("scheduled:{id}:failed:fetch_original")]
    );
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(row.last_error.as_deref(), Some("failed:fetch_original"));
    assert!(!row.maybe_sent, "nothing was posted");
}

#[tokio::test]
async fn a_fired_group_send_records_who_it_went_to_not_who_is_in_the_group_now() {
    let (base, _gmail) = mock_gmail_seq(vec![
        (
            200,
            "{\"id\":\"sent-m\",\"threadId\":\"thread-new\"}".to_string(),
        ),
        (200, echo_response("sent-m")),
    ])
    .await;
    let (Harness { app, store, acct }, state) = scheduled_rig(Some(base), |_, _| {});
    let member = |addr: &str| squelch_core::store::sqlite::groups::NewGroupMember {
        addr: addr.into(),
        display_name: None,
    };
    let group = store
        .create_send_group(
            acct,
            "Fund",
            squelch_core::types::GroupMode::Bcc,
            "",
            &[member("ann@fund.com"), member("bo@fund.com")],
        )
        .unwrap();
    let (status, created) = outbox(
        &app,
        "POST",
        "/client/scheduled",
        Some(serde_json::json!({
            "group_id": group,
            // A non-member typed in `to`: a direct recipient, but not in the
            // field the Bcc group expanded into, so not recorded as reached.
            "to": "boss@work.com",
            "bcc": "ann@fund.com, bo@fund.com",
            "subject": "Update",
            "body": "hi all",
            "confirm": true,
            "send_at": in_minutes(60),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_i64().unwrap();

    // Since: bo left, carol and dan joined. Three members now, two recipients.
    store
        .update_send_group(
            acct,
            group,
            "Fund",
            squelch_core::types::GroupMode::Bcc,
            "",
            &[
                member("ann@fund.com"),
                member("carol@fund.com"),
                member("dan@fund.com"),
            ],
        )
        .unwrap();
    store
        .send_scheduled_now(acct, id, chrono::Utc::now())
        .unwrap();
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(report.sent, 1, "{report:?}");

    let history = store.group_history(acct, group, 10, 0).unwrap();
    let recorded: Vec<_> = history
        .iter()
        .filter(|h| h.group_send_id.is_some())
        .collect();
    assert_eq!(recorded.len(), 1, "{history:?}");
    assert_eq!(
        (recorded[0].reached, recorded[0].group_size),
        (2, 2),
        "ann and bo got it in bcc; carol and dan never did; the boss in to is not the group's"
    );
}

#[tokio::test]
async fn a_fired_group_send_records_the_mode_it_was_scheduled_under() {
    let (base, _gmail) = mock_gmail_seq(vec![
        (
            200,
            "{\"id\":\"sent-p\",\"threadId\":\"thread-new\"}".to_string(),
        ),
        (200, echo_response("sent-p")),
        (
            200,
            "{\"id\":\"sent-q\",\"threadId\":\"thread-new-2\"}".to_string(),
        ),
        (200, echo_response("sent-q")),
        (
            200,
            "{\"id\":\"sent-r\",\"threadId\":\"thread-new-3\"}".to_string(),
        ),
        (200, echo_response("sent-r")),
    ])
    .await;
    let (Harness { app, store, acct }, state) = scheduled_rig(Some(base), |_, _| {});
    use squelch_core::types::GroupMode;
    let member = |addr: &str| squelch_core::store::sqlite::groups::NewGroupMember {
        addr: addr.into(),
        display_name: None,
    };
    let members = [member("ann@fund.com"), member("bo@fund.com")];
    let blind = store
        .create_send_group(acct, "Blind", GroupMode::Bcc, "", &members)
        .unwrap();
    let open = store
        .create_send_group(acct, "Open", GroupMode::To, "", &members)
        .unwrap();
    let schedule = |body: Value| {
        let app = app.clone();
        async move {
            let (status, created) = outbox(&app, "POST", "/client/scheduled", Some(body)).await;
            assert_eq!(status, StatusCode::CREATED, "{created}");
            created["id"].as_i64().unwrap()
        }
    };
    // A client cannot stamp the mode: the route overwrites it.
    let a = schedule(serde_json::json!({
        "group_id": blind,
        "group_mode": "to",
        "bcc": "ann@fund.com, bo@fund.com",
        "subject": "Update",
        "body": "hi all",
        "confirm": true,
        "send_at": in_minutes(60),
    }))
    .await;
    let b = schedule(serde_json::json!({
        "group_id": open,
        "to": "ann@fund.com, bo@fund.com",
        "subject": "Update",
        "body": "hi all",
        "confirm": true,
        "send_at": in_minutes(60),
    }))
    .await;
    let frozen = |id: i64| -> Value {
        serde_json::from_str(
            &store
                .scheduled_send_by_id(acct, id)
                .unwrap()
                .unwrap()
                .request,
        )
        .unwrap()
    };
    assert_eq!(frozen(a)["group_mode"], "bcc");
    assert_eq!(frozen(b)["group_mode"], "to");

    // Between scheduling and firing, each group flips to the other mode.
    store
        .update_send_group(acct, blind, "Blind", GroupMode::To, "", &members)
        .unwrap();
    store
        .update_send_group(acct, open, "Open", GroupMode::Bcc, "", &members)
        .unwrap();

    // A row scheduled before `group_mode` existed, whose group reads `to`
    // now while its frozen request addressed `bcc`: the field the current
    // mode names is EMPTY, so nothing is recorded, rather than a row of
    // zero recipients.
    let legacy = due_row(
        &store,
        acct,
        &format!(
            r#"{{"group_id":{blind},"bcc":"ann@fund.com, bo@fund.com","subject":"Old","body":"hi","confirm":true}}"#
        ),
        "<legacy-group@passband.local>",
    );

    for id in [a, b] {
        store
            .send_scheduled_now(acct, id, chrono::Utc::now())
            .unwrap();
    }
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(report.sent, 3, "{report:?}");
    assert_eq!(
        store
            .scheduled_send_by_id(acct, legacy)
            .unwrap()
            .unwrap()
            .state,
        ScheduledSendState::Sent
    );

    let recorded = |group: i64| {
        store
            .group_history(acct, group, 10, 0)
            .unwrap()
            .into_iter()
            .filter(|h| h.group_send_id.is_some())
            .map(|h| (h.mode, h.reached, h.group_size))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        recorded(blind),
        vec![(GroupMode::Bcc, 2, 2)],
        "scheduled as Bcc: recorded as Bcc with its frozen bcc list; the legacy row wrote nothing"
    );
    assert_eq!(
        recorded(open),
        vec![(GroupMode::To, 2, 2)],
        "scheduled as To: recorded as To with its frozen to list"
    );
}

#[tokio::test]
async fn a_search_gmail_answers_403_is_not_now_not_an_attempt() {
    // Gmail answers a per-user rate limit with a 403 as often as a permission
    // refusal: "not now", bounded by the give-up, never an attempt spent.
    let (base, gmail) = mock_gmail_seq(vec![(
        403,
        r#"{"error":{"errors":[{"reason":"userRateLimitExceeded"}],"code":403}}"#.to_string(),
    )])
    .await;
    let (Harness { store, acct, .. }, state) = scheduled_rig(Some(base), |_, _| {});
    let id = maybe_sent_row(&store, acct, "<limited@passband>");

    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(
        (report.deferred, report.retried, report.failed),
        (1, 0, 0),
        "{report:?}"
    );
    let reqs = gmail.await.unwrap();
    assert_eq!(reqs.len(), 1);
    assert!(reqs[0].contains("rfc822msgid"), "the search, and no send");
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(
        (row.state, row.attempts, row.last_error.as_deref()),
        (
            ScheduledSendState::Scheduled,
            1,
            Some("recovery:unreachable")
        ),
        "the lapsed attempt counted; the search spent none"
    );
    assert!(
        row.search_deferred_since.is_some(),
        "the give-up clock runs"
    );
    assert!(row.maybe_sent);
    assert!(
        audited(&store, acct, "send").is_empty(),
        "a deferral is not in the ledger"
    );
}

#[tokio::test]
async fn a_send_now_right_after_a_recovery_give_up_owes_no_fresh_grace() {
    // Recovery never got to ask (no credential), and gave up half an hour
    // past the lease. The attempt ENDED then, not at the give-up: a send-now
    // straight after the toast searches and sends on the next pass.
    let (Harness { store, acct, .. }, state) = scheduled_rig(None, |_, _| {});
    let id = lapsed_row(&store, acct, "<prompt@passband>", 31);
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(report.failed, 1, "{report:?}");
    let row = store.scheduled_send_by_id(acct, id).unwrap().unwrap();
    assert_eq!(row.last_error.as_deref(), Some("failed:recovery"));
    assert!(
        row.last_attempt_at.expect("stamped") <= chrono::Utc::now() - chrono::Duration::minutes(30),
        "stamped at the lapsed lease, not the give-up"
    );

    let (base, gmail) = mock_gmail_seq(vec![
        (200, "{\"resultSizeEstimate\":0}".to_string()),
        (200, "{\"resultSizeEstimate\":0}".to_string()),
        (
            200,
            "{\"id\":\"sent-p\",\"threadId\":\"thread-new\"}".to_string(),
        ),
        (200, echo_response("sent-p")),
    ])
    .await;
    // NO clock skew: the very next pass.
    let state = state.with_write_test_harness(Arc::new(StubCreds), base);
    assert!(
        store
            .send_scheduled_now(acct, id, chrono::Utc::now())
            .unwrap()
    );
    let report = squelch_api::run_scheduled_send_pass(&state).await;
    assert_eq!(
        (report.sent, report.deferred),
        (1, 0),
        "no grace hold: {report:?}"
    );
    let reqs = gmail.await.unwrap();
    assert!(reqs[0].contains("rfc822msgid"), "searched first");
    assert!(reqs[1].contains("q=in%3Asent"), "then scanned");
    assert!(reqs[2].contains("/messages/send"));
    assert_eq!(
        store.scheduled_send_by_id(acct, id).unwrap().unwrap().state,
        ScheduledSendState::Sent
    );
}

/// A write-enabled app whose mailbox budget is in a ten-minute quota hold,
/// with one stored signal message to reply to or forward.
fn paced_send_app() -> (axum::Router, Arc<SqliteStore>, i64, i64) {
    use squelch_core::config::GmailQuotaProfile;
    use squelch_core::gmail_budget::GmailBudget;
    let budget = Arc::new(
        GmailBudget::with_unit(GmailQuotaProfile::Standard, std::time::Duration::ZERO)
            .with_adaptation(
                std::time::Duration::from_secs(600),
                std::time::Duration::from_secs(600),
            ),
    );
    budget.note_quota_refusal(None);
    // A parent with NO STORED FROM, so the paced read's own arm is the only
    // way to a 503 here: had the read degraded to an unthreaded reply
    // instead, the stored-From fallback would have nobody to address, and
    // the send would be refused as unaddressable before it ever asked the
    // budget again. (A forward reads its original the same way and never
    // gets as far as an address.)
    let mut parent = 0;
    let (state, store, acct) = state_with(|store, acct| {
        parent = store
            .upsert_message(&squelch_core::types::NewMessage {
                from_addr: String::new(),
                from_name: None,
                ..msg(acct, "gmail-parent", "thread-77", "Quarterly", "body")
            })
            .unwrap();
    });
    // Nothing listens: a request that got past the budget would fail loudly
    // as a transport error, never as the 503 asserted.
    let app = router(
        state
            .with_write_test_harness(Arc::new(StubCreds), "http://127.0.0.1:1".into())
            .with_gmail_budget(budget),
    );
    (app, store, acct, parent)
}

#[tokio::test]
async fn a_live_reply_whose_parent_read_is_paced_is_503_and_audited_paced() {
    // Plain reply and reply-all alike: a paced parent read is "not now", not
    // an unthreaded reply with no derived audience, and not a Gmail failure.
    // The two legs cannot pass by accident: the parent has no stored From
    // (see `paced_send_app`), so a plain reply that fell through to the
    // unthreaded path is a 400 (nobody to address), and a reply-all that
    // fell through is a 502 (`failed:gmail`); only the Paced arm is a 503.
    for reply_all in [false, true] {
        let (app, store, acct, parent) = paced_send_app();
        let resp = app
            .oneshot(authed_json(
                "POST",
                "/client/actions/send",
                serde_json::json!({
                    "reply_to_message_id": parent,
                    "reply_all": reply_all,
                    "body": "sounds good",
                    "confirm": true,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "reply_all={reply_all}"
        );
        assert_eq!(
            audited(&store, acct, "send"),
            vec!["paced".to_string()],
            "reply_all={reply_all}"
        );
    }
}

#[tokio::test]
async fn a_live_forward_whose_original_read_is_paced_is_503_and_audited_paced() {
    let (app, store, acct, original) = paced_send_app();
    let resp = app
        .oneshot(authed_json(
            "POST",
            "/client/actions/send",
            serde_json::json!({
                "forward_of_message_id": original,
                "to": "c@example.com",
                "body": "fyi",
                "confirm": true,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        audited(&store, acct, "send"),
        vec!["paced".to_string()],
        "not `failed:fetch_original`"
    );
}

/// Responses gzip when the caller asks and stay plain when it does not (the
/// Swift client's `URLSession` asks on every request), the gzipped body IS
/// the plain one, the console's HTML gzips too (one layer over the whole
/// door), and two kinds of response are never compressed whatever the caller
/// asks: the events stream, because an SSE frame has to leave the process
/// the moment it is written and a gzip encoder would hold it, and an
/// attachment, already-compressed bytes served as `application/pdf` or
/// `application/octet-stream` that would cost a worker thread for nothing.
#[tokio::test]
async fn json_gzips_when_asked_and_the_event_stream_never_does() {
    use std::io::Read;

    let id = std::sync::Arc::new(std::sync::Mutex::new(0i64));
    let id_seed = id.clone();
    let Harness { app, .. } = harness(move |store, acct| {
        let m = store
            .upsert_message(&msg(acct, "g1", "t1", "s", "b"))
            .unwrap();
        store
            .set_triage(
                m,
                acct,
                60,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
        // Well over the 32-byte floor, so only the content type can be what
        // keeps it plain.
        let pdf = vec![b'%'; 4096];
        *id_seed.lock().unwrap() = store
            .insert_attachment(
                acct,
                m,
                "a.pdf",
                "application/pdf",
                pdf.len() as i64,
                Some(&pdf),
            )
            .unwrap();
    });
    let attachment_id = *id.lock().unwrap();
    let gzip_get = |uri: &str| {
        let mut req = authed("GET", uri);
        req.headers_mut()
            .insert(header::ACCEPT_ENCODING, "gzip".parse().unwrap());
        req
    };
    let encoding = |resp: &axum::response::Response| {
        resp.headers()
            .get(header::CONTENT_ENCODING)
            .map(|v| v.to_str().unwrap().to_string())
    };

    let plain = app
        .clone()
        .oneshot(authed("GET", "/client/stats"))
        .await
        .unwrap();
    assert_eq!(plain.status(), StatusCode::OK);
    assert_eq!(encoding(&plain), None, "no Accept-Encoding, no encoding");
    let plain: Value = body_json(plain).await;

    let resp = app
        .clone()
        .oneshot(gzip_get("/client/stats"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        encoding(&resp).as_deref(),
        Some("gzip"),
        "a JSON body is gzipped for a caller that asks"
    );
    let compressed = body_bytes(resp).await;
    let mut inflated = Vec::new();
    flate2::read::GzDecoder::new(compressed.as_slice())
        .read_to_end(&mut inflated)
        .expect("the body is a gzip stream");
    let inflated: Value = serde_json::from_slice(&inflated).unwrap();
    assert_eq!(inflated, plain, "the gzipped body is the plain one");

    // The console's server-rendered HTML sits under the same layer; its
    // login page needs no session.
    let resp = app.clone().oneshot(gzip_get("/console")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .map(|v| v.to_str().unwrap())
            .is_some_and(|ct| ct.starts_with("text/html")),
        "the console serves HTML"
    );
    assert_eq!(
        encoding(&resp).as_deref(),
        Some("gzip"),
        "console HTML gzips"
    );

    let resp = app
        .clone()
        .oneshot(gzip_get("/client/events"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .map(|v| v.to_str().unwrap()),
        Some("text/event-stream")
    );
    assert_eq!(
        encoding(&resp),
        None,
        "the event stream must never be compressed"
    );

    let resp = app
        .oneshot(gzip_get(&format!("/client/attachments/{attachment_id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .map(|v| v.to_str().unwrap()),
        Some("application/pdf")
    );
    assert_eq!(encoding(&resp), None, "an attachment is never compressed");
    assert_eq!(
        body_bytes(resp).await.len(),
        4096,
        "and arrives byte for byte"
    );
}

// --- GET /client/thread/{id} buys nothing; POST .../translate buys it -------

#[tokio::test]
async fn reading_a_thread_never_promotes_and_the_translate_post_does() {
    let (state, store, acct) = common::state_with(|store, acct| {
        let id = store
            .upsert_message(&msg(acct, "g-fr", "t-fr", "Votre facture", "Bonjour"))
            .unwrap();
        store
            .set_triage(
                id,
                acct,
                40,
                Tier::Signal,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
        // An archive row: parked until somebody displays the thread.
        store
            .queue_translation(acct, id, "fr", true, chrono::Utc::now())
            .unwrap();
        // And an English neighbour, which carries no translation at all.
        store
            .upsert_message(&msg(acct, "g-en", "t-fr", "Re: Votre facture", "Thanks"))
            .unwrap();
    });
    let poke = squelch_core::store::archive_surface::RefinePoke::new();
    let app = router(state.with_refine_poke(poke.clone()));
    let status_of = |store: &squelch_core::store::SqliteStore, id: i64| {
        store.message_translations(acct, &[id]).unwrap()[0].status
    };

    // THE GET IS A READ, and the client prefetches through it: the row is
    // served as `deferred` (a translation is available) and stays parked.
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/thread/t-fr"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let msgs = json["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["translation"]["status"], "deferred");
    assert_eq!(msgs[0]["translation"]["source_lang"], "fr");
    assert_eq!(msgs[0]["translation"]["source_language"], "French");
    assert_eq!(msgs[0]["translation"]["content"], "");
    assert_eq!(msgs[0]["translation"]["subject"], "");
    assert!(
        msgs[1].get("translation").is_none(),
        "English mail has no key"
    );
    let id = msgs[0]["id"].as_i64().unwrap();
    assert_eq!(
        status_of(&store, id),
        squelch_core::store::TranslationStatus::Deferred,
        "reading spent nothing"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), poke.poked())
            .await
            .is_err(),
        "a read does not wake the lane"
    );

    // THE POST is the reader displaying it: promoted, and the lane poked.
    let resp = app
        .clone()
        .oneshot(authed("POST", "/client/thread/t-fr/translate"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["promoted"], 1);
    tokio::time::timeout(std::time::Duration::from_millis(100), poke.poked())
        .await
        .expect("the refine lane was poked");
    assert_eq!(
        status_of(&store, id),
        squelch_core::store::TranslationStatus::Pending
    );

    // Now the GET says translating.
    let json = body_json(
        app.clone()
            .oneshot(authed("GET", "/client/thread/t-fr"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(json["messages"][0]["translation"]["status"], "pending");

    // IDEMPOTENT: a second POST moves nothing and pokes nothing.
    let resp = app
        .clone()
        .oneshot(authed("POST", "/client/thread/t-fr/translate"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["promoted"], 0);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), poke.poked())
            .await
            .is_err(),
        "nothing moved, nothing poked"
    );

    // An unknown thread is a no-op, not an error.
    let resp = app
        .oneshot(authed("POST", "/client/thread/t-nope/translate"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["promoted"], 0);
}

#[tokio::test]
async fn without_a_translator_nothing_promises_a_translation() {
    // Translation off or no LLM: a queued row would never finish, so the
    // reader is not told "Translating...", and the POST promotes nothing. A
    // translation that already landed is still served.
    let (state, store, acct) = common::state_with(|store, acct| {
        let parked = store
            .upsert_message(&msg(acct, "g-fr", "t-fr", "Votre facture", "Bonjour"))
            .unwrap();
        store
            .queue_translation(acct, parked, "fr", true, chrono::Utc::now())
            .unwrap();
        let queued = store
            .upsert_message(&msg(acct, "g-fr2", "t-fr", "Re: Votre facture", "Merci"))
            .unwrap();
        store
            .queue_translation(acct, queued, "fr", false, chrono::Utc::now())
            .unwrap();
    });
    let poke = squelch_core::store::archive_surface::RefinePoke::new();
    let app = router(
        state
            .with_refine_poke(poke.clone())
            .with_translate_active(false),
    );
    let json = body_json(
        app.clone()
            .oneshot(authed("GET", "/client/thread/t-fr"))
            .await
            .unwrap(),
    )
    .await;
    let ids: Vec<i64> = json["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            assert!(m.get("translation").is_none(), "{m}");
            m["id"].as_i64().unwrap()
        })
        .collect();
    assert_eq!(ids.len(), 2);
    let resp = app
        .oneshot(authed("POST", "/client/thread/t-fr/translate"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["promoted"], 0);
    let rows = store
        .message_translations(acct, &ids)
        .unwrap()
        .into_iter()
        .map(|t| t.status)
        .collect::<Vec<_>>();
    assert!(rows.contains(&squelch_core::store::TranslationStatus::Deferred));
}

#[tokio::test]
async fn the_translate_post_needs_the_human_door_token() {
    let (state, _store, _acct) = common::state_with(|_, _| {});
    let resp = router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/client/thread/t-fr/translate")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
