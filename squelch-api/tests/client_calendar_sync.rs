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
    /// Answer this calendar's every events call with this status and body.
    fail_calendar: HashMap<String, (u16, Value)>,
    /// Answer this page (by index) of every full read with this status.
    fail_page: Option<(usize, u16)>,
    /// Hold this page (by index) of every full read this long before answering.
    delay_page: Option<(usize, StdDuration)>,
    /// (calendar id, query) of every events request, in order.
    requests: Vec<(String, HashMap<String, String>)>,
    /// The query of every `calendarList` request, in order.
    list_requests: Vec<HashMap<String, String>>,
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

async fn list_route(
    State(g): State<Fake>,
    Query(q): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
) -> Response {
    let mut g = g.lock().unwrap();
    g.bearers.push(bearer(&headers));
    g.list_requests.push(q);
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
    // The answer is decided under the lock; a delay is served after it is
    // dropped, so a held request does not stall the test reading the fake.
    let (delay, response) = {
        let mut g = g.lock().unwrap();
        g.bearers.push(bearer(&headers));
        g.requests.push((calendar.clone(), q.clone()));
        let page: usize = q.get("pageToken").and_then(|p| p.parse().ok()).unwrap_or(0);
        let full = !q.contains_key("syncToken");
        let delay = g
            .delay_page
            .filter(|(n, _)| full && *n == page)
            .map(|(_, d)| d);
        (delay, answer(&g, &calendar, &q))
    };
    if let Some(delay) = delay {
        tokio::time::sleep(delay).await;
    }
    response
}

fn answer(g: &Google, calendar: &str, q: &HashMap<String, String>) -> Response {
    let calendar = calendar.to_string();
    if let Some((status, body)) = g.fail_calendar.get(&calendar) {
        return (StatusCode::from_u16(*status).unwrap(), Json(body.clone())).into_response();
    }
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
    if let Some((page, status)) = g.fail_page
        && page == n
    {
        return (StatusCode::from_u16(status).unwrap(), Json(json!({}))).into_response();
    }
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
/// production makes a ghost. Returns the message's id.
fn seed_ghost(store: &SqliteStore, account: i64, gmail: &str, title: &str, start: String) -> i64 {
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
    id
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
        json!({"granted": true, "synced_at": null, "error": null, "truncated": false})
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
    assert_eq!(
        sorted_keys(status),
        ["error", "granted", "synced_at", "truncated"]
    );
    assert_eq!(status["granted"], true);
    assert_eq!(status["error"], Value::Null);
    assert_eq!(status["truncated"], false);
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
    let (count, _) = w
        .store
        .list_calendar_events(
            w.acct,
            (Utc::now() - Duration::days(60)).fixed_offset(),
            (Utc::now() + Duration::days(700)).fixed_offset(),
            Utc::now(),
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
        json!({"granted": false, "synced_at": null, "error": null, "truncated": false})
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
            Utc::now(),
        )
    };
    assert_eq!(wide(w.acct).unwrap().0.len(), 1);
    assert!(wide(other).unwrap().0.is_empty());
    assert!(w.store.calendar_calendars(other).unwrap().is_empty());
    assert_eq!(
        w.store.calendar_status(other).unwrap(),
        squelch_core::gcal::CalendarStatus::default()
    );
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

/// Calendar data is human-door only. After a sync, every `/client/agent/*`
/// route there is (thread, search, feed, records of every kind, a triage
/// decision) answers 200 with JSON for real mail, and none of them carries an
/// event's title, place, organizer, link or calendar name.
#[tokio::test]
async fn nothing_from_the_calendar_reaches_the_agent_door() {
    let w = world("agent", GRANTED_WITH_LIST).await;
    let team = "team@group.calendar.google.com";
    let mut event = timed(
        "e",
        "Secret offsite planning",
        minute(Utc::now() + Duration::days(2)),
        60,
    );
    event["organizer"] = json!({"email": "organizer-secret@example.com"});
    event["location"] = json!("Room Zanzibar");
    {
        let mut g = w.google();
        g.list = vec![
            list_entry("me@example.com", "Me", true, true),
            list_entry(team, "Covert Team Calendar", true, false),
        ];
        g.full.insert(PRIMARY.into(), vec![vec![event]]);
        g.full.insert(
            team.into(),
            vec![vec![timed(
                "t",
                "Another hidden thing",
                minute(Utc::now() + Duration::days(3)),
                30,
            )]],
        );
    }
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(w.events().await.len(), 2, "the human door has them");
    // Mail the agent routes do serve: a plain message, and a triaged one
    // (a ghost of its own, so the records and decision routes have content).
    w.store
        .upsert_message(&msg(w.acct, "m1", "t1", "Hello", "planning notes"))
        .unwrap();
    let triaged = seed_ghost(
        &w.store,
        w.acct,
        "g1",
        "Dinner plans",
        rfc(minute(Utc::now() + Duration::days(4))),
    );
    let mut uris = vec![
        // A triaged thread, the kind the agent door serves.
        "/client/agent/thread/t-g1".to_string(),
        "/client/agent/feed".to_string(),
        "/client/agent/search?q=planning".to_string(),
        "/client/agent/search?q=offsite".to_string(),
        "/client/agent/search?q=Zanzibar".to_string(),
        format!("/client/agent/triage/{triaged}"),
    ];
    for kind in [
        "bills",
        "receipts",
        "calendar",
        "banking",
        "shipments",
        "marketing",
    ] {
        uris.push(format!("/client/agent/records?kind={kind}"));
    }
    for uri in &uris {
        let resp = w.app.clone().oneshot(authed("GET", uri)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{uri}");
        let bytes = http_body_util::BodyExt::collect(resp.into_body())
            .await
            .unwrap()
            .to_bytes();
        serde_json::from_slice::<Value>(&bytes).unwrap_or_else(|e| panic!("{uri}: not JSON: {e}"));
        let text = String::from_utf8_lossy(&bytes);
        for secret in [
            "Secret offsite",
            "Another hidden thing",
            "organizer-secret",
            "Zanzibar",
            "google.com/calendar",
            "Covert Team Calendar",
            "html_link",
            "calendar_name",
        ] {
            assert!(!text.contains(secret), "{uri} serves {secret}: {text}");
        }
    }
}

// --- review fixes: shutdown, sweeps, per-calendar failure, request shape ------

/// Two timed events stored by a first full read, at fixed starts.
async fn two_stored(w: &World) -> DateTime<Utc> {
    let start = minute(Utc::now() + Duration::days(3));
    w.google().full.insert(
        PRIMARY.into(),
        vec![vec![
            timed("a", "Kept A", start, 30),
            timed("b", "Kept B", start + Duration::hours(1), 30),
        ]],
    );
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(w.titles().await, ["Kept A", "Kept B"]);
    start
}

fn primary_row(w: &World) -> squelch_core::gcal::StoredCalendar {
    w.store
        .calendar_calendars(w.acct)
        .unwrap()
        .into_iter()
        .find(|c| c.calendar_id == PRIMARY)
        .unwrap()
}

/// SHUTDOWN DOES NOT WAIT FOR GOOGLE. A full read whose second page Google
/// holds for ten seconds: shutdown is signalled while it is held, `run()`
/// returns within a second, and the cut-off read swept nothing (the event
/// the first page left out is still stored) and recorded nothing.
#[tokio::test]
async fn shutdown_interrupts_a_pass_in_flight_and_leaves_the_store_as_it_was() {
    let w = world("shutdown", GRANTED).await;
    let start = two_stored(&w).await;
    let before = primary_row(&w);
    {
        let mut g = w.google();
        // No incremental answer: the token is 410, and the pass reads in full.
        g.full.insert(
            PRIMARY.into(),
            vec![
                vec![timed("a", "Kept A", start, 30)],
                vec![timed("c", "Never arrives", start + Duration::hours(2), 30)],
            ],
        );
        g.delay_page = Some((1, StdDuration::from_secs(10)));
        g.requests.clear();
    }
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let sync = CalendarSync::with_api_base(w.store.clone(), w.acct, w.creds.clone(), &w.base)
        .unwrap()
        .with_test_timing(StdDuration::from_millis(30));
    let task = tokio::spawn(sync.run(shutdown));
    let mut waited = 0;
    while !w
        .google()
        .requests
        .iter()
        .any(|(_, q)| q.get("pageToken").is_some_and(|p| p == "1"))
    {
        assert!(waited < 200, "the held page was never asked for");
        tokio::time::sleep(StdDuration::from_millis(20)).await;
        waited += 1;
    }
    tokio::time::sleep(StdDuration::from_millis(100)).await;
    let signalled = std::time::Instant::now();
    stop.send(true).unwrap();
    tokio::time::timeout(StdDuration::from_secs(5), task)
        .await
        .expect("run() still waiting on Google 5s after shutdown")
        .unwrap()
        .unwrap();
    let took = signalled.elapsed();
    eprintln!("run() returned {took:?} after shutdown was signalled");
    assert!(took < StdDuration::from_secs(1), "{took:?}");
    assert_eq!(w.titles().await, ["Kept A", "Kept B"], "no partial sweep");
    let after = primary_row(&w);
    assert_eq!(after.synced_at, before.synced_at, "no finish was recorded");
    assert_eq!(after.full_synced_at, before.full_synced_at);
}

/// THE DAILY FULL READ SWEEPS ONLY WHAT A WHOLE READ DID NOT SEE. Page 2 of
/// a full re-read fails: every row stored before it is still there, and the
/// sync token is the one the last good read left.
#[tokio::test]
async fn a_full_read_whose_later_page_fails_sweeps_nothing_and_keeps_the_token() {
    let w = world("sweep-fail", GRANTED).await;
    let start = two_stored(&w).await;
    let before = primary_row(&w);
    assert_eq!(before.sync_token.as_deref(), Some("tok-primary-1"));
    {
        let mut g = w.google();
        g.full.insert(
            PRIMARY.into(),
            vec![
                vec![timed("a", "Kept A", start, 30)],
                vec![timed("c", "Never arrives", start + Duration::hours(2), 30)],
            ],
        );
        g.fail_page = Some((1, 500));
        g.requests.clear();
    }
    // A day later: the token is kept, but the read is the daily full one.
    let later = Utc::now() + Duration::hours(25);
    assert_eq!(
        w.sync.pass(later).await,
        PassOutcome::Failed(CalendarErrorKind::Unavailable)
    );
    {
        let g = w.google();
        assert!(g.requests[0].1.contains_key("timeMin"), "a full read");
        assert_eq!(g.requests.len(), 2, "page 2 was asked for and failed");
    }
    assert_eq!(w.titles().await, ["Kept A", "Kept B"], "nothing swept");
    let after = primary_row(&w);
    assert_eq!(after.sync_token.as_deref(), Some("tok-primary-1"));
    assert_eq!(after.full_synced_at, before.full_synced_at);
}

/// The same for a full re-read that is cut off at the 40-page cap.
#[tokio::test]
async fn a_full_read_cut_off_at_the_page_cap_sweeps_nothing_and_keeps_the_token() {
    let w = world("sweep-cap", GRANTED).await;
    two_stored(&w).await;
    let before = primary_row(&w);
    {
        let mut g = w.google();
        g.endless = true;
        g.requests.clear();
    }
    let later = Utc::now() + Duration::hours(25);
    assert_eq!(
        w.sync.pass(later).await,
        PassOutcome::Failed(CalendarErrorKind::Unavailable)
    );
    assert_eq!(w.google().requests.len(), 40, "stopped at the page cap");
    let titles = w.titles().await;
    assert!(
        titles.contains(&"Kept A".to_string()) && titles.contains(&"Kept B".to_string()),
        "nothing swept: {titles:?}"
    );
    let after = primary_row(&w);
    assert_eq!(after.sync_token.as_deref(), Some("tok-primary-1"));
    assert_eq!(after.full_synced_at, before.full_synced_at);
}

/// ONE BROKEN SHARED CALENDAR IS NOT THE ACCOUNT FAILING. A secondary
/// calendar answering 403 forever: every pass still reads the primary and
/// comes out `Synced` (so the loop keeps its cadence), `calendar.error` stays
/// null, and the broken calendar is retried on its own doubling schedule.
#[tokio::test]
async fn a_secondary_calendar_failing_forever_holds_back_nothing() {
    let w = world("secondary", GRANTED_WITH_LIST).await;
    let team = "team@group.calendar.google.com";
    let start = minute(Utc::now() + Duration::days(2));
    {
        let mut g = w.google();
        g.list = vec![
            list_entry("me@example.com", "Me", true, true),
            list_entry(team, "Team", true, false),
        ];
        g.full
            .insert(PRIMARY.into(), vec![vec![timed("mine", "Mine", start, 30)]]);
        g.fail_calendar.insert(
            team.into(),
            (
                403,
                json!({"error": {"code": 403, "errors": [{"reason": "forbidden"}]}}),
            ),
        );
    }
    // The production pacing: a calendar's own backoff starts at a minute.
    let sync =
        CalendarSync::with_api_base(w.store.clone(), w.acct, w.creds.clone(), &w.base).unwrap();
    let t0 = Utc::now();
    let reads = |w: &World, cal: &str| w.google().requests.iter().filter(|(c, _)| c == cal).count();
    let mut team_reads = Vec::new();
    for (i, at) in [0, 30, 61, 90, 150, 182].into_iter().enumerate() {
        let before = reads(&w, PRIMARY);
        assert_eq!(
            sync.pass(t0 + Duration::seconds(at)).await,
            PassOutcome::Synced,
            "pass {i}"
        );
        assert!(
            reads(&w, PRIMARY) > before,
            "pass {i} did not read the primary"
        );
        team_reads.push(reads(&w, team));
        let status = w.timeline().await["calendar"].clone();
        assert_eq!(status["error"], Value::Null, "pass {i}: {status}");
        assert_eq!(status["granted"], true);
        assert!(status["synced_at"].is_string(), "pass {i}: {status}");
    }
    // Read at 0 (fails, retry at 60), skipped at 30, read at 61 (fails
    // again, retry at 61+120=181), skipped at 90 and 150, read at 182.
    assert_eq!(team_reads, [1, 1, 2, 2, 2, 3]);
    let team_row = w
        .store
        .calendar_calendars(w.acct)
        .unwrap()
        .into_iter()
        .find(|c| c.calendar_id == team)
        .unwrap();
    assert_eq!(team_row.last_error_kind, Some(CalendarErrorKind::Auth));
    assert_eq!(team_row.failures, 3);
    assert_eq!(w.titles().await, ["Mine"]);

    // The primary failing is still the account's error.
    w.google()
        .fail_calendar
        .insert(PRIMARY.into(), (500, json!({})));
    assert_eq!(
        sync.pass(t0 + Duration::seconds(400)).await,
        PassOutcome::Failed(CalendarErrorKind::Unavailable)
    );
    assert_eq!(w.timeline().await["calendar"]["error"], "unavailable");
}

/// A quota answer from a SECONDARY calendar still backs the whole loop off:
/// the limit is per user.
#[tokio::test]
async fn a_quota_answer_from_any_calendar_fails_the_pass() {
    let w = world("secondary-quota", GRANTED_WITH_LIST).await;
    let team = "team@group.calendar.google.com";
    {
        let mut g = w.google();
        g.list = vec![
            list_entry("me@example.com", "Me", true, true),
            list_entry(team, "Team", true, false),
        ];
        g.fail_calendar.insert(team.into(), (429, json!({})));
    }
    assert_eq!(
        w.pass().await,
        PassOutcome::Failed(CalendarErrorKind::Quota)
    );
    assert_eq!(
        w.timeline().await["calendar"]["error"],
        Value::Null,
        "the primary is fine; the loop backs off without an account error"
    );
}

/// ASK GOOGLE FOR LESS. Every events request, the first full one, a later
/// page of it and an incremental one, carries `maxAttendees=1` and the same
/// `fields` mask; the calendar list carries its own mask.
#[tokio::test]
async fn every_request_names_the_fields_it_reads_and_one_attendee() {
    use squelch_core::gcal::client::{EVENTS_FIELDS, LIST_FIELDS};
    let w = world("fields", GRANTED_WITH_LIST).await;
    let start = minute(Utc::now() + Duration::days(2));
    {
        let mut g = w.google();
        g.list = vec![list_entry("me@example.com", "Me", true, true)];
        g.full.insert(
            PRIMARY.into(),
            vec![
                vec![timed("p0", "Page one", start, 30)],
                vec![timed("p1", "Page two", start + Duration::hours(1), 30)],
            ],
        );
        g.changes.insert(
            PRIMARY.into(),
            HashMap::from([(
                "tok-primary-1".to_string(),
                (vec![], "tok-primary-2".to_string()),
            )]),
        );
    }
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(w.pass().await, PassOutcome::Synced);
    let g = w.google();
    let kinds: Vec<_> = g
        .requests
        .iter()
        .map(|(_, q)| (q.contains_key("syncToken"), q.get("pageToken").cloned()))
        .collect();
    assert_eq!(
        kinds,
        [(false, None), (false, Some("1".into())), (true, None)],
        "first, paged, incremental"
    );
    for (_, query) in &g.requests {
        assert_eq!(query["maxAttendees"], "1", "{query:?}");
        assert_eq!(query["fields"], EVENTS_FIELDS, "{query:?}");
        assert_eq!(query["singleEvents"], "true");
        assert_eq!(query["maxResults"], "250");
    }
    for field in [
        "nextPageToken",
        "nextSyncToken",
        "attendees(self,responseStatus)",
    ] {
        assert!(EVENTS_FIELDS.contains(field), "{field}");
    }
    assert!(!EVENTS_FIELDS.contains("description"));
    assert_eq!(g.list_requests.len(), 2);
    for query in &g.list_requests {
        assert_eq!(query["fields"], LIST_FIELDS, "{query:?}");
    }
}

/// With `maxAttendees=1` an invitation comes back with the user's entry
/// alone (`attendeesOmitted`), and the user's answer still reads from it.
#[tokio::test]
async fn the_users_answer_reads_from_the_lone_attendee_google_sends() {
    let w = world("one-attendee", GRANTED).await;
    let mut invite = timed(
        "inv",
        "Big all-hands",
        minute(Utc::now() + Duration::days(2)),
        60,
    );
    invite["attendeesOmitted"] = json!(true);
    invite["attendees"] = json!([{"email": "me@example.com", "self": true,
                                  "responseStatus": "accepted"}]);
    w.google().full.insert(PRIMARY.into(), vec![vec![invite]]);
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(w.events().await[0]["self_response"], "accepted");
}

/// An all-day event whose start names a zone (Google does, on a recurring
/// all-day series) is served with no zone.
#[tokio::test]
async fn an_all_day_event_is_served_with_no_zone_even_when_google_names_one() {
    let w = world("allday-zone", GRANTED).await;
    let day = (Utc::now() + Duration::days(4)).date_naive();
    let mut series = all_day("series_1", "Holiday", day, 1);
    series["start"]["timeZone"] = json!("America/New_York");
    w.google().full.insert(PRIMARY.into(), vec![vec![series]]);
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(w.events().await[0]["start_timezone"], Value::Null);
}

/// An events item for the cap tests: `minutes` from `now`.
fn row_at(id: String, now: DateTime<Utc>, minutes: i64) -> squelch_core::gcal::EventItem {
    let start = minute(now + Duration::minutes(minutes));
    squelch_core::gcal::EventItem::Upsert(Box::new(squelch_core::gcal::EventRow {
        event_id: id,
        ical_uid: None,
        tentative: false,
        summary: Some("Event".into()),
        location: None,
        start: squelch_core::triage::ghost::EventTime::parse(&rfc(start)).unwrap(),
        end: None,
        start_timezone: None,
        organizer_email: None,
        self_response: None,
        transparent: false,
        html_link: None,
        updated_at: None,
    }))
}

/// THE PER-CALENDAR CAP KEEPS WHAT IS NEAREST NOW. A calendar past 5,000
/// events, arriving farthest-future first (Google's order is undefined): the
/// farthest are dropped whichever page brought them, the near ones (past and
/// future) are all kept, and the timeline says `truncated`. A full read that
/// fits clears the mark.
#[tokio::test]
async fn the_per_calendar_cap_keeps_the_events_nearest_now_and_says_so() {
    let w = world("percal-cap", GRANTED).await;
    // A row for the primary calendar, as a pass makes.
    w.google().full.insert(PRIMARY.into(), vec![vec![]]);
    assert_eq!(w.pass().await, PassOutcome::Synced);
    let now = Utc::now();
    let cap = squelch_core::gcal::MAX_EVENTS_PER_CALENDAR;
    // 100 far-future events first (days out), then `cap` near ones spread
    // from 10 days back to 10 days ahead, in pages of 250.
    let mut items: Vec<_> = (0..100)
        .map(|i| row_at(format!("far-{i}"), now, 60 * 24 * (200 + i)))
        .collect();
    items.extend((0..cap).map(|i| {
        let minutes = (i - cap / 2) * (60 * 24 * 10) / (cap / 2);
        row_at(format!("near-{i}"), now, minutes)
    }));
    let mut dropped = 0;
    for page in items.chunks(250) {
        dropped += w
            .store
            .calendar_apply_page(w.acct, PRIMARY, page, now)
            .unwrap()
            .dropped;
    }
    assert_eq!(dropped, 100);
    let (stored, _) = w
        .store
        .list_calendar_events_capped(
            w.acct,
            (now - Duration::days(40)).fixed_offset(),
            (now + Duration::days(400)).fixed_offset(),
            now,
            usize::MAX,
        )
        .unwrap();
    assert_eq!(stored.len() as i64, cap);
    assert!(
        stored.iter().all(|e| e.row.event_id.starts_with("near-")),
        "a far-future event was kept over a near one"
    );
    assert_eq!(w.timeline().await["calendar"]["truncated"], true);

    // A full read that fits clears the mark.
    let seen = std::collections::HashSet::new();
    w.store
        .calendar_finish_sync(w.acct, PRIMARY, Some("t"), now, Some((&seen, false)))
        .unwrap();
    let body = w.timeline().await;
    assert_eq!(body["calendar"]["truncated"], false);
}

/// THE LISTING CAP KEEPS WHAT IS NEAREST NOW, AFTER THE RANGE. More stored
/// events in range than one listing serves: the listing holds the ones
/// nearest now (none of the farthest), in start order, and says it cut; rows
/// outside the range never count against the cap.
#[tokio::test]
async fn the_listing_cap_counts_rows_in_range_and_keeps_the_nearest() {
    let w = world("list-cap", GRANTED_WITH_LIST).await;
    let team = "team@group.calendar.google.com";
    w.google().list = vec![
        list_entry("me@example.com", "Me", true, true),
        list_entry(team, "Team", true, false),
    ];
    assert_eq!(w.pass().await, PassOutcome::Synced);
    let now = Utc::now();
    // Out of the requested range but inside the listing's padded SQL bound:
    // 1,500 events just before `from`.
    let before: Vec<_> = (0..1500)
        .map(|i| row_at(format!("old-{i}"), now, -60 * 24 * 10 - 60 - i))
        .collect();
    // In range: 3,000 per calendar, the team's farther out.
    let primary: Vec<_> = (0..3000)
        .map(|i| row_at(format!("p-{i}"), now, 10 + i))
        .collect();
    let teams: Vec<_> = (0..3000)
        .map(|i| row_at(format!("t-{i}"), now, 6000 + i))
        .collect();
    for page in before.chunks(500).chain(primary.chunks(500)) {
        w.store
            .calendar_apply_page(w.acct, PRIMARY, page, now)
            .unwrap();
    }
    for page in teams.chunks(500) {
        w.store
            .calendar_apply_page(w.acct, team, page, now)
            .unwrap();
    }
    let from = (now - Duration::days(10)).fixed_offset();
    let to = (now + Duration::days(30)).fixed_offset();
    let (listed, cut) = w.store.list_calendar_events(w.acct, from, to, now).unwrap();
    assert!(cut);
    assert_eq!(listed.len(), 5000);
    let ids: std::collections::HashSet<_> =
        listed.iter().map(|e| e.row.event_id.as_str()).collect();
    assert!(ids.contains("p-0") && ids.contains("p-2999") && ids.contains("t-1999"));
    assert!(
        !ids.contains("t-2000") && !ids.contains("t-2999"),
        "the farthest go"
    );
    assert!(!ids.iter().any(|id| id.starts_with("old-")));
    assert!(
        listed
            .windows(2)
            .all(|p| p[0].row.start.to_string() <= p[1].row.start.to_string()),
        "served in start order"
    );
    // Through the door: the same cut, said.
    let uri = format!(
        "/client/timeline?from={}&to={}",
        q(from.to_utc()),
        q(to.to_utc())
    );
    let body = get(&w.app, &uri).await;
    assert_eq!(body["events"].as_array().unwrap().len(), 5000);
    assert_eq!(body["calendar"]["truncated"], true);
    // A range that holds fewer than the cap is not cut.
    let narrow = (now + Duration::days(1)).fixed_offset();
    let (listed, cut) = w
        .store
        .list_calendar_events(w.acct, from, narrow, now)
        .unwrap();
    assert!(!cut);
    assert!(
        !listed.is_empty() && listed.len() < 1500,
        "{}",
        listed.len()
    );
}

/// A kick that lands during a quota backoff does not start a second pass the
/// moment the backoff ends: the pass after the backoff is followed by the
/// usual interval, not another pass at once.
#[tokio::test]
async fn a_kick_during_a_quota_backoff_leaves_no_extra_pass_behind() {
    let w = world("quota-kick", GRANTED).await;
    w.google().fail = Some((429, json!({})));
    let tick = StdDuration::from_millis(600);
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let sync = CalendarSync::with_api_base(w.store.clone(), w.acct, w.creds.clone(), &w.base)
        .unwrap()
        .with_test_timing(tick);
    let kick = sync.kick_handle();
    let task = tokio::spawn(sync.run(shutdown));
    let mut waited = 0;
    while w.google().requests.is_empty() {
        assert!(waited < 100);
        tokio::time::sleep(StdDuration::from_millis(10)).await;
        waited += 1;
    }
    let first = std::time::Instant::now();
    // In the backoff: a kick, and Google recovers.
    tokio::time::sleep(StdDuration::from_millis(100)).await;
    kick.notify_one();
    {
        let mut g = w.google();
        g.fail = None;
        g.full.insert(PRIMARY.into(), vec![vec![]]);
    }
    // The pass after the backoff (one full read) lands at ~600ms; the next
    // is due a tick later, at ~1200ms. Look in between.
    tokio::time::sleep(StdDuration::from_millis(950).saturating_sub(first.elapsed())).await;
    let seen = w.google().requests.len();
    stop.send(true).unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(seen, 2, "the quota pass and one pass after it, no more");
}
