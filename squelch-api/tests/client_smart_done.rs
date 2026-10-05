//! Contract tests for the Reading keep record (`POST
//! /client/v2/reading/senders/keep`), smart done (`POST
//! /client/v2/reading/smart_done`) and its undo.
mod common;
use axum::http::StatusCode;
use chrono::{Duration, Utc};
use common::*;
use serde_json::{Value, json};
use squelch_core::store::{
    SqliteStore, Store, UsageTokens,
    agent_triage::{AgentCommitOutcome, AgentTriageStore},
};
use squelch_core::triage::decision::*;
use squelch_core::types::{AttentionStatus, NewMessage, Sensitivity, Tier};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

/// Strings that must never reach the model: a subject, a body, an address and
/// a display name, each unlike anything a summary here says.
const SUBJECT: &str = "SUBJECT-ZEBRA-7731";
const BODY: &str = "BODY-QUARTZ-9920";
const ADDRESS: &str = "hidden-sender@private-domain.example";
const NAME: &str = "NAME-ORCHID-4415";

/// One Reading message, `days_ago` old, with the given triage summary. The
/// decision is a promotion unless `adjust` says otherwise.
fn reading(
    store: &SqliteStore,
    acct: i64,
    key: &str,
    days_ago: i64,
    summary: &str,
    adjust: impl FnOnce(&mut MessageDecision),
) -> i64 {
    let message = NewMessage {
        from_addr: ADDRESS.to_string(),
        from_name: Some(NAME.to_string()),
        received_at: Utc::now() - Duration::days(days_ago),
        ..msg(acct, key, key, SUBJECT, BODY)
    };
    let id = store.upsert_message(&message).unwrap();
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
    commit(store, acct, id, summary, adjust);
    id
}

/// Commit a triage decision for `id`, the way the worker does. Called again
/// for the same message it is a re-triage: same id, new summary.
fn commit(
    store: &SqliteStore,
    acct: i64,
    id: i64,
    summary: &str,
    adjust: impl FnOnce(&mut MessageDecision),
) {
    store.enqueue_agent_triage(acct, id, "test", false).unwrap();
    let job = store
        .claim_agent_job(acct, "triage", Utc::now(), 60)
        .unwrap()
        .unwrap();
    assert_eq!(job.message_id, id);
    let context = store.load_agent_context(&job).unwrap();
    let mut decision = MessageDecision {
        kinds: vec![EmailKind::Promotional],
        destinations: vec![MessageDestination::Reading],
        reading_kind: Some(ReadingKind::Promotion),
        summary: summary.into(),
        reason: "A promotion".into(),
        ..Default::default()
    };
    adjust(&mut decision);
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

/// What the mock model saw, and how many times it was asked.
#[derive(Default)]
struct Seen {
    bodies: Vec<String>,
}

/// A model mock. Answers every request with `results` (the model's JSON
/// output) or, when `results` is `None`, with a 500. Records each request
/// body, and runs `on_request` before answering so a test can change the
/// store while the "model" is thinking.
async fn model_mock(
    results: Option<Value>,
    on_request: impl Fn() + Send + Sync + 'static,
) -> (String, Arc<Mutex<Seen>>) {
    model_mock_after(std::time::Duration::ZERO, results, on_request).await
}

/// [`model_mock`] that thinks for `delay` before it answers.
async fn model_mock_after(
    delay: std::time::Duration,
    results: Option<Value>,
    on_request: impl Fn() + Send + Sync + 'static,
) -> (String, Arc<Mutex<Seen>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let record = seen.clone();
    let on_request = Arc::new(on_request);
    let reply = results.map(|results| {
        json!({
            "content": [{"type": "text", "text": json!({"results": results}).to_string()}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 900, "output_tokens": 40}
        })
        .to_string()
    });
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let record = record.clone();
            let on_request = on_request.clone();
            let reply = reply.clone();
            tokio::spawn(async move {
                // Headers, then exactly Content-Length bytes of body.
                let mut buf = Vec::new();
                let mut chunk = [0u8; 8192];
                let body = loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break String::new();
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    let Some(split) = text.find("\r\n\r\n") else {
                        continue;
                    };
                    let length = text[..split]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if buf.len() >= split + 4 + length {
                        break text[split + 4..].to_string();
                    }
                };
                record.lock().unwrap().bodies.push(body);
                on_request();
                tokio::time::sleep(delay).await;
                let (status, body) = match &reply {
                    Some(body) => ("200 OK", body.clone()),
                    None => ("500 Internal Server Error", "{}".to_string()),
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    (format!("http://{addr}"), seen)
}

/// A harness with smart done's model pointed at `url`.
fn with_model(url: String) -> Harness {
    use squelch_core::config::Stage2Provider;
    use squelch_core::triage::smart_done::SmartDoneClient;
    let (state, store, acct) = state_with(|_, _| {});
    let app = squelch_api::router(state.with_smart_done(Some(SmartDoneClient::new(
        reqwest::Client::new(),
        url,
        "sk-test",
        Stage2Provider::Anthropic,
        "claude-sonnet-5",
        Some("low".into()),
    ))));
    Harness { app, store, acct }
}

async fn press(h: &Harness) -> Value {
    let response = h
        .app
        .clone()
        .oneshot(authed("POST", "/client/v2/reading/smart_done"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await
}

async fn undo(h: &Harness, batch: &str) -> (StatusCode, Value) {
    let response = h
        .app
        .clone()
        .oneshot(authed(
            "POST",
            &format!("/client/v2/reading/smart_done/{batch}/undo"),
        ))
        .await
        .unwrap();
    let status = response.status();
    (status, body_json(response).await)
}

/// The ids currently open in Reading, as the feed reports them.
async fn open_in_reading(h: &Harness) -> Vec<i64> {
    let response = h
        .app
        .clone()
        .oneshot(authed("GET", "/client/v2/feed?destination=reading"))
        .await
        .unwrap();
    let mut ids: Vec<i64> = body_json(response).await["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["message_id"].as_i64().unwrap())
        .collect();
    ids.sort();
    ids
}

fn audits(h: &Harness, action: &str) -> Vec<(Option<String>, Option<String>)> {
    h.store
        .list_audit(h.acct, 100)
        .unwrap()
        .into_iter()
        .filter(|a| a.action == action)
        .map(|a| (a.target, a.detail))
        .collect()
}

#[tokio::test]
async fn smart_done_clears_a_batch_reports_it_and_undo_gives_it_back() {
    let h = harness(|_, _| {});
    let stale_a = reading(&h.store, h.acct, "a", 20, "Autumn sale", |_| {});
    let stale_b = reading(&h.store, h.acct, "b", 21, "Winter sale", |_| {});
    let ended = reading(&h.store, h.acct, "c", 8, "Sale to the 1st", |d| {
        d.ends = Some(SupportedTime {
            value: (Utc::now() - Duration::days(4))
                .format("%Y-%m-%d")
                .to_string(),
            ..Default::default()
        });
    });
    // An essay as old as the stale promotions, and a promotion from today.
    let essay = reading(&h.store, h.acct, "d", 20, "On walking", |d| {
        d.kinds = vec![EmailKind::Editorial];
        d.reading_kind = Some(ReadingKind::Content);
    });
    let today = reading(&h.store, h.acct, "e", 0, "Flash sale", |_| {});
    // A thread sibling of a cleared row, outside Reading altogether.
    let sibling = h
        .store
        .upsert_message(&msg(h.acct, "sibling", "a", "Re: sale", "a reply"))
        .unwrap();
    h.store
        .set_triage(
            sibling,
            h.acct,
            0,
            Tier::Signal,
            Sensitivity::Normal,
            None,
            "",
            "",
            None,
        )
        .unwrap();

    let body = press(&h).await;
    let batch = body["batch_id"].as_str().expect("a string id").to_string();
    assert_eq!(body["cleared"], 3);
    assert_eq!(body["senders"], 1);
    assert_eq!(
        body["by_reason"],
        json!({"event_passed": 0, "offer_ended": 1, "expired": 0,
               "ignored_sender": 0, "stale_promo": 2})
    );
    let mut ids: Vec<i64> = serde_json::from_value(body["message_ids"].clone()).unwrap();
    ids.sort();
    assert_eq!(ids, [stale_a, stale_b, ended]);
    // No model is configured, and the essay had a summary worth asking about.
    assert_eq!(body["model"], "skipped_unconfigured");
    assert_eq!(open_in_reading(&h).await, [essay, today]);
    // The sibling of a cleared message was not resolved with it.
    let updates = h
        .store
        .triage_debug(h.acct, sibling)
        .unwrap()
        .expect("the sibling");
    assert_ne!(updates.status, "done");
    // ONE audit row for the batch, carrying the count.
    assert_eq!(
        audits(&h, "reading.smart_done"),
        [(Some(batch.clone()), Some("3".to_string()))]
    );

    // A second press finds nothing, records nothing, and says so.
    let again = press(&h).await;
    assert_eq!(again["batch_id"], Value::Null);
    assert_eq!(again["cleared"], 0);
    assert_eq!(again["message_ids"], json!([]));
    assert_eq!(audits(&h, "reading.smart_done").len(), 1);

    // A person re-does one of them, and then the batch is undone.
    h.store
        .set_attention_status(h.acct, stale_b, AttentionStatus::Open)
        .unwrap();
    h.store
        .set_attention_status(h.acct, stale_b, AttentionStatus::Done)
        .unwrap();
    assert_eq!(
        undo(&h, &batch).await,
        (StatusCode::OK, json!({"reopened": 2}))
    );
    assert_eq!(open_in_reading(&h).await, [stale_a, ended, essay, today]);
    assert_eq!(
        undo(&h, &batch).await,
        (StatusCode::OK, json!({"reopened": 0}))
    );
    assert_eq!(
        audits(&h, "reading.smart_done_undo"),
        [
            (Some(batch.clone()), Some("0".to_string())),
            (Some(batch.clone()), Some("2".to_string()))
        ]
    );

    // A batch this account does not have is 404, whatever the reason.
    assert_eq!(undo(&h, "99999").await.0, StatusCode::NOT_FOUND);
    assert_eq!(undo(&h, "not-a-batch").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn another_accounts_batch_cannot_be_undone() {
    let h = harness(|_, _| {});
    let other = h.store.ensure_account("other@example.com").unwrap();
    let theirs = reading(&h.store, other, "t", 20, "Their sale", |_| {});
    let inventory = squelch_core::store::agent_triage::AgentListQuery::default();
    let scan = h
        .store
        .smart_done_scan(other, Utc::now(), &inventory, 200)
        .unwrap();
    let batch = h
        .store
        .smart_done_apply(
            other,
            Utc::now(),
            &inventory,
            200,
            &scan,
            &Default::default(),
        )
        .unwrap()
        .batch_id
        .expect("their batch");
    assert_eq!(undo(&h, &batch.to_string()).await.0, StatusCode::NOT_FOUND);
    // And it is still theirs to undo.
    assert_eq!(
        h.store.smart_done_undo(other, batch, Utc::now()).unwrap(),
        Some(1)
    );
    let _ = theirs;
}

#[tokio::test]
async fn nothing_to_ask_is_reported_before_anything_else() {
    let h = harness(|_, _| {});
    // Every row is decided by a stored fact or is too young to touch.
    reading(&h.store, h.acct, "a", 20, "Old sale", |_| {});
    reading(&h.store, h.acct, "b", 0, "New sale", |_| {});
    let body = press(&h).await;
    assert_eq!(body["model"], "skipped_nothing_to_ask");
    assert_eq!(body["cleared"], 1);
}

#[tokio::test]
async fn the_model_sees_summaries_and_dates_only_and_its_stray_answers_are_ignored() {
    let (url, seen) = model_mock(
        Some(json!([
            {"index": 1, "expired": true, "kind": "offer_ended"},
            {"index": 2, "expired": false, "kind": null},
            {"index": 3, "expired": true, "kind": null},
            // Rows it was never given.
            {"index": 4, "expired": true, "kind": null},
            {"index": 0, "expired": true, "kind": null},
        ])),
        || {},
    )
    .await;
    let h = with_model(url);
    // Newest first is the order they are numbered in.
    let first = reading(&h.store, h.acct, "a", 2, "Sale ended September 30", |_| {});
    let second = reading(&h.store, h.acct, "b", 3, "New boots are in stock", |_| {});
    let third = reading(&h.store, h.acct, "c", 4, "Signups closed last week", |_| {});
    // No summary: not asked about, so index 4 names nothing.
    let silent = reading(&h.store, h.acct, "d", 5, "", |_| {});

    let body = press(&h).await;
    assert_eq!(body["model"], "ok");
    assert_eq!(
        body["by_reason"],
        json!({"event_passed": 0, "offer_ended": 1, "expired": 1,
               "ignored_sender": 0, "stale_promo": 0})
    );
    let mut ids: Vec<i64> = serde_json::from_value(body["message_ids"].clone()).unwrap();
    ids.sort();
    assert_eq!(ids, [first, third]);
    assert_eq!(open_in_reading(&h).await, [second, silent]);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.bodies.len(), 1, "one call");
    let sent = &seen.bodies[0];
    let request: Value = serde_json::from_str(sent).unwrap();
    assert_eq!(request["model"], "claude-sonnet-5");
    for summary in [
        "[1] arrived:",
        "Sale ended September 30",
        "[2] arrived:",
        "New boots are in stock",
        "[3] arrived:",
        "Signups closed last week",
    ] {
        assert!(sent.contains(summary), "missing {summary}");
    }
    assert!(!sent.contains("[4] arrived:"), "the row with no summary");
    for secret in [SUBJECT, BODY, ADDRESS, NAME, "private-domain"] {
        assert!(!sent.contains(secret), "{secret} left the process");
    }
    drop(seen);

    // Billed to its own category, once.
    let usage = h.store.list_usage_by_category(h.acct, 1).unwrap();
    let (category, days) = &usage[0];
    assert_eq!((usage.len(), category.as_str()), (1, "smart_done"));
    assert_eq!((days[0].calls, days[0].input_tokens), (1, 900));
}

#[tokio::test]
async fn a_failed_model_pass_degrades_to_stored_facts() {
    let (url, seen) = model_mock(None, || {}).await;
    let h = with_model(url);
    let stale = reading(&h.store, h.acct, "a", 20, "Old sale", |_| {});
    let undecided = reading(&h.store, h.acct, "b", 3, "Sale ended yesterday", |_| {});
    let body = press(&h).await;
    assert_eq!(body["model"], "skipped_failed");
    assert_eq!(body["message_ids"], json!([stale]));
    assert_eq!(open_in_reading(&h).await, [undecided]);
    assert_eq!(seen.lock().unwrap().bodies.len(), 1, "one try, no retry");
}

#[tokio::test]
async fn the_daily_cap_skips_the_model_and_still_clears() {
    let (url, seen) = model_mock(
        Some(json!([{"index": 1, "expired": true, "kind": null}])),
        || {},
    )
    .await;
    let h = with_model(url);
    let stale = reading(&h.store, h.acct, "a", 20, "Old sale", |_| {});
    let undecided = reading(&h.store, h.acct, "b", 3, "Sale ended yesterday", |_| {});
    // Twenty passes already billed today.
    let today = Utc::now().format("%Y-%m-%d").to_string();
    for _ in 0..20 {
        h.store
            .extract_bump_usage(h.acct, &today, "smart_done", UsageTokens::default())
            .unwrap();
    }
    let body = press(&h).await;
    assert_eq!(body["model"], "skipped_cap");
    assert_eq!(body["message_ids"], json!([stale]));
    assert_eq!(open_in_reading(&h).await, [undecided]);
    assert!(
        seen.lock().unwrap().bodies.is_empty(),
        "the model was not asked"
    );
}

/// The owner keeps using the app while the model thinks. What they did in
/// that time wins over what the model then says.
#[tokio::test]
async fn rows_that_changed_during_the_model_call_are_not_written() {
    // (store, account, the row to reopen by hand, the row to remind)
    type During = (Arc<SqliteStore>, i64, i64, i64);
    let slot: Arc<Mutex<Option<During>>> = Arc::new(Mutex::new(None));
    let during = slot.clone();
    let (url, _seen) = model_mock(
        Some(json!([
            {"index": 1, "expired": true, "kind": null},
            {"index": 2, "expired": true, "kind": null},
            {"index": 3, "expired": true, "kind": null},
        ])),
        move || {
            let guard = during.lock().unwrap();
            let (store, acct, resolved, reminded) = guard.as_ref().unwrap();
            store
                .set_attention_status(*acct, *resolved, AttentionStatus::Done)
                .unwrap();
            store
                .set_attention_status(*acct, *resolved, AttentionStatus::Open)
                .unwrap();
            store
                .set_reminder(*acct, *reminded, Utc::now() - Duration::minutes(1))
                .unwrap();
            store.fire_due_reminders(*acct, Utc::now()).unwrap();
        },
    )
    .await;
    let h = with_model(url);
    let untouched = reading(&h.store, h.acct, "a", 2, "Sale ended Monday", |_| {});
    // Reopened by hand mid-call: still open, so still the model's to take.
    let reopened = reading(&h.store, h.acct, "b", 3, "Offer ended Sunday", |_| {});
    // Reminded mid-call, and the reminder fired: the owner wants it back.
    let reminded = reading(&h.store, h.acct, "c", 4, "Event was Saturday", |_| {});
    *slot.lock().unwrap() = Some((h.store.clone(), h.acct, reopened, reminded));

    let body = press(&h).await;
    assert_eq!(body["model"], "ok");
    let mut ids: Vec<i64> = serde_json::from_value(body["message_ids"].clone()).unwrap();
    ids.sort();
    assert_eq!(ids, [untouched, reopened]);
    assert_eq!(open_in_reading(&h).await, [reminded]);
}

#[tokio::test]
async fn keeping_a_sender_zeroes_its_score_and_is_audited() {
    let h = harness(|_, _| {});
    h.store
        .set_app_setting(
            h.acct,
            "opened_ledger_since",
            &(Utc::now() - Duration::days(400)).to_rfc3339(),
        )
        .unwrap();
    for i in 0..8 {
        reading(
            &h.store,
            h.acct,
            &format!("n{i}"),
            i + 15,
            "An issue",
            |_| {},
        );
    }
    let senders = |h: &Harness| {
        let app = h.app.clone();
        async move {
            let response = app
                .oneshot(authed("GET", "/client/v2/reading/senders"))
                .await
                .unwrap();
            body_json(response).await["senders"][0].clone()
        }
    };
    let before = senders(&h).await;
    assert_eq!(before["kept_at"], Value::Null);
    assert_eq!(before["score"], 55);

    let keep = |body: Value| {
        let app = h.app.clone();
        async move {
            app.oneshot(authed_json("POST", "/client/v2/reading/senders/keep", body))
                .await
                .unwrap()
                .status()
        }
    };
    // Whatever the case and padding, it is the one sender.
    assert_eq!(
        keep(json!({"sender": format!("  {} ", ADDRESS.to_uppercase())})).await,
        StatusCode::NO_CONTENT
    );
    let after = senders(&h).await;
    assert!(after["kept_at"].is_string(), "{after}");
    assert_eq!(after["score"], 0);
    assert_eq!(after["reasons"], json!([{"code": "kept_recently"}]));
    assert_eq!(
        audits(&h, "reading.keep_sender"),
        [(Some(ADDRESS.to_string()), Some("kept".to_string()))]
    );
    // And smart done leaves a kept sender's mail where it is, though every
    // one of the eight is a stale promotion it would otherwise take.
    assert_eq!(press(&h).await["cleared"], 0);

    // Not an address: refused, and not recorded.
    for bad in [json!({"sender": ""}), json!({"sender": "not an address"})] {
        assert_eq!(keep(bad).await, StatusCode::BAD_REQUEST);
    }
    assert_eq!(keep(json!({})).await, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(audits(&h, "reading.keep_sender").len(), 1);
}

#[tokio::test]
async fn the_three_writes_are_behind_the_bearer() {
    let h = harness(|_, _| {});
    for uri in [
        "/client/v2/reading/senders/keep",
        "/client/v2/reading/smart_done",
        "/client/v2/reading/smart_done/1/undo",
    ] {
        let response = h
            .app
            .clone()
            .oneshot(json_request(
                "POST",
                uri,
                &json!({"sender": "a@b.example"}),
                false,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
    assert!(h.store.list_audit(h.acct, 10).unwrap().is_empty());
}

// ---- review fixes ---------------------------------------------------------

/// The prompt tells the model to answer expired only when the summary states
/// a date. The daemon checks, because a summary is attacker-influenced text
/// and one of them can talk the model into yes for every row.
#[tokio::test]
async fn a_yes_about_a_summary_that_states_no_date_is_ignored() {
    let (url, _seen) = model_mock(
        Some(json!([
            {"index": 1, "expired": true, "kind": "offer_ended"},
            {"index": 2, "expired": true, "kind": null},
            {"index": 3, "expired": true, "kind": null},
        ])),
        || {},
    )
    .await;
    let h = with_model(url);
    let dated = reading(&h.store, h.acct, "a", 2, "Sale ended September 30", |_| {});
    let essay = reading(&h.store, h.acct, "b", 3, "A long essay on walking", |d| {
        d.kinds = vec![EmailKind::Editorial];
        d.reading_kind = Some(ReadingKind::Content);
    });
    let undated = reading(&h.store, h.acct, "c", 4, "New boots are in stock", |_| {});
    let body = press(&h).await;
    assert_eq!(body["model"], "ok");
    assert_eq!(body["message_ids"], json!([dated]));
    assert_eq!(open_in_reading(&h).await, [essay, undated]);
}

/// Ten or more rows asked and nearly all of them "expired" is not a reading
/// of the mail, it is a pass that went wrong. None of it is used.
#[tokio::test]
async fn a_pass_that_calls_nearly_everything_expired_is_discarded() {
    let results: Vec<Value> = (1..=10)
        .map(|i| json!({"index": i, "expired": i != 10, "kind": null}))
        .collect();
    let (url, _seen) = model_mock(Some(json!(results)), || {}).await;
    let h = with_model(url);
    let mut ids = Vec::new();
    for i in 0..10 {
        ids.push(reading(
            &h.store,
            h.acct,
            &format!("r{i}"),
            2,
            &format!("Offer {i} ended on October 1"),
            |_| {},
        ));
    }
    let stale = reading(&h.store, h.acct, "stale", 20, "Old sale", |_| {});
    let body = press(&h).await;
    assert_eq!(body["model"], "skipped_failed");
    assert_eq!(body["message_ids"], json!([stale]));
    ids.sort();
    assert_eq!(open_in_reading(&h).await, ids);
    // The call still happened and is still billed.
    let usage = h.store.list_usage_by_category(h.acct, 1).unwrap();
    assert_eq!(usage[0].0, "smart_done");
}

/// One press at a time. A second one while the first is still waiting on the
/// model is refused without touching the store.
#[tokio::test]
async fn a_second_press_while_one_is_running_is_refused() {
    let (url, seen) = model_mock_after(
        std::time::Duration::from_millis(400),
        Some(json!([{"index": 1, "expired": false, "kind": null}])),
        || {},
    )
    .await;
    let h = with_model(url);
    reading(&h.store, h.acct, "a", 2, "Sale ends October 30", |_| {});
    let stale = reading(&h.store, h.acct, "b", 20, "Old sale", |_| {});
    let post = || {
        let app = h.app.clone();
        async move {
            app.oneshot(authed("POST", "/client/v2/reading/smart_done"))
                .await
                .unwrap()
        }
    };
    let second = async {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        post().await
    };
    let (first, second) = tokio::join!(post(), second);
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body_json(first).await["message_ids"], json!([stale]));
    assert_eq!(seen.lock().unwrap().bodies.len(), 1, "one model call");
    assert_eq!(audits(&h, "reading.smart_done").len(), 1);
    // And the next press, once it is over, is served.
    assert_eq!(post().await.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_keep_route_refuses_oversized_and_control_character_senders() {
    let h = harness(|_, _| {});
    let keep = |sender: String| {
        let app = h.app.clone();
        async move {
            app.oneshot(authed_json(
                "POST",
                "/client/v2/reading/senders/keep",
                json!({ "sender": sender }),
            ))
            .await
            .unwrap()
            .status()
        }
    };
    let long = format!("{}@example.com", "a".repeat(320));
    for bad in [
        long,
        "a\u{0}b@example.com".to_string(),
        "a\u{1b}[31m@example.com".to_string(),
        "a\u{202e}b@example.com".to_string(),
        "@example.com".to_string(),
        "someone@".to_string(),
        "a@b@example.com".to_string(),
    ] {
        assert_eq!(keep(bad.clone()).await, StatusCode::BAD_REQUEST, "{bad:?}");
    }
    assert_eq!(
        keep("news@example.com".into()).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(audits(&h, "reading.keep_sender").len(), 1);
}

/// Under a week of ledger there is no monthly rate to state, and under two
/// there is no score.
#[tokio::test]
async fn a_young_ledger_reports_no_rate_and_no_score() {
    let h = harness(|_, _| {});
    let senders = |h: &Harness| {
        let app = h.app.clone();
        async move {
            let response = app
                .oneshot(authed("GET", "/client/v2/reading/senders"))
                .await
                .unwrap();
            body_json(response).await["senders"][0].clone()
        }
    };
    let set_ledger = |days: i64| {
        h.store
            .set_app_setting(
                h.acct,
                "opened_ledger_since",
                &(Utc::now() - Duration::days(days)).to_rfc3339(),
            )
            .unwrap();
    };
    for i in 0..8 {
        reading(
            &h.store,
            h.acct,
            &format!("n{i}"),
            1,
            &format!("Issue {i}"),
            |_| {},
        );
    }
    set_ledger(3);
    let s = senders(&h).await;
    assert_eq!(s["signals"]["received"], 8);
    assert_eq!(s["signals"]["per_month"], Value::Null);
    assert_eq!(s["score"], Value::Null);
    assert_eq!(s["reasons"], json!([{"code": "not_enough_history"}]));

    set_ledger(10);
    let s = senders(&h).await;
    assert_eq!(s["signals"]["per_month"], 24.0, "a rate from a week on");
    assert_eq!(s["score"], Value::Null, "and still no score");

    set_ledger(15);
    let s = senders(&h).await;
    assert_eq!(s["signals"]["per_month"], 16.0);
    assert_eq!(s["score"], 60, "55 ignored and 5 for sixteen a month");
}
