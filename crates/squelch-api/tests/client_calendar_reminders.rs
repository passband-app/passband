//! Calendar reminders for ghosts, with no calendar granted (docs/CALENDAR.md,
//! "Reminders and tickets"): a committed booking from authenticated mail is
//! reminded on its own, with the fallback when no model planned it; the
//! owner's preference and the `calendar` feature each stop all of it; and the
//! timeline's tickets button falls back to the booking's PDF. The planned
//! path, against a synced event and a model, is
//! `client_calendar_sync::an_event_is_planned_once_fires_once_and_serves_its_tickets`.
mod common;
use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use common::*;
use serde_json::{Value, json};
use squelch_api::{ApiState, RemindOutcome, router, run_fire_pass, run_plan_pass};
use squelch_core::features::Features;
use squelch_core::store::{
    SqliteStore, Store,
    agent_triage::{AgentCommitOutcome, AgentTriageStore},
};
use squelch_core::triage::decision::*;
use squelch_core::types::NewMessage;
use std::sync::Arc;
use tower::ServiceExt;

fn rfc(at: DateTime<Utc>) -> String {
    let at = DateTime::from_timestamp(at.timestamp() / 60 * 60, 0).unwrap();
    at.with_timezone(&chrono::FixedOffset::west_opt(4 * 3600).unwrap())
        .format("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}

/// A triage decision carrying one `event` record, committed the way
/// production makes a ghost. `auth_pass` false is mail that failed
/// authentication. Returns the message id.
fn seed(
    store: &SqliteStore,
    account: i64,
    gmail: &str,
    title: &str,
    start: String,
    commitment: EventCommitment,
    auth_pass: bool,
) -> i64 {
    let id = store
        .upsert_message(&NewMessage {
            received_at: Utc::now() - Duration::minutes(5),
            auth_pass: Some(auth_pass),
            ..msg(
                account,
                gmail,
                &format!("t-{gmail}"),
                "Your tickets",
                "details",
            )
        })
        .unwrap();
    store
        .enqueue_agent_triage(account, id, "test", false)
        .unwrap();
    let job = store
        .claim_agent_job(account, "triage", Utc::now(), 60)
        .unwrap()
        .unwrap();
    let context = store.load_agent_context(&job).unwrap();
    let evidence = vec![EvidenceRef {
        message_id: id,
        location: "body".into(),
    }];
    let decision = MessageDecision {
        kinds: vec![EmailKind::EventReservation],
        summary: "Plans".into(),
        reason: "An event".into(),
        records: vec![RecordProposal::Event {
            title: title.into(),
            start: Some(SupportedTime {
                value: start,
                timezone: Some("America/New_York".into()),
                source_message_id: id,
                interpreted_relative: false,
            }),
            location: Some("Brooklyn Steel".into()),
            end: None,
            duration_estimate: None,
            event_kind: EventRecordKind::Ticket,
            commitment,
            change: EventChange::Created,
            identity_refs: vec![format!("REF{gmail}12345")],
            likelihood: None,
            evidence: evidence.clone(),
            identity: None,
        }],
        external_access: AccessAssessment {
            restricted: false,
            reason: "Assessment".into(),
            evidence: evidence.clone(),
        },
        attention: ThreadAttentionDecision {
            relevant_message_ids: vec![id],
            evidence,
            summary: "Plans".into(),
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
    id
}

fn setup(features: Features) -> (ApiState, axum::Router, Arc<SqliteStore>, i64) {
    let (state, store, acct) = state_with(|_, _| {});
    let state = state.with_features(features);
    let app = router(state.clone());
    (state, app, store, acct)
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = match body {
        Some(body) => authed_json(method, uri, body),
        None => authed(method, uri),
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

fn fired(outcome: RemindOutcome) -> Vec<i64> {
    match outcome {
        RemindOutcome::Fired(ids) => ids,
        other => panic!("expected a fire pass, got {other:?}"),
    }
}

/// A booking 20 minutes out with no plan (no model configured) fires the
/// fallback once: 30 minutes, no line, opening the booking's PDF, which the
/// phone fetches by id. A second one 40 minutes out is not due yet.
#[tokio::test]
async fn a_committed_ghost_fires_the_fallback_once_without_a_calendar() {
    let (state, app, store, acct) = setup(calendar_on());
    let start = Utc::now() + Duration::minutes(20);
    let message = seed(
        &store,
        acct,
        "g1",
        "Big Thief",
        rfc(start),
        EventCommitment::Committed,
        true,
    );
    let pdf = store
        .insert_attachment(
            acct,
            message,
            "tickets.pdf",
            "application/pdf",
            3,
            Some(b"pdf"),
        )
        .unwrap();
    // An inline image is never the ticket.
    store
        .insert_inline_attachment(
            acct,
            message,
            "logo.png",
            "image/png",
            3,
            Some(b"png"),
            Some("logo"),
        )
        .unwrap();
    seed(
        &store,
        acct,
        "g2",
        "Later show",
        rfc(Utc::now() + Duration::minutes(40)),
        EventCommitment::Committed,
        true,
    );
    // No model: nothing is asked.
    assert_eq!(
        run_plan_pass(&state, Utc::now()).await.unwrap(),
        RemindOutcome::Planned {
            asked: 0,
            stored: 0
        }
    );
    let mut pings = state.subscribe_calendar_pings();
    let ids = fired(run_fire_pass(&state, Utc::now()).await.unwrap());
    assert_eq!(ids.len(), 1, "only the booking inside 30 minutes");
    let ping = pings.try_recv().unwrap();
    assert_eq!(ping.fire_id, ids[0]);

    let (status, frame) = call(
        &app,
        "GET",
        &format!("/client/calendar/reminders/{}", ids[0]),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(frame["item"]["kind"], "ghost");
    assert_eq!(frame["item"]["title"], "Big Thief");
    assert_eq!(frame["item"]["place"], "Brooklyn Steel");
    assert_eq!(frame["lead_minutes"], 30);
    assert_eq!(frame["line"], Value::Null);
    assert_eq!(
        frame["open"],
        json!({"message_id": message, "thread_id": "t-g1", "attachment_id": pdf})
    );
    assert_eq!(frame["ticket"]["filename"], "tickets.pdf");
    assert_eq!(frame["ticket"]["kind"], "ticket");

    assert_eq!(
        fired(run_fire_pass(&state, Utc::now()).await.unwrap()),
        Vec::<i64>::new()
    );

    // The tickets button falls back to the PDF too.
    let (status, body) = call(&app, "GET", "/client/timeline", None).await;
    assert_eq!(status, StatusCode::OK);
    let ghost = body["ghosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["title"] == "Big Thief")
        .unwrap();
    assert_eq!(
        ghost["tickets"],
        json!({"message_id": message, "thread_id": "t-g1", "kind": "ticket",
               "attachment": {"id": pdf, "filename": "tickets.pdf", "mime": "application/pdf"}})
    );
}

/// An invitation not yet accepted, and a booking from mail that failed
/// authentication, are never reminded.
#[tokio::test]
async fn an_uncommitted_or_unauthenticated_ghost_is_not_reminded() {
    let (state, app, store, acct) = setup(calendar_on());
    let start = rfc(Utc::now() + Duration::minutes(20));
    seed(
        &store,
        acct,
        "g1",
        "Maybe party",
        start.clone(),
        EventCommitment::Invited,
        true,
    );
    seed(
        &store,
        acct,
        "g2",
        "Phishy show",
        start,
        EventCommitment::Committed,
        false,
    );
    // Both are ghosts on the timeline: what keeps them quiet is the rule.
    let (_, body) = call(&app, "GET", "/client/timeline", None).await;
    let ghosts = body["ghosts"].as_array().unwrap();
    let verified = |title: &str| {
        ghosts
            .iter()
            .find(|g| g["title"] == title)
            .unwrap_or_else(|| panic!("{title} in {ghosts:?}"))["verified"]
            .clone()
    };
    assert_eq!(verified("Maybe party"), json!(true));
    assert_eq!(verified("Phishy show"), json!(false));
    assert_eq!(
        fired(run_fire_pass(&state, Utc::now()).await.unwrap()),
        Vec::<i64>::new()
    );
}

/// The owner preference: on by default, and off stops every firing.
#[tokio::test]
async fn the_preference_turns_it_all_off_and_on() {
    let (state, app, store, acct) = setup(calendar_on());
    seed(
        &store,
        acct,
        "g1",
        "Big Thief",
        rfc(Utc::now() + Duration::minutes(20)),
        EventCommitment::Committed,
        true,
    );
    let uri = "/client/calendar/reminders/settings";
    assert_eq!(
        call(&app, "GET", uri, None).await,
        (StatusCode::OK, json!({"enabled": true}))
    );
    assert_eq!(
        call(&app, "PUT", uri, Some(json!({"enabled": false}))).await,
        (StatusCode::OK, json!({"enabled": false}))
    );
    assert_eq!(
        call(&app, "GET", uri, None).await.1,
        json!({"enabled": false})
    );
    assert_eq!(
        run_fire_pass(&state, Utc::now()).await.unwrap(),
        RemindOutcome::Off
    );
    assert_eq!(
        run_plan_pass(&state, Utc::now()).await.unwrap(),
        RemindOutcome::Off
    );
    let resp = app
        .clone()
        .oneshot(authed_json("PUT", uri, json!({"enabled": "no"})))
        .await
        .unwrap();
    assert!(
        resp.status().is_client_error(),
        "a body that is not a bool is refused"
    );
    assert!(!store.calendar_reminders_enabled(acct).unwrap());
    call(&app, "PUT", uri, Some(json!({"enabled": true}))).await;
    assert_eq!(
        fired(run_fire_pass(&state, Utc::now()).await.unwrap()).len(),
        1
    );
}

/// With the `calendar` feature off, every route 404s and nothing runs.
#[tokio::test]
async fn with_the_calendar_feature_off_nothing_exists() {
    let (state, app, store, acct) = setup(Features::none());
    seed(
        &store,
        acct,
        "g1",
        "Big Thief",
        rfc(Utc::now() + Duration::minutes(20)),
        EventCommitment::Committed,
        true,
    );
    assert_eq!(
        run_fire_pass(&state, Utc::now()).await.unwrap(),
        RemindOutcome::Off
    );
    for (method, uri, body) in [
        ("GET", "/client/calendar/reminders/settings", None),
        (
            "PUT",
            "/client/calendar/reminders/settings",
            Some(json!({"enabled": false})),
        ),
        ("GET", "/client/calendar/reminders/1", None),
    ] {
        assert_eq!(
            call(&app, method, uri, body).await.0,
            StatusCode::NOT_FOUND,
            "{method} {uri}"
        );
    }
    assert!(
        store.calendar_reminders_enabled(acct).unwrap(),
        "the PUT wrote nothing"
    );
}

/// A STALE plan still governs firing (correctness review m1): one that said
/// no reminder stays silent inside the fallback's window, and one that said
/// yes fires on its own lead, not as the fallback, and without its line,
/// which was made from what has since changed. No model here, so nothing is
/// planned again.
#[tokio::test]
async fn a_stale_plan_still_decides_whether_and_when_it_fires() {
    use squelch_core::gcal::remind::Plan;
    let (state, app, store, acct) = setup(calendar_on());
    seed(
        &store,
        acct,
        "g1",
        "Big Thief",
        rfc(Utc::now() + Duration::minutes(20)),
        EventCommitment::Committed,
        true,
    );
    let (_, body) = call(&app, "GET", "/client/timeline", None).await;
    let id = body["ghosts"][0]["id"].as_i64().unwrap();
    let key = format!("g:{id}");
    let plan = |remind, lead_minutes| Plan {
        remind,
        lead_minutes,
        line: Some("Old words".into()),
        open: None,
        ticket: None,
        same_as: Vec::new(),
        code: None,
    };
    store
        .calendar_reminder_put(acct, &key, "stale", &plan(false, 30), Utc::now())
        .unwrap();
    assert_eq!(
        fired(run_fire_pass(&state, Utc::now()).await.unwrap()),
        Vec::<i64>::new(),
        "a stale no stays no"
    );
    store
        .calendar_reminder_put(acct, &key, "stale", &plan(true, 60), Utc::now())
        .unwrap();
    let ids = fired(run_fire_pass(&state, Utc::now()).await.unwrap());
    assert_eq!(ids.len(), 1);
    let (_, frame) = call(
        &app,
        "GET",
        &format!("/client/calendar/reminders/{}", ids[0]),
        None,
    )
    .await;
    assert_eq!(frame["lead_minutes"], 60, "its lead, not the fallback's 30");
    assert_eq!(frame["line"], Value::Null);
}
