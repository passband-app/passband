//! Shared handler state for the human door.

use std::sync::Arc;

use squelch_core::config::{Config, CredentialBackend, OAuthClientConfig, Stage2CapSources};
use squelch_core::credentials::{CredentialKind, CredentialStore};
use squelch_core::features::Features;
use squelch_core::store::SqliteStore;
use squelch_core::triage::fye_headline::HeadlineClient;
use squelch_core::triage::rule_infer::RuleInferClient;
use squelch_core::triage::rule_suggest::RuleSuggestClient;
use squelch_core::triage::smart_clear::SmartClearClient;
use squelch_core::triage::unsub_page::UnsubPageClient;
use squelch_core::types::AccountId;

/// The agent door's mode as the console describes it. Mirrors squelch-mcp's
/// `AgentDoorAuth` without depending on it (the human door does not link the
/// agent door's crate), carrying only what the page shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AgentDoorStatus {
    /// No agent sign-in configured: the self-host door, Host guard only.
    #[default]
    Open,
    /// OAuth sign-in configured; agents connect at this public URL.
    OAuth { public_url: String },
    /// Half-set or invalid settings: the door refuses everything (503).
    Unavailable,
}
/// A board hash and the headline written for it (`None`: nothing to say).
pub(crate) type HeadlineCacheEntry = (u64, Option<String>);

/// State threaded through every `/client/*` handler and the auth middleware.
/// Cheap to clone: `Arc`s plus small copies.
#[derive(Clone)]
pub struct ApiState {
    pub(crate) store: Arc<SqliteStore>,
    pub(crate) account_id: AccountId,
    pub(crate) triage_config: squelch_core::triage::agent_config::AgentTriageConfig,
    /// The MASTER token from `SQUELCH_API_TOKEN`, checked before any issued
    /// device token and first-class forever: it is the self-host operator's way
    /// in when the store holds no usable credential, including the store whose
    /// last device token was just revoked.
    ///
    /// `None` when the variable is unset or blank. That is NOT a refusal to
    /// serve: the door comes up and every request 401s until
    /// `squelchd token issue` or a pairing claim mints a device token. An
    /// exported-but-empty variable must not become a credential the door would
    /// accept, and must not keep the door shut either.
    pub(crate) master_token: Option<Arc<str>>,
    /// The WRITE-bound credential store, present only when write credentials are
    /// configured. The ONLY handle to the write token in the process; handlers
    /// load it per-request and never retain it. `None` => actions return 403.
    pub(crate) write_creds: Option<Arc<dyn CredentialStore>>,
    /// Gmail API base override for the write client. Tests only; production
    /// leaves it `None` so live traffic always hits the real API.
    pub(crate) write_api_base: Option<String>,
    /// The ONE HTTP client every Gmail write goes through, handed to each
    /// `GmailWriteClient` the action handlers make. Built per action instead,
    /// as it was, each send or archive paid a fresh rustls config and a fresh
    /// TCP + TLS handshake to Gmail and discarded the warm connection of the
    /// action before it. Built LAZILY, on the first write: a daemon with no
    /// write credential never pays for the rustls config, and `new` stays
    /// infallible. Behind an `Arc` because axum clones this state per request
    /// and every clone must share the one pool, not initialise its own.
    pub(crate) write_http: Arc<std::sync::OnceLock<reqwest::Client>>,
    /// Per-MTok input price (USD) behind `est_cost_usd_today` in
    /// `/client/stats`. Wire the operator's value in with
    /// [`ApiState::with_stage2_prices`].
    pub(crate) stage2_price_in_per_mtok: f64,
    /// Per-MTok output price (USD) for `est_cost_usd_today`.
    pub(crate) stage2_price_out_per_mtok: f64,
    /// The configured Stage-2 model id, surfaced as a label on `/client/usage`.
    pub(crate) stage2_model: Arc<str>,
    /// The configured Stage-2 provider label, surfaced on `/client/usage`.
    /// `None` when not explicitly configured.
    pub(crate) stage2_provider: Option<Arc<str>>,
    /// The CONFIG/ENV-layer Stage-2 daily caps (already env-folded), reported by
    /// `/client/triage-config` when no runtime override row exists.
    pub(crate) stage2_thread_daily_cap: u32,
    pub(crate) stage2_sender_daily_cap: u32,
    pub(crate) stage2_global_daily_cap: u32,
    /// Whether each config/env-layer cap came from the built-in default or from
    /// config/env, so triage-config can report "default" vs "config" (the
    /// "override" case is decided at read time from `app_settings`).
    pub(crate) stage2_cap_sources: Stage2CapSources,
    /// The configured Stage-1 (small) model id, surfaced on
    /// `/client/triage-config` and `/client/usage`.
    pub(crate) stage1_model: Arc<str>,
    /// Per-MTok Stage-1 prices for the triage-config estimator + usage cost.
    pub(crate) stage1_price_in_per_mtok: f64,
    pub(crate) stage1_price_out_per_mtok: f64,
    /// The CONFIG/ENV-layer Stage-1 GLOBAL daily cap (Stage-1's only scope),
    /// reported by `/client/triage-config` when no runtime override row exists.
    pub(crate) stage1_global_daily_cap: u32,
    /// The configured NOTIFY model id, surfaced as the label on `/client/usage`'s
    /// `notify` category.
    pub(crate) notify_model: Arc<str>,
    /// Per-MTok prices of [`ApiState::notify_model`]. THEIR OWN NUMBERS: the
    /// fast lane is the one ledger category that runs neither the Stage-2 model
    /// nor the Stage-1 one, so pricing it off either would misreport the lane by
    /// the whole ratio between a small model and a capable one.
    pub(crate) notify_price_in_per_mtok: f64,
    pub(crate) notify_price_out_per_mtok: f64,
    /// The configured SMART CLEAR model id and its per-MTok prices: the label
    /// and the fourth pricing arm on `/client/usage`'s `smart_clear` category.
    /// Its own numbers for the reason the notify lane has its own: it runs a
    /// model that is neither stage's and not the notify model either.
    pub(crate) smart_clear_model: Arc<str>,
    pub(crate) smart_clear_price_in_per_mtok: f64,
    pub(crate) smart_clear_price_out_per_mtok: f64,
    /// The TRANSLATE model and its per-MTok prices (docs/TRANSLATION.md),
    /// which `/client/usage` costs the `translate` category with. Its own arm
    /// for the notify arm's reason: it is neither Stage-1's model nor the
    /// notify model, so either one's prices would misreport it.
    pub(crate) translate_model: Arc<str>,
    pub(crate) translate_price_in_per_mtok: f64,
    pub(crate) translate_price_out_per_mtok: f64,
    /// Whether a translator will ever work a queued translation (translation
    /// on and an LLM configured, [`squelch_core::config::Config::translation_active`]).
    /// Without one, `GET /client/thread/{id}` serves no `pending` or
    /// `deferred` translation (nothing would ever finish it, and the reader
    /// would say "Translating..." for good), and the translate POST promotes
    /// nothing. Default `true`, matching `[translate].enabled`.
    pub(crate) translate_active: bool,
    /// Manual-refresh signal shared with the Gmail sync loop, fired by
    /// `POST /client/refresh`. `None` when no sync loop is wired in, which the
    /// endpoint reports as `triggered: false` rather than faking a poke.
    pub(crate) refresh: Option<Arc<tokio::sync::Notify>>,
    /// HOW MANY `GET /client/events` STREAMS ARE OPEN, counted by the handler
    /// for the life of each response body and shared with the sync loop
    /// through [`Self::with_event_listeners`], whose poll lane stays at its
    /// fast interval while the count is nonzero. Private to this state when
    /// nothing is wired in; the count is still kept, nobody reads it.
    pub(crate) event_listeners: Arc<std::sync::atomic::AtomicUsize>,
    /// On-demand spam-fetch signal shared with the Gmail sync loop, fired by
    /// `POST /client/spam/refresh`. `None` when no sync loop is wired in, which
    /// the endpoint reports as `triggered: false` rather than pretending.
    pub(crate) spam_refresh: Option<Arc<tokio::sync::Notify>>,
    /// How the agent search surfaces archive mail: the configured
    /// `sync.archive_days` and, once [`Self::with_refine_poke`] is called, the
    /// refine lane's poke. The agent door is handed the SAME value
    /// ([`Self::archive_surfacing`]) so the two cannot disagree.
    pub(crate) archive_surfacing: squelch_core::store::archive_surface::ArchiveSurfacing,
    /// Wake channel for `GET /client/events`, the SAME sender attached to the
    /// store. THE PAYLOAD IS ONLY A HINT — the `events` table is the truth and
    /// every connection re-reads past its own cursor — so a missed or lagged
    /// message costs latency, never an event. `None` => the live path never
    /// fires; replay still works.
    ///
    /// [`SqliteStore::attach_event_notifier`]: squelch_core::store::SqliteStore::attach_event_notifier
    pub(crate) event_notifier: Option<tokio::sync::broadcast::Sender<i64>>,
    /// "Show this draft" requests from the agent door (`open_draft`), carried
    /// to every connected Mac app that asked for them on its event stream
    /// (`GET /client/events?opens=1`). The payload is a draft id.
    ///
    /// DELIBERATELY NOT THE EVENTS LOG. Nothing is persisted and nothing is
    /// replayed: a request to put something on screen is about now, and a Mac
    /// that connects an hour later must not have a window thrown at it. Its
    /// `receiver_count` is also how the agent learns whether anything was
    /// listening.
    pub(crate) draft_opens: tokio::sync::broadcast::Sender<i64>,
    /// When the last open request was let through. HERE, shared by every
    /// clone of the state, rather than on the mailer: it rations the owner's
    /// screen, of which there is one per mailbox however many mailers exist.
    pub(crate) draft_open_clock: Arc<std::sync::Mutex<Option<std::time::Instant>>>,
    /// Smart re-notify (issue #625, [`crate::renotify`]): how it behaves, the
    /// channel its banners reach the Mac on, and the wake the event stream
    /// pokes when a Mac arrives. See the module for the contract.
    pub(crate) renotify: crate::renotify::RenotifyShared,
    /// The same requests, for the owner's PHONE: handed to the APNs pusher
    /// (see [`squelch_core::push::DraftPing`]), which pushes them to every
    /// registered iPhone. Not durable either, for the same reason as
    /// `draft_opens`; with no pusher running (no relay configured) a send
    /// finds no receiver and is simply the answer "no phone was told".
    pub(crate) draft_pings: tokio::sync::broadcast::Sender<squelch_core::push::DraftPing>,
    /// What the phone has already been told; see [`DraftPingLedger`].
    pub(crate) draft_pinged: Arc<std::sync::Mutex<DraftPingLedger>>,
    /// The daemon's shutdown signal. An SSE stream is infinite and axum's
    /// graceful shutdown waits for open connections, so without this one
    /// resident client would hold squelchd open forever.
    pub(crate) shutdown: Option<tokio::sync::watch::Receiver<bool>>,
    /// THE SCHEDULED-SEND WORKER'S WAKE: a row may just have been scheduled
    /// or pulled forward. Poked by `POST /client/scheduled` and
    /// `POST /client/scheduled/{id}/now` after their store write commits, and
    /// awaited by [`crate::scheduled_send_worker`], which otherwise sleeps
    /// until the next `send_at` it knows of. Always present: the worker and
    /// the handlers share this state.
    pub(crate) scheduled_wake: Arc<tokio::sync::Notify>,
    /// TEST HOOK: added to the wall clock the scheduled-send worker reads, so
    /// a test can step past a grace or a give-up without sleeping through it.
    /// Zero in production; nothing but the worker reads it.
    pub(crate) worker_clock_skew: chrono::Duration,
    /// PUBLIC base URL that reaches this daemon's `/t/:token` pixel route, from
    /// `[tracking] base_url` / `SQUELCH_TRACK_URL`. THE FEATURE FLAG for
    /// minting: `None` means no send is ever tracked, whatever the client asks
    /// for, and `/client/tracking-config` reports `configured: false`.
    pub(crate) tracking_base_url: Option<Arc<str>>,
    /// Slots for in-flight tracking-pixel writes. The pixel is one of the two
    /// unauthenticated routes that touch the store, so its share of the store
    /// mutex is capped rather than left to whoever floods it.
    pub(crate) pixel_slots: Arc<tokio::sync::Semaphore>,
    /// Slots for in-flight pairing claims — the OTHER unauthenticated route onto
    /// the same store mutex, capped for the same reason. Bounded by waiting
    /// rather than by refusing: a claim that failed fast because the daemon was
    /// busy would be a response that differs from the uniform 401.
    pub(crate) pair_slots: Arc<tokio::sync::Semaphore>,
    /// Slots for in-flight DEVICE-token verifications. The master-token compare
    /// is process-local and unbounded, but any `sqd_`-shaped guess from a caller
    /// with no credential reaches the store, so that branch queues here.
    pub(crate) device_auth_slots: Arc<tokio::sync::Semaphore>,
    /// The control plane's origin, from `[console] sso_url` /
    /// `SQUELCH_CONSOLE_SSO_URL`, behind the console's "Continue with Google"
    /// link.
    ///
    /// THE FEATURE FLAG for console SSO, and the whole self-host/hosted split in
    /// one field: hosted tenants are provisioned with it, a self-host never sets
    /// it, and without it the login page simply does not render the button. It is
    /// a LINK TARGET and nothing more: no trust flows from here, because whatever
    /// comes back is a pairing code the store adjudicates on its own terms.
    pub(crate) console_sso_url: Option<Arc<str>>,
    /// The calendar token resolver, which is also how `/client/stats` learns
    /// whether calendar is granted. `None` when no OAuth client is configured
    /// (no refresh is possible, so nothing could call the Calendar API) and in
    /// every test harness that did not attach one; `None` reports
    /// `calendar.granted: false`. HUMAN DOOR ONLY: nothing on `/mcp` or
    /// `/client/agent/*` reads it.
    pub(crate) calendar: Option<Arc<squelch_core::credentials::CalendarCredentials>>,
    /// The calendar sync loop's kick, behind `POST /client/timeline/refresh`,
    /// and when each account last kicked it. The stamps live here, shared by
    /// every clone of the state, so the 30 second floor holds across requests
    /// and clients: the calendar page opening must not become a way to hammer
    /// Google. `kick` is `None` when no loop runs (no OAuth client), and the
    /// endpoint then answers `kicked: false`. HUMAN DOOR ONLY.
    pub(crate) calendar_refresh: CalendarRefresh,
    /// Where the user's own calendar writes go: `None` is Google's Calendar
    /// API ([`squelch_core::gcal::client::API_BASE`]); a test's local fake
    /// otherwise. Never read from the environment or a config file.
    pub(crate) calendar_api_base: Option<Arc<str>>,
    /// How long one calendar write may take in all, Google's token refresh,
    /// the guest list's reread and the write itself together
    /// ([`crate::calendar_write::WRITE_DEADLINE`]). Past it the write is
    /// answered 504, "may have happened". Shorter only in tests.
    pub(crate) calendar_write_deadline: std::time::Duration,
    /// The ceilings a `/client/*` request runs under, by route class
    /// ([`crate::limits`]). The constants, except in tests.
    pub(crate) request_timeouts: crate::limits::RequestTimeouts,
    /// The door's store-call slots ([`crate::limits::STORE_CALL_SLOTS`]):
    /// how many of its store calls may be on blocking threads at once.
    /// Shared by every clone of the state.
    pub(crate) store_call_slots: Arc<tokio::sync::Semaphore>,
    /// The creates naming their own event id that are in flight, as
    /// (account, calendar, id), shared by every clone: a second create with
    /// the same key while the first is still running is refused at once
    /// rather than racing it to Google
    /// ([`crate::calendar_write::InFlight`]).
    pub(crate) calendar_creates_in_flight:
        Arc<std::sync::Mutex<std::collections::HashSet<(AccountId, String, String)>>>,
    /// The one Google Calendar consent this daemon may have in flight for the
    /// app's Connect button (`POST /client/calendar/connect`, self-host only),
    /// and the loopback listener that waits for its redirect. Shared by every
    /// clone, so a second Connect replaces the first. HUMAN DOOR ONLY.
    pub(crate) calendar_consent: Arc<crate::calendar_connect::CalendarConsent>,
    /// The loopback port that consent's listener binds: the headless CLI's
    /// fixed port, or `SQUELCH_AUTH_PORT`.
    pub(crate) calendar_consent_port: u16,
    /// Where that consent's exchange talks to Google. Google's pinned pair in
    /// production; see [`squelch_core::auth::GoogleEndpoints`].
    pub(crate) calendar_consent_endpoints: Arc<squelch_core::auth::GoogleEndpoints>,
    /// What the console tells the owner about the agent door (`/mcp`). Set by
    /// squelchd from the mode it resolved at startup; the default, Open, is
    /// the self-host posture and what every test that does not care gets.
    /// Display only: the door's actual auth lives in squelch-mcp, and nothing
    /// here can loosen it.
    pub(crate) agent_door: AgentDoorStatus,
    /// Who the audit log says performed this state's write actions.
    /// `client-api` for every request on the human door; `agent` on the copy
    /// handed to the agent door's mailer ([`ApiState::agent_mailer`]), so the
    /// shared send pipeline attributes an agent's send to the agent.
    pub(crate) audit_actor: &'static str,
    /// The process's metrics registry, read by `/client/stats` to answer
    /// whether this mailbox's Gmail credential is currently working.
    ///
    /// The same `Arc` the sync engine writes, so the answer is live rather than
    /// a copy taken at boot. `None` in tests and in any embedding that did not
    /// wire one, and absence is reported as "no answer" rather than as
    /// "connected": a door that cannot see the credential must not tell a
    /// client the mailbox is fine.
    pub(crate) sync_metrics: Option<Arc<squelch_core::metrics::SyncMetrics>>,
    /// The mailbox's Gmail quota budget, shared with the sync engines. `None`
    /// when no daemon wired one (tests); the write door is then unpaced.
    pub(crate) gmail_budget: Option<Arc<squelch_core::gmail_budget::GmailBudget>>,
    /// The account's mail provider once `handlers::write_client` has read it.
    /// It cannot change under a running door (nothing writes the column yet),
    /// so after the first successful read no write action pays a store call
    /// for it, or can fail on one. Shared by every clone of the state; a
    /// failed read is not cached.
    pub(crate) mail_provider: Arc<std::sync::OnceLock<squelch_core::mail::MailProvider>>,
    /// The IMAP sends in flight and those settled in the last two minutes,
    /// by request (final review N4; `crate::send_dedupe`). Shared by every
    /// clone of the state.
    pub(crate) imap_send_dedupe: Arc<crate::send_dedupe::SendDedupe>,
    /// Where an IMAP account's sealed `#imap` slot lives (backend, file and
    /// address; never its contents). The write door reads the slot per
    /// action, as it loads a Gmail token per action, so a `squelchd auth
    /// --imap` under a running daemon takes effect at once. `None` (tests,
    /// a door with no config): an IMAP account has no write credential.
    pub(crate) imap_write: Option<Arc<ImapWriteSlot>>,
    /// THE ESCAPE HATCH, from `[console] allow_insecure_cookie` /
    /// `SQUELCH_CONSOLE_ALLOW_INSECURE_COOKIE`: the operator's declaration that
    /// this console is served over plain http on an origin that is not loopback.
    ///
    /// OFF BY DEFAULT AND MEANT TO STAY THERE. The cookie IS a device token, so
    /// turning this on puts a live credential on the wire in the clear for
    /// anything on the path to take. It exists for the self-host running the
    /// console over plain http on a LAN, who would otherwise have a console that
    /// simply does not work (a browser will not store a `Secure` cookie from
    /// `http://`). When it is on, the login page says so where the user can see
    /// it, and the daemon warns at startup.
    ///
    /// IT IS NOT ONLY THE COOKIE, despite the name. It reaches the console as
    /// `Site::origin_is_https`, the one answer behind the cookie's `Secure`
    /// attribute, the scheme of the pairing deep link, the origin the CSRF check
    /// compares `Origin` against, and whether the SSO link can name a tenant.
    /// They move together on purpose; `crate::console`'s `Site::origin_is_https`
    /// has the reasoning and the cost of setting this on a real https origin.
    pub(crate) console_allow_insecure_cookie: bool,
    /// Per-client buckets over the two console routes that reach the store with
    /// no credential in front of them. See
    /// [`crate::console::CONSOLE_SIGNIN_REQUESTS_PER_MINUTE`].
    pub(crate) console_signin_limiter: Arc<std::sync::Mutex<ConsoleRateLimiter>>,
    /// The cheap-model handle behind rule-disposition inference: when a rule
    /// write omits `disposition`, the owner's `want` sentence is classified with
    /// this. `None` (no API key configured, and every test harness) means
    /// inference resolves to `filtered`, which is the safe default. HUMAN DOOR
    /// ONLY, like every other rule write.
    ///
    /// A route that AWAITS any of the model handles below before it answers
    /// owes `ROUTE_CLASSES` in `http_metrics.rs` a row, or its seconds are
    /// charted as the daemon's own.
    pub(crate) rule_infer: Option<Arc<RuleInferClient>>,
    /// The small model that suggests a rule sentence for the rule card. It
    /// reads the sender's recent subjects, so its output is only ever SHOWN,
    /// never saved on its own. `None` (no key, every test harness) means no
    /// suggestions. See [`squelch_core::triage::rule_suggest`].
    pub(crate) rule_suggest: Option<RuleSuggestClient>,
    /// The small model behind the sitrep's headline. `None` (no key, every
    /// test harness) means `/client/v2/fye/headline` answers null and the
    /// client keeps its own line. See [`squelch_core::triage::fye_headline`].
    pub(crate) fye_headline: Option<HeadlineClient>,
    /// The last headline and the board it was written for, by hash. The route
    /// is asked every time the sitrep settles, and the band is usually the
    /// one it already described; a miss is the only thing that spends. A
    /// `None` headline is cached too, so a board the model had nothing to say
    /// about is not asked again.
    pub(crate) fye_headline_cache: Arc<std::sync::Mutex<Option<HeadlineCacheEntry>>>,
    /// Headline calls ATTEMPTED today, as (UTC day, count): the half of the
    /// daily cap the usage ledger cannot see (timeouts report no usage) and
    /// cannot see in time (two calls racing the same ledger read).
    pub(crate) fye_headline_attempts: Arc<std::sync::Mutex<(String, u32)>>,
    /// The model behind smart clear's "has this expired" pass. `None` (no key,
    /// every test harness) means a press clears on stored facts alone and
    /// says so. See [`squelch_core::triage::smart_clear`].
    pub(crate) smart_clear: Option<SmartClearClient>,
    /// Smart clear model passes ATTEMPTED today, as (UTC day, count): the half
    /// of the daily cap the usage ledger cannot see, exactly as
    /// `fye_headline_attempts` is for the headline.
    pub(crate) smart_clear_attempts: Arc<std::sync::Mutex<(String, u32)>>,
    /// A smart clear press is in flight. One at a time: the second answers
    /// 429 without touching the store (see `triage::smart_clear`).
    pub(crate) smart_clear_running: Arc<std::sync::atomic::AtomicBool>,
    /// The hosted assistant relay: forwards the Passband app's Anthropic-shaped
    /// streaming request to the gateway with a daemon-held credential. `None`
    /// (no gateway + assistant key configured, and every test harness) means
    /// `/client/assistant/messages` answers 404 and the app falls back to BYOK.
    /// HUMAN DOOR ONLY: the request spends the tenant's assistant budget.
    pub(crate) assistant: Option<Arc<crate::assistant::AssistantRelay>>,
    /// Invite sharing: the control plane to mint a code at, and the share token
    /// to mint with. `None` (no `SQUELCH_CONTROL_URL` + `SQUELCH_CONTROL_TOKEN`,
    /// which is every self-host and every tenant nobody has run `share mint`
    /// for) means `/client/invites` reports `can_share: false` and the app never
    /// offers a button that could only fail. HUMAN DOOR ONLY: it spends a quota
    /// and sends mail as the user.
    pub(crate) sharing: Option<Arc<crate::sharing::Sharing>>,
    /// The hidden features this mailbox has been let into; see
    /// [`squelch_core::features`]. Nothing is on by default. `/client/stats`
    /// lists it for the apps, and a handler that gates on one asks
    /// `state.features().enabled(..)` at the moment it decides.
    pub(crate) features: Features,
    /// The carrier poller's kick handle plus the carriers it was built over,
    /// behind `POST /client/shipments/poll`. `None` when no poller runs — which
    /// is the resting state of every daemon with no carrier credentials — and
    /// the endpoint reports that as `kicked: false` rather than an error.
    pub(crate) shipment_poll: Option<ShipmentPoll>,
    /// The READ-SIDE listing policy for `GET /client/shipments`, derived from
    /// `[carriers]`: the silence window, and the retirement cap past which a
    /// carrier stops vouching for a row. The AGENT DOOR CARRIES THE SAME VALUE
    /// (`SquelchServer::with_shipment_policy`) and applies the same
    /// `ShipmentListPolicy::hides`, so the two doors cannot disagree about which
    /// packages exist. Defaults to the config default so a hand-built state
    /// still filters sensibly.
    pub(crate) shipment_policy: squelch_core::config::ShipmentListPolicy,
    /// Automatic unsubscribes: the jobs running in this process, their slots,
    /// and the small model that reads an unsubscribe page. See
    /// [`crate::unsub_job`]. HUMAN DOOR ONLY.
    pub(crate) unsub: crate::unsub_job::UnsubJobs,
}

/// The loopback port for the self-host calendar consent: `SQUELCH_AUTH_PORT`
/// when it is a port number, else the headless CLI's fixed port, so one SSH
/// tunnel serves both.
fn auth_port_from_env() -> u16 {
    let default = squelch_core::auth::DEFAULT_HEADLESS_PORT;
    match std::env::var("SQUELCH_AUTH_PORT") {
        Ok(raw) if !raw.trim().is_empty() => raw.trim().parse().unwrap_or_else(|_| {
            eprintln!(
                "squelch-api: SQUELCH_AUTH_PORT is not a port number; the calendar consent \
                 listener uses {default}."
            );
            default
        }),
        _ => default,
    }
}

/// The calendar sync kick and its per-account throttle. See
/// [`ApiState::calendar_refresh`].
#[derive(Clone, Default)]
pub(crate) struct CalendarRefresh {
    pub(crate) kick: Option<Arc<tokio::sync::Notify>>,
    pub(crate) last_kick:
        Arc<std::sync::Mutex<std::collections::HashMap<AccountId, std::time::Instant>>>,
}

impl CalendarRefresh {
    /// Start a calendar sync now, past the 30 second floor, and restart that
    /// floor. For a grant that just landed, which is worth a sync whenever the
    /// page last asked for one. `false` when no sync loop runs.
    pub(crate) fn kick_now(&self, account: AccountId) -> bool {
        let Some(kick) = &self.kick else {
            return false;
        };
        self.last_kick
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(account, std::time::Instant::now());
        kick.notify_one();
        true
    }
}

/// What the human door needs to force a carrier pass: the poller's
/// [`Notify`](tokio::sync::Notify) and the carrier slugs it was built over, so
/// the response can say WHICH carriers a kick will reach. Cheap to clone, like
/// every other field on [`ApiState`].
#[derive(Clone)]
pub(crate) struct ShipmentPoll {
    pub(crate) kick: Arc<tokio::sync::Notify>,
    /// Enabled carrier slugs, sorted and deduplicated at build time.
    pub(crate) carriers: Arc<[String]>,
}

/// A fixed-rate token bucket per client address, for the console's
/// unauthenticated sign-in paths.
///
/// A SIBLING of `squelch_control::ratelimit`'s, not a copy of convenience: the
/// arithmetic and the pruning are the same because they were right there, and
/// what differs is what it hangs off. In-memory, per-process abuse dampening
/// rather than a quota system: a restart forgives everyone.
///
/// WHO A "CLIENT" IS depends on how the router was served. The peer address is
/// read from axum's `ConnectInfo` when the listener attached one; when it did
/// not, every request meters under one shared key. That is coarse, never a
/// bypass: the worst case is one bucket for the whole process, and no caller can
/// choose their own key (no forwarded header is read here, because one that is
/// believed is a way to mint unlimited fresh identities).
pub(crate) struct ConsoleRateLimiter {
    buckets: std::collections::HashMap<std::net::IpAddr, (f64, std::time::Instant)>,
    capacity: f64,
    refill_per_sec: f64,
    last_prune: Option<std::time::Instant>,
}

/// Buckets idle longer than this are dropped on the next prune. An idle bucket
/// has refilled to capacity anyway, so forgetting it costs nothing.
const BUCKET_IDLE_TTL: std::time::Duration = std::time::Duration::from_secs(120);

/// A prune is a full-map scan, so it runs on a clock rather than per request.
const BUCKET_PRUNE_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

/// Hard ceiling on tracked buckets: a client rotating addresses faster than
/// [`BUCKET_IDLE_TTL`] leaves nothing to reclaim, so the TTL alone does not
/// bound the map. Forgetting a bucket only ever FORGIVES a client, so evicting
/// is safe; an unbounded map would not be.
const MAX_BUCKETS: usize = 16_384;

impl ConsoleRateLimiter {
    /// `per_minute` sustained requests, with a burst allowance of the same size.
    pub(crate) fn per_minute(per_minute: f64) -> Self {
        Self {
            buckets: std::collections::HashMap::new(),
            capacity: per_minute,
            refill_per_sec: per_minute / 60.0,
            last_prune: None,
        }
    }

    /// Charge one request against `ip`. `false` means the bucket is empty.
    pub(crate) fn check(&mut self, ip: std::net::IpAddr, now: std::time::Instant) -> bool {
        self.maintain(now);
        let capacity = self.capacity;
        let refill = self.refill_per_sec;
        let bucket = self.buckets.entry(ip).or_insert((capacity, now));
        // `saturating_duration_since` cannot panic on a non-monotonic clock; the
        // worst case is no refill for this request.
        let elapsed = now.saturating_duration_since(bucket.1).as_secs_f64();
        bucket.0 = (bucket.0 + elapsed * refill).min(capacity);
        bucket.1 = now;
        if bucket.0 >= 1.0 {
            bucket.0 -= 1.0;
            true
        } else {
            false
        }
    }

    /// Everything expensive lives here so `check`'s common path stays one hash
    /// lookup.
    fn maintain(&mut self, now: std::time::Instant) {
        let due = self
            .last_prune
            .is_none_or(|t| now.saturating_duration_since(t) >= BUCKET_PRUNE_EVERY);
        if due {
            self.buckets
                .retain(|_, (_, last)| now.saturating_duration_since(*last) < BUCKET_IDLE_TTL);
            self.last_prune = Some(now);
        }
        if self.buckets.len() >= MAX_BUCKETS {
            // Nothing left to reclaim on the clock, so drop the lot rather than
            // grow: every client is forgiven, which is the safe direction.
            self.buckets.clear();
        }
    }
}

/// Capacity of the `GET /client/events` wake channel. THE PAYLOAD IS ONLY A HINT
/// — every stream re-reads the `events` table past its own cursor — so this only
/// bounds how far a slow reader falls behind before taking the harmless Lagged
/// path.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Build the event wake channel and attach it to the store, returning the sender
/// for [`ApiState::with_event_notifier`] and for any other reader that wants to
/// `subscribe` (the APNs pusher does). One helper so every binary uses the same
/// capacity and the same single attach point.
///
/// Subscribe BEFORE handing the sender to the human door, so nothing appended
/// during startup is missed.
pub fn attach_event_channel(
    store: &SqliteStore,
) -> Result<tokio::sync::broadcast::Sender<i64>, squelch_core::CoreError> {
    let (tx, _) = tokio::sync::broadcast::channel::<i64>(EVENT_CHANNEL_CAPACITY);
    store.attach_event_notifier(tx.clone())?;
    Ok(tx)
}

/// Why [`ApiState`] could not be constructed.
///
/// A MISSING BEARER IS NOT IN HERE ANY MORE. The door used to refuse to build
/// without `SQUELCH_API_TOKEN`; it now serves and answers 401 until a credential
/// exists, because pairing is how the first credential arrives and pairing needs
/// a door to talk to. What is left is resolving the account, which is still a
/// store operation and still fallible, and, in [`ApiState::from_config`], a
/// credential backend this build cannot use, refused where its stores are
/// built.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error(transparent)]
    Core(#[from] squelch_core::CoreError),
}

impl ApiState {
    /// Build state from an explicit store + account + master token.
    ///
    /// INFALLIBLE, and deliberately so: there is no configuration this can be
    /// handed that should stop the human door coming up. A blank master token is
    /// simply no master token (see [`ApiState::master_token`]).
    pub fn new(
        store: Arc<SqliteStore>,
        account_id: AccountId,
        master_token: impl Into<String>,
    ) -> Self {
        let master_token = master_token.into();
        // Default prices so /client/stats always has a cost basis; the bin
        // overrides them from the loaded config.
        let s2 = squelch_core::config::Stage2Config::default();
        Self {
            store,
            account_id,
            triage_config: Default::default(),
            master_token: Some(master_token.trim())
                .filter(|t| !t.is_empty())
                .map(Arc::from),
            write_creds: None,
            write_api_base: None,
            write_http: Arc::new(std::sync::OnceLock::new()),
            stage2_price_in_per_mtok: s2.price_in_per_mtok,
            stage2_price_out_per_mtok: s2.price_out_per_mtok,
            stage2_model: Arc::from(s2.model.as_str()),
            stage2_provider: None,
            stage2_thread_daily_cap: s2.thread_daily_cap,
            stage2_sender_daily_cap: s2.sender_daily_cap,
            stage2_global_daily_cap: s2.global_daily_cap,
            stage2_cap_sources: Stage2CapSources::default(),
            stage1_model: {
                let s1 = squelch_core::config::Stage1Config::default();
                Arc::from(s1.model.as_str())
            },
            stage1_price_in_per_mtok: squelch_core::config::Stage1Config::default()
                .price_in_per_mtok,
            stage1_price_out_per_mtok: squelch_core::config::Stage1Config::default()
                .price_out_per_mtok,
            stage1_global_daily_cap: squelch_core::config::Stage1Config::default().global_daily_cap,
            notify_model: {
                let n = squelch_core::config::NotifyConfig::default();
                Arc::from(n.model.as_str())
            },
            notify_price_in_per_mtok: squelch_core::config::NotifyConfig::default()
                .price_in_per_mtok,
            notify_price_out_per_mtok: squelch_core::config::NotifyConfig::default()
                .price_out_per_mtok,
            smart_clear_model: {
                let s = squelch_core::config::SmartClearConfig::default();
                Arc::from(s.model.as_str())
            },
            smart_clear_price_in_per_mtok: squelch_core::config::SmartClearConfig::default()
                .price_in_per_mtok,
            smart_clear_price_out_per_mtok: squelch_core::config::SmartClearConfig::default()
                .price_out_per_mtok,
            translate_model: Arc::from(
                squelch_core::config::TranslateConfig::default()
                    .model
                    .as_str(),
            ),
            translate_price_in_per_mtok: squelch_core::config::TranslateConfig::default()
                .price_in_per_mtok,
            translate_price_out_per_mtok: squelch_core::config::TranslateConfig::default()
                .price_out_per_mtok,
            translate_active: true,
            refresh: None,
            event_listeners: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            spam_refresh: None,
            archive_surfacing: Default::default(),
            event_notifier: None,
            draft_opens: tokio::sync::broadcast::channel(16).0,
            draft_open_clock: Arc::new(std::sync::Mutex::new(None)),
            renotify: crate::renotify::RenotifyShared::default(),
            draft_pings: tokio::sync::broadcast::channel(16).0,
            draft_pinged: Default::default(),
            shutdown: None,
            scheduled_wake: Arc::new(tokio::sync::Notify::new()),
            worker_clock_skew: chrono::Duration::zero(),
            tracking_base_url: None,
            pixel_slots: Arc::new(tokio::sync::Semaphore::new(
                crate::tracking::PIXEL_CONCURRENCY,
            )),
            pair_slots: Arc::new(tokio::sync::Semaphore::new(crate::pair::PAIR_CONCURRENCY)),
            device_auth_slots: Arc::new(tokio::sync::Semaphore::new(
                crate::auth::DEVICE_AUTH_CONCURRENCY,
            )),
            console_sso_url: None,
            calendar: None,
            calendar_refresh: CalendarRefresh::default(),
            calendar_api_base: None,
            calendar_write_deadline: crate::calendar_write::WRITE_DEADLINE,
            request_timeouts: crate::limits::RequestTimeouts::default(),
            store_call_slots: Arc::new(tokio::sync::Semaphore::new(
                crate::limits::STORE_CALL_SLOTS,
            )),
            calendar_creates_in_flight: Default::default(),
            calendar_consent: Default::default(),
            calendar_consent_port: squelch_core::auth::DEFAULT_HEADLESS_PORT,
            calendar_consent_endpoints: Default::default(),
            agent_door: AgentDoorStatus::Open,
            audit_actor: crate::handlers::ACTION_ACTOR,
            sync_metrics: None,
            gmail_budget: None,
            mail_provider: Arc::new(std::sync::OnceLock::new()),
            imap_send_dedupe: Default::default(),
            imap_write: None,
            console_allow_insecure_cookie: false,
            console_signin_limiter: Arc::new(std::sync::Mutex::new(
                ConsoleRateLimiter::per_minute(crate::console::CONSOLE_SIGNIN_REQUESTS_PER_MINUTE),
            )),
            rule_infer: None,
            rule_suggest: None,
            fye_headline: None,
            fye_headline_cache: Arc::new(std::sync::Mutex::new(None)),
            fye_headline_attempts: Arc::new(std::sync::Mutex::new((String::new(), 0))),
            smart_clear: None,
            smart_clear_attempts: Arc::new(std::sync::Mutex::new((String::new(), 0))),
            smart_clear_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            assistant: None,
            sharing: None,
            features: Features::none(),
            shipment_poll: None,
            shipment_policy: squelch_core::config::ShipmentListPolicy::default(),
            unsub: crate::unsub_job::UnsubJobs::default(),
        }
    }

    /// The master token, if one is configured. `None` means the door accepts
    /// ONLY issued device tokens, which is the hosted posture and the resting
    /// state of a self-host that has finished pairing.
    pub(crate) fn master_token(&self) -> Option<&str> {
        self.master_token.as_deref()
    }

    /// Share the sync loop's manual-refresh [`Notify`](tokio::sync::Notify).
    /// Wire the SAME handle you pass to [`SyncEngine::with_refresh`]; without it
    /// `POST /client/refresh` is a no-op (`triggered: false`).
    ///
    /// [`SyncEngine::with_refresh`]: squelch_core::sync::SyncEngine::with_refresh
    pub fn with_refresh(mut self, refresh: Arc<tokio::sync::Notify>) -> Self {
        self.refresh = Some(refresh);
        self
    }

    /// Share the count of open event streams with the sync loop. Wire the
    /// SAME handle you pass to `SyncEngine::with_event_listeners`; without it
    /// the poll lane never learns a client has the app open, and its idle
    /// ladder (`sync.idle_poll_secs`, off by default) runs on as if nobody
    /// were watching.
    pub fn with_event_listeners(mut self, listeners: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        self.event_listeners = listeners;
        self
    }

    /// Share the sync loop's ON-DEMAND SPAM signal. Wire the SAME handle you
    /// pass to [`SyncEngine::with_spam_refresh`]; without it
    /// `POST /client/spam/refresh` is a no-op (`triggered: false`) and the spam
    /// page shows whatever was last fetched, which on a fresh install is
    /// nothing.
    ///
    /// SEPARATE FROM `with_refresh` on purpose. The spam folder is the one thing
    /// the poll loop deliberately does not track, and folding the two signals
    /// together would put its fetch back on every ordinary manual refresh.
    ///
    /// [`SyncEngine::with_spam_refresh`]: squelch_core::sync::SyncEngine::with_spam_refresh
    pub fn with_spam_refresh(mut self, spam_refresh: Arc<tokio::sync::Notify>) -> Self {
        self.spam_refresh = Some(spam_refresh);
        self
    }

    /// Share the sync loop's REFINE POKE. Wire the SAME handle you pass to
    /// `SyncEngine::with_refine_poke`; without it an agent search still queues
    /// access checks for the archive mail it matched but never waits for them.
    pub fn with_refine_poke(
        mut self,
        poke: squelch_core::store::archive_surface::RefinePoke,
    ) -> Self {
        self.archive_surfacing.poke = Some(poke);
        self
    }

    /// `sync.archive_days`, so the agent search can say how far back the
    /// index is meant to reach. 0 (the default here) reports no archive.
    pub fn with_archive_days(mut self, archive_days: u32) -> Self {
        self.archive_surfacing.archive_days = archive_days;
        self
    }

    /// What surfacing archive mail may spend (`triage.agent`), so a search
    /// never queues a check the worker would only park.
    pub fn with_archive_access_caps(
        mut self,
        caps: squelch_core::store::ArchiveAccessCaps,
    ) -> Self {
        self.archive_surfacing.caps = caps;
        self
    }

    /// How long an agent search waits on the checks it queued. Tests shorten it.
    pub fn with_surface_wait(
        mut self,
        wait: squelch_core::store::archive_surface::SurfaceWait,
    ) -> Self {
        self.archive_surfacing.wait = wait;
        self
    }

    /// The archive-surfacing settings, for the agent door to be built with.
    pub fn archive_surfacing(&self) -> squelch_core::store::archive_surface::ArchiveSurfacing {
        self.archive_surfacing.clone()
    }

    /// Share the carrier poller's kick [`Notify`](tokio::sync::Notify) and the
    /// carriers it was built over, enabling `POST /client/shipments/poll`. Wire
    /// the SAME handle the poller handed out
    /// ([`ShipmentPoller::kick_handle`]), with
    /// [`ShipmentPoller::enabled_carriers`] as `carriers`.
    ///
    /// NOT CALLING THIS IS A SUPPORTED STATE, not a misconfiguration: carrier
    /// polling is BYOK, so a daemon with no carrier credentials runs no poller
    /// and the endpoint answers `kicked: false` with an empty carrier list.
    ///
    /// The list is sorted and deduplicated here rather than trusted, so the
    /// response shape does not depend on how the caller assembled it.
    ///
    /// [`ShipmentPoller::kick_handle`]: squelch_core::carriers::poller::ShipmentPoller::kick_handle
    /// [`ShipmentPoller::enabled_carriers`]: squelch_core::carriers::poller::ShipmentPoller::enabled_carriers
    pub fn with_shipment_poll_kick(
        mut self,
        kick: Arc<tokio::sync::Notify>,
        carriers: Vec<String>,
    ) -> Self {
        let mut carriers = carriers;
        carriers.sort();
        carriers.dedup();
        self.shipment_poll = Some(ShipmentPoll {
            kick,
            carriers: carriers.into(),
        });
        self
    }

    /// Set the shipments LISTING policy from `[carriers]`. Pass the SAME value to
    /// the agent door's `SquelchServer::with_shipment_policy` — the two doors are
    /// supposed to see the same package list, and once did not.
    pub fn with_shipment_policy(
        mut self,
        policy: squelch_core::config::ShipmentListPolicy,
    ) -> Self {
        self.shipment_policy = policy;
        self
    }

    /// The listing policy this door is serving. Public so the process hosting
    /// BOTH doors can hand the agent door the human door's own value instead of
    /// resolving config twice and drifting.
    pub fn triage_ranking(&self) -> squelch_core::triage::agent_config::RankingConfig {
        self.triage_config.ranking.clone()
    }

    pub fn shipment_policy(&self) -> squelch_core::config::ShipmentListPolicy {
        self.shipment_policy
    }

    /// Share the event-notification broadcast so `GET /client/events` wakes
    /// instead of polling. Wire the SAME sender you pass to
    /// [`SqliteStore::attach_event_notifier`]; without it the SSE endpoint
    /// replays and holds the connection, it just never goes live.
    ///
    /// [`SqliteStore::attach_event_notifier`]: squelch_core::store::SqliteStore::attach_event_notifier
    /// The APNs pusher's end of the agent door's draft review requests (see
    /// [`squelch_core::push::Pusher::with_draft_pings`]).
    pub fn subscribe_draft_pings(
        &self,
    ) -> tokio::sync::broadcast::Receiver<squelch_core::push::DraftPing> {
        self.draft_pings.subscribe()
    }

    pub fn with_event_notifier(mut self, tx: tokio::sync::broadcast::Sender<i64>) -> Self {
        self.event_notifier = Some(tx);
        self
    }

    /// Share the process shutdown signal so open SSE streams end when the daemon
    /// stops. REQUIRED wherever `with_graceful_shutdown` is used: a never-ending
    /// stream otherwise keeps the server from finishing its drain.
    pub fn with_shutdown(mut self, shutdown: tokio::sync::watch::Receiver<bool>) -> Self {
        self.shutdown = Some(shutdown);
        self
    }

    /// Smart re-notify's tunables (`[renotify]`). Whether it runs at all is
    /// the `renotify` hidden feature.
    pub fn with_renotify_config(mut self, config: squelch_core::config::RenotifyConfig) -> Self {
        self.renotify = crate::renotify::RenotifyShared::new(config);
        self
    }

    /// The scheduled-send worker's wake (see the field), for a test that
    /// wants to see a handler poke it.
    pub fn scheduled_wake(&self) -> &Arc<tokio::sync::Notify> {
        &self.scheduled_wake
    }

    /// The event-notification broadcast, if one was wired in.
    pub(crate) fn event_notifier(&self) -> Option<&tokio::sync::broadcast::Sender<i64>> {
        self.event_notifier.as_ref()
    }

    /// Set the public base URL the read-tracking pixel is minted against. A
    /// blank value is treated as unset: an exported-but-empty setting must not
    /// put `/t/<token>` into outbound mail. A value without an http(s) scheme is
    /// unset too — it would ride into the HTML as a RELATIVE url and resolve
    /// against whatever the recipient's mail client happens to be, so a typo
    /// must disable tracking rather than mint a pixel that points anywhere.
    pub fn with_tracking_base_url(mut self, base_url: Option<String>) -> Self {
        self.tracking_base_url = base_url
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty())
            // The shared predicate: the opens-poller gate and the startup
            // posture report apply the same rule, so all three agree.
            .filter(|u| squelch_core::tracking::is_mintable_base_url(u))
            .map(|u| Arc::from(u.as_str()));
        self
    }

    /// The tracking base URL, already trimmed of a trailing slash. `None` =>
    /// tracking is not configured and no send may mint a token.
    pub(crate) fn tracking_base_url(&self) -> Option<&str> {
        self.tracking_base_url.as_deref()
    }

    /// Set the control plane's origin behind the console's Google sign-in link,
    /// from `[console] sso_url` / `SQUELCH_CONSOLE_SSO_URL`.
    ///
    /// Treated exactly like [`ApiState::with_tracking_base_url`]: blank is unset
    /// (an exported-but-empty variable must not become configuration), a trailing
    /// slash is trimmed, and a value with no http(s) scheme is unset, because it
    /// would ride into the page as a RELATIVE href and resolve against the tenant
    /// origin rather than the control plane.
    /// Wire the process's metrics registry so `/client/stats` can answer
    /// whether the Gmail credential is currently working.
    /// Share the mailbox's Gmail quota budget with the write door, so actions
    /// book against the same per-user limit the sync engines pace to.
    pub fn with_gmail_budget(
        mut self,
        budget: Arc<squelch_core::gmail_budget::GmailBudget>,
    ) -> Self {
        self.gmail_budget = Some(budget);
        self
    }

    pub(crate) fn gmail_budget(&self) -> Option<&Arc<squelch_core::gmail_budget::GmailBudget>> {
        self.gmail_budget.as_ref()
    }

    pub fn with_sync_metrics(mut self, metrics: Arc<squelch_core::metrics::SyncMetrics>) -> Self {
        self.sync_metrics = Some(metrics);
        self
    }

    pub fn with_console_sso_url(mut self, url: Option<String>) -> Self {
        self.console_sso_url = url
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty())
            .filter(|u| {
                let lower = u.to_ascii_lowercase();
                lower.starts_with("https://") || lower.starts_with("http://")
            })
            .map(|u| Arc::from(u.as_str()));
        self
    }

    /// Tell the console which mode the agent door is in. See
    /// [`AgentDoorStatus`].
    pub fn with_agent_door(mut self, status: AgentDoorStatus) -> Self {
        self.agent_door = status;
        self
    }

    pub(crate) fn agent_door(&self) -> &AgentDoorStatus {
        &self.agent_door
    }

    /// A copy of this state whose audit rows are written by `actor`.
    pub(crate) fn with_audit_actor(mut self, actor: &'static str) -> Self {
        self.audit_actor = actor;
        self
    }

    /// The control plane's origin, if console SSO is configured. `None` => the
    /// console renders the pasted-code form only, which is the self-host posture.
    pub(crate) fn sync_metrics(&self) -> Option<&Arc<squelch_core::metrics::SyncMetrics>> {
        self.sync_metrics.as_ref()
    }

    pub(crate) fn console_sso_url(&self) -> Option<&str> {
        self.console_sso_url.as_deref()
    }

    /// Attach the calendar token resolver. See [`ApiState::calendar`].
    pub fn with_calendar_credentials(
        mut self,
        calendar: squelch_core::credentials::CalendarCredentials,
    ) -> Self {
        self.calendar = Some(Arc::new(calendar));
        self
    }

    /// Attach the calendar sync loop's kick handle
    /// ([`CalendarSync::kick_handle`]), for `POST /client/timeline/refresh`.
    ///
    /// [`CalendarSync::kick_handle`]: squelch_core::gcal::sync::CalendarSync::kick_handle
    pub fn with_calendar_sync_kick(mut self, kick: Arc<tokio::sync::Notify>) -> Self {
        self.calendar_refresh.kick = Some(kick);
        self
    }

    /// Send the user's calendar writes to `base` instead of Google. TESTS
    /// ONLY, like [`Self::with_calendar_consent_endpoints`].
    #[doc(hidden)]
    pub fn with_calendar_api_base(mut self, base: impl Into<Arc<str>>) -> Self {
        self.calendar_api_base = Some(base.into());
        self
    }

    /// A shorter overall deadline on a calendar write. TESTS ONLY.
    #[doc(hidden)]
    pub fn with_calendar_write_deadline(mut self, deadline: std::time::Duration) -> Self {
        self.calendar_write_deadline = deadline;
        self
    }

    /// The ceilings a `/client/*` request runs under, for a test that wants
    /// to see one fire. The daemon runs [`crate::limits::RequestTimeouts::default`].
    pub fn with_request_timeouts(mut self, timeouts: crate::limits::RequestTimeouts) -> Self {
        self.request_timeouts = timeouts;
        self
    }

    /// How many of the door's store calls may be on blocking threads at
    /// once, for a test that wants to see the cap bite. The daemon runs
    /// [`crate::limits::STORE_CALL_SLOTS`].
    pub fn with_store_call_slots(mut self, max: usize) -> Self {
        self.store_call_slots = Arc::new(tokio::sync::Semaphore::new(max));
        self
    }

    /// Store-call slots not in use right now.
    pub fn store_call_slots_free(&self) -> usize {
        self.store_call_slots.available_permits()
    }

    /// The loopback port the self-host calendar consent's listener binds.
    /// `0` binds an ephemeral port (tests), and the consent URL then carries
    /// the port that was bound.
    pub fn with_calendar_consent_port(mut self, port: u16) -> Self {
        self.calendar_consent_port = port;
        self
    }

    /// Point the self-host calendar consent's exchange at `endpoints`. TESTS
    /// ONLY: production keeps Google's pinned pair, and nothing reads this
    /// from the environment or a config file.
    #[doc(hidden)]
    pub fn with_calendar_consent_endpoints(
        mut self,
        endpoints: squelch_core::auth::GoogleEndpoints,
    ) -> Self {
        self.calendar_consent_endpoints = Arc::new(endpoints);
        self
    }

    /// The calendar token resolver, if one is attached. Slice B's sync and
    /// `/client/stats` read the grant through this one object.
    pub fn calendar(&self) -> Option<&Arc<squelch_core::credentials::CalendarCredentials>> {
        self.calendar.as_ref()
    }

    /// Declare that this console is served over plain http, the LAN escape
    /// hatch. See [`ApiState::console_allow_insecure_cookie`] for what it moves,
    /// which is more than the cookie.
    pub fn with_console_allow_insecure_cookie(mut self, allow: bool) -> Self {
        self.console_allow_insecure_cookie = allow;
        self
    }

    /// Slots for concurrent tracking-pixel writes; see
    /// [`crate::tracking::PIXEL_CONCURRENCY`].
    pub(crate) fn pixel_slots(&self) -> &tokio::sync::Semaphore {
        &self.pixel_slots
    }

    /// Attach the cheap-model handle that infers a rule's disposition from the
    /// owner's plain-English `want`. `None` disables inference gracefully:
    /// rule writes that omit `disposition` are stored `filtered`, which is what
    /// the daemon does when no API key is configured.
    ///
    /// The classifier only ever sees the OWNER'S SENTENCE. See
    /// [`squelch_core::triage::rule_infer`].
    pub fn with_rule_inference(mut self, client: Option<RuleInferClient>) -> Self {
        self.rule_infer = client.map(Arc::new);
        self
    }

    /// Attach the rule suggester. `None` keeps `/client/rules/suggest` answering
    /// null.
    pub fn with_rule_suggestion(mut self, client: Option<RuleSuggestClient>) -> Self {
        self.rule_suggest = client;
        self
    }

    /// Attach the sitrep headline writer. `None` keeps
    /// `/client/v2/fye/headline` answering null.
    pub fn with_fye_headline(mut self, client: Option<HeadlineClient>) -> Self {
        self.fye_headline = client;
        self
    }

    /// Attach smart clear's model. `None` keeps
    /// `/client/v2/reading/smart_clear` clearing on stored facts alone.
    pub fn with_smart_clear(mut self, client: Option<SmartClearClient>) -> Self {
        self.smart_clear = client;
        self
    }

    /// Set the SMART CLEAR model + its per-MTok prices, which `/client/usage`
    /// costs the `smart_clear` category with. See
    /// [`ApiState::with_notify_config`] for why each model that is not a
    /// stage's gets its own entry point.
    pub fn with_smart_clear_config(
        mut self,
        model: impl Into<String>,
        price_in_per_mtok: f64,
        price_out_per_mtok: f64,
    ) -> Self {
        self.smart_clear_model = Arc::from(model.into().as_str());
        self.smart_clear_price_in_per_mtok = price_in_per_mtok;
        self.smart_clear_price_out_per_mtok = price_out_per_mtok;
        self
    }

    /// The small model that reads an unsubscribe page when a job needs one.
    /// `None` (no key, every test harness) leaves the no-model rule: done only
    /// when the page says so, and one action only when it is the obvious one.
    pub fn with_unsub_page_reader(mut self, client: Option<UnsubPageClient>) -> Self {
        self.unsub.page_reader = client.map(Arc::new);
        self
    }

    /// Where the automatic unsubscribe's browser rung rents its browser
    /// (`crate::unsub_browser`). `None` and that rung does not exist here.
    pub(crate) fn with_unsub_browser(
        mut self,
        browser: Option<crate::unsub_browser::Browserbase>,
    ) -> Self {
        self.unsub.browser = browser.map(Arc::new);
        self
    }

    /// TEST HOOK: let the automatic unsubscribe's fetcher reach loopback and
    /// any port, so a test can serve the sender's pages itself. Production
    /// never calls this; the SSRF guard in `unsub_fetch` is otherwise absolute.
    pub fn with_unsub_fetch_allow_local(mut self, allow: bool) -> Self {
        self.unsub.allow_local = allow;
        self
    }

    /// The rule-disposition classifier, if one is configured.
    pub(crate) fn rule_infer(&self) -> Option<&RuleInferClient> {
        self.rule_infer.as_deref()
    }

    /// Attach the assistant relay. `None` (no gateway + assistant key) keeps
    /// `/client/assistant/messages` a 404, which is what tells the app to fall
    /// back to its own BYOK path.
    pub fn with_assistant(mut self, relay: Option<crate::assistant::AssistantRelay>) -> Self {
        self.assistant = relay.map(Arc::new);
        self
    }

    /// The assistant relay, if one is configured.
    pub(crate) fn assistant(&self) -> Option<&Arc<crate::assistant::AssistantRelay>> {
        self.assistant.as_ref()
    }

    /// Point this daemon at a control plane it may mint invites against.
    pub fn with_sharing(mut self, sharing: Option<crate::sharing::Sharing>) -> Self {
        self.sharing = sharing.map(Arc::new);
        self
    }

    /// Use `features` as this daemon's hidden-feature set. The handle is
    /// shared, not copied: whatever refreshes it is seen here.
    pub fn with_features(mut self, features: Features) -> Self {
        self.features = features;
        self
    }

    /// The hidden features that are on. Clone it to hand a subsystem outside
    /// this crate the same live set.
    pub fn features(&self) -> &Features {
        &self.features
    }

    /// The loop that keeps [`Self::features`] in step with the control plane,
    /// or `None` when there is no control plane to ask (every self-host, where
    /// the only features on are the ones `SQUELCH_FEATURES` pins).
    pub fn feature_refresher(&self) -> Option<crate::sharing::FeatureRefresher> {
        Some(crate::sharing::FeatureRefresher {
            sharing: self.sharing.clone()?,
            features: self.features.clone(),
        })
    }

    /// The invite minter, if this daemon has one.
    pub(crate) fn sharing(&self) -> Option<&Arc<crate::sharing::Sharing>> {
        self.sharing.as_ref()
    }

    /// Set the Stage-2 model + provider labels surfaced on `/client/usage`, so
    /// the usage page shows what model produced the spend.
    pub fn with_agent_triage(
        mut self,
        config: squelch_core::triage::agent_config::AgentTriageConfig,
    ) -> Self {
        self.triage_config = config;
        self
    }

    pub fn with_stage2_model(mut self, model: impl Into<String>, provider: Option<String>) -> Self {
        self.stage2_model = Arc::from(model.into().as_str());
        self.stage2_provider = provider.map(|p| Arc::from(p.as_str()));
        self
    }

    /// Override the Stage-2 per-MTok prices behind `est_cost_usd_today`, so
    /// switching model reflects in the surfaced cost.
    pub fn with_stage2_prices(mut self, price_in_per_mtok: f64, price_out_per_mtok: f64) -> Self {
        self.stage2_price_in_per_mtok = price_in_per_mtok;
        self.stage2_price_out_per_mtok = price_out_per_mtok;
        self
    }

    /// Set the CONFIG/ENV-layer Stage-2 daily caps + their sources, which
    /// `/client/triage-config` reports until a runtime override row is written.
    pub fn with_stage2_caps(
        mut self,
        thread_daily_cap: u32,
        sender_daily_cap: u32,
        global_daily_cap: u32,
        sources: Stage2CapSources,
    ) -> Self {
        self.stage2_thread_daily_cap = thread_daily_cap;
        self.stage2_sender_daily_cap = sender_daily_cap;
        self.stage2_global_daily_cap = global_daily_cap;
        self.stage2_cap_sources = sources;
        self
    }

    /// Set the Stage-1 model + per-MTok prices + config/env GLOBAL daily cap
    /// surfaced on `/client/triage-config` and `/client/usage`. The cap SOURCE
    /// arrives separately, via [`ApiState::with_stage2_caps`].
    pub fn with_stage1_config(
        mut self,
        model: impl Into<String>,
        price_in_per_mtok: f64,
        price_out_per_mtok: f64,
        global_daily_cap: u32,
    ) -> Self {
        self.stage1_model = Arc::from(model.into().as_str());
        self.stage1_price_in_per_mtok = price_in_per_mtok;
        self.stage1_price_out_per_mtok = price_out_per_mtok;
        self.stage1_global_daily_cap = global_daily_cap;
        self
    }

    /// Set the NOTIFY model + its per-MTok prices, which `/client/usage` costs
    /// the `notify` category with. Its OWN entry point rather than a fourth and
    /// fifth argument on [`ApiState::with_stage1_config`], because the whole
    /// point of the category is that it is not Stage-1's model.
    pub fn with_notify_config(
        mut self,
        model: impl Into<String>,
        price_in_per_mtok: f64,
        price_out_per_mtok: f64,
    ) -> Self {
        self.notify_model = Arc::from(model.into().as_str());
        self.notify_price_in_per_mtok = price_in_per_mtok;
        self.notify_price_out_per_mtok = price_out_per_mtok;
        self
    }

    /// Set the TRANSLATE model + its per-MTok prices, which `/client/usage`
    /// costs the `translate` category with.
    pub fn with_translate_config(
        mut self,
        model: impl Into<String>,
        price_in_per_mtok: f64,
        price_out_per_mtok: f64,
    ) -> Self {
        self.translate_model = Arc::from(model.into().as_str());
        self.translate_price_in_per_mtok = price_in_per_mtok;
        self.translate_price_out_per_mtok = price_out_per_mtok;
        self
    }

    /// Whether a translator exists to finish queued translations (see the
    /// field). `from_config` sets it from the config.
    pub fn with_translate_active(mut self, active: bool) -> Self {
        self.translate_active = active;
        self
    }

    /// TEST HOOK: run the scheduled-send worker's clock `skew` ahead of the
    /// wall clock (see `worker_clock_skew`). Never used in production.
    #[doc(hidden)]
    pub fn with_worker_clock_skew(mut self, skew: chrono::Duration) -> Self {
        self.worker_clock_skew = skew;
        self
    }

    /// TEST HOOK: attach a write-credential store AND a mock Gmail API base so
    /// action handlers run end-to-end without live Gmail. Never used in production.
    #[doc(hidden)]
    pub fn with_write_test_harness(
        mut self,
        write_creds: Arc<dyn CredentialStore>,
        api_base: String,
    ) -> Self {
        self.write_creds = Some(write_creds);
        self.write_api_base = Some(api_base);
        self
    }

    /// Attach a WRITE-bound credential store, enabling the action endpoints.
    /// A distinct opt-in step so write capability is never wired in implicitly;
    /// without it the state has no path to any write token and actions 403.
    pub fn with_write_store(mut self, write_creds: Arc<dyn CredentialStore>) -> Self {
        self.write_creds = Some(write_creds);
        self
    }

    /// The ONLY place squelch-api constructs a write credential. Bound to
    /// [`CredentialKind::Write`], so it can never yield the read token, and
    /// built by the core's one constructor, so a backend this build cannot
    /// serve is refused here rather than on the first action.
    pub fn with_write_credentials(
        self,
        backend: CredentialBackend,
        account_email: String,
        credentials_path: std::path::PathBuf,
        client: OAuthClientConfig,
    ) -> Result<Self, StateError> {
        let creds = squelch_core::credentials::store_for_backend(
            backend,
            self.account_id,
            account_email,
            CredentialKind::Write,
            credentials_path,
            client,
        )?;
        Ok(self.with_write_store(creds))
    }

    /// Where this account's IMAP credential slot is, for the write door (see
    /// the field). Holds no secret: the slot is read per action.
    pub fn with_imap_write_slot(
        mut self,
        backend: CredentialBackend,
        account_email: String,
        credentials_path: std::path::PathBuf,
    ) -> Self {
        self.imap_write = Some(Arc::new(ImapWriteSlot {
            backend,
            account_email,
            credentials_path,
        }));
        self
    }

    /// The WRITE-bound credential store, if action capability is enabled.
    pub(crate) fn write_creds(&self) -> Option<&Arc<dyn CredentialStore>> {
        self.write_creds.as_ref()
    }

    /// The Gmail API base override (tests only); `None` in production.
    pub(crate) fn write_api_base(&self) -> Option<&str> {
        self.write_api_base.as_deref()
    }

    /// The shared Gmail write client, built on first use and the same one
    /// for every clone of this state. See the field.
    pub(crate) fn write_http(&self) -> reqwest::Client {
        self.write_http
            .get_or_init(crate::write::gmail::GmailWriteClient::build_http)
            .clone()
    }

    /// Build state, resolving the account email to an id and reading the master
    /// token from `SQUELCH_API_TOKEN`.
    ///
    /// An unset variable is a supported configuration, not an error: the door
    /// serves, every request 401s, and the operator mints the first credential
    /// with `squelchd token issue` or `squelchd pair`. Announcing that here
    /// rather than leaving the operator to discover it as a wall of 401s.
    pub fn from_env(store: Arc<SqliteStore>, account_email: &str) -> Result<Self, StateError> {
        let token = std::env::var("SQUELCH_API_TOKEN").unwrap_or_default();
        if token.trim().is_empty() {
            eprintln!(
                "squelch-api: SQUELCH_API_TOKEN is unset, so the human door accepts only issued \
                 device tokens; run `squelchd pair` (or `squelchd token issue`) to mint one."
            );
        }
        let account_id = store.ensure_account(account_email)?;
        // Console SSO is env-only and hosted-only: the warden sets it when it
        // provisions a tenant, and a self-host leaves it unset, which is what
        // keeps the Google button off a page that has no control plane behind it.
        // Sharing is env-only and hosted-only for the same reason console SSO
        // is: the warden sets both when it provisions a tenant, and a self-host
        // has no control plane to mint a code at. The pair is resolved together
        // (see `Sharing::new`), so a half-configured daemon is simply not
        // sharing rather than sharing badly.
        let sharing = crate::sharing::Sharing::new(
            std::env::var("SQUELCH_CONTROL_URL").unwrap_or_default(),
            std::env::var("SQUELCH_CONTROL_TOKEN").unwrap_or_default(),
        );
        Ok(Self::new(store, account_id, token)
            .with_console_sso_url(std::env::var("SQUELCH_CONSOLE_SSO_URL").ok())
            .with_calendar_consent_port(auth_port_from_env())
            .with_sharing(sharing)
            // Pinned from `SQUELCH_FEATURES`, which no hosted pod sets; what
            // the control plane grants arrives later, through the refresher.
            .with_features(Features::from_env()))
    }

    /// THE one way a binary turns a loaded [`Config`] into serving state:
    /// [`ApiState::from_env`] plus every config-derived knob (Stage-2 prices,
    /// model/provider labels, daily caps + their sources, the Stage-1 block) and
    /// the write credential. Both bins call this, so a knob added here reaches
    /// both instead of silently misconfiguring the one that was not updated.
    ///
    /// Action endpoints are enabled ONLY when OAuth client credentials are
    /// configured; the store built here is bound to [`CredentialKind::Write`]
    /// and the sync/triage read path never touches it. Without it, actions 403.
    ///
    /// Process-level plumbing stays with the caller: the manual-refresh
    /// [`Notify`](tokio::sync::Notify), the event notifier, and the shutdown
    /// watch are all shared with other tasks the caller owns.
    pub fn from_config(
        store: Arc<SqliteStore>,
        account_email: &str,
        cfg: &Config,
        cap_sources: Stage2CapSources,
    ) -> Result<Self, StateError> {
        let state = Self::from_env(store, account_email)?
            .with_stage2_prices(cfg.stage2.price_in_per_mtok, cfg.stage2.price_out_per_mtok)
            .with_agent_triage(cfg.triage.clone())
            .with_archive_days(cfg.sync.archive_days)
            .with_archive_access_caps((&cfg.triage.agent).into())
            .with_stage2_model(
                cfg.stage2.model.clone(),
                cfg.stage2.stage2_provider.map(|p| p.as_str().to_string()),
            )
            .with_stage2_caps(
                cfg.stage2.thread_daily_cap,
                cfg.stage2.sender_daily_cap,
                cfg.stage2.global_daily_cap,
                cap_sources,
            )
            .with_stage1_config(
                cfg.stage1.model.clone(),
                cfg.stage1.price_in_per_mtok,
                cfg.stage1.price_out_per_mtok,
                cfg.stage1.global_daily_cap,
            )
            // The notify fast lane's model and prices. Threaded here for the
            // same reason every other knob is: a price left at its default while
            // the model moved is how a ledger starts lying quietly.
            .with_notify_config(
                cfg.notify.model.clone(),
                cfg.notify.price_in_per_mtok,
                cfg.notify.price_out_per_mtok,
            )
            .with_renotify_config(cfg.renotify.clone())
            // Smart clear's own model and prices, for the same reason.
            .with_smart_clear_config(
                cfg.smart_clear.model.clone(),
                cfg.smart_clear.price_in_per_mtok,
                cfg.smart_clear.price_out_per_mtok,
            )
            .with_translate_config(
                cfg.translate.model.clone(),
                cfg.translate.price_in_per_mtok,
                cfg.translate.price_out_per_mtok,
            )
            .with_translate_active(cfg.translation_active())
            .with_tracking_base_url(cfg.tracking.base_url.clone())
            .with_console_allow_insecure_cookie(cfg.console.allow_insecure_cookie)
            // The poller's retirement cap doubles as the listing's suppression
            // cap, so a row the poller gave up on stops being shown at the same
            // point it stops being polled; the staleness window rides along.
            .with_shipment_policy(cfg.carriers.list_policy())
            // Rule-disposition inference rides the SAME key/provider the triage
            // stages resolve and the Stage-1 model; no key => `None` => rules
            // that omit a disposition are stored filtered.
            .with_rule_inference(RuleInferClient::from_config(cfg))
            // The rule card's suggestion rides the same key and the NOTIFY
            // model, the one small model the daemon already runs and prices.
            .with_rule_suggestion(RuleSuggestClient::from_config(cfg))
            // The sitrep headline: same key, same notify model.
            .with_fye_headline(HeadlineClient::from_config(cfg))
            // Smart clear's expiry pass: same key, its OWN model
            // (`[smart_clear]`), billed under its own ledger category.
            .with_smart_clear(SmartClearClient::from_config(cfg))
            // The unsubscribe page reader rides the same key and the notify
            // model, billed under its own ledger category.
            .with_unsub_page_reader(UnsubPageClient::from_config(cfg))
            // The browser rung, for a daemon that was given a Browserbase key.
            // The key is the whole switch: no key, no rung.
            .with_unsub_browser(crate::unsub_browser::Browserbase::from_env())
            // The assistant relay resolves only when BOTH the gateway base URL
            // and the assistant key are present; anything less is `None` and
            // the route 404s (see `Stage2Config::resolve_assistant`).
            .with_assistant(
                cfg.stage2
                    .resolve_assistant()
                    .map(crate::assistant::AssistantRelay::new),
            );

        // `[console] sso_url` has TWO sources and the FILE IS THE WEAKER ONE.
        // [`ApiState::from_env`] has already applied the variable, which is what
        // the warden sets when it provisions a tenant, so a file value fills in
        // only where that left nothing. Resolving it here rather than in
        // `from_env` is what makes the file setting reach the console at all:
        // without this the field parses, and every self-host that put it in
        // `squelch.toml` instead of the environment gets no button and no reason
        // why. Env still wins, twice over, since `Config::load` also folds the
        // variable into `cfg` before this runs.
        let state = match &cfg.console.sso_url {
            Some(url) if state.console_sso_url().is_none() => {
                state.with_console_sso_url(Some(url.clone()))
            }
            _ => state,
        };

        // The hatch is the one setting whose whole job is to weaken a control,
        // so it says so on the way up. The login page carries the same warning,
        // but an operator who pairs from the CLI may never load it.
        if state.console_allow_insecure_cookie {
            eprintln!(
                "squelch-api: [console] allow_insecure_cookie is ON, so the console session \
                 cookie is served without `Secure` and this daemon treats its own origin as \
                 plain http. The cookie is a live device token: anything on the network path \
                 can take it. Front the console with TLS or reach it on loopback instead."
            );
        }

        // Always wired: it names where the slot would be, and an account that
        // is not IMAP never reads it.
        let state = state.with_imap_write_slot(
            cfg.credential_backend,
            account_email.to_string(),
            cfg.resolve_credentials_path(),
        );

        Ok(match cfg.oauth_client() {
            Ok(client) => {
                let account_id = state.account_id;
                state
                    .with_calendar_credentials(squelch_core::credentials::CalendarCredentials::new(
                        account_id,
                        account_email.to_string(),
                        cfg.credential_backend,
                        cfg.resolve_credentials_path(),
                        client.clone(),
                    )?)
                    .with_write_credentials(
                        cfg.credential_backend,
                        account_email.to_string(),
                        cfg.resolve_credentials_path(),
                        client,
                    )?
            }
            Err(_) => {
                eprintln!(
                    "squelch-api: no OAuth client configured; action endpoints will return 403 \
                     (set client_id/client_secret and run `squelchd auth`)"
                );
                state
            }
        })
    }

    /// The active account id (exposed for tests / embedders).
    pub fn account_id(&self) -> AccountId {
        self.account_id
    }
}

/// Where an IMAP account's credential slot is: what the write door needs to
/// read it ([`squelch_core::credentials::load_imap_slot_backend`]).
#[derive(Debug, Clone)]
pub(crate) struct ImapWriteSlot {
    pub(crate) backend: CredentialBackend,
    pub(crate) account_email: String,
    pub(crate) credentials_path: std::path::PathBuf,
}

/// The phone's side of the agent door's review requests: which drafts it was
/// already told about, and when. ONE PUSH PER DRAFT (asking again may bring a
/// draft up on the Mac again, never buzz the phone again), and a buzz budget of
/// its own, much tighter than the Mac's five-second clock: an agent looping
/// over `create_draft open:true` must not be able to ring the owner's phone
/// once per draft it can hold. Both lists are bounded.
#[derive(Debug, Default)]
pub(crate) struct DraftPingLedger {
    /// `(draft id, created)`, oldest first.
    pub(crate) told: std::collections::VecDeque<(i64, i64)>,
    /// When each push in the last hour went out, oldest first.
    pub(crate) sent_at: std::collections::VecDeque<std::time::Instant>,
}
