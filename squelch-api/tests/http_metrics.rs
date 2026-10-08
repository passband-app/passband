//! The per-route latency middleware: templates, not paths, and nothing a client
//! typed ever reaches a label.

mod common;

use axum::body::Body;
use axum::http::Request;
use common::{authed, state_with};
use squelch_api::{ROUTE_CLASSES, record_http_metrics, router};
use squelch_core::metrics::{SyncMetrics, render};
use tower::ServiceExt;

/// One request's `_count` line for a label set.
fn count(labels: &str) -> String {
    format!("squelchd_http_request_duration_seconds_count{{{labels}}} 1\n")
}

#[tokio::test]
async fn latency_is_keyed_by_route_template_and_unmatched_paths_collapse() {
    let (state, _store, _acct) = state_with(|_, _| {});
    let metrics = SyncMetrics::new();
    let app = router(state).layer(axum::middleware::from_fn_with_state(
        metrics.clone(),
        record_http_metrics,
    ));

    // A path parameter that does not exist: the 404 is recorded under the
    // TEMPLATE, and the id never appears in the exposition.
    app.clone()
        .oneshot(authed("GET", "/client/thread/thread-id-that-must-not-leak"))
        .await
        .unwrap();
    app.clone()
        .oneshot(authed("GET", "/client/stats"))
        .await
        .unwrap();
    // No bearer: the 401 is still attributed to its route, not to "unmatched".
    app.clone()
        .oneshot(
            Request::builder()
                .uri("/client/stats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Nothing routes here; the path is the client's and must collapse.
    app.oneshot(authed("GET", "/no/such/route?q=secret-query"))
        .await
        .unwrap();

    let text = render(&metrics, None);
    assert!(
        text.contains(&count(
            r#"route="/client/thread/{thread_id}",method="GET",status="4xx",class="local""#
        )),
        "{text}"
    );
    assert!(text.contains(&count(
        r#"route="/client/stats",method="GET",status="2xx",class="local""#
    )));
    assert!(text.contains(&count(
        r#"route="/client/stats",method="GET",status="4xx",class="local""#
    )));
    assert!(text.contains(&count(
        r#"route="unmatched",method="GET",status="4xx",class="local""#
    )));
    assert!(
        !text.contains("must-not-leak"),
        "a path parameter became a label"
    );
    assert!(
        !text.contains("secret-query"),
        "an unmatched path became a label"
    );
    assert!(!text.contains("/no/such/route"));
}

/// The table is strings, and a string does not notice its route being renamed.
/// Every row has to land on a real (method, template) in the router, or the
/// route it meant is being recorded as `local`.
#[tokio::test]
async fn every_classed_route_is_a_real_route_and_records_its_class() {
    let (state, _store, _acct) = state_with(|_, _| {});
    let metrics = SyncMetrics::new();
    let app = router(state).layer(axum::middleware::from_fn_with_state(
        metrics.clone(),
        record_http_metrics,
    ));

    for (method, template, class) in ROUTE_CLASSES {
        let uri = template.replace("{id}", "1");
        assert!(!uri.contains('{'), "fill in the parameter of {template}");
        let resp = app.clone().oneshot(authed(method, &uri)).await.unwrap();
        // A matched path with the wrong verb still carries its template, so
        // the label check below would pass on it.
        assert_ne!(resp.status(), 405, "{method} is not a verb of {template}");
        let text = render(&metrics, None);
        let series = format!(r#"route="{template}",method="{method}",status="#);
        let line = text
            .lines()
            .find(|l| l.contains(&series) && l.contains("_count"))
            .unwrap_or_else(|| panic!("{method} {template} matched no route\n{text}"));
        let want = format!(r#"class="{}""#, class.label());
        assert!(line.contains(&want), "{line}");
    }

    // The same template under a verb that reads the store stays local.
    app.oneshot(authed("GET", "/client/rules")).await.unwrap();
    let text = render(&metrics, None);
    assert!(text.contains(&count(
        r#"route="/client/rules",method="GET",status="2xx",class="local""#
    )));
}
