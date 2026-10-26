//! squelch-api: the HUMAN DOOR — the `/client/*` API for the user's own client.
//! Bearer auth on every route but the pairing claim, which is how a device with
//! no credential gets one. Carries sealed METADATA and exactly one sealed body
//! (audited before it is served, `no-store`), and is the only surface with write
//! capability. No token, secret, or message body is ever logged. See
//! docs/SECURITY.md §4.

mod agent;
mod agent_write;
mod assistant;
mod auth;
mod calendar_connect;
mod calendar_write;
mod console;
mod devices;
mod error;
mod events;
pub mod gmail_write;
mod group_send;
mod groups;
pub mod guard;
mod handlers;
mod http_metrics;
mod invite_mail;
pub mod limits;
mod markdown;
mod pair;
mod scheduled_send;
mod serve;
mod sharing;
mod state;
pub mod tracking;
mod triage;
mod unsub_browser;
mod unsub_cdp;
mod unsub_fetch;
mod unsub_html;
mod unsub_job;
pub mod unsubscribe;

pub use assistant::AssistantRelay;
pub use auth::{DEVICE_AUTH_CONCURRENCY, require_bearer};
pub use error::ApiError;
/// The auth-mail retention pass. Exported for the daemon's timer: it uses the
/// WRITE credential, which the readonly-bound sync loop must never touch.
pub use handlers::run_shred_pass;
pub use http_metrics::{ROUTE_CLASSES, record_http_metrics};
/// The ceilings a `/client/*` request runs under and the store-call cap.
pub use limits::{
    LONG_REQUEST_TIMEOUT, REQUEST_TIMEOUT, ROUTE_CEILINGS, RequestCeiling, RequestTimeouts,
    STORE_CALL_SLOTS, UPSTREAM_REQUEST_TIMEOUT, route_ceiling,
};
/// The scheduled-send worker and its pass. Exported for the daemon for the
/// same reason as the shredder: it sends on the WRITE credential.
pub use scheduled_send::{
    ScheduledPassReport, ScheduledWorkerTiming, run_scheduled_send_pass, scheduled_send_worker,
};
/// The accept loop every daemon door runs on: `axum::serve` with a live
/// hyper timer.
pub use serve::{HEADER_READ_TIMEOUT, serve, serve_with_header_timeout};
/// The invite minter. Exported for the daemon binaries that build state by
/// hand and for the integration suite, which points it at a mock control plane.
pub use sharing::{FEATURE_REFRESH, FeatureRefresher, Sharing};
pub use state::{AgentDoorStatus, ApiState, StateError, attach_event_channel};

use axum::{
    Router, middleware,
    routing::{delete, get, patch, post, put},
};

/// Build this crate's router: the bearer-authed `/client/*` tree, plus the TWO
/// unauthenticated routes, each in its own single-route router so the auth
/// boundary is a line you can see rather than a layer you have to trace:
///
/// - `GET /t/{token}` ([`tracking::pixel_router`]) — the read-tracking pixel,
///   fetched by a recipient's mail client, which has no token and never will;
/// - `POST /client/pair` ([`pair::pair_router`]) — the pairing claim, which is
///   how a device with no credential gets its first one.
///
/// Nothing else is ever added to either. Everything in [`client_router`] is
/// behind the bearer.
///
/// `/console` ([`console::console_router`]) is the third tree, and the only one
/// authenticated by a COOKIE rather than a header. It sits outside the bearer
/// layer (signing in has to be possible without a credential) and outside the
/// `/client` CORS layer (a browser attaches a cookie by itself, so the
/// permissive CORS the JSON tree can afford would be a gift to any page on the
/// web). What it authenticates with is not a new credential: a console session
/// IS a device token, verified by the same store call.
pub fn router(state: ApiState) -> Router {
    client_router(state.clone())
        .merge(pair::pair_router(state.clone()))
        .merge(console::console_router(state.clone()))
        .merge(tracking::pixel_router(state))
        // The bare root: a browser typing the tenant hostname lands on the
        // console's login page instead of a 404. A redirect carries nothing,
        // so it needs no auth; on hosted the Ingress publishes "/" as an
        // EXACT match so this stays the only extra path a vhost serves.
        .route(
            "/",
            axum::routing::get(|| async { axum::response::Redirect::temporary("/console") }),
        )
        // gzip over the whole door, outermost: one layer, and the predicate
        // below decides per response what is left alone.
        .layer(compression_layer())
}

/// The `/client/*` router. Bearer auth is layered over every route in it: the
/// `SQUELCH_API_TOKEN` master token, or any device token the store has issued
/// and not revoked. A daemon with neither still serves this tree; it just 401s
/// everything until `squelchd token issue` or a pairing claim mints one.
fn client_router(state: ApiState) -> Router {
    Router::new()
        .route("/client/agent/thread/{thread_id}", get(agent::thread))
        .route("/client/agent/search", get(agent::search))
        .route("/client/agent/feed", get(agent::feed))
        .route("/client/agent/records", get(agent::records))
        .route("/client/agent/triage/{message_id}", get(agent::decision))
        .route("/client/v2/capabilities", get(triage::capabilities))
        .route("/client/v2/feed", get(triage::feed))
        .route("/client/v2/fye/headline", get(triage::fye_headline))
        .route("/client/v2/messages/{message_id}", get(triage::message))
        .route(
            "/client/v2/messages/{message_id}/opened",
            post(triage::opened),
        )
        .route(
            "/client/v2/messages/{message_id}/corrections",
            post(triage::correct),
        )
        .route("/client/v2/triage/{message_id}", get(triage::decision))
        .route("/client/updates", get(handlers::get_updates))
        .route(
            "/client/updates/{message_id}/status",
            post(handlers::set_update_status),
        )
        // "Remind me about this later." Human door only, like every write: it is
        // the user scheduling their own attention, and the agent door gets no
        // say in when the user is shown something. POST sets (and re-sets, one
        // reminder per message), DELETE un-schedules.
        .route(
            "/client/updates/{message_id}/reminder",
            post(handlers::set_update_reminder).delete(handlers::clear_update_reminder),
        )
        .route("/client/refresh", post(handlers::refresh_now))
        .route("/client/spam/refresh", post(handlers::refresh_spam))
        .route("/client/thread/{thread_id}", get(handlers::get_thread))
        // "I opened this." Its own route rather than a side effect of the GET
        // above, because the client warms threads it has not shown and opens
        // warmed ones without asking: see `handlers::mark_thread_opened`.
        // HUMAN DOOR ONLY, and not merely by being on this tree — it is a
        // statement about what a PERSON looked at, which an agent cannot make.
        .route(
            "/client/thread/{thread_id}/opened",
            post(handlers::mark_thread_opened),
        )
        // "Translate what I am showing." The GET serves an archive thread's
        // translations as `deferred` and buys nothing, for the same prefetch
        // reason; the reader displaying the thread asks here. HUMAN DOOR ONLY.
        .route(
            "/client/thread/{thread_id}/translate",
            post(handlers::translate_thread),
        )
        // "Languages I read": mail in them is never translated.
        .route(
            "/client/translate-config",
            get(handlers::get_translate_config).post(handlers::set_translate_config),
        )
        .route("/client/attachments/{id}", get(handlers::get_attachment))
        // FILES ON THEIR WAY OUT (see the handlers' header). The upload carries
        // its own body limit: the router's default is 2 MB, sized for JSON, and
        // this is the one route whose body is a file. Sized for the cap plus a
        // little, so the cap's own 413 message is what a too-large file gets
        // rather than the framework's.
        .route(
            "/client/compose/attachments",
            post(handlers::stage_compose_attachment).layer(axum::extract::DefaultBodyLimit::max(
                handlers::MAX_OUTBOUND_ATTACHMENT_BYTES + 64 * 1024,
            )),
        )
        .route(
            "/client/compose/attachments/{id}",
            get(handlers::get_compose_attachment).delete(handlers::delete_compose_attachment),
        )
        .route("/client/shipments", get(handlers::get_shipments))
        // Force a carrier pass. HUMAN DOOR ONLY: the agent door reads the
        // shipments table and never gets to spend the operator's carrier quota.
        .route("/client/shipments/poll", post(handlers::poll_shipments_now))
        // "Stop showing me this package." HUMAN DOOR ONLY, like every write: it
        // is a statement about what the user wants to see. A read-side hide, not
        // a delete — the row keeps polling and returns on its own when an update
        // lands, so there is deliberately no un-clear route to pair with it.
        .route(
            "/client/shipments/{id}/clear",
            post(handlers::clear_shipment),
        )
        .route("/client/receipts", get(handlers::get_receipts))
        .route("/client/banking", get(handlers::get_banking))
        .route("/client/calendar", get(handlers::get_calendar))
        // Everything behind the Calendar page, behind the `calendar` hidden
        // feature: see `calendar_routes`.
        .merge(calendar_routes(state.clone()))
        .route("/client/search", get(handlers::search))
        // The user's own sent mail. Human door only — this is the one listing
        // that reads `is_sent = 1`, and the agent door gets no such route: what
        // the user writes is not the agent's to page through.
        .route("/client/sent", get(handlers::get_sent))
        .route("/client/rules", get(handlers::list_rules))
        .route("/client/rules", post(handlers::create_rule))
        // A suggested rule sentence for the card. Human door only: it reads a
        // sender's recent subjects, and its answer is shown to the owner, never
        // saved on its own.
        .route("/client/rules/suggest", get(handlers::suggest_rule))
        .route("/client/rules/{id}", put(handlers::update_rule))
        .route("/client/rules/{id}", delete(handlers::delete_rule))
        // Local drafts: served EXCLUSIVELY here. Unsent compositions stay on
        // this machine — the agent door has no drafts route and never learns the
        // table exists. The keyed PUT edits in place (one draft per reply
        // target, one per forwarded message; no key edits the newest new-message
        // draft, for apps that predate many drafts). New-message drafts are
        // many: POST creates one, PUT /{id} edits one by id. GET omits forward
        // drafts unless asked with `forwards=1`: an app predating them would
        // restore one as its plain new message (see `ListDraftsQuery`).
        .route(
            "/client/drafts",
            get(handlers::list_drafts)
                .put(handlers::put_draft)
                .post(handlers::create_draft),
        )
        .route(
            "/client/drafts/{id}",
            put(handlers::update_draft).delete(handlers::delete_draft),
        )
        // The outbox: finished mail the daemon sends later. Served EXCLUSIVELY
        // here, like drafts — the agent door has no route to it and never
        // learns the table exists. POST schedules (running every check a send
        // runs, guard included, while the user is there to answer), GET lists
        // what is still owed an outcome, DELETE pulls one back into the
        // composer, and /now wakes the worker to send one. Its own router so
        // one `no-store` layer covers every response on it, an extractor's
        // rejection included.
        .merge(scheduled_send::routes())
        // The composer/signature preview: the send path's own markdown render,
        // exposed so the client never grows a second, drifting renderer.
        .route("/client/markdown/preview", post(handlers::markdown_preview))
        // Recipient autocomplete over Sent-derived contacts. Human door only —
        // the agent door must never see who the user writes to.
        .route("/client/contacts", get(handlers::get_contacts))
        // Sender autocomplete for the search field's `from:` operator: who
        // writes to the user. Human door only, for the reason contacts is.
        .route("/client/senders", get(handlers::get_senders))
        .route("/client/sender-domains", get(handlers::get_sender_domains))
        // The reader's contact card: one address, everything known about it.
        // Human door only, for the reason contacts is.
        .route("/client/person", get(handlers::get_person))
        .merge(groups::routes())
        .route("/client/sealed", get(handlers::list_sealed))
        .route(
            "/client/sealed/{message_id}/reveal",
            post(handlers::reveal_sealed),
        )
        // The owner's "That was me / Not me" on one auth message. Human door
        // only: the agent door never reads or writes a verdict.
        .route(
            "/client/sealed/{message_id}/verdict",
            put(handlers::put_sealed_verdict),
        )
        .route(
            "/client/shredder",
            get(handlers::get_shredder).post(handlers::set_shredder),
        )
        // Read tracking, human door only: the opens of the user's OWN sent mail,
        // and whether the client should default new sends to tracked. The pixel
        // that produces these rows is the unauthenticated route merged in
        // `router`; the agent door has neither.
        .route(
            "/client/messages/{message_id}/opens",
            get(tracking::get_message_opens),
        )
        // Reply-recipient preview: who a reply (or `?all=true`, a reply-all)
        // would be addressed to, derived server-side from the parent's headers
        // by the same code the send path uses. A READ — it reaches Gmail for
        // metadata only and sends nothing — so it needs no confirm gate, but it
        // does need the write credential the fetch rides on.
        .route(
            "/client/messages/{message_id}/reply_recipients",
            get(handlers::reply_recipients),
        )
        .route(
            "/client/tracking-config",
            get(tracking::get_tracking_config).post(tracking::set_tracking_config),
        )
        .route("/client/shredder/run", post(handlers::run_shredder))
        .route("/client/marketing", get(handlers::get_marketing))
        .route("/client/audit", get(handlers::get_audit))
        .route("/client/stats", get(handlers::get_stats))
        .route("/client/account/photo", get(handlers::get_account_photo))
        // SHARING. The GET is a read (may this daemon share, and what could the
        // mail say); the POST spends a quota and sends mail as the user, so it
        // sits with the write routes below rather than here... except it does
        // not, and that is deliberate: the action routes are gated on the WRITE
        // credential being configured at all, and this route's own refusal says
        // something more useful than a blanket 403. See `sharing::post_invites`.
        .route("/client/invites", get(sharing::get_invites))
        .route("/client/invites", post(sharing::post_invites))
        .route("/client/usage", get(handlers::get_usage))
        .route("/client/mail-activity", get(handlers::get_mail_activity))
        .route(
            "/client/triage-config",
            get(handlers::get_triage_config).post(handlers::set_triage_config),
        )
        .route(
            "/client/triage-feedback",
            get(handlers::get_triage_feedback).post(handlers::post_triage_feedback),
        )
        // Dev re-triage + inspector: human-door only.
        .route(
            "/client/retriage",
            get(handlers::retriage_progress).post(handlers::retriage),
        )
        .route(
            "/client/triage-debug/{message_id}",
            get(handlers::triage_debug),
        )
        // Unsubscribe: human-door only (never exposed on the agent door).
        .route("/client/unsubscribe", post(handlers::unsubscribe))
        .route("/client/unsubscribes", get(handlers::list_unsubscribes))
        .route(
            "/client/unsubscribes/opened",
            post(handlers::unsubscribe_opened),
        )
        .route(
            "/client/unsubscribes/resolution",
            post(handlers::unsubscribe_resolution),
        )
        .route(
            "/client/unsubscribes/smart_block",
            get(handlers::unsubscribe_smart_block),
        )
        // Notification delivery: the SSE feed plus the iOS NSE's by-id fetch.
        // Human door only — the agent door gains no access to the event log.
        .route("/client/events", get(events::events_stream))
        .route("/client/events/{id}", get(events::get_event))
        // Push-device registration. Human door only — the agent door never
        // learns a device exists.
        .route("/client/devices", post(devices::register_device))
        // Unregister carries the token in the BODY, not a path segment: a device
        // token is capability material, and a URL path is the most-logged part
        // of a request.
        .route(
            "/client/devices/unregister",
            post(devices::unregister_device),
        )
        // Assistant relay: human door ONLY. The body is the user's own
        // conversation, sent by their paired client, and it is spent against
        // the tenant's assistant budget at the gateway — the agent door never
        // gains a route that burns tenant money or fronts a daemon-held
        // credential.
        .route(
            "/client/assistant/messages",
            post(assistant::assistant_messages),
        )
        // Actions: the only write capability. Require the opt-in write
        // credential; 403 without one.
        .route("/client/actions/archive", post(handlers::action_archive))
        .route("/client/actions/not_spam", post(handlers::action_not_spam))
        .route("/client/actions/label", post(handlers::action_label))
        .route("/client/actions/send", post(handlers::action_send))
        // What an agent on the agent door may write (issue #394). Set HERE,
        // by the owner, and only read over there: the agent door has no route
        // or tool that changes it.
        .route(
            "/client/settings/agent-access",
            get(agent_write::get_agent_access).put(agent_write::put_agent_access),
        )
        // Bearer auth wraps EVERY route above.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_bearer,
        ))
        // The per-route ceiling, over auth: a request that has not produced
        // its response head by its ceiling is answered 504, the bearer check
        // and any wait for a store-call slot included. Inside CORS, so the
        // 504 carries the headers a browser caller needs to read it. See
        // `limits` for why it never touches a streaming body.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            limits::request_timeout,
        ))
        // CORS wraps both (outermost) so credential-less OPTIONS preflights are
        // answered instead of 401ing. Permissive by design: bearer auth is the
        // security boundary and no cookies are involved.
        .layer(
            tower_http::cors::CorsLayer::new()
                .allow_origin(tower_http::cors::Any)
                .allow_methods(tower_http::cors::Any)
                .allow_headers(tower_http::cors::Any),
        )
        .with_state(state)
}

/// gzip, for any caller that asks (`Accept-Encoding: gzip`), at the fastest
/// level. The bodies this door serves are JSON and server-rendered or
/// sanitized HTML, which gzip cuts 5-10x, and the Swift client's `URLSession`
/// asks on every request and decodes transparently. gzip is the only encoding
/// compiled in (`compression-gzip`), so nothing here has to be switched on.
///
/// What is NOT compressed is the predicate's call, per response, and three of
/// its rules carry real weight:
///
/// - `text/event-stream` (tower-http's default): the events stream and the
///   assistant relay, which mirrors the upstream content-type. An SSE frame
///   has to leave the process the moment it is written, and a gzip encoder
///   would hold it.
/// - images (default): the tracking pixel, the account photo, and image
///   attachments, already-compressed bytes.
/// - `application/octet-stream` and `application/pdf` (ours): the attachment
///   door serves everything that is not a PDF or a non-SVG image as
///   octet-stream (`handlers::safe_content_type`), and a PDF or an
///   archive deflated on a worker thread comes out the same size for the
///   CPU spent.
///
/// Bodies under 32 bytes (default) are left alone too: a gzip header costs
/// more than it saves there.
fn compression_layer()
-> tower_http::compression::CompressionLayer<impl tower_http::compression::Predicate> {
    use tower_http::compression::predicate::{DefaultPredicate, NotForContentType, Predicate};
    tower_http::compression::CompressionLayer::new()
        .quality(tower_http::CompressionLevel::Fastest)
        .compress_when(
            DefaultPredicate::new()
                .and(NotForContentType::const_new("application/octet-stream"))
                .and(NotForContentType::const_new("application/pdf")),
        )
}

/// The Calendar page's routes, ALL BEHIND THE `calendar` HIDDEN FEATURE
/// ([`squelch_core::features::CALENDAR`]). Off is as if the feature did not
/// exist: every route here answers [`ApiError::not_found`], the opaque 404 a
/// missing thread or a hidden sealed message gets, asked at the moment of each request (a flag flipped under a running daemon
/// changes the next answer), and BEFORE ANY OTHER WORK. The gate is a route
/// layer rather than a line in each handler because the handlers' extractors
/// (the query, the JSON body, the path) run before a handler body could ask,
/// and a refused request should not have its body read at all. For the same
/// reason a write refused here writes NO audit row: a path the daemon does
/// not serve writes none either, and the audit log records attempts at
/// actions that exist (the calendar writes audit their own refusals, from
/// inside a feature that is on).
///
/// Merged inside [`client_router`], so bearer auth still runs first: a caller
/// with no credential gets the 401 and learns nothing about the flag.
fn calendar_routes(state: ApiState) -> Router<ApiState> {
    Router::new()
        // Calendar ghosts: the timeline read and the user's word on one ghost
        // (dismissed, going, a corrected end). HUMAN DOOR ONLY in both
        // directions: the agent door has no timeline route.
        .route("/client/timeline", get(handlers::get_timeline))
        .route("/client/timeline/ghosts/{id}", patch(handlers::patch_ghost))
        // Start a Google Calendar sync now (the calendar page opening), at
        // most once per account per 30 seconds. HUMAN DOOR ONLY: the agent
        // door neither reads the calendar nor spends its quota.
        .route("/client/timeline/refresh", post(handlers::refresh_timeline))
        // Connect Google Calendar on a SELF-HOST: the daemon runs the consent
        // `squelchd auth --calendar` runs, on a loopback listener, and answers
        // the consent URL. 409 on hosted, where the control plane runs it.
        // HUMAN DOOR ONLY: the agent door can neither grant nor read calendar.
        .route(
            "/client/calendar/connect",
            post(calendar_connect::connect_calendar),
        )
        // The user's own create, edit and delete of one event on their Google
        // Calendar: Google first, then the local row, each attempt audited.
        // HUMAN DOOR ONLY: the agent door and the in-app assistant can neither
        // read nor write a calendar.
        .route(
            "/client/calendar/events",
            post(calendar_write::create_event),
        )
        .route(
            "/client/calendar/events/{calendar_id}/{id}",
            patch(calendar_write::edit_event).delete(calendar_write::delete_event),
        )
        // The event pane's guest suggestion: people the user has written to
        // whose name a title mentions. A read, and HUMAN DOOR ONLY: it is a
        // lookup into who the user writes to, which the agent door, the agent
        // routes and the assistant relay never get.
        .route(
            "/client/calendar/people",
            get(handlers::get_calendar_people),
        )
        // The user's correction of a person's inferred timezone (docs/
        // CALENDAR.md §7). A local write, human door only, for the reason the
        // contact card is; part of the calendar feature, so it is here. The
        // inference itself runs at ingest whatever the flag says, and
        // `GET /client/person` serves the zone only while the flag is on.
        .route(
            "/client/person/timezone",
            put(handlers::put_person_timezone),
        )
        .route_layer(middleware::from_fn_with_state(state, require_calendar))
}

/// The `calendar` gate itself. See [`calendar_routes`].
async fn require_calendar(
    axum::extract::State(state): axum::extract::State<ApiState>,
    req: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if !state.features().enabled(squelch_core::features::CALENDAR) {
        return ApiError::not_found().into_response();
    }
    next.run(req).await
}
