//! `POST /client/calendar/connect`: the app's one "Connect your Google
//! Calendar" button, on a SELF-HOST daemon.
//!
//! A hosted daemon has a control plane that runs the consent (the stats
//! object's `calendar.connect_url`), so it answers 409 here and the app opens
//! that link instead. A self-host has no control plane, so the daemon runs the
//! same consent `squelchd auth --calendar` runs: it mints a `state` and a PKCE
//! pair ([`LoopbackConsent`], shared with the CLI), hands the app the consent
//! URL, and waits for Google's redirect on a loopback listener.
//!
//! The listener:
//! - binds `127.0.0.1:<port>` only (the headless CLI's fixed port, or
//!   `SQUELCH_AUTH_PORT`), and refuses any peer that is not loopback;
//! - serves exactly one route, the redirect path the Desktop client uses;
//! - lives while a consent is pending: at most one per daemon, replaced by the
//!   next Connect, expired after ten minutes, consumed by the first redirect
//!   that carries its `state`;
//! - stores nothing unless the `state` matches (constant time), the exchange
//!   succeeds, the grant covers `calendar.events`, and Google names the
//!   configured mailbox. Then the token goes into the Calendar slot through the
//!   configured backend, the `granted` cache is dropped, and the calendar sync
//!   is kicked.
//!
//! Pages carry no scripts and no links, and never name an address. Logs carry
//! event kinds only: never the code, the `state`, a token, or an address.
//! HUMAN DOOR ONLY: nothing on `/mcp` or `/client/agent/*` reaches any of it.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::extract::{ConnectInfo, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use squelch_core::auth::{
    AuthScopes, ConsentFailure, GoogleEndpoints, LOOPBACK_REDIRECT_PATH, LoopbackConsent,
};
use squelch_core::credentials::CalendarCredentials;
use squelch_core::types::AccountId;

use crate::error::ApiError;
use crate::handlers::no_store;
use crate::state::{ApiState, CalendarRefresh};

/// How long one consent stays redeemable, and the `expires_in` the app is
/// told. The same ten minutes the CLI's listener waits.
pub(crate) const CONSENT_TTL: Duration = Duration::from_secs(600);

/// The fixed answer on a daemon with a control plane.
pub(crate) const HOSTED_ERROR: &str =
    "calendar consent for this mailbox runs on the control plane; open calendar.connect_url";

/// The fixed answer on a daemon with no OAuth client to run a consent with.
pub(crate) const NO_CLIENT_ERROR: &str =
    "this daemon has no Google OAuth client configured, so it cannot run the calendar consent";

/// The fixed answer when the loopback port cannot be bound.
pub(crate) const PORT_BUSY_ERROR: &str = "the loopback port for the calendar consent is in use";

/// How long a fresh bind keeps retrying while a listener that was just told to
/// stop lets go of the port.
const BIND_RETRY: Duration = Duration::from_millis(100);
const BIND_ATTEMPTS: usize = 10;

// --- the pages --------------------------------------------------------------

pub(crate) const PAGE_CONNECTED: &str =
    "Calendar is connected. You can close this tab and go back to Passband.";
pub(crate) const PAGE_STALE: &str = "This link is no longer valid, so nothing changed. Go back to Passband and press Connect again.";
pub(crate) const PAGE_WRONG_ACCOUNT: &str = "That was a different Google account than this \
     Passband mailbox, so nothing changed. Go back to Passband and try again.";
pub(crate) const PAGE_NOT_GRANTED: &str = "Calendar was not connected because the calendar \
     permission was not ticked on the Google screen, so nothing changed. Go back to Passband, \
     press Connect again, and leave the calendar box ticked.";
pub(crate) const PAGE_DECLINED: &str = "Calendar was not connected, and nothing changed. You can close this tab and go back to Passband.";
pub(crate) const PAGE_FAILED: &str =
    "Calendar could not be connected, so nothing changed. Go back to Passband and try again.";
pub(crate) const PAGE_NOT_FOUND: &str = "Nothing is here.";

/// One plain page: a title, one sentence, no scripts, no links, no styles a
/// policy would have to allow.
fn page(status: StatusCode, text: &'static str) -> Response {
    let body = format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width\">\
         <title>Passband</title></head>\
         <body><p>{text}</p></body></html>\n"
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    (status, headers, body).into_response()
}

/// An event kind, and nothing else. See the module docs.
fn log_event(kind: &str) {
    eprintln!("squelch-api: calendar consent: {kind}");
}

// --- the one pending consent ------------------------------------------------

/// The daemon's one pending calendar consent and the listener waiting for it.
#[derive(Default)]
pub(crate) struct CalendarConsent {
    /// Serializes Connect calls, so two at once cannot both bind.
    starting: tokio::sync::Mutex<()>,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    pending: Option<Pending>,
    server: Option<Server>,
    /// Bumped by every Connect, so an expiry timer can tell whether the
    /// consent it was set for is still the pending one.
    generation: u64,
}

struct Pending {
    consent: LoopbackConsent,
    expires_at: tokio::time::Instant,
    generation: u64,
}

struct Server {
    port: u16,
    shutdown: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl Inner {
    /// Stop the listener. It stops accepting at once and finishes whatever
    /// request is in flight (the one that consumed the consent, usually).
    fn stop_server(&mut self) {
        if let Some(server) = self.server.take() {
            let _ = server.shutdown.send(());
        }
    }

    /// The pending consent when `state` is its state and it has not expired,
    /// TAKEN, so it can be redeemed once. Anything else leaves it in place: a
    /// stray request must not cancel the person's consent.
    fn take_matching(&mut self, state: &str) -> Option<LoopbackConsent> {
        let live = self.pending.as_ref().is_some_and(|p| {
            tokio::time::Instant::now() < p.expires_at && p.consent.state_matches(state)
        });
        if !live {
            return None;
        }
        self.pending.take().map(|p| p.consent)
    }
}

/// What the listener's handler needs.
#[derive(Clone)]
struct ListenerCtx {
    consent: Arc<CalendarConsent>,
    calendar: Arc<CalendarCredentials>,
    refresh: CalendarRefresh,
    account_id: AccountId,
}

impl CalendarConsent {
    /// Make `consent`-to-be the one pending consent, binding the listener when
    /// none is running, and return the consent URL. A consent pending before
    /// this call is gone: its `state` is refused from here on.
    async fn start(
        self: &Arc<Self>,
        ctx: ListenerCtx,
        port: u16,
        endpoints: &GoogleEndpoints,
    ) -> Result<String, ApiError> {
        let _one_at_a_time = self.starting.lock().await;

        let running = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            // A listener whose task ended on its own (it should not) is gone.
            if inner.server.as_ref().is_some_and(|s| s.task.is_finished()) {
                inner.server = None;
            }
            inner.server.as_ref().map(|s| s.port)
        };
        let bound_port = match running {
            Some(port) => port,
            None => {
                let listener = bind_loopback(port).await.map_err(|_| {
                    log_event("refused: loopback port busy");
                    ApiError::new(StatusCode::SERVICE_UNAVAILABLE, PORT_BUSY_ERROR)
                })?;
                let bound = listener
                    .local_addr()
                    .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, PORT_BUSY_ERROR))?
                    .port();
                let (tx, rx) = tokio::sync::oneshot::channel::<()>();
                let app = listener_router(ctx.clone());
                let task = tokio::spawn(async move {
                    let _ = axum::serve(
                        listener,
                        app.into_make_service_with_connect_info::<SocketAddr>(),
                    )
                    .with_graceful_shutdown(async move {
                        let _ = rx.await;
                    })
                    .await;
                });
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.server = Some(Server {
                    port: bound,
                    shutdown: tx,
                    task,
                });
                bound
            }
        };

        let calendar = &ctx.calendar;
        let consent = LoopbackConsent::begin_at(
            calendar.client(),
            AuthScopes::Calendar,
            bound_port,
            Some(calendar.account_email()),
            endpoints,
        )
        .map_err(|_| {
            log_event("failed: could not build the consent");
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if inner.pending.is_none() {
                inner.stop_server();
            }
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        })?;
        let url = consent.url().to_string();

        let generation = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.generation += 1;
            let generation = inner.generation;
            inner.pending = Some(Pending {
                consent,
                expires_at: tokio::time::Instant::now() + CONSENT_TTL,
                generation,
            });
            generation
        };

        // The expiry: if THIS consent is still the pending one when its time
        // is up, it goes, and the listener with it.
        let this = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(CONSENT_TTL).await;
            let mut inner = this.inner.lock().unwrap_or_else(|e| e.into_inner());
            if inner
                .pending
                .as_ref()
                .is_some_and(|p| p.generation == generation)
            {
                inner.pending = None;
                inner.stop_server();
                log_event("expired");
            }
        });

        log_event("started");
        Ok(url)
    }

    /// Whether a listener is bound right now. For tests.
    #[cfg(test)]
    fn listening(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .server
            .is_some()
    }
}

/// Bind `127.0.0.1:<port>`, retrying briefly: a listener this process just
/// told to stop may not have let go of the port yet.
async fn bind_loopback(port: u16) -> std::io::Result<tokio::net::TcpListener> {
    let mut last = None;
    for attempt in 0..BIND_ATTEMPTS {
        match tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await {
            Ok(listener) => return Ok(listener),
            Err(e) => last = Some(e),
        }
        if attempt + 1 < BIND_ATTEMPTS {
            tokio::time::sleep(BIND_RETRY).await;
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("bind failed")))
}

/// The listener's router: the redirect path, and a plain 404 for anything
/// else.
fn listener_router(ctx: ListenerCtx) -> axum::Router {
    axum::Router::new()
        .route(LOOPBACK_REDIRECT_PATH, axum::routing::get(callback))
        .fallback(|| async { page(StatusCode::NOT_FOUND, PAGE_NOT_FOUND) })
        .with_state(ctx)
}

/// Google's redirect. See the module docs for every check.
async fn callback(
    State(ctx): State<ListenerCtx>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    RawQuery(query): RawQuery,
) -> Response {
    // Bound to 127.0.0.1, so nothing else can connect; checked anyway, because
    // the whole safety of this route rests on it.
    if !peer.ip().is_loopback() {
        log_event("refused: not a loopback peer");
        return page(StatusCode::FORBIDDEN, PAGE_STALE);
    }

    let mut code = None;
    let mut state = None;
    let mut error = false;
    for (key, value) in url::form_urlencoded::parse(query.unwrap_or_default().as_bytes()) {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = true,
            _ => {}
        }
    }

    let Some(state) = state else {
        log_event("refused: no state");
        return page(StatusCode::BAD_REQUEST, PAGE_STALE);
    };
    let consent = {
        let mut inner = ctx.consent.inner.lock().unwrap_or_else(|e| e.into_inner());
        let taken = inner.take_matching(&state);
        if taken.is_some() {
            // Consumed: whatever happens next, this listener has served its
            // one redirect.
            inner.stop_server();
        }
        taken
    };
    let Some(consent) = consent else {
        log_event("refused: state");
        return page(StatusCode::BAD_REQUEST, PAGE_STALE);
    };

    let Some(code) = code.filter(|c| !c.is_empty()) else {
        // Google's own "no": the person pressed Cancel, or the screen failed.
        log_event(if error {
            "declined"
        } else {
            "refused: no code"
        });
        return page(StatusCode::BAD_REQUEST, PAGE_DECLINED);
    };

    let calendar = ctx.calendar.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let (_mailbox, token) = consent.finish(code, calendar.account_email())?;
        calendar
            .store_calendar_token_blocking(&token)
            .map_err(ConsentFailure::Failed)
    })
    .await;

    match outcome {
        Ok(Ok(())) => {
            ctx.refresh.kick_now(ctx.account_id);
            log_event("connected");
            page(StatusCode::OK, PAGE_CONNECTED)
        }
        Ok(Err(ConsentFailure::WrongAccount(_))) => {
            log_event("refused: a different account consented");
            page(StatusCode::FORBIDDEN, PAGE_WRONG_ACCOUNT)
        }
        Ok(Err(ConsentFailure::ScopesNotGranted(_))) => {
            log_event("refused: calendar not granted");
            page(StatusCode::BAD_REQUEST, PAGE_NOT_GRANTED)
        }
        Ok(Err(ConsentFailure::Failed(_))) | Err(_) => {
            log_event("failed: exchange or store");
            page(StatusCode::BAD_GATEWAY, PAGE_FAILED)
        }
    }
}

// --- the human-door endpoint ------------------------------------------------

/// `POST /client/calendar/connect`: start the self-host calendar consent and
/// answer `{"url", "expires_in"}`. 409 on a daemon with a control plane (the
/// app opens `calendar.connect_url` there) or with no OAuth client; 503 when
/// the loopback port is taken (a `squelchd auth --headless` holding it, say).
pub async fn connect_calendar(State(state): State<ApiState>) -> Result<Response, ApiError> {
    if state.console_sso_url().is_some() {
        return Err(ApiError::new(StatusCode::CONFLICT, HOSTED_ERROR));
    }
    let Some(calendar) = state.calendar().cloned() else {
        return Err(ApiError::new(StatusCode::CONFLICT, NO_CLIENT_ERROR));
    };
    let ctx = ListenerCtx {
        consent: state.calendar_consent.clone(),
        calendar,
        refresh: state.calendar_refresh.clone(),
        account_id: state.account_id,
    };
    let url = state
        .calendar_consent
        .start(
            ctx,
            state.calendar_consent_port,
            &state.calendar_consent_endpoints,
        )
        .await?;
    Ok((
        StatusCode::OK,
        no_store(),
        Json(json!({ "url": url, "expires_in": CONSENT_TTL.as_secs() })),
    )
        .into_response())
}

/// Whether this daemon can run the calendar consent itself: a self-host (no
/// control plane) with an OAuth client configured. `calendar.self_connect` on
/// `/client/stats`.
pub(crate) fn self_connect(state: &ApiState) -> bool {
    state.console_sso_url().is_none() && state.calendar().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page names no address, carries no script and no link, whatever it
    /// says.
    #[test]
    fn pages_are_plain() {
        for text in [
            PAGE_CONNECTED,
            PAGE_STALE,
            PAGE_WRONG_ACCOUNT,
            PAGE_NOT_GRANTED,
            PAGE_DECLINED,
            PAGE_FAILED,
            PAGE_NOT_FOUND,
        ] {
            assert!(!text.contains('@'), "{text}");
            assert!(!text.contains('\u{2014}'), "{text}");
        }
    }

    fn consent() -> (LoopbackConsent, String) {
        let client = squelch_core::config::OAuthClientConfig {
            client_id: "id".into(),
            client_secret: "secret".into(),
        };
        let consent =
            LoopbackConsent::begin(&client, AuthScopes::Calendar, 8847, Some("me@x.com")).unwrap();
        let state = consent
            .url()
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.into_owned())
            .unwrap();
        (consent, state)
    }

    /// A pending consent is redeemable once, by its own state, while it is
    /// live; an expired one is refused even by its own state, and a wrong
    /// state leaves it in place.
    #[tokio::test]
    async fn only_a_live_matching_state_takes_the_consent() {
        let (c, state) = consent();
        let mut inner = Inner {
            pending: Some(Pending {
                consent: c,
                expires_at: tokio::time::Instant::now() + CONSENT_TTL,
                generation: 1,
            }),
            ..Default::default()
        };
        assert!(inner.take_matching("wrong").is_none());
        assert!(inner.pending.is_some(), "a stray request cancelled it");
        assert!(inner.take_matching(&state).is_some());
        assert!(inner.take_matching(&state).is_none(), "redeemed twice");

        let (c, state) = consent();
        inner.pending = Some(Pending {
            consent: c,
            expires_at: tokio::time::Instant::now() - Duration::from_secs(1),
            generation: 2,
        });
        assert!(inner.take_matching(&state).is_none(), "an expired consent");
    }

    /// A listener with nothing pending is stopped: a fresh consent slot has
    /// none.
    #[test]
    fn nothing_listens_until_a_consent_starts() {
        let consent = CalendarConsent::default();
        assert!(!consent.listening());
    }
}
