//! Smart re-notify on the human door (docs/RENOTIFY.md): the Mac stream's
//! session, a pass that sends a `renotify` frame down it, the tap report and
//! the stats, and the feature gate over all of it.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::StatusCode;
use chrono::Utc;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use squelch_api::{ApiState, PassOutcome, router, run_renotify_pass};
use squelch_core::config::RenotifyConfig;
use squelch_core::features::{self, Features};
use squelch_core::renotify::{Hold, Trigger};
use squelch_core::store::{NewEvent, SqliteStore, Store};
use squelch_core::types::{EventKind, Tier};
use tower::ServiceExt;

mod common;
use common::{authed, authed_json, body_json, state_with};

const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// A config whose working day is the whole day, so a test passes at any hour.
fn always_on() -> RenotifyConfig {
    RenotifyConfig {
        work_start_hour: 0,
        work_end_hour: 0,
        ..RenotifyConfig::default()
    }
}

fn notified(store: &SqliteStore, acct: i64, label: i64) -> i64 {
    let message_id = store
        .upsert_message(&common::msg(
            acct,
            &format!("m{label}"),
            &format!("t{label}"),
            "Invoice",
            "Please pay",
        ))
        .unwrap();
    store
        .append_event(&NewEvent {
            account_id: acct,
            message_id,
            thread_id: format!("t{label}"),
            kind: EventKind::Surfaced,
            tier: Tier::Signal,
            importance: 70,
            sender: "alice@example.com".into(),
            one_line: format!("line {label}"),
            deadline: None,
            is_auth: false,
            sealed_kind: None,
        })
        .unwrap()
        .unwrap();
    message_id
}

fn setup(on: bool) -> (ApiState, axum::Router, Arc<SqliteStore>, i64) {
    let (state, store, acct) = state_with(|_, _| {});
    let features = if on {
        Features::pinned([features::RENOTIFY])
    } else {
        Features::none()
    };
    let state = state
        .with_features(features)
        .with_renotify_config(always_on());
    let app = router(state.clone());
    (state, app, store, acct)
}

/// The next `renotify` frame's data on an SSE body, skipping anything else.
async fn next_renotify(body: &mut Body, buf: &mut String) -> Option<Value> {
    let deadline = tokio::time::Instant::now() + READ_TIMEOUT;
    loop {
        while let Some(end) = buf.find("\n\n") {
            let raw = buf[..end].to_string();
            buf.drain(..end + 2);
            if raw.lines().any(|l| l == "event: renotify") {
                let data: String = raw
                    .lines()
                    .filter_map(|l| l.strip_prefix("data:"))
                    .map(str::trim_start)
                    .collect();
                return Some(serde_json::from_str(&data).unwrap());
            }
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, body.frame()).await {
            Err(_) | Ok(None) => return None,
            Ok(Some(chunk)) => {
                if let Some(data) = chunk.unwrap().data_ref() {
                    buf.push_str(std::str::from_utf8(data).unwrap());
                }
            }
        }
    }
}

#[tokio::test]
async fn with_the_feature_off_nothing_exists() {
    let (state, app, store, acct) = setup(false);
    notified(&store, acct, 1);
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/renotify/stats"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let resp = app
        .clone()
        .oneshot(authed("POST", "/client/renotify/1/opened"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // A Mac stream records no session...
    let _stream = app
        .clone()
        .oneshot(authed("GET", "/client/events?opens=1"))
        .await
        .unwrap();
    let (_, away) = store
        .open_client_session(acct, "macos", Utc::now())
        .unwrap();
    assert_eq!(away, None, "the stream wrote no session before this one");
    // ...and a pass does nothing.
    let later = Utc::now() + chrono::Duration::hours(1);
    assert_eq!(
        run_renotify_pass(&state, Trigger::ClientOnline, later)
            .await
            .unwrap(),
        PassOutcome::Off
    );
}

#[tokio::test]
async fn a_pass_with_no_mac_listening_sends_and_records_nothing() {
    let (state, _app, store, acct) = setup(true);
    notified(&store, acct, 1);
    let later = Utc::now() + chrono::Duration::hours(1);
    assert_eq!(
        run_renotify_pass(&state, Trigger::ClientOnline, later)
            .await
            .unwrap(),
        PassOutcome::NoListener
    );
    assert!(
        store
            .renotify_outcomes(acct, Utc::now() - chrono::Duration::days(1))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_banner_reaches_the_mac_and_its_tap_is_counted() {
    let (state, app, store, acct) = setup(true);
    let message_id = notified(&store, acct, 1);

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/events?opens=1"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut body = resp.into_body();
    let mut buf = String::new();

    // The stream recorded an open session.
    let (_, away) = store
        .open_client_session(acct, "macos", Utc::now())
        .unwrap();
    assert_eq!(
        away,
        Some(chrono::Duration::zero()),
        "the stream's session is open"
    );

    // Too soon after the first banner: nothing due.
    assert_eq!(
        run_renotify_pass(&state, Trigger::ClientOnline, Utc::now())
            .await
            .unwrap(),
        PassOutcome::Held(Hold::NothingDue)
    );

    let later = Utc::now() + chrono::Duration::hours(1);
    let outcome = run_renotify_pass(&state, Trigger::ClientOnline, later)
        .await
        .unwrap();
    let PassOutcome::Fired { batch, message_ids } = outcome else {
        panic!("expected a banner, got {outcome:?}");
    };
    assert_eq!(message_ids, vec![message_id]);

    let frame = next_renotify(&mut body, &mut buf)
        .await
        .expect("a renotify frame on the Mac stream");
    assert_eq!(frame["renotify"], batch);
    assert_eq!(frame["trigger"], "client_online");
    assert_eq!(frame["items"][0]["message_id"], message_id);
    assert_eq!(frame["items"][0]["one_line"], "line 1");

    // The same message is now in its cooldown.
    assert_eq!(
        run_renotify_pass(&state, Trigger::ClientOnline, later)
            .await
            .unwrap(),
        PassOutcome::Held(Hold::NothingDue)
    );

    // A tap naming a message the banner did not carry is refused; the real
    // one lands.
    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            &format!("/client/renotify/{batch}/opened"),
            json!({"message_id": message_id + 100}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            &format!("/client/renotify/{batch}/opened"),
            json!({"message_id": message_id}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    // A tap with no body is fine too, and idempotent.
    let resp = app
        .clone()
        .oneshot(authed("POST", &format!("/client/renotify/{batch}/opened")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/renotify/stats"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let stats = body_json(resp).await;
    assert_eq!(stats["summary"]["by_trigger"]["client_online"]["fired"], 1);
    assert_eq!(stats["summary"]["by_trigger"]["client_online"]["opened"], 1);
    assert_eq!(stats["zone"], "UTC");
}

#[tokio::test]
async fn a_meeting_holds_the_banner() {
    let (state, app, store, acct) = setup(true);
    notified(&store, acct, 1);
    let later = Utc::now() + chrono::Duration::hours(1);
    // A meeting around `later`, on the primary calendar.
    seed_meeting(
        &store,
        acct,
        later - chrono::Duration::minutes(10),
        later + chrono::Duration::minutes(20),
    );
    let _stream = app
        .clone()
        .oneshot(authed("GET", "/client/events?opens=1"))
        .await
        .unwrap();
    assert_eq!(
        run_renotify_pass(&state, Trigger::ClientOnline, later)
            .await
            .unwrap(),
        PassOutcome::Held(Hold::InMeeting)
    );
}

fn seed_meeting(
    store: &SqliteStore,
    acct: i64,
    start: chrono::DateTime<Utc>,
    end: chrono::DateTime<Utc>,
) {
    use squelch_core::gcal::{CalendarMeta, EventRow, PRIMARY};
    use squelch_core::triage::ghost::EventTime;
    store
        .calendar_set_selection(acct, &[CalendarMeta::primary_only()])
        .unwrap();
    let row = EventRow {
        event_id: "standup".into(),
        ical_uid: None,
        tentative: false,
        summary: Some("Standup".into()),
        location: None,
        start: EventTime::At(start.fixed_offset()),
        end: Some(EventTime::At(end.fixed_offset())),
        start_timezone: None,
        organizer_email: None,
        organizer_unreadable: false,
        from_gmail: false,
        organizer_self: false,
        self_response: None,
        transparent: false,
        html_link: None,
        updated_at: None,
        color_id: None,
        description: None,
        guests: Vec::new(),
        guests_truncated: false,
        meeting: None,
    };
    assert!(store.calendar_put_event(acct, PRIMARY, &row).unwrap());
}

/// THE ARRIVAL PATH, end to end: a Mac that was away longer than
/// `min_offline_mins` connects, the stream wakes the worker, and once the
/// connection has settled a banner arrives on it.
#[tokio::test]
async fn a_mac_coming_back_gets_the_banner_from_the_worker() {
    let (state, store, acct) = state_with(|_, _| {});
    let state = state
        .with_features(Features::pinned([features::RENOTIFY]))
        .with_renotify_config(RenotifyConfig {
            min_age_mins: 0,
            online_settle_secs: 0,
            ..always_on()
        });
    let app = router(state.clone());
    let message_id = notified(&store, acct, 1);
    // Away three hours.
    let (old, _) = store
        .open_client_session(acct, "macos", Utc::now() - chrono::Duration::hours(4))
        .unwrap();
    store
        .close_client_session(acct, old, Utc::now() - chrono::Duration::hours(3))
        .unwrap();

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(squelch_api::renotify_worker(
        state,
        shutdown_rx,
        squelch_api::RenotifyTiming {
            tick: Duration::from_secs(3600),
        },
    ));
    // Let the worker close stale sessions before the Mac connects.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/events?opens=1"))
        .await
        .unwrap();
    let mut body = resp.into_body();
    let mut buf = String::new();
    let frame = next_renotify(&mut body, &mut buf)
        .await
        .expect("the worker's banner");
    assert_eq!(frame["trigger"], "client_online");
    assert_eq!(frame["items"][0]["message_id"], message_id);

    shutdown_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .expect("the worker stops on shutdown")
        .unwrap();
}

/// A reconnect after a short absence is no arrival: no banner.
#[tokio::test]
async fn a_short_absence_is_not_an_arrival() {
    let (state, store, acct) = state_with(|_, _| {});
    let state = state
        .with_features(Features::pinned([features::RENOTIFY]))
        .with_renotify_config(RenotifyConfig {
            min_age_mins: 0,
            online_settle_secs: 0,
            ..always_on()
        });
    let app = router(state.clone());
    notified(&store, acct, 1);
    let (old, _) = store
        .open_client_session(acct, "macos", Utc::now() - chrono::Duration::minutes(20))
        .unwrap();
    store
        .close_client_session(acct, old, Utc::now() - chrono::Duration::minutes(10))
        .unwrap();
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(squelch_api::renotify_worker(
        state,
        shutdown_rx,
        squelch_api::RenotifyTiming {
            tick: Duration::from_secs(3600),
        },
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/events?opens=1"))
        .await
        .unwrap();
    let mut body = resp.into_body();
    let mut buf = String::new();
    assert!(next_renotify(&mut body, &mut buf).await.is_none());
}

/// A Wi-Fi handoff during the settle: the stream that woke the worker drops
/// and another opens (reporting no gap, so no second wake). The owner is still
/// there, so the banner reaches the replacement.
#[tokio::test]
async fn a_reconnect_during_the_settle_still_gets_the_banner() {
    let (state, store, acct) = state_with(|_, _| {});
    let state = state
        .with_features(Features::pinned([features::RENOTIFY]))
        .with_renotify_config(RenotifyConfig {
            min_age_mins: 0,
            online_settle_secs: 1,
            ..always_on()
        });
    let app = router(state.clone());
    let message_id = notified(&store, acct, 1);
    let (old, _) = store
        .open_client_session(acct, "macos", Utc::now() - chrono::Duration::hours(4))
        .unwrap();
    store
        .close_client_session(acct, old, Utc::now() - chrono::Duration::hours(3))
        .unwrap();
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(squelch_api::renotify_worker(
        state,
        shutdown_rx,
        squelch_api::RenotifyTiming {
            tick: Duration::from_secs(3600),
        },
    ));

    let first = app
        .clone()
        .oneshot(authed("GET", "/client/events?opens=1"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(first);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/events?opens=1"))
        .await
        .unwrap();
    let mut body = resp.into_body();
    let mut buf = String::new();
    let frame = next_renotify(&mut body, &mut buf)
        .await
        .expect("the banner reaches the replacement connection");
    assert_eq!(frame["items"][0]["message_id"], message_id);
}

/// A card on the For your eyes board from its own sender, committed the way
/// the triage worker commits one. `action` is an open action on it, if any.
fn fye_card(
    store: &SqliteStore,
    acct: i64,
    label: &str,
    action: Option<squelch_core::triage::decision::ActionKind>,
) -> i64 {
    use squelch_core::store::agent_triage::{AgentCommitOutcome, AgentTriageStore};
    use squelch_core::triage::decision::*;
    let mut m = common::msg(acct, label, &format!("t-{label}"), "Notice", "A notice");
    m.from_addr = format!("{label}@notices.example");
    let id = store.upsert_message(&m).unwrap();
    store.enqueue_agent_triage(acct, id, "test", false).unwrap();
    let job = store
        .claim_agent_job(acct, "triage", Utc::now(), 60)
        .unwrap()
        .unwrap();
    assert_eq!(job.message_id, id);
    let context = store.load_agent_context(&job).unwrap();
    let evidence = vec![EvidenceRef {
        message_id: id,
        location: "body".into(),
    }];
    let decision = MessageDecision {
        kinds: vec![EmailKind::General],
        summary: format!("Notice {label}"),
        reason: "A notice".into(),
        external_access: AccessAssessment {
            restricted: false,
            reason: "Nothing secret".into(),
            evidence: evidence.clone(),
        },
        attention: ThreadAttentionDecision {
            show_in_fye: true,
            state: if action.is_some() {
                AttentionState::NeedsUser
            } else {
                AttentionState::Informational
            },
            actions: action
                .into_iter()
                .map(|kind| AttentionAction {
                    kind,
                    description: "Answer it".into(),
                    evidence: evidence.clone(),
                    ..Default::default()
                })
                .collect(),
            summary: format!("Notice {label}"),
            relevant_message_ids: vec![id],
            evidence,
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

/// The next frame of SSE event `event` on a body, skipping anything else.
async fn next_frame(body: &mut Body, buf: &mut String, event: &str) -> Option<Value> {
    let want = format!("event: {event}");
    let deadline = tokio::time::Instant::now() + READ_TIMEOUT;
    loop {
        while let Some(end) = buf.find("\n\n") {
            let raw = buf[..end].to_string();
            buf.drain(..end + 2);
            if raw.lines().any(|l| l == want) {
                assert!(
                    !raw.lines().any(|l| l.starts_with("id:")),
                    "a banner moves no cursor"
                );
                let data: String = raw
                    .lines()
                    .filter_map(|l| l.strip_prefix("data:"))
                    .map(str::trim_start)
                    .collect();
                return Some(serde_json::from_str(&data).unwrap());
            }
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, body.frame()).await {
            Err(_) | Ok(None) => return None,
            Ok(Some(chunk)) => {
                if let Some(data) = chunk.unwrap().data_ref() {
                    buf.push_str(std::str::from_utf8(data).unwrap());
                }
            }
        }
    }
}

/// QUICK CLEAR, end to end: a backlog with enough quick items, a Mac
/// listening, a pass, a `quick_clear` frame carrying the queue and the
/// counts, a tap on the same route, and the stats that read it back.
#[tokio::test]
async fn a_quick_clear_reaches_the_mac_with_its_queue_and_its_tap_is_counted() {
    use squelch_core::triage::decision::ActionKind;
    let (state, app, store, acct) = setup(true);
    let quick: Vec<i64> = (1..=5)
        .map(|n| fye_card(&store, acct, &format!("q{n}"), None))
        .collect();
    let slow: Vec<i64> = (1..=3)
        .map(|n| fye_card(&store, acct, &format!("r{n}"), Some(ActionKind::Reply)))
        .collect();

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/events?opens=1"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut body = resp.into_body();
    let mut buf = String::new();
    let _ = store
        .open_client_session(acct, "macos", Utc::now())
        .unwrap();

    let now = Utc::now();
    let outcome = run_renotify_pass(&state, Trigger::ClientOnline, now)
        .await
        .unwrap();
    let PassOutcome::QuickClear { batch, message_ids } = outcome else {
        panic!("expected a quick clear, got {outcome:?}");
    };
    let mut got = message_ids.clone();
    got.sort();
    assert_eq!(got, quick, "the replies are never in the queue");

    let frame = next_frame(&mut body, &mut buf, "quick_clear")
        .await
        .expect("a quick_clear frame on the Mac stream");
    assert_eq!(frame["quick_clear"], batch);
    assert_eq!(frame["trigger"], "client_online");
    assert_eq!(frame["at"], now.timestamp());
    assert_eq!(frame["budget_secs"], 300);
    assert_eq!(frame["fit_count"], 5);
    assert_eq!(frame["open_count"], 8, "the replies count in the backlog");
    assert_eq!(frame["smart_clear_count"], 0);
    assert_eq!(frame["est_secs"], 5 * 8);
    let items = frame["items"].as_array().unwrap();
    assert_eq!(items.len(), 5);
    let first = &items[0];
    assert_eq!(first["message_id"], message_ids[0]);
    assert_eq!(first["thread_id"], "t-q5", "equal costs go newest first");
    assert_eq!(first["est_secs"], 8);
    assert_eq!(first["basis"], "heuristic");
    assert_eq!(first["kind"], "glance");
    assert_eq!(first["sender"], "q5@notices.example");
    assert_eq!(first["one_line"], "Notice q5");
    assert!(
        !items
            .iter()
            .any(|i| slow.contains(&i["message_id"].as_i64().unwrap()))
    );

    // At most one quick clear every few hours.
    assert_eq!(
        run_renotify_pass(&state, Trigger::ClientOnline, now)
            .await
            .unwrap(),
        PassOutcome::Held(Hold::NothingDue)
    );

    // The tap reuses the re-notify route.
    let resp = app
        .clone()
        .oneshot(authed_json(
            "POST",
            &format!("/client/renotify/{batch}/opened"),
            json!({"message_id": message_ids[0]}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .clone()
        .oneshot(authed("GET", "/client/renotify/stats"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let stats = body_json(resp).await;
    assert_eq!(stats["summary"]["by_kind"]["quick_clear"]["fired"], 1);
    assert_eq!(stats["summary"]["by_kind"]["quick_clear"]["opened"], 1);
    assert_eq!(stats["summary"]["muted_quick_clear"], json!([]));
    let heuristic = &stats["quick_clear"]["calibration"]["by_basis"]["heuristic"];
    assert_eq!(heuristic["items"], 5);
    assert_eq!(stats["quick_clear"]["calibration"]["multiplier"], 1.0);
}

#[tokio::test]
async fn too_few_quick_items_send_no_quick_clear() {
    let (state, app, store, acct) = setup(true);
    for n in 1..=4 {
        fye_card(&store, acct, &format!("q{n}"), None);
    }
    let _stream = app
        .clone()
        .oneshot(authed("GET", "/client/events?opens=1"))
        .await
        .unwrap();
    assert_eq!(
        run_renotify_pass(&state, Trigger::ClientOnline, Utc::now())
            .await
            .unwrap(),
        PassOutcome::Held(Hold::NothingDue)
    );
}
