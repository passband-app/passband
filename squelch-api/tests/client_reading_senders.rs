//! Contract tests for reading attention (`POST
//! /client/v2/messages/{id}/attention`) and the per-sender purge read (`GET
//! /client/v2/reading/senders`).
mod common;
use axum::http::StatusCode;
use chrono::{Duration, Utc};
use common::*;
use serde_json::{Value, json};
use squelch_core::store::{
    ContactEntry, SqliteStore, Store,
    agent_triage::{AgentCommitOutcome, AgentTriageStore},
};
use squelch_core::triage::decision::*;
use squelch_core::types::{AttentionStatus, NewMessage, SealedKind, Sensitivity, Tier};
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

async fn senders(h: &Harness) -> Value {
    let response = h
        .app
        .clone()
        .oneshot(authed("GET", "/client/v2/reading/senders"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    body_json(response).await
}

fn sender<'a>(body: &'a Value, addr: &str) -> &'a Value {
    body["senders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["sender"] == addr)
        .unwrap_or_else(|| panic!("{addr} missing from {body:#}"))
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
    let signals = sender(&senders(&h).await, NEWS)["signals"].clone();
    assert_eq!(signals["dwell_sample"], 1);
    assert_eq!(signals["median_dwell_ms"], 4000);
    assert_eq!(signals["clicks"], 3);

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
    let signals = sender(&senders(&h).await, NEWS)["signals"].clone();
    assert_eq!(signals["median_dwell_ms"], 604_000);
    assert_eq!(signals["clicks"], 23);

    // And the total stops at an hour and a thousand clicks.
    for _ in 0..60 {
        post(&h, &uri, json!({"dwell_ms": 600_000, "link_clicks": 20})).await;
    }
    let signals = sender(&senders(&h).await, NEWS)["signals"].clone();
    assert_eq!(signals["median_dwell_ms"], 3_600_000);
    assert_eq!(signals["clicks"], 1000);
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
    let theirs_now = h
        .store
        .reading_senders(
            other,
            Utc::now(),
            &squelch_core::store::agent_triage::AgentListQuery::default(),
            200,
        )
        .unwrap();
    assert_eq!(theirs_now.senders[0].signals.opened, 1);
    assert_eq!(theirs_now.senders[0].signals.dwell_sample, 0);
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
    let signals = sender(&senders(&h).await, NEWS)["signals"].clone();
    assert_eq!(signals["opened"], 1, "the unsealed row is counted now");
    assert_eq!(signals["dwell_sample"], 0, "and carries no report");
    assert_eq!(signals["clicks"], 0);

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
    let response = h
        .app
        .clone()
        .oneshot(json_request(
            "GET",
            "/client/v2/reading/senders",
            &json!({}),
            false,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn senders_lists_each_open_reading_sender_with_its_evidence() {
    let h = harness(|_, _| {});
    // NEWS: eight issues, two open in Reading, one opened and skimmed, one
    // cleared without opening.
    let mut ids = Vec::new();
    for i in 0..8 {
        ids.push(seed(
            &h.store,
            &from(h.acct, &format!("n{i}"), NEWS, i + 1),
            false,
        ));
    }
    into_reading(&h.store, h.acct, ids[0]);
    into_reading(&h.store, h.acct, ids[1]);
    h.store
        .acknowledge_agent_message(h.acct, ids[2], Utc::now())
        .unwrap();
    h.store
        .record_message_attention(h.acct, ids[2], 2_000, 0)
        .unwrap();
    for id in [ids[3], ids[4]] {
        h.store
            .set_attention_status(h.acct, id, AttentionStatus::Done)
            .unwrap();
    }
    // Mail that must not be counted: a login code from the same address, and
    // a copy the owner sent (own mail is authorship, and is nobody's signal).
    seed(&h.store, &from(h.acct, "code", NEWS, 1), true);
    seed(
        &h.store,
        &NewMessage {
            is_sent: true,
            ..from(h.acct, "mine", NEWS, 1)
        },
        false,
    );
    // A sender whose only Reading mail is done is not in the list at all.
    let gone = seed(
        &h.store,
        &from(h.acct, "gone", "gone@done.example", 1),
        false,
    );
    into_reading(&h.store, h.acct, gone);
    h.store
        .set_attention_status(h.acct, gone, AttentionStatus::Done)
        .unwrap();
    let rule = h
        .store
        .set_sender_rule(
            h.acct,
            "*@letters.example",
            "only the monthly digest",
            squelch_core::types::Disposition::Filtered,
        )
        .unwrap();

    let body = senders(&h).await;
    assert_eq!(body["window_days"], 90);
    assert_eq!(body["ledger_days"], 0, "an account made a moment ago");
    assert_eq!(body["total_senders"], 1);
    assert_eq!(body["senders"].as_array().unwrap().len(), 1);
    let s = sender(&body, NEWS);
    assert_eq!(s["display_name"], "The Letter");
    assert_eq!(s["open_count"], 2);
    assert_eq!(s["open_message_ids"], json!([ids[0], ids[1]]));
    assert_eq!(s["known_contact"], false);
    assert_eq!(
        s["rule"],
        json!({"id": rule, "disposition": "filtered", "match_pattern": "*@letters.example"})
    );
    assert_eq!(s["unsubscribe"], Value::Null);
    assert_eq!(s["unsubscribed"], Value::Null);
    // Every message here is older than the account, so the open ledger has
    // seen none of it: no history, no score, and one reason saying so.
    assert_eq!(
        s["signals"],
        json!({
            "received": 0, "opened": 0, "cleared_unopened": 0,
            "dwell_sample": 0, "median_dwell_ms": null, "clicks": 0,
            "read_sample": 0, "median_read_fraction": null, "median_words": null,
            "per_month": 0.0
        })
    );
    assert_eq!(s["score"], Value::Null);
    assert_eq!(s["reasons"], json!([{"code": "not_enough_history"}]));

    // With a ledger that has been running all along, the same mail is counted.
    let since = (Utc::now() - Duration::days(400)).to_rfc3339();
    for key in ["opened_ledger_since", "dwell_ledger_since"] {
        h.store.set_app_setting(h.acct, key, &since).unwrap();
    }
    let body = senders(&h).await;
    assert_eq!(body["ledger_days"], 90);
    let s = sender(&body, NEWS);
    assert_eq!(
        s["signals"],
        json!({
            "received": 8, "opened": 1, "cleared_unopened": 2,
            "dwell_sample": 1, "median_dwell_ms": 2000, "clicks": 0,
            "read_sample": 0, "median_read_fraction": null, "median_words": null,
            "per_month": 2.7
        })
    );
    // 55 * 7/8 for the ignored share, 15 * 2/8 for the cleared pair.
    assert_eq!(s["score"], 52);
    assert_eq!(
        s["reasons"],
        json!([
            {"code": "rarely_opened", "opened": 1, "received": 8},
            {"code": "cleared_unopened", "count": 2}
        ])
    );
}

#[tokio::test]
async fn senders_are_ordered_and_a_known_contact_is_zeroed() {
    let h = harness(|_, _| {});
    let since = (Utc::now() - Duration::days(400)).to_rfc3339();
    h.store
        .set_app_setting(h.acct, "opened_ledger_since", &since)
        .unwrap();
    // Three senders ignored identically (eight unopened each). One is a person
    // the owner writes to. `few` has too little history to score.
    let friend = "friend@people.example";
    for (addr, count) in [
        (friend, 8),
        ("zed@x.example", 8),
        ("abe@x.example", 8),
        ("busy@x.example", 12),
        ("few@x.example", 2),
    ] {
        for i in 0..count {
            let id = seed(
                &h.store,
                &from(h.acct, &format!("{addr}{i}"), addr, i + 1),
                false,
            );
            if i == 0 {
                into_reading(&h.store, h.acct, id);
            }
        }
    }
    h.store
        .merge_harvested_contacts(
            h.acct,
            &[ContactEntry {
                addr: friend.into(),
                display_name: None,
                sent_count: 3,
                last_sent_at: Some(Utc::now()),
            }],
        )
        .unwrap();

    let body = senders(&h).await;
    let order: Vec<(&str, &Value)> = body["senders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["sender"].as_str().unwrap(), &s["score"]))
        .collect();
    assert_eq!(
        order,
        [
            // Score first; the tie at 55 goes to the heavier sender, then to
            // the address.
            ("busy@x.example", &json!(55)),
            ("abe@x.example", &json!(55)),
            ("zed@x.example", &json!(55)),
            ("friend@people.example", &json!(0)),
            // Unscored last.
            ("few@x.example", &Value::Null),
        ]
    );
    let f = sender(&body, friend);
    assert_eq!(f["known_contact"], true);
    assert_eq!(f["signals"]["received"], 8, "the counts are still shown");
    assert_eq!(f["reasons"], json!([{"code": "known_contact"}]));
}

#[tokio::test]
async fn the_unsubscribe_representative_is_the_newest_mail_with_a_link() {
    let h = harness(|_, _| {});
    let with = |key: &str, addr: &str, days_ago: i64, f: &dyn Fn(&mut NewMessage)| {
        let mut m = from(h.acct, key, addr, days_ago);
        f(&mut m);
        let id = seed(&h.store, &m, false);
        if days_ago == 1 {
            into_reading(&h.store, h.acct, id);
        }
        id
    };
    // HEADER, authenticated: the automatic path would run.
    let auto = with("a1", "a@auto.example", 1, &|m| {
        m.list_unsubscribe = Some("<mailto:u@auto.example>, <https://auto.example/u>".into());
        m.list_unsub_one_click = true;
        m.auth_pass = Some(true);
    });
    // HEADER, but the mail failed authentication: a link, never automatic.
    let unverified = with("u1", "u@unverified.example", 1, &|m| {
        m.list_unsubscribe = Some("<https://unverified.example/u>".into());
        m.auth_pass = Some(false);
    });
    // The newest mail has no link at all, so the next newest is the one.
    with("o1", "o@older.example", 1, &|_| {});
    let older = with("o2", "o@older.example", 2, &|m| {
        m.list_unsubscribe = Some("<https://older.example/u>".into());
    });
    // FOOTER link on the sender's own site: found in the body, and automatic.
    let footer = with("f1", "f@footer.example", 1, &|m| {
        m.body_html =
            Some("<p>Hi</p><a href=\"https://mail.footer.example/stop\">Unsubscribe</a>".into());
        m.auth_pass = Some(true);
    });
    // FOOTER link on somebody else's site (a forward): a link, not automatic.
    let foreign = with("x1", "x@forwarder.example", 1, &|m| {
        m.body_html = Some("<a href=\"https://elsewhere.example/stop\">unsubscribe</a>".into());
        m.auth_pass = Some(true);
    });
    // Only a mailto: nothing a browser or the daemon can follow.
    with("m1", "m@mailto.example", 1, &|m| {
        m.list_unsubscribe = Some("<mailto:u@mailto.example>".into());
        m.auth_pass = Some(true);
    });
    // Already asked, by the browser path.
    h.store
        .upsert_unsubscribe(h.acct, "a@auto.example", "browser", Some(auto), Utc::now())
        .unwrap();

    let body = senders(&h).await;
    let unsub = |addr: &str| sender(&body, addr)["unsubscribe"].clone();
    assert_eq!(
        unsub("a@auto.example"),
        json!({"message_id": auto, "auto": true})
    );
    assert_eq!(
        unsub("u@unverified.example"),
        json!({"message_id": unverified, "auto": false})
    );
    assert_eq!(
        unsub("o@older.example"),
        json!({"message_id": older, "auto": false}),
        "never authenticated, so never automatic"
    );
    assert_eq!(
        unsub("f@footer.example"),
        json!({"message_id": footer, "auto": true})
    );
    assert_eq!(
        unsub("x@forwarder.example"),
        json!({"message_id": foreign, "auto": false})
    );
    assert_eq!(unsub("m@mailto.example"), Value::Null);

    let asked = &sender(&body, "a@auto.example")["unsubscribed"];
    assert_eq!(asked["status"], "browser");
    assert!(asked["requested_at"].is_string());
    assert_eq!(
        sender(&body, "f@footer.example")["unsubscribed"],
        Value::Null
    );

    // Listing changed nothing: no sender was unsubscribed by being looked at.
    assert_eq!(h.store.list_unsubscribes(h.acct).unwrap().len(), 1);
}
