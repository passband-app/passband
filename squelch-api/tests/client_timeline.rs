//! Contract tests for the calendar timeline: `GET /client/timeline` and
//! `PATCH /client/timeline/ghosts/{id}`, human door only. Ghosts are seeded the
//! way production makes them, through a committed triage decision carrying
//! `event` records, so the projection under test is the real one.
mod common;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{DateTime, Duration, Utc};
use common::*;
use serde_json::{Value, json};
use squelch_core::store::{
    SqliteStore, Store,
    agent_triage::{AgentCommitOutcome, AgentTriageStore},
};
use squelch_core::triage::decision::*;
use squelch_core::types::NewMessage;
use tower::ServiceExt;

/// One `event` record, defaulting to a committed ticket with a stated end.
struct Ev<'a> {
    title: &'a str,
    start: String,
    end: Option<String>,
    estimate: Option<(i64, &'a str)>,
    kind: EventRecordKind,
    commitment: EventCommitment,
    change: EventChange,
    refs: &'a [&'a str],
    likelihood: Option<(LikelihoodBand, &'a str)>,
    location: Option<&'a str>,
}
impl Default for Ev<'_> {
    fn default() -> Self {
        Ev {
            title: "Show",
            start: rfc(Utc::now() + Duration::days(10)),
            end: None,
            estimate: None,
            kind: EventRecordKind::Ticket,
            commitment: EventCommitment::Committed,
            change: EventChange::Created,
            refs: &["ABC12345"],
            likelihood: None,
            location: Some("Brooklyn Steel"),
        }
    }
}
impl Ev<'_> {
    fn record(&self, message_id: i64) -> RecordProposal {
        let time = |value: &str| SupportedTime {
            value: value.into(),
            timezone: Some("America/New_York".into()),
            source_message_id: message_id,
            interpreted_relative: false,
        };
        RecordProposal::Event {
            title: self.title.into(),
            start: Some(time(&self.start)),
            location: self.location.map(str::to_string),
            end: self.end.as_deref().map(time),
            duration_estimate: self.estimate.map(|(minutes, basis)| DurationEstimate {
                minutes,
                basis: basis.into(),
            }),
            event_kind: self.kind,
            commitment: self.commitment,
            change: self.change,
            identity_refs: self.refs.iter().map(|r| r.to_string()).collect(),
            likelihood: self.likelihood.map(|(band, reason)| EventLikelihood {
                band,
                reason: reason.into(),
            }),
            evidence: vec![EvidenceRef {
                message_id,
                location: "body".into(),
            }],
        }
    }
}

/// RFC3339 in New York summer time, the shape the model emits.
fn rfc(at: DateTime<Utc>) -> String {
    at.with_timezone(&chrono::FixedOffset::west_opt(4 * 3600).unwrap())
        .format("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}
/// UTC RFC3339 with `Z`: safe in a query string as is.
fn q(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Ingest a message `minutes_ago` old and commit a triage decision carrying
/// `events`, through the real claim and commit path. Returns the message id.
fn decide(
    store: &SqliteStore,
    account: i64,
    gmail: &str,
    thread: &str,
    minutes_ago: i64,
    events: &[Ev],
) -> i64 {
    // Authenticated: only an authenticated sender's records match other mail
    // by ref or commit a ghost (identity v2).
    decide_as(
        store,
        account,
        gmail,
        thread,
        minutes_ago,
        Some(true),
        events,
    )
}

/// [`decide`] for a message whose authentication result is `auth_pass`.
fn decide_as(
    store: &SqliteStore,
    account: i64,
    gmail: &str,
    thread: &str,
    minutes_ago: i64,
    auth_pass: Option<bool>,
    events: &[Ev],
) -> i64 {
    let id = store
        .upsert_message(&NewMessage {
            received_at: Utc::now() - Duration::minutes(minutes_ago),
            auth_pass,
            ..msg(account, gmail, thread, "Your plans", "details")
        })
        .unwrap();
    store
        .enqueue_agent_triage(account, id, "test", false)
        .unwrap();
    let job = store
        .claim_agent_job(account, "triage", Utc::now(), 60)
        .unwrap()
        .unwrap();
    assert_eq!(job.message_id, id);
    let context = store.load_agent_context(&job).unwrap();
    let evidence = vec![EvidenceRef {
        message_id: id,
        location: "body".into(),
    }];
    let decision = MessageDecision {
        kinds: vec![EmailKind::EventReservation],
        summary: "Plans".into(),
        reason: "An event".into(),
        records: events.iter().map(|e| e.record(id)).collect(),
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

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(authed("GET", uri)).await.unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn patch(app: &axum::Router, id: i64, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(authed_json(
            "PATCH",
            &format!("/client/timeline/ghosts/{id}"),
            body,
        ))
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

/// Every ghost in a window wide enough for any fixture here.
async fn wide(app: &axum::Router) -> Vec<Value> {
    let uri = format!(
        "/client/timeline?from={}&to={}",
        q(Utc::now() - Duration::days(100)),
        q(Utc::now() + Duration::days(290)),
    );
    let (status, body) = get(app, &uri).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["ghosts"].as_array().unwrap().clone()
}

fn by_title<'a>(ghosts: &'a [Value], title: &str) -> &'a Value {
    ghosts
        .iter()
        .find(|g| g["title"] == title)
        .unwrap_or_else(|| panic!("no ghost {title:?} in {ghosts:?}"))
}

#[tokio::test]
async fn timeline_serves_every_field_with_the_right_end_tier() {
    let start = Utc::now() + Duration::days(10);
    let day = (Utc::now() + Duration::days(5)).date_naive();
    let h = harness(|_, _| {});
    let stated = decide(
        &h.store,
        h.acct,
        "g1",
        "t1",
        5,
        &[Ev {
            title: "Stated",
            start: rfc(start),
            end: Some(rfc(start + Duration::hours(3))),
            refs: &["STATED001"],
            // Committed: a likelihood in the record must not be served.
            likelihood: Some((LikelihoodBand::Unlikely, "ignored")),
            ..Default::default()
        }],
    );
    decide(
        &h.store,
        h.acct,
        "g2",
        "t2",
        4,
        &[Ev {
            title: "Modelled",
            start: rfc(start + Duration::days(1)),
            estimate: Some((150, "doors 7, show 8")),
            kind: EventRecordKind::Invite,
            commitment: EventCommitment::Invited,
            refs: &["MODEL0001"],
            likelihood: Some((LikelihoodBand::Likely, "a friend asked")),
            ..Default::default()
        }],
    );
    decide(
        &h.store,
        h.acct,
        "g3",
        "t3",
        3,
        &[Ev {
            title: "Defaulted",
            start: rfc(start + Duration::days(2)),
            kind: EventRecordKind::Reservation,
            refs: &["DEFAULT01"],
            location: None,
            ..Default::default()
        }],
    );
    decide(
        &h.store,
        h.acct,
        "g4",
        "t4",
        2,
        &[Ev {
            title: "All day",
            start: day.format("%Y-%m-%d").to_string(),
            kind: EventRecordKind::Other,
            commitment: EventCommitment::Proposed,
            refs: &["ALLDAY001"],
            ..Default::default()
        }],
    );

    let resp = h
        .app
        .clone()
        .oneshot(authed("GET", "/client/timeline"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["cache-control"], "no-store");
    let body = body_json(resp).await;
    let ghosts = body["ghosts"].as_array().unwrap();
    // Sorted by start: the all-day one (day 5) first.
    let titles: Vec<_> = ghosts.iter().map(|g| g["title"].clone()).collect();
    assert_eq!(
        titles,
        vec![
            json!("All day"),
            json!("Stated"),
            json!("Modelled"),
            json!("Defaulted")
        ]
    );

    let s = by_title(ghosts, "Stated");
    eprintln!("served ghost: {}", serde_json::to_string_pretty(s).unwrap());
    let mut keys: Vec<_> = s.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
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
            // Null here: no calendar is granted, so no real event to match.
            "matched_event",
            "sources",
            "start",
            "start_timezone",
            "state",
            // The tickets button: a ticket's own mail, with no attachment.
            "tickets",
            "title",
            "verified"
        ]
    );
    assert_eq!(s["matched_event"], Value::Null);
    assert_eq!(s["tickets"]["message_id"], s["sources"][0]["message_id"]);
    assert_eq!(s["tickets"]["kind"], "ticket");
    assert_eq!(s["tickets"]["attachment"], Value::Null);
    // The whole body: ghosts beside the real events and the calendar status,
    // which with no grant is empty and says so.
    let mut top: Vec<_> = body.as_object().unwrap().keys().cloned().collect();
    top.sort();
    assert_eq!(top, ["calendar", "calendars", "events", "ghosts"]);
    assert_eq!(body["events"], json!([]));
    assert_eq!(body["calendars"], json!([]));
    assert_eq!(
        body["calendar"],
        json!({"granted": false, "synced_at": null, "error": null, "truncated": false})
    );
    assert!(s["id"].as_i64().unwrap() > 0);
    assert_eq!(s["kind"], "ticket");
    assert_eq!(s["start"], rfc(start));
    assert_eq!(s["start_timezone"], "America/New_York");
    assert_eq!(s["all_day"], false);
    assert_eq!(s["end"], rfc(start + Duration::hours(3)));
    assert_eq!(s["end_estimated"], false);
    assert_eq!(s["end_source"], "stated");
    assert_eq!(s["estimate"], Value::Null);
    assert_eq!(s["location"], "Brooklyn Steel");
    assert_eq!(s["commitment"], "committed");
    assert_eq!(s["state"], "ghost");
    assert_eq!(s["likelihood"], Value::Null, "committed ghosts are certain");
    assert_eq!(
        s["sources"],
        json!([{ "message_id": stated, "thread_id": "t1", "role": "created" }])
    );

    let m = by_title(ghosts, "Modelled");
    assert_eq!(m["kind"], "invite");
    assert_eq!(
        m["end"],
        rfc(start + Duration::days(1) + Duration::minutes(150))
    );
    assert_eq!(m["end_estimated"], true);
    assert_eq!(m["end_source"], "model");
    assert_eq!(
        m["estimate"],
        json!({ "minutes": 150, "basis": "doors 7, show 8" })
    );
    assert_eq!(m["commitment"], "invited");
    assert_eq!(
        m["likelihood"],
        json!({ "band": "likely", "reason": "a friend asked" })
    );

    let d = by_title(ghosts, "Defaulted");
    assert_eq!(d["kind"], "reservation");
    assert_eq!(
        d["end"],
        rfc(start + Duration::days(2) + Duration::minutes(120))
    );
    assert_eq!(d["end_estimated"], true);
    assert_eq!(d["end_source"], "default");
    assert_eq!(d["estimate"], Value::Null);
    assert_eq!(d["location"], Value::Null);

    let a = by_title(ghosts, "All day");
    assert_eq!(a["all_day"], true);
    assert_eq!(a["start"], day.format("%Y-%m-%d").to_string());
    assert_eq!(
        a["end"],
        Value::Null,
        "a date-only start is never estimated"
    );
    assert_eq!(a["end_estimated"], false);
    assert_eq!(a["end_source"], Value::Null);
    assert_eq!(a["estimate"], Value::Null);
    assert_eq!(a["commitment"], "proposed");
    assert_eq!(a["likelihood"], Value::Null, "none was given");
    assert_eq!(a["state"], "ghost");
}

#[tokio::test]
async fn timeline_range_defaults_explicit_bounds_and_rejects_bad_input() {
    let now = Utc::now();
    let h = harness(|_, _| {});
    for (n, (title, days)) in [("Past", -30), ("Soon", 10), ("Later", 200), ("Far", 360)]
        .into_iter()
        .enumerate()
    {
        decide(
            &h.store,
            h.acct,
            &format!("g{n}"),
            &format!("t{n}"),
            10 - n as i64,
            &[Ev {
                title,
                start: rfc(now + Duration::days(days)),
                refs: &[&format!("RANGE000{n}")],
                ..Default::default()
            }],
        );
    }
    let titles = |body: &Value| -> Vec<String> {
        body["ghosts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["title"].as_str().unwrap().to_string())
            .collect()
    };

    // Default: a day back to ninety days ahead.
    let (status, body) = get(&h.app, "/client/timeline").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&body), ["Soon"]);

    // Explicit bounds, `[from, to)`.
    let uri = format!(
        "/client/timeline?from={}&to={}",
        q(now - Duration::days(60)),
        q(now)
    );
    let (status, body) = get(&h.app, &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&body), ["Past"]);
    assert_eq!(body["ghosts"][0]["state"], "expired");

    // Only `from`: `to` follows it by the default window's length.
    let uri = format!("/client/timeline?from={}", q(now + Duration::days(150)));
    let (_, body) = get(&h.app, &uri).await;
    assert_eq!(titles(&body), ["Later"]);

    // An offset other than Z, percent-encoded as a client must.
    let from = (now - Duration::days(1))
        .with_timezone(&chrono::FixedOffset::east_opt(2 * 3600).unwrap())
        .to_rfc3339()
        .replace('+', "%2B");
    let (status, body) = get(&h.app, &format!("/client/timeline?from={from}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(titles(&body), ["Soon"]);

    // Over-long: clamped to 400 days from `from` (now-60d .. now+340d), which
    // keeps "Later" and drops "Far" rather than refusing the request.
    let uri = format!(
        "/client/timeline?from={}&to={}",
        q(now - Duration::days(60)),
        q(now + Duration::days(1000))
    );
    let (status, body) = get(&h.app, &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&body), ["Past", "Soon", "Later"]);

    // An inverted range is empty, not an error.
    let uri = format!(
        "/client/timeline?from={}&to={}",
        q(now + Duration::days(20)),
        q(now)
    );
    let (status, body) = get(&h.app, &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ghosts"], json!([]));
    assert_eq!(body["events"], json!([]));

    for bad in [
        "/client/timeline?from=yesterday",
        "/client/timeline?to=2026-13-01T00:00:00Z",
        "/client/timeline?from=2026-10-01",
        "/client/timeline?from=2026-10-01T25:00:00Z",
    ] {
        let (status, body) = get(&h.app, bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        assert!(body["error"].is_string(), "{bad}: {body}");
    }
}

#[tokio::test]
async fn timeline_state_comes_from_the_user_then_the_mail_then_the_clock() {
    let now = Utc::now();
    let h = harness(|_, _| {});
    decide(
        &h.store,
        h.acct,
        "g1",
        "t1",
        30,
        &[Ev {
            title: "Gone by",
            start: rfc(now - Duration::days(2)),
            refs: &["PAST00001"],
            ..Default::default()
        }],
    );
    // A booking, then its cancellation by the same reference.
    decide(
        &h.store,
        h.acct,
        "g2",
        "t2",
        20,
        &[Ev {
            title: "Dinner",
            start: rfc(now + Duration::days(3)),
            kind: EventRecordKind::Reservation,
            refs: &["RESV00001"],
            ..Default::default()
        }],
    );
    let cancel = decide(
        &h.store,
        h.acct,
        "g3",
        "t2",
        10,
        &[Ev {
            title: "Dinner",
            start: rfc(now + Duration::days(3)),
            kind: EventRecordKind::Reservation,
            change: EventChange::Cancelled,
            refs: &["RESV00001"],
            ..Default::default()
        }],
    );
    decide(
        &h.store,
        h.acct,
        "g4",
        "t4",
        5,
        &[Ev {
            title: "Party",
            start: rfc(now + Duration::days(4)),
            kind: EventRecordKind::Invite,
            commitment: EventCommitment::Invited,
            refs: &["PARTY0001"],
            ..Default::default()
        }],
    );

    let ghosts = wide(&h.app).await;
    assert_eq!(by_title(&ghosts, "Gone by")["state"], "expired");
    let dinner = by_title(&ghosts, "Dinner");
    assert_eq!(dinner["state"], "cancelled");
    assert_eq!(dinner["sources"].as_array().unwrap().len(), 2);
    assert!(
        dinner["sources"]
            .as_array()
            .unwrap()
            .contains(&json!({ "message_id": cancel, "thread_id": "t2", "role": "cancelled" }))
    );
    let party = by_title(&ghosts, "Party");
    assert_eq!(party["state"], "ghost");

    // The user's word outranks the mail's and the clock's.
    let (status, _) = patch(
        &h.app,
        dinner["id"].as_i64().unwrap(),
        json!({ "state": "going" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = patch(
        &h.app,
        by_title(&ghosts, "Gone by")["id"].as_i64().unwrap(),
        json!({ "state": "dismissed" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ghosts = wide(&h.app).await;
    assert_eq!(by_title(&ghosts, "Dinner")["state"], "going");
    assert_eq!(by_title(&ghosts, "Gone by")["state"], "dismissed");
    assert_eq!(by_title(&ghosts, "Party")["state"], "ghost");
}

/// Round 2 (1.6): `state` says `going` over a real cancellation, so the mail's
/// own word is served beside it as `mail_state`; and `verified` says whether
/// the mail behind a ghost passed authentication.
#[tokio::test]
async fn timeline_serves_mail_state_and_verified() {
    let now = Utc::now();
    let h = harness(|_, _| {});
    let dinner = |change| Ev {
        title: "Dinner",
        start: rfc(now + Duration::days(3)),
        kind: EventRecordKind::Reservation,
        change,
        refs: &["RESV00001"],
        ..Default::default()
    };
    decide(
        &h.store,
        h.acct,
        "g1",
        "t1",
        20,
        &[dinner(EventChange::Created)],
    );
    decide_as(
        &h.store,
        h.acct,
        "g2",
        "t2",
        15,
        None,
        &[Ev {
            title: "Rumour",
            start: rfc(now + Duration::days(4)),
            refs: &["RUMOR0001"],
            ..Default::default()
        }],
    );
    let ghosts = wide(&h.app).await;
    let d = by_title(&ghosts, "Dinner");
    assert_eq!(
        (&d["mail_state"], &d["verified"]),
        (&json!("live"), &json!(true))
    );
    let r = by_title(&ghosts, "Rumour");
    assert_eq!(
        (&r["mail_state"], &r["verified"]),
        (&json!("live"), &json!(false))
    );
    assert_eq!(
        r["commitment"], "unknown",
        "unverified mail commits nothing"
    );

    let (status, _) = patch(
        &h.app,
        d["id"].as_i64().unwrap(),
        json!({ "state": "going" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    decide(
        &h.store,
        h.acct,
        "g3",
        "t1",
        10,
        &[dinner(EventChange::Cancelled)],
    );
    let ghosts = wide(&h.app).await;
    let d = by_title(&ghosts, "Dinner");
    assert_eq!(d["state"], "going", "the user's word is the state");
    assert_eq!(
        d["mail_state"], "cancelled",
        "and the cancellation is still served"
    );
}

#[tokio::test]
async fn patch_state_sets_and_clears_and_absent_leaves_it() {
    let h = harness(|_, _| {});
    decide(&h.store, h.acct, "g1", "t1", 5, &[Ev::default()]);
    let id = wide(&h.app).await[0]["id"].as_i64().unwrap();
    let state = |ghosts: Vec<Value>| ghosts[0]["state"].clone();

    let (status, body) = patch(&h.app, id, json!({ "state": "dismissed" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "updated": true, "id": id }));
    assert_eq!(state(wide(&h.app).await), "dismissed");

    let (status, _) = patch(&h.app, id, json!({ "state": "going" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state(wide(&h.app).await), "going");

    // Absent: an end-only PATCH leaves the state alone.
    let start = wide(&h.app).await[0]["start"].as_str().unwrap().to_string();
    let end = DateTime::parse_from_rfc3339(&start).unwrap() + Duration::hours(4);
    let (status, _) = patch(&h.app, id, json!({ "end": end.to_rfc3339() })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state(wide(&h.app).await), "going");

    // Explicit null clears it.
    let (status, _) = patch(&h.app, id, json!({ "state": null })).await;
    assert_eq!(status, StatusCode::OK);
    let ghosts = wide(&h.app).await;
    assert_eq!(state(ghosts.clone()), "ghost");
    assert_eq!(
        ghosts[0]["end_source"], "user",
        "clearing the state left the end"
    );
}

#[tokio::test]
async fn patch_end_corrects_clears_and_refuses_impossible_ends() {
    let start = Utc::now() + Duration::days(10);
    let day = (Utc::now() + Duration::days(5)).date_naive();
    let h = harness(|_, _| {});
    decide(
        &h.store,
        h.acct,
        "g1",
        "t1",
        5,
        &[Ev {
            title: "Modelled",
            start: rfc(start),
            estimate: Some((150, "doors 7, show 8")),
            refs: &["MODEL0001"],
            // No venue, so the correction below records no duration prior
            // and clearing it falls back to the model tier, not history.
            location: None,
            ..Default::default()
        }],
    );
    decide(
        &h.store,
        h.acct,
        "g2",
        "t2",
        4,
        &[Ev {
            title: "All day",
            start: day.format("%Y-%m-%d").to_string(),
            refs: &["ALLDAY001"],
            ..Default::default()
        }],
    );
    let ghosts = wide(&h.app).await;
    let timed = by_title(&ghosts, "Modelled")["id"].as_i64().unwrap();
    let all_day = by_title(&ghosts, "All day")["id"].as_i64().unwrap();

    // A correction is a fact; the model's estimate is still served beside it.
    let corrected = rfc(start + Duration::hours(5));
    let (status, _) = patch(&h.app, timed, json!({ "end": corrected })).await;
    assert_eq!(status, StatusCode::OK);
    let ghosts = wide(&h.app).await;
    let g = by_title(&ghosts, "Modelled");
    assert_eq!(g["end"], corrected);
    assert_eq!(g["end_source"], "user");
    assert_eq!(g["end_estimated"], false);
    assert_eq!(
        g["estimate"],
        json!({ "minutes": 150, "basis": "doors 7, show 8" })
    );
    assert_eq!(g["state"], "ghost", "absent state left alone");

    // Null clears back to the estimate.
    let (status, _) = patch(&h.app, timed, json!({ "end": null })).await;
    assert_eq!(status, StatusCode::OK);
    let ghosts = wide(&h.app).await;
    let g = by_title(&ghosts, "Modelled");
    assert_eq!(g["end_source"], "model");
    assert_eq!(g["end_estimated"], true);
    assert_eq!(g["end"], rfc(start + Duration::minutes(150)));

    // An end not after the start, and a timed end on a date-only ghost: 400,
    // and nothing written.
    for (id, end) in [
        (timed, rfc(start - Duration::hours(1))),
        (timed, rfc(start)),
        (all_day, rfc(start)),
    ] {
        let (status, body) = patch(&h.app, id, json!({ "end": end, "state": "going" })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{end}: {body}");
        assert!(body["error"].is_string());
    }
    let ghosts = wide(&h.app).await;
    assert_eq!(by_title(&ghosts, "Modelled")["end_source"], "model");
    assert_eq!(by_title(&ghosts, "Modelled")["state"], "ghost");
    assert_eq!(by_title(&ghosts, "All day")["end"], Value::Null);
    assert_eq!(by_title(&ghosts, "All day")["state"], "ghost");

    // A date-only ghost takes a date-only end.
    let last = (day + Duration::days(2)).format("%Y-%m-%d").to_string();
    let (status, _) = patch(&h.app, all_day, json!({ "end": last })).await;
    assert_eq!(status, StatusCode::OK);
    let ghosts = wide(&h.app).await;
    let g = by_title(&ghosts, "All day");
    assert_eq!(g["end"], last);
    assert_eq!(g["end_source"], "user");
    assert_eq!(g["end_estimated"], false);
}

/// Both fields land in ONE store transaction: a refused end takes the state
/// change in the same body down with it, whichever the store writes first,
/// and an accepted pair lands together.
#[tokio::test]
async fn patch_with_an_impossible_end_leaves_the_state_as_it_was() {
    let start = Utc::now() + Duration::days(10);
    let h = harness(|_, _| {});
    decide(
        &h.store,
        h.acct,
        "g1",
        "t1",
        5,
        &[Ev {
            start: rfc(start),
            ..Default::default()
        }],
    );
    let id = wide(&h.app).await[0]["id"].as_i64().unwrap();
    let (status, _) = patch(&h.app, id, json!({ "state": "dismissed" })).await;
    assert_eq!(status, StatusCode::OK);

    for state in [json!("going"), Value::Null] {
        let (status, body) = patch(
            &h.app,
            id,
            json!({ "state": state, "end": rfc(start - Duration::hours(1)) }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{state}: {body}");
        let ghost = wide(&h.app).await[0].clone();
        assert_eq!(ghost["state"], "dismissed", "{state}: untouched");
        assert_ne!(ghost["end_source"], "user", "{state}: no end written");
    }

    let end = rfc(start + Duration::hours(4));
    let (status, _) = patch(&h.app, id, json!({ "state": "going", "end": end })).await;
    assert_eq!(status, StatusCode::OK);
    let ghost = wide(&h.app).await[0].clone();
    assert_eq!(ghost["state"], "going");
    assert_eq!(ghost["end"], end);
    assert_eq!(ghost["end_source"], "user");
}

#[tokio::test]
async fn patch_rejects_bad_bodies_and_unknown_ids() {
    let h = harness(|_, _| {});
    decide(&h.store, h.acct, "g1", "t1", 5, &[Ev::default()]);
    // Another account's ghost, in the same database.
    let other = h.store.ensure_account("other@example.com").unwrap();
    decide(
        &h.store,
        other,
        "o1",
        "ot1",
        5,
        &[Ev {
            title: "Theirs",
            ..Default::default()
        }],
    );
    let id = wide(&h.app).await[0]["id"].as_i64().unwrap();
    let theirs: i64 = {
        let (from, to) = (
            (Utc::now() - Duration::days(1)).fixed_offset(),
            (Utc::now() + Duration::days(90)).fixed_offset(),
        );
        h.store.list_ghost_events(other, from, to).unwrap()[0].id
    };
    assert_ne!(id, theirs);

    for body in [
        json!({}),
        json!({ "unrelated": 1 }),
        json!({ "state": "maybe" }),
        json!({ "state": "Dismissed" }),
        json!({ "state": 5 }),
        json!({ "end": "tomorrow evening" }),
        json!({ "end": 1_700_000_000 }),
        json!({ "state": "going", "end": "nope" }),
    ] {
        let (status, resp) = patch(&h.app, id, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {resp}");
        assert!(resp["error"].is_string(), "{body}: {resp}");
    }
    assert_eq!(wide(&h.app).await[0]["state"], "ghost", "nothing written");

    // A literally empty body with the JSON content type is malformed JSON.
    let resp = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/client/timeline/ghosts/{id}"))
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    for missing in [id + 1000, theirs] {
        for body in [
            json!({ "state": "dismissed" }),
            json!({ "end": rfc(Utc::now() + Duration::days(30)) }),
        ] {
            let (status, _) = patch(&h.app, missing, body.clone()).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{missing} {body}");
        }
    }
    // Their ghost is untouched and ours never listed theirs.
    let (from, to) = (
        (Utc::now() - Duration::days(1)).fixed_offset(),
        (Utc::now() + Duration::days(90)).fixed_offset(),
    );
    let listed = h.store.list_ghost_events(other, from, to).unwrap();
    assert_eq!(listed[0].user_state, None);
    assert!(wide(&h.app).await.iter().all(|g| g["title"] != "Theirs"));
}

#[tokio::test]
async fn timeline_routes_require_the_client_credential() {
    let h = harness(|_, _| {});
    decide(&h.store, h.acct, "g1", "t1", 5, &[Ev::default()]);
    let id = wide(&h.app).await[0]["id"].as_i64().unwrap();
    for bearer in [None, Some("Bearer nope")] {
        for (method, uri, body) in [
            ("GET", "/client/stats".to_string(), None),
            ("GET", "/client/timeline".to_string(), None),
            (
                "PATCH",
                format!("/client/timeline/ghosts/{id}"),
                Some(json!({ "state": "dismissed" })),
            ),
        ] {
            let mut req = Request::builder().method(method).uri(&uri);
            if let Some(b) = bearer {
                req = req.header(header::AUTHORIZATION, b);
            }
            let req = match &body {
                Some(b) => req
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(b).unwrap())),
                None => req.body(Body::empty()),
            }
            .unwrap();
            let resp = h.app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }
    assert_eq!(
        wide(&h.app).await[0]["state"],
        "ghost",
        "the 401 wrote nothing"
    );
}

#[tokio::test]
async fn stats_advertise_timeline() {
    let h = harness(|_, _| {});
    let (status, body) = get(&h.app, "/client/stats").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["timeline"], true);
}

/// Ghosts are human-door only in this wave: nothing on the agent door's paths
/// serves them, and the agent's record listing gains no ghost shape.
#[tokio::test]
async fn the_agent_door_serves_no_ghosts() {
    let h = harness(|_, _| {});
    decide(&h.store, h.acct, "g1", "t1", 5, &[Ev::default()]);
    let id = wide(&h.app).await[0]["id"].as_i64().unwrap();
    for (method, uri) in [
        ("GET", "/client/agent/timeline".to_string()),
        ("PATCH", format!("/client/agent/timeline/ghosts/{id}")),
        ("GET", "/mcp/timeline".to_string()),
        ("GET", "/mcp".to_string()),
    ] {
        let resp = h
            .app
            .clone()
            .oneshot(authed_json(method, &uri, json!({ "state": "dismissed" })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{method} {uri}");
    }
    assert_eq!(wide(&h.app).await[0]["state"], "ghost");
    // The agent's calendar records are per-message triage records, as before
    // this wave; none of the projection (ghost id, user state, resolved end)
    // reaches them.
    let (status, body) = get(&h.app, "/client/agent/records?kind=calendar").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let text = body.to_string();
    // The event itself is still there, with the fields it always had...
    assert!(text.contains("\"title\""), "no event record served: {text}");
    // ...and neither the projection nor the calendar fields on the record
    // (booking refs, the duration guess, commitment, likelihood) ride along.
    for word in [
        "ghost",
        "end_source",
        "end_estimated",
        "user_state",
        "\"state\"",
        "identity_refs",
        "duration_estimate",
        "commitment",
        "event_kind",
        "likelihood",
        "\"change\"",
        "\"end\"",
    ] {
        assert!(!text.contains(word), "agent records carry {word}: {text}");
    }
}
