//! Read-only Google Calendar sync, end to end against a local fake of the
//! Calendar API: the sync loop (`squelch_core::gcal::sync`) fills the store,
//! and `GET /client/timeline` serves what it stored beside the ghosts.
//! Nothing here reaches the network: every call goes to a fake on loopback.
mod common;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use common::*;
use serde_json::{Value, json};
use squelch_api::router;
use squelch_core::config::{
    CALENDAR_EVENTS_SCOPE, CALENDAR_LIST_SCOPE, CredentialBackend, GMAIL_READONLY_SCOPE,
    OAuthClientConfig,
};
use squelch_core::credentials::{CalendarCredentials, CredentialKind, StoredToken};
use squelch_core::gcal::sync::{CalendarSync, PassOutcome};
use squelch_core::gcal::{CalendarErrorKind, PRIMARY};
use squelch_core::store::{
    SqliteStore, Store,
    agent_triage::{AgentCommitOutcome, AgentTriageStore},
};
use squelch_core::triage::decision::*;
use squelch_core::types::NewMessage;
use tower::ServiceExt;

// --- the fake Calendar API ---------------------------------------------------

/// What the fake answers. Every events request is recorded with its query.
#[derive(Default)]
struct Google {
    /// `calendarList` items.
    list: Vec<Value>,
    /// Answer every `calendarList` call with this status instead.
    list_status: Option<u16>,
    /// A full sync's pages, per calendar id as Google sees it.
    full: HashMap<String, Vec<Vec<Value>>>,
    /// Incremental answers: calendar -> sync token in -> (items, token out).
    /// A token with no entry is answered 410.
    changes: HashMap<String, HashMap<String, (Vec<Value>, String)>>,
    /// Answer every events call with this status and body.
    fail: Option<(u16, Value)>,
    /// Answer every events call 200 with this raw body.
    raw: Option<String>,
    /// A full sync whose pages never end.
    endless: bool,
    /// (calendar id, query) of every events request, in order.
    requests: Vec<(String, HashMap<String, String>)>,
    /// The bearer of every request.
    bearers: Vec<String>,
}
type Fake = Arc<Mutex<Google>>;

fn bearer(headers: &axum::http::HeaderMap) -> String {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

async fn list_route(State(g): State<Fake>, headers: axum::http::HeaderMap) -> Response {
    let mut g = g.lock().unwrap();
    g.bearers.push(bearer(&headers));
    if let Some(status) = g.list_status {
        let body =
            json!({"error": {"code": status, "errors": [{"reason": "insufficientPermissions"}]}});
        return (StatusCode::from_u16(status).unwrap(), Json(body)).into_response();
    }
    Json(json!({"kind": "calendar#calendarList", "items": g.list})).into_response()
}

async fn events_route(
    State(g): State<Fake>,
    Path(calendar): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
) -> Response {
    let mut g = g.lock().unwrap();
    g.bearers.push(bearer(&headers));
    g.requests.push((calendar.clone(), q.clone()));
    if let Some((status, body)) = &g.fail {
        return (StatusCode::from_u16(*status).unwrap(), Json(body.clone())).into_response();
    }
    if let Some(raw) = &g.raw {
        return (StatusCode::OK, raw.clone()).into_response();
    }
    let meta = json!({"summary": format!("Calendar {calendar}"), "timeZone": "America/New_York"});
    let mut body = meta.clone();
    if let Some(token) = q.get("syncToken") {
        let Some((items, next)) = g.changes.get(&calendar).and_then(|m| m.get(token)) else {
            return (
                StatusCode::GONE,
                Json(json!({"error": {"code": 410, "errors": [{"reason": "fullSyncRequired"}]}})),
            )
                .into_response();
        };
        body["items"] = json!(items);
        body["nextSyncToken"] = json!(next);
        return Json(body).into_response();
    }
    let n: usize = q.get("pageToken").and_then(|p| p.parse().ok()).unwrap_or(0);
    if g.endless {
        body["items"] = json!([timed(
            &format!("endless-{n}"),
            "Again",
            Utc::now() + Duration::days(3),
            30
        )]);
        body["nextPageToken"] = json!((n + 1).to_string());
        return Json(body).into_response();
    }
    let pages = g.full.get(&calendar).cloned().unwrap_or_default();
    body["items"] = json!(pages.get(n).cloned().unwrap_or_default());
    if n + 1 < pages.len() {
        body["nextPageToken"] = json!((n + 1).to_string());
    } else {
        body["nextSyncToken"] = json!(format!("tok-{calendar}-1"));
    }
    Json(body).into_response()
}

async fn spawn_google(g: Fake) -> String {
    let app = axum::Router::new()
        .route("/users/me/calendarList", axum::routing::get(list_route))
        .route("/calendars/{id}/events", axum::routing::get(events_route))
        .with_state(g);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

// --- fixtures ----------------------------------------------------------------

/// RFC3339 in New York summer time, the shape Google sends for a calendar in
/// that zone.
fn rfc(at: DateTime<Utc>) -> String {
    at.with_timezone(&chrono::FixedOffset::west_opt(4 * 3600).unwrap())
        .format("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}
/// UTC with `Z`, safe in a query string as is.
fn q(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// A whole minute, so a fixture's start round-trips through text exactly.
fn minute(at: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp(at.timestamp() / 60 * 60, 0).unwrap()
}

fn timed(id: &str, title: &str, start: DateTime<Utc>, minutes: i64) -> Value {
    json!({
        "id": id,
        "status": "confirmed",
        "summary": title,
        "start": {"dateTime": rfc(start), "timeZone": "America/New_York"},
        "end": {"dateTime": rfc(start + Duration::minutes(minutes))},
        "htmlLink": format!("https://www.google.com/calendar/event?eid={id}"),
        "iCalUID": format!("{id}@google.com"),
        "updated": "2026-09-30T10:00:00.000Z",
    })
}

fn all_day(id: &str, title: &str, day: NaiveDate, days: i64) -> Value {
    json!({
        "id": id,
        "summary": title,
        "start": {"date": day.format("%Y-%m-%d").to_string()},
        "end": {"date": (day + Duration::days(days)).format("%Y-%m-%d").to_string()},
    })
}

/// A credentials file in the daemon's own shape: one hosted token in the
/// Read and Write slots with `scopes`, valid for an hour (no refresh).
fn write_grant(path: &std::path::Path, scopes: &[&str]) {
    let token = StoredToken {
        access_token: "cal-access".into(),
        refresh_token: Some("r".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        scopes: Some(scopes.iter().map(|s| s.to_string()).collect()),
    };
    squelch_core::credentials::store_tokens_backend(
        CredentialBackend::File,
        path,
        "me@example.com",
        &[
            (CredentialKind::Read, token.clone()),
            (CredentialKind::Write, token),
        ],
    )
    .unwrap();
}

const GRANTED: &[&str] = &[GMAIL_READONLY_SCOPE, CALENDAR_EVENTS_SCOPE];
const GRANTED_WITH_LIST: &[&str] = &[
    GMAIL_READONLY_SCOPE,
    CALENDAR_EVENTS_SCOPE,
    CALENDAR_LIST_SCOPE,
];

struct World {
    store: Arc<SqliteStore>,
    acct: i64,
    app: axum::Router,
    sync: CalendarSync,
    google: Fake,
    base: String,
    creds: Arc<CalendarCredentials>,
    path: PathBuf,
    metrics: Arc<squelch_core::metrics::SyncMetrics>,
}

impl Drop for World {
    fn drop(&mut self) {
        std::fs::remove_file(&self.path).ok();
    }
}

fn cred_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "squelch-calsync-{tag}-{}-{}.json",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ))
}

/// A store, one account, a fake Google, the sync loop over them, and the
/// human door with the same resolver and the loop's kick. `scopes` is what
/// the stored token was granted.
async fn world(tag: &str, scopes: &[&str]) -> World {
    let (state, store, acct) = state_with(|_, _| {});
    let path = cred_path(tag);
    write_grant(&path, scopes);
    let creds = Arc::new(CalendarCredentials::new(
        acct,
        "me@example.com".into(),
        CredentialBackend::File,
        path.clone(),
        OAuthClientConfig {
            client_id: "id".into(),
            client_secret: "secret".into(),
        },
    ));
    let google: Fake = Arc::default();
    let base = spawn_google(google.clone()).await;
    let sync = CalendarSync::with_api_base(store.clone(), acct, creds.clone(), &base)
        .unwrap()
        .with_test_timing(StdDuration::from_millis(50));
    let metrics = squelch_core::metrics::SyncMetrics::new();
    let app = router(
        state
            .with_calendar_credentials((*creds).clone())
            .with_calendar_sync_kick(sync.kick_handle())
            .with_sync_metrics(metrics.clone()),
    );
    World {
        store,
        acct,
        app,
        sync,
        google,
        base,
        creds,
        path,
        metrics,
    }
}

impl World {
    fn google(&self) -> std::sync::MutexGuard<'_, Google> {
        self.google.lock().unwrap()
    }
    async fn pass(&self) -> PassOutcome {
        self.sync.pass(Utc::now()).await
    }
    async fn timeline(&self) -> Value {
        let uri = format!(
            "/client/timeline?from={}&to={}",
            q(Utc::now() - Duration::days(40)),
            q(Utc::now() + Duration::days(359)),
        );
        get(&self.app, &uri).await
    }
    async fn events(&self) -> Vec<Value> {
        self.timeline().await["events"].as_array().unwrap().clone()
    }
    async fn titles(&self) -> Vec<String> {
        self.events()
            .await
            .iter()
            .map(|e| e["title"].as_str().unwrap_or("").to_string())
            .collect()
    }
}

async fn get(app: &axum::Router, uri: &str) -> Value {
    let resp = app.clone().oneshot(authed("GET", uri)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "GET {uri}");
    body_json(resp).await
}

async fn refresh(app: &axum::Router) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(authed("POST", "/client/timeline/refresh"))
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

/// A committed triage decision carrying one `event` record, the way
/// production makes a ghost. Returns nothing: the ghost is found by title.
fn seed_ghost(store: &SqliteStore, account: i64, gmail: &str, title: &str, start: String) {
    let id = store
        .upsert_message(&NewMessage {
            received_at: Utc::now() - Duration::minutes(5),
            auth_pass: Some(true),
            ..msg(
                account,
                gmail,
                &format!("t-{gmail}"),
                "Your plans",
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
    let record = RecordProposal::Event {
        title: title.into(),
        start: Some(SupportedTime {
            value: start,
            timezone: Some("America/New_York".into()),
            source_message_id: id,
            interpreted_relative: false,
        }),
        location: None,
        end: None,
        duration_estimate: None,
        event_kind: EventRecordKind::Reservation,
        commitment: EventCommitment::Committed,
        change: EventChange::Created,
        identity_refs: vec![format!("REF{gmail}12345")],
        likelihood: None,
        evidence: evidence.clone(),
    };
    let decision = MessageDecision {
        kinds: vec![EmailKind::EventReservation],
        summary: "Plans".into(),
        reason: "An event".into(),
        records: vec![record],
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
}

fn sorted_keys(v: &Value) -> Vec<String> {
    let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    keys
}

// --- first sync, incremental, 410, cancellation ------------------------------

#[tokio::test]
async fn a_first_sync_reads_the_window_in_full_and_serves_every_field() {
    let w = world("first", GRANTED).await;
    let start = minute(Utc::now() + Duration::days(3));
    let day = (Utc::now() + Duration::days(5)).date_naive();
    let mut meeting = timed("evt-1", "Design review", start, 45);
    meeting["location"] = json!("Zoom: https://zoom.us/j/123?pwd=secret");
    meeting["organizer"] = json!({"email": "boss@example.com"});
    meeting["attendees"] = json!([
        {"email": "boss@example.com", "responseStatus": "accepted"},
        {"email": "me@example.com", "self": true, "responseStatus": "tentative"}
    ]);
    meeting["transparency"] = json!("transparent");
    meeting["status"] = json!("tentative");
    w.google().full.insert(
        PRIMARY.into(),
        vec![vec![
            meeting,
            all_day("evt-2", "Offsite", day, 3),
            all_day("evt-3", "Holiday", day + Duration::days(1), 1),
        ]],
    );

    // Before any pass: granted, never synced, no error.
    let before = w.timeline().await;
    eprintln!("calendar, granted and never synced: {}", before["calendar"]);
    assert_eq!(
        before["calendar"],
        json!({"granted": true, "synced_at": null, "error": null})
    );

    assert_eq!(w.pass().await, PassOutcome::Synced);
    {
        let g = w.google();
        assert_eq!(g.requests.len(), 1);
        let (cal, query) = &g.requests[0];
        assert_eq!(cal, PRIMARY);
        assert_eq!(query["singleEvents"], "true");
        assert_eq!(query["maxResults"], "250");
        assert!(query.contains_key("timeMin") && query.contains_key("timeMax"));
        assert!(!query.contains_key("syncToken"));
        let min = DateTime::parse_from_rfc3339(&query["timeMin"]).unwrap();
        let max = DateTime::parse_from_rfc3339(&query["timeMax"]).unwrap();
        assert_eq!((max - min).num_days(), 430, "30 days back, 400 ahead");
        assert!(g.bearers.iter().all(|b| b == "Bearer cal-access"));
    }

    let body = w.timeline().await;
    assert_eq!(sorted_keys(&body), ["calendar", "events", "ghosts"]);
    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 3);
    let event = &events[0];
    eprintln!(
        "served event: {}",
        serde_json::to_string_pretty(event).unwrap()
    );
    assert_eq!(
        sorted_keys(event),
        [
            "all_day",
            "busy",
            "calendar_id",
            "calendar_name",
            "end",
            "html_link",
            "id",
            "location",
            "organizer",
            "self_response",
            "start",
            "start_timezone",
            "status",
            "title"
        ]
    );
    assert_eq!(
        *event,
        json!({
            "id": "evt-1",
            "calendar_id": "primary",
            "calendar_name": "Calendar primary",
            "title": "Design review",
            "start": rfc(start),
            "end": rfc(start + Duration::minutes(45)),
            "all_day": false,
            "start_timezone": "America/New_York",
            "location": "Zoom: zoom.us/j/123",
            "status": "tentative",
            "self_response": "tentative",
            "busy": false,
            "organizer": "boss@example.com",
            "html_link": "https://www.google.com/calendar/event?eid=evt-1",
        })
    );
    // The all-day ones: dates, the INCLUSIVE last day as the end (Google
    // sent the day after), no zone, the user's own.
    let offsite = &events[1];
    eprintln!("served three-day all-day event: {offsite}");
    assert_eq!(offsite["all_day"], true);
    assert_eq!(offsite["start"], day.format("%Y-%m-%d").to_string());
    assert_eq!(
        offsite["end"],
        (day + Duration::days(2)).format("%Y-%m-%d").to_string(),
        "the 3rd day, not Google's exclusive 4th"
    );
    let holiday = &events[2];
    eprintln!("served one-day all-day event: {holiday}");
    assert_eq!(holiday["title"], "Holiday");
    assert_eq!(
        holiday["start"],
        (day + Duration::days(1)).format("%Y-%m-%d").to_string()
    );
    assert_eq!(
        holiday["end"],
        Value::Null,
        "a one-day event ends the day it starts"
    );
    assert_eq!(offsite["start_timezone"], Value::Null);
    assert_eq!(offsite["self_response"], Value::Null);
    assert_eq!(offsite["status"], "confirmed");
    assert_eq!(offsite["busy"], true);

    let status = &body["calendar"];
    eprintln!("calendar, synced: {status}");
    assert_eq!(status["granted"], true);
    assert_eq!(status["error"], Value::Null);
    let synced = DateTime::parse_from_rfc3339(status["synced_at"].as_str().unwrap()).unwrap();
    assert!((Utc::now() - synced.with_timezone(&Utc)).num_seconds() < 60);
}

#[tokio::test]
async fn the_next_sync_sends_the_token_alone_and_applies_the_changes() {
    let w = world("incremental", GRANTED).await;
    let start = minute(Utc::now() + Duration::days(3));
    w.google().full.insert(
        PRIMARY.into(),
        vec![vec![
            timed("a", "Standup", start, 15),
            timed("b", "Lunch", start + Duration::hours(3), 60),
        ]],
    );
    w.google().changes.insert(
        PRIMARY.into(),
        HashMap::from([(
            "tok-primary-1".to_string(),
            (
                vec![
                    timed("a", "Standup (moved)", start + Duration::hours(1), 15),
                    timed("c", "Gym", start + Duration::hours(5), 60),
                ],
                "tok-primary-2".to_string(),
            ),
        )]),
    );
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(w.pass().await, PassOutcome::Synced);
    {
        let g = w.google();
        let (_, query) = &g.requests[1];
        assert_eq!(query["syncToken"], "tok-primary-1");
        assert_eq!(
            query["singleEvents"], "true",
            "same parameters as the first"
        );
        for refused in [
            "timeMin",
            "timeMax",
            "updatedMin",
            "orderBy",
            "q",
            "iCalUID",
        ] {
            assert!(
                !query.contains_key(refused),
                "{refused} beside a sync token"
            );
        }
    }
    assert_eq!(w.titles().await, ["Standup (moved)", "Lunch", "Gym"]);
    let stored = w.store.calendar_calendars(w.acct).unwrap();
    assert_eq!(stored[0].sync_token.as_deref(), Some("tok-primary-2"));
}

#[tokio::test]
async fn a_410_forgets_the_token_and_resyncs_in_full_in_the_same_pass() {
    let w = world("gone", GRANTED).await;
    let start = minute(Utc::now() + Duration::days(3));
    w.google().full.insert(
        PRIMARY.into(),
        vec![vec![
            timed("keep", "Kept", start, 30),
            timed(
                "gone",
                "Deleted while the token was dead",
                start + Duration::hours(1),
                30,
            ),
        ]],
    );
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(w.events().await.len(), 2);

    // Google invalidates the token (no incremental answer => 410), and the
    // calendar has lost an event in the meantime.
    w.google()
        .full
        .insert(PRIMARY.into(), vec![vec![timed("keep", "Kept", start, 30)]]);
    assert_eq!(w.pass().await, PassOutcome::Synced);
    {
        let g = w.google();
        let queries: Vec<_> = g.requests.iter().map(|(_, q)| q.clone()).collect();
        assert_eq!(queries.len(), 3);
        assert_eq!(
            queries[1]["syncToken"], "tok-primary-1",
            "the incremental try"
        );
        assert!(
            queries[2].contains_key("timeMin"),
            "then a full read, same pass"
        );
        assert!(!queries[2].contains_key("syncToken"));
    }
    assert_eq!(
        w.titles().await,
        ["Kept"],
        "the full read swept what it did not see"
    );
    let row = &w.store.calendar_calendars(w.acct).unwrap()[0];
    assert_eq!(row.sync_token.as_deref(), Some("tok-primary-1"));
    assert_eq!(row.last_error_kind, None);
}

#[tokio::test]
async fn a_cancelled_event_is_removed() {
    let w = world("cancel", GRANTED).await;
    let start = minute(Utc::now() + Duration::days(3));
    w.google().full.insert(
        PRIMARY.into(),
        vec![vec![
            timed("x", "Cancelled later", start, 30),
            timed("y", "Stays", start + Duration::hours(1), 30),
            timed("z", "Organizer cancels", start + Duration::hours(2), 30),
        ]],
    );
    // How Google reports a deletion in an incremental sync: usually only the
    // id and status, but on the organizer's calendar the whole event, start
    // included ("so they can be restored"), which must not read as an update.
    let mut full_detail = timed("z", "Organizer cancels", start + Duration::hours(2), 30);
    full_detail["status"] = json!("cancelled");
    w.google().changes.insert(
        PRIMARY.into(),
        HashMap::from([(
            "tok-primary-1".to_string(),
            (
                vec![json!({"id": "x", "status": "cancelled"}), full_detail],
                "tok-primary-2".to_string(),
            ),
        )]),
    );
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(
        w.titles().await,
        ["Cancelled later", "Stays", "Organizer cancels"]
    );
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(w.titles().await, ["Stays"]);
}

#[tokio::test]
async fn an_event_that_moves_out_of_the_window_is_removed() {
    let w = world("outside", GRANTED).await;
    let start = minute(Utc::now() + Duration::days(3));
    w.google().full.insert(
        PRIMARY.into(),
        vec![vec![timed("m", "Moves far", start, 30)]],
    );
    w.google().changes.insert(
        PRIMARY.into(),
        HashMap::from([(
            "tok-primary-1".to_string(),
            (
                vec![timed("m", "Moves far", start + Duration::days(500), 30)],
                "tok-primary-2".to_string(),
            ),
        )]),
    );
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert!(w.events().await.is_empty());
    let count: Vec<_> = w
        .store
        .list_calendar_events(
            w.acct,
            (Utc::now() - Duration::days(60)).fixed_offset(),
            (Utc::now() + Duration::days(700)).fixed_offset(),
        )
        .unwrap();
    assert!(count.is_empty(), "not kept outside the window either");
}

#[tokio::test]
async fn a_recurring_events_instances_are_served_one_by_one() {
    let w = world("recurring", GRANTED).await;
    let first = minute(Utc::now() + Duration::days(2));
    // With singleEvents=true Google sends each instance, never the series.
    let instances: Vec<Value> = (0..3)
        .map(|i| {
            let mut e = timed(
                &format!("weekly_{i}"),
                "Weekly 1:1",
                first + Duration::days(7 * i),
                30,
            );
            e["recurringEventId"] = json!("weekly");
            e
        })
        .collect();
    w.google().full.insert(PRIMARY.into(), vec![instances]);
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(w.google().requests[0].1["singleEvents"], "true");
    let events = w.events().await;
    assert_eq!(events.len(), 3);
    let ids: Vec<_> = events.iter().map(|e| e["id"].clone()).collect();
    assert_eq!(
        ids,
        [json!("weekly_0"), json!("weekly_1"), json!("weekly_2")]
    );
}

#[tokio::test]
async fn paging_reads_every_page_and_takes_the_token_from_the_last() {
    let w = world("paging", GRANTED).await;
    let start = minute(Utc::now() + Duration::days(1));
    let pages: Vec<Vec<Value>> = (0..3)
        .map(|p| {
            (0..2)
                .map(|i| {
                    timed(
                        &format!("p{p}e{i}"),
                        &format!("Event {p}.{i}"),
                        start + Duration::hours(p * 2 + i),
                        30,
                    )
                })
                .collect()
        })
        .collect();
    w.google().full.insert(PRIMARY.into(), pages);
    assert_eq!(w.pass().await, PassOutcome::Synced);
    {
        let g = w.google();
        let tokens: Vec<_> = g
            .requests
            .iter()
            .map(|(_, q)| q.get("pageToken").cloned())
            .collect();
        assert_eq!(tokens, [None, Some("1".into()), Some("2".into())]);
        assert!(
            g.requests.iter().all(|(_, q)| q.contains_key("timeMin")),
            "one query, paged"
        );
    }
    assert_eq!(w.events().await.len(), 6);
    assert_eq!(
        w.store.calendar_calendars(w.acct).unwrap()[0]
            .sync_token
            .as_deref(),
        Some("tok-primary-1")
    );
}

#[tokio::test]
async fn a_calendar_too_large_to_page_through_is_unavailable_and_bounded() {
    let w = world("endless", GRANTED).await;
    w.google().endless = true;
    assert_eq!(
        w.pass().await,
        PassOutcome::Failed(CalendarErrorKind::Unavailable)
    );
    assert_eq!(w.google().requests.len(), 40, "stopped at the page cap");
    let row = &w.store.calendar_calendars(w.acct).unwrap()[0];
    assert_eq!(row.sync_token, None, "no token without the last page");
    assert_eq!(row.last_error_kind, Some(CalendarErrorKind::Unavailable));
}

// --- calendars ---------------------------------------------------------------

fn list_entry(id: &str, summary: &str, selected: bool, primary: bool) -> Value {
    json!({"id": id, "summary": summary, "selected": selected, "primary": primary,
           "timeZone": "America/New_York", "accessRole": "reader"})
}

#[tokio::test]
async fn without_the_list_scope_only_the_primary_calendar_is_read() {
    let w = world("nolist", GRANTED).await;
    w.google().list = vec![list_entry(
        "team@group.calendar.google.com",
        "Team",
        true,
        false,
    )];
    assert_eq!(w.pass().await, PassOutcome::Synced);
    let g = w.google();
    assert_eq!(g.requests.len(), 1);
    assert_eq!(g.requests[0].0, PRIMARY);
    assert_eq!(g.bearers.len(), 1, "the list was never asked for");
}

#[tokio::test]
async fn a_calendar_the_user_stops_showing_leaves_the_timeline() {
    let w = world("deselect", GRANTED_WITH_LIST).await;
    let team = "team@group.calendar.google.com";
    let start = minute(Utc::now() + Duration::days(2));
    {
        let mut g = w.google();
        g.list = vec![
            list_entry("me@example.com", "Me", true, true),
            list_entry(team, "Team", true, false),
            list_entry("hidden@group.calendar.google.com", "Hidden", false, false),
        ];
        g.full
            .insert(PRIMARY.into(), vec![vec![timed("mine", "Mine", start, 30)]]);
        g.full.insert(
            team.into(),
            vec![vec![timed(
                "theirs",
                "Team offsite",
                start + Duration::hours(1),
                30,
            )]],
        );
    }
    assert_eq!(w.pass().await, PassOutcome::Synced);
    let events = w.events().await;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["calendar_id"], "primary");
    assert_eq!(events[0]["calendar_name"], "Me");
    assert_eq!(events[1]["calendar_id"], team);
    assert_eq!(events[1]["calendar_name"], "Team");
    let read: Vec<_> = w.google().requests.iter().map(|(c, _)| c.clone()).collect();
    assert_eq!(
        read,
        [PRIMARY, team],
        "the unselected calendar is never read"
    );

    w.google().list[1]["selected"] = json!(false);
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(w.titles().await, ["Mine"]);
    let rows = w.store.calendar_calendars(w.acct).unwrap();
    let team_row = rows.iter().find(|c| c.calendar_id == team).unwrap();
    assert!(!team_row.selected);
    assert_eq!(
        team_row.sync_token, None,
        "a later re-select reads it in full"
    );
}

#[tokio::test]
async fn at_most_25_calendars_are_read_the_primary_first() {
    let w = world("cap", GRANTED_WITH_LIST).await;
    {
        let mut g = w.google();
        g.list = (0..40)
            .map(|i| {
                list_entry(
                    &format!("c{i:02}@group.calendar.google.com"),
                    "Shared",
                    true,
                    false,
                )
            })
            .collect();
        g.list.push(list_entry("me@example.com", "Me", true, true));
    }
    assert_eq!(w.pass().await, PassOutcome::Synced);
    let read: Vec<_> = w.google().requests.iter().map(|(c, _)| c.clone()).collect();
    assert_eq!(read.len(), 25);
    assert_eq!(read[0], PRIMARY);
}

#[tokio::test]
async fn a_refused_list_falls_back_to_the_primary_calendar() {
    let w = world("listrefused", GRANTED_WITH_LIST).await;
    w.google().list_status = Some(403);
    assert_eq!(w.pass().await, PassOutcome::Synced);
    let read: Vec<_> = w.google().requests.iter().map(|(c, _)| c.clone()).collect();
    assert_eq!(read, [PRIMARY]);
}

#[tokio::test]
async fn an_unavailable_list_keeps_the_calendars_already_read() {
    let w = world("listdown", GRANTED_WITH_LIST).await;
    let team = "team@group.calendar.google.com";
    let start = minute(Utc::now() + Duration::days(2));
    {
        let mut g = w.google();
        g.list = vec![
            list_entry("me@example.com", "Me", true, true),
            list_entry(team, "Team", true, false),
        ];
        g.full
            .insert(team.into(), vec![vec![timed("t", "Team thing", start, 30)]]);
    }
    assert_eq!(w.pass().await, PassOutcome::Synced);
    w.google().list_status = Some(503);
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(
        w.titles().await,
        ["Team thing"],
        "a list outage drops nothing"
    );
}

// --- failures ----------------------------------------------------------------

#[tokio::test]
async fn a_403_marks_calendar_errored_and_keeps_the_rows() {
    let w = world("forbidden", GRANTED).await;
    let start = minute(Utc::now() + Duration::days(2));
    w.google().full.insert(
        PRIMARY.into(),
        vec![vec![timed("e", "Kept through errors", start, 30)]],
    );
    assert_eq!(w.pass().await, PassOutcome::Synced);

    w.google().fail = Some((
        403,
        json!({"error": {"code": 403, "message": "Request had insufficient authentication scopes.",
                         "errors": [{"reason": "insufficientPermissions"}], "status": "PERMISSION_DENIED"}}),
    ));
    assert_eq!(w.pass().await, PassOutcome::Failed(CalendarErrorKind::Auth));
    let body = w.timeline().await;
    eprintln!("calendar, errored: {}", body["calendar"]);
    assert_eq!(body["calendar"]["error"], "auth");
    assert_eq!(body["calendar"]["granted"], true);
    assert_eq!(w.titles().await, ["Kept through errors"]);

    // A rate-limit 403 is quota, not auth.
    w.google().fail = Some((
        403,
        json!({"error": {"code": 403, "errors": [{"reason": "rateLimitExceeded"}]}}),
    ));
    assert_eq!(
        w.pass().await,
        PassOutcome::Failed(CalendarErrorKind::Quota)
    );
    assert_eq!(w.timeline().await["calendar"]["error"], "quota");

    // The next good read clears it.
    w.google().fail = None;
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(w.timeline().await["calendar"]["error"], Value::Null);
}

#[tokio::test]
async fn a_network_error_leaves_the_old_rows_and_sets_the_error_kind() {
    let w = world("network", GRANTED).await;
    let start = minute(Utc::now() + Duration::days(2));
    w.google().full.insert(
        PRIMARY.into(),
        vec![vec![timed("e", "Still here", start, 30)]],
    );
    assert_eq!(w.pass().await, PassOutcome::Synced);
    let synced_before = w.timeline().await["calendar"]["synced_at"].clone();

    // The same store and grant, but Google is unreachable (nothing listens).
    let dead = CalendarSync::with_api_base(
        w.store.clone(),
        w.acct,
        w.creds.clone(),
        "http://127.0.0.1:1",
    )
    .unwrap();
    assert_eq!(
        dead.pass(Utc::now()).await,
        PassOutcome::Failed(CalendarErrorKind::Unavailable)
    );
    let body = w.timeline().await;
    assert_eq!(body["calendar"]["error"], "unavailable");
    assert_eq!(body["calendar"]["synced_at"], synced_before, "not a sync");
    assert_eq!(w.titles().await, ["Still here"]);
}

/// INVARIANT 5: whatever goes wrong with the calendar, Gmail's health on
/// `/client/stats` does not move.
#[tokio::test]
async fn no_calendar_failure_touches_gmail_health() {
    let w = world("isolation", GRANTED).await;
    let stats = |app: axum::Router| async move { get(&app, "/client/stats").await };
    let before = stats(w.app.clone()).await;
    assert_eq!(before["gmail"]["connected"], true);
    let fixed = |body: &Value| (body["gmail"].clone(), body["sync"].clone());

    type Setup = Box<dyn Fn(&mut Google)>;
    let modes: Vec<(&str, Setup)> = vec![
        (
            "401",
            Box::new(|g| g.fail = Some((401, json!({"error": {"code": 401}})))),
        ),
        (
            "403",
            Box::new(|g| g.fail = Some((403, json!({"error": {"code": 403}})))),
        ),
        ("500", Box::new(|g| g.fail = Some((500, json!({}))))),
        ("429", Box::new(|g| g.fail = Some((429, json!({}))))),
        (
            "malformed",
            Box::new(|g| {
                g.fail = None;
                g.raw = Some("{\"items\": [ not json".into());
            }),
        ),
        (
            "huge",
            Box::new(|g| {
                g.raw = None;
                g.endless = true;
            }),
        ),
    ];
    for (name, setup) in modes {
        setup(&mut w.google());
        assert!(
            matches!(w.pass().await, PassOutcome::Failed(_)),
            "{name} should fail the calendar pass"
        );
        let after = stats(w.app.clone()).await;
        assert_eq!(fixed(&after), fixed(&before), "{name} moved Gmail health");
    }
    // Unreachable, and a token whose refresh Google refuses.
    let dead = CalendarSync::with_api_base(
        w.store.clone(),
        w.acct,
        w.creds.clone(),
        "http://127.0.0.1:1",
    )
    .unwrap();
    assert!(matches!(
        dead.pass(Utc::now()).await,
        PassOutcome::Failed(_)
    ));
    let expired = StoredToken {
        access_token: "old".into(),
        refresh_token: None,
        expires_at: Some(Utc::now() - Duration::hours(1)),
        scopes: Some(GRANTED.iter().map(|s| s.to_string()).collect()),
    };
    squelch_core::credentials::store_token_file(
        &w.path,
        "me@example.com",
        CredentialKind::Calendar,
        &expired,
    )
    .unwrap();
    assert_eq!(w.pass().await, PassOutcome::Failed(CalendarErrorKind::Auth));
    let after = stats(w.app.clone()).await;
    assert_eq!(
        fixed(&after),
        fixed(&before),
        "a dead calendar token moved Gmail health"
    );
    assert_eq!(w.metrics.gmail_auth_failed_since(), None);
    // And not granted at all is no failure of anything. (A fresh file: the
    // Calendar slot above would otherwise still answer.)
    std::fs::remove_file(&w.path).ok();
    write_grant(&w.path, &[GMAIL_READONLY_SCOPE]);
    assert_eq!(w.pass().await, PassOutcome::NotGranted);
    assert_eq!(fixed(&stats(w.app.clone()).await), fixed(&before));
}

// --- the grant ---------------------------------------------------------------

#[tokio::test]
async fn without_a_grant_nothing_is_called_or_served_and_the_status_says_so() {
    let w = world("nogrant", &[GMAIL_READONLY_SCOPE]).await;
    w.google().full.insert(
        PRIMARY.into(),
        vec![vec![timed(
            "e",
            "Never",
            Utc::now() + Duration::days(1),
            30,
        )]],
    );
    assert_eq!(w.pass().await, PassOutcome::NotGranted);
    assert!(w.google().bearers.is_empty(), "Google was never called");
    let body = w.timeline().await;
    eprintln!("calendar, not granted: {}", body["calendar"]);
    assert_eq!(
        body["calendar"],
        json!({"granted": false, "synced_at": null, "error": null})
    );
    assert_eq!(body["events"], json!([]));
}

#[tokio::test]
async fn a_grant_taken_back_hides_what_was_synced() {
    let w = world("revoked", GRANTED).await;
    w.google().full.insert(
        PRIMARY.into(),
        vec![vec![timed(
            "e",
            "Synced",
            minute(Utc::now() + Duration::days(1)),
            30,
        )]],
    );
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(w.events().await.len(), 1);
    write_grant(&w.path, &[GMAIL_READONLY_SCOPE]);
    let body = w.timeline().await;
    assert_eq!(body["calendar"]["granted"], false);
    assert_eq!(body["events"], json!([]));
}

/// The loop runs whether or not calendar is granted: it idles without calling
/// Google, and starts reading on its own once a grant appears.
#[tokio::test]
async fn the_loop_idles_without_a_grant_and_starts_when_one_appears() {
    let w = world("appears", &[GMAIL_READONLY_SCOPE]).await;
    w.google().full.insert(
        PRIMARY.into(),
        vec![vec![timed(
            "e",
            "After the grant",
            minute(Utc::now() + Duration::days(1)),
            30,
        )]],
    );
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let sync = CalendarSync::with_api_base(w.store.clone(), w.acct, w.creds.clone(), &w.base)
        .unwrap()
        .with_test_timing(StdDuration::from_millis(30));
    let task = tokio::spawn(sync.run(shutdown));
    tokio::time::sleep(StdDuration::from_millis(300)).await;
    assert!(w.google().bearers.is_empty(), "idle: Google not called");

    write_grant(&w.path, GRANTED);
    let mut waited = 0;
    while w.google().requests.is_empty() {
        assert!(waited < 100, "the loop never noticed the grant");
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        waited += 1;
    }
    let mut waited = 0;
    while w.titles().await.is_empty() {
        assert!(waited < 100, "nothing stored after the grant");
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        waited += 1;
    }
    assert_eq!(w.titles().await, ["After the grant"]);
    stop.send(true).unwrap();
    task.await.unwrap().unwrap();
}

// --- refresh -----------------------------------------------------------------

#[tokio::test]
async fn refresh_kicks_the_loop_at_most_once_per_30_seconds() {
    let w = world("kick", GRANTED).await;
    w.google().full.insert(PRIMARY.into(), vec![vec![]]);
    w.google().changes.insert(
        PRIMARY.into(),
        HashMap::from([(
            "tok-primary-1".to_string(),
            (vec![], "tok-primary-1".to_string()),
        )]),
    );
    // A loop that, left alone, would not pass again for an hour.
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let sync = CalendarSync::with_api_base(w.store.clone(), w.acct, w.creds.clone(), &w.base)
        .unwrap()
        .with_test_timing(StdDuration::from_secs(3600));
    let kick = sync.kick_handle();
    let app = router(
        state_with(|_, _| {})
            .0
            .with_calendar_credentials((*w.creds).clone())
            .with_calendar_sync_kick(kick),
    );
    let task = tokio::spawn(sync.run(shutdown));
    let requests = |w: &World| w.google().requests.len();
    let mut waited = 0;
    while requests(&w) < 1 {
        assert!(waited < 100);
        tokio::time::sleep(StdDuration::from_millis(20)).await;
        waited += 1;
    }

    let (status, body) = refresh(&app).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body, json!({"kicked": true}));
    let mut waited = 0;
    while requests(&w) < 2 {
        assert!(waited < 100, "the kick did not start a pass");
        tokio::time::sleep(StdDuration::from_millis(20)).await;
        waited += 1;
    }
    // Inside the floor: accepted, nothing done, across requests.
    for _ in 0..3 {
        let (status, body) = refresh(&app).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body, json!({"kicked": false}));
    }
    tokio::time::sleep(StdDuration::from_millis(200)).await;
    assert_eq!(requests(&w), 2, "throttled kicks reached Google");
    stop.send(true).unwrap();
    task.await.unwrap().unwrap();

    // A door with no loop answers the same shape.
    let Harness { app, .. } = harness(|_, _| {});
    assert_eq!(
        refresh(&app).await,
        (StatusCode::ACCEPTED, json!({"kicked": false}))
    );
}

// --- the listing -------------------------------------------------------------

#[tokio::test]
async fn events_are_served_in_range_and_in_order() {
    let w = world("range", GRANTED_WITH_LIST).await;
    let team = "team@group.calendar.google.com";
    let base = minute(Utc::now() + Duration::days(10));
    let day = base.date_naive();
    {
        let mut g = w.google();
        g.list = vec![
            list_entry("me@example.com", "Me", true, true),
            list_entry(team, "Team", true, false),
        ];
        g.full.insert(
            PRIMARY.into(),
            vec![vec![
                timed("late", "Late", base + Duration::hours(5), 30),
                timed("same-p", "Same start, primary", base, 30),
                all_day("day", "All day", day + Duration::days(2), 1),
                timed("outside", "Outside", base + Duration::days(3), 30),
            ]],
        );
        g.full.insert(
            team.into(),
            vec![vec![
                timed("same-t", "Same start, team", base, 30),
                timed("before", "Before", base - Duration::hours(1), 30),
            ]],
        );
    }
    assert_eq!(w.pass().await, PassOutcome::Synced);
    // [from, to): `from` is the earliest start itself, and `to` is the
    // outside event's start, which is excluded. The all-day event's date is
    // inside, and sorts at its midnight.
    let uri = format!(
        "/client/timeline?from={}&to={}",
        q(base - Duration::hours(1)),
        q(base + Duration::days(3)),
    );
    let body = get(&w.app, &uri).await;
    let titles: Vec<_> = body["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        titles,
        [
            "Before",
            "Same start, primary",
            "Same start, team",
            "Late",
            "All day"
        ]
    );
    // `to` is exclusive.
    let uri = format!(
        "/client/timeline?from={}&to={}",
        q(base - Duration::hours(2)),
        q(base - Duration::hours(1)),
    );
    assert_eq!(get(&w.app, &uri).await["events"], json!([]));
}

#[tokio::test]
async fn events_are_scoped_to_their_account() {
    let w = world("isolated", GRANTED).await;
    w.google().full.insert(
        PRIMARY.into(),
        vec![vec![timed(
            "e",
            "Mine only",
            minute(Utc::now() + Duration::days(1)),
            30,
        )]],
    );
    assert_eq!(w.pass().await, PassOutcome::Synced);
    let other = w.store.ensure_account("other@example.com").unwrap();
    let wide = |acct| {
        w.store.list_calendar_events(
            acct,
            (Utc::now() - Duration::days(40)).fixed_offset(),
            (Utc::now() + Duration::days(359)).fixed_offset(),
        )
    };
    assert_eq!(wide(w.acct).unwrap().len(), 1);
    assert!(wide(other).unwrap().is_empty());
    assert!(w.store.calendar_calendars(other).unwrap().is_empty());
    assert_eq!(w.store.calendar_status(other).unwrap(), (None, None));
    // The other account's door, granted with a token of its own, serves none.
    let path = cred_path("isolated-other");
    write_grant(&path, GRANTED);
    let other_creds = CalendarCredentials::new(
        other,
        "me@example.com".into(),
        CredentialBackend::File,
        path.clone(),
        OAuthClientConfig {
            client_id: "id".into(),
            client_secret: "secret".into(),
        },
    );
    let app = router(
        squelch_api::ApiState::new(w.store.clone(), other, TOKEN)
            .with_calendar_credentials(other_creds),
    );
    let body = get(&app, "/client/timeline").await;
    assert_eq!(body["calendar"]["granted"], true);
    assert_eq!(body["events"], json!([]));
    std::fs::remove_file(&path).ok();
}

// --- the ghost match ---------------------------------------------------------

#[tokio::test]
async fn a_ghost_names_the_real_event_it_duplicates_and_near_misses_name_none() {
    let w = world("match", GRANTED).await;
    let at = minute(Utc::now() + Duration::days(6));
    let day = (Utc::now() + Duration::days(8)).date_naive();
    seed_ghost(&w.store, w.acct, "g1", "Dinner at Lilia", rfc(at));
    seed_ghost(
        &w.store,
        w.acct,
        "g2",
        "Dinner at Lilia",
        rfc(at + Duration::days(1)),
    );
    seed_ghost(&w.store, w.acct, "g3", "Dentist", rfc(at));
    seed_ghost(
        &w.store,
        w.acct,
        "g4",
        "Conference",
        day.format("%Y-%m-%d").to_string(),
    );
    seed_ghost(
        &w.store,
        w.acct,
        "g5",
        "Workshop",
        rfc(minute(day.and_hms_opt(16, 0, 0).unwrap().and_utc())),
    );
    w.google().full.insert(
        PRIMARY.into(),
        vec![vec![
            // Same instant, written in UTC; title differs only in case and
            // spacing.
            json!({"id": "dinner", "summary": "dinner  at LILIA",
                   "start": {"dateTime": at.to_rfc3339()},
                   "end": {"dateTime": (at + Duration::hours(2)).to_rfc3339()}}),
            all_day("conf", "Big Conference 2026", day, 1),
            // All-day on the timed ghost's date: not a match.
            all_day("workshop", "Workshop", day, 1),
        ]],
    );
    assert_eq!(w.pass().await, PassOutcome::Synced);
    let body = w.timeline().await;
    let ghosts = body["ghosts"].as_array().unwrap();
    let matched = |title: &str, start_day: Option<&str>| -> Value {
        ghosts
            .iter()
            .find(|g| {
                g["title"] == title
                    && start_day.is_none_or(|d| g["start"].as_str().unwrap().starts_with(d))
            })
            .unwrap_or_else(|| panic!("no ghost {title}"))["matched_event"]
            .clone()
    };
    let dinner = ghosts
        .iter()
        .find(|g| g["title"] == "Dinner at Lilia" && g["matched_event"] != Value::Null)
        .unwrap();
    eprintln!(
        "ghost with matched_event: {}",
        serde_json::to_string_pretty(dinner).unwrap()
    );
    assert_eq!(
        dinner["matched_event"],
        json!({"calendar_id": "primary", "id": "dinner"})
    );
    assert_eq!(dinner["state"], "ghost", "matching changes no ghost state");
    let next_day = rfc(at + Duration::days(1));
    assert_eq!(
        matched("Dinner at Lilia", Some(&next_day[..10])),
        Value::Null,
        "a day apart"
    );
    assert_eq!(
        matched("Dentist", None),
        Value::Null,
        "same instant, unrelated title"
    );
    assert_eq!(
        matched("Conference", None),
        json!({"calendar_id": "primary", "id": "conf"}),
        "same date, one title inside the other"
    );
    assert_eq!(
        matched("Workshop", None),
        Value::Null,
        "all-day against timed"
    );
    // The ghost's key set, wave 1's plus `matched_event`.
    assert_eq!(
        sorted_keys(dinner),
        [
            "all_day",
            "commitment",
            "end",
            "end_estimated",
            "end_source",
            "estimate",
            "id",
            "kind",
            "likelihood",
            "location",
            "mail_state",
            "matched_event",
            "sources",
            "start",
            "start_timezone",
            "state",
            "title",
            "verified"
        ]
    );
    // Matching is computed, not stored: drop the event and the match goes.
    w.google().changes.insert(
        PRIMARY.into(),
        HashMap::from([(
            "tok-primary-1".to_string(),
            (
                vec![json!({"id": "dinner", "status": "cancelled"})],
                "tok-primary-2".to_string(),
            ),
        )]),
    );
    assert_eq!(w.pass().await, PassOutcome::Synced);
    let body = w.timeline().await;
    assert!(
        body["ghosts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|g| g["title"] != "Dinner at Lilia" || g["matched_event"] == Value::Null)
    );
}

// --- the agent door ----------------------------------------------------------

/// Calendar data is human-door only: no agent route serves an event, its
/// title, organizer or link, and the refresh has no agent twin.
#[tokio::test]
async fn nothing_from_the_calendar_reaches_the_agent_door() {
    let w = world("agent", GRANTED).await;
    let mut event = timed(
        "e",
        "Secret offsite planning",
        minute(Utc::now() + Duration::days(2)),
        60,
    );
    event["organizer"] = json!({"email": "organizer-secret@example.com"});
    event["location"] = json!("Room Zanzibar");
    w.google().full.insert(PRIMARY.into(), vec![vec![event]]);
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(w.events().await.len(), 1, "the human door has it");
    // Some mail, so the agent listings have something to serve.
    w.store
        .upsert_message(&msg(w.acct, "m1", "t1", "Hello", "planning notes"))
        .unwrap();

    for (method, uri) in [
        ("GET", "/client/agent/timeline"),
        ("POST", "/client/agent/timeline/refresh"),
        ("GET", "/client/agent/calendar"),
        ("GET", "/client/agent/events"),
        ("GET", "/mcp/timeline"),
    ] {
        let resp = w.app.clone().oneshot(authed(method, uri)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{method} {uri}");
    }
    for uri in [
        "/client/agent/feed",
        "/client/agent/records?kind=calendar",
        "/client/agent/search?q=offsite",
        "/client/agent/search?q=Zanzibar",
    ] {
        let resp = w.app.clone().oneshot(authed("GET", uri)).await.unwrap();
        let status = resp.status();
        let text = String::from_utf8_lossy(
            &http_body_util::BodyExt::collect(resp.into_body())
                .await
                .unwrap()
                .to_bytes(),
        )
        .to_string();
        assert!(
            status.is_success() || status.is_client_error(),
            "{uri}: {status}"
        );
        for secret in [
            "Secret offsite",
            "organizer-secret",
            "Zanzibar",
            "google.com/calendar",
            "html_link",
            "calendar_name",
        ] {
            assert!(!text.contains(secret), "{uri} serves {secret}: {text}");
        }
    }
}
