//! The signup flow end to end, against a MOCK Google and a MOCK warden that
//! speaks wire v2.
//!
//! Both mocks are real axum servers on ephemeral loopback ports, the way
//! squelch-core tests the APNs relay, because the properties under test are
//! properties OF THE WIRE:
//!
//! - provisioning is TWO calls, and the credential rides only on the second,
//! - the ciphertext is sealed to EXACTLY the recipient the first call answered
//!   with, and what is inside it is a credentials-file slot map (not a bare
//!   token, which would decrypt into an empty map on the daemon),
//! - a failure between the two calls leaves a retriable signup: the address is
//!   held for that mailbox, the invite is NOT burned, and the retry lands on the
//!   same recipient,
//! - Google receives the PKCE verifier that matches the challenge the consent
//!   URL carried,
//! - the success page carries the pairing code, the tenant URL, and the deep
//!   link.
//!
//! The MOCK WARDEN mints a per-tenant age identity, exactly as the real one
//! does, and keeps it. This test process is the only place both halves of a key
//! ever exist; the real control plane never sees an identity at all.
//!
//! Nothing here touches a real Google endpoint, a real cluster, a real port
//! 8848, or any store outside the throwaway Postgres schema `common` makes.
//! `Config` is constructed directly rather than read from the environment,
//! which is exactly why `token_url`/`profile_url` are fields:
//! `Config::from_env` pins Google's and nothing can move them.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    Json, Router,
    body::to_bytes,
    extract::State as AxumState,
    http::{HeaderMap, Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use base64::Engine as _;
use serde_json::{Value, json};
use squelch_control::config::{
    BifrostConfig, Config, DEFAULT_ASSISTANT_BUDGET_USD, DEFAULT_LLM_BUDGET_USD, MicrosoftConfig,
    WaitlistConfig,
};
use squelch_control::warden::HttpWarden;
use squelch_control::{ControlState, activation, invites, retention, router, seal};
use tower::ServiceExt as _;

/// The throwaway Postgres schema every harness in this file is built on.
mod common;

/// The refresh token the mock Google hands out. Every assertion that this
/// never leaves the process in the clear greps for this exact string.
const REFRESH_TOKEN: &str = "1//THE-SECRET-REFRESH-TOKEN";
const ACCESS_TOKEN: &str = "ya29.THE-SECRET-ACCESS-TOKEN";
const MAILBOX: &str = "ada@example.com";
const READONLY: &str = "https://www.googleapis.com/auth/gmail.readonly";
const MODIFY: &str = "https://www.googleapis.com/auth/gmail.modify";
const SEND: &str = "https://www.googleapis.com/auth/gmail.send";
const CAL_EVENTS: &str = "https://www.googleapis.com/auth/calendar.events";
const CAL_LIST: &str = "https://www.googleapis.com/auth/calendar.calendarlist.readonly";

/// The mock Microsoft app and account. The account is the one Outlook was
/// built against: a PERSONAL Microsoft account that signs in as an address on
/// another company's domain (here the same address a Gmail tenant has) while
/// its mailbox is an `@outlook.com` alias.
const MS_CLIENT_ID: &str = "11111111-2222-3333-4444-555555555555";
const MS_CLIENT_SECRET: &str = "ms-hosted-secret";
const MS_ID: &str = "313658d2f27bcf04";
const MS_SIGNIN: &str = "braelyn@passband.test";
const MS_MAILBOX: &str = "passband-qa@outlook.com";
const MS_GRANT: &str = "https://graph.microsoft.com/Mail.ReadWrite https://graph.microsoft.com/Mail.Send https://graph.microsoft.com/User.Read https://graph.microsoft.com/Calendars.ReadWrite openid";

/// What a consent that left every box checked reports back, the way Google
/// spells a scope set: space delimited, in one string.
fn full_grant() -> String {
    MODIFY.to_string()
}

/// A calendar consent with every box left checked.
fn calendar_grant() -> String {
    format!("{MODIFY} {CAL_EVENTS} {CAL_LIST}")
}

/// One tenant as the mock warden holds it: the identity it minted (which never
/// leaves the warden in production), the mailbox that reserved the label, and
/// whether a credential has been installed.
struct MockTenant {
    account_email: String,
    identity: age::x25519::Identity,
    recipient: String,
    provisioned: bool,
}

/// What the mocks recorded, so the test can assert on the bytes each service
/// actually received.
#[derive(Default)]
struct Recorder {
    /// When set, the replace route records the install and then answers this
    /// status instead of confirming the rollout: the warden's side of an
    /// outcome control cannot settle (503), or one it must not retry (401).
    reconnect_status_after_install: Option<u16>,
    /// When set, the replace route takes this long to answer, so a test can
    /// land a second consent while the first is in flight.
    reconnect_delay: Option<Duration>,
    /// Form fields of the token exchange.
    token_form: Vec<(String, String)>,
    /// Bodies posted to `POST /v1/tenants` (call 1).
    create_bodies: Vec<Value>,
    /// `(label, body)` seen on `PUT /v1/tenants/{label}/credentials` (call 2).
    /// Recorded BEFORE the mock decides whether to fail, so a failed install is
    /// still visible to the assertions.
    credential_puts: Vec<(String, Value)>,
    /// `(label, body)` seen on `PUT /v1/tenants/{label}/llm-key`. The whole
    /// body, so absence of a slot can be asserted, not just presence.
    llm_key_puts: Vec<(String, Value)>,
    /// `(label, body)` seen on `PUT /v1/tenants/{label}/control-token`, the
    /// share token signup installs before the workload is applied.
    control_token_puts: Vec<(String, Value)>,
    /// When true, the control-token install answers 503, which must cost the
    /// tenant its button and nothing else.
    fail_control_token: bool,
    /// Bodies posted to the mock Bifrost's mint route.
    bifrost_mint_bodies: Vec<Value>,
    /// Authorization headers the mock Bifrost saw, on every route. The live
    /// gateway takes HTTP Basic with the admin `username:password`.
    bifrost_auths: Vec<String>,
    /// Key NAMES the mock Bifrost should refuse to mint, so one of a signup's
    /// two mints can fail while the other lands.
    bifrost_fail_names: Vec<String>,
    /// Keys the mock Bifrost minted, `(id, top-up)`, which its listing
    /// reports with one budget each (`b-<id>`) and any top-up applied.
    bifrost_minted: Vec<(String, f64)>,
    /// Budget-override PUTs the mock Bifrost saw: `(vk id, budget id, body)`.
    bifrost_overrides: Vec<(String, String, Value)>,
    /// When true, the override route answers 500.
    bifrost_fail_override: bool,
    /// Bearer values the warden saw, on every route.
    warden_bearers: Vec<String>,
    /// Labels asked about via `GET /v1/tenants/{label}`.
    status_lookups: Vec<String>,
    /// Labels the warden should report as ALREADY LIVE (somebody else's).
    taken: Vec<String>,
    /// Tenants the warden has minted an identity for, by label.
    tenants: BTreeMap<String, MockTenant>,
    /// When true, call 2 fails the way an apply that never went Ready does.
    fail_credentials: bool,
    /// When true, the llm-key install answers 503 `llm_not_configured`, the
    /// way a warden whose LLM wiring is absent refuses a key it cannot place.
    fail_llm_key: bool,
    /// When set, call 2 answers with this status instead of doing anything. For
    /// the statuses that are a WIRE disagreement rather than an outcome.
    credentials_status: Option<u16>,
    /// When set, the mock Google verifies PKCE the way Google does: the
    /// presented verifier must S256-hash to this challenge.
    expected_challenge: Option<String>,
    /// The `scope` the mock Google reports on the token. `None` is the whole set
    /// signup asks for; `Some` is a user who unchecked something.
    granted_scope: Option<String>,
    /// The mailbox the mock Gmail profile names. `None` is [`MAILBOX`]; `Some`
    /// is a consent finished while signed in as somebody else.
    profile_email: Option<String>,
    /// What `GET /v1/tenants/{label}/devices` answers, by label: a present
    /// entry is a 200 with this `first_paired_at` (None = the null body,
    /// nobody paired yet), an absent one is a 404. Labels listed in
    /// `devices_unavailable` answer 503 `not_running` first — the mid-roll
    /// tenant the poller must leave for the next pass.
    devices_paired: BTreeMap<String, Option<String>>,
    devices_unavailable: Vec<String>,
    /// What `GET /v1/tenants/{label}/activity` answers, by label: a present
    /// entry is a 200 with this as the body's `activity` (Null = a daemon too
    /// old to have the command), an absent one is a 404. Every ask is
    /// counted in `activity_asks`.
    activity: BTreeMap<String, Value>,
    activity_asks: Vec<String>,
    /// The `text` of every post to the operator's webhook, which this mock
    /// serves at `/hook` so every harness has alerts wired the way production
    /// does.
    alerts: Vec<String>,
    /// Every event PostHog's `/batch/` was sent, which this mock also serves.
    posthog: Vec<Value>,
    /// Form fields of the last exchange at the mock MICROSOFT token endpoint.
    ms_token_form: Vec<(String, String)>,
    /// What the mock Graph `/me` answers. `None` is a personal account that
    /// signs in as an address on somebody else's domain.
    ms_me: Option<Value>,
    /// The To addresses on the mock inbox, newest first.
    ms_inbox_to: Vec<String>,
}

type Shared = Arc<Mutex<Recorder>>;

/// A mock Google: the token endpoint and the Gmail profile endpoint.
async fn spawn_google(rec: Shared) -> String {
    let app = Router::new()
        .route(
            "/token",
            post(
                |AxumState(rec): AxumState<Shared>, body: String| async move {
                    let form: Vec<(String, String)> = url::form_urlencoded::parse(body.as_bytes())
                        .map(|(k, v)| (k.into_owned(), v.into_owned()))
                        .collect();
                    let (expected_challenge, granted_scope) = {
                        let mut r = rec.lock().unwrap();
                        r.token_form = form.clone();
                        (
                            r.expected_challenge.clone(),
                            r.granted_scope.clone().unwrap_or_else(full_grant),
                        )
                    };

                    // PKCE is verified here, the way Google verifies it: the
                    // presented verifier must S256-hash to the challenge that
                    // rode on the consent URL. The test drives the real consent
                    // URL through, so this is a genuine check and not a
                    // rubber stamp.
                    let get = |k: &str| {
                        form.iter()
                            .find(|(f, _)| f == k)
                            .map(|(_, v)| v.clone())
                            .unwrap_or_default()
                    };
                    let verifier = get("code_verifier");
                    let challenge_ok = match &expected_challenge {
                        None => !verifier.is_empty(),
                        Some(expected) => &s256(&verifier) == expected,
                    };
                    if get("grant_type") != "authorization_code" || !challenge_ok {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({"error":"invalid_grant"})),
                        )
                            .into_response();
                    }
                    Json(json!({
                        "access_token": ACCESS_TOKEN,
                        "refresh_token": REFRESH_TOKEN,
                        "expires_in": 3599,
                        "token_type": "Bearer",
                        "scope": granted_scope,
                    }))
                    .into_response()
                },
            ),
        )
        // THE MOCK MICROSOFT, on the same server: the confidential app's
        // token endpoint and the two Graph reads an Outlook signup makes.
        .route(
            "/ms/token",
            post(
                |AxumState(rec): AxumState<Shared>, body: String| async move {
                    let form: Vec<(String, String)> = url::form_urlencoded::parse(body.as_bytes())
                        .map(|(k, v)| (k.into_owned(), v.into_owned()))
                        .collect();
                    rec.lock().unwrap().ms_token_form = form;
                    Json(json!({
                        "access_token": ACCESS_TOKEN,
                        "refresh_token": REFRESH_TOKEN,
                        "expires_in": 3599,
                        "token_type": "Bearer",
                        "scope": MS_GRANT,
                    }))
                },
            ),
        )
        .route(
            "/graph/me",
            get(|AxumState(rec): AxumState<Shared>| async move {
                let me = rec.lock().unwrap().ms_me.clone().unwrap_or_else(
                    || json!({"id": MS_ID, "mail": MS_SIGNIN, "userPrincipalName": MS_SIGNIN}),
                );
                Json(me)
            }),
        )
        .route(
            "/graph/me/mailFolders/inbox/messages",
            get(|AxumState(rec): AxumState<Shared>| async move {
                let to = rec.lock().unwrap().ms_inbox_to.clone();
                let value: Vec<Value> = to
                    .iter()
                    .map(|a| json!({"toRecipients": [{"emailAddress": {"address": a}}]}))
                    .collect();
                Json(json!({ "value": value }))
            }),
        )
        .route(
            "/profile",
            get(|AxumState(rec): AxumState<Shared>| async move {
                let email = rec
                    .lock()
                    .unwrap()
                    .profile_email
                    .clone()
                    .unwrap_or_else(|| MAILBOX.to_string());
                Json(json!({ "emailAddress": email }))
            }),
        )
        .with_state(rec);
    spawn(app).await
}

fn bearer_of(headers: &HeaderMap) -> String {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

fn str_field(v: &Value, name: &str) -> String {
    v.get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn json_status(status: StatusCode, reason: &str) -> Response {
    (status, Json(json!({ "error": reason }))).into_response()
}

/// A mock warden implementing the control -> warden contract, v2.
async fn spawn_warden(rec: Shared) -> String {
    let app = Router::new()
        // Not the warden's: the operator's Slack webhook, served from this
        // mock only so the harness needs no fourth server.
        .route(
            "/hook",
            post(
                |AxumState(rec): AxumState<Shared>, body: String| async move {
                    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    rec.lock().unwrap().alerts.push(str_field(&parsed, "text"));
                    "ok"
                },
            ),
        )
        // Not the warden's either: PostHog's ingest, for the hosted identify.
        .route(
            "/batch/",
            post(
                |AxumState(rec): AxumState<Shared>, body: String| async move {
                    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    let mut r = rec.lock().unwrap();
                    for e in parsed["batch"].as_array().cloned().unwrap_or_default() {
                        r.posthog.push(e);
                    }
                    Json(json!({ "status": 1 }))
                },
            ),
        )
        .route(
            "/v1/tenants",
            post(
                |AxumState(rec): AxumState<Shared>, headers: HeaderMap, body: String| async move {
                    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    let mut r = rec.lock().unwrap();
                    r.create_bodies.push(parsed.clone());
                    r.warden_bearers.push(bearer_of(&headers));

                    let label = str_field(&parsed, "label");
                    let account_email = str_field(&parsed, "account_email");
                    if label.is_empty() || account_email.is_empty() {
                        return json_status(StatusCode::UNPROCESSABLE_ENTITY, "invalid");
                    }
                    if r.taken.contains(&label) {
                        return json_status(StatusCode::CONFLICT, "exists");
                    }
                    // THE IDEMPOTENCY THE CONTRACT PINS: a second create for a
                    // PENDING label from the SAME mailbox answers with the SAME
                    // recipient. A provisioned label, or a different mailbox,
                    // is a duplicate and gets 409.
                    if let Some(existing) = r.tenants.get(&label) {
                        if existing.provisioned || existing.account_email != account_email {
                            return json_status(StatusCode::CONFLICT, "exists");
                        }
                        return (
                            StatusCode::CREATED,
                            Json(json!({ "recipient": existing.recipient })),
                        )
                            .into_response();
                    }

                    let identity = age::x25519::Identity::generate();
                    let recipient = identity.to_public().to_string();
                    r.tenants.insert(
                        label,
                        MockTenant {
                            account_email,
                            identity,
                            recipient: recipient.clone(),
                            provisioned: false,
                        },
                    );
                    (StatusCode::CREATED, Json(json!({ "recipient": recipient }))).into_response()
                },
            ),
        )
        .route(
            "/v1/tenants/{label}/credentials/replace",
            put(
                |AxumState(rec): AxumState<Shared>,
                 axum::extract::Path(label): axum::extract::Path<String>,
                 headers: HeaderMap,
                 body: String| async move {
                    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    let delay = rec.lock().unwrap().reconnect_delay;
                    if let Some(delay) = delay {
                        tokio::time::sleep(delay).await;
                    }
                    let mut r = rec.lock().unwrap();
                    r.warden_bearers.push(bearer_of(&headers));
                    let account_email = str_field(&parsed, "account_email");
                    // UNLIKE the signup route, a PROVISIONED tenant is the whole
                    // point. What is refused is a mailbox that does not own it.
                    let owned = matches!(
                        r.tenants.get(&label),
                        Some(t) if t.account_email == account_email
                    );
                    if !r.tenants.contains_key(&label) {
                        return json_status(StatusCode::NOT_FOUND, "unknown");
                    }
                    if !owned {
                        return json_status(StatusCode::CONFLICT, "not_owner");
                    }
                    // THE STATUS GUARD the real warden carries: a PENDING
                    // tenant is a signup stopped between its two calls, and
                    // installing here would finish it with no invite spent.
                    if !r.tenants[&label].provisioned {
                        return json_status(StatusCode::CONFLICT, "not_serving");
                    }
                    r.credential_puts.push((label.clone(), parsed));
                    if let Some(status) = r.reconnect_status_after_install {
                        return json_status(
                            StatusCode::from_u16(status).unwrap(),
                            "rollout_pending",
                        );
                    }
                    (
                        StatusCode::OK,
                        Json(json!({
                            "pair_code": "ABCD-EFGH",
                            "pair_url": format!("https://{label}.passband.test"),
                            "deep_link": "passband://pair?url=x&code=ABCD-EFGH",
                        })),
                    )
                        .into_response()
                },
            ),
        )
        // A login's pairing code, for the Outlook app-login tests.
        .route(
            "/v1/tenants/{label}/pair",
            post(
                |axum::extract::Path(label): axum::extract::Path<String>| async move {
                    Json(json!({
                        "pair_code": "ABCD-EFGH",
                        "pair_url": format!("https://{label}.passband.test"),
                        "deep_link": "passband://pair?url=x&code=ABCD-EFGH",
                    }))
                },
            ),
        )
        .route(
            "/v1/tenants/{label}/recipient",
            post(
                |AxumState(rec): AxumState<Shared>,
                 axum::extract::Path(label): axum::extract::Path<String>,
                 headers: HeaderMap,
                 body: String| async move {
                    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    let mut r = rec.lock().unwrap();
                    r.warden_bearers.push(bearer_of(&headers));
                    let account_email = str_field(&parsed, "account_email");
                    // UNLIKE create: a PROVISIONED tenant is exactly what this
                    // route is for, so being serving is not a conflict. What is
                    // a conflict is a different mailbox.
                    match r.tenants.get(&label) {
                        None => json_status(StatusCode::NOT_FOUND, "not_found"),
                        Some(t) if t.account_email != account_email => {
                            json_status(StatusCode::CONFLICT, "exists")
                        }
                        Some(t) => (StatusCode::OK, Json(json!({ "recipient": t.recipient })))
                            .into_response(),
                    }
                },
            ),
        )
        .route(
            "/v1/tenants/{label}/credentials",
            put(
                |AxumState(rec): AxumState<Shared>,
                 axum::extract::Path(label): axum::extract::Path<String>,
                 headers: HeaderMap,
                 body: String| async move {
                    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    let mut r = rec.lock().unwrap();
                    r.credential_puts.push((label.clone(), parsed));
                    r.warden_bearers.push(bearer_of(&headers));

                    if let Some(status) = r.credentials_status {
                        let status = StatusCode::from_u16(status).unwrap();
                        return json_status(status, "refused");
                    }
                    if r.fail_credentials {
                        // The apply failed, or the pod never went Ready. The
                        // tenant stays pending and keeps its identity.
                        return json_status(StatusCode::INTERNAL_SERVER_ERROR, "apply_failed");
                    }
                    match r.tenants.get_mut(&label) {
                        None => json_status(StatusCode::NOT_FOUND, "unknown"),
                        Some(t) if t.provisioned => {
                            json_status(StatusCode::CONFLICT, "provisioned")
                        }
                        Some(t) => {
                            t.provisioned = true;
                            Json(json!({
                                "pair_code": "ABCD-EFGH",
                                "pair_url": format!("https://{label}.passband.test"),
                                "deep_link": "passband://pair?url=x&code=ABCD-EFGH",
                            }))
                            .into_response()
                        }
                    }
                },
            ),
        )
        .route(
            "/v1/tenants/{label}/control-token",
            put(
                |AxumState(rec): AxumState<Shared>,
                 axum::extract::Path(label): axum::extract::Path<String>,
                 headers: HeaderMap,
                 body: String| async move {
                    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    let mut r = rec.lock().unwrap();
                    r.control_token_puts.push((label, parsed));
                    r.warden_bearers.push(bearer_of(&headers));
                    if r.fail_control_token {
                        return json_status(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
                    }
                    Json(json!({})).into_response()
                },
            ),
        )
        .route(
            "/v1/tenants/{label}/llm-key",
            put(
                |AxumState(rec): AxumState<Shared>,
                 axum::extract::Path(label): axum::extract::Path<String>,
                 headers: HeaderMap,
                 body: String| async move {
                    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    let mut r = rec.lock().unwrap();
                    // Recorded BEFORE the mock decides whether to fail, so a
                    // refused install is still visible to the assertions.
                    r.llm_key_puts.push((label, parsed));
                    r.warden_bearers.push(bearer_of(&headers));
                    if r.fail_llm_key {
                        return json_status(StatusCode::SERVICE_UNAVAILABLE, "llm_not_configured");
                    }
                    Json(json!({})).into_response()
                },
            ),
        )
        .route(
            "/v1/tenants/{label}",
            get(
                |AxumState(rec): AxumState<Shared>,
                 axum::extract::Path(label): axum::extract::Path<String>,
                 headers: HeaderMap| async move {
                    let mut r = rec.lock().unwrap();
                    r.status_lookups.push(label.clone());
                    r.warden_bearers.push(bearer_of(&headers));
                    if r.taken.contains(&label) {
                        return Json(json!({"status":"active"})).into_response();
                    }
                    match r.tenants.get(&label) {
                        Some(t) if t.provisioned => {
                            Json(json!({"status":"active"})).into_response()
                        }
                        Some(_) => Json(json!({"status":"pending"})).into_response(),
                        None => json_status(StatusCode::NOT_FOUND, "unknown"),
                    }
                },
            ),
        )
        .route(
            "/v1/tenants/{label}/activity",
            get(
                |AxumState(rec): AxumState<Shared>,
                 axum::extract::Path(label): axum::extract::Path<String>,
                 headers: HeaderMap| async move {
                    let mut r = rec.lock().unwrap();
                    r.warden_bearers.push(bearer_of(&headers));
                    r.activity_asks.push(label.clone());
                    match r.activity.get(&label) {
                        Some(a) => Json(json!({ "activity": a })).into_response(),
                        None => json_status(StatusCode::NOT_FOUND, "not_found"),
                    }
                },
            ),
        )
        .route(
            "/v1/tenants/{label}/devices",
            get(
                |AxumState(rec): AxumState<Shared>,
                 axum::extract::Path(label): axum::extract::Path<String>,
                 headers: HeaderMap| async move {
                    let mut r = rec.lock().unwrap();
                    r.warden_bearers.push(bearer_of(&headers));
                    if r.devices_unavailable.contains(&label) {
                        return json_status(StatusCode::SERVICE_UNAVAILABLE, "not_running");
                    }
                    match r.devices_paired.get(&label) {
                        Some(at) => Json(json!({ "first_paired_at": at })).into_response(),
                        None => json_status(StatusCode::NOT_FOUND, "not_found"),
                    }
                },
            ),
        )
        .with_state(rec);
    spawn(app).await
}

/// The id and value the mock Bifrost mints, derived from the requested key
/// NAME so a signup's two mints — `tenant-<label>` and
/// `tenant-<label>-assistant` — stay distinguishable end to end. The control
/// plane must forward each value to the warden and keep no other copy.
fn vk_id_for(name: &str) -> String {
    format!("vk-{name}")
}

fn vk_value_for(name: &str) -> String {
    format!("sk-bf-KEY-FOR-{name}")
}

/// The Bifrost admin credential the harness configures: `username:password`,
/// sent by the client as HTTP Basic on every governance call.
const BIFROST_ADMIN: &str = "bifrost-admin:the-basic-password";

/// The provider key id the mock gateway lists, mirroring the one the live
/// gateway auto-detects.
const PROVIDER_KEY_ID: &str = "ANTHROPIC_API_KEY_auto_detected";

fn bifrost_basic_auth() -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(BIFROST_ADMIN)
    )
}

/// A mock Bifrost: the two routes a mint speaks (the provider-key listing and
/// the governance create), answering the LIVE gateway's shapes — the mint echo
/// carries the attached `budgets` and `provider_configs`, which the client
/// refuses to trust a key without.
async fn spawn_bifrost(rec: Shared) -> String {
    let app = Router::new()
        .route(
            "/api/providers/anthropic/keys",
            get(
                |AxumState(rec): AxumState<Shared>, headers: HeaderMap| async move {
                    let mut r = rec.lock().unwrap();
                    r.bifrost_auths.push(bearer_of(&headers));
                    Json(json!({ "keys": [{ "id": PROVIDER_KEY_ID, "models": [] }] }))
                        .into_response()
                },
            ),
        )
        .route(
            "/api/governance/virtual-keys",
            post(
                |AxumState(rec): AxumState<Shared>, headers: HeaderMap, body: String| async move {
                    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    let name = str_field(&parsed, "name");
                    let mut r = rec.lock().unwrap();
                    // Recorded BEFORE the mock decides whether to fail, so a
                    // refused mint is still visible to the assertions.
                    r.bifrost_mint_bodies.push(parsed);
                    r.bifrost_auths.push(bearer_of(&headers));
                    if r.bifrost_fail_names.contains(&name) {
                        return json_status(StatusCode::INTERNAL_SERVER_ERROR, "mint_failed");
                    }
                    r.bifrost_minted.push((vk_id_for(&name), 0.0));
                    (
                        StatusCode::OK,
                        Json(json!({
                            "message": "Virtual key created successfully",
                            "virtual_key": {
                                "id": vk_id_for(&name),
                                "value": vk_value_for(&name),
                                "budgets": [{ "max_limit": DEFAULT_LLM_BUDGET_USD, "reset_duration": "1M" }],
                                "provider_configs": [{ "provider": "anthropic", "weight": 1 }],
                            },
                        })),
                    )
                        .into_response()
                },
            )
            .get(
                |AxumState(rec): AxumState<Shared>, headers: HeaderMap| async move {
                    let mut r = rec.lock().unwrap();
                    r.bifrost_auths.push(bearer_of(&headers));
                    let keys: Vec<Value> = r
                        .bifrost_minted
                        .iter()
                        .map(|(id, top_up)| {
                            json!({
                                "id": id,
                                "name": id,
                                "budgets": [{
                                    "id": format!("b-{id}"),
                                    "max_limit": DEFAULT_LLM_BUDGET_USD,
                                    "current_usage": 0,
                                    "override_amount": top_up,
                                }],
                            })
                        })
                        .collect();
                    Json(json!({ "virtual_keys": keys })).into_response()
                },
            ),
        )
        .route(
            "/api/governance/virtual-keys/{id}/budgets/{budget}/override",
            axum::routing::put(
                |AxumState(rec): AxumState<Shared>,
                 axum::extract::Path((id, budget)): axum::extract::Path<(String, String)>,
                 headers: HeaderMap,
                 body: String| async move {
                    let mut r = rec.lock().unwrap();
                    r.bifrost_auths.push(bearer_of(&headers));
                    let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    r.bifrost_overrides.push((id.clone(), budget, body.clone()));
                    if r.bifrost_fail_override {
                        return StatusCode::INTERNAL_SERVER_ERROR;
                    }
                    let amount = body["amount"].as_f64().unwrap_or(0.0);
                    for key in r.bifrost_minted.iter_mut().filter(|(k, _)| *k == id) {
                        key.1 = amount;
                    }
                    StatusCode::OK
                },
            ),
        )
        .with_state(rec);
    spawn(app).await
}

async fn spawn(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    format!("http://{addr}")
}

/// A whole control plane wired to the two mocks.
struct Harness {
    app: Router,
    state: ControlState,
    rec: Shared,
    invite_code: String,
    /// The schema this harness's store lives in, for the assertions that read a
    /// column no method returns. `ControlStore::client` is crate-private, so a
    /// test needing raw SQL opens its own connection and can only land on the
    /// same schema if it is given the same URL.
    db_url: String,
}

/// How a harness is wired to Bifrost: not at all (the trio unset), a live
/// mock, or a configured gateway that is DOWN (the fail-soft case).
enum Bifrost {
    Off,
    Mock,
    Down,
}

impl Harness {
    async fn new() -> Self {
        Self::build(Bifrost::Off, false).await
    }

    /// The same, with the invite policy configured, which is what production
    /// has and what signup's share-token mint is gated on: a deployment with no
    /// invite feature installs no credential for a door that answers 503.
    async fn with_invites() -> Self {
        Self::build(Bifrost::Off, true).await
    }

    async fn with_bifrost(bifrost: Bifrost) -> Self {
        Self::build(bifrost, false).await
    }

    async fn build(bifrost: Bifrost, waitlist: bool) -> Self {
        let rec: Shared = Arc::new(Mutex::new(Recorder::default()));
        let google = spawn_google(rec.clone()).await;
        let warden_url = spawn_warden(rec.clone()).await;
        let bifrost_url = match bifrost {
            Bifrost::Off => None,
            Bifrost::Mock => Some(spawn_bifrost(rec.clone()).await),
            // Port 1: nothing listens, so every mint fails the way an outage
            // fails.
            Bifrost::Down => Some("http://127.0.0.1:1".to_string()),
        };

        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            public_url: "https://signup.passband.test".into(),
            redirect_uri: "https://signup.passband.test/oauth/callback".into(),
            base_domain: "passband.test".into(),
            client_id: "test-client-id".into(),
            client_secret: "test-client-secret".into(),
            cookie_key: vec![42; 32],
            warden_url: warden_url.clone(),
            warden_token: "warden-bearer".into(),
            database_url: "postgres://unused".into(),
            trusted_proxy_hops: 0,
            token_url: format!("{google}/token"),
            auth_url: format!("{google}/authorize"),
            profile_url: format!("{google}/profile"),
            // Never reached on this flow: a signup's mailbox comes from Gmail's
            // profile endpoint, which is what its own grant permits. The console
            // login is the one that reads userinfo (`tests/console_auth.rs`).
            userinfo_url: format!("{google}/userinfo"),
            microsoft: Some(MicrosoftConfig {
                client_id: MS_CLIENT_ID.into(),
                client_secret: MS_CLIENT_SECRET.into(),
                auth_url: format!("{google}/ms/authorize"),
                token_url: format!("{google}/ms/token"),
                graph_base: format!("{google}/graph"),
            }),
            alert_webhook_url: Some(format!("{warden_url}/hook")),
            posthog_url: Some(warden_url.clone()),
            // No agent door: `/agent/auth` is not mounted.
            workos: None,
            // The struct is constructed directly, so an http mock URL is fine
            // here; `from_env` is where https is enforced.
            bifrost: bifrost_url.map(|url| BifrostConfig {
                url,
                admin_token: BIFROST_ADMIN.into(),
                budget_usd: DEFAULT_LLM_BUDGET_USD,
                models: vec!["claude-haiku-4-5".into(), "claude-sonnet-5".into()],
                assistant_budget_usd: DEFAULT_ASSISTANT_BUDGET_USD,
                assistant_models: vec!["claude-haiku-4-5".into(), "claude-opus-4-8".into()],
            }),
            // Off by default: the signup flow does not touch the waitlist, so
            // those routes are not mounted at all. On for the tests that care
            // about the share token, which signup mints only where there is an
            // invite feature to spend it on.
            waitlist: waitlist.then(|| WaitlistConfig {
                admin_token: "0123456789abcdef0123456789abcdef".into(),
                resend_api_key: "re_test_0123456789".into(),
                invite_from: "Passband <invites@passband.test>".into(),
                allowed_origin: "https://passband.test".into(),
                // Port 1: nothing listens. Nothing on this flow mails anybody,
                // and a reachable mock would only hide it if something did.
                resend_url: "http://127.0.0.1:1".into(),
            }),
        };

        let (store, db_url) = common::fresh_store().await;
        let minted = invites::mint().unwrap();
        store
            .insert_invite(&minted.code_hash, default_expiry(), None)
            .await
            .unwrap();

        let warden = Arc::new(
            HttpWarden::new(warden_url, "warden-bearer".into(), Duration::from_secs(5)).unwrap(),
        );
        // The Bifrost client is DERIVED inside `ControlState::new` from
        // `config.bifrost`; there is no second wiring point to disagree with.
        let state = ControlState::new(config, store, warden).unwrap();
        Self {
            app: router(state.clone()),
            state,
            rec,
            invite_code: minted.code,
            db_url,
        }
    }

    /// Drive one whole signup to the pairing page, the way
    /// `signup_provisions_a_tenant_and_hands_back_a_pairing_code` does at
    /// length. For the tests that care about what the flow LEFT BEHIND rather
    /// than about the flow itself.
    async fn complete_signup(&self, label: &str) {
        let (consent_url, cookie) = self.start_signup(label).await;
        self.rec.lock().unwrap().expected_challenge =
            Some(query_param(&consent_url, "code_challenge"));
        let state = state_param(&consent_url);
        let (status, _, body) = self
            .get(
                &format!("/oauth/callback?code=the-auth-code&state={state}"),
                Some(&cookie),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    /// The share-token hash recorded against a tenant, or `None`.
    ///
    /// Read raw, like `analytics_id` above and for a related reason: no method
    /// returns it, because nothing outside the tenant door's own lookup has any
    /// business with a credential's hash.
    async fn share_token_hash(&self, label: &str) -> Option<String> {
        common::raw_client(&self.db_url)
            .await
            .query_one(
                "SELECT share_token_hash FROM tenants WHERE label = $1",
                &[&label],
            )
            .await
            .unwrap()
            .get(0)
    }

    /// Every funnel row, as `(email, account_email, tenant_label, signed_up_at,
    /// analytics_id)`. Read raw because `analytics_id` is deliberately not on
    /// `UserRow`: it must never travel beside an address.
    async fn funnel(
        &self,
    ) -> Vec<(
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    )> {
        common::raw_client(&self.db_url)
            .await
            .query(
                "SELECT email, account_email, tenant_label, signed_up_at, analytics_id
                   FROM users ORDER BY id",
                &[],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4)))
            .collect()
    }

    /// Mint another invite straight into the control store, the way
    /// `squelch-control invite issue` does.
    async fn issue_invite(&self) -> String {
        self.issue_invite_expiring(default_expiry()).await
    }

    /// ...with an expiry of the test's choosing. A past one is a code that ran
    /// out before the test started.
    async fn issue_invite_expiring(&self, expires_at: chrono::DateTime<chrono::Utc>) -> String {
        let minted = invites::mint().unwrap();
        self.state
            .store()
            .insert_invite(&minted.code_hash, expires_at, None)
            .await
            .unwrap();
        minted.code
    }

    /// Whether the code the harness minted is available to be held right now.
    /// The reservation is invisible from the outside, so this is how a test
    /// asserts a failed signup handed it back.
    async fn invite_is_available(&self) -> bool {
        self.state
            .store()
            .find_available_invite(&invites::hash(&self.invite_code), chrono::Utc::now())
            .await
            .unwrap()
            .is_some()
    }

    /// How the store recorded the invite: `(used_by_label, still available)`.
    /// Turn hidden feature `key` on for `label`, as a ticked box on
    /// `/admin/features` would.
    async fn grant_feature(&self, label: &str, key: &str) {
        let store = self.state.store();
        let board = store.feature_board().await.unwrap();
        let id = board
            .tenants
            .iter()
            .find(|t| t.label == label)
            .expect("an active tenant")
            .id;
        store
            .save_features(
                &[id],
                &[key.to_string()],
                &[(id, key.to_string())],
                &[],
                chrono::Utc::now(),
            )
            .await
            .unwrap();
    }

    async fn invite_row(&self) -> (Option<String>, bool) {
        let rows = self.state.store().list_invites().await.unwrap();
        let row = rows.last().expect("the harness minted one").clone();
        (row.used_by_label, self.invite_is_available().await)
    }

    /// The recipient the mock warden minted for `label`.
    fn recipient_of(&self, label: &str) -> String {
        self.rec.lock().unwrap().tenants[label].recipient.clone()
    }

    /// Open a ciphertext with the identity the WARDEN kept for `label`. This is
    /// the assertion the whole per-tenant-key design rests on: if the control
    /// plane sealed to anything other than the recipient call 1 answered with,
    /// this fails.
    fn open_with_tenant_identity(&self, label: &str, armor: &str) -> String {
        let rec = self.rec.lock().unwrap();
        let tenant = rec
            .tenants
            .get(label)
            .expect("the warden minted an identity for this tenant");
        let decryptor =
            age::Decryptor::new(age::armor::ArmoredReader::new(armor.as_bytes())).unwrap();
        let mut reader = decryptor
            .decrypt(std::iter::once(&tenant.identity as &dyn age::Identity))
            .unwrap();
        let mut plaintext = String::new();
        std::io::Read::read_to_string(&mut reader, &mut plaintext).unwrap();
        plaintext
    }

    /// The armor the control plane sent on the nth `PUT .../credentials`.
    fn credential_armor(&self, n: usize) -> (String, String) {
        let rec = self.rec.lock().unwrap();
        let (label, body) = &rec.credential_puts[n];
        (
            label.clone(),
            body["cred_read_ciphertext"]
                .as_str()
                .expect("the credential body carries armor")
                .to_string(),
        )
    }

    async fn get(
        &self,
        uri: &str,
        cookie: Option<&str>,
    ) -> (StatusCode, axum::http::HeaderMap, String) {
        let mut req = Request::builder().method("GET").uri(uri);
        if let Some(c) = cookie {
            req = req.header(header::COOKIE, c);
        }
        self.send(req.body(axum::body::Body::empty()).unwrap())
            .await
    }

    async fn post_form(
        &self,
        uri: &str,
        body: String,
    ) -> (StatusCode, axum::http::HeaderMap, String) {
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(axum::body::Body::from(body))
            .unwrap();
        self.send(req).await
    }

    async fn send(
        &self,
        req: Request<axum::body::Body>,
    ) -> (StatusCode, axum::http::HeaderMap, String) {
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = String::from_utf8_lossy(&to_bytes(resp.into_body(), 1 << 20).await.unwrap())
            .to_string();
        (status, headers, body)
    }

    /// Start a reconnect and return `(consent url, cookie value)`. Takes no
    /// input, exactly like the route: the mailbox comes from Google.
    async fn start_reconnect(&self) -> (String, String) {
        let (status, headers, _) = self.get("/reconnect", None).await;
        assert_eq!(status, StatusCode::FOUND, "reconnect should redirect");
        let location = headers[header::LOCATION].to_str().unwrap().to_string();
        let cookie = headers[header::SET_COOKIE].to_str().unwrap().to_string();
        let cookie = cookie.split(';').next().unwrap().to_string();
        (location, cookie)
    }

    /// Start a reconnect with a query string of the test's choosing, the way
    /// the daemon's `connect_url` and `reconnect_url` arrive.
    async fn start_reconnect_with(&self, query: &str) -> (String, String) {
        let (status, headers, _) = self.get(&format!("/reconnect?{query}"), None).await;
        assert_eq!(status, StatusCode::FOUND, "reconnect should redirect");
        assert_eq!(
            headers.get_all(header::LOCATION).iter().count(),
            1,
            "exactly one Location"
        );
        let location = headers[header::LOCATION].to_str().unwrap().to_string();
        let cookie = headers[header::SET_COOKIE].to_str().unwrap().to_string();
        let cookie = cookie.split(';').next().unwrap().to_string();
        (location, cookie)
    }

    /// Walk a whole reconnect: the hop out, then Google's redirect back.
    async fn run_reconnect(&self) -> (StatusCode, String) {
        self.run_reconnect_with("").await
    }

    /// [`Harness::run_reconnect`] with a query string on `/reconnect`.
    async fn run_reconnect_with(&self, query: &str) -> (StatusCode, String) {
        let (consent, cookie) = if query.is_empty() {
            self.start_reconnect().await
        } else {
            self.start_reconnect_with(query).await
        };
        self.rec.lock().unwrap().expected_challenge = Some(query_param(&consent, "code_challenge"));
        let (status, headers, body) = self
            .get(
                &format!(
                    "/oauth/callback?code=test-code&state={}",
                    state_param(&consent)
                ),
                Some(&cookie),
            )
            .await;
        if status != StatusCode::SEE_OTHER {
            return (status, body);
        }
        let cookie = reconnect_cookie(&headers);
        assert_eq!(headers[header::LOCATION], "/reconnect/status");
        self.wait_for_reconnect(&cookie).await
    }

    /// Poll the status page the way a browser does until it stops refreshing.
    /// No worker pass is run here: the callback handler started one, and its
    /// jobs run detached, which is exactly what this waits out.
    async fn wait_for_reconnect(&self, cookie: &str) -> (StatusCode, String) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let (status, headers, body) = self.get("/reconnect/status", Some(cookie)).await;
                if !headers.contains_key(header::REFRESH) {
                    return (status, body);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("reconnect should finish")
    }

    /// Post the signup form and return `(consent url, cookie value)`.
    async fn start_signup(&self, label: &str) -> (String, String) {
        let code = self.invite_code.clone();
        self.start_signup_with(label, &code).await
    }

    async fn start_signup_with(&self, label: &str, invite: &str) -> (String, String) {
        let (status, headers, _) = self
            .post_form(
                "/signup",
                format!("invite={}&label={label}", urlencode(invite)),
            )
            .await;
        assert_eq!(status, StatusCode::SEE_OTHER, "signup should redirect");
        let location = headers[header::LOCATION].to_str().unwrap().to_string();
        let cookie = headers[header::SET_COOKIE].to_str().unwrap().to_string();
        // Only the cookie pair, as a browser would send it back.
        let cookie = cookie.split(';').next().unwrap().to_string();
        (location, cookie)
    }

    /// Post the signup form with the Microsoft button and return
    /// `(consent url, cookie value)`.
    async fn start_outlook_signup(&self, label: &str) -> (String, String) {
        let (status, headers, body) = self
            .post_form(
                "/signup",
                format!(
                    "invite={}&label={label}&provider=microsoft",
                    urlencode(&self.invite_code)
                ),
            )
            .await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
        let location = headers[header::LOCATION].to_str().unwrap().to_string();
        let cookie = headers[header::SET_COOKIE].to_str().unwrap().to_string();
        (location, cookie.split(';').next().unwrap().to_string())
    }

    /// Walk a whole Outlook signup to its answer.
    async fn run_outlook_signup(&self, label: &str) -> (StatusCode, String) {
        let (consent, cookie) = self.start_outlook_signup(label).await;
        let (status, _, body) = self
            .get(
                &format!(
                    "/oauth/callback?code=the-auth-code&state={}",
                    state_param(&consent)
                ),
                Some(&cookie),
            )
            .await;
        (status, body)
    }

    /// Walk a whole app login with `provider=` (empty for Google).
    async fn run_app_login(&self, provider: &str) -> (StatusCode, String) {
        let uri = if provider.is_empty() {
            "/app/auth".to_string()
        } else {
            format!("/app/auth?provider={provider}")
        };
        let (status, headers, body) = self.get(&uri, None).await;
        assert!(status.is_redirection(), "{status}: {body}");
        let consent = headers[header::LOCATION].to_str().unwrap().to_string();
        let cookie = headers[header::SET_COOKIE].to_str().unwrap().to_string();
        let cookie = cookie.split(';').next().unwrap().to_string();
        let (status, _, body) = self
            .get(
                &format!(
                    "/oauth/callback?code=the-auth-code&state={}",
                    state_param(&consent)
                ),
                Some(&cookie),
            )
            .await;
        (status, body)
    }

    /// Walk one whole signup: form post, consent, callback.
    async fn run_signup(&self, label: &str, invite: &str) -> (StatusCode, String) {
        let (consent, cookie) = self.start_signup_with(label, invite).await;
        let (status, _, body) = self
            .get(
                &format!(
                    "/oauth/callback?code=the-auth-code&state={}",
                    state_param(&consent)
                ),
                Some(&cookie),
            )
            .await;
        (status, body)
    }
}

/// Whether a string is shaped like the UUIDv4 the store mints for
/// `analytics_id`: 8-4-4-4-12 lowercase hex, version nibble 4, variant in
/// `89ab`. The app holds an adopted id to this shape before it will adopt it.
fn is_uuid_shaped(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && parts.iter().map(|p| p.len()).collect::<Vec<_>>() == [8, 4, 4, 4, 12]
        && parts.iter().all(|p| {
            p.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        })
        && parts[2].starts_with('4')
        && matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b')
}

/// What `squelch-control invite issue` would stamp on a code minted now.
fn default_expiry() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() + chrono::Duration::days(invites::DEFAULT_TTL_DAYS)
}

fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// The PKCE S256 transform: base64url, unpadded, of SHA-256 over the verifier.
fn s256(verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn query_param(url: &str, name: &str) -> String {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

fn state_param(consent_url: &str) -> String {
    query_param(consent_url, "state")
}

/// The happy path, asserted at every point the design makes a promise.
#[tokio::test]
async fn signup_provisions_a_tenant_and_hands_back_a_pairing_code() {
    let h = Harness::new().await;

    let (consent_url, cookie) = h.start_signup("ada").await;
    // The consent URL is the mock's authorize endpoint, asking for all three
    // grants in ONE consent: hosted Passband ships the actions, and the daemon's
    // write path has no other way to be filled. The optional name-and-photo
    // scope and both calendar scopes ride along; the mock below grants only
    // the three, which is the unticked-box case, and the signup still
    // completes.
    assert!(consent_url.contains("/authorize"), "{consent_url}");
    let asked = query_param(&consent_url, "scope");
    assert_eq!(
        asked.split(' ').collect::<Vec<_>>(),
        vec![
            MODIFY,
            squelch_core::config::GOOGLE_PROFILE_SCOPE,
            CAL_EVENTS,
            CAL_LIST,
        ],
        "{consent_url}"
    );
    assert!(consent_url.contains("code_challenge_method=S256"));
    // ...and offline access, which is what makes Google return the refresh token
    // this whole flow exists to seal. Asserted here because the scope set and
    // these parameters are now chosen per flow, and the console login asks for
    // neither.
    assert_eq!(query_param(&consent_url, "access_type"), "offline");
    assert_eq!(query_param(&consent_url, "prompt"), "consent");
    // Incremental, so a later grant never loses an earlier one; and signup
    // names no account.
    assert_eq!(query_param(&consent_url, "include_granted_scopes"), "true");
    assert_eq!(query_param(&consent_url, "login_hint"), "");

    // Hold the exchange to the PKCE challenge that rode on THIS consent URL,
    // so the verifier is proven rather than merely present.
    h.rec.lock().unwrap().expected_challenge = Some(query_param(&consent_url, "code_challenge"));

    let state = state_param(&consent_url);
    let (status, headers, body) = h
        .get(
            &format!("/oauth/callback?code=the-auth-code&state={state}"),
            Some(&cookie),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The success page: the mailbox's host and the download. The app's own
    // onboarding pairs, so the warden's code never reaches this page.
    assert!(body.contains("Your mailbox is ready"), "{body}");
    assert!(body.contains("ada.passband.test"), "{body}");
    assert!(body.contains("passband.app/download"), "{body}");
    assert!(!body.contains("ABCD-EFGH"), "{body}");
    assert!(!body.contains("passband://"), "{body}");
    // No token, ever, on a page.
    assert!(!body.contains(REFRESH_TOKEN));
    assert!(!body.contains(ACCESS_TOKEN));
    // The session cookie is cleared on the way out.
    let set_cookie = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect::<Vec<_>>()
        .join("|");
    assert!(set_cookie.contains("Max-Age=0"), "{set_cookie}");

    // THE FUNNEL, THROUGH THE CLI DOOR. The harness's invite is the shape
    // `squelch-control invite issue` mints: a code addressed to nobody, so no
    // row named this person before they redeemed it. One appears at the
    // callback, keyed on the Google account, which is the only identity this
    // door ever learns.
    //
    // BEFORE the recorder lock below, which is held to the end of this test: a
    // std `MutexGuard` may not be alive across an await, and reading the funnel
    // is one.
    let funnel = h.funnel().await;
    assert_eq!(funnel.len(), 1, "one person, one row: {funnel:?}");
    let (email, account, label, signed_up, analytics_id) = &funnel[0];
    assert_eq!(email, MAILBOX, "known by the account that redeemed it");
    assert_eq!(account.as_deref(), Some(MAILBOX));
    assert_eq!(label.as_deref(), Some("ada"));
    assert!(signed_up.is_some(), "stamped at the callback");
    assert!(is_uuid_shaped(analytics_id), "{analytics_id}");

    // ---- what the warden actually received ----
    let (put_label, armor) = h.credential_armor(0);
    let plaintext = h.open_with_tenant_identity("ada", &armor);
    let recipient = h.recipient_of("ada");
    let rec = h.rec.lock().unwrap();

    // Google got the PKCE verifier and our redirect URI, and the exchange was
    // an authorization_code grant.
    let form: std::collections::HashMap<_, _> = rec.token_form.iter().cloned().collect();
    assert_eq!(form.get("grant_type").unwrap(), "authorization_code");
    assert_eq!(form.get("code").unwrap(), "the-auth-code");
    assert!(!form.get("code_verifier").unwrap().is_empty());
    assert_eq!(
        form.get("redirect_uri").unwrap(),
        "https://signup.passband.test/oauth/callback"
    );

    // CALL 1 carries the label and the mailbox and NO credential: at that point
    // the recipient it would be sealed to does not exist yet.
    assert_eq!(rec.create_bodies.len(), 1);
    let created = &rec.create_bodies[0];
    assert_eq!(created["label"], "ada");
    assert_eq!(created["account_email"], MAILBOX);
    assert!(
        created.get("cred_read_ciphertext").is_none(),
        "the credential must not ride on call 1: {created}"
    );

    // CALL 2 carries armor, on the right label, and no token in the clear.
    assert_eq!(rec.credential_puts.len(), 1);
    assert_eq!(put_label, "ada");
    assert!(armor.starts_with(seal::ARMOR_HEADER), "{armor}");
    assert!(!armor.contains(REFRESH_TOKEN));
    assert!(!armor.contains(ACCESS_TOKEN));
    let whole_body = serde_json::to_string(&rec.credential_puts[0].1).unwrap();
    assert!(!whole_body.contains(REFRESH_TOKEN), "{whole_body}");
    assert!(!whole_body.contains(ACCESS_TOKEN), "{whole_body}");
    // What call 1 answered with is a PUBLIC key. The private half never left the
    // mock warden, which is the only reason this test process can decrypt at all.
    assert!(recipient.starts_with("age1"), "{recipient}");

    // THE CENTRAL ASSERTION: the ciphertext opens with the identity the warden
    // minted for THIS tenant (proved by `open_with_tenant_identity` above), and
    // what is inside is the credentials-file SLOT MAP the daemon reads, not a
    // bare token (which would decrypt into an empty map and fail days later).
    //
    // BOTH SLOTS, from the one consent: `email` is what sync and triage load,
    // `email#write` is what the action handlers load. A blob with only the first
    // provisions a mailbox whose Archive and Send buttons fail forever.
    let parsed: Value = serde_json::from_str(&plaintext).unwrap();
    let write_slot = format!("{MAILBOX}#write");
    assert_eq!(
        parsed["slots"].as_object().map(|s| s.len()),
        Some(2),
        "{plaintext}"
    );
    for slot in [MAILBOX, write_slot.as_str()] {
        assert_eq!(
            parsed["slots"][slot]["refresh_token"], REFRESH_TOKEN,
            "{slot}"
        );
        assert_eq!(
            parsed["slots"][slot]["access_token"], ACCESS_TOKEN,
            "{slot}"
        );
    }
    assert!(parsed.get("refresh_token").is_none(), "{plaintext}");

    // ...and it is sealed to EXACTLY that recipient: another key opens nothing.
    let stranger = age::x25519::Identity::generate();
    let decryptor = age::Decryptor::new(age::armor::ArmoredReader::new(armor.as_bytes())).unwrap();
    assert!(
        decryptor
            .decrypt(std::iter::once(&stranger as &dyn age::Identity))
            .is_err()
    );

    // The bearer went on every warden call, and availability was checked before
    // consent.
    assert!(!rec.warden_bearers.is_empty());
    assert!(
        rec.warden_bearers
            .iter()
            .all(|b| b == "Bearer warden-bearer"),
        "{:?}",
        rec.warden_bearers
    );
    assert_eq!(rec.status_lookups, vec!["ada".to_string()]);
}

/// THE OTHER DOOR, and the case that made `account_email` a column: an invite
/// MAILED to one address, redeemed by a Google account that is a different
/// address. A bearer code cannot promise otherwise, and before this the board
/// showed the invited address for a mailbox belonging to somebody else.
///
/// The row is seeded through the store the way the admin page seeds it —
/// `invite_directly`, then `set_user_invite` with the id the mint returned — so
/// the pointer under test is the one production writes.
/// A NEW TENANT CAN SHARE THE DAY IT IS MADE. Nobody runs a command for it, and
/// the person who just arrived through an invite is exactly the one most likely
/// to send another.
///
/// The token is installed BEFORE the workload is applied, so the pod's first
/// render carries it and nothing has to roll a second time to pick it up.
#[tokio::test]
async fn signup_leaves_a_tenant_that_can_already_share() {
    let h = Harness::with_invites().await;
    h.complete_signup("ada").await;

    // The lock is taken and released INSIDE this block: a std `MutexGuard` may
    // not be alive across the await below.
    {
        let rec = h.rec.lock().unwrap();
        assert_eq!(rec.control_token_puts.len(), 1, "one token, once");
        let (label, body) = &rec.control_token_puts[0];
        assert_eq!(label, "ada");
        let token = body["share_token"].as_str().expect("a token was installed");
        assert!(token.starts_with("pbs_"), "{token}");

        // BEFORE the workload: the render that first applies the Deployment has
        // to see the Secret, or the tenant is rolled again a moment later -
        // during the one minute its owner is opening the app with a pairing
        // code.
        assert_eq!(rec.credential_puts.len(), 1);
        assert!(
            rec.warden_bearers.len() >= 2,
            "both calls carried the bearer: {:?}",
            rec.warden_bearers
        );
    }

    // AND THE STORE KNOWS IT: the hash is recorded against the row, so the
    // token the pod holds is one the tenant door will accept.
    let hash = h.share_token_hash("ada").await;
    assert!(hash.is_some(), "the hash joined the tenant row");
    assert_eq!(hash.unwrap().len(), 64);
}

/// A warden that will not take the token costs the tenant its button and
/// NOTHING ELSE. The mailbox is made, the code is spent, the pairing code is on
/// screen; `share mint` backfills the rest.
#[tokio::test]
async fn a_refused_share_token_still_provisions_the_mailbox() {
    let h = Harness::with_invites().await;
    h.rec.lock().unwrap().fail_control_token = true;
    h.complete_signup("ada").await;

    // The mailbox exists and the funnel recorded it.
    let funnel = h.funnel().await;
    assert_eq!(funnel.len(), 1, "{funnel:?}");
    assert_eq!(funnel[0].2.as_deref(), Some("ada"));

    // And the store did NOT record a hash for a token no pod holds: a row that
    // said otherwise would claim a credential nothing can present.
    assert!(
        h.share_token_hash("ada").await.is_none(),
        "an uninstalled token must not be recorded as installed"
    );
}

#[tokio::test]
async fn a_mailed_invite_stamps_the_row_it_was_minted_for() {
    let h = Harness::new().await;
    let store = h.state.store();

    const INVITED_AT: &str = "ada@work.example.com";
    let user_id = store
        .invite_directly(INVITED_AT, chrono::Utc::now())
        .await
        .unwrap()
        .expect("a fresh address is approved by this call");
    let minted = invites::mint().unwrap();
    let invite_id = store
        .insert_invite(&minted.code_hash, default_expiry(), None)
        .await
        .unwrap();
    assert!(
        store
            .set_user_invite(user_id, invite_id, None)
            .await
            .unwrap()
    );
    let before = h.funnel().await;
    assert_eq!(before.len(), 1);
    let analytics_id = before[0].4.clone();

    // The mock Google always answers with MAILBOX, which is NOT the address the
    // invite was addressed to. That is the whole point of this test.
    let (status, body) = h.run_signup("ada", &minted.code).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let funnel = h.funnel().await;
    assert_eq!(
        funnel.len(),
        1,
        "the SAME row was stamped, not a second one: {funnel:?}"
    );
    let (email, account, label, signed_up, id_after) = &funnel[0];
    assert_eq!(
        email, INVITED_AT,
        "still known by the address the invite went to"
    );
    assert_eq!(
        account.as_deref(),
        Some(MAILBOX),
        "and the account that actually signed up is recorded beside it"
    );
    assert_ne!(
        email.as_str(),
        MAILBOX,
        "the mismatch this column exists for"
    );
    assert_eq!(label.as_deref(), Some("ada"));
    assert!(signed_up.is_some());
    assert_eq!(
        id_after, &analytics_id,
        "signing up is not a new person: the id is the one they were minted"
    );
}

/// The emailed link opens a form with a username already in it, made from the
/// address the invite went to: the part before the `@`, or that part and the
/// domain when the first is taken. A suggestion only: the address itself is
/// never on the page, and a shared code (nobody's address) suggests nothing.
#[tokio::test]
async fn the_emailed_link_suggests_a_username_from_the_address() {
    let h = Harness::new().await;
    let store = h.state.store();
    let mint_for = |email: &'static str| async move {
        let user_id = store
            .invite_directly(email, chrono::Utc::now())
            .await
            .unwrap()
            .unwrap();
        let minted = invites::mint().unwrap();
        let invite_id = store
            .insert_invite(&minted.code_hash, default_expiry(), None)
            .await
            .unwrap();
        assert!(
            store
                .set_user_invite(user_id, invite_id, None)
                .await
                .unwrap()
        );
        minted.code
    };
    let username = |page: &str| {
        let (_, field) = page
            .split_once(r#"id="label" name="label" value=""#)
            .expect(page);
        field.split_once('"').unwrap().0.to_string()
    };

    let grace = mint_for("grace.hopper+news@navy.example.com").await;
    let (status, _, page) = h.get(&format!("/?invite={grace}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(username(&page), "grace-hopper");
    assert!(
        !page.contains("navy.example.com"),
        "the address stays off the page"
    );

    // Taken: the domain joins it.
    store
        .insert_tenant("grace-hopper", "someone@else.example")
        .await
        .unwrap();
    let (_, _, page) = h.get(&format!("/?invite={grace}"), None).await;
    assert_eq!(username(&page), "grace-hopper-navy");

    // Both taken: an empty field, the form as it always was.
    store
        .insert_tenant("grace-hopper-navy", "another@else.example")
        .await
        .unwrap();
    let (_, _, page) = h.get(&format!("/?invite={grace}"), None).await;
    assert_eq!(username(&page), "");

    // Asked of the store only: a page view never waits on the warden.
    assert!(
        h.rec.lock().unwrap().status_lookups.is_empty(),
        "the form asked the warden nothing"
    );

    // The harness's own code was minted with no address behind it.
    let (_, _, page) = h.get(&format!("/?invite={}", h.invite_code), None).await;
    assert_eq!(username(&page), "");
}

/// The alerts, end to end through the real handler: a half-finished signup
/// reaches the operator with the label and where to look, the finished retry
/// reaches them too, and neither carries the mailbox. 2026-09-28 was a
/// half-finished signup that nobody heard about until the person said so.
#[tokio::test]
async fn the_operator_hears_about_a_half_finished_signup_and_its_retry() {
    let h = Harness::new().await;
    h.rec.lock().unwrap().fail_credentials = true;
    let invite = h.invite_code.clone();

    let (status, body) = h.run_signup("ada", &invite).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    let alerts = alerts_after(&h.rec, 1).await;
    assert!(alerts[0].contains("Signup half-finished"), "{}", alerts[0]);
    assert!(alerts[0].contains("`ada`"), "{}", alerts[0]);
    assert!(
        alerts[0].contains("kubectl -n tenants get events | grep ada"),
        "{}",
        alerts[0]
    );

    h.rec.lock().unwrap().fail_credentials = false;
    let (status, body) = h.run_signup("ada", &invite).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let alerts = alerts_after(&h.rec, 2).await;
    assert!(
        alerts[1].contains("New tenant provisioned: `ada`"),
        "{}",
        alerts[1]
    );

    for a in &alerts {
        assert!(!a.contains(MAILBOX), "the mailbox reached the webhook: {a}");
        assert!(!a.contains('@'), "an address-shaped token in: {a}");
    }
}

/// A hosted signup names its person in PostHog: one `$set` carrying the
/// mailbox, under the same analytics id the funnel row holds and the app will
/// adopt when it pairs.
#[tokio::test]
async fn a_signup_names_the_person_in_posthog_under_their_analytics_id() {
    let h = Harness::new().await;
    let invite = h.invite_code.clone();
    let (status, body) = h.run_signup("ada", &invite).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let funnel = h.funnel().await;
    let analytics_id = funnel[0].4.clone();

    let mut events = Vec::new();
    for _ in 0..200 {
        events = h.rec.lock().unwrap().posthog.clone();
        if !events.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(events.len(), 1, "{events:?}");
    let e = &events[0];
    assert_eq!(e["event"], "$set");
    assert_eq!(e["distinct_id"], analytics_id.as_str());
    assert_eq!(e["properties"]["$set"]["email"], MAILBOX);
    assert_eq!(e["properties"]["$set"]["hosted"], true);
}

/// The webhook is posted from a spawned task, so a test waits for it rather
/// than racing it. Two seconds is forever for a loopback post.
async fn alerts_after(rec: &Shared, n: usize) -> Vec<String> {
    for _ in 0..200 {
        {
            let r = rec.lock().unwrap();
            if r.alerts.len() >= n {
                return r.alerts.clone();
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("expected {n} alerts, got {:?}", rec.lock().unwrap().alerts);
}

/// A failure on call 2 is the state wire v2 invents, so it gets the most
/// assertions: the address is held, the invite is NOT burned, the control plane
/// records nothing, and the retry lands on the SAME per-tenant key.
#[tokio::test]
async fn a_failed_credential_install_leaves_a_retriable_signup() {
    let h = Harness::new().await;
    h.rec.lock().unwrap().fail_credentials = true;

    let invite = h.invite_code.clone();
    let (status, body) = h.run_signup("ada", &invite).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(body.contains("not finished"), "{body}");
    assert!(body.contains("has not been used"), "{body}");
    // Honest copy: it does NOT claim nothing was set up, because the address is
    // being held.
    assert!(!body.contains("Nothing was set up"), "{body}");

    // Nothing was recorded here: the tenant does not exist as far as this
    // control plane is concerned, so its own label check will not block a retry.
    assert!(!h.state.store().label_exists("ada").await.unwrap());
    // ...and the code was handed back THE MOMENT the provision failed, not left
    // held until the session would have expired ten minutes from now. The page
    // above tells this person to start again with the same code; this is what
    // makes that sentence true.
    assert!(
        h.invite_is_available().await,
        "the failed signup released its hold"
    );
    {
        let rec = h.rec.lock().unwrap();
        assert_eq!(rec.create_bodies.len(), 1);
        assert_eq!(rec.credential_puts.len(), 1, "the install was attempted");
        assert!(!rec.tenants["ada"].provisioned, "and it did not take");
    }

    // THE RETRY, with the same code and the same address. That the form accepts
    // it at all proves two things: the invite was never spent, and a PENDING
    // label passes the pre-consent availability check.
    h.rec.lock().unwrap().fail_credentials = false;
    let (status, body) = h.run_signup("ada", &invite).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Your mailbox is ready"), "{body}");

    // ONE identity for this tenant across both attempts, and both ciphertexts
    // open with it: the retry re-used the same recipient rather than orphaning
    // the first key.
    let (_, first) = h.credential_armor(0);
    let (_, second) = h.credential_armor(1);
    assert_ne!(first, second, "each seal is its own ciphertext");
    for armor in [&first, &second] {
        let plaintext = h.open_with_tenant_identity("ada", armor);
        let parsed: Value = serde_json::from_str(&plaintext).unwrap();
        assert_eq!(parsed["slots"][MAILBOX]["refresh_token"], REFRESH_TOKEN);
        assert_eq!(
            parsed["slots"][format!("{MAILBOX}#write")]["refresh_token"],
            REFRESH_TOKEN
        );
    }

    {
        let rec = h.rec.lock().unwrap();
        assert_eq!(rec.tenants.len(), 1, "no orphaned second tenant");
        assert_eq!(rec.create_bodies.len(), 2, "call 1 ran again");
        assert!(rec.tenants["ada"].provisioned);
    }
    // The invite is spent now, and only now.
    assert!(h.state.store().label_exists("ada").await.unwrap());
    assert_eq!(h.invite_row().await, (Some("ada".to_string()), false));
}

/// THE RACE: one code, two tabs. The second is refused at the form, before it
/// can spend a consent, and the first is unaffected. Without the hold both
/// sessions passed the check and provisioned a tenant each, and only the second
/// consume lost.
#[tokio::test]
async fn one_code_opens_one_signup_at_a_time() {
    let h = Harness::new().await;
    let invite = h.invite_code.clone();

    // Tab one is at Google.
    let (consent, cookie) = h.start_signup("ada").await;

    // Tab two posts the same code. One message, the same one a wrong code gets.
    let (status, _, body) = h
        .post_form(
            "/signup",
            format!("invite={}&label=grace", urlencode(&invite)),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "the form re-renders");
    assert!(body.contains("not usable"), "{body}");
    {
        let rec = h.rec.lock().unwrap();
        assert!(rec.token_form.is_empty(), "tab two reached nothing");
        assert!(rec.create_bodies.is_empty());
    }

    // Tab one finishes normally: being raced cost it nothing.
    let (status, _, body) = h
        .get(
            &format!("/oauth/callback?code=c1&state={}", state_param(&consent)),
            Some(&cookie),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rec = h.rec.lock().unwrap();
    assert_eq!(rec.create_bodies.len(), 1, "one code, one tenant");
    assert_eq!(rec.credential_puts.len(), 1);
}

/// A hold lapses with the session that took it, so a signup abandoned at Google
/// does not strand the code for the person who was sent it.
#[tokio::test]
async fn a_lapsed_hold_does_not_strand_the_code() {
    let h = Harness::new().await;
    let hash = invites::hash(&h.invite_code);

    // Somebody posted the form twenty minutes ago, went to Google, and closed
    // the tab. Their session is long gone; only the row remains.
    let then = chrono::Utc::now() - chrono::Duration::minutes(20);
    h.state
        .store()
        .reserve_invite(
            &hash,
            "an-abandoned-session",
            then,
            then + chrono::Duration::minutes(10),
        )
        .await
        .unwrap()
        .expect("the abandoned signup held it");
    assert!(
        h.invite_is_available().await,
        "and the hold has since lapsed"
    );

    let invite = h.invite_code.clone();
    let (status, body) = h.run_signup("ada", &invite).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.invite_row().await, (Some("ada".to_string()), false));
}

/// A code that ran out is refused exactly the way a wrong one is, and nothing
/// downstream is asked about it.
#[tokio::test]
async fn an_expired_code_is_refused_like_any_other() {
    let h = Harness::new().await;
    let expired = h
        .issue_invite_expiring(chrono::Utc::now() - chrono::Duration::seconds(1))
        .await;

    let (status, _, body) = h
        .post_form(
            "/signup",
            format!("invite={}&label=ada", urlencode(&expired)),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "the form re-renders");
    assert!(body.contains("not usable"), "{body}");

    let rec = h.rec.lock().unwrap();
    assert!(rec.token_form.is_empty(), "nothing reached Google");
    assert!(rec.status_lookups.is_empty(), "nor the warden");
}

/// The shape codes are minted in now: sixteen Crockford symbols in four groups,
/// working end to end through the form.
#[tokio::test]
async fn a_minted_code_signs_up_in_the_shape_it_is_printed() {
    let h = Harness::new().await;
    assert_eq!(h.invite_code.len(), 19, "{}", h.invite_code);
    assert_eq!(h.invite_code.matches('-').count(), 3, "{}", h.invite_code);

    let invite = h.invite_code.clone();
    let (status, body) = h.run_signup("ada", &invite).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Retyped the way a person retypes it: no dashes, wrong case, extra space.
    // Same credential, and it is spent.
    let sloppy = format!(" {} ", invite.replace('-', "").to_lowercase());
    let (status, _, page) = h
        .post_form(
            "/signup",
            format!("invite={}&label=grace", urlencode(&sloppy)),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("not usable"), "{page}");
}

/// A label that goes live while the user is at Google is refused on call 1,
/// after consent and before anything is sealed. The invite survives.
#[tokio::test]
async fn a_label_taken_during_consent_does_not_burn_the_invite() {
    let h = Harness::new().await;
    let (consent, cookie) = h.start_signup("ada").await;
    h.rec.lock().unwrap().taken.push("ada".to_string());

    let (status, _, body) = h
        .get(
            &format!("/oauth/callback?code=c1&state={}", state_param(&consent)),
            Some(&cookie),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("just taken"), "{body}");
    assert!(body.contains("has not been used"), "{body}");
    {
        let rec = h.rec.lock().unwrap();
        assert_eq!(rec.create_bodies.len(), 1, "call 1 was attempted");
        assert!(rec.credential_puts.is_empty(), "and nothing was sealed");
    }

    // The invite still works: the form takes it and sends the user to Google.
    // `start_signup_with` asserts the 303 itself, so reaching the end of this
    // test is the assertion.
    let invite = h.invite_code.clone();
    h.start_signup_with("grace", &invite).await;
}

/// One mailbox, one daemon. The mock Google always names the same mailbox, so
/// a second signup under a different label and a fresh invite is the duplicate
/// case, and it is refused at the callback, after consent and before anything
/// is provisioned.
#[tokio::test]
async fn a_mailbox_gets_one_daemon() {
    let h = Harness::new().await;
    let invite = h.invite_code.clone();
    let (status, body) = h.run_signup("ada", &invite).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let second_code = h.issue_invite().await;
    let (status, body) = h.run_signup("grace", &second_code).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("already"), "{body}");

    let rec = h.rec.lock().unwrap();
    assert_eq!(rec.create_bodies.len(), 1, "only the first was created");
    assert_eq!(rec.credential_puts.len(), 1);
}

/// An invite is spent by the signup that used it, and the code stops working
/// the moment the tenant exists.
#[tokio::test]
async fn an_invite_code_works_once() {
    let h = Harness::new().await;
    let invite = h.invite_code.clone();
    let (status, body) = h.run_signup("ada", &invite).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // THE INVARIANT the hold buys: the session that provisioned held the code
    // the whole way, so the consume at the end cannot have lost a race. If it
    // had, the row would carry no label and this signup would have made a
    // tenant for free.
    assert_eq!(h.invite_row().await, (Some("ada".to_string()), false));

    let (status, _, body) = h
        .post_form(
            "/signup",
            format!("invite={}&label=grace", urlencode(&invite)),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "the form re-renders");
    assert!(body.contains("not usable"), "{body}");
}

/// A 422 from call 2 is the two sides of the wire disagreeing, not something the
/// person who typed the address did. It reads as the generic retriable failure,
/// and NOTHING on the page blames their input.
#[tokio::test]
async fn a_refused_credential_body_is_not_reported_as_an_address_problem() {
    let h = Harness::new().await;
    h.rec.lock().unwrap().credentials_status = Some(422);

    let invite = h.invite_code.clone();
    let (status, body) = h.run_signup("ada", &invite).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(body.contains("not finished"), "{body}");
    assert!(!body.contains("taken"), "{body}");
    assert!(!body.contains("address is already"), "{body}");
    assert!(!body.contains("refused"), "{body}");

    // Retriable means retriable: the code went straight back.
    assert!(h.invite_is_available().await);
    assert_eq!(h.rec.lock().unwrap().credential_puts.len(), 1);
}

/// If the verifier does not match the challenge, Google refuses the exchange
/// and nothing is provisioned. The control plane says so without leaking why.
#[tokio::test]
async fn an_exchange_google_refuses_provisions_nothing() {
    let h = Harness::new().await;
    let (consent, cookie) = h.start_signup("ada").await;
    // A challenge no verifier this process holds can satisfy.
    h.rec.lock().unwrap().expected_challenge = Some("a-challenge-from-another-flow".to_string());

    let (status, _, body) = h
        .get(
            &format!("/oauth/callback?code=c1&state={}", state_param(&consent)),
            Some(&cookie),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(body.contains("did not complete"), "{body}");

    let rec = h.rec.lock().unwrap();
    assert!(!rec.token_form.is_empty(), "the exchange was attempted");
    assert!(rec.create_bodies.is_empty(), "and no tenant was created");
    assert!(rec.credential_puts.is_empty());
}

/// A callback whose `state` does not match the session is refused, and nothing
/// is exchanged or provisioned.
#[tokio::test]
async fn a_state_mismatch_is_refused() {
    let h = Harness::new().await;
    let (_, cookie) = h.start_signup("ada").await;

    let (status, _, body) = h
        .get("/oauth/callback?code=c1&state=not-the-state", Some(&cookie))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("could not be verified"), "{body}");

    let rec = h.rec.lock().unwrap();
    assert!(rec.token_form.is_empty(), "no exchange was attempted");
    assert!(rec.create_bodies.is_empty());
}

/// A tampered cookie fails its MAC and is indistinguishable from no cookie.
#[tokio::test]
async fn a_tampered_cookie_is_refused() {
    let h = Harness::new().await;
    let (consent, cookie) = h.start_signup("ada").await;
    let state = state_param(&consent);

    // Flip the label inside the signed payload, keeping the original MAC.
    let value = cookie.split_once('=').unwrap().1;
    let (payload_b64, mac_b64) = value.split_once('.').unwrap();
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let mut claim: Value = serde_json::from_slice(&engine.decode(payload_b64).unwrap()).unwrap();
    claim["label"] = json!("www");
    let forged = format!(
        "passband_signup={}.{}",
        engine.encode(serde_json::to_vec(&claim).unwrap()),
        mac_b64
    );

    let (status, _, body) = h
        .get(
            &format!("/oauth/callback?code=c1&state={state}"),
            Some(&forged),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("could not be verified"), "{body}");

    // ...and so is no cookie at all: one message for both.
    let (status_no_cookie, _, body_no_cookie) = h
        .get(&format!("/oauth/callback?code=c1&state={state}"), None)
        .await;
    assert_eq!(status_no_cookie, status);
    assert_eq!(body_no_cookie, body);

    let rec = h.rec.lock().unwrap();
    assert!(rec.token_form.is_empty());
    assert!(rec.create_bodies.is_empty());
}

/// A callback is one-shot: replaying the same code and cookie gets the same
/// refusal a stranger would get.
#[tokio::test]
async fn a_replayed_callback_finds_nothing() {
    let h = Harness::new().await;
    let (consent, cookie) = h.start_signup("ada").await;
    let state = state_param(&consent);
    let uri = format!("/oauth/callback?code=c1&state={state}");

    let (first, _, _) = h.get(&uri, Some(&cookie)).await;
    assert_eq!(first, StatusCode::OK);
    let (second, _, body) = h.get(&uri, Some(&cookie)).await;
    assert_eq!(second, StatusCode::BAD_REQUEST);
    assert!(body.contains("could not be verified"), "{body}");

    assert_eq!(h.rec.lock().unwrap().credential_puts.len(), 1);
}

/// A label the warden already serves is refused BEFORE the user is sent to
/// Google, so nobody spends a consent on an address they cannot have.
#[tokio::test]
async fn a_taken_label_is_refused_before_consent() {
    let h = Harness::new().await;
    h.rec.lock().unwrap().taken.push("ada".to_string());

    let (status, _, body) = h
        .post_form(
            "/signup",
            format!("invite={}&label=ada", urlencode(&h.invite_code)),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("already taken"), "{body}");
    assert!(h.rec.lock().unwrap().token_form.is_empty());
}

/// Invalid labels and unusable invite codes never reach Google either, and the
/// invite refusal says the same thing whatever was wrong with the code.
#[tokio::test]
async fn the_form_refuses_bad_input_without_leaving_the_page() {
    let h = Harness::new().await;

    for (body, needle) in [
        (
            format!("invite={}&label=ab", urlencode(&h.invite_code)),
            "too short",
        ),
        (
            format!("invite={}&label=www", urlencode(&h.invite_code)),
            "reserved",
        ),
        (
            format!("invite={}&label=-ada", urlencode(&h.invite_code)),
            "hyphen",
        ),
        (
            format!("invite={}&label=ada..x", urlencode(&h.invite_code)),
            "lowercase letters",
        ),
        ("invite=ZZZZ-ZZZZ&label=ada".to_string(), "not usable"),
        ("invite=nonsense&label=ada".to_string(), "not usable"),
        ("invite=&label=ada".to_string(), "not usable"),
    ] {
        let (status, _, page) = h.post_form("/signup", body.clone()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(page.contains(needle), "{body} -> {page}");
    }

    let rec = h.rec.lock().unwrap();
    assert!(rec.token_form.is_empty(), "nothing reached Google");
    assert!(rec.create_bodies.is_empty());
}

/// A consent that left the Gmail box unchecked provisions NOTHING, including a
/// LEGACY read/send grant without modify. The answer is the same page, the invite is handed back, and
/// no half-capable tenant is created.
#[tokio::test]
async fn a_partial_consent_provisions_nothing_and_says_so() {
    for partial in [
        READONLY.to_string(),
        SEND.to_string(),
        format!("{READONLY} {SEND}"),
        format!("{CAL_EVENTS} {CAL_LIST}"),
        String::new(),
    ] {
        let h = Harness::new().await;
        h.rec.lock().unwrap().granted_scope = Some(partial.clone());

        let invite = h.invite_code.clone();
        let (status, body) = h.run_signup("ada", &invite).await;
        assert_eq!(status, StatusCode::OK, "{partial:?} -> {body}");
        // ONE wording, whichever box it was, and it names what is needed.
        assert!(body.contains("needs its Gmail permission"), "{body}");
        assert!(body.contains("has not been used"), "{body}");
        assert!(body.contains("Nothing was set up"), "{body}");

        // The exchange happened; nothing past it did.
        {
            let rec = h.rec.lock().unwrap();
            assert!(!rec.token_form.is_empty(), "the exchange was attempted");
            assert!(rec.create_bodies.is_empty(), "no tenant was created");
            assert!(rec.credential_puts.is_empty(), "nothing was sealed");
        }
        assert!(!h.state.store().label_exists("ada").await.unwrap());
        // ...and the code is spendable again, which is what "start again" means.
        assert!(h.invite_is_available().await, "{partial:?}");
    }
}

/// The union case, on the wire: a Google account that has granted this project
/// more than signup asks for reports MORE, and that is a pass, not a refusal.
#[tokio::test]
async fn a_wider_grant_than_we_asked_for_is_accepted() {
    let h = Harness::new().await;
    h.rec.lock().unwrap().granted_scope = Some(format!(
        "{} https://www.googleapis.com/auth/userinfo.email",
        full_grant()
    ));

    let invite = h.invite_code.clone();
    let (status, body) = h.run_signup("ada", &invite).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Your mailbox is ready"), "{body}");
}

/// SIGNUP CARRIES CALENDAR EVEN WITH THE FEATURE OFF. A REGRESSION GUARD, not
/// proof of the request (the consent-URL assertions above are that): it fails
/// if someone later adds a feature gate to signup's install. A new tenant has no
/// `calendar` feature row, and a signup that granted calendar still provisions
/// and seals the whole grant: the reconnect pipeline's feature gate is for a
/// calendar-only consent, not for signup. The daemon holds the scope unused
/// until the feature turns on, and then needs no second trip through Google.
#[tokio::test]
async fn a_signup_that_grants_calendar_keeps_it_with_the_feature_off() {
    let h = Harness::new().await;
    h.rec.lock().unwrap().granted_scope = Some(calendar_grant());

    let invite = h.invite_code.clone();
    let (status, body) = h.run_signup("ada", &invite).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Your mailbox is ready"), "{body}");

    let slots = last_installed_slots(&h, "ada");
    let read = &slots["slots"][MAILBOX];
    assert_eq!(read, &slots["slots"][format!("{MAILBOX}#write")]);
    let recorded: Vec<&str> = read["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(recorded, vec![MODIFY, CAL_EVENTS, CAL_LIST]);
    // One token, no separate calendar slot: the same shape a calendar
    // reconnect installs.
    assert!(slots["slots"].get(format!("{MAILBOX}#calendar")).is_none());
}

/// With the Bifrost trio configured, signup mints TWO virtual keys per tenant
/// — triage and assistant, each with its own name, budget, and model list —
/// hands both VALUES to the warden in one PUT, and keeps only the IDS. The
/// values never reach the page or the store.
#[tokio::test]
async fn signup_mints_and_installs_a_tenant_llm_key() {
    let h = Harness::with_bifrost(Bifrost::Mock).await;
    let invite = h.invite_code.clone();
    let (status, body) = h.run_signup("ada", &invite).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !body.contains(&vk_value_for("tenant-ada")),
        "the key value must never reach a page"
    );
    assert!(
        !body.contains(&vk_value_for("tenant-ada-assistant")),
        "the assistant key value must never reach a page"
    );

    {
        let rec = h.rec.lock().unwrap();
        // SEVEN governance requests — a provider-key listing then a mint,
        // twice, plus the triage key's signup allowance (listing, override,
        // read-back) — all carrying HTTP Basic with the admin
        // `username:password`.
        assert_eq!(rec.bifrost_auths, vec![bifrost_basic_auth(); 7]);
        // THE SIGNUP ALLOWANCE: one more month's limit, one cycle, on the
        // TRIAGE key's budget only. The assistant key is the person's own
        // on-demand spend and gets nothing extra.
        assert_eq!(
            rec.bifrost_overrides,
            vec![(
                vk_id_for("tenant-ada"),
                format!("b-{}", vk_id_for("tenant-ada")),
                json!({ "amount": DEFAULT_LLM_BUDGET_USD, "mode": "cycles", "cycles": 1 }),
            )]
        );
        assert_eq!(rec.bifrost_mint_bodies.len(), 2);
        let mint = &rec.bifrost_mint_bodies[0];
        assert_eq!(mint["name"], "tenant-ada");
        // `budgets` is an ARRAY on the live gateway; a singular `budget` object
        // is silently ignored and mints an unbudgeted key.
        assert_eq!(mint["budgets"].as_array().unwrap().len(), 1);
        assert_eq!(mint["budgets"][0]["max_limit"], DEFAULT_LLM_BUDGET_USD);
        assert_eq!(mint["budgets"][0]["reset_duration"], "1M");
        assert_eq!(mint["is_active"], true);
        // The provider config pins the listed key ids and a non-empty model
        // allow-list; without both the key cannot serve inference.
        let pc = &mint["provider_configs"][0];
        assert_eq!(pc["provider"], "anthropic");
        assert_eq!(pc["key_ids"], json!([PROVIDER_KEY_ID]));
        assert_eq!(
            pc["allowed_models"],
            json!(["claude-haiku-4-5", "claude-sonnet-5"])
        );
        // The SECOND mint is the assistant's: its own name, its own budget, its
        // own model list.
        let mint = &rec.bifrost_mint_bodies[1];
        assert_eq!(mint["name"], "tenant-ada-assistant");
        assert_eq!(mint["budgets"].as_array().unwrap().len(), 1);
        assert_eq!(
            mint["budgets"][0]["max_limit"],
            DEFAULT_ASSISTANT_BUDGET_USD
        );
        assert_eq!(mint["budgets"][0]["reset_duration"], "1M");
        let pc = &mint["provider_configs"][0];
        assert_eq!(pc["provider"], "anthropic");
        assert_eq!(pc["key_ids"], json!([PROVIDER_KEY_ID]));
        assert_eq!(
            pc["allowed_models"],
            json!(["claude-haiku-4-5", "claude-opus-4-8"])
        );
        // BOTH values went to the warden, in ONE put, for this tenant.
        assert_eq!(rec.llm_key_puts.len(), 1);
        let (label, put) = &rec.llm_key_puts[0];
        assert_eq!(label, "ada");
        assert_eq!(put["api_key"], vk_value_for("tenant-ada"));
        assert_eq!(
            put["assistant_api_key"],
            vk_value_for("tenant-ada-assistant")
        );
    }
    // The store kept the IDS, and only the IDS.
    assert_eq!(
        h.state.store().tenant_vk("ada").await.unwrap(),
        Some(vk_id_for("tenant-ada"))
    );
    assert_eq!(
        h.state.store().tenant_assistant_vk("ada").await.unwrap(),
        Some(vk_id_for("tenant-ada-assistant"))
    );
}

/// A gateway that refuses the signup allowance costs the tenant the allowance
/// and nothing else: both keys are still installed and recorded, and the
/// signup completes. `llm topup` adds the allowance by hand.
#[tokio::test]
async fn a_refused_signup_allowance_still_installs_both_keys() {
    let h = Harness::with_bifrost(Bifrost::Mock).await;
    h.rec.lock().unwrap().bifrost_fail_override = true;

    let invite = h.invite_code.clone();
    let (status, body) = h.run_signup("ada", &invite).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Your mailbox is ready"), "{body}");

    {
        let rec = h.rec.lock().unwrap();
        assert_eq!(
            rec.bifrost_overrides.len(),
            1,
            "the allowance was asked for"
        );
        assert_eq!(rec.llm_key_puts.len(), 1);
        let (_, put) = &rec.llm_key_puts[0];
        assert_eq!(put["api_key"], vk_value_for("tenant-ada"));
        assert_eq!(
            put["assistant_api_key"],
            vk_value_for("tenant-ada-assistant")
        );
    }
    assert_eq!(
        h.state.store().tenant_vk("ada").await.unwrap(),
        Some(vk_id_for("tenant-ada"))
    );
}

/// THE ORPHAN PROMISE: Bifrost minted the keys but the warden would not take
/// them. The signup still completes — fail-soft, exactly like an outage — AND
/// the vk ids are still recorded on the tenant row, because a key that is live
/// in Bifrost with no record is one no operator can ever find to revoke.
#[tokio::test]
async fn a_failed_key_install_still_records_the_vk_id() {
    let h = Harness::with_bifrost(Bifrost::Mock).await;
    h.rec.lock().unwrap().fail_llm_key = true;

    let invite = h.invite_code.clone();
    let (status, body) = h.run_signup("ada", &invite).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Your mailbox is ready"), "{body}");
    assert!(
        !body.contains(&vk_value_for("tenant-ada")),
        "the key value must never reach a page"
    );

    {
        let rec = h.rec.lock().unwrap();
        assert_eq!(rec.bifrost_mint_bodies.len(), 2, "both mints happened");
        assert_eq!(rec.llm_key_puts.len(), 1, "the install was attempted");
        assert_eq!(
            rec.credential_puts.len(),
            1,
            "the mailbox still provisioned"
        );
    }
    // The ids are on the row for the manual `llm revoke` or `llm mint` the log
    // line points the operator at.
    assert_eq!(
        h.state.store().tenant_vk("ada").await.unwrap(),
        Some(vk_id_for("tenant-ada"))
    );
    assert_eq!(
        h.state.store().tenant_assistant_vk("ada").await.unwrap(),
        Some(vk_id_for("tenant-ada-assistant"))
    );
    // ...and the signup was a real one: the invite is spent.
    assert_eq!(h.invite_row().await, (Some("ada".to_string()), false));
}

/// THE FAIL-SOFT PROMISE: a Bifrost outage costs the tenant its LLM keys,
/// never the signup. Triage is not mail custody, and `llm mint` backfills.
#[tokio::test]
async fn a_bifrost_outage_does_not_refuse_a_signup() {
    let h = Harness::with_bifrost(Bifrost::Down).await;
    let invite = h.invite_code.clone();
    let (status, body) = h.run_signup("ada", &invite).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Your mailbox is ready"), "{body}");

    {
        let rec = h.rec.lock().unwrap();
        assert!(rec.llm_key_puts.is_empty(), "no key existed to install");
        assert_eq!(
            rec.credential_puts.len(),
            1,
            "the mailbox still provisioned"
        );
    }
    // Nothing was minted, so nothing is recorded: both pointers stay empty
    // rather than naming keys that do not exist.
    assert_eq!(h.state.store().tenant_vk("ada").await.unwrap(), None);
    assert_eq!(
        h.state.store().tenant_assistant_vk("ada").await.unwrap(),
        None
    );
    // ...and the signup was a real one: the invite is spent.
    assert_eq!(h.invite_row().await, (Some("ada".to_string()), false));
}

/// THE INDEPENDENCE PROMISE, one way: the assistant mint fails, and the tenant
/// still signs up WITH its triage key installed. The one warden PUT carries
/// only the key that exists — no `assistant_api_key` slot at all, not a null —
/// and no assistant id is recorded, so nothing points at a key that was never
/// minted.
#[tokio::test]
async fn an_assistant_mint_failure_still_installs_the_triage_key() {
    let h = Harness::with_bifrost(Bifrost::Mock).await;
    h.rec
        .lock()
        .unwrap()
        .bifrost_fail_names
        .push("tenant-ada-assistant".to_string());

    let invite = h.invite_code.clone();
    let (status, body) = h.run_signup("ada", &invite).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Your mailbox is ready"), "{body}");

    {
        let rec = h.rec.lock().unwrap();
        assert_eq!(
            rec.bifrost_mint_bodies.len(),
            2,
            "both mints were attempted"
        );
        assert_eq!(rec.llm_key_puts.len(), 1, "one PUT, with what succeeded");
        let (label, put) = &rec.llm_key_puts[0];
        assert_eq!(label, "ada");
        assert_eq!(put["api_key"], vk_value_for("tenant-ada"));
        assert!(
            put.get("assistant_api_key").is_none(),
            "the missing key must be absent, not null: {put}"
        );
    }
    assert_eq!(
        h.state.store().tenant_vk("ada").await.unwrap(),
        Some(vk_id_for("tenant-ada"))
    );
    assert_eq!(
        h.state.store().tenant_assistant_vk("ada").await.unwrap(),
        None
    );
    assert_eq!(h.invite_row().await, (Some("ada".to_string()), false));
}

/// ...and the other way: the triage mint fails, and the assistant key is still
/// minted, installed, and recorded. The PUT carries only `assistant_api_key`.
#[tokio::test]
async fn a_triage_mint_failure_still_installs_the_assistant_key() {
    let h = Harness::with_bifrost(Bifrost::Mock).await;
    h.rec
        .lock()
        .unwrap()
        .bifrost_fail_names
        .push("tenant-ada".to_string());

    let invite = h.invite_code.clone();
    let (status, body) = h.run_signup("ada", &invite).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Your mailbox is ready"), "{body}");

    {
        let rec = h.rec.lock().unwrap();
        assert_eq!(
            rec.bifrost_mint_bodies.len(),
            2,
            "both mints were attempted"
        );
        assert_eq!(rec.llm_key_puts.len(), 1, "one PUT, with what succeeded");
        let (label, put) = &rec.llm_key_puts[0];
        assert_eq!(label, "ada");
        assert!(
            put.get("api_key").is_none(),
            "the missing key must be absent, not null: {put}"
        );
        assert_eq!(
            put["assistant_api_key"],
            vk_value_for("tenant-ada-assistant")
        );
    }
    assert_eq!(h.state.store().tenant_vk("ada").await.unwrap(), None);
    assert_eq!(
        h.state.store().tenant_assistant_vk("ada").await.unwrap(),
        Some(vk_id_for("tenant-ada-assistant"))
    );
    assert_eq!(h.invite_row().await, (Some("ada".to_string()), false));
}

/// The form and the health check are the only things reachable without a
/// session, and the form states the grants it is about to ask for.
#[tokio::test]
async fn the_landing_page_states_the_grant() {
    let h = Harness::new().await;
    let (status, _, body) = h.get("/", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("gmail.modify"), "{body}");
    assert!(!body.contains("gmail.readonly"), "{body}");
    assert!(!body.contains("gmail.send"), "{body}");
    assert!(body.contains(".passband.test"), "{body}");

    let (status, _, body) = h.get("/healthz", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok");
}

/// The activation poller end to end against the mock warden: a paired tenant
/// is stamped ONCE with the daemon's own timestamp, a mid-roll tenant (503)
/// is left for the next pass, and a stamped user leaves the candidate set
/// forever — the follow-up pass stamps nobody and moves nothing.
#[tokio::test]
async fn the_activation_poller_stamps_once_and_quiesces() {
    let h = Harness::new().await;

    // Two signed-up people, seeded the way record_signup's CLI door works:
    // no invite pointer, keyed on the Google account that redeemed.
    let now = chrono::Utc::now();
    h.state
        .store()
        .insert_tenant("ada", "ada@example.com")
        .await
        .unwrap();
    h.state
        .store()
        .insert_tenant("bea", "bea@example.com")
        .await
        .unwrap();
    h.state
        .store()
        .record_signup(9001, "ada@example.com", "ada", now)
        .await
        .unwrap();
    h.state
        .store()
        .record_signup(9002, "bea@example.com", "bea", now)
        .await
        .unwrap();

    // ada's daemon reports a pairing at ITS OWN timestamp; bea's pod is
    // mid-roll and answers 503.
    let paired_at = "2026-08-20T09:30:00Z";
    {
        let mut r = h.rec.lock().unwrap();
        r.devices_paired
            .insert("ada".into(), Some(paired_at.into()));
        r.devices_unavailable.push("bea".into());
    }
    assert_eq!(activation::poll_first_paired(&h.state).await, 1);

    let stamps = |rows: Vec<tokio_postgres::Row>| -> Vec<(Option<String>, Option<String>)> {
        rows.iter()
            .map(|r| (r.get("tenant_label"), r.get("first_paired_at")))
            .collect()
    };
    let read = || async {
        stamps(
            common::raw_client(&h.db_url)
                .await
                .query(
                    "SELECT tenant_label, first_paired_at FROM users ORDER BY id",
                    &[],
                )
                .await
                .unwrap(),
        )
    };
    let after_first = read().await;
    assert_eq!(after_first.len(), 2);
    let ada_stamp = after_first[0].1.clone().expect("ada stamped");
    // The DAEMON's instant survives re-serialization; the poller never
    // substitutes its own clock.
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(&ada_stamp).unwrap(),
        chrono::DateTime::parse_from_rfc3339(paired_at).unwrap()
    );
    assert_eq!(after_first[1], ("bea".to_string().into(), None));

    // Next pass: ada has left the candidate set, and bea's warden now answers
    // the null body — asked again, still honestly unstamped.
    {
        let mut r = h.rec.lock().unwrap();
        r.devices_unavailable.clear();
        r.devices_paired.insert("bea".into(), None);
    }
    assert_eq!(activation::poll_first_paired(&h.state).await, 0);
    assert_eq!(read().await, after_first);

    // bea finally pairs; only bea is stamped, and ada's first stamp stands.
    h.rec
        .lock()
        .unwrap()
        .devices_paired
        .insert("bea".into(), Some("2026-08-21T00:00:00Z".into()));
    assert_eq!(activation::poll_first_paired(&h.state).await, 1);
    let after_third = read().await;
    assert_eq!(after_third[0].1.as_deref(), Some(ada_stamp.as_str()));
    assert!(after_third[1].1.is_some());
}

/// The retention poller end to end against the mock warden: a daemon that
/// answers is recorded with its own dates, one too old to have the command is
/// left NOT KNOWN, and both are left alone until the refresh interval passes.
#[tokio::test]
async fn the_retention_poller_records_activity_and_waits_out_the_interval() {
    let h = Harness::new().await;
    let now = chrono::Utc::now();
    for (i, label) in ["ada", "bea"].iter().enumerate() {
        let email = format!("{label}@example.com");
        h.state.store().insert_tenant(label, &email).await.unwrap();
        h.state
            .store()
            .record_signup(9100 + i as i64, &email, label, now)
            .await
            .unwrap();
    }
    {
        let mut r = h.rec.lock().unwrap();
        r.activity.insert(
            "ada".into(),
            json!({
                "first_paired_at": "2026-08-20T09:30:00Z",
                "platforms": {
                    "ios": {"first_paired_at": "2026-08-21T10:00:00Z",
                            "last_seen_at": "2026-08-25T09:00:00Z",
                            "last_action_at": "2026-08-24T08:00:00Z"},
                    "macos": {"first_paired_at": "2026-08-20T09:30:00Z",
                              "last_seen_at": "2026-08-25T10:00:00Z",
                              "last_action_at": null}
                }
            }),
        );
        r.activity.insert("bea".into(), Value::Null);
    }
    assert_eq!(retention::poll_activity(&h.state).await, 1);

    let read = || async {
        common::raw_client(&h.db_url)
            .await
            .query(
                "SELECT tenant_label, last_seen_at, last_action_at, mac_paired_at,
                        ios_paired_at, activity_read_at, activity_tried_at
                   FROM users ORDER BY id",
                &[],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| {
                (0..7)
                    .map(|i| r.get::<_, Option<String>>(i))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let same = |a: &Option<String>, b: &str| {
        chrono::DateTime::parse_from_rfc3339(a.as_deref().expect("stamped")).unwrap()
            == chrono::DateTime::parse_from_rfc3339(b).unwrap()
    };
    let rows = read().await;
    let ada = &rows[0];
    // The DAEMON's dates: newest seen and action across apps, each app's
    // first pairing.
    assert!(same(&ada[1], "2026-08-25T10:00:00Z"));
    assert!(same(&ada[2], "2026-08-24T08:00:00Z"));
    assert!(same(&ada[3], "2026-08-20T09:30:00Z"));
    assert!(same(&ada[4], "2026-08-21T10:00:00Z"));
    assert!(ada[5].is_some() && ada[6].is_some());
    // bea's daemon predates the command: asked, and nothing known.
    let bea = &rows[1];
    assert_eq!(&bea[1..6], &[None, None, None, None, None]);
    assert!(bea[6].is_some(), "the attempt is stamped");

    // Next pass, inside the interval: nobody is asked again.
    let asks = h.rec.lock().unwrap().activity_asks.len();
    assert_eq!(retention::poll_activity(&h.state).await, 0);
    assert_eq!(h.rec.lock().unwrap().activity_asks.len(), asks);
    assert_eq!(read().await, rows);
}

// --- reconnect ---------------------------------------------------------------
fn reconnect_cookie(headers: &HeaderMap) -> String {
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("pb_reconnect="))
        .expect("reconnect progress cookie")
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

async fn enqueue_test_reconnect(h: &Harness, token: &str) {
    let ciphertext = signup_ciphertext(h);
    enqueue_ciphertext(h, token, &ciphertext).await;
}

/// The sealed blob signup installed: a real age armor the mock will accept.
fn signup_ciphertext(h: &Harness) -> String {
    let r = h.rec.lock().unwrap();
    str_field(&r.credential_puts[0].1, "cred_read_ciphertext")
}

async fn enqueue_ciphertext(h: &Harness, token: &str, ciphertext: &str) {
    squelch_control::reconnect::enqueue(&h.state, token, "ada", MAILBOX, ciphertext)
        .await
        .unwrap();
}

/// One worker pass, and every job it started run to its end. What the ticker
/// does minus the detaching: a test wants the outcome, not the concurrency.
async fn run_workers(state: &ControlState) {
    for job in squelch_control::reconnect::run_pending(state).await {
        job.await.unwrap();
    }
}

async fn reconnect_page(h: &Harness, token: &str) -> (HeaderMap, String) {
    let (_, headers, body) = h
        .get("/reconnect/status", Some(&format!("pb_reconnect={token}")))
        .await;
    (headers, body)
}

/// The `purpose` of every job row, oldest first.
async fn job_purposes(h: &Harness) -> Vec<String> {
    common::raw_client(&h.db_url)
        .await
        .query(
            "SELECT purpose FROM reconnect_jobs ORDER BY created_at",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect()
}

/// `(status, ciphertext, attempts)` of the one job row.
async fn job_row(h: &Harness) -> (String, Option<String>, i32) {
    let row = common::raw_client(&h.db_url)
        .await
        .query_one(
            "SELECT status, ciphertext, attempts FROM reconnect_jobs",
            &[],
        )
        .await
        .unwrap();
    (row.get(0), row.get(1), row.get(2))
}

async fn make_due(h: &Harness) {
    common::raw_client(&h.db_url)
        .await
        .execute("UPDATE reconnect_jobs SET retry_at = now()", &[])
        .await
        .unwrap();
}

#[tokio::test]
async fn reconnect_progress_survives_a_new_control_process_and_refresh() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    let token = "a".repeat(43);
    enqueue_test_reconnect(&h, &token).await;
    let (headers, body) = reconnect_page(&h, &token).await;
    assert_eq!(headers[header::REFRESH], "5; url=/reconnect/status");
    assert!(body.contains("Reconnecting your mailbox"));
    assert!(body.contains("You can close this page"));
    assert_eq!(h.rec.lock().unwrap().credential_puts.len(), 1);

    // Only the database survives into this fresh state; no pending task does.
    let config = h.state.config().clone();
    let warden = HttpWarden::new(
        config.warden_url.clone(),
        config.warden_token.clone(),
        Duration::from_secs(2),
    )
    .unwrap();
    let fresh = ControlState::new(
        config,
        squelch_control::ControlStore::connect(&h.db_url)
            .await
            .unwrap(),
        Arc::new(warden),
    )
    .unwrap();
    run_workers(&fresh).await;
    for _ in 0..2 {
        let (headers, body) = reconnect_page(&h, &token).await;
        assert!(!headers.contains_key(header::REFRESH));
        assert!(body.contains("You’re reconnected"));
        assert!(body.contains("Open Passband"));
        assert!(!body.contains(REFRESH_TOKEN));
    }
    let (status, ciphertext, _) = job_row(&h).await;
    assert_eq!(status, "complete");
    assert!(
        ciphertext.is_none(),
        "finished work must discard ciphertext"
    );
    assert_eq!(h.rec.lock().unwrap().credential_puts.len(), 2);
}

#[tokio::test]
async fn reconnect_retries_a_lost_result_without_another_google_grant() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    let token = "b".repeat(43);
    enqueue_test_reconnect(&h, &token).await;
    h.rec.lock().unwrap().reconnect_status_after_install = Some(503);
    run_workers(&h.state).await;
    let (headers, body) = reconnect_page(&h, &token).await;
    assert!(headers.contains_key(header::REFRESH));
    assert!(body.contains("taking longer than usual"));
    assert!(!body.contains("Nothing changed"));
    assert!(!body.contains("Try again"));
    assert!(!body.contains("You’re reconnected"));
    // Repeated browser reads never re-run the credential install, and neither
    // does a pass that arrives before the backoff is up.
    run_workers(&h.state).await;
    assert_eq!(h.rec.lock().unwrap().credential_puts.len(), 2);
    h.rec.lock().unwrap().reconnect_status_after_install = None;
    make_due(&h).await;
    run_workers(&h.state).await;
    let r = h.rec.lock().unwrap();
    assert_eq!(r.credential_puts.len(), 3);
    assert_eq!(
        r.credential_puts[1], r.credential_puts[2],
        "retry the identical ciphertext"
    );
    assert_eq!(r.credential_puts[2].1["pair"], false);
}

/// Two consents carrying the same blob share one job, and two passes racing
/// for it install it once.
#[tokio::test]
async fn reconnect_coalesces_consent_and_serializes_workers() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    enqueue_test_reconnect(&h, &"c".repeat(43)).await;
    enqueue_test_reconnect(&h, &"d".repeat(43)).await;
    let (a, b) = tokio::join!(
        squelch_control::reconnect::run_pending(&h.state),
        squelch_control::reconnect::run_pending(&h.state)
    );
    for job in a.into_iter().chain(b) {
        job.await.unwrap();
    }
    assert_eq!(h.rec.lock().unwrap().credential_puts.len(), 2);
    for token in ["c".repeat(43), "d".repeat(43)] {
        let (_, body) = reconnect_page(&h, &token).await;
        assert!(body.contains("You’re reconnected"));
    }
}

/// THE NEWEST CONSENT WINS, even over an install already in flight. Somebody
/// impatient with a stuck job revokes Passband at Google - which kills every
/// earlier refresh token - and re-consents; the blob that arrives second is
/// the only live one, and the row must end up installing THAT, not confirming
/// the dead one and reporting success.
#[tokio::test]
async fn a_second_consent_replaces_an_install_that_is_still_in_flight() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    let first = signup_ciphertext(&h);
    let second = format!("{}\nsecond-consent", squelch_control::seal::ARMOR_HEADER);
    enqueue_ciphertext(&h, &"g".repeat(43), &first).await;

    // The first install is on the wire when the second consent lands.
    h.rec.lock().unwrap().reconnect_delay = Some(Duration::from_millis(300));
    let in_flight = squelch_control::reconnect::run_pending(&h.state).await;
    assert_eq!(in_flight.len(), 1, "the pass must have claimed the job");
    tokio::time::sleep(Duration::from_millis(50)).await;
    enqueue_ciphertext(&h, &"h".repeat(43), &second).await;
    for job in in_flight {
        job.await.unwrap();
    }

    // The first install "succeeded", and that success was about the wrong
    // blob: the row is still pending, holding the second one.
    let (status, ciphertext, attempts) = job_row(&h).await;
    assert_eq!(status, "pending");
    assert_eq!(ciphertext.as_deref(), Some(second.as_str()));
    assert_eq!(attempts, 0, "a new consent starts its own attempt count");
    let (headers, body) = reconnect_page(&h, &"g".repeat(43)).await;
    assert!(headers.contains_key(header::REFRESH));
    assert!(!body.contains("You’re reconnected"));

    h.rec.lock().unwrap().reconnect_delay = None;
    run_workers(&h.state).await;
    let (status, ciphertext, _) = job_row(&h).await;
    assert_eq!(status, "complete");
    assert!(ciphertext.is_none());
    let installed: Vec<String> = {
        let r = h.rec.lock().unwrap();
        r.credential_puts[1..]
            .iter()
            .map(|(_, body)| str_field(body, "cred_read_ciphertext"))
            .collect()
    };
    assert_eq!(installed, [first, second], "both blobs, newest last");
    for token in ["g".repeat(43), "h".repeat(43)] {
        let (_, body) = reconnect_page(&h, &token).await;
        assert!(body.contains("You’re reconnected"));
    }
}

#[tokio::test]
async fn reconnect_status_requires_its_private_unexpired_cookie() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    let token = "e".repeat(43);
    enqueue_test_reconnect(&h, &token).await;
    for cookie in [None, Some("pb_reconnect=forged")] {
        let (_, headers, body) = h.get("/reconnect/status", cookie).await;
        assert!(!headers.contains_key(header::REFRESH));
        assert!(body.contains("This reconnect page has expired"));
        assert!(!body.contains(MAILBOX));
    }
    common::raw_client(&h.db_url)
        .await
        .execute(
            "UPDATE reconnect_views SET expires_at = now() - interval '1 second'",
            &[],
        )
        .await
        .unwrap();
    let (_, body) = reconnect_page(&h, &token).await;
    assert!(body.contains("This reconnect page has expired"));
}

/// A refusal is terminal AND neutral. The warden saying "no such tenant" when
/// the store said "active tenant" is the two records disagreeing about
/// custody, which the pre-check in the handler already declines to explain to
/// the browser; the worker's answer to the same disagreement must not explain
/// it either, and must not send the user round the consent loop again.
#[tokio::test]
async fn a_refused_install_is_terminal_and_explains_nothing() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    let token = "f".repeat(43);
    enqueue_test_reconnect(&h, &token).await;
    h.rec.lock().unwrap().tenants.remove("ada");
    run_workers(&h.state).await;
    let (headers, body) = reconnect_page(&h, &token).await;
    assert!(!headers.contains_key(header::REFRESH));
    assert!(body.contains("Your mailbox couldn’t be reconnected"));
    assert!(body.contains("Open Passband"));
    assert!(!body.contains("Try again"));
    assert!(!body.contains("You’re reconnected"));
    assert!(!body.contains("account"), "no hint about custody: {body}");
    let (status, ciphertext, _) = job_row(&h).await;
    assert_eq!(status, "refused");
    assert!(ciphertext.is_none(), "a refusal discards the ciphertext");
    // And it stays refused: the row is not due again, so the warden is not
    // asked again. (The mock refuses before it records an install, so the
    // call count is the bearer list, not the install list.)
    let calls = h.rec.lock().unwrap().warden_bearers.len();
    make_due(&h).await;
    run_workers(&h.state).await;
    assert_eq!(h.rec.lock().unwrap().warden_bearers.len(), calls);
}

/// RETRIES END. An outcome the warden never settles is retried with a backoff
/// and then given up on, which NULLs the ciphertext and hands the user a fresh
/// start rather than a page that refreshes until the view expires.
#[tokio::test]
async fn reconnect_gives_up_after_bounded_retries_and_offers_a_fresh_start() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    let token = "i".repeat(43);
    enqueue_test_reconnect(&h, &token).await;
    h.rec.lock().unwrap().reconnect_status_after_install = Some(503);
    let max = squelch_control::reconnect::MAX_ATTEMPTS;
    for attempt in 1..=max {
        make_due(&h).await;
        run_workers(&h.state).await;
        let (status, ciphertext, attempts) = job_row(&h).await;
        assert_eq!(attempts, attempt);
        if attempt < max {
            assert_eq!(status, "pending", "attempt {attempt} must still retry");
            assert!(ciphertext.is_some(), "a pending job keeps its blob");
        } else {
            assert_eq!(status, "gave_up");
            assert!(ciphertext.is_none(), "giving up discards the ciphertext");
        }
    }
    // Not one install more, however due the row looks.
    make_due(&h).await;
    run_workers(&h.state).await;
    assert_eq!(
        h.rec.lock().unwrap().credential_puts.len(),
        1 + usize::try_from(max).unwrap()
    );
    let (headers, body) = reconnect_page(&h, &token).await;
    assert!(!headers.contains_key(header::REFRESH));
    assert!(body.contains("Reconnecting didn’t finish"));
    assert!(body.contains("Try again"));
    assert!(body.contains("href=\"/reconnect\""));
    assert!(!body.contains("You’re reconnected"));
}

/// The backoff is real: a retried row is not due on the next pass.
#[tokio::test]
async fn a_retried_reconnect_waits_out_its_backoff() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    enqueue_test_reconnect(&h, &"j".repeat(43)).await;
    h.rec.lock().unwrap().reconnect_status_after_install = Some(503);
    run_workers(&h.state).await;
    let due_in: f64 = common::raw_client(&h.db_url)
        .await
        .query_one(
            "SELECT EXTRACT(EPOCH FROM retry_at - now())::float8 FROM reconnect_jobs",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(
        (50.0..=60.0).contains(&due_in),
        "first backoff is a minute, got {due_in}"
    );
    run_workers(&h.state).await;
    assert_eq!(
        h.rec.lock().unwrap().credential_puts.len(),
        2,
        "not due yet"
    );
}

/// A warden that refuses our bearer will refuse it every minute until an
/// operator fixes the deployment. That is one loud log line and a terminal
/// row, not a worker slot held for an hour.
#[tokio::test]
async fn an_unauthorized_warden_ends_the_job_on_the_first_attempt() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    let token = "k".repeat(43);
    enqueue_test_reconnect(&h, &token).await;
    h.rec.lock().unwrap().reconnect_status_after_install = Some(401);
    run_workers(&h.state).await;
    let (status, ciphertext, attempts) = job_row(&h).await;
    assert_eq!((status.as_str(), attempts), ("gave_up", 1));
    assert!(ciphertext.is_none());
    let (headers, body) = reconnect_page(&h, &token).await;
    assert!(!headers.contains_key(header::REFRESH));
    assert!(body.contains("Reconnecting didn’t finish"));
}

/// A claimed row is invisible to the next pass for the length of its lease,
/// and a process that dies mid-call leaves a lease that expires on its own.
#[tokio::test]
async fn a_leased_row_is_not_claimed_twice_until_the_lease_lapses() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    enqueue_test_reconnect(&h, &"l".repeat(43)).await;
    let client = common::raw_client(&h.db_url).await;
    // A worker somewhere else holds this row.
    client
        .execute(
            "UPDATE reconnect_jobs SET lease_until = now() + interval '10 minutes'",
            &[],
        )
        .await
        .unwrap();
    assert!(
        squelch_control::reconnect::run_pending(&h.state)
            .await
            .is_empty()
    );
    assert_eq!(h.rec.lock().unwrap().credential_puts.len(), 1);
    // That worker died; its lease ran out.
    client
        .execute(
            "UPDATE reconnect_jobs SET lease_until = now() - interval '1 second'",
            &[],
        )
        .await
        .unwrap();
    run_workers(&h.state).await;
    let (status, _, _) = job_row(&h).await;
    assert_eq!(status, "complete");
    let released: bool = client
        .query_one("SELECT lease_until IS NULL FROM reconnect_jobs", &[])
        .await
        .unwrap()
        .get(0);
    assert!(released, "a finished job releases its lease");
}

/// Housekeeping is the sweeper's, and it keeps a finished job for as long as a
/// browser can still ask about it.
#[tokio::test]
async fn the_sweep_drops_expired_views_and_then_the_jobs_nobody_can_see() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    enqueue_test_reconnect(&h, &"m".repeat(43)).await;
    run_workers(&h.state).await;
    let client = common::raw_client(&h.db_url).await;
    client
        .execute(
            "UPDATE reconnect_jobs SET created_at = now() - interval '2 days'",
            &[],
        )
        .await
        .unwrap();
    squelch_control::reconnect::sweep(&h.state).await;
    let count =
        |sql: &'static str| async { client.query_one(sql, &[]).await.unwrap().get::<_, i64>(0) };
    assert_eq!(
        count("SELECT count(*) FROM reconnect_jobs").await,
        1,
        "still viewable"
    );
    client
        .execute(
            "UPDATE reconnect_views SET expires_at = now() - interval '1 second'",
            &[],
        )
        .await
        .unwrap();
    squelch_control::reconnect::sweep(&h.state).await;
    assert_eq!(count("SELECT count(*) FROM reconnect_views").await, 0);
    assert_eq!(count("SELECT count(*) FROM reconnect_jobs").await, 0);
}
//
// A refresh token can stop working while everything else is healthy: revoked by
// the user, or expired because the consent screen is still in Testing, where
// Google gives them seven days. The daemon cannot ask for a new one, so before
// `/reconnect` the only repair was an operator with a shell on the cluster.

/// THE HAPPY PATH: a second grant for a mailbox that already has a tenant
/// installs a fresh sealed credential over the dead one, and provisions nothing.
///
/// It goes through the REPLACE route, not signup's: that one answers 409 for an
/// active tenant, which is every tenant worth reconnecting.
#[tokio::test]
async fn a_reconnect_installs_a_fresh_credential_over_the_dead_one() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;

    let installs_after_signup = h.rec.lock().unwrap().credential_puts.len();
    let tenants_after_signup = h.rec.lock().unwrap().tenants.len();
    let invite_before = h.invite_row().await;

    let (status, body) = h.run_reconnect().await;
    assert_eq!(status, StatusCode::OK, "{body}");

    {
        let r = h.rec.lock().unwrap();
        assert_eq!(
            r.credential_puts.len(),
            installs_after_signup + 1,
            "the reconnect must install a credential"
        );
        let (label, put) = r.credential_puts.last().unwrap();
        assert_eq!(label, "ada");
        // A REAL SEALED BLOB, not an echo of anything: age armor, openable by
        // the identity the warden minted for this tenant and nobody else.
        let ciphertext = str_field(put, "cred_read_ciphertext");
        assert!(
            ciphertext.starts_with("-----BEGIN AGE ENCRYPTED FILE-----"),
            "expected age armor, got {ciphertext:.40}"
        );
        // NOTHING WAS PROVISIONED: no second tenant.
        assert_eq!(r.tenants.len(), tenants_after_signup);
    }
    // And no invite spent: the mailbox was paid for once, and re-consenting
    // must not cost another. Read after the guard is gone, never across it.
    assert_eq!(
        h.invite_row().await,
        invite_before,
        "no invite may be spent"
    );
}

/// A real Google account with no mailbox here is told so, and NOTHING is
/// installed. This is the check that stops `/reconnect` becoming a way to
/// provision without an invite.
#[tokio::test]
async fn a_reconnect_for_a_mailbox_with_no_tenant_installs_nothing() {
    let h = Harness::new().await;

    let (status, _) = h.run_reconnect().await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let r = h.rec.lock().unwrap();
    assert!(
        r.credential_puts.is_empty(),
        "a mailbox with no tenant must install nothing"
    );
    assert!(r.tenants.is_empty(), "and must provision nothing");
}

/// THE PARAMETER THE WHOLE FEATURE RESTS ON. Without `prompt=consent` Google
/// recognises a returning user and can answer with an access token and no
/// refresh token, which is precisely what this flow exists to replace — and it
/// would fail silently, an hour later, looking exactly like the original bug.
#[tokio::test]
async fn a_reconnect_asks_google_for_a_replacement_refresh_token() {
    let h = Harness::new().await;
    let (consent, _) = h.start_reconnect().await;

    assert_eq!(query_param(&consent, "access_type"), "offline");
    assert_eq!(query_param(&consent, "prompt"), "consent");
    // A plain reconnect made after a calendar grant must hand back a token
    // that still carries calendar, which is what this parameter asks for.
    assert_eq!(query_param(&consent, "include_granted_scopes"), "true");
    // And the full Gmail set: a replacement that covered less than the original
    // would leave the mailbox half readable.
    let scope = query_param(&consent, "scope");
    assert!(
        !scope.contains("gmail.readonly") && !scope.contains("gmail.send"),
        "{scope}"
    );
    assert!(
        scope.contains("gmail.modify"),
        "scope {scope} is missing gmail.modify"
    );
    // Calendar asked for again, optional, as signup asks it: a REVOKED grant
    // leaves `include_granted_scopes` nothing to keep, and the install replaces
    // the whole credentials file, so a Gmail-only repair would disconnect the
    // calendar (oauth.rs, `Flow::Reconnect`).
    assert!(scope.contains("calendar.events"), "{scope}");
    assert!(scope.contains("calendar.calendarlist.readonly"), "{scope}");
}

/// THE CUSTODY CHECK, from the cluster's side. If the store and the warden ever
/// disagree about who owns a mailbox, the credential does NOT get installed.
///
/// The store's mapping is not the only thing standing between a Google account
/// and somebody else's mailbox: the warden matches the same address against the
/// tenant's own identity Secret and refuses on its own. This drives that second
/// check by making the two records disagree, which is the shape a compromised
/// or buggy control plane would have.
#[tokio::test]
async fn a_reconnect_is_refused_when_the_cluster_disagrees_about_the_owner() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    let installs_after_signup = h.rec.lock().unwrap().credential_puts.len();

    // The cluster now believes a different mailbox owns `ada`, while the
    // control store still maps this Google account to it.
    h.rec
        .lock()
        .unwrap()
        .tenants
        .get_mut("ada")
        .unwrap()
        .account_email = "someone-else@example.com".to_string();

    let (status, _) = h.run_reconnect().await;
    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "a custody disagreement must not read as a user error"
    );

    let r = h.rec.lock().unwrap();
    assert_eq!(
        r.credential_puts.len(),
        installs_after_signup,
        "no credential may be installed when the two records disagree"
    );
}

/// A PENDING tenant may not be finished through the reconnect link.
///
/// Signup is two warden calls, and a tenant that stopped between them exists
/// with an identity and no workload. Installing a credential into that is
/// completing a signup: a provisioned mailbox with no invite spent, reached
/// through a public link. The control store's `active_tenant_for_email` already
/// refuses to name a non-active tenant, so this drives the case where the store
/// says yes anyway, which is what the warden's own status guard is for.
#[tokio::test]
async fn a_reconnect_cannot_finish_a_half_done_signup() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;

    // Put the cluster back into the shape a stopped-mid-signup tenant has:
    // identity present, nothing serving.
    h.rec
        .lock()
        .unwrap()
        .tenants
        .get_mut("ada")
        .unwrap()
        .provisioned = false;
    let installs_before = h.rec.lock().unwrap().credential_puts.len();

    let (status, body) = h.run_reconnect().await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Your mailbox couldn’t be reconnected"));

    assert_eq!(
        h.rec.lock().unwrap().credential_puts.len(),
        installs_before,
        "a pending tenant must not be provisioned through reconnect"
    );
}

// --- the calendar grant ------------------------------------------------------
//
// The same token with more scopes, through the same reconnect pipeline. The
// link the daemon serves is `/reconnect?calendar=1&hint=<account>`.

/// The link the daemon builds for this mailbox, hint URL-encoded.
fn calendar_link() -> String {
    format!("calendar=1&hint={}", urlencode(MAILBOX))
}

/// The slot map inside the last ciphertext installed for `label`.
fn last_installed_slots(h: &Harness, label: &str) -> Value {
    let armor = {
        let r = h.rec.lock().unwrap();
        str_field(&r.credential_puts.last().unwrap().1, "cred_read_ciphertext")
    };
    serde_json::from_str(&h.open_with_tenant_identity(label, &armor)).unwrap()
}

/// THE CALENDAR CONSENT URL, every parameter: the Gmail scope, the
/// optional profile scope and both calendar scopes in one ask, offline, forced consent, incremental, PKCE, the
/// callback, and the hint as `login_hint`.
#[tokio::test]
async fn the_calendar_consent_url_carries_every_parameter() {
    let h = Harness::new().await;
    let (consent, _) = h.start_reconnect_with(&calendar_link()).await;
    eprintln!("calendar consent url: {consent}");

    let url = url::Url::parse(&consent).unwrap();
    assert!(url.path().ends_with("/authorize"), "{consent}");
    assert_eq!(
        query_param(&consent, "scope")
            .split(' ')
            .collect::<Vec<_>>(),
        vec![
            MODIFY,
            squelch_core::config::GOOGLE_PROFILE_SCOPE,
            CAL_EVENTS,
            CAL_LIST
        ]
    );
    assert_eq!(query_param(&consent, "access_type"), "offline");
    assert_eq!(query_param(&consent, "prompt"), "consent");
    assert_eq!(query_param(&consent, "include_granted_scopes"), "true");
    assert_eq!(query_param(&consent, "login_hint"), MAILBOX);
    assert_eq!(query_param(&consent, "response_type"), "code");
    assert_eq!(query_param(&consent, "code_challenge_method"), "S256");
    assert!(!query_param(&consent, "code_challenge").is_empty());
    assert_eq!(query_param(&consent, "client_id"), "test-client-id");
    assert_eq!(
        query_param(&consent, "redirect_uri"),
        "https://signup.passband.test/oauth/callback"
    );
    assert_eq!(state_param(&consent).len(), 43);
    // Every key once: a repeated parameter is one Google reads and we did not.
    let keys: Vec<String> = url.query_pairs().map(|(k, _)| k.into_owned()).collect();
    let mut unique = keys.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(keys.len(), unique.len(), "{keys:?}");

    // A plain reconnect with a hint: the hint rides along, and it asks for
    // the same scopes as the calendar consent (calendar optional in both).
    // What `calendar=1` changes is the CALLBACK: a calendar consent that comes
    // back without calendar, or for a tenant without the feature, installs
    // nothing, where a reconnect installs the repaired mailbox either way.
    let (plain, _) = h
        .start_reconnect_with(&format!("hint={}", urlencode(MAILBOX)))
        .await;
    assert_eq!(query_param(&plain, "login_hint"), MAILBOX);
    assert_eq!(
        query_param(&plain, "scope"),
        query_param(&consent, "scope"),
        "a reconnect asks for calendar too"
    );
    assert_eq!(query_param(&plain, "include_granted_scopes"), "true");
    for not_calendar in ["calendar=0", "calendar=true", "calendar=", "Calendar=1"] {
        let (c, _) = h.start_reconnect_with(not_calendar).await;
        assert_eq!(
            query_param(&c, "scope"),
            query_param(&consent, "scope"),
            "{not_calendar}"
        );
    }
}

/// A GRANTED calendar consent goes through the reconnect pipeline: the tenant
/// is discovered from the consenting account, ONE token carrying calendar is
/// sealed into both slots, the durable job records its purpose, and the status
/// page says calendar.
#[tokio::test]
async fn a_granted_calendar_consent_installs_through_the_reconnect_pipeline() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    h.grant_feature("ada", "calendar").await;
    let installs = h.rec.lock().unwrap().credential_puts.len();
    let invite_before = h.invite_row().await;
    h.rec.lock().unwrap().granted_scope = Some(calendar_grant());

    let (status, body) = h.run_reconnect_with(&calendar_link()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Calendar is connected"), "{body}");
    assert!(
        body.contains("You can close this tab and go back to Passband."),
        "{body}"
    );
    assert!(!body.contains(MAILBOX), "{body}");
    assert!(!body.contains(REFRESH_TOKEN));

    assert_eq!(h.rec.lock().unwrap().credential_puts.len(), installs + 1);
    let (label, put) = h
        .rec
        .lock()
        .unwrap()
        .credential_puts
        .last()
        .unwrap()
        .clone();
    assert_eq!(label, "ada");
    assert_eq!(put["account_email"], MAILBOX);
    assert_eq!(put["pair"], false);
    // ONE token in both slots, its granted scopes recorded on it.
    let slots = last_installed_slots(&h, "ada");
    let read = &slots["slots"][MAILBOX];
    let write = &slots["slots"][format!("{MAILBOX}#write")];
    assert_eq!(read, write, "the one token is sealed into both slots");
    assert_eq!(read["refresh_token"], REFRESH_TOKEN);
    let recorded: Vec<&str> = read["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(recorded, vec![MODIFY, CAL_EVENTS, CAL_LIST]);
    assert!(slots["slots"].get(format!("{MAILBOX}#calendar")).is_none());

    assert_eq!(job_purposes(&h).await, vec!["calendar"]);
    let (job_status, ciphertext, _) = job_row(&h).await;
    assert_eq!(job_status, "complete");
    assert!(ciphertext.is_none());
    assert_eq!(h.invite_row().await, invite_before, "no invite spent");
}

/// THE CALENDAR HIDDEN FEATURE. A tenant without `calendar` (no row of its
/// own, no Everyone default) can start the consent, because the start names no
/// tenant and must not become an oracle; but once Google has said whose
/// mailbox this is, nothing is installed or queued and the page says calendar
/// is not available yet. The plain reconnect is untouched. With the Everyone
/// default on, the same consent installs.
#[tokio::test]
async fn a_calendar_consent_for_a_tenant_without_the_feature_installs_nothing() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    let installs = h.rec.lock().unwrap().credential_puts.len();
    h.rec.lock().unwrap().granted_scope = Some(calendar_grant());
    // ANOTHER tenant holds the feature: that grant is theirs, not ada's. The
    // gate reads the grant of the tenant the consent is for, never "anyone's".
    let other: i64 = common::raw_client(&h.db_url)
        .await
        .query_one(
            "INSERT INTO tenants(label, account_email, status, created_at)
             VALUES('bob', 'bob@example.com', 'active', '2026-10-01T00:00:00Z')
             RETURNING id",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    h.state
        .store()
        .save_features(
            &[other],
            &["calendar".to_string()],
            &[(other, "calendar".to_string())],
            &[],
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    assert!(
        h.state
            .store()
            .tenant_has_feature("bob", "calendar")
            .await
            .unwrap(),
        "the other tenant does hold it"
    );

    let (status, body) = h.run_reconnect_with(&calendar_link()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("Calendar isn&#39;t available for this mailbox yet.")
            || body.contains("Calendar isn't available for this mailbox yet."),
        "{body}"
    );
    assert!(!body.contains(MAILBOX), "{body}");
    assert!(!body.contains(REFRESH_TOKEN));
    assert_eq!(h.rec.lock().unwrap().credential_puts.len(), installs);
    assert!(job_purposes(&h).await.is_empty(), "nothing queued");

    // Everyone: the default row turns it on for this tenant too.
    let store = h.state.store();
    store
        .save_features(
            &[],
            &["calendar".to_string()],
            &[],
            &["calendar".to_string()],
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let (status, body) = h.run_reconnect_with(&calendar_link()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Calendar is connected"), "{body}");
    assert_eq!(h.rec.lock().unwrap().credential_puts.len(), installs + 1);
    assert_eq!(job_purposes(&h).await, vec!["calendar"]);
}

/// THE GATE FAILS CLOSED: when the feature cannot be read (Postgres down,
/// the table gone), a calendar consent installs nothing and queues nothing,
/// and the page says to try again; it never falls through to the install.
#[tokio::test]
async fn a_calendar_consent_whose_feature_read_fails_installs_nothing() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    let installs = h.rec.lock().unwrap().credential_puts.len();
    h.rec.lock().unwrap().granted_scope = Some(calendar_grant());
    // The Everyone default would answer yes if it were read: the failure, not
    // a "no", is what refuses.
    h.state
        .store()
        .save_features(
            &[],
            &["calendar".to_string()],
            &[],
            &["calendar".to_string()],
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let raw = common::raw_client(&h.db_url).await;
    raw.execute(
        "ALTER TABLE tenant_features RENAME TO tenant_features_gone",
        &[],
    )
    .await
    .unwrap();
    assert!(
        h.state
            .store()
            .tenant_has_feature("ada", "calendar")
            .await
            .is_err()
    );

    let (status, body) = h.run_reconnect_with(&calendar_link()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(
        body.contains("Reconnecting is unavailable right now"),
        "{body}"
    );
    assert!(!body.contains(MAILBOX), "{body}");
    assert!(!body.contains(REFRESH_TOKEN));
    assert_eq!(h.rec.lock().unwrap().credential_puts.len(), installs);
    assert!(job_purposes(&h).await.is_empty(), "nothing queued");

    // The control: with the table back, the same consent installs.
    raw.execute(
        "ALTER TABLE tenant_features_gone RENAME TO tenant_features",
        &[],
    )
    .await
    .unwrap();
    let (status, body) = h.run_reconnect_with(&calendar_link()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Calendar is connected"), "{body}");
    assert_eq!(h.rec.lock().unwrap().credential_puts.len(), installs + 1);
}

/// The plain reconnect never asks about the calendar feature.
#[tokio::test]
async fn a_plain_reconnect_does_not_need_the_calendar_feature() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    let installs = h.rec.lock().unwrap().credential_puts.len();
    let (status, body) = h.run_reconnect_with("").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.rec.lock().unwrap().credential_puts.len(), installs + 1);
    assert_eq!(job_purposes(&h).await, vec!["reconnect"]);
}

/// CALENDAR UNTICKED: the Gmail floor passes, calendar is absent, and NOTHING
/// is installed or queued. The tenant keeps the credential it had.
#[tokio::test]
async fn an_unticked_calendar_installs_nothing() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    let installs = h.rec.lock().unwrap().credential_puts.len();
    let bearers = h.rec.lock().unwrap().warden_bearers.len();
    // Every Gmail box left checked, both calendar boxes unticked; and again
    // with only the list scope, which is not a calendar grant.
    for granted in [full_grant(), format!("{} {CAL_LIST}", full_grant())] {
        h.rec.lock().unwrap().granted_scope = Some(granted);
        let (consent, cookie) = h.start_reconnect_with(&calendar_link()).await;
        h.rec.lock().unwrap().expected_challenge = Some(query_param(&consent, "code_challenge"));
        let (status, headers, body) = h
            .get(
                &format!(
                    "/oauth/callback?code=test-code&state={}",
                    state_param(&consent)
                ),
                Some(&cookie),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("Calendar was not connected"), "{body}");
        assert!(
            body.contains("nothing about your Passband mailbox changed"),
            "{body}"
        );
        assert!(!body.contains(MAILBOX), "{body}");
        assert!(
            headers
                .get_all(header::SET_COOKIE)
                .iter()
                .all(|v| !v.to_str().unwrap().starts_with("pb_reconnect=")),
            "no job to watch"
        );
    }
    let (puts, asked) = {
        let r = h.rec.lock().unwrap();
        (r.credential_puts.len(), r.warden_bearers.len())
    };
    assert_eq!(puts, installs, "nothing installed");
    assert_eq!(
        asked, bearers,
        "the warden was not even asked for a recipient"
    );
    assert!(job_purposes(&h).await.is_empty(), "nothing queued");
}

/// The Gmail floor still holds on the calendar flow: a consent missing
/// gmail.modify is refused as a reconnect's is, calendar granted or not, and
/// a legacy read+send grant does not stand in for it.
#[tokio::test]
async fn a_calendar_consent_missing_a_gmail_scope_is_refused() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    let installs = h.rec.lock().unwrap().credential_puts.len();
    for granted in [
        format!("{CAL_EVENTS} {CAL_LIST}"),
        format!("{READONLY} {SEND} {CAL_EVENTS} {CAL_LIST}"),
    ] {
        let missing = MODIFY;
        h.rec.lock().unwrap().granted_scope = Some(granted);
        let (status, body) = h.run_reconnect_with(&calendar_link()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.contains("Passband needs its Gmail permission"),
            "missing {missing}: {body}"
        );
        assert!(body.contains("Nothing changed"), "{body}");
    }
    assert_eq!(h.rec.lock().unwrap().credential_puts.len(), installs);
    assert!(job_purposes(&h).await.is_empty());
}

/// THE HINT GUARD. The link named one account and a DIFFERENT one consented:
/// nothing is installed, even though the consenting account owns a tenant of
/// its own here (which a hintless reconnect would have re-sealed), and the page
/// names neither address.
#[tokio::test]
async fn a_mismatched_account_with_a_hint_installs_nothing_and_names_no_address() {
    const BOB: &str = "bob@example.com";
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    h.grant_feature("ada", "calendar").await;
    // Bob has a mailbox here too.
    h.rec.lock().unwrap().profile_email = Some(BOB.to_string());
    h.rec.lock().unwrap().expected_challenge = None;
    let invite = h.issue_invite().await;
    let (status, body) = h.run_signup("bob", &invite).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let installs = h.rec.lock().unwrap().credential_puts.len();

    // Ada's link, Bob's Google session.
    for query in [calendar_link(), format!("hint={}", urlencode(MAILBOX))] {
        h.rec.lock().unwrap().granted_scope = Some(calendar_grant());
        let (status, body) = h.run_reconnect_with(&query).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.contains(
                "That was a different Google account than this Passband mailbox. Go back to the app and try again."
            ),
            "{body}"
        );
        assert!(!body.contains(MAILBOX), "{body}");
        assert!(!body.contains(BOB), "{body}");
        assert!(!body.contains("example.com"), "{body}");
    }
    assert_eq!(
        h.rec.lock().unwrap().credential_puts.len(),
        installs,
        "neither tenant was touched"
    );
    assert!(job_purposes(&h).await.is_empty());

    // The comparison is case-insensitive: the same account in other casing
    // is the same account, and installs.
    h.rec.lock().unwrap().profile_email = None;
    h.rec.lock().unwrap().granted_scope = Some(calendar_grant());
    let (status, body) = h
        .run_reconnect_with(&format!("calendar=1&hint={}", urlencode("ADA@Example.COM")))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Calendar is connected"), "{body}");
    let (label, _) = h
        .rec
        .lock()
        .unwrap()
        .credential_puts
        .last()
        .unwrap()
        .clone();
    assert_eq!(label, "ada");
}

/// A hint that is not one bare address is IGNORED, never passed to Google and
/// never a guard: the flow is exactly a hintless one. Nothing from it reaches a
/// header or the page.
#[tokio::test]
async fn a_garbage_or_oversized_hint_is_ignored() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    h.grant_feature("ada", "calendar").await;
    let oversized = format!("{}@example.com", "a".repeat(400));
    for raw in [
        "not-an-address".to_string(),
        "Ada <ada@example.com>".to_string(),
        "ada@example.com\r\nSet-Cookie: pwned=1".to_string(),
        "ada@example.com,bob@example.com".to_string(),
        "\"><script>alert(1)</script>@x.com".to_string(),
        oversized.clone(),
    ] {
        let query = format!("calendar=1&hint={}", urlencode(&raw));
        let (status, headers, body) = h.get(&format!("/reconnect?{query}"), None).await;
        assert_eq!(status, StatusCode::FOUND);
        let location = headers[header::LOCATION].to_str().unwrap();
        assert_eq!(query_param(location, "login_hint"), "", "{raw:?}");
        assert!(!location.contains("script"), "{location}");
        assert!(!location.contains("pwned"), "{location}");
        for v in headers.get_all(header::SET_COOKIE) {
            assert!(!v.to_str().unwrap().contains("pwned"));
        }
        assert!(body.is_empty() || !body.contains("script"));
    }

    // And the flow completes as a hintless one would: the account Google
    // names is the tenant, with no guard in the way.
    h.rec.lock().unwrap().granted_scope = Some(calendar_grant());
    let (status, body) = h
        .run_reconnect_with(&format!("calendar=1&hint={}", urlencode(&oversized)))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Calendar is connected"), "{body}");
}

/// A session completes as the purpose it STARTED as. The purpose lives in the
/// server-side session, so query parameters added to the callback change
/// nothing, and a plain reconnect whose token happens to carry calendar is
/// still a reconnect.
#[tokio::test]
async fn a_session_completes_as_the_purpose_it_started_as() {
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    h.rec.lock().unwrap().granted_scope = Some(calendar_grant());

    let (consent, cookie) = h.start_reconnect().await;
    h.rec.lock().unwrap().expected_challenge = Some(query_param(&consent, "code_challenge"));
    let (status, headers, _) = h
        .get(
            &format!(
                "/oauth/callback?code=test-code&state={}&calendar=1&hint=bob%40example.com",
                state_param(&consent)
            ),
            Some(&cookie),
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, body) = h.wait_for_reconnect(&reconnect_cookie(&headers)).await;
    assert!(body.contains("You’re reconnected"), "{body}");
    assert!(!body.contains("Calendar is connected"), "{body}");
    assert_eq!(job_purposes(&h).await, vec!["reconnect"]);

    // The other way round: a calendar session whose callback says nothing
    // about calendar is still a calendar session, and still holds its guard.
    h.rec.lock().unwrap().profile_email = Some("bob@example.com".to_string());
    let (consent, cookie) = h.start_reconnect_with(&calendar_link()).await;
    h.rec.lock().unwrap().expected_challenge = Some(query_param(&consent, "code_challenge"));
    let (_, _, body) = h
        .get(
            &format!(
                "/oauth/callback?code=test-code&state={}&calendar=0&hint=",
                state_param(&consent)
            ),
            Some(&cookie),
        )
        .await;
    assert!(
        body.contains("That was a different Google account"),
        "{body}"
    );
}

/// THE NEWER CONSENT'S PURPOSE WINS along with its ciphertext, since the row
/// installs the newer consent's token.
#[tokio::test]
async fn a_newer_consent_overwrites_a_pending_jobs_purpose() {
    use squelch_control::reconnect::{Purpose, enqueue_for};
    let h = Harness::new().await;
    h.complete_signup("ada").await;
    let ciphertext = signup_ciphertext(&h);
    enqueue_for(
        &h.state,
        &"p".repeat(43),
        "ada",
        MAILBOX,
        &ciphertext,
        Purpose::Calendar,
    )
    .await
    .unwrap();
    assert_eq!(job_purposes(&h).await, vec!["calendar"]);
    enqueue_for(
        &h.state,
        &"q".repeat(43),
        "ada",
        MAILBOX,
        &ciphertext,
        Purpose::Reconnect,
    )
    .await
    .unwrap();
    assert_eq!(job_purposes(&h).await, vec!["reconnect"]);
    enqueue_for(
        &h.state,
        &"r".repeat(43),
        "ada",
        MAILBOX,
        &ciphertext,
        Purpose::Calendar,
    )
    .await
    .unwrap();
    assert_eq!(job_purposes(&h).await, vec!["calendar"]);
    run_workers(&h.state).await;
    // Every browser that joined the job reads the newest purpose's words.
    for token in ["p".repeat(43), "q".repeat(43), "r".repeat(43)] {
        let (_, body) = reconnect_page(&h, &token).await;
        assert!(body.contains("Calendar is connected"), "{body}");
    }
}

/// THE SHIPPED SCHEMA MIGRATES. A `reconnect_jobs` table written before
/// `purpose` existed gains the column on open, and its rows read as the plain
/// reconnects they were.
#[tokio::test]
async fn an_old_reconnect_jobs_table_gains_purpose_as_reconnect() {
    let url = common::fresh_schema().await;
    let raw = common::raw_client(&url).await;
    raw.batch_execute(
        "CREATE TABLE reconnect_jobs (
            id TEXT PRIMARY KEY,
            label TEXT NOT NULL,
            account_email TEXT NOT NULL,
            ciphertext TEXT,
            status TEXT NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'complete', 'refused', 'gave_up')),
            attempts INTEGER NOT NULL DEFAULT 0,
            retry_at TIMESTAMPTZ NOT NULL DEFAULT now(),
            lease_until TIMESTAMPTZ,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        INSERT INTO reconnect_jobs (id, label, account_email, ciphertext)
            VALUES ('old-job', 'ada', 'ada@example.com', 'x');",
    )
    .await
    .unwrap();

    // Twice: the migration is idempotent across restarts.
    for _ in 0..2 {
        squelch_control::ControlStore::connect(&url).await.unwrap();
    }
    let purpose: String = raw
        .query_one(
            "SELECT purpose FROM reconnect_jobs WHERE id = 'old-job'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(purpose, "reconnect");
    // The constraint arrived with the column.
    assert!(
        raw.execute("UPDATE reconnect_jobs SET purpose = 'bogus'", &[])
            .await
            .is_err()
    );
}

// ---- Outlook -----------------------------------------------------------------

/// THE CASE OUTLOOK WAS BUILT AGAINST. A personal Microsoft account signs in as
/// an address a GMAIL tenant already has, while its mailbox is an
/// `@outlook.com` alias that mail arrives at. The signup provisions an Outlook
/// tenant for the MAILBOX, not the sign-in label, so it is not refused as a
/// duplicate of the Gmail one; and everything the daemon will need is in what
/// was sealed and sent.
#[tokio::test]
async fn an_outlook_signup_is_for_the_mailbox_mail_arrives_at() {
    let h = Harness::new().await;
    // The Gmail tenant at the Microsoft account's sign-in address.
    h.state
        .store()
        .insert_tenant("gmailone", MS_SIGNIN)
        .await
        .unwrap();
    h.rec.lock().unwrap().ms_inbox_to = vec![MS_MAILBOX.into(), MS_MAILBOX.into()];

    let (consent, cookie) = h.start_outlook_signup("qa2").await;
    let url = url::Url::parse(&consent).unwrap();
    assert_eq!(url.path(), "/ms/authorize");
    assert_eq!(query_param(&consent, "client_id"), MS_CLIENT_ID);
    assert_eq!(query_param(&consent, "prompt"), "select_account");
    assert!(query_param(&consent, "scope").contains("Mail.ReadWrite"));
    assert!(query_param(&consent, "access_type").is_empty());

    let (status, _, body) = h
        .get(
            &format!(
                "/oauth/callback?code=the-auth-code&state={}",
                state_param(&consent)
            ),
            Some(&cookie),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let create = h.rec.lock().unwrap().create_bodies.last().cloned().unwrap();
    assert_eq!(create["provider"], "outlook");
    assert_eq!(create["account_email"], MS_MAILBOX);
    // The confidential app's secret went to Microsoft, in the body.
    let form = h.rec.lock().unwrap().ms_token_form.clone();
    assert!(
        form.iter()
            .any(|(k, v)| k == "client_secret" && v == MS_CLIENT_SECRET),
        "{form:?}"
    );
    // The sealed credential records Graph scopes, which is how the tenant's
    // daemon knows it is an Outlook mailbox.
    let (label, armor) = h.credential_armor(0);
    let plaintext = h.open_with_tenant_identity(&label, &armor);
    assert!(
        plaintext.contains("graph.microsoft.com/Mail.ReadWrite"),
        "{plaintext}"
    );
    assert!(plaintext.contains(MS_MAILBOX), "{plaintext}");

    let store = h.state.store();
    assert_eq!(
        store.active_outlook_tenant_for_ms_id(MS_ID).await.unwrap(),
        Some("qa2".to_string())
    );
    let owner = store.active_tenant_owner("qa2").await.unwrap().unwrap();
    assert_eq!(owner.provider, "outlook");
    assert_eq!(owner.account_email, MS_MAILBOX);

    // And an Outlook app login finds it, by account id.
    let (status, body) = h.run_app_login("microsoft").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("qa2.passband.test"), "{body}");
    assert!(!body.contains("gmailone"), "{body}");
}

/// A MICROSOFT LOGIN NEVER OPENS A MAILBOX BY ADDRESS. Graph names an account
/// by whatever address its owner (or its tenant's admin) set, so a Microsoft
/// account claiming a Gmail tenant's address must find nothing.
#[tokio::test]
async fn a_microsoft_login_never_opens_a_mailbox_by_address() {
    let h = Harness::new().await;
    h.state
        .store()
        .insert_tenant("victim", MS_SIGNIN)
        .await
        .unwrap();
    h.rec.lock().unwrap().ms_me = Some(json!({
        "id": "deadbeefdeadbeef",
        "mail": MS_SIGNIN,
        "userPrincipalName": MS_SIGNIN
    }));
    let (status, body) = h.run_app_login("microsoft").await;
    assert!(!body.contains("victim.passband.test"), "{body}");
    assert!(!body.contains("pair"), "{status}: {body}");
}

/// A REAL DUPLICATE is refused across companies: an Outlook signup whose
/// mailbox already has a tenant (here a Google account registered on that
/// address) names the mailbox and the company that has it.
#[tokio::test]
async fn an_outlook_signup_for_a_mailbox_that_has_a_tenant_is_refused() {
    let h = Harness::new().await;
    h.state
        .store()
        .insert_tenant("already", MS_MAILBOX)
        .await
        .unwrap();
    h.rec.lock().unwrap().ms_inbox_to = vec![MS_MAILBOX.into()];
    let (status, body) = h.run_outlook_signup("dupe").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains(MS_MAILBOX), "{body}");
    assert!(body.contains("(Gmail)"), "{body}");
    assert!(h.rec.lock().unwrap().create_bodies.is_empty());
}
