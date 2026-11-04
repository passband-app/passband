//! Hidden features, end to end on the control plane's side: an operator ticks
//! boxes on `/admin/features`, and each tenant's daemon reads its own list back
//! from `GET /tenant/features` with the control token it already holds.
//!
//! Everything here goes through the router, against a real Postgres, with the
//! registry swapped for one of this file's own: the build's real list is
//! allowed to be empty, and these tests must not start failing on the day the
//! last hidden feature launches.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    body::to_bytes,
    http::{HeaderMap, Request, StatusCode, header},
};
use serde_json::Value;
use squelch_control::config::{Config, WaitlistConfig};
use squelch_control::warden::HttpWarden;
use squelch_control::{ControlState, router, share};
use squelch_core::features::Feature;
use tower::ServiceExt as _;

mod common;

const ADMIN_TOKEN: &str = "0123456789abcdef0123456789abcdef";
const ORIGIN: &str = "https://signup.passband.test";

const REGISTRY: &[Feature] = &[
    Feature {
        key: "calendar",
        summary: "The calendar page & <its> grid.",
    },
    Feature {
        key: "agent_send",
        summary: "Agents may send mail.",
    },
];

struct Harness {
    app: Router,
    state: ControlState,
    db_url: String,
}

impl Harness {
    async fn new() -> Self {
        Self::with_registry(REGISTRY).await
    }

    async fn with_registry(registry: &'static [Feature]) -> Self {
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            public_url: ORIGIN.into(),
            redirect_uri: format!("{ORIGIN}/oauth/callback"),
            base_domain: "passband.test".into(),
            client_id: "test-client-id".into(),
            client_secret: "test-client-secret".into(),
            cookie_key: vec![42; 32],
            warden_url: "http://127.0.0.1:1".into(),
            warden_token: "warden-bearer".into(),
            database_url: "postgres://unused".into(),
            trusted_proxy_hops: 0,
            token_url: "http://127.0.0.1:1/token".into(),
            auth_url: "http://127.0.0.1:1/authorize".into(),
            profile_url: "http://127.0.0.1:1/profile".into(),
            userinfo_url: "http://127.0.0.1:1/userinfo".into(),
            microsoft: None,
            alert_webhook_url: None,
            posthog_url: None,
            bifrost: None,
            workos: None,
            waitlist: Some(WaitlistConfig {
                admin_token: ADMIN_TOKEN.into(),
                resend_api_key: "re_test_0123456789".into(),
                invite_from: "Passband <invites@passband.test>".into(),
                allowed_origin: "https://passband.test".into(),
                resend_url: "http://127.0.0.1:1".into(),
            }),
        };
        let (store, db_url) = common::fresh_store().await;
        let warden = Arc::new(
            HttpWarden::new(
                "http://127.0.0.1:1".into(),
                "warden-bearer".into(),
                Duration::from_secs(5),
            )
            .unwrap(),
        );
        let state = ControlState::new(config, store, warden)
            .unwrap()
            .with_feature_registry(registry);
        Self {
            app: router(state.clone()),
            state,
            db_url,
        }
    }

    /// Provision a tenant with a control token. Returns its row id and the
    /// plaintext token its daemon would present.
    async fn tenant(&self, label: &str) -> (i64, String) {
        let store = self.state.store();
        store
            .insert_tenant(label, &format!("{label}@example.com"))
            .await
            .unwrap();
        let minted = share::mint().unwrap();
        assert!(
            store
                .set_tenant_share_token(label, &minted.token_hash)
                .await
                .unwrap()
        );
        let id = common::raw_client(&self.db_url)
            .await
            .query_one("SELECT id FROM tenants WHERE label = $1", &[&label])
            .await
            .unwrap()
            .get(0);
        (id, minted.token)
    }

    async fn send(&self, req: Request<axum::body::Body>) -> (StatusCode, HeaderMap, String) {
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = String::from_utf8_lossy(&to_bytes(resp.into_body(), 1 << 20).await.unwrap())
            .to_string();
        (status, headers, body)
    }

    /// What a daemon holding `token` is told.
    async fn daemon_reads(&self, token: Option<&str>) -> (StatusCode, Value) {
        let mut req = Request::builder().method("GET").uri("/tenant/features");
        if let Some(token) = token {
            req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let (status, _, body) = self
            .send(req.body(axum::body::Body::empty()).unwrap())
            .await;
        (status, serde_json::from_str(&body).unwrap_or(Value::Null))
    }

    /// The features a daemon holding `token` is granted, as plain strings.
    async fn granted(&self, token: &str) -> Vec<String> {
        let (status, body) = self.daemon_reads(Some(token)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }

    async fn sign_in(&self) -> String {
        let (status, headers, body) = self
            .send(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::ORIGIN, ORIGIN)
                    .body(axum::body::Body::from(format!("token={ADMIN_TOKEN}")))
                    .unwrap(),
            )
            .await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
        let cookie = headers[header::SET_COOKIE].to_str().unwrap();
        cookie.split(';').next().unwrap().to_string()
    }

    async fn page(&self, cookie: Option<&str>) -> (StatusCode, String) {
        let mut req = Request::builder().method("GET").uri("/admin/features");
        if let Some(cookie) = cookie {
            req = req.header(header::COOKIE, cookie);
        }
        let (status, _, body) = self
            .send(req.body(axum::body::Body::empty()).unwrap())
            .await;
        (status, body)
    }

    /// Press Save on a page whose ticked boxes are `body`.
    async fn save(
        &self,
        body: &str,
        cookie: Option<&str>,
        origin: Option<&str>,
    ) -> (StatusCode, HeaderMap, String) {
        let mut req = Request::builder()
            .method("POST")
            .uri("/admin/features")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        if let Some(cookie) = cookie {
            req = req.header(header::COOKIE, cookie);
        }
        if let Some(origin) = origin {
            req = req.header(header::ORIGIN, origin);
        }
        self.send(req.body(axum::body::Body::from(body.to_string())).unwrap())
            .await
    }

    /// The same, as the signed-in operator's own browser on a page that drew
    /// every column in [`REGISTRY`], asserting it took.
    async fn save_ok(&self, cookie: &str, body: &str) {
        self.save_columns(cookie, &["calendar", "agent_send"], body)
            .await;
    }

    /// The same from a page that drew only `columns`.
    async fn save_columns(&self, cookie: &str, columns: &[&str], body: &str) {
        let columns: String = columns.iter().map(|k| format!("k={k}&")).collect();
        let body = format!("{columns}{body}");
        let (status, headers, page) = self.save(&body, Some(cookie), Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{page}");
        assert_eq!(headers[header::LOCATION], "/admin/features?saved");
    }

    async fn enabled_at(&self, tenant: i64, feature: &str) -> Option<String> {
        common::raw_client(&self.db_url)
            .await
            .query_opt(
                "SELECT enabled_at FROM tenant_features WHERE tenant_id = $1 AND feature = $2",
                &[&tenant, &feature],
            )
            .await
            .unwrap()
            .map(|r| r.get(0))
    }
}

/// Nothing is on for anybody until somebody ticks a box, and the answer to a
/// daemon that asks is an empty list rather than a refusal.
#[tokio::test]
async fn a_tenant_starts_with_nothing() {
    let h = Harness::new().await;
    let (_, token) = h.tenant("ada").await;
    assert!(h.granted(&token).await.is_empty());
}

/// The list is behind the control token, and every way a token can be bad is
/// one answer, exactly as it is on the mint beside it.
#[tokio::test]
async fn the_daemon_route_answers_only_a_live_control_token() {
    let h = Harness::new().await;
    let (id, token) = h.tenant("ada").await;
    let cookie = h.sign_in().await;
    h.save_ok(&cookie, &format!("t={id}&g={id}.calendar")).await;

    for bad in [None, Some("pbs_not_a_token"), Some("")] {
        let (status, body) = h.daemon_reads(bad).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{bad:?}");
        assert_eq!(body["error"], "unauthorized");
        assert!(body.get("features").is_none());
    }

    // A cancelled tenant's token stops answering too, grants and all.
    common::raw_client(&h.db_url)
        .await
        .execute("UPDATE tenants SET status = 'cancelled'", &[])
        .await
        .unwrap();
    assert_eq!(
        h.daemon_reads(Some(&token)).await.0,
        StatusCode::UNAUTHORIZED
    );
}

/// The whole errand: tick a box for one tenant, and that tenant's daemon (and
/// only that one) is told. Untick it and it is gone.
#[tokio::test]
async fn a_ticked_box_reaches_that_tenant_and_no_other() {
    let h = Harness::new().await;
    let (ada, ada_token) = h.tenant("ada").await;
    let (grace, grace_token) = h.tenant("grace").await;
    let cookie = h.sign_in().await;

    h.save_ok(
        &cookie,
        &format!("t={ada}&t={grace}&g={ada}.calendar&g={ada}.agent_send&g={grace}.agent_send"),
    )
    .await;
    assert_eq!(h.granted(&ada_token).await, ["agent_send", "calendar"]);
    assert_eq!(h.granted(&grace_token).await, ["agent_send"]);

    // The page shows what was saved: three boxes ticked, and the acknowledgement.
    let (status, page) = h.page(Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        page.contains(&format!(r#"value="{ada}.calendar" checked"#)),
        "{page}"
    );
    assert!(page.contains(&format!(r#"value="{grace}.agent_send" checked"#)));
    assert!(page.contains(&format!(r#"value="{grace}.calendar">"#)));
    assert!(page.contains("ada@example.com"));

    // Unticking is a box that does not come back.
    h.save_ok(&cookie, &format!("t={ada}&t={grace}&g={grace}.agent_send"))
        .await;
    assert!(h.granted(&ada_token).await.is_empty());
    assert_eq!(h.granted(&grace_token).await, ["agent_send"]);
}

/// The "everyone" row is on for every tenant, including one provisioned after
/// the box was ticked, and it is merged with a tenant's own grants.
#[tokio::test]
async fn everyone_means_the_tenant_who_signs_up_tomorrow_too() {
    let h = Harness::new().await;
    let (ada, ada_token) = h.tenant("ada").await;
    let cookie = h.sign_in().await;
    h.save_ok(&cookie, &format!("t={ada}&all=calendar&g={ada}.agent_send"))
        .await;

    let (_, late_token) = h.tenant("late").await;
    assert_eq!(h.granted(&late_token).await, ["calendar"]);
    assert_eq!(h.granted(&ada_token).await, ["agent_send", "calendar"]);

    let (_, page) = h.page(Some(&cookie)).await;
    assert!(
        page.contains(r#"name="all" value="calendar" checked"#),
        "{page}"
    );

    // And it comes off for everyone at once.
    h.save_ok(&cookie, &format!("t={ada}&g={ada}.agent_send"))
        .await;
    assert!(h.granted(&late_token).await.is_empty());
}

/// A save speaks only for the tenants the page listed. Somebody provisioned
/// (or granted something) after the page was rendered is not on the form, so a
/// stale Save must not read their absence as "untick everything".
#[tokio::test]
async fn a_stale_page_leaves_the_tenants_it_never_showed_alone() {
    let h = Harness::new().await;
    let (ada, ada_token) = h.tenant("ada").await;
    let (grace, grace_token) = h.tenant("grace").await;
    let cookie = h.sign_in().await;
    h.save_ok(&cookie, &format!("t={ada}&t={grace}&g={grace}.calendar"))
        .await;

    // A page that only ever listed ada, saved with a box for her.
    h.save_ok(&cookie, &format!("t={ada}&g={ada}.agent_send"))
        .await;
    assert_eq!(h.granted(&ada_token).await, ["agent_send"]);
    assert_eq!(h.granted(&grace_token).await, ["calendar"]);

    // And a ticked box for a tenant the form did not list is not a grant.
    h.save_ok(&cookie, &format!("t={ada}&g={grace}.agent_send"))
        .await;
    assert_eq!(h.granted(&grace_token).await, ["calendar"]);
}

/// The columns are the build's list, never the form's: a key the registry does
/// not name cannot be granted, to one tenant or to everyone, and neither can
/// something that is not a tenant.
#[tokio::test]
async fn only_a_registered_feature_can_be_granted() {
    let h = Harness::new().await;
    let (ada, token) = h.tenant("ada").await;
    let cookie = h.sign_in().await;
    h.save_ok(
        &cookie,
        &format!(
            "t={ada}&t=999999&t=junk&g={ada}.nope&g=999999.calendar&g=junk&g=.calendar&all=nope&all="
        ),
    )
    .await;
    assert!(h.granted(&token).await.is_empty());
}

/// A grant for a feature that has since LEFT the registry is outside what the
/// page shows, so saving the page does not disturb it: launching a feature is
/// deleting it from the code, and that must not need a database cleanup first.
#[tokio::test]
async fn a_grant_for_a_feature_this_build_no_longer_lists_is_left_alone() {
    let h = Harness::new().await;
    let (ada, token) = h.tenant("ada").await;
    let cookie = h.sign_in().await;
    let db = common::raw_client(&h.db_url).await;
    db.execute(
        "INSERT INTO tenant_features(tenant_id, feature, enabled_at)
         VALUES($1, 'retired', '2026-01-01T00:00:00.000Z')",
        &[&ada],
    )
    .await
    .unwrap();

    h.save_ok(&cookie, &format!("t={ada}")).await;
    assert_eq!(h.granted(&token).await, ["retired"]);
}

/// `enabled_at` is the only record of how long somebody has had a feature, so
/// saving the page again must not restamp the boxes that did not change.
#[tokio::test]
async fn an_unchanged_grant_keeps_the_day_it_was_given() {
    let h = Harness::new().await;
    let (ada, _) = h.tenant("ada").await;
    let cookie = h.sign_in().await;
    h.save_ok(&cookie, &format!("t={ada}&g={ada}.calendar"))
        .await;
    common::raw_client(&h.db_url)
        .await
        .execute(
            "UPDATE tenant_features SET enabled_at = '2026-01-01T00:00:00.000Z'",
            &[],
        )
        .await
        .unwrap();

    h.save_ok(
        &cookie,
        &format!("t={ada}&g={ada}.calendar&g={ada}.agent_send"),
    )
    .await;
    assert_eq!(
        h.enabled_at(ada, "calendar").await.as_deref(),
        Some("2026-01-01T00:00:00.000Z")
    );
    assert!(h.enabled_at(ada, "agent_send").await.is_some());
}

/// The page and its button are the operator's. A stranger gets the door and
/// learns nothing; a POST with no session, or from another origin with a real
/// one, changes nothing.
#[tokio::test]
async fn the_page_and_the_save_are_behind_the_admin_door() {
    let h = Harness::new().await;
    let (ada, token) = h.tenant("ada").await;

    let (status, page) = h.page(None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("Admin token"));
    assert!(!page.contains("ada@example.com"));
    assert!(!page.contains("calendar"));

    let body = format!("t={ada}&g={ada}.calendar");
    let (status, _, _) = h.save(&body, None, Some(ORIGIN)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let cookie = h.sign_in().await;
    let (status, _, _) = h
        .save(&body, Some(&cookie), Some("https://passband.test"))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(h.granted(&token).await.is_empty());
}

/// Nothing on the page is interpolated raw, the registry's own strings
/// included.
#[tokio::test]
async fn the_page_escapes_everything_it_prints() {
    let h = Harness::new().await;
    h.tenant("ada").await;
    let cookie = h.sign_in().await;
    let (_, page) = h.page(Some(&cookie)).await;
    assert!(
        page.contains("The calendar page &amp; &lt;its&gt; grid."),
        "{page}"
    );
    assert!(!page.contains("<its>"));
}

/// A build that hides nothing still has a page, and the page says so instead
/// of drawing an empty grid with a Save button under it.
#[tokio::test]
async fn an_empty_registry_is_a_sentence_not_a_form() {
    let h = Harness::with_registry(&[]).await;
    let (ada, token) = h.tenant("ada").await;
    let cookie = h.sign_in().await;
    let (status, page) = h.page(Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("This build hides nothing"), "{page}");
    assert!(!page.contains(r#"action="/admin/features""#));

    // And a hand-built POST against it grants nothing.
    h.save_ok(&cookie, &format!("t={ada}&g={ada}.calendar&all=calendar"))
        .await;
    assert!(h.granted(&token).await.is_empty());
}

/// A save speaks only for the COLUMNS its page drew, the same rule as for rows.
/// A tab rendered before a deploy added a feature has no boxes for it, and
/// saving that tab must not read their absence as "off", for one tenant or for
/// everyone.
#[tokio::test]
async fn a_page_from_before_a_feature_existed_cannot_untick_it() {
    let h = Harness::new().await;
    let (ada, token) = h.tenant("ada").await;
    let (_, late_token) = h.tenant("late").await;
    let cookie = h.sign_in().await;
    h.save_ok(
        &cookie,
        &format!("t={ada}&g={ada}.agent_send&all=agent_send"),
    )
    .await;

    // The old tab: it knows `calendar` only, and ticks it for ada.
    h.save_columns(&cookie, &["calendar"], &format!("t={ada}&g={ada}.calendar"))
        .await;
    assert_eq!(h.granted(&token).await, ["agent_send", "calendar"]);
    assert_eq!(h.granted(&late_token).await, ["agent_send"]);
    assert!(h.enabled_at(ada, "agent_send").await.is_some());

    // And it cannot grant through a column it did not draw either.
    h.save_columns(
        &cookie,
        &["calendar"],
        &format!("t={ada}&g={ada}.agent_send"),
    )
    .await;
    assert!(h.enabled_at(ada, "calendar").await.is_none());
}

/// A body that names no columns is not a page: cut short on the way, or built
/// by hand. It changes nothing, and in particular it does not switch the
/// "everyone" row off, which has no tenant field of its own to be missing.
#[tokio::test]
async fn a_body_with_no_columns_changes_nothing() {
    let h = Harness::new().await;
    let (ada, token) = h.tenant("ada").await;
    let cookie = h.sign_in().await;
    h.save_ok(&cookie, &format!("t={ada}&g={ada}.calendar&all=agent_send"))
        .await;

    for body in [String::new(), format!("t={ada}"), "k=nope".to_string()] {
        let (status, _, page) = h.save(&body, Some(&cookie), Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{page}");
        assert_eq!(
            h.granted(&token).await,
            ["agent_send", "calendar"],
            "{body:?}"
        );
    }
}

/// The page posts back the columns it drew, so the rule above has something to
/// read.
#[tokio::test]
async fn the_page_names_its_own_columns() {
    let h = Harness::new().await;
    h.tenant("ada").await;
    let cookie = h.sign_in().await;
    let (_, page) = h.page(Some(&cookie)).await;
    for key in ["calendar", "agent_send"] {
        assert!(
            page.contains(&format!(r#"<input type="hidden" name="k" value="{key}">"#)),
            "{page}"
        );
    }
}

/// A STORE THAT COULD NOT BE ASKED IS NOT A REFUSAL. The daemon drops every
/// feature it holds on a 401 and keeps them on anything else, so answering 401
/// for a database blip would switch the fleet's hidden features off for the
/// length of a Postgres restart.
#[tokio::test]
async fn a_store_that_cannot_answer_is_a_503_and_never_a_401() {
    let h = Harness::new().await;
    let (_, token) = h.tenant("ada").await;
    let db = common::raw_client(&h.db_url).await;

    // The grant read failing, with the token lookup fine.
    db.execute("ALTER TABLE tenant_features RENAME TO gone", &[])
        .await
        .unwrap();
    let (status, body) = h.daemon_reads(Some(&token)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");

    // The token lookup itself failing.
    db.execute("ALTER TABLE tenants RENAME TO gone_too", &[])
        .await
        .unwrap();
    let (status, body) = h.daemon_reads(Some(&token)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"], "unavailable");

    // A request with no token at all is still refused without asking the store.
    assert_eq!(h.daemon_reads(None).await.0, StatusCode::UNAUTHORIZED);
}

/// A tenant whose daemon holds no control token cannot ask what it was
/// granted, and the page says so on that row instead of offering boxes that
/// look live.
#[tokio::test]
async fn a_tenant_with_no_control_token_is_marked() {
    let h = Harness::new().await;
    h.tenant("ada").await;
    let cookie = h.sign_in().await;
    let (_, page) = h.page(Some(&cookie)).await;
    assert!(!page.contains("no control token"), "{page}");

    h.state
        .store()
        .insert_tenant("old", "old@example.com")
        .await
        .unwrap();
    let (_, page) = h.page(Some(&cookie)).await;
    assert_eq!(page.matches("no control token").count(), 1, "{page}");
}
