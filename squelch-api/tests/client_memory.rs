//! `/client/memory`: the owner-profile review page's routes
//! (docs/UNDERSTANDING.md §6), behind the `understanding` hidden feature.

use axum::http::StatusCode;
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use squelch_core::features::{Features, UNDERSTANDING};
use squelch_core::store::{SqliteStore, Store as _};
use squelch_core::triage::memory::{MemoryRecord, Origin, Provenance, inferred_expiry, new_ulid};
use tower::ServiceExt;

mod common;
use common::{Harness, authed, authed_json, body_json, state_with};

fn entry(id: &str, origin: Origin, line: &str) -> MemoryRecord {
    let at = Utc::now() - Duration::days(2);
    MemoryRecord {
        id: id.into(),
        scope: "owner".into(),
        origin,
        kind: "role".into(),
        body: json!({"what": line}),
        line: format!("{line}."),
        provenance: Provenance {
            message_ids: vec![],
            addrs: vec!["sam@acme.example".into()],
        },
        confidence: 0.6,
        revision: 1,
        created_at: at,
        updated_at: at,
        affirmed_at: at,
        expires_at: (origin == Origin::Inferred).then(|| inferred_expiry(at)),
        retired_at: None,
        deleted_at: None,
    }
}

/// Three entries: a live inferred one, a retired one, a deleted one.
fn seeded(store: &SqliteStore, acct: i64) -> (String, String, String) {
    let live = new_ulid(Utc::now());
    let retired = new_ulid(Utc::now() + Duration::milliseconds(1));
    let deleted = new_ulid(Utc::now() + Duration::milliseconds(2));
    store
        .insert_memory_entry_unchecked(acct, &entry(&live, Origin::Inferred, "Runs a studio"))
        .unwrap();
    let mut r = entry(&retired, Origin::Inferred, "Ran a shop");
    r.retired_at = Some(Utc::now() - Duration::days(1));
    store.insert_memory_entry_unchecked(acct, &r).unwrap();
    let mut d = entry(&deleted, Origin::Stated, "Gone");
    d.deleted_at = Some(Utc::now() - Duration::days(1));
    store.insert_memory_entry_unchecked(acct, &d).unwrap();
    (live, retired, deleted)
}

fn harness_with(features: Features) -> (Harness, (String, String, String)) {
    let mut ids = None;
    let (state, store, acct) = state_with(|store, acct| ids = Some(seeded(store, acct)));
    (
        Harness {
            app: squelch_api::router(state.with_features(features)),
            store,
            acct,
        },
        ids.unwrap(),
    )
}

fn on() -> Features {
    Features::pinned([UNDERSTANDING])
}

#[tokio::test]
async fn every_route_404s_while_the_feature_is_off() {
    let (h, (live, _, _)) = harness_with(Features::none());
    for request in [
        authed("GET", "/client/memory"),
        authed_json(
            "PUT",
            &format!("/client/memory/{live}"),
            json!({"confirm": true}),
        ),
        authed("DELETE", &format!("/client/memory/{live}")),
    ] {
        let resp = h.app.clone().oneshot(request).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
    // Nothing changed and nothing was audited: the routes do not exist.
    let entries = h.store.list_memory_entries(h.acct).unwrap();
    let live_row = entries.iter().find(|e| e.id == live).unwrap();
    assert_eq!(live_row.origin, Origin::Inferred);
    assert!(h.store.list_audit(h.acct, 50).unwrap().is_empty());
}

#[tokio::test]
async fn the_list_carries_retired_entries_and_never_deleted_ones() {
    let (h, (live, retired, deleted)) = harness_with(on());
    let resp = h
        .app
        .clone()
        .oneshot(authed("GET", "/client/memory"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CACHE_CONTROL)
            .unwrap(),
        "no-store"
    );
    let body = body_json(resp).await;
    // A brand-new account: the ledger is young, so the pass is not running.
    assert_eq!(body["enabled"], false);
    assert_eq!(body["coverage_days"], 0);
    let entries = body["entries"].as_array().unwrap();
    let ids: Vec<&str> = entries.iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [retired.as_str(), live.as_str()]);
    assert!(!ids.contains(&deleted.as_str()));
    let first: &Value = &entries[1];
    for key in [
        "id",
        "scope",
        "origin",
        "kind",
        "body",
        "line",
        "provenance",
        "confidence",
        "created_at",
        "updated_at",
        "affirmed_at",
        "expires_at",
        "retired_at",
    ] {
        assert!(first.get(key).is_some(), "missing {key}: {first}");
    }
    assert!(first.get("deleted_at").is_none());
    assert_eq!(first["origin"], "inferred");
    assert_eq!(first["body"], json!({"what": "Runs a studio"}));
    assert_eq!(
        first["provenance"],
        json!({"message_ids": [], "addrs": ["sam@acme.example"]})
    );
    assert!(entries[0]["retired_at"].is_string());
}

#[tokio::test]
async fn confirm_makes_an_entry_stated_and_permanent() {
    let (h, (live, _, _)) = harness_with(on());
    let resp = h
        .app
        .clone()
        .oneshot(authed_json(
            "PUT",
            &format!("/client/memory/{live}"),
            json!({"confirm": true}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CACHE_CONTROL)
            .unwrap(),
        "no-store"
    );
    let body = body_json(resp).await;
    assert_eq!(body["origin"], "stated");
    assert_eq!(body["expires_at"], Value::Null);
    assert_eq!(body["revision"], 1, "a bare confirm changes no words");
    let audit = h.store.list_audit(h.acct, 50).unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "memory.update" && a.detail.as_deref() == Some("confirmed"))
    );
}

#[tokio::test]
async fn an_edit_bumps_the_revision_and_rerenders_or_takes_the_owners_line() {
    let (h, (live, retired, _)) = harness_with(on());
    let put = |id: &str, body: Value| {
        h.app
            .clone()
            .oneshot(authed_json("PUT", &format!("/client/memory/{id}"), body))
    };
    let body = body_json(
        put(&live, json!({"body": {"what": "runs a bakery"}}))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["line"], "Runs a bakery.");
    assert_eq!(body["body"], json!({"what": "runs a bakery"}));
    assert_eq!(body["revision"], 2);
    assert_eq!(body["origin"], "stated");

    let body = body_json(
        put(&live, json!({"line": "I run a bakery with my sister."}))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["line"], "I run a bakery with my sister.");
    assert_eq!(body["revision"], 3);

    // Editing a retired entry brings it back as the owner's word.
    let body = body_json(put(&retired, json!({"confirm": true})).await.unwrap()).await;
    assert_eq!(body["retired_at"], Value::Null);
    assert_eq!(body["origin"], "stated");

    // A link never rides into the profile, even in the owner's own words.
    let resp = put(&live, json!({"line": "See https://example.com"}))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    // An id we never mint, and a well-formed one we do not hold: both 404,
    // and neither reaches the audit log.
    let before = h.store.list_audit(h.acct, 50).unwrap().len();
    for id in [
        "01NOSUCHENTRY",
        "x'%20OR%201=1",
        "01ZZZZZZZZZZZZZZZZZZZZZZZZ",
    ] {
        let resp = put(id, json!({"confirm": true})).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{id}");
        let resp = h
            .app
            .clone()
            .oneshot(authed("DELETE", &format!("/client/memory/{id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{id}");
    }
    assert_eq!(h.store.list_audit(h.acct, 50).unwrap().len(), before);
}

#[tokio::test]
async fn delete_is_audited_and_the_entry_is_gone_to_every_reader() {
    let (h, (live, _, deleted)) = harness_with(on());
    let resp = h
        .app
        .clone()
        .oneshot(authed("DELETE", &format!("/client/memory/{live}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(
        h.store
            .list_audit(h.acct, 50)
            .unwrap()
            .iter()
            .any(|a| a.action == "memory.delete" && a.target.as_deref() == Some(live.as_str()))
    );
    assert!(
        !h.store
            .list_memory_entries(h.acct)
            .unwrap()
            .iter()
            .any(|e| e.id == live)
    );
    for id in [&live, &deleted] {
        let resp = h
            .app
            .clone()
            .oneshot(authed("DELETE", &format!("/client/memory/{id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
