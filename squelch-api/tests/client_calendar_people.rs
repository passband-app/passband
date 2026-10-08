//! Contract tests for `GET /client/calendar/people?name=`: the calendar event
//! pane's guest suggestion. Human door only, behind the `calendar` feature.
//! Who qualifies and in what order is the store's (`people_named`, tested in
//! squelch-core); these pin the wire, the gate and the doors.
mod common;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{Duration, Utc};
use common::*;
use serde_json::{Value, json};
use squelch_core::store::{ContactEntry, SqliteStore, Store};
use squelch_core::types::NewMessage;
use tower::ServiceExt;

const PEOPLE: &str = "/client/calendar/people";

/// A person the account wrote to `days_ago`.
fn wrote_to(addr: &str, name: Option<&str>, days_ago: i64) -> ContactEntry {
    ContactEntry {
        addr: addr.into(),
        display_name: name.map(Into::into),
        sent_count: 1,
        last_sent_at: Some(Utc::now() - Duration::days(days_ago)),
    }
}

fn seed(store: &SqliteStore, account: i64, people: &[ContactEntry]) {
    store.merge_harvested_contacts(account, people).unwrap();
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(authed("GET", uri)).await.unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

/// The four Stevens most tests ask about, and somebody who is not one.
fn stevens(store: &SqliteStore, account: i64) {
    seed(
        store,
        account,
        &[
            wrote_to("s1@x.com", Some("Steven Ng"), 1),
            wrote_to("s2@x.com", Some("Steven Park"), 2),
            wrote_to("steven.three@x.com", None, 3),
            wrote_to("s4@x.com", Some("Steven Four"), 4),
            wrote_to("bob@x.com", Some("Bob Jones"), 1),
        ],
    );
}

#[tokio::test]
async fn people_answers_three_at_most_best_first_and_is_never_cached() {
    let h = calendar_harness(stevens);
    let resp = h
        .app
        .clone()
        .oneshot(authed("GET", &format!("{PEOPLE}?name=steven")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    assert_eq!(
        body_json(resp).await,
        json!({ "people": [
            { "addr": "s1@x.com", "name": "Steven Ng" },
            { "addr": "s2@x.com", "name": "Steven Park" },
            { "addr": "steven.three@x.com", "name": null },
        ] })
    );
    // The name is trimmed, and matched on the given name alone.
    let (status, body) = get(&h.app, &format!("{PEOPLE}?name=%20%20Steven%20n%20")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "people": [{ "addr": "s1@x.com", "name": "Steven Ng" }] })
    );
    let (_, body) = get(&h.app, &format!("{PEOPLE}?name=ng")).await;
    assert_eq!(body, json!({ "people": [] }));
}

/// Mail TO the user makes nobody a suggested guest, whatever name it carries.
#[tokio::test]
async fn somebody_who_only_wrote_to_the_user_is_not_suggested() {
    let h = calendar_harness(|store, account| {
        store
            .upsert_message(&NewMessage {
                from_addr: "steven@stranger.test".into(),
                from_name: Some("Steven Stranger".into()),
                auth_pass: Some(true),
                ..msg(account, "g1", "t1", "Dinner?", "hello")
            })
            .unwrap();
    });
    let (status, body) = get(&h.app, &format!("{PEOPLE}?name=steven")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "people": [] }));
    // The same address once the user has written to it.
    seed(
        &h.store,
        h.acct,
        &[wrote_to("steven@stranger.test", None, 1)],
    );
    let (_, body) = get(&h.app, &format!("{PEOPLE}?name=steven")).await;
    assert_eq!(
        body,
        json!({ "people": [{ "addr": "steven@stranger.test", "name": null }] })
    );
}

/// Empty, over-long and address-shaped names answer the empty list: 200, not
/// an error, and nothing of the input. Each bad name here is one a person in
/// the store WOULD match, so the refusal is the handler's.
#[tokio::test]
async fn a_name_that_is_not_a_name_answers_nobody() {
    let long = "x".repeat(65);
    let longest_allowed = "y".repeat(64);
    let (long_name, allowed_name) = (long.clone(), longest_allowed.clone());
    let h = calendar_harness(move |store, account| {
        stevens(store, account);
        seed(
            store,
            account,
            &[
                wrote_to("long@x.com", Some(&long_name), 1),
                wrote_to("allowed@x.com", Some(&allowed_name), 1),
                wrote_to("addr@x.com", Some("steven@x.com"), 1),
            ],
        );
    });
    for uri in [
        PEOPLE.to_string(),
        format!("{PEOPLE}?name="),
        format!("{PEOPLE}?name=%20%20"),
        format!("{PEOPLE}?name={long}"),
        format!("{PEOPLE}?name=steven%40x.com"),
        format!("{PEOPLE}?name=%40"),
    ] {
        let resp = h.app.clone().oneshot(authed("GET", &uri)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{uri}");
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store",
            "{uri}"
        );
        assert_eq!(body_json(resp).await, json!({ "people": [] }), "{uri}");
    }
    // The longest name allowed is still looked up, and the spaces around it
    // are not part of its length.
    for uri in [
        format!("{PEOPLE}?name={longest_allowed}"),
        format!("{PEOPLE}?name=%20%20{longest_allowed}%20"),
    ] {
        let (_, body) = get(&h.app, &uri).await;
        assert_eq!(
            body,
            json!({ "people": [{ "addr": "allowed@x.com", "name": longest_allowed }] }),
            "{uri}"
        );
    }
}

#[tokio::test]
async fn people_require_the_client_credential() {
    let h = calendar_harness(stevens);
    for bearer in [None, Some("Bearer nope")] {
        let mut req = Request::builder()
            .method("GET")
            .uri(format!("{PEOPLE}?name=steven"));
        if let Some(b) = bearer {
            req = req.header(header::AUTHORIZATION, b);
        }
        let resp = h
            .app
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{bearer:?}");
        assert!(
            !body_json(resp).await.to_string().contains("x.com"),
            "{bearer:?}"
        );
    }
}

/// Off, the route is not there, for the same people the tests above find.
#[tokio::test]
async fn people_are_not_there_until_the_calendar_flag_is_on() {
    let h = harness(stevens);
    let (status, body) = get(&h.app, &format!("{PEOPLE}?name=steven")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({ "error": "not found" }));
    // And with no credential it is the 401 every route gives: the flag is not
    // something an unauthenticated caller can read.
    let resp = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("{PEOPLE}?name=steven"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn another_accounts_people_are_never_served() {
    let h = calendar_harness(stevens);
    let other = h.store.ensure_account("other@example.com").unwrap();
    seed(
        &h.store,
        other,
        &[
            wrote_to("theirs@x.com", Some("Steven Theirs"), 0),
            wrote_to("zed@x.com", Some("Zed Theirs"), 0),
        ],
    );
    assert_eq!(
        h.store.people_named(other, "zed", 3).unwrap().len(),
        1,
        "the other account has somebody to leak"
    );
    let (_, body) = get(&h.app, &format!("{PEOPLE}?name=steven")).await;
    assert!(!body.to_string().contains("theirs"), "{body}");
    assert_eq!(body["people"].as_array().unwrap().len(), 3);
    let (_, body) = get(&h.app, &format!("{PEOPLE}?name=zed")).await;
    assert_eq!(body, json!({ "people": [] }));
}

/// Human door only: nothing on the agent's paths answers for it.
#[tokio::test]
async fn the_agent_door_has_no_people_route() {
    let h = calendar_harness(stevens);
    for uri in [
        "/client/agent/calendar/people?name=steven",
        "/client/agent/people?name=steven",
        "/client/agent/contacts?q=steven",
        "/mcp/calendar/people?name=steven",
        "/mcp/people?name=steven",
    ] {
        let (status, body) = get(&h.app, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert!(!body.to_string().contains("x.com"), "{uri}: {body}");
    }
    // The route itself is there, so the 404s above are the door's.
    let (status, _) = get(&h.app, &format!("{PEOPLE}?name=steven")).await;
    assert_eq!(status, StatusCode::OK);
}
