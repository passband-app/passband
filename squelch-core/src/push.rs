//! The APNs pusher: the second delivery adapter on the durable `events` log.
//!
//! THE RELAY IS BLIND — this task reads full [`Event`] rows but serializes only
//! [`PushRequest`] (event id + collapse id), so adding a field there is a
//! security change. Its own `sync_state` cursor ([`CURSOR_KEY`]) advances only
//! after a delivery: at-least-once, never a skip, and an event that reached
//! anybody is never re-pushed (see [`Pusher::drain`]). Bearer and device tokens
//! are never logged.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch};

use crate::config::Config;
use crate::error::{CoreError, Result};
use crate::metrics::{ApnsPushOutcome, SyncMetrics};
use crate::store::{Store, SyncState};
use crate::types::{AccountId, Event};

/// The pusher's cursor row in `sync_state`: `uidvalidity` unused, `last_uid`
/// holds the last event id the relay accepted. A key of its own, distinct from
/// the sync engine's `'history'` row, so neither channel can stall the other.
pub const CURSOR_KEY: &str = "apns_push_cursor";

/// Events read per store round-trip. The drain loops until the table is caught
/// up, so this only bounds how long the store mutex is held per read.
const BATCH: usize = 50;

/// The relay's own per-request token ceiling. Registered devices are chunked to
/// it rather than truncated — every device gets the ping.
const MAX_TOKENS_PER_REQUEST: usize = 100;

/// APNs caps `apns-collapse-id` at 64 BYTES and rejects anything longer.
const MAX_COLLAPSE_ID: usize = 64;

/// Coarse fallback tick. The broadcast wake is the fast path; this covers a
/// missed or `Lagged` wake and a daemon that started with a stale cursor.
const IDLE_INTERVAL: Duration = Duration::from_secs(60);

/// Backoff after a relay failure: the events are already durable, so there is no
/// reason to hammer a relay that is down.
const BACKOFF_BASE: Duration = Duration::from_secs(5);
const BACKOFF_CAP: Duration = Duration::from_secs(300);

/// Whole-request budget. The relay's own fan-out budget is 30s, so anything past
/// ~35s means the relay itself is gone rather than slow.
const HTTP_TIMEOUT: Duration = Duration::from_secs(35);
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How the event id goes out. A bare int for an untagged device; otherwise
/// `"<tag>:<id>"`, the tag being the opaque label the client registered with.
///
/// The tag is client-minted and names nothing about the mailbox: it is the
/// receiver's own word for one of its accounts, so the relay and APNs carry it
/// while staying exactly as blind as they were. `:` cannot appear inside a tag
/// (the human door refuses one that does), so the split is unambiguous.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum PushEventId {
    Plain(i64),
    Tagged(String),
}

impl PushEventId {
    fn new(tag: Option<&str>, id: i64) -> Self {
        match tag {
            Some(tag) => Self::Tagged(format!("{tag}:{id}")),
            None => Self::Plain(id),
        }
    }
}

/// Where one device's push goes: the payload it rides in (its `tag`) and the
/// APNs environment its token was minted in. Both are per-REQUEST on the wire,
/// so devices that differ in either cannot share a request.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Route {
    tag: Option<String>,
    /// The device row's declared environment; `None` = the client never said.
    environment: Option<String>,
}

/// The complete wire body, and THE PRIVACY BOUNDARY: the content-bearing fields
/// of [`Event`] (sender, one_line, tier, importance, kind, deadline) are absent
/// and must stay absent. Adding a field here is a security change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PushRequest<'a> {
    pub device_tokens: &'a [String],
    /// Monotonic event id; the relay forwards it verbatim, never interprets it.
    pub event_id: PushEventId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collapse_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<&'a str>,
    /// Which of the relay's FIXED fallback lines the alert carries: absent for
    /// mail, `"draft"` for a [`DraftPing`], `"calendar"` for a
    /// [`CalendarPing`], `"clear"` for a [`ClearPing`] (no line at all: a
    /// silent push). A closed set the relay enforces;
    /// it picks a line and can never carry one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<&'static str>,
}

/// An agent asked its owner to review one of its drafts (the agent door's
/// `open_draft`). Not an [`Event`]: there is no received message behind it,
/// nothing to replay, and a review request from an hour ago is not one
/// anybody wants buzzing their phone. So it is NOT DURABLE: handed to a running
/// pusher over a channel, tried once, and dropped if the relay is down.
///
/// On the wire it is `"<tag>:draft:<draft_id>:<created>"`. `created` (Unix
/// seconds) binds the push to ONE draft, as it does the `passband://draft`
/// link: draft ids are reused row ids, so the phone opens the push only for a
/// draft created at exactly that second. Untagged devices are skipped; the
/// phone cannot route a draft without knowing which account it is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DraftPing {
    pub draft_id: i64,
    pub created: i64,
}

impl DraftPing {
    fn event_id(&self, tag: Option<&str>) -> Option<PushEventId> {
        tag.map(|tag| {
            PushEventId::Tagged(format!("{tag}:draft:{}:{}", self.draft_id, self.created))
        })
    }

    /// Carries the creation second as well as the id: draft ids are small
    /// per-mailbox row ids, so two mailboxes on one phone both have a draft 3,
    /// and APNs would let one's banner silently replace the other's.
    fn collapse_id(&self) -> String {
        format!("draft-{}-{}", self.draft_id, self.created)
    }
}

/// A calendar reminder fired (docs/CALENDAR.md, "Reminders and tickets"):
/// not durable, like [`DraftPing`], and for the same reason. A reminder is
/// about the next few minutes, and its row in `calendar_reminder_fires` is
/// what the phone fetches, not this ping.
///
/// On the wire it is `"<tag>:cal:<fire_id>:<fired>"`, `fired` the Unix second
/// it fired: fire ids are per-mailbox row ids, so the second binds the push
/// to one firing. Untagged devices are skipped. The relay sees no title, no
/// time and no place: the phone's extension fetches
/// `GET /client/calendar/reminders/{fire_id}` from its own daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalendarPing {
    pub fire_id: i64,
    pub fired: i64,
}

impl CalendarPing {
    fn event_id(&self, tag: Option<&str>) -> Option<PushEventId> {
        tag.map(|tag| PushEventId::Tagged(format!("{tag}:cal:{}:{}", self.fire_id, self.fired)))
    }

    /// With the second, as [`DraftPing::collapse_id`] is, so two mailboxes'
    /// fire 3 never replace each other's banner.
    fn collapse_id(&self) -> String {
        format!("cal-{}-{}", self.fire_id, self.fired)
    }
}

/// The owner dealt with one event on another device (copied its login code
/// on the Mac), so the phone's banner for it should come down. Not durable,
/// like [`DraftPing`]: a clear that cannot go out now is not worth sending
/// later, when the banner has likely been swiped away anyway.
///
/// On the wire it is `"<tag>:clear:<event_id>"`, sent as a SILENT background
/// push (the relay's `kind: "clear"`): the phone app wakes, finds the banner
/// it holds for that event, and removes it. Untagged devices are skipped, for
/// the reason draft pings skip them: an event id means nothing without the
/// account it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClearPing {
    pub event_id: i64,
}

impl ClearPing {
    fn event_id(&self, tag: Option<&str>) -> Option<PushEventId> {
        tag.map(|tag| PushEventId::Tagged(format!("{tag}:clear:{}", self.event_id)))
    }

    /// Its own collapse id, never the banner's `thread-…` one: APNs replacing
    /// the banner with a background push would not take it down.
    fn collapse_id(&self) -> String {
        format!("clear-{}", self.event_id)
    }
}

/// One token's outcome, as the relay reports it. `status` is the APNs HTTP
/// status VERBATIM; `410` is data (delete the row), not an error. `reason` is
/// APNs' own error constant (`BadDeviceToken`, `IdleTimeout`, ...), relayed as
/// given; absent on a 200.
#[derive(Debug, Clone, Deserialize)]
struct PushResult {
    token: String,
    status: u16,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PushResponse {
    #[serde(default)]
    results: Vec<PushResult>,
}

/// What one event's fan-out produced across every token chunk. The relay
/// answering 200 only means the RELAY is up; each token carries its own APNs
/// status, and this tally decides whether the cursor may move.
#[derive(Debug, Default, PartialEq, Eq)]
struct PushOutcome {
    /// Tokens APNs retired (`410`), for the caller to delete.
    dead: Vec<String>,
    /// Tokens APNs accepted (`200`).
    delivered: usize,
    /// Tokens that neither landed nor retired — `0` for an APNs the relay could
    /// not reach or that timed out, `403` (the relay's provider token is bad,
    /// which an operator fixes), `429` throttling, `5xx`, anything unexpected.
    /// All retryable.
    failed: usize,
    /// Tokens APNs refused PERMANENTLY for this request: any 4xx other than
    /// 403, 410 and 429 (400 `BadDeviceToken` / `DeviceTokenNotForTopic`, 404,
    /// 405, 413). Retrying the same request cannot help, so these do NOT hold
    /// the cursor. They are NOT deleted either: a 400 is also what configuration
    /// drift looks like (a topic missing from the relay's allowlist, a wrong
    /// fallback environment for a legacy row), and deleting on that would
    /// silently unregister every phone.
    rejected: usize,
    /// A request for this event failed at the RELAY hop (unreachable, non-2xx,
    /// unreadable body) and the fan-out stopped there. Everything above is what
    /// the requests BEFORE it produced: kept, not discarded, because tokens
    /// that already took the event must not be pushed it again and a 410
    /// already seen is definitive.
    relay_failed: bool,
}

/// The classes a per-token APNs status falls into. `410` is retirement, the
/// permanent 4xxs are rejection, and EVERYTHING ELSE that is not a 200 is
/// retryable: an unknown status is safer held than dropped.
///
/// The one 4xx the reason overrides is `400 IdleTimeout`: APNs closing a
/// connection the relay held idle too long says nothing about the token, so it
/// is retried like a `0`. Matched EXACTLY; any other 400 reason stays rejected.
fn classify(status: u16, reason: Option<&str>) -> ApnsPushOutcome {
    match status {
        200 => ApnsPushOutcome::Delivered,
        410 => ApnsPushOutcome::Dead,
        403 | 429 => ApnsPushOutcome::Failed,
        400 if reason == Some("IdleTimeout") => ApnsPushOutcome::Failed,
        400..=499 => ApnsPushOutcome::Rejected,
        _ => ApnsPushOutcome::Failed,
    }
}

/// The relay's `reason`, reduced to something safe to put in a log line. APNs'
/// reasons are short CamelCase enum constants, and naming one is what lets an
/// operator tell `BadDeviceToken` (the phone) from `DeviceTokenNotForTopic` or
/// `TopicDisallowed` (our config). It still crosses a network hop, so it is
/// treated as UNTRUSTED TEXT: printed only when it is 1..=40 ASCII alphanumeric
/// characters, which no token (64 hex chars), bearer, URL or log-forging
/// newline fits, and `-` otherwise.
fn loggable_reason(reason: Option<&str>) -> &str {
    match reason {
        Some(r) if (1..=40).contains(&r.len()) && r.bytes().all(|b| b.is_ascii_alphanumeric()) => r,
        _ => "-",
    }
}

/// The two environments the relay accepts. Its parse is EXACT and
/// case-sensitive, and an unknown value fails the WHOLE request with a 400, so
/// a typo here would silence every device on the daemon.
const ENVIRONMENTS: [&str; 2] = ["production", "sandbox"];

/// Normalize `[pusher] environment` / `SQUELCH_RELAY_APNS_ENV`: trimmed and
/// ASCII-lowercased, so `Production` means what it says. Blank is unset. A value
/// that is still neither accepted spelling is DROPPED with a loud warning
/// rather than forwarded: omitted, the relay applies its own default and the
/// devices that declared their own environment keep working; forwarded, every
/// legacy-row request would 400. The echoed value is clamped to 32 chars.
fn normalize_environment(raw: Option<&str>) -> Option<String> {
    let value = raw?.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }
    if ENVIRONMENTS.contains(&value.as_str()) {
        return Some(value);
    }
    let shown: String = value.chars().take(32).collect();
    eprintln!(
        "squelch: SQUELCH_RELAY_APNS_ENV / [pusher] environment {shown:?} is not `production` or `sandbox`; IGNORED, the relay's default applies"
    );
    None
}

impl PushOutcome {
    /// Nothing landed and something retryable failed: the APNs hop is out while
    /// the relay hop is fine — leave the cursor, back off, retry. Deliberately
    /// NOT "any failure": the pusher's one rule is that AN EVENT THAT REACHED
    /// ANYBODY IS NEVER RE-PUSHED, so a partial delivery advances (and so does
    /// a relay failure on a later request, see [`Pusher::drain`]). `rejected`
    /// is deliberately ABSENT: an event every token refused permanently
    /// advances, or a daemon whose only device is rejected stalls forever and
    /// then dumps the whole backlog on the first good device to register.
    fn nothing_landed(&self) -> bool {
        self.delivered == 0 && self.failed > 0
    }
}

/// `thread-<thread id>`, clamped to APNs' 64-BYTE collapse-id limit on a char
/// boundary (Gmail thread ids are hex, but the truncation must not be able to
/// split a UTF-8 sequence and produce a body the relay rejects).
fn collapse_id(thread_id: &str) -> String {
    const PREFIX: &str = "thread-";
    let budget = MAX_COLLAPSE_ID - PREFIX.len();
    let mut end = thread_id.len().min(budget);
    while end > 0 && !thread_id.is_char_boundary(end) {
        end -= 1;
    }
    format!("{PREFIX}{}", &thread_id[..end])
}

fn next_backoff(current: Option<Duration>) -> Duration {
    match current {
        None => BACKOFF_BASE,
        Some(d) => (d * 2).min(BACKOFF_CAP),
    }
}

/// Devices sorted into one bucket per (tag, environment), first-seen order
/// preserved so a fan-out and its logs stay reproducible. One request per
/// bucket is the price of both: every token in a bucket shares one payload
/// (which carries the tag) and one `environment` (which APNs host the relay
/// dials). A sandbox token sent to production is REJECTED, so two devices under
/// the same tag but different environments must never share a request.
fn by_route(devices: &[(Route, String)]) -> Vec<(Route, Vec<String>)> {
    let mut groups: Vec<(Route, Vec<String>)> = Vec::new();
    for (route, token) in devices {
        match groups.iter_mut().find(|(r, _)| r == route) {
            Some((_, tokens)) => tokens.push(token.clone()),
            None => groups.push((route.clone(), vec![token.clone()])),
        }
    }
    groups
}

/// A device token reduced to something safe to log: at most 8 characters, enough
/// to correlate with a `devices` row and useless as a capability.
fn token_prefix(token: &str) -> &str {
    let mut end = token.len().min(8);
    while end > 0 && !token.is_char_boundary(end) {
        end -= 1;
    }
    &token[..end]
}

/// The pusher's HTTP client. REDIRECTS ARE REFUSED: every request carries the
/// relay bearer, and reqwest's default policy would let a `307` walk that
/// token-bearing POST to a host of the relay's choosing. Off, a 3xx is simply a
/// non-2xx — backoff, cursor unmoved.
fn http_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

/// The APNs pusher task. Construct with [`Pusher::from_config`], which returns
/// `Ok(None)` when no relay is configured.
pub struct Pusher {
    store: Arc<dyn Store>,
    account_id: AccountId,
    /// Fully-resolved `{relay_url}/v1/push`.
    push_url: String,
    /// The relay bearer. NEVER logged, never included in an error.
    relay_token: Option<String>,
    topic: Option<String>,
    /// The FALLBACK environment, for devices that did not declare their own.
    /// `None` omits the field and the relay applies its default.
    environment: Option<String>,
    http: reqwest::Client,
    /// Per-token outcome counters. A private registry until
    /// [`Pusher::with_metrics`] hands it the process's one.
    metrics: Arc<SyncMetrics>,
    /// Review requests from the agent door; see [`DraftPing`]. `None` until
    /// [`Pusher::with_draft_pings`] hands it the human door's channel.
    draft_pings: Option<broadcast::Receiver<DraftPing>>,
    /// Calendar reminders from the human door's reminder worker; see
    /// [`CalendarPing`]. `None` until [`Pusher::with_calendar_pings`].
    calendar_pings: Option<broadcast::Receiver<CalendarPing>>,
    /// Banners to take down; see [`ClearPing`]. `None` until
    /// [`Pusher::with_clear_pings`].
    clear_pings: Option<broadcast::Receiver<ClearPing>>,
}

impl Pusher {
    /// Build the pusher, or `Ok(None)` when `pusher.relay_url` is unset — the
    /// feature flag for the whole thing, so a daemon with no relay configured
    /// never builds a client aimed at one. Disabled and broken stay distinct
    /// answers: `Err` means a relay WAS named and the client could not be built.
    pub fn from_config(
        store: Arc<dyn Store>,
        account_id: AccountId,
        config: &Config,
    ) -> Result<Option<Self>> {
        let Some(relay_url) = config
            .pusher
            .relay_url
            .as_deref()
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty())
        else {
            return Ok(None);
        };
        // The error carries no url and no bearer; neither reached the builder.
        let http = http_client()
            .map_err(|e| CoreError::Other(anyhow::anyhow!("APNs relay HTTP client: {e}")))?;
        Ok(Some(Self {
            store,
            account_id,
            push_url: format!("{}/v1/push", relay_url.trim_end_matches('/')),
            relay_token: config.pusher.relay_token.clone().filter(|t| !t.is_empty()),
            topic: config.pusher.topic.clone().filter(|t| !t.is_empty()),
            environment: normalize_environment(config.pusher.environment.as_deref()),
            http,
            metrics: SyncMetrics::new(),
            draft_pings: None,
            calendar_pings: None,
            clear_pings: None,
        }))
    }

    /// TEST HOOK: aim a pusher at an explicit base URL (a mock relay on an
    /// ephemeral port) without going through env/config.
    #[doc(hidden)]
    pub fn for_test(store: Arc<dyn Store>, account_id: AccountId, base_url: &str) -> Self {
        Self {
            store,
            account_id,
            push_url: format!("{}/v1/push", base_url.trim_end_matches('/')),
            relay_token: None,
            topic: None,
            environment: None,
            // The SAME client the real path uses, redirect policy included —
            // otherwise the redirect test would prove nothing about production.
            http: http_client().expect("test http client"),
            metrics: SyncMetrics::new(),
            draft_pings: None,
            calendar_pings: None,
            clear_pings: None,
        }
    }

    /// Report per-token outcomes into the process's metrics registry.
    pub fn with_metrics(mut self, metrics: Arc<SyncMetrics>) -> Self {
        self.metrics = metrics;
        self
    }

    /// Listen for agent draft review requests (see [`DraftPing`]).
    pub fn with_draft_pings(mut self, pings: broadcast::Receiver<DraftPing>) -> Self {
        self.draft_pings = Some(pings);
        self
    }

    /// Listen for calendar reminders (see [`CalendarPing`]).
    pub fn with_calendar_pings(mut self, pings: broadcast::Receiver<CalendarPing>) -> Self {
        self.calendar_pings = Some(pings);
        self
    }

    /// Listen for banners to take down (see [`ClearPing`]).
    pub fn with_clear_pings(mut self, pings: broadcast::Receiver<ClearPing>) -> Self {
        self.clear_pings = Some(pings);
        self
    }

    /// TEST HOOK: set the relay bearer / topic / environment pass-throughs.
    #[doc(hidden)]
    pub fn with_relay_auth(mut self, token: Option<&str>) -> Self {
        self.relay_token = token.map(|t| t.to_string());
        self
    }

    /// TEST HOOK: set the configured fallback environment.
    #[doc(hidden)]
    pub fn with_environment(mut self, environment: Option<&str>) -> Self {
        self.environment = environment.map(|e| e.to_string());
        self
    }

    /// The pusher's persisted cursor. A first-ever start has none and joins at
    /// the HEAD of the log: a phone registering today must not get the backlog.
    fn cursor(&self) -> Result<i64> {
        match self.store.sync_state(self.account_id, CURSOR_KEY)? {
            Some(s) => Ok(s.last_uid as i64),
            None => {
                let latest = self.store.latest_event_id(self.account_id)?;
                self.set_cursor(latest)?;
                Ok(latest)
            }
        }
    }

    fn set_cursor(&self, id: i64) -> Result<()> {
        self.store.set_sync_state(
            self.account_id,
            CURSOR_KEY,
            &SyncState {
                uidvalidity: 0,
                last_uid: id.max(0) as u64,
            },
        )
    }

    /// Push everything past the cursor, oldest first, one event at a time.
    ///
    /// ONE POLICY: an event that reached anybody is never re-pushed, and one
    /// that reached nobody is never skipped while a retry could land it. So
    /// `Err` (cursor unmoved, backoff, retry) when the relay hop fails before
    /// any device took the event, or a 200 carries no delivered token and
    /// something retryable is in the mix. When a LATER request fails after an
    /// earlier one delivered, the cursor ADVANCES past the event (the devices
    /// still owed it lose this one ping rather than every other device getting
    /// it twice, or forever if the failure persists) and `Err` is STILL
    /// returned, after the write, so the run loop backs off a relay that just
    /// failed instead of hammering it with the next event.
    async fn drain(&self) -> Result<()> {
        let mut cursor = self.cursor()?;
        loop {
            let batch = self.store.events_after(self.account_id, cursor, BATCH)?;
            if batch.is_empty() {
                return Ok(());
            }
            // The TAG and the ENVIRONMENT ride with each token: they decide which
            // payload and which APNs host the token's push goes out on, so they
            // cannot be dropped here the way the rest of the row is.
            let mut devices: Vec<(Route, String)> = self
                .store
                .list_devices(self.account_id)?
                .into_iter()
                .map(|d| {
                    (
                        Route {
                            tag: d.tag,
                            environment: d.environment,
                        },
                        d.token,
                    )
                })
                .collect();

            // Nothing registered: an event nobody can receive is history, not
            // backlog. Advance without opening a socket to the relay.
            if devices.is_empty() {
                if let Some(last) = batch.last() {
                    cursor = last.id;
                    self.set_cursor(cursor)?;
                }
                continue;
            }

            for ev in &batch {
                if !self
                    .store
                    .notification_delivery_allowed(self.account_id, ev.id)?
                {
                    cursor = ev.id;
                    self.set_cursor(cursor)?;
                    continue;
                }
                let outcome = self.push_event(ev, &devices).await;
                let dead = &outcome.dead;
                for token in dead {
                    // The relay passes APNs' 410 back verbatim so THIS daemon
                    // owns the cleanup; the relay remembers nothing.
                    match self.store.delete_device_by_token(self.account_id, token) {
                        Ok(true) => eprintln!(
                            "squelch: APNs pusher dropped an unregistered device (token {}…)",
                            token_prefix(token)
                        ),
                        Ok(false) => {}
                        Err(e) => eprintln!("squelch: APNs pusher could not drop a device: {e}"),
                    }
                }
                if !dead.is_empty() {
                    devices.retain(|(_, t)| !dead.contains(t));
                }
                // The relay hop failed partway. Dead rows are already gone (a
                // 410 is definitive whatever came after it); what remains is
                // whether anybody got the event.
                if outcome.relay_failed {
                    if outcome.delivered > 0 {
                        eprintln!(
                            "squelch: APNs relay failed a later request after {} device(s) took this event; not re-pushing it",
                            outcome.delivered
                        );
                        cursor = ev.id;
                        self.set_cursor(cursor)?;
                    } else {
                        eprintln!("squelch: nobody took this event; cursor unmoved");
                    }
                    return Err(relay_failure());
                }
                if !dead.is_empty() && devices.is_empty() {
                    // Every device just died. The remaining events have
                    // nowhere to go; skip to the end of the batch.
                    cursor = batch.last().map(|e| e.id).unwrap_or(cursor);
                    self.set_cursor(cursor)?;
                    break;
                }
                // The relay answered, APNs did not: every surviving token came
                // back 0/403/429/5xx (or was rejected), so this event reached
                // nobody and something retryable is in the mix. The relay's own
                // 200 is not evidence of delivery — bail, cursor unmoved. An
                // event whose tokens were ALL rejected falls through and advances.
                if outcome.nothing_landed() {
                    eprintln!(
                        "squelch: APNs took none of {} device(s) for this event; cursor unmoved",
                        outcome.failed
                    );
                    return Err(relay_failure());
                }
                // Advance only after a delivery — the at-least-once guarantee:
                // a hop dying mid-batch leaves the rest to be retried.
                cursor = ev.id;
                self.set_cursor(cursor)?;
            }
        }
    }

    /// POST one event to the relay for every registered device — one request
    /// per (tag, environment) bucket, each chunked to the relay's ceiling —
    /// returning the per-token tally: a 200 from the relay says nothing about
    /// whether APNs took anything.
    ///
    /// INFALLIBLE BY DESIGN. A relay-hop failure stops the fan-out at that
    /// request and sets [`PushOutcome::relay_failed`], but the tally of the
    /// requests that already went out is RETURNED, not thrown away: discarding
    /// it would re-push the event to devices that already have it and forget
    /// their 410s. [`Pusher::drain`] decides what the cursor does with it.
    async fn push_event(&self, ev: &Event, devices: &[(Route, String)]) -> PushOutcome {
        self.fan_out(devices, collapse_id(&ev.thread_id), None, |tag| {
            Some(PushEventId::new(tag, ev.id))
        })
        .await
    }

    /// Push one [`DraftPing`] to every tagged device, ONCE. Best effort by
    /// design (see [`DraftPing`]): a relay failure is logged and the ping is
    /// gone. A 410 still retires the device, because that answer is the same
    /// whatever the push was about.
    async fn push_draft(&self, ping: DraftPing) {
        self.push_once("draft", ping.collapse_id(), "a draft review ping", |tag| {
            ping.event_id(tag)
        })
        .await;
    }

    /// Push one [`CalendarPing`] to every tagged device, ONCE, as
    /// [`Self::push_draft`] does.
    async fn push_calendar(&self, ping: CalendarPing) {
        self.push_once(
            "calendar",
            ping.collapse_id(),
            "a calendar reminder",
            |tag| ping.event_id(tag),
        )
        .await;
    }

    /// Push one [`ClearPing`] to every tagged device, ONCE, as
    /// [`Self::push_draft`] does.
    async fn push_clear(&self, ping: ClearPing) {
        self.push_once("clear", ping.collapse_id(), "a banner clear", |tag| {
            ping.event_id(tag)
        })
        .await;
    }

    /// The non-durable push every ping makes: every TAGGED device, once, the
    /// relay's `kind` line, a failure logged and dropped.
    async fn push_once(
        &self,
        kind: &'static str,
        collapse: String,
        what: &str,
        id_for: impl Fn(Option<&str>) -> Option<PushEventId>,
    ) {
        let devices: Vec<(Route, String)> = match self.store.list_devices(self.account_id) {
            Ok(rows) => rows
                .into_iter()
                .filter(|d| d.tag.is_some())
                .map(|d| {
                    (
                        Route {
                            tag: d.tag,
                            environment: d.environment,
                        },
                        d.token,
                    )
                })
                .collect(),
            Err(e) => {
                eprintln!("squelch: APNs pusher could not list devices for {what}: {e}");
                return;
            }
        };
        if devices.is_empty() {
            return;
        }
        let outcome = self.fan_out(&devices, collapse, Some(kind), id_for).await;
        for token in &outcome.dead {
            if let Err(e) = self.store.delete_device_by_token(self.account_id, token) {
                eprintln!("squelch: APNs pusher could not drop a device: {e}");
            }
        }
        if outcome.relay_failed || outcome.nothing_landed() {
            eprintln!("squelch: {what} reached no device; not retried");
        }
    }

    /// POST one payload to every device it has an id for: one request per
    /// (tag, environment) bucket, each chunked to the relay's ceiling. A bucket
    /// `id_for` answers `None` is skipped.
    async fn fan_out(
        &self,
        devices: &[(Route, String)],
        collapse: String,
        kind: Option<&'static str>,
        id_for: impl Fn(Option<&str>) -> Option<PushEventId>,
    ) -> PushOutcome {
        let mut outcome = PushOutcome::default();
        for (route, tokens) in by_route(devices) {
            let Some(event_id) = id_for(route.tag.as_deref()) else {
                continue;
            };
            // The device's OWN environment wins: it is a fact about the token.
            // The configured one is only the fallback for rows that never said,
            // and with neither the field is omitted for the relay's default.
            let environment = route.environment.as_deref().or(self.environment.as_deref());
            for chunk in tokens.chunks(MAX_TOKENS_PER_REQUEST) {
                let body = PushRequest {
                    device_tokens: chunk,
                    event_id: event_id.clone(),
                    collapse_id: Some(collapse.clone()),
                    topic: self.topic.as_deref(),
                    environment,
                    kind,
                };
                let mut req = self.http.post(&self.push_url).json(&body);
                if let Some(token) = &self.relay_token {
                    req = req.bearer_auth(token);
                }
                // The error is NOT inspected: reqwest's Display embeds the URL and
                // the builder held the bearer. Only the class of failure escapes.
                let Ok(resp) = req.send().await else {
                    eprintln!("squelch: APNs relay unreachable");
                    outcome.relay_failed = true;
                    return outcome;
                };
                let status = resp.status();
                if !status.is_success() {
                    eprintln!(
                        "squelch: APNs relay refused the push (HTTP {})",
                        status.as_u16()
                    );
                    outcome.relay_failed = true;
                    return outcome;
                }
                let Ok(parsed) = resp.json::<PushResponse>().await else {
                    eprintln!("squelch: APNs relay returned an unreadable body");
                    outcome.relay_failed = true;
                    return outcome;
                };
                for result in parsed.results {
                    let reason = result.reason.as_deref();
                    let class = classify(result.status, reason);
                    self.metrics.record_apns_push(class);
                    match class {
                        ApnsPushOutcome::Delivered => outcome.delivered += 1,
                        // The device is gone. Data, not an error.
                        ApnsPushOutcome::Dead => outcome.dead.push(result.token),
                        // Permanent for this token and request: not retried, NOT
                        // deleted (see `PushOutcome::rejected`). Status, token
                        // prefix, and the reason ONLY as `loggable_reason`
                        // passes it: the reason is what separates a bad phone
                        // from a bad topic, but it is untrusted text.
                        ApnsPushOutcome::Rejected => {
                            outcome.rejected += 1;
                            eprintln!(
                                "squelch: APNs rejected the push for one device with status {} reason {} (token {}…); not retried",
                                result.status,
                                loggable_reason(reason),
                                token_prefix(&result.token)
                            );
                        }
                        // Retryable: `0` is the relay's word for an unreachable or
                        // timed-out APNs; 403/429/5xx/`400 IdleTimeout` come
                        // through verbatim. Never the token value; the reason
                        // only through the same sanitizer.
                        ApnsPushOutcome::Failed => {
                            outcome.failed += 1;
                            eprintln!(
                                "squelch: APNs push for one device returned status {} reason {} (token {}…)",
                                result.status,
                                loggable_reason(reason),
                                token_prefix(&result.token)
                            );
                        }
                    }
                }
            }
        }
        outcome
    }

    /// Run until shutdown, waking on the events broadcast, a coarse
    /// [`IDLE_INTERVAL`] tick, or the shutdown watch. After a relay failure only
    /// shutdown may cut the backoff short — a wake must not turn a dead relay
    /// into a hot retry loop.
    pub async fn run(
        mut self,
        mut wake: broadcast::Receiver<i64>,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        let mut pings = self.draft_pings.take();
        let mut reminders = self.calendar_pings.take();
        let mut clears = self.clear_pings.take();
        let mut backoff: Option<Duration> = None;
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            // Drain FIRST: a daemon that restarts with a stale cursor catches up
            // immediately instead of waiting out an idle interval.
            match self.drain().await {
                Ok(()) => backoff = None,
                Err(_) => {
                    let delay = next_backoff(backoff);
                    eprintln!(
                        "squelch: APNs pusher backing off {}s (events are durable; nothing is lost)",
                        delay.as_secs()
                    );
                    backoff = Some(delay);
                }
            }

            match backoff {
                // A draft ping during the backoff is still tried, once: it is
                // not durable, so waiting it out would only lose it. It does
                // not end the backoff.
                Some(delay) => {
                    let until = tokio::time::Instant::now() + delay;
                    loop {
                        tokio::select! {
                            _ = tokio::time::sleep_until(until) => break,
                            Some(ping) = next_ping(&mut pings) => {
                                if let Some(ping) = ping { self.push_draft(ping).await; }
                            }
                            Some(ping) = next_ping(&mut reminders) => {
                                if let Some(ping) = ping { self.push_calendar(ping).await; }
                            }
                            Some(ping) = next_ping(&mut clears) => {
                                if let Some(ping) = ping { self.push_clear(ping).await; }
                            }
                            _ = shutdown.changed() => {
                                if *shutdown.borrow() { return Ok(()); }
                            }
                        }
                    }
                }
                None => {
                    tokio::select! {
                        _ = tokio::time::sleep(IDLE_INTERVAL) => {}
                        recv = wake.recv() => {
                            // A new id and `Lagged` are handled identically:
                            // the payload is a hint, the table is the truth.
                            if matches!(recv, Err(broadcast::error::RecvError::Closed)) {
                                return Ok(());
                            }
                        }
                        Some(ping) = next_ping(&mut pings) => {
                            if let Some(ping) = ping { self.push_draft(ping).await; }
                        }
                        Some(ping) = next_ping(&mut reminders) => {
                            if let Some(ping) = ping { self.push_calendar(ping).await; }
                        }
                        Some(ping) = next_ping(&mut clears) => {
                            if let Some(ping) = ping { self.push_clear(ping).await; }
                        }
                        _ = shutdown.changed() => {
                            if *shutdown.borrow() { return Ok(()); }
                        }
                    }
                }
            }
        }
    }
}

/// The next ping (a draft, a calendar reminder or a clear): `Some(Some(p))` for a
/// ping, `Some(None)` when some were missed (lagged: dropped, they are not
/// durable), and pending forever once there is no channel, so a `select!` arm
/// on it simply never fires.
async fn next_ping<T: Clone>(pings: &mut Option<broadcast::Receiver<T>>) -> Option<Option<T>> {
    let Some(rx) = pings else {
        return std::future::pending().await;
    };
    match rx.recv().await {
        Ok(ping) => Some(Some(ping)),
        Err(broadcast::error::RecvError::Lagged(_)) => Some(None),
        Err(broadcast::error::RecvError::Closed) => {
            *pings = None;
            Some(None)
        }
    }
}

/// One opaque failure value for every relay problem. Carries NO url, NO status
/// text, and above all no bearer — the specific cause is already on stderr.
fn relay_failure() -> CoreError {
    CoreError::Other(anyhow::anyhow!("relay push failed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EventKind, Tier};
    use chrono::Utc;

    fn event(id: i64, thread_id: &str) -> Event {
        Event {
            id,
            kind: EventKind::Urgent,
            message_id: 7,
            thread_id: thread_id.to_string(),
            tier: Tier::PastDue,
            importance: 95,
            sender: "alice@example.com".to_string(),
            one_line: "wire transfer needs approval today".to_string(),
            deadline: Some("2026-08-01T00:00:00Z".to_string()),
            is_auth: false,
            sealed_kind: None,
            created_at: Utc::now(),
        }
    }

    /// THE INVARIANT: if this test fails, the relay stopped being blind.
    #[test]
    fn body_is_blind() {
        let mut ev = event(4711, "abc123");
        ev.is_auth = true;
        let tokens = vec!["aa".repeat(32)];
        let body = PushRequest {
            device_tokens: &tokens,
            event_id: PushEventId::new(None, ev.id),
            collapse_id: Some(collapse_id(&ev.thread_id)),
            topic: Some("dev.squelch.ios"),
            environment: Some("production"),
            kind: None,
        };
        let json = serde_json::to_value(&body).unwrap();
        let obj = json.as_object().unwrap();

        // The shape is closed: exactly these keys, nothing else.
        let mut keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "collapse_id",
                "device_tokens",
                "environment",
                "event_id",
                "topic"
            ]
        );
        assert_eq!(obj["event_id"], serde_json::json!(4711));
        assert_eq!(obj["collapse_id"], serde_json::json!("thread-abc123"));

        // And nothing about the mail survives into the encoded bytes.
        let raw = serde_json::to_string(&body).unwrap();
        for leak in [
            "alice@example.com",
            "wire transfer",
            "past_due",
            "urgent",
            "95",
            "2026-08-01",
        ] {
            assert!(!raw.contains(leak), "content leaked onto the wire: {leak}");
        }
    }

    /// The optional pass-throughs vanish from the body entirely when unset, so a
    /// default deployment sends the smallest possible request.
    #[test]
    fn optional_passthroughs_are_omitted_not_nulled() {
        let tokens = vec!["bb".repeat(32)];
        let body = PushRequest {
            device_tokens: &tokens,
            event_id: PushEventId::new(None, 1),
            collapse_id: None,
            topic: None,
            environment: None,
            kind: None,
        };
        let json = serde_json::to_value(&body).unwrap();
        let obj = json.as_object().unwrap();
        assert_eq!(obj.len(), 2);
        assert!(obj.contains_key("device_tokens"));
        assert!(obj.contains_key("event_id"));
    }

    /// The tagged spelling stays a STRING on the wire and the plain one stays a
    /// number: the relay accepts both and forwards whichever it got verbatim.
    #[test]
    fn a_tag_rides_in_the_event_id_or_the_id_stays_a_number() {
        let plain = serde_json::to_value(PushEventId::new(None, 41)).unwrap();
        assert_eq!(plain, serde_json::json!(41));

        let tagged = serde_json::to_value(PushEventId::new(Some("acct-a"), 41)).unwrap();
        assert_eq!(tagged, serde_json::json!("acct-a:41"));
    }

    /// A draft ping names the draft and its creation second, never anything
    /// it says, and only to a device that can tell which account it is in.
    #[test]
    fn a_draft_ping_is_tagged_only_and_carries_its_binding() {
        let ping = DraftPing {
            draft_id: 12,
            created: 1_790_000_000,
        };
        assert_eq!(ping.event_id(None), None);
        assert_eq!(
            serde_json::to_value(ping.event_id(Some("acct-a")).unwrap()).unwrap(),
            serde_json::json!("acct-a:draft:12:1790000000")
        );
        assert_eq!(ping.collapse_id(), "draft-12-1790000000");
    }

    /// A calendar reminder names its firing and second, never the event,
    /// and only to a device that can tell which account it is in.
    #[test]
    fn a_calendar_ping_is_tagged_only_and_carries_its_binding() {
        let ping = CalendarPing {
            fire_id: 7,
            fired: 1_793_210_000,
        };
        assert_eq!(ping.event_id(None), None);
        assert_eq!(
            serde_json::to_value(ping.event_id(Some("acct-a")).unwrap()).unwrap(),
            serde_json::json!("acct-a:cal:7:1793210000")
        );
        assert_eq!(ping.collapse_id(), "cal-7-1793210000");
    }

    #[test]
    fn a_clear_ping_is_tagged_only_and_never_shares_the_banner_collapse_id() {
        let ping = ClearPing { event_id: 41 };
        assert_eq!(ping.event_id(None), None);
        assert_eq!(
            serde_json::to_value(ping.event_id(Some("acct-a")).unwrap()).unwrap(),
            serde_json::json!("acct-a:clear:41")
        );
        assert_eq!(ping.collapse_id(), "clear-41");
    }

    fn route(tag: Option<&str>, environment: Option<&str>) -> Route {
        Route {
            tag: tag.map(str::to_string),
            environment: environment.map(str::to_string),
        }
    }

    /// Two mailboxes on one phone register the same device under different
    /// tags; each bucket gets its own payload, and a token appears in exactly
    /// one of them.
    #[test]
    fn devices_bucket_by_tag_in_first_seen_order() {
        let devices = vec![
            (route(Some("a"), None), "t1".to_string()),
            (route(None, None), "t2".to_string()),
            (route(Some("a"), None), "t3".to_string()),
            (route(Some("b"), None), "t4".to_string()),
        ];
        let groups = by_route(&devices);
        assert_eq!(
            groups,
            vec![
                (
                    route(Some("a"), None),
                    vec!["t1".to_string(), "t3".to_string()]
                ),
                (route(None, None), vec!["t2".to_string()]),
                (route(Some("b"), None), vec!["t4".to_string()]),
            ]
        );
    }

    /// The SAME tag with two environments is two buckets: a dev build and a
    /// TestFlight build of one mailbox cannot share a request, because the
    /// request names one APNs host. First-seen order still holds.
    #[test]
    fn one_tag_splits_by_environment_in_first_seen_order() {
        let devices = vec![
            (route(Some("a"), Some("sandbox")), "t1".to_string()),
            (route(Some("a"), Some("production")), "t2".to_string()),
            (route(Some("a"), Some("sandbox")), "t3".to_string()),
            (route(Some("a"), None), "t4".to_string()),
        ];
        let groups = by_route(&devices);
        assert_eq!(
            groups,
            vec![
                (
                    route(Some("a"), Some("sandbox")),
                    vec!["t1".to_string(), "t3".to_string()]
                ),
                (route(Some("a"), Some("production")), vec!["t2".to_string()]),
                (route(Some("a"), None), vec!["t4".to_string()]),
            ]
        );
    }

    /// An untagged, undeclared fleet is the pre-tag deployment: one bucket, one
    /// request, the same body it always sent.
    #[test]
    fn an_untagged_fleet_is_still_one_bucket() {
        let devices: Vec<(Route, String)> = (0..3)
            .map(|i| (route(None, None), format!("t{i}")))
            .collect();
        assert_eq!(by_route(&devices).len(), 1);
    }

    /// The status classes: 410 retires, 403 and 429 are retryable even though
    /// they are 4xx, every other 4xx is a permanent rejection, and anything
    /// else (0, 5xx, the unexpected) is held for retry.
    #[test]
    fn statuses_classify_into_retire_reject_and_retry() {
        assert_eq!(classify(200, None), ApnsPushOutcome::Delivered);
        assert_eq!(classify(410, None), ApnsPushOutcome::Dead);
        for permanent in [400, 404, 405, 413] {
            assert_eq!(
                classify(permanent, None),
                ApnsPushOutcome::Rejected,
                "{permanent}"
            );
        }
        for retry in [0, 403, 429, 500, 503, 302, 201] {
            assert_eq!(classify(retry, None), ApnsPushOutcome::Failed, "{retry}");
        }
    }

    /// `400 IdleTimeout` is APNs dropping an idle connection, not a verdict on
    /// the token: retried. Every other 400 reason, and IdleTimeout on any
    /// other status, classifies exactly as the status alone says.
    #[test]
    fn a_400_idle_timeout_is_retried_and_only_that() {
        assert_eq!(classify(400, Some("IdleTimeout")), ApnsPushOutcome::Failed);
        for permanent in [
            "BadDeviceToken",
            "DeviceTokenNotForTopic",
            "TopicDisallowed",
            "idletimeout",
            "IdleTimeout ",
            "",
        ] {
            assert_eq!(
                classify(400, Some(permanent)),
                ApnsPushOutcome::Rejected,
                "{permanent:?}"
            );
        }
        assert_eq!(
            classify(404, Some("IdleTimeout")),
            ApnsPushOutcome::Rejected
        );
        assert_eq!(
            classify(200, Some("IdleTimeout")),
            ApnsPushOutcome::Delivered
        );
        assert_eq!(classify(410, Some("IdleTimeout")), ApnsPushOutcome::Dead);
    }

    /// The reason reaches a log only as a short alphanumeric constant.
    #[test]
    fn a_reason_is_logged_only_when_it_looks_like_an_apns_constant() {
        assert_eq!(loggable_reason(Some("BadDeviceToken")), "BadDeviceToken");
        assert_eq!(loggable_reason(Some("unreachable")), "unreachable");
        assert_eq!(loggable_reason(Some(&"A".repeat(40))), "A".repeat(40));
        for unsafe_reason in [
            "",
            "Bad Device",
            "line\nforged",
            "https://relay.example.com/v1/push",
            "Bearer-secret",
            "Bad\u{e9}Token",
        ] {
            assert_eq!(
                loggable_reason(Some(unsafe_reason)),
                "-",
                "{unsafe_reason:?}"
            );
        }
        assert_eq!(loggable_reason(Some(&"A".repeat(41))), "-");
        // A device token is 64 hex chars: alphanumeric, but too long to pass.
        assert_eq!(loggable_reason(Some(&"ab".repeat(32))), "-");
        assert_eq!(loggable_reason(None), "-");
    }

    /// The configured environment is normalized to what the relay's exact parse
    /// accepts, and anything else is dropped so the relay's default applies.
    #[test]
    fn the_configured_environment_is_normalized_or_dropped() {
        assert_eq!(
            normalize_environment(Some("Production")).as_deref(),
            Some("production")
        );
        assert_eq!(
            normalize_environment(Some(" sandbox ")).as_deref(),
            Some("sandbox")
        );
        assert_eq!(normalize_environment(Some("prod")), None);
        assert_eq!(normalize_environment(Some("")), None);
        assert_eq!(normalize_environment(Some("   ")), None);
        assert_eq!(normalize_environment(None), None);
        // A long garbage value is dropped too (and only its head is echoed).
        assert_eq!(normalize_environment(Some(&"x".repeat(500))), None);
    }

    #[test]
    fn collapse_id_clamps_to_64_bytes_on_a_char_boundary() {
        assert_eq!(collapse_id("abc"), "thread-abc");
        assert_eq!(collapse_id(""), "thread-");

        // Long ASCII: exactly 64 bytes, never more.
        let long = "f".repeat(200);
        let c = collapse_id(&long);
        assert_eq!(c.len(), MAX_COLLAPSE_ID);
        assert!(c.starts_with("thread-"));

        // A multi-byte thread id truncates DOWN to a boundary rather than
        // splitting a sequence into invalid UTF-8 the relay would reject.
        let wide = "é".repeat(100); // 2 bytes each
        let c = collapse_id(&wide);
        assert!(c.len() <= MAX_COLLAPSE_ID);
        assert!(c.is_char_boundary(c.len()));
        // 57 bytes of budget holds 28 two-byte chars (56 bytes), not 28.5.
        assert_eq!(c, format!("thread-{}", "é".repeat(28)));
    }

    #[test]
    fn backoff_doubles_from_the_base_and_caps() {
        assert_eq!(next_backoff(None), BACKOFF_BASE);
        assert_eq!(next_backoff(Some(BACKOFF_BASE)), BACKOFF_BASE * 2);
        assert_eq!(next_backoff(Some(BACKOFF_CAP)), BACKOFF_CAP);
        assert_eq!(next_backoff(Some(BACKOFF_CAP * 4)), BACKOFF_CAP);
    }

    /// The relay answering 200 is not delivery: APNs taking nothing is a failure
    /// the cursor must not survive; a partial delivery is not.
    #[test]
    fn nothing_landed_is_the_apns_hop_being_out() {
        let outcome = |delivered, failed, dead: &[&str]| PushOutcome {
            dead: dead.iter().map(|t| t.to_string()).collect(),
            delivered,
            failed,
            rejected: 0,
            relay_failed: false,
        };
        let with_rejected = |delivered, failed, rejected| PushOutcome {
            dead: Vec::new(),
            delivered,
            failed,
            rejected,
            relay_failed: false,
        };

        // Every token came back 0/429/5xx: nobody got it, retry.
        assert!(outcome(0, 3, &[]).nothing_landed());
        assert!(outcome(0, 1, &["gone"]).nothing_landed());

        // Something landed: the event was delivered, however partially.
        assert!(!outcome(1, 2, &[]).nothing_landed());
        assert!(!outcome(3, 0, &[]).nothing_landed());

        // Nothing failed. Zero delivered here means every device retired (410),
        // which is cleanup the drain loop already handles — not a retry.
        assert!(!outcome(0, 0, &["a", "b"]).nothing_landed());
        assert!(!PushOutcome::default().nothing_landed());

        // Every token rejected permanently: retrying cannot help, so the event
        // ADVANCES rather than wedging the pusher behind it.
        assert!(!with_rejected(0, 0, 2).nothing_landed());
        // Rejected plus retryable-failed with nothing delivered still holds:
        // the failed share may land on a retry.
        assert!(with_rejected(0, 1, 2).nothing_landed());
        // And a delivery alongside rejections advances as any partial does.
        assert!(!with_rejected(1, 1, 1).nothing_landed());
    }

    #[test]
    fn token_prefix_never_exposes_the_capability() {
        let token = "abcdef0123456789".repeat(4);
        assert_eq!(token_prefix(&token), "abcdef01");
        assert_eq!(token_prefix("short"), "short");
        assert_eq!(token_prefix(""), "");
    }

    /// No relay configured => no pusher, and `Ok(None)` rather than `Err`:
    /// disabled is the normal case, not a misconfiguration.
    #[test]
    fn from_config_is_none_without_a_relay_url() {
        let store: Arc<dyn Store> = Arc::new(crate::store::SqliteStore::open_in_memory().unwrap());
        let mut config = Config::default();
        assert!(
            Pusher::from_config(store.clone(), 1, &config)
                .expect("no relay is not an error")
                .is_none()
        );

        config.pusher.relay_url = Some("   ".to_string());
        assert!(
            Pusher::from_config(store.clone(), 1, &config)
                .expect("a blank relay is not an error")
                .is_none()
        );

        config.pusher.relay_url = Some("https://relay.example.com/".to_string());
        let p = Pusher::from_config(store, 1, &config)
            .expect("building the client")
            .expect("a named relay builds a pusher");
        // The trailing slash does not become a double slash on the path.
        assert_eq!(p.push_url, "https://relay.example.com/v1/push");
    }
}
