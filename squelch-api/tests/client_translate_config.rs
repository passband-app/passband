//! "Languages I read" over HTTP: `/client/translate-config`.

use squelch_core::store::Store;
use tower::ServiceExt;

mod common;
use common::{Harness, authed, authed_json, body_json, harness};

#[tokio::test]
async fn read_languages_round_trip_clean_and_list_every_choice() {
    let Harness { app, store, acct } = harness(|_, _| {});

    let json = body_json(
        app.clone()
            .oneshot(authed("GET", "/client/translate-config"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(json["read_languages"], serde_json::json!([]));
    let available = json["available"].as_array().unwrap();
    assert!(available.len() > 60, "{}", available.len());
    assert!(available.iter().all(|l| l["code"] != "en"));
    assert!(
        available
            .iter()
            .any(|l| l["code"] == "fr" && l["name"] == "French")
    );
    assert!(json["active"].is_boolean());

    let json = body_json(
        app.clone()
            .oneshot(authed_json(
                "POST",
                "/client/translate-config",
                serde_json::json!({ "read_languages": ["es", "EN", "zz", "fr", "es"] }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(json["read_languages"], serde_json::json!(["es", "fr"]));
    assert_eq!(store.read_languages(acct).unwrap(), vec!["es", "fr"]);

    let json = body_json(
        app.oneshot(authed("GET", "/client/translate-config"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(json["read_languages"], serde_json::json!(["es", "fr"]));
}
