//! squelch-control: the hosted signup control plane (Passband hosted tier).
//!
//! It runs on AWS (ECS Fargate), holds the confidential web OAuth client, and turns one
//! invite code plus one Google consent into one provisioned tenant daemon, which
//! runs as its own pod. The provisioning itself belongs to `squelch-warden`, and
//! this crate only ever talks to it over the two-call wire contract spelled out
//! in [`warden`].
//!
//! THE TRUST SPLIT IS THE WHOLE DESIGN, and every module here exists to keep one
//! half of it:
//!
//! - This process holds the OAuth client secret. It holds no age identity, and
//!   as of wire v2 no long-lived recipient either: the warden mints an identity
//!   PER TENANT and answers the first provisioning call with the public half, so
//!   what this process can do is seal one tenant's token to one key it cannot
//!   open, for the length of one request.
//! - A plaintext refresh token exists ONLY in memory, between the token exchange
//!   returning and [`seal`] encrypting it. It is never written to the control
//!   store, never logged at any level, and never rendered into a page.
//! - The warden receives ciphertext (age ASCII armor) and writes it verbatim
//!   into that tenant's Secret. Only that tenant's daemon, whose pod mounts the
//!   matching identity, can decrypt, and it opens nothing else.
//!
//! PRIVACY, enforced by review of every `tracing` call in this crate: invite
//! codes, their hashes, pairing codes, OAuth codes, `state`, PKCE verifiers,
//! session ids, cookie MACs, the warden bearer, the admin token, the Resend
//! key, and access/refresh tokens NEVER reach a log line. Labels, account
//! emails, statuses, and counts may.
//!
//! A WAITLIST ADDRESS MAY NOT. It belongs to somebody who is not a tenant and
//! has consented to nothing but being told when there is room, so the waitlist
//! and admin paths log the ROW ID and never the address they hold.

pub mod activation;
pub mod admin;
pub mod alert;
mod appearance;
pub mod bifrost;
pub mod config;
pub mod cookie;
pub mod handlers;
pub mod import;
pub mod invites;
pub mod labels;
pub mod oauth;
pub mod pages;
pub mod pg_tls;
pub mod posthog;
pub mod ratelimit;
pub mod reconnect;
pub mod resend;
pub mod seal;
pub mod sessions;
pub mod share;
pub mod state;
pub mod store;
pub mod tenant;
pub mod warden;
pub mod workos;

pub use config::{Config, ConfigError};
pub use state::{ControlState, StateError};
pub use store::ControlStore;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post},
};

/// Build the control-plane router.
///
/// Seven surfaces, seven budgets, because a 429 costs a different thing on each
/// (the shape is stolen from `squelch-broker`, which learned it the same way):
///
/// - `GET /` is a page. Browsers prefetch it and link scanners fetch it, and
///   that traffic must not spend the budget signup needs.
/// - `POST /signup` is tight. It validates an invite code, so it is the one
///   route where a stranger can guess at a secret, and a real human posts it
///   once.
/// - `GET /console/auth`, `GET /app/auth`, `GET /reconnect` and (when WorkOS is
///   configured) `GET /agent/auth` are TIGHTER, and share ONE bucket. They are
///   the only routes that open a server-side session with nothing presented at
///   all (an `external_auth_id` is a string anybody can make up, so it counts
///   as nothing), which makes them the cheapest things here to loop; a real
///   human hits one of them once per sign in, and anything sustained above the
///   budget is somebody walking the label space or flooding the session table.
///   They share a bucket because they are the same errand through the same
///   consent: a budget each would let one client spend four times by rotating
///   them. The pair used to share signup's budget, which meant login traffic
///   could spend it.
/// - `GET /oauth/callback` is the most generous. Refusing it destroys a consent
///   the user has ALREADY granted at Google, which they cannot grant twice
///   without walking the whole flow again. All three flows come back through it.
///   `POST /agent/confirm` (mounted with WorkOS) shares this bucket for the
///   same reason: it is the agent login's second half, after Google.
/// - `POST /waitlist` is small. It is a stranger-facing WRITE, a person joins
///   once, and everything above the budget is rows of junk for an operator to
///   read past; a real submitter who is refused loses one retry, because the
///   answer is the same 200 whether the address was new or not.
/// - `POST /tenant/invite` is LOOSE, and it is the only budget here set by who
///   the caller is rather than by what the route costs: every hosted daemon
///   reaches this listener from one cluster's handful of egress addresses, so a
///   tight per-client bucket would meter the whole fleet as one client. Its
///   real limit is the per-tenant quota in the store, which counts the right
///   thing. See [`tenant`]. `GET /tenant/features` shares the bucket: the same
///   callers, asking once every few minutes each.
/// - `POST /admin/login` is tight and in its OWN bucket, for the reason
///   `/console/auth` has one: it is the second route where a stranger guesses at
///   a secret, and it must not be able to spend the budget the dashboard needs.
/// - The admin dashboard and its two buttons are generous. One operator with a
///   cookie, clicking through a list that re-renders after every approval.
///   `/admin/features` sits in the same bucket behind the same cookie.
///
/// THE LAST THREE ARE MOUNTED ONLY WHEN THE WAITLIST IS CONFIGURED. With the
/// feature off they do not exist, so an unconfigured deployment answers 404 at
/// an admin URL rather than 403: there is no door to knock on, and no admin
/// token for a stranger to spend their bucket guessing at.
///
/// `/healthz` sits outside every layer so liveness answers while a client is
/// throttled.
pub fn router(state: ControlState) -> Router {
    let form = Router::new()
        .route("/", get(handlers::signup_form))
        .route("/reconnect/status", get(reconnect::status))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            ratelimit::limit_page,
        ))
        .with_state(state.clone());

    let signup = Router::new()
        .route("/signup", post(handlers::signup))
        .layer(DefaultBodyLimit::max(handlers::MAX_BODY))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            ratelimit::limit_signup,
        ))
        .with_state(state.clone());

    // ALL THE UNAUTHENTICATED CONSENT HOPS, on ONE bucket. They are the same
    // errand through the same consent, and a limiter each would let the same
    // client spend once per route by rotating them.
    //
    // `/reconnect` belongs here rather than with signup despite being a grant:
    // what this bucket rations is the ability of a stranger to start consent
    // hops, and a reconnect is reachable with no credential at all.
    //
    // `/agent/auth` is MOUNTED ONLY WHEN WORKOS IS CONFIGURED, like the
    // waitlist below: with the feature off there is no door (a 404), so a
    // deployment that cannot complete an AuthKit login never opens a Google
    // consent on AuthKit's behalf. When it is mounted it is this bucket's
    // fourth route rather than a bucket of its own, because it opens exactly
    // the session the app login opens and is exactly as cheap to loop.
    let mut console = Router::new()
        .route("/console/auth", get(handlers::console_auth))
        .route("/app/auth", get(handlers::app_auth))
        .route("/reconnect", get(handlers::reconnect_start));
    if state.config().workos.is_some() {
        console = console.route("/agent/auth", get(handlers::agent_auth));
    }
    let console = console
        .layer(middleware::from_fn_with_state(
            state.clone(),
            ratelimit::limit_console_auth,
        ))
        .with_state(state.clone());

    // `POST /agent/confirm` rides the CALLBACK's bucket, for the callback's
    // reason: it is the second half of a flow whose Google consent has ALREADY
    // been given, so refusing it costs a person something they have to walk
    // the whole flow again to get back. It opens no session (it can only spend
    // one the callback parked, under a cookie and a CSRF token a stranger does
    // not have), so it is not the cheap-to-loop thing the login bucket
    // rations. Mounted only with WorkOS configured, like `/agent/auth`.
    let mut callback = Router::new().route("/oauth/callback", get(handlers::oauth_callback));
    if state.config().workos.is_some() {
        // The GET renders the parked page and the POST answers it. The GET is
        // here for the POST's reason, plus one: it is what a refresh asks for,
        // and a person refreshing a page they already consented their way to
        // must not meet a 429. It opens nothing and spends nothing.
        callback = callback.route(
            "/agent/confirm",
            get(handlers::agent_confirm_page)
                .post(handlers::agent_confirm)
                .layer(DefaultBodyLimit::max(handlers::MAX_BODY)),
        );
    }
    let callback = callback
        .layer(middleware::from_fn_with_state(
            state.clone(),
            ratelimit::limit_callback,
        ))
        .with_state(state.clone());

    // MOUNTED UNCONDITIONALLY, unlike the waitlist block below, and the route
    // itself answers 503 when the deployment has no invite policy. A daemon
    // that gets a 404 here cannot tell "this control plane is too old to share"
    // from "the URL is wrong", and the daemon's own copy has to tell a user
    // which it is.
    let tenant = Router::new()
        .route("/tenant/invite", post(tenant::invite))
        // The same door and the same bucket: one credential, one kind of
        // caller. A daemon asks this every few minutes, which the bucket's
        // size already assumes (see `TENANT_REQUESTS_PER_MINUTE`).
        .route("/tenant/features", get(tenant::features))
        .layer(DefaultBodyLimit::max(handlers::MAX_BODY))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            ratelimit::limit_tenant,
        ))
        .with_state(state.clone());

    let accounts = Router::new()
        .merge(form)
        .merge(signup)
        .merge(console)
        .merge(callback)
        .layer(middleware::from_fn_with_state(
            !state.config().is_insecure(),
            appearance::apply,
        ));
    let app = Router::new()
        .route("/healthz", get(handlers::healthz))
        .route(pages::FONT_PATH, get(pages::newsreader))
        .merge(accounts)
        .merge(tenant);

    if state.config().waitlist.is_none() {
        return app;
    }

    let waitlist = Router::new()
        .route(
            "/waitlist",
            post(handlers::waitlist).options(handlers::waitlist_preflight),
        )
        .layer(DefaultBodyLimit::max(handlers::MAX_BODY))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            ratelimit::limit_waitlist,
        ))
        // OUTERMOST, so the throttle's 429 and the body limit's 413 carry the
        // CORS headers too. A refusal a browser cannot read is a refusal the
        // form has to report as a network failure.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            handlers::waitlist_cors,
        ))
        .with_state(state.clone());

    let admin_login = Router::new()
        .route("/admin/login", post(admin::login))
        .layer(DefaultBodyLimit::max(handlers::MAX_BODY))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            ratelimit::limit_admin_login,
        ))
        .with_state(state.clone());

    let admin = Router::new()
        .route("/admin", get(admin::page))
        .route("/admin/invite", post(admin::invite))
        .route("/admin/approve", post(admin::approve))
        .route("/admin/send", post(admin::send))
        .route("/admin/delete", post(admin::delete))
        .route("/admin/logout", post(admin::logout))
        .layer(DefaultBodyLimit::max(handlers::MAX_BODY))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            ratelimit::limit_admin,
        ))
        .with_state(state.clone());

    // The same door and the same bucket as the dashboard, split out only for
    // its body limit: this form carries a field per ticked box and outgrows
    // the ceiling every other form here fits under.
    let admin_features = Router::new()
        .route(
            "/admin/features",
            get(admin::features_page).post(admin::features_save),
        )
        .layer(DefaultBodyLimit::max(admin::MAX_FEATURES_BODY))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            ratelimit::limit_admin,
        ))
        .with_state(state.clone());

    app.merge(waitlist)
        .merge(admin_login)
        .merge(admin)
        .merge(admin_features)
}
