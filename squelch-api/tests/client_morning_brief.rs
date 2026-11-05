//! The morning brief: the worker's pass (once a local day from 08:00 to
//! 11:00, never twice, never off, never empty, iPhones only), the route the
//! phone's extension reads, and the owner's switch.
mod common;
use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use common::*;
use serde_json::{Value, json};
use squelch_api::{ApiState, BriefOutcome, router, run_brief_pass};
use squelch_core::store::{
    ClientTag, SqliteStore, Store,
    agent_triage::{AgentCommitOutcome, AgentTriageStore},
};
use squelch_core::triage::decision::*;
use std::sync::Arc;
use tower::ServiceExt;

const TAG: &str = "3F2A9C1E-7B44-4D0A-9E21-8C5B6D0F1A32";
const PHONE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb2";
const LA: &str = "America/Los_Angeles";

fn at(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

/// The owner's phone has connected from `zone` (which is how the daemon
/// learns the owner's zone) and registered for pushes.
fn phone_in(store: &SqliteStore, acct: i64, zone: &str) {
    let token = store.issue_device_token(acct, "phone").unwrap();
    store
        .verify_device_token_from(
            &token.token,
            Some(
                &ClientTag::parse("ios/0.0.14")
                    .unwrap()
                    .with_zone(Some(zone)),
            ),
        )
        .unwrap()
        .unwrap();
    store
        .upsert_device(acct, PHONE, "ios", Some(TAG), None)
        .unwrap();
}

/// One triaged message whose decision `shape` fills in. Returns its id.
fn seed(
    store: &SqliteStore,
    acct: i64,
    gmail: &str,
    shape: impl FnOnce(i64, &mut MessageDecision),
) {
    let id = store
        .upsert_message(&msg(acct, gmail, &format!("t-{gmail}"), "Hello", "details"))
        .unwrap();
    store.enqueue_agent_triage(acct, id, "test", false).unwrap();
    let job = store
        .claim_agent_job(acct, "triage", Utc::now(), 60)
        .unwrap()
        .unwrap();
    let context = store.load_agent_context(&job).unwrap();
    let mut decision = MessageDecision {
        summary: "Something".into(),
        ..Default::default()
    };
    decision.attention.relevant_message_ids = vec![id];
    shape(id, &mut decision);
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

fn bill(store: &SqliteStore, acct: i64, gmail: &str, merchant: &str, due: &str) {
    let due = due.to_string();
    let merchant = merchant.to_string();
    seed(store, acct, gmail, move |id, d| {
        d.kinds = vec![EmailKind::Bill];
        d.records = vec![RecordProposal::Bill {
            merchant,
            amount: Some(84.0),
            currency: Some("USD".into()),
            due: Some(SupportedTime {
                value: due,
                source_message_id: id,
                ..Default::default()
            }),
            autopay: None,
            evidence: vec![],
        }];
    });
}

fn needs_you(store: &SqliteStore, acct: i64, gmail: &str) {
    seed(store, acct, gmail, |_, d| {
        d.kinds = vec![EmailKind::Correspondence];
        d.attention.show_in_fye = true;
        d.attention.state = AttentionState::NeedsUser;
        d.attention.summary = "Maya asked about Thursday".into();
    });
}

fn setup() -> (ApiState, Arc<SqliteStore>, i64) {
    state_with(|_, _| {})
}

/// 08:15 in Los Angeles on Thursday, November 5, 2026.
const EIGHT_FIFTEEN: &str = "2026-11-05T16:15:00Z";

#[tokio::test]
async fn fires_once_from_eight_and_never_twice() {
    let (state, store, acct) = setup();
    phone_in(&store, acct, LA);
    bill(&store, acct, "b1", "PG&E", "2026-11-05");
    let mut pings = state.subscribe_brief_pings();

    // 07:59: not yet.
    assert_eq!(
        run_brief_pass(&state, at("2026-11-05T15:59:00Z"))
            .await
            .unwrap(),
        BriefOutcome::NotDue
    );
    assert!(pings.try_recv().is_err());

    let day = chrono::NaiveDate::from_ymd_opt(2026, 11, 5).unwrap();
    assert_eq!(
        run_brief_pass(&state, at(EIGHT_FIFTEEN)).await.unwrap(),
        BriefOutcome::Sent(day)
    );
    assert_eq!(pings.try_recv().unwrap().day, day);

    // The next pass, and one an hour later (a restart reads the same stamp):
    // already decided.
    for later in ["2026-11-05T16:16:00Z", "2026-11-05T17:30:00Z"] {
        assert_eq!(
            run_brief_pass(&state, at(later)).await.unwrap(),
            BriefOutcome::NotDue
        );
    }
    assert!(pings.try_recv().is_err(), "one brief a day");
    assert_eq!(
        store
            .get_app_setting(acct, "morning_brief_day")
            .unwrap()
            .as_deref(),
        Some("2026-11-05")
    );
}

#[tokio::test]
async fn a_daemon_up_late_still_sends_until_eleven_and_not_after() {
    let (state, store, acct) = setup();
    phone_in(&store, acct, LA);
    needs_you(&store, acct, "n1");
    let mut pings = state.subscribe_brief_pings();

    // Down through the morning, back at 11:00: the day is skipped.
    assert_eq!(
        run_brief_pass(&state, at("2026-11-05T19:00:00Z"))
            .await
            .unwrap(),
        BriefOutcome::NotDue
    );
    assert!(pings.try_recv().is_err());

    // Back at 10:40 the next day: still the morning, so it goes.
    assert!(matches!(
        run_brief_pass(&state, at("2026-11-06T18:40:00Z"))
            .await
            .unwrap(),
        BriefOutcome::Sent(_)
    ));
    assert!(pings.try_recv().is_ok());
}

#[tokio::test]
async fn off_sends_nothing() {
    let (state, store, acct) = setup();
    phone_in(&store, acct, LA);
    needs_you(&store, acct, "n1");
    let app = router(state.clone());
    let resp = app
        .oneshot(authed_json(
            "PUT",
            "/client/brief/settings",
            json!({ "enabled": false }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await, json!({ "enabled": false }));

    let mut pings = state.subscribe_brief_pings();
    assert_eq!(
        run_brief_pass(&state, at(EIGHT_FIFTEEN)).await.unwrap(),
        BriefOutcome::Off
    );
    assert!(pings.try_recv().is_err());
}

#[tokio::test]
async fn an_empty_day_is_decided_and_silent() {
    let (state, store, acct) = setup();
    phone_in(&store, acct, LA);
    // A bill three days out and a login code on the board: neither is
    // something today holds.
    bill(&store, acct, "b1", "PG&E", "2026-11-08");
    seed(&store, acct, "a1", |_, d| {
        d.kinds = vec![EmailKind::AuthenticationSecurity];
        d.auth.kinds = vec![AuthKind::Otp];
        d.attention.show_in_fye = true;
        d.attention.state = AttentionState::NeedsUser;
        d.attention.summary = "A login code".into();
    });
    let mut pings = state.subscribe_brief_pings();
    let day = chrono::NaiveDate::from_ymd_opt(2026, 11, 5).unwrap();
    assert_eq!(
        run_brief_pass(&state, at(EIGHT_FIFTEEN)).await.unwrap(),
        BriefOutcome::Empty(day)
    );
    assert!(pings.try_recv().is_err());
    // Decided: mail that lands at 9 does not turn into a 9:01 brief.
    needs_you(&store, acct, "n1");
    assert_eq!(
        run_brief_pass(&state, at("2026-11-05T17:01:00Z"))
            .await
            .unwrap(),
        BriefOutcome::NotDue
    );
    assert!(pings.try_recv().is_err());
}

#[tokio::test]
async fn no_iphone_no_brief_and_the_day_stays_open() {
    let (state, store, acct) = setup();
    // The zone is known (a Mac connected from LA) but only a Mac registered.
    let token = store.issue_device_token(acct, "laptop").unwrap();
    store
        .verify_device_token_from(
            &token.token,
            Some(
                &ClientTag::parse("macos/0.0.19")
                    .unwrap()
                    .with_zone(Some(LA)),
            ),
        )
        .unwrap()
        .unwrap();
    store
        .upsert_device(acct, PHONE, "macos", Some(TAG), None)
        .unwrap();
    needs_you(&store, acct, "n1");
    assert_eq!(
        run_brief_pass(&state, at(EIGHT_FIFTEEN)).await.unwrap(),
        BriefOutcome::NoPhone
    );
    assert_eq!(
        store.get_app_setting(acct, "morning_brief_day").unwrap(),
        None
    );
}

#[tokio::test]
async fn no_known_zone_no_brief() {
    let (state, store, acct) = setup();
    // Registered, but no app ever said where it is: 08:00 UTC is nobody's
    // morning in particular.
    store
        .upsert_device(acct, PHONE, "ios", Some(TAG), None)
        .unwrap();
    needs_you(&store, acct, "n1");
    assert_eq!(
        run_brief_pass(&state, at("2026-11-05T08:15:00Z"))
            .await
            .unwrap(),
        BriefOutcome::NotDue
    );
}

/// The route the phone's extension reads: today's words, built now.
#[tokio::test]
async fn the_route_serves_todays_words() {
    let (state, store, acct) = setup();
    phone_in(&store, acct, LA);
    let zone: chrono_tz::Tz = LA.parse().unwrap();
    let local = Utc::now().with_timezone(&zone);
    let today = local.date_naive();
    let tomorrow = today + Duration::days(1);
    needs_you(&store, acct, "n1");
    needs_you(&store, acct, "n2");
    bill(
        &store,
        acct,
        "b1",
        "PG&E",
        &tomorrow.format("%Y-%m-%d").to_string(),
    );
    let app = router(state);

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/brief"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = body_json(resp).await;
    assert_eq!(body["day"], json!(today.format("%Y-%m-%d").to_string()));
    assert_eq!(
        body["title"],
        json!(format!(
            "{}, {}",
            today.format("%A"),
            today.format("%b %-d")
        ))
    );
    assert_eq!(
        body["lines"],
        json!(["2 need you · PG&E bill due tomorrow"])
    );

    // Settings default on.
    let resp = app
        .oneshot(authed("GET", "/client/brief/settings"))
        .await
        .unwrap();
    assert_eq!(body_json(resp).await, json!({ "enabled": true }));
}

#[tokio::test]
async fn the_route_on_an_empty_day() {
    let (state, store, acct) = setup();
    phone_in(&store, acct, LA);
    let resp = router(state)
        .oneshot(authed("GET", "/client/brief"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = body_json(resp).await;
    assert_eq!(body["title"], Value::Null);
    assert_eq!(body["lines"], json!([]));
}
