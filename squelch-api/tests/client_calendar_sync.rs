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
    /// Every write received, in order.
    writes: Vec<Write>,
    /// Answer every write with this status (and an error body) instead.
    write_fail: Option<u16>,
    /// Answer every insert or patch 200 with this raw body instead.
    write_raw: Option<String>,
    /// Ids handed to inserted events: `new-1`, `new-2`, ...
    inserted: usize,
    /// (calendar id, event id, query) of every one-event read, in order.
    reads: Vec<(String, String, HashMap<String, String>)>,
    /// Answer every one-event read with this status instead.
    read_fail: Option<u16>,
    /// Let this many one-event reads through, then answer every later one
    /// with this status: how a test fails only the RE-READ that follows an
    /// insert Google answered 409.
    read_fail_late: Option<(usize, u16)>,
    /// Apply every write, then LOSE the answer: the connection is dropped
    /// before the response goes out, as a timeout or a reset would.
    lose_answers: bool,
    /// Apply every write, then hold the answer this long.
    answer_delay: Option<StdDuration>,
    /// Google "cannot guarantee that ID collisions will be detected at event
    /// creation time": true makes the fake miss them, so an insert naming a
    /// taken id makes a second event rather than a 409.
    misses_duplicates: bool,
    /// The calendar ids of every `calendarList.get`, in order.
    list_gets: Vec<String>,
    /// Answer every `calendarList.get` with this status instead.
    list_get_status: Option<u16>,
    /// Answer every delete of this event id 403, as Google refuses a change
    /// it does not allow.
    fail_delete_of: Option<String>,
    /// Answer every delete 403.
    fail_deletes: bool,
    /// Answer this many of the next one-event reads 404, found or not, as a
    /// replica that has not caught up would.
    stale_reads: usize,
}
type Fake = Arc<Mutex<Google>>;

/// One write as the fake received it.
#[derive(Debug, Clone)]
struct Write {
    method: &'static str,
    calendar: String,
    event: Option<String>,
    query: HashMap<String, String>,
    body: Value,
    bearer: String,
}

/// Google's patch semantics on a stored event: "field values you specify
/// replace the existing values", nested objects merge, and "To delete a
/// field, specify the field and set it to `null`."
fn merge(target: &mut Value, patch: &Value) {
    let (Some(target), Some(patch)) = (target.as_object_mut(), patch.as_object()) else {
        return;
    };
    for (key, value) in patch {
        match value {
            Value::Null => {
                target.remove(key);
            }
            Value::Object(_) if target.get(key).is_some_and(Value::is_object) => {
                merge(target.get_mut(key).unwrap(), value);
            }
            _ => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Google's `maxAttendees` on one event of an answer: "If there are more than
/// the specified number of attendees, only the participant is returned", and
/// the event is marked `attendeesOmitted`.
fn omit_attendees(event: &mut Value, q: &HashMap<String, String>) {
    let Some(max) = q.get("maxAttendees").and_then(|m| m.parse::<usize>().ok()) else {
        return;
    };
    let Some(attendees) = event.get_mut("attendees").and_then(Value::as_array_mut) else {
        return;
    };
    if attendees.len() > max {
        attendees.retain(|a| a["self"] == true);
        event["attendeesOmitted"] = json!(true);
    }
}

/// `events.get`, as a guest edit reads it: the attendees the fake holds.
async fn get_route(
    State(g): State<Fake>,
    Path((calendar, id)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
) -> Response {
    let mut g = g.lock().unwrap();
    g.bearers.push(bearer(&headers));
    g.reads.push((calendar.clone(), id.clone(), q.clone()));
    if let Some(status) = g.read_fail {
        return StatusCode::from_u16(status).unwrap().into_response();
    }
    match g.read_fail_late {
        Some((0, status)) => return StatusCode::from_u16(status).unwrap().into_response(),
        Some((left, status)) => g.read_fail_late = Some((left - 1, status)),
        None => {}
    }
    if g.stale_reads > 0 {
        g.stale_reads -= 1;
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(event) = g.find(&calendar, &id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut event = event.clone();
    omit_attendees(&mut event, &q);
    Json(event).into_response()
}

impl Google {
    /// The event as the fake Google holds it (the full read's pages).
    fn find(&mut self, calendar: &str, id: &str) -> Option<&mut Value> {
        self.full
            .get_mut(calendar)?
            .iter_mut()
            .flatten()
            .find(|e| e["id"] == id)
    }

    /// Record a write, and the failure to answer it with, if any.
    fn record(
        &mut self,
        method: &'static str,
        calendar: &str,
        event: Option<&str>,
        query: HashMap<String, String>,
        body: Value,
        headers: &axum::http::HeaderMap,
    ) -> Option<Response> {
        self.writes.push(Write {
            method,
            calendar: calendar.to_string(),
            event: event.map(str::to_string),
            query,
            body,
            bearer: bearer(headers),
        });
        if let Some(status) = self.write_fail {
            let body = json!({"error": {"code": status, "message": "backend error"}});
            return Some((StatusCode::from_u16(status).unwrap(), Json(body)).into_response());
        }
        if method != "DELETE"
            && let Some(raw) = &self.write_raw
        {
            return Some((StatusCode::OK, raw.clone()).into_response());
        }
        None
    }
}

/// `events.insert`: the event as Google stores it, with an id, a link, the
/// calendar as its organizer, added to the calendar's events.
async fn insert_route(
    State(g): State<Fake>,
    Path(calendar): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let mut g = g.lock().unwrap();
    if let Some(failed) = g.record("POST", &calendar, None, q.clone(), body.clone(), &headers) {
        return failed;
    }
    // A client-chosen id is Google's own from then on; one the calendar
    // already holds is a 409, "The requested identifier already exists"
    // (unless the fake is told to miss it, as Google may).
    let id = match body.get("id").and_then(Value::as_str) {
        Some(id) => {
            if !g.misses_duplicates && g.find(&calendar, id).is_some() {
                let error = json!({"error": {"code": 409, "message": "The requested identifier already exists.",
                                   "errors": [{"domain": "global", "reason": "duplicate"}]}});
                return (StatusCode::CONFLICT, Json(error)).into_response();
            }
            id.to_string()
        }
        None => {
            g.inserted += 1;
            format!("new-{}", g.inserted)
        }
    };
    let mut event = body;
    event["id"] = json!(id);
    event["status"] = json!("confirmed");
    event["iCalUID"] = json!(format!("{id}@google.com"));
    event["htmlLink"] = json!(format!("https://www.google.com/calendar/event?eid={id}"));
    event["organizer"] = json!({"email": "me@example.com", "self": true});
    event["updated"] = json!("2026-10-05T10:00:00.000Z");
    let pages = g.full.entry(calendar).or_default();
    if pages.is_empty() {
        pages.push(Vec::new());
    }
    pages.last_mut().unwrap().push(event.clone());
    omit_attendees(&mut event, &q);
    Json(event).into_response()
}

/// `events.patch`, applied to the event the fake holds.
async fn patch_route(
    State(g): State<Fake>,
    Path((calendar, id)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let mut g = g.lock().unwrap();
    let recorded = g.record(
        "PATCH",
        &calendar,
        Some(&id),
        q.clone(),
        body.clone(),
        &headers,
    );
    if let Some(failed) = recorded {
        return failed;
    }
    let Some(event) = g.find(&calendar, &id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    merge(event, &body);
    event["updated"] = json!("2026-10-05T11:00:00.000Z");
    let mut event = event.clone();
    omit_attendees(&mut event, &q);
    Json(event).into_response()
}

/// `events.delete`: "an empty response body", or 410 for one already gone.
async fn delete_route(
    State(g): State<Fake>,
    Path((calendar, id)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
) -> Response {
    let mut g = g.lock().unwrap();
    if let Some(failed) = g.record("DELETE", &calendar, Some(&id), q, Value::Null, &headers) {
        return failed;
    }
    if g.fail_deletes || g.fail_delete_of.as_deref() == Some(id.as_str()) {
        let error = json!({"error": {"code": 403, "errors": [{"reason": "forbidden"}]}});
        return (StatusCode::FORBIDDEN, Json(error)).into_response();
    }
    let Some(pages) = g.full.get_mut(&calendar) else {
        return StatusCode::GONE.into_response();
    };
    let before: usize = pages.iter().map(Vec::len).sum();
    for page in pages.iter_mut() {
        page.retain(|e| e["id"] != id.as_str());
    }
    let after: usize = pages.iter().map(Vec::len).sum();
    if before == after {
        return StatusCode::GONE.into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

fn bearer(headers: &axum::http::HeaderMap) -> String {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// `calendarList.get`: the entry, or 404 for a calendar the user no longer
/// has.
async fn list_get_route(State(g): State<Fake>, Path(id): Path<String>) -> Response {
    let mut g = g.lock().unwrap();
    g.list_gets.push(id.clone());
    if let Some(status) = g.list_get_status {
        return StatusCode::from_u16(status).unwrap().into_response();
    }
    match g.list.iter().find(|e| e["id"] == id.as_str()) {
        Some(entry) => Json(entry.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
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
    let mut items = pages.get(n).cloned().unwrap_or_default();
    for item in &mut items {
        omit_attendees(item, q);
    }
    body["items"] = json!(items);
    if n + 1 < pages.len() {
        body["nextPageToken"] = json!((n + 1).to_string());
    } else {
        body["nextSyncToken"] = json!(format!("tok-{calendar}-1"));
    }
    Json(body).into_response()
}

/// Around every write: the route applies it (Google made the change), then
/// the answer is held ([`Google::answer_delay`]) or lost
/// ([`Google::lose_answers`]: the task serving the connection unwinds with no
/// response written, so the client sees the connection drop mid-request).
async fn answers(
    State(g): State<Fake>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let write = req.method() != axum::http::Method::GET;
    let resp = next.run(req).await;
    let (lose, delay) = {
        let g = g.lock().unwrap();
        (g.lose_answers, g.answer_delay)
    };
    if write && let Some(delay) = delay {
        tokio::time::sleep(delay).await;
    }
    if write && lose {
        std::panic::resume_unwind(Box::new("the answer is lost"));
    }
    resp
}

async fn spawn_google(g: Fake) -> String {
    let app = axum::Router::new()
        .route("/users/me/calendarList", axum::routing::get(list_route))
        .route(
            "/users/me/calendarList/{id}",
            axum::routing::get(list_get_route),
        )
        .route(
            "/calendars/{id}/events",
            axum::routing::get(events_route).post(insert_route),
        )
        .route(
            "/calendars/{id}/events/{event}",
            axum::routing::get(get_route)
                .patch(patch_route)
                .delete(delete_route),
        )
        .layer(axum::middleware::from_fn_with_state(g.clone(), answers))
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
    world_with(tag, scopes, |state, _| state).await
}

/// [`world`], with the door's state adjusted by `door` (handed the fake's
/// base) before the router is built: a shorter write deadline, writes sent
/// somewhere else.
async fn world_with(
    tag: &str,
    scopes: &[&str],
    door: impl FnOnce(squelch_api::ApiState, &str) -> squelch_api::ApiState,
) -> World {
    let (state, store, acct) = state_with(|_, _| {});
    let path = cred_path(tag);
    write_grant(&path, scopes);
    let creds = Arc::new(
        CalendarCredentials::new(
            acct,
            "me@example.com".into(),
            CredentialBackend::File,
            path.clone(),
            OAuthClientConfig {
                client_id: "id".into(),
                client_secret: "secret".into(),
                issuer: Default::default(),
            },
        )
        .unwrap(),
    );
    let google: Fake = Arc::default();
    let base = spawn_google(google.clone()).await;
    // NO HIDDEN FEATURE: calendar is on for every mailbox, so the door and
    // the loop are built with none at all.
    let sync = CalendarSync::with_api_base(store.clone(), acct, creds.clone(), &base)
        .unwrap()
        .with_test_timing(StdDuration::from_millis(50));
    let metrics = squelch_core::metrics::SyncMetrics::new();
    let app = router(door(
        state
            .with_calendar_credentials((*creds).clone())
            .with_calendar_sync_kick(sync.kick_handle())
            .with_calendar_api_base(base.clone())
            .with_sync_metrics(metrics.clone()),
        &base,
    ));
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
    seed_ghost_as(
        store,
        account,
        gmail,
        title,
        start,
        EventCommitment::Committed,
    )
}

/// [`seed_ghost`] with the record's commitment.
fn seed_ghost_as(
    store: &SqliteStore,
    account: i64,
    gmail: &str,
    title: &str,
    start: String,
    commitment: EventCommitment,
) -> i64 {
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
        commitment,
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
    assert_eq!(
        sorted_keys(&body),
        ["calendar", "calendars", "events", "ghosts"]
    );
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
            "color",
            "description",
            "editable",
            "end",
            "from_gmail",
            "guest_editable",
            "guests",
            "guests_truncated",
            "html_link",
            "id",
            "location",
            "organizer",
            "self_response",
            "start",
            "start_timezone",
            "status",
            "tickets",
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
            // Without the list scope the primary calendar's colour is
            // unknown; the boss organizes this one, so it is not the user's.
            "color": null,
            "editable": false,
            "description": null,
            // The guest list as Google sent it; an invitation is not the
            // user's to re-invite.
            "guests": [
                {"email": "boss@example.com", "name": null, "response": "accepted",
                 "organizer": false, "self": false},
                {"email": "me@example.com", "name": null, "response": "tentative",
                 "organizer": false, "self": true},
            ],
            "guests_truncated": false,
            "guest_editable": false,
            "from_gmail": false,
            // No booking mail is linked to it.
            "tickets": null,
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
           "timeZone": "America/New_York",
           "accessRole": if primary { "owner" } else { "reader" }})
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

/// CALENDAR FOR EVERYONE. A mailbox with no hidden features at all (the
/// `calendar` key is retired) syncs, serves its events, and says so on
/// `/client/stats`; a leftover grant of the retired key, as a control plane
/// with an old `tenant_features` row still serves it, changes nothing.
#[tokio::test]
async fn a_mailbox_with_no_hidden_features_syncs_and_serves_its_calendar() {
    let w = world("everyone", GRANTED).await;
    w.google().full.insert(
        PRIMARY.into(),
        vec![vec![timed(
            "e",
            "For everyone",
            minute(Utc::now() + Duration::days(1)),
            30,
        )]],
    );
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(w.titles().await, ["For everyone"]);
    let stats = get(&w.app, "/client/stats").await;
    assert_eq!(stats["timeline"], true, "{stats}");
    assert_eq!(stats["calendar"]["granted"], true, "{stats}");
    assert_eq!(stats["features"], json!([]), "no hidden feature was needed");

    let leftover = router(
        squelch_api::ApiState::new(w.store.clone(), w.acct, TOKEN)
            .with_calendar_credentials((*w.creds).clone())
            .with_features(squelch_core::features::Features::pinned(["calendar"])),
    );
    assert_eq!(get(&leftover, "/client/stats").await["timeline"], true);
    let body = get(
        &leftover,
        &format!(
            "/client/timeline?from={}&to={}",
            q(Utc::now() - Duration::days(1)),
            q(Utc::now() + Duration::days(7)),
        ),
    )
    .await;
    assert_eq!(body["events"].as_array().unwrap().len(), 1, "{body}");
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
            issuer: Default::default(),
        },
    )
    .unwrap();
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
            "tickets",
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
    event["description"] = json!("Quokka agenda for the offsite");
    event["colorId"] = json!("5");
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
            "Quokka",
            "description",
            "#fbd75b",
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
/// page of it and an incremental one, carries `maxAttendees=100` and the same
/// `fields` mask; the calendar list carries its own mask.
#[tokio::test]
async fn every_request_names_the_fields_it_reads_and_bounds_the_attendees() {
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
        assert_eq!(query["maxAttendees"], "100", "{query:?}");
        assert_eq!(query["fields"], EVENTS_FIELDS, "{query:?}");
        assert_eq!(query["singleEvents"], "true");
        assert_eq!(query["maxResults"], "250");
    }
    for field in [
        "nextPageToken",
        "nextSyncToken",
        "attendeesOmitted",
        "attendees(email,displayName,responseStatus,organizer,self)",
    ] {
        assert!(EVENTS_FIELDS.contains(field), "{field}");
    }
    for field in ["description", "colorId", "organizer(email,self)"] {
        assert!(EVENTS_FIELDS.contains(field), "{field}");
    }
    for never in [
        "attachments",
        "conferenceData",
        "comment",
        "additionalGuests",
    ] {
        assert!(!EVENTS_FIELDS.contains(never), "{never}");
    }
    assert_eq!(g.list_requests.len(), 2);
    for query in &g.list_requests {
        assert_eq!(query["fields"], LIST_FIELDS, "{query:?}");
    }
}

/// Past `maxAttendees` an invitation comes back with the user's entry alone
/// (`attendeesOmitted`), and the user's answer still reads from it.
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

// --- the user's own writes: create, edit, delete ------------------------------

const TEAM: &str = "team@group.calendar.google.com";
const HOLIDAYS: &str = "holidays@group.v.calendar.google.com";

/// A world whose list names the primary calendar (owner, a colour), a team
/// calendar the user writes to, and a read-only holidays calendar, synced
/// once. On the primary: the user's own event (`own`), an invitation from the
/// boss (`invite`) and one with its own colour (`colored`).
async fn write_world(tag: &str) -> (World, DateTime<Utc>) {
    write_world_with(tag, |state, _| state).await
}

/// [`write_world`] with the door adjusted ([`world_with`]).
async fn write_world_with(
    tag: &str,
    door: impl FnOnce(squelch_api::ApiState, &str) -> squelch_api::ApiState,
) -> (World, DateTime<Utc>) {
    let w = world_with(tag, GRANTED_WITH_LIST, door).await;
    let start = minute(Utc::now() + Duration::days(3));
    let mut own = timed("own", "My event", start, 60);
    own["organizer"] = json!({"email": "me@example.com", "self": true});
    own["location"] = json!("Kitchen");
    own["description"] = json!("Old notes");
    let mut invite = timed("invite", "Boss meeting", start + Duration::hours(2), 30);
    invite["organizer"] = json!({"email": "boss@example.com"});
    let mut colored = timed("colored", "Coloured", start + Duration::hours(4), 30);
    colored["colorId"] = json!("11");
    let mut team = timed("team-1", "Team sync", start + Duration::hours(6), 30);
    team["organizer"] = json!({"email": TEAM, "self": true});
    {
        let mut g = w.google();
        let mut primary = list_entry("me@example.com", "Me", true, true);
        primary["backgroundColor"] = json!("#9FE1E7");
        let mut team_entry = list_entry(TEAM, "Team", true, false);
        team_entry["accessRole"] = json!("writer");
        team_entry["backgroundColor"] = json!("#f83a22");
        let mut holidays = list_entry(HOLIDAYS, "Holidays", true, false);
        holidays["colorId"] = json!("8");
        g.list = vec![primary, team_entry, holidays];
        g.full
            .insert(PRIMARY.into(), vec![vec![own, invite, colored]]);
        g.full.insert(TEAM.into(), vec![vec![team]]);
        g.full.insert(
            HOLIDAYS.into(),
            vec![vec![all_day(
                "h1",
                "Holiday",
                (start + Duration::days(1)).date_naive(),
                1,
            )]],
        );
    }
    assert_eq!(w.pass().await, PassOutcome::Synced);
    (w, start)
}

fn event_uri(calendar: &str, id: &str) -> String {
    format!(
        "/client/calendar/events/{}/{id}",
        calendar.replace('#', "%23")
    )
}

/// One request: status, JSON body (Null when empty), Cache-Control.
async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value, Option<String>) {
    let req = match body {
        Some(body) => authed_json(method, uri, body),
        None => authed(method, uri),
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let cache = resp
        .headers()
        .get(header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    (status, body_json(resp).await, cache)
}

/// The served event with this id, if any, without `tickets`: like `place`,
/// a timeline-only field the write's answer does not carry.
async fn served(w: &World, id: &str) -> Option<Value> {
    let mut event = w.events().await.into_iter().find(|e| e["id"] == id)?;
    event.as_object_mut()?.remove("tickets");
    Some(event)
}

/// The newest audit rows, as (action, target, detail).
fn audit(w: &World) -> Vec<(String, Option<String>, Option<String>)> {
    w.store
        .list_audit(w.acct, 50)
        .unwrap()
        .into_iter()
        .map(|a| (a.action, a.target, a.detail))
        .collect()
}

fn writes(w: &World) -> Vec<Write> {
    w.google().writes.clone()
}

fn date(day: NaiveDate) -> String {
    day.format("%Y-%m-%d").to_string()
}

#[tokio::test]
async fn creating_an_event_goes_to_google_then_the_store_and_is_served() {
    use squelch_core::gcal::client::EVENT_FIELDS;
    let (w, start) = write_world("create").await;
    let begin = start + Duration::days(1);
    let request = json!({
        "calendar_id": "primary",
        "title": "  Dinner with Sam ",
        "start": rfc(begin),
        "end": rfc(begin + Duration::hours(2)),
        "all_day": false,
        "location": "Lilia, Brooklyn",
        "description": "Bring wine\nhttps://meet.example/x?pwd=secret",
    });
    eprintln!("POST /client/calendar/events {request}");
    let (status, body, cache) =
        send(&w.app, "POST", "/client/calendar/events", Some(request)).await;
    eprintln!("201 {}", serde_json::to_string_pretty(&body).unwrap());
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(cache.as_deref(), Some("no-store"));

    // Google got exactly the checked, laundered event, telling no one.
    let sent = writes(&w);
    assert_eq!(sent.len(), 1);
    let write = &sent[0];
    assert_eq!((write.method, write.calendar.as_str()), ("POST", "primary"));
    assert_eq!(write.event, None);
    assert_eq!(write.query["sendUpdates"], "none");
    assert_eq!(write.query["fields"], EVENT_FIELDS);
    assert_eq!(write.bearer, "Bearer cal-access");
    // THE ONE ASYMMETRY: Google gets the user's text as typed (the meeting
    // link keeps its passcode); the copy stored and served is laundered like
    // any synced event's.
    assert_eq!(
        write.body,
        json!({
            "summary": "Dinner with Sam",
            "start": {"dateTime": rfc(begin)},
            "end": {"dateTime": rfc(begin + Duration::hours(2))},
            "location": "Lilia, Brooklyn",
            "description": "Bring wine\nhttps://meet.example/x?pwd=secret",
        })
    );

    // Google's answer is what is served: its id, link and organizer.
    assert_eq!(
        body,
        json!({
            "id": "new-1",
            "calendar_id": "primary",
            "calendar_name": "Me",
            "title": "Dinner with Sam",
            "start": rfc(begin),
            "end": rfc(begin + Duration::hours(2)),
            "all_day": false,
            "start_timezone": "America/New_York",
            "location": "Lilia, Brooklyn",
            "status": "confirmed",
            "self_response": null,
            "busy": true,
            "organizer": "me@example.com",
            "html_link": "https://www.google.com/calendar/event?eid=new-1",
            "color": "#9fe1e7",
            "editable": true,
            "description": "Bring wine\nmeet.example/x",
            "guests": [],
            "guests_truncated": false,
            "guest_editable": true,
            "from_gmail": false,
        })
    );
    // Stored at once, served by the timeline as answered, and the next sync
    // (which reads the same event back from Google) changes nothing.
    assert_eq!(served(&w, "new-1").await.as_ref(), Some(&body));
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(served(&w, "new-1").await.as_ref(), Some(&body));
    assert_eq!(
        w.events()
            .await
            .iter()
            .filter(|e| e["id"] == "new-1")
            .count(),
        1
    );

    assert!(
        !body.to_string().contains("pwd"),
        "the served copy is laundered"
    );

    // One audit row, in the mail actions' shape: no title, place or notes.
    let rows = w.store.list_audit(w.acct, 10).unwrap();
    let row = &rows[0];
    eprintln!("audit row: {}", serde_json::to_string(row).unwrap());
    assert_eq!(row.actor, "client-api");
    assert_eq!(row.action, "calendar_event_create");
    assert_eq!(row.target.as_deref(), Some("calendar:primary/new-1"));
    assert_eq!(row.detail.as_deref(), Some("ok guests=0 notified=false"));
    assert_eq!(row.target_sender, None);
    let text = serde_json::to_string(&rows).unwrap();
    for secret in ["Dinner", "Lilia", "wine", "meet.example"] {
        assert!(!text.contains(secret), "the audit log carries {secret}");
    }

    // On a calendar the user writes to that is not the primary one.
    let (status, team, _) = send(
        &w.app,
        "POST",
        "/client/calendar/events",
        Some(
            json!({"calendar_id": TEAM, "title": "Retro", "start": rfc(begin),
                    "end": rfc(begin + Duration::hours(1)), "all_day": false}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{team}");
    assert_eq!(team["calendar_id"], TEAM);
    assert_eq!(team["color"], "#f83a22");
    assert_eq!(team["location"], Value::Null);
    assert_eq!(team["description"], Value::Null);
    assert!(
        !writes(&w)[1]
            .body
            .as_object()
            .unwrap()
            .contains_key("location"),
        "an absent location is not sent"
    );
}

/// The wire's all-day end is the INCLUSIVE last day; Google's is exclusive.
/// One day and several, out on a create and an edit, and back.
#[tokio::test]
async fn an_all_day_end_converts_to_googles_exclusive_end_and_back() {
    let (w, start) = write_world("allday-write").await;
    let day = (start + Duration::days(10)).date_naive();
    let create = |start: NaiveDate, end: NaiveDate| {
        json!({"calendar_id": "primary", "title": "Trip", "start": date(start),
               "end": date(end), "all_day": true})
    };
    // One day: the inclusive end is the start; Google is sent the next day.
    let (status, one, _) = send(
        &w.app,
        "POST",
        "/client/calendar/events",
        Some(create(day, day)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    eprintln!("one-day all-day create answered: {one}");
    assert_eq!(writes(&w)[0].body["start"], json!({"date": date(day)}));
    assert_eq!(
        writes(&w)[0].body["end"],
        json!({"date": date(day + Duration::days(1))})
    );
    assert_eq!(one["all_day"], true);
    assert_eq!(one["start"], date(day));
    assert_eq!(
        one["end"],
        Value::Null,
        "a one-day event ends the day it starts"
    );
    assert_eq!(one["start_timezone"], Value::Null);
    // Three days, the inclusive last day the third.
    let last = day + Duration::days(2);
    let (_, three, _) = send(
        &w.app,
        "POST",
        "/client/calendar/events",
        Some(create(day, last)),
    )
    .await;
    assert_eq!(
        writes(&w)[1].body["end"],
        json!({"date": date(day + Duration::days(3))})
    );
    assert_eq!(three["end"], date(last));
    // Across a month end.
    let month_end = NaiveDate::from_ymd_opt(chrono::Datelike::year(&day), 12, 31).unwrap();
    let (_, new_year, _) = send(
        &w.app,
        "POST",
        "/client/calendar/events",
        Some(create(month_end, month_end)),
    )
    .await;
    assert_eq!(
        writes(&w)[2].body["end"]["date"],
        date(month_end + Duration::days(1))
    );
    assert_eq!(new_year["end"], Value::Null);

    // An edit stretching the one-day event to two: both ends go out, the
    // clock form nulled, the end exclusive.
    let (status, two, _) = send(
        &w.app,
        "PATCH",
        &event_uri("primary", "new-1"),
        Some(json!({"end": date(day + Duration::days(1))})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{two}");
    assert_eq!(
        writes(&w)[3].body,
        json!({
            "start": {"date": date(day), "dateTime": null},
            "end": {"date": date(day + Duration::days(2)), "dateTime": null},
        })
    );
    assert_eq!(two["end"], date(day + Duration::days(1)));
    // Shrunk back to one day: the end is null again.
    let (_, back, _) = send(
        &w.app,
        "PATCH",
        &event_uri("primary", "new-1"),
        Some(json!({"end": date(day)})),
    )
    .await;
    assert_eq!(
        writes(&w)[4].body["end"],
        json!({"date": date(day + Duration::days(1)), "dateTime": null})
    );
    assert_eq!(back["end"], Value::Null);
    // The three-day event made timed: the date form nulled.
    let at = minute(Utc::now() + Duration::days(12));
    let (status, timed_now, _) = send(
        &w.app,
        "PATCH",
        &event_uri("primary", "new-2"),
        Some(json!({"all_day": false, "start": rfc(at), "end": rfc(at + Duration::hours(1))})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{timed_now}");
    assert_eq!(
        writes(&w)[5].body,
        json!({
            "start": {"dateTime": rfc(at), "date": null},
            "end": {"dateTime": rfc(at + Duration::hours(1)), "date": null},
        })
    );
    assert_eq!(timed_now["all_day"], false);
    assert_eq!(timed_now["start"], rfc(at));
    // What the sync reads back agrees with what the writes served.
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(served(&w, "new-1").await.unwrap(), back);
    assert_eq!(served(&w, "new-2").await.unwrap(), timed_now);
}

/// Each field on its own, absent left alone and `null` cleared, at Google and
/// in the row.
#[tokio::test]
async fn an_edit_patches_each_field_and_tells_absent_from_null() {
    let (w, start) = write_world("edit").await;
    let uri = event_uri("primary", "own");
    let before = served(&w, "own").await.unwrap();
    assert_eq!(before["editable"], true);
    assert_eq!(before["location"], "Kitchen");
    assert_eq!(before["description"], "Old notes");

    let request = json!({"title": "Renamed"});
    let (status, renamed, cache) = send(&w.app, "PATCH", &uri, Some(request.clone())).await;
    eprintln!("PATCH {uri} {request} -> {status} {renamed}");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cache.as_deref(), Some("no-store"));
    let write = writes(&w).pop().unwrap();
    assert_eq!(
        (write.method, write.event.as_deref()),
        ("PATCH", Some("own"))
    );
    assert_eq!(write.query["sendUpdates"], "none");
    assert_eq!(write.body, json!({"summary": "Renamed"}), "only the title");
    assert_eq!(renamed["title"], "Renamed");
    assert_eq!(renamed["location"], "Kitchen", "absent is left alone");
    assert_eq!(renamed["description"], "Old notes");
    assert_eq!(renamed["start"], before["start"]);

    let (_, cleared, _) = send(&w.app, "PATCH", &uri, Some(json!({"location": null}))).await;
    assert_eq!(writes(&w).pop().unwrap().body, json!({"location": null}));
    assert_eq!(cleared["location"], Value::Null);
    assert_eq!(cleared["description"], "Old notes");

    let (_, noted, _) = send(
        &w.app,
        "PATCH",
        &uri,
        Some(json!({"description": "New\u{202E} notes", "location": "Room 2"})),
    )
    .await;
    assert_eq!(
        writes(&w).pop().unwrap().body,
        json!({"description": "New notes", "location": "Room 2"})
    );
    assert_eq!(noted["description"], "New notes");
    assert_eq!(noted["location"], "Room 2");
    assert_eq!(noted["title"], "Renamed");

    // A meeting link goes to Google whole; the row keeps it without its query.
    let (_, linked, _) = send(
        &w.app,
        "PATCH",
        &uri,
        Some(json!({"location": "https://zoom.us/j/9?pwd=abc"})),
    )
    .await;
    assert_eq!(
        writes(&w).pop().unwrap().body,
        json!({"location": "https://zoom.us/j/9?pwd=abc"})
    );
    assert_eq!(linked["location"], "zoom.us/j/9");
    let (_, _, _) = send(&w.app, "PATCH", &uri, Some(json!({"location": "Room 2"}))).await;

    let (_, unnoted, _) = send(&w.app, "PATCH", &uri, Some(json!({"description": null}))).await;
    assert_eq!(writes(&w).pop().unwrap().body, json!({"description": null}));
    assert_eq!(unnoted["description"], Value::Null);
    assert_eq!(unnoted["location"], "Room 2");

    // A new start alone keeps the stored end (the event was an hour long and
    // starts half an hour later): both go out.
    let later = start + Duration::minutes(30);
    let (status, moved, _) = send(&w.app, "PATCH", &uri, Some(json!({"start": rfc(later)}))).await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(
        writes(&w).pop().unwrap().body,
        json!({
            "start": {"dateTime": rfc(later), "date": null},
            "end": {"dateTime": rfc(start + Duration::hours(1)), "date": null},
        })
    );
    assert_eq!(moved["start"], rfc(later));
    assert_eq!(moved["title"], "Renamed");

    // The row is what was served.
    assert_eq!(served(&w, "own").await.unwrap(), moved);
    let edits = audit(&w)
        .into_iter()
        .filter(|(a, _, _)| a == "calendar_event_edit")
        .collect::<Vec<_>>();
    assert_eq!(edits.len(), 7);
    assert!(edits.iter().all(|(_, target, detail)| {
        target.as_deref() == Some("calendar:primary/own")
            && detail.as_deref() == Some("ok guests=0 notified=false")
    }));
}

#[tokio::test]
async fn a_delete_removes_the_event_at_google_and_here() {
    let (w, _) = write_world("delete").await;
    let (status, body, cache) = send(&w.app, "DELETE", &event_uri("primary", "own"), None).await;
    eprintln!("DELETE {} -> {status} {body}", event_uri("primary", "own"));
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(body, Value::Null);
    assert_eq!(cache.as_deref(), Some("no-store"));
    let write = writes(&w).pop().unwrap();
    assert_eq!(
        (write.method, write.event.as_deref()),
        ("DELETE", Some("own"))
    );
    assert_eq!(write.query["sendUpdates"], "none");
    assert!(!write.query.contains_key("fields"));
    assert_eq!(served(&w, "own").await, None);
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(served(&w, "own").await, None, "Google no longer has it");
    assert_eq!(
        audit(&w)[0],
        (
            "calendar_event_delete".into(),
            Some("calendar:primary/own".into()),
            Some("ok guests=0 notified=false".into())
        )
    );

    // An event Google already deleted (410) is deleted here too.
    w.google().full.insert(TEAM.into(), vec![vec![]]);
    let (status, _, _) = send(&w.app, "DELETE", &event_uri(TEAM, "team-1"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(served(&w, "team-1").await, None);
}

/// Every bad request is a 400 with a fixed message, reaches nobody, changes
/// no row, and is audited without its content.
#[tokio::test]
async fn every_bad_request_is_a_400_and_reaches_nobody() {
    let (w, start) = write_world("bad").await;
    let before = w.events().await;
    let at = |h: i64| rfc(start + Duration::hours(h));
    let day = date(start.date_naive());
    let good = || {
        json!({"calendar_id": "primary", "title": "Zanzibar-secret", "start": at(30),
               "end": at(31), "all_day": false})
    };
    let with = |key: &str, value: Value| {
        let mut body = good();
        body[key] = value;
        body
    };
    let creates = [
        ("end before start", with("end", json!(at(29)))),
        ("end at start", with("end", json!(at(30)))),
        ("mixed precision", with("end", json!(day))),
        ("all-day with times", with("all_day", json!(true))),
        (
            "unknown calendar",
            with("calendar_id", json!("zanzibar-cal@example.com")),
        ),
        ("read-only calendar", with("calendar_id", json!(HOLIDAYS))),
        ("empty title", with("title", json!("   "))),
        (
            "long title",
            with("title", json!(format!("Zanzibar{}", "a".repeat(200)))),
        ),
        ("no end", {
            let mut b = good();
            b.as_object_mut().unwrap().remove("end");
            b
        }),
        (
            "a time with no offset",
            with("start", json!("2026-10-05T19:00:00")),
        ),
        ("an array", json!(["Zanzibar-secret"])),
    ];
    for (case, body) in creates {
        let (status, answer, _) = send(&w.app, "POST", "/client/calendar/events", Some(body)).await;
        eprintln!("400 {case}: {answer}");
        assert_eq!(status, StatusCode::BAD_REQUEST, "{case}: {answer}");
        let text = answer.to_string();
        assert!(!text.contains("Zanzibar"), "{case} echoes input: {text}");
        assert!(!text.contains("zanzibar"), "{case} echoes input: {text}");
    }
    // Not JSON at all.
    let resp = w
        .app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/client/calendar/events")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(axum::body::Body::from("Zanzibar{"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(!body_json(resp).await.to_string().contains("Zanzibar"));

    let edits = [
        ("end before start", json!({"end": at(-1)})),
        ("mixed precision", json!({"end": day})),
        ("to all-day without dates", json!({"all_day": true})),
        ("long title", json!({"title": "Zanzibar".repeat(30)})),
        ("cleared title", json!({"title": null})),
        (
            "moved to another calendar",
            json!({"calendar_id": TEAM, "title": "x"}),
        ),
        ("nothing", json!({})),
        ("bad location", json!({"location": 7})),
    ];
    for (case, body) in edits {
        let (status, answer, _) =
            send(&w.app, "PATCH", &event_uri("primary", "own"), Some(body)).await;
        eprintln!("400 edit {case}: {answer}");
        assert_eq!(status, StatusCode::BAD_REQUEST, "{case}: {answer}");
        assert!(!answer.to_string().contains("Zanzibar"));
    }
    assert!(writes(&w).is_empty(), "Google was never called");
    assert_eq!(w.events().await, before, "no row changed");
    // Audited, by reason; the calendar named only once it is one being read.
    let rows = audit(&w);
    assert!(rows.iter().any(|r| r
        == &(
            "calendar_event_create".to_string(),
            None,
            Some("rejected:calendar guests=0 notified=false".to_string())
        )));
    assert!(rows.iter().any(|r| r
        == &(
            "calendar_event_create".to_string(),
            Some(format!("calendar:{HOLIDAYS}")),
            Some("rejected:read_only guests=0 notified=false".to_string())
        )));
    assert!(rows.iter().any(|r| r
        == &(
            "calendar_event_edit".to_string(),
            Some("calendar:primary/own".to_string()),
            Some("rejected:body guests=0 notified=false".to_string())
        )));
    let text = format!("{rows:?}");
    assert!(!text.contains("Zanzibar") && !text.contains("zanzibar"));
}

#[tokio::test]
async fn every_write_is_403_when_calendar_is_not_granted() {
    let w = world("write-nogrant", &[GMAIL_READONLY_SCOPE]).await;
    let start = minute(Utc::now() + Duration::days(2));
    for (method, uri, body) in [
        (
            "POST",
            "/client/calendar/events".to_string(),
            Some(
                json!({"calendar_id": "primary", "title": "x", "start": rfc(start),
                        "end": rfc(start + Duration::hours(1)), "all_day": false}),
            ),
        ),
        (
            "PATCH",
            event_uri("primary", "e"),
            Some(json!({"title": "x"})),
        ),
        ("DELETE", event_uri("primary", "e"), None),
    ] {
        let (status, answer, _) = send(&w.app, method, &uri, body).await;
        eprintln!("403 not granted {method}: {answer}");
        assert_eq!(status, StatusCode::FORBIDDEN, "{method}");
        assert_eq!(answer["error"], "Google Calendar is not connected");
        assert_eq!(answer["code"], "not_granted");
    }
    assert!(writes(&w).is_empty());
    assert!(
        audit(&w).iter().all(|(_, target, detail)| target.is_none()
            && detail.as_deref() == Some("rejected:not_granted"))
    );
}

/// THE EDIT GUARD on the wire: another organizer's invitation, and anything
/// on a calendar the user only reads, is served `editable: false` and
/// refused with 403 before Google is called.
#[tokio::test]
async fn an_event_that_is_not_the_users_own_cannot_be_changed() {
    let (w, _) = write_world("guard").await;
    assert_eq!(served(&w, "invite").await.unwrap()["editable"], false);
    assert_eq!(served(&w, "h1").await.unwrap()["editable"], false);
    assert_eq!(served(&w, "own").await.unwrap()["editable"], true);
    assert_eq!(served(&w, "team-1").await.unwrap()["editable"], true);
    // No organizer at all on the user's own calendar: theirs.
    assert_eq!(served(&w, "colored").await.unwrap()["editable"], true);
    for (calendar, id) in [(PRIMARY, "invite"), (HOLIDAYS, "h1")] {
        let before = served(&w, id).await;
        for (method, body) in [
            ("PATCH", Some(json!({"title": "Mine now"}))),
            ("DELETE", None),
        ] {
            let (status, answer, _) = send(&w.app, method, &event_uri(calendar, id), body).await;
            eprintln!("403 not editable {method} {id}: {answer}");
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {id}");
            assert_eq!(
                answer["error"],
                "this event cannot be changed from Passband"
            );
            assert_eq!(answer["code"], "not_editable");
        }
        assert_eq!(served(&w, id).await, before);
    }
    assert!(writes(&w).is_empty());
    assert_eq!(
        audit(&w)[0],
        (
            "calendar_event_delete".into(),
            Some(format!("calendar:{HOLIDAYS}/h1")),
            Some("rejected:not_editable guests=0 notified=false".into())
        )
    );
}

#[tokio::test]
async fn an_unknown_event_is_404() {
    let (w, _) = write_world("unknown").await;
    for (method, uri, body) in [
        (
            "PATCH",
            event_uri("primary", "nope"),
            Some(json!({"title": "x"})),
        ),
        ("DELETE", event_uri("primary", "nope"), None),
        (
            "PATCH",
            event_uri("nope@example.com", "own"),
            Some(json!({"title": "x"})),
        ),
        ("DELETE", event_uri(TEAM, "own"), None),
        ("DELETE", event_uri("primary", &"x".repeat(2000)), None),
    ] {
        let (status, answer, _) = send(&w.app, method, &uri, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}: {answer}");
        assert_eq!(answer, json!({"error": "not found", "code": "not_found"}));
    }
    assert!(writes(&w).is_empty());
    assert!(
        audit(&w)
            .iter()
            .all(|(_, target, detail)| target.is_none()
                && detail.as_deref() == Some("failed:target"))
    );
}

/// GOOGLE FIRST, THEN LOCAL. Google refusing (a 5xx, a 403, a 429) is a 502
/// with a fixed message, and every local row is exactly as it was.
#[tokio::test]
async fn a_google_failure_leaves_every_row_as_it_was() {
    let (w, start) = write_world("google-fails").await;
    let before = w.events().await;
    for status in [400, 403, 429, 404] {
        w.google().write_fail = Some(status);
        let create = json!({"calendar_id": "primary", "title": "Never", "start": rfc(start),
                            "end": rfc(start + Duration::hours(1)), "all_day": false});
        for (method, uri, body) in [
            ("POST", "/client/calendar/events".to_string(), Some(create)),
            (
                "PATCH",
                event_uri("primary", "own"),
                Some(json!({"title": "Never", "location": null})),
            ),
            ("DELETE", event_uri("primary", "own"), None),
        ] {
            // A delete Google answers 404 found it already gone: see
            // `a_delete_google_does_not_know_is_already_done`.
            if method == "DELETE" && status == 404 {
                continue;
            }
            let (code, answer, _) = send(&w.app, method, &uri, body).await;
            eprintln!("Google {status} on {method}: {code} {answer}");
            assert_eq!(code, StatusCode::BAD_GATEWAY, "{status} {method}");
            assert_eq!(
                answer,
                json!({"error": "Google Calendar did not accept the change",
                       "code": "google_refused"})
            );
            assert_eq!(w.events().await, before, "{status} {method} changed a row");
        }
    }
    assert_eq!(writes(&w).len(), 11, "each was sent once, never retried");
    assert!(
        audit(&w).iter().all(
            |(_, _, detail)| detail.as_deref() == Some("failed:google guests=0 notified=false")
        )
    );
    // Google said yes but with an event the daemon cannot read: nothing is
    // stored, and the answer says the change happened: a 504 the app holds
    // the pane on (verify4 X1), never the 502 a refusal is.
    w.google().write_fail = None;
    w.google().write_raw = Some(r#"{"id": "x", "status": "cancelled"}"#.into());
    let (code, answer, _) = send(
        &w.app,
        "PATCH",
        &event_uri("primary", "own"),
        Some(json!({"title": "Odd"})),
    )
    .await;
    assert_eq!(code, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(
        answer,
        json!({"error": "Google Calendar saved the change, but its answer could not be read",
               "code": "uncertain"})
    );
    assert_eq!(w.events().await, before);
    assert_eq!(
        audit(&w)[0].2.as_deref(),
        Some("failed:answer guests=0 notified=false")
    );
}

/// verify5 Y1: Google's OWN 5xx on a write is not a refusal. Its error
/// guide calls a 500 "an unexpected error occurred while processing the
/// request", so the write may have been applied: the answer is the 504 the
/// app freezes its pane on, with a sync kicked, never `google_refused`.
#[tokio::test]
async fn a_google_5xx_on_a_write_may_have_happened() {
    let (w, start) = write_world("google-5xx").await;
    let before = w.events().await;
    for status in [500, 502, 503, 504] {
        w.google().write_fail = Some(status);
        let create = json!({"calendar_id": "primary", "title": "Maybe", "start": rfc(start),
                            "end": rfc(start + Duration::hours(1)), "all_day": false});
        for (method, uri, body) in [
            ("POST", "/client/calendar/events".to_string(), Some(create)),
            (
                "PATCH",
                event_uri("primary", "own"),
                Some(json!({"title": "Maybe"})),
            ),
            ("DELETE", event_uri("primary", "own"), None),
        ] {
            let (code, answer, _) = send(&w.app, method, &uri, body).await;
            assert_eq!(
                code,
                StatusCode::GATEWAY_TIMEOUT,
                "{status} {method}: {answer}"
            );
            assert_eq!(answer["code"], json!("uncertain"), "{status} {method}");
            assert_eq!(w.events().await, before, "{status} {method} changed a row");
            assert!(kicked(&w).await, "{status} {method}: a sync was kicked");
        }
    }
    assert!(
        audit(&w).iter().all(|(_, _, detail)| detail
            .as_deref()
            .is_some_and(|d| d.starts_with("failed:uncertain"))),
        "{:?}",
        audit(&w)
    );
}

/// Colours and roles from the list, the event's own colour over its
/// calendar's, and `calendars` in the timeline.
#[tokio::test]
async fn colours_and_writable_calendars_are_served() {
    let (w, _) = write_world("colours").await;
    let body = w.timeline().await;
    eprintln!(
        "calendars: {}",
        serde_json::to_string_pretty(&body["calendars"]).unwrap()
    );
    assert_eq!(
        body["calendars"],
        json!([
            {"id": "primary", "name": "Me", "color": "#9fe1e7", "primary": true, "writable": true,
             "can_invite": true},
            {"id": HOLIDAYS, "name": "Holidays", "color": "#16a765", "primary": false,
             "writable": false, "can_invite": false},
            {"id": TEAM, "name": "Team", "color": "#f83a22", "primary": false, "writable": true,
             "can_invite": false},
        ])
    );
    assert_eq!(served(&w, "own").await.unwrap()["color"], "#9fe1e7");
    assert_eq!(served(&w, "colored").await.unwrap()["color"], "#dc2127");
    assert_eq!(served(&w, "team-1").await.unwrap()["color"], "#f83a22");
    assert_eq!(served(&w, "h1").await.unwrap()["color"], "#16a765");

    // Without the list scope: the primary alone, the user's own, no colour.
    let plain = world("colours-nolist", GRANTED).await;
    assert_eq!(plain.pass().await, PassOutcome::Synced);
    assert_eq!(
        plain.timeline().await["calendars"],
        json!([{"id": "primary", "name": "Calendar primary", "color": null, "primary": true,
                "writable": true, "can_invite": true}])
    );
    // A list role that drops to reader stops the calendar being writable.
    w.google().list[1]["accessRole"] = json!("reader");
    assert_eq!(w.pass().await, PassOutcome::Synced);
    let calendars = w.timeline().await["calendars"].clone();
    assert_eq!(calendars[2]["writable"], false);
    assert_eq!(served(&w, "team-1").await.unwrap()["editable"], false);
}

/// Another account's door can neither see nor change this account's events.
#[tokio::test]
async fn writes_are_scoped_to_their_account() {
    let (w, start) = write_world("write-isolated").await;
    let other = w.store.ensure_account("other@example.com").unwrap();
    let path = cred_path("write-isolated-other");
    write_grant(&path, GRANTED);
    let other_creds = CalendarCredentials::new(
        other,
        "me@example.com".into(),
        CredentialBackend::File,
        path.clone(),
        OAuthClientConfig {
            client_id: "id".into(),
            client_secret: "secret".into(),
            issuer: Default::default(),
        },
    )
    .unwrap();
    let app = router(
        squelch_api::ApiState::new(w.store.clone(), other, TOKEN)
            .with_calendar_credentials(other_creds)
            .with_calendar_api_base(w.base.clone()),
    );
    let before = w.events().await;
    let (status, _, _) = send(
        &app,
        "PATCH",
        &event_uri("primary", "own"),
        Some(json!({"title": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = send(&app, "DELETE", &event_uri("primary", "own"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = send(
        &app,
        "POST",
        "/client/calendar/events",
        Some(
            json!({"calendar_id": "primary", "title": "x", "start": rfc(start),
                    "end": rfc(start + Duration::hours(1)), "all_day": false}),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the other account reads no calendar"
    );
    assert!(writes(&w).is_empty());
    assert_eq!(w.events().await, before);
    assert!(w.store.list_audit(other, 10).unwrap().len() == 3);
    std::fs::remove_file(&path).ok();
}

/// The writes are on the human door alone: no agent route by any name.
#[tokio::test]
async fn the_agent_door_has_no_calendar_write() {
    let (w, start) = write_world("agent-write").await;
    let body = json!({"calendar_id": "primary", "title": "x", "start": rfc(start),
                      "end": rfc(start + Duration::hours(1)), "all_day": false});
    for (method, uri) in [
        ("POST", "/client/agent/calendar/events".to_string()),
        (
            "PATCH",
            "/client/agent/calendar/events/primary/own".to_string(),
        ),
        (
            "DELETE",
            "/client/agent/calendar/events/primary/own".to_string(),
        ),
        ("POST", "/client/agent/timeline".to_string()),
        ("GET", "/client/agent/calendar".to_string()),
    ] {
        let (status, _, _) = send(&w.app, method, &uri, Some(body.clone())).await;
        assert!(
            status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED,
            "{method} {uri}: {status}"
        );
    }
    assert!(writes(&w).is_empty());
}

// --- guests, invited by Google -------------------------------------------------

const FAMILY: &str = "family@group.calendar.google.com";
const SAM: &str = "Sam.Lee@example.com";

/// Every address a guest test puts anywhere, for "the audit log names none".
const GUEST_ADDRESSES: [&str; 8] = [
    "sam.lee", "pat@", "lee@", "kim@", "new@", "crowd", "boss@", "nia@",
];

fn me_entry() -> Value {
    json!({"email": "me@example.com", "self": true, "organizer": true,
           "responseStatus": "accepted"})
}

/// Sam as Google holds him: an answer, and fields the daemon never stores.
fn sam_entry() -> Value {
    json!({"email": SAM, "displayName": "Sam \u{202E}Lee", "responseStatus": "accepted",
           "comment": "See you there", "optional": true, "additionalGuests": 1})
}

/// [`write_world`], plus on the primary calendar: `party` (the user's own,
/// with four guests), `big` (the user's own, 101 guests and the user),
/// `orphan` (names no organizer); and a calendar the user OWNS that is not
/// the primary one (`FAMILY`, with `fam-1`).
async fn guest_world(tag: &str) -> (World, DateTime<Utc>) {
    let (w, start) = write_world(tag).await;
    let own = json!({"email": "me@example.com", "self": true});
    let mut party = timed("party", "Party", start + Duration::hours(8), 60);
    party["organizer"] = own.clone();
    party["attendees"] = json!([
        me_entry(),
        sam_entry(),
        {"email": "pat@example.com", "responseStatus": "declined"},
        {"email": "lee@example.com", "responseStatus": "tentative"},
        {"email": "kim@example.com", "responseStatus": "needsAction"},
    ]);
    let mut big = timed("big", "All hands", start + Duration::hours(10), 60);
    big["organizer"] = own.clone();
    let mut crowd: Vec<Value> = (0..101)
        .map(|i| json!({"email": format!("crowd{i}@example.com"), "responseStatus": "accepted"}))
        .collect();
    crowd.push(me_entry());
    big["attendees"] = json!(crowd);
    let orphan = timed("orphan", "Orphan", start + Duration::hours(12), 60);
    let mut fam = timed("fam-1", "Family dinner", start + Duration::hours(14), 60);
    fam["organizer"] = json!({"email": FAMILY, "self": true});
    {
        let mut g = w.google();
        let mut family = list_entry(FAMILY, "Family", true, false);
        family["accessRole"] = json!("owner");
        g.list.push(family);
        g.full
            .get_mut(PRIMARY)
            .unwrap()
            .last_mut()
            .unwrap()
            .extend([party, big, orphan]);
        g.full.insert(FAMILY.into(), vec![vec![fam]]);
        g.requests.clear();
    }
    assert_eq!(w.pass().await, PassOutcome::Synced);
    (w, start)
}

fn guest_emails(event: &Value) -> Vec<String> {
    event["guests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["email"].as_str().unwrap().to_string())
        .collect()
}

/// No audit row, of any account action so far, names a guest.
fn audit_names_no_guest(w: &World) {
    let text = serde_json::to_string(&w.store.list_audit(w.acct, 200).unwrap())
        .unwrap()
        .to_lowercase();
    for address in GUEST_ADDRESSES {
        assert!(!text.contains(address), "the audit log carries {address}");
    }
}

/// The sync stores an event's guests and the timeline serves them, laundered,
/// with each one's answer; a list past the cap is marked truncated and keeps
/// the user's own answer.
#[tokio::test]
async fn the_timeline_serves_an_events_guests() {
    let (w, _) = guest_world("guests-served").await;
    let party = served(&w, "party").await.unwrap();
    eprintln!(
        "served guests: {}",
        serde_json::to_string_pretty(&party["guests"]).unwrap()
    );
    assert_eq!(
        party["guests"],
        json!([
            {"email": "me@example.com", "name": null, "response": "accepted",
             "organizer": true, "self": true},
            {"email": SAM, "name": "Sam Lee", "response": "accepted",
             "organizer": false, "self": false},
            {"email": "pat@example.com", "name": null, "response": "declined",
             "organizer": false, "self": false},
            {"email": "lee@example.com", "name": null, "response": "tentative",
             "organizer": false, "self": false},
            {"email": "kim@example.com", "name": null, "response": "pending",
             "organizer": false, "self": false},
        ]),
        "Google's order, the bidi override gone from the name, needsAction as pending"
    );
    assert_eq!(party["guests_truncated"], false);
    assert_eq!(party["guest_editable"], true);
    assert_eq!(party["self_response"], "accepted");
    // What Google holds beyond the address, name and answer is not served
    // (nor stored: the fields mask never asks for it).
    let text = party.to_string();
    assert!(!text.contains("See you there") && !text.contains("additionalGuests"));

    // 102 attendees at Google: only the user's entry comes back, marked.
    let big = served(&w, "big").await.unwrap();
    assert_eq!(guest_emails(&big), ["me@example.com"]);
    assert_eq!(big["guests_truncated"], true);
    assert_eq!(
        big["guest_editable"], false,
        "a truncated list is read-only"
    );
    assert_eq!(
        big["editable"], true,
        "the event itself is still the user's"
    );
    assert_eq!(big["self_response"], "accepted");

    // No guests: an empty list, and whether one may be added.
    let own = served(&w, "own").await.unwrap();
    assert_eq!(own["guests"], json!([]));
    assert_eq!(own["guest_editable"], true);
    // Someone else's invitation, an event that names no organizer, an event
    // on a calendar the user only writes to: not the user's to invite to.
    for id in ["invite", "orphan", "team-1", "h1"] {
        let event = served(&w, id).await.unwrap();
        assert_eq!(event["guest_editable"], false, "{id}");
    }
    assert_eq!(served(&w, "orphan").await.unwrap()["editable"], true);
    assert_eq!(served(&w, "team-1").await.unwrap()["editable"], true);
    // A calendar the user owns invites like the primary one.
    assert_eq!(served(&w, "fam-1").await.unwrap()["guest_editable"], true);
    let calendars = w.timeline().await["calendars"].clone();
    let can: Vec<(String, bool)> = calendars
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["id"].as_str().unwrap().to_string(),
                c["can_invite"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        can,
        [
            ("primary".to_string(), true),
            (FAMILY.to_string(), true),
            (HOLIDAYS.to_string(), false),
            (TEAM.to_string(), false),
        ]
    );

    // A guest list that shrinks at Google shrinks here; an event that goes
    // takes its guests with it.
    {
        let mut g = w.google();
        let party = g.find(PRIMARY, "party").unwrap();
        party["attendees"] = json!([me_entry(), sam_entry()]);
        g.changes.clear();
    }
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(
        guest_emails(&served(&w, "party").await.unwrap()),
        ["me@example.com", SAM]
    );
    // An event that goes and comes back under the same id without guests
    // has none: no guest list outlives its event (the store's trigger, which
    // the store's own tests read directly).
    w.google().find(PRIMARY, "party").unwrap()["status"] = json!("cancelled");
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert!(served(&w, "party").await.is_none());
    {
        let mut g = w.google();
        let party = g.find(PRIMARY, "party").unwrap();
        party["status"] = json!("confirmed");
        party.as_object_mut().unwrap().remove("attendees");
    }
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(served(&w, "party").await.unwrap()["guests"], json!([]));
}

/// THE NOTIFY DEFAULT, on the wire, for all three routes: Google is told
/// `sendUpdates=all` ONLY when the request says `notify: true`; absent and
/// false both send `none`, and no other value is ever sent.
#[tokio::test]
async fn google_is_asked_to_mail_guests_only_when_the_request_says_notify_true() {
    let (w, start) = guest_world("guests-notify").await;
    let create = |notify: Option<bool>| {
        let mut body = json!({"calendar_id": "primary", "title": "Lunch",
            "start": rfc(start + Duration::days(2)),
            "end": rfc(start + Duration::days(2) + Duration::hours(1)),
            "all_day": false, "guests": ["nia@example.com"]});
        if let Some(notify) = notify {
            body["notify"] = json!(notify);
        }
        body
    };
    let edit = |notify: Option<bool>| {
        let mut body = json!({"title": "Renamed"});
        if let Some(notify) = notify {
            body["notify"] = json!(notify);
        }
        body
    };
    for (notify, want) in [(None, "none"), (Some(false), "none"), (Some(true), "all")] {
        let before = writes(&w).len();
        // Create.
        let (status, made, _) = send(
            &w.app,
            "POST",
            "/client/calendar/events",
            Some(create(notify)),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        // Edit (no guest change: the event's guests hear of a new title).
        let (status, _, _) = send(
            &w.app,
            "PATCH",
            &event_uri("primary", "party"),
            Some(edit(notify)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        // Delete, with the flag in the query.
        let id = made["id"].as_str().unwrap();
        let uri = match notify {
            None => event_uri("primary", id),
            Some(flag) => format!("{}?notify={flag}", event_uri("primary", id)),
        };
        let (status, _, _) = send(&w.app, "DELETE", &uri, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let sent = writes(&w)[before..].to_vec();
        let methods: Vec<_> = sent.iter().map(|s| s.method).collect();
        assert_eq!(methods, ["POST", "PATCH", "DELETE"]);
        for write in &sent {
            eprintln!(
                "notify={notify:?}: {} sendUpdates={}",
                write.method, write.query["sendUpdates"]
            );
            assert_eq!(
                write.query["sendUpdates"], want,
                "{} with notify={notify:?}",
                write.method
            );
            assert!(!write.query.contains_key("sendNotifications"));
        }
        // The audit rows say whether guests were told, and how many there
        // were: the three newest are the delete, the edit and the create.
        let told = notify == Some(true);
        let rows = audit(&w);
        assert_eq!(
            rows[0].2.as_deref(),
            Some(format!("ok guests=1 notified={told}").as_str()),
            "delete"
        );
        assert_eq!(
            rows[1].2.as_deref(),
            Some(format!("ok guests=4 notified={told}").as_str()),
            "edit"
        );
        assert_eq!(
            rows[2].2.as_deref(),
            Some(format!("ok guests=1 notified={told}").as_str()),
            "create"
        );
    }
    // Every write this test made named sendUpdates, as one of two values.
    let all: Vec<String> = writes(&w)
        .iter()
        .map(|write| write.query["sendUpdates"].clone())
        .collect();
    assert_eq!(all.len(), 9);
    assert!(all.iter().all(|v| v == "none" || v == "all"));
    assert_eq!(all.iter().filter(|v| *v == "all").count(), 3);
    audit_names_no_guest(&w);
}

/// A `notify` that is not exactly true or false is a 400 that reaches nobody:
/// it is never read as a yes, and never silently as a no.
#[tokio::test]
async fn a_notify_that_is_not_a_boolean_is_refused_and_reaches_nobody() {
    let (w, start) = guest_world("guests-badnotify").await;
    let before = w.events().await;
    for odd in [
        json!("true"),
        json!(1),
        json!(null),
        json!("all"),
        json!([true]),
    ] {
        let (status, answer, _) = send(
            &w.app,
            "POST",
            "/client/calendar/events",
            Some(
                json!({"calendar_id": "primary", "title": "x", "start": rfc(start),
                "end": rfc(start + Duration::hours(1)), "all_day": false,
                "guests": ["nia@example.com"], "notify": odd.clone()}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "create {odd}: {answer}");
        let (status, _, _) = send(
            &w.app,
            "PATCH",
            &event_uri("primary", "party"),
            Some(json!({"title": "x", "notify": odd.clone()})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "edit {odd}");
    }
    // `notify` alone is not a change.
    let (status, _, _) = send(
        &w.app,
        "PATCH",
        &event_uri("primary", "party"),
        Some(json!({"notify": true})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    for query in [
        "notify=1",
        "notify=TRUE",
        "notify=yes",
        "notify=",
        "notify=all",
        "notify=false&notify=true",
        "notify=true&notify=true",
    ] {
        let uri = format!("{}?{query}", event_uri("primary", "party"));
        let (status, _, _) = send(&w.app, "DELETE", &uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
    }
    {
        let g = w.google();
        assert!(g.writes.is_empty(), "Google was never written to");
        assert!(g.reads.is_empty(), "nor read");
    }
    assert_eq!(w.events().await, before);
}

#[tokio::test]
async fn creating_an_event_with_guests_sends_them_bare_and_stores_googles_answer() {
    let (w, start) = guest_world("guests-create").await;
    let begin = start + Duration::days(2);
    let (status, body, _) = send(
        &w.app,
        "POST",
        "/client/calendar/events",
        Some(
            json!({"calendar_id": "primary", "title": "Lunch", "start": rfc(begin),
            "end": rfc(begin + Duration::hours(1)), "all_day": false,
            "guests": [" nia@example.com ", "New@Example.com"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let write = writes(&w).pop().unwrap();
    assert_eq!(write.query["sendUpdates"], "none");
    assert_eq!(write.query["maxAttendees"], "100");
    assert_eq!(
        write.body["attendees"],
        json!([{"email": "nia@example.com"}, {"email": "New@Example.com"}]),
        "bare: an address and nothing else"
    );
    assert_eq!(
        body["guests"],
        json!([
            {"email": "nia@example.com", "name": null, "response": "pending",
             "organizer": false, "self": false},
            {"email": "New@Example.com", "name": null, "response": "pending",
             "organizer": false, "self": false},
        ])
    );
    assert_eq!(body["guest_editable"], true);
    // Stored at once, and the next sync changes nothing.
    assert_eq!(served(&w, "new-1").await.as_ref(), Some(&body));
    w.google().changes.clear();
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(served(&w, "new-1").await.as_ref(), Some(&body));
    assert_eq!(
        audit(&w)[0],
        (
            "calendar_event_create".to_string(),
            Some("calendar:primary/new-1".to_string()),
            Some("ok guests=2 notified=false".to_string())
        )
    );
    // On a calendar the user owns that is not the primary one.
    let (status, fam, _) = send(
        &w.app,
        "POST",
        "/client/calendar/events",
        Some(
            json!({"calendar_id": FAMILY, "title": "Picnic", "start": rfc(begin),
            "end": rfc(begin + Duration::hours(1)), "all_day": false,
            "guests": ["nia@example.com"], "notify": true}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{fam}");
    assert_eq!(writes(&w).pop().unwrap().query["sendUpdates"], "all");
    // No guests: no `attendees` key.
    let (status, _, _) = send(
        &w.app,
        "POST",
        "/client/calendar/events",
        Some(
            json!({"calendar_id": "primary", "title": "Solo", "start": rfc(begin),
            "end": rfc(begin + Duration::hours(1)), "all_day": false, "guests": []}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(
        !writes(&w)
            .pop()
            .unwrap()
            .body
            .as_object()
            .unwrap()
            .contains_key("attendees")
    );
    audit_names_no_guest(&w);
}

/// A guest edit sends the FULL list, built from what Google holds NOW: kept
/// guests exactly as Google has them (THE RESPONSE-STATUS PRESERVATION),
/// removed ones gone, new ones bare. An edit with no `guests` sends no
/// `attendees` at all.
#[tokio::test]
async fn a_guest_edit_sends_the_full_list_keeping_what_google_holds() {
    use squelch_core::gcal::client::ATTENDEES_FIELDS;
    let (w, _) = guest_world("guests-edit").await;
    // Since the last sync Sam changed his answer at Google. The stored row
    // still says accepted; the list sent back must say declined.
    {
        let mut g = w.google();
        let party = g.find(PRIMARY, "party").unwrap();
        party["attendees"][1]["responseStatus"] = json!("declined");
    }
    let stored = served(&w, "party").await.unwrap();
    assert_eq!(stored["guests"][1]["response"], "accepted", "stale here");

    // Keep Sam (typed in another case) and Kim, drop Pat and Lee, add one.
    let (status, body, _) = send(
        &w.app,
        "PATCH",
        &event_uri("primary", "party"),
        Some(json!({"guests": ["sam.lee@EXAMPLE.com", "kim@example.com", "new@example.com"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    {
        let g = w.google();
        // One fresh read of the event's attendees, whole, then one patch.
        assert_eq!(g.reads.len(), 1);
        let (calendar, id, query) = &g.reads[0];
        assert_eq!((calendar.as_str(), id.as_str()), ("primary", "party"));
        assert_eq!(query["fields"], ATTENDEES_FIELDS);
        assert_eq!(query["maxAttendees"], "100");
        assert_eq!(g.writes.len(), 1);
        let write = &g.writes[0];
        eprintln!("PATCH body: {}", write.body);
        assert_eq!(write.query["sendUpdates"], "none");
        let mut sam = sam_entry();
        sam["responseStatus"] = json!("declined");
        assert_eq!(
            write.body,
            json!({"attendees": [
                me_entry(),
                sam,
                {"email": "kim@example.com", "responseStatus": "needsAction"},
                {"email": "new@example.com"},
            ]}),
            "the user's entry unasked, Sam whole with the answer Google holds now \
             (his comment, optional flag and extra guests too), Kim as she was, \
             Pat and Lee gone, the new guest bare"
        );
    }
    assert_eq!(
        guest_emails(&body),
        ["me@example.com", SAM, "kim@example.com", "new@example.com"]
    );
    assert_eq!(body["guests"][1]["response"], "declined");
    assert_eq!(body["guests"][3]["response"], "pending");
    assert_eq!(served(&w, "party").await.as_ref(), Some(&body));
    assert_eq!(
        audit(&w)[0].2.as_deref(),
        Some("ok guests=3 notified=false")
    );

    // ABSENT: a title edit reads nothing and sends no `attendees` key, so
    // Google's list is untouched.
    let (status, body, _) = send(
        &w.app,
        "PATCH",
        &event_uri("primary", "party"),
        Some(json!({"title": "Bigger party"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    {
        let g = w.google();
        assert_eq!(g.reads.len(), 1, "no second read");
        assert_eq!(g.writes[1].body, json!({"summary": "Bigger party"}));
    }
    assert_eq!(guest_emails(&body).len(), 4);

    // PRESENT AND EMPTY: every guest removed; the user's own entry stays.
    // Told this time.
    let (status, body, _) = send(
        &w.app,
        "PATCH",
        &event_uri("primary", "party"),
        Some(json!({"guests": [], "notify": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    {
        let g = w.google();
        assert_eq!(g.reads.len(), 2);
        assert_eq!(g.writes[2].body, json!({"attendees": [me_entry()]}));
        assert_eq!(g.writes[2].query["sendUpdates"], "all");
    }
    assert_eq!(guest_emails(&body), ["me@example.com"]);
    assert_eq!(audit(&w)[0].2.as_deref(), Some("ok guests=0 notified=true"));

    // Guests and another field in one edit, on an event that had none.
    let (status, body, _) = send(
        &w.app,
        "PATCH",
        &event_uri("primary", "own"),
        Some(json!({"guests": ["nia@example.com"], "location": null})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        writes(&w).pop().unwrap().body,
        json!({"attendees": [{"email": "nia@example.com"}], "location": null})
    );
    audit_names_no_guest(&w);
}

/// THE TRUNCATED GUARD. Google replaces the attendee array, so a list the
/// daemon holds only part of must never be sent back: the guest edit is a 400
/// that reaches nobody, while a title edit of the same event still works and
/// carries no `attendees`.
#[tokio::test]
async fn a_truncated_guest_list_refuses_a_guest_edit_but_not_a_title_edit() {
    let (w, _) = guest_world("guests-truncated").await;
    assert_eq!(served(&w, "big").await.unwrap()["guests_truncated"], true);
    for guests in [json!(["new@example.com"]), json!([])] {
        let (status, answer, _) = send(
            &w.app,
            "PATCH",
            &event_uri("primary", "big"),
            Some(json!({"guests": guests, "title": "Renamed"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
        assert!(!answer.to_string().contains("new@"));
    }
    {
        let g = w.google();
        assert!(
            g.writes.is_empty() && g.reads.is_empty(),
            "nobody was asked"
        );
        assert_eq!(
            g.full[PRIMARY]
                .iter()
                .flatten()
                .find(|e| e["id"] == "big")
                .unwrap()["attendees"]
                .as_array()
                .unwrap()
                .len(),
            102,
            "Google's list is whole"
        );
    }
    assert_eq!(
        audit(&w)[0],
        (
            "calendar_event_edit".to_string(),
            Some("calendar:primary/big".to_string()),
            Some("rejected:guests_read_only guests=0+ notified=false".to_string())
        )
    );
    assert_eq!(served(&w, "big").await.unwrap()["title"], "All hands");

    // The title alone: allowed, no read, no `attendees`.
    let (status, body, _) = send(
        &w.app,
        "PATCH",
        &event_uri("primary", "big"),
        Some(json!({"title": "All hands, moved"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    {
        let mut g = w.google();
        assert!(g.reads.is_empty());
        assert_eq!(g.writes.len(), 1);
        assert_eq!(g.writes[0].body, json!({"summary": "All hands, moved"}));
        assert_eq!(
            g.find(PRIMARY, "big").unwrap()["attendees"]
                .as_array()
                .unwrap()
                .len(),
            102
        );
    }
    assert_eq!(body["guests_truncated"], true, "still truncated");
    assert_eq!(body["guest_editable"], false);

    // The list grew past the cap at Google AFTER the last sync: the stored
    // row looks whole, the fresh read does not, and nothing is patched.
    {
        let mut g = w.google();
        let party = g.find(PRIMARY, "party").unwrap();
        let mut crowd: Vec<Value> = (0..120)
            .map(|i| json!({"email": format!("crowd{i}@example.com")}))
            .collect();
        crowd.push(me_entry());
        party["attendees"] = json!(crowd);
    }
    assert_eq!(served(&w, "party").await.unwrap()["guest_editable"], true);
    let (status, _, _) = send(
        &w.app,
        "PATCH",
        &event_uri("primary", "party"),
        Some(json!({"guests": ["new@example.com"]})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    {
        let mut g = w.google();
        assert_eq!(g.reads.len(), 1, "it was read");
        assert_eq!(g.writes.len(), 1, "and not patched");
        assert_eq!(
            g.find(PRIMARY, "party").unwrap()["attendees"]
                .as_array()
                .unwrap()
                .len(),
            121
        );
    }
    // A guest Google names no address for cannot be kept by address either.
    {
        let mut g = w.google();
        g.find(PRIMARY, "party").unwrap()["attendees"] = json!([
            me_entry(),
            {"displayName": "No address", "responseStatus": "accepted"},
        ]);
    }
    let (status, _, _) = send(
        &w.app,
        "PATCH",
        &event_uri("primary", "party"),
        Some(json!({"guests": ["new@example.com"]})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(w.google().writes.len(), 1, "not patched");
    audit_names_no_guest(&w);
}

/// Every guest request that is a 400, each reaching nobody and echoing
/// nothing. THE OWNER-CALENDAR GUARD is among them.
#[tokio::test]
async fn every_bad_guest_request_is_a_400_and_reaches_nobody() {
    let (w, start) = guest_world("guests-400").await;
    let before = w.events().await;
    let many: Vec<String> = (0..51)
        .map(|i| format!("zanzibar{i}@example.com"))
        .collect();
    let lists = [
        ("more than 50", json!(many)),
        ("not a list", json!("zanzibar@example.com")),
        ("a display name", json!(["Zanzibar <zanzibar@example.com>"])),
        (
            "two in one",
            json!(["zanzibar@example.com, other@example.com"]),
        ),
        ("no domain dot", json!(["zanzibar@localhost"])),
        (
            "a header smuggle",
            json!(["zanzibar@example.com\r\nBcc: x@y.com"]),
        ),
        ("not text", json!([{"email": "zanzibar@example.com"}])),
        (
            "a duplicate after case folding",
            json!(["zanzibar@example.com", "Zanzibar@EXAMPLE.com"]),
        ),
        (
            "the calendar's owner",
            json!(["zanzibar@example.com", "ME@example.com"]),
        ),
    ];
    let create = |calendar: &str, guests: Value| {
        json!({"calendar_id": calendar, "title": "x", "start": rfc(start + Duration::days(2)),
            "end": rfc(start + Duration::days(2) + Duration::hours(1)), "all_day": false,
            "guests": guests, "notify": true})
    };
    let refused = |case: String, status: StatusCode, answer: Value| {
        eprintln!("400 {case}: {answer}");
        assert_eq!(status, StatusCode::BAD_REQUEST, "{case}: {answer}");
        let text = answer.to_string().to_lowercase();
        assert!(!text.contains("zanzibar"), "{case} echoes input: {text}");
    };
    for (case, guests) in &lists {
        let (status, answer, _) = send(
            &w.app,
            "POST",
            "/client/calendar/events",
            Some(create("primary", guests.clone())),
        )
        .await;
        refused(format!("create, {case}"), status, answer);
        let (status, answer, _) = send(
            &w.app,
            "PATCH",
            &event_uri("primary", "party"),
            Some(json!({"guests": guests, "notify": true})),
        )
        .await;
        refused(format!("edit, {case}"), status, answer);
    }
    // `null` is not a list on an edit.
    let (status, answer, _) = send(
        &w.app,
        "PATCH",
        &event_uri("primary", "party"),
        Some(json!({"guests": null})),
    )
    .await;
    refused("edit, null".into(), status, answer);
    // The calendar's own id is its owner too (a calendar the user owns).
    let (status, answer, _) = send(
        &w.app,
        "POST",
        "/client/calendar/events",
        Some(create(FAMILY, json!([FAMILY]))),
    )
    .await;
    refused("create, the calendar itself".into(), status, answer);

    // THE OWNER-CALENDAR GUARD: a calendar the user writes to but does not
    // own takes events, and takes no guests. Create and edit.
    let good = json!(["zanzibar@example.com"]);
    let (status, answer, _) = send(
        &w.app,
        "POST",
        "/client/calendar/events",
        Some(create(TEAM, good.clone())),
    )
    .await;
    refused(
        "create on a calendar that is not the user's".into(),
        status,
        answer,
    );
    assert_eq!(
        audit(&w)[0],
        (
            "calendar_event_create".to_string(),
            Some(format!("calendar:{TEAM}")),
            Some("rejected:cannot_invite guests=1 notified=true".to_string())
        )
    );
    let (status, answer, _) = send(
        &w.app,
        "PATCH",
        &event_uri(TEAM, "team-1"),
        Some(json!({"guests": good, "notify": true})),
    )
    .await;
    refused(
        "edit on a calendar that is not the user's".into(),
        status,
        answer,
    );
    assert_eq!(
        audit(&w)[0].2.as_deref(),
        Some("rejected:cannot_invite guests=0 notified=true")
    );
    // Even an empty list is a guest edit there.
    let (status, answer, _) = send(
        &w.app,
        "PATCH",
        &event_uri(TEAM, "team-1"),
        Some(json!({"guests": []})),
    )
    .await;
    refused(
        "edit, an empty list, not the user's calendar".into(),
        status,
        answer,
    );

    // An event that is editable but whose guest list is not the user's to
    // change: one that names no organizer.
    let (status, answer, _) = send(
        &w.app,
        "PATCH",
        &event_uri("primary", "orphan"),
        Some(json!({"guests": good})),
    )
    .await;
    refused("edit, not guest_editable".into(), status, answer);
    assert_eq!(
        audit(&w)[0].2.as_deref(),
        Some("rejected:guests_read_only guests=0 notified=false")
    );
    // Someone else's invitation stays what it was: 403, whatever it carries.
    let (status, _, _) = send(
        &w.app,
        "PATCH",
        &event_uri("primary", "invite"),
        Some(json!({"guests": good, "notify": true})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    {
        let g = w.google();
        assert!(g.writes.is_empty(), "Google was never written to");
        assert!(g.reads.is_empty(), "nor read");
    }
    assert_eq!(w.events().await, before, "no row changed");
    let text = format!("{:?}", audit(&w)).to_lowercase();
    assert!(
        !text.contains("zanzibar"),
        "the audit log carries an address"
    );

    // The same create without guests is fine on that calendar, told or not.
    let mut plain = create(TEAM, json!([]));
    plain["notify"] = json!(false);
    let (status, _, _) = send(&w.app, "POST", "/client/calendar/events", Some(plain)).await;
    assert_eq!(status, StatusCode::CREATED);
}

/// Deleting an event with guests: silent unless asked, and gone here with
/// its guest rows either way.
#[tokio::test]
async fn deleting_an_event_with_guests_tells_them_only_when_asked() {
    let (w, _) = guest_world("guests-delete").await;
    let (status, _, _) = send(&w.app, "DELETE", &event_uri("primary", "party"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(writes(&w)[0].query["sendUpdates"], "none");
    assert_eq!(
        audit(&w)[0],
        (
            "calendar_event_delete".to_string(),
            Some("calendar:primary/party".to_string()),
            Some("ok guests=4 notified=false".to_string())
        )
    );
    assert!(served(&w, "party").await.is_none());
    // A truncated event may be deleted, and its guests told: the audit row
    // says the count is a floor.
    let uri = format!("{}?notify=true", event_uri("primary", "big"));
    let (status, _, _) = send(&w.app, "DELETE", &uri, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let write = writes(&w).pop().unwrap();
    assert_eq!(write.query["sendUpdates"], "all");
    assert!(!write.query.contains_key("fields"));
    assert_eq!(
        audit(&w)[0].2.as_deref(),
        Some("ok guests=0+ notified=true")
    );
    audit_names_no_guest(&w);
}

/// Google first, then local: a failed read or a failed patch leaves the guest
/// list here exactly as it was, and the audit row still says what was asked.
#[tokio::test]
async fn a_google_failure_during_a_guest_edit_changes_nothing_here() {
    let (w, _) = guest_world("guests-fail").await;
    let before = w.events().await;
    let edit = json!({"guests": ["new@example.com"], "notify": true});
    w.google().read_fail = Some(500);
    let (status, answer, _) = send(
        &w.app,
        "PATCH",
        &event_uri("primary", "party"),
        Some(edit.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{answer}");
    assert!(w.google().writes.is_empty(), "no patch after a failed read");
    assert_eq!(w.events().await, before);
    {
        let mut g = w.google();
        g.read_fail = None;
        // A refusal Google answered (a 5xx may have happened: see
        // `a_google_5xx_on_a_write_may_have_happened`).
        g.write_fail = Some(400);
    }
    let (status, _, _) = send(&w.app, "PATCH", &event_uri("primary", "party"), Some(edit)).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(w.events().await, before);
    assert_eq!(
        audit(&w)[0].2.as_deref(),
        Some("failed:google guests=4 notified=true"),
        "a write that asked Google to mail guests says so even when it failed"
    );
    audit_names_no_guest(&w);
}

/// Guests are scoped to their account: another account sees none of them and
/// can change none of them.
#[tokio::test]
async fn guests_are_scoped_to_their_account() {
    let (w, _) = guest_world("guests-isolated").await;
    let other = w.store.ensure_account("other@example.com").unwrap();
    let path = cred_path("guests-isolated-other");
    write_grant(&path, GRANTED);
    // The grant file is keyed by this address: the other account is granted
    // too, so what it is refused is refused by the account scope alone.
    let other_creds = CalendarCredentials::new(
        other,
        "me@example.com".into(),
        CredentialBackend::File,
        path.clone(),
        OAuthClientConfig {
            client_id: "id".into(),
            client_secret: "secret".into(),
            issuer: Default::default(),
        },
    )
    .unwrap();
    let app = router(
        squelch_api::ApiState::new(w.store.clone(), other, TOKEN)
            .with_calendar_credentials(other_creds)
            .with_calendar_api_base(w.base.clone()),
    );
    let uri = format!(
        "/client/timeline?from={}&to={}",
        q(Utc::now() - Duration::days(40)),
        q(Utc::now() + Duration::days(359)),
    );
    let body = get(&app, &uri).await;
    assert_eq!(body["calendar"]["granted"], true);
    assert_eq!(body["events"], json!([]));
    assert!(!body.to_string().to_lowercase().contains("sam.lee"));
    assert!(
        w.store
            .calendar_event(other, "primary", "party")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        w.store
            .calendar_event(w.acct, "primary", "party")
            .unwrap()
            .unwrap()
            .row
            .guests
            .len(),
        5
    );
    let (status, _, _) = send(
        &app,
        "PATCH",
        &event_uri("primary", "party"),
        Some(json!({"guests": [], "notify": true})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let uri = format!("{}?notify=true", event_uri("primary", "party"));
    let (status, _, _) = send(&app, "DELETE", &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    {
        let g = w.google();
        assert!(g.writes.is_empty() && g.reads.is_empty());
    }
    assert_eq!(
        guest_emails(&served(&w, "party").await.unwrap()).len(),
        5,
        "the owner's list is untouched"
    );
    std::fs::remove_file(&path).ok();
}

/// The agent door can neither read a guest nor cause one to be mailed.
#[tokio::test]
async fn the_agent_door_has_no_guests_and_no_notify() {
    let (w, start) = guest_world("guests-agent").await;
    let body = json!({"calendar_id": "primary", "title": "x", "start": rfc(start),
        "end": rfc(start + Duration::hours(1)), "all_day": false,
        "guests": ["nia@example.com"], "notify": true});
    for (method, uri) in [
        ("POST", "/client/agent/calendar/events"),
        ("PATCH", "/client/agent/calendar/events/primary/party"),
        (
            "DELETE",
            "/client/agent/calendar/events/primary/party?notify=true",
        ),
        ("GET", "/client/agent/calendar/events/primary/party"),
        ("GET", "/client/agent/timeline"),
        ("POST", "/mcp/calendar/events"),
        ("DELETE", "/mcp/calendar/events/primary/party?notify=true"),
    ] {
        let (status, answer, _) = send(&w.app, method, uri, Some(body.clone())).await;
        assert!(
            matches!(
                status,
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED | StatusCode::UNAUTHORIZED
            ),
            "{method} {uri}: {status}"
        );
        assert!(!answer.to_string().to_lowercase().contains("sam.lee"));
    }
    let g = w.google();
    assert!(g.writes.is_empty() && g.reads.is_empty());
}

// --- fix round 1 ---------------------------------------------------------------

/// THE EDIT GUARD FAILS CLOSED (the reviewer's case): an invitation on the
/// user's own primary calendar whose organizer address does not read, and
/// whose guest list names that organizer, is somebody else's. Served
/// `editable: false`, and an edit or a delete telling the guests is refused
/// before Google hears anything.
#[tokio::test]
async fn an_invitation_whose_organizer_does_not_read_is_not_the_users() {
    let (w, start) = write_world("odd-organizer").await;
    let mut odd = timed("odd", "Boss offsite", start + Duration::hours(8), 60);
    odd["organizer"] = json!({"email": "boss at example.com"});
    odd["attendees"] = json!([
        {"email": "boss@example.com", "organizer": true, "responseStatus": "accepted"},
        {"email": "me@example.com", "self": true, "responseStatus": "needsAction"}
    ]);
    // And one with an organizer Google names by display name alone: the
    // organizer field by itself, with no guest list to say who organizes it,
    // is enough (it is stored, and read back from the row).
    let mut bare = timed("bare", "Boss lunch", start + Duration::hours(10), 60);
    bare["organizer"] = json!({"displayName": "Boss"});
    w.google().full.get_mut(PRIMARY).unwrap()[0].extend([odd, bare]);
    // The fake answers the stored token 410, so this pass reads in full.
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(served(&w, "bare").await.unwrap()["editable"], false);
    let (status, _, _) = send(&w.app, "DELETE", &event_uri(PRIMARY, "bare"), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let served = served(&w, "odd").await.unwrap();
    assert_eq!(served["organizer"], Value::Null);
    assert_eq!(served["editable"], false, "{served}");
    assert_eq!(served["guest_editable"], false);
    for (method, uri, body) in [
        (
            "DELETE",
            format!("{}?notify=true", event_uri(PRIMARY, "odd")),
            None,
        ),
        (
            "PATCH",
            event_uri(PRIMARY, "odd"),
            Some(json!({"title": "Mine now", "notify": true})),
        ),
    ] {
        let (status, answer, _) = send(&w.app, method, &uri, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method}: {answer}");
        assert_eq!(
            answer["error"],
            "this event cannot be changed from Passband"
        );
        assert_eq!(answer["code"], "not_editable");
    }
    assert!(
        writes(&w).is_empty(),
        "Google was never asked to mail anyone"
    );
}

/// THE NOTIFY GUARD: an update or a cancellation mailed to an event's guests
/// goes out under the calendar's name, so `notify: true` on an edit or a
/// delete is refused on a calendar the user only WRITES to (400, a fixed
/// message, nobody reached), as guests are. Silent edits there still work.
#[tokio::test]
async fn telling_guests_is_refused_on_a_calendar_the_user_only_writes_to() {
    let (w, _) = write_world("notify-writer").await;
    assert_eq!(served(&w, "team-1").await.unwrap()["editable"], true);
    let uri = event_uri(TEAM, "team-1");
    let (status, answer, _) = send(
        &w.app,
        "PATCH",
        &uri,
        Some(json!({"title": "Moved", "notify": true})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    assert_eq!(
        answer,
        json!({"error": "guests can only be told about changes on your own calendar", "code": "cannot_notify"})
    );
    let (status, answer, _) = send(&w.app, "DELETE", &format!("{uri}?notify=true"), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    assert_eq!(
        answer["error"],
        "guests can only be told about changes on your own calendar"
    );
    assert_eq!(answer["code"], "cannot_notify");
    assert!(writes(&w).is_empty(), "nobody reached");
    assert!(served(&w, "team-1").await.is_some());
    assert_eq!(
        audit(&w)[0],
        (
            "calendar_event_delete".into(),
            Some(format!("calendar:{TEAM}/team-1")),
            Some("rejected:cannot_notify guests=0 notified=true".into())
        )
    );
    // Silently, the same edit and delete go through.
    let (status, _, _) = send(&w.app, "PATCH", &uri, Some(json!({"title": "Moved"}))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(&w.app, "DELETE", &format!("{uri}?notify=false"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        writes(&w)
            .iter()
            .all(|write| write.query["sendUpdates"] == "none")
    );
    // On the user's own calendar, telling guests is still allowed.
    let (status, _, _) = send(
        &w.app,
        "PATCH",
        &event_uri(PRIMARY, "own"),
        Some(json!({"title": "Told", "notify": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(writes(&w).last().unwrap().query["sendUpdates"], "all");
}

/// A body naming `notify` twice is refused like a delete query naming it
/// twice, rather than resolved to the last one.
#[tokio::test]
async fn a_body_with_two_notify_keys_is_refused_and_reaches_nobody() {
    let (w, start) = write_world("notify-twice").await;
    let raw = |method: &str, uri: &str, body: String| {
        axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(body))
            .unwrap()
    };
    let create = format!(
        r#"{{"calendar_id": "primary", "title": "x", "start": "{}", "end": "{}",
            "all_day": false, "notify": false, "notify": true}}"#,
        rfc(start),
        rfc(start + Duration::hours(1))
    );
    for req in [
        raw("POST", "/client/calendar/events", create),
        raw(
            "PATCH",
            &event_uri(PRIMARY, "own"),
            r#"{"title": "x", "notify": false, "notify": true}"#.into(),
        ),
    ] {
        let resp = w.app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(resp).await,
            json!({"error": "notify must be true or false", "code": "invalid"})
        );
    }
    assert!(writes(&w).is_empty());
}

/// GOOGLE'S ANSWER LOST: the write went out and Google made the change, but
/// the connection dropped before the answer. That is NOT "Google did not
/// accept the change": it is a 504 saying the change may have happened, the
/// local row is left for the sync, and a refresh (the pass the write kicked)
/// shows what Google holds. A refusal Google DID answer stays a 502.
#[tokio::test]
async fn a_write_whose_answer_is_lost_is_a_504_that_says_it_may_have_happened() {
    let (w, start) = write_world("lost-answer").await;
    w.google().lose_answers = true;
    let uncertain = json!({"error": "Google Calendar may have made this change; refresh before trying again", "code": "uncertain"});
    let create = json!({"calendar_id": "primary", "title": "Maybe", "start": rfc(start),
                        "end": rfc(start + Duration::hours(1)), "all_day": false});
    for (method, uri, body) in [
        ("POST", "/client/calendar/events".to_string(), Some(create)),
        (
            "PATCH",
            event_uri(PRIMARY, "own"),
            Some(json!({"title": "Renamed maybe"})),
        ),
        ("DELETE", event_uri(PRIMARY, "colored"), None),
    ] {
        let (status, answer, _) = send(&w.app, method, &uri, body).await;
        eprintln!("lost answer on {method}: {status} {answer}");
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{method}");
        assert_eq!(answer, uncertain, "{method}");
    }
    assert_eq!(writes(&w).len(), 3, "each sent once, never retried");
    assert!(audit(&w).iter().take(3).all(|(_, _, detail)| {
        detail
            .as_deref()
            .is_some_and(|d| d.starts_with("failed:uncertain"))
    }));
    // Nothing stored from a guess...
    assert_eq!(served(&w, "own").await.unwrap()["title"], "My event");
    assert!(served(&w, "colored").await.is_some());
    // ...and the refresh shows what Google did.
    w.google().lose_answers = false;
    assert_eq!(w.pass().await, PassOutcome::Synced);
    assert_eq!(served(&w, "own").await.unwrap()["title"], "Renamed maybe");
    assert!(served(&w, "colored").await.is_none());
    assert!(served(&w, "new-1").await.is_some());
    // A refusal Google answered (a 4xx) is still a refusal.
    w.google().write_fail = Some(400);
    let (status, answer, _) = send(
        &w.app,
        "PATCH",
        &event_uri(PRIMARY, "own"),
        Some(json!({"title": "No"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(
        answer,
        json!({"error": "Google Calendar did not accept the change", "code": "google_refused"})
    );
}

/// A delete Google answers 404 found the event already gone there, like a
/// 410: the local row goes, 204.
#[tokio::test]
async fn a_delete_google_does_not_know_is_already_done() {
    let (w, _) = write_world("delete-404").await;
    w.google().write_fail = Some(404);
    let (status, answer, _) = send(&w.app, "DELETE", &event_uri(PRIMARY, "own"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{answer}");
    assert_eq!(served(&w, "own").await, None);
    assert!(
        w.google().list_gets.is_empty(),
        "the primary calendar is always the user's"
    );
    assert_eq!(
        audit(&w)[0].2.as_deref(),
        Some("ok guests=0 notified=false")
    );
}

/// THE AUDIT ROW SURVIVES A DISCONNECT. The app goes away while Google is
/// answering a write: the request future is dropped, but the write's own task
/// is not, so Google's answer is still stored and the attempt still audited.
#[tokio::test]
async fn a_write_whose_client_goes_away_is_still_stored_and_audited() {
    let (w, _) = write_world("disconnect").await;
    w.google().answer_delay = Some(StdDuration::from_millis(400));
    let app = w.app.clone();
    let request = tokio::spawn(async move {
        send(
            &app,
            "PATCH",
            &event_uri(PRIMARY, "own"),
            Some(json!({"title": "Sent before the app left"})),
        )
        .await
    });
    // Google has the write (and is holding its answer)...
    let deadline = std::time::Instant::now() + StdDuration::from_secs(5);
    while writes(&w).is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the write never went out"
        );
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }
    // ...when the client goes away.
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    let deadline = std::time::Instant::now() + StdDuration::from_secs(5);
    loop {
        let edits: Vec<_> = audit(&w)
            .into_iter()
            .filter(|(action, _, _)| action == "calendar_event_edit")
            .collect();
        if let Some((_, target, detail)) = edits.first() {
            assert_eq!(target.as_deref(), Some("calendar:primary/own"));
            assert_eq!(detail.as_deref(), Some("ok guests=0 notified=false"));
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the write was never audited"
        );
        tokio::time::sleep(StdDuration::from_millis(20)).await;
    }
    assert_eq!(
        served(&w, "own").await.unwrap()["title"],
        "Sent before the app left"
    );
}

/// Whether the write kicked the sync: the kick leaves a permit when nothing
/// is waiting on it (no loop runs in these tests). Takes the permit.
async fn kicked(w: &World) -> bool {
    tokio::time::timeout(
        StdDuration::from_millis(100),
        w.sync.kick_handle().notified(),
    )
    .await
    .is_ok()
}

/// ONE DEADLINE PER WRITE (verify2 V1). Each Google call may take 30
/// seconds, and a write can make several, more than the app's 45 second
/// wait. The whole write runs under one deadline below it: past it, the
/// answer is the 504 "may have happened", with a sync kicked, for a create,
/// an edit and a delete alike.
#[tokio::test]
async fn a_write_past_its_deadline_is_a_504_and_kicks_a_sync() {
    let (w, start) = write_world_with("deadline", |state, _| {
        state.with_calendar_write_deadline(StdDuration::from_millis(300))
    })
    .await;
    assert!(!kicked(&w).await, "nothing kicked before");
    w.google().answer_delay = Some(StdDuration::from_secs(3));
    let uncertain = json!({"error": "Google Calendar may have made this change; refresh before trying again", "code": "uncertain"});
    let create = json!({"calendar_id": "primary", "title": "Slow", "start": rfc(start),
                        "end": rfc(start + Duration::hours(1)), "all_day": false});
    for (method, uri, body) in [
        ("POST", "/client/calendar/events".to_string(), Some(create)),
        (
            "PATCH",
            event_uri(PRIMARY, "own"),
            Some(json!({"title": "Slow rename"})),
        ),
        ("DELETE", event_uri(PRIMARY, "colored"), None),
    ] {
        let began = std::time::Instant::now();
        let (status, answer, _) = send(&w.app, method, &uri, body).await;
        eprintln!(
            "{method} past the deadline: {status} {answer} after {:?}",
            began.elapsed()
        );
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{method}");
        assert_eq!(answer, uncertain, "{method}");
        assert!(
            began.elapsed() < StdDuration::from_secs(2),
            "{method}: answered at the deadline, not after Google"
        );
        assert!(kicked(&w).await, "{method}: a sync was kicked");
        assert!(
            audit(&w)[0]
                .2
                .as_deref()
                .is_some_and(|d| d.starts_with("failed:uncertain")),
            "{method}: audited as uncertain"
        );
    }
    assert_eq!(writes(&w).len(), 3, "each sent once");
}

const CLIENT_ID: &str = "0f3c9a7e5b2d4e6f8a1b3c5d7e9f0a2b";

/// A CREATE SENT AGAIN FINDS THE FIRST (verify2 V2). The app names the new
/// event's id and sends the same one with every press of the same pane. The
/// daemon reads that id first, so a second send (after a 504, or a timeout
/// in the app) is answered 200 with the event the first one made: nothing
/// inserted, nobody invited twice, even when Google would not have noticed
/// the id was taken.
#[tokio::test]
async fn a_create_sent_twice_with_its_id_makes_one_event_and_one_invitation() {
    let (w, start) = write_world("create-id").await;
    // As Google may: an insert naming a taken id is not caught.
    w.google().misses_duplicates = true;
    let request = json!({"calendar_id": "primary", "title": "Dinner  with\u{200B} Sam",
                         "start": rfc(start), "end": rfc(start + Duration::hours(1)),
                         "all_day": false, "guests": ["sam@example.com"], "notify": true,
                         "id": CLIENT_ID});
    let (status, first, _) = send(
        &w.app,
        "POST",
        "/client/calendar/events",
        Some(request.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    assert_eq!(first["id"], CLIENT_ID);
    assert_eq!(writes(&w).len(), 1);
    assert_eq!(writes(&w)[0].body["id"], CLIENT_ID, "Google is sent the id");
    assert_eq!(writes(&w)[0].query["sendUpdates"], "all");
    assert_eq!(
        w.google().reads.last().map(|r| r.1.clone()),
        Some(CLIENT_ID.to_string()),
        "the id was read before the insert"
    );

    // The same press again: answered with that event, and Google is sent
    // nothing at all.
    let (status, again, _) = send(&w.app, "POST", "/client/calendar/events", Some(request)).await;
    eprintln!("second send: {status} {again}");
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again, first);
    assert_eq!(
        writes(&w).len(),
        1,
        "no second insert, no second invitation"
    );
    assert_eq!(
        audit(&w)[0],
        (
            "calendar_event_create".into(),
            Some(format!("calendar:primary/{CLIENT_ID}")),
            Some("ok:existing guests=1 notified=false".into())
        )
    );
    assert_eq!(
        w.events()
            .await
            .iter()
            .filter(|e| e["id"] == CLIENT_ID)
            .count(),
        1
    );
}

/// THE RE-READ AFTER A 409 FAILING (verify6 Z2). The first send made the
/// event and Google mailed its guest. The app sends again; the read ahead of
/// the insert misses it (a replica behind), Google answers the insert 409,
/// and the read that should fetch the event fails. Whatever that read failed
/// with, the daemon does not know what Google holds: 504 `uncertain` with a
/// sync kicked, never `google_refused` (which would thaw the pane for an
/// edited resend). And still one event, one invitation.
#[tokio::test]
async fn a_409_whose_reread_fails_is_uncertain_with_one_invitation() {
    for status in [500u16, 503, 404, 403, 429] {
        let (w, start) = guest_world(&format!("create-409-reread-{status}")).await;
        let request = json!({"calendar_id": "primary", "title": "Once", "start": rfc(start),
                             "end": rfc(start + Duration::hours(1)), "all_day": false,
                             "id": CLIENT_ID, "guests": ["nia@example.com"], "notify": true});
        let (code, _, _) = send(
            &w.app,
            "POST",
            "/client/calendar/events",
            Some(request.clone()),
        )
        .await;
        assert_eq!(code, StatusCode::CREATED, "{status}");
        // Whatever the first create kicked is not this test's.
        let _ = kicked(&w).await;
        let before = w.events().await;
        {
            let mut g = w.google();
            g.stale_reads = 1;
            g.read_fail_late = Some((1, status));
        }
        let (code, answer, _) =
            send(&w.app, "POST", "/client/calendar/events", Some(request)).await;
        eprintln!("409 then a re-read {status}: {code} {answer}");
        assert_eq!(code, StatusCode::GATEWAY_TIMEOUT, "{status}: {answer}");
        assert_eq!(answer["code"], json!("uncertain"), "{status}");
        assert!(kicked(&w).await, "{status}: a sync was kicked");
        assert_eq!(w.events().await, before, "{status}: no row changed");
        let sent = writes(&w);
        assert_eq!(
            sent.len(),
            2,
            "{status}: the second insert was sent and refused"
        );
        let held = w
            .google()
            .full
            .get(PRIMARY)
            .unwrap()
            .iter()
            .flatten()
            .filter(|e| e["id"] == CLIENT_ID)
            .count();
        assert_eq!(
            held, 1,
            "{status}: Google holds one event, so one invitation went out"
        );
        assert_eq!(
            audit(&w)[0]
                .2
                .as_deref()
                .map(|d| d.starts_with("failed:uncertain")),
            Some(true),
            "{status}: {:?}",
            audit(&w)
        );
    }
}

/// The other half: a read that has not caught up misses the event, and the
/// insert Google answers 409 (`duplicate`) is read as "the first send made
/// it": 200 with that event, still one event.
#[tokio::test]
async fn a_create_google_answers_409_is_the_event_already_made() {
    let (w, start) = write_world("create-409").await;
    let request = json!({"calendar_id": "primary", "title": "Once", "start": rfc(start),
                         "end": rfc(start + Duration::hours(1)), "all_day": false,
                         "id": CLIENT_ID});
    let (status, first, _) = send(
        &w.app,
        "POST",
        "/client/calendar/events",
        Some(request.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    w.google().stale_reads = 1;
    let (status, again, _) = send(&w.app, "POST", "/client/calendar/events", Some(request)).await;
    eprintln!("after a 409: {status} {again}");
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again, first);
    assert_eq!(
        writes(&w).len(),
        2,
        "the second insert was sent and refused"
    );
    let held = w
        .google()
        .full
        .get(PRIMARY)
        .unwrap()
        .iter()
        .flatten()
        .filter(|e| e["id"] == CLIENT_ID)
        .count();
    assert_eq!(held, 1, "Google holds one event");
    assert_eq!(
        audit(&w)[0].2.as_deref(),
        Some("ok:existing guests=0 notified=false")
    );
}

/// An id Google would refuse is refused here, before Google hears anything,
/// and one already made and since deleted is a 409, not a second event.
#[tokio::test]
async fn a_bad_or_spent_create_id_is_refused_and_reaches_nobody() {
    let (w, start) = write_world("create-bad-id").await;
    let body = |id: Value| {
        json!({"calendar_id": "primary", "title": "x", "start": rfc(start),
               "end": rfc(start + Duration::hours(1)), "all_day": false, "id": id})
    };
    for bad in [
        json!("abcd"),
        json!("ABCDEF12"),
        json!("abcdwxyz"),
        json!("abc-def-12"),
        json!(7),
        json!("a".repeat(1025)),
    ] {
        let (status, answer, _) =
            send(&w.app, "POST", "/client/calendar/events", Some(body(bad))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
        assert_eq!(
            answer,
            json!({"error": "id must be 5 to 1024 characters, each a lowercase letter a to v or a digit", "code": "invalid"})
        );
    }
    assert!(writes(&w).is_empty() && w.google().reads.is_empty());
    let mut spent = timed(CLIENT_ID, "Gone", start, 30);
    spent["status"] = json!("cancelled");
    w.google()
        .full
        .get_mut(PRIMARY)
        .unwrap()
        .last_mut()
        .unwrap()
        .push(spent);
    let (status, answer, _) = send(
        &w.app,
        "POST",
        "/client/calendar/events",
        Some(body(json!(CLIENT_ID))),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(
        answer,
        json!({"error": "that event was already created and has since been deleted", "code": "already_deleted"})
    );
    assert!(writes(&w).is_empty(), "nothing inserted");
}

/// A 404 ON A DELETE (verify2 V4) is "already deleted" only while the
/// calendar is still the user's: Google answers 404 too for a calendar the
/// user lost. A calendar no longer on their list is a 410 saying so, the
/// row stays for the kicked sync, and nothing says "deleted".
#[tokio::test]
async fn a_delete_404_on_a_calendar_no_longer_listed_is_refused() {
    let (w, _) = write_world("delete-404-unlisted").await;
    w.google().write_fail = Some(404);
    w.google().list.retain(|e| e["id"] != TEAM);
    let uri = event_uri(TEAM, "team-1");
    let (status, answer, _) = send(&w.app, "DELETE", &uri, None).await;
    eprintln!("404 on an unlisted calendar: {status} {answer}");
    assert_eq!(status, StatusCode::GONE, "{answer}");
    assert_eq!(
        answer,
        json!({"error": "that calendar is no longer available to you", "code": "calendar_gone"})
    );
    assert_eq!(w.google().list_gets, [TEAM]);
    assert!(
        served(&w, "team-1").await.is_some(),
        "the row is the sync's"
    );
    assert!(kicked(&w).await, "a sync was kicked");
    assert_eq!(
        audit(&w)[0].2.as_deref(),
        Some("failed:calendar_gone guests=0 notified=false")
    );
    // Still listed: the event is gone, and so is the row.
    let mut team_entry = list_entry(TEAM, "Team", true, false);
    team_entry["accessRole"] = json!("writer");
    w.google().list.push(team_entry);
    let (status, answer, _) = send(&w.app, "DELETE", &uri, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{answer}");
    assert!(served(&w, "team-1").await.is_none());
    assert_eq!(w.google().list_gets.len(), 2);
}

/// A WRITE THAT NEVER CONNECTED (verify2 V10) reached nobody: a 502 "did not
/// accept", never the 504 "may have happened", and no sync is kicked.
#[tokio::test]
async fn a_write_that_never_connects_is_a_502_and_kicks_nothing() {
    // Nothing listens on the discard port.
    let (w, start) = write_world_with("no-connect", |state, _| {
        state.with_calendar_api_base("http://127.0.0.1:9")
    })
    .await;
    let refused =
        json!({"error": "Google Calendar did not accept the change", "code": "google_refused"});
    let create = json!({"calendar_id": "primary", "title": "Nowhere", "start": rfc(start),
                        "end": rfc(start + Duration::hours(1)), "all_day": false});
    for (method, uri, body) in [
        ("POST", "/client/calendar/events".to_string(), Some(create)),
        (
            "PATCH",
            event_uri(PRIMARY, "own"),
            Some(json!({"title": "Nowhere"})),
        ),
        ("DELETE", event_uri(PRIMARY, "own"), None),
    ] {
        let (status, answer, _) = send(&w.app, method, &uri, body).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{method} {answer}");
        assert_eq!(answer, refused, "{method}");
        assert!(!kicked(&w).await, "{method}: nothing kicked");
        assert_eq!(
            audit(&w)[0]
                .2
                .as_deref()
                .map(|d| d.split(' ').next().unwrap()),
            Some("failed:google"),
            "{method}"
        );
    }
    assert_eq!(served(&w, "own").await.unwrap()["title"], "My event");
}

/// THE SAME ID FOR A DIFFERENT EVENT IS REFUSED (verify3 W1, W2). An event
/// already under a create's id answers it only when it is the event this
/// create would make. A changed title, time or guest list under the same id
/// is a 409 that says so: nothing inserted, nobody invited, and never a 200
/// with the old event standing in for the new one. The same through the
/// 409-then-read path.
#[tokio::test]
async fn a_create_whose_id_holds_a_different_event_is_refused() {
    let (w, start) = write_world("create-id-in-use").await;
    w.google().misses_duplicates = true;
    let request = json!({"calendar_id": "primary", "title": "Dinner", "start": rfc(start),
                         "end": rfc(start + Duration::hours(1)), "all_day": false,
                         "guests": ["sam@example.com"], "id": CLIENT_ID});
    let (status, first, _) = send(
        &w.app,
        "POST",
        "/client/calendar/events",
        Some(request.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let in_use = json!({"error": "that event id is already used for a different event", "code": "id_in_use"});
    type Change<'a> = (&'a str, &'a dyn Fn(&mut Value));
    let changed: [Change; 5] = [
        ("title", &|r| r["title"] = json!("Supper")),
        ("start", &|r| {
            r["start"] = json!(rfc(start - Duration::hours(1)))
        }),
        ("end", &|r| {
            r["end"] = json!(rfc(start + Duration::hours(2)))
        }),
        ("guest added, and told", &|r| {
            r["guests"] = json!(["sam@example.com", "kim@example.com"]);
            r["notify"] = json!(true);
        }),
        ("guest gone", &|r| r["guests"] = json!([])),
    ];
    for (what, change) in changed {
        let mut again = request.clone();
        change(&mut again);
        let (status, answer, _) =
            send(&w.app, "POST", "/client/calendar/events", Some(again)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{what}: {answer}");
        assert_eq!(answer, in_use, "{what}");
        assert_eq!(writes(&w).len(), 1, "{what}: nothing inserted");
        assert_eq!(
            audit(&w)[0]
                .2
                .as_deref()
                .map(|d| d.split(' ').next().unwrap()),
            Some("rejected:id_in_use"),
            "{what}"
        );
        assert!(
            audit(&w)[0]
                .2
                .as_deref()
                .unwrap()
                .ends_with("notified=false"),
            "{what}: this send told nobody"
        );
    }
    // Through Google's 409: a read that misses it, an insert refused, the
    // read after it finds the old event, and the changed draft is refused.
    w.google().misses_duplicates = false;
    w.google().stale_reads = 1;
    let mut moved = request.clone();
    moved["title"] = json!("Supper");
    let (status, answer, _) = send(&w.app, "POST", "/client/calendar/events", Some(moved)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer, in_use);
    assert_eq!(writes(&w).len(), 2, "the insert Google refused");
    // The draft as first sent is still answered with its event.
    let (status, same, _) = send(&w.app, "POST", "/client/calendar/events", Some(request)).await;
    assert_eq!(status, StatusCode::OK, "{same}");
    assert_eq!(same, first);
    let held = w
        .google()
        .full
        .get(PRIMARY)
        .unwrap()
        .iter()
        .flatten()
        .filter(|e| e["id"] == CLIENT_ID)
        .count();
    assert_eq!(held, 1);
    assert_eq!(served(&w, CLIENT_ID).await.unwrap()["title"], "Dinner");
}

/// ONE CREATE PER ID AT A TIME (verify3 W9). A second create naming an id
/// whose first create is still out is a 409 at once, before Google hears
/// it; the key is let go when the first finishes, and when the deadline
/// drops it.
#[tokio::test]
async fn a_second_create_with_an_id_in_flight_is_refused_without_google() {
    let (w, start) = write_world_with("create-in-flight", |state, _| {
        state.with_calendar_write_deadline(StdDuration::from_millis(1500))
    })
    .await;
    let request = json!({"calendar_id": "primary", "title": "Twice", "start": rfc(start),
                         "end": rfc(start + Duration::hours(1)), "all_day": false,
                         "guests": ["sam@example.com"], "notify": true, "id": CLIENT_ID});
    w.google().answer_delay = Some(StdDuration::from_millis(600));
    let app = w.app.clone();
    let body = request.clone();
    let first =
        tokio::spawn(
            async move { send(&app, "POST", "/client/calendar/events", Some(body)).await },
        );
    tokio::time::sleep(StdDuration::from_millis(200)).await;
    let reads_before = w.google().reads.len();
    let (status, answer, _) = send(
        &w.app,
        "POST",
        "/client/calendar/events",
        Some(request.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(
        answer,
        json!({"error": "that event is already being created", "code": "already_creating"})
    );
    assert_eq!(w.google().reads.len(), reads_before, "Google was not read");
    assert_eq!(
        audit(&w)[0]
            .2
            .as_deref()
            .map(|d| d.split(' ').next().unwrap()),
        Some("rejected:in_flight")
    );
    let (status, made, _) = first.await.unwrap();
    assert_eq!(status, StatusCode::CREATED, "{made}");
    assert_eq!(writes(&w).len(), 1, "one insert, one invitation");
    // Done: the key is free, and the same create is answered by its event.
    w.google().answer_delay = None;
    let (status, again, _) = send(
        &w.app,
        "POST",
        "/client/calendar/events",
        Some(request.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    // A create the deadline drops lets its key go too.
    let mut other = request.clone();
    other["id"] = json!("0f3c9a7e5b2d4e6f8a1b3c5d7e9f0a2c");
    w.google().answer_delay = Some(StdDuration::from_secs(3));
    let (status, _, _) = send(
        &w.app,
        "POST",
        "/client/calendar/events",
        Some(other.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    w.google().answer_delay = None;
    let (status, answer, _) = send(&w.app, "POST", "/client/calendar/events", Some(other)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the made event, not 'being created': {answer}"
    );
    assert_eq!(writes(&w).len(), 2);
}

/// A 404 delete on a calendar still LISTED but no longer written to by the
/// user (verify3 W8) is not "already deleted": an event the user cannot see
/// is a 404 to them too. Refused like an unlisted calendar, the row kept.
#[tokio::test]
async fn a_delete_404_on_a_calendar_the_user_only_reads_now_is_refused() {
    let (w, _) = write_world("delete-404-reader").await;
    w.google().write_fail = Some(404);
    for entry in w.google().list.iter_mut().filter(|e| e["id"] == TEAM) {
        entry["accessRole"] = json!("freeBusyReader");
    }
    let uri = event_uri(TEAM, "team-1");
    let (status, answer, _) = send(&w.app, "DELETE", &uri, None).await;
    assert_eq!(status, StatusCode::GONE, "{answer}");
    assert_eq!(
        answer,
        json!({"error": "that calendar is no longer available to you", "code": "calendar_gone"})
    );
    assert!(
        served(&w, "team-1").await.is_some(),
        "the row is the sync's"
    );
    // An owner's role is still the user's: the 404 is the event gone.
    for entry in w.google().list.iter_mut().filter(|e| e["id"] == TEAM) {
        entry["accessRole"] = json!("owner");
    }
    let (status, _, _) = send(&w.app, "DELETE", &uri, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(served(&w, "team-1").await.is_none());
}

/// A 404 delete whose calendar list read FAILS (verify3 W7) cannot say the
/// calendar is still the user's: a 502, nothing deleted here, the row kept.
#[tokio::test]
async fn a_delete_404_whose_list_read_fails_deletes_nothing() {
    let (w, _) = write_world("delete-404-list-fails").await;
    w.google().write_fail = Some(404);
    let uri = event_uri(TEAM, "team-1");
    for failure in [500, 403] {
        w.google().list_get_status = Some(failure);
        let (status, answer, _) = send(&w.app, "DELETE", &uri, None).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{failure}: {answer}");
        assert_eq!(
            answer,
            json!({"error": "Google Calendar did not accept the change", "code": "google_refused"})
        );
        assert!(served(&w, "team-1").await.is_some(), "{failure}: row kept");
        assert_eq!(
            audit(&w)[0].2.as_deref(),
            Some("failed:google guests=0 notified=false")
        );
    }
}

/// EVERY REFUSAL NAMES ITS KIND (verify4 X1): each of the daemon's own
/// refusals before Google carries its `code`, with its status, from the
/// closed list in docs/CALENDAR.md §8. The app keys on these codes to decide
/// that nothing happened; the rest (`google_refused`, `uncertain`, the 409s,
/// `calendar_gone`, `not_granted`) are pinned where those answers are tested.
#[tokio::test]
async fn every_refusal_before_google_names_its_code() {
    let (w, start) = guest_world("codes").await;
    let create = |calendar: &str, guests: Value| {
        json!({"calendar_id": calendar, "title": "x", "start": rfc(start),
               "end": rfc(start + Duration::hours(1)), "all_day": false, "guests": guests})
    };
    let cases = [
        (
            "POST",
            "/client/calendar/events".to_string(),
            Some(json!({})),
            StatusCode::BAD_REQUEST,
            "invalid",
        ),
        (
            "POST",
            "/client/calendar/events".to_string(),
            Some(create("nope@example.com", json!([]))),
            StatusCode::BAD_REQUEST,
            "unknown_calendar",
        ),
        (
            "POST",
            "/client/calendar/events".to_string(),
            Some(create(HOLIDAYS, json!([]))),
            StatusCode::BAD_REQUEST,
            "read_only",
        ),
        (
            "POST",
            "/client/calendar/events".to_string(),
            Some(create(TEAM, json!([SAM]))),
            StatusCode::BAD_REQUEST,
            "cannot_invite",
        ),
        (
            "PATCH",
            event_uri(PRIMARY, "big"),
            Some(json!({"guests": [SAM]})),
            StatusCode::BAD_REQUEST,
            "guests_read_only",
        ),
        (
            "PATCH",
            event_uri(TEAM, "team-1"),
            Some(json!({"title": "x", "notify": true})),
            StatusCode::BAD_REQUEST,
            "cannot_notify",
        ),
        (
            "PATCH",
            event_uri(PRIMARY, "invite"),
            Some(json!({"title": "x"})),
            StatusCode::FORBIDDEN,
            "not_editable",
        ),
        (
            "DELETE",
            event_uri(PRIMARY, "nope"),
            None,
            StatusCode::NOT_FOUND,
            "not_found",
        ),
    ];
    for (method, uri, body, status, code) in cases {
        let (got, answer, _) = send(&w.app, method, &uri, body).await;
        assert_eq!(got, status, "{code}: {answer}");
        assert_eq!(answer["code"], code, "{answer}");
        assert!(answer["error"].is_string(), "the human message stays");
    }
    assert!(writes(&w).is_empty(), "nothing reached Google");
}

// --- the card's place ---------------------------------------------------------

/// A fake model that answers every call with `name` and `address`, counting
/// the calls and keeping each request body.
async fn spawn_place_model(
    name: &'static str,
    address: &'static str,
) -> (String, Arc<Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let bodies: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = bodies.clone();
    let answer = json!({ "name": name, "address": address }).to_string();
    let body = json!({
        "content": [{ "type": "text", "text": answer }],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 200, "output_tokens": 8 }
    })
    .to_string();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let body = body.clone();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                seen.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf[..n]).into_owned());
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    (format!("http://{addr}"), bodies)
}

/// After a pass, a booking service's location is cleaned once by the model
/// and served as `place`, beside the untouched `location`; a short location
/// makes no call and carries no `place`; and Google's own boilerplate never
/// reaches `description`.
#[tokio::test]
async fn a_noisy_location_is_served_as_a_clean_place_and_boilerplate_notes_go() {
    use squelch_core::config::Stage2Provider;
    use squelch_core::gcal::place::{PlaceClient, USAGE_CATEGORY};
    let w = world("place", GRANTED).await;
    let tock = "Dingles Public House, Tock is now part of the Resy network. \
                Explore even more of our dining network on";
    let start = minute(Utc::now() + Duration::days(2));
    let mut dinner = timed("evt-1", "Reservation at Dingles Public House", start, 60);
    dinner["location"] = json!(tock);
    dinner["description"] = json!(
        "To see detailed information for automatically created events like this one, \
         use the official Google Calendar app. https://g.co/calendar"
    );
    let mut again = timed("evt-2", "Dinner again", start + Duration::days(7), 60);
    again["location"] = json!(tock);
    let mut call = timed("evt-3", "Call", start + Duration::days(1), 30);
    call["location"] = json!("Zoom");
    w.google()
        .full
        .insert(PRIMARY.into(), vec![vec![dinner, again, call]]);

    let (url, bodies) = spawn_place_model("Dingles Public House", "").await;
    let sync = CalendarSync::with_api_base(w.store.clone(), w.acct, w.creds.clone(), &w.base)
        .unwrap()
        .with_places(Some(PlaceClient::new(
            reqwest::Client::new(),
            url,
            "sk-test",
            Stage2Provider::Anthropic,
            "claude-haiku-4-5",
            None,
        )));
    assert_eq!(sync.pass(Utc::now()).await, PassOutcome::Synced);

    // Before the model has answered: no `place`, the location as it was.
    let events = w.events().await;
    assert!(events.iter().all(|e| e.get("place").is_none()));
    assert_eq!(events[0]["location"], json!(tock));
    assert_eq!(
        events[0]["description"],
        Value::Null,
        "boilerplate is not notes"
    );

    sync.clean_places(Utc::now()).await;
    // One call for the one long location, carrying it and nothing else of
    // the event.
    {
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1);
        assert!(bodies[0].contains("Tock is now part of the Resy network"));
        assert!(!bodies[0].contains("Reservation at Dingles"));
        assert!(!bodies[0].contains("Zoom"));
    }
    let events = w.events().await;
    let by_id = |id: &str| events.iter().find(|e| e["id"] == id).unwrap().clone();
    let dingles = json!({ "name": "Dingles Public House", "address": null, "original": tock });
    assert_eq!(by_id("evt-1")["place"], dingles);
    assert_eq!(by_id("evt-1")["location"], json!(tock), "the editor's text");
    assert_eq!(by_id("evt-2")["place"], dingles);
    assert!(by_id("evt-3").get("place").is_none());
    assert_eq!(by_id("evt-3")["location"], json!("Zoom"));

    // Billed to its own category; asked once, not again.
    let billed = w
        .store
        .list_usage_by_category(w.acct, 1)
        .unwrap()
        .into_iter()
        .find(|(c, _)| c == USAGE_CATEGORY)
        .map(|(_, days)| days.iter().map(|d| d.calls).sum::<u64>());
    assert_eq!(billed, Some(1));
    sync.clean_places(Utc::now()).await;
    assert_eq!(bodies.lock().unwrap().len(), 1);
}

/// A model answer that adds a word the location never had, in its name or
/// its address, is not believed: the location is served as the place's name,
/// unchanged, with no address.
#[tokio::test]
async fn a_place_answer_that_adds_a_word_keeps_the_location() {
    use squelch_core::config::Stage2Provider;
    use squelch_core::gcal::place::PlaceClient;
    let w = world("place-added", GRANTED).await;
    let noisy = "Harbor Tavern. Book your next table with us on our brand new app today";
    let mut dinner = timed(
        "evt-1",
        "Dinner",
        minute(Utc::now() + Duration::days(2)),
        60,
    );
    dinner["location"] = json!(noisy);
    w.google().full.insert(PRIMARY.into(), vec![vec![dinner]]);
    let (url, _) = spawn_place_model("Harbor Tavern", "12 Water St").await;
    let sync = CalendarSync::with_api_base(w.store.clone(), w.acct, w.creds.clone(), &w.base)
        .unwrap()
        .with_places(Some(PlaceClient::new(
            reqwest::Client::new(),
            url,
            "sk-test",
            Stage2Provider::Anthropic,
            "claude-haiku-4-5",
            None,
        )));
    assert_eq!(sync.pass(Utc::now()).await, PassOutcome::Synced);
    sync.clean_places(Utc::now()).await;
    assert_eq!(
        w.events().await[0]["place"],
        json!({ "name": noisy, "address": null, "original": noisy })
    );
}

/// A location with a street address in it is served as the place's name and
/// address apart, beside the location it came from; and the event Passband
/// makes in place of Gmail's copy carries both, as one line, so the address
/// is not lost when Gmail's copy is deleted.
#[tokio::test]
async fn a_place_keeps_its_address_apart_and_on_the_adopted_event() {
    use squelch_core::config::Stage2Provider;
    use squelch_core::gcal::place::PlaceClient;
    let w = world("place-address", GRANTED).await;
    let booked = "Harbor Tavern, 12 Water St, Portland. Tock is now part of the Resy \
                  network. Explore even more of our dining network on";
    let start = minute(Utc::now() + Duration::days(2));
    let mut gmail = timed("gm1", "Reservation at Harbor Tavern", start, 60);
    gmail["eventType"] = json!("fromGmail");
    gmail["location"] = json!(booked);
    gmail["organizer"] = json!({"email": "me@example.com", "self": true});
    w.google().full.insert(PRIMARY.into(), vec![vec![gmail]]);
    seed_ghost(
        &w.store,
        w.acct,
        "g1",
        "Dinner at Harbor Tavern",
        rfc(start),
    );
    let (url, _) = spawn_place_model("Harbor Tavern", "12 Water St, Portland").await;
    let sync = CalendarSync::with_api_base(w.store.clone(), w.acct, w.creds.clone(), &w.base)
        .unwrap()
        .with_places(Some(PlaceClient::new(
            reqwest::Client::new(),
            url,
            "sk-test",
            Stage2Provider::Anthropic,
            "claude-haiku-4-5",
            None,
        )));
    assert_eq!(sync.pass(Utc::now()).await, PassOutcome::Synced);
    sync.clean_places(Utc::now()).await;
    assert_eq!(
        w.events().await[0]["place"],
        json!({ "name": "Harbor Tavern", "address": "12 Water St, Portland", "original": booked })
    );

    sync.own_events(Utc::now()).await;
    let g = w.google();
    let insert = g
        .writes
        .iter()
        .find(|w| w.method == "POST")
        .expect("an adoption");
    assert_eq!(
        insert.body["location"],
        "Harbor Tavern, 12 Water St, Portland"
    );
}

// --- Passband owns Gmail's events -------------------------------------------

/// Gmail's own copy of a reservation on the user's calendar is replaced by
/// Passband's event, which nobody is emailed about and which the audit log
/// records; an invitation Gmail added from someone else's mail is left
/// alone; a second pass changes nothing; and when Gmail adds its copy again,
/// it is deleted as a duplicate.
#[tokio::test]
async fn passband_takes_over_gmails_own_events_and_leaves_invitations_alone() {
    use squelch_core::gcal::own::{AUDIT_ACTOR, OWNED_NOTE};
    let w = world("own", GRANTED).await;
    let start = minute(Utc::now() + Duration::days(2));
    let later = start + Duration::days(1);
    let mut gmail = timed("gm1", "Reservation at Dingles Public House", start, 60);
    gmail["eventType"] = json!("fromGmail");
    gmail["location"] = json!("Dingles Public House, 165 Main St");
    gmail["organizer"] = json!({"email": "me@example.com", "self": true});
    let mut invite = timed("gm2", "Drinks at Bemelmans", later, 60);
    invite["eventType"] = json!("fromGmail");
    invite["organizer"] = json!({"email": "ellie@example.com"});
    invite["attendees"] = json!([
        {"email": "ellie@example.com", "organizer": true, "responseStatus": "accepted"},
        {"email": "me@example.com", "self": true, "responseStatus": "accepted"}
    ]);
    w.google()
        .full
        .insert(PRIMARY.into(), vec![vec![gmail.clone(), invite]]);
    seed_ghost(&w.store, w.acct, "g1", "Dinner at Dingles", rfc(start));
    seed_ghost(&w.store, w.acct, "g2", "Drinks at Bemelmans", rfc(later));
    assert_eq!(w.pass().await, PassOutcome::Synced);

    w.sync.own_events(Utc::now()).await;
    let new_id = {
        let g = w.google();
        assert_eq!(g.writes.len(), 2, "{:?}", g.writes);
        let insert = &g.writes[0];
        assert_eq!((insert.method, insert.calendar.as_str()), ("POST", PRIMARY));
        assert_eq!(insert.query["sendUpdates"], "none");
        assert_eq!(
            insert.body["summary"],
            "Reservation at Dingles Public House"
        );
        assert_eq!(insert.body["description"], OWNED_NOTE);
        assert_eq!(insert.body["start"]["dateTime"], json!(rfc(start)));
        let id = insert.body["id"].as_str().unwrap().to_string();
        assert!(id.starts_with("passband"));
        let delete = &g.writes[1];
        assert_eq!(
            (delete.method, delete.event.as_deref()),
            ("DELETE", Some("gm1"))
        );
        assert_eq!(delete.query["sendUpdates"], "none");
        id
    };

    // Served: Passband's event, editable, in Gmail's place; the invitation
    // as it was.
    let events = w.events().await;
    let ids: Vec<&str> = events.iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [new_id.as_str(), "gm2"]);
    assert_eq!(events[0]["editable"], json!(true));
    assert_eq!(events[0]["from_gmail"], json!(false));
    assert_eq!(events[1]["from_gmail"], json!(true));
    assert_eq!(events[1]["editable"], json!(false));
    assert_eq!(events[0]["passband_owned"], json!(true));
    assert!(events[1].get("passband_owned").is_none());
    // The ghost Passband made it for is matched to it, though the titles
    // differ, so the timeline draws one item, not two.
    let ghosts = w.timeline().await["ghosts"].as_array().unwrap().clone();
    let dinner = ghosts
        .iter()
        .find(|g| g["title"] == "Dinner at Dingles")
        .unwrap();
    assert_eq!(
        dinner["matched_event"],
        json!({"calendar_id": PRIMARY, "id": new_id})
    );

    // Audited, with no title or place.
    let rows = w.store.list_audit(w.acct, 10).unwrap();
    let actions: Vec<(&str, &str, &str)> = rows
        .iter()
        .map(|r| {
            (
                r.actor.as_str(),
                r.action.as_str(),
                r.detail.as_deref().unwrap_or(""),
            )
        })
        .collect();
    assert!(actions.contains(&(AUDIT_ACTOR, "calendar_event_adopt", "ok")));
    assert!(actions.contains(&(AUDIT_ACTOR, "calendar_event_replace_gmail", "ok")));
    let text = serde_json::to_string(&rows).unwrap();
    for secret in ["Dingles", "Bemelmans", "Main St"] {
        assert!(!text.contains(secret), "the audit log carries {secret}");
    }

    // Again: nothing to do.
    w.sync.own_events(Utc::now()).await;
    assert_eq!(w.google().writes.len(), 2);

    // Gmail adds its copy back: the next sync brings it, and it goes.
    let mut again = gmail.clone();
    again["id"] = json!("gm3");
    w.google().changes.insert(
        PRIMARY.into(),
        HashMap::from([(
            "tok-primary-1".to_string(),
            (vec![again], "tok-primary-2".to_string()),
        )]),
    );
    assert_eq!(w.pass().await, PassOutcome::Synced);
    w.sync.own_events(Utc::now()).await;
    {
        let g = w.google();
        assert_eq!(g.writes.len(), 3);
        assert_eq!(
            (g.writes[2].method, g.writes[2].event.as_deref()),
            ("DELETE", Some("gm3"))
        );
    }
    let ids: Vec<String> = w
        .events()
        .await
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, [new_id, "gm2".to_string()]);

    // Once more, and Google will not let this copy go: asked once, not
    // again every pass.
    let mut stuck = gmail;
    stuck["id"] = json!("gm4");
    w.google().changes.insert(
        PRIMARY.into(),
        HashMap::from([(
            "tok-primary-2".to_string(),
            (vec![stuck], "tok-primary-3".to_string()),
        )]),
    );
    assert_eq!(w.pass().await, PassOutcome::Synced);
    w.google().fail_delete_of = Some("gm4".into());
    w.sync.own_events(Utc::now()).await;
    assert_eq!(w.google().writes.len(), 4);
    w.sync.own_events(Utc::now()).await;
    w.sync.own_events(Utc::now()).await;
    assert_eq!(
        w.google().writes.len(),
        4,
        "a refused duplicate is not retried"
    );
}

/// When Google refuses to delete Gmail's copy, Passband's event is taken
/// back off, the pair is left as Gmail made it, and it is not tried again.
#[tokio::test]
async fn a_refused_gmail_delete_takes_passbands_event_back_off_for_good() {
    let w = world("own-refused", GRANTED).await;
    let start = minute(Utc::now() + Duration::days(2));
    let mut gmail = timed("gm1", "Reservation at Dingles Public House", start, 60);
    gmail["eventType"] = json!("fromGmail");
    gmail["organizer"] = json!({"email": "me@example.com", "self": true});
    w.google().full.insert(PRIMARY.into(), vec![vec![gmail]]);
    seed_ghost(&w.store, w.acct, "g1", "Dinner at Dingles", rfc(start));
    assert_eq!(w.pass().await, PassOutcome::Synced);
    w.google().fail_delete_of = Some("gm1".into());
    w.sync.own_events(Utc::now()).await;
    {
        let g = w.google();
        let methods: Vec<(&str, Option<&str>)> = g
            .writes
            .iter()
            .map(|w| (w.method, w.event.as_deref()))
            .collect();
        let id = g.writes[0].body["id"].as_str().unwrap().to_string();
        assert_eq!(
            methods,
            [
                ("POST", None),
                ("DELETE", Some("gm1")),
                ("DELETE", Some(id.as_str()))
            ]
        );
    }
    let ids: Vec<String> = w
        .events()
        .await
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, ["gm1"]);
    w.sync.own_events(Utc::now()).await;
    assert_eq!(w.google().writes.len(), 3, "never tried again");
}

/// A refused duplicate delete is not spent again every pass; and when the
/// undo of an adoption is refused too, both copies are not left for good:
/// the undo is tried again once the refusal has cooled, even after the event
/// has left the listing window.
#[tokio::test]
async fn refusals_are_not_retried_every_pass_and_a_failed_undo_is_retried() {
    let w = world("own-undo", GRANTED).await;
    let start = minute(Utc::now() + Duration::days(2));
    let mut gmail = timed("gm1", "Reservation at Dingles Public House", start, 60);
    gmail["eventType"] = json!("fromGmail");
    gmail["organizer"] = json!({"email": "me@example.com", "self": true});
    w.google().full.insert(PRIMARY.into(), vec![vec![gmail]]);
    seed_ghost(&w.store, w.acct, "g1", "Dinner at Dingles", rfc(start));
    assert_eq!(w.pass().await, PassOutcome::Synced);
    w.google().fail_deletes = true;
    w.sync.own_events(Utc::now()).await;
    let own_id = {
        let g = w.google();
        let methods: Vec<&str> = g.writes.iter().map(|w| w.method).collect();
        assert_eq!(methods, ["POST", "DELETE", "DELETE"]);
        g.writes[0].body["id"].as_str().unwrap().to_string()
    };
    // Both refused: nothing is asked again while the refusals are fresh.
    w.sync.own_events(Utc::now()).await;
    w.sync.own_events(Utc::now()).await;
    assert_eq!(w.google().writes.len(), 3, "not spent again every pass");

    // A week on, Google allows it: the undo lands, though the event is now
    // outside the listing window, and the pair is as Gmail made it.
    w.google().fail_deletes = false;
    let later = Utc::now() + Duration::days(8);
    w.sync.own_events(later).await;
    {
        let g = w.google();
        assert_eq!(g.writes.len(), 4, "{:?}", g.writes);
        assert_eq!(
            (g.writes[3].method, g.writes[3].event.as_deref()),
            ("DELETE", Some(own_id.as_str()))
        );
    }
    let ids: Vec<String> = w
        .events()
        .await
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, ["gm1"]);
    w.sync.own_events(later).await;
    assert_eq!(w.google().writes.len(), 4, "done for good");
}

// ---------------------------------------------------------------------------
// Calendar search (docs/CALENDAR.md §10.1): `GET /client/calendar/search` and
// the assistant's `GET /client/agent/calendar/search`, over events the real
// sync stored and ghosts the real projection made.

async fn search(app: &axum::Router, route: &str, q: &str) -> Value {
    let q: String = url::form_urlencoded::byte_serialize(q.as_bytes()).collect();
    get(app, &format!("{route}?q={q}")).await
}

fn hit_titles(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            let (kind, v) = match (&item["event"], &item["ghost"]) {
                (Value::Null, ghost) => ("ghost", ghost),
                (event, _) => ("event", event),
            };
            format!("{kind}:{}", v["title"].as_str().unwrap_or(""))
        })
        .collect()
}

/// `event` with the owner's own RSVP a no.
fn declined(mut event: Value) -> Value {
    event["attendees"] = json!([
        {"email": "me@example.com", "self": true, "responseStatus": "declined"}
    ]);
    event
}

/// The fixture every search test reads: two Sam dinners (one ahead, one past,
/// the one ahead with Sam as a guest at a private address), a third the owner
/// declined (never served), a standup, and a ghost from mail. Synced and projected.
async fn search_world(tag: &str) -> World {
    let w = world(tag, GRANTED).await;
    let ahead = minute(Utc::now() + Duration::days(3));
    let past = minute(Utc::now() - Duration::days(10));
    let mut dinner = timed("evt-dinner", "Dinner w/ Sam", ahead, 90);
    dinner["location"] = json!("Nopa");
    dinner["attendees"] = json!([
        {"email": "sam@hidden-domain.test", "displayName": "Sam Patel",
         "responseStatus": "accepted", "organizer": true},
        {"email": "me@example.com", "self": true, "responseStatus": "accepted"},
        {"email": "nameless@hidden-domain.test", "responseStatus": "needsAction"}
    ]);
    dinner["organizer"] = json!({"email": "sam@hidden-domain.test"});
    dinner["description"] = json!("Bring the wine");
    w.google().full.insert(
        PRIMARY.into(),
        vec![vec![
            dinner,
            timed("evt-old", "Dinner w/ Sam", past, 90),
            timed("evt-standup", "Team standup", ahead, 15),
            declined(timed("evt-no", "Dinner w/ Sam's cousin", ahead, 60)),
        ]],
    );
    assert_eq!(w.pass().await, PassOutcome::Synced);
    seed_ghost(
        &w.store,
        w.acct,
        "g-bday",
        "Sam's birthday dinner",
        rfc(minute(Utc::now() + Duration::days(12))),
    );
    w
}

#[tokio::test]
async fn calendar_search_lists_upcoming_soonest_then_past_newest_with_ghosts() {
    let w = search_world("search-order").await;
    let body = search(&w.app, "/client/calendar/search", "dinner sam").await;
    eprintln!("dinner sam: {body}");
    assert_eq!(
        hit_titles(&body),
        [
            "event:Dinner w/ Sam",
            "ghost:Sam's birthday dinner",
            "event:Dinner w/ Sam"
        ]
    );
    assert_eq!(body["total"], 3);
    assert_eq!(body["granted"], true);
    // Each item is the timeline's own view: the first is the one ahead,
    // served whole, guests and all.
    let first = &body["items"][0]["event"];
    assert_eq!(first["id"], "evt-dinner");
    assert_eq!(first["location"], "Nopa");
    assert_eq!(first["guests"].as_array().unwrap().len(), 3);
    assert!(body["items"][1]["ghost"]["sources"][0]["thread_id"].is_string());

    // Plurals, prefixes, the place, a guest's name and (for the owner) a
    // guest's address all find it; inside a word does not.
    for q in ["dinners", "din", "nopa", "patel", "hidden-domain", "wine"] {
        let body = search(&w.app, "/client/calendar/search", q).await;
        assert!(
            hit_titles(&body).contains(&"event:Dinner w/ Sam".to_string()),
            "{q}: {body}"
        );
    }
    let none = search(&w.app, "/client/calendar/search", "inner").await;
    assert_eq!(none["total"], 0, "{none}");

    // The limit cuts the list, not the count.
    let one = get(&w.app, "/client/calendar/search?q=dinner&limit=1").await;
    assert_eq!(one["items"].as_array().unwrap().len(), 1);
    assert_eq!(one["total"], 3);

    // Mail operators ask nothing of the calendar; dates bound the start.
    for q in [
        "from:sam dinner",
        "to:sam dinner",
        "in:sent dinner",
        "",
        "a",
    ] {
        let body = search(&w.app, "/client/calendar/search", q).await;
        assert_eq!(body["total"], 0, "{q}: {body}");
        assert_eq!(body["items"], json!([]));
    }
    let after = (Utc::now() + Duration::days(1)).format("%Y-%m-%d");
    let body = search(
        &w.app,
        "/client/calendar/search",
        &format!("dinner after:{after}"),
    )
    .await;
    assert_eq!(
        hit_titles(&body),
        ["event:Dinner w/ Sam", "ghost:Sam's birthday dinner"]
    );

    // No word has all three, so the ones missing exactly one are served.
    let body = search(&w.app, "/client/calendar/search", "dinner sam zebra").await;
    assert_eq!(body["total"], 3, "{body}");
}

#[tokio::test]
async fn the_agent_view_names_people_and_never_addresses_them() {
    let w = search_world("search-agent").await;
    let body = search(&w.app, "/client/agent/calendar/search", "dinner sam").await;
    eprintln!("agent: {body}");
    let text = body.to_string();
    assert!(!text.contains('@'), "an address reached the agent: {text}");
    assert!(!text.contains("hidden-domain"), "{text}");
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(body["total"], 3);
    let first = &results[0];
    assert_eq!(first["kind"], "event");
    assert_eq!(first["title"], "Dinner w/ Sam");
    assert_eq!(first["guests"], json!(["Sam Patel"]));
    // Sam and the nameless guest; never the owner.
    assert_eq!(first["guest_count"], 2);
    assert_eq!(first["organizer"], "Sam Patel");
    assert_eq!(first["organizer_is_you"], false);
    assert_eq!(first["description"], "Bring the wine");
    assert_eq!(first["thread_id"], Value::Null);
    let ghost = &results[1];
    assert_eq!(ghost["kind"], "ghost");
    assert_eq!(ghost["thread_id"], "t-g-bday");

    // An address is not searchable here: a word only an address holds finds
    // nothing, where the owner's own search finds the event.
    let agent = search(&w.app, "/client/agent/calendar/search", "hidden-domain").await;
    assert_eq!(agent["total"], 0, "{agent}");
    let owner = search(&w.app, "/client/calendar/search", "hidden-domain").await;
    assert_eq!(owner["total"], 1, "{owner}");
}

#[tokio::test]
async fn a_ghost_that_is_a_real_event_is_served_once_as_the_event() {
    let w = world("search-twin", GRANTED).await;
    let at = minute(Utc::now() + Duration::days(4));
    w.google().full.insert(
        PRIMARY.into(),
        vec![vec![timed("evt-show", "Big Thief", at, 120)]],
    );
    assert_eq!(w.pass().await, PassOutcome::Synced);
    seed_ghost(&w.store, w.acct, "g-show", "Big Thief", rfc(at));
    // The timeline draws the ghost as the event...
    let timeline = w.timeline().await;
    assert!(
        timeline["ghosts"][0]["matched_event"].is_object(),
        "{timeline}"
    );
    // ...and search serves the event alone.
    let body = search(&w.app, "/client/calendar/search", "big thief").await;
    assert_eq!(hit_titles(&body), ["event:Big Thief"], "{body}");
}

#[tokio::test]
async fn calendar_search_follows_the_grant() {
    let w = search_world("search-gate").await;
    // A grant taken back hides the synced events; the ghost stays.
    std::fs::remove_file(&w.path).unwrap();
    let body = search(&w.app, "/client/calendar/search", "dinner").await;
    assert_eq!(body["granted"], false, "{body}");
    assert_eq!(hit_titles(&body), ["ghost:Sam's birthday dinner"]);
}

#[tokio::test]
async fn the_agent_gets_no_ghost_whose_thread_it_may_not_open() {
    let w = search_world("search-guard").await;
    // A later message in the ghost's thread, never assessed: the thread is
    // closed to the agent door (get_thread would refuse it), so its ghost is
    // not the agent's to see, though the owner still sees it.
    w.store
        .upsert_message(&NewMessage {
            received_at: Utc::now() - Duration::minutes(1),
            auth_pass: Some(true),
            ..msg(w.acct, "g-bday-2", "t-g-bday", "Re: Your plans", "more")
        })
        .unwrap();
    assert!(!w.store.external_thread_allowed(w.acct, "t-g-bday").unwrap());
    let owner = search(&w.app, "/client/calendar/search", "birthday").await;
    assert_eq!(
        hit_titles(&owner),
        ["ghost:Sam's birthday dinner"],
        "{owner}"
    );
    let agent = search(&w.app, "/client/agent/calendar/search", "birthday").await;
    assert_eq!(agent["total"], 0, "{agent}");
}

// --- Adversarial pass, 2026-10-28 -----------------------------------------

#[tokio::test]
async fn a_half_typed_word_finds_its_event_as_the_user_types() {
    let w = world("search-typing", GRANTED).await;
    let at = minute(Utc::now() + Duration::days(2));
    w.google().full.insert(
        PRIMARY.into(),
        vec![vec![
            timed("evt-m", "Team Meeting", at, 30),
            timed("evt-c", "Conference call", at, 30),
            timed("evt-p", "Birthday party", at, 30),
        ]],
    );
    assert_eq!(w.pass().await, PassOutcome::Synced);
    // Every prefix of the word, not just the ones porter leaves alone.
    for q in [
        "me", "mee", "meet", "meeti", "meetin", "meeting", "meetings",
    ] {
        let body = search(&w.app, "/client/calendar/search", q).await;
        assert_eq!(hit_titles(&body), ["event:Team Meeting"], "{q}: {body}");
    }
    for q in ["conferenc", "conferences", "parties", "party"] {
        let body = search(&w.app, "/client/calendar/search", q).await;
        assert_eq!(body["total"], 1, "{q}: {body}");
    }
}

#[tokio::test]
async fn no_address_reaches_the_agent_through_names_titles_or_notes() {
    let w = world("search-addr", GRANTED).await;
    let at = minute(Utc::now() + Duration::days(2));
    let mut sync = timed("evt-s", "Sync with ops@corp.example", at, 30);
    sync["description"] = json!("host carol@corp.example, dial in");
    sync["location"] = json!("ask frontdesk@corp.example");
    sync["attendees"] = json!([
        {"email": "dan@evil.com", "displayName": "dan@evil.com",
         "responseStatus": "accepted", "organizer": true},
        {"email": "me@example.com", "self": true, "responseStatus": "accepted"}
    ]);
    w.google().full.insert(PRIMARY.into(), vec![vec![sync]]);
    assert_eq!(w.pass().await, PassOutcome::Synced);
    // A coworker's calendar Google named after their address.
    w.store
        .calendar_note_meta(w.acct, PRIMARY, Some("boss.person@corp.example"), None)
        .unwrap();

    let body = search(&w.app, "/client/agent/calendar/search", "sync").await;
    eprintln!("agent: {body}");
    let text = body.to_string();
    assert!(!text.contains('@'), "an address reached the agent: {text}");
    let hit = &body["results"][0];
    assert_eq!(hit["guests"], json!([]));
    assert_eq!(hit["guest_count"], 1);
    assert_eq!(hit["organizer"], Value::Null);
    assert_eq!(hit["calendar"], Value::Null);
    assert!(hit["description"].as_str().unwrap().contains("(address)"));
    // Nor can a word only an address holds find it.
    for q in ["evil", "carol", "corp", "frontdesk", "ops", "boss"] {
        let agent = search(&w.app, "/client/agent/calendar/search", q).await;
        assert_eq!(agent["total"], 0, "{q}: {agent}");
    }
    // The owner's own search box finds it by any of them.
    for q in ["evil", "carol", "corp", "frontdesk", "ops"] {
        let owner = search(&w.app, "/client/calendar/search", q).await;
        assert_eq!(owner["total"], 1, "{q}: {owner}");
    }
}

#[tokio::test]
async fn a_huge_query_is_cut_not_an_error_and_limit_zero_serves_nothing() {
    let w = search_world("search-huge").await;
    let huge: String = (0..2000).map(|i| format!("al x{i} ")).collect();
    let started = std::time::Instant::now();
    let body = search(&w.app, "/client/agent/calendar/search", &huge).await;
    assert!(body["results"].is_array(), "{body}");
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    // Repeats count once: "sam standup sam" is two terms that no event has
    // both of, so the standup (no Sam) is an all-but-one hit like the Sam
    // dinners. Counted twice, "sam" would be demanded and the standup lost.
    let body = search(&w.app, "/client/calendar/search", "sam standup sam").await;
    assert!(
        hit_titles(&body).contains(&"event:Team standup".to_string()),
        "{body}"
    );
    assert_eq!(body["total"], 4, "{body}");
    let none = get(&w.app, "/client/calendar/search?q=dinner&limit=0").await;
    assert_eq!(none["items"], json!([]));
    assert_eq!(none["total"], 3);
}

#[tokio::test]
async fn only_mail_events_triage_committed_to_or_the_user_is_going_to_are_searched() {
    let w = search_world("search-committed").await;
    let at = |days| rfc(minute(Utc::now() + Duration::days(days)));
    // An invitation and a proposal: possibilities, not on the calendar.
    seed_ghost_as(
        &w.store,
        w.acct,
        "g-inv",
        "Gala dinner",
        at(20),
        EventCommitment::Invited,
    );
    seed_ghost_as(
        &w.store,
        w.acct,
        "g-prop",
        "Dinner pitch",
        at(21),
        EventCommitment::Proposed,
    );
    seed_ghost_as(
        &w.store,
        w.acct,
        "g-unk",
        "Dinner maybe",
        at(22),
        EventCommitment::Unknown,
    );
    let body = search(&w.app, "/client/calendar/search", "dinner").await;
    assert_eq!(
        hit_titles(&body),
        [
            "event:Dinner w/ Sam",
            "ghost:Sam's birthday dinner",
            "event:Dinner w/ Sam"
        ],
        "{body}"
    );
    let agent = search(&w.app, "/client/agent/calendar/search", "gala").await;
    assert_eq!(agent["total"], 0, "{agent}");

    // The user saying they are going makes the invitation a commitment.
    let timeline = w.timeline().await;
    let gala = timeline["ghosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["title"] == "Gala dinner")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = w
        .app
        .clone()
        .oneshot(authed_json(
            "PATCH",
            &format!("/client/timeline/ghosts/{gala}"),
            json!({"state": "going"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = search(&w.app, "/client/calendar/search", "gala").await;
    assert_eq!(hit_titles(&body), ["ghost:Gala dinner"], "{body}");
}

// --- calendar reminders ------------------------------------------------------

/// A fake model that answers every call with `answer`, keeping each request
/// body whole (read to its `Content-Length`).
async fn spawn_remind_model(answer: Value) -> (String, Arc<Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let bodies: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = bodies.clone();
    let body = json!({
        "content": [{ "type": "text", "text": answer.to_string() }],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 900, "output_tokens": 40 }
    })
    .to_string();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let body = body.clone();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut raw = Vec::new();
                let mut buf = vec![0u8; 65536];
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&raw).into_owned();
                    if let Some(head_end) = text.find("\r\n\r\n") {
                        let length = text[..head_end]
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().ok())?
                            })
                            .unwrap_or(0);
                        if raw.len() >= head_end + 4 + length {
                            break;
                        }
                    }
                }
                seen.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&raw).into_owned());
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    (format!("http://{addr}"), bodies)
}

/// The next `calendar_reminder` frame's data on an SSE body.
async fn next_reminder(body: &mut axum::body::Body, buf: &mut String) -> Option<Value> {
    use http_body_util::BodyExt;
    let deadline = tokio::time::Instant::now() + StdDuration::from_secs(2);
    loop {
        while let Some(end) = buf.find("\n\n") {
            let raw = buf[..end].to_string();
            buf.drain(..end + 2);
            if raw.lines().any(|l| l == "event: calendar_reminder") {
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

/// End to end: a synced event and the booking mail matched to it are planned
/// once by the model (which reads the guest COUNT, never a guest), the
/// reminder fires once with the model's laundered line to the Mac's stream
/// and the phone's pusher, the phone fetches the same frame by id, and the
/// timeline's tickets button is the plan's pick on the event and its ghost.
#[tokio::test]
async fn an_event_is_planned_once_fires_once_and_serves_its_tickets() {
    use squelch_api::{RemindOutcome, run_fire_pass, run_plan_pass};
    use squelch_core::config::Stage2Provider;
    use squelch_core::gcal::remind::{RemindClient, USAGE_CATEGORY};
    let (url, bodies) = spawn_remind_model(json!({
        "remind": true,
        "lead_minutes": 55,
        "line": "Your booking is in\u{202E} the Alice email",
        "open": {"mail": 1, "attachment": 1},
        "ticket": {"mail": 1, "attachment": 1},
        "same_as": [4],
    }))
    .await;
    let door_state: Arc<Mutex<Option<squelch_api::ApiState>>> = Arc::default();
    let keep = door_state.clone();
    let w = world_with("remind", GRANTED, move |state, _| {
        let state = state.with_calendar_reminders(Some(RemindClient::new(
            reqwest::Client::new(),
            url,
            "sk-test",
            Stage2Provider::Anthropic,
            "claude-haiku-4-5",
            None,
        )));
        *keep.lock().unwrap() = Some(state.clone());
        state
    })
    .await;
    let state = door_state.lock().unwrap().clone().unwrap();

    let start = minute(Utc::now() + Duration::minutes(50));
    let mut dinner = timed("dinner", "Dinner at Dingles", start, 90);
    dinner["attendees"] = json!([
        {"email": "me@example.com", "self": true, "responseStatus": "accepted"},
        {"email": "friend@example.com", "displayName": "Dana Friend",
         "responseStatus": "accepted"},
    ]);
    w.google().full.insert(PRIMARY.into(), vec![vec![dinner]]);
    let message = seed_ghost(&w.store, w.acct, "g1", "Dinner at Dingles", rfc(start));
    let pdf = w
        .store
        .insert_attachment(
            w.acct,
            message,
            "booking.pdf",
            "application/pdf",
            3,
            Some(b"pdf"),
        )
        .unwrap();
    assert_eq!(w.pass().await, PassOutcome::Synced);

    // Planned once, nearest first, billed under its own line.
    assert_eq!(
        run_plan_pass(&state, Utc::now()).await.unwrap(),
        RemindOutcome::Planned {
            asked: 1,
            stored: 1
        }
    );
    {
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1);
        let sent = &bodies[0];
        assert!(sent.contains("Dinner at Dingles"));
        assert!(sent.contains("booking.pdf"));
        assert!(sent.contains("Other guests: 1"));
        assert!(!sent.contains("friend@example.com"), "no guest address");
        assert!(!sent.contains("Dana Friend"), "no guest name");
        assert!(!sent.contains("alice@example.com"), "no sender address");
        assert!(!sent.contains("details"), "no mail body");
    }
    let billed = w
        .store
        .list_usage_by_category(w.acct, 1)
        .unwrap()
        .into_iter()
        .find(|(c, _)| c == USAGE_CATEGORY)
        .map(|(_, days)| days.iter().map(|d| d.calls).sum::<u64>());
    assert_eq!(billed, Some(1));
    // A current plan is not asked about again.
    assert_eq!(
        run_plan_pass(&state, Utc::now()).await.unwrap(),
        RemindOutcome::Planned {
            asked: 0,
            stored: 0
        }
    );
    assert_eq!(bodies.lock().unwrap().len(), 1);

    // A Mac listening, and the phone's pusher.
    let resp = w
        .app
        .clone()
        .oneshot(authed("GET", "/client/events?opens=1"))
        .await
        .unwrap();
    let mut sse = resp.into_body();
    let mut buf = String::new();
    let mut pings = state.subscribe_calendar_pings();

    // 55 snaps to 60, and the event is 50 minutes out: due now.
    let now = Utc::now();
    let RemindOutcome::Fired(ids) = run_fire_pass(&state, now).await.unwrap() else {
        panic!("a fire pass");
    };
    assert_eq!(ids.len(), 1);
    let id = ids[0];
    let frame = next_reminder(&mut sse, &mut buf).await.expect("a frame");
    assert_eq!(frame["calendar_reminder"], id);
    assert_eq!(frame["at"], now.timestamp());
    assert_eq!(frame["lead_minutes"], 60);
    // The model's line, bidi override gone.
    assert_eq!(frame["line"], "Your booking is in the Alice email");
    assert_eq!(frame["item"]["kind"], "event");
    assert_eq!(frame["item"]["id"], "dinner");
    assert_eq!(frame["item"]["calendar_id"], "primary");
    assert_eq!(frame["item"]["title"], "Dinner at Dingles");
    assert_eq!(frame["open"]["message_id"], message);
    assert_eq!(frame["open"]["attachment_id"], pdf);
    assert_eq!(frame["ticket"]["filename"], "booking.pdf");
    assert_eq!(frame["ticket"]["kind"], "reservation");
    let ping = pings.try_recv().expect("a ping for the phone");
    assert_eq!(ping.fire_id, id);
    assert_eq!(ping.fired, now.timestamp());

    // The phone's fetch is the same frame.
    let fetched = get(&w.app, &format!("/client/calendar/reminders/{id}")).await;
    assert_eq!(fetched, frame);
    let resp = w
        .app
        .clone()
        .oneshot(authed(
            "GET",
            &format!("/client/calendar/reminders/{}", id + 1),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Once per start.
    assert_eq!(
        run_fire_pass(&state, Utc::now()).await.unwrap(),
        RemindOutcome::Fired(Vec::new())
    );
    assert!(pings.try_recv().is_err());

    // The tickets button: the plan's pick, on the event and on its ghost.
    let body = w.timeline().await;
    let tickets = json!({
        "message_id": message,
        "thread_id": "t-g1",
        "kind": "reservation",
        "attachment": {"id": pdf, "filename": "booking.pdf", "mime": "application/pdf"},
    });
    assert_eq!(body["events"][0]["tickets"], tickets);
    assert_eq!(body["ghosts"][0]["matched_event"]["id"], "dinner");
    assert_eq!(body["ghosts"][0]["tickets"], tickets);

    // Nothing of it on the agent door.
    let resp = w
        .app
        .clone()
        .oneshot(authed(
            "GET",
            &format!("/client/agent/calendar/reminders/{id}"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
