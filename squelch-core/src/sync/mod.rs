//! The sync engine: cadence, ingest, triage and cursor persistence over one
//! account's [`MailSource`]. First run backfills `backfill_days` of the inbox
//! and the sent folder and records the source's cursor; after that the
//! source's change feed is walked for BOTH folders — the inbox for incoming
//! mail, sent for whatever the user wrote from another client — an expired
//! cursor falling back to a full catch-up. The source is picked from the
//! account's `provider` row: Gmail ([`crate::mail::gmail`]) or IMAP
//! ([`crate::mail::imap`], read only).
//! Tokens, headers and bodies are NEVER logged; ingest seals first (SECURITY.md).

mod embed_backoff;
pub mod html;
pub mod ingest;
pub mod notify_lane;
mod profile;
mod relationships;
pub mod sender_signals;
mod translate_pass;
/// Public for `squelch-eval`, which replays triage through the production
/// evidence reader and snapshot builder rather than copies of them.
pub mod triage_worker;
mod understanding;

use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};

use crate::config::{ARCHIVE_DAYS_MAX, Config, ResolvedLlm, Stage2Provider};
use crate::credentials::CredentialStore;
use crate::error::{CoreError, Result};
use crate::gmail_budget::GmailBudget;
use crate::mail::gmail::{GMAIL_QUOTA_RETRY_CAP, GmailSource, GmailSourceParts};
use crate::mail::graph::{GRAPH_API_BASE, GraphSource, GraphSourceParts};
use crate::mail::imap::{IMAP_ACCOUNT_SETTING, ImapCredential, ImapSource, StoredImapSlot};
use crate::mail::{
    Changes, ChangesError, FetchedMessage, Folder, Listing, MailProvider, MailSource, Placement,
    RawBody, SourceRefusals, SyncCursor, Urgency,
};
use crate::metrics::{ArchiveHold, SyncMetrics};
use crate::store::{ContactEntry, SPAM_SYNCED_AT_KEY, Store, SyncState, TriagedMessage};
use crate::sync::ingest::{
    RawFetched, collect_mailboxes, format_recipients, ingest_with_rules, is_robot_address,
};
use crate::triage::events;
use crate::types::{AccountId, SenderRule};
use futures::StreamExt;

// The Gmail definitions squelch-api's write door shares, at the paths it has
// always imported them from. They live with the Gmail source now.
pub use crate::mail::gmail::{
    GMAIL_API_BASE, GmailMessage, HistoryDecision, LABEL_INBOX, LABEL_SPAM, MessageHeader,
    MessagePayload, advance_history_cursor, decode_raw_b64url, history_poll_decision,
    parse_internal_date,
};

/// `sync_state` row key for the one-time Sent-contacts harvest's done flag
/// (`last_uid >= 1` = complete; absent/0 = redo on next daemon start).
const SENT_CONTACTS_KEY: &str = "sent_contacts";

/// `sync_state` row key for the one-time RECIPIENTS backfill's done flag, with
/// the same semantics as [`SENT_CONTACTS_KEY`]. Distinct from it because the two
/// sweeps fill different columns and either can complete alone.
///
/// A KEY OF ITS OWN, not the `sent_recipients` one the sent-only sweep used:
/// every install that finished that pass has the old flag set, and reusing it
/// would declare the per-header split done on mailboxes where not one received
/// message has ever had its To read. The old key is simply left where it is —
/// harmless, and it says what it always said.
const RECIPIENTS_KEY: &str = "recipients_per_header";

/// How many rows the recipients backfill claims from the store per batch. Only a
/// memory bound: the pass loops until the queue is empty.
const RECIPIENTS_BATCH: u32 = 500;

/// Pause between the metadata GETs of the recipients backfill.
///
/// The sweep used to walk Sent alone; it now walks the whole mailbox, which is
/// an order of magnitude more requests on the same credential the sync loop is
/// using. Gmail bills a `messages.get` at 5 quota units against a 250-unit/sec
/// per-user budget, so 50ms between requests holds this pass at ~20/sec — 100
/// units/sec flat out, leaving the sync loop the rest of the budget. A 429 here
/// is not fatal (the pass retries next start) but it is charged to the same user
/// the sync loop is spending, and THAT bounces the lifecycle into backoff.
const RECIPIENTS_PACE: std::time::Duration = std::time::Duration::from_millis(50);

/// `sync_state` row key for the one-time blank-body heal's CURSOR. Unlike the
/// two flags above this one records progress, not just completion:
/// `uidvalidity = 1` means the sweep walked the whole mailbox and is done;
/// otherwise `last_uid` is the message id the next start resumes BELOW (absent
/// = start from the newest row). A sweep that stops early — the per-start fetch
/// ceiling, a Gmail error — leaves the cursor where it got to.
const BLANK_BODY_HEAL_KEY: &str = "blank_body_heal";

/// How many rows one blank-body scan chunk reads under the store lock. A memory
/// and lock-hold bound only; the sweep loops chunks until the ceiling or the end.
const BLANK_BODY_SCAN_CHUNK: u32 = 500;

/// The most raw fetches one daemon start spends on the blank-body heal. Each is
/// a `format=raw` GET of a whole message, so this bounds the Gmail spend and the
/// sweep's runtime per start; a mailbox with more converges over restarts, the
/// cursor carrying it. Sized so an ordinary self-host mailbox (tens of such
/// rows) finishes in one go.
const BLANK_BODY_HEAL_MAX_FETCHES: usize = 400;

/// `wake_budget.thread_id` sentinel for the per-account-per-day Stage-2 budget.
/// Gmail thread ids are hex, so no real thread can collide with it.
const GLOBAL_BUDGET_KEY: &str = crate::triage::agent_config::TOTAL_BUDGET_KEY;

/// The `wake_budget` sentinel for the NOTIFY FAST LANE (docs/NOTIFY.md §11.5).
/// Its own key beside the other three, and for the sharpest version of the same
/// reason: this lane runs at INGEST, in front of a user waiting for a buzz, so
/// sharing a counter with a pass that grinds a queue would let a backlog spend
/// the notification budget before today's mail ever arrived.
///
/// It is declared HERE, beside the other three, rather than next to the
/// [`notify_lane`] code that spends it: a sentinel invented later, somewhere
/// else, is a sentinel nobody ever checks these for a collision against.
const NOTIFY_FAST_BUDGET_KEY: &str = "__notify_fast__";
/// The usage-ledger category the fast lane books under. NOT a literal here: it
/// is the one category with prices of its own, and both cost estimators price
/// it by matching this exact string, so a second spelling would not be a
/// missing row, it would be a row silently costed at the Stage-1 model's rates
/// (see [`crate::metrics::estimate_cost_usd`]).
const NOTIFY_USAGE_CATEGORY: &str = crate::metrics::NOTIFY_USAGE_CATEGORY;

/// Which sync path an ingest batch is on. Decides ONE thing: whether a
/// notification-worthy verdict may append an `events` row. Backfill never
/// notifies (a fresh install must not fire a hundred pushes for a month of
/// already-read mail); every incremental path may, kept safe by the first-sight
/// test plus the one-event-per-message key even when `catch_up()` re-scans the
/// whole backfill window.
///
/// It is answered ONCE, here at ingest, and stamped on the row as
/// `triage.notify_eligible_at` (see [`notify_eligible_stamp`]) — this is the one
/// site that knows which path it is on, and the emission sites downstream cannot
/// re-derive it from anything the row carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IngestOrigin {
    /// First-run backfill. Structurally silent.
    Backfill,
    /// A history walk or a catch-up re-scan: mail that may be genuinely new.
    Incremental,
    /// The archive walk: mail older than the backfill window, stored to be
    /// found and for nothing else. Silent like a backfill, and beyond that it
    /// queues no model work, wakes no lane and moves no catch-up gauge (see
    /// [`Store::ingest_archive_message`]).
    ///
    /// A window read (the first backfill, a catch-up) also files its OLD BULK
    /// MAIL here, message by message: see [`files_as_old_bulk`]. That is not
    /// an origin of its own, because nothing else about the batch changes.
    Archive,
}

/// WHETHER A WINDOW READ FILES THIS MESSAGE AS ARCHIVE instead of triaging it:
/// list mail (a non-empty `List-Unsubscribe` header) that ARRIVED before
/// `floor`, that no Surface or Filtered sender rule matches, and that none of
/// the deterministic record detectors recognises. `floor` is `None` everywhere
/// but an inbox window read (the first backfill or a catch-up), so live
/// arrivals, the archive walk, sent mail and spam are never asked.
///
/// WHY: a new account's backfill used to queue a model call for every message
/// in a month of inbox, and about three quarters of that is newsletters and
/// promotions. A new tenant with an ordinary mailbox (75 messages a day,
/// 2,300 jobs at about 3 cents each) spent the month's model budget in two
/// days, mostly on three-week-old list mail that the first load never shows:
/// the first-load boards reach back a week. Mail from people, and anything
/// without the header, is triaged at any age in the window, exactly as before.
///
/// AGE IS WHEN THE PROVIDER RECEIVED IT (`arrived_at`: Gmail's `internalDate`,
/// Graph's `receivedDateTime`, IMAP's `INTERNALDATE`), falling back to the
/// stored `received_at` only for a source that reports none. Not the Date
/// header: a sender's misconfigured or stale `Date:` would file a newsletter
/// that arrived yesterday.
///
/// A SURFACE OR FILTERED RULE OUTRANKS IT. Either is the owner asking for this
/// sender's mail to be read (always shown, or shown when it matches what they
/// want), and an archive row would quietly ignore that. A Squelch rule asks
/// for the opposite, so its mail is filed like any other. Matched with the
/// one matcher the rest of the daemon uses ([`match_sender_rule`]). A first
/// run has no rules, so this only matters to a catch-up.
///
/// A RECORD OUTRANKS IT TOO. Tickets, bookings, bills, order confirmations
/// and shipping notices are often sent through the same bulk mailers that
/// send newsletters, header included, and a three-week-old booking for a
/// flight next month is exactly what the first load must show. Ingest no
/// longer extracts records itself (the triage agent does), so the four
/// deterministic detectors are asked here, as a yes or no and nothing more:
/// a hit is triaged like any other mail and what they found is dropped. They
/// are pure pattern matches over the message's own text, with no model and
/// no store, and they run only on mail that is already old list mail.
///
/// [`match_sender_rule`]: crate::triage::rules::match_sender_rule
fn files_as_old_bulk(
    triaged: &TriagedMessage,
    arrived_at: DateTime<Utc>,
    floor: Option<DateTime<Utc>>,
    rules: &[SenderRule],
) -> bool {
    let Some(floor) = floor else {
        return false;
    };
    let m = &triaged.message;
    let old_list_mail = !m.is_sent
        && !m.is_spam
        && m.list_unsubscribe
            .as_deref()
            .is_some_and(|h| !h.trim().is_empty())
        && arrived_at < floor;
    old_list_mail && !owner_reads_sender(&m.from_addr, rules) && !looks_like_a_record(m)
}

/// Whether a Surface or Filtered rule matches `from_addr` (see
/// [`files_as_old_bulk`]).
fn owner_reads_sender(from_addr: &str, rules: &[SenderRule]) -> bool {
    crate::triage::rules::match_sender_rule(from_addr, rules)
        .is_some_and(|r| r.disposition != crate::types::Disposition::Squelch)
}

/// Whether any deterministic record detector fires on the message: a bill, a
/// calendar update, a receipt or a shipment (see [`files_as_old_bulk`]).
/// Dated from the message's own `received_at`, which is what the detectors
/// anchor year-less dates to.
fn looks_like_a_record(m: &crate::types::NewMessage) -> bool {
    use crate::triage::{calendar, deadline, receipt, shipment};
    deadline::detect_bill(&m.subject, &m.body, m.received_at).is_some()
        || calendar::detect_calendar(
            &m.from_addr,
            m.from_name.as_deref(),
            &m.subject,
            &m.body,
            m.received_at,
        )
        .is_some()
        || receipt::detect_receipt(&m.from_addr, &m.subject, &m.body).is_some()
        || shipment::detect_shipment(&m.from_addr, &m.subject, &m.body).is_some()
}

/// What one change-feed walk found ([`SyncEngine::walk_changes`]).
struct Walked {
    /// The mailbox changed (or might have): the poll's idle ladder resets.
    moved: bool,
    /// Where the cursor may move, or `None` to hold it where it is.
    advance: Option<SyncCursor>,
}

/// Persisted message and optional embedding input. `embed_text` is `Some`
/// only when the stored row passes the sealed gate (`Store::message_embeddable`):
/// provider spam never, inbound mail not until a non-sealed decision lands
/// (the drain embeds it then), the owner's own mail at once.
struct Ingested {
    id: i64,
    embed_text: Option<String>,
    /// The row was stamped eligible to notify ([`notify_eligible_stamp`]),
    /// so the ingest transaction queued its `notification` job: the one case
    /// the notification lane is poked for.
    notify_eligible: bool,
    /// The row was written as an ARCHIVE row (old bulk mail on a window read,
    /// see [`files_as_old_bulk`]): it queued no work, so the refine lane is
    /// not poked for it.
    archived: bool,
}

/// The longest the refine lane sleeps between looks at an empty queue. Its
/// producers (ingest, the doors through `RefinePoke`) poke it awake, so this
/// bounds only what nobody poked for.
pub(crate) const MAX_LANE_IDLE_BACKOFF: Duration = Duration::from_secs(30);

/// MAY THIS MESSAGE EVER NOTIFY, and from when — the whole of docs/NOTIFY.md
/// §11.3, in one pure function so the engine and its tests cannot drift.
///
/// `Some(now)` only for mail we are seeing for the first time on an incremental
/// path, that is not the user's own sent copy, and whose sender-claimed `Date:`
/// was still inside `notify.freshness_window_secs` when we laid eyes on it.
/// Everything else is `None`, the silent direction.
///
/// [`events::is_fresh`] is asked HERE and nowhere else. That is the fix for the
/// bug this whole change exists for: asked at every emission site it measured
/// the sender's clock against the wall clock, so mail that WAS fresh when it
/// arrived and simply waited behind a queue of model calls aged out and was
/// dropped, silently, 24.7% of the time (docs/NOTIFY.md §2a). Asked once, it
/// answers the question it was always meant to ask.
fn notify_eligible_stamp(
    triaged: &TriagedMessage,
    origin: IngestOrigin,
    cfg: &crate::config::NotifyConfig,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let eligible = origin == IngestOrigin::Incremental
        && !triaged.message.is_sent
        && events::is_fresh(triaged.message.received_at, cfg, now);
    eligible.then_some(now)
}

/// WHICH FOLDER a fetch batch came from, and therefore how the rows land.
///
/// An enum rather than the pair of booleans it replaces. Two bools can spell
/// four states, only three of which mean anything, and the two that get passed
/// most often differ from each other by argument ORDER alone — the exact shape
/// of mistake that puts inbound mail in the sent bucket and makes it vanish from
/// every listing. One argument cannot be transposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mailbox {
    /// Ordinary received mail: the only batch that is triaged.
    Inbox,
    /// The user's own outbox. Neutral row, no LLM, seeds contacts.
    Sent,
    /// The provider's spam folder (Gmail's SPAM label). Neutral row, no LLM,
    /// hidden from every surface but the human door's spam page.
    Spam,
}

impl Mailbox {
    /// Whether one fetched message lands as sent. THE MESSAGE'S OWN PLACEMENT
    /// DECIDES when the provider reports it, not the walk that found it: the
    /// `labelId` filter on `history.list` leaks in both directions, and an
    /// INBOX walk has handed back the user's own outbound mail. The composer's
    /// send echo had already written that row as sent, the INBOX sighting
    /// MINed `is_sent` to 0, and every message one tenant sent from the app
    /// was stored as received mail and gone from the sent page.
    ///
    /// INBOX present is received, whichever walk found it, which keeps a note
    /// to self (both labels) the visible copy, as the walk order already does.
    /// Otherwise the INBOX walk defers to a SENT label, and the SENT walk keeps
    /// what it always did: a leaked archived message carrying neither label
    /// stays out of triage rather than surfacing mail a filter skipped the
    /// inbox for. No labels at all (a format that omits them) falls back to
    /// the walk. Spam stays spam: that batch is never sent mail.
    fn sighting_is_sent(self, placement: Placement) -> bool {
        if self == Mailbox::Spam {
            return false;
        }
        if placement.in_inbox {
            return false;
        }
        self == Mailbox::Sent || placement.in_sent
    }
    fn is_spam(self) -> bool {
        matches!(self, Mailbox::Spam)
    }
}

/// Reconnect / retry backoff bounds for the outer driver loop.
const BACKOFF_START: Duration = Duration::from_secs(2);
const BACKOFF_CAP: Duration = Duration::from_secs(5 * 60);

/// The longest the FIRST backfill waits on the embedder gate before going ahead
/// without it. The ceiling is not a deadline anybody is meant to hit, it is the
/// guarantee that a gate nobody ever opens (an init task wedged on a hung
/// download) degrades to exactly today's behaviour, an unembedded backfill the
/// vector pass drains later, rather than to a daemon that never syncs at all.
/// See [`SyncEngine::with_embedder_gate`].
///
/// THREE MINUTES, and short on purpose, because the wait is time a brand-new
/// tenant's mailbox is EMPTY: nothing is on the wire until the backfill runs.
/// A cached model loads in about a second and a cold download of the 126 MB
/// model takes tens of seconds on a normal link, so past this the init is not
/// coming back. Hosted makes that cost concrete today: readiness there is
/// TCP-only and there is no models volume yet, so every new signup's first run
/// IS the cold download, and a generous ceiling would show them an empty
/// mailbox for all of it. 15 minutes, the first cut of this, was also exactly
/// the documented sync-staleness alert threshold (900 s, see
/// `deploy/monitoring/README.md`), which would park every first-run tenant on
/// the alert line for the duration.
///
/// The trade against releasing early is bounded now: a backfill that starts
/// unembedded costs one vector pass at `embed.backfill_batch`, about +123 MB
/// at a batch of 8 under the shipped 256-token `embed.max_tokens` (+324 MB was
/// the same batch at the model's 512-token ceiling, and +1.7 GB was a batch of
/// 64 there). It is also paid AGAIN per retry, because a backfill that errors
/// before the cursor is stored comes back through here on the next lifecycle.
const EMBEDDER_GATE_CEILING: Duration = Duration::from_secs(3 * 60);

/// How a wait on the embedder gate ended. Every variant but `Settled` says the
/// same thing to a caller ("nothing more is coming, go ahead"); they are apart
/// because they read differently in a log and only one of them is worth a
/// metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedderGate {
    /// The embedder init resolved, whichever way it resolved.
    Settled,
    /// The sender is gone, so nothing is ever going to open this gate.
    Dropped,
    /// [`EMBEDDER_GATE_CEILING`] passed first.
    TimedOut,
}

/// Park until the embedder gate opens, the ceiling passes, or whoever would have
/// opened it goes away. Returns at once on a gate that is already open, so the
/// steady state (a restart, model on disk) pays nothing.
///
/// Public because TWO callers wait on this one bit for two different reasons.
/// The first backfill waits for memory ([`SyncEngine::with_embedder_gate`]). The
/// daemon's one-time Sent sweeps wait for Gmail quota: their stagger is meant to
/// sit past the startup sync burst, and the gate can now push the start of that
/// burst minutes out, which would leave a metadata GET per Sent message racing a
/// 30-day raw backfill on one credential.
pub async fn wait_for_embedder_gate(gate: &mut tokio::sync::watch::Receiver<bool>) -> EmbedderGate {
    let deadline = tokio::time::Instant::now() + EMBEDDER_GATE_CEILING;
    wait_for_embedder_gate_until(gate, deadline).await
}

/// [`wait_for_embedder_gate`] against a FIXED deadline, for a caller that may
/// have to park more than once (the sync engine re-enters after a shutdown
/// wakeup that carried no shutdown). The deadline is computed once by that
/// caller, so re-entering cannot re-arm the ceiling and turn a bounded wait
/// into an unbounded one.
pub async fn wait_for_embedder_gate_until(
    gate: &mut tokio::sync::watch::Receiver<bool>,
    deadline: tokio::time::Instant,
) -> EmbedderGate {
    if *gate.borrow() {
        return EmbedderGate::Settled;
    }
    tokio::select! {
        settled = gate.wait_for(|settled| *settled) => match settled {
            Ok(_) => EmbedderGate::Settled,
            Err(_) => EmbedderGate::Dropped,
        },
        _ = tokio::time::sleep_until(deadline) => EmbedderGate::TimedOut,
    }
}

/// Drop from `ids` every id also in `claimed`, preserving order. Used to hand
/// each later label walk only what the earlier ones did NOT already ingest: a
/// self-addressed message carries both INBOX and SENT, and re-ingesting a
/// message under a hiding label (`is_sent`, `is_spam`) would take the visible
/// copy off every listing. Pure, so the precedence rule is unit-testable.
fn subtract_ids(ids: Vec<String>, claimed: &[String]) -> Vec<String> {
    if claimed.is_empty() {
        return ids;
    }
    let claimed: std::collections::HashSet<&str> = claimed.iter().map(|s| s.as_str()).collect();
    ids.into_iter()
        .filter(|id| !claimed.contains(id.as_str()))
        .collect()
}

/// Clears the catch-up progress pair on the way out of `Syncer::catch_up`, on
/// every path including the `?` returns.
///
/// A guard rather than a call at the end, because the end is the one place a
/// catch-up reliably does NOT reach: it is a long walk over a network, and a
/// Gmail hiccup halfway through leaves by `?`. A pair left standing then would
/// report a run that is not happening, which is a worse lie than the silence
/// this replaced.
struct CatchUpGuard<'a> {
    metrics: &'a crate::metrics::SyncMetrics,
    window: &'a AtomicBool,
}

impl<'a> CatchUpGuard<'a> {
    /// Also raises `window_in_flight` for the guard's lifetime, so the one
    /// thing that says "a window is being fetched" is the same thing that says
    /// how far along it is, and neither can be left standing without the other.
    fn new(metrics: &'a crate::metrics::SyncMetrics, window: &'a AtomicBool) -> Self {
        window.store(true, Ordering::Relaxed);
        Self { metrics, window }
    }
}

impl Drop for CatchUpGuard<'_> {
    fn drop(&mut self) {
        self.window.store(false, Ordering::Relaxed);
        self.metrics.catchup_end();
    }
}

/// HOW LONG THE POLL LANE WAITS after a tick: `interval` after a tick that
/// found the mailbox changed and after the first idle tick (one quiet poll is
/// not an idle mailbox), then doubling per idle tick to `idle_cap`, whatever
/// the cap. With the cap at or under the interval (the default,
/// `idle_poll_secs` unset) every wait is the interval. Pure, so the ladder is
/// testable without a clock.
fn poll_wait(idle_rounds: u32, interval: Duration, idle_cap: Duration) -> Duration {
    interval
        .saturating_mul(2u32.saturating_pow(idle_rounds.saturating_sub(1)))
        .min(idle_cap.max(interval))
}

/// Which daily cap a budget-exhausted notice is about; each is rate-limited to
/// once per UTC day (see [`SyncEngine::warn_days`]).
#[derive(Debug, Clone, Copy)]
enum CapKind {
    /// The notify fast lane's daily cap ([`NOTIFY_FAST_BUDGET_KEY`]). Its own
    /// kind, not Revisit's, or a capped fast lane would go unmentioned on any
    /// day a revisit notice had already been logged.
    NotifyFast,
    /// The notify fast lane's CONFIG-FAILURE park (see [`notify_lane`]).
    ///
    /// A SEPARATE SLOT FROM `NotifyFast`, though both are about the same lane
    /// and both are worth saying once a day rather than once a message. They
    /// are the only two diagnoses for "the fast lane stopped notifying", they
    /// are unrelated faults, and sharing one slot means whichever fires first
    /// on a UTC day silences the other for the rest of it: a mailbox that
    /// exhausts the cap at 09:00 would swallow the 14:00 line naming a broken
    /// allow-list, on the day somebody is reading the log precisely because
    /// notifications stopped. The [[fleet LLM outage]] was a config-level 400
    /// that read as normal for four days.
    NotifyFastConfig,
    /// The translate pass's daily dollar budget
    /// (`[translate].daily_budget_usd`). Its stamp for today is also how
    /// ingest knows the translator has nothing left.
    Translate,
}

/// Outcome of a check-then-increment budget gate.
enum BudgetGate {
    Proceed,
    /// Cap hit: every remaining row this cycle is blocked.
    Exhausted,
    /// Budget read or increment failed: skip this row, try the next.
    SkipRow,
}

/// The last UTC day (`YYYY-MM-DD`) each cap kind's notice was emitted; re-armed
/// when the day rolls over, so a capped account logs once a day, not per poll.
#[derive(Default)]
struct WarnDays {
    notify_fast: Option<String>,
    notify_fast_config: Option<String>,
    translate: Option<String>,
}

/// The account's daily-budget ledger plus the warn-once state that goes with it:
/// everything a cap gate needs, and nothing else.
///
/// IT IS A STRUCT RATHER THAN THREE METHODS ON [`SyncEngine`] because the fast
/// lane (docs/NOTIFY.md §11.5) is its own `Arc`-held object rather than a method
/// on the engine, and it has to gate on the same code AND — the part a second
/// copy would quietly break — the same [`WarnDays`]. "Once per UTC day per cap
/// kind" is a promise about a shared slot; two `WarnDays` would make it twice,
/// on the day somebody is reading the log because notifications stopped.
///
/// Borrowed, so neither holder pays for a clone to ask a question, and the
/// engine's own three methods below are thin wrappers over it so every existing
/// call site is unchanged.
struct BudgetLedger<'a, S: Store + ?Sized> {
    store: &'a S,
    account_id: AccountId,
    warn_days: &'a std::sync::Mutex<WarnDays>,
}

impl<S: Store + ?Sized> BudgetLedger<'_, S> {
    /// True at most once per UTC `day` per cap `kind`, so a persistently-capped
    /// account logs each notice once a day rather than every poll. Stamps the
    /// day as a side effect; a poisoned lock defaults to warning, never to
    /// silently swallowing the notice.
    fn warn_once(&self, kind: CapKind, day: &str) -> bool {
        let mut guard = match self.warn_days.lock() {
            Ok(g) => g,
            Err(_) => return true,
        };
        let slot = match kind {
            CapKind::NotifyFast => &mut guard.notify_fast,
            CapKind::NotifyFastConfig => &mut guard.notify_fast_config,
            CapKind::Translate => &mut guard.translate,
        };
        if slot.as_deref() == Some(day) {
            false
        } else {
            *slot = Some(day.to_string());
            true
        }
    }

    /// An account-scoped daily budget gate over one `wake_budget` sentinel key.
    /// INCREMENT-BEFORE-CALL, so a retry storm cannot exceed the cap: a call
    /// that is about to be made is charged whether or not it comes back.
    fn gate(
        &self,
        key: &str,
        day: &str,
        cap: u32,
        kind: CapKind,
        label: &str,
        tail: &str,
    ) -> BudgetGate {
        match self.store.stage2_budget_used(self.account_id, key, day) {
            Ok(used) if used >= cap => {
                if self.warn_once(kind, day) {
                    eprintln!(
                        "squelch: {label} daily budget exhausted ({used}/{cap}); {tail} stay queued"
                    );
                }
                return BudgetGate::Exhausted;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("squelch: {label} budget read failed ({e}); skipping row");
                return BudgetGate::SkipRow;
            }
        }
        if let Err(e) = self
            .store
            .stage2_increment_budget(self.account_id, key, day)
        {
            eprintln!("squelch: {label} budget increment failed ({e}); skipping row");
            return BudgetGate::SkipRow;
        }
        BudgetGate::Proceed
    }

    /// Give back the charge for a call that was rejected at CONFIG level.
    ///
    /// The charge-before-call rule above is a retry-storm guard and it stays.
    /// What it must not do is let a broken config spend the day's cap on 4xxs
    /// that cost nothing: those are rejected in ~0ms, spend no tokens, and are
    /// identical for every queued row, so a handful of cycles can exhaust a
    /// 500-call budget. Since the pass also STOPS on a config failure and
    /// leaves its rows queued, the un-refunded charge outlives the outage — the
    /// budget key is the UTC DAY, so a gateway fixed at noon stays capped until
    /// midnight UTC. Refunding here is what makes "the outage is over" and "the
    /// fleet is triaging again" the same moment.
    ///
    /// Best-effort by design: a refund that fails must never turn a config
    /// failure into a second, louder failure. The worst case is the behaviour
    /// this method exists to fix, which is where we already were.
    fn refund(&self, key: &str, day: &str, label: &str) {
        if let Err(e) = self.store.stage2_refund_budget(self.account_id, key, day) {
            eprintln!("squelch: {label} budget refund failed ({e}); the cap keeps the charge");
        }
    }
}

/// Everything the sync loop needs, resolved once at startup.
pub struct SyncEngine<S: Store, C: CredentialStore + ?Sized> {
    store: Arc<S>,
    creds: Arc<C>,
    account_id: AccountId,
    /// The account's own email; passed to ingest so the user's own address is
    /// excluded from the Sent-derived contacts table.
    account_email: String,
    config: Config,
    http: reqwest::Client,
    /// The mailbox's Gmail quota budget, shared with every other caller for
    /// the same account (see [`crate::gmail_budget`]). An engine built without
    /// [`Self::with_gmail_budget`] gets its own, which is only right when it is
    /// the only Gmail caller in the process.
    gmail_budget: Arc<GmailBudget>,
    gmail_quota_backoff: Duration,
    /// The longest a catch-up request sleeps on its own between quota retries
    /// ([`GMAIL_QUOTA_RETRY_CAP`]; a field so a test can shrink it).
    gmail_quota_retry_cap: Duration,
    /// LLM key + provider + endpoint URL, resolved once at startup. `None`
    /// disables LLM triage gracefully: rows stay queued, one stderr notice,
    /// sync continues. The key is never logged.
    stage2_llm: Option<ResolvedLlm>,
    /// Embedder OVERRIDE; usually `None`, with [`SyncEngine::embedder`] falling
    /// back to the store's. Resolving per tick is what lets a LATE-attached
    /// embedder be picked up without a restart.
    embedder: Option<Arc<dyn crate::embed::Embedder>>,
    /// "The embedder init has RESOLVED, whichever way it resolved", flipped by
    /// the caller that builds the embedder in the background. The FIRST backfill
    /// waits on it and nothing else does. `None` means never wait: sync-only
    /// mode builds its embedder before the engine exists, and the tests have
    /// nothing to wait for. See [`SyncEngine::with_embedder_gate`].
    embedder_gate: Option<tokio::sync::watch::Receiver<bool>>,
    /// Manual-refresh signal: notifying it wakes the sleeping poll loop early.
    /// Coalescing is intentional — several pokes during one in-flight poll
    /// collapse into a single extra tick.
    refresh: Arc<tokio::sync::Notify>,
    /// HOW MANY CLIENTS ARE LISTENING: the open `GET /client/events` streams,
    /// counted by the human door and shared through
    /// [`SyncEngine::with_event_listeners`]. The poll lane's idle ladder
    /// (`sync.idle_poll_secs`) stays on its first rung while this is nonzero:
    /// an app that is open is a person waiting on their mail. Counted at the
    /// door rather than read off the store's event notifier, whose receivers
    /// include the daemon's own APNs pusher for the life of the process.
    /// Without the handle (`squelchd run`, tests) nobody is ever listening.
    event_listeners: Arc<std::sync::atomic::AtomicUsize>,
    /// POLL LANE -> REFINE LANE: poked once per poll tick that actually ingested
    /// something, so new mail starts through Stage-1 the moment it lands instead
    /// of waiting out whatever the refine lane is in the middle of.
    ///
    /// Not `Arc` and not shared outside the engine, because unlike `refresh`
    /// nothing outside the engine may poke it: it means "I just ingested rows",
    /// which is a statement of fact about this engine's own tick. Coalescing is
    /// what makes a busy tick cheap — `Notify` holds one permit, so twenty
    /// pokes during one grind are one extra pass.
    ///
    /// Poked by [`SyncEngine::fetch_raw_and_ingest`] and awaited by
    /// [`SyncEngine::refine_lane`]; see [`SyncEngine::run_lanes`] for why the
    /// two are separate futures at all.
    refine_wake: tokio::sync::Notify,
    /// REFINE LANE -> TRANSLATE LANE: poked at the top of every refine round,
    /// so the translate lane (docs/TRANSLATION.md, Pipeline) starts whenever
    /// the refine lane does (new mail, an archive thread the reader asked to
    /// translate, the interval) without either lane waiting on the other.
    translate_wake: tokio::sync::Notify,
    /// THE NOTIFICATION LANE'S WAKE: a job it claims may just have been
    /// queued. Poked by the two producers, this engine's own ingest of an
    /// incremental arrival (its `notification` job is queued inside the
    /// ingest transaction) and the investigation lane's commit of an
    /// arrival-eligible job (which queues the `deliberate_notification`),
    /// and awaited by [`SyncEngine::agent_notification_lane`], which
    /// otherwise backs off to `MAX_NOTIFY_IDLE_BACKOFF` between looks at an
    /// empty queue. Coalesces like `refine_wake`: a window of fifty arrivals
    /// is one extra round. The store's other producers of a push job (a
    /// correction, a rule change, a heal, a sent reply re-evaluating its
    /// thread) have no handle on this engine and do not poke; that cap is
    /// sized for them.
    notify_wake: tokio::sync::Notify,
    /// ON-DEMAND SPAM SIGNAL: notifying it makes the poll loop run ONE spam
    /// window sync on its next pass. Separate from `refresh` because it is a
    /// different request — `refresh` means "check my mail now", this means "go
    /// and get the folder we deliberately do not track" — and because
    /// collapsing them would put the spam fetch back on every manual refresh,
    /// which is most of what this change removed.
    spam_refresh: Arc<tokio::sync::Notify>,
    /// THE AGENT DOOR'S POKE at the refine lane: an agent search just queued
    /// access checks for archive rows and is waiting on them (see
    /// `store::archive_surface`). Its own handle rather than a second use of
    /// `refine_wake`, which stays private to this engine's own ingest; the two
    /// wake the same lane. It also carries the lane's answer to "would a poke
    /// be acted on", which the door reads before it waits (see
    /// [`Self::refine_lane`]).
    refine_poke: crate::store::archive_surface::RefinePoke,
    /// Per-cap-kind last-warned UTC day. In-memory only; a restart re-arms them,
    /// and one fresh notice on restart is acceptable.
    ///
    /// `Arc` because the FAST LANE shares this exact mutex rather than keeping a
    /// second one; see [`BudgetLedger`] for why that matters.
    warn_days: Arc<std::sync::Mutex<WarnDays>>,
    /// THE FAST LANE (docs/NOTIFY.md §11.5), built ON FIRST USE rather than in
    /// [`SyncEngine::new`].
    ///
    /// The lane holds CLONES of what the engine resolved — the metrics registry,
    /// the LLM — and two builder methods replace exactly those after `new`
    /// returns: `with_metrics` (which every production daemon calls, and whose
    /// whole purpose is that the engine records into the registry `/metrics`
    /// serves) and `without_stage2_llm`. A lane built in `new` would therefore
    /// count into an orphan registry in production and run the model path in the
    /// tests that exist to prove the no-model path. `OnceLock` makes the
    /// builders' order irrelevant instead of making every future builder
    /// remember to rebuild it, and it is safe because both builders take
    /// `mut self` and so can only run before the engine moves into `run()`.
    ///
    /// RECORDED as the fifth deliberate departure in docs/NOTIFY.md §11.9, so
    /// a later reader diffing the code against the contract finds it named
    /// rather than re-deriving whether it was on purpose.
    notify_lane: std::sync::OnceLock<Arc<notify_lane::NotifyLane<S>>>,
    /// Set while the INBOX unread fetch is failing, so a persistent failure
    /// (revoked scope, Gmail outage) says so ONCE instead of once per poll. The
    /// next success re-arms it.
    unread_warned: AtomicBool,
    /// The account's creation time, read from the store once and kept: it
    /// is stamped by `ensure_account` and never changes, and every triage
    /// job used to re-read it under the store mutex to learn "not new".
    /// Only a FOUND stamp is kept; a read that failed or found no row is
    /// asked again next time, so a store hiccup on the first job cannot pin
    /// a new account at the steady-state effort for its window.
    account_created: std::sync::OnceLock<DateTime<Utc>>,
    /// THE UNREAD COUNT IS OWED: fetch it on the next tick whether or not the
    /// walk moved. Set at boot (the stored pair may be a day old), by a manual
    /// refresh (somebody is looking), and by a fetch that failed (so the retry
    /// is per tick, as it was when every tick fetched). Otherwise the count is
    /// fetched only after a walk that found the mailbox changed: an idle
    /// mailbox cannot change its unread count, so asking Gmail every five
    /// seconds for a number that cannot have moved was a third of the idle
    /// request rate.
    unread_stale: AtomicBool,
    /// Unix seconds before which the Google profile (name and photo) is not
    /// re-fetched. Zero at start, so the first poll tick of a process tries.
    /// In memory on purpose: a restart costs one extra pair of small requests,
    /// which is cheaper than a column whose only job is to save them.
    profile_due: AtomicI64,
    /// The profile fetch's once-per-failing-streak latch, as `unread_warned`.
    profile_warned: AtomicBool,
    /// HAS GMAIL ANSWERED IN THIS PROCESS, and has it answered since the last
    /// failure. Set by [`SyncEngine::poll_lane`] the moment `poll_once` returns
    /// `Ok`, and by [`SyncEngine::backfill_inner`] the moment the first run's
    /// INBOX listing does; cleared by [`SyncEngine::run`] when a lifecycle
    /// bubbles an `Err`. Read by [`SyncEngine::refine_lane`], which spends no
    /// model call while it is false.
    ///
    /// THIS EXISTS BECAUSE A CANCELLED MODEL CALL IS STILL BILLED. The lane split
    /// made the refine lane a sibling of the poll lane under one `select!`, so a
    /// Gmail `Err` now drops a refine future that may be mid-classify, and
    /// [`SyncEngine::gate_budget`] charges BEFORE the call (that is what makes the
    /// cap a cap) while nothing refunds a cancellation. `run()`'s backoff is
    /// initialised outside its loop and caps at [`BACKOFF_CAP`], so a credential
    /// that has gone `invalid_grant` retries every five minutes forever: without
    /// this gate, each cycle re-entered `run_lanes`, the refine lane ran
    /// synchronously through `gate_budget` and issued a POST before `poll_lane`
    /// reached its first real await, and the whole thing was cancelled one Gmail
    /// round trip later. `stage2.global_daily_cap` defaults to 120 and the budget
    /// key is the UTC DAY, so ten hours of Gmail downtime drained the entire day's
    /// Stage-2 budget on verdicts that never landed, and nothing escalated again
    /// until midnight even after the credential was fixed.
    ///
    /// FALSE AT STARTUP, deliberately, so not even the first cycle can spend on a
    /// dead credential: the cost is that the refine lane waits out one poll
    /// interval on a healthy daemon before its first steady-state round, which
    /// is `sync.poll_secs` — 5 seconds by default. A FIRST RUN has no poll to
    /// wait for: the refine lane runs beside the backfill there (see
    /// [`SyncEngine::run_once`]), and the backfill's own INBOX listing is the
    /// proof that flips this, see [`SyncEngine::backfill_inner`].
    ///
    /// It restores exactly what the serial loop gave for free: `poll_once().await?`
    /// returned before any pass started, so a Gmail outage cost zero model calls.
    /// `Relaxed` throughout: this is a hint that costs at most one pass either way,
    /// never a lock.
    poll_healthy: AtomicBool,
    /// DID THIS LIFECYCLE POLL, for [`SyncEngine::run`]'s backoff alone. It used
    /// to read `poll_healthy` for that, which was fine while only a poll could
    /// set it. Now a first run's listing sets `poll_healthy` too (so the refine
    /// lane may spend beside the backfill), and a first run that lists fine and
    /// then fails the same way every time (one body Gmail will not serve, a
    /// scope that covers `list` but not `get`) must still back off to the cap
    /// rather than re-list the whole window every two seconds. Set only by the
    /// poll lane, swapped out by `run()`.
    polled_ok: AtomicBool,
    /// A WINDOW OF MAIL IS BEING FETCHED: the first backfill or a catch-up.
    /// Held true by [`CatchUpGuard`] for exactly as long as the progress pair
    /// stands, and read by [`SyncEngine::refine_lane`], which does not drain
    /// vectors while it is. Its own flag rather than the progress gauge because
    /// the gauge is for display, and control flow hung off a display value
    /// comes back to bite when the display changes.
    window_in_flight: AtomicBool,
    /// The archive walk's waits. [`ArchivePace::default`] outside the tests.
    archive_pace: ArchivePace,
    /// What the archive walk remembers between chunks and tells the other
    /// lanes. See [`ArchiveRun`].
    archive: ArchiveRun,
    /// SOME MESSAGE MAY BE MISSING ITS VECTOR, so the next refine round should
    /// run the drain. Raised at construction (rows ingested before this
    /// process had an embedder, and failures from an earlier run, are what the
    /// first drain is for), by every arm of [`SyncEngine::embed_and_store`]
    /// that did not write a vector, by an applied triage decision (inbound
    /// mail is embedded only once it is decided, so a decision is a row the
    /// drain may now owe a vector), and by a drain that stopped on an error;
    /// lowered by a drain as it starts, so a failure that lands while it runs
    /// is not lost under its clear. Without this the drain ran EVERY round,
    /// and in steady state every non-spam row already has a vector, so its
    /// `NOT EXISTS` never short-circuited: a walk of the whole account with a
    /// vec0 probe per row, 50k probes every 30 s idle and every second while
    /// triage was busy, under the store mutex, for nothing.
    ///
    /// An `Arc`, because the debt is a property of the STORE and more than
    /// one engine writes to it: the daemon's sweeps engine runs the blank-body
    /// heal, which deletes a healed row's vector for this drain to redo, and
    /// its raise has to land on the flag the sync engine's refine lane reads.
    /// [`SyncEngine::with_vectors_dirty`] hands one flag to every engine over
    /// one store.
    vectors_dirty: Arc<AtomicBool>,
    /// The window during which a remote embedder is not called after a
    /// transient failure. Never opens for a local embedder. See
    /// [`embed_backoff`].
    embed_backoff: embed_backoff::EmbedBackoff,
    /// Shared cooldown after provider failure. Rejected configuration must not
    /// trigger one request per queued message or consume message retry attempts.
    agent_retry_after: AtomicI64,
    /// How many provider probes have failed in a row since an investigation
    /// last succeeded: the rung of the throttle cooldown's ladder
    /// (`triage_worker::throttle_cooldown_secs`). The access lane's fast
    /// model answering does not reset it.
    agent_throttle_strikes: AtomicU32,
    /// The access lane's own ladder: its fast model is another model,
    /// throttled and recovering on its own, so its failures climb this one
    /// and its successes reset it, and neither touches the investigation
    /// lane's.
    agent_access_strikes: AtomicU32,
    /// Whether an investigation has answered since the last investigation
    /// timed out: the account-level half of the "two timeouts in a row"
    /// rule in `triage_worker::retry_agent`, which fails a job only when
    /// one has. Raised when an investigation answers (a whole run, or an
    /// earlier turn of a run that then failed), lowered by every timeout.
    /// Starts lowered: a fresh process has seen no answer, and a job whose
    /// previous attempt timed out before a roll must not be failed on the
    /// strength of the restart. The access lane's fast model does not raise
    /// it.
    agent_answered_since_timeout: AtomicBool,
    /// When the account's last provider cooldown ended (or will end), as a
    /// timestamp: the throttle episode's own stamp, which no success clears
    /// (a success clears `agent_retry_after`), so `open_cooldown` can tell
    /// a fresh throttle hours after an old episode from the next probe of
    /// a continuing one. Zero until a cooldown has been opened.
    agent_last_cooldown_end: AtomicI64,
    /// THE TRANSLATOR (docs/TRANSLATION.md): `None` when translation is
    /// switched off or no LLM is configured. A trait object so the tests can
    /// stand a scripted one in its place.
    translator: Option<Arc<dyn crate::translate::TranslateBackend>>,
    /// The translate pass's own cooldown after a configuration failure (unix
    /// seconds). Separate from `agent_retry_after`: the translate model can be
    /// missing from an allow-list that carries the triage model.
    translate_retry_after: AtomicI64,
    /// When the TRANSLATE MODEL last answered this account (unix ms, 0 = not
    /// since start): a translation outcome that is not a configuration
    /// failure or an outage. What the release cap needs before it charges a
    /// row for a 400, which may be about the model rather than the message.
    translate_answer_ms: AtomicI64,
    /// When the GATEWAY last answered this account (unix ms): the above, or
    /// a successful agent triage run or access assessment on the same
    /// gateway and key. Enough for the cap to charge a row that times out.
    /// Neither moves in a total outage, so an outage never spends attempts.
    gateway_answer_ms: AtomicI64,
    /// Scrape-facing counters. Its own registry unless the daemon shares one
    /// via [`SyncEngine::with_metrics`], so an engine built anywhere (tests, the
    /// contacts harvest, `run`) still records rather than branching on absence.
    metrics: Arc<SyncMetrics>,
    /// Gmail REST base for every call this engine's Gmail source makes. Always
    /// [`GMAIL_API_BASE`] in production; the tests point it at a loopback mock so
    /// the walk/cursor rules are asserted on the wire rather than on a struct.
    api_base: String,
    /// Graph base for an Outlook account's source. Always [`GRAPH_API_BASE`]
    /// in production; test-only setter, for the reason `api_base` has none.
    graph_api_base: String,
    /// THE ACCOUNT'S MAIL SOURCE, picked from its `provider` row and built ON
    /// FIRST USE, for the reason [`Self::notify_lane`] is: it holds clones of
    /// what the builders (`with_metrics`, `with_gmail_budget`) replace after
    /// `new`, so one built in `new` would book against an orphan budget and
    /// count into an orphan registry. A provider read that fails is not
    /// cached; the next caller asks again.
    source: std::sync::OnceLock<Arc<dyn MailSource>>,
    /// The cursor this engine last read from or wrote to the store, for
    /// [`MailSource::wait_for_change`] between ticks without a store read per
    /// tick. `None` until the first.
    last_cursor: std::sync::Mutex<Option<SyncCursor>>,
    /// The daemon's live hidden features, for the triage worker's
    /// [`crate::features::UNCAPPED_TRIAGE`] and the owner profile's
    /// [`crate::features::UNDERSTANDING`] (`sync/understanding.rs`). Nothing
    /// is on without [`Self::with_features`].
    features: crate::features::Features,
    /// Whether the last triage pass saw `uncapped_triage` on, so the jobs the
    /// caps parked are released once when it flips on, not every pass.
    uncapped_seen: AtomicBool,
    /// Where [`Self::refresh_profile`] asks who the account is. Always
    /// [`profile::PEOPLE_ME_URL`] in production; only the tests in that module
    /// assign it, for the same reason `api_base` has no public setter.
    profile_url: String,
}

/// The "a vector may be missing" flag's lifecycle across ONE drain pass:
/// lowered by the pass as it starts, and raised again on drop unless the pass
/// set `drained`, which it does only when the queue came back empty or short.
/// Every other ending, a query error, a failed batch, or the future being
/// dropped mid-pass when `run_lanes` restarts the lanes, leaves the debt on
/// the books for the next round. See [`SyncEngine::backfill_missing_vectors`].
struct DrainGuard<'a> {
    flag: &'a AtomicBool,
    drained: bool,
}

impl Drop for DrainGuard<'_> {
    fn drop(&mut self) {
        if !self.drained {
            self.flag.store(true, Ordering::Relaxed);
        }
    }
}

impl<S: Store + 'static, C: CredentialStore + 'static + ?Sized> SyncEngine<S, C> {
    pub fn new(
        store: Arc<S>,
        creds: Arc<C>,
        account_id: AccountId,
        account_email: String,
        config: Config,
    ) -> Self {
        config
            .triage
            .validate()
            .expect("invalid agent triage configuration");
        // Timeouts keep a hung connection from wedging the poll loop.
        // Redirects are REFUSED deliberately: this client carries credentials
        // (Gmail bearer token, LLM x-api-key) and reqwest re-sends custom
        // headers cross-host on a redirect. Gmail's API does not redirect, so
        // this matches the repo's other Google-facing clients.
        // 180s, not 60: the long pole on this client is now a reasoning-model
        // triage call at high effort, which legitimately thinks for minutes on a
        // hard row. A Gmail fetch that runs that long is already wedged, and
        // this timeout only exists to unwedge it. An agent turn the operator
        // allows to run longer sets its own request timeout
        // (`llm::classify_agent_step`), so this one never cuts it.
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client build");
        // Absence => graceful disable, one notice, no key material logged.
        let stage2_llm = config.stage2.resolve_llm();
        if stage2_llm.is_none() {
            eprintln!(
                "squelch: no Stage-2 API key set (SQUELCH_STAGE2_API_KEY / ANTHROPIC_API_KEY / \
                 OPENAI_API_KEY) — agent triage unavailable (messages stay pending; \
                 sync continues)"
            );
        }
        let gmail_budget = Arc::new(GmailBudget::new(config.sync.gmail_quota_profile));
        let translator = crate::translate::TranslateClient::from_config(&config)
            .map(|c| Arc::new(c) as Arc<dyn crate::translate::TranslateBackend>);
        Self {
            store,
            creds,
            account_id,
            account_email,
            config,
            http,
            gmail_budget,
            gmail_quota_backoff: Duration::from_secs(1),
            gmail_quota_retry_cap: GMAIL_QUOTA_RETRY_CAP,
            stage2_llm,
            embedder: None,
            embedder_gate: None,
            refresh: Arc::new(tokio::sync::Notify::new()),
            event_listeners: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            refine_wake: tokio::sync::Notify::new(),
            translate_wake: tokio::sync::Notify::new(),
            notify_wake: tokio::sync::Notify::new(),
            spam_refresh: Arc::new(tokio::sync::Notify::new()),
            refine_poke: Default::default(),
            warn_days: Arc::new(std::sync::Mutex::new(WarnDays::default())),
            notify_lane: std::sync::OnceLock::new(),
            unread_warned: AtomicBool::new(false),
            account_created: std::sync::OnceLock::new(),
            unread_stale: AtomicBool::new(true),
            profile_due: AtomicI64::new(0),
            profile_warned: AtomicBool::new(false),
            // Nothing has polled yet, so nothing may spend yet.
            poll_healthy: AtomicBool::new(false),
            polled_ok: AtomicBool::new(false),
            window_in_flight: AtomicBool::new(false),
            archive_pace: ArchivePace::default(),
            archive: ArchiveRun::default(),
            vectors_dirty: Arc::new(AtomicBool::new(true)),
            embed_backoff: embed_backoff::EmbedBackoff::default(),
            agent_retry_after: AtomicI64::new(0),
            agent_throttle_strikes: AtomicU32::new(0),
            agent_access_strikes: AtomicU32::new(0),
            agent_answered_since_timeout: AtomicBool::new(false),
            agent_last_cooldown_end: AtomicI64::new(0),
            translator,
            translate_retry_after: AtomicI64::new(0),
            translate_answer_ms: AtomicI64::new(0),
            gateway_answer_ms: AtomicI64::new(0),
            metrics: SyncMetrics::new(),
            api_base: GMAIL_API_BASE.to_string(),
            graph_api_base: GRAPH_API_BASE.to_string(),
            profile_url: profile::PEOPLE_ME_URL.to_string(),
            source: std::sync::OnceLock::new(),
            last_cursor: std::sync::Mutex::new(None),
            features: crate::features::Features::none(),
            uncapped_seen: AtomicBool::new(false),
        }
    }

    /// Share the daemon's [`SyncMetrics`] so what this engine records is what
    /// `/metrics` serves: create ONE at startup and hand a clone to each side.
    /// Without it the engine still counts, into a registry nobody scrapes.
    pub fn with_metrics(mut self, metrics: Arc<SyncMetrics>) -> Self {
        self.metrics = metrics;
        self
    }

    /// Share the daemon's live hidden features (the human door's handle), so
    /// the triage worker sees an operator's tick within one refresh.
    pub fn with_features(mut self, features: crate::features::Features) -> Self {
        self.features = features;
        self
    }

    /// Share the mailbox's [`GmailBudget`] with every other Gmail caller for
    /// this account. Create ONE per account at startup; two engines on one
    /// mailbox with a budget each is how a tenant asked for 160% of its quota.
    pub fn with_gmail_budget(mut self, budget: Arc<GmailBudget>) -> Self {
        self.gmail_budget = budget;
        self
    }

    /// The budget this engine books its Gmail requests against.
    pub fn gmail_budget(&self) -> &Arc<GmailBudget> {
        &self.gmail_budget
    }

    /// Point every Gmail call at `base` instead of [`GMAIL_API_BASE`]. Test-only:
    /// a build that could be aimed at an arbitrary host would be a bearer-token
    /// exfiltration primitive.
    #[cfg(test)]
    pub(crate) fn with_api_base(mut self, base: String) -> Self {
        // The profile fetch goes wherever Gmail goes: a mocked engine must not
        // be one poll tick away from a real request to Google. The mock has no
        // such route, which reads as "no profile", exactly like an old grant.
        self.profile_url = format!("{base}/people/me");
        self.graph_api_base = base.clone();
        self.api_base = base;
        self
    }

    /// Share a manual-refresh [`Notify`](tokio::sync::Notify) so the human door's
    /// `POST /client/refresh` can wake the poll loop between intervals: create
    /// ONE at daemon startup and hand a clone to each side. Without it the engine
    /// still polls on its own interval, just never early.
    pub fn with_refresh(mut self, refresh: Arc<tokio::sync::Notify>) -> Self {
        self.refresh = refresh;
        self
    }

    /// Share the human door's count of open event streams
    /// (`ApiState::with_event_listeners` in `squelch-api`), so the poll lane's
    /// idle ladder stays at its interval while a client has the app open.
    /// Create ONE at daemon startup and hand a clone to each side.
    pub fn with_event_listeners(mut self, listeners: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        self.event_listeners = listeners;
        self
    }

    /// Share the ON-DEMAND SPAM signal, so the human door's spam page can ask
    /// for a fetch of a folder this loop otherwise never touches. Wire the same
    /// handle into `ApiState::with_spam_refresh`; without it the endpoint is a
    /// no-op and the page stays on whatever was last fetched.
    pub fn with_spam_refresh(mut self, spam_refresh: Arc<tokio::sync::Notify>) -> Self {
        self.spam_refresh = spam_refresh;
        self
    }

    /// Share the REFINE POKE, so a door that just queued work somebody is
    /// waiting on (an agent search surfacing archive mail) starts the refine
    /// lane now instead of at its next interval. Wire the same handle into
    /// `ApiState::with_refine_poke` and the agent door; without it that work
    /// still runs, on the lane's own clock.
    pub fn with_refine_poke(
        mut self,
        refine_poke: crate::store::archive_surface::RefinePoke,
    ) -> Self {
        self.refine_poke = refine_poke;
        self
    }

    /// Attach an [`Embedder`](crate::embed::Embedder) OVERRIDE, for callers that
    /// build one eagerly and want it used even if the store's copy differs.
    /// Usually unnecessary — [`SyncEngine::embedder`] falls back to the store's —
    /// and absence keeps sync fully functional, just writing no vectors.
    pub fn with_embedder(mut self, embedder: Arc<dyn crate::embed::Embedder>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// Share the "a vector may be missing" flag with another engine over the
    /// same store, so a raise on either lands where the drain reads it. The
    /// daemon builds its sweeps engine with the sync engine's handle; see the
    /// field.
    pub fn with_vectors_dirty(mut self, flag: Arc<AtomicBool>) -> Self {
        self.vectors_dirty = flag;
        self
    }

    /// This engine's "a vector may be missing" flag, to hand to another
    /// engine through [`SyncEngine::with_vectors_dirty`].
    pub fn vectors_dirty_handle(&self) -> Arc<AtomicBool> {
        self.vectors_dirty.clone()
    }

    /// The EFFECTIVE embedder for this tick: the override, else whatever is
    /// attached to the store RIGHT NOW — so an embedder attached in the
    /// background after startup is picked up without a restart. Until then this
    /// is `None`, ingest skips the vector write, and the backfill pass fills in.
    fn embedder(&self) -> Option<Arc<dyn crate::embed::Embedder>> {
        self.embedder.clone().or_else(|| self.store.embedder())
    }

    /// Hold the FIRST backfill until `gate` reads true, meaning the caller's
    /// embedder init has RESOLVED, whichever way it resolved. Nothing else in
    /// the engine waits on it: the poll loop and the catch-up run whether or not
    /// there is an embedder, exactly as they do today.
    ///
    /// Why it exists. `squelchd serve` builds the embedder on a background task
    /// so both doors come up at once, and a brand-new tenant's first run starts
    /// its 30-day backfill in that same instant. Without the gate the backfill
    /// races the model download: thousands of rows ingest with no vector because
    /// [`SyncEngine::embed_and_store`] no-ops when there is no embedder to call,
    /// and [`SyncEngine::backfill_missing_vectors`] then has to drain all of them
    /// in batches. A batch is what the memory cost scales with, and it is memory
    /// the allocator does not give back (see `EmbedConfig::backfill_batch`), so
    /// that race is how a tenant daemon ends up permanently over a 1 Gi pod
    /// limit. Waiting turns it back into the cheap path: ingest writes each
    /// vector as it goes, one message at a time, and the batch pass only ever
    /// sees leftovers.
    ///
    /// The wait is bounded by [`EMBEDDER_GATE_CEILING`]; past it the backfill
    /// proceeds exactly as it did before this existed.
    pub fn with_embedder_gate(mut self, gate: tokio::sync::watch::Receiver<bool>) -> Self {
        self.embedder_gate = Some(gate);
        self
    }

    /// The wait [`SyncEngine::with_embedder_gate`] describes. `true` means "go
    /// ahead with the backfill", which is every outcome except one: `false` says
    /// shutdown arrived while we were parked, and there is no point starting a
    /// 30-day backfill nobody will be around to finish.
    ///
    /// Returns at once, silently, with no gate or with one already open, so the
    /// steady state (a restart, where the model is on disk and the init resolves
    /// in well under the time it takes to reach here) logs nothing.
    async fn wait_for_embedder(&self, shutdown: &mut tokio::sync::watch::Receiver<bool>) -> bool {
        // Cloned because waiting needs `&mut` and the engine is shared.
        let Some(mut gate) = self.embedder_gate.clone() else {
            return true;
        };
        if *gate.borrow() {
            return true;
        }
        eprintln!("squelch: waiting for the embedder before the first backfill");
        // One deadline for the whole wait, however many times it parks: a
        // wakeup on the shutdown watch that carries no shutdown re-enters the
        // select below, and a ceiling armed per entry would be no ceiling.
        let deadline = tokio::time::Instant::now() + EMBEDDER_GATE_CEILING;
        loop {
            tokio::select! {
                how = wait_for_embedder_gate_until(&mut gate, deadline) => {
                    match how {
                        EmbedderGate::Settled => {
                            eprintln!("squelch: embedder settled; starting the first backfill")
                        }
                        EmbedderGate::Dropped => eprintln!(
                            "squelch: embedder gate dropped; the first backfill is running without the \
                             embedder, and its vectors will be drained by the vector backfill pass"
                        ),
                        EmbedderGate::TimedOut => {
                            self.metrics.record_embedder_gate_timeout();
                            eprintln!(
                                "squelch: embedder still not settled after {}m; the first backfill is \
                                 running without the embedder, and its vectors will be drained by the \
                                 vector backfill pass",
                                EMBEDDER_GATE_CEILING.as_secs() / 60
                            )
                        }
                    }
                    return true;
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        return false;
                    }
                    // A wakeup that did not carry a shutdown (a same-value send,
                    // or the sender going away) is not permission to run the
                    // backfill unembedded: the gate is still the thing being
                    // waited on, and the deadline above still bounds it. Nothing
                    // in production sends `false` today; this is what keeps a
                    // future second signal on that watch from silently
                    // reopening the 2026-08-19 memory profile.
                }
            }
        }
    }

    /// THE ACCOUNT'S MAIL SOURCE: the `provider` row, read once, picks it.
    /// An account whose provider this build does not know is an error on
    /// every call, never a Gmail account by default; the lifecycle reports it
    /// and backs off like any other failure.
    fn source(&self) -> Result<Arc<dyn MailSource>> {
        if let Some(source) = self.source.get() {
            return Ok(source.clone());
        }
        let built: Arc<dyn MailSource> = match self.store.account_provider(self.account_id)? {
            MailProvider::Gmail => Arc::new(GmailSource::new(self.gmail_parts())),
            MailProvider::Outlook => Arc::new(GraphSource::new(self.graph_parts())),
            MailProvider::Imap => Arc::new(self.imap_source()?),
        };
        Ok(self.source.get_or_init(|| built).clone())
    }

    /// An IMAP account's source, from what `squelchd auth --imap` stored: the
    /// server config and the password, sealed together in the credential
    /// slot. Never `self.creds`, which is a Google token store: nothing
    /// Google is read for this account. The store is the source's location
    /// table.
    ///
    /// A slot from an earlier build holds the password alone, with the server
    /// in `app_settings`: it is moved into the slot here, once (the CA file
    /// it named read now), and the setting deleted.
    fn imap_source(&self) -> Result<ImapSource> {
        let backend = self.config.credential_backend;
        let path = self.config.resolve_credentials_path();
        let text = crate::credentials::load_imap_slot_backend(backend, &path, &self.account_email)?;
        let credential = match ImapCredential::from_slot(&text) {
            StoredImapSlot::Credential(c) => c,
            StoredImapSlot::LegacyPassword(password) => {
                let stored = self
                    .store
                    .get_app_setting(self.account_id, IMAP_ACCOUNT_SETTING)?
                    .ok_or_else(|| {
                        CoreError::InvalidInput(
                            "this imap account has no server stored: run `squelchd auth --imap`"
                                .into(),
                        )
                    })?;
                let c = ImapCredential::from_legacy(password, &stored)?;
                crate::credentials::store_imap_slot_backend(
                    backend,
                    &path,
                    &self.account_email,
                    &c.to_slot()?,
                )?;
                self.store
                    .delete_app_setting(self.account_id, IMAP_ACCOUNT_SETTING)?;
                eprintln!(
                    "squelch: moved this imap account's server settings into its credential slot"
                );
                c
            }
        };
        Ok(ImapSource::from_config(
            self.account_id,
            &credential.config,
            credential.password,
            self.store.clone(),
            self.archive.refusals.clone(),
        )?
        .with_backfill_days(self.config.sync.backfill_days))
    }

    /// What this engine's Gmail source is built from, as the engine stands.
    fn gmail_parts(&self) -> GmailSourceParts<C> {
        GmailSourceParts {
            // The SAME client as the LLM calls, so the pool and the redirect
            // refusal are shared: the bearer token must never be re-sent
            // cross-host.
            http: self.http.clone(),
            creds: self.creds.clone(),
            account_id: self.account_id,
            budget: self.gmail_budget.clone(),
            metrics: self.metrics.clone(),
            refusals: self.archive.refusals.clone(),
            api_base: self.api_base.clone(),
            quota_backoff: self.gmail_quota_backoff,
            quota_retry_cap: self.gmail_quota_retry_cap,
        }
    }

    /// What this engine's Outlook source is built from. The same HTTP client
    /// (redirects refused) and the same refusal counters as Gmail's; Graph's
    /// quota is Microsoft's per-mailbox throttle, so the Gmail budget does not
    /// apply.
    fn graph_parts(&self) -> GraphSourceParts<C> {
        GraphSourceParts {
            http: self.http.clone(),
            creds: self.creds.clone(),
            account_id: self.account_id,
            account_email: self.account_email.clone(),
            metrics: self.metrics.clone(),
            refusals: self.archive.refusals.clone(),
            api_base: self.graph_api_base.clone(),
            quota_retry_cap: self.gmail_quota_retry_cap,
        }
    }

    /// A Gmail source over this engine's parts, for the tests that drive the
    /// request layer itself (retries, pacing, the budget) rather than a walk.
    #[cfg(test)]
    fn gmail_source(&self) -> GmailSource<C> {
        GmailSource::new(self.gmail_parts())
    }

    /// Stand `source` in for the one the provider row would pick. Test-only,
    /// for the reason [`Self::with_api_base`] is.
    #[cfg(test)]
    fn with_source(self, source: Arc<dyn MailSource>) -> Self {
        assert!(
            self.source.set(source).is_ok(),
            "with_source after the source was built would be ignored"
        );
        self
    }

    /// [`MailSource::list`] on this account's source.
    async fn list(
        &self,
        listing: Listing,
        cap: Option<usize>,
        urgency: Urgency,
    ) -> Result<Vec<String>> {
        self.source()?.list(listing, cap, urgency).await
    }

    /// A folder over the history window: what the backfill, the catch-up and
    /// the spam sync list.
    fn window_listing(&self, folder: Folder) -> Listing {
        Listing::Folder {
            folder,
            within_days: Some(self.config.sync.backfill_days),
        }
    }

    /// [`MailSource::changes`] on this account's source, with an expired
    /// cursor surfacing as [`CoreError::NotFound`], which is what the walk and
    /// [`Self::poll_once`] have always branched on.
    async fn changes(&self, since: &SyncCursor, folder: Folder) -> Result<Changes> {
        self.source()?
            .changes(since, folder)
            .await
            .map_err(|e| match e {
                ChangesError::Expired => CoreError::NotFound,
                ChangesError::Other(e) => e,
            })
    }

    /// One full lifecycle: the first-run backfill if there is no cursor yet,
    /// then the steady-state lanes ([`Self::run_lanes`]) until an error bubbles
    /// up (caller retries with backoff) or shutdown.
    ///
    /// THE FIRST RUN IS NOT SERIAL ANY MORE. It used to be backfill, then one
    /// inline triage batch, then the lanes, on the theory that the first thing
    /// the user saw should be a triaged inbox rather than an untriaged one. In
    /// practice the inline batch was `concurrency` rows out of a month of mail
    /// and the rest drained in the refine lane once the lanes started, so what
    /// the user actually saw was "Catching up N of M" for the length of the
    /// download and then a board that filled in a trickle. Now the refine lane
    /// runs BESIDE the backfill: ingest queues a `backfill` job per row as it
    /// lands, the lane claims in ingest order, which is the listing's order,
    /// newest first, and the board fills while the download is still going.
    ///
    /// The known-contact signal is what this costs. Contacts are seeded from
    /// the SENT leg, which runs after INBOX, so a first-run verdict on an INBOX
    /// row sees its sender as a stranger. Accepted for the first pass: a fast
    /// first board beats a marginally better one, and a re-triage repairs any
    /// row it turns out to matter for.
    ///
    /// The backfill sits at the HEAD OF THE POLL LANE'S FUTURE rather than in
    /// front of the lanes (see [`Self::run_lanes`]): there is no cursor to poll
    /// from until it establishes one, so "backfill, then poll" is the poll
    /// lane's own story, and telling it there is what makes the refine lane ONE
    /// future across both phases. A select boundary between the two would be a
    /// cancellation point on every healthy first run: the model calls in flight
    /// when the last body landed would be dropped mid-call, paid for (the budget
    /// charges before the call and never refunds) and their rows left leased
    /// for the whole lease, untriaged on the board a new user is looking at.
    async fn run_once(&self, shutdown: &mut tokio::sync::watch::Receiver<bool>) -> Result<()> {
        // The provider first: an account this build cannot sync stops here,
        // before a backfill is waited for or a lane is started.
        let source = self.source()?;
        eprintln!(
            "squelch: {} sync starting for <redacted account>",
            match source.provider() {
                MailProvider::Gmail => "gmail REST",
                MailProvider::Outlook => "outlook graph",
                MailProvider::Imap => "imap",
            }
        );

        // First run (no cursor) => full backfill + seed contacts.
        let first_run = self.load_cursor()?.is_none();
        // BEFORE the backfill, and only before the FIRST one: every row it
        // ingests gets its vector written at ingest, one message at a time,
        // provided the embedder is there to write it. Start without one and
        // the whole 30-day window lands unembedded, to be drained in batches
        // afterwards instead. See [`SyncEngine::with_embedder_gate`].
        if first_run && !self.wait_for_embedder(shutdown).await {
            return Ok(());
        }

        self.run_lanes(shutdown, first_run).await
    }

    /// First-run backfill: INBOX bodies over the window, then SENT headers to
    /// seed contacts, then persist the source's current cursor.
    ///
    /// The trim runs WHATEVER THE OUTCOME. A run that failed halfway through the
    /// window still parsed and stored everything up to the failure, and that
    /// peak is the highest watermark this process ever reaches; leaving it in
    /// glibc's arenas because the last call errored would make a failed first
    /// run the most expensive thing a pod ever does. See [`crate::mem`].
    async fn backfill(&self, ahead: &StdMutex<Option<SyncCursor>>) -> Result<SyncCursor> {
        let out = self.backfill_inner(ahead).await;
        crate::mem::trim_off_runtime().await;
        out
    }

    /// THE FIRST RUN: the backfill, and LIVE POLLING BESIDE IT.
    ///
    /// A backfill is minutes of fetches (Gmail's quota is about four bodies a
    /// second), and mail that arrives while it runs is exactly the mail the
    /// owner is watching for: the new account's first proof that the app
    /// works. So when the source mints its cursor before the window
    /// ([`MailSource::cursor_before_window`]), [`Self::poll_during_backfill`]
    /// walks the change feed from it every poll interval while the download
    /// goes on, and that mail arrives as live mail (it notifies, and it is
    /// not first-load backlog).
    ///
    /// `tokio::join!`, NOT `select!`: neither half may cancel the other
    /// mid-walk. The poller only ever moves the shared cursor forward and
    /// finishes its walk before the join returns, and the cursor is stored
    /// ONCE, here, after both: never inside the backfill, where a walk still
    /// in flight could lose its advance. A walk that loses the race only
    /// replays on the first ordinary poll, which the store collapses.
    async fn first_run(&self, shutdown: &tokio::sync::watch::Receiver<bool>) -> Result<()> {
        let ahead = StdMutex::new(None);
        let finished = AtomicBool::new(false);
        let done = tokio::sync::Notify::new();
        let (backfilled, ()) = tokio::join!(
            async {
                let out = self.backfill(&ahead).await;
                finished.store(true, Ordering::Release);
                done.notify_one();
                out
            },
            self.poll_during_backfill(&ahead, &finished, &done, shutdown),
        );
        let minted = backfilled?;
        let cursor = ahead
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .unwrap_or(minted);
        self.store_cursor(&cursor)?;
        eprintln!(
            "squelch: history cursor established (backfill window from {})",
            self.backfill_since()
        );
        Ok(())
    }

    /// The live half of [`Self::first_run`]: every `sync.poll_secs`, until the
    /// backfill finishes or shutdown, walk the change feed from the cursor the
    /// backfill minted (`ahead`) and move it forward. Idle for a source that
    /// mints no cursor before its window.
    ///
    /// NOTHING HERE FAILS THE RUN. A walk's error is logged and the next
    /// interval tries again: the backfill is what decides whether the first
    /// run worked, and a live-path refusal must not throw away minutes of
    /// download. Nor does it stamp poll health (`polled_ok`, `note_poll_ok`):
    /// that is the ordinary poll's to say once the first run is over.
    async fn poll_during_backfill(
        &self,
        ahead: &StdMutex<Option<SyncCursor>>,
        finished: &AtomicBool,
        done: &tokio::sync::Notify,
        shutdown: &tokio::sync::watch::Receiver<bool>,
    ) {
        let interval = Duration::from_secs(self.config.sync.poll_secs.max(1));
        loop {
            tokio::select! {
                () = tokio::time::sleep(interval) => {}
                () = done.notified() => {}
            }
            if finished.load(Ordering::Acquire) || *shutdown.borrow() {
                return;
            }
            let start = ahead
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let Some(start) = start else { continue };
            match self.walk_changes(&start).await {
                Ok(walk) => {
                    if let Some(next) = walk.advance {
                        *ahead
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(next);
                    }
                }
                // Error strings from this crate never carry secrets.
                Err(e) => {
                    eprintln!("squelch: live poll during the first backfill failed ({e}); retrying")
                }
            }
        }
    }

    /// The backfill proper. Returns the cursor to store; [`Self::first_run`]
    /// stores it, after the live walks beside it have had their say. A
    /// source that mints its cursor before the window leaves it in `ahead`
    /// before anything is listed, which is what those walks start from.
    async fn backfill_inner(&self, ahead: &StdMutex<Option<SyncCursor>>) -> Result<SyncCursor> {
        let early_cursor = self.cursor_ahead_of_window().await?;
        if let Some(cursor) = &early_cursor {
            *ahead
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(cursor.clone());
        }

        // INBOX bodies.
        let inbox_ids = self
            .list(self.window_listing(Folder::Inbox), None, Urgency::Window)
            .await?;
        // GMAIL ANSWERED, and a first run has no poll to say so. The refine
        // lane runs beside this backfill (see [`Self::run_once`]) and spends
        // nothing until this flag is true; the listing is the same proof a
        // poll gives, that the credential works, so it is set here, before the
        // first body is fetched, and the lane starts on rows as they land.
        self.poll_healthy.store(true, Ordering::Relaxed);
        // THE FIRST RUN IS PROGRESS, not an outage. It runs for many minutes
        // before the first poll, and without a denominator the client could
        // only say "not synced yet" over it. Reported on the catch-up pair,
        // which is what it is from outside: a known amount of mail arriving.
        let _guard = CatchUpGuard::new(&self.metrics, &self.window_in_flight);
        self.metrics.catchup_begin(inbox_ids.len() as u64);
        // Backfill NEVER notifies (see `IngestOrigin`).
        let n = self
            .fetch_raw_and_ingest(
                &inbox_ids,
                Mailbox::Inbox,
                IngestOrigin::Backfill,
                Urgency::Window,
            )
            .await?;
        eprintln!("squelch: backfilled {n} INBOX messages");

        // SENT bodies, not just headers, so semantic recall covers WHAT THE USER
        // WROTE. Contacts still come from To/Cc; the row is stored neutral
        // (tier=noise, importance=0) and the is_sent exclusions keep it out of
        // triage/updates/search.
        let sent_ids = self
            .list(self.window_listing(Folder::Sent), None, Urgency::Window)
            .await?;
        self.metrics
            .catchup_begin_extend(inbox_ids.len() as u64 + sent_ids.len() as u64);
        let seeded = self
            .fetch_raw_and_ingest(
                &sent_ids,
                Mailbox::Sent,
                IngestOrigin::Backfill,
                Urgency::Window,
            )
            .await?;
        eprintln!("squelch: backfilled {seeded} SENT messages (bodies for recall + contacts)");

        // NO SPAM LEG HERE, deliberately. See `sync_spam_window`: the spam
        // folder is fetched only when somebody opens the page that shows it.

        // The cursor to establish: minted before the window, or (for a
        // source that snapshots its own position) asked for now.
        match early_cursor {
            Some(cursor) => Ok(cursor),
            None => self.source()?.current_cursor().await,
        }
    }

    /// THE STEADY STATE: two lanes, not one (docs/NOTIFY.md §11.2).
    ///
    /// This used to be a single serial conga line — poll, then Stage-1, then
    /// Stage-2, then the extractors, then the revisits, then sleep — and that
    /// shape is what made mail arrive LATE rather than SLOWLY. A refine pass
    /// spends one reasoning-model call per row and a batch can be minutes wide;
    /// for every one of those minutes the daemon was not asking Gmail whether
    /// anything new had landed, because the fetch was queued behind the
    /// thinking. New mail sat unfetched behind the classification of OLD mail,
    /// which is the exact inversion of what the user wants, and it is also how
    /// a notification aged past its freshness window before anybody had ever
    /// looked at the message (docs/NOTIFY.md §2a).
    ///
    /// So: fetching and refining are separate futures, polled concurrently by
    /// one `tokio::select!` over `&self`. NO `tokio::spawn`, no `'static`, no
    /// new `Arc`s — both lanes borrow the engine, which is what keeps this a
    /// restructuring of one function rather than a new ownership story.
    ///
    /// - The poll lane's `Err` (Gmail auth/quota) ends the `select!` and bubbles
    ///   to [`Self::run`]'s backoff exactly as it did when this was one loop.
    ///   The refine lane is then cancelled at its next await, which leaves the
    ///   in-flight row queued (`stage1_model_used` NULL) — the same state a
    ///   crash leaves it in, and the same state the next lifecycle picks up.
    ///   ONE THING THAT IS NEW, and it costs money rather than correctness: the
    ///   cancelled await can now be a model call, which the old serial loop
    ///   could not be (`poll_once().await?` returned before any pass started).
    ///   [`Self::gate_budget`] charges BEFORE the call so a retry storm cannot
    ///   exceed the cap, and nothing refunds a cancellation, so the cancelled
    ///   verdict is paid for out of a DAY-scoped budget and never lands.
    ///   The charge-before-call rule stays (a refund path would be a second way
    ///   to move the counter, and charging first is what makes the cap a cap);
    ///   what bounds the damage is the `poll_healthy` gate, which stops the
    ///   refine lane re-entering a pass on the next backoff cycle while the
    ///   credential is still broken. So a multi-hour Gmail outage costs at most
    ///   the one round that was in flight when it started, not one per cycle.
    /// - The refine lane never returns an `Err` at all: every failure inside the
    ///   passes is handled and logged internally, so a model outage must not be
    ///   able to bounce the Gmail lifecycle.
    /// - The two shutdown receivers are the SAME channel. The second is a clone
    ///   because `changed()` needs `&mut`, and a clone starts out having seen
    ///   the current value, which is why each lane still checks `borrow()` at
    ///   the top of its own loop rather than trusting the wakeup.
    ///
    /// `backfill_first` is the first run: the poll lane's future then begins
    /// with the backfill and continues into polling, and the other two lanes
    /// run across both unbroken. See [`Self::run_once`] for why that shape and
    /// not a select in front of this one.
    async fn run_lanes(
        &self,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
        backfill_first: bool,
    ) -> Result<()> {
        let mut refine_shutdown = shutdown.clone();
        let mut notify_shutdown = shutdown.clone();
        let mut archive_shutdown = shutdown.clone();
        let mut translate_shutdown = shutdown.clone();
        tokio::select! {
            polled = async {
                if backfill_first {
                    self.first_run(shutdown).await?;
                }
                self.poll_lane(shutdown).await
            } => polled,
            () = self.agent_notification_lane(&mut notify_shutdown) => Ok(()),
            // Only ever completes on shutdown; it has no error to report.
            () = self.refine_lane(&mut refine_shutdown) => Ok(()),
            // The same. Its own lane so a long translation never holds up
            // triage of new mail (docs/TRANSLATION.md, Pipeline).
            () = self.translate_lane(&mut translate_shutdown) => Ok(()),
            // The same: a Gmail error inside the walk is the walk's to wait
            // out, never a reason to bounce the lifecycle.
            () = self.archive_lane(&mut archive_shutdown) => Ok(()),
        }
    }

    /// THE ARCHIVE WALK: mail older than the backfill window, fetched once,
    /// slowly, newest first, and stored to be found (see
    /// [`Store::ingest_archive_message`]). One 7-day chunk at a time backwards
    /// from the persisted cursor until it reaches `now - sync.archive_days`.
    ///
    /// No `Result`, for the refine lane's reason: this lane is no part of
    /// "is the mailbox syncing", so nothing that goes wrong in it may end the
    /// `select!`. An error is logged, waited out and the same chunk retried.
    /// The poll lane stays the only future whose `Err` reaches [`Self::run`].
    ///
    /// CANCELLATION IS A RESUME. When the poll lane errors this future is
    /// dropped at whatever await it is on. Each message is one synchronous
    /// store transaction, so a row is either wholly there or absent; the
    /// cursor only moves after a whole chunk, so the next lifecycle lists the
    /// same chunk again, drops the ids already stored and fetches the rest. A
    /// row cancelled between its commit and its vector write is picked up by
    /// the refine lane's `backfill_missing_vectors`, like any other.
    ///
    /// It is idle until a poll has succeeded, for the same reason the refine
    /// lane is (`poll_healthy`): a dead credential must not be retried by two
    /// lanes. And it yields to a window of mail being fetched: a first
    /// backfill or a catch-up is mail somebody is waiting for, and the two
    /// would otherwise split one Gmail budget.
    async fn archive_lane(&self, shutdown: &mut tokio::sync::watch::Receiver<bool>) {
        loop {
            if *shutdown.borrow() {
                return;
            }
            let wait = match self.archive_walk_chunk(shutdown).await {
                // Straight on to the next chunk; the rests are inside.
                Ok(ArchiveStep::Walked) => Duration::ZERO,
                Ok(ArchiveStep::Held) => self.archive_pace.held_recheck,
                Ok(ArchiveStep::Done | ArchiveStep::Off) => self.archive_pace.done_recheck,
                Err(e) => {
                    let wait = self.archive_error_wait();
                    // Error strings from this crate never carry secrets.
                    eprintln!(
                        "squelch: archive walk error ({e}); retrying in {}s",
                        wait.as_secs()
                    );
                    wait
                }
            };
            if !Self::archive_rest(wait, shutdown).await {
                return;
            }
        }
    }

    /// Sleep `d` unless shutdown arrives first; `false` means it did.
    async fn archive_rest(d: Duration, shutdown: &mut tokio::sync::watch::Receiver<bool>) -> bool {
        if *shutdown.borrow() {
            return false;
        }
        let sleep = tokio::time::sleep(d);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                () = &mut sleep => return true,
                changed = shutdown.changed() => {
                    if *shutdown.borrow() {
                        return false;
                    }
                    // The sender is gone without ever saying stop: nothing
                    // will wake this again, so the sleep is all that is left.
                    if changed.is_err() {
                        sleep.await;
                        return true;
                    }
                }
            }
        }
    }

    /// WHAT, IF ANYTHING, STOPS THE WALK TOUCHING GMAIL RIGHT NOW. See
    /// [`Self::archive_lane`]. A poll that has not succeeded comes first: a
    /// window is only ever in flight under one that has.
    fn archive_gate(&self) -> Option<ArchiveHold> {
        if !self.poll_healthy.load(Ordering::Relaxed) {
            Some(ArchiveHold::Poll)
        } else if self.window_in_flight.load(Ordering::Relaxed) {
            Some(ArchiveHold::Window)
        } else {
            None
        }
    }

    /// A HOLD IS SAID ONCE, WHEN IT STARTS OR CHANGES, and shown for as long
    /// as it lasts. The lane looks again every thirty seconds, so the log gets
    /// one line per stretch and the gauge carries the rest. `None` (walking,
    /// finished, off) clears the gauge and says so if something had been
    /// holding it. Returns [`ArchiveStep::Held`] for a hold, for the caller's
    /// convenience.
    fn archive_hold(&self, hold: Option<ArchiveHold>) -> ArchiveStep {
        self.metrics.note_archive_hold(hold);
        let (before, room) = {
            let mut state = self.archive.state();
            (
                std::mem::replace(&mut state.hold, hold),
                state.room_note.clone(),
            )
        };
        if before != hold {
            self.archive.hold_changes.fetch_add(1, Ordering::Relaxed);
            match hold {
                Some(hold) => eprintln!(
                    "squelch: archive walk holding ({}): {}",
                    hold.label(),
                    match hold {
                        ArchiveHold::Poll => "no poll has succeeded yet",
                        ArchiveHold::Window => "a window of mail is being fetched",
                        ArchiveHold::Embedder => "the embedder has not finished loading",
                        // The numbers, from the check that just refused.
                        ArchiveHold::Disk | ArchiveHold::DbSize => room.as_str(),
                        ArchiveHold::Clock => "the clock reads before 2024",
                        ArchiveHold::QuotaDay => "Gmail refused it three times today",
                    }
                ),
                None => eprintln!("squelch: archive walk no longer holding"),
            }
        }
        ArchiveStep::Held
    }

    /// Whether the vector drain has anything to do: this engine's
    /// `vectors_dirty`, or a write the store saw that no engine made (a
    /// person's correction through the API lifting a restriction), whose debt
    /// becomes this flag's here.
    fn vectors_owed(&self) -> bool {
        if self.store.take_vectors_owed() {
            self.vectors_dirty.store(true, Ordering::Relaxed);
        }
        self.vectors_dirty.load(Ordering::Relaxed)
    }

    /// May the refine lane drain missing vectors this round. See the call.
    fn may_drain_vectors(&self) -> bool {
        !self.window_in_flight.load(Ordering::Relaxed)
            && !self.archive.in_flight.load(Ordering::Relaxed)
    }

    /// HAS THE EMBEDDER INIT RESOLVED, whichever way. The first backfill waits
    /// on the same gate ([`Self::with_embedder_gate`]) with a ceiling, because
    /// a new user is looking at an empty mailbox; the walk has nobody waiting
    /// and simply does not start until it has. A year of rows stored with no
    /// embedder is a year of rows for the batch drain, which is the memory
    /// profile the gate exists to avoid. No gate, or one whose sender is gone,
    /// is nothing to wait for.
    fn archive_embedder_settled(&self) -> bool {
        match &self.embedder_gate {
            None => true,
            Some(gate) => *gate.borrow() || gate.has_changed().is_err(),
        }
    }

    /// The walk's clock: the wall clock outside the tests.
    fn archive_now(&self) -> DateTime<Utc> {
        self.archive.state().clock.unwrap_or_else(Utc::now)
    }

    /// IS THERE ROOM TO STORE MORE, and if not, which guard says so. Asked
    /// BEFORE EVERY BATCH as well as before every chunk: a chunk is a whole
    /// week of mail, however much that is, and these are the only things
    /// between a heavy week and a full volume. A number that cannot be read
    /// is no room: the walk is the one writer here that can always wait.
    /// Nothing else asks this. Live mail is stored whatever it answers.
    ///
    /// THE FREE-SPACE FLOOR is `sync.archive_min_free_mb` or a fifth of the
    /// whole filesystem ([`ARCHIVE_FLOOR_VOLUME_SHARE`], capped at 5 GiB),
    /// whichever is larger,
    /// measured on the filesystem the database file is on. The share is there
    /// because that filesystem can be one every tenant's database is on: a
    /// fixed 2 GiB is then the fleet's last 2 GiB, and each walk would write
    /// until it was. ONCE HELD it resumes only at the floor plus a tenth
    /// ([`ARCHIVE_FLOOR_RESUME_MARGIN`]); without the margin a walk sitting
    /// at the floor starts, stores a batch, and stops, every thirty seconds.
    ///
    /// THE DATABASE CEILING is `sync.archive_max_db_mb` on this database's own
    /// file and log: the one limit that is this account's alone, where free
    /// space on a shared volume is whatever the neighbours left.
    ///
    /// Says nothing itself. The numbers are left for [`Self::archive_hold`],
    /// which logs a hold once, when it starts.
    fn archive_room(&self) -> Option<ArchiveHold> {
        const MIB: u64 = 1024 * 1024;
        let sync = &self.config.sync;
        if sync.archive_min_free_mb == 0 && sync.archive_max_db_mb == 0 {
            self.archive.state().disk_short = false;
            return None;
        }
        let disk = match self.archive.state().disk_probe {
            Some(probe) => probe(),
            None => self.store.archive_disk().ok(),
        };
        let mut state = self.archive.state();
        let Some(disk) = disk else {
            // With the free-space check switched off, an unreadable volume
            // holds nothing, as it never did: 0 means "whatever the disk
            // says". The ceiling goes unread with it, on the one kind of
            // host (no `statvfs`) where nothing here can be measured.
            if sync.archive_min_free_mb == 0 {
                state.disk_short = false;
                return None;
            }
            state.room_note = "the database volume could not be measured".into();
            return Some(ArchiveHold::Disk);
        };
        if sync.archive_min_free_mb == 0 {
            state.disk_short = false;
        } else {
            let floor = archive_floor_bytes(sync.archive_min_free_mb, disk.total_bytes);
            let needed = if state.disk_short {
                floor.saturating_add(floor / ARCHIVE_FLOOR_RESUME_MARGIN)
            } else {
                floor
            };
            state.disk_short = disk.free_bytes < needed;
            if state.disk_short {
                state.room_note = format!(
                    "{} MiB free on the database volume; the floor is {} MiB (the larger of \
                     sync.archive_min_free_mb and a fifth of the volume, capped at 5 GiB) and it resumes at {} MiB",
                    disk.free_bytes / MIB,
                    floor / MIB,
                    floor.saturating_add(floor / ARCHIVE_FLOOR_RESUME_MARGIN) / MIB
                );
                return Some(ArchiveHold::Disk);
            }
        }
        let ceiling = sync.archive_max_db_mb.saturating_mul(MIB);
        if ceiling != 0 && disk.db_bytes >= ceiling {
            state.room_note = format!(
                "the database is {} MiB, at its {} MiB ceiling (sync.archive_max_db_mb)",
                disk.db_bytes / MIB,
                sync.archive_max_db_mb
            );
            return Some(ArchiveHold::DbSize);
        }
        None
    }

    /// How long the lane waits after the error the last chunk returned: the
    /// quota backoff when Gmail refused it, else the error backoff that error
    /// earned ([`ArchivePace::error_backoff`], doubling per consecutive
    /// failure up to [`ArchivePace::error_backoff_cap`]).
    fn archive_error_wait(&self) -> Duration {
        let state = self.archive.state();
        state
            .quota_wait
            .or(state.error_wait)
            .unwrap_or(self.archive_pace.error_backoff)
    }

    /// One step of the archive walk, or the reason there was none: every
    /// reason to hold first, then the chunk itself, then the bookkeeping its
    /// outcome calls for.
    ///
    /// A QUOTA REFUSAL backs the walk off for [`ArchivePace::quota_backoff`],
    /// doubling per refusal up to [`ArchivePace::quota_backoff_cap`], and the
    /// third in one UTC day ends the walk until the next. The refusal has
    /// already paused and slowed the shared budget for the poll lane and the
    /// write door; mail somebody is waiting for must not keep paying for mail
    /// nobody is.
    ///
    /// ANY OTHER ERROR fails the chunk and leaves the cursor, to be tried
    /// again whole. The one thing that ever lets a failing chunk through is a
    /// MESSAGE that has itself failed on [`ArchivePace::poison_attempts`]
    /// separate passes (see [`Self::archive_skip`]), so one message Gmail will
    /// not serve cannot stop the walk at its week for good, and a bad quarter
    /// of an hour at Gmail cannot make it drop mail it never retried.
    async fn archive_walk_chunk(
        &self,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<ArchiveStep> {
        let days = self.config.sync.archive_days.min(ARCHIVE_DAYS_MAX);
        if days == 0 {
            return Ok(ArchiveStep::Off);
        }
        if let Some(gate) = self.archive_gate() {
            return Ok(self.archive_hold(Some(gate)));
        }
        if !self.archive_embedder_settled() {
            return Ok(self.archive_hold(Some(ArchiveHold::Embedder)));
        }
        let now = self.archive_now();
        // A CLOCK THAT IS PLAINLY WRONG persists nothing. The walk's start is
        // written once from this instant: a board that boots at 1970 would
        // otherwise store a start decades back and read as finished for ever
        // once the clock was set.
        if now.timestamp() < ARCHIVE_CLOCK_FLOOR_UNIX {
            return Ok(self.archive_hold(Some(ArchiveHold::Clock)));
        }
        let today = now.date_naive();
        {
            let mut state = self.archive.state();
            if state.quota_day != Some(today) {
                state.quota_day = Some(today);
                state.quota_refusals_today = 0;
            }
            let refused = state.quota_refusals_today >= self.archive_pace.quota_refusals_per_day;
            drop(state);
            if refused {
                return Ok(self.archive_hold(Some(ArchiveHold::QuotaDay)));
            }
        }
        if let Some(full) = self.archive_room() {
            return Ok(self.archive_hold(Some(full)));
        }
        self.archive_hold(None);

        let refusals = self.archive.refusals.quota.load(Ordering::Relaxed);
        self.archive.skipped.store(0, Ordering::Relaxed);
        let out = self.archive_walk_step(shutdown, now, days).await;
        let refused = self.archive.refusals.quota.load(Ordering::Relaxed) != refusals;
        let pace = self.archive_pace;
        let mut state = self.archive.state();
        match &out {
            Ok(ArchiveStep::Walked) => {
                // The chunk is behind the cursor; so is what failed in it.
                state.failing = None;
                state.skips_counted.clear();
                state.quota_wait = None;
                state.quota_backoff = None;
                state.error_wait = None;
                state.error_backoff = None;
            }
            // A finished walk asked the provider and was answered (or had
            // nothing to ask): the run of failures is over too.
            Ok(ArchiveStep::Done) => {
                state.error_wait = None;
                state.error_backoff = None;
            }
            Err(_) if refused => {
                let wait = state.quota_backoff.unwrap_or(pace.quota_backoff);
                state.quota_backoff = Some((wait * 2).min(pace.quota_backoff_cap));
                state.quota_wait = Some(wait);
                state.quota_refusals_today += 1;
                if state.quota_refusals_today >= pace.quota_refusals_per_day {
                    eprintln!(
                        "squelch: archive walk stopped until tomorrow (UTC): Gmail refused it {} \
                         times today",
                        state.quota_refusals_today
                    );
                }
            }
            // Nothing is counted against the CHUNK. What may eventually be
            // stepped over is one message that has itself failed, pass after
            // pass, and that is booked where it fails ([`Self::archive_skip`]).
            //
            // THE WAIT GROWS WITH THE RUN OF FAILURES. Most errors here are
            // transient (a laptop waking with no network yet, one dropped
            // connection) and are gone in seconds, so the first retry comes
            // quickly; one that keeps failing is an outage, and the wait
            // doubles toward the cap so the walk stops knocking every few
            // seconds. Separate from the quota backoff, which is untouched.
            Err(_) => {
                state.quota_wait = None;
                let wait = state.error_backoff.unwrap_or(pace.error_backoff);
                state.error_backoff = Some((wait * 2).min(pace.error_backoff_cap));
                state.error_wait = Some(wait);
            }
            Ok(_) => {}
        }
        out
    }

    /// The chunk [`Self::archive_walk_chunk`] wraps: a forward one when one is
    /// due, else the next one backwards.
    ///
    /// LISTS ALL MAIL backwards, with no `labelIds`: the archive is everything
    /// the mailbox holds, read or filed, received or sent, and `is_sent` comes
    /// from each message's own labels. Gmail leaves spam and trash out of a
    /// listing unless asked for them; drafts and chats are excluded by the
    /// query.
    ///
    /// IDS ALREADY STORED ARE DROPPED BEFORE ANY FETCH. That is the quota
    /// saving (a list call costs a quarter of one body) and it is the whole
    /// resume mechanism: a chunk interrupted halfway is listed again and only
    /// its unfetched half is paid for.
    ///
    /// [`Urgency::Background`]: a quota refusal ends the chunk rather than being
    /// slept on in place. Retrying in place (`Window`) is for a window
    /// somebody is waiting on, where losing position costs a re-list of
    /// thousands; here position is the stored rows and costs nothing to
    /// recover.
    ///
    /// THE CURSOR MOVES LAST, after every id of the chunk is ingested, so an
    /// error or a cancellation anywhere above leaves it where it was.
    async fn archive_walk_step(
        &self,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
        now: DateTime<Utc>,
        days: u32,
    ) -> Result<ArchiveStep> {
        let pace = self.archive_pace;
        // Relative to NOW, every chunk: a walk that finished last month is
        // still finished, and one resumed after a week away has a week less to
        // do at the far end.
        let target = now - ChronoDuration::days(i64::from(days));
        let walked_to = match self.store.archive_walked_to(self.account_id)? {
            Some(at) => at,
            // FIRST USE. The walk starts where the backfill window ended, and
            // the store leaves creating the row to the one caller that knows
            // where that is. The forward edge starts at the same instant.
            //
            // AND NEVER NEARER TO NOW THAN THE FORWARD PASS'S OWN LAG. The
            // backward listing carries no `-in:inbox`, so with a backfill
            // window of zero days it would start at this instant and could
            // store a message that arrived a second ago before the poll
            // ingests it: an archive row is write-once, so that message would
            // never be triaged or notified. A day back, the live paths have
            // long since had their turn.
            None => {
                let start = (now - ChronoDuration::days(i64::from(self.config.sync.backfill_days)))
                    .min(now - pace.forward_lag);
                self.store.set_archive_walked_to(self.account_id, start)?;
                start
            }
        };
        let done = walked_to <= target;
        self.metrics.note_archive_walk(walked_to, done);

        if let Some(step) = self.archive_forward_chunk(shutdown, now).await? {
            return Ok(step);
        }
        if done {
            return Ok(ArchiveStep::Done);
        }

        let from = (walked_to - pace.chunk).max(target);
        // The source owns the edges: Gmail's listing starts a second early so
        // neighbouring chunks overlap whichever way it rounds the boundary
        // (see `mail::gmail::listing_params`); the overlap costs nothing,
        // because its ids are already stored.
        let listing = Listing::Window {
            from,
            to: walked_to,
            filed_only: false,
        };
        let key = (false, walked_to.timestamp());
        let Some((stored, rest)) = self.archive_ingest_listing(listing, key, shutdown).await?
        else {
            return Ok(ArchiveStep::Held);
        };

        self.store.set_archive_walked_to(self.account_id, from)?;
        self.metrics.note_archive_walk(from, from <= target);
        // One line per chunk that fetched something: a count and a date,
        // nothing of the mail. An empty week says nothing.
        if stored > 0 {
            eprintln!(
                "squelch: archive walk stored {stored} message(s), back to {}",
                from.format("%Y-%m-%d")
            );
        }
        // A chunk is hundreds of raw bodies parsed and dropped; hand the pages
        // back before the long rest rather than holding them through it.
        crate::mem::trim_off_runtime().await;
        Self::archive_rest(rest, shutdown).await;
        Ok(ArchiveStep::Walked)
    }

    /// THE FORWARD PASS: mail newer than the walk's start that carries neither
    /// INBOX nor SENT. The live paths list those two labels and nothing else,
    /// so mail a filter filed past the inbox, or that was archived before this
    /// daemon first synced, is in no listing they make and behind the edge the
    /// backward walk started from. `None` when no pass is due.
    ///
    /// IT MUST NEVER TAKE A MESSAGE THE LIVE PATH WOULD HAVE TRIAGED. An
    /// archive row is write-once and a live sighting of one is a no-op, so a
    /// message stored here first would stay untriaged for good. Three things
    /// keep it from that, and it needs all three:
    ///
    /// - `filed_only` (Gmail's `-in:inbox -in:sent`): a message that carries
    ///   either label now is the live paths' (history walks those labels, a
    ///   catch-up lists them) and is never listed here.
    /// - The upper bound is a day behind THE LAST SUCCESSFUL POLL, not behind
    ///   the clock. A poll that succeeded has replayed every arrival up to
    ///   that instant, whatever labels the mail has since lost; a message
    ///   older than that which the store still does not hold was never going
    ///   to arrive by history. Measured from the clock instead, a daemon
    ///   suspended for two days (a laptop lid) would wake with this lane and
    ///   the poll lane both due, and the walk could store mail that arrived in
    ///   the inbox and was archived on a phone during the gap, a moment before
    ///   the poll replayed it. No poll yet this lifecycle is no pass at all.
    /// - The day itself, for whatever is still settling at the head of the
    ///   mailbox: a message delivered but not yet in a history page, a send
    ///   whose SENT label lands after its first sighting.
    ///
    /// What is left is mail that had neither label when it was listed and is
    /// given INBOX by hand afterwards: see the note on [`ArchivePace::
    /// forward_lag`].
    ///
    /// One chunk of [`ArchivePace::chunk`] per call, like the backward walk,
    /// until the edge reaches the bound; then nothing for [`ArchivePace::
    /// done_recheck`], so a caught-up walk costs one small listing an hour.
    async fn archive_forward_chunk(
        &self,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
        now: DateTime<Utc>,
    ) -> Result<Option<ArchiveStep>> {
        let pace = self.archive_pace;
        if self
            .archive
            .state()
            .forward_caught_up
            .is_some_and(|at| at.elapsed() < pace.done_recheck)
        {
            return Ok(None);
        }
        let polled = self.archive.last_poll_ok_unix.load(Ordering::Relaxed);
        let Some(polled) = DateTime::from_timestamp(polled, 0).filter(|_| polled > 0) else {
            return Ok(None);
        };
        let bound = polled.min(now) - pace.forward_lag;
        let Some(covered_to) = self.store.archive_covered_to(self.account_id)? else {
            return Ok(None);
        };
        self.metrics.note_archive_covered_to(covered_to);
        if covered_to >= bound {
            self.archive.state().forward_caught_up = Some(tokio::time::Instant::now());
            return Ok(None);
        }
        let to = (covered_to + pace.chunk).min(bound);
        let listing = Listing::Window {
            from: covered_to,
            to,
            filed_only: true,
        };
        let key = (true, covered_to.timestamp());
        let Some((stored, rest)) = self.archive_ingest_listing(listing, key, shutdown).await?
        else {
            return Ok(Some(ArchiveStep::Held));
        };
        self.store.set_archive_covered_to(self.account_id, to)?;
        self.metrics.note_archive_covered_to(to);
        if stored > 0 {
            eprintln!(
                "squelch: archive walk stored {stored} unlabelled message(s), forward to {}",
                to.format("%Y-%m-%d")
            );
        }
        crate::mem::trim_off_runtime().await;
        Self::archive_rest(rest, shutdown).await;
        Ok(Some(ArchiveStep::Walked))
    }

    /// List `listing` over all mail, drop the ids already stored and ingest the rest
    /// as archive rows, paced. `Some((new ids, the rest the caller owes after
    /// it has persisted its cursor))`, or `None` when the walk had to stop
    /// between batches (a window of mail started, or shutdown): nothing may be
    /// persisted then, and what was stored is skipped on the way back.
    ///
    /// TWO LIMITS, and the slower one wins. The rest after each batch (twice
    /// its own elapsed time) keeps the walk to a third of whatever pace the
    /// shared [`GmailBudget`] is allowing. [`ArchivePace::per_minute`] is the
    /// absolute one: however fast the budget would let it go, the walk fetches
    /// no more than that many messages in any minute.
    async fn archive_ingest_listing(
        &self,
        listing: Listing,
        chunk: (bool, i64),
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<Option<(usize, Duration)>> {
        let pace = self.archive_pace;
        let mut burst = tokio::time::Instant::now();
        let listed = self.list(listing, None, Urgency::Background).await?;
        // AFTER the listing, so a listing that fails is no pass at all: the
        // per-message failures below are of THIS chunk (another chunk's are
        // forgotten here), and each id counts at most once per pass.
        self.archive.state().begin_pass(chunk);
        let stored = self.store.stored_gmail_msg_ids(self.account_id, &listed)?;
        let ids: Vec<String> = listed
            .into_iter()
            .filter(|id| !stored.contains(id))
            .collect();

        let per_batch = pace.batch.max(1);
        let batches = ids.len().div_ceil(per_batch);
        for (i, batch) in ids.chunks(per_batch).enumerate() {
            // Between batches, never inside one: a catch-up that starts now
            // gets the budget back within one batch, and the rows this chunk
            // has already stored are skipped when the walk comes back to it.
            if let Some(gate) = self.archive_gate() {
                self.archive_hold(Some(gate));
                return Ok(None);
            }
            // THE DISK GUARDS, PER BATCH. One chunk is a week of mail and can
            // be thousands of bodies; asked only at its start, a heavy week
            // would be written straight through the floor or the ceiling.
            // Stopping here loses nothing: the cursor has not moved, and what
            // this chunk already stored is skipped when the walk comes back.
            if let Some(full) = self.archive_room() {
                self.archive_hold(Some(full));
                return Ok(None);
            }
            let wait =
                self.archive
                    .state()
                    .admit(batch.len(), pace.per_minute, pace.per_minute_window);
            if !wait.is_zero() {
                if !Self::archive_rest(wait, shutdown).await {
                    return Ok(None);
                }
                // The minute's wait is not work, and must not be rested for.
                burst = tokio::time::Instant::now();
            }
            {
                // Raised for the batch and no longer: the refine lane's vector
                // drain stays out while bodies are being parsed and embedded.
                let _in_flight = ArchiveBatchGuard::new(&self.archive);
                self.fetch_raw_and_ingest(
                    batch,
                    Mailbox::Inbox,
                    IngestOrigin::Archive,
                    Urgency::Background,
                )
                .await?;
            }
            // The last batch's rest comes after the cursor is safe.
            if i + 1 == batches {
                break;
            }
            if !Self::archive_rest(burst.elapsed() * pace.rest_factor, shutdown).await {
                return Ok(None);
            }
            burst = tokio::time::Instant::now();
        }
        Ok(Some((ids.len(), burst.elapsed() * pace.rest_factor)))
    }

    /// MAY THE WALK SKIP THIS MESSAGE and go on to the next. Always for a 404:
    /// the message was deleted between the listing and the fetch, and there is
    /// nothing to come back for.
    ///
    /// OTHERWISE ONLY A MESSAGE THAT HAS ITSELF FAILED BEFORE, on
    /// [`ArchivePace::poison_attempts`] separate passes over this chunk: its
    /// own fetch, or its own write to the store. Never on a first failure,
    /// whatever else has gone wrong with the chunk. Neither cursor ever looks
    /// back, so a skip is for good, and what earns one has to be evidence
    /// about that message: a transient Gmail error fails the pass, is booked
    /// against the one id it hit, and the whole chunk is tried again.
    ///
    /// NOT ABOUT THE MESSAGE, and so neither booked nor skipped: a credential
    /// error, and anything in a batch where Gmail has refused the walk for
    /// quota or auth. (A failed LISTING never reaches here at all.)
    ///
    /// AT MOST [`ArchivePace::poison_skips`] A PASS. A message Gmail will not
    /// serve is one or two; more than a handful failing over and over is an
    /// outage or a broken store, and the chunk stays where it is.
    ///
    /// COUNTED ONCE. A chunk that fails for some other reason is walked again,
    /// and a message already stepped over is stepped over again on every one
    /// of those passes; the counter is of messages, not of steps.
    fn archive_skip(&self, id: &str, error: &CoreError, quota_or_auth_before: u64) -> bool {
        if matches!(error, CoreError::NotFound) {
            if self.archive.state().count_skip(id) {
                self.metrics.count_archive_skipped();
            }
            return true;
        }
        if matches!(error, CoreError::Credential(_))
            || self.archive.quota_or_auth() != quota_or_auth_before
        {
            return false;
        }
        let earlier = self.archive.state().book_failure(id);
        if earlier < self.archive_pace.poison_attempts
            || self.archive.skipped.fetch_add(1, Ordering::Relaxed)
                >= self.archive_pace.poison_skips
        {
            return false;
        }
        if self.archive.state().count_skip(id) {
            self.metrics.count_archive_skipped();
        }
        true
    }

    /// Poll `history.list` every `poll_secs`, ingesting `messageAdded` INBOX
    /// messages and advancing the cursor. A poll batch IS the coalesced batch.
    ///
    /// This lane touches Gmail and the store and nothing else: no model call
    /// runs here, so nothing that thinks can delay the next fetch. The refine
    /// work it used to do inline is [`Self::refine_lane`], woken by
    /// `refine_wake` the moment this lane ingests anything.
    async fn poll_lane(&self, shutdown: &mut tokio::sync::watch::Receiver<bool>) -> Result<()> {
        let source = self.source()?;
        let interval = Duration::from_secs(self.config.sync.poll_secs);
        let idle_cap = Duration::from_secs(self.config.sync.idle_poll_secs);
        let mut idle_rounds = 0u32;
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            let moved = self.poll_tick().await?;
            // THE IDLE LADDER (off unless `sync.idle_poll_secs` says
            // otherwise): a tick that found the mailbox unchanged lengthens the
            // next wait, a tick that found a change, or a client with the app
            // open (an event stream connected), puts it back to the interval.
            let listening = self.event_listeners.load(Ordering::Relaxed) > 0;
            idle_rounds = if moved || listening {
                0
            } else {
                idle_rounds.saturating_add(1)
            };
            let wait = poll_wait(idle_rounds, interval, idle_cap);
            let cursor = self.last_cursor();

            // A refresh poke that arrives mid-poll is not lost: `Notify` stores
            // one permit, so the next `notified()` returns at once and the loop
            // runs one more immediate tick.
            //
            // THE SOURCE DOES THE WAITING: a plain sleep of `wait` for a
            // polling source (Gmail), an early return for one the server can
            // tell about a change (IMAP IDLE). Either way the next tick asks
            // the change feed and finds out.
            tokio::select! {
                () = source.wait_for_change(cursor.as_ref(), wait) => {}
                _ = self.refresh.notified() => {
                    eprintln!("squelch: manual refresh — polling now");
                    // Somebody asked: the next tick is at the fast interval and
                    // fetches the unread count whether or not the walk moves.
                    idle_rounds = 0;
                    self.unread_stale.store(true, Ordering::Relaxed);
                }
                // THE SPAM PAGE ASKED. Run the window sync right here and then
                // fall through to an ordinary tick, so one click costs one spam
                // fetch and nothing else changes about the loop's rhythm.
                //
                // Its failure is swallowed on purpose. This is a fetch of a
                // folder nothing depends on, requested by a page somebody is
                // looking at; letting it bounce the lifecycle would mean a
                // Gmail hiccup on the spam tab stops the user's actual mail from
                // syncing. The page learns from the stamp not moving.
                _ = self.spam_refresh.notified() => {
                    if let Err(e) = self.sync_spam_window().await {
                        eprintln!("squelch: spam sync failed ({e})");
                    }
                    // Somebody is looking at the spam page: the same signal
                    // as a refresh for the ladder, if not for the unread count.
                    idle_rounds = 0;
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() { return Ok(()); }
                }
            }
        }
    }

    /// The thinking half: Stage-1, Stage-2, the specialist extractors and the
    /// scheduled re-evaluations, over whatever is queued RIGHT NOW.
    ///
    /// Runs one full round and then parks until [`Self::fetch_raw_and_ingest`]
    /// pokes `refine_wake` (new mail is queued, start on it immediately) or
    /// `poll_secs` elapses (the floor, so a queue that filled up some other way
    /// — a re-triage, a revisit falling due, a row an earlier round left behind
    /// — still drains without new mail having to arrive to trigger it).
    ///
    /// No `Result`: every failure inside these passes is already handled and
    /// logged where it happens, and a lane that could `Err` would be a lane that
    /// could bounce the Gmail lifecycle over a model outage.
    ///
    /// IT SPENDS NOTHING UNTIL A POLL HAS SUCCEEDED. See `poll_healthy` for the
    /// whole argument; the short version is that this lane is cancelled whenever
    /// the poll lane errors, a cancelled classify is charged and never refunded,
    /// and `run()` retries the pair every five minutes for as long as the
    /// credential stays broken.
    async fn refine_lane(&self, shutdown: &mut tokio::sync::watch::Receiver<bool>) {
        let interval = Duration::from_secs(self.config.triage.agent.worker_poll_secs);
        let mut idle_rounds = 0u32;
        // Whatever ends this lane (shutdown, or `run_lanes` dropping it when
        // the poll lane errors), a door must stop waiting on it.
        struct NotRunnable<'a>(&'a crate::store::archive_surface::RefinePoke);
        impl Drop for NotRunnable<'_> {
            fn drop(&mut self) {
                self.0.set_runnable(false);
            }
        }
        let _withdrawn = NotRunnable(&self.refine_poke);
        loop {
            if *shutdown.borrow() {
                return;
            }
            // TELL THE DOORS WHETHER A POKE WOULD BUY ANYTHING, once a round
            // and from the same three facts that gate the spending below: a
            // poll has succeeded, a model is configured, the provider circuit
            // is closed. An agent search waits on its access checks only
            // while this is true.
            self.refine_poke.set_runnable(self.refine_can_spend());
            // TRANSLATION RUNS BESIDE THIS LANE, never in it
            // (docs/TRANSLATION.md, Pipeline): one long message must not hold
            // up triage of everything that arrives after it. Triage instead
            // holds back the one message whose translation is pending, for a
            // bounded wait (`TRIAGE_WAIT_SECS` in the store), so foreign mail
            // is normally triaged once, on the English.
            self.translate_wake.notify_one();
            // THE RELATIONSHIP LEDGER, ahead of the triage pass so a new
            // sender's row exists before its first job is claimed, and outside
            // the gate below because it spends no model call
            // (`relationships`).
            self.relationship_round().await;
            if self.poll_healthy.load(Ordering::Relaxed) {
                if self.agent_triage_pass().await {
                    idle_rounds = 0;
                } else {
                    idle_rounds = idle_rounds.saturating_add(1).min(5);
                }
                // The nightly owner profile (docs/UNDERSTANDING.md §2.3):
                // a no-op every round but the first one after the owner's
                // 03:00, and gated on its feature and its own daily cap.
                self.profile_pass().await;

                // Per-round, so an embedder attached after startup catches up on
                // rows ingested before it was ready, no restart needed.
                //
                // INSIDE THE GATE with the rest, even though it costs no model
                // call: a round that skipped Stage-1 has nothing new to embed,
                // and the next healthy round picks up whatever is missing. One
                // shape for the whole round is easier to reason about than a
                // half-round.
                //
                // NOT WHILE A WINDOW IS BEING FETCHED. This lane runs beside the
                // first backfill now (see `run_once`), and a drain in the middle
                // of one is wrong both ways: with an embedder, every row the
                // sealed gate passes at ingest (the owner's own mail; inbound
                // mail waits for its decision) is embedded a moment AFTER its
                // commit, so the drain's query keeps finding the row that is
                // mid-embed and embeds it a second time, one ONNX pass per message thrown away on the CPU
                // the backfill needs; without one (the gate released at its
                // ceiling), the batch drain's own peak (`EmbedConfig::
                // backfill_batch`) would land on top of the backfill's instead
                // of after it, which is the exact watermark `crate::mem` is
                // built to avoid. The first round after the guard drops drains
                // whatever the window left behind, beside the first poll.
                //
                // NOR WHILE AN ARCHIVE BATCH IS, for both of the same reasons:
                // the walk embeds each row a moment after its commit, and a
                // walk with no embedder leaves rows whose batch drain must not
                // land on top of the bodies it is parsing. The walk rests
                // between batches for twice as long as it works, and the drain
                // has that.
                //
                // AND ONLY WHEN SOMETHING MAY BE MISSING: the drain's query
                // walks the whole account when nothing is, which in steady
                // state is every round. See `vectors_dirty`.
                if self.may_drain_vectors() && self.vectors_owed() {
                    self.backfill_missing_vectors().await;
                }
            }

            // A poke that arrives mid-round is not lost, for the same reason a
            // mid-poll `refresh` is not: `Notify` holds one permit, so the next
            // `notified()` returns at once. It also COALESCES — twenty pokes
            // during one long round are one extra round, not twenty.
            tokio::select! {
                _ = self.refine_wake.notified() => { idle_rounds = 0; }
                _ = self.refine_poke.poked() => { idle_rounds = 0; }
                _ = tokio::time::sleep(self.refine_idle_sleep(interval, idle_rounds)) => {}
                _ = shutdown.changed() => {
                    if *shutdown.borrow() { return; }
                }
            }
        }
    }

    /// How long the refine lane sleeps between rounds: the idle backoff, cut
    /// to the moment a provider cooldown lapses when that is sooner, so the
    /// recovery probe runs when the cooldown says (seven seconds after a
    /// `retry-after: 7`) and not at the next idle tick, up to thirty
    /// seconds later. A round that found the cooldown open counts as idle,
    /// which is what would otherwise stretch the sleep.
    fn refine_idle_sleep(&self, interval: Duration, idle_rounds: u32) -> Duration {
        let backoff = Duration::from_secs(triage_worker::doubling_backoff_secs(
            interval.as_secs(),
            idle_rounds,
            MAX_LANE_IDLE_BACKOFF.as_secs(),
        ));
        let until = self.agent_retry_after.load(Ordering::Relaxed) - Utc::now().timestamp();
        if until > 0 {
            // A second past the stamp, which is whole seconds: a wake just
            // short of it would find the cooldown still open.
            backoff.min(Duration::from_secs(until as u64 + 1))
        } else {
            backoff
        }
    }

    /// Whether a refine round started now would run an access check: the gates
    /// of [`Self::refine_lane`] and [`Self::agent_triage_pass`], read without
    /// taking any of them.
    fn refine_can_spend(&self) -> bool {
        self.poll_healthy.load(Ordering::Relaxed)
            && self.stage2_llm.is_some()
            && Utc::now().timestamp() >= self.agent_retry_after.load(Ordering::Relaxed)
    }

    /// FETCH THE PROVIDER'S SPAM FOLDER, ONCE, ON REQUEST.
    ///
    /// The poll loop does not walk the SPAM label — not on backfill, not on a
    /// history tick, not on a catch-up. Spam is the largest thing in a mailbox
    /// and the least read, and every message is a body fetch, a parse and a
    /// sanitize; tracking it continuously would spend most of this daemon's
    /// budget keeping a page current that nobody has open. So the fetch happens
    /// when the page is opened, and the click pays for it.
    ///
    /// CAPPED at `sync.spam_max`, newest first. The cap is a bound on how long
    /// that click can take rather than a claim about which messages are in the
    /// window — see [`MailSource::list`] (Gmail's cap note is in
    /// `mail::gmail`).
    ///
    /// Rows land exactly as the label walk used to leave them: `is_spam = 1`,
    /// neutral tier, `'n/a'` stage markers, no embedding, no notification. The
    /// ingest branch owns all of that, so on-demand and routine fetches cannot
    /// disagree about what a spam row is.
    ///
    /// The completion stamp is written LAST and only on success, which is what
    /// makes it readable as "we looked": a half-finished sync leaves the old
    /// stamp, and the page keeps saying it is still checking.
    pub async fn sync_spam_window(&self) -> Result<usize> {
        let cap = self.config.sync.spam_max as usize;
        let ids = self
            .list(self.window_listing(Folder::Spam), Some(cap), Urgency::Live)
            .await?;
        eprintln!("squelch: spam sync — {} message(s) to fetch", ids.len());
        // `Backfill`, not `Incremental`: this is a bulk read of old mail the
        // user asked to look at. `worthy_kind` already refuses a spam row, so
        // the origin is belt to that brace rather than the guarantee.
        let n = self
            .fetch_raw_and_ingest(&ids, Mailbox::Spam, IngestOrigin::Backfill, Urgency::Live)
            .await?;
        self.store.set_app_setting(
            self.account_id,
            SPAM_SYNCED_AT_KEY,
            &Utc::now().to_rfc3339(),
        )?;
        eprintln!("squelch: spam sync complete — {n} message(s)");
        Ok(n)
    }

    /// ONE TICK OF THE POLL LANE, everything between two sleeps: the daily
    /// profile, the reminder sweep, the past-event re-check, the walk, the
    /// stamps a successful walk leaves, and the unread count when it is owed.
    /// Returns whether the walk found the mailbox changed, which is what the
    /// lane's idle ladder and the unread fetch both key on. Its own function
    /// so a test can run a tick without the lane's clock.
    async fn poll_tick(&self) -> Result<bool> {
        // The account's Google name and photo. Due once a day, so nearly
        // every tick this is a single atomic load; it sits before the walk
        // because a mailbox whose walk keeps failing should still have a face.
        //
        // GOOGLE ACCOUNTS ONLY, and exhaustively so: the fetch carries the
        // account's Read token to Google's People API, and another provider's
        // token must never be handed to Google. A new provider has to decide
        // here, in a match the compiler makes it write. No source, no
        // profile, and no `?`: nothing before the walk may fail the tick
        // (the reminder sweep below depends on that).
        if let Ok(source) = self.source() {
            match source.provider() {
                MailProvider::Gmail => self.refresh_profile().await,
                // No Google profile for a Microsoft mailbox, and no Graph
                // photo fetch yet: the client shows the initial.
                MailProvider::Outlook => {}
                // No Google profile behind an IMAP account.
                MailProvider::Imap => {}
            }
        }
        // BEFORE the walk, for the same reason: the sweep is purely local —
        // one indexed UPDATE, no Gmail, no model spend — so hanging it off a
        // call that can Err would let a bad credential or a Gmail outage
        // starve it for as long as the outage lasts.
        self.reminder_pass();
        // Local too, and for the same reason.
        self.event_revisit_pass();
        // THE UNREAD COUNT WHEN IT IS OWED (see `unread_stale`), BEFORE the
        // walk as every tick's fetch used to be: the walk is what can return
        // Err and bounce the whole lifecycle, and a mailbox stuck in that
        // retry loop still gets the count it is owed.
        let owed = self.unread_stale.load(Ordering::Relaxed);
        if owed {
            self.refresh_inbox_unread().await;
        }
        let moved = self.poll_once().await?;
        // AND AFTER A WALK THAT FOUND THE MAILBOX CHANGED, which is the only
        // time an unread count can have moved: an idle mailbox is not asked,
        // and a tick that already fetched above is not asked twice (Gmail's
        // count already reflected the mail the walk went on to find). Before
        // the stamps below, as the fetch always was: it is cosmetic, and its
        // failure must not be the "last error" a poll that just succeeded
        // reports.
        if moved && !owed {
            self.refresh_inbox_unread().await;
        }
        // GMAIL ANSWERED, so the refine lane may spend again. Set AFTER the
        // walk and before anything else, because this is the only proof in
        // the process that the credential works; `fetch_raw_and_ingest` has
        // already poked `refine_wake` from inside that call if it ingested
        // anything, and `Notify` holds the permit, so the lane wakes on the
        // poke and reads a flag that is true by the time it does. See the
        // field's own doc for what this is protecting.
        self.poll_healthy.store(true, Ordering::Relaxed);
        self.polled_ok.store(true, Ordering::Relaxed);
        // WHEN, for the archive walk's forward pass: every arrival up to
        // this instant has been through the live path, so mail older than
        // this (less a day) that the store does not hold was never going
        // to be. See [`Self::archive_forward_chunk`].
        self.archive
            .last_poll_ok_unix
            .store(Utc::now().timestamp(), Ordering::Relaxed);
        // THE freshness stamp for a healthy daemon: `run()` only records one
        // on its way out, and a mailbox that polls happily for a month never
        // goes there — staleness alerts would fire on the boot timestamp.
        //
        // IT STAYS IN THIS LANE, deliberately. Sync freshness measures
        // POLLING, not refinement: that is what the staleness alert was
        // always meant to mean, and stamping it from the refine lane would
        // let a wedged Gmail credential keep looking healthy for as long as
        // there were queued rows left to classify.
        //
        // `note_poll_ok` also ends the failure streak and clears the last
        // error, so the client stops reporting an outage that is over.
        self.metrics.note_poll_ok();
        Ok(moved)
    }

    /// A single poll: consult the cursor, either run the incremental history
    /// walk or (on absent/expired cursor) a fresh catch-up. `Ok(false)` is a
    /// walk that found the mailbox exactly where it left it; a catch-up
    /// always counts as a change, since it re-establishes the cursor.
    async fn poll_once(&self) -> Result<bool> {
        let cursor = self.load_cursor()?;
        let source = self.source()?;
        match cursor.filter(|c| source.cursor_is_usable(c)) {
            Some(start) => {
                match self.history_walk(&start).await {
                    Ok(moved) => Ok(moved),
                    // Expired historyId (404): fall back to a fresh catch-up.
                    Err(CoreError::NotFound) => {
                        eprintln!("squelch: historyId expired; falling back to catch-up");
                        self.catch_up().await?;
                        Ok(true)
                    }
                    Err(e) => Err(e),
                }
            }
            None => {
                self.catch_up().await?;
                Ok(true)
            }
        }
    }

    /// The cursor to store at the end of a window, minted NOW, before the
    /// window is listed, for a source whose cursor is a point in time
    /// ([`MailSource::cursor_before_window`]); `None` for one that is asked
    /// where its feed stands after the window, as Gmail always has been.
    async fn cursor_ahead_of_window(&self) -> Result<Option<SyncCursor>> {
        let source = self.source()?;
        if !source.cursor_before_window() {
            return Ok(None);
        }
        Ok(Some(source.current_cursor().await?))
    }

    /// The ids of an incremental walk worth fetching. All of them for a
    /// source whose change feed names only arrivals (Gmail); for one that also
    /// names CHANGED mail (Graph's delta, on every read-state flip), only the
    /// ids not already stored, so a poll on an account somebody reads in
    /// another client does not re-download every message they opened. The
    /// incremental path only: see [`MailSource::changes_report_updates`].
    fn unstored_changes(&self, ids: &[String]) -> Result<Vec<String>> {
        if ids.is_empty() || !self.source()?.changes_report_updates() {
            return Ok(ids.to_vec());
        }
        let mut fresh = Vec::with_capacity(ids.len());
        for id in ids {
            if !self.store.has_message(self.account_id, id)? {
                fresh.push(id.clone());
            }
        }
        Ok(fresh)
    }

    /// One incremental poll: THREE label-filtered `history.list` walks from the
    /// SAME start cursor, INBOX then SENT then SPAM, with the cursor committed
    /// ONCE at the end at the max historyId any walk observed.
    ///
    /// The SENT walk is what makes mail the user writes from Gmail web or their
    /// phone land here at all: such a message never carries INBOX, so an
    /// INBOX-only walk is blind to it, and the api's post-send echo covers only
    /// what was sent through the daemon itself. The SPAM walk is blind to the
    /// other two for the same structural reason, which is why spam was invisible
    /// in this client until it got a walk of its own. All three start from the
    /// same `start_history_id` because one Gmail cursor spans the whole mailbox.
    ///
    /// ORDER IS LOAD-BEARING. A self-addressed message carries both INBOX and
    /// SENT and so appears in two walks; the store's upsert keeps the MINIMUM of
    /// each visibility flag, but the walk order is the first defense and does not
    /// depend on that clause holding. INBOX therefore runs FIRST and its ids are
    /// subtracted from both later batches, making the visible copy authoritative.
    ///
    /// AN IDLE MAILBOX COSTS ONE REQUEST. Every `history.list` answer carries
    /// the mailbox's CURRENT historyId whatever label it was filtered by, so
    /// when the INBOX walk returns no record and that id is exactly the
    /// cursor, nothing in the mailbox has changed since the last poll: no
    /// sent mail, no spam, no read state. The SENT walk would answer the
    /// same and the cursor write would store the value already stored, so
    /// both are skipped and the walk reports `false`. Anything else (a record
    /// in either walk, or an id past the cursor with no record, which is
    /// mail outside INBOX or a change of read state) runs the SENT walk as
    /// before and reports `true`.
    ///
    /// Propagates [`CoreError::NotFound`] on an expired historyId so the caller
    /// can fall back to a catch-up.
    async fn history_walk(&self, start: &SyncCursor) -> Result<bool> {
        let walk = self.walk_changes(start).await?;
        if let Some(cursor) = &walk.advance {
            self.store_cursor(cursor)?;
        }
        Ok(walk.moved)
    }

    /// [`Self::history_walk`]'s walk, without storing the cursor: what it
    /// found, and where the cursor may move (`None`: hold it where it is).
    /// The first run's live poll keeps its own cursor until the backfill is
    /// done ([`Self::first_run`]), so the store must not see it early.
    async fn walk_changes(&self, start: &SyncCursor) -> Result<Walked> {
        let hold = |moved| Walked {
            moved,
            advance: None,
        };
        let inbox = self.changes(start, Folder::Inbox).await?;
        if inbox.ids.is_empty() && inbox.mailbox_unchanged {
            return Ok(hold(false));
        }
        let (inbox_ids, inbox_cursor, inbox_saw) = (inbox.ids, inbox.cursor, inbox.saw_changes);
        let inbox_new = self.unstored_changes(&inbox_ids)?;
        if !inbox_new.is_empty() {
            let n = self
                .fetch_raw_and_ingest(
                    &inbox_new,
                    Mailbox::Inbox,
                    IngestOrigin::Incremental,
                    Urgency::Live,
                )
                .await?;
            eprintln!("squelch: ingested {n} new INBOX messages");
        }

        // The SENT half is best-effort AGAINST THE CURSOR: any failure holds the
        // cursor where it is and returns, so the next poll re-walks both labels
        // from the same start rather than stepping over INBOX history that the
        // successful half already covered. Re-walking costs a few fetches and
        // changes nothing: `UNIQUE(account_id, gmail_msg_id)` collapses the
        // message rows and `UNIQUE(message_id)` on `events` collapses the
        // notifications.
        //
        // THE FAST LANE WOULD HAVE MADE THAT FALSE — a re-walked message still
        // inside `freshness_window_secs` is a fresh `notify_lane::candidate`,
        // and a second run is a second PAID model call and a second daily-cap
        // unit, neither of which `UNIQUE` collapses. It re-reads the ledger
        // first for exactly this path (`Store::notify_decision_exists`), so a
        // re-walk costs one indexed probe per message and still changes nothing.
        let (sent_ids, sent_cursor, sent_saw) = match self.changes(start, Folder::Sent).await {
            Ok(walk) => (walk.ids, walk.cursor, walk.saw_changes),
            // An expired historyId is not a SENT-specific failure: the cursor
            // itself is gone and only a catch-up can recover it.
            Err(CoreError::NotFound) => return Err(CoreError::NotFound),
            Err(e) => {
                eprintln!("squelch: sent history walk failed ({e}); holding the cursor");
                return Ok(hold(true));
            }
        };

        // A source whose feed names every change (Graph's delta) proves an
        // idle mailbox the other way round from Gmail: both rounds empty.
        let idle = !inbox_saw && !sent_saw && self.source()?.changes_report_updates();
        let sent_only = self.unstored_changes(&subtract_ids(sent_ids, &inbox_ids))?;
        if !sent_only.is_empty() {
            // `Incremental`, like the INBOX half: the silence guarantee for the
            // user's own mail is NOT the origin. It is, in order,
            // `notify_eligible_stamp` refusing to stamp an `is_sent` row at all
            // (docs/NOTIFY.md §11.3), so there is nothing for any site to emit
            // from; `events::worthy_kind` refusing one with
            // `Err(Refusal::NotWorthy)` as defense in depth; and the 'n/a' stage
            // markers ingest stamps, which keep it out of both LLM queues.
            match self
                .fetch_raw_and_ingest(
                    &sent_only,
                    Mailbox::Sent,
                    IngestOrigin::Incremental,
                    Urgency::Live,
                )
                .await
            {
                Ok(n) => eprintln!("squelch: ingested {n} new SENT messages"),
                Err(e) => {
                    eprintln!("squelch: sent ingest failed ({e}); holding the cursor");
                    return Ok(hold(true));
                }
            }
        }

        // AND NO SPAM WALK. A poll tick runs every few seconds; the spam folder
        // is the largest thing in a mailbox and the least looked at, so walking
        // it here would spend most of this daemon's fetch budget on mail nobody
        // asked to see. It is fetched on demand instead — see `sync_spam_window`.

        // Both walks started from `start`; the merge is the one commit
        // covering both.
        Ok(Walked {
            moved: !idle,
            advance: Some(
                self.source()?
                    .merge_cursors(start, &inbox_cursor, &sent_cursor),
            ),
        })
    }

    /// Fresh catch-up: re-run the backfill-window INBOX fetch, then the SENT
    /// fetch over the same window (dedup makes both idempotent), and
    /// re-establish the historyId. Used on first run's poll and on an
    /// expired-history 404 — exactly the cases where the history walk cannot
    /// account for the gap, so the sent half has to be re-listed too or mail the
    /// user wrote from another client during it is lost for good.
    ///
    /// Trimmed on the way out either way, for the reason [`Self::backfill`]
    /// gives: a catch-up that dies mid-window has already paid the peak.
    async fn catch_up(&self) -> Result<()> {
        // THE PROGRESS PAIR IS SET UP FRONT AND CLEARED ON EVERY EXIT, including
        // the `?` paths in the body below, which is what `_guard` is for. A
        // catch-up is this loop's longest single call by orders of magnitude and
        // it used to emit nothing at all while it ran: no log, no counter, no
        // freshness stamp, because `poll_once` had not returned yet. Worse, most
        // of its work is upserting mail already stored, so the message count and
        // the database size sit still as well. On 2026-08-26 a tenant's mailbox
        // spent half an hour in here and the only honest thing anybody could say
        // from outside was that it was indistinguishable from wedged.
        //
        // It is held ACROSS the trim on purpose: handing the heap back is part
        // of the catch-up as seen from outside, so progress clears when the
        // whole thing is done rather than when the last fetch returns.
        let _guard = CatchUpGuard::new(&self.metrics, &self.window_in_flight);
        let out = self.catch_up_inner().await;
        crate::mem::trim_off_runtime().await;
        out
    }

    /// The catch-up body, split out so [`Self::catch_up`] can hold the progress
    /// guard and run the trim across every exit path, `?` included.
    async fn catch_up_inner(&self) -> Result<()> {
        let early_cursor = self.cursor_ahead_of_window().await?;
        let ids = self
            .list(self.window_listing(Folder::Inbox), None, Urgency::Window)
            .await?;
        // BEFORE the first fetch, so the size of the job is known while it is
        // still a job rather than after it is a result. This is the line that
        // turns "silent for 30 minutes" into "re-fetching 4,500 messages".
        eprintln!(
            "squelch: catch-up re-fetching {} INBOX message(s) from the last {} days; {}",
            ids.len(),
            self.config.sync.backfill_days,
            if self.poll_healthy.load(Ordering::Relaxed) {
                "triage runs alongside it"
            } else {
                "triage waits for a successful sync"
            }
        );
        self.metrics.catchup_begin(ids.len() as u64);
        // A catch-up may carry genuinely new mail, so it is allowed to notify.
        // What keeps the whole-window re-scan from storming is the FIRST-SIGHT
        // STAMP (`triage.notify_eligible_at`, see [`notify_eligible_stamp`])
        // plus the one-event-per-message key, and the two halves of the window
        // are bounded by DIFFERENT parts of that rule:
        //
        // - Mail this re-scan has seen before ALREADY EXISTS, so
        //   `ingest_message` preserves whatever stamp it was given the first
        //   time — NULL for the backfill's rows, an hour-old timestamp for
        //   anything the last catch-up already saw — and the rescue ceiling or
        //   the NULL refuses it at every emission site.
        // - Mail that arrived while the historyId was dead has never been seen
        //   here and does NOT already exist, so preservation cannot be what
        //   bounds it. `events::is_fresh`, asked ONCE at first sight, is: a
        //   catch-up after a week-long outage stamps only the mail whose `Date:`
        //   is still inside `notify.freshness_window_secs` when we finally lay
        //   eyes on it, and everything older is stamped NULL and silent forever.
        //
        // It is no longer the freshness window doing the first job, which is
        // exactly the fix: asked at emission time it measured the SENDER's clock
        // against the wall clock, so a row that waited behind a queue of model
        // calls aged out and went silent.
        let n = self
            .fetch_raw_and_ingest(
                &ids,
                Mailbox::Inbox,
                IngestOrigin::Incremental,
                Urgency::Window,
            )
            .await?;
        if n > 0 {
            eprintln!("squelch: catch-up ingested {n} INBOX messages");
        }

        // INBOX first, and its ids subtracted, for the same reason the history
        // walk orders itself that way: a self-addressed message is in both
        // listings and its visible copy must win the unique-key race.
        let sent_ids = self
            .list(self.window_listing(Folder::Sent), None, Urgency::Window)
            .await?;
        let sent_only = subtract_ids(sent_ids, &ids);
        // EXTENDS the same run rather than starting a second one: from outside
        // this is one wait, and a progress bar that reached the end and then
        // restarted at zero would read as a loop rather than as two phases.
        self.metrics
            .catchup_begin_extend(ids.len() as u64 + sent_only.len() as u64);
        eprintln!(
            "squelch: catch-up re-fetching {} SENT message(s)",
            sent_only.len()
        );
        let sent_n = self
            .fetch_raw_and_ingest(
                &sent_only,
                Mailbox::Sent,
                IngestOrigin::Incremental,
                Urgency::Window,
            )
            .await?;
        if sent_n > 0 {
            eprintln!("squelch: catch-up ingested {sent_n} SENT messages");
        }

        // No SPAM leg, same as the backfill and the walk: a catch-up is already
        // the longest thing this loop does, and the spam folder is fetched only
        // when the page that shows it is opened.

        let cursor = match early_cursor {
            Some(cursor) => cursor,
            None => self.source()?.current_cursor().await?,
        };
        self.store_cursor(&cursor)?;
        Ok(())
    }

    // ---- Sent-contacts harvest --------------------------------------------

    /// ONE-TIME sweep of the ENTIRE Sent mailbox — no window — seeding the
    /// contacts table from every To/Cc the account has ever written to.
    /// Only To/Cc are fetched ([`MailSource::fetch_headers`]): headers, never
    /// bodies, so deep history costs one light call per sent message. Merged with MAX
    /// semantics ([`Store::merge_harvested_contacts`]) so overlap with the
    /// ingest path's own seeding and an interrupted re-run cannot double-count.
    ///
    /// Completion is a `sync_state` flag (`mailbox = 'sent_contacts'`); an
    /// interrupted pass leaves it unset and the next daemon start redoes the
    /// sweep from scratch — idempotent, just re-paged.
    pub async fn harvest_sent_contacts(&self) -> Result<()> {
        let done = self
            .store
            .sync_state(self.account_id, SENT_CONTACTS_KEY)?
            .map(|s| s.last_uid >= 1)
            .unwrap_or(false);
        if done {
            return Ok(());
        }

        let source = self.source()?;
        let ids = self
            .list(
                Listing::Folder {
                    folder: Folder::Sent,
                    within_days: None,
                },
                None,
                Urgency::Live,
            )
            .await?;
        eprintln!(
            "squelch: sent-contacts harvest scanning {} sent messages (headers only)",
            ids.len()
        );

        let self_addr = self.account_email.trim().to_ascii_lowercase();
        // addr -> aggregate. Counting per message occurrence matches the ingest
        // path's per-send bump closely enough for ranking.
        let mut agg: std::collections::HashMap<String, ContactEntry> =
            std::collections::HashMap::new();
        for (i, id) in ids.iter().enumerate() {
            let msg = source.fetch_headers(id, &["To", "Cc"]).await?;
            let sent_at = msg.received_at;
            // Through mail-parser via header synthesis, so grouped lists, quoted
            // display names and RFC2047 encoding parse exactly as ingest does.
            let blob = synthesize_rfc822_headers(&msg.headers);
            let Some(parsed) = mail_parser::MessageParser::default().parse(blob.as_bytes()) else {
                continue;
            };
            for list in [parsed.to(), parsed.cc()].into_iter().flatten() {
                for mailbox in list.iter() {
                    let Some(addr) = mailbox.address() else {
                        continue;
                    };
                    let addr = addr.trim().to_ascii_lowercase();
                    // Same gate as ingest seeding: never the account itself,
                    // never robot/unsubscribe traffic.
                    if addr.is_empty() || addr == self_addr || is_robot_address(&addr) {
                        continue;
                    }
                    let name = mailbox
                        .name()
                        .map(|n| n.trim().to_string())
                        .filter(|n| !n.is_empty());
                    let entry = agg.entry(addr.clone()).or_insert_with(|| ContactEntry {
                        addr,
                        display_name: None,
                        sent_count: 0,
                        last_sent_at: None,
                    });
                    entry.sent_count += 1;
                    if entry.display_name.is_none() {
                        entry.display_name = name;
                    }
                    if sent_at > entry.last_sent_at {
                        entry.last_sent_at = sent_at;
                    }
                }
            }
            if (i + 1) % 1000 == 0 {
                eprintln!(
                    "squelch: sent-contacts harvest {}/{} messages scanned",
                    i + 1,
                    ids.len()
                );
            }
        }

        let batch: Vec<ContactEntry> = agg.into_values().collect();
        self.store
            .merge_harvested_contacts(self.account_id, &batch)?;
        self.store.set_sync_state(
            self.account_id,
            SENT_CONTACTS_KEY,
            &SyncState {
                uidvalidity: 1,
                last_uid: 1,
            },
        )?;
        eprintln!(
            "squelch: sent-contacts harvest complete — {} unique recipients",
            batch.len()
        );
        Ok(())
    }

    // ---- Blank-body heal ----------------------------------------------------

    /// ONE-TIME sweep over mail stored with a body that is blank to a reader
    /// beside a full HTML one — every message ingested before the body selection
    /// learned to flatten the HTML alternative when the text/plain part is empty
    /// (senders that build both alternatives from a template routinely ship an
    /// empty text part). Each such message is re-fetched `format=raw` and put
    /// back through the REAL ingest path, so the body it gets is exactly the one
    /// a fresh arrival would, and written with [`Store::ingest_message_fresh`].
    ///
    /// THE ROW IS RE-INVESTIGATED ONLY WHERE THE CUTOVER WOULD HAVE LOOKED. A
    /// row inside the history window (`sync.backfill_days`, the same window
    /// the agent cutover re-triaged) gets its text and a fresh background
    /// investigation at its new content revision — the background lane and
    /// budget, behind live arrivals, never ahead of them. Older mail gets its
    /// text (search, the agent door, the vector backfill) and an access
    /// reassessment only: its decision stays, because a placement minted for
    /// a years-old message is spend nobody asked for. The recent tail is
    /// exactly where a blank-bodied bill was judged on its subject line
    /// ("Your Garmin Services Bill" read as an invoice with no date and sat in
    /// For-your-eyes while the HTML said "no action is required, your payment
    /// will be processed"), which is why those rows are worth the calls.
    ///
    /// THE AGENT DOOR STAYS SHUT UNTIL THE NEW TEXT IS ASSESSED. Sensitivity
    /// is the model's call, not ingest's, so a re-read whose HTML turns out to
    /// carry a login code is written like any other — the human reader gets
    /// it, as they would a fresh arrival — while the content-revision bump
    /// puts the row's external access back to `pending`, and it stays there
    /// until the queued reassessment has read the real words.
    ///
    /// NO NOTIFICATION CAN COME OF THIS: the engine's eligibility stamp is
    /// left unset (a backfill is not an arrival), the store preserves whatever
    /// stamp the row already carries, and the fast lane is never spawned here.
    ///
    /// PROGRESS IS A CURSOR, NOT A FLAG. The scan walks newest-first in chunks
    /// (the store lock is held per chunk) and the cursor is written after each
    /// chunk, so a start that stops early — the per-start fetch ceiling, a
    /// Gmail error — resumes where it stopped instead of re-reading the
    /// mailbox. A message Gmail no longer has, or whose HTML also flattens to
    /// nothing, is passed over and counted; it does not hold the sweep open.
    /// A transport or API error stops the sweep for this start (the error is
    /// reported by kind, and grinding on through a quota refusal is how a
    /// tenant deepens it); the chunk is re-read next start, and rows already
    /// healed are no longer candidates. Best-effort and non-fatal, like the
    /// sweeps it sits beside.
    pub async fn heal_blank_bodies(&self) -> Result<()> {
        let cursor = self
            .store
            .sync_state(self.account_id, BLANK_BODY_HEAL_KEY)?;
        if cursor.as_ref().is_some_and(|c| c.uidvalidity >= 1) {
            return Ok(());
        }
        let mut before_id: i64 = cursor
            .map(|c| i64::try_from(c.last_uid).unwrap_or(i64::MAX))
            .unwrap_or(i64::MAX);
        eprintln!(
            "squelch: blank-body heal starting (at most {BLANK_BODY_HEAL_MAX_FETCHES} raw fetches \
             this start; rows inside the history window are re-investigated)"
        );
        let started = std::time::Instant::now();
        let source = self.source()?;
        let rules = self.store.list_sender_rules(self.account_id)?;
        // The cutover's own window: what it re-triaged is what a re-read is
        // worth re-investigating.
        let history_cutoff = self.backfill_since();

        let mut fetched = 0usize;
        let mut re_read = 0usize;
        let mut text_only = 0usize;
        let mut still_blank = 0usize;
        let mut refused = 0usize;
        let mut gone = 0usize;
        let mut done = false;
        let mut stopped: Option<String> = None;
        'chunks: loop {
            let scan = self.store.blank_body_messages(
                self.account_id,
                before_id,
                BLANK_BODY_SCAN_CHUNK,
            )?;
            for row in &scan.candidates {
                if fetched >= BLANK_BODY_HEAL_MAX_FETCHES {
                    // The cursor stays above this row: it is re-read next start.
                    before_id = row.message_id + 1;
                    break 'chunks;
                }
                fetched += 1;
                let msg = match source.fetch_raw(&row.gmail_msg_id, Urgency::Live).await {
                    Ok(msg) => msg,
                    // Gone upstream: nothing to heal from, and nothing to retry.
                    Err(CoreError::NotFound) => {
                        gone += 1;
                        continue;
                    }
                    Err(e) => {
                        before_id = row.message_id + 1;
                        stopped = Some(e.to_string());
                        break 'chunks;
                    }
                };
                let raw = match msg.raw {
                    RawBody::Bytes(bytes) => bytes,
                    // Undecodable is deterministic: retrying it every start
                    // buys a raw GET for the same answer. Id only, never content.
                    RawBody::Undecodable(_) => {
                        eprintln!(
                            "squelch: blank-body heal skipping message {} (decode error)",
                            row.message_id
                        );
                        gone += 1;
                        continue;
                    }
                    RawBody::Missing => {
                        gone += 1;
                        continue;
                    }
                };
                let fetched_msg = RawFetched {
                    account_id: self.account_id,
                    gmail_msg_id: row.gmail_msg_id.clone(),
                    gmail_thread_id: msg.thread_id.clone(),
                    raw,
                    internal_date: msg.received_at,
                    // The candidate query is inbound, non-spam mail; the store
                    // re-checks the live row before writing anything.
                    is_sent: false,
                    is_spam: false,
                    account_addr: self.account_email.clone(),
                };
                // `notify_eligible_at` stays None: see the doc comment.
                let triaged = ingest_with_rules(
                    &fetched_msg,
                    &self.config.stage1,
                    Utc::now(),
                    &rules,
                    |addr| {
                        self.store
                            .is_known_contact(self.account_id, addr)
                            .unwrap_or(false)
                    },
                );
                // Judged by the same function ingest just used to pick the
                // body: if the HTML flattened to nothing either, the row is
                // what it was, and writing it would only buy a model call over
                // an empty page.
                if crate::triage::text::is_blank(&triaged.message.body) {
                    still_blank += 1;
                    continue;
                }
                let scope = if row.received_at >= history_cutoff {
                    crate::store::HealScope::TextAndTriage
                } else {
                    crate::store::HealScope::Text
                };
                match self.store.ingest_message_fresh(&triaged, scope)? {
                    Some(_) => {
                        // The heal DELETES the row's vector (it was computed
                        // from the subject alone) for the drain to redo from
                        // the real text, so the drain must know there is one
                        // to redo. This engine is the daemon's SWEEPS engine;
                        // the flag it raises is the sync engine's, shared.
                        self.vectors_dirty.store(true, Ordering::Relaxed);
                        if scope == crate::store::HealScope::TextAndTriage {
                            re_read += 1;
                        } else {
                            text_only += 1;
                        }
                    }
                    None => refused += 1,
                }
                if fetched.is_multiple_of(100) {
                    eprintln!(
                        "squelch: blank-body heal {fetched} fetched, {re_read} re-read, \
                         {text_only} text-only, {}s",
                        started.elapsed().as_secs()
                    );
                }
            }
            match scan.next_before_id {
                Some(next) => {
                    before_id = next;
                    // Progress survives a stop between chunks.
                    self.store.set_sync_state(
                        self.account_id,
                        BLANK_BODY_HEAL_KEY,
                        &SyncState {
                            uidvalidity: 0,
                            last_uid: u64::try_from(before_id).unwrap_or(0),
                        },
                    )?;
                }
                None => {
                    done = true;
                    break;
                }
            }
        }

        self.store.set_sync_state(
            self.account_id,
            BLANK_BODY_HEAL_KEY,
            &SyncState {
                uidvalidity: done as u32,
                last_uid: if done {
                    0
                } else {
                    u64::try_from(before_id).unwrap_or(0)
                },
            },
        )?;
        let counts = format!(
            "{re_read} re-read into triage, {text_only} text only, {still_blank} still blank, \
             {refused} refused, {gone} gone from gmail, {fetched} fetched in {}s",
            started.elapsed().as_secs()
        );
        match (stopped, done) {
            (Some(err), _) => eprintln!(
                "squelch: blank-body heal stopped on a gmail error ({err}); {counts}; \
                 resuming next start"
            ),
            (None, true) => eprintln!("squelch: blank-body heal complete — {counts}"),
            (None, false) => eprintln!(
                "squelch: blank-body heal paused at the per-start ceiling — {counts}; \
                 resuming next start"
            ),
        }
        Ok(())
    }

    // ---- Recipients backfill ----------------------------------------------

    /// ONE-TIME sweep filling `messages.to_addrs` / `cc_addrs` / `bcc_addrs` on
    /// every row ingested before those columns were read per header, so the
    /// reader can show who a message went to instead of only who it came from.
    /// Same shape as [`harvest_sent_contacts`](Self::harvest_sent_contacts):
    /// only To/Cc/Bcc fetched (headers, never bodies) on the READ credential,
    /// and a `sync_state` completion flag so it runs once per install.
    ///
    /// THE WHOLE MAILBOX, not just Sent, which is what its predecessor walked.
    /// A recipient list on received mail is the answer to "was this written to
    /// me or copied to me", and the only rows that can answer it are the ones
    /// this pass reads. That is an order of magnitude more requests, which is
    /// what [`RECIPIENTS_PACE`] is for.
    ///
    /// BEST-EFFORT AND NON-FATAL by construction: it is spawned beside the sync
    /// loop, never inside it, and any error returns early with the flag UNSET, so
    /// the next daemon start simply redoes whatever is still NULL. A message
    /// Gmail no longer has (404) is not an error — it is written as "" ("looked,
    /// nobody named"), which is what keeps one deleted message from re-queueing
    /// the whole pass forever.
    pub async fn backfill_recipients(&self) -> Result<()> {
        let done = self
            .store
            .sync_state(self.account_id, RECIPIENTS_KEY)?
            .map(|s| s.last_uid >= 1)
            .unwrap_or(false);
        if done {
            return Ok(());
        }

        let source = self.source()?;
        let mut filled = 0usize;
        // Per-message failures are SKIPPED, not fatal: one message with a
        // persistent 4xx quirk must not abort the sweep (or re-run the whole
        // pass on every daemon start forever). Skipped rows stay NULL and are
        // filtered out of each batch so the loop still terminates; the done
        // flag is only set after a pass with zero skips, so they retry on the
        // next start.
        let mut skipped: Vec<i64> = Vec::new();
        loop {
            let pending = self
                .store
                .messages_missing_recipients(self.account_id, RECIPIENTS_BATCH)?;
            let pending: Vec<_> = pending
                .iter()
                .filter(|r| !skipped.contains(&r.message_id))
                .collect();
            if pending.is_empty() {
                break;
            }
            for row in &pending {
                // BCC IS REQUESTED FOR EVERY ROW and kept for almost none: the
                // store writes NULL for it on anything with `is_sent = 0`. Asking
                // per row would mean a second shape of request for one header
                // Gmail has already stripped from every inbound copy it holds.
                let msg = match source
                    .fetch_headers(&row.gmail_msg_id, &["To", "Cc", "Bcc"])
                    .await
                {
                    Ok(msg) => msg,
                    // Gone upstream: record the absence rather than retrying it
                    // on every daemon start for the life of the install.
                    Err(CoreError::NotFound) => Default::default(),
                    Err(_) => {
                        skipped.push(row.message_id);
                        continue;
                    }
                };
                // Through mail-parser via header synthesis, so grouped lists,
                // quoted display names and RFC2047 encoding render exactly as the
                // ingest path renders them.
                let blob = synthesize_rfc822_headers(&msg.headers);
                let mut to_mailboxes: Vec<(String, Option<String>)> = Vec::new();
                let mut cc_mailboxes: Vec<(String, Option<String>)> = Vec::new();
                let mut bcc_mailboxes: Vec<(String, Option<String>)> = Vec::new();
                if let Some(parsed) = mail_parser::MessageParser::default().parse(blob.as_bytes()) {
                    for (list, into) in [
                        (parsed.to(), &mut to_mailboxes),
                        (parsed.cc(), &mut cc_mailboxes),
                        (parsed.bcc(), &mut bcc_mailboxes),
                    ] {
                        if let Some(list) = list {
                            collect_mailboxes(list, into);
                        }
                    }
                }
                // EVERY row is written, "" included: the store predicate is
                // `cc_addrs IS NULL`, so a row left NULL would be handed back by
                // the next batch query and loop forever.
                self.store.set_message_recipients(
                    self.account_id,
                    row.message_id,
                    &format_recipients(&to_mailboxes).unwrap_or_default(),
                    &format_recipients(&cc_mailboxes).unwrap_or_default(),
                    &format_recipients(&bcc_mailboxes).unwrap_or_default(),
                )?;
                filled += 1;
                tokio::time::sleep(RECIPIENTS_PACE).await;
            }
        }

        if skipped.is_empty() {
            self.store.set_sync_state(
                self.account_id,
                RECIPIENTS_KEY,
                &SyncState {
                    uidvalidity: 1,
                    last_uid: 1,
                },
            )?;
            if filled > 0 {
                eprintln!("squelch: recipients backfill complete — {filled} messages filled");
            }
        } else {
            eprintln!(
                "squelch: recipients backfill left {} of {} unfetched; retrying next start",
                skipped.len(),
                skipped.len() + filled
            );
        }
        Ok(())
    }

    // ---- Fetch and ingest ----------------------------------------------------

    /// Fetch each id's raw RFC822 from the source and run it through the
    /// ingest pipeline. Requests are paced and, for a [`Urgency::Window`],
    /// quota retries retain the current position. Returns the count ingested.
    async fn fetch_raw_and_ingest(
        &self,
        ids: &[String],
        mailbox: Mailbox,
        origin: IngestOrigin,
        retry: Urgency,
    ) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let source = self.source()?;
        let rules = self.store.list_sender_rules(self.account_id)?;
        let mut count = 0usize;

        // FETCHES OVERLAP, INGEST DOES NOT. Up to `sync.fetch_concurrency`
        // GETs are in flight at once, each paced by the shared Gmail budget on
        // its way out, so the budget is the only limiter rather than the sum
        // of one round trip plus one ingest plus one embedding per message.
        // `FuturesOrdered`, not unordered: rows are ingested in listing order,
        // newest first, because that is the order the refine lane claims them
        // in and the order the board should fill in. The window is topped up
        // BEFORE the row just fetched is ingested, so the wire stays full while
        // the store and the embedder do their part.
        //
        // A hand-rolled window rather than `StreamExt::buffered` because a
        // closure returning a future that borrows `self` makes the whole
        // lifecycle future non-`Send` under rustc's current rules, and `run()`
        // is spawned.
        //
        // An `Err` from any fetch drops the window, which cancels the GETs
        // still in flight. The retry re-lists, and ingest is idempotent.
        let width = self.config.sync.fetch_concurrency.max(1);
        let mut queue = ids.iter().map(String::as_str);
        let mut window = futures::stream::FuturesOrdered::new();
        for id in queue.by_ref().take(width) {
            window.push_back(Self::fetch_raw_message(&*source, id, retry));
        }
        // Read before the first GET: a quota or auth refusal anywhere in this
        // batch means no error in it is about one message. See
        // [`Self::archive_skip`].
        let quota_or_auth = self.archive.quota_or_auth();

        while let Some((id, fetched)) = window.next().await {
            // THE WALK MAY STEP OVER A MESSAGE; nothing else may. A live path
            // that dropped an id would have lost mail somebody is owed.
            let skip = match &fetched {
                Err(e) if origin == IngestOrigin::Archive => {
                    self.archive_skip(id, e, quota_or_auth)
                }
                _ => false,
            };
            // AFTER the `?`, so a failed fetch does not push one more GET
            // through the budget only to drop it with the window.
            let (msg, now) = match fetched {
                Ok(fetched) => fetched,
                Err(e) if skip => {
                    // Redacted: the id and the kind of error, never content.
                    eprintln!(
                        "squelch: archive walk skipping message {id} ({})",
                        archive_skip_kind(&e)
                    );
                    if let Some(id) = queue.next() {
                        window.push_back(Self::fetch_raw_message(&*source, id, retry));
                    }
                    continue;
                }
                Err(e) => return Err(e),
            };
            if let Some(id) = queue.next() {
                window.push_back(Self::fetch_raw_message(&*source, id, retry));
            }
            // Only a catch-up has a denominator, so this is a no-op on the
            // ordinary incremental path — `catchup_step` moves a gauge that is
            // zero unless `catch_up` set one up. A line every 50 messages
            // rather than every message: enough to prove movement in a log
            // somebody is tailing, few enough not to bury the lines that mean
            // something.
            //
            // Not for the archive walk, which runs beside a catch-up's
            // aftermath and is no part of its denominator.
            //
            // Nor for a LIVE fetch (`Urgency::Live`): the first run polls
            // beside its backfill (`Self::first_run`), and mail that walk
            // brings in is no part of the window's count either.
            if origin != IngestOrigin::Archive
                && retry == Urgency::Window
                && let Some((_, total)) = self.metrics.catchup_progress()
            {
                let done = self.metrics.catchup_step();
                if done.is_multiple_of(50) {
                    eprintln!("squelch: catch-up {done}/{total} messages");
                }
            }
            let FetchedMessage {
                id: msg_id,
                thread_id,
                raw,
                received_at,
                placement,
            } = msg;
            let raw = match raw {
                RawBody::Bytes(bytes) => bytes,
                RawBody::Missing => continue, // nothing to ingest
                RawBody::Undecodable(e) => {
                    // Redacted: id + error only, never content.
                    eprintln!("squelch: skipping message (decode error): {e}");
                    continue;
                }
            };
            let fetched = RawFetched {
                account_id: self.account_id,
                gmail_msg_id: msg_id,
                gmail_thread_id: thread_id,
                raw,
                internal_date: received_at,
                is_sent: mailbox.sighting_is_sent(placement),
                is_spam: mailbox.is_spam(),
                account_addr: self.account_email.clone(),
            };
            // THE CLOCK IS READ PER MESSAGE AND AFTER ITS FETCH, not once for the
            // batch, and that is load bearing rather than tidy. `now` came back
            // with the message from [`Self::fetch_raw_message`], taken the
            // instant its GET returned and before it waited its turn behind the
            // rows ahead of it in the window. This value is what
            // `notify_eligible_stamp` records as "the moment we first saw the
            // message" (docs/NOTIFY.md §11.3) and what the rescue ceiling is
            // measured from, and this loop walks the whole window a few GETs at
            // a time: `catch_up` runs it over the whole backfill window, which
            // [`Self::catch_up_inner`]'s own comment records taking half an hour
            // for a real tenant. Frozen at the top of the function, both
            // directions are wrong:
            //
            // - a genuinely new message fetched 30 minutes in would enter the
            //   Stage-1 queue with 30 minutes of its rescue window already spent,
            //   and expire before the refine lane ever reached it: a notification
            //   dropped AND booked as `deliberate/expired`, which would have the
            //   one number §11.11 reads reporting a miss that the stamp itself
            //   invented;
            // - past `MAX_FUTURE_SKEW_SECS` of batch runtime every remaining
            //   message would read as dated in the future, fail `is_fresh`, and be
            //   stamped NULL: permanently unnotifiable, at every site, forever.
            //   That is exactly §2a's silent drop, reintroduced on the
            //   post-outage path where notifications matter most.
            //
            // AFTER the GET rather than before it, so a slow fetch is charged to
            // the fetch and not to the message's rescue window; and at the GET
            // rather than here, so the ingest and embedding of the rows ahead
            // are not charged to it either. It costs one `clock_gettime` per
            // network round trip. `ingest_with_rules` reads the same value; it
            // is a pure function of the message and the clock, so per-message is
            // simply the more truthful "now" for it too.
            // OLD BULK MAIL ON A WINDOW READ is filed as archive, not triaged
            // (see [`files_as_old_bulk`]). The inbox leg only: sent mail and
            // spam queue no model work anyway, and the archive walk is
            // archive already. Measured from the same per-message `now`.
            let bulk_floor = (retry == Urgency::Window
                && mailbox == Mailbox::Inbox
                && origin != IngestOrigin::Archive)
                .then(|| {
                    now - ChronoDuration::days(i64::from(
                        self.config.sync.backfill_bulk_triage_days,
                    ))
                });
            let ingested = match self.ingest_fetched(&fetched, &rules, now, origin, bulk_floor) {
                Ok(ingested) => ingested,
                // A message the store will not take is the walk's to step
                // over on the same terms as one Gmail will not serve.
                Err(e)
                    if origin == IngestOrigin::Archive
                        && self.archive_skip(id, &e, quota_or_auth) =>
                {
                    eprintln!(
                        "squelch: archive walk skipping message {id} ({})",
                        archive_skip_kind(&e)
                    );
                    continue;
                }
                Err(e) => return Err(e),
            };
            let Some(ingested) = ingested else {
                count += 1;
                continue;
            };
            // POLL LANE -> NOTIFICATION LANE, BEFORE THE EMBED. The fast push
            // used to be spawned from this spot (docs/NOTIFY.md §11.5 still
            // describes that shape); it is a durable `notification` job now,
            // queued inside the ingest transaction that just committed, and
            // the lane that claims it is poked from the same spot for the
            // same reason: the embedder below is ONNX inference on a
            // `spawn_blocking` thread that can take seconds per message, and a
            // poke behind it would be a notification arriving after the thing
            // it was about. Only a row the ingest stamped eligible, which is
            // the row it queued a push for: a backfill's rows, the user's own
            // sent copies and a catch-up's stale rows are stamped ineligible
            // and queue nothing, and a poke per such row would be a lane
            // round per row (a read over the tenant's pending queue, under
            // the store mutex the ingest itself needs) for the length of a
            // first backfill or a post-outage catch-up.
            if ingested.notify_eligible && !mailbox.is_spam() {
                self.notify_wake.notify_one();
            }
            if let Some(text) = ingested.embed_text {
                self.embed_and_store(ingested.id, text).await;
            }
            count += 1;
            // POLL LANE -> REFINE LANE, PER ROW. Poked from here rather than
            // from the poll loop because this is the one place that knows a row
            // actually landed. `Notify` stores a permit when nobody is waiting,
            // so a poke that arrives while the refine lane is mid-round wakes
            // it as soon as it finishes, and a window of pokes in one round
            // collapses into one extra round, not one per message.
            //
            // Per row rather than once per batch, and on a backfill as much as
            // on a walk: the refine lane runs beside the first backfill now
            // (see [`Self::run_once`]), and "start on the first row the moment
            // it lands" is the whole point of that. On a walk it is the
            // contract's "start on new mail immediately" (docs/NOTIFY.md
            // §11.2), applied to the first row of a batch rather than the last.
            //
            // Not for spam: those rows queue no agent work, so a poke per spam
            // row is a round that claims nothing, two hundred times over when
            // somebody opens the Spam page.
            //
            // Nor for the archive, for the same reason at a larger scale: a
            // year of rows that queue nothing.
            //
            // Nor for old bulk mail a window read filed as archive, which
            // queued nothing either.
            if !mailbox.is_spam() && origin != IngestOrigin::Archive && !ingested.archived {
                self.refine_wake.notify_one();
            }
        }
        Ok(count)
    }

    /// One raw fetch, as its own future so the fetch window above can hold
    /// several of them. The timestamp is "the moment we first saw the
    /// message", read the instant the fetch returns (see the clock note in
    /// [`Self::fetch_raw_and_ingest`]).
    async fn fetch_raw_message<'a>(
        source: &'a dyn MailSource,
        id: &'a str,
        retry: Urgency,
    ) -> (&'a str, Result<(FetchedMessage, DateTime<Utc>)>) {
        let fetched = source
            .fetch_raw(id, retry)
            .await
            .map(|msg| (msg, Utc::now()));
        (id, fetched)
    }

    /// Parse and atomically persist one message plus its durable model jobs,
    /// with no window-read floor: what the tests drive one message through.
    #[cfg(test)]
    fn ingest_one(
        &self,
        fetched: &RawFetched,
        rules: &[SenderRule],
        now: DateTime<Utc>,
        origin: IngestOrigin,
    ) -> Result<Option<Ingested>> {
        self.ingest_fetched(fetched, rules, now, origin, None)
    }

    /// [`Self::ingest_one`], with the window read's floor for old bulk mail:
    /// list mail dated before `bulk_floor` is filed as archive (see
    /// [`files_as_old_bulk`]). `None` files nothing.
    fn ingest_fetched(
        &self,
        fetched: &RawFetched,
        rules: &[SenderRule],
        now: DateTime<Utc>,
        origin: IngestOrigin,
        bulk_floor: Option<DateTime<Utc>>,
    ) -> Result<Option<Ingested>> {
        let mut triaged = ingest_with_rules(fetched, &self.config.stage1, now, rules, |addr| {
            self.store
                .is_known_contact(self.account_id, addr)
                .unwrap_or(false)
        });
        // THE ELIGIBILITY STAMP, computed BEFORE the store call because
        // `ingest_message` writes it on the triage row's FIRST INSERT only and
        // preserves it verbatim on conflict. `ingest_with_rules` deliberately
        // leaves it `None`: the pure pipeline does not know which sync path it
        // is on, and that fact is the whole decision.
        triaged.notify_eligible_at =
            notify_eligible_stamp(&triaged, origin, &self.config.notify, now);
        triaged.foreground_triage = origin == IngestOrigin::Incremental
            && !triaged.message.is_sent
            && !triaged.message.is_spam;
        triaged.first_load = origin == IngestOrigin::Backfill;
        // NO TRANSLATOR, NO QUEUE: a row queued here with nothing to work it
        // would read as "Translating..." in the reader for good. Detection is
        // free; the row is what promises a translation.
        if self.translator.is_none() {
            triaged.translate_lang = None;
        }
        // OVER TODAY'S TRANSLATE BUDGET, nothing holds triage, and a
        // backfilled message waits `deferred` for its reader. Asked only of
        // foreign mail: English mail has no row to queue.
        if triaged.translate_lang.is_some() {
            triaged.translate_over_budget = self.translate_over_budget(now);
        }
        // OLD BULK MAIL, FILED AS ARCHIVE on a window read, but only when the
        // store does not hold it yet: the insert-only archive write refuses an
        // id it already has, and such a message falls through to the ordinary
        // ingest below, unchanged. A live row a catch-up re-reads therefore
        // stays live and is refreshed as it always was (that re-read is how a
        // sanitizer fix reaches `body_html`), and an archive row stays archive.
        let arrived_at = fetched.internal_date.unwrap_or(triaged.message.received_at);
        let old_bulk = origin != IngestOrigin::Archive
            && files_as_old_bulk(&triaged, arrived_at, bulk_floor, rules);
        let filed = if old_bulk {
            // Never stamped: the store ignores the stamp on an archive row,
            // and this one must not poke the notification lane either.
            triaged.notify_eligible_at = None;
            self.store.ingest_archive_message(&triaged)?
        } else {
            None
        };
        let id = if let Some(id) = filed {
            id
        } else if origin == IngestOrigin::Archive {
            // Insert-only: an id the store already holds is left as it is, and
            // there is nothing to embed for a row this call did not write.
            //
            // MORE THAN A YEAR BACK IT IS STORED LITE: the text and the index,
            // not the HTML, which is the bulk of a row and is fetched back from
            // the provider if the owner ever opens it. Dropped here so the
            // bytes are never written; the store drops it again regardless.
            let stored = if archive_row_is_lite(triaged.message.received_at, self.archive_now()) {
                triaged.message.body_html = None;
                self.store.ingest_lite_archive_message(&triaged)?
            } else {
                self.store.ingest_archive_message(&triaged)?
            };
            let Some(id) = stored else {
                return Ok(None);
            };
            id
        } else {
            let id = self.store.ingest_message(&triaged)?;
            // A LIVE SIGHTING OF AN ARCHIVE ROW wrote nothing (the store keeps
            // archive rows write-once), so there is nothing new to embed and
            // nothing queued to wake the refine lane for. Without this the row
            // was re-embedded and the lane poked on every overlap at the
            // window's edge.
            if self.store.message_is_archive(self.account_id, id)? {
                return Ok(None);
            }
            id
        };
        // THE SEALED GATE, asked of the row as stored (`embeddable_mail_sql!`).
        // At ingest an inbound message has no decision yet, so it could still
        // turn out to be a login code: its text goes to no embedder now, and
        // the drain embeds it once a non-sealed decision lands. The owner's
        // own mail passes at once. Spam never does (below).
        let embeddable =
            !triaged.message.is_spam && self.store.message_embeddable(self.account_id, id)?;
        let embed_text = if !embeddable {
            // NEITHER IS PROVIDER SPAM, for two reasons that point the same way.
            // It would be the largest single consumer of embedder time in the
            // daemon — spam outnumbers real mail — spent on the one category of
            // message semantic recall must never return. And an embedding is a
            // similarity claim: spam is written to imitate the mail it is
            // impersonating, so a vector space containing it puts convincing
            // forgeries next to the real thing in results the user reads as
            // "your mail".
            //
            // AND NOT YET-UNDECIDED OR SEALED MAIL: see the gate above.
            None
        } else {
            Some(crate::embed::message_embed_text(
                &triaged.message.subject,
                &triaged.message.body,
                self.config.embed.max_chars,
            ))
        };
        Ok(Some(Ingested {
            id,
            embed_text,
            notify_eligible: triaged.notify_eligible_at.is_some(),
            archived: filed.is_some(),
        }))
    }

    /// When the account was created, from the store the first time and from
    /// [`SyncEngine::account_created`] after. `None` when the store could not
    /// say, which reads as "not new" wherever it is asked.
    fn account_created_at(&self) -> Option<DateTime<Utc>> {
        if let Some(created) = self.account_created.get() {
            return Some(*created);
        }
        let found = self
            .store
            .account_created_at(self.account_id)
            .ok()
            .flatten();
        if let Some(created) = found {
            let _ = self.account_created.set(created);
        }
        found
    }

    /// The fast lane, built on first use. See the [`SyncEngine::notify_lane`]
    /// field for why it is not built in `new`.
    fn notify_lane(&self) -> &Arc<notify_lane::NotifyLane<S>> {
        self.notify_lane.get_or_init(|| {
            Arc::new(notify_lane::NotifyLane::new(
                self.store.clone(),
                // The SAME client, so the lane shares the connection pool (and
                // the redirect refusal, which is what keeps a credentialed
                // header from being re-sent cross-host).
                self.http.clone(),
                self.config.notify.clone(),
                self.config.stage1.known_contact_importance,
                self.stage2_llm.clone(),
                self.metrics.clone(),
                self.account_id,
                self.warn_days.clone(),
            ))
        })
    }

    /// Embed `text` off the async runtime and write the vector for
    /// `message_id`. No-op without an embedder, and inside a remote embedder's
    /// backoff window ([`embed_backoff`]), which is what keeps a down
    /// embedding service from costing mail ingest a timeout per message. A
    /// failure logs a redacted one-liner (id + error kind, never body) and
    /// never propagates — the backfill pass recovers the vector, so ingest
    /// must not block on it.
    async fn embed_and_store(&self, message_id: i64, text: String) {
        let Some(embedder) = self.embedder() else {
            // No vector written: the drain owes this row one once an embedder
            // is attached.
            self.vectors_dirty.store(true, Ordering::Relaxed);
            return;
        };
        if !self.embed_backoff.may_call(tokio::time::Instant::now()) {
            // Same debt, same payer: the drain, once the window closes.
            self.vectors_dirty.store(true, Ordering::Relaxed);
            return;
        }
        let account_id = self.account_id;
        let store = self.store.clone();
        // ONNX inference is CPU-bound; keep it off the poll loop.
        // Re-checked under the store lock at the write: a decision that sealed
        // the message while its text was at the embedder wins.
        let result = tokio::task::spawn_blocking(move || {
            let vec = embedder.embed(&text)?;
            store
                .write_embedded_vectors(account_id, &[(message_id, &vec)])
                .map(|_| ())
        })
        .await;
        match &result {
            Ok(Ok(())) => self.embed_backoff.succeeded(),
            Ok(Err(e)) => {
                if !self.embed_backoff_opened(e) {
                    eprintln!(
                        "squelch: embed failed for message {message_id} (recoverable via backfill): {e}"
                    );
                }
            }
            Err(e) => eprintln!("squelch: embed task join error for message {message_id}: {e}"),
        }
        // Anything but a written vector is a row the drain owes.
        if !matches!(result, Ok(Ok(()))) {
            self.vectors_dirty.store(true, Ordering::Relaxed);
        }
    }

    /// Embed every message still missing a vector, in throttled batches, so
    /// recall covers pre-existing rows, ingest-time embed failures and mail
    /// whose triage decision has just landed. SEALED MAIL IS NEVER SELECTED:
    /// `messages_missing_vectors` reads only rows that pass
    /// `embeddable_mail_sql!` (not spam, not known sealed, and either a stored
    /// decision with access settled or the owner's own mail), and the write
    /// re-checks it under the store lock (docs/SECURITY.md §4d). Inbound mail
    /// still awaiting its decision is not selected until one lands. A failed
    /// batch logs a redacted one-liner and is retried on a later pass. No-op
    /// with no embedder.
    ///
    /// Runs only while [`SyncEngine::vectors_dirty`] says a vector may be
    /// missing; the flag's lifecycle across one pass is [`DrainGuard`]'s.
    async fn backfill_missing_vectors(&self) {
        let Some(embedder) = self.embedder() else {
            // Leave `vectors_dirty` standing: the rows are still owed, and the
            // round after an embedder is attached is the one to pay them.
            return;
        };
        if !self.embed_backoff.may_call(tokio::time::Instant::now()) {
            // Inside a remote embedder's backoff window. Before the query, not
            // after: the query walks the account, and the answer would only be
            // thrown away. The flag stays standing for the round after.
            return;
        }
        // Lowered BEFORE the first query, not after the last: a failure that
        // lands on the poll lane while this drain is between awaits raises it
        // again, and a clear at the end would erase that. The guard raises it
        // back for every way this pass can end short of draining the queue:
        // a query error, a failed batch, and CANCELLATION, which is not an
        // arm anywhere: when the poll lane errors, `run_lanes` drops this
        // future wherever it is parked, and a flag left lowered then would
        // leave every row past the dropped batch without a vector until the
        // next restart.
        self.vectors_dirty.store(false, Ordering::Relaxed);
        let mut pass = DrainGuard {
            flag: &self.vectors_dirty,
            drained: false,
        };
        let mut total = 0usize;
        // The originals first, then the translations' own table: the same
        // drain, the same batches, a different source and destination.
        for translations in [false, true] {
            match self.drain_vector_kind(&embedder, translations).await {
                Some(n) => total += n,
                // Stopped short: the guard leaves the debt on the books.
                None => {
                    Self::trim_after_drain(total).await;
                    return;
                }
            }
        }
        pass.drained = true;
        Self::trim_after_drain(total).await;
    }

    /// One table's half of [`Self::backfill_missing_vectors`]: `Some(embedded)`
    /// when the queue came back empty or short, `None` when a query or a batch
    /// failed and the rest is owed.
    async fn drain_vector_kind(
        &self,
        embedder: &Arc<dyn crate::embed::Embedder>,
        translations: bool,
    ) -> Option<usize> {
        let batch = self.config.embed.backfill_batch.max(1);
        let max_chars = self.config.embed.max_chars;
        let account_id = self.account_id;
        let mut total = 0usize;
        loop {
            let missing = if translations {
                self.store.translations_missing_vectors(account_id, batch)
            } else {
                self.store.messages_missing_vectors(account_id, batch)
            };
            let missing = match missing {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("squelch: vector backfill query failed ({e}); stopping pass");
                    return None;
                }
            };
            if missing.is_empty() {
                return Some(total);
            }
            let n = missing.len();
            // Flatten each message the SAME way ingest does. (Query text takes a
            // different road: search embeds the query as typed, uncut.)
            let store = self.store.clone();
            let embedder = embedder.clone();
            let result = tokio::task::spawn_blocking(move || -> Result<()> {
                let texts: Vec<String> = missing
                    .iter()
                    .map(|m| crate::embed::message_embed_text(&m.subject, &m.body, max_chars))
                    .collect();
                let vecs = embedder.embed_batch(&texts)?;
                if translations {
                    // One at a time: each re-checks, under the lock, that its
                    // translation is still served.
                    for (m, vec) in missing.iter().zip(vecs.iter()) {
                        store.upsert_translation_vector(account_id, m.message_id, vec)?;
                    }
                    return Ok(());
                }
                // One commit for the batch, not two per vector, each item
                // re-checked against the sealed gate under that commit's lock.
                let items: Vec<(i64, &[f32])> = missing
                    .iter()
                    .zip(vecs.iter())
                    .map(|(m, vec)| (m.message_id, vec.as_slice()))
                    .collect();
                store.write_embedded_vectors(account_id, &items).map(|_| ())
            })
            .await;

            match result {
                Ok(Ok(())) => {
                    self.embed_backoff.succeeded();
                    total += n;
                }
                Ok(Err(e)) => {
                    if !self.embed_backoff_opened(&e) {
                        eprintln!("squelch: vector backfill batch failed ({e}); stopping pass");
                    }
                    return None;
                }
                Err(e) => {
                    eprintln!("squelch: vector backfill task join error ({e}); stopping pass");
                    return None;
                }
            }

            // A short batch means we drained the queue; stop before re-querying.
            if n < batch {
                return Some(total);
            }
            // Throttle between batches so a large backfill doesn't peg the CPU
            // or starve the poll lane, which shares this task: the two lanes are
            // futures under one `select!`, not two runtimes, so a tight loop
            // here still delays the next `history.list`.
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Record a failed embed against the backoff, and say so when it opened a
    /// window: one line per window rather than one per message, carrying the
    /// error (a status or a transport class, never text). `false` means the
    /// error was not a transient remote one and the caller logs it as before.
    fn embed_backoff_opened(&self, e: &CoreError) -> bool {
        let Some(delay) = self.embed_backoff.failed(e, tokio::time::Instant::now()) else {
            return false;
        };
        eprintln!(
            "squelch: {e}; embedding paused for {} s (mail sync unaffected; missing \
             vectors are filled in by the backfill afterwards)",
            delay.as_secs()
        );
        true
    }

    async fn trim_after_drain(total: usize) {
        if total > 0 {
            eprintln!("squelch: vector backfill embedded {total} message(s) for semantic recall");
            // A batch pass is the single largest transient this process makes:
            // ONNX arenas plus every flattened body, hundreds of MB on a first
            // run, and glibc keeps all of it unless asked. Worth doing with the
            // session still loaded (+324 MB to +290 MB on the measured pass),
            // and gated on `total > 0` so it is at most once per poll tick. See
            // [`crate::mem`].
            crate::mem::trim_off_runtime().await;
        }
    }

    /// Bring back the mail whose reminders have come due.
    ///
    /// The poll tick is the clock: there is no timer per reminder, because a
    /// reminder is stored state and the daemon may well have been off when the
    /// moment passed. Whatever is due when the loop next runs fires then, so a
    /// restart costs punctuality and never a reminder.
    ///
    /// A CREDENTIAL OUTAGE COSTS NO MORE THAN A RESTART DOES, which is why the
    /// loop runs this AHEAD of `poll_once` rather than after it: nothing here
    /// talks to Gmail, so a mailbox stuck retrying an `invalid_grant` — or a
    /// weekend of Gmail being down — must not be what swallows Saturday's
    /// reminder.
    ///
    /// IT NO LONGER LANDS BEFORE `revisit_pass`, and that used to be stated here
    /// as a guarantee. The lane split (docs/NOTIFY.md §11.2) put this in the
    /// poll lane and `revisit_pass` in the refine lane, on independent clocks
    /// with independent wake sources, so the sweep can now read the standing
    /// band before this tick's reminders have fired and miss a mail that came
    /// back a moment ago. It is a race, not a lost reminder: both lanes cycle at
    /// `poll_secs`, the row stays `open` once it fires, and the next sweep sees
    /// it. One round of latency on a re-evaluation, self-correcting.
    ///
    /// Synchronous and infallible from the loop's point of view: one indexed
    /// UPDATE, and an error is logged and dropped. A store hiccup must not bounce
    /// the whole sync lifecycle over a feature that will retry in a minute.
    fn reminder_pass(&self) {
        match self.store.fire_due_reminders(self.account_id, Utc::now()) {
            Ok(ids) if !ids.is_empty() => {
                eprintln!("squelch: {} reminder(s) came due", ids.len())
            }
            Ok(_) => {}
            Err(e) => eprintln!("squelch: reminder sweep failed ({e}); skipping"),
        }
    }

    /// THE PAST-EVENT RE-CHECK (docs/CALENDAR.md §3.8): a day after a trip,
    /// ticket or booking is over, its mail gets a scheduled triage revisit,
    /// and the agent decides whether it is done. The same shape as
    /// [`Self::reminder_pass`]: local, synchronous, a count in the log and
    /// never a subject, and an error is logged and dropped, because the next
    /// tick tries again.
    fn event_revisit_pass(&self) {
        match self.store.schedule_past_event_revisits(
            self.account_id,
            Utc::now(),
            &self.config.revisit,
        ) {
            Ok(ids) if !ids.is_empty() => {
                eprintln!(
                    "squelch: {} message(s) about past events queued for a re-check",
                    ids.len()
                )
            }
            Ok(_) => {}
            Err(e) => eprintln!("squelch: past-event re-check scheduling failed ({e}); skipping"),
        }
    }

    /// The provider's own inbox unread counts ([`MailSource::inbox_unread`];
    /// Gmail's `users.labels.get(INBOX)`), into the store.
    ///
    /// The ONLY source for these numbers: the read scope cannot see (or write)
    /// read state through any other call, and our tables hold just the backfill
    /// window, so nothing local can answer "how much unread mail is sitting in
    /// Gmail right now".
    ///
    /// Cannot fail a cycle — sync's job is mail, and a stale count is a far
    /// better outcome than a poll loop that gives up over a cosmetic number. On
    /// failure of EITHER half (the fetch or the store write) the previous row
    /// stands, no clearing and no zeroing, and the notice is printed once per
    /// failing streak rather than every poll.
    async fn refresh_inbox_unread(&self) {
        // Both halves land in one Result so both go through the one latch:
        // whichever way this fails, it fails every poll tick, and a branch that
        // printed on its own would be the branch that spams.
        let fetched = match self.source() {
            Ok(source) => source.inbox_unread().await,
            Err(e) => Err(e),
        };
        let outcome = match fetched {
            Ok((messages, threads)) => self
                .store
                .set_inbox_unread(self.account_id, messages, threads)
                .map_err(|e| format!("storing the inbox unread counts failed ({e})")),
            Err(e) => Err(format!("inbox unread count fetch failed ({e})")),
        };
        match outcome {
            // Re-armed only by a whole cycle that worked, fetch and write both;
            // and the count is no longer owed.
            Ok(()) => {
                self.unread_warned.store(false, Ordering::Relaxed);
                self.unread_stale.store(false, Ordering::Relaxed);
            }
            // `swap` is the arm-and-test: only the transition into failure
            // prints. Error strings from this crate carry no secrets. The
            // count stays owed, so the next tick tries again.
            Err(why) => {
                self.unread_stale.store(true, Ordering::Relaxed);
                if !self.unread_warned.swap(true, Ordering::Relaxed) {
                    eprintln!("squelch: {why}; keeping the last known counts");
                }
            }
        }
    }

    // ---- cursor persistence (`sync_cursor`; see Store::sync_cursor) --------

    /// The stored cursor, `None` before the first. Remembered for
    /// [`Self::last_cursor`].
    fn load_cursor(&self) -> Result<Option<SyncCursor>> {
        let cursor = self.store.sync_cursor(self.account_id)?;
        self.remember_cursor(cursor.clone());
        Ok(cursor)
    }

    fn store_cursor(&self, cursor: &SyncCursor) -> Result<()> {
        self.store.set_sync_cursor(self.account_id, cursor)?;
        self.remember_cursor(Some(cursor.clone()));
        Ok(())
    }

    fn remember_cursor(&self, cursor: Option<SyncCursor>) {
        // Plain bookkeeping with no invariant a panic could break halfway.
        *self
            .last_cursor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = cursor;
    }

    /// The cursor last read or written, for the source's wait between ticks.
    fn last_cursor(&self) -> Option<SyncCursor> {
        self.last_cursor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// THE HISTORY WINDOW's far edge, and the only place it is computed: the
    /// first backfill, the triage cutover (`agent_triage_pass`) and the
    /// blank-body heal all measure from here. `sync.archive_days` is not in
    /// it, which `archive_days_moves_no_other_window` pins.
    fn backfill_since(&self) -> DateTime<Utc> {
        Utc::now() - ChronoDuration::days(self.config.sync.backfill_days as i64)
    }

    fn rules_for_stage2_note() -> &'static str {
        // Documentation anchor for the Stage-2 queue predicate: non-confident
        // rows are the ones left with model_used NULL.
        "model_used IS NULL AND sensitivity='normal'"
    }

    /// The top-level driver: loop, retrying with exponential backoff on any
    /// error, until shutdown is signalled.
    pub async fn run(&self, mut shutdown: tokio::sync::watch::Receiver<bool>) -> Result<()> {
        let _ = Self::rules_for_stage2_note();
        let mut backoff = BACKOFF_START;
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            let mut cancel = shutdown.clone();
            self.metrics.note_lifecycle_start();
            let result = tokio::select! {
                result = self.run_once(&mut shutdown) => result,
                _ = cancel.wait_for(|stopped| *stopped) => return Ok(()),
            };
            match result {
                Ok(()) => {
                    self.metrics.record_sync_ok();
                    return Ok(());
                }
                Err(e) => {
                    self.metrics.record_sync_error();
                    // GMAIL IS THE THING THAT FAILED — only the poll lane can
                    // return an `Err` at all — so the refine lane must not spend
                    // on the next cycle until a poll has proved otherwise. Without
                    // this, every backoff cycle re-entered `run_lanes` and burned
                    // a Stage-1 and a Stage-2 unit of a DAY-scoped cap on classify
                    // calls that were cancelled one Gmail round trip later. See
                    // the `poll_healthy` field.
                    //
                    // A lifecycle that polled successfully before failing starts
                    // the backoff over: without this, one bad hour a week ago
                    // left every later hiccup waiting the full five minutes.
                    self.poll_healthy.store(false, Ordering::Relaxed);
                    let polled_ok = self.polled_ok.swap(false, Ordering::Relaxed);
                    backoff = lifecycle_backoff(backoff, polled_ok);
                    if *shutdown.borrow() {
                        return Ok(());
                    }
                    self.metrics
                        .note_sync_retry_at(Utc::now().timestamp() + backoff.as_secs() as i64);
                    // Error strings from this crate never carry secrets.
                    eprintln!(
                        "squelch: sync error ({e}); retrying in {}s",
                        backoff.as_secs()
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        _ = shutdown.changed() => {
                            if *shutdown.borrow() { return Ok(()); }
                        }
                    }
                    backoff = (backoff * 2).min(BACKOFF_CAP);
                }
            }
        }
    }
}

/// The wait before the next lifecycle: back to [`BACKOFF_START`] when the one
/// that just failed had polled successfully first, otherwise the running value.
fn lifecycle_backoff(current: Duration, polled_ok: bool) -> Duration {
    if polled_ok { BACKOFF_START } else { current }
}

/// HOW THE ARCHIVE WALK SPENDS TIME. Its own struct, on the engine, so the
/// tests can shrink every wait to nothing and assert on the wire instead of
/// sleeping; production only ever builds [`ArchivePace::default`].
#[derive(Clone, Copy)]
struct ArchivePace {
    /// How much mail one chunk lists: the cursor moves back this far per
    /// persisted step. A week is small enough that a restart mid-chunk re-lists
    /// little, and large enough that a year is about fifty list calls.
    chunk: ChronoDuration,
    /// Ids fetched between two rests, and between two looks at shutdown and
    /// `window_in_flight`.
    batch: usize,
    /// After each burst the walk rests this many times the burst's own elapsed
    /// time. 2 is a one-third duty cycle: whatever pace the shared
    /// [`GmailBudget`] is currently allowing, the walk takes about a third of
    /// it and the poll lane, the sweeps and the write door queue behind an
    /// archive request at most a third of the time.
    rest_factor: u32,
    /// The FIRST wait after an error inside the walk (other than a quota
    /// refusal) before the same chunk is tried again. Doubles per consecutive
    /// failure up to `error_backoff_cap`; a walked chunk, or a walk found
    /// finished, resets it.
    error_backoff: Duration,
    /// The longest that wait gets.
    error_backoff_cap: Duration,
    /// How often an idle walk looks again while it is held off (no poll has
    /// succeeded yet, or a window of mail is being fetched).
    held_recheck: Duration,
    /// How often a finished (or switched-off) walk looks again. The target is
    /// `now - archive_days`, so a finished walk stays finished as time passes;
    /// this exists so a lane that outlives a config change still notices. Also
    /// how often a caught-up forward pass lists the hour that has gone by.
    done_recheck: Duration,
    /// THE ABSOLUTE CEILING: messages fetched in any one minute, whatever the
    /// shared budget would allow. 30 is a tenth of the standard quota
    /// profile's 300 bodies a minute, so the walk at full tilt leaves nine
    /// tenths of the published limit to the mail somebody is waiting for.
    per_minute: usize,
    /// The minute itself, a field so a test can make it short.
    per_minute_window: Duration,
    /// The first wait after Gmail refuses the walk for quota. Doubles per
    /// refusal up to `quota_backoff_cap`; a walked chunk resets it.
    quota_backoff: Duration,
    quota_backoff_cap: Duration,
    /// Quota refusals in one UTC day after which the walk stops until the next.
    quota_refusals_per_day: u32,
    /// Separate passes on which ONE MESSAGE must have failed before the next
    /// pass may step over it. See [`SyncEngine::archive_skip`].
    poison_attempts: u32,
    /// The most such messages one pass may step over before it fails instead.
    poison_skips: u32,
    /// How far behind the last successful poll the forward pass stays. See
    /// [`SyncEngine::archive_forward_chunk`] for what the day is for.
    ///
    /// WHAT IT DOES NOT COVER, on purpose. Mail that carried neither INBOX nor
    /// SENT a day after it arrived is stored as archive, and a hand that moves
    /// it into the inbox later does not change that: a catch-up that then
    /// lists it sees a row it must leave alone. The owner's way out is the
    /// human door's re-triage of that one message.
    forward_lag: ChronoDuration,
}

impl Default for ArchivePace {
    fn default() -> Self {
        Self {
            chunk: ChronoDuration::days(7),
            // A third of the minute's allowance, so the ceiling is met in
            // three even bursts rather than one of 25 and a long wait.
            batch: 10,
            rest_factor: 2,
            // 15s, 30s, 1m, 2m, 4m, then 5m for as long as it keeps failing.
            // The first retry is soon because the usual cause (a laptop
            // waking before its network, one dropped connection) is gone in
            // seconds, and it used to cost a flat five minutes. The cap stays
            // at the old flat wait: a failed request that is not a quota
            // refusal costs the budget next to nothing, so waiting longer
            // than five minutes only delays recovery from a real outage.
            error_backoff: Duration::from_secs(15),
            error_backoff_cap: Duration::from_secs(5 * 60),
            held_recheck: Duration::from_secs(30),
            done_recheck: Duration::from_secs(60 * 60),
            per_minute: 30,
            per_minute_window: Duration::from_secs(60),
            quota_backoff: Duration::from_secs(5 * 60),
            quota_backoff_cap: Duration::from_secs(60 * 60),
            quota_refusals_per_day: 3,
            poison_attempts: 3,
            poison_skips: 5,
            forward_lag: ChronoDuration::hours(24),
        }
    }
}

/// THE LITE TIER'S EDGE, in days: archive mail received longer ago than this
/// is stored without its HTML (`messages.html_lite`). A year, because that is
/// how far the archive reached before it reached five, and it is where the
/// reader stops being a place people browse and becomes a place search lands
/// them: the text is what keyword search and the agent read, and the HTML is
/// fetched back from the provider when the owner opens the thread. Not a
/// setting: it trades disk for one provider fetch per old message opened, and
/// that trade does not change per deployment.
pub const ARCHIVE_LITE_AFTER_DAYS: i64 = 365;

/// Whether an archive message received at `received_at` is stored lite,
/// judged against the walk's clock `now`.
fn archive_row_is_lite(received_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    received_at < now - ChronoDuration::days(ARCHIVE_LITE_AFTER_DAYS)
}

/// 2024-01-01T00:00:00Z. A clock reading earlier than this is wrong, and the
/// archive walk writes nothing by it. See [`SyncEngine::archive_walk_chunk`].
const ARCHIVE_CLOCK_FLOOR_UNIX: i64 = 1_704_067_200;

/// The archive walk's free-space floor is never less than the filesystem's
/// size divided by this: a fifth. See [`SyncEngine::archive_room`].
const ARCHIVE_FLOOR_VOLUME_SHARE: u64 = 5;

/// The most the volume share may ask for. A fifth of a 50 GiB tenant volume
/// is a sensible reserve; a fifth of a laptop's terabyte is 200 GiB the walk
/// has no claim on, and would hold it on any ordinarily full disk.
const ARCHIVE_FLOOR_SHARE_CAP: u64 = 5 * 1024 * 1024 * 1024;

/// The free-space floor in bytes: the configured floor, or the volume share
/// capped at [`ARCHIVE_FLOOR_SHARE_CAP`], whichever is larger.
fn archive_floor_bytes(min_free_mb: u64, total_bytes: u64) -> u64 {
    min_free_mb
        .saturating_mul(1024 * 1024)
        .max((total_bytes / ARCHIVE_FLOOR_VOLUME_SHARE).min(ARCHIVE_FLOOR_SHARE_CAP))
}

/// A walk held by the free-space floor resumes at the floor plus the floor
/// divided by this: a tenth more.
const ARCHIVE_FLOOR_RESUME_MARGIN: u64 = 10;

/// What the archive walk keeps between chunks, and the two flags it raises
/// for the rest of the engine. All of it is in memory and none of it is the
/// walk's position (that is the stored cursor): a restart forgets a backoff
/// and a failure count, which costs one more attempt and nothing else.
#[derive(Default)]
struct ArchiveRun {
    /// AN ARCHIVE BATCH IS BEING FETCHED AND INGESTED. Read by the refine lane,
    /// which does not drain vectors while it is. Deliberately not
    /// `window_in_flight`: that flag also stops the walk itself and drives the
    /// catch-up gauges, and the walk is no window anybody is waiting on.
    in_flight: AtomicBool,
    /// Messages the pass now running has stepped over for failing repeatedly.
    skipped: std::sync::atomic::AtomicU32,
    /// How many times the hold has started, changed or ended, which is how
    /// many lines [`SyncEngine::archive_hold`] has logged.
    hold_changes: AtomicU64,
    /// Quota and credential refusals the source has given the walk, ever.
    /// Counters rather than a last-error slot so a caller compares before and
    /// after its own work and concurrent fetches cannot overwrite each other.
    /// Shared with the source, which counts its [`Urgency::Background`]
    /// refusals into it.
    refusals: Arc<SourceRefusals>,
    /// Unix seconds of the last poll that succeeded in this process; 0 before
    /// the first. The forward pass's upper bound hangs off it.
    last_poll_ok_unix: AtomicI64,
    /// Everything read and written only between awaits.
    state: std::sync::Mutex<ArchiveState>,
}

#[derive(Default)]
struct ArchiveState {
    /// THE MESSAGES THAT HAVE FAILED IN THE CHUNK BEING WALKED: the chunk, as
    /// (forward?, cursor epoch), and for each Gmail id that failed in it, on
    /// how many separate passes. Per message and never per chunk: what it
    /// licenses is skipping that one message. Dropped when the chunk is
    /// walked or another chunk is begun.
    failing: Option<((bool, i64), std::collections::HashMap<String, u32>)>,
    /// The ids already booked in the pass now running, so one pass counts
    /// once however many times it meets an id.
    failed_this_pass: std::collections::HashSet<String>,
    /// The ids of the chunk being walked that the skipped-messages counter
    /// has already counted. Kept and dropped with `failing`.
    skips_counted: std::collections::HashSet<String>,
    /// What the walk last said was holding it off; `None` for nothing.
    hold: Option<ArchiveHold>,
    /// The wait the next quota refusal earns; `None` is the starting one.
    quota_backoff: Option<Duration>,
    /// The wait the LAST error earned, when it was a quota refusal.
    quota_wait: Option<Duration>,
    /// The wait the next non-quota error earns; `None` is the starting one.
    error_backoff: Option<Duration>,
    /// The wait the LAST error earned, when it was not a quota refusal.
    error_wait: Option<Duration>,
    quota_day: Option<chrono::NaiveDate>,
    quota_refusals_today: u32,
    /// The current minute of the absolute ceiling: when it began and how many
    /// messages it has admitted.
    minute: Option<(tokio::time::Instant, usize)>,
    /// When the forward pass last found nothing left to cover.
    forward_caught_up: Option<tokio::time::Instant>,
    /// THE FREE-SPACE FLOOR IS HOLDING THE WALK. While it is, room means the
    /// floor plus its margin; see [`SyncEngine::archive_room`].
    disk_short: bool,
    /// What the last disk guard to refuse measured, for the hold's log line.
    /// Numbers and setting names only.
    room_note: String,
    /// Test seams: a fixed clock, and a stand-in for the store's disk numbers.
    clock: Option<DateTime<Utc>>,
    disk_probe: Option<fn() -> Option<crate::store::ArchiveDisk>>,
}

impl ArchiveRun {
    fn state(&self) -> std::sync::MutexGuard<'_, ArchiveState> {
        // Plain bookkeeping with no invariant a panic could break halfway.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn quota_or_auth(&self) -> u64 {
        self.refusals.quota.load(Ordering::Relaxed) + self.refusals.auth.load(Ordering::Relaxed)
    }
}

impl ArchiveState {
    /// A pass over `chunk` is starting: keep its failures so far, forget any
    /// other chunk's, and open a fresh pass.
    fn begin_pass(&mut self, chunk: (bool, i64)) {
        if self
            .failing
            .as_ref()
            .is_none_or(|(failing, _)| *failing != chunk)
        {
            self.failing = Some((chunk, std::collections::HashMap::new()));
            self.skips_counted.clear();
        }
        self.failed_this_pass.clear();
    }

    /// `id` is being stepped over. Returns whether this is the first time in
    /// this chunk, which is when it is counted.
    fn count_skip(&mut self, id: &str) -> bool {
        self.skips_counted.insert(id.to_string())
    }

    /// `id` failed in the pass now running. Returns on how many EARLIER
    /// passes it had failed, and books this one (once per pass).
    fn book_failure(&mut self, id: &str) -> u32 {
        let Some((_, failures)) = self.failing.as_mut() else {
            return 0;
        };
        let count = failures.entry(id.to_string()).or_insert(0);
        if self.failed_this_pass.insert(id.to_string()) {
            *count += 1;
            *count - 1
        } else {
            count.saturating_sub(1)
        }
    }

    /// Book `n` messages against the minute's ceiling. Zero means go now;
    /// anything else is how long until the minute turns, and the booking then
    /// stands for the minute that starts when the caller wakes.
    fn admit(&mut self, n: usize, per_minute: usize, minute: Duration) -> Duration {
        let now = tokio::time::Instant::now();
        match self.minute {
            Some((began, taken)) if now.duration_since(began) < minute => {
                // A batch larger than the whole allowance still goes, alone.
                if taken + n <= per_minute.max(n) {
                    self.minute = Some((began, taken + n));
                    Duration::ZERO
                } else {
                    let wait = minute - now.duration_since(began);
                    self.minute = Some((now + wait, n));
                    wait
                }
            }
            _ => {
                self.minute = Some((now, n));
                Duration::ZERO
            }
        }
    }
}

/// Holds [`ArchiveRun::in_flight`] for one batch, and lowers it however the
/// batch ends: an error, or the lane's future being dropped at an await when
/// the poll lane bounces the lifecycle.
struct ArchiveBatchGuard<'a>(&'a ArchiveRun);

impl<'a> ArchiveBatchGuard<'a> {
    fn new(run: &'a ArchiveRun) -> Self {
        run.in_flight.store(true, Ordering::Relaxed);
        Self(run)
    }
}

impl Drop for ArchiveBatchGuard<'_> {
    fn drop(&mut self) {
        self.0.in_flight.store(false, Ordering::Relaxed);
    }
}

/// What a skipped archive message is logged as: a kind, never the error text
/// (a decode error can quote the body it choked on).
fn archive_skip_kind(error: &CoreError) -> &'static str {
    match error {
        CoreError::NotFound => "gone",
        CoreError::Sqlite(_) => "unstorable",
        _ => "unreadable",
    }
}

/// What one call of [`SyncEngine::archive_walk_chunk`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveStep {
    /// A chunk was listed, ingested and the cursor moved back over it.
    Walked,
    /// Not now: no poll has succeeded yet, a window of mail is being fetched,
    /// the embedder has not settled, the disk is short, the clock is wrong,
    /// Gmail has refused the walk enough for one day, or shutdown arrived
    /// mid-chunk. Nothing was persisted.
    Held,
    /// The cursor has reached `now - archive_days`.
    Done,
    /// `sync.archive_days` is 0.
    Off,
}

/// Rebuild a header-only RFC822 blob from a source's `(name, value)` headers
/// so mail-parser runs over it unchanged; the trailing blank line ends the
/// header section (empty body). Used by the Sent-contacts harvest and the
/// recipients backfill ([`MailSource::fetch_headers`] carries headers only)
/// and the contacts-seeding tests.
fn synthesize_rfc822_headers(headers: &[(String, String)]) -> String {
    let mut out = String::new();
    for (name, value) in headers {
        // HEADER INJECTION GUARD: Gmail names and values are single-line, but
        // upstream is never trusted blindly — a CR/LF in either would splice a
        // synthetic header into the blob.
        if name.contains('\r')
            || name.contains('\n')
            || value.contains('\r')
            || value.contains('\n')
        {
            continue;
        }
        out.push_str(name);
        out.push_str(": ");
        out.push_str(value);
        out.push_str("\r\n");
    }
    out.push_str("\r\n");
    out
}

/// Type alias helper so callers can name the concrete rule slice.
pub type Rules = Vec<SenderRule>;

#[cfg(test)]
mod outlook_tests;

#[cfg(test)]
mod tests {
    use super::*;
    // The Gmail request layer and its helpers moved to the Gmail source; the
    // tests that pin them on the engine's wire stay here and reach them there.
    use crate::mail::gmail::*;
    use crate::metrics::GmailErrorKind;
    use base64::Engine as _;

    /// The legacy `sync_state` key the Gmail cursor is mirrored under (see
    /// `Store::set_sync_cursor`), which these tests read and seed directly.
    const HISTORY_KEY: &str = "history";

    /// Gmail metadata headers as the `(name, value)` pairs a source returns.
    fn pairs(headers: &[MessageHeader]) -> Vec<(String, String)> {
        headers
            .iter()
            .map(|h| (h.name.clone(), h.value.clone()))
            .collect()
    }
    use crate::config::Stage1Config;
    use crate::store::SpamScope;
    use crate::store::SqliteStore;
    use crate::types::{AttentionUpdate, Disposition, NewMessage, Tier};

    /// The mail list as the client reads it (`human_agent_updates`, every
    /// band, nothing filtered but spam scope): what a sync test means by
    /// "visible in the mailbox". Sent copies never appear in it.
    fn inventory(
        store: &SqliteStore,
        acct: AccountId,
        since: DateTime<Utc>,
        spam: SpamScope,
    ) -> Vec<AttentionUpdate> {
        use crate::store::agent_triage::{AgentInventoryQuery, AgentTriageStore};
        store
            .human_agent_updates(
                acct,
                &AgentInventoryQuery {
                    since,
                    min_importance: None,
                    status: None,
                    band: None,
                    pending_reminders: false,
                    spam,
                    ranking: Default::default(),
                    before: None,
                    limit: None,
                },
            )
            .unwrap()
    }

    /// The last day of non-spam mail in [`inventory`].
    fn listed(store: &SqliteStore, acct: AccountId, now: DateTime<Utc>) -> Vec<AttentionUpdate> {
        inventory(
            store,
            acct,
            now - ChronoDuration::days(1),
            SpamScope::Exclude,
        )
    }

    #[test]
    fn gmail_quota_limit_note_names_the_limit_and_nothing_else() {
        let per_user = r#"{"error":{"details":[{"reason":"RATE_LIMIT_EXCEEDED","metadata":{"quota_limit":"UnitsPerMinutePerUser","quota_limit_value":"6000","quota_metric":"gmail.googleapis.com/default_units","consumer":"projects/1"}}]}}"#;
        assert_eq!(
            gmail_quota_limit_note(per_user),
            ", limit: UnitsPerMinutePerUser = 6000"
        );
        let odd_value = r#"{"error":{"details":[{"metadata":{"quota_limit":"PerUser","quota_limit_value":"lots"}}]}}"#;
        assert_eq!(gmail_quota_limit_note(odd_value), ", limit: PerUser");
        let metric_only = r#"{"error":{"details":[{"metadata":{"quota_metric":"gmail.googleapis.com/default_units"}}]}}"#;
        assert_eq!(
            gmail_quota_limit_note(metric_only),
            ", limit: gmail.googleapis.com/default_units"
        );
        for body in [
            r#"{"error":{"details":[{"metadata":{"quota_limit":"has spaces and private words"}}]}}"#,
            r#"{"error":{"errors":[{"reason":"userRateLimitExceeded"}]}}"#,
            "not json",
        ] {
            assert_eq!(gmail_quota_limit_note(body), "");
        }
    }

    #[test]
    fn gmail_error_reason_reports_known_reasons_without_freeform_content() {
        assert_eq!(
            gmail_error_reason(
                r#"{"error":{"errors":[{"reason":"userRateLimitExceeded"}],"message":"private account details"}}"#
            ),
            "userRateLimitExceeded"
        );
        assert_eq!(
            gmail_error_reason(
                r#"{"error":{"details":[{"reason":"ACCESS_TOKEN_SCOPE_INSUFFICIENT"}],"errors":[{"reason":"forbidden"}]}}"#
            ),
            "ACCESS_TOKEN_SCOPE_INSUFFICIENT"
        );
        for body in [
            "private non-JSON response",
            r#"{"error":{"errors":[{"reason":"private account details"}]}}"#,
            r#"{"error":{"message":"userRateLimitExceeded"}}"#,
        ] {
            assert_eq!(gmail_error_reason(body), "unavailable");
        }
    }

    /// The 403 split is the whole reason this classifier reads the body: Google
    /// spends one status on "too fast" and on "not allowed", and only the reason
    /// string tells them apart.
    #[test]
    fn gmail_status_classification_splits_403_on_the_reason() {
        let quota_body = r#"{"error":{"errors":[{"reason":"userRateLimitExceeded"}]}}"#;
        assert_eq!(
            classify_gmail_status(403, quota_body),
            GmailErrorKind::Quota
        );
        assert_eq!(
            classify_gmail_status(403, r#"{"error":{"errors":[{"reason":"forbidden"}]}}"#),
            GmailErrorKind::Http
        );
        assert_eq!(classify_gmail_status(429, ""), GmailErrorKind::Quota);
        assert_eq!(classify_gmail_status(401, ""), GmailErrorKind::Auth);
        assert_eq!(classify_gmail_status(500, ""), GmailErrorKind::Http);
    }

    /// Build a RawFetched from an RFC822 string, as the transport layer would.
    /// The account's own address is fixed to `me@example.com` in these fixtures.
    fn fixture(account_id: AccountId, msgid: &str, eml: &str, is_sent: bool) -> RawFetched {
        RawFetched {
            account_id,
            gmail_msg_id: msgid.to_string(),
            gmail_thread_id: None,
            raw: eml.as_bytes().to_vec(),
            internal_date: Some(Utc::now()),
            is_sent,
            is_spam: false,
            account_addr: "me@example.com".to_string(),
        }
    }

    /// End-to-end through the real store: ingest_with_rules -> ingest_message.
    fn ingest_into(
        store: &SqliteStore,
        account_id: AccountId,
        f: &RawFetched,
        now: DateTime<Utc>,
    ) -> i64 {
        let rules = store.list_sender_rules(account_id).unwrap();
        let triaged = ingest_with_rules(f, &Stage1Config::default(), now, &rules, |addr| {
            store.is_known_contact(account_id, addr).unwrap_or(false)
        });
        store.ingest_message(&triaged).unwrap()
    }

    // ---- notification events at the ingest call site -----------------------
    //
    // These drive the real pipeline through the real store; only the HTTP fetch
    // above it is out of reach, which is why the helper repeats the engine's two
    // lines of gating instead of calling `ingest_one` directly.

    /// The engine's ingest path with NO LLM configured, driven end to end:
    /// ingest, build the candidate, run the REAL fast lane. Returns
    /// `(message_id, emitted_event_id)`.
    ///
    /// IT IS NO LONGER A MIRROR OF AN EMISSION SITE, because there is no longer
    /// an emission site at ingest to mirror. The confident-seed-with-no-model
    /// path moved into [`notify_lane`] (docs/NOTIFY.md §11.5, `Model` step 1),
    /// which is what these tests now drive: the same three lines
    /// `fetch_raw_and_ingest` runs, awaited instead of spawned so an assertion
    /// cannot race the decision.
    ///
    /// The eligibility stamp is computed by the engine's own
    /// [`notify_eligible_stamp`], not re-derived, so a change to the §11.3 rule
    /// cannot pass these tests while breaking the daemon.
    async fn ingest_and_notify(
        store: &Arc<SqliteStore>,
        account_id: AccountId,
        f: &RawFetched,
        now: DateTime<Utc>,
        origin: IngestOrigin,
    ) -> (i64, Option<i64>) {
        let cfg = crate::config::NotifyConfig::default();
        let (id, triaged, rules) = ingest_stamped(store, account_id, f, now, origin, &cfg);
        let before = store.latest_event_id(account_id).unwrap();
        if let Some(c) = notify_lane::candidate(&triaged, id, &rules, &cfg, |addr| {
            store.is_known_contact(account_id, addr).unwrap_or(false)
        }) {
            let lane = Arc::new(notify_lane::NotifyLane::new(
                store.clone(),
                reqwest::Client::new(),
                cfg,
                Stage1Config::default().known_contact_importance,
                // NO MODEL, which is the whole premise: with nothing to wait for
                // the confident seed is the final word.
                None,
                SyncMetrics::new(),
                account_id,
                Arc::new(std::sync::Mutex::new(WarnDays::default())),
            ));
            lane.run(c).await.unwrap();
        }
        let emitted = store
            .events_after(account_id, before, 100)
            .unwrap()
            .into_iter()
            .find(|e| e.message_id == id)
            .map(|e| e.id);
        (id, emitted)
    }

    /// The engine's ingest path with an LLM configured: the row is committed
    /// carrying its eligibility stamp and NOTHING is emitted, because the
    /// promise is that a model verdict is coming and the refine site will emit.
    /// That is the state every refine-site test needs to start from.
    ///
    /// Returns the row's stamp so a test can assert on it directly — the whole
    /// point of §11.3 is that this one value, not the sender's `Date:`, decides
    /// what the refine sites are allowed to do.
    fn ingest_deferring_to_refine(
        store: &SqliteStore,
        account_id: AccountId,
        f: &RawFetched,
        now: DateTime<Utc>,
        origin: IngestOrigin,
    ) -> (i64, Option<DateTime<Utc>>) {
        let cfg = crate::config::NotifyConfig::default();
        let (id, triaged, _) = ingest_stamped(store, account_id, f, now, origin, &cfg);
        (id, triaged.notify_eligible_at)
    }

    /// Shared spine of the two helpers above: triage, stamp, commit.
    fn ingest_stamped(
        store: &SqliteStore,
        account_id: AccountId,
        f: &RawFetched,
        now: DateTime<Utc>,
        origin: IngestOrigin,
        cfg: &crate::config::NotifyConfig,
    ) -> (i64, TriagedMessage, Vec<crate::types::SenderRule>) {
        let rules = store.list_sender_rules(account_id).unwrap();
        let mut triaged = ingest_with_rules(f, &Stage1Config::default(), now, &rules, |addr| {
            store.is_known_contact(account_id, addr).unwrap_or(false)
        });
        triaged.notify_eligible_at = notify_eligible_stamp(&triaged, origin, cfg, now);
        let id = store.ingest_message(&triaged).unwrap();
        (id, triaged, rules)
    }

    /// The eligibility stamp as the DATABASE holds it, read back through a
    /// store method rather than off the `TriagedMessage` the test just built.
    /// That round trip is the assertion: a stamp that never reached SQLite, or
    /// one the `DO UPDATE SET` quietly rewrote, would pass every in-memory check
    /// and still leave the emission sites reading the wrong clock.
    ///
    /// `notify_eligible_at` because it reads the column for ANY row, sent
    /// copies and sealed mail included.
    fn stamp_of(
        store: &SqliteStore,
        account_id: AccountId,
        message_id: i64,
    ) -> Option<DateTime<Utc>> {
        store.notify_eligible_at(account_id, message_id).unwrap()
    }

    /// An ops-alert EML dated `at`: automated sender + alert language, so it
    /// lands Signal / importance 75 / CONFIDENT.
    fn alert_eml(at: DateTime<Utc>) -> String {
        format!(
            "From: Monitoring <alerts@monitoring.example>\r\n\
             To: me@example.com\r\n\
             Subject: Incident: checkout api is down\r\n\
             Date: {}\r\n\
             \r\n\
             A high-severity incident was opened for the checkout service.\r\n",
            at.to_rfc2822()
        )
    }

    #[tokio::test]
    async fn backfill_never_emits() {
        // A fresh install backfills a month of already-read mail. Not one push.
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let acct = store.ensure_account("me@example.com").unwrap();
        let now = Utc::now();
        let eml = alert_eml(now);
        let f = fixture(acct, "g-alert", &eml, false);

        let (mid, ev) = ingest_and_notify(&store, acct, &f, now, IngestOrigin::Backfill).await;
        assert_eq!(ev, None, "backfill is structurally silent");
        assert!(store.events_after(acct, 0, 100).unwrap().is_empty());
        assert_eq!(store.latest_event_id(acct).unwrap(), 0);

        // NULL, not "old": the mail itself is minutes fresh and the verdict is
        // above the line. The origin alone decided it, and the NULL is what
        // makes the silence hold at every later emission site too, not just at
        // this one.
        assert_eq!(stamp_of(&store, acct, mid), None);
    }

    #[tokio::test]
    async fn the_users_own_sent_copy_is_never_stamped() {
        // The user's own outbox is on the INCREMENTAL path and is as fresh as
        // mail gets, so neither of the other two arms of the stamp rule stops
        // it. `is_sent` is its own arm for exactly that reason: buzzing someone
        // about the mail they just sent is the most obviously wrong
        // notification the system could produce.
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let acct = store.ensure_account("me@example.com").unwrap();
        let now = Utc::now();

        let eml = format!(
            "From: me@example.com\r\n\
             To: Utility <billing@utilityco.example>\r\n\
             Subject: PAST DUE: Your electric bill\r\n\
             Date: {}\r\n\
             \r\n\
             Paying this now, sorry for the delay.\r\n",
            now.to_rfc2822()
        );
        let f = fixture(acct, "g-mine", &eml, /* is_sent */ true);
        let (mid, ev) = ingest_and_notify(&store, acct, &f, now, IngestOrigin::Incremental).await;

        assert_eq!(
            stamp_of(&store, acct, mid),
            None,
            "a sent copy is unstamped"
        );
        assert_eq!(ev, None);
        assert!(store.events_after(acct, 0, 100).unwrap().is_empty());
    }

    #[test]
    fn a_re_ingest_keeps_the_first_sight_stamp_and_a_null_stays_null() {
        // WRITTEN ONCE, ON FIRST INSERT. Both halves matter and they fail in
        // opposite directions: a stamp that got REFRESHED on every re-ingest
        // would hand a row an unbounded rescue window, so a catch-up touching a
        // day-old message could buzz for it; a NULL that got FILLED IN on
        // re-ingest would let the first catch-up after a backfill push the
        // month of archived mail the backfill deliberately silenced.
        //
        // Both are live paths, not hypotheticals: the history walk overlaps and
        // `catch_up` re-fetches the whole window, so almost every row in a
        // long-lived database is ingested more than once.
        let store = SqliteStore::open_in_memory().unwrap();
        let acct = store.ensure_account("me@example.com").unwrap();
        let first = Utc::now();
        // Inside the freshness window, so a naive recompute would produce a
        // NEW stamp rather than nothing. That is what makes this discriminate.
        let again = first + ChronoDuration::minutes(5);

        let eml = alert_eml(first);
        let f = fixture(acct, "g-alert", &eml, false);
        let (mid, _) =
            ingest_deferring_to_refine(&store, acct, &f, first, IngestOrigin::Incremental);
        assert_eq!(stamp_of(&store, acct, mid), Some(first));

        let (mid2, _) =
            ingest_deferring_to_refine(&store, acct, &f, again, IngestOrigin::Incremental);
        assert_eq!(mid2, mid, "same message row");
        assert_eq!(
            stamp_of(&store, acct, mid),
            Some(first),
            "the ORIGINAL stamp, not the fresher one: the rescue window is \
             measured from when we first saw the mail, and a re-scan is not a \
             new sighting"
        );

        // The other direction: a backfilled row re-ingested as fresh
        // incremental mail stays NULL.
        let back = alert_eml(first).replace("alerts@monitoring.example", "alerts@second.example");
        let bf = fixture(acct, "g-back", &back, false);
        let (bmid, _) =
            ingest_deferring_to_refine(&store, acct, &bf, first, IngestOrigin::Backfill);
        assert_eq!(stamp_of(&store, acct, bmid), None);
        let (bmid2, _) =
            ingest_deferring_to_refine(&store, acct, &bf, again, IngestOrigin::Incremental);
        assert_eq!(bmid2, bmid);
        assert_eq!(
            stamp_of(&store, acct, bmid),
            None,
            "a NULL stays NULL: the backfill's silence is permanent, and a \
             later catch-up cannot talk it out of it"
        );
    }

    #[tokio::test]
    async fn squelched_sender_and_noise_are_both_silent() {
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let acct = store.ensure_account("me@example.com").unwrap();
        let now = Utc::now();

        // A SQUELCH rule outranks the heuristic that would otherwise have
        // surfaced this sender (proved above).
        store
            .set_sender_rule(
                acct,
                "*@monitoring.example",
                "not urgent",
                Disposition::Squelch,
            )
            .unwrap();
        let eml = alert_eml(now);
        let f = fixture(acct, "g-alert", &eml, false);
        let (_, ev) = ingest_and_notify(&store, acct, &f, now, IngestOrigin::Incremental).await;
        assert_eq!(ev, None, "a squelch-ruled sender is silent");

        // Plain below-the-line noise: fresh, confident, and simply not important.
        let news = format!(
            "From: News <hello@newsletter.example>\r\n\
             To: me@example.com\r\n\
             Subject: This week in widgets\r\n\
             Date: {}\r\n\
             \r\n\
             Lots of widget news. Click here to unsubscribe from these emails.\r\n",
            now.to_rfc2822()
        );
        let nf = fixture(acct, "g-news", &news, false);
        let (_, ev) = ingest_and_notify(&store, acct, &nf, now, IngestOrigin::Incremental).await;
        assert_eq!(ev, None, "noise below the line is silent");

        assert!(store.events_after(acct, 0, 100).unwrap().is_empty());
    }

    /// The model id a deliberate emission site records, standing in for
    /// `stage1.model` / `stage2.model` in the tests that call
    /// [`SyncEngine::emit_deliberate`] directly. A fixed string so a ledger
    /// assertion is about the plumbing rather than about the default config.

    // ---- budget-notice log redaction (PII safety) -------------------------

    // ---- base64url raw decode ---------------------------------------------

    #[test]
    fn decode_raw_b64url_no_pad_round_trips() {
        let eml = "From: a@b.com\r\nSubject: hi\r\n\r\nbody\r\n";
        let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(eml);
        let out = decode_raw_b64url(&enc).unwrap();
        assert_eq!(out, eml.as_bytes());
    }

    #[test]
    fn decode_raw_b64url_accepts_padded_and_web_safe() {
        // 4 bytes => 6 base64 chars + '==' padding; values force '-'/'_' web-safe.
        let bytes: Vec<u8> = vec![0xfb, 0xff, 0xbf, 0xf0];
        let padded = base64::engine::general_purpose::URL_SAFE.encode(&bytes);
        assert!(padded.contains('='), "expected padding in this fixture");
        assert!(
            padded.contains('-') || padded.contains('_'),
            "expected web-safe chars in this fixture"
        );
        let out = decode_raw_b64url(&padded).unwrap();
        assert_eq!(out, bytes);
    }

    #[test]
    fn decode_raw_b64url_rejects_garbage() {
        assert!(decode_raw_b64url("!!!not base64!!!").is_err());
    }

    // ---- history cursor advance -------------------------------------------

    #[test]
    fn advance_history_cursor_takes_max_never_regresses() {
        assert_eq!(advance_history_cursor(100, [50, 75, 40]), 100);
        assert_eq!(advance_history_cursor(100, [150, 120, 200]), 200);
        assert_eq!(advance_history_cursor(0, std::iter::empty()), 0);
        assert_eq!(advance_history_cursor(10, [10]), 10);
    }

    // ---- 404 / expired-history fallback decision --------------------------

    #[test]
    fn history_decision_incremental_when_cursor_present_and_fresh() {
        assert_eq!(
            history_poll_decision(Some(4242), false),
            HistoryDecision::Incremental(4242)
        );
    }

    #[test]
    fn history_decision_full_catchup_on_expired() {
        assert_eq!(
            history_poll_decision(Some(4242), true),
            HistoryDecision::FullCatchUp
        );
    }

    #[test]
    fn history_decision_full_catchup_when_absent_or_zero() {
        assert_eq!(
            history_poll_decision(None, false),
            HistoryDecision::FullCatchUp
        );
        assert_eq!(
            history_poll_decision(Some(0), false),
            HistoryDecision::FullCatchUp
        );
    }

    // ---- header synthesis for metadata-only sent seeding ------------------

    #[test]
    fn synthesize_headers_seeds_recipients_not_self() {
        // From is the account itself; contacts come from To/Cc recipients.
        let headers = vec![
            MessageHeader {
                name: "From".into(),
                value: "me@example.com".into(),
            },
            MessageHeader {
                name: "To".into(),
                value: "alice@friends.com".into(),
            },
            MessageHeader {
                name: "Cc".into(),
                value: "bob@friends.com".into(),
            },
            MessageHeader {
                name: "Subject".into(),
                value: "re: lunch".into(),
            },
            MessageHeader {
                name: "Date".into(),
                value: "Mon, 7 Jul 2026 10:00:00 +0000".into(),
            },
        ];
        let raw = synthesize_rfc822_headers(&pairs(&headers));
        assert!(raw.ends_with("\r\n\r\n"));

        let store = SqliteStore::open_in_memory().unwrap();
        let acct = store.ensure_account("me@example.com").unwrap();
        let mut f = fixture(acct, "g-sent", &raw, true);
        f.raw = raw.into_bytes();
        ingest_into(&store, acct, &f, Utc::now());
        assert!(store.is_known_contact(acct, "alice@friends.com").unwrap());
        assert!(store.is_known_contact(acct, "bob@friends.com").unwrap());
        // The account's own address is a contact only because `ensure_account`
        // seeds it — the From header must not add to it.
        let me = store.search_contacts(acct, "me@example.com", 10).unwrap();
        assert_eq!(me[0].sent_count, 1);
    }

    #[test]
    fn synthesize_headers_drops_injected_newlines() {
        let headers = vec![MessageHeader {
            name: "From".into(),
            value: "x@y.com\r\nBcc: evil@z.com".into(),
        }];
        let raw = synthesize_rfc822_headers(&pairs(&headers));
        assert!(!raw.contains("Bcc"), "CRLF-injected header must be dropped");
    }

    // ---- internalDate parsing ---------------------------------------------

    #[test]
    fn parse_internal_date_millis() {
        // 2026-07-07T10:00:00Z = 1783591200000 ms.
        let dt = parse_internal_date(Some("1783591200000")).unwrap();
        assert_eq!(dt.timestamp(), 1783591200);
        assert!(parse_internal_date(None).is_none());
        assert!(parse_internal_date(Some("garbage")).is_none());
    }

    #[test]
    fn parse_history_id_handles_bad_input() {
        assert_eq!(parse_history_id("12345"), 12345);
        assert_eq!(parse_history_id(""), 0);
        assert_eq!(parse_history_id("not-a-number"), 0);
    }

    // ---- ingest pipeline invariants (unchanged behavior) ------------------

    #[test]
    fn sent_message_seeds_recipient_contacts_never_self_and_skips_inbox() {
        let store = SqliteStore::open_in_memory().unwrap();
        let acct = store.ensure_account("me@example.com").unwrap();
        // The user (me@example.com) sends to Alice, cc Bob. From == self.
        let eml = "From: me@example.com\r\n\
                   To: Alice <alice@friends.com>\r\n\
                   Cc: bob@friends.com\r\n\
                   Subject: re: lunch\r\n\
                   Date: Mon, 7 Jul 2026 10:00:00 +0000\r\n\
                   \r\n\
                   sounds good\r\n";
        let now = Utc::now();
        let f = fixture(acct, "g-sent", eml, /* is_sent */ true);
        ingest_into(&store, acct, &f, now);

        // Recipients become contacts; nobody else does.
        assert!(store.is_known_contact(acct, "alice@friends.com").unwrap());
        assert!(store.is_known_contact(acct, "bob@friends.com").unwrap());
        assert!(!store.is_known_contact(acct, "stranger@nowhere.io").unwrap());
        // The account's own address is a contact — `ensure_account` seeds it —
        // but NOT by way of this send: writing to Alice from your own address is
        // not evidence you write to yourself, so the seeded count stands where
        // it was.
        let me = store.search_contacts(acct, "me@example.com", 10).unwrap();
        assert_eq!(me.len(), 1);
        assert_eq!(me[0].sent_count, 1, "From: self must not bump self");

        // Sent mail must NOT pollute the mail list.
        assert!(
            listed(&store, acct, now).is_empty(),
            "sent mail must never surface in the mail list"
        );

        // And it must not appear in search results either.
        let hits = store.search(acct, "lunch", 10, 0).unwrap();
        assert!(hits.is_empty(), "sent mail must not appear in search");
    }

    // ---- HTML body: ingest sanitize + human-door serving ------------------

    #[test]
    fn html_email_stores_sanitized_html_served_by_client_thread_view() {
        let store = SqliteStore::open_in_memory().unwrap();
        let acct = store.ensure_account("me@example.com").unwrap();
        // Dangerous markup (script, onerror, javascript: href, form) alongside
        // benign table/img/style content.
        let eml = "From: News <news@substack.com>\r\n\
                   To: me@example.com\r\n\
                   Subject: Weekly\r\n\
                   Date: Mon, 7 Jul 2026 10:00:00 +0000\r\n\
                   Content-Type: text/html; charset=utf-8\r\n\
                   \r\n\
                   <html><body><script>steal()</script>\
                   <table><tr><td style=\"color:red\">Hello</td></tr></table>\
                   <img src=\"https://cdn.example.com/x.png\" onerror=\"evil()\">\
                   <a href=\"javascript:evil()\">bad</a>\
                   <form action=\"http://evil\"><input name=\"pw\"></form>\
                   </body></html>\r\n";
        let f = fixture(acct, "g-html", eml, false);
        ingest_into(&store, acct, &f, Utc::now());

        // gmail_thread_id is None in `fixture`, so thread_id falls back to the
        // message id "g-html".
        let view = store
            .thread_view_with_html(acct, "g-html")
            .expect("thread present");
        let msg = &view.messages[0];
        let html = msg.html.as_deref().expect("html stored");

        // Dangerous constructs are gone.
        assert!(!html.to_lowercase().contains("script"));
        assert!(!html.contains("steal"));
        assert!(!html.to_lowercase().contains("onerror"));
        assert!(!html.contains("evil"));
        assert!(!html.to_lowercase().contains("javascript:"));
        assert!(!html.to_lowercase().contains("<form"));
        assert!(!html.to_lowercase().contains("<input"));
        // Benign content survives recognizably.
        assert!(html.contains("<table"));
        assert!(html.contains("style=\"color:red\""));
        assert!(html.contains("https://cdn.example.com/x.png"));

        // The flattened text path still feeds triage/FTS.
        assert!(msg.content.contains("Hello"));
        assert!(!msg.content.contains('<'));
    }

    #[test]
    fn plaintext_email_leaves_html_null() {
        let store = SqliteStore::open_in_memory().unwrap();
        let acct = store.ensure_account("me@example.com").unwrap();
        let eml = "From: Alice <alice@friends.com>\r\n\
                   To: me@example.com\r\n\
                   Subject: hi\r\n\
                   Date: Mon, 7 Jul 2026 10:00:00 +0000\r\n\
                   \r\n\
                   plain text only, no markup\r\n";
        let f = fixture(acct, "g-plain", eml, false);
        ingest_into(&store, acct, &f, Utc::now());

        let view = store.thread_view_with_html(acct, "g-plain").unwrap();
        assert!(
            view.messages[0].html.is_none(),
            "plain-text-only mail must leave html NULL"
        );
        assert!(view.messages[0].content.contains("plain text only"));
    }

    #[test]
    fn sync_state_round_trips_history_id() {
        let store = SqliteStore::open_in_memory().unwrap();
        let acct = store.ensure_account("me@example.com").unwrap();
        assert!(store.sync_state(acct, HISTORY_KEY).unwrap().is_none());

        // A historyId larger than u32::MAX, to prove the field holds it.
        let big = (u32::MAX as u64) + 123_456;
        store
            .set_sync_state(
                acct,
                HISTORY_KEY,
                &SyncState {
                    uidvalidity: 0,
                    last_uid: big,
                },
            )
            .unwrap();
        let s = store.sync_state(acct, HISTORY_KEY).unwrap().unwrap();
        assert_eq!(s.last_uid, big);
    }

    #[test]
    fn urlencode_escapes_spaces_and_reserved() {
        assert_eq!(urlencode("newer_than:30d"), "newer_than:30d");
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("x&y"), "x%26y");
    }

    #[test]
    fn subtract_ids_drops_the_claimed_and_keeps_order() {
        let claimed = vec!["b".to_string(), "d".to_string()];
        let ids = vec!["a".into(), "b".into(), "c".into(), "d".into()];
        assert_eq!(
            subtract_ids(ids, &claimed),
            vec!["a".to_string(), "c".into()]
        );
        // Nothing claimed: the batch passes through untouched.
        assert_eq!(
            subtract_ids(vec!["a".to_string()], &[]),
            vec!["a".to_string()]
        );
    }

    // ---- the two-walk incremental poll, on the wire ------------------------
    //
    // `history.list` is LABEL-FILTERED, so an INBOX-only poll is structurally
    // blind to mail the user sends from Gmail web or their phone. Everything
    // under test here — walk ORDER, the single cursor commit, a cursor HELD on a
    // sent-side failure — lives in the sequence of HTTP calls and cannot be
    // asserted on a struct, so these drive the real engine against an axum app on
    // an ephemeral loopback port (the shape tests/push_relay.rs already uses).

    use async_trait::async_trait;
    use axum::extract::{Path as AxumPath, Query, State};
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::{Value, json};
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// One label's scripted `history.list` answer.
    #[derive(Clone, Default)]
    struct LabelHistory {
        /// `messageAdded` records, as (historyId, message ids).
        records: Vec<(u64, Vec<String>)>,
        /// The page-level newest historyId.
        top: u64,
        /// Answer 500 instead: an outage that hits ONE label's walk.
        fail: bool,
    }

    impl LabelHistory {
        fn added(top: u64, records: &[(u64, &str)]) -> Self {
            Self {
                records: records
                    .iter()
                    .map(|(hid, id)| (*hid, vec![id.to_string()]))
                    .collect(),
                top,
                fail: false,
            }
        }
        fn quiet(top: u64) -> Self {
            Self {
                records: Vec::new(),
                top,
                fail: false,
            }
        }
        fn broken() -> Self {
            Self {
                records: Vec::new(),
                top: 0,
                fail: true,
            }
        }
    }

    #[derive(Default)]
    struct GmailState {
        /// labelId -> its `history.list` answer.
        history: HashMap<String, LabelHistory>,
        /// labelIds -> the ids `messages.list` returns (the catch-up path).
        listing: HashMap<String, Vec<String>>,
        /// gmail message id -> RFC822 source.
        bodies: HashMap<String, String>,
        /// gmail message id -> the `labelIds` its `messages.get` reports.
        /// Absent means the field is omitted, as the older fixtures expect.
        message_labels: HashMap<String, Vec<String>>,
        errors: HashMap<String, std::collections::VecDeque<(u16, String)>>,
        retry_after: Option<String>,
        /// labelId -> the verbatim 200 body `labels.get` answers with. Absent
        /// (or `None`) means 500: an unscripted label must not read as a real
        /// zero. Whole bodies rather than a counter pair because the shapes
        /// under test include ones that are not label resources at all.
        labels: HashMap<String, Option<Value>>,
        /// `users.getProfile`'s historyId.
        profile_history_id: u64,
        /// gmail message id -> how long `messages.get` stalls before answering.
        /// A FIXTURE, never an assertion: it exists so a test can make one batch
        /// take measurable wall-clock time between two ids and then assert on the
        /// two timestamps the engine wrote, which is the only deterministic way
        /// to tell a per-message clock from a per-batch one.
        slow: HashMap<String, Duration>,
        /// gmail message id -> the `internalDate` `messages.get` reports for
        /// it, in unix milliseconds. Absent is "now", as it always was.
        internal_dates: HashMap<String, i64>,
        /// ALL MAIL, for a `messages.list` with no `labelIds` (the archive
        /// walk): `(internalDate in unix seconds, id)`, served by the `after:`
        /// and `before:` bounds of the request's `q`.
        all_mail: Vec<(i64, String)>,
        /// How many label-less listings still answer 500 before one succeeds.
        all_mail_failures: usize,
        /// The `q` of every `messages.list` served, as `(labelIds, q)`.
        list_queries: Vec<(String, String)>,
        /// Every call this mock served, in order, as `verb:key`.
        seen: Vec<String>,
    }

    #[derive(Clone, Default)]
    struct MockGmail(Arc<Mutex<GmailState>>);

    impl MockGmail {
        fn history(&self, label: &str, h: LabelHistory) -> &Self {
            self.0.lock().unwrap().history.insert(label.to_string(), h);
            self
        }
        fn listing(&self, label: &str, ids: &[&str]) -> &Self {
            self.0.lock().unwrap().listing.insert(
                label.to_string(),
                ids.iter().map(|s| s.to_string()).collect(),
            );
            self
        }
        fn body(&self, id: &str, eml: String) -> &Self {
            self.0.lock().unwrap().bodies.insert(id.to_string(), eml);
            self
        }
        fn message_labels(&self, id: &str, labels: &[&str]) -> &Self {
            self.0.lock().unwrap().message_labels.insert(
                id.to_string(),
                labels.iter().map(|s| s.to_string()).collect(),
            );
            self
        }
        /// A body the provider says it RECEIVED at `at` (its `internalDate`),
        /// whatever the message's own `Date:` header claims.
        fn received(&self, id: &str, at: DateTime<Utc>, eml: String) -> &Self {
            let mut st = self.0.lock().unwrap();
            st.internal_dates
                .insert(id.to_string(), at.timestamp_millis());
            st.bodies.insert(id.to_string(), eml);
            drop(st);
            self
        }
        /// One message that is in no listing by label, only in all mail,
        /// dated `at`: what the archive walk exists to find.
        fn old_mail(&self, id: &str, at: DateTime<Utc>, eml: String) -> &Self {
            let mut st = self.0.lock().unwrap();
            st.all_mail.push((at.timestamp(), id.to_string()));
            st.bodies.insert(id.to_string(), eml);
            self
        }
        /// The `q` of each listing served for `label` (`""` is all mail).
        fn queries(&self, label: &str) -> Vec<String> {
            let st = self.0.lock().unwrap();
            st.list_queries
                .iter()
                .filter(|(l, _)| l == label)
                .map(|(_, q)| q.clone())
                .collect()
        }
        /// Make `messages.get` for `id` take `d` before it answers.
        fn slow_body(&self, id: &str, d: Duration) -> &Self {
            self.0.lock().unwrap().slow.insert(id.to_string(), d);
            self
        }
        /// Script `labels.get` for one label: `Some(counts)` answers a
        /// well-formed label resource, `None` answers 500 (the outage the
        /// stored counts must survive).
        fn label(&self, label: &str, unread: Option<(i64, i64)>) -> &Self {
            let body = unread.map(|(messages, threads)| {
                json!({
                    "id": label,
                    "messagesUnread": messages,
                    "threadsUnread": threads,
                })
            });
            self.0
                .lock()
                .unwrap()
                .labels
                .insert(label.to_string(), body);
            self
        }
        /// Script `labels.get` with a verbatim 200 body, for the shapes
        /// [`MockGmail::label`] cannot express: a label Gmail sent without
        /// counters, or a 200 that is not a label resource.
        fn label_body(&self, label: &str, body: Value) -> &Self {
            self.0
                .lock()
                .unwrap()
                .labels
                .insert(label.to_string(), Some(body));
            self
        }
        fn profile(&self, history_id: u64) -> &Self {
            self.0.lock().unwrap().profile_history_id = history_id;
            self
        }
        fn seen(&self) -> Vec<String> {
            self.0.lock().unwrap().seen.clone()
        }
        fn calls(&self, key: &str) -> usize {
            self.seen().iter().filter(|s| *s == key).count()
        }
        /// Index of `key` in the call log; panics if it was never served.
        fn at(&self, key: &str) -> usize {
            self.seen()
                .iter()
                .position(|s| s == key)
                .unwrap_or_else(|| panic!("{key} was never called; saw {:?}", self.seen()))
        }
    }

    async fn mock_history(
        State(g): State<MockGmail>,
        Query(q): Query<HashMap<String, String>>,
    ) -> Response {
        let label = q.get("labelId").cloned().unwrap_or_default();
        let mut st = g.0.lock().unwrap();
        st.seen.push(format!("history:{label}"));
        let h = st.history.get(&label).cloned().unwrap_or_default();
        if h.fail {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        let records: Vec<Value> = h
            .records
            .iter()
            .map(|(hid, ids)| {
                json!({
                    "id": hid.to_string(),
                    "messagesAdded": ids
                        .iter()
                        .map(|m| json!({ "message": { "id": m } }))
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        Json(json!({ "history": records, "historyId": h.top.to_string() })).into_response()
    }

    async fn mock_messages_list(
        State(g): State<MockGmail>,
        Query(q): Query<HashMap<String, String>>,
    ) -> Response {
        let query = q.get("q").cloned().unwrap_or_default();
        let mut st = g.0.lock().unwrap();
        let ids = match q.get("labelIds") {
            Some(label) => {
                st.seen.push(format!("list:{label}"));
                st.list_queries.push((label.clone(), query));
                st.listing.get(label).cloned().unwrap_or_default()
            }
            // No label at all: all mail, narrowed only by the query's bounds.
            None => {
                st.seen.push("list-all".to_string());
                st.list_queries.push((String::new(), query.clone()));
                if st.all_mail_failures > 0 {
                    st.all_mail_failures -= 1;
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
                let bound = |name: &str| {
                    query
                        .split_whitespace()
                        .find_map(|term| term.strip_prefix(name)?.parse::<i64>().ok())
                };
                let (after, before) = (bound("after:"), bound("before:"));
                // `-in:inbox` and `-in:sent`, by the labels the message has
                // at the moment of THIS listing, as Gmail does.
                let wears = |id: &String, label: &str| {
                    st.message_labels
                        .get(id)
                        .is_some_and(|labels| labels.iter().any(|l| l == label))
                };
                let term = |t: &str| query.split_whitespace().any(|term| term == t);
                st.all_mail
                    .iter()
                    .filter(|(at, _)| after.is_none_or(|a| *at >= a))
                    .filter(|(at, _)| before.is_none_or(|b| *at < b))
                    .filter(|(_, id)| !(term("-in:inbox") && wears(id, "INBOX")))
                    .filter(|(_, id)| !(term("-in:sent") && wears(id, "SENT")))
                    .map(|(_, id)| id.clone())
                    .collect()
            }
        };
        Json(json!({
            "messages": ids.iter().map(|id| json!({ "id": id })).collect::<Vec<_>>(),
        }))
        .into_response()
    }

    async fn mock_message_get(
        State(g): State<MockGmail>,
        AxumPath(id): AxumPath<String>,
    ) -> Response {
        // Taken and DROPPED before the await: a std guard held across an await
        // point would deadlock the single-threaded test runtime.
        let stall = g.0.lock().unwrap().slow.get(&id).copied();
        if let Some(d) = stall {
            tokio::time::sleep(d).await;
        }
        let mut st = g.0.lock().unwrap();
        st.seen.push(format!("get:{id}"));
        if let Some((status, reason)) = st.errors.get_mut(&id).and_then(|errors| errors.pop_front())
        {
            let mut response = (
                StatusCode::from_u16(status).unwrap(),
                Json(json!({"error": {"details": [{"reason": reason}]}})),
            )
                .into_response();
            if let Some(value) = &st.retry_after {
                response
                    .headers_mut()
                    .insert("retry-after", value.parse().unwrap());
            }
            return response;
        }
        match st.bodies.get(&id) {
            // Both shapes at once: `raw` for the ingest fetches, and the
            // `payload.headers` a `format=metadata` caller reads. Gmail sends one
            // or the other per the requested format; serving both keeps the mock
            // format-agnostic, and each caller reads only its own field.
            Some(eml) => {
                let mut body = json!({
                    "id": id,
                    "raw": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(eml),
                    "payload": { "headers": mock_headers(eml) },
                    "internalDate": st
                        .internal_dates
                        .get(&id)
                        .copied()
                        .unwrap_or_else(|| Utc::now().timestamp_millis())
                        .to_string(),
                });
                if let Some(labels) = st.message_labels.get(&id) {
                    body["labelIds"] = json!(labels);
                }
                Json(body).into_response()
            }
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }

    /// The header block of an RFC822 fixture as Gmail's `payload.headers[]`.
    /// Fixtures are single-line headers, so no continuation handling. Values are
    /// trimmed of the CR the fixtures' CRLF endings leave behind — Gmail's own
    /// values are bare, and `synthesize_rfc822_headers` drops anything carrying
    /// one as an injection attempt.
    fn mock_headers(eml: &str) -> Vec<Value> {
        eml.lines()
            .take_while(|l| !l.trim().is_empty())
            .filter_map(|l| l.split_once(": "))
            .map(|(name, value)| json!({ "name": name.trim(), "value": value.trim() }))
            .collect()
    }

    async fn mock_label(State(g): State<MockGmail>, AxumPath(id): AxumPath<String>) -> Response {
        let mut st = g.0.lock().unwrap();
        st.seen.push(format!("label:{id}"));
        if let Some((status, reason)) = st
            .errors
            .get_mut(&format!("label:{id}"))
            .and_then(|errors| errors.pop_front())
        {
            return (
                StatusCode::from_u16(status).unwrap(),
                Json(json!({"error": {"details": [{"reason": reason}]}})),
            )
                .into_response();
        }

        match st.labels.get(&id).cloned().flatten() {
            Some(body) => Json(body).into_response(),
            None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }

    async fn mock_profile(State(g): State<MockGmail>) -> Json<Value> {
        let st = g.0.lock().unwrap();
        Json(json!({ "historyId": st.profile_history_id.to_string() }))
    }

    /// Bind the mock on an ephemeral loopback port; returns its API base.
    async fn serve_mock(g: MockGmail) -> String {
        let app = Router::new()
            .route("/history", get(mock_history))
            .route("/messages", get(mock_messages_list))
            .route("/messages/{id}", get(mock_message_get))
            .route("/labels/{id}", get(mock_label))
            .route("/profile", get(mock_profile))
            .with_state(g);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    /// A credential store handing out one fixed token; the mock ignores it.
    struct FixedToken;

    #[async_trait]
    impl CredentialStore for FixedToken {
        async fn token(&self, _account: AccountId) -> Result<crate::credentials::OAuthToken> {
            Ok(crate::credentials::OAuthToken {
                access_token: "test-token".to_string(),
                refresh_token: None,
                expires_at: None,
            })
        }
    }

    fn engine(
        store: Arc<SqliteStore>,
        acct: AccountId,
        base: &str,
    ) -> SyncEngine<SqliteStore, FixedToken> {
        engine_with_config(store, acct, base, Config::default())
    }

    /// [`engine`] with the config spelled out, for the tests whose whole subject
    /// is a config field (an LLM key, a poll interval).
    fn engine_with_config(
        store: Arc<SqliteStore>,
        acct: AccountId,
        base: &str,
        config: Config,
    ) -> SyncEngine<SqliteStore, FixedToken> {
        let mut engine = SyncEngine::new(
            store,
            Arc::new(FixedToken),
            acct,
            "me@example.com".to_string(),
            config,
        )
        .with_api_base(base.to_string());
        // Unpaced, and no refusal pause: a quota test must not sit through
        // production's one-minute hold.
        engine.gmail_budget = Arc::new(
            GmailBudget::with_unit(crate::config::GmailQuotaProfile::Standard, Duration::ZERO)
                .with_adaptation(Duration::ZERO, Duration::ZERO),
        );
        engine
    }

    /// Open an in-memory store with a live history cursor at `cursor`.
    fn store_at_cursor(cursor: Option<u64>) -> (Arc<SqliteStore>, AccountId) {
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let acct = store.ensure_account("me@example.com").unwrap();
        if let Some(c) = cursor {
            store
                .set_sync_state(
                    acct,
                    HISTORY_KEY,
                    &SyncState {
                        uidvalidity: 0,
                        last_uid: c,
                    },
                )
                .unwrap();
        }
        (store, acct)
    }

    /// The ledger category the fast lane books under is the SAME STRING both
    /// cost estimators price off. It is the one category with prices of its
    /// own, so a second spelling would not drop the row, it would cost it at the
    /// Stage-1 model's rates and overstate the cheapest pass in the pipeline.
    #[test]
    fn the_notify_ledger_category_is_the_one_the_cost_estimators_price() {
        assert_eq!(NOTIFY_USAGE_CATEGORY, "notify");
        assert_eq!(NOTIFY_USAGE_CATEGORY, crate::metrics::NOTIFY_USAGE_CATEGORY);
    }

    fn cursor_of(store: &SqliteStore, acct: AccountId) -> Option<u64> {
        store
            .sync_state(acct, HISTORY_KEY)
            .unwrap()
            .map(|s| s.last_uid)
    }

    /// Mail the user WROTE, dated `at` so it passes the first-sight freshness
    /// test — an old `Date:` must not be what makes these tests quiet. (Sent
    /// mail earns no eligibility stamp anyway; that is the point being tested,
    /// so it has to be the ONLY reason it is silent.)
    fn sent_eml(at: DateTime<Utc>, to: &str, subject: &str) -> String {
        format!(
            "From: me@example.com\r\n\
             To: {to}\r\n\
             Subject: {subject}\r\n\
             Date: {}\r\n\
             \r\n\
             writing this from the phone app\r\n",
            at.to_rfc2822()
        )
    }

    fn spam_eml(at: DateTime<Utc>, subject: &str) -> String {
        format!(
            "From: winner@prize-draw.example\r\n\
             To: me@example.com\r\n\
             Subject: {subject}\r\n\
             Date: {}\r\n\
             \r\n\
             Claim your prize now. Reply with your bank details.\r\n",
            at.to_rfc2822()
        )
    }

    #[tokio::test]
    async fn gmail_quota_retry_keeps_catchup_position() {
        for status in [403, 429] {
            let g = MockGmail::default();
            g.listing(LABEL_INBOX, &["a", "b"]);
            g.body("a", sent_eml(Utc::now(), "a@example.com", "a"));
            g.body("b", sent_eml(Utc::now(), "b@example.com", "b"));
            g.0.lock()
                .unwrap()
                .errors
                .insert("b".into(), [(status, "RATE_LIMIT_EXCEEDED".into())].into());
            let base = serve_mock(g.clone()).await;
            let (store, acct) = store_at_cursor(Some(1));
            let mut e = engine(store, acct, &base);
            e.gmail_quota_backoff = Duration::from_millis(30);
            let started = tokio::time::Instant::now();
            e.catch_up().await.unwrap();
            assert!(started.elapsed() >= Duration::from_millis(30));
            assert_eq!(g.calls("list:INBOX"), 1);
            assert_eq!(g.calls("get:a"), 1);
            assert_eq!(g.calls("get:b"), 2);
            let metrics = crate::metrics::render(&e.metrics, None);
            assert!(metrics.contains("squelchd_gmail_quota_retries_total 1\n"));
            assert!(metrics.contains("squelchd_gmail_api_errors_total{kind=\"quota\"} 0\n"));
        }
    }

    /// A credential store that counts how often the engine asked past its
    /// in-memory copy ([`CredentialStore::fresh_token`]).
    #[derive(Clone, Default)]
    struct CountingToken {
        fresh: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl CredentialStore for CountingToken {
        async fn token(&self, _account: AccountId) -> Result<crate::credentials::OAuthToken> {
            Ok(crate::credentials::OAuthToken {
                access_token: "cached-token".to_string(),
                refresh_token: None,
                expires_at: None,
            })
        }
        async fn fresh_token(&self, account: AccountId) -> Result<crate::credentials::OAuthToken> {
            self.fresh.fetch_add(1, Ordering::SeqCst);
            self.token(account).await
        }
    }

    /// THE 401 RETRY ASKS PAST THE CACHE: the first attempt takes the token
    /// the store has in memory, and the retry after Gmail refuses it asks
    /// for a fresh one, so a token rotated behind the daemon's back is picked
    /// up on the retry rather than at expiry.
    #[tokio::test]
    async fn a_401_retries_once_with_a_token_read_past_the_cache() {
        let g = MockGmail::default();
        g.0.lock()
            .unwrap()
            .errors
            .insert("label:INBOX".into(), [(401, "authError".into())].into());
        g.label(LABEL_INBOX, Some((2, 1)));
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(Some(1));
        let creds = CountingToken::default();
        let e = SyncEngine::new(
            store.clone(),
            Arc::new(creds.clone()),
            acct,
            "me@example.com".to_string(),
            Config::default(),
        )
        .with_api_base(base);
        e.refresh_inbox_unread().await;
        assert_eq!(g.calls("label:INBOX"), 2, "refused once, retried once");
        assert_eq!(
            creds.fresh.load(Ordering::SeqCst),
            1,
            "the retry, and only the retry, asked past the cache"
        );
        assert_eq!(
            store.inbox_unread(acct).unwrap().unwrap().messages,
            2,
            "and the retry succeeded"
        );
    }

    #[tokio::test]
    async fn gmail_cosmetic_quota_error_does_not_delay_the_poll() {
        let g = MockGmail::default();
        g.0.lock().unwrap().errors.insert(
            "label:INBOX".into(),
            [(403, "RATE_LIMIT_EXCEEDED".into())].into(),
        );
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(Some(1));
        let mut e = engine(store, acct, &base);
        e.gmail_quota_backoff = Duration::from_secs(30);
        tokio::time::timeout(Duration::from_secs(2), async {
            e.refresh_inbox_unread().await;
            e.poll_once().await.unwrap();
        })
        .await
        .expect("unread quota response parked the poll lane");
        assert_eq!(g.calls("label:INBOX"), 1);
        assert_eq!(g.calls("history:INBOX"), 1);
        let metrics = crate::metrics::render(&e.metrics, None);
        assert!(metrics.contains("squelchd_gmail_quota_retries_total 0\n"));
        assert!(metrics.contains("squelchd_gmail_api_errors_total{kind=\"quota\"} 1\n"));
    }

    #[tokio::test]
    async fn gmail_quota_retry_exhaustion_counts_one_failed_call() {
        let g = MockGmail::default();
        g.0.lock().unwrap().errors.insert(
            "a".into(),
            (0..9)
                .map(|_| (429, "RATE_LIMIT_EXCEEDED".into()))
                .collect(),
        );
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(Some(1));
        let mut e = engine(store, acct, &base);
        e.gmail_quota_backoff = Duration::ZERO;
        assert!(
            e.gmail_source()
                .test_get_json_with_retry::<Value>(&format!("{base}/messages/a"), Urgency::Window)
                .await
                .is_err()
        );
        assert_eq!(g.calls("get:a"), 3);
        let metrics = crate::metrics::render(&e.metrics, None);
        assert!(metrics.contains("squelchd_gmail_quota_retries_total 2\n"));
        assert!(metrics.contains("squelchd_gmail_api_errors_total{kind=\"quota\"} 1\n"));
    }

    #[tokio::test]
    async fn gmail_long_retry_after_returns_without_sleeping_or_retrying_early() {
        let g = MockGmail::default();
        {
            let mut state = g.0.lock().unwrap();
            state
                .errors
                .insert("a".into(), [(429, "RATE_LIMIT_EXCEEDED".into())].into());
            state.retry_after = Some("86400".into());
        }
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(Some(1));
        let e = engine(store, acct, &base);
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            e.gmail_source()
                .test_get_json_with_retry::<Value>(&format!("{base}/messages/a"), Urgency::Window),
        )
        .await
        .expect("Retry-After held this call open");
        assert!(result.is_err());
        assert_eq!(g.calls("get:a"), 1);
    }

    #[test]
    fn gmail_quota_sleep_budget_is_bounded() {
        assert_eq!(
            gmail_quota_delay(
                Duration::from_secs(1),
                Some(Duration::from_secs(20)),
                GMAIL_QUOTA_RETRY_CAP
            ),
            Some(Duration::from_secs(20))
        );
        assert_eq!(
            gmail_quota_delay(Duration::from_secs(500), None, GMAIL_QUOTA_RETRY_CAP),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            gmail_quota_delay(
                Duration::from_secs(1),
                Some(Duration::from_secs(31)),
                GMAIL_QUOTA_RETRY_CAP
            ),
            None
        );
        assert!(GMAIL_QUOTA_RETRY_CAP * GMAIL_QUOTA_RETRIES as u32 <= Duration::from_secs(60));
        let future = (Utc::now() + chrono::Duration::hours(1)).to_rfc2822();
        assert_eq!(
            gmail_quota_delay(
                Duration::from_secs(1),
                gmail_retry_after(&future),
                GMAIL_QUOTA_RETRY_CAP
            ),
            None
        );
    }

    /// Google's quota refusal slows the MAILBOX, not just this request: the
    /// shared budget comes back at half the rate, so the sweeps engine and
    /// the write door slow down with the sync loop. A permission refusal is
    /// not a quota signal and leaves the pace alone.
    #[tokio::test]
    async fn a_quota_refusal_slows_the_shared_budget() {
        let g = MockGmail::default();
        g.0.lock()
            .unwrap()
            .errors
            .insert("a".into(), [(403, "RATE_LIMIT_EXCEEDED".into())].into());
        g.0.lock().unwrap().errors.insert(
            "b".into(),
            [(403, "ACCESS_TOKEN_SCOPE_INSUFFICIENT".into())].into(),
        );
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(Some(1));
        let shared = Arc::new(
            GmailBudget::with_unit(
                crate::config::GmailQuotaProfile::Standard,
                Duration::from_micros(10),
            )
            .with_adaptation(Duration::ZERO, Duration::from_secs(3600)),
        );
        let e = engine(store, acct, &base).with_gmail_budget(shared.clone());
        let _ = e
            .gmail_source()
            .test_get_json::<Value>(&format!("{base}/messages/b"))
            .await;
        assert_eq!(
            shared.unit(),
            Duration::from_micros(10),
            "a permission 403 is not quota"
        );
        let _ = e
            .gmail_source()
            .test_get_json::<Value>(&format!("{base}/messages/a"))
            .await;
        assert_eq!(
            shared.unit(),
            Duration::from_micros(20),
            "a quota 403 halves the rate"
        );
    }

    /// A success through the ENGINE lets the shared budget recover; without
    /// it production would stay slowed until the daemon restarts.
    #[tokio::test]
    async fn an_engine_success_lets_the_budget_recover() {
        let g = MockGmail::default();
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(Some(1));
        let shared = Arc::new(
            GmailBudget::with_unit(
                crate::config::GmailQuotaProfile::Standard,
                Duration::from_micros(10),
            )
            .with_adaptation(Duration::ZERO, Duration::ZERO),
        );
        shared.note_quota_refusal(None);
        assert_eq!(shared.unit(), Duration::from_micros(20));
        let e = engine(store, acct, &base).with_gmail_budget(shared.clone());
        e.gmail_source()
            .test_get_json::<Value>(&format!("{base}/profile"))
            .await
            .unwrap();
        assert_eq!(shared.unit(), Duration::from_micros(18));
    }

    /// Google asks for longer than the catch-up's own sleep cap, but the
    /// budget is doing the waiting now: the request retries after the hold
    /// instead of failing and restarting the whole catch-up.
    #[tokio::test]
    async fn a_catch_up_retries_through_a_hold_longer_than_its_sleep_cap() {
        let g = MockGmail::default();
        g.body("a", sent_eml(Utc::now(), "a@example.com", "a"));
        {
            let mut state = g.0.lock().unwrap();
            state
                .errors
                .insert("a".into(), [(429, "RATE_LIMIT_EXCEEDED".into())].into());
            state.retry_after = Some("2".into());
        }
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(Some(1));
        let mut e = engine(store, acct, &base);
        e.gmail_quota_retry_cap = Duration::from_millis(500);
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            e.gmail_source()
                .test_get_json_with_retry::<Value>(&format!("{base}/messages/a"), Urgency::Window),
        )
        .await
        .expect("the hold is seconds, not forever");
        assert!(
            result.is_ok(),
            "the catch-up request should survive the hold"
        );
        assert_eq!(g.calls("get:a"), 2);
    }

    #[tokio::test]
    async fn gmail_permission_error_does_not_retry() {
        let g = MockGmail::default();
        g.0.lock().unwrap().errors.insert(
            "a".into(),
            [(403, "ACCESS_TOKEN_SCOPE_INSUFFICIENT".into())].into(),
        );
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(Some(1));
        let e = engine(store, acct, &base);
        assert!(
            e.gmail_source()
                .test_get_json::<Value>(&format!("{base}/messages/a"))
                .await
                .is_err()
        );
        assert_eq!(g.calls("get:a"), 1);
    }

    /// An engine handed a shared budget books its Gmail requests against IT,
    /// not a private one: after one request, another holder of the same
    /// budget has to wait. If `with_gmail_budget` stopped taking effect, the
    /// sweeps engine and the sync loop would each pace alone again (#263).
    #[tokio::test]
    async fn an_engine_books_against_the_budget_it_was_given() {
        let g = MockGmail::default();
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(Some(1));
        // 10 ms a unit; a message read costs 20, so one read books 200 ms.
        let shared = Arc::new(GmailBudget::with_unit(
            crate::config::GmailQuotaProfile::Standard,
            Duration::from_millis(10),
        ));
        let e = engine(store, acct, &base).with_gmail_budget(shared.clone());
        assert!(Arc::ptr_eq(e.gmail_budget(), &shared));
        let _ = e
            .gmail_source()
            .test_get_json::<Value>(&format!("{base}/messages/a"))
            .await;
        let started = tokio::time::Instant::now();
        shared.admit("GET", "/profile").await;
        assert!(started.elapsed() >= Duration::from_millis(150));
    }

    #[tokio::test]
    async fn gmail_requests_are_paced() {
        let g = MockGmail::default();
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(Some(1));
        let mut e = engine(store, acct, &base);
        e.gmail_budget = Arc::new(GmailBudget::with_unit(
            crate::config::GmailQuotaProfile::Standard,
            Duration::from_millis(10),
        ));
        let started = tokio::time::Instant::now();
        let url = format!("{base}/profile");
        let source = e.gmail_source();
        let (a, b) = tokio::join!(
            source.test_get_json::<Value>(&url),
            source.test_get_json::<Value>(&url)
        );
        a.unwrap();
        b.unwrap();
        assert!(started.elapsed() >= Duration::from_millis(10));
    }

    #[tokio::test]
    async fn gmail_shutdown_interrupts_quota_wait_during_first_backfill() {
        let g = MockGmail::default();
        g.listing(LABEL_INBOX, &["a"]);
        g.0.lock()
            .unwrap()
            .errors
            .insert("a".into(), [(403, "RATE_LIMIT_EXCEEDED".into())].into());
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(None);
        let e = engine(store, acct, &base);
        let (stop, rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move { e.run(rx).await });
        tokio::time::timeout(Duration::from_secs(3), async {
            while g.calls("get:a") == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(g.calls("get:a"), 1);
    }

    /// AN IDLE TICK IS ONE REQUEST AND NO WRITE. The INBOX walk's answer
    /// carries the mailbox's current historyId; at the cursor with no record,
    /// nothing anywhere has changed, so the SENT walk, the cursor write and
    /// the unread fetch are all skipped. The first tick fetches the unread
    /// count once because it is owed at boot.
    #[tokio::test]
    async fn an_idle_tick_costs_one_request_and_writes_nothing() {
        let (store, acct) = store_at_cursor(Some(100));
        let g = MockGmail::default();
        g.history(LABEL_INBOX, LabelHistory::quiet(100));
        g.history(LABEL_SENT, LabelHistory::quiet(100));
        g.label(LABEL_INBOX, Some((3, 2)));
        let base = serve_mock(g.clone()).await;
        let eng = engine(store.clone(), acct, &base);
        eng.profile_due.store(i64::MAX, Ordering::Relaxed);

        assert!(!eng.poll_tick().await.unwrap(), "nothing changed");
        assert_eq!(g.calls("history:INBOX"), 1);
        assert_eq!(g.calls("history:SENT"), 0, "it would answer the same");
        assert_eq!(g.calls("label:INBOX"), 1, "owed at boot, fetched once");
        let first = store.inbox_unread(acct).unwrap().expect("stored");
        assert_eq!((first.messages, first.threads), (3, 2));

        for _ in 0..19 {
            assert!(!eng.poll_tick().await.unwrap());
        }
        assert_eq!(g.calls("history:INBOX"), 20, "one request per tick");
        assert_eq!(g.calls("history:SENT"), 0);
        assert_eq!(g.calls("label:INBOX"), 1);
        assert_eq!(cursor_of(&store, acct), Some(100), "not rewritten");
        assert_eq!(
            store.inbox_unread(acct).unwrap().unwrap().fetched_at,
            first.fetched_at,
            "the count was not re-stored"
        );
    }

    /// A CHANGED MAILBOX IS THE OLD TICK: a current historyId past the cursor
    /// with no INBOX record (mail outside INBOX, or read state) walks SENT,
    /// commits the cursor and fetches the unread count after the walk; the
    /// next idle tick leaves all three alone.
    #[tokio::test]
    async fn a_changed_mailbox_walks_sent_commits_the_cursor_and_refreshes_the_count() {
        let (store, acct) = store_at_cursor(Some(100));
        let g = MockGmail::default();
        g.history(LABEL_INBOX, LabelHistory::quiet(120));
        g.history(LABEL_SENT, LabelHistory::quiet(120));
        g.label(LABEL_INBOX, Some((3, 2)));
        let base = serve_mock(g.clone()).await;
        let eng = engine(store.clone(), acct, &base);
        eng.profile_due.store(i64::MAX, Ordering::Relaxed);

        assert!(eng.poll_tick().await.unwrap(), "the mailbox moved");
        assert_eq!(g.calls("history:SENT"), 1);
        assert_eq!(cursor_of(&store, acct), Some(120));
        assert_eq!(
            g.calls("label:INBOX"),
            1,
            "owed at boot and fetched once, not again for the move: {:?}",
            g.seen()
        );

        // Somebody reads mail in Gmail: the id moves, the count changes.
        g.label(LABEL_INBOX, Some((1, 1)));
        g.history(LABEL_INBOX, LabelHistory::quiet(130));
        g.history(LABEL_SENT, LabelHistory::quiet(130));
        assert!(eng.poll_tick().await.unwrap());
        assert_eq!(store.inbox_unread(acct).unwrap().unwrap().messages, 1);
        assert_eq!(cursor_of(&store, acct), Some(130));
        assert_eq!(
            g.seen().last().map(String::as_str),
            Some("label:INBOX"),
            "the count is fetched after the walk: {:?}",
            g.seen()
        );

        assert!(!eng.poll_tick().await.unwrap(), "and then nothing");
        assert_eq!(g.calls("history:SENT"), 2);
        assert_eq!(g.calls("label:INBOX"), 2);
    }

    /// A FAILED UNREAD FETCH STAYS OWED: the next tick tries again whether
    /// or not the mailbox moved, as every tick did before; once it succeeds
    /// an idle tick stops asking.
    #[tokio::test]
    async fn a_failed_unread_fetch_is_retried_on_the_next_idle_tick() {
        let (store, acct) = store_at_cursor(Some(100));
        let g = MockGmail::default();
        g.history(LABEL_INBOX, LabelHistory::quiet(100));
        g.history(LABEL_SENT, LabelHistory::quiet(100));
        // No label scripted: `labels.get` answers 500.
        let base = serve_mock(g.clone()).await;
        let eng = engine(store.clone(), acct, &base);
        eng.profile_due.store(i64::MAX, Ordering::Relaxed);

        assert!(!eng.poll_tick().await.unwrap());
        assert!(!eng.poll_tick().await.unwrap());
        assert_eq!(g.calls("label:INBOX"), 2, "owed until it succeeds");
        assert!(store.inbox_unread(acct).unwrap().is_none());
        g.label(LABEL_INBOX, Some((4, 4)));
        assert!(!eng.poll_tick().await.unwrap());
        assert_eq!(store.inbox_unread(acct).unwrap().unwrap().messages, 4);
        assert!(!eng.poll_tick().await.unwrap());
        assert_eq!(g.calls("label:INBOX"), 3, "paid, and not asked again idle");
    }

    /// THE POLL LANE'S WAIT, as a table: the interval while the cap is the
    /// interval (the default), doubling to the cap otherwise, back to the
    /// interval after a tick that found a change.
    #[test]
    fn the_poll_wait_is_the_interval_unless_a_cap_lets_it_back_off() {
        let s = Duration::from_secs;
        for (idle_rounds, interval, cap, expected, why) in [
            (0, s(5), s(5), s(5), "the interval"),
            (3, s(5), s(5), s(5), "no backoff without a cap above it"),
            (
                2,
                s(5),
                s(3),
                s(5),
                "a cap under the interval is the interval",
            ),
            (0, s(5), s(60), s(5), "a tick that found a change"),
            (
                1,
                s(5),
                s(60),
                s(5),
                "and the first idle tick keep the interval",
            ),
            (2, s(5), s(60), s(10), "the second doubles"),
            (4, s(5), s(60), s(40), "and keeps doubling"),
            (5, s(5), s(60), s(60), "to the cap"),
            (9, s(5), s(60), s(60), "which more ticks cannot pass"),
            (
                8,
                s(5),
                s(600),
                s(600),
                "whatever the cap: the ladder reaches it",
            ),
            (40, s(5), s(600), s(600), "and does not overflow on the way"),
        ] {
            assert_eq!(
                poll_wait(idle_rounds, interval, cap),
                expected,
                "{why}: idle_rounds={idle_rounds} interval={interval:?} cap={cap:?}"
            );
        }
    }

    /// THE LADDER IN THE LANE: with `idle_poll_secs` set, an idle mailbox is
    /// polled at 0, 1, 3 and 7 s (interval 1 s, cap 4 s: the waits are 1, 2,
    /// 4, 4...) where the fixed interval would poll six times in the same
    /// five and a half seconds; a manual refresh polls at once and starts
    /// the ladder over.
    #[tokio::test]
    async fn an_idle_mailbox_backs_the_poll_off_and_a_refresh_starts_it_over() {
        let (store, acct) = store_at_cursor(Some(100));
        let g = MockGmail::default();
        g.history(LABEL_INBOX, LabelHistory::quiet(100));
        g.history(LABEL_SENT, LabelHistory::quiet(100));
        g.label(LABEL_INBOX, Some((0, 0)));
        let base = serve_mock(g.clone()).await;
        let mut config = Config::default();
        config.sync.poll_secs = 1;
        config.sync.idle_poll_secs = 4;
        let eng = Arc::new(engine_with_config(store.clone(), acct, &base, config));
        eng.profile_due.store(i64::MAX, Ordering::Relaxed);
        let (stop, mut rx) = tokio::sync::watch::channel(false);
        let lane = {
            let eng = eng.clone();
            tokio::spawn(async move { eng.poll_lane(&mut rx).await })
        };
        tokio::time::sleep(Duration::from_millis(5500)).await;
        assert_eq!(
            g.calls("history:INBOX"),
            3,
            "ticks at 0, 1 and 3 s; the next is at 7: {:?}",
            g.seen()
        );
        eng.refresh.notify_one();
        wait_for(2, "the refresh tick", || g.calls("history:INBOX") == 4).await;
        assert_eq!(
            g.calls("label:INBOX"),
            2,
            "a refresh fetches the unread count whether or not the walk moved"
        );
        // The ladder restarted: the next tick is one interval on (1 s), not
        // the cap's four.
        wait_for(3, "the tick after the refresh, one interval on", || {
            g.calls("history:INBOX") == 5
        })
        .await;
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), lane)
            .await
            .expect("the lane stops on shutdown")
            .unwrap()
            .unwrap();
    }

    /// A CONNECTED EVENT STREAM KEEPS THE POLL AT ITS INTERVAL: the human
    /// door's count of open streams is a client with the app open, and the
    /// ladder never leaves its first rung while it is nonzero.
    #[tokio::test]
    async fn a_connected_event_stream_keeps_the_poll_at_its_interval() {
        let (store, acct) = store_at_cursor(Some(100));
        let listeners = Arc::new(std::sync::atomic::AtomicUsize::new(1));
        let g = MockGmail::default();
        g.history(LABEL_INBOX, LabelHistory::quiet(100));
        g.history(LABEL_SENT, LabelHistory::quiet(100));
        g.label(LABEL_INBOX, Some((0, 0)));
        let base = serve_mock(g.clone()).await;
        let mut config = Config::default();
        config.sync.poll_secs = 1;
        config.sync.idle_poll_secs = 4;
        let eng = Arc::new(
            engine_with_config(store.clone(), acct, &base, config)
                .with_event_listeners(listeners.clone()),
        );
        eng.profile_due.store(i64::MAX, Ordering::Relaxed);
        let (stop, mut rx) = tokio::sync::watch::channel(false);
        let lane = {
            let eng = eng.clone();
            tokio::spawn(async move { eng.poll_lane(&mut rx).await })
        };
        tokio::time::sleep(Duration::from_millis(4500)).await;
        assert!(
            g.calls("history:INBOX") >= 4,
            "ticks at 0, 1, 2, 3 and 4 s where the ladder would have made three: {:?}",
            g.seen()
        );
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), lane)
            .await
            .expect("the lane stops on shutdown")
            .unwrap()
            .unwrap();
    }

    // ---- the provider seam ----------------------------------------------

    /// THE PROVIDER ROW PICKS THE SOURCE: an ordinary account gets Gmail, and
    /// the choice is made once and kept.
    #[tokio::test]
    async fn a_gmail_account_is_synced_through_the_gmail_source() {
        let (store, acct) = store_at_cursor(Some(100));
        let eng = engine(store, acct, "http://127.0.0.1:9");
        let first = eng.source().unwrap();
        assert_eq!(first.provider(), MailProvider::Gmail);
        assert!(Arc::ptr_eq(&first, &eng.source().unwrap()), "built once");
    }

    /// A throwaway CA certificate (public half only, from the IMAP test bed),
    /// for the tests that need a PEM that parses.
    const TEST_CA_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIDDDCCAfSgAwIBAgIUbWHSoPGtQbOiIqxCtGZYDN70gZYwDQYJKoZIhvcNAQEL
BQAwHjEcMBoGA1UEAwwTc3F1ZWxjaCBpbWFwLWRldiBDQTAeFw0yNjEwMjgyMTM5
MjRaFw0zNjEwMjUyMTM5MjRaMB4xHDAaBgNVBAMME3NxdWVsY2ggaW1hcC1kZXYg
Q0EwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQC6wLzoOtuvRnC1burK
xYdNKXXzyU89h1JS38K1tD4B/jX1wMh+fXuflF6aLgnijDTJVTgB3G7Vk0jHreJ8
qeTx60bMMgn1STnBE9dDHyLSsDkOKRCEoOpPLXfX9jWTRzciUbsq9qz/MVmM9oIW
yr7rx21EB+ruw6+TS3jgqlWcesi8U/90rGSBXYeyFfwnkP8knnsWmZcVnMSUjLTs
wInSpU0QECajVJhVCLL2ZAMAvGOur12BUYk60ZQ8ZKkzOchKdWkh7PhEp/Hzn6GG
yyAqDFVycGVtVx06vY6/Ffcc3GEitT23J8/72VYAFOhJnBJvzv8/8SufbOIR7xH1
YJl5AgMBAAGjQjBAMB0GA1UdDgQWBBTSBZ6WlapKlExzCP1Dssp8TbBZEDAPBgNV
HRMBAf8EBTADAQH/MA4GA1UdDwEB/wQEAwIBBjANBgkqhkiG9w0BAQsFAAOCAQEA
DFuZtyAJMM64nAu9g6QomeNcLwvMUnMWxuNi3tp/wWcd76EP5HoTEQNFQ49IJeps
6lhxzYaXaX1NxOz3txyitaYMFJQ77Ou/M1PTbp0wFQVNjkwRrMr46SDmgTv62DcX
C/qb0J/jO7bK0fWoYxkn0s9/vONV3R4qJVLfpJ3cmuE9S7Rv5EYLlAe738P1W9Ca
hh9GKNWVU/WrLL83hrprpJF2sJmGn4gd10HHO/226pDs8cpgfl6PkFO8aQAPxsiu
ABumuMlx0tOhaEkm2z6hKPb+lGz+12dn7bZjIkPaZ9OeBp7/GwXL9sJDiHLBYzS8
Q0/00yfa39rgtJLa0DnOoA==
-----END CERTIFICATE-----
";

    fn imap_creds_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "squelch-imap-engine-{}-{}.json",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ))
    }

    fn imap_server() -> crate::mail::imap::ImapServer {
        crate::mail::imap::ImapServer {
            host: "127.0.0.1".into(),
            port: 9,
            tls: crate::mail::imap::ImapTls::Plain,
            username: "me@example.com".into(),
        }
    }

    fn file_config(creds: &std::path::Path) -> Config {
        let mut config = Config::default();
        config.credential_backend = crate::config::CredentialBackend::File;
        config.credentials_path = Some(creds.to_path_buf());
        config
    }

    /// AN IMAP ACCOUNT BUILDS AN IMAP SOURCE from what `squelchd auth --imap`
    /// stores: the server, its CA and the password together in the credential
    /// slot, and nothing in the database. Building connects to nothing (the
    /// server here is a dead port).
    #[tokio::test]
    async fn an_imap_account_with_a_stored_server_gets_an_imap_source() {
        let (store, acct) = store_at_cursor(None);
        let creds = imap_creds_path();
        let credential = ImapCredential {
            config: crate::mail::imap::ImapAccountConfig {
                server: imap_server(),
                ca_pem: Some(TEST_CA_PEM.into()),
                smtp: None,
            },
            password: crate::mail::imap::ImapPassword::new("pw"),
            smtp_password: None,
        };
        crate::credentials::store_imap_slot_backend(
            crate::config::CredentialBackend::File,
            &creds,
            "me@example.com",
            &credential.to_slot().unwrap(),
        )
        .unwrap();
        store
            .set_account_provider(acct, MailProvider::Imap)
            .unwrap();
        let eng = engine_with_config(store, acct, "http://127.0.0.1:9", file_config(&creds));
        assert_eq!(eng.source().unwrap().provider(), MailProvider::Imap);
        let _ = std::fs::remove_file(&creds);
    }

    /// THE SERVER CONFIG LEAVES THE DATABASE (security review 5): a slot an
    /// earlier build wrote holds the password alone, with the server (and a
    /// CA by path) in `app_settings`. Building the source moves both into the
    /// slot, the CA as its certificate bytes, and deletes the setting, so a
    /// later write to the database or to the CA's path cannot redirect where
    /// the password goes.
    #[tokio::test]
    async fn a_legacy_imap_account_moves_its_server_into_the_credential_slot() {
        let (store, acct) = store_at_cursor(None);
        let creds = imap_creds_path();
        // The legacy layout: a bare password in the slot.
        crate::credentials::store_imap_slot_backend(
            crate::config::CredentialBackend::File,
            &creds,
            "me@example.com",
            "pw",
        )
        .unwrap();
        let ca_path = creds.with_extension("ca.crt");
        std::fs::write(&ca_path, TEST_CA_PEM).unwrap();
        let legacy = serde_json::json!({
            "host": "127.0.0.1", "port": 9, "tls": "plain",
            "username": "me@example.com", "ca_file": ca_path,
        });
        store
            .set_app_setting(acct, IMAP_ACCOUNT_SETTING, &legacy.to_string())
            .unwrap();
        store
            .set_account_provider(acct, MailProvider::Imap)
            .unwrap();
        let eng = engine_with_config(
            store.clone(),
            acct,
            "http://127.0.0.1:9",
            file_config(&creds),
        );
        assert_eq!(eng.source().unwrap().provider(), MailProvider::Imap);

        assert_eq!(
            store.get_app_setting(acct, IMAP_ACCOUNT_SETTING).unwrap(),
            None
        );
        // The CA file can go: its bytes are in the slot now.
        std::fs::remove_file(&ca_path).unwrap();
        let text = crate::credentials::load_imap_slot_backend(
            crate::config::CredentialBackend::File,
            &creds,
            "me@example.com",
        )
        .unwrap();
        match ImapCredential::from_slot(&text) {
            StoredImapSlot::Credential(c) => {
                assert_eq!(c.config.server, imap_server());
                assert_eq!(c.config.ca_pem.as_deref(), Some(TEST_CA_PEM));
            }
            StoredImapSlot::LegacyPassword(_) => panic!("not migrated"),
        }
        // And a second engine builds from the slot alone.
        let eng = engine_with_config(store, acct, "http://127.0.0.1:9", file_config(&creds));
        assert_eq!(eng.source().unwrap().provider(), MailProvider::Imap);
        let _ = std::fs::remove_file(&creds);
    }

    /// AN IMAP ACCOUNT WITH NOTHING STORED IS REFUSED: the provider row says
    /// imap but `squelchd auth --imap` never ran, so the engine refuses it,
    /// never as Gmail.
    #[tokio::test]
    async fn an_imap_account_without_a_stored_server_is_refused() {
        let (store, acct) = store_at_cursor(None);
        store.execute_batch_for_test(&format!(
            "UPDATE accounts SET provider = 'imap' WHERE id = {acct}"
        ));
        let creds = imap_creds_path();
        let eng = engine_with_config(store, acct, "http://127.0.0.1:9", file_config(&creds));
        let err = eng.source().err().expect("refused");
        assert!(err.to_string().contains("squelchd auth --imap"), "{err}");
    }

    /// AN ACCOUNT THIS BUILD CANNOT SYNC IS NEVER SYNCED AS GMAIL. A database
    /// a newer build set up an IMAP account in must cost that account nothing
    /// here: no request to Gmail with its credential, no cursor, no rows; every
    /// entry point says why.
    #[tokio::test]
    async fn an_account_whose_provider_this_build_does_not_know_never_reaches_gmail() {
        let (store, acct) = store_at_cursor(None);
        store.execute_batch_for_test(&format!(
            "UPDATE accounts SET provider = 'exchange-ews' WHERE id = {acct}"
        ));
        let g = MockGmail::default();
        g.listing(LABEL_INBOX, &["a"])
            .body("a", sent_eml(Utc::now(), "x@y.com", "s"));
        g.profile(100);
        let base = serve_mock(g.clone()).await;
        let eng = engine(store.clone(), acct, &base);
        let (_stop, mut rx) = tokio::sync::watch::channel(false);
        let refused = |r: Result<()>| matches!(r, Err(CoreError::InvalidInput(_)));
        assert!(refused(eng.run_once(&mut rx).await));
        assert!(refused(eng.poll_once().await.map(|_| ())));
        assert!(refused(eng.harvest_sent_contacts().await));
        assert!(refused(eng.backfill_recipients().await));
        assert!(refused(eng.heal_blank_bodies().await));
        assert!(refused(eng.sync_spam_window().await.map(|_| ())));
        eng.refresh_inbox_unread().await;
        assert!(g.seen().is_empty(), "nothing reached Gmail: {:?}", g.seen());
        assert!(store.sync_cursor(acct).unwrap().is_none());
        assert!(store.inbox_unread(acct).unwrap().is_none());
    }

    /// THE CURSOR IS COMMITTED OPAQUE, and mirrored for a rolled-back build:
    /// the engine's write lands in `sync_cursor` and in the legacy row both.
    #[tokio::test]
    async fn a_walk_commits_its_cursor_to_the_cursor_table_and_the_legacy_row() {
        let (store, acct) = store_at_cursor(Some(100));
        let g = MockGmail::default();
        g.history(LABEL_INBOX, LabelHistory::quiet(120));
        g.history(LABEL_SENT, LabelHistory::quiet(120));
        g.label(LABEL_INBOX, Some((0, 0)));
        let base = serve_mock(g.clone()).await;
        let eng = engine(store.clone(), acct, &base);
        eng.profile_due.store(i64::MAX, Ordering::Relaxed);
        assert!(eng.poll_tick().await.unwrap());
        assert_eq!(
            store
                .sync_cursor(acct)
                .unwrap()
                .map(|c| c.as_str().to_string()),
            Some("120".to_string())
        );
        assert_eq!(cursor_of(&store, acct), Some(120));
    }

    /// A source that can be told about changes, over the real Gmail one: its
    /// wait returns at once `early` times, then sleeps like Gmail's.
    struct Pushy {
        inner: GmailSource<FixedToken>,
        early: std::sync::atomic::AtomicUsize,
        waits: Mutex<Vec<(Option<String>, Duration)>>,
    }

    #[async_trait]
    impl MailSource for Pushy {
        fn provider(&self) -> MailProvider {
            self.inner.provider()
        }
        async fn list(
            &self,
            listing: Listing,
            cap: Option<usize>,
            urgency: Urgency,
        ) -> Result<Vec<String>> {
            self.inner.list(listing, cap, urgency).await
        }
        async fn current_cursor(&self) -> Result<SyncCursor> {
            self.inner.current_cursor().await
        }
        fn cursor_is_usable(&self, cursor: &SyncCursor) -> bool {
            self.inner.cursor_is_usable(cursor)
        }
        async fn changes(
            &self,
            since: &SyncCursor,
            folder: Folder,
        ) -> std::result::Result<Changes, ChangesError> {
            self.inner.changes(since, folder).await
        }
        fn merge_cursors(&self, since: &SyncCursor, a: &SyncCursor, b: &SyncCursor) -> SyncCursor {
            self.inner.merge_cursors(since, a, b)
        }
        async fn fetch_raw(&self, id: &str, urgency: Urgency) -> Result<FetchedMessage> {
            self.inner.fetch_raw(id, urgency).await
        }
        async fn fetch_headers(
            &self,
            id: &str,
            names: &[&str],
        ) -> Result<crate::mail::FetchedHeaders> {
            self.inner.fetch_headers(id, names).await
        }
        async fn inbox_unread(&self) -> Result<(i64, i64)> {
            self.inner.inbox_unread().await
        }
        async fn wait_for_change(&self, cursor: Option<&SyncCursor>, max_wait: Duration) {
            self.waits
                .lock()
                .unwrap()
                .push((cursor.map(|c| c.as_str().to_string()), max_wait));
            let early = self
                .early
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
                .is_ok();
            if !early {
                tokio::time::sleep(max_wait).await;
            }
        }
    }

    /// THE POLL LANE PARKS IN THE SOURCE'S WAIT, handed the cursor it last
    /// committed and the interval it would have slept, so a source that is
    /// told about a change (IMAP IDLE) ticks again at once rather than at the
    /// next interval. With an hour's interval, three ticks in a moment can
    /// only have come from the wait returning early.
    #[tokio::test]
    async fn the_poll_lane_waits_in_the_source_with_the_cursor_it_committed() {
        let (store, acct) = store_at_cursor(Some(100));
        let g = MockGmail::default();
        g.history(LABEL_INBOX, LabelHistory::quiet(120));
        g.history(LABEL_SENT, LabelHistory::quiet(120));
        g.label(LABEL_INBOX, Some((0, 0)));
        let base = serve_mock(g.clone()).await;
        let mut config = Config::default();
        config.sync.poll_secs = 3600;
        let plain = engine_with_config(store.clone(), acct, &base, config.clone());
        let pushy = Arc::new(Pushy {
            inner: plain.gmail_source(),
            early: std::sync::atomic::AtomicUsize::new(2),
            waits: Mutex::new(Vec::new()),
        });
        let eng = Arc::new(
            engine_with_config(store.clone(), acct, &base, config).with_source(pushy.clone()),
        );
        eng.profile_due.store(i64::MAX, Ordering::Relaxed);
        let (stop, mut rx) = tokio::sync::watch::channel(false);
        let lane = {
            let eng = eng.clone();
            tokio::spawn(async move { eng.poll_lane(&mut rx).await })
        };
        wait_for(5, "three ticks", || g.calls("history:INBOX") == 3).await;
        wait_for(5, "the third park", || {
            pushy.waits.lock().unwrap().len() == 3
        })
        .await;
        let hour = Duration::from_secs(3600);
        assert_eq!(
            *pushy.waits.lock().unwrap(),
            vec![
                (Some("120".to_string()), hour),
                (Some("120".to_string()), hour),
                (Some("120".to_string()), hour),
            ],
            "the committed cursor and the interval, every park"
        );
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), lane)
            .await
            .expect("shutdown interrupts the source's wait")
            .unwrap()
            .unwrap();
        assert_eq!(g.calls("history:INBOX"), 3, "and no tick without a wake");
    }

    /// IDLE COST OF THE POLL LANE, ignored by default: twenty ticks of the
    /// lane's body over a mailbox where nothing changes, on a FILE-backed
    /// store against the mock Gmail, printing the requests per tick by
    /// endpoint and the process CPU. Run it under
    /// `strace -f -c -e trace=fsync,fdatasync,pwrite64` on the test binary to
    /// count what those ticks wrote; `SQUELCH_BENCH_SKIP=1` runs the setup
    /// alone so the two counts can be subtracted.
    ///
    /// ```text
    /// cargo test -p squelch-core --lib sync::tests::poll_lane_idle_cost -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore]
    async fn poll_lane_idle_cost() {
        let dir = std::env::temp_dir().join(format!("squelch-poll-idle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(SqliteStore::open(dir.join("store.sqlite3")).unwrap());
        let acct = store.ensure_account("me@example.com").unwrap();
        store
            .set_sync_state(
                acct,
                HISTORY_KEY,
                &SyncState {
                    uidvalidity: 0,
                    last_uid: 100,
                },
            )
            .unwrap();
        let g = MockGmail::default();
        g.history(LABEL_INBOX, LabelHistory::quiet(100));
        g.history(LABEL_SENT, LabelHistory::quiet(100));
        g.label(LABEL_INBOX, Some((3, 2)));
        let base = serve_mock(g.clone()).await;
        let eng = engine(store.clone(), acct, &base);
        // The daily profile refresh is not the idle tick's cost: park it.
        eng.profile_due.store(i64::MAX, Ordering::Relaxed);
        if std::env::var_os("SQUELCH_BENCH_SKIP").is_some() {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        // CPU consumed by the whole process so far, user and system.
        let process_cpu = || {
            let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
            // SAFETY: `usage` is a valid, writable rusage for the call's duration.
            let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
            assert_eq!(rc, 0, "getrusage(RUSAGE_SELF)");
            let secs = |tv: libc::timeval| {
                Duration::from_secs(tv.tv_sec as u64) + Duration::from_micros(tv.tv_usec as u64)
            };
            secs(usage.ru_utime) + secs(usage.ru_stime)
        };
        const TICKS: usize = 20;
        let cpu_start = process_cpu();
        let start = std::time::Instant::now();
        for _ in 0..TICKS {
            eng.poll_tick().await.unwrap();
        }
        let elapsed = start.elapsed();
        let cpu = process_cpu() - cpu_start;
        let mut by_key: std::collections::BTreeMap<String, usize> = Default::default();
        for call in g.seen() {
            *by_key.entry(call).or_default() += 1;
        }
        eprintln!(
            "BENCH poll_lane_idle: {TICKS} ticks in {elapsed:?}, {cpu:?} process cpu; requests per tick {:.2}; by endpoint {by_key:?}",
            g.seen().len() as f64 / TICKS as f64
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE POKE REACHES THE LANE. Idle for a hundred-odd seconds of paused
    /// time, the notification lane has backed off to its five-second sleep,
    /// four seconds of which remain. An arrival's `notification` job is then
    /// queued and the wake poked (by hand here; the producers' pokes are
    /// [`an_incremental_arrival_pokes_the_notification_lane_but_a_backfill_does_not`]
    /// and the applied-commit assertion in
    /// [`agent_worker_commits_model_decision_without_legacy_placement`]),
    /// and the job is claimed and run inside the next few milliseconds of
    /// the clock: the poke ended the sleep, the timer did not.
    ///
    /// MUTATION: dropping the `notified()` arm from the lane's `select!`
    /// leaves the job for the timer, four seconds on, and the elapsed-time
    /// assertion fails.
    #[tokio::test(start_paused = true)]
    async fn a_poke_claims_a_queued_notification_inside_the_backoff() {
        let (store, acct) = store_at_cursor(None);
        let eng = Arc::new(engine(store.clone(), acct, "http://127.0.0.1:9"));
        let (stop, mut rx) = tokio::sync::watch::channel(false);
        let lane = {
            let eng = eng.clone();
            tokio::spawn(async move { eng.agent_notification_lane(&mut rx).await })
        };
        // Rounds at 0, 2, 6, then every five seconds (11, 16, ... 101) of
        // the paused clock: the next timer is at 106 s.
        tokio::time::sleep(Duration::from_secs(102)).await;
        use crate::store::agent_triage::AgentTriageStore;
        let kinds = ["notification", "deliberate_notification"];
        assert_eq!(store.next_agent_jobs_due(acct, &kinds).unwrap(), []);
        // A fresh incremental arrival queues its push job inside the ingest.
        let raw = "From: friend@example.com\r\nTo: me@example.com\r\nSubject: now\r\n\r\nhello\r\n";
        eng.ingest_one(
            &fixture(acct, "g-now", raw, false),
            &[],
            Utc::now(),
            IngestOrigin::Incremental,
        )
        .unwrap()
        .expect("ingested");
        let queued = store.next_agent_jobs_due(acct, &kinds).unwrap();
        assert_eq!(queued.len(), 1, "the arrival queued a notification job");
        assert_eq!(queued[0].0, "notification");
        assert!(queued[0].1 <= Utc::now(), "due now");
        let poked_at = tokio::time::Instant::now();
        eng.notify_wake.notify_one();
        // Claimed and run (no model configured: recorded unavailable and
        // completed) in the same poll, so the queue reads empty again.
        wait_for(5, "the lane to claim and complete the job", || {
            store.next_agent_jobs_due(acct, &kinds).unwrap().is_empty()
        })
        .await;
        let waited = poked_at.elapsed();
        assert!(
            waited < Duration::from_secs(1),
            "claimed on the poke, not the timer: {waited:?}"
        );
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), lane)
            .await
            .expect("the lane stops on shutdown")
            .unwrap();
    }

    /// IDLE COST OF THE NOTIFICATION LANE, ignored by default: ten seconds of
    /// the lane over an empty queue on a FILE-backed store, printing the
    /// process CPU it burned. Run it under
    /// `strace -f -c -e trace=fcntl,pread64,pwrite64,fdatasync` on the test
    /// binary to count what those seconds asked the kernel for:
    ///
    /// ```text
    /// cargo test -p squelch-core --lib sync::tests::notification_lane_idle_cost -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore]
    async fn notification_lane_idle_cost() {
        let dir = std::env::temp_dir().join(format!("squelch-notify-idle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(SqliteStore::open(dir.join("store.sqlite3")).unwrap());
        let acct = store.ensure_account("me@example.com").unwrap();
        let eng = engine(store, acct, "http://127.0.0.1:9");
        let (stop, mut rx) = tokio::sync::watch::channel(false);
        // `SQUELCH_BENCH_SKIP=1` runs the setup alone, so a syscall count of
        // the two runs differs by the lane's own.
        if std::env::var_os("SQUELCH_BENCH_SKIP").is_some() {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let cpu_start = process_cpu();
        let start = std::time::Instant::now();
        let lane = eng.agent_notification_lane(&mut rx);
        tokio::pin!(lane);
        tokio::select! {
            _ = &mut lane => {}
            _ = tokio::time::sleep(Duration::from_secs(10)) => {}
        }
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), lane)
            .await
            .expect("the lane stops on shutdown");
        eprintln!(
            "BENCH notification_lane_idle: {:?} wall, {:?} process cpu",
            start.elapsed(),
            process_cpu() - cpu_start
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// CPU consumed by the whole process so far, user and system.
    fn process_cpu() -> Duration {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: `usage` is a valid, writable rusage for the call's duration.
        let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
        assert_eq!(rc, 0, "getrusage(RUSAGE_SELF)");
        let secs = |tv: libc::timeval| {
            Duration::from_secs(tv.tv_sec as u64) + Duration::from_micros(tv.tv_usec as u64)
        };
        secs(usage.ru_utime) + secs(usage.ru_stime)
    }

    /// Poll `cond` every 10ms until it holds, or panic after `secs`.
    async fn wait_for(secs: u64, what: &str, cond: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(secs), async {
            while !cond() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
    }

    /// THE OUTAGE ENDS WHEN GMAIL ANSWERS. A failing poll grows the streak and
    /// the backoff; the first successful poll must zero the streak, clear the
    /// error and the scheduled retry, and the NEXT failure must start the
    /// backoff over at `BACKOFF_START` rather than where the old outage left it.
    #[tokio::test]
    async fn a_successful_poll_resets_the_failure_streak_and_the_backoff() {
        let g = MockGmail::default();
        g.history(LABEL_INBOX, LabelHistory::broken());
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(Some(1));
        let mut config = Config::default();
        config.sync.poll_secs = 1;
        let metrics = SyncMetrics::new();
        let e = engine_with_config(store, acct, &base, config).with_metrics(metrics.clone());
        let (stop, rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move { e.run(rx).await });

        // Two failed lifecycles: the second waits twice as long as the first.
        wait_for(10, "two failures", || {
            metrics.sync_consecutive_failures() >= 2
        })
        .await;
        assert!(!metrics.poll_ok());
        let error = metrics.sync_last_error().expect("the failure is recorded");
        assert_eq!(error.kind, GmailErrorKind::Http);
        let retry_in =
            metrics.sync_next_retry_at().expect("a retry is scheduled") - Utc::now().timestamp();
        assert!(retry_in >= 3, "the backoff doubled to 4s, got {retry_in}s");

        // Gmail recovers.
        g.history(LABEL_INBOX, LabelHistory::quiet(1));
        wait_for(10, "a healthy poll", || metrics.poll_ok()).await;
        assert_eq!(metrics.sync_consecutive_failures(), 0, "the streak is over");
        assert_eq!(metrics.sync_last_error(), None);
        assert_eq!(metrics.sync_next_retry_at(), None);

        // And breaks again: the wait starts over, it does not resume at 8s.
        g.history(LABEL_INBOX, LabelHistory::broken());
        wait_for(10, "the next failure", || !metrics.poll_ok()).await;
        wait_for(2, "the retry to be scheduled", || {
            metrics.sync_next_retry_at().is_some()
        })
        .await;
        let retry_in = metrics.sync_next_retry_at().unwrap() - Utc::now().timestamp();
        assert!(
            retry_in <= BACKOFF_START.as_secs() as i64,
            "backoff restarts after a healthy poll, got {retry_in}s"
        );
        assert_eq!(metrics.sync_consecutive_failures(), 1);

        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    /// THE FIRST RUN READS AS PROGRESS. Onboarding's backfill runs for minutes
    /// before the first poll; it must report a done/total pair and must not
    /// read as a failing sync while it does.
    #[tokio::test]
    async fn the_first_backfill_reports_progress_not_an_outage() {
        let g = MockGmail::default();
        g.listing(LABEL_INBOX, &["a", "b"]);
        g.body("a", alert_eml(Utc::now()));
        g.body("b", alert_eml(Utc::now()));
        g.slow_body("b", Duration::from_secs(2));
        g.history(LABEL_INBOX, LabelHistory::quiet(1));
        g.history(LABEL_SENT, LabelHistory::quiet(1));
        g.profile(1);
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(None);
        let metrics = SyncMetrics::new();
        let e = engine(store, acct, &base).with_metrics(metrics.clone());
        let (stop, rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move { e.run(rx).await });

        wait_for(5, "the backfill to fetch the first message", || {
            metrics.catchup_progress() == Some((1, 2))
        })
        .await;
        assert!(!metrics.poll_ok(), "no poll has run yet");
        assert!(!metrics.sync_failing(), "and that is not an outage");

        wait_for(10, "the first poll", || metrics.poll_ok()).await;
        assert_eq!(metrics.catchup_progress(), None, "the pair clears after");

        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    /// A RETRY IN PROGRESS HAS NO "NEXT TRY". After a failed lifecycle the
    /// scheduled retry time must clear once the retry starts, so a long
    /// recovery (here, the backfill running again) does not keep reporting a
    /// retry time that has passed.
    #[tokio::test]
    async fn the_scheduled_retry_clears_when_the_retry_starts() {
        let g = MockGmail::default();
        g.listing(LABEL_INBOX, &["a", "b"]);
        g.body("a", alert_eml(Utc::now()));
        g.body("b", alert_eml(Utc::now()));
        // The first lifecycle dies on `a`; the retry stalls on `b`.
        g.0.lock()
            .unwrap()
            .errors
            .insert("a".into(), [(500, "backendError".into())].into());
        g.slow_body("b", Duration::from_secs(3));
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(None);
        let metrics = SyncMetrics::new();
        let e = engine(store, acct, &base).with_metrics(metrics.clone());
        let (stop, rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move { e.run(rx).await });

        wait_for(5, "the first failure", || metrics.sync_failing()).await;
        wait_for(5, "the retry to reach `b`", || g.calls("get:a") >= 2).await;
        wait_for(2, "the retry's progress", || {
            metrics.catchup_progress() == Some((1, 2))
        })
        .await;
        assert_eq!(
            metrics.sync_next_retry_at(),
            None,
            "the retry is running, it is not scheduled"
        );
        assert!(metrics.sync_failing(), "still failing until a poll lands");

        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[test]
    fn lifecycle_backoff_restarts_only_after_a_healthy_poll() {
        assert_eq!(lifecycle_backoff(BACKOFF_CAP, true), BACKOFF_START);
        assert_eq!(lifecycle_backoff(BACKOFF_CAP, false), BACKOFF_CAP);
    }

    #[test]
    fn gmail_retry_after_accepts_seconds_and_http_dates() {
        assert_eq!(gmail_retry_after("90"), Some(Duration::from_secs(90)));
        assert_eq!(
            gmail_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some(Duration::ZERO)
        );
        assert_eq!(gmail_retry_after("invalid"), None);
    }

    // ---- the spam folder is fetched ON DEMAND, never on the poll loop -------

    /// THE POINT OF THE WHOLE ARRANGEMENT: an ordinary poll tick must not so
    /// much as ASK about the SPAM label. Asserted against the mock's call log
    /// rather than against what landed, because "no spam rows appeared" would
    /// also pass if the walk ran and the folder happened to be empty — and the
    /// cost this avoids is the request, not the row.
    #[tokio::test]
    async fn a_poll_tick_never_touches_the_spam_label() {
        let (store, acct) = store_at_cursor(Some(100));
        let g = MockGmail::default();
        // The mailbox's id has moved (the spam arrival moved it), so the
        // walk runs in full rather than stopping at the idle check.
        g.history(LABEL_INBOX, LabelHistory::quiet(140));
        g.history(LABEL_SENT, LabelHistory::quiet(140));
        // Scripted so a walk WOULD find something, which is what makes the
        // absence of the call meaningful.
        g.history(LABEL_SPAM, LabelHistory::added(140, &[(140, "g-spam")]));
        g.body("g-spam", spam_eml(Utc::now(), "you have won"));
        let base = serve_mock(g.clone()).await;

        engine(store.clone(), acct, &base)
            .poll_once()
            .await
            .unwrap();

        assert!(
            !g.seen().iter().any(|c| c.contains(LABEL_SPAM)),
            "a poll tick asked about SPAM: {:?}",
            g.seen()
        );
    }

    /// And a catch-up — the expensive whole-window re-listing — does not either.
    #[tokio::test]
    async fn a_catch_up_never_lists_the_spam_label() {
        let (store, acct) = store_at_cursor(None);
        let g = MockGmail::default();
        g.listing(LABEL_INBOX, &[]);
        g.listing(LABEL_SENT, &[]);
        g.listing(LABEL_SPAM, &["g-spam"]);
        g.body("g-spam", spam_eml(Utc::now(), "you have won"));
        g.profile(500);
        let base = serve_mock(g.clone()).await;

        engine(store.clone(), acct, &base)
            .poll_once()
            .await
            .unwrap();

        assert!(
            !g.seen().iter().any(|c| c.contains(LABEL_SPAM)),
            "a catch-up listed SPAM: {:?}",
            g.seen()
        );
    }

    /// Asked for explicitly, it fetches — and the row lands exactly as the old
    /// label walk left it: flagged spam, neutral, out of both LLM queues.
    #[tokio::test]
    async fn an_on_demand_spam_sync_fetches_and_lands_neutral() {
        let (store, acct) = store_at_cursor(Some(100));
        let g = MockGmail::default();
        g.listing(LABEL_SPAM, &["g-spam"]);
        g.body("g-spam", spam_eml(Utc::now(), "you have won"));
        let base = serve_mock(g.clone()).await;

        let n = engine(store.clone(), acct, &base)
            .sync_spam_window()
            .await
            .unwrap();
        assert_eq!(n, 1);

        // It is spam, and it is invisible to the ordinary mailbox.
        let inbox = listed(&store, acct, Utc::now());
        assert!(inbox.is_empty(), "spam must not reach the mailbox listing");

        let spam = inventory(
            &store,
            acct,
            Utc::now() - ChronoDuration::days(1),
            SpamScope::Only,
        );
        assert_eq!(spam.len(), 1, "and it must reach the spam page");

        // Never triaged: the same 'n/a' markers the sent branch gets.
        let row = store
            .triage_debug(acct, spam[0].update.id)
            .unwrap()
            .expect("triage row");
        assert_eq!(row.tier, "noise");
        assert_eq!(row.importance, 0);
        assert_eq!(row.stage1_model_used.as_deref(), Some("n/a"));

        // And the completion stamp is written, which is what lets the page say
        // "we looked" rather than guessing.
        assert!(
            store
                .get_app_setting(acct, SPAM_SYNCED_AT_KEY)
                .unwrap()
                .is_some()
        );
    }

    /// THE STAMP IS WRITTEN ONLY ON SUCCESS. A page that read a stamp moved by a
    /// failed fetch would tell somebody their provider filtered nothing when
    /// nobody managed to look.
    #[tokio::test]
    async fn a_failed_spam_sync_leaves_no_stamp() {
        let (store, acct) = store_at_cursor(Some(100));
        // No listing scripted for SPAM and a mock that fails unscripted keys.
        let dead = "http://127.0.0.1:1".to_string();
        let out = engine(store.clone(), acct, &dead).sync_spam_window().await;
        assert!(out.is_err());
        assert!(
            store
                .get_app_setting(acct, SPAM_SYNCED_AT_KEY)
                .unwrap()
                .is_none(),
            "a failed fetch must not look like a completed one"
        );
    }

    /// The cap is a bound on how long one click can take: `spam_max` messages,
    /// and pagination stops once it is reached rather than walking the folder.
    #[tokio::test]
    async fn the_spam_sync_stops_at_the_cap() {
        let (store, acct) = store_at_cursor(Some(100));
        let g = MockGmail::default();
        let ids: Vec<String> = (0..10).map(|i| format!("g-spam-{i}")).collect();
        let refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        g.listing(LABEL_SPAM, &refs);
        for id in &ids {
            g.body(id, spam_eml(Utc::now(), "you have won"));
        }
        let base = serve_mock(g.clone()).await;

        let mut config = Config::default();
        config.sync.spam_max = 3;
        let eng = SyncEngine::new(
            store.clone(),
            Arc::new(FixedToken),
            acct,
            "me@example.com".to_string(),
            config,
        )
        .with_api_base(base.clone());

        assert_eq!(eng.sync_spam_window().await.unwrap(), 3);
        let fetched = g.seen().iter().filter(|c| c.starts_with("get:")).count();
        assert_eq!(fetched, 3, "the cap bounds the FETCHES, not just the rows");
    }

    #[tokio::test]
    async fn a_sent_history_record_ingests_neutral_and_silent() {
        // Sent from the phone: SENT only, never INBOX. Before the sent walk this
        // message simply did not exist locally until the next full backfill.
        let (store, acct) = store_at_cursor(Some(100));
        let now = Utc::now();

        let g = MockGmail::default();
        // The mailbox's current historyId is at least the SENT record's: an
        // INBOX walk that reports it still at the cursor would say nothing
        // anywhere changed, and the SENT walk would rightly be skipped.
        g.history(LABEL_INBOX, LabelHistory::quiet(140));
        g.history(LABEL_SENT, LabelHistory::added(140, &[(140, "g-phone")]));
        g.body(
            "g-phone",
            sent_eml(now, "alice@friends.com", "lunch thursday"),
        );
        let base = serve_mock(g.clone()).await;

        engine(store.clone(), acct, &base)
            .poll_once()
            .await
            .unwrap();

        // The row landed...
        let view = store.thread_view_with_html(acct, "g-phone").unwrap();
        assert_eq!(view.messages.len(), 1);
        let mid = view.messages[0].id;

        // ...NEUTRAL, and out of both LLM queues: no triage spend on the user's
        // own writing, ever.
        let row = store.triage_debug(acct, mid).unwrap().expect("triage row");
        assert_eq!(row.tier, "noise");
        assert_eq!(row.importance, 0);
        assert_eq!(row.stage1_model_used.as_deref(), Some("n/a"));
        assert!(!row.needs_stage2);

        // THE SILENCE INVARIANT: never a push for mail the user sent themselves.
        assert!(store.events_after(acct, 0, 100).unwrap().is_empty());
        assert_eq!(store.latest_event_id(acct).unwrap(), 0);

        // And it stays out of the bands and out of search, as sent mail must.
        assert!(listed(&store, acct, now).is_empty());
        assert!(store.search(acct, "lunch", 10, 0).unwrap().is_empty());

        // The recipients still seed contacts, which is half the point of having
        // the mail at all.
        assert!(store.is_known_contact(acct, "alice@friends.com").unwrap());
        assert_eq!(cursor_of(&store, acct), Some(140));
    }

    /// A row exactly as an install predating the recipient columns left it:
    /// recipients NULL, triage row present. `is_sent` picks which mail it is.
    fn old_row(store: &SqliteStore, acct: AccountId, gmail: &str, is_sent: bool) -> i64 {
        let id = store
            .upsert_message(&crate::types::NewMessage {
                account_id: acct,
                gmail_msg_id: gmail.to_string(),
                thread_id: format!("t-{gmail}"),
                from_addr: "me@example.com".to_string(),
                from_name: Some("Me".to_string()),
                subject: "lunch thursday".to_string(),
                received_at: Utc::now(),
                snippet: String::new(),
                body: "writing this from the phone app".to_string(),
                body_html: None,
                is_sent,
                is_spam: false,
                to_addrs: None,
                cc_addrs: None,
                bcc_addrs: None,
                list_unsubscribe: None,
                list_unsub_one_click: false,
                auth_pass: None,
                sender_signals: None,
            })
            .unwrap();
        store
            .set_triage(
                id,
                acct,
                0,
                Tier::Noise,
                crate::types::Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
        id
    }

    #[tokio::test]
    async fn the_recipients_backfill_fills_old_rows_exactly_once() {
        // Mail ingested before the recipient columns existed has nobody to show.
        // The one-shot backfill fetches headers only and fills them, then its
        // sync_state flag keeps it from ever paying for that walk again.
        let (store, acct) = store_at_cursor(Some(100));
        let id = old_row(&store, acct, "g-old", true);

        let g = MockGmail::default();
        g.body(
            "g-old",
            sent_eml(Utc::now(), "Alice <alice@friends.com>", "lunch thursday"),
        );
        let base = serve_mock(g.clone()).await;
        let eng = engine(store.clone(), acct, &base);

        eng.backfill_recipients().await.unwrap();
        let listed = store.sent_listing(acct, 10, 0).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        assert_eq!(listed[0].to, "Alice <alice@friends.com>");
        assert!(
            store
                .messages_missing_recipients(acct, 10)
                .unwrap()
                .is_empty()
        );

        // Second run: the flag is set, so not one more Gmail call.
        let calls = g.calls("get:g-old");
        eng.backfill_recipients().await.unwrap();
        assert_eq!(
            g.calls("get:g-old"),
            calls,
            "the sweep runs once per install"
        );
    }

    /// THE SWEEP WALKS RECEIVED MAIL TOO, splits the headers, and refuses to
    /// keep the one header inbound mail must never keep.
    #[tokio::test]
    async fn the_recipients_backfill_splits_headers_and_drops_an_inbound_bcc() {
        let (store, acct) = store_at_cursor(Some(100));
        let id = old_row(&store, acct, "g-in", false);

        let g = MockGmail::default();
        g.body(
            "g-in",
            format!(
                "From: alice@friends.com\r\n\
                 To: me@example.com, \"Doe, Jane\" <jane@friends.com>\r\n\
                 Cc: team@friends.com\r\n\
                 Bcc: leaked@friends.com\r\n\
                 Subject: lunch thursday\r\n\
                 Date: {}\r\n\
                 \r\n\
                 body\r\n",
                Utc::now().to_rfc2822()
            ),
        );
        let base = serve_mock(g.clone()).await;
        engine(store.clone(), acct, &base)
            .backfill_recipients()
            .await
            .unwrap();

        let m = store.thread_view_with_html(acct, "t-g-in").unwrap();
        let m = &m.messages[0];
        assert_eq!(m.id, id);
        assert_eq!(
            m.to_addrs.as_deref(),
            Some("me@example.com, \"Doe, Jane\" <jane@friends.com>"),
            "a comma inside a display name is quoted, not a second recipient"
        );
        assert_eq!(m.cc_addrs.as_deref(), Some("team@friends.com"));
        assert_eq!(
            m.bcc_addrs, None,
            "a Bcc header on inbound mail is the sender's leak or a forgery"
        );
    }

    #[tokio::test]
    async fn the_recipients_backfill_records_a_message_gmail_no_longer_has() {
        // A 404 is an answer, not a failure: the row is marked "looked, nobody
        // named" so it leaves the queue. Left NULL it would be handed back by the
        // very next batch query, and the pass would never terminate.
        let (store, acct) = store_at_cursor(Some(100));
        old_row(&store, acct, "g-gone", true);

        let base = serve_mock(MockGmail::default()).await;
        engine(store.clone(), acct, &base)
            .backfill_recipients()
            .await
            .unwrap();

        assert!(
            store
                .messages_missing_recipients(acct, 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.sent_listing(acct, 10, 0).unwrap()[0].to, "");
    }

    #[tokio::test]
    async fn a_failed_recipients_backfill_leaves_the_flag_unset() {
        // A non-404 error SKIPS the message (its row stays NULL) and the pass
        // finishes Ok — one message with a persistent 4xx quirk must not abort
        // the sweep for everything behind it. But the done flag is only set on
        // a clean pass, so the next daemon start redoes whatever is still NULL.
        let (store, acct) = store_at_cursor(Some(100));
        old_row(&store, acct, "g-old", true);

        // No route at all: every fetch is a transport error, not a 404.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let dead = format!("http://{addr}");

        engine(store.clone(), acct, &dead)
            .backfill_recipients()
            .await
            .unwrap();
        assert!(
            store.sync_state(acct, RECIPIENTS_KEY).unwrap().is_none(),
            "an interrupted sweep must retry on the next start"
        );
        assert_eq!(
            store.messages_missing_recipients(acct, 10).unwrap().len(),
            1
        );
    }

    // ---- blank-body heal ---------------------------------------------------

    /// The Garmin shape: a multipart/alternative whose text/plain part is a
    /// bare CRLF and whose HTML carries the whole bill.
    fn html_only_eml(at: DateTime<Utc>, html: &str) -> String {
        format!(
            "From: Garmin <garminservices@billing.garmin.com>\r\n\
             To: me@example.com\r\n\
             Subject: Your Garmin Services Bill\r\n\
             Date: {}\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/alternative; boundary=\"alt\"\r\n\
             \r\n\
             --alt\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\
             \r\n\
             \r\n\
             --alt\r\n\
             Content-Type: text/html; charset=utf-8\r\n\
             \r\n\
             {html}\r\n\
             --alt--\r\n",
            at.to_rfc2822()
        )
    }

    const GARMIN_HTML: &str = "<p>Your current Garmin Services bill is now available.</p>\
        <p>If your saved payment method is up to date, no action is required. Your payment \
        will be processed on the due date listed below.</p><p>Balance due: $14.99</p>";

    /// A row as the pre-fix daemon stored it: blank text beside real HTML, a
    /// legacy verdict reached over the subject line alone, and the agent's
    /// access assessment already made over that blank content (the door is
    /// OPEN: there was nothing on the page). `age` places it relative to the
    /// history window (`sync.backfill_days`, 30 by default).
    fn blank_bill_row(
        store: &SqliteStore,
        acct: AccountId,
        gmail: &str,
        age: ChronoDuration,
    ) -> i64 {
        use crate::store::agent_triage::AgentTriageStore;
        let id = store
            .upsert_message(&crate::types::NewMessage {
                account_id: acct,
                gmail_msg_id: gmail.to_string(),
                thread_id: format!("t-{gmail}"),
                from_addr: "garminservices@billing.garmin.com".to_string(),
                from_name: Some("Garmin".to_string()),
                subject: "Your Garmin Services Bill".to_string(),
                received_at: Utc::now() - age,
                snippet: String::new(),
                body: "\r\n".to_string(),
                body_html: Some("<table><tr><td>old html</td></tr></table>".to_string()),
                is_sent: false,
                is_spam: false,
                to_addrs: None,
                cc_addrs: None,
                bcc_addrs: None,
                list_unsubscribe: None,
                list_unsub_one_click: false,
                auth_pass: None,
                sender_signals: None,
            })
            .unwrap();
        store
            .set_triage(
                id,
                acct,
                15,
                Tier::Deadline,
                crate::types::Sensitivity::Normal,
                None,
                "Garmin services bill notice; no amount or due date stated",
                "stage-2: subject only",
                None,
            )
            .unwrap();
        // The access assessment over the blank page, run to its commit: the
        // agent door opens, and nothing is left queued.
        store
            .enqueue_agent_triage(acct, id, "source_access", false)
            .unwrap();
        let job = store
            .claim_agent_job(acct, "access", Utc::now(), 60)
            .unwrap()
            .expect("the access job is claimable");
        assert_eq!(job.message_id, id);
        let original = store.load_agent_access_message(&job).unwrap();
        store
            .commit_agent_access(
                &job,
                &original,
                &crate::triage::access::AccessDecision {
                    restricted: false,
                    reason: "nothing on the page".into(),
                },
                &serde_json::json!({}),
            )
            .unwrap();
        assert_eq!(store.triage_job_counts(acct).unwrap(), [0, 0]);
        assert!(
            store.thread_id_for_message(acct, id).unwrap().is_some(),
            "the agent door is open over the blank page"
        );
        id
    }

    /// The row's text as the human reader sees it, reached through the HUMAN
    /// door: the agent door (`thread_view`, `thread_id_for_message`) is gated
    /// on an access assessment the heal has just put back to pending.
    fn reader_text(store: &SqliteStore, acct: AccountId, id: i64) -> String {
        let hit = store
            .search(acct, "garmin", 50, 0)
            .unwrap()
            .into_iter()
            .find(|h| h.id == id)
            .expect("the row is searchable by its subject");
        store
            .thread_view_with_html(acct, &hit.thread_id)
            .unwrap()
            .messages
            .into_iter()
            .find(|m| m.id == id)
            .expect("the thread holds the row")
            .content
    }

    fn heal_done(store: &SqliteStore, acct: AccountId) -> bool {
        store
            .sync_state(acct, BLANK_BODY_HEAL_KEY)
            .unwrap()
            .is_some_and(|s| s.uidvalidity >= 1)
    }

    /// The one job the heal left queued for `id`, by kind, as the worker would
    /// claim it — `None` when nothing of that kind waits.
    fn queued_job(
        store: &SqliteStore,
        acct: AccountId,
        kind: &str,
    ) -> Option<crate::store::agent_triage::AgentJob> {
        use crate::store::agent_triage::AgentTriageStore;
        store.claim_agent_job(acct, kind, Utc::now(), 60).unwrap()
    }

    #[tokio::test]
    async fn the_blank_body_heal_re_reads_recent_mail_and_hands_it_back_to_the_agent() {
        let (store, acct) = store_at_cursor(Some(100));
        let id = blank_bill_row(&store, acct, "g-garmin", ChronoDuration::days(2));
        assert_eq!(
            store
                .blank_body_messages(acct, i64::MAX, 10)
                .unwrap()
                .candidates
                .len(),
            1
        );

        let g = MockGmail::default();
        g.body("g-garmin", html_only_eml(Utc::now(), GARMIN_HTML));
        let base = serve_mock(g.clone()).await;
        let eng = engine(store.clone(), acct, &base);
        eng.heal_blank_bodies().await.unwrap();

        // The text a fresh arrival would get, in the reader and in FTS. (The
        // mock serves no threadId, so the re-ingest re-derives the thread the
        // way ingest does for any message without one; read it back rather
        // than assuming the seeded value.)
        assert!(reader_text(&store, acct, id).contains("Balance due: $14.99"));
        assert!(
            store
                .search(acct, "balance", 10, 0)
                .unwrap()
                .iter()
                .any(|h| h.id == id),
            "the FTS row is rewritten with the healed body"
        );
        assert!(
            store
                .blank_body_messages(acct, i64::MAX, 10)
                .unwrap()
                .candidates
                .is_empty()
        );
        // The row is back in front of the agent: one background investigation
        // under the heal's own trigger, over the body it can now read. Not an
        // arrival — no notification job, and the legacy stamp untouched.
        let job = queued_job(&store, acct, "triage").expect("a heal investigation waits");
        assert_eq!(job.message_id, id);
        assert_eq!(job.trigger, "heal");
        assert!(!job.foreground, "behind live arrivals, not ahead of them");
        assert!(!job.arrival_eligible);
        assert!(queued_job(&store, acct, "notification").is_none());
        // Not embedded inline: the vector backfill picks it up, batched, ONCE
        // THE RE-TRIAGE HAS DECIDED IT (new content is undecided content, and
        // undecided inbound mail reaches no embedder). The whole contract,
        // not only the query: the heal raised the drain's flag (on the
        // SWEEPS engine, which the daemon builds over the sync engine's
        // handle), a drain that fails leaves it raised, and a drain that
        // succeeds writes the vector and lowers it.
        let owed = |store: &SqliteStore| {
            store
                .messages_missing_vectors(acct, 10)
                .unwrap()
                .iter()
                .any(|m| m.message_id == id)
        };
        assert!(!owed(&store), "undecided: no embedder reads it yet");
        {
            use crate::store::agent_triage::AgentTriageStore;
            let context = store.load_agent_context(&job).unwrap();
            let decision = crate::triage::decision::MessageDecision {
                kinds: vec![crate::triage::decision::EmailKind::Bill],
                summary: "A bill".into(),
                reason: "A bill".into(),
                ..Default::default()
            };
            assert_eq!(
                store
                    .commit_agent_decision(
                        &job,
                        &context,
                        &decision,
                        std::slice::from_ref(&context.message.source),
                    )
                    .unwrap(),
                crate::store::agent_triage::AgentCommitOutcome::Applied
            );
        }
        assert!(owed(&store), "decided and not sealed: owed a vector");
        assert!(
            eng.vectors_dirty.load(Ordering::Relaxed),
            "the heal raises the drain's flag"
        );
        {
            use crate::embed::StubEmbedder;
            use crate::store::sqlite::VEC_DIMS;
            let failing = engine(store.clone(), acct, &base)
                .with_vectors_dirty(eng.vectors_dirty_handle())
                .with_embedder(Arc::new(StubEmbedder::new(0)));
            failing.backfill_missing_vectors().await;
            assert!(
                eng.vectors_dirty.load(Ordering::Relaxed),
                "a drain that fails its batch leaves the debt standing"
            );
            let drainer = engine(store.clone(), acct, &base)
                .with_vectors_dirty(eng.vectors_dirty_handle())
                .with_embedder(Arc::new(StubEmbedder::new(VEC_DIMS)));
            drainer.backfill_missing_vectors().await;
            assert!(
                !store
                    .messages_missing_vectors(acct, 10)
                    .unwrap()
                    .iter()
                    .any(|m| m.message_id == id),
                "the drain re-embeds the healed row"
            );
            assert!(
                !eng.vectors_dirty.load(Ordering::Relaxed),
                "a drain that finishes lowers the shared flag"
            );
        }

        // Once per install: the walk finished, so the cursor says done and a
        // second start makes not one more Gmail call.
        assert_eq!(g.calls("get:g-garmin"), 1);
        assert!(heal_done(&store, acct));
        eng.heal_blank_bodies().await.unwrap();
        assert_eq!(g.calls("get:g-garmin"), 1);
    }

    #[tokio::test]
    async fn the_blank_body_heal_gives_old_mail_its_text_and_an_access_reassessment_only() {
        // Past the history window the cutover did not investigate either: the
        // words arrive, external access is re-read over them (one small model
        // call), and no investigation is spent on a placement for old mail.
        let (store, acct) = store_at_cursor(Some(100));
        let id = blank_bill_row(&store, acct, "g-old", ChronoDuration::days(60));

        let g = MockGmail::default();
        g.body("g-old", html_only_eml(Utc::now(), GARMIN_HTML));
        let base = serve_mock(g.clone()).await;
        engine(store.clone(), acct, &base)
            .heal_blank_bodies()
            .await
            .unwrap();

        assert!(reader_text(&store, acct, id).contains("Balance due"));
        assert!(
            queued_job(&store, acct, "triage").is_none(),
            "no investigation for mail outside the window"
        );
        let job = queued_job(&store, acct, "access").expect("access is re-read");
        assert_eq!(job.message_id, id);
        assert_eq!(job.trigger, "source_access");
        assert!(heal_done(&store, acct));
    }

    #[tokio::test]
    async fn the_blank_body_heal_leaves_a_human_correction_standing() {
        use crate::store::agent_triage::AgentTriageStore;
        let (store, acct) = store_at_cursor(Some(100));
        let id = blank_bill_row(&store, acct, "g-garmin", ChronoDuration::days(2));
        store
            .correct_agent_triage(
                acct,
                id,
                "show_in_fye",
                &serde_json::json!(false),
                Utc::now(),
            )
            .unwrap();

        let g = MockGmail::default();
        g.body("g-garmin", html_only_eml(Utc::now(), GARMIN_HTML));
        let base = serve_mock(g.clone()).await;
        engine(store.clone(), acct, &base)
            .heal_blank_bodies()
            .await
            .unwrap();

        // The words arrive and the row is re-investigated, but the person's
        // correction is field-scoped state the next commit re-applies: it is
        // still on the row, and the healed text did not move it.
        assert!(reader_text(&store, acct, id).contains("Balance due"));
        let job = queued_job(&store, acct, "triage").expect("re-investigated all the same");
        assert_eq!(job.trigger, "heal");
        let context = store.load_agent_context(&job).unwrap();
        assert!(
            context
                .corrections
                .iter()
                .any(|c| c["field"] == "show_in_fye" && c["value"] == false),
            "the correction is handed to the re-read, not lost with the blank body: {:?}",
            context.corrections
        );
    }

    #[tokio::test]
    async fn a_re_read_that_turns_out_to_carry_a_login_code_stays_behind_the_access_gate() {
        // The HTML carries a login code the blank text never showed. Sealing is
        // the model's decision, so the words are written for the human reader
        // like any arrival's; but the content changed, so the external agent's
        // access is pending again and its door stays shut until the queued
        // reassessment has read the real text.
        let (store, acct) = store_at_cursor(Some(100));
        let id = blank_bill_row(&store, acct, "g-otp", ChronoDuration::days(2));

        let g = MockGmail::default();
        g.body(
            "g-otp",
            html_only_eml(
                Utc::now(),
                "<p>Your verification code is 483920. Enter this code to continue.</p>",
            ),
        );
        let base = serve_mock(g.clone()).await;
        engine(store.clone(), acct, &base)
            .heal_blank_bodies()
            .await
            .unwrap();

        assert!(
            reader_text(&store, acct, id).contains("483920"),
            "the human reader gets the words"
        );
        assert!(
            store.thread_id_for_message(acct, id).unwrap().is_none(),
            "the agent door shut again on the content change"
        );
        assert!(store.thread_view(acct, "t-g-otp").is_err());
        let job = queued_job(&store, acct, "triage").expect("re-investigated over the real text");
        assert_eq!(job.trigger, "heal");
        assert!(heal_done(&store, acct));
    }

    #[tokio::test]
    async fn the_blank_body_heal_leaves_mail_whose_html_is_also_blank_alone() {
        // An image-only message: the HTML flattens to nothing too. Nothing is
        // written — a model call over an empty page is a guess — and the pass
        // still converges.
        let (store, acct) = store_at_cursor(Some(100));
        blank_bill_row(&store, acct, "g-pixel", ChronoDuration::days(2));

        let g = MockGmail::default();
        g.body(
            "g-pixel",
            html_only_eml(Utc::now(), "<p>&nbsp;</p><img src=\"https://x/y.png\">"),
        );
        let base = serve_mock(g.clone()).await;
        let eng = engine(store.clone(), acct, &base);
        eng.heal_blank_bodies().await.unwrap();

        assert_eq!(
            store.triage_job_counts(acct).unwrap(),
            [0, 0],
            "nothing queued"
        );
        assert!(
            heal_done(&store, acct),
            "a message that cannot be healed does not hold the pass open"
        );
        eng.heal_blank_bodies().await.unwrap();
        assert_eq!(g.calls("get:g-pixel"), 1);
    }

    #[tokio::test]
    async fn the_blank_body_heal_passes_over_a_missing_message_and_heals_the_rest() {
        // Two candidates, newest first: the newer heals, the older is gone
        // from Gmail (404). A missing message is an answer, not a stop — it is
        // passed over, counted, and the walk still finishes.
        let (store, acct) = store_at_cursor(Some(100));
        let older = blank_bill_row(&store, acct, "g-older", ChronoDuration::days(3));
        let newer = blank_bill_row(&store, acct, "g-newer", ChronoDuration::days(2));

        let g = MockGmail::default();
        g.body("g-newer", html_only_eml(Utc::now(), GARMIN_HTML));
        let base = serve_mock(g.clone()).await;
        engine(store.clone(), acct, &base)
            .heal_blank_bodies()
            .await
            .unwrap();

        let job = queued_job(&store, acct, "triage").expect("the newer row was re-read");
        assert_eq!(job.message_id, newer);
        assert!(
            queued_job(&store, acct, "triage").is_none(),
            "the missing one is left as it was"
        );
        assert!(reader_text(&store, acct, older).trim().is_empty());
        assert!(heal_done(&store, acct));
        assert_eq!(g.calls("get:g-older"), 1);
    }

    #[tokio::test]
    async fn a_transport_error_leaves_the_blank_body_heal_resumable() {
        let (store, acct) = store_at_cursor(Some(100));
        let id = blank_bill_row(&store, acct, "g-garmin", ChronoDuration::days(2));

        // No route at all: every fetch is a transport error, not a 404.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let dead = format!("http://{addr}");

        engine(store.clone(), acct, &dead)
            .heal_blank_bodies()
            .await
            .unwrap();
        assert!(!heal_done(&store, acct), "an interrupted sweep must retry");
        assert_eq!(
            store.triage_job_counts(acct).unwrap(),
            [0, 0],
            "nothing written"
        );

        // Next start, Gmail answering: the same row is re-read and the walk
        // completes.
        let g = MockGmail::default();
        g.body("g-garmin", html_only_eml(Utc::now(), GARMIN_HTML));
        let base = serve_mock(g.clone()).await;
        engine(store.clone(), acct, &base)
            .heal_blank_bodies()
            .await
            .unwrap();
        assert!(heal_done(&store, acct));
        let job = queued_job(&store, acct, "triage").expect("re-read on the retry");
        assert_eq!(job.message_id, id);
        assert_eq!(g.calls("get:g-garmin"), 1);
    }

    #[tokio::test]
    async fn the_blank_body_heal_treats_mail_gmail_no_longer_has_as_settled() {
        // A 404 is an answer: nothing to heal from, nothing to retry.
        let (store, acct) = store_at_cursor(Some(100));
        blank_bill_row(&store, acct, "g-gone", ChronoDuration::days(2));
        let base = serve_mock(MockGmail::default()).await;
        engine(store.clone(), acct, &base)
            .heal_blank_bodies()
            .await
            .unwrap();
        assert!(heal_done(&store, acct));
    }

    #[tokio::test]
    async fn outbound_auth_copy_is_stored_for_humans_pending_access_assessment() {
        // The user replies from their phone to a thread quoting an OTP. Seal
        // detection fires on the reply, and `thread_guard_and_subject` 404s any
        // thread holding a sealed row — so committing it would hide the
        // counterparty's mail the user is reading. Nothing is written at all.
        let (store, acct) = store_at_cursor(Some(100));
        let now = Utc::now();
        let reply = format!(
            "From: me@example.com\r\n\
             To: Bank <noreply@bank.example>\r\n\
             Subject: Re: Your verification code\r\n\
             Date: {}\r\n\
             \r\n\
             got it, thanks\r\n\
             > Your one-time passcode is 483920. Enter this code to continue.\r\n",
            now.to_rfc2822()
        );

        let g = MockGmail::default();
        // Current historyId at the SENT record's, as a real mailbox reports it.
        g.history(LABEL_INBOX, LabelHistory::quiet(170));
        g.history(
            LABEL_SENT,
            LabelHistory::added(170, &[(170, "g-sealed-out")]),
        );
        g.body("g-sealed-out", reply);
        let base = serve_mock(g.clone()).await;

        engine(store.clone(), acct, &base)
            .poll_once()
            .await
            .unwrap();

        assert!(
            store.thread_view_with_html(acct, "g-sealed-out").is_ok(),
            "the human can read their sent copy immediately"
        );
        assert!(
            store.sealed_messages(acct).unwrap().is_empty(),
            "legacy deterministic sealing is not used"
        );
        assert!(
            store.thread_view(acct, "g-sealed-out").is_err(),
            "external access awaits assessment"
        );
        assert!(store.events_after(acct, 0, 100).unwrap().is_empty());
        // The poll is otherwise normal: the cursor still commits.
        assert_eq!(cursor_of(&store, acct), Some(170));
    }

    #[tokio::test]
    async fn a_self_addressed_send_keeps_its_visible_inbox_copy() {
        // Mail to yourself carries BOTH labels, so both filtered walks return it.
        // The message upsert writes `is_sent = excluded.is_sent` on conflict, so a
        // sent copy landing second would hide the message from every band.
        let (store, acct) = store_at_cursor(Some(100));
        let now = Utc::now();
        let eml = format!(
            "From: me@example.com\r\n\
             To: me@example.com\r\n\
             Subject: note to self about the lease\r\n\
             Date: {}\r\n\
             \r\n\
             remember to countersign\r\n",
            now.to_rfc2822()
        );

        let g = MockGmail::default();
        g.history(LABEL_INBOX, LabelHistory::added(150, &[(150, "g-self")]));
        g.history(LABEL_SENT, LabelHistory::added(150, &[(150, "g-self")]));
        g.body("g-self", eml);
        let base = serve_mock(g.clone()).await;

        engine(store.clone(), acct, &base)
            .poll_once()
            .await
            .unwrap();

        // ONE row, and it is the visible one.
        let view = store.thread_view_with_html(acct, "g-self").unwrap();
        assert_eq!(view.messages.len(), 1, "one row, not two");
        let visible = listed(&store, acct, now);
        assert_eq!(
            visible.len(),
            1,
            "the inbox copy must win the unique-key race"
        );
        assert_eq!(visible[0].update.id, view.messages[0].id);

        // The INBOX walk ran FIRST and claimed the id, so the sent half never even
        // re-fetched it.
        assert!(
            g.at("history:INBOX") < g.at("history:SENT"),
            "{:?}",
            g.seen()
        );
        assert_eq!(g.calls("get:g-self"), 1, "fetched once, not twice");
    }

    #[tokio::test]
    async fn a_failing_sent_walk_keeps_the_inbox_batch_and_holds_the_cursor() {
        // Losing the sent half is survivable; losing INBOX progress is not. The
        // cursor stays put and the next poll re-walks both labels from it.
        let (store, acct) = store_at_cursor(Some(100));
        let now = Utc::now();
        let eml = format!(
            "From: Alice <alice@friends.com>\r\n\
             To: me@example.com\r\n\
             Subject: the lease\r\n\
             Date: {}\r\n\
             \r\n\
             sending the countersigned copy over\r\n",
            now.to_rfc2822()
        );

        let g = MockGmail::default();
        g.history(LABEL_INBOX, LabelHistory::added(150, &[(150, "g-in")]));
        g.history(LABEL_SENT, LabelHistory::broken());
        g.body("g-in", eml);
        let base = serve_mock(g.clone()).await;

        // The poll SUCCEEDS: a sent-side outage is not worth a backoff cycle.
        engine(store.clone(), acct, &base)
            .poll_once()
            .await
            .unwrap();

        assert_eq!(
            listed(&store, acct, now).len(),
            1,
            "the inbox batch is ingested even though the sent walk failed"
        );
        assert_eq!(
            cursor_of(&store, acct),
            Some(100),
            "a partial poll must not commit the cursor"
        );

        // The re-walk next poll is idempotent: still one row, still one event.
        let events_before = store.events_after(acct, 0, 100).unwrap().len();
        engine(store.clone(), acct, &base)
            .poll_once()
            .await
            .unwrap();
        assert_eq!(
            store
                .thread_view_with_html(acct, "g-in")
                .unwrap()
                .messages
                .len(),
            1
        );
        assert_eq!(
            store.events_after(acct, 0, 100).unwrap().len(),
            events_before
        );
        assert_eq!(cursor_of(&store, acct), Some(100));
    }

    #[tokio::test]
    async fn the_cursor_commits_once_at_the_max_across_both_walks() {
        // One commit, at the highest historyId either walk saw — committing per
        // walk would let the second one rewind the first.
        let (store, acct) = store_at_cursor(Some(100));
        let now = Utc::now();

        let g = MockGmail::default();
        g.history(LABEL_INBOX, LabelHistory::added(150, &[(150, "g-in")]));
        g.history(LABEL_SENT, LabelHistory::added(220, &[(220, "g-out")]));
        g.body(
            "g-in",
            format!(
                "From: Alice <alice@friends.com>\r\n\
                 To: me@example.com\r\n\
                 Subject: the lease\r\n\
                 Date: {}\r\n\
                 \r\n\
                 countersigned copy attached\r\n",
                now.to_rfc2822()
            ),
        );
        g.body("g-out", sent_eml(now, "alice@friends.com", "re: the lease"));
        let base = serve_mock(g.clone()).await;

        engine(store.clone(), acct, &base)
            .poll_once()
            .await
            .unwrap();

        assert_eq!(cursor_of(&store, acct), Some(220), "max across both walks");
        // Both landed, each on its own side of the visibility line.
        assert_eq!(
            listed(&store, acct, now).len(),
            1,
            "only the received message is in a band"
        );
        assert_eq!(
            store
                .thread_view_with_html(acct, "g-out")
                .unwrap()
                .messages
                .len(),
            1
        );
        assert!(store.is_known_contact(acct, "alice@friends.com").unwrap());
    }

    #[tokio::test]
    async fn catch_up_lists_inbox_and_sent_over_the_same_window() {
        // No cursor: the history walk cannot account for the gap, so both labels
        // are re-listed or everything written from another client is lost.
        let (store, acct) = store_at_cursor(None);
        let now = Utc::now();

        let g = MockGmail::default();
        // "g-self" is in BOTH listings, as a self-addressed message is.
        g.listing(LABEL_INBOX, &["g-in", "g-self"]);
        g.listing(LABEL_SENT, &["g-out", "g-self"]);
        g.body(
            "g-in",
            format!(
                "From: Alice <alice@friends.com>\r\n\
                 To: me@example.com\r\n\
                 Subject: the lease\r\n\
                 Date: {}\r\n\
                 \r\n\
                 countersigned copy attached\r\n",
                now.to_rfc2822()
            ),
        );
        g.body("g-out", sent_eml(now, "bob@friends.com", "re: the lease"));
        g.body(
            "g-self",
            format!(
                "From: me@example.com\r\n\
                 To: me@example.com\r\n\
                 Subject: note to self\r\n\
                 Date: {}\r\n\
                 \r\n\
                 countersign this\r\n",
                now.to_rfc2822()
            ),
        );
        g.profile(900);
        let base = serve_mock(g.clone()).await;

        engine(store.clone(), acct, &base)
            .poll_once()
            .await
            .unwrap();

        assert!(g.at("list:INBOX") < g.at("list:SENT"), "{:?}", g.seen());
        assert_eq!(
            store
                .thread_view_with_html(acct, "g-out")
                .unwrap()
                .messages
                .len(),
            1
        );
        assert!(store.is_known_contact(acct, "bob@friends.com").unwrap());
        // The dual-listed message keeps its visible copy, exactly as in the walk.
        let visible: Vec<String> = listed(&store, acct, now)
            .into_iter()
            .map(|u| u.update.thread_id)
            .collect();
        assert!(visible.contains(&"g-in".to_string()));
        assert!(visible.contains(&"g-self".to_string()));
        assert_eq!(g.calls("get:g-self"), 1, "fetched once, not twice");
        assert_eq!(cursor_of(&store, acct), Some(900));
    }

    #[tokio::test]
    async fn a_first_backfill_waits_for_the_embedder_gate() {
        // A brand-new tenant: no cursor, so the very next thing `run_once` would
        // do is the 30-day backfill. Behind a shut gate it does not do it, and
        // "does not" is asserted on the WIRE rather than on a flag, because the
        // failure this prevents is thousands of rows ingested with no vector.
        let (store, acct) = store_at_cursor(None);
        let g = MockGmail::default();
        g.listing(LABEL_INBOX, &[]);
        g.listing(LABEL_SENT, &[]);
        g.profile(900);
        let base = serve_mock(g.clone()).await;

        // The sender is HELD for the length of the wait: dropping it says "the
        // init task is gone, nothing will ever settle this", which is a reason
        // to stop waiting, not to keep waiting.
        let (_gate_tx, gate_rx) = tokio::sync::watch::channel(false);
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
        // Sent before the call, but the receiver has not observed it, so its
        // `changed()` fires the instant the engine parks on the gate. That is
        // the shutdown-during-the-wait path, and it is what makes this test
        // terminate rather than sit here for the ceiling.
        shutdown_tx.send(true).unwrap();

        engine(store.clone(), acct, &base)
            .with_embedder_gate(gate_rx)
            .run_once(&mut shutdown_rx)
            .await
            .unwrap();

        assert!(
            g.seen().is_empty(),
            "a shut gate holds the first backfill off the wire, saw {:?}",
            g.seen()
        );
        assert_eq!(
            cursor_of(&store, acct),
            None,
            "and leaves no cursor, so the next start is still a first run"
        );

        // Same engine, same mailbox, one bit different: an OPEN gate is not
        // waited on at all and the backfill goes straight out. Driven through
        // `run()` and stopped once the cursor exists: the lanes run beside the
        // backfill now, and a shutdown they can see ends the lifecycle at once,
        // backfill included, as it should.
        let (_open_tx, open_rx) = tokio::sync::watch::channel(true);
        let (stop, rx) = tokio::sync::watch::channel(false);
        let e = engine(store.clone(), acct, &base).with_embedder_gate(open_rx);
        let task = tokio::spawn(async move { e.run(rx).await });
        wait_for(5, "the first backfill to establish a cursor", || {
            cursor_of(&store, acct).is_some()
        })
        .await;
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        assert!(g.at("list:INBOX") < g.at("list:SENT"), "{:?}", g.seen());
        assert_eq!(
            cursor_of(&store, acct),
            Some(900),
            "an open gate releases the first backfill"
        );
    }

    #[tokio::test]
    async fn an_engine_with_no_embedder_gate_backfills_at_once() {
        // Sync-only mode builds its embedder before the engine exists, and the
        // rest of the tests have nothing to wait for. Absence of a gate must
        // therefore mean "never wait", not "wait for something that will never
        // arrive" — that reading would wedge `squelchd run` on startup.
        let (store, acct) = store_at_cursor(None);
        let g = MockGmail::default();
        g.listing(LABEL_INBOX, &[]);
        g.listing(LABEL_SENT, &[]);
        g.profile(900);
        let base = serve_mock(g.clone()).await;

        let (stop, rx) = tokio::sync::watch::channel(false);
        let e = engine(store.clone(), acct, &base);
        let task = tokio::spawn(async move { e.run(rx).await });
        wait_for(5, "the first backfill to establish a cursor", || {
            cursor_of(&store, acct).is_some()
        })
        .await;
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        assert_eq!(
            cursor_of(&store, acct),
            Some(900),
            "no gate, no wait: the first backfill ran on this lifecycle"
        );
    }

    /// An engine parked on the gate, for the three tests below. No mock: they
    /// drive the WAIT, and the wait touches neither Gmail nor the store, so the
    /// base is a port nothing is listening on and reaching it would be the bug.
    fn gated_engine(
        gate: tokio::sync::watch::Receiver<bool>,
        metrics: Arc<SyncMetrics>,
    ) -> SyncEngine<SqliteStore, FixedToken> {
        let (store, acct) = store_at_cursor(None);
        engine(store, acct, "http://127.0.0.1:1")
            .with_metrics(metrics)
            .with_embedder_gate(gate)
    }

    /// `changed()` fires on ANY send and on the last sender dropping, so the
    /// value is the only thing that says a shutdown happened. Read the wakeup
    /// alone as one and the cost is not a slow first backfill, it is no sync at
    /// all: `run_once` returns `Ok` on a reported shutdown and `run` reads that
    /// as "we are done" and stops the lifecycle.
    ///
    /// MUTATION 1: `_ = shutdown.changed() => return false`, dropping the value
    /// check, the obvious reading: the wait reports "shut down" and the sync
    /// lifecycle ends. MUTATION 2: the arm returning `true` instead of
    /// re-parking: the backfill runs unembedded on a wakeup that asked for
    /// nothing. This asserts both halves: the wait is still parked after the
    /// wakeup, and it releases only when the gate opens.
    #[tokio::test(start_paused = true)]
    async fn a_shutdown_wakeup_carrying_false_is_not_a_shutdown() {
        let (gate_tx, gate_rx) = tokio::sync::watch::channel(false);
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
        // Unseen by this receiver, so `changed()` completes the instant the
        // engine parks. The VALUE stays false: nobody asked for a shutdown.
        shutdown_tx.send(false).unwrap();
        let e = gated_engine(gate_rx, SyncMetrics::new());

        let wait = e.wait_for_embedder(&mut shutdown_rx);
        tokio::pin!(wait);
        // Still parked after the wakeup: under the paused clock a one-second
        // timeout elapses before the three-minute ceiling could.
        assert!(
            tokio::time::timeout(Duration::from_secs(1), &mut wait)
                .await
                .is_err(),
            "a false shutdown wakeup released the first backfill"
        );
        gate_tx.send(true).unwrap();
        let go_ahead = tokio::time::timeout(EMBEDDER_GATE_CEILING / 2, &mut wait)
            .await
            .expect("the gate opening releases the wait");
        assert!(go_ahead, "false on the shutdown watch is not a shutdown");
    }

    /// The gate's sender dropping is the init task GONE. Nothing is ever going
    /// to open it, so waiting the ceiling out would buy a brand-new tenant
    /// minutes of empty mailbox for an answer that has already arrived.
    ///
    /// MUTATION: folding `Err` into the "keep waiting" side (an `Err` arm that
    /// falls through to the sleep, or no `Err` arm at all). Under a paused clock
    /// that runs to the ceiling, which this timeout is half of.
    #[tokio::test(start_paused = true)]
    async fn a_dropped_embedder_gate_releases_the_backfill_at_once() {
        let (gate_tx, gate_rx) = tokio::sync::watch::channel(false);
        drop(gate_tx);
        // Never signalled: the shutdown arm must not be what ends this wait.
        let (_shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
        let e = gated_engine(gate_rx, SyncMetrics::new());

        let go_ahead = tokio::time::timeout(
            EMBEDDER_GATE_CEILING / 2,
            e.wait_for_embedder(&mut shutdown_rx),
        )
        .await
        .expect("a gate nobody holds any more is not worth waiting on");
        assert!(go_ahead, "a dropped gate releases the first backfill");
    }

    /// The ceiling arm: a gate whose sender is alive and never sends (an init
    /// wedged on a hung download) degrades to the behaviour that predates the
    /// gate — an unembedded backfill the vector pass drains later — rather than
    /// to a daemon that never syncs. And it SAYS SO to the scrape, because this
    /// is the one event that re-arms the memory the gate exists to avoid, and
    /// nobody reads a tenant's stderr until something has already fallen over.
    ///
    /// Time is paused, so the ceiling is a fact here and not three real minutes.
    ///
    /// MUTATIONS: no ceiling arm at all (the outer timeout fires instead of the
    /// wait returning); a ceiling that reports a shutdown (`go_ahead`); a
    /// ceiling that does not wait, e.g. `Duration::ZERO` (`elapsed`); and the
    /// counter left unrecorded (the exposition assert).
    #[tokio::test(start_paused = true)]
    async fn the_gate_ceiling_releases_the_backfill_and_says_so_to_the_scrape() {
        let (_gate_tx, gate_rx) = tokio::sync::watch::channel(false);
        let (_shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
        let metrics = SyncMetrics::new();
        let e = gated_engine(gate_rx, metrics.clone());

        let started = tokio::time::Instant::now();
        let go_ahead = tokio::time::timeout(
            EMBEDDER_GATE_CEILING * 2,
            e.wait_for_embedder(&mut shutdown_rx),
        )
        .await
        .expect("a wedged init must not wedge sync");
        assert!(go_ahead, "the ceiling releases the backfill");
        assert!(
            started.elapsed() >= EMBEDDER_GATE_CEILING,
            "and only after the ceiling, not instead of waiting"
        );
        assert!(
            crate::metrics::render(&metrics, None)
                .contains("squelchd_embedder_gate_timeouts_total 1\n"),
            "a ceiling nobody can scrape is a ceiling nobody knows fired"
        );
    }

    #[tokio::test]
    async fn an_inbox_walk_that_leaks_an_echoed_send_does_not_demote_it() {
        // Production, 2026-10: the INBOX history walk handed back the user's
        // own sends. The INBOX half ingests first and the SENT half subtracts
        // its ids, so the walk alone decided the row was received mail, and
        // it was gone from the sent page. The message's own labels
        // (SENT, no INBOX) are what must decide.
        let (store, acct) = store_at_cursor(Some(100));
        let now = Utc::now();
        let eml = sent_eml(now, "alice@friends.com", "re: the lease");

        let echoed = ingest::ingest_sent(
            &store,
            acct,
            "me@example.com",
            "g-out",
            None,
            eml.clone().into_bytes(),
            Some(now),
            now,
        )
        .unwrap()
        .expect("the echo commits a row");

        let g = MockGmail::default();
        g.history(LABEL_INBOX, LabelHistory::added(160, &[(160, "g-out")]));
        g.history(LABEL_SENT, LabelHistory::added(160, &[(160, "g-out")]));
        g.body("g-out", eml);
        g.message_labels("g-out", &["SENT"]);
        let base = serve_mock(g.clone()).await;

        engine(store.clone(), acct, &base)
            .poll_once()
            .await
            .unwrap();

        assert!(
            store.stored_is_sent(echoed).unwrap(),
            "the INBOX sighting must not demote the echo to received mail"
        );
        assert!(store.events_after(acct, 0, 100).unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_note_to_self_found_by_the_sent_walk_alone_lands_visible() {
        // The other direction of the same rule: a message carrying INBOX is
        // received mail whichever walk reached it, so a note to self that only
        // the SENT history returned still lands as the visible copy.
        let (store, acct) = store_at_cursor(Some(100));
        let now = Utc::now();
        let eml = format!(
            "From: me@example.com\r\n\
             To: me@example.com\r\n\
             Subject: note to self about the lease\r\n\
             Date: {}\r\n\
             \r\n\
             remember to countersign\r\n",
            now.to_rfc2822()
        );

        let g = MockGmail::default();
        g.history(LABEL_INBOX, LabelHistory::quiet(150));
        g.history(LABEL_SENT, LabelHistory::added(150, &[(150, "g-self")]));
        g.body("g-self", eml);
        g.message_labels("g-self", &["INBOX", "SENT", "UNREAD"]);
        let base = serve_mock(g.clone()).await;

        engine(store.clone(), acct, &base)
            .poll_once()
            .await
            .unwrap();

        let view = store.thread_view_with_html(acct, "g-self").unwrap();
        assert!(
            !store.stored_is_sent(view.messages[0].id).unwrap(),
            "the inbox copy is received mail, whichever walk found it"
        );
    }

    #[tokio::test]
    async fn a_message_the_api_already_echoed_redelivers_as_a_no_op() {
        // The api echoes a send at send time; the sent walk then delivers the very
        // same Gmail id on the next poll.
        let (store, acct) = store_at_cursor(Some(100));
        let now = Utc::now();
        let eml = sent_eml(now, "alice@friends.com", "re: the lease");

        let echoed = ingest::ingest_sent(
            &store,
            acct,
            "me@example.com",
            "g-echo",
            None,
            eml.clone().into_bytes(),
            Some(now),
            now,
        )
        .unwrap()
        .expect("the echo commits a row");

        let g = MockGmail::default();
        // Current historyId at the SENT record's, as a real mailbox reports it.
        g.history(LABEL_INBOX, LabelHistory::quiet(160));
        g.history(LABEL_SENT, LabelHistory::added(160, &[(160, "g-echo")]));
        g.body("g-echo", eml);
        let base = serve_mock(g.clone()).await;

        engine(store.clone(), acct, &base)
            .poll_once()
            .await
            .unwrap();

        let view = store.thread_view_with_html(acct, "g-echo").unwrap();
        assert_eq!(view.messages.len(), 1, "one row, not a second copy");
        assert_eq!(view.messages[0].id, echoed, "the same local row");
        assert!(store.events_after(acct, 0, 100).unwrap().is_empty());
        assert!(
            listed(&store, acct, now).is_empty(),
            "re-delivery must not promote the echo into a band"
        );
        let row = store
            .triage_debug(acct, echoed)
            .unwrap()
            .expect("triage row");
        assert_eq!(row.tier, "noise");
        assert_eq!(row.importance, 0);
    }

    #[tokio::test]
    async fn the_inbox_unread_counts_refresh_and_survive_a_failed_fetch() {
        // Gmail owns these numbers: nothing local can be re-derived into them,
        // so a failed fetch must leave the last known pair standing rather than
        // clear it or read as "0 unread".
        let (store, acct) = store_at_cursor(Some(100));

        let g = MockGmail::default();
        g.label(LABEL_INBOX, Some((214, 190)));
        let base = serve_mock(g.clone()).await;
        let engine = engine(store.clone(), acct, &base);

        engine.refresh_inbox_unread().await;
        let got = store.inbox_unread(acct).unwrap().expect("counts stored");
        assert_eq!((got.messages, got.threads), (214, 190));
        assert_eq!(g.calls("label:INBOX"), 1, "one labels.get per refresh");

        // Gmail goes down mid-run: the refresh swallows it (a poll cycle must
        // not die over a cosmetic number) and the stored pair is untouched.
        g.label(LABEL_INBOX, None);
        engine.refresh_inbox_unread().await;
        let kept = store.inbox_unread(acct).unwrap().expect("last counts kept");
        assert_eq!((kept.messages, kept.threads), (214, 190));
        assert_eq!(kept.fetched_at, got.fetched_at, "the stamp did not move");

        // Recovery overwrites with the newer truth.
        g.label(LABEL_INBOX, Some((3, 3)));
        engine.refresh_inbox_unread().await;
        let fresh = store.inbox_unread(acct).unwrap().unwrap();
        assert_eq!((fresh.messages, fresh.threads), (3, 3));
    }

    #[tokio::test]
    async fn a_200_that_is_not_a_label_keeps_the_counts_but_a_counterless_label_zeroes_them() {
        // The two 200s that look alike to a defaulted decoder and must not be
        // treated alike: Gmail dropping the counters (a real zero) versus a
        // body that is not a label at all (no answer, keep the last one).
        let (store, acct) = store_at_cursor(Some(100));

        let g = MockGmail::default();
        g.label(LABEL_INBOX, Some((214, 190)));
        let base = serve_mock(g.clone()).await;
        let engine = engine(store.clone(), acct, &base);
        engine.refresh_inbox_unread().await;
        let seeded = store.inbox_unread(acct).unwrap().expect("counts stored");

        // Not label resources: an empty object and an error envelope served
        // with a 200. Required `id` makes each a decode error, so the seeded
        // pair stands untouched — the stamp included.
        for body in [json!({}), json!({ "error": { "code": 403 } })] {
            g.label_body(LABEL_INBOX, body);
            engine.refresh_inbox_unread().await;
            let kept = store.inbox_unread(acct).unwrap().expect("last counts kept");
            assert_eq!((kept.messages, kept.threads), (214, 190));
            assert_eq!(kept.fetched_at, seeded.fetched_at, "the stamp did not move");
        }

        // A label resource WITHOUT the counters is Gmail's way of saying zero:
        // it omits them on a label with nothing unread, so this one is stored.
        g.label_body(LABEL_INBOX, json!({ "id": LABEL_INBOX, "name": "INBOX" }));
        engine.refresh_inbox_unread().await;
        let zeroed = store
            .inbox_unread(acct)
            .unwrap()
            .expect("zero is an answer");
        assert_eq!((zeroed.messages, zeroed.threads), (0, 0));
    }

    #[tokio::test]
    async fn a_gmail_outage_cannot_starve_the_reminder_sweep() {
        // THE SWEEP IS LOCAL, so it runs AHEAD of the walk. A tick that bounces
        // — an expired credential, a Gmail outage — takes everything after
        // `poll_once` with it, and a reminder hung off the far side of that call
        // would sit unfired for as long as the outage lasts: a weekend of Gmail
        // being down means Saturday's reminder never arrives.
        let (store, acct) = store_at_cursor(Some(100));
        let mid = store
            .upsert_message(&NewMessage {
                account_id: acct,
                gmail_msg_id: "g-parked".into(),
                thread_id: "t-parked".into(),
                from_addr: "alice@friends.com".into(),
                from_name: None,
                subject: "the thing you parked".into(),
                received_at: Utc::now() - ChronoDuration::days(1),
                snippet: String::new(),
                body: "body".into(),
                body_html: None,
                is_sent: false,
                is_spam: false,
                to_addrs: None,
                cc_addrs: None,
                bcc_addrs: None,
                list_unsubscribe: None,
                list_unsub_one_click: false,
                auth_pass: None,
                sender_signals: None,
            })
            .unwrap();
        store
            .set_triage(
                mid,
                acct,
                0,
                Tier::Noise,
                crate::types::Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
        store
            .set_reminder(acct, mid, Utc::now() - ChronoDuration::minutes(1))
            .unwrap();

        // Gmail is down for this mailbox: the INBOX walk 500s, so the tick
        // returns Err and the loop hands it to the caller's backoff.
        let g = MockGmail::default();
        g.history(LABEL_INBOX, LabelHistory::broken());
        let base = serve_mock(g.clone()).await;
        let (_tx, mut shutdown) = tokio::sync::watch::channel(false);
        assert!(
            engine(store.clone(), acct, &base)
                .poll_lane(&mut shutdown)
                .await
                .is_err(),
            "the poll itself failed, which is the whole premise"
        );

        // ...and the reminder fired anyway.
        let rows = inventory(
            &store,
            acct,
            Utc::now() - ChronoDuration::days(30),
            SpamScope::Exclude,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, crate::types::AttentionStatus::Open);
        assert!(rows[0].remind_at.is_none(), "the pending stamp MOVED");
        assert!(rows[0].reminded_at.is_some(), "it came back on time");
    }

    /// THE PAST-EVENT RE-CHECK RIDES THE SAME TICK, ahead of the walk: a
    /// flight that landed days ago has its mail queued for the agent to
    /// decide on even while Gmail is down. Queued, not done: that is the
    /// model's answer.
    #[tokio::test]
    async fn the_poll_tick_queues_a_past_flights_recheck() {
        let (store, acct) = store_at_cursor(Some(100));
        let mid = store
            .upsert_message(&NewMessage {
                account_id: acct,
                gmail_msg_id: "g-checkin".into(),
                thread_id: "t-checkin".into(),
                from_addr: "noreply@airline.example".into(),
                from_name: None,
                subject: "Check in now".into(),
                received_at: Utc::now() - ChronoDuration::days(6),
                snippet: String::new(),
                body: "body".into(),
                body_html: None,
                is_sent: false,
                is_spam: false,
                to_addrs: None,
                cc_addrs: None,
                bcc_addrs: None,
                list_unsubscribe: None,
                list_unsub_one_click: false,
                auth_pass: Some(true),
                sender_signals: None,
            })
            .unwrap();
        store
            .set_triage(
                mid,
                acct,
                0,
                Tier::Noise,
                crate::types::Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
        // The ghost as the projection leaves it: left five days ago, landed
        // the same day, its facts from this message, which may feed it.
        let start = Utc::now() - ChronoDuration::days(5);
        let end = start + ChronoDuration::hours(11);
        // The message's own record, which the scheduler also reads.
        use crate::triage::decision::{
            EventChange, EventCommitment, EventRecordKind, MessageDecision, RecordProposal,
            SupportedTime,
        };
        let at = |t: DateTime<Utc>| {
            Some(SupportedTime {
                value: t.to_rfc3339(),
                timezone: None,
                source_message_id: mid,
                interpreted_relative: false,
            })
        };
        let decision = MessageDecision {
            records: vec![RecordProposal::Event {
                title: "Flight".into(),
                start: at(start),
                location: None,
                end: at(end),
                duration_estimate: None,
                event_kind: EventRecordKind::Travel,
                commitment: EventCommitment::Committed,
                change: EventChange::Created,
                identity_refs: vec!["PNR1".into()],
                likelihood: None,
                evidence: vec![],
                identity: None,
            }],
            ..Default::default()
        };
        store.execute_batch_for_test(&format!(
            "INSERT INTO agent_message_decisions(account_id,message_id,revision,decision_json,decided_at)
                 VALUES({acct},{mid},1,'{json}','{s}');
             INSERT INTO agent_message_state(account_id,message_id,content_snapshot,access)
                 VALUES({acct},{mid},'','allowed');
             INSERT INTO ghost_events(id,account_id,kind,title,starts_at,start_sort,ends_at,
                 commitment,owner_scope,owner_addr,supplier_message_id,created_at,updated_at)
                 VALUES(1,{acct},'travel','Flight','{s}','{sort}','{e}','committed',
                 'airline.example','noreply@airline.example',{mid},'{s}','{s}');
             INSERT INTO ghost_event_sources(account_id,ghost_id,message_id,position,role)
                 VALUES({acct},1,{mid},0,'created');",
            s = start.to_rfc3339(),
            json = serde_json::to_string(&decision).unwrap().replace('\'', "''"),
            sort = start.format("%Y-%m-%dT%H:%M:%SZ"),
            e = end.to_rfc3339(),
        ));

        let g = MockGmail::default();
        g.history(LABEL_INBOX, LabelHistory::broken());
        let base = serve_mock(g.clone()).await;
        let (_tx, mut shutdown) = tokio::sync::watch::channel(false);
        assert!(
            engine(store.clone(), acct, &base)
                .poll_lane(&mut shutdown)
                .await
                .is_err()
        );
        let rows = inventory(
            &store,
            acct,
            Utc::now() - ChronoDuration::days(30),
            SpamScope::Exclude,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, crate::types::AttentionStatus::New);
        use crate::store::agent_triage::AgentTriageStore;
        let job = store
            .claim_agent_job(acct, "investigation", Utc::now(), 120)
            .unwrap()
            .expect("a re-check queued");
        assert_eq!(
            (job.message_id, job.trigger),
            (
                mid,
                format!(
                    "revisit:event_ended:{}",
                    (end + ChronoDuration::hours(24)).to_rfc3339()
                )
            )
        );
    }

    // ---- the two lanes, on the wire ----------------------------------------
    //
    // Nothing else in this file drives a refine pass against a mock LLM through
    // the engine; the emission sites are tested by hand-written mirrors. This
    // one property is worth the harness because it is the property the whole
    // change is about (docs/NOTIFY.md §11.2, §11.10).

    /// THE STAMP IS READ PER MESSAGE, NOT PER BATCH. One
    /// `fetch_raw_and_ingest` call over two ids, with the second one's fetch
    /// made to take real time, and the two stamps the engine wrote must differ
    /// by roughly that much.
    ///
    /// A batch-wide `let now = Utc::now()` passes every other test in this file:
    /// the engine's own emission tests call `ingest_one` with an injected clock,
    /// so only this path can tell. What it costs in production is the whole
    /// point of docs/NOTIFY.md §11.3. `catch_up` runs this loop over the entire
    /// backfill window, one sequential `format=raw` GET per id, and
    /// `catch_up_inner`'s own comment records a tenant spending half an hour in
    /// there. Frozen at the top:
    ///
    /// - a genuinely new message fetched 30 minutes in enters the Stage-1 queue
    ///   with half its rescue window already spent, and expires before the refine
    ///   lane reaches it: a notification dropped AND booked as `deliberate/expired`
    ///   in the ledger the rollout decision is read off;
    /// - past an hour of batch runtime every remaining message reads as dated in
    ///   the future, fails `is_fresh`, and is stamped NULL — unnotifiable at every
    ///   site, forever. That is §2a's silent drop, reintroduced on the
    ///   post-outage path where notifications matter most.
    ///
    /// The stall is a FIXTURE, not an assertion: the test asserts on two
    /// timestamps the engine wrote to SQLite, and a slow machine only widens the
    /// gap it is checking for.
    #[tokio::test]
    async fn the_eligibility_stamp_is_taken_per_message_not_per_batch() {
        const STALL: Duration = Duration::from_millis(400);

        let (store, acct) = store_at_cursor(Some(100));
        let now = Utc::now();

        // BOTH IDS IN ONE BATCH: one history record, so `history_walk` makes a
        // single `fetch_raw_and_ingest` call over both.
        let g = MockGmail::default();
        g.history(
            LABEL_INBOX,
            LabelHistory::added(141, &[(140, "g-early"), (141, "g-late")]),
        );
        g.history(LABEL_SENT, LabelHistory::quiet(100));
        g.body("g-early", alert_eml(now));
        g.body(
            "g-late",
            alert_eml(now).replace("checkout api", "billing api"),
        );
        g.slow_body("g-late", STALL);
        let base = serve_mock(g.clone()).await;

        engine(store.clone(), acct, &base)
            .poll_once()
            .await
            .unwrap();

        let id_of =
            |thread: &str| store.thread_view_with_html(acct, thread).unwrap().messages[0].id;
        let early = stamp_of(&store, acct, id_of("g-early")).expect("stamped at first sight");
        let late = stamp_of(&store, acct, id_of("g-late")).expect("stamped at first sight");

        let gap = late - early;
        assert!(
            gap >= ChronoDuration::milliseconds(STALL.as_millis() as i64 / 2),
            "the second message was fetched {STALL:?} after the first and must be \
             stamped accordingly; the stamps are {gap:?} apart, which is a clock \
             read once for the whole batch"
        );
    }

    /// THE POKE, on its own. Deleting `refine_wake.notify_one()` from
    /// `fetch_raw_and_ingest` used to leave the whole suite green, because
    /// `poll_secs` defaults to 5 and that is also the refine lane's sleep floor:
    /// the lane woke on the timer and every test still passed, with the contract's
    /// "start on new mail immediately" (docs/NOTIFY.md §11.2) quietly downgraded
    /// to "up to poll_secs late".
    ///
    /// Asserted on the `Notify` PERMIT rather than on the lane's timing, which is
    /// what makes it deterministic and free of sleeps in both directions:
    /// `tokio::time::timeout` polls its inner future before its timer, and
    /// `notified()` is immediately ready exactly when a permit is stored. So a
    /// ZERO budget is a synchronous "is there a permit", not a wait.
    #[tokio::test]
    async fn ingesting_mail_pokes_the_refine_lane_on_a_backfill_and_a_walk_alike() {
        let (store, acct) = store_at_cursor(Some(100));
        let now = Utc::now();
        let g = MockGmail::default();
        g.history(LABEL_INBOX, LabelHistory::added(140, &[(140, "g-new")]));
        g.history(LABEL_SENT, LabelHistory::quiet(100));
        g.listing(LABEL_INBOX, &["g-old"]);
        g.listing(LABEL_SENT, &[]);
        g.body("g-new", alert_eml(now));
        g.body(
            "g-old",
            alert_eml(now).replace("checkout api", "billing api"),
        );
        let base = serve_mock(g.clone()).await;
        let eng = engine(store.clone(), acct, &base);

        let poked = || tokio::time::timeout(Duration::ZERO, eng.refine_wake.notified());

        // Nothing has been ingested, so nothing has poked.
        assert!(poked().await.is_err(), "a fresh engine holds no permit");

        // A BACKFILL POKES: the refine lane runs beside the first backfill
        // (`run_once`), and a row it can start on is a row it should be woken
        // for, whether or not the row is genuinely new.
        let n = eng
            .fetch_raw_and_ingest(
                &["g-old".to_string()],
                Mailbox::Inbox,
                IngestOrigin::Backfill,
                Urgency::Live,
            )
            .await
            .unwrap();
        assert_eq!(n, 1, "the backfill really did ingest a row");
        assert!(
            poked().await.is_ok(),
            "a backfilled row must wake the refine lane"
        );

        // An incremental tick that ingests DOES poke, and the permit is stored
        // before `poll_once` even returns, so a lane that is mid-round when it
        // lands still wakes as soon as it finishes.
        eng.poll_once().await.unwrap();
        assert!(
            poked().await.is_ok(),
            "new mail must start through Stage-1 immediately, not at the next \
             poll_secs tick"
        );

        // ...and the permit COALESCES rather than accumulating: one round is
        // queued, not one per message.
        assert!(
            poked().await.is_err(),
            "the permit was consumed by the wait above; Notify holds exactly one"
        );
    }

    /// THE NOTIFICATION LANE'S PRODUCER POKE, through `fetch_raw_and_ingest`.
    /// A backfill queues no `notification` job (its rows are stamped
    /// ineligible), so it does not poke: a poke per backfilled row would be a
    /// lane round per row of the first backfill. Neither does the user's own
    /// sent copy. An incremental arrival does, and the permit is stored
    /// before `poll_once` returns, so a lane that is mid-round when the row
    /// lands wakes as soon as it finishes. One permit, however many rows:
    /// the lane's rounds coalesce.
    #[tokio::test]
    async fn an_incremental_arrival_pokes_the_notification_lane_but_a_backfill_does_not() {
        let (store, acct) = store_at_cursor(Some(100));
        let now = Utc::now();
        let g = MockGmail::default();
        g.history(LABEL_INBOX, LabelHistory::added(140, &[(140, "g-new")]));
        g.history(LABEL_SENT, LabelHistory::quiet(100));
        g.listing(LABEL_INBOX, &["g-old"]);
        g.listing(LABEL_SENT, &[]);
        g.body("g-new", alert_eml(now));
        g.body(
            "g-old",
            alert_eml(now).replace("checkout api", "billing api"),
        );
        let base = serve_mock(g.clone()).await;
        let eng = engine(store.clone(), acct, &base);

        let poked = || tokio::time::timeout(Duration::ZERO, eng.notify_wake.notified());
        assert!(poked().await.is_err(), "a fresh engine holds no permit");

        let n = eng
            .fetch_raw_and_ingest(
                &["g-old".to_string()],
                Mailbox::Inbox,
                IngestOrigin::Backfill,
                Urgency::Live,
            )
            .await
            .unwrap();
        assert_eq!(n, 1, "the backfill really did ingest a row");
        assert!(
            poked().await.is_err(),
            "a backfilled row queues no push and must not wake the lane"
        );

        // Nor does the user's own sent copy, incremental or not: never
        // eligible, never queued.
        g.body("g-sent", alert_eml(now).replace("checkout api", "sent api"));
        let n = eng
            .fetch_raw_and_ingest(
                &["g-sent".to_string()],
                Mailbox::Sent,
                IngestOrigin::Incremental,
                Urgency::Live,
            )
            .await
            .unwrap();
        assert_eq!(n, 1, "the sent copy really did ingest");
        assert!(
            poked().await.is_err(),
            "a sent copy queues no push and must not wake the lane"
        );

        eng.poll_once().await.unwrap();
        assert!(
            poked().await.is_ok(),
            "an incremental arrival's push job must be claimed now, not at \
             the lane's next timer"
        );
        assert!(
            poked().await.is_err(),
            "the permit was consumed by the wait above; Notify holds exactly one"
        );
    }

    /// THE ARCHIVE ORIGIN, through the engine: the row lands as archive with no
    /// work behind it, the refine lane is not woken for it, a catch-up's gauge
    /// does not count it, and a second sighting of the same id ingests nothing.
    #[tokio::test]
    async fn an_archive_ingest_wakes_no_lane_moves_no_gauge_and_queues_no_work() {
        use crate::store::agent_triage::AgentTriageStore;
        let (store, acct) = store_at_cursor(Some(100));
        let g = MockGmail::default();
        g.body("g-old", alert_eml(Utc::now()));
        let base = serve_mock(g.clone()).await;
        let eng = engine(store.clone(), acct, &base);
        // A catch-up is under way, as it may be when the walk's batch lands.
        eng.metrics.catchup_begin(10);

        let n = eng
            .fetch_raw_and_ingest(
                &["g-old".to_string()],
                Mailbox::Inbox,
                IngestOrigin::Archive,
                Urgency::Live,
            )
            .await
            .unwrap();
        assert_eq!(n, 1);
        assert!(
            tokio::time::timeout(Duration::ZERO, eng.refine_wake.notified())
                .await
                .is_err(),
            "an archive row queues nothing, so there is nothing to wake for"
        );
        assert_eq!(eng.metrics.catchup_progress(), Some((0, 10)));
        assert_eq!(
            store
                .stored_gmail_msg_ids(acct, &["g-old".to_string()])
                .unwrap()
                .len(),
            1
        );
        // Fresh by its Date, on a path that is not Backfill: still no stamp,
        // no job, no notification.
        assert!(
            store
                .claim_agent_job(acct, "investigation", Utc::now(), 120)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .claim_agent_job(acct, "notification", Utc::now(), 120)
                .unwrap()
                .is_none()
        );

        // The same id again is counted as handled and writes nothing.
        let raw = "From: alice@example.com\r\nSubject: Note\r\n\r\nHello";
        assert!(
            eng.ingest_one(
                &fixture(acct, "g-old", raw, false),
                &[],
                Utc::now(),
                IngestOrigin::Archive,
            )
            .unwrap()
            .is_none()
        );
    }

    // ---- old bulk mail on a window read -------------------------------------

    /// List mail dated `at`: a newsletter, with the `List-Unsubscribe` header
    /// that makes it bulk and nothing else that would set it apart.
    fn list_eml(at: DateTime<Utc>) -> String {
        list_eml_with(
            at,
            "This week's deals",
            "This week's deals, picked for you.",
        )
    }

    /// List mail dated `at` with its own subject and body: a bill, a booking or
    /// a newsletter, all sent through the same bulk mailer.
    fn list_eml_with(at: DateTime<Utc>, subject: &str, body: &str) -> String {
        format!(
            "From: Shop News <news@shop.example>\r\n\
             To: me@example.com\r\n\
             Subject: {subject}\r\n\
             Date: {}\r\n\
             List-Unsubscribe: <https://shop.example/unsubscribe?u=1>\r\n\
             \r\n\
             {body}\r\n",
            at.to_rfc2822()
        )
    }

    /// A person's mail dated `at`: no List-Unsubscribe, so never bulk.
    fn person_eml(at: DateTime<Utc>) -> String {
        format!(
            "From: Dana Reyes <dana@example.org>\r\n\
             To: me@example.com\r\n\
             Subject: Dinner on Saturday?\r\n\
             Date: {}\r\n\
             \r\n\
             Are you still free Saturday? Let me know.\r\n",
            at.to_rfc2822()
        )
    }

    /// THE FIRST BACKFILL TRIAGES A WEEK OF LIST MAIL AND FILES THE REST. A
    /// new account's month of inbox is mostly newsletters, and a model call
    /// each for the three-week-old ones bought a board nobody shows (a new
    /// tenant spent a month's budget in two days on it). Through the real
    /// signup path: list mail that arrived more than
    /// `sync.backfill_bulk_triage_days` ago lands as an archive row with no
    /// job behind it; list mail inside the week, and a person's mail as old
    /// as the filed newsletter, are queued exactly as before.
    #[tokio::test]
    async fn the_first_backfill_files_list_mail_older_than_a_week_and_triages_the_rest() {
        let now = Utc::now();
        let old = now - ChronoDuration::days(20);
        let recent = now - ChronoDuration::days(2);
        let g = MockGmail::default();
        g.listing(LABEL_INBOX, &["fresh-list", "old-list", "old-person"]);
        g.listing(LABEL_SENT, &[]);
        g.received("fresh-list", recent, list_eml(recent));
        g.received("old-list", old, list_eml(old));
        g.received("old-person", old, person_eml(old));
        g.profile(1);
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(None);
        let eng = engine(store.clone(), acct, &base);

        eng.backfill(&StdMutex::new(None)).await.unwrap();

        let row = |id: &str| store.archive_and_jobs_by_provider_id(acct, id).unwrap();
        assert_eq!(
            row("old-list"),
            Some((true, 0)),
            "three-week-old list mail is stored as archive, with no model work"
        );
        let (archived, jobs) = row("fresh-list").unwrap();
        assert!(
            !archived && jobs > 0,
            "list mail inside the week is triaged"
        );
        let (archived, jobs) = row("old-person").unwrap();
        assert!(
            !archived && jobs > 0,
            "a person's mail is triaged at any age in the window"
        );
        assert_eq!(
            store.triage_by_provider_id(acct, "fresh-list").unwrap(),
            Some((true, false)),
            "the triaged rows are first-load backlog, as they always were"
        );
    }

    /// BILLS AND BOOKINGS SENT THROUGH A BULK MAILER ARE NOT FILED. A
    /// three-week-old bill due next week and an invitation to something next
    /// month carry the same header as the newsletter beside them, and both are
    /// exactly what the first load must show. Ingest extracts no records
    /// itself, so a filed one would be on no board at all. Beside them, a
    /// plain newsletter of the same age IS filed, so the exemption is about
    /// what they say and not the harness.
    #[tokio::test]
    async fn old_list_mail_that_is_a_bill_or_a_booking_is_still_triaged() {
        let now = Utc::now();
        let old = now - ChronoDuration::days(20);
        let due = (now + ChronoDuration::days(7)).format("%B %-d, %Y");
        let when = (now + ChronoDuration::days(30)).format("%a %b %-d, %Y");
        let g = MockGmail::default();
        g.listing(LABEL_INBOX, &["bill", "invite", "news"]);
        g.listing(LABEL_SENT, &[]);
        g.received(
            "bill",
            old,
            list_eml_with(
                old,
                "Your bill is ready",
                &format!("Your bill\r\nAmount due\r\n$67.29\r\nPay by {due}"),
            ),
        );
        g.received(
            "invite",
            old,
            list_eml_with(
                old,
                &format!("Invitation: Concert night @ {when} 7pm - 10pm (PDT)"),
                "You have been invited.",
            ),
        );
        g.received("news", old, list_eml(old));
        g.profile(1);
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(None);
        let eng = engine(store.clone(), acct, &base);

        eng.backfill(&StdMutex::new(None)).await.unwrap();

        let row = |id: &str| store.archive_and_jobs_by_provider_id(acct, id).unwrap();
        for id in ["bill", "invite"] {
            let (archived, jobs) = row(id).unwrap();
            assert!(!archived && jobs > 0, "{id} is triaged, not filed");
        }
        assert_eq!(row("news"), Some((true, 0)), "the newsletter is filed");
    }

    /// AGE IS WHEN THE PROVIDER RECEIVED IT, not what the sender's `Date:`
    /// claims. A newsletter whose mailer stamps a stale Date header but that
    /// arrived two days ago is inside the week, and is triaged.
    #[tokio::test]
    async fn a_stale_date_header_on_recent_list_mail_does_not_file_it() {
        let now = Utc::now();
        let g = MockGmail::default();
        g.received(
            "stale",
            now - ChronoDuration::days(2),
            list_eml(now - ChronoDuration::days(20)),
        );
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(None);
        let eng = engine(store.clone(), acct, &base);
        eng.fetch_raw_and_ingest(
            &["stale".to_string()],
            Mailbox::Inbox,
            IngestOrigin::Backfill,
            Urgency::Window,
        )
        .await
        .unwrap();
        let (archived, jobs) = store
            .archive_and_jobs_by_provider_id(acct, "stale")
            .unwrap()
            .unwrap();
        assert!(!archived && jobs > 0, "arrived two days ago: triaged");
    }

    /// The filed row wakes no lane: it queued nothing. Paired with a row that
    /// DOES wake it, so the silence is about the filing and not the harness.
    #[tokio::test]
    async fn old_list_mail_filed_on_a_window_read_wakes_no_lane() {
        let now = Utc::now();
        let old = now - ChronoDuration::days(20);
        let recent = now - ChronoDuration::days(2);
        let g = MockGmail::default();
        g.received("old-list", old, list_eml(old));
        g.received("fresh-list", recent, list_eml(recent));
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(None);
        let eng = engine(store.clone(), acct, &base);
        let woken = || async {
            tokio::time::timeout(Duration::ZERO, eng.refine_wake.notified())
                .await
                .is_ok()
        };

        eng.fetch_raw_and_ingest(
            &["old-list".to_string()],
            Mailbox::Inbox,
            IngestOrigin::Backfill,
            Urgency::Window,
        )
        .await
        .unwrap();
        assert!(!woken().await, "an archive row queues nothing to wake for");
        eng.fetch_raw_and_ingest(
            &["fresh-list".to_string()],
            Mailbox::Inbox,
            IngestOrigin::Backfill,
            Urgency::Window,
        )
        .await
        .unwrap();
        assert!(woken().await, "a triaged row still wakes the lane");
    }

    /// A CATCH-UP FILES THE SAME WAY, and only what it has never stored. Old
    /// list mail that arrived during an outage costs what it would have cost
    /// on a first run: nothing. A row the store already holds as LIVE stays
    /// live, re-read as every catch-up re-reads it, rather than being demoted.
    /// And a LIVE walk (`Urgency::Live`, the history walk) never files: an
    /// arrival with an old `Date:` is still an arrival.
    #[tokio::test]
    async fn a_catch_up_files_unseen_old_list_mail_but_a_live_walk_and_a_stored_row_do_not() {
        let now = Utc::now();
        let old = now - ChronoDuration::days(20);
        let g = MockGmail::default();
        g.received("stored", old, list_eml(old));
        g.received("unseen", old, list_eml(old));
        g.received("walked", old, list_eml(old));
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(Some(1));
        let eng = engine(store.clone(), acct, &base);

        // Stored live before this daemon filed anything (an older build's
        // first run, say).
        eng.ingest_one(
            &fixture(acct, "stored", &list_eml(old), false),
            &[],
            now,
            IngestOrigin::Backfill,
        )
        .unwrap();
        let before = store
            .archive_and_jobs_by_provider_id(acct, "stored")
            .unwrap();
        assert_eq!(before.map(|(a, _)| a), Some(false));

        eng.fetch_raw_and_ingest(
            &["stored".to_string(), "unseen".to_string()],
            Mailbox::Inbox,
            IngestOrigin::Incremental,
            Urgency::Window,
        )
        .await
        .unwrap();
        assert_eq!(
            store
                .archive_and_jobs_by_provider_id(acct, "unseen")
                .unwrap(),
            Some((true, 0)),
            "unseen old list mail on a catch-up is filed"
        );
        assert_eq!(
            store
                .archive_and_jobs_by_provider_id(acct, "stored")
                .unwrap(),
            before,
            "a live row the catch-up re-reads stays live, its jobs untouched"
        );

        eng.fetch_raw_and_ingest(
            &["walked".to_string()],
            Mailbox::Inbox,
            IngestOrigin::Incremental,
            Urgency::Live,
        )
        .await
        .unwrap();
        let (archived, jobs) = store
            .archive_and_jobs_by_provider_id(acct, "walked")
            .unwrap()
            .unwrap();
        assert!(
            !archived && jobs > 0,
            "a live walk triages whatever it finds"
        );
    }

    /// THE OWNER'S RULES, through the store and the engine's own rule load. A
    /// Surface rule on the newsletter's sender keeps its old mail triaged on
    /// a catch-up; a Squelch rule on another sender files it like any list
    /// mail, because squelching is the opposite of asking to read it.
    #[tokio::test]
    async fn a_surface_rule_keeps_old_list_mail_triaged_and_a_squelch_rule_does_not() {
        let now = Utc::now();
        let old = now - ChronoDuration::days(20);
        let squelched = list_eml(old).replace("news@shop.example", "deals@other.example");
        let g = MockGmail::default();
        g.received("surfaced", old, list_eml(old));
        g.received("squelched", old, squelched);
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(Some(1));
        store
            .set_sender_rule(acct, "news@shop.example", "", Disposition::Surface)
            .unwrap();
        store
            .set_sender_rule(acct, "*@other.example", "", Disposition::Squelch)
            .unwrap();
        let eng = engine(store.clone(), acct, &base);

        eng.fetch_raw_and_ingest(
            &["surfaced".to_string(), "squelched".to_string()],
            Mailbox::Inbox,
            IngestOrigin::Incremental,
            Urgency::Window,
        )
        .await
        .unwrap();
        let (archived, jobs) = store
            .archive_and_jobs_by_provider_id(acct, "surfaced")
            .unwrap()
            .unwrap();
        assert!(!archived && jobs > 0, "the owner asked to see this sender");
        assert_eq!(
            store
                .archive_and_jobs_by_provider_id(acct, "squelched")
                .unwrap(),
            Some((true, 0)),
            "a squelched sender's old list mail is filed"
        );
    }

    /// THE KNOB: at `backfill_days` the whole window is triaged again, which
    /// is the documented way back to the old behaviour.
    #[tokio::test]
    async fn backfill_bulk_triage_days_at_the_window_triages_all_of_it() {
        let now = Utc::now();
        let old = now - ChronoDuration::days(20);
        let g = MockGmail::default();
        g.received("old-list", old, list_eml(old));
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(None);
        let mut config = Config::default();
        config.sync.backfill_bulk_triage_days = config.sync.backfill_days;
        let eng = engine_with_config(store.clone(), acct, &base, config);
        eng.fetch_raw_and_ingest(
            &["old-list".to_string()],
            Mailbox::Inbox,
            IngestOrigin::Backfill,
            Urgency::Window,
        )
        .await
        .unwrap();
        let (archived, jobs) = store
            .archive_and_jobs_by_provider_id(acct, "old-list")
            .unwrap()
            .unwrap();
        assert!(!archived && jobs > 0);
    }

    /// [`files_as_old_bulk`] condition by condition: only list mail, only
    /// arrived before the floor, only with a floor, never the owner's own
    /// mail or spam, a blank header is none, and each of the four record
    /// detectors (a receipt and a shipment here; the bill and the booking
    /// go through the engine above) keeps it out.
    #[test]
    fn files_as_old_bulk_asks_for_every_condition() {
        let now = Utc::now();
        let floor = Some(now - ChronoDuration::days(7));
        let old = now - ChronoDuration::days(8);
        let parse = |eml: &str| {
            ingest_with_rules(
                &fixture(1, "m", eml, false),
                &crate::config::Stage1Config::default(),
                now,
                &[],
                |_| false,
            )
        };
        let news = parse(&list_eml(old));
        assert!(files_as_old_bulk(&news, old, floor, &[]));
        assert!(!files_as_old_bulk(&news, old, None, &[]), "no floor");
        assert!(
            !files_as_old_bulk(&news, now - ChronoDuration::days(6), floor, &[]),
            "arrived inside the week"
        );
        let person = parse(&person_eml(old));
        assert!(
            !files_as_old_bulk(&person, old, floor, &[]),
            "not list mail"
        );

        let mut sent = news.clone();
        sent.message.is_sent = true;
        assert!(!files_as_old_bulk(&sent, old, floor, &[]));
        let mut spam = news.clone();
        spam.message.is_spam = true;
        assert!(!files_as_old_bulk(&spam, old, floor, &[]));
        let mut blank = news.clone();
        blank.message.list_unsubscribe = Some("  ".into());
        assert!(!files_as_old_bulk(&blank, old, floor, &[]), "blank header");

        let receipt = parse(&list_eml_with(
            old,
            "Your Bay Wheels ride receipt",
            "Thanks for riding! Receipt for your ride. Total: $3.49. Ride time 12 min.",
        ));
        assert!(!files_as_old_bulk(&receipt, old, floor, &[]), "a receipt");
        let shipment = parse(&list_eml_with(
            old,
            "Your UPS package has shipped",
            "Tracking number: 1Z999AA10123456784. Track your package.",
        ));
        assert!(!files_as_old_bulk(&shipment, old, floor, &[]), "a shipment");
    }

    // ---- the archive walk ---------------------------------------------------

    /// Every wait in the walk shrunk to nothing, so these tests assert on the
    /// wire and the store instead of sleeping.
    fn instant_pace() -> ArchivePace {
        ArchivePace {
            rest_factor: 0,
            error_backoff: Duration::from_millis(10),
            error_backoff_cap: Duration::from_millis(80),
            held_recheck: Duration::from_millis(5),
            ..ArchivePace::default()
        }
    }

    /// An engine whose poll has "succeeded" and whose walk does not rest.
    fn archive_engine(
        store: Arc<SqliteStore>,
        acct: AccountId,
        base: &str,
        archive_days: u32,
    ) -> SyncEngine<SqliteStore, FixedToken> {
        let mut config = Config::default();
        config.sync.archive_days = archive_days;
        let mut eng = engine_with_config(store, acct, base, config);
        eng.archive_pace = instant_pace();
        eng.poll_healthy.store(true, Ordering::Relaxed);
        eng
    }

    /// Disk numbers in MiB, for the walk's `disk_probe`.
    fn disk_mb(free: u64, total: u64, db: u64) -> crate::store::ArchiveDisk {
        const MIB: u64 = 1024 * 1024;
        crate::store::ArchiveDisk {
            free_bytes: free * MIB,
            total_bytes: total * MIB,
            db_bytes: db * MIB,
        }
    }

    /// A shutdown channel nobody fires. The sender is returned to be held: a
    /// dropped one reads as "changed" forever.
    fn never_shutdown() -> (
        tokio::sync::watch::Sender<bool>,
        tokio::sync::watch::Receiver<bool>,
    ) {
        tokio::sync::watch::channel(false)
    }

    /// A whole-second instant `days` ago, so it survives the cursor's round
    /// trip and the query's epoch seconds exactly.
    fn days_ago(days: i64) -> DateTime<Utc> {
        DateTime::from_timestamp((Utc::now() - ChronoDuration::days(days)).timestamp(), 0).unwrap()
    }

    fn archive_q(from: DateTime<Utc>, to: DateTime<Utc>) -> String {
        format!(
            "after:{} before:{} -in:draft -in:chats",
            from.timestamp() - 1,
            to.timestamp()
        )
    }

    /// THE WALK, end to end: mail older than the backfill window, in no label
    /// the live paths list, lands as archive rows (received and sent alike)
    /// with no model work behind any of them, and the walk then reports done.
    #[tokio::test]
    async fn the_archive_walk_stores_old_mail_as_archive_rows_and_queues_nothing() {
        use crate::store::agent_triage::AgentTriageStore;
        let (store, acct) = store_at_cursor(Some(100));
        let g = MockGmail::default();
        g.old_mail("g-old-in", days_ago(40), alert_eml(days_ago(40)));
        g.old_mail(
            "g-old-out",
            days_ago(55),
            sent_eml(days_ago(55), "alice@friends.com", "the lease"),
        );
        g.message_labels("g-old-out", &["SENT"]);
        let base = serve_mock(g.clone()).await;
        let eng = archive_engine(store.clone(), acct, &base, 60);
        let (_tx, mut rx) = never_shutdown();

        let mut steps = 0;
        while eng.archive_walk_chunk(&mut rx).await.unwrap() == ArchiveStep::Walked {
            steps += 1;
            assert!(steps < 20, "the walk must reach its target");
        }
        // 30 days of backfill to 60 days of archive, a week at a time.
        assert_eq!(steps, 5);
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Done
        );

        for (gmail_id, sent) in [("g-old-in", false), ("g-old-out", true)] {
            let view = store.thread_view_with_html(acct, gmail_id).unwrap();
            assert_eq!(view.messages.len(), 1, "{gmail_id} is openable");
            assert!(
                store.message_is_archive(acct, view.messages[0].id).unwrap(),
                "{gmail_id} is an archive row"
            );
            assert_eq!(
                view.messages[0].is_sent, sent,
                "{gmail_id}: its own labels decide"
            );
        }
        for kind in ["investigation", "notification"] {
            assert!(
                store
                    .claim_agent_job(acct, kind, Utc::now(), 120)
                    .unwrap()
                    .is_none(),
                "no {kind} job for a year of old mail"
            );
        }
        // A contact is someone the user writes to NOW; the archive seeds none.
        assert!(!store.is_known_contact(acct, "alice@friends.com").unwrap());
        // Its own gauges, and not the catch-up's.
        let (at, complete) = eng.metrics.archive_walk().unwrap();
        assert!(complete);
        assert!(at <= days_ago(60).timestamp() + 5);
        assert_eq!(eng.metrics.catchup_progress(), None);
    }

    /// ON THE WIRE: all mail, so no `labelIds`, bounded to exactly one week
    /// back from the cursor in epoch seconds, drafts and chats excluded.
    #[tokio::test]
    async fn the_archive_listing_carries_no_label_and_one_week_of_bounds() {
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let g = MockGmail::default();
        let base = serve_mock(g.clone()).await;
        let eng = archive_engine(store.clone(), acct, &base, 365);
        let (_tx, mut rx) = never_shutdown();

        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert_eq!(g.seen(), vec!["list-all"], "one listing, with no labelIds");
        assert_eq!(
            g.queries(""),
            vec![archive_q(cursor - ChronoDuration::days(7), cursor)]
        );
    }

    /// THE QUOTA SAVING AND THE RESUME: an id the store already holds, from
    /// the live window or from an earlier pass over this chunk, is never
    /// fetched again.
    #[tokio::test]
    async fn the_archive_walk_does_not_fetch_ids_it_already_holds() {
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let at = cursor - ChronoDuration::days(1);
        let g = MockGmail::default();
        g.old_mail("g-have", at, alert_eml(at));
        g.old_mail("g-new", at, alert_eml(at));
        let base = serve_mock(g.clone()).await;
        ingest_into(
            &store,
            acct,
            &fixture(acct, "g-have", &alert_eml(at), false),
            at,
        );
        let eng = archive_engine(store.clone(), acct, &base, 365);
        let (_tx, mut rx) = never_shutdown();

        eng.archive_walk_chunk(&mut rx).await.unwrap();
        assert_eq!(g.calls("get:g-have"), 0, "already stored, so not fetched");
        assert_eq!(g.calls("get:g-new"), 1);
        let have = store.thread_view_with_html(acct, "g-have").unwrap();
        assert!(
            !store.message_is_archive(acct, have.messages[0].id).unwrap(),
            "and a live row is never demoted"
        );
    }

    /// THE CURSOR: created at the backfill window's edge on first use, moved
    /// back one chunk per fully ingested chunk, and picked up by the next run
    /// (a new engine over the same store) without re-fetching anything.
    #[tokio::test]
    async fn the_archive_cursor_is_persisted_per_chunk_and_resumed() {
        let (store, acct) = store_at_cursor(Some(100));
        let g = MockGmail::default();
        g.old_mail("g-week1", days_ago(33), alert_eml(days_ago(33)));
        g.old_mail("g-week2", days_ago(40), alert_eml(days_ago(40)));
        let base = serve_mock(g.clone()).await;
        let (_tx, mut rx) = never_shutdown();

        assert_eq!(store.archive_walked_to(acct).unwrap(), None);
        let first = archive_engine(store.clone(), acct, &base, 365);
        // The row is written on first use, before the first listing, so a walk
        // whose first chunk fails still has a fixed place to start from.
        g.0.lock().unwrap().all_mail_failures = 1;
        assert!(first.archive_walk_chunk(&mut rx).await.is_err());
        let start = store.archive_walked_to(acct).unwrap().unwrap();
        assert!(
            (start - (Utc::now() - ChronoDuration::days(30)))
                .num_seconds()
                .abs()
                < 30,
            "created at the backfill window's edge, got {start}"
        );
        assert_eq!(
            first.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        let walked = store.archive_walked_to(acct).unwrap().unwrap();
        let expected = Utc::now() - ChronoDuration::days(30 + 7);
        assert!(
            (walked - expected).num_seconds().abs() < 30,
            "one chunk back from the backfill window's edge, got {walked}"
        );
        assert_eq!(g.calls("get:g-week1"), 1);
        assert_eq!(g.calls("get:g-week2"), 0, "the next chunk's mail");

        // A restart: nothing of the first engine survives but the store.
        let second = archive_engine(store.clone(), acct, &base, 365);
        assert_eq!(
            second.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert_eq!(
            g.queries("")[2],
            archive_q(walked - ChronoDuration::days(7), walked),
            "the second run starts where the first one stopped"
        );
        assert_eq!(g.calls("get:g-week1"), 1, "not fetched a second time");
        assert_eq!(g.calls("get:g-week2"), 1);
        assert_eq!(
            store.archive_walked_to(acct).unwrap().unwrap(),
            walked - ChronoDuration::days(7)
        );
    }

    /// THE FAR END: the last chunk is cut at `now - archive_days` rather than
    /// running a week past it, and a finished walk asks Gmail nothing.
    #[tokio::test]
    async fn the_archive_walk_stops_at_its_target() {
        let (store, acct) = store_at_cursor(Some(100));
        let g = MockGmail::default();
        // Past the target: listed by nothing, so never fetched.
        g.old_mail("g-too-old", days_ago(44), alert_eml(days_ago(44)));
        let base = serve_mock(g.clone()).await;
        // 30 days of backfill, 40 of archive: one whole week and one of three days.
        let eng = archive_engine(store.clone(), acct, &base, 40);
        let (_tx, mut rx) = never_shutdown();

        for _ in 0..2 {
            assert_eq!(
                eng.archive_walk_chunk(&mut rx).await.unwrap(),
                ArchiveStep::Walked
            );
        }
        let walked = store.archive_walked_to(acct).unwrap().unwrap();
        let target = Utc::now() - ChronoDuration::days(40);
        assert!(
            (walked - target).num_seconds().abs() < 30,
            "the cursor stops at the target, got {walked}"
        );
        for _ in 0..3 {
            assert_eq!(
                eng.archive_walk_chunk(&mut rx).await.unwrap(),
                ArchiveStep::Done
            );
        }
        assert_eq!(g.calls("list-all"), 2, "a finished walk lists nothing");
        assert_eq!(g.calls("get:g-too-old"), 0);
        assert_eq!(eng.metrics.archive_walk().map(|(_, done)| done), Some(true));

        // RAISING archive_days resumes it from the same cursor.
        let wider = archive_engine(store.clone(), acct, &base, 60);
        assert_eq!(
            wider.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert_eq!(g.calls("get:g-too-old"), 1);
    }

    /// A multipart/alternative message with a text half and an HTML half, both
    /// saying `word`.
    fn html_eml(at: DateTime<Utc>, word: &str) -> String {
        format!(
            "From: Harbour Office <office@harbour.example>\r\n\
             To: me@example.com\r\n\
             Subject: Mooring invoice\r\n\
             Date: {}\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/alternative; boundary=\"b\"\r\n\
             \r\n\
             --b\r\n\
             Content-Type: text/plain; charset=\"UTF-8\"\r\n\
             \r\n\
             The {word} mooring is paid.\r\n\
             --b\r\n\
             Content-Type: text/html; charset=\"UTF-8\"\r\n\
             \r\n\
             <table><tr><td>The <b>{word}</b> mooring is paid.</td></tr></table>\r\n\
             --b--\r\n",
            at.to_rfc2822()
        )
    }

    /// FIVE YEARS. A walk that finished at the old one-year default is not
    /// finished at the new one: its cursor stays where it stopped, the target
    /// moves back four years, and the next chunk carries on from the cursor.
    /// What it stores out there is LITE: text and index, no HTML. Inside the
    /// year an archive row keeps its HTML as it always did.
    #[tokio::test]
    async fn a_walk_finished_at_one_year_extends_to_five_and_stores_old_mail_lite() {
        let (store, acct) = store_at_cursor(Some(100));
        // Where a 365-day walk stopped: a little past its target.
        let finished = days_ago(365) - ChronoDuration::hours(1);
        store.set_archive_walked_to(acct, finished).unwrap();
        let at = finished - ChronoDuration::days(2);
        let g = MockGmail::default();
        g.old_mail("g-2024", at, html_eml(at, "kittiwake"));
        let base = serve_mock(g.clone()).await;
        let (_tx, mut rx) = never_shutdown();

        let one_year = archive_engine(store.clone(), acct, &base, 365);
        assert_eq!(
            one_year.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Done,
            "finished, at a year"
        );
        assert_eq!(g.calls("list-all"), 0);

        let default_days = Config::default().sync.archive_days;
        assert_eq!(default_days, 1825, "five years by default");
        let five = archive_engine(store.clone(), acct, &base, default_days);
        assert_eq!(
            five.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked,
            "not finished at five"
        );
        assert_eq!(
            g.queries(""),
            vec![archive_q(finished - ChronoDuration::days(7), finished)],
            "resumed from the cursor the one-year walk left"
        );
        assert_eq!(g.calls("get:g-2024"), 1);
        assert_eq!(
            store.archive_walked_to(acct).unwrap(),
            Some(finished - ChronoDuration::days(7))
        );
        assert_eq!(
            five.metrics.archive_walk().map(|(_, done)| done),
            Some(false)
        );

        let view = store.thread_view_with_html(acct, "g-2024").unwrap();
        let m = &view.messages[0];
        assert!(store.message_is_archive(acct, m.id).unwrap());
        assert!(m.lite, "more than a year back: lite");
        assert_eq!(m.html, None, "its HTML is not kept");
        assert!(m.content.contains("kittiwake"), "its text is");
        let hits = store.search(acct, "kittiwake", 10, 0).unwrap();
        assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), [m.id]);

        // INSIDE THE YEAR, the walk keeps the HTML as it always has.
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let at = cursor - ChronoDuration::days(1);
        let g = MockGmail::default();
        g.old_mail("g-recent", at, html_eml(at, "fulmar"));
        let base = serve_mock(g.clone()).await;
        let eng = archive_engine(store.clone(), acct, &base, default_days);
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        let view = store.thread_view_with_html(acct, "g-recent").unwrap();
        assert!(!view.messages[0].lite);
        assert!(
            view.messages[0]
                .html
                .as_deref()
                .is_some_and(|h| h.contains("fulmar"))
        );
    }

    /// THE GUARDS HOLD AT FIVE YEARS. The floor and the ceiling are asked per
    /// batch whatever the target is, so a walk out in its third year stops
    /// for a short disk or a full database exactly as the first year's did,
    /// with nothing fetched and the cursor where it was.
    #[tokio::test]
    async fn the_five_year_walk_still_holds_at_the_disk_floor_and_its_ceiling() {
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(800);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let at = cursor - ChronoDuration::days(1);
        let g = MockGmail::default();
        g.old_mail("g-old", at, html_eml(at, "gannet"));
        let base = serve_mock(g.clone()).await;
        let eng = archive_engine(store.clone(), acct, &base, 1825);
        let (_tx, mut rx) = never_shutdown();

        for full in [
            // Under the 2 GiB floor.
            (|| Some(disk_mb(2047, 0, 0))) as fn() -> Option<crate::store::ArchiveDisk>,
            // A fifth of a 49 GiB tenant volume is the floor there: 5 GiB.
            || Some(disk_mb(5 * 1024 - 1, 49 * 1024, 0)),
            // At the 4 GiB database ceiling.
            || Some(disk_mb(u64::MAX >> 20, 0, 4096)),
            // Free space that cannot be read.
            || None,
        ] {
            eng.archive.state().disk_probe = Some(full);
            assert_eq!(
                eng.archive_walk_chunk(&mut rx).await.unwrap(),
                ArchiveStep::Held
            );
        }
        assert_eq!(g.calls("get:g-old"), 0, "held means nothing fetched");
        assert_eq!(store.archive_walked_to(acct).unwrap(), Some(cursor));

        eng.archive.state().disk_probe = Some(|| Some(crate::store::ArchiveDisk::UNBOUNDED));
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert_eq!(g.calls("get:g-old"), 1);
    }

    /// `archive_days = 0` IS OFF: no listing, no cursor row, no gauge.
    #[tokio::test]
    async fn an_archive_of_zero_days_does_nothing() {
        let (store, acct) = store_at_cursor(Some(100));
        let g = MockGmail::default();
        g.old_mail("g-old", days_ago(40), alert_eml(days_ago(40)));
        let base = serve_mock(g.clone()).await;
        let eng = archive_engine(store.clone(), acct, &base, 0);
        let (_tx, mut rx) = never_shutdown();

        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Off
        );
        assert!(g.seen().is_empty(), "saw {:?}", g.seen());
        assert_eq!(store.archive_walked_to(acct).unwrap(), None);
        assert_eq!(eng.metrics.archive_walk(), None);
    }

    /// THE WALK YIELDS. Before a poll has succeeded and while a window of mail
    /// is being fetched it asks Gmail nothing; and a window that opens in the
    /// middle of a chunk stops the chunk at the next batch boundary with the
    /// cursor where it was.
    #[tokio::test]
    async fn the_archive_walk_holds_off_while_a_window_is_in_flight() {
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let at = cursor - ChronoDuration::days(1);
        let g = MockGmail::default();
        g.old_mail("g-a", at, alert_eml(at));
        g.old_mail("g-b", at, alert_eml(at));
        // The first batch takes long enough that its rest (twice its elapsed
        // time) is a window the test can reliably land a flag in.
        g.slow_body("g-a", Duration::from_millis(60));
        let base = serve_mock(g.clone()).await;
        let mut eng = archive_engine(store.clone(), acct, &base, 365);
        eng.archive_pace.batch = 1;
        eng.archive_pace.rest_factor = 2;
        let (_tx, mut rx) = never_shutdown();

        eng.poll_healthy.store(false, Ordering::Relaxed);
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Held,
            "no poll has succeeded"
        );
        eng.poll_healthy.store(true, Ordering::Relaxed);
        eng.window_in_flight.store(true, Ordering::Relaxed);
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Held,
            "a catch-up is running"
        );
        assert!(
            g.seen().is_empty(),
            "held means off the wire: {:?}",
            g.seen()
        );

        // Now mid-chunk: the window opens while the walk rests after g-a.
        eng.window_in_flight.store(false, Ordering::Relaxed);
        let (step, ()) = tokio::join!(eng.archive_walk_chunk(&mut rx), async {
            while g.calls("get:g-a") == 0 {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            eng.window_in_flight.store(true, Ordering::Relaxed);
        });
        assert_eq!(step.unwrap(), ArchiveStep::Held);
        assert_eq!(g.calls("get:g-b"), 0, "the second batch was not started");
        assert_eq!(
            store.archive_walked_to(acct).unwrap(),
            Some(cursor),
            "an unfinished chunk does not move the cursor"
        );

        // The window closes; the chunk is finished from where it stopped.
        eng.window_in_flight.store(false, Ordering::Relaxed);
        eng.archive_pace.rest_factor = 0;
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert_eq!(g.calls("get:g-a"), 1);
        assert_eq!(g.calls("get:g-b"), 1);
    }

    /// THE DUTY CYCLE: a burst of work is followed by a rest of twice its own
    /// length, so the walk holds about a third of whatever the shared budget
    /// allows. Measured on a real clock because the fetch is real I/O.
    #[tokio::test]
    async fn the_archive_walk_rests_twice_as_long_as_it_worked() {
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let at = cursor - ChronoDuration::days(1);
        let g = MockGmail::default();
        for id in ["g-slow-a", "g-slow-b"] {
            g.old_mail(id, at, alert_eml(at));
            g.slow_body(id, Duration::from_millis(50));
        }
        let base = serve_mock(g.clone()).await;
        let mut eng = archive_engine(store.clone(), acct, &base, 365);
        // Two batches, so both rests are in the measurement: the one between
        // batches and the one after the cursor is written.
        eng.archive_pace.batch = 1;
        eng.archive_pace.rest_factor = 2;
        let (_tx, mut rx) = never_shutdown();

        let started = std::time::Instant::now();
        eng.archive_walk_chunk(&mut rx).await.unwrap();
        assert!(
            started.elapsed() >= Duration::from_millis(300),
            "twice 50ms of work, each followed by at least 100ms of rest, took {:?}",
            started.elapsed()
        );
    }

    /// A GMAIL ERROR MID-CHUNK leaves the cursor where it was, keeps what was
    /// already ingested, and the retry pays only for the rest.
    #[tokio::test]
    async fn a_gmail_error_in_the_archive_walk_does_not_advance_the_cursor() {
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let at = cursor - ChronoDuration::days(1);
        let g = MockGmail::default();
        g.old_mail("g-ok", at, alert_eml(at));
        g.old_mail("g-bad", at, alert_eml(at));
        g.0.lock().unwrap().errors.insert(
            "g-bad".to_string(),
            std::collections::VecDeque::from([(500, "backendError".to_string())]),
        );
        let base = serve_mock(g.clone()).await;
        let eng = archive_engine(store.clone(), acct, &base, 365);
        let (_tx, mut rx) = never_shutdown();

        assert!(eng.archive_walk_chunk(&mut rx).await.is_err());
        assert_eq!(store.archive_walked_to(acct).unwrap(), Some(cursor));

        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert_eq!(
            store.archive_walked_to(acct).unwrap(),
            Some(cursor - ChronoDuration::days(7))
        );
        assert_eq!(
            g.calls("get:g-ok"),
            1,
            "ingested before the error, not re-fetched"
        );
        assert_eq!(g.calls("get:g-bad"), 2);
    }

    /// A REFUSAL INSIDE THE WALK IS NOT A SYNC OUTAGE. It is counted, and the
    /// client-visible last error stays clear: the inbox is current, and a 500
    /// on a year-old week is the walk's to retry.
    #[tokio::test]
    async fn a_gmail_error_in_the_archive_walk_is_counted_but_not_shown_as_a_sync_error() {
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let at = cursor - ChronoDuration::days(1);
        let g = MockGmail::default();
        g.old_mail("g-bad", at, alert_eml(at));
        g.0.lock().unwrap().errors.insert(
            "g-bad".to_string(),
            std::collections::VecDeque::from([(500, "backendError".to_string())]),
        );
        g.0.lock().unwrap().all_mail_failures = 1;
        let base = serve_mock(g.clone()).await;
        let metrics = SyncMetrics::new();
        let eng = archive_engine(store.clone(), acct, &base, 365).with_metrics(metrics.clone());
        let (_tx, mut rx) = never_shutdown();
        let http_errors = |n: u32| format!("squelchd_gmail_api_errors_total{{kind=\"http\"}} {n}");

        // The listing is refused, then the fetch is.
        assert!(eng.archive_walk_chunk(&mut rx).await.is_err());
        assert!(eng.archive_walk_chunk(&mut rx).await.is_err());
        assert_eq!(metrics.sync_last_error(), None);
        assert_eq!(metrics.sync_consecutive_failures(), 0);
        let rendered = crate::metrics::render(&metrics, None);
        assert!(rendered.contains(&http_errors(2)), "{rendered}");

        // The same refusal on a live path is one, so the difference is the walk.
        g.0.lock().unwrap().errors.insert(
            "g-live".to_string(),
            std::collections::VecDeque::from([(500, "backendError".to_string())]),
        );
        assert!(
            eng.fetch_raw_and_ingest(
                &["g-live".to_string()],
                Mailbox::Inbox,
                IngestOrigin::Incremental,
                Urgency::Live
            )
            .await
            .is_err()
        );
        assert!(metrics.sync_last_error().is_some());
    }

    /// THE AGENT DOOR'S POKE, through the real refine lane and the real access
    /// job: a search that surfaced an archive row wakes the lane, the lane
    /// spends ONE small model call on it, and the search's wait returns with
    /// the row readable. The lane's own clock here is 30 seconds, so only the
    /// poke can explain the answer arriving inside the wait.
    #[tokio::test]
    async fn the_agent_doors_poke_wakes_the_refine_lane_to_run_the_archive_access_check() {
        use crate::store::agent_triage::AgentTriageStore;
        use crate::store::archive_surface::{SurfaceWait, surface_archive_matches};
        let (store, account) = store_at_cursor(Some(100));
        let at = days_ago(200);
        let mut fetched = fixture(account, "g-old", &alert_eml(at), false);
        fetched.internal_date = Some(at);
        let parsed = ingest_with_rules(&fetched, &Stage1Config::default(), at, &[], |_| false);
        let id = store.ingest_archive_message(&parsed).unwrap().unwrap();

        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = calls.clone();
        let app = Router::new().route(
            "/",
            axum::routing::post(move |Json(_): Json<serde_json::Value>| {
                let counted = counted.clone();
                async move {
                    counted.fetch_add(1, Ordering::Relaxed);
                    let verdict = json!({"restricted":false,"reason":"An ordinary alert"});
                    Json(json!({"choices":[{"message":{"content":verdict.to_string()},"finish_reason":"stop"}],
                        "usage":{"prompt_tokens":20,"completion_tokens":10}}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm = ResolvedLlm {
            url: format!("http://{}/", listener.local_addr().unwrap()),
            api_key: "test".into(),
            provider: Stage2Provider::OpenAI,
        };
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut config = Config::default();
        config.triage.agent.worker_poll_secs = 3600;
        let poke = crate::store::archive_surface::RefinePoke::new();
        let mut eng = engine_with_config(store.clone(), account, "http://127.0.0.1:1", config)
            .with_refine_poke(poke.clone());
        eng.stage2_llm = Some(llm);
        eng.poll_healthy.store(true, Ordering::Relaxed);
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        assert!(!poke.runnable(), "no lane has spoken for it yet");

        let ((), surfaced) = tokio::join!(eng.refine_lane(&mut rx), async {
            // Let the lane finish its first, empty round and park.
            tokio::time::sleep(Duration::from_millis(300)).await;
            // THE LANE SAID IT CAN RUN, which is the only reason the door
            // below waits at all.
            assert!(poke.runnable());
            let surfacing = crate::store::archive_surface::ArchiveSurfacing {
                archive_days: 365,
                poke: Some(poke.clone()),
                wait: SurfaceWait {
                    deadline: Duration::from_secs(10),
                    idle: Duration::from_secs(10),
                    interval: Duration::from_millis(20),
                },
                caps: Default::default(),
                features: Default::default(),
            };
            let started = std::time::Instant::now();
            let rerun = surface_archive_matches(&store, account, &[id], &surfacing)
                .await
                .unwrap();
            let waited = started.elapsed();
            tx.send(true).unwrap();
            (rerun, waited)
        });
        let (rerun, waited) = surfaced;
        assert!(rerun);
        assert!(
            !poke.runnable(),
            "the lane withdrew its word on the way out"
        );
        assert!(
            waited < Duration::from_secs(8),
            "the check finished inside the wait: {waited:?}"
        );
        assert!(store.agent_access_allowed(account, id).unwrap());
        assert_eq!(calls.load(Ordering::Relaxed), 1, "one small model call");
        assert_eq!(
            store.agent_diagnostics(account, id).unwrap()["jobs"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "and one job"
        );
        server.abort();
    }

    /// WHAT THE LANE TELLS THE DOORS. A door waits on its access checks only
    /// while the lane says a poke would be acted on, and the lane says so from
    /// the three facts that gate its own spending: a poll has succeeded, a
    /// model is configured, the provider circuit is closed. Each one missing
    /// is a lane that would sit on the queue while the caller's wait ran out.
    #[tokio::test]
    async fn the_refine_lane_says_whether_a_poke_would_be_acted_on() {
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let account = store.ensure_account("me@localhost").unwrap();
        let llm = || ResolvedLlm {
            url: "http://127.0.0.1:1/".into(),
            api_key: "test".into(),
            provider: Stage2Provider::OpenAI,
        };
        // (poll healthy, a model, circuit open) -> runnable
        for (healthy, model, cooling, expected) in [
            (true, true, false, true),
            (false, true, false, false),
            (true, false, false, false),
            (true, true, true, false),
        ] {
            let mut config = Config::default();
            config.triage.agent.worker_poll_secs = 3600;
            let poke = crate::store::archive_surface::RefinePoke::new();
            let mut eng = engine_with_config(store.clone(), account, "http://127.0.0.1:1", config)
                .with_refine_poke(poke.clone());
            eng.stage2_llm = model.then(llm);
            eng.poll_healthy.store(healthy, Ordering::Relaxed);
            if cooling {
                eng.agent_retry_after
                    .store(Utc::now().timestamp() + 600, Ordering::Relaxed);
            }
            let (tx, mut rx) = tokio::sync::watch::channel(false);
            let ((), said) = tokio::join!(eng.refine_lane(&mut rx), async {
                tokio::time::sleep(Duration::from_millis(150)).await;
                let said = poke.runnable();
                tx.send(true).unwrap();
                said
            });
            assert_eq!(
                said, expected,
                "healthy={healthy} model={model} cooling={cooling}"
            );
            assert!(!poke.runnable(), "withdrawn when the lane ends");
        }
    }

    /// THE LANE, inside the real `select!`: a Gmail error in the walk is waited
    /// out and retried by the walk, and the lifecycle neither ends nor errors
    /// for it. It ends here because the test says stop, with `Ok`.
    #[tokio::test]
    async fn a_failing_archive_walk_does_not_bounce_the_lifecycle() {
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let at = cursor - ChronoDuration::days(1);
        let g = MockGmail::default();
        g.history(LABEL_INBOX, LabelHistory::quiet(100));
        g.history(LABEL_SENT, LabelHistory::quiet(100));
        g.old_mail("g-old", at, alert_eml(at));
        g.0.lock().unwrap().all_mail_failures = 2;
        let base = serve_mock(g.clone()).await;
        // 47 days: the one chunk under test and then done, so the lane goes
        // quiet instead of walking a year of empty weeks behind the assertions.
        let mut eng = archive_engine(store.clone(), acct, &base, 47);
        // The lane must earn this from the poll lane, as in production.
        eng.poll_healthy.store(false, Ordering::Relaxed);
        eng.archive_pace.done_recheck = Duration::from_secs(3600);
        let (tx, mut rx) = tokio::sync::watch::channel(false);

        let (lifecycle, landed) = tokio::join!(eng.run_lanes(&mut rx, false), async {
            let landed = tokio::time::timeout(Duration::from_secs(10), async {
                while g.calls("get:g-old") == 0 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .is_ok();
            tx.send(true).unwrap();
            landed
        });
        assert!(
            lifecycle.is_ok(),
            "the walk's errors are not the lifecycle's"
        );
        assert!(
            landed,
            "the walk retried past two failed listings: {:?}",
            g.seen()
        );
        // Two refused listings, then whatever the forward pass listed on its
        // way to the bound, then the one backward chunk under test.
        assert!(g.calls("list-all") >= 3, "{:?}", g.seen());
        let backward = g
            .queries("")
            .into_iter()
            .filter(|q| !q.contains("-in:inbox"))
            .count();
        assert!(backward >= 1, "{:?}", g.queries(""));
        assert!(
            g.at("history:INBOX") < g.at("list-all"),
            "the walk waited for a poll to succeed: {:?}",
            g.seen()
        );
    }

    // ---- the walk's limits: disk, embedder, clock, quota, poison -----------

    /// A message with one real attachment of `bytes` bytes, base64 on the wire.
    fn attachment_eml(at: DateTime<Utc>, bytes: usize) -> String {
        let encoded = base64::engine::general_purpose::STANDARD.encode(vec![0x5a_u8; bytes]);
        let mut wrapped = String::with_capacity(encoded.len() + encoded.len() / 38);
        for line in encoded.as_bytes().chunks(76) {
            wrapped.push_str(std::str::from_utf8(line).unwrap());
            wrapped.push_str("\r\n");
        }
        format!(
            "From: Scans <scans@office.example>\r\n\
             To: me@example.com\r\n\
             Subject: the signed lease\r\n\
             Date: {}\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"mix\"\r\n\
             \r\n\
             --mix\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\
             \r\n\
             The countersigned copy is attached.\r\n\
             --mix\r\n\
             Content-Type: application/pdf; name=\"lease.pdf\"\r\n\
             Content-Disposition: attachment; filename=\"lease.pdf\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             {wrapped}--mix--\r\n",
            at.to_rfc2822()
        )
    }

    /// AN ARCHIVE ATTACHMENT IS A NAME AND A SIZE. A real 3 MB part, well
    /// under the ingest cap, keeps its bytes on the live path and none in the
    /// archive: the reader lists it as not downloadable and the byte read
    /// answers "not stored", which the endpoint turns into a 410.
    #[tokio::test]
    async fn the_archive_walk_stores_attachments_as_metadata_only() {
        const SIZE: usize = 3 * 1024 * 1024;
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let at = cursor - ChronoDuration::days(1);
        let g = MockGmail::default();
        g.old_mail("g-old", at, attachment_eml(at, SIZE));
        g.body("g-live", attachment_eml(Utc::now(), SIZE));
        let base = serve_mock(g.clone()).await;
        let eng = archive_engine(store.clone(), acct, &base, 365);
        let (_tx, mut rx) = never_shutdown();
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        eng.fetch_raw_and_ingest(
            &["g-live".to_string()],
            Mailbox::Inbox,
            IngestOrigin::Incremental,
            Urgency::Live,
        )
        .await
        .unwrap();

        let part = |gmail_id: &str| {
            let view = store.thread_view_with_html(acct, gmail_id).unwrap();
            let listed = view.messages[0].attachments.clone();
            assert_eq!(listed.len(), 1, "{gmail_id}: the part is listed");
            assert_eq!(listed[0].filename, "lease.pdf");
            assert_eq!(listed[0].size, SIZE as i64, "{gmail_id}: the real size");
            let (_, _, bytes) = store
                .attachment_bytes(acct, listed[0].id)
                .unwrap()
                .expect("the row exists");
            (listed[0].downloadable, bytes.map(|b| b.len()))
        };
        assert_eq!(part("g-live"), (true, Some(SIZE)), "live mail keeps it");
        assert_eq!(part("g-old"), (false, None), "the archive keeps no blob");
    }

    /// THE DISK FLOOR: under `sync.archive_min_free_mb` the walk is off the
    /// wire and writes nothing, a number that cannot be read counts as no
    /// room, and 0 switches the check off.
    #[tokio::test]
    async fn the_archive_walk_stores_nothing_while_the_disk_is_short() {
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let g = MockGmail::default();
        let base = serve_mock(g.clone()).await;
        let mut eng = archive_engine(store.clone(), acct, &base, 365);
        assert_eq!(eng.config.sync.archive_min_free_mb, 2048);
        let (_tx, mut rx) = never_shutdown();

        for short in [
            (|| Some(disk_mb(2047, 0, 0))) as fn() -> Option<crate::store::ArchiveDisk>,
            || None,
        ] {
            eng.archive.state().disk_probe = Some(short);
            assert_eq!(
                eng.archive_walk_chunk(&mut rx).await.unwrap(),
                ArchiveStep::Held
            );
        }
        assert!(g.seen().is_empty(), "held means off the wire");
        assert_eq!(store.archive_walked_to(acct).unwrap(), Some(cursor));

        // Nothing free on a volume of any size: 0 switches off the fifth too.
        eng.archive.state().disk_probe = Some(|| Some(disk_mb(0, 50 * 1024, 0)));
        eng.config.sync.archive_min_free_mb = 0;
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked,
            "0 is no floor, whatever the disk says"
        );
        eng.archive.state().disk_probe = Some(|| None);
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked,
            "or cannot say"
        );
        eng.config.sync.archive_min_free_mb = 2048;
        eng.archive.state().disk_probe = Some(|| Some(disk_mb(2048, 0, 0)));
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked,
            "exactly the floor is room"
        );
        // With no probe the engine asks the store, and an in-memory database
        // has no disk to fill.
        eng.archive.state().disk_probe = None;
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
    }

    /// THE EMBEDDER GATE: the walk does not start while the embedder init is
    /// unresolved, so a year of rows is never stored vectorless for the batch
    /// drain. It has no ceiling, because nobody is waiting on it; a gate whose
    /// sender is gone is nothing to wait for.
    #[tokio::test]
    async fn the_archive_walk_waits_for_the_embedder_to_settle() {
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let g = MockGmail::default();
        let base = serve_mock(g.clone()).await;
        let (gate_tx, gate_rx) = tokio::sync::watch::channel(false);
        let eng = archive_engine(store.clone(), acct, &base, 365).with_embedder_gate(gate_rx);
        let (_tx, mut rx) = never_shutdown();

        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Held
        );
        assert!(g.seen().is_empty(), "held means off the wire");
        gate_tx.send(true).unwrap();
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );

        let (dropped_tx, dropped_rx) = tokio::sync::watch::channel(false);
        drop(dropped_tx);
        let eng = archive_engine(store.clone(), acct, &base, 365).with_embedder_gate(dropped_rx);
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked,
            "nothing will ever open a dropped gate"
        );
    }

    /// THE VECTOR DRAIN STAYS OUT OF AN ARCHIVE BATCH, by a flag of the walk's
    /// own: `window_in_flight` never moves, so the catch-up gauges and the
    /// walk's own gate see nothing. The flag falls when the batch ends.
    #[tokio::test]
    async fn the_vector_drain_stays_out_of_an_archive_batch() {
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let at = cursor - ChronoDuration::days(1);
        let g = MockGmail::default();
        g.old_mail("g-a", at, alert_eml(at));
        g.slow_body("g-a", Duration::from_millis(150));
        let base = serve_mock(g.clone()).await;
        let eng = archive_engine(store.clone(), acct, &base, 365);
        let (_tx, mut rx) = never_shutdown();
        assert!(eng.may_drain_vectors());

        let (step, seen) = tokio::join!(eng.archive_walk_chunk(&mut rx), async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !eng.archive.in_flight.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                (
                    eng.may_drain_vectors(),
                    eng.window_in_flight.load(Ordering::Relaxed),
                )
            })
            .await
        });
        assert_eq!(step.unwrap(), ArchiveStep::Walked);
        assert_eq!(
            seen.expect("the batch raised its flag"),
            (false, false),
            "no drain mid-batch, and not by borrowing the window's flag"
        );
        assert!(eng.may_drain_vectors(), "the flag falls with the batch");
        eng.window_in_flight.store(true, Ordering::Relaxed);
        assert!(!eng.may_drain_vectors(), "a window still stops it too");
    }

    /// A CLOCK BEFORE 2024 PERSISTS NOTHING. The start is written once, from
    /// the clock: a 1970 start would read as a finished walk for ever.
    #[tokio::test]
    async fn the_archive_walk_holds_on_a_clock_that_is_plainly_wrong() {
        let (store, acct) = store_at_cursor(Some(100));
        let g = MockGmail::default();
        let base = serve_mock(g.clone()).await;
        let eng = archive_engine(store.clone(), acct, &base, 365);
        let (_tx, mut rx) = never_shutdown();
        eng.archive.state().clock = DateTime::from_timestamp(ARCHIVE_CLOCK_FLOOR_UNIX - 1, 0);
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Held
        );
        assert_eq!(store.archive_walked_to(acct).unwrap(), None);
        assert!(g.seen().is_empty());

        eng.archive.state().clock = DateTime::from_timestamp(ARCHIVE_CLOCK_FLOOR_UNIX, 0);
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
    }

    /// AN ENGINE HANDED AN UNCLAMPED DEPTH (a config built in code rather than
    /// loaded) still does no date arithmetic on it.
    #[tokio::test]
    async fn an_absurd_archive_depth_does_not_overflow_the_walk() {
        let (store, acct) = store_at_cursor(Some(100));
        let g = MockGmail::default();
        let base = serve_mock(g.clone()).await;
        let eng = archive_engine(store.clone(), acct, &base, u32::MAX);
        let (_tx, mut rx) = never_shutdown();
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
    }

    /// A MESSAGE DELETED BETWEEN THE LISTING AND THE FETCH is stepped over by
    /// the walk, first time, and the chunk completes. On a live path the same
    /// 404 is still an error: nothing there may drop an id.
    #[tokio::test]
    async fn a_message_gone_before_its_fetch_is_skipped_by_the_walk_alone() {
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let at = cursor - ChronoDuration::days(1);
        let g = MockGmail::default();
        // Listed, and no body behind it: the mock answers 404.
        g.0.lock()
            .unwrap()
            .all_mail
            .push((at.timestamp(), "g-gone".to_string()));
        g.old_mail("g-ok", at, alert_eml(at));
        let base = serve_mock(g.clone()).await;
        let eng = archive_engine(store.clone(), acct, &base, 365);
        let (_tx, mut rx) = never_shutdown();

        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert_eq!(
            store.archive_walked_to(acct).unwrap(),
            Some(cursor - ChronoDuration::days(7))
        );
        assert_eq!(g.calls("get:g-gone"), 1);
        assert_eq!(
            store
                .thread_view_with_html(acct, "g-ok")
                .unwrap()
                .messages
                .len(),
            1
        );
        assert!(matches!(
            eng.fetch_raw_and_ingest(
                &["g-gone".to_string()],
                Mailbox::Inbox,
                IngestOrigin::Incremental,
                Urgency::Live,
            )
            .await,
            Err(CoreError::NotFound)
        ));
    }

    /// How many separate passes `id` has failed on in the chunk being walked.
    fn booked(eng: &SyncEngine<SqliteStore, FixedToken>, id: &str) -> u32 {
        eng.archive
            .state()
            .failing
            .as_ref()
            .and_then(|(_, failures)| failures.get(id).copied())
            .unwrap_or(0)
    }

    fn stored(store: &SqliteStore, acct: AccountId, gmail_id: &str) -> bool {
        store.thread_view_with_html(acct, gmail_id).is_ok()
    }

    /// A POISON MESSAGE HOLDS ITS CHUNK FOR THREE PASSES AND NO MORE. One body
    /// Gmail will not serve used to stop the walk at its week for good. Once
    /// THAT MESSAGE has failed on three separate passes, the fourth steps over
    /// it, stores the rest and moves the cursor.
    #[tokio::test]
    async fn a_message_that_has_itself_failed_three_passes_is_stepped_over_on_the_fourth() {
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let at = cursor - ChronoDuration::days(1);
        let g = MockGmail::default();
        g.old_mail("g-bad", at, alert_eml(at));
        g.old_mail("g-ok", at, alert_eml(at));
        g.0.lock().unwrap().errors.insert(
            "g-bad".to_string(),
            std::iter::repeat_n((500, "backendError".to_string()), 10).collect(),
        );
        let base = serve_mock(g.clone()).await;
        let mut eng = archive_engine(store.clone(), acct, &base, 365);
        eng.config.sync.fetch_concurrency = 1;
        let (_tx, mut rx) = never_shutdown();

        for pass in 1..=3 {
            assert!(
                eng.archive_walk_chunk(&mut rx).await.is_err(),
                "pass {pass} still fails whole"
            );
            assert_eq!(store.archive_walked_to(acct).unwrap(), Some(cursor));
            assert_eq!(booked(&eng, "g-bad"), pass, "booked against the message");
            assert_eq!(booked(&eng, "g-ok"), 0);
            assert_eq!(
                eng.archive_error_wait(),
                eng.archive_pace.error_backoff * 2u32.pow(pass - 1),
                "not a quota refusal, so the error backoff, doubling per pass"
            );
            assert!(!eng.archive.in_flight.load(Ordering::Relaxed));
            assert_eq!(eng.metrics.archive_skipped(), 0);
        }
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert_eq!(
            store.archive_walked_to(acct).unwrap(),
            Some(cursor - ChronoDuration::days(7))
        );
        assert_eq!(g.calls("get:g-bad"), 4);
        assert!(!stored(&store, acct, "g-bad"));
        assert!(stored(&store, acct, "g-ok"));
        assert_eq!(eng.metrics.archive_skipped(), 1, "and counted");
        assert!(
            eng.archive.state().failing.is_none(),
            "what failed in a chunk is forgotten with the chunk"
        );
    }

    /// m4. A SKIPPED MESSAGE IS COUNTED ONCE. A chunk holds a message deleted
    /// before its fetch and one Gmail will not serve. The chunk is walked
    /// four times before it passes, and the deleted message is stepped over
    /// on every one of them: two messages were skipped, and the counter says
    /// two, not five.
    #[tokio::test]
    async fn a_message_stepped_over_on_every_pass_is_counted_once() {
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let at = cursor - ChronoDuration::days(1);
        let g = MockGmail::default();
        // Listed, and no body behind it: the mock answers 404.
        g.0.lock()
            .unwrap()
            .all_mail
            .push((at.timestamp(), "g-gone".to_string()));
        g.old_mail("g-bad", at, alert_eml(at));
        g.0.lock().unwrap().errors.insert(
            "g-bad".to_string(),
            std::iter::repeat_n((500, "backendError".to_string()), 10).collect(),
        );
        let base = serve_mock(g.clone()).await;
        let mut eng = archive_engine(store.clone(), acct, &base, 365);
        eng.config.sync.fetch_concurrency = 1;
        let (_tx, mut rx) = never_shutdown();

        for _ in 1..=3 {
            assert!(eng.archive_walk_chunk(&mut rx).await.is_err());
            assert_eq!(eng.metrics.archive_skipped(), 1, "the deleted one, once");
        }
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert_eq!(g.calls("get:g-gone"), 4, "stepped over on every pass");
        assert_eq!(g.calls("get:g-bad"), 4);
        assert_eq!(eng.metrics.archive_skipped(), 2, "two messages, two counts");

        // The next chunk is its own: a message gone from it is counted.
        let at = cursor - ChronoDuration::days(8);
        g.0.lock()
            .unwrap()
            .all_mail
            .push((at.timestamp(), "g-gone-2".to_string()));
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert_eq!(eng.metrics.archive_skipped(), 3);
    }

    /// A TRANSIENT ERROR NEVER COSTS A MESSAGE. Gmail fails one body for a
    /// quarter of an hour and then a different one once. Counted per CHUNK,
    /// the fourth pass was "tolerant" and dropped the second message on its
    /// very first failure, for good: neither cursor ever looks back. Counted
    /// per MESSAGE, the second one has failed once, the pass fails, and the
    /// next pass stores them both.
    #[tokio::test]
    async fn a_message_is_never_stepped_over_on_its_first_failure() {
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let at = cursor - ChronoDuration::days(1);
        let g = MockGmail::default();
        g.old_mail("g-a", at, alert_eml(at));
        g.old_mail("g-b", at, alert_eml(at));
        {
            let mut st = g.0.lock().unwrap();
            st.errors.insert(
                "g-a".to_string(),
                std::iter::repeat_n((500, "backendError".to_string()), 3).collect(),
            );
            st.errors.insert(
                "g-b".to_string(),
                std::collections::VecDeque::from([(503, "backendError".to_string())]),
            );
            // And a listing that fails first: no pass, nothing booked.
            st.all_mail_failures = 2;
        }
        let base = serve_mock(g.clone()).await;
        let mut eng = archive_engine(store.clone(), acct, &base, 365);
        // One GET at a time, so which message a pass reaches is not a race.
        eng.config.sync.fetch_concurrency = 1;
        let (_tx, mut rx) = never_shutdown();

        for _ in 0..2 {
            assert!(eng.archive_walk_chunk(&mut rx).await.is_err());
        }
        assert!(
            eng.archive.state().failing.is_none(),
            "a failed listing is not a pass over any message"
        );
        assert_eq!(g.calls("get:g-a") + g.calls("get:g-b"), 0);

        for pass in 1..=3 {
            assert!(eng.archive_walk_chunk(&mut rx).await.is_err());
            assert_eq!((booked(&eng, "g-a"), booked(&eng, "g-b")), (pass, 0));
        }
        // The fourth pass: g-a is served at last, and g-b fails for the first
        // time. It is not stepped over.
        assert!(eng.archive_walk_chunk(&mut rx).await.is_err());
        assert_eq!(store.archive_walked_to(acct).unwrap(), Some(cursor));
        assert!(stored(&store, acct, "g-a"));
        assert!(!stored(&store, acct, "g-b"));
        assert_eq!(booked(&eng, "g-b"), 1);
        assert_eq!(eng.metrics.archive_skipped(), 0);

        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert!(stored(&store, acct, "g-b"), "nothing was lost");
        assert_eq!(eng.metrics.archive_skipped(), 0);
        assert_eq!(g.calls("get:g-a"), 4, "stored once, never fetched again");
    }

    /// REPEATED FAILURE IS STILL NOT A LICENCE FOR EVERYTHING. A message that
    /// has failed three passes is not stepped over when Gmail refuses the walk
    /// for quota (that is not about the message, and is not booked against
    /// it), nor when more than a handful are in the same state (that is an
    /// outage, not a poison message). Either way the cursor stays.
    #[tokio::test]
    async fn repeated_failure_does_not_excuse_a_quota_refusal_or_an_outage() {
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let at = cursor - ChronoDuration::days(1);
        let g = MockGmail::default();
        g.old_mail("g-refused", at, alert_eml(at));
        g.0.lock().unwrap().errors.insert(
            "g-refused".to_string(),
            std::collections::VecDeque::from([(429, "rateLimitExceeded".to_string())]),
        );
        let base = serve_mock(g.clone()).await;
        let mut eng = archive_engine(store.clone(), acct, &base, 365);
        eng.config.sync.fetch_concurrency = 1;
        let (_tx, mut rx) = never_shutdown();
        // Every id here has already failed on three separate passes.
        let thrice = |ids: &[&str]| {
            Some((
                (false, cursor.timestamp()),
                ids.iter().map(|id| (id.to_string(), 3)).collect(),
            ))
        };

        eng.archive.state().failing = thrice(&["g-refused"]);
        assert!(eng.archive_walk_chunk(&mut rx).await.is_err());
        assert_eq!(store.archive_walked_to(acct).unwrap(), Some(cursor));
        assert_eq!(booked(&eng, "g-refused"), 3, "a refusal is not booked");
        assert_eq!(eng.metrics.archive_skipped(), 0);

        // Six bodies that all fail again: one more than a pass may step over.
        let down: Vec<String> = (0..6).map(|i| format!("g-down-{i}")).collect();
        for id in &down {
            g.old_mail(id, at, alert_eml(at));
            g.0.lock().unwrap().errors.insert(
                id.clone(),
                std::iter::repeat_n((500, "backendError".to_string()), 2).collect(),
            );
        }
        // The refused one is served now; start the day's refusals afresh.
        eng.archive.state().quota_refusals_today = 0;
        let ids: Vec<&str> = down.iter().map(String::as_str).collect();
        eng.archive.state().failing = thrice(&ids);
        assert!(eng.archive_walk_chunk(&mut rx).await.is_err());
        assert_eq!(store.archive_walked_to(acct).unwrap(), Some(cursor));

        // Five of them: a pass may step over those, and does.
        g.0.lock().unwrap().errors.remove("g-down-5");
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert!(stored(&store, acct, "g-down-5"));
        assert!(!stored(&store, acct, "g-down-0"));
    }

    /// A MESSAGE THE STORE WILL NOT TAKE is stepped over on the same terms as
    /// one Gmail will not serve: three separate passes first. It used to be
    /// the one failure that held a chunk for good.
    #[tokio::test]
    async fn a_message_the_store_refuses_three_passes_running_is_stepped_over_too() {
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let at = cursor - ChronoDuration::days(1);
        let g = MockGmail::default();
        g.old_mail("g-bad", at, alert_eml(at));
        g.old_mail("g-ok", at, alert_eml(at));
        store.execute_batch_for_test(
            "CREATE TRIGGER refuse_one BEFORE INSERT ON messages
             WHEN NEW.gmail_msg_id='g-bad'
             BEGIN SELECT RAISE(ABORT,'refused'); END;",
        );
        let base = serve_mock(g.clone()).await;
        let mut eng = archive_engine(store.clone(), acct, &base, 365);
        eng.config.sync.fetch_concurrency = 1;
        let (_tx, mut rx) = never_shutdown();

        for pass in 1..=3 {
            assert!(matches!(
                eng.archive_walk_chunk(&mut rx).await,
                Err(CoreError::Sqlite(_))
            ));
            assert_eq!(booked(&eng, "g-bad"), pass);
            assert_eq!(store.archive_walked_to(acct).unwrap(), Some(cursor));
        }
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert!(stored(&store, acct, "g-ok"));
        assert!(!stored(&store, acct, "g-bad"));
        assert_eq!(eng.metrics.archive_skipped(), 1);
        assert_eq!(
            store.archive_walked_to(acct).unwrap(),
            Some(cursor - ChronoDuration::days(7))
        );
    }

    /// W2. THE WALK NEVER STARTS NEARER TO NOW THAN A DAY. With a backfill
    /// window of zero days the start used to be this instant, and the first
    /// backward listing (all mail, no `-in:inbox`) could store a message that
    /// arrived a second ago before the poll ingested it: write-once, so never
    /// triaged and never notified.
    #[tokio::test]
    async fn a_zero_day_backfill_does_not_start_the_walk_at_now() {
        let (store, acct) = store_at_cursor(Some(100));
        let now = days_ago(0);
        let g = MockGmail::default();
        let just_now = now - ChronoDuration::minutes(1);
        g.old_mail("g-new", just_now, alert_eml(just_now));
        g.message_labels("g-new", &["INBOX"]);
        let older = now - ChronoDuration::days(2);
        g.old_mail("g-older", older, alert_eml(older));
        let base = serve_mock(g.clone()).await;
        let mut config = Config::default();
        config.sync.archive_days = 365;
        config.sync.backfill_days = 0;
        let mut eng = engine_with_config(store.clone(), acct, &base, config);
        eng.archive_pace = instant_pace();
        eng.poll_healthy.store(true, Ordering::Relaxed);
        eng.archive.state().clock = Some(now);
        let (_tx, mut rx) = never_shutdown();

        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        let start = now - eng.archive_pace.forward_lag;
        assert_eq!(
            g.queries(""),
            vec![archive_q(start - ChronoDuration::days(7), start)],
            "the first week listed ends a day ago, not now"
        );
        assert_eq!(g.calls("get:g-new"), 0);
        assert!(!stored(&store, acct, "g-new"), "the poll's, not the walk's");
        assert!(stored(&store, acct, "g-older"));
        assert_eq!(
            store.archive_walked_to(acct).unwrap(),
            Some(start - ChronoDuration::days(7))
        );

        // A real backfill window is older than a day and is where it starts.
        let (store, acct) = store_at_cursor(Some(100));
        let eng = archive_engine(store.clone(), acct, &base, 365);
        eng.archive.state().clock = Some(now);
        assert_eq!(eng.config.sync.backfill_days, 30);
        eng.archive_walk_chunk(&mut rx).await.unwrap();
        assert_eq!(
            store.archive_walked_to(acct).unwrap(),
            Some(now - ChronoDuration::days(37))
        );
    }

    /// W4. THE DISK FLOOR IS ASKED BEFORE EVERY BATCH. A chunk is a week of
    /// mail, however much that is; asked only at its start, a heavy week was
    /// written straight through the floor. The walk stops between batches,
    /// the cursor stays, and what the chunk already stored is not fetched
    /// again when there is room.
    #[tokio::test]
    async fn the_disk_floor_stops_a_chunk_between_batches() {
        use std::sync::atomic::AtomicUsize;
        // Room for the chunk's own check and the first batch's, then none.
        static ASKED: AtomicUsize = AtomicUsize::new(0);
        fn filling_up() -> Option<crate::store::ArchiveDisk> {
            Some(if ASKED.fetch_add(1, Ordering::SeqCst) < 2 {
                disk_mb(2048, 0, 0)
            } else {
                disk_mb(2047, 0, 0)
            })
        }
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let at = cursor - ChronoDuration::days(1);
        let g = MockGmail::default();
        let ids: Vec<String> = (0..25).map(|i| format!("g-{i:02}")).collect();
        for id in &ids {
            g.old_mail(id, at, alert_eml(at));
        }
        let base = serve_mock(g.clone()).await;
        let eng = archive_engine(store.clone(), acct, &base, 365);
        assert_eq!(eng.archive_pace.batch, 10);
        let (_tx, mut rx) = never_shutdown();
        let fetched = |g: &MockGmail| g.seen().iter().filter(|s| s.starts_with("get:")).count();

        eng.archive.state().disk_probe = Some(filling_up);
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Held
        );
        assert_eq!(
            ASKED.load(Ordering::SeqCst),
            3,
            "the chunk, then each batch"
        );
        assert_eq!(fetched(&g), 10, "one batch and no more");
        assert_eq!(ids.iter().filter(|id| stored(&store, acct, id)).count(), 10);
        assert_eq!(store.archive_walked_to(acct).unwrap(), Some(cursor));
        assert_eq!(eng.metrics.archive_hold(), Some(ArchiveHold::Disk));

        // Still short: nothing at all, not even the listing.
        let served = g.seen().len();
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Held
        );
        assert_eq!(g.seen().len(), served, "held means off the wire");

        // Room again: the other fifteen, and only those.
        eng.archive.state().disk_probe = Some(|| Some(crate::store::ArchiveDisk::UNBOUNDED));
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert_eq!(fetched(&g), 25);
        assert!(ids.iter().all(|id| stored(&store, acct, id)));
        assert_eq!(
            store.archive_walked_to(acct).unwrap(),
            Some(cursor - ChronoDuration::days(7))
        );
        assert_eq!(eng.metrics.archive_hold(), None);
    }

    /// F3. ON A VOLUME OTHER DATABASES SHARE, THE FLOOR IS A FIFTH OF IT. The
    /// configured 2 GiB is everybody's last 2 GiB there, so the floor in force
    /// is the larger of the two. AND IT DOES NOT FLAP: once held, the walk
    /// resumes at the floor plus a tenth, and the hold is said once when it
    /// starts and once when it ends. Live mail is stored throughout.
    #[tokio::test]
    async fn the_disk_floor_is_a_fifth_of_the_volume_and_resumes_with_a_margin() {
        use std::sync::atomic::AtomicU64;
        // A 25 GiB volume: the floor is 5 GiB and the walk resumes at 5.5.
        const GIB: u64 = 1024;
        static FREE_MB: AtomicU64 = AtomicU64::new(0);
        fn shared() -> Option<crate::store::ArchiveDisk> {
            Some(disk_mb(FREE_MB.load(Ordering::SeqCst), 25 * GIB, 0))
        }
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let g = MockGmail::default();
        let live_at = days_ago(1);
        g.old_mail("g-live", live_at, alert_eml(live_at));
        let base = serve_mock(g.clone()).await;
        let eng = archive_engine(store.clone(), acct, &base, 365);
        assert_eq!(eng.config.sync.archive_min_free_mb, 2048);
        let (_tx, mut rx) = never_shutdown();
        eng.archive.state().disk_probe = Some(shared);
        let changes = || eng.archive.hold_changes.load(Ordering::Relaxed);
        let mut step = async |free_mb: u64| {
            FREE_MB.store(free_mb, Ordering::SeqCst);
            eng.archive_walk_chunk(&mut rx).await.unwrap()
        };

        // Twice the configured floor free, and still short of a fifth.
        assert_eq!(step(4 * GIB).await, ArchiveStep::Held);
        assert_eq!(eng.metrics.archive_hold(), Some(ArchiveHold::Disk));
        assert_eq!(changes(), 1);
        assert!(g.seen().is_empty(), "held means off the wire");
        // Back at the floor, and anywhere under floor plus a tenth: held, and
        // nothing more said about it.
        for free_mb in [5 * GIB - 1, 5 * GIB, 5 * GIB + 256, 5 * GIB + 511] {
            assert_eq!(step(free_mb).await, ArchiveStep::Held, "{free_mb} MiB free");
        }
        assert_eq!(changes(), 1, "one line for the whole stretch");
        assert_eq!(store.archive_walked_to(acct).unwrap(), Some(cursor));

        // LIVE MAIL IS NOT THE WALK'S TO HOLD: it is stored under the floor.
        eng.fetch_raw_and_ingest(
            &["g-live".to_string()],
            Mailbox::Inbox,
            IngestOrigin::Incremental,
            Urgency::Live,
        )
        .await
        .unwrap();
        assert!(stored(&store, acct, "g-live"));
        assert_eq!(eng.metrics.archive_hold(), Some(ArchiveHold::Disk));

        // The margin reached: it walks, and says so once.
        assert_eq!(step(5 * GIB + 512).await, ArchiveStep::Walked);
        assert_eq!(eng.metrics.archive_hold(), None);
        assert_eq!(changes(), 2);
        // Not held, so the floor itself is room again: no second threshold
        // for a walk that never stopped.
        assert_eq!(step(5 * GIB).await, ArchiveStep::Walked);
        assert_eq!(changes(), 2);
        assert_eq!(step(5 * GIB - 1).await, ArchiveStep::Held);
        assert_eq!(changes(), 3);
    }

    /// The volume share stops growing at 5 GiB, so a large, ordinarily full
    /// disk does not hold the walk; the configured floor still wins when it
    /// is the larger.
    #[test]
    fn the_volume_share_of_the_disk_floor_is_capped() {
        const GIB: u64 = 1024 * 1024 * 1024;
        assert_eq!(archive_floor_bytes(2048, 5 * GIB), 2 * GIB);
        assert_eq!(archive_floor_bytes(2048, 20 * GIB), 4 * GIB);
        assert_eq!(archive_floor_bytes(2048, 50 * GIB), 5 * GIB);
        assert_eq!(archive_floor_bytes(2048, 926 * GIB), 5 * GIB);
        assert_eq!(archive_floor_bytes(20 * 1024, 926 * GIB), 20 * GIB);
    }

    /// F3. THE ACCOUNT'S OWN CEILING. Free space on a shared volume is what
    /// the neighbours left; the size of this database is this account's
    /// alone. At `sync.archive_max_db_mb` the walk holds, with its own reason,
    /// before a chunk and between its batches; 0 is no ceiling; and live mail
    /// is stored regardless.
    #[tokio::test]
    async fn the_archive_walk_holds_at_its_own_database_ceiling() {
        use std::sync::atomic::AtomicUsize;
        // Under the ceiling for the chunk's own check and the first batch's,
        // then at it. All the free space in the world throughout.
        static ASKED: AtomicUsize = AtomicUsize::new(0);
        fn growing() -> Option<crate::store::ArchiveDisk> {
            Some(if ASKED.fetch_add(1, Ordering::SeqCst) < 2 {
                disk_mb(u64::MAX >> 20, 0, 4095)
            } else {
                disk_mb(u64::MAX >> 20, 0, 4096)
            })
        }
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let at = cursor - ChronoDuration::days(1);
        let g = MockGmail::default();
        let ids: Vec<String> = (0..25).map(|i| format!("g-{i:02}")).collect();
        for id in &ids {
            g.old_mail(id, at, alert_eml(at));
        }
        let live_at = days_ago(1);
        g.old_mail("g-live", live_at, alert_eml(live_at));
        let base = serve_mock(g.clone()).await;
        let mut eng = archive_engine(store.clone(), acct, &base, 365);
        assert_eq!(eng.config.sync.archive_max_db_mb, 4096);
        let (_tx, mut rx) = never_shutdown();
        let fetched = |g: &MockGmail| g.seen().iter().filter(|s| s.starts_with("get:")).count();

        eng.archive.state().disk_probe = Some(growing);
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Held
        );
        assert_eq!(fetched(&g), 10, "one batch and no more");
        assert_eq!(store.archive_walked_to(acct).unwrap(), Some(cursor));
        assert_eq!(eng.metrics.archive_hold(), Some(ArchiveHold::DbSize));
        assert_eq!(eng.archive.hold_changes.load(Ordering::Relaxed), 1);

        // Still at it: nothing at all, not even the listing.
        let served = g.seen().len();
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Held
        );
        assert_eq!(g.seen().len(), served, "held means off the wire");
        assert_eq!(eng.archive.hold_changes.load(Ordering::Relaxed), 1);
        // The free-space floor switched off does not switch this off.
        eng.config.sync.archive_min_free_mb = 0;
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Held
        );
        eng.config.sync.archive_min_free_mb = 2048;

        // LIVE MAIL IS NOT THE WALK'S TO HOLD.
        eng.fetch_raw_and_ingest(
            &["g-live".to_string()],
            Mailbox::Inbox,
            IngestOrigin::Incremental,
            Urgency::Live,
        )
        .await
        .unwrap();
        assert!(stored(&store, acct, "g-live"));

        // 0 is no ceiling: the other fifteen, and only those.
        eng.config.sync.archive_max_db_mb = 0;
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert_eq!(fetched(&g), 26);
        assert!(ids.iter().all(|id| stored(&store, acct, id)));
        assert_eq!(eng.metrics.archive_hold(), None);
        // And one MiB under it is room.
        eng.config.sync.archive_max_db_mb = 4096;
        eng.archive.state().disk_probe = Some(|| Some(disk_mb(u64::MAX >> 20, 0, 4095)));
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
    }

    /// W3. A HELD WALK SAYS WHY. Each reason the walk can hold for reaches the
    /// gauge (and the log, once, when it changes); a walk that then runs
    /// clears it. Without this a walk waiting on a hung embedder or a wrong
    /// clock looked exactly like a walk that was switched off.
    #[tokio::test]
    async fn a_held_walk_reports_the_reason_it_is_holding() {
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let g = MockGmail::default();
        let base = serve_mock(g.clone()).await;
        let (gate_tx, gate_rx) = tokio::sync::watch::channel(false);
        let eng = archive_engine(store.clone(), acct, &base, 365).with_embedder_gate(gate_rx);
        let (_tx, mut rx) = never_shutdown();
        let now = Utc::now();
        assert_eq!(eng.metrics.archive_hold(), None, "nothing said yet");

        // Every reason at once, lifted one at a time in the order the walk
        // checks them.
        eng.poll_healthy.store(false, Ordering::Relaxed);
        eng.window_in_flight.store(true, Ordering::Relaxed);
        {
            let mut state = eng.archive.state();
            state.clock = DateTime::from_timestamp(ARCHIVE_CLOCK_FLOOR_UNIX - 1, 0);
            state.disk_probe = Some(|| None);
        }
        let mut seen = Vec::new();
        for lift in 0..7 {
            match lift {
                0 => {}
                1 => eng.poll_healthy.store(true, Ordering::Relaxed),
                2 => eng.window_in_flight.store(false, Ordering::Relaxed),
                3 => gate_tx.send(true).unwrap(),
                4 => {
                    let mut state = eng.archive.state();
                    state.clock = Some(now);
                    state.quota_day = Some(now.date_naive());
                    state.quota_refusals_today = eng.archive_pace.quota_refusals_per_day;
                }
                5 => eng.archive.state().quota_refusals_today = 0,
                _ => {
                    eng.archive.state().disk_probe = Some(|| Some(disk_mb(u64::MAX >> 20, 0, 4096)))
                }
            }
            assert_eq!(
                eng.archive_walk_chunk(&mut rx).await.unwrap(),
                ArchiveStep::Held
            );
            let hold = eng
                .metrics
                .archive_hold()
                .expect("a held walk has a reason");
            assert_eq!(eng.archive.state().hold, Some(hold));
            seen.push(hold);
        }
        assert_eq!(
            seen,
            [
                ArchiveHold::Poll,
                ArchiveHold::Window,
                ArchiveHold::Embedder,
                ArchiveHold::Clock,
                ArchiveHold::QuotaDay,
                ArchiveHold::Disk,
                ArchiveHold::DbSize,
            ]
        );
        assert_eq!(seen.len(), ArchiveHold::ALL.len(), "every reason there is");
        assert!(g.seen().is_empty(), "held means off the wire, every time");
        assert_eq!(
            eng.archive.hold_changes.load(Ordering::Relaxed),
            7,
            "said once per change"
        );

        eng.archive.state().disk_probe = Some(|| Some(crate::store::ArchiveDisk::UNBOUNDED));
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert_eq!(eng.metrics.archive_hold(), None);
        assert_eq!(eng.archive.state().hold, None);
    }

    /// A QUOTA REFUSAL BACKS THE WALK OFF, LONGER EACH TIME, AND THREE IN A DAY
    /// END IT UNTIL THE NEXT. Five minutes, ten, and then nothing on the wire
    /// until the UTC date changes. None of it counts as a poison attempt.
    #[tokio::test]
    async fn a_quota_refusal_backs_the_walk_off_and_three_in_a_day_stop_it() {
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let at = cursor - ChronoDuration::days(1);
        let g = MockGmail::default();
        g.old_mail("g-a", at, alert_eml(at));
        g.0.lock().unwrap().errors.insert(
            "g-a".to_string(),
            std::iter::repeat_n((429, "rateLimitExceeded".to_string()), 3).collect(),
        );
        let base = serve_mock(g.clone()).await;
        let eng = archive_engine(store.clone(), acct, &base, 365);
        let (_tx, mut rx) = never_shutdown();
        let noon = Utc::now()
            .date_naive()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc();
        eng.archive.state().clock = Some(noon);

        for minutes in [5, 10, 20] {
            assert!(eng.archive_walk_chunk(&mut rx).await.is_err());
            assert_eq!(
                eng.archive_error_wait(),
                Duration::from_secs(minutes * 60),
                "the wait doubles"
            );
        }
        assert_eq!(
            booked(&eng, "g-a"),
            0,
            "a refusal is not a failure of the message"
        );
        let served = g.seen().len();
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Held,
            "three refusals end the day"
        );
        assert_eq!(g.seen().len(), served, "held means off the wire");

        eng.archive.state().clock = Some(noon + ChronoDuration::days(1));
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert_eq!(
            eng.archive_error_wait(),
            eng.archive_pace.error_backoff,
            "a walked chunk clears the backoff"
        );
    }

    /// The backoff's ceiling: an hour, however many refusals came before.
    #[test]
    fn the_quota_backoff_is_capped_at_an_hour() {
        let pace = ArchivePace::default();
        let mut wait = pace.quota_backoff;
        for _ in 0..10 {
            wait = (wait * 2).min(pace.quota_backoff_cap);
        }
        assert_eq!(wait, Duration::from_secs(3600));
        assert_eq!(pace.per_minute, 30);
        assert_eq!(pace.per_minute_window, Duration::from_secs(60));
        assert!(pace.batch <= pace.per_minute);
    }

    /// THE ERROR BACKOFF: a failure that is not a quota refusal waits the
    /// first step, each consecutive one twice the last, never past the cap;
    /// a walked chunk starts the run over. By default 15s, 30s, 1m, 2m, 4m,
    /// then 5m.
    #[tokio::test]
    async fn the_archive_error_backoff_doubles_caps_and_resets_on_success() {
        let d = ArchivePace::default();
        let mut wait = d.error_backoff;
        let mut seen = vec![wait.as_secs()];
        for _ in 0..6 {
            wait = (wait * 2).min(d.error_backoff_cap);
            seen.push(wait.as_secs());
        }
        assert_eq!(seen, [15, 30, 60, 120, 240, 300, 300]);

        let (store, acct) = store_at_cursor(Some(100));
        store.set_archive_walked_to(acct, days_ago(40)).unwrap();
        let g = MockGmail::default();
        let base = serve_mock(g.clone()).await;
        let mut eng = archive_engine(store.clone(), acct, &base, 365);
        eng.archive_pace.error_backoff = Duration::from_secs(10);
        eng.archive_pace.error_backoff_cap = Duration::from_secs(40);
        let (_tx, mut rx) = never_shutdown();

        // Five failed listings in a row: 10, 20, 40, then held at the cap.
        g.0.lock().unwrap().all_mail_failures = 5;
        let mut waits = Vec::new();
        for _ in 0..5 {
            assert!(eng.archive_walk_chunk(&mut rx).await.is_err());
            waits.push(eng.archive_error_wait().as_secs());
        }
        assert_eq!(waits, [10, 20, 40, 40, 40], "grows, then the cap holds");

        // A walked chunk ends the run: the next failure starts small again.
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert_eq!(eng.archive_error_wait(), Duration::from_secs(10));
        g.0.lock().unwrap().all_mail_failures = 1;
        assert!(eng.archive_walk_chunk(&mut rx).await.is_err());
        assert_eq!(
            eng.archive_error_wait(),
            Duration::from_secs(10),
            "reset by the success"
        );
    }

    /// THE ABSOLUTE CEILING, on a virtual clock: 30 in a minute and the 31st
    /// waits for the minute to turn, then the count starts again.
    #[tokio::test(start_paused = true)]
    async fn the_archive_ceiling_admits_thirty_a_minute() {
        const MINUTE: Duration = Duration::from_secs(60);
        let mut state = ArchiveState::default();
        for _ in 0..3 {
            assert_eq!(state.admit(10, 30, MINUTE), Duration::ZERO);
        }
        tokio::time::advance(Duration::from_secs(20)).await;
        let wait = state.admit(10, 30, MINUTE);
        assert_eq!(wait, Duration::from_secs(40), "until the minute turns");
        tokio::time::advance(wait).await;
        // That batch is booked against the new minute; two more fit.
        assert_eq!(state.admit(10, 30, MINUTE), Duration::ZERO);
        assert_eq!(state.admit(10, 30, MINUTE), Duration::ZERO);
        assert_eq!(state.admit(1, 30, MINUTE), MINUTE);
        // A quiet minute later the count is fresh.
        tokio::time::advance(MINUTE * 3).await;
        assert_eq!(state.admit(10, 30, MINUTE), Duration::ZERO);
    }

    /// THE CEILING IS IN THE WALK, not just beside it: with two messages
    /// allowed per (shortened) minute, the third waits the minute out even
    /// though the budget and the rest would let it go at once.
    #[tokio::test]
    async fn the_archive_walk_waits_out_its_ceiling() {
        let (store, acct) = store_at_cursor(Some(100));
        let cursor = days_ago(40);
        store.set_archive_walked_to(acct, cursor).unwrap();
        let at = cursor - ChronoDuration::days(1);
        let g = MockGmail::default();
        for id in ["g-a", "g-b", "g-c"] {
            g.old_mail(id, at, alert_eml(at));
        }
        let base = serve_mock(g.clone()).await;
        let mut eng = archive_engine(store.clone(), acct, &base, 365);
        eng.archive_pace.batch = 1;
        eng.archive_pace.per_minute = 2;
        eng.archive_pace.per_minute_window = Duration::from_millis(400);
        let (_tx, mut rx) = never_shutdown();
        let began = std::time::Instant::now();
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert!(
            began.elapsed() >= Duration::from_millis(350),
            "the third fetch waited for the window: {:?}",
            began.elapsed()
        );
        assert_eq!(g.calls("get:g-c"), 1);
    }

    // ---- the forward pass ---------------------------------------------------

    fn forward_queries(g: &MockGmail) -> Vec<String> {
        g.queries("")
            .into_iter()
            .filter(|q| q.contains("-in:inbox"))
            .collect()
    }

    /// THE FORWARD PASS: mail newer than the walk's start that carries neither
    /// INBOX nor SENT is stored as archive, up to a day behind the last poll.
    /// Mail in either label is never listed (the live path owns it), nor is
    /// anything from the last day.
    #[tokio::test]
    async fn the_forward_pass_stores_unlabelled_mail_and_leaves_the_live_paths_theirs() {
        let (store, acct) = store_at_cursor(Some(100));
        let start = days_ago(30);
        store.set_archive_walked_to(acct, start).unwrap();
        assert_eq!(store.archive_covered_to(acct).unwrap(), Some(start));
        let g = MockGmail::default();
        g.old_mail("g-filed", days_ago(10), alert_eml(days_ago(10)));
        g.old_mail("g-inbox", days_ago(10), alert_eml(days_ago(10)));
        g.message_labels("g-inbox", &["INBOX"]);
        g.old_mail(
            "g-sent",
            days_ago(9),
            sent_eml(days_ago(9), "alice@friends.com", "the lease"),
        );
        g.message_labels("g-sent", &["SENT"]);
        let fresh = Utc::now() - ChronoDuration::hours(2);
        g.old_mail("g-fresh", fresh, alert_eml(fresh));
        let base = serve_mock(g.clone()).await;
        // 30 days of archive on 30 of backfill: nothing to walk backwards, so
        // every listing here is the forward pass's.
        let eng = archive_engine(store.clone(), acct, &base, 30);
        let (_tx, mut rx) = never_shutdown();

        // NO POLL HAS SUCCEEDED IN THIS PROCESS: no forward pass at all.
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Done
        );
        assert!(g.seen().is_empty(), "{:?}", g.seen());

        let polled = Utc::now();
        eng.archive
            .last_poll_ok_unix
            .store(polled.timestamp(), Ordering::Relaxed);
        let mut steps = 0;
        while eng.archive_walk_chunk(&mut rx).await.unwrap() == ArchiveStep::Walked {
            steps += 1;
            assert!(steps < 20, "the pass must catch up");
        }
        // 29 days to cover, a week at a time.
        assert_eq!(steps, 5);
        let queries = forward_queries(&g);
        assert_eq!(queries.len(), 5);
        assert_eq!(queries.len(), g.queries("").len(), "nothing backwards");
        assert_eq!(
            queries[0],
            format!(
                "after:{} before:{} -in:inbox -in:sent -in:draft -in:chats",
                start.timestamp() - 1,
                (start + ChronoDuration::days(7)).timestamp()
            )
        );

        let filed = store.thread_view_with_html(acct, "g-filed").unwrap();
        assert!(
            store
                .message_is_archive(acct, filed.messages[0].id)
                .unwrap()
        );
        for live in ["g-inbox", "g-sent", "g-fresh"] {
            assert_eq!(
                g.calls(&format!("get:{live}")),
                0,
                "{live} is not the walk's"
            );
            assert!(store.thread_view_with_html(acct, live).is_err());
        }
        let covered = store.archive_covered_to(acct).unwrap().unwrap();
        assert_eq!(
            covered.timestamp(),
            polled.timestamp() - 24 * 3600,
            "a day behind the poll, to the second"
        );
        assert_eq!(
            store.archive_walked_to(acct).unwrap(),
            Some(start),
            "the backward edge is not the forward pass's to move"
        );
        assert_eq!(
            eng.metrics.archive_covered_to(),
            covered.timestamp(),
            "and the gauge shows the forward edge"
        );

        // CAUGHT UP: nothing more on the wire until the recheck comes round.
        let served = g.seen().len();
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Done
        );
        assert_eq!(g.seen().len(), served);
    }

    /// THE RACE THE FORWARD PASS MUST LOSE. A daemon suspended for three days
    /// wakes with a message that arrived in the inbox during the gap and was
    /// archived on a phone since: it carries no label now and is more than a
    /// day old, and the poll that will replay its arrival has not run yet. The
    /// pass is bounded by the last poll that SUCCEEDED, so it does not take
    /// it; the live path ingests it, with its triage job; and the pass, once
    /// the poll has caught up, finds it stored and leaves it alone.
    #[tokio::test]
    async fn the_forward_pass_never_takes_mail_the_poll_has_not_replayed_yet() {
        use crate::store::agent_triage::AgentTriageStore;
        let (store, acct) = store_at_cursor(Some(100));
        let start = days_ago(30);
        store.set_archive_walked_to(acct, start).unwrap();
        let g = MockGmail::default();
        g.old_mail("g-gap", days_ago(2), alert_eml(days_ago(2)));
        let base = serve_mock(g.clone()).await;
        let mut eng = archive_engine(store.clone(), acct, &base, 30);
        // One chunk reaches the bound, so each call below is one whole pass.
        eng.archive_pace.chunk = ChronoDuration::days(60);
        eng.archive_pace.done_recheck = Duration::ZERO;
        let (_tx, mut rx) = never_shutdown();

        let last_poll = days_ago(3);
        eng.archive
            .last_poll_ok_unix
            .store(last_poll.timestamp(), Ordering::Relaxed);
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert_eq!(g.calls("get:g-gap"), 0, "newer than the last poll's reach");
        assert_eq!(
            store.archive_covered_to(acct).unwrap(),
            Some(last_poll - ChronoDuration::hours(24))
        );

        // The poll lane replays the gap: the arrival is ingested live.
        eng.fetch_raw_and_ingest(
            &["g-gap".to_string()],
            Mailbox::Inbox,
            IngestOrigin::Incremental,
            Urgency::Live,
        )
        .await
        .unwrap();
        eng.archive
            .last_poll_ok_unix
            .store(Utc::now().timestamp(), Ordering::Relaxed);
        assert_eq!(
            eng.archive_walk_chunk(&mut rx).await.unwrap(),
            ArchiveStep::Walked
        );
        assert_eq!(g.calls("get:g-gap"), 1, "stored ids are never re-fetched");
        let view = store.thread_view_with_html(acct, "g-gap").unwrap();
        assert!(
            !store.message_is_archive(acct, view.messages[0].id).unwrap(),
            "the live path's row, with its triage"
        );
        assert!(
            store
                .claim_agent_job(acct, "investigation", Utc::now(), 120)
                .unwrap()
                .is_some(),
            "and its job"
        );
    }

    /// `archive_days` IS ITS OWN WINDOW. The catch-up and spam listings, and
    /// the instant the backfill, cutover and heal measure from, are the same
    /// whether the archive is off, a year or a decade.
    ///
    /// The heal is run for real: a blank row inside the 30 days is handed back
    /// to triage and one outside gets its text and an access check only, at
    /// every archive depth. The cutover is held to the same instant by
    /// construction (it and the heal both call `backfill_since`, asserted
    /// below); its pass needs a model and is not run here.
    #[tokio::test]
    async fn archive_days_moves_no_other_window() {
        let mut wire = Vec::new();
        for archive_days in [0, 365, 3650] {
            let (store, acct) = store_at_cursor(None);
            let g = MockGmail::default();
            g.listing(LABEL_INBOX, &[]);
            g.listing(LABEL_SENT, &[]);
            g.listing(LABEL_SPAM, &[]);
            g.profile(900);
            let base = serve_mock(g.clone()).await;
            let eng = archive_engine(store.clone(), acct, &base, archive_days);
            // No cursor, so this poll is a catch-up over the backfill window.
            eng.poll_once().await.unwrap();
            eng.sync_spam_window().await.unwrap();
            let since = eng.backfill_since();
            assert!(
                (since - (Utc::now() - ChronoDuration::days(30)))
                    .num_seconds()
                    .abs()
                    < 30
            );
            assert!(g.queries("").is_empty(), "none of these list all mail");

            let recent = blank_bill_row(&store, acct, "g-recent", ChronoDuration::days(20));
            let old = blank_bill_row(&store, acct, "g-old", ChronoDuration::days(60));
            g.body("g-recent", html_only_eml(Utc::now(), GARMIN_HTML));
            g.body("g-old", html_only_eml(Utc::now(), GARMIN_HTML));
            eng.heal_blank_bodies().await.unwrap();
            let triage = queued_job(&store, acct, "triage").expect("inside the window");
            assert_eq!(
                (triage.message_id, triage.trigger.as_str()),
                (recent, "heal")
            );
            assert!(
                queued_job(&store, acct, "triage").is_none(),
                "archive_days={archive_days}: 60 days is outside the heal's window"
            );
            let access = queued_job(&store, acct, "access").expect("text only");
            assert_eq!(access.message_id, old);
            wire.push((
                g.queries(LABEL_INBOX),
                g.queries(LABEL_SENT),
                g.queries(LABEL_SPAM),
            ));
        }
        let thirty = vec!["newer_than:30d".to_string()];
        for seen in &wire {
            assert_eq!(seen, &(thirty.clone(), thirty.clone(), thirty.clone()));
        }
    }

    /// A LIVE PATH RE-SEEING AN ARCHIVE ROW is a true no-op: the store writes
    /// nothing, and the engine neither re-embeds the row nor wakes the refine
    /// lane for work that was never queued.
    #[tokio::test]
    async fn a_live_sighting_of_an_archive_row_embeds_nothing_and_wakes_no_lane() {
        let (store, acct) = store_at_cursor(Some(100));
        let g = MockGmail::default();
        g.body("g-old", alert_eml(Utc::now()));
        g.body("g-live", alert_eml(Utc::now()));
        let base = serve_mock(g.clone()).await;
        let eng = engine(store.clone(), acct, &base);
        let poked = || tokio::time::timeout(Duration::ZERO, eng.refine_wake.notified());
        let fetch = |id: &str, origin| {
            let ids = [id.to_string()];
            let eng = &eng;
            async move {
                eng.fetch_raw_and_ingest(&ids, Mailbox::Inbox, origin, Urgency::Live)
                    .await
                    .unwrap()
            }
        };
        fetch("g-old", IngestOrigin::Archive).await;

        // A catch-up overlapping the archive's edge finds the same id.
        assert_eq!(fetch("g-old", IngestOrigin::Incremental).await, 1);
        assert!(poked().await.is_err(), "nothing was queued to wake for");
        let raw = alert_eml(Utc::now());
        assert!(
            eng.ingest_one(
                &fixture(acct, "g-old", &raw, false),
                &[],
                Utc::now(),
                IngestOrigin::Incremental,
            )
            .unwrap()
            .is_none(),
            "no embed text for a row this call did not write"
        );

        // The control: a row the live path DID write is embedded and wakes it.
        assert_eq!(fetch("g-live", IngestOrigin::Incremental).await, 1);
        assert!(poked().await.is_ok());
    }

    /// THE BOARD FILLS WHILE THE DOWNLOAD IS STILL GOING. A first run used to
    /// hold every model call until the last message of the window had landed;
    /// now the refine lane runs beside the backfill and claims rows as ingest
    /// queues them. Asserted on the wire: the model is asked about the first
    /// message while the second is still in flight and no cursor exists yet.
    #[tokio::test]
    async fn a_first_run_triages_while_the_backfill_is_still_downloading() {
        let g = MockGmail::default();
        g.listing(LABEL_INBOX, &["a", "b"]);
        g.listing(LABEL_SENT, &[]);
        g.body("a", alert_eml(Utc::now()));
        g.body("b", alert_eml(Utc::now()));
        g.slow_body("b", Duration::from_secs(20));
        g.history(LABEL_INBOX, LabelHistory::quiet(1));
        g.history(LABEL_SENT, LabelHistory::quiet(1));
        g.profile(1);
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(None);

        // Any answer proves the ask; a 503 keeps the test from needing a
        // decision fixture and trips the circuit so there is exactly one call.
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        let app = Router::new().route(
            "/",
            axum::routing::post(move || {
                count.fetch_add(1, Ordering::Relaxed);
                async { StatusCode::SERVICE_UNAVAILABLE }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm = ResolvedLlm {
            url: format!("http://{}/", listener.local_addr().unwrap()),
            api_key: "test".into(),
            provider: Stage2Provider::OpenAI,
        };
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let metrics = SyncMetrics::new();
        let mut e = engine(store.clone(), acct, &base).with_metrics(metrics.clone());
        e.stage2_llm = Some(llm);
        let (stop, rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move { e.run(rx).await });

        wait_for(10, "a model call during the backfill", || {
            calls.load(Ordering::Relaxed) >= 1
        })
        .await;
        assert_eq!(
            metrics.catchup_progress(),
            Some((1, 2)),
            "the second message was still in flight when the model was asked"
        );
        assert_eq!(cursor_of(&store, acct), None, "and no cursor existed yet");
        assert!(
            !metrics.poll_ok(),
            "no poll has run; the listing was the proof"
        );

        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        server.abort();
    }

    /// LIVE MAIL ARRIVES DURING THE FIRST LOAD. A new account's backfill is
    /// minutes of downloads; mail that lands meanwhile is the owner's first
    /// proof the app works. It used to wait for the whole window, and on Gmail
    /// it was LOST: the historyId was asked for after the fetches, past it.
    /// Now the cursor is minted before the listing and the change feed is
    /// walked from it beside the download: the live message is ingested while
    /// the window is still in flight, as live mail (stamped to notify, not
    /// first-load backlog), the window's progress count is untouched by it,
    /// and the stored cursor is where the live walk got to, not the minted one.
    #[tokio::test]
    async fn live_mail_arrives_while_the_first_backfill_downloads() {
        let g = MockGmail::default();
        g.listing(LABEL_INBOX, &["a", "b"]);
        g.listing(LABEL_SENT, &[]);
        g.body("a", alert_eml(Utc::now()));
        g.body("b", alert_eml(Utc::now()));
        g.slow_body("b", Duration::from_secs(8));
        // Arrived after the listing: in the change feed, in no listing.
        g.body("live", alert_eml(Utc::now()));
        g.history(LABEL_INBOX, LabelHistory::added(5, &[(5, "live")]));
        g.history(LABEL_SENT, LabelHistory::quiet(5));
        g.profile(1);
        let base = serve_mock(g.clone()).await;
        let (store, acct) = store_at_cursor(None);

        let mut config = Config::default();
        config.sync.poll_secs = 1;
        let metrics = SyncMetrics::new();
        let e =
            engine_with_config(store.clone(), acct, &base, config).with_metrics(metrics.clone());
        let (stop, rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move { e.run(rx).await });

        let seen = store.clone();
        wait_for(6, "the live message during the backfill", || {
            seen.triage_by_provider_id(acct, "live").unwrap().is_some()
        })
        .await;
        assert_eq!(
            cursor_of(&store, acct),
            None,
            "the backfill was still going"
        );
        assert_eq!(
            metrics.catchup_progress(),
            Some((1, 2)),
            "the live message is no part of the window's count"
        );
        assert_eq!(
            store.triage_by_provider_id(acct, "live").unwrap(),
            Some((false, true)),
            "live mail: not first-load, and stamped to notify"
        );

        let seen = store.clone();
        wait_for(15, "the backfill to finish", || {
            cursor_of(&seen, acct).is_some()
        })
        .await;
        assert_eq!(
            cursor_of(&store, acct),
            Some(5),
            "the cursor is where the live walk got to"
        );
        assert_eq!(
            store.triage_by_provider_id(acct, "b").unwrap(),
            Some((true, false)),
            "the window itself is still silent first-load backlog"
        );

        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    /// Exercise parsing, the durable claim, real model transport, validation,
    /// atomic projections, and the independent late-notification queue together.
    #[tokio::test]
    async fn agent_worker_commits_model_decision_without_legacy_placement() {
        use crate::store::agent_triage::AgentTriageStore;
        use crate::triage::decision::*;
        let (store, account) = store_at_cursor(Some(100));
        let raw = fixture(
            account,
            "agent-offer",
            "From: shop@example.com\r\nSubject: A sale\r\n\r\nToday we offer a discount.",
            false,
        );
        let mut parsed =
            ingest_with_rules(&raw, &Stage1Config::default(), Utc::now(), &[], |_| false);
        parsed.notify_eligible_at = Some(Utc::now());
        let id = store.ingest_message(&parsed).unwrap();
        assert!(store.agent_reading(account, 20).unwrap().is_empty());
        assert!(!store.agent_access_allowed(account, id).unwrap());
        let evidence = vec![EvidenceRef {
            message_id: id,
            location: "body".into(),
        }];
        let decision = MessageDecision {
            kinds: vec![EmailKind::Promotional],
            destinations: vec![MessageDestination::Reading],
            summary: "An offer from the shop".into(),
            reason: "Promotions belong in Reading by default".into(),
            external_access: AccessAssessment {
                restricted: false,
                reason: "No access credentials".into(),
                evidence: evidence.clone(),
            },
            attention: ThreadAttentionDecision {
                relevant_message_ids: vec![id],
                evidence,
                ..Default::default()
            },
            ..Default::default()
        };
        let response = json!({"choices":[{"message":{"content":json!({"result":{"step":"finish", "decision":decision}}).to_string()},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":12,"completion_tokens":8}});
        let app = Router::new().route(
            "/",
            axum::routing::post(move || {
                let response = response.clone();
                async move { Json(response) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let engine = engine(store.clone(), account, "http://127.0.0.1:1");
        let job = store
            .claim_agent_job(account, "triage", Utc::now(), 120)
            .unwrap()
            .unwrap();
        engine
            .process_agent_job(
                job,
                &ResolvedLlm {
                    api_key: "test".into(),
                    provider: Stage2Provider::OpenAI,
                    url: format!("http://{address}/"),
                },
                &Utc::now().format("%Y-%m-%d").to_string(),
            )
            .await;
        assert_eq!(store.agent_reading(account, 20).unwrap()[0].message_id, id);
        assert!(
            tokio::time::timeout(Duration::ZERO, engine.notify_wake.notified())
                .await
                .is_ok(),
            "an applied commit of an arrival-eligible job wakes the notification \
             lane for the deliberate push it queued"
        );
        assert!(
            store
                .agent_fye(account, 20, &Default::default(), Utc::now())
                .unwrap()
                .is_empty()
        );
        assert!(store.agent_access_allowed(account, id).unwrap());
        assert!(
            store
                .claim_agent_job(account, "deliberate_notification", Utc::now(), 30)
                .unwrap()
                .is_some()
        );
        let diagnostics = store.agent_diagnostics(account, id).unwrap();
        assert!(
            diagnostics
                .to_string()
                .contains(crate::triage::agent::prompt_version(false))
        );
        server.abort();
    }
    /// THE MEMORY SCRUB, END TO END: with `understanding` on and a memory line
    /// served, a model that copies the line into its summary has that run
    /// thrown away and the job re-run ONCE on the flag-off prompt with no
    /// memory; that answer commits, and the run's metadata says scrubbed.
    /// The sender has a relationship row too: the first request carries the
    /// block, and the re-run carries neither it nor the memory line.
    #[tokio::test]
    async fn a_decision_that_repeats_a_memory_line_is_rerun_without_memory() {
        use crate::store::agent_triage::AgentTriageStore;
        use crate::triage::decision::*;
        use crate::triage::memory::{MemoryRecord, Origin, Provenance, new_ulid};
        const LINE: &str = "Runs a small pottery studio with two partners in Leeds.";
        let (store, account) = store_at_cursor(Some(100));
        let raw = fixture(
            account,
            "agent-memory",
            "From: shop@example.com\r\nSubject: A sale\r\n\r\nToday we offer a discount.",
            false,
        );
        let parsed = ingest_with_rules(&raw, &Stage1Config::default(), Utc::now(), &[], |_| false);
        let id = store.ingest_message(&parsed).unwrap();
        // The sender's ledger row, backfilled, so the flag-on run is handed it.
        store.recompute_all_relationships(account).unwrap();
        store
            .set_sync_state(
                account,
                crate::store::sqlite::relationships::RELATIONSHIPS_BACKFILL_KEY,
                &SyncState {
                    uidvalidity: 0,
                    last_uid: 1,
                },
            )
            .unwrap();
        let now = Utc::now();
        store
            .insert_memory_entry_unchecked(
                account,
                &MemoryRecord {
                    id: new_ulid(now),
                    scope: "owner".into(),
                    origin: Origin::Stated,
                    kind: "role".into(),
                    body: json!({"what": LINE}),
                    line: LINE.into(),
                    provenance: Provenance::default(),
                    confidence: 1.0,
                    revision: 1,
                    created_at: now,
                    updated_at: now,
                    affirmed_at: now,
                    expires_at: None,
                    retired_at: None,
                    deleted_at: None,
                },
            )
            .unwrap();
        let evidence = vec![EvidenceRef {
            message_id: id,
            location: "body".into(),
        }];
        let decision = |summary: &str| MessageDecision {
            kinds: vec![EmailKind::Promotional],
            destinations: vec![MessageDestination::Reading],
            summary: summary.into(),
            reason: "Promotions belong in Reading by default".into(),
            external_access: AccessAssessment {
                restricted: false,
                reason: "No access credentials".into(),
                evidence: evidence.clone(),
            },
            attention: ThreadAttentionDecision {
                relevant_message_ids: vec![id],
                evidence: evidence.clone(),
                ..Default::default()
            },
            ..Default::default()
        };
        let answer = |d: MessageDecision| {
            json!({"choices":[{"message":{"content":json!({"result":{"step":"finish", "decision":d}}).to_string()},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":12,"completion_tokens":8}})
        };
        // Echoes the memory line whenever the request carries it; answers
        // cleanly when it does not.
        let echo = answer(decision(&format!("The owner {LINE}")));
        let clean = answer(decision("An offer from the shop"));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let bodies = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let kept = bodies.clone();
        let app = Router::new().route(
            "/",
            axum::routing::post(move |body: String| {
                seen.fetch_add(1, Ordering::SeqCst);
                kept.lock().unwrap().push(body.clone());
                let response = if body.contains("pottery studio") {
                    echo.clone()
                } else {
                    clean.clone()
                };
                async move { Json(response) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let engine = engine(store.clone(), account, "http://127.0.0.1:1").with_features(
            crate::features::Features::pinned([crate::features::UNDERSTANDING]),
        );
        let job = store
            .claim_agent_job(account, "triage", Utc::now(), 120)
            .unwrap()
            .unwrap();
        engine
            .process_agent_job(
                job,
                &ResolvedLlm {
                    api_key: "test".into(),
                    provider: Stage2Provider::OpenAI,
                    url: format!("http://{address}/"),
                },
                &Utc::now().format("%Y-%m-%d").to_string(),
            )
            .await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "one run, one scrubbed re-run"
        );
        {
            let bodies = bodies.lock().unwrap();
            // The request body is JSON, so the context's key arrives escaped.
            assert!(
                bodies[0].contains(r#"\"relationship\""#),
                "control: the flag-on run is handed the ledger"
            );
            assert!(
                !bodies[1].contains(r#"\"relationship\""#),
                "the re-run carries no relationship block"
            );
            assert!(!bodies[1].contains("pottery"), "nor the memory line");
        }
        let reading = store.agent_reading(account, 20).unwrap();
        assert_eq!(reading[0].message_id, id);
        assert_eq!(reading[0].decision.summary, "An offer from the shop");
        let diagnostics = store.agent_diagnostics(account, id).unwrap().to_string();
        assert!(!diagnostics.contains("pottery"), "{diagnostics}");
        let metadata = &store.agent_diagnostics(account, id).unwrap()["runs"][0]["metadata"];
        assert_eq!(metadata["memory_scrubbed"], true);
        assert_eq!(metadata["understanding"], false);
        assert_eq!(
            metadata["prompt_version"],
            crate::triage::agent::prompt_version(false)
        );
        server.abort();
    }
    /// THE AGENT'S DONE, END TO END: the real worker, transport, validation
    /// and commit, with a model that answers `done` every time. On the
    /// arrival run the answer commits and closes nothing; on a scheduled
    /// re-check (`revisit:` trigger) the same answer marks the message done.
    #[tokio::test]
    async fn agent_done_closes_mail_only_on_a_scheduled_recheck() {
        use crate::store::agent_triage::AgentTriageStore;
        use crate::triage::decision::*;
        use crate::types::AttentionStatus;
        let (store, account) = store_at_cursor(Some(100));
        let raw = fixture(
            account,
            "agent-show",
            "From: tickets@venue.example\r\nSubject: Your tickets\r\n\r\nThe show is on Friday.",
            false,
        );
        let parsed = ingest_with_rules(&raw, &Stage1Config::default(), Utc::now(), &[], |_| false);
        let id = store.ingest_message(&parsed).unwrap();
        // Authenticated: a done may rest only on an authenticated sender's
        // words.
        store.execute_batch_for_test(&format!("UPDATE messages SET auth_pass=1 WHERE id={id}"));
        let evidence = vec![EvidenceRef {
            message_id: id,
            location: "body".into(),
        }];
        let decision = MessageDecision {
            kinds: vec![EmailKind::General],
            summary: "Tickets for a show".into(),
            reason: "Tickets".into(),
            external_access: AccessAssessment {
                restricted: false,
                reason: "No access credentials".into(),
                evidence: evidence.clone(),
            },
            attention: ThreadAttentionDecision {
                relevant_message_ids: vec![id],
                evidence: evidence.clone(),
                ..Default::default()
            },
            done: Some(DoneRequest {
                reason: "The show is over".into(),
                evidence,
            }),
            ..Default::default()
        };
        let response = json!({"choices":[{"message":{"content":json!({"result":{"step":"finish", "decision":decision}}).to_string()},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":12,"completion_tokens":8}});
        let app = Router::new().route(
            "/",
            axum::routing::post(move || {
                let response = response.clone();
                async move { Json(response) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let engine = engine(store.clone(), account, "http://127.0.0.1:1");
        let llm = ResolvedLlm {
            api_key: "test".into(),
            provider: Stage2Provider::OpenAI,
            url: format!("http://{address}/"),
        };
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let status = || {
            inventory(
                &store,
                account,
                Utc::now() - ChronoDuration::days(30),
                SpamScope::Exclude,
            )
            .into_iter()
            .find(|row| row.update.id == id)
            .map(|row| row.status)
        };
        let job = store
            .claim_agent_job(account, "investigation", Utc::now(), 120)
            .unwrap()
            .unwrap();
        assert!(!job.trigger.starts_with("revisit:"));
        engine.process_agent_job(job, &llm, &day).await;
        assert!(
            store
                .external_agent_decision(account, id)
                .unwrap()
                .done
                .is_some(),
            "the arrival run committed its answer"
        );
        assert_eq!(status(), Some(AttentionStatus::New), "and closed nothing");
        store
            .enqueue_agent_triage(account, id, "revisit:2026-10-20T00:00:00+00:00", false)
            .unwrap();
        let job = store
            .claim_agent_job(account, "investigation", Utc::now(), 120)
            .unwrap()
            .unwrap();
        assert!(job.trigger.starts_with("revisit:"));
        engine.process_agent_job(job, &llm, &day).await;
        assert_eq!(status(), Some(AttentionStatus::Done));
        server.abort();
    }

    /// An applied access check can be what makes a message embeddable (own
    /// mail waits for it), so it raises the drain's flag like a triage
    /// decision does. And a correction made through the API, which no engine
    /// sees, reaches the drain through the store.
    #[tokio::test]
    async fn an_applied_access_check_and_a_correction_raise_the_vector_drain() {
        use crate::store::agent_triage::AgentTriageStore;
        let (store, account) = store_at_cursor(Some(100));
        // Own sent mail: filed access-only, so its access check is its job.
        let raw = fixture(
            account,
            "access-dirty",
            "From: me@example.com\r\nTo: friend@example.com\r\nSubject: Notes\r\n\r\nThe notes from Tuesday.",
            true,
        );
        let parsed = ingest_with_rules(&raw, &Stage1Config::default(), Utc::now(), &[], |_| false);
        let id = store.ingest_message(&parsed).unwrap();
        let content = json!({"restricted": false, "reason": "No credentials"}).to_string();
        let response = json!({"choices":[{"message":{"content":content},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":12,"completion_tokens":8}});
        let app = Router::new().route(
            "/",
            axum::routing::post(move || {
                let response = response.clone();
                async move { Json(response) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let engine = engine(store.clone(), account, "http://127.0.0.1:1");
        let llm = ResolvedLlm {
            api_key: "test".into(),
            provider: Stage2Provider::OpenAI,
            url: format!("http://{address}/"),
        };
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let job = store
            .claim_agent_job(
                account,
                "access",
                Utc::now() + ChronoDuration::hours(1),
                120,
            )
            .unwrap()
            .expect("an access job waits");
        engine.vectors_dirty.store(false, Ordering::Relaxed);
        engine.process_agent_job(job, &llm, &day).await;
        assert!(
            store.agent_access_allowed(account, id).unwrap(),
            "the check applied"
        );
        assert!(
            engine.vectors_dirty.load(Ordering::Relaxed),
            "and told the drain"
        );
        server.abort();

        engine.vectors_dirty.store(false, Ordering::Relaxed);
        let _ = store.take_vectors_owed();
        assert!(!engine.vectors_owed());
        store
            .correct_agent_triage(account, id, "external_access", &json!(false), Utc::now())
            .unwrap();
        assert!(engine.vectors_owed(), "the correction reached the drain");
        assert!(engine.vectors_dirty.load(Ordering::Relaxed));
    }
    /// THE PAST-EVENT RE-CHECK, END TO END: the model's arrival answer
    /// records a show that is over (and asks for done, which an arrival run
    /// never applies); the real commit projects it into a ghost; the poll
    /// tick's scheduler queues the re-check; the real worker runs it with the
    /// same answer, and the message is done.
    #[tokio::test]
    async fn a_past_events_mail_is_rechecked_and_the_agent_closes_it() {
        use crate::store::agent_triage::AgentTriageStore;
        use crate::triage::decision::*;
        use crate::types::AttentionStatus;
        let (store, account) = store_at_cursor(Some(100));
        let raw = fixture(
            account,
            "agent-past-show",
            "From: tickets@venue.example\r\nSubject: Your tickets\r\n\r\nThe show was on Friday.",
            false,
        );
        let parsed = ingest_with_rules(&raw, &Stage1Config::default(), Utc::now(), &[], |_| false);
        let id = store.ingest_message(&parsed).unwrap();
        // Authenticated, so the ghost it makes has an owner.
        store.execute_batch_for_test(&format!("UPDATE messages SET auth_pass=1 WHERE id={id}"));
        let evidence = vec![EvidenceRef {
            message_id: id,
            location: "body".into(),
        }];
        let start = Utc::now() - ChronoDuration::days(3);
        let at = |t: DateTime<Utc>| {
            Some(SupportedTime {
                value: t.to_rfc3339(),
                timezone: None,
                source_message_id: id,
                interpreted_relative: false,
            })
        };
        let decision = MessageDecision {
            kinds: vec![EmailKind::General],
            summary: "Tickets for a show".into(),
            reason: "Tickets".into(),
            external_access: AccessAssessment {
                restricted: false,
                reason: "No access credentials".into(),
                evidence: evidence.clone(),
            },
            attention: ThreadAttentionDecision {
                relevant_message_ids: vec![id],
                evidence: evidence.clone(),
                ..Default::default()
            },
            records: vec![RecordProposal::Event {
                title: "Show".into(),
                start: at(start),
                location: None,
                end: at(start + ChronoDuration::hours(3)),
                duration_estimate: None,
                event_kind: EventRecordKind::Ticket,
                commitment: EventCommitment::Committed,
                change: EventChange::Created,
                identity_refs: vec!["SEAT12".into()],
                likelihood: None,
                evidence: evidence.clone(),
                identity: None,
            }],
            done: Some(DoneRequest {
                reason: "The show is over".into(),
                evidence,
            }),
            ..Default::default()
        };
        let response = json!({"choices":[{"message":{"content":json!({"result":{"step":"finish", "decision":decision}}).to_string()},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":12,"completion_tokens":8}});
        let app = Router::new().route(
            "/",
            axum::routing::post(move || {
                let response = response.clone();
                async move { Json(response) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let engine = engine(store.clone(), account, "http://127.0.0.1:1");
        let llm = ResolvedLlm {
            api_key: "test".into(),
            provider: Stage2Provider::OpenAI,
            url: format!("http://{address}/"),
        };
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let status = || {
            inventory(
                &store,
                account,
                Utc::now() - ChronoDuration::days(30),
                SpamScope::Exclude,
            )
            .into_iter()
            .find(|row| row.update.id == id)
            .map(|row| row.status)
        };
        let job = store
            .claim_agent_job(account, "investigation", Utc::now(), 120)
            .unwrap()
            .unwrap();
        assert!(!job.trigger.starts_with("revisit:"));
        engine.process_agent_job(job, &llm, &day).await;
        assert_eq!(
            status(),
            Some(AttentionStatus::New),
            "arrival closes nothing"
        );
        let config = crate::config::RevisitPassConfig::default();
        assert_eq!(
            store
                .schedule_past_event_revisits(account, Utc::now(), &config)
                .unwrap(),
            vec![id],
            "the show ended more than a day ago"
        );
        let job = store
            .claim_agent_job(account, "investigation", Utc::now(), 120)
            .unwrap()
            .unwrap();
        assert!(job.trigger.starts_with("revisit:event_ended:"));
        engine.process_agent_job(job, &llm, &day).await;
        assert_eq!(status(), Some(AttentionStatus::Done));
        server.abort();
    }
    #[tokio::test]
    async fn access_jobs_make_one_small_model_call_without_placement_or_fanout() {
        use crate::store::agent_triage::AgentTriageStore;
        for extra_field in [false, true] {
            let (store, account) = store_at_cursor(Some(100));
            let fetched = fixture(
                account,
                "legacy-access",
                "From: alice@example.com\r\nSubject: Old note\r\n\r\nHello",
                false,
            );
            let parsed =
                ingest_with_rules(&fetched, &Stage1Config::default(), Utc::now(), &[], |_| {
                    false
                });
            let id = store.upsert_message(&parsed.message).unwrap();
            let mut sibling = parsed.message.clone();
            sibling.gmail_msg_id = "unseen-sibling".into();
            sibling.body = "SIBLING_MUST_NOT_ENTER_ACCESS_PROMPT".into();
            store.upsert_message(&sibling).unwrap();
            store
                .enqueue_agent_triage(account, id, "source_access", false)
                .unwrap();
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counted = calls.clone();
            let app=Router::new().route("/",axum::routing::post(move |Json(request):Json<serde_json::Value>| {
                let counted=counted.clone();
                async move {
                    counted.fetch_add(1,Ordering::Relaxed);
                    assert_eq!(request["model"],"access-test-model");
                    assert!(!request.to_string().contains("SIBLING_MUST_NOT_ENTER_ACCESS_PROMPT"));
                    assert!(request["response_format"]["json_schema"]["schema"]["properties"]["destinations"].is_null());
                    let mut verdict=json!({"restricted":false,"reason":"No actionable authentication material"});
                    if extra_field { verdict["destinations"]=json!(["reading"]); }
                    Json(json!({"choices":[{"message":{"content":verdict.to_string()},"finish_reason":"stop"}],
                        "usage":{"prompt_tokens":20,"completion_tokens":10}}))
                }
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let llm = ResolvedLlm {
                url: format!("http://{}/", listener.local_addr().unwrap()),
                api_key: "test".into(),
                provider: Stage2Provider::OpenAI,
            };
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let mut config = Config::default();
            config.notify.model = "access-test-model".into();
            let engine = engine_with_config(store.clone(), account, "http://127.0.0.1:1", config);
            let job = store
                .claim_agent_job(account, "investigation", Utc::now(), 60)
                .unwrap()
                .unwrap();
            assert_eq!(job.kind, "access");
            engine
                .process_agent_job(
                    job.clone(),
                    &llm,
                    &Utc::now().format("%Y-%m-%d").to_string(),
                )
                .await;
            assert_eq!(
                calls.load(Ordering::Relaxed),
                1,
                "no tools or repair turns in access contract"
            );
            assert_eq!(
                store.agent_access_allowed(account, id).unwrap(),
                !extra_field
            );
            let context = store.load_agent_context(&job).unwrap();
            assert!(context.previous_decision.is_none());
            assert!(context.attention.is_none());
            assert!(store.agent_reading(account, 20).unwrap().is_empty());
            assert!(store.agent_records(account, 20).unwrap().is_empty());
            assert_eq!(
                store.agent_diagnostics(account, id).unwrap()["jobs"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            let usage = store.list_usage_by_category(account, 1).unwrap();
            let access = usage
                .iter()
                .find(|(name, _)| name == crate::triage::access::USAGE_CATEGORY)
                .unwrap();
            assert_eq!(
                access.1.iter().map(|row| row.calls).sum::<u64>(),
                1,
                "invalid paid output still records usage"
            );
            server.abort();
        }
    }

    /// The onboarding effort goes to the jobs on the foreground lane, from
    /// the account's first day: an arrival (the incremental origin) and a
    /// person's one-message request. The history a new account backfills
    /// (the backfill origin), the `migration` jobs the cutover queues for an
    /// account that predates the agent, and a person's window re-triage (a
    /// `manual:` trigger the store keeps off the lane) go out at the
    /// steady-state effort. Driven through the real ingest origins and
    /// `process_agent_job`, with the effort read off the request the mock
    /// model received for each.
    /// Which path FIRST brought a message in is what makes it first-load
    /// (`triage.first_load`, the FYE allowance's input): the backfill, and
    /// only the backfill. A poll re-sighting the same mail does not move it.
    #[tokio::test]
    async fn only_the_backfill_stamps_mail_first_load() {
        let (store, account) = store_at_cursor(Some(100));
        let now = Utc::now();
        let engine = engine(store.clone(), account, "http://127.0.0.1:1");
        let raw = |msgid: &str| {
            fixture(
                account,
                msgid,
                "From: alice@example.com\r\nSubject: A note\r\n\r\nHello",
                false,
            )
        };
        let ingest = |msgid: &str, origin| {
            engine
                .ingest_one(&raw(msgid), &[], now, origin)
                .unwrap()
                .expect("ingested")
                .id
        };
        let first_load = |id: i64| store.triage_first_load(account, id).unwrap();
        let backlog = ingest("backlog", IngestOrigin::Backfill);
        let arrival = ingest("arrival", IngestOrigin::Incremental);
        assert!(first_load(backlog));
        assert!(!first_load(arrival));
        ingest("backlog", IngestOrigin::Incremental);
        assert!(first_load(backlog), "a re-sighting is the same mail");
    }

    #[tokio::test]
    async fn only_the_jobs_somebody_waits_on_go_out_at_the_onboarding_effort() {
        use crate::store::agent_triage::AgentTriageStore;
        let (store, account) = store_at_cursor(Some(100));
        let now = Utc::now();
        let engine = engine(store.clone(), account, "http://127.0.0.1:1");
        assert_eq!(
            engine.config.stage2.onboarding_effort.as_deref(),
            Some("xhigh"),
            "the default onboarding effort is what this test measures against"
        );
        // Five messages in five threads: the claim hands out one job per
        // thread at a time, and a job it has already handed out stays leased.
        let raw = |msgid: &str| {
            let mut raw = fixture(
                account,
                msgid,
                "From: alice@example.com\r\nSubject: A note\r\n\r\nHello",
                false,
            );
            raw.gmail_thread_id = Some(format!("thread-{msgid}"));
            raw
        };
        let stored = |msgid: &str| {
            let parsed =
                ingest_with_rules(&raw(msgid), &Stage1Config::default(), now, &[], |_| false);
            store.upsert_message(&parsed.message).unwrap()
        };
        // A row that predates the agent (no state row), which the cutover
        // queues as `migration`; then the two origins; then a person's
        // window re-triage (one row of it, enqueued as `retriage_reset`
        // enqueues a window: off the lane) and a person's one-message
        // request (through `retriage_reset` itself, which puts it on the
        // lane) on a backfilled row.
        stored("old");
        store
            .initialize_agent_cutover(account, now - ChronoDuration::days(30))
            .unwrap();
        engine
            .ingest_one(&raw("history"), &[], now, IngestOrigin::Backfill)
            .unwrap();
        engine
            .ingest_one(&raw("arrival"), &[], now, IngestOrigin::Incremental)
            .unwrap();
        let window = stored("window");
        store
            .enqueue_agent_triage(account, window, "manual:window", false)
            .unwrap();
        let asked = engine
            .ingest_one(&raw("asked"), &[], now, IngestOrigin::Backfill)
            .unwrap()
            .expect("ingested")
            .id;
        assert_eq!(store.retriage_reset(account, Some(asked), 7).unwrap(), 1);

        let efforts: Arc<std::sync::Mutex<Vec<serde_json::Value>>> = Default::default();
        let seen = efforts.clone();
        let app = Router::new().route(
            "/",
            axum::routing::post(move |Json(body): Json<serde_json::Value>| {
                let seen = seen.clone();
                async move {
                    seen.lock()
                        .unwrap()
                        .push(body["output_config"]["effort"].clone());
                    // Nothing a step can use: the job retries, the request
                    // is what this test is about.
                    Json(json!({}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm = ResolvedLlm {
            url: format!("http://{}/", listener.local_addr().unwrap()),
            api_key: "test".into(),
            // The effort rides the Anthropic wire (`output_config.effort`).
            provider: Stage2Provider::Anthropic,
        };
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let day = now.format("%Y-%m-%d").to_string();
        let mut sent = std::collections::BTreeMap::new();
        // One job per claim, in the claim's own order; each failed job is
        // parked past this claim time after it runs, so each is seen once.
        // A job makes several requests (every step the agent retries before
        // giving the job up), all at the one effort.
        for _ in 0..5 {
            let job = store
                .claim_agent_job(
                    account,
                    "investigation",
                    Utc::now() + ChronoDuration::seconds(1),
                    120,
                )
                .unwrap()
                .expect("a job to run");
            // A one-message request's trigger carries its timestamp; the
            // lane is what tells it from the window.
            let trigger = match (job.trigger.as_str(), job.foreground) {
                (t, true) if t.starts_with("manual:") => "manual (one message)".to_string(),
                (t, false) if t.starts_with("manual:") => "manual (window)".to_string(),
                (t, _) => t.to_string(),
            };
            let before = efforts.lock().unwrap().len();
            let ran = job.clone();
            engine.process_agent_job(job, &llm, &day).await;
            // Its retry is due at once now; park it so the next claim is
            // the next job, which is what this test walks.
            store.execute_batch_for_test(&format!(
                "UPDATE agent_triage_jobs SET available_at='{}' WHERE id={}",
                (Utc::now() + ChronoDuration::hours(1)).to_rfc3339(),
                ran.id
            ));
            // An empty answer is the job's failure, not a provider outage:
            // nothing here may put the account on a cooldown that would
            // keep the next job from making its request.
            assert_eq!(engine.agent_retry_after.load(Ordering::Relaxed), 0);
            let requests = efforts.lock().unwrap();
            let made = &requests[before..];
            let effort = made
                .first()
                .cloned()
                .unwrap_or_else(|| panic!("{trigger} made no request"));
            assert!(
                made.iter().all(|e| *e == effort),
                "{trigger} changed effort between steps: {made:?}"
            );
            sent.insert(trigger, effort);
        }
        assert_eq!(
            sent,
            std::collections::BTreeMap::from([
                ("backfill".to_string(), json!("low")),
                ("ingest".to_string(), json!("xhigh")),
                ("manual (one message)".to_string(), json!("xhigh")),
                ("manual (window)".to_string(), json!("low")),
                ("migration".to_string(), json!("low")),
            ])
        );
        server.abort();
    }

    /// The engine keeps the account's creation time once it has one, and
    /// keeps asking while it has none: a store that could not say on the
    /// first job must not pin a new account at the steady-state effort for
    /// its window.
    #[tokio::test]
    async fn the_creation_time_is_kept_only_once_found() {
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        // The engine is built for the account the store is about to hold,
        // before it holds it: the first ask finds nothing.
        let engine = engine(store.clone(), 1, "http://127.0.0.1:1");
        assert_eq!(engine.account_created_at(), None);
        let account = store.ensure_account("me@example.com").unwrap();
        assert_eq!(account, 1);
        let created = engine.account_created_at().expect("found now");
        // Kept: the store's answer no longer matters, even once it has none.
        store.test_set_account_created_at(account, "not a stamp");
        assert_eq!(store.account_created_at(account).unwrap(), None);
        assert_eq!(engine.account_created_at(), Some(created));
    }

    /// A throttle stands the account off for what the provider asked, a
    /// server error (and a throttle that named no time) for a short spread
    /// wait, and a configuration failure for the outage interval. The one
    /// 429 that used to cost every job on the account five minutes now
    /// costs the seconds the provider named.
    #[tokio::test]
    async fn a_throttle_stands_the_account_off_for_the_providers_retry_after() {
        use crate::store::agent_triage::AgentTriageStore;
        for (status, retry_after, lo, hi) in [
            (StatusCode::TOO_MANY_REQUESTS, Some("7"), 6, 23),
            // No number from the provider: a 429 is most often a limit the
            // host's tenants share, so it starts short and spread; a server
            // error retries at once.
            (StatusCode::TOO_MANY_REQUESTS, None, 14, 30),
            (StatusCode::SERVICE_UNAVAILABLE, None, -1, 0),
            (StatusCode::FORBIDDEN, None, 284, 300),
        ] {
            let (store, account) = store_at_cursor(Some(100));
            ingest_into(
                &store,
                account,
                &fixture(
                    account,
                    "throttled",
                    "From: alice@example.com\r\nSubject: Hello\r\n\r\nA note",
                    false,
                ),
                Utc::now(),
            );
            let app = Router::new().route(
                "/",
                axum::routing::post(move || async move {
                    let mut resp =
                        (status, Json(json!({"error": {"type": "unavailable"}}))).into_response();
                    if let Some(after) = retry_after {
                        resp.headers_mut()
                            .insert("retry-after", after.parse().unwrap());
                    }
                    resp
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let llm = ResolvedLlm {
                url: format!("http://{}/", listener.local_addr().unwrap()),
                api_key: "test".into(),
                provider: Stage2Provider::OpenAI,
            };
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let engine = engine(store.clone(), account, "http://127.0.0.1:1");
            let day = Utc::now().format("%Y-%m-%d").to_string();
            let job = store
                .claim_agent_job(account, "investigation", Utc::now(), 120)
                .unwrap()
                .unwrap();
            engine.process_agent_job(job, &llm, &day).await;
            let wait = engine.agent_retry_after.load(Ordering::Relaxed) - Utc::now().timestamp();
            assert!(
                (lo..=hi).contains(&wait),
                "{status}, retry-after {retry_after:?}: stood off {wait}s"
            );
            server.abort();
        }
    }

    /// A throttle that does not lift (a spent monthly budget answers 429 to
    /// every probe) is probed on a doubling ladder up to the outage
    /// interval, not every fifteen seconds for the rest of the month.
    #[tokio::test]
    async fn a_throttle_that_does_not_lift_backs_off_to_the_outage_interval() {
        use crate::store::agent_triage::AgentTriageStore;
        let (store, account) = store_at_cursor(Some(100));
        ingest_into(
            &store,
            account,
            &fixture(
                account,
                "throttled-for-good",
                "From: alice@example.com\r\nSubject: Hello\r\n\r\nA note",
                false,
            ),
            Utc::now(),
        );
        let app = Router::new().route(
            "/",
            axum::routing::post(|| async {
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(json!({"error": {"type": "rate_limit_error"}})),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm = ResolvedLlm {
            url: format!("http://{}/", listener.local_addr().unwrap()),
            api_key: "test".into(),
            provider: Stage2Provider::OpenAI,
        };
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let engine = engine(store.clone(), account, "http://127.0.0.1:1");
        assert_eq!(engine.config.triage.agent.outage_retry_secs, 300);
        let day = Utc::now().format("%Y-%m-%d").to_string();
        // Two jobs fail together in the first pass, as `concurrency` jobs do
        // when the provider goes: one rung, not two.
        let second = ingest_into(
            &store,
            account,
            &fixture(
                account,
                "throttled-too",
                "From: bob@example.com\r\nSubject: Other\r\n\r\nA note",
                false,
            ),
            Utc::now(),
        );
        assert_ne!(second, 0);
        let claim = || {
            store
                .claim_agent_job(
                    account,
                    "investigation",
                    Utc::now() + ChronoDuration::hours(2),
                    120,
                )
                .unwrap()
                .expect("deferred, never failed")
        };
        let (one, two) = (claim(), claim());
        assert_ne!(one.id, two.id);
        futures::future::join(
            engine.process_agent_job(one, &llm, &day),
            engine.process_agent_job(two, &llm, &day),
        )
        .await;
        let wait = engine.agent_retry_after.load(Ordering::Relaxed) - Utc::now().timestamp();
        // A 429 skips the immediate rung: 15 s and spread, then climbing.
        assert!((14..=30).contains(&wait), "first rung: stood off {wait}s");
        assert_eq!(engine.agent_throttle_strikes.load(Ordering::Relaxed), 1);
        for rung in [30, 60, 120, 240, 285, 285] {
            // The previous cooldown has lapsed: this probe is admitted.
            engine.agent_retry_after.store(0, Ordering::Relaxed);
            let job = claim();
            assert_eq!(job.attempts, 1, "a cooldown spends no attempt");
            engine.process_agent_job(job, &llm, &day).await;
            let wait = engine.agent_retry_after.load(Ordering::Relaxed) - Utc::now().timestamp();
            assert!(
                (rung - 1..=rung + 15).contains(&wait),
                "rung {rung}: stood off {wait}s"
            );
        }
        server.abort();
    }

    /// A run dropped before it wrote the job's state (the lane is dropped at
    /// shutdown) hands the job back at once, keeping the attempt it took and
    /// due after the ladder's wait for it, instead of holding its lease, and
    /// every job on its thread, for the eight minutes the lease now covers.
    #[tokio::test]
    async fn a_run_dropped_at_shutdown_hands_its_job_back() {
        use crate::store::agent_triage::AgentTriageStore;
        let (store, account) = store_at_cursor(Some(100));
        ingest_into(
            &store,
            account,
            &fixture(
                account,
                "interrupted",
                "From: alice@example.com\r\nSubject: Slow\r\n\r\nA note",
                false,
            ),
            Utc::now(),
        );
        let app = Router::new().route(
            "/",
            axum::routing::post(|| async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Json(json!({}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm = ResolvedLlm {
            url: format!("http://{}/", listener.local_addr().unwrap()),
            api_key: "test".into(),
            provider: Stage2Provider::OpenAI,
        };
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let engine = engine(store.clone(), account, "http://127.0.0.1:1");
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let lease = engine.config.triage.agent.run_deadline_secs() as i64 + 30;
        let job = store
            .claim_agent_job(account, "investigation", Utc::now(), lease)
            .unwrap()
            .unwrap();
        let id = job.id;
        // Nothing else can claim it while it runs...
        assert!(
            store
                .claim_agent_job(account, "investigation", Utc::now(), lease)
                .unwrap()
                .is_none()
        );
        // ...and dropping the run mid-request is the shutdown.
        let dropped = tokio::time::timeout(
            Duration::from_millis(300),
            engine.process_agent_job(job, &llm, &day),
        )
        .await;
        assert!(dropped.is_err(), "the run was still in flight");
        // Handed back at once, due after the local ladder's wait for the
        // attempt it took: not re-run on the spot, so a crash loop does not
        // re-bill a long run straight away.
        assert!(
            store
                .claim_agent_job(account, "investigation", Utc::now(), lease)
                .unwrap()
                .is_none()
        );
        let again = store
            .claim_agent_job(
                account,
                "investigation",
                Utc::now() + ChronoDuration::hours(2),
                lease,
            )
            .unwrap()
            .expect("handed back the moment the run was dropped");
        assert_eq!(again.id, id);
        assert_eq!(again.attempts, 2, "the hand-back kept the attempt it took");
        // A hand-back is not a push-back: `last_error` stays as it was, so an
        // `archive_access` check keeps its place at the head of the claim
        // order (CLAIM_JOB_SQL reads `last_error IS NULL`).
        assert_eq!(again.last_error, None);
        server.abort();
    }

    /// A job dropped on its last allowed attempt is failed, not handed
    /// back: the claim has no attempts filter and a retry's check runs only
    /// after a failed attempt, so a hand-back there would run it again.
    #[tokio::test]
    async fn a_job_dropped_on_its_last_attempt_is_failed_not_handed_back() {
        use crate::store::agent_triage::AgentTriageStore;
        let (store, account) = store_at_cursor(Some(100));
        ingest_into(
            &store,
            account,
            &fixture(
                account,
                "interrupted-again",
                "From: alice@example.com\r\nSubject: Slow\r\n\r\nA note",
                false,
            ),
            Utc::now(),
        );
        let app = Router::new().route(
            "/",
            axum::routing::post(|| async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Json(json!({}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm = ResolvedLlm {
            url: format!("http://{}/", listener.local_addr().unwrap()),
            api_key: "test".into(),
            provider: Stage2Provider::OpenAI,
        };
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let engine = engine(store.clone(), account, "http://127.0.0.1:1");
        let max_attempts = engine.config.triage.agent.max_attempts;
        assert_eq!(max_attempts, 6);
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let claim = |at: DateTime<Utc>| {
            store
                .claim_agent_job(account, "investigation", at, 120)
                .unwrap()
        };
        // Five interruptions, each keeping its attempt and handing the job
        // back on the ladder's spacing (claimed here from the future)...
        for expected in 1..max_attempts {
            let job = claim(Utc::now() + ChronoDuration::hours(2)).expect("handed back");
            assert_eq!(job.attempts, i64::from(expected));
            let dropped = tokio::time::timeout(
                Duration::from_millis(200),
                engine.process_agent_job(job, &llm, &day),
            )
            .await;
            assert!(dropped.is_err());
        }
        // ...and the sixth, on the last allowed attempt, is the end.
        let job = claim(Utc::now() + ChronoDuration::hours(2)).expect("one attempt left");
        assert_eq!(job.attempts, i64::from(max_attempts));
        let dropped = tokio::time::timeout(
            Duration::from_millis(200),
            engine.process_agent_job(job, &llm, &day),
        )
        .await;
        assert!(dropped.is_err());
        assert!(
            claim(Utc::now() + ChronoDuration::hours(2)).is_none(),
            "failed, not handed back a seventh time"
        );
        server.abort();
    }

    /// A fresh throttle hours after an old episode starts the ladder over
    /// at the provider's own number, even when the episode's ladder was
    /// never reset (its recovery probe was an access check, or the queue
    /// drained on a failure); the next probe of a continuing episode does
    /// not.
    #[tokio::test]
    async fn a_fresh_throttle_hours_after_an_episode_starts_the_ladder_over() {
        use super::triage_worker::JobStart;
        let (store, account) = store_at_cursor(Some(100));
        let engine = engine(store.clone(), account, "http://127.0.0.1:1");
        let asked = Some(Duration::from_secs(7));
        let hours_ago = Utc::now().timestamp() - 7 * 3600;
        engine.agent_throttle_strikes.store(4, Ordering::Relaxed);
        engine
            .agent_last_cooldown_end
            .store(hours_ago, Ordering::Relaxed);
        // The episode ended by a success: the cooldown stamp was cleared.
        engine.agent_retry_after.store(0, Ordering::Relaxed);
        let start = JobStart {
            generation: hours_ago,
            at: Utc::now(),
        };
        let (_, wait) = engine.open_cooldown(asked, false, false, start, true);
        assert!((7..=22).contains(&wait), "the provider's number: {wait}s");
        assert_eq!(engine.agent_throttle_strikes.load(Ordering::Relaxed), 1);
        // A continuing episode: its last cooldown lapsed moments before
        // this probe started, so the rung reached stands.
        let engine = super::tests::engine(store.clone(), account, "http://127.0.0.1:1");
        let lapsed = Utc::now().timestamp() - 2;
        engine.agent_throttle_strikes.store(4, Ordering::Relaxed);
        engine
            .agent_last_cooldown_end
            .store(lapsed, Ordering::Relaxed);
        engine.agent_retry_after.store(lapsed, Ordering::Relaxed);
        let start = JobStart {
            generation: lapsed,
            at: Utc::now(),
        };
        let (_, wait) = engine.open_cooldown(asked, false, false, start, true);
        assert!((119..=135).contains(&wait), "the fifth rung: {wait}s");
        assert_eq!(engine.agent_throttle_strikes.load(Ordering::Relaxed), 5);
        // And a probe that itself ran long before failing is still that
        // episode's probe: its start is what is measured.
        let engine = super::tests::engine(store.clone(), account, "http://127.0.0.1:1");
        engine.agent_throttle_strikes.store(4, Ordering::Relaxed);
        engine
            .agent_last_cooldown_end
            .store(lapsed, Ordering::Relaxed);
        engine.agent_retry_after.store(lapsed, Ordering::Relaxed);
        let start = JobStart {
            generation: lapsed,
            at: DateTime::from_timestamp(lapsed + 1, 0).unwrap(),
        };
        let (_, wait) = engine.open_cooldown(asked, false, false, start, true);
        assert!((119..=135).contains(&wait), "still the fifth rung: {wait}s");
    }

    /// The access lane's failures climb a ladder of their own, the
    /// investigation lane's untouched: its model is another model, so a
    /// throttle whose recovery probes are access checks is still probed as
    /// the outage it is, and a throttled fast model does not park the
    /// investigation lane's ladder at the ceiling.
    #[tokio::test]
    async fn an_access_failure_climbs_the_access_lanes_own_ladder() {
        use super::triage_worker::JobStart;
        let (store, account) = store_at_cursor(Some(100));
        let engine = engine(store.clone(), account, "http://127.0.0.1:1");
        for rung in [0, 15, 30] {
            engine.agent_retry_after.store(0, Ordering::Relaxed);
            let start = JobStart {
                generation: engine.agent_last_cooldown_end.load(Ordering::Relaxed),
                at: Utc::now(),
            };
            let (_, wait) = engine.open_cooldown(None, false, false, start, false);
            assert!(
                (rung - 1..=rung + 15).contains(&wait),
                "rung {rung}: stood off {wait}s"
            );
            assert_eq!(engine.agent_throttle_strikes.load(Ordering::Relaxed), 0);
        }
        assert_eq!(engine.agent_access_strikes.load(Ordering::Relaxed), 3);
    }

    /// A job that times out twice while the provider answered another job
    /// in between is failed: two billed attempts at the long message, not
    /// `max_attempts` (six), and the account is never paused.
    #[tokio::test]
    async fn a_job_that_times_out_twice_is_failed_not_retried_four_more_times() {
        use crate::store::agent_triage::AgentTriageStore;
        use crate::triage::decision::*;
        let (store, account) = store_at_cursor(Some(100));
        let slow = ingest_into(
            &store,
            account,
            &fixture(
                account,
                "slow-mail",
                "From: alice@example.com\r\nSubject: Slow\r\n\r\nA note",
                false,
            ),
            Utc::now(),
        );
        let quick = ingest_into(
            &store,
            account,
            &fixture(
                account,
                "quick-mail",
                "From: bob@example.com\r\nSubject: Quick\r\n\r\nA note",
                false,
            ),
            Utc::now(),
        );
        let evidence = vec![EvidenceRef {
            message_id: quick,
            location: "body".into(),
        }];
        let decision = MessageDecision {
            kinds: vec![EmailKind::Correspondence],
            summary: "A note".into(),
            reason: "Personal mail".into(),
            external_access: AccessAssessment {
                restricted: false,
                reason: "No credentials".into(),
                evidence: evidence.clone(),
            },
            attention: ThreadAttentionDecision {
                relevant_message_ids: vec![quick],
                evidence,
                ..Default::default()
            },
            ..Default::default()
        };
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = calls.clone();
        let app = Router::new().route(
            "/",
            axum::routing::post(move || {
                let counted = counted.clone();
                let decision = decision.clone();
                async move {
                    // The slow message's two attempts never answer; the quick
                    // one, between them, does.
                    if counted.fetch_add(1, Ordering::Relaxed) != 1 {
                        tokio::time::sleep(Duration::from_secs(10)).await;
                        return Json(json!({})).into_response();
                    }
                    let answer = json!({"result": {"step": "finish", "decision": decision}});
                    Json(json!({
                        "choices": [{
                            "message": {"content": answer.to_string()},
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 12, "completion_tokens": 8}
                    }))
                    .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm = ResolvedLlm {
            url: format!("http://{}/", listener.local_addr().unwrap()),
            api_key: "test".into(),
            provider: Stage2Provider::OpenAI,
        };
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut config = Config::default();
        config.triage.agent.timeout_secs = 1;
        assert_eq!(config.triage.agent.max_attempts, 6, "the default allowance");
        let engine = engine_with_config(store.clone(), account, "http://127.0.0.1:1", config);
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let claim = |at: DateTime<Utc>| {
            store
                .claim_agent_job(account, "investigation", at, 120)
                .unwrap()
        };
        let (one, two) = (claim(Utc::now()).unwrap(), claim(Utc::now()).unwrap());
        let (first, other) = if one.message_id == slow {
            (one, two)
        } else {
            (two, one)
        };
        assert_eq!(other.message_id, quick);
        engine.process_agent_job(first, &llm, &day).await;
        engine.process_agent_job(other, &llm, &day).await;
        assert_eq!(engine.agent_retry_after.load(Ordering::Relaxed), 0);
        // The retry is scheduled ahead; claiming from the future sees it.
        let again = claim(Utc::now() + ChronoDuration::hours(2)).expect("retried once");
        assert_eq!((again.message_id, again.attempts), (slow, 2));
        engine.process_agent_job(again, &llm, &day).await;
        assert_eq!(engine.agent_retry_after.load(Ordering::Relaxed), 0);
        assert!(
            claim(Utc::now() + ChronoDuration::hours(2)).is_none(),
            "failed, not retried a third time"
        );
        assert_eq!(calls.load(Ordering::Relaxed), 3);
        server.abort();
    }

    /// The headline case, alone on a quiet queue: a message whose first
    /// turn answers and whose second overruns is failed after two attempts,
    /// the answered turn being the provider's proof that the message is
    /// what is slow.
    #[tokio::test]
    async fn a_message_whose_second_turn_times_out_twice_is_failed() {
        use crate::store::agent_triage::AgentTriageStore;
        let (store, account) = store_at_cursor(Some(100));
        ingest_into(
            &store,
            account,
            &fixture(
                account,
                "two-turns",
                "From: alice@example.com\r\nSubject: Long\r\n\r\nA note",
                false,
            ),
            Utc::now(),
        );
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = calls.clone();
        let app = Router::new().route(
            "/",
            axum::routing::post(move || {
                let counted = counted.clone();
                async move {
                    // Odd calls answer with a step of no known shape, which
                    // costs a repair turn; even calls (that turn) never answer.
                    if counted.fetch_add(1, Ordering::Relaxed) % 2 == 1 {
                        tokio::time::sleep(Duration::from_secs(10)).await;
                        return Json(json!({})).into_response();
                    }
                    let answer = json!({"result": {"step": "ponder"}});
                    Json(json!({
                        "choices": [{
                            "message": {"content": answer.to_string()},
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 12, "completion_tokens": 8}
                    }))
                    .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm = ResolvedLlm {
            url: format!("http://{}/", listener.local_addr().unwrap()),
            api_key: "test".into(),
            provider: Stage2Provider::OpenAI,
        };
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut config = Config::default();
        config.triage.agent.timeout_secs = 1;
        let engine = engine_with_config(store.clone(), account, "http://127.0.0.1:1", config);
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let mut attempts = 0;
        while let Some(job) = store
            .claim_agent_job(
                account,
                "investigation",
                Utc::now() + ChronoDuration::hours(2),
                120,
            )
            .unwrap()
        {
            attempts += 1;
            assert!(attempts <= 3, "the job keeps coming back");
            engine.process_agent_job(job, &llm, &day).await;
        }
        assert_eq!(attempts, 2);
        assert_eq!(calls.load(Ordering::Relaxed), 4, "two turns per attempt");
        server.abort();
    }

    /// A provider that answers nothing is not a long message: with no
    /// success between its timeouts a job walks the ordinary attempts and
    /// stays pending, so a slow quarter hour at the provider does not leave
    /// the head of the queue failed for a person to re-triage.
    #[tokio::test]
    async fn a_slow_provider_leaves_timed_out_work_pending_not_failed() {
        use crate::store::agent_triage::AgentTriageStore;
        let (store, account) = store_at_cursor(Some(100));
        ingest_into(
            &store,
            account,
            &fixture(
                account,
                "slow-provider",
                "From: alice@example.com\r\nSubject: Slow\r\n\r\nA note",
                false,
            ),
            Utc::now(),
        );
        let app = Router::new().route(
            "/",
            axum::routing::post(|| async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Json(json!({}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm = ResolvedLlm {
            url: format!("http://{}/", listener.local_addr().unwrap()),
            api_key: "test".into(),
            provider: Stage2Provider::OpenAI,
        };
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut config = Config::default();
        config.triage.agent.timeout_secs = 1;
        config.triage.agent.max_attempts = 4;
        let engine = engine_with_config(store.clone(), account, "http://127.0.0.1:1", config);
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let mut attempts = 0;
        while let Some(job) = store
            .claim_agent_job(
                account,
                "investigation",
                Utc::now() + ChronoDuration::hours(2),
                120,
            )
            .unwrap()
        {
            attempts += 1;
            assert!(attempts <= 4, "the allowance holds");
            assert_eq!(job.attempts, attempts);
            engine.process_agent_job(job, &llm, &day).await;
            assert_eq!(engine.agent_retry_after.load(Ordering::Relaxed), 0);
        }
        assert_eq!(attempts, 4, "every attempt, not two");
        assert!(
            !engine.agent_answered_since_timeout.load(Ordering::Relaxed),
            "nothing answered"
        );
        server.abort();
    }

    /// A job whose previous attempt timed out before a restart is not
    /// failed on its first timeout after it: the new process has seen no
    /// answer, so it cannot say the message rather than the provider is
    /// what is slow.
    #[tokio::test]
    async fn a_restart_does_not_fail_a_job_on_its_first_timeout() {
        use crate::store::agent_triage::AgentTriageStore;
        let (store, account) = store_at_cursor(Some(100));
        ingest_into(
            &store,
            account,
            &fixture(
                account,
                "before-the-roll",
                "From: alice@example.com\r\nSubject: Slow\r\n\r\nA note",
                false,
            ),
            Utc::now(),
        );
        // The previous process: one attempt, timed out, retried.
        let job = store
            .claim_agent_job(account, "investigation", Utc::now(), 120)
            .unwrap()
            .unwrap();
        assert!(
            store
                .retry_agent_job(&job, Utc::now(), crate::triage::agent::AGENT_TIMEOUT)
                .unwrap()
        );
        let app = Router::new().route(
            "/",
            axum::routing::post(|| async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Json(json!({}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm = ResolvedLlm {
            url: format!("http://{}/", listener.local_addr().unwrap()),
            api_key: "test".into(),
            provider: Stage2Provider::OpenAI,
        };
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut config = Config::default();
        config.triage.agent.timeout_secs = 1;
        // The new process.
        let engine = engine_with_config(store.clone(), account, "http://127.0.0.1:1", config);
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let job = store
            .claim_agent_job(account, "investigation", Utc::now(), 120)
            .unwrap()
            .unwrap();
        assert_eq!(
            (job.attempts, job.last_error.as_deref()),
            (2, Some(crate::triage::agent::AGENT_TIMEOUT))
        );
        engine.process_agent_job(job, &llm, &day).await;
        let again = store
            .claim_agent_job(
                account,
                "investigation",
                Utc::now() + ChronoDuration::hours(2),
                120,
            )
            .unwrap()
            .expect("retried, not failed");
        assert_eq!(again.attempts, 3);
        server.abort();
    }

    /// A job in flight when another opened a cooldown is the same probe
    /// even when that cooldown has lapsed by the time its own request
    /// fails: no strike of its own, so the ladder does not skip a rung.
    #[tokio::test]
    async fn a_sibling_of_a_lapsed_cooldown_adds_no_strike() {
        use crate::store::agent_triage::AgentTriageStore;
        let (store, account) = store_at_cursor(Some(100));
        for name in ["opener", "sibling"] {
            ingest_into(
                &store,
                account,
                &fixture(
                    account,
                    name,
                    "From: alice@example.com\r\nSubject: Hello\r\n\r\nA note",
                    false,
                ),
                Utc::now(),
            );
        }
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = calls.clone();
        let app = Router::new().route(
            "/",
            axum::routing::post(move || {
                let counted = counted.clone();
                async move {
                    // The first request to arrive is throttled at once; the
                    // second is slow, and then fails too.
                    if counted.fetch_add(1, Ordering::Relaxed) > 0 {
                        tokio::time::sleep(Duration::from_millis(2000)).await;
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({"error": {"type": "overloaded"}})),
                        );
                    }
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        Json(json!({"error": {"type": "rate_limit_error"}})),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm = ResolvedLlm {
            url: format!("http://{}/", listener.local_addr().unwrap()),
            api_key: "test".into(),
            provider: Stage2Provider::OpenAI,
        };
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let engine = engine(store.clone(), account, "http://127.0.0.1:1");
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let claim = || {
            store
                .claim_agent_job(account, "investigation", Utc::now(), 120)
                .unwrap()
                .unwrap()
        };
        let (one, two) = (claim(), claim());
        let lapse = async {
            // The opener's cooldown lapses before the sibling fails (a
            // second in: well after the opener's instant 429, well before
            // the sibling's 2 s).
            tokio::time::sleep(Duration::from_millis(1000)).await;
            engine
                .agent_retry_after
                .store(Utc::now().timestamp() - 1, Ordering::Relaxed);
        };
        futures::future::join3(
            engine.process_agent_job(one, &llm, &day),
            engine.process_agent_job(two, &llm, &day),
            lapse,
        )
        .await;
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(
            engine.agent_throttle_strikes.load(Ordering::Relaxed),
            1,
            "one cooldown, one strike"
        );
        let wait = engine.agent_retry_after.load(Ordering::Relaxed) - Utc::now().timestamp();
        // The opener's rung is the first, an immediate retry; the sibling
        // waits that, not the second rung.
        assert!(
            (-2..=0).contains(&wait),
            "the sibling waits the opener's rung: {wait}s"
        );
        server.abort();
    }

    /// Between rounds the refine lane sleeps its idle backoff, or until a
    /// provider cooldown lapses when that is sooner: the recovery probe is
    /// not left to the next idle tick.
    #[tokio::test]
    async fn the_refine_lane_sleeps_until_the_recovery_probe_is_due() {
        let (store, account) = store_at_cursor(Some(100));
        let engine = engine(store.clone(), account, "http://127.0.0.1:1");
        let interval = Duration::from_secs(engine.config.triage.agent.worker_poll_secs);
        assert_eq!(engine.refine_idle_sleep(interval, 5), MAX_LANE_IDLE_BACKOFF);
        engine
            .agent_retry_after
            .store(Utc::now().timestamp() + 7, Ordering::Relaxed);
        let sleep = engine.refine_idle_sleep(interval, 5);
        assert!(
            (Duration::from_secs(7)..=Duration::from_secs(8)).contains(&sleep),
            "{sleep:?}"
        );
        assert_eq!(engine.refine_idle_sleep(interval, 0), interval.min(sleep));
        engine
            .agent_retry_after
            .store(Utc::now().timestamp() - 1, Ordering::Relaxed);
        assert_eq!(engine.refine_idle_sleep(interval, 5), MAX_LANE_IDLE_BACKOFF);
    }

    #[tokio::test]
    async fn investigation_timeouts_exhaust_the_job_without_pausing_other_mail() {
        use crate::store::agent_triage::AgentTriageStore;
        let (store, account) = store_at_cursor(Some(100));
        let id = ingest_into(
            &store,
            account,
            &fixture(
                account,
                "timeout-mail",
                "From: alice@example.com\r\nSubject: Slow\r\n\r\nA note",
                false,
            ),
            Utc::now(),
        );
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = calls.clone();
        let app = Router::new().route(
            "/",
            axum::routing::post(move || {
                let counted = counted.clone();
                async move {
                    counted.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    Json(json!({}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm = ResolvedLlm {
            url: format!("http://{}/", listener.local_addr().unwrap()),
            api_key: "test".into(),
            provider: Stage2Provider::OpenAI,
        };
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut config = Config::default();
        config.triage.agent.timeout_secs = 1;
        config.triage.agent.max_attempts = 2;
        let engine = engine_with_config(store.clone(), account, "http://127.0.0.1:1", config);
        let day = Utc::now().format("%Y-%m-%d").to_string();
        for attempt in 1..=2 {
            let job = store
                .claim_agent_job(
                    account,
                    "investigation",
                    Utc::now() + ChronoDuration::hours(1),
                    120,
                )
                .unwrap()
                .unwrap();
            assert_eq!(job.attempts, attempt);
            engine.process_agent_job(job, &llm, &day).await;
            assert_eq!(engine.agent_retry_after.load(Ordering::Relaxed), 0);
        }
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(
            store
                .stage2_budget_used(account, GLOBAL_BUDGET_KEY, &day)
                .unwrap(),
            2,
            "timed-out requests have uncertain billing, so are not refunded"
        );
        assert!(
            store
                .claim_agent_job(
                    account,
                    "investigation",
                    Utc::now() + ChronoDuration::days(2),
                    120
                )
                .unwrap()
                .is_none()
        );
        let diagnostics = store.agent_diagnostics(account, id).unwrap().to_string();
        assert!(diagnostics.contains("agent_timeout"));
        assert!(diagnostics.contains("\"state\":\"failed\""));
        server.abort();
    }

    #[test]
    fn incremental_origin_not_date_freshness_controls_reserved_triage_capacity() {
        use crate::store::agent_triage::AgentTriageStore;
        let (store, account) = store_at_cursor(Some(100));
        let engine = engine(store.clone(), account, "http://127.0.0.1:1");
        let raw = "From: alice@example.com\r\nDate: Tue, 01 Jan 2019 12:00:00 +0000\r\nSubject: Note\r\n\r\nHello";
        engine
            .ingest_one(
                &fixture(account, "historical", raw, false),
                &[],
                Utc::now(),
                IngestOrigin::Backfill,
            )
            .unwrap();
        engine
            .ingest_one(
                &fixture(account, "new-with-old-date", raw, false),
                &[],
                Utc::now(),
                IngestOrigin::Incremental,
            )
            .unwrap();
        let job = store
            .claim_agent_job(account, "investigation", Utc::now(), 120)
            .unwrap()
            .unwrap();
        assert!(
            job.foreground,
            "freshly discovered incremental mail has reserved capacity regardless of sender Date"
        );
        assert!(!job.arrival_eligible, "push freshness is a separate policy");
        store.fail_agent_job(&job, "test").unwrap();
        let historical = store
            .claim_agent_job(account, "investigation", Utc::now(), 120)
            .unwrap()
            .unwrap();
        assert!(
            !historical.foreground,
            "initial backfill must use the background sub-cap"
        );
    }

    /// The live bug: a spent background cap parked a hand-requested re-triage
    /// of one email until midnight. One message is foreground and pays from
    /// the global cap only; a window request still waits for the background.
    #[tokio::test]
    async fn single_message_retriage_runs_past_a_spent_background_budget() {
        use crate::store::agent_triage::AgentTriageStore;
        for single in [true, false] {
            let (store, account) = store_at_cursor(Some(100));
            let id = ingest_into(
                &store,
                account,
                &fixture(
                    account,
                    "manual-mail",
                    "From: alice@example.com\r\nSubject: Hello\r\n\r\nA note",
                    false,
                ),
                Utc::now(),
            );
            while let Some(job) = store
                .claim_agent_job(account, "investigation", Utc::now(), 120)
                .unwrap()
            {
                store.complete_agent_job(&job).unwrap();
            }
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let count = calls.clone();
            let app = Router::new().route(
                "/",
                axum::routing::post(move || {
                    count.fetch_add(1, Ordering::Relaxed);
                    async { StatusCode::SERVICE_UNAVAILABLE }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let llm = ResolvedLlm {
                url: format!("http://{}/", listener.local_addr().unwrap()),
                api_key: "test".into(),
                provider: Stage2Provider::OpenAI,
            };
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let engine = engine(store.clone(), account, "http://127.0.0.1:1");
            let day = Utc::now().format("%Y-%m-%d").to_string();
            let background = vec![(
                "__agent_background__".to_string(),
                engine
                    .config
                    .triage
                    .agent
                    .effective_background_daily_run_cap(),
            )];
            while store
                .reserve_agent_budget(account, &day, &background)
                .unwrap()
            {}
            store
                .retriage_reset(account, single.then_some(id), 7)
                .unwrap();
            let job = store
                .claim_agent_job(account, "investigation", Utc::now(), 120)
                .unwrap()
                .unwrap();
            assert!(job.trigger.starts_with("manual:"));
            engine.process_agent_job(job, &llm, &day).await;
            let parked = store.retriage_progress(account).unwrap().budget_parked;
            if single {
                assert_eq!(calls.load(Ordering::Relaxed), 1, "the model was asked");
                assert_eq!(parked, 0);
            } else {
                assert_eq!(calls.load(Ordering::Relaxed), 0);
                assert_eq!(parked, 1, "a window request waits for the background");
            }
            server.abort();
        }
    }

    /// `uncapped_triage` end to end through the engine: with the background
    /// cap spent, a window re-triage parks while the flag is off; ticking it
    /// releases the parked job once, and the job then reaches the model
    /// instead of parking again.
    #[tokio::test]
    async fn uncapped_triage_releases_parked_work_and_runs_it_past_a_spent_cap() {
        use crate::store::agent_triage::AgentTriageStore;
        let (store, account) = store_at_cursor(Some(100));
        ingest_into(
            &store,
            account,
            &fixture(
                account,
                "uncapped-mail",
                "From: alice@example.com\r\nSubject: Hello\r\n\r\nA note",
                false,
            ),
            Utc::now(),
        );
        while let Some(job) = store
            .claim_agent_job(account, "investigation", Utc::now(), 120)
            .unwrap()
        {
            store.complete_agent_job(&job).unwrap();
        }
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        let app = Router::new().route(
            "/",
            axum::routing::post(move || {
                count.fetch_add(1, Ordering::Relaxed);
                async { StatusCode::SERVICE_UNAVAILABLE }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm = ResolvedLlm {
            url: format!("http://{}/", listener.local_addr().unwrap()),
            api_key: "test".into(),
            provider: Stage2Provider::OpenAI,
        };
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let features = crate::features::Features::none();
        let engine =
            engine(store.clone(), account, "http://127.0.0.1:1").with_features(features.clone());
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let background = vec![(
            "__agent_background__".to_string(),
            engine
                .config
                .triage
                .agent
                .effective_background_daily_run_cap(),
        )];
        while store
            .reserve_agent_budget(account, &day, &background)
            .unwrap()
        {}
        store.retriage_reset(account, None, 7).unwrap();
        let claim = || {
            store
                .claim_agent_job(account, "investigation", Utc::now(), 120)
                .unwrap()
        };

        // Off: the window request parks until tomorrow, and the flag's
        // release does nothing.
        engine.release_if_uncapped();
        engine.process_agent_job(claim().unwrap(), &llm, &day).await;
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(store.retriage_progress(account).unwrap().budget_parked, 1);
        assert!(claim().is_none(), "parked, not due");

        // On: released at once, and run past the spent cap.
        features.replace_granted([crate::features::UNCAPPED_TRIAGE]);
        engine.release_if_uncapped();
        assert_eq!(store.retriage_progress(account).unwrap().budget_parked, 0);
        let job = claim().expect("released");
        engine.process_agent_job(job, &llm, &day).await;
        assert_eq!(calls.load(Ordering::Relaxed), 1, "the model was asked");
        assert_eq!(store.retriage_progress(account).unwrap().budget_parked, 0);
        server.abort();
    }

    #[tokio::test]
    async fn expired_provider_circuit_admits_one_probe_and_recloses() {
        let (store, account) = store_at_cursor(Some(100));
        for index in 0..4 {
            ingest_into(
                &store,
                account,
                &fixture(
                    account,
                    &format!("probe-{index}"),
                    "From: alice@example.com\r\nSubject: Hello\r\n\r\nA note",
                    false,
                ),
                Utc::now(),
            );
        }
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        let app = Router::new().route(
            "/",
            axum::routing::post(move || {
                count.fetch_add(1, Ordering::Relaxed);
                async { StatusCode::SERVICE_UNAVAILABLE }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm = ResolvedLlm {
            url: format!("http://{}/", listener.local_addr().unwrap()),
            api_key: "test".into(),
            provider: Stage2Provider::OpenAI,
        };
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut engine = engine(store, account, "http://127.0.0.1:1");
        engine.stage2_llm = Some(llm);
        engine.config.triage.agent.concurrency = 4;
        // The probe after an immediate retry: one strike already taken.
        engine.agent_throttle_strikes.store(1, Ordering::Relaxed);
        engine
            .agent_retry_after
            .store(Utc::now().timestamp() - 1, Ordering::Relaxed);
        assert!(engine.agent_triage_pass().await);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(engine.agent_retry_after.load(Ordering::Relaxed) > Utc::now().timestamp());
        assert!(!engine.agent_triage_pass().await);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "cooldown must not wake the queue"
        );
        server.abort();
    }

    #[tokio::test]
    async fn provider_outage_preserves_attempts_refunds_rejection_and_recovers() {
        use crate::store::agent_triage::AgentTriageStore;
        use crate::triage::decision::*;
        let (store, account) = store_at_cursor(Some(100));
        let id = ingest_into(
            &store,
            account,
            &fixture(
                account,
                "outage-mail",
                "From: alice@example.com\r\nSubject: Hello\r\n\r\nA note",
                false,
            ),
            Utc::now(),
        );
        let evidence = vec![EvidenceRef {
            message_id: id,
            location: "body".into(),
        }];
        let decision = MessageDecision {
            kinds: vec![EmailKind::Correspondence],
            summary: "A note".into(),
            reason: "Personal mail".into(),
            external_access: AccessAssessment {
                restricted: false,
                reason: "No credentials".into(),
                evidence: evidence.clone(),
            },
            attention: ThreadAttentionDecision {
                relevant_message_ids: vec![id],
                evidence,
                ..Default::default()
            },
            ..Default::default()
        };
        let unavailable = Arc::new(AtomicBool::new(true));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let failing = unavailable.clone();
        let count = calls.clone();
        let app = Router::new().route(
            "/",
            axum::routing::post(move || {
                let failing = failing.clone();
                let count = count.clone();
                let decision = decision.clone();
                async move {
                    count.fetch_add(1, Ordering::Relaxed);
                    if failing.load(Ordering::Relaxed) {
                        return (
                            StatusCode::FORBIDDEN,
                            Json(json!({"error": {"type": "permission_error"}})),
                        )
                            .into_response();
                    }
                    let answer = json!({"result": {"step": "finish", "decision": decision}});
                    Json(json!({
                        "choices": [{
                            "message": {"content": answer.to_string()},
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 12, "completion_tokens": 8}
                    }))
                    .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm = ResolvedLlm {
            url: format!("http://{}/", listener.local_addr().unwrap()),
            api_key: "test".into(),
            provider: Stage2Provider::OpenAI,
        };
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let engine = engine(store.clone(), account, "http://127.0.0.1:1");
        let day = Utc::now().format("%Y-%m-%d").to_string();
        // More outages than max_attempts: they never become a message failure.
        for _ in 0..8 {
            engine.agent_retry_after.store(0, Ordering::Relaxed);
            let job = store
                .claim_agent_job(
                    account,
                    "investigation",
                    Utc::now() + ChronoDuration::hours(1),
                    120,
                )
                .unwrap()
                .unwrap();
            assert_eq!(job.attempts, 1);
            engine.process_agent_job(job, &llm, &day).await;
            assert_eq!(
                store
                    .stage2_budget_used(account, GLOBAL_BUDGET_KEY, &day)
                    .unwrap(),
                0
            );
        }
        assert_eq!(
            calls.load(Ordering::Relaxed),
            8,
            "config rejection is not a model repair turn"
        );
        unavailable.store(false, Ordering::Relaxed);
        engine.agent_retry_after.store(0, Ordering::Relaxed);
        // As if the probes above had been throttles: the ladder is reset by
        // the success below.
        engine.agent_throttle_strikes.store(3, Ordering::Relaxed);
        let job = store
            .claim_agent_job(
                account,
                "investigation",
                Utc::now() + ChronoDuration::hours(1),
                120,
            )
            .unwrap()
            .unwrap();
        engine.process_agent_job(job, &llm, &day).await;
        assert!(store.agent_access_allowed(account, id).unwrap());
        assert_eq!(engine.agent_throttle_strikes.load(Ordering::Relaxed), 0);
        assert_eq!(
            store
                .stage2_budget_used(account, GLOBAL_BUDGET_KEY, &day)
                .unwrap(),
            1
        );
        let diagnostics = store.agent_diagnostics(account, id).unwrap().to_string();
        assert!(!diagnostics.contains("\"state\":\"failed\""));
        server.abort();
    }

    /// The vector drain runs only while something may be missing. A fresh
    /// engine owes one (rows from before this process had an embedder, and
    /// an earlier run's failures); a clean drain lowers the flag; every arm of
    /// `embed_and_store` that wrote no vector raises it, including "no
    /// embedder at all", which a drain must then leave standing rather than
    /// clear, so the round after an embedder is attached pays the debt.
    #[tokio::test]
    async fn vector_drain_runs_only_while_something_may_be_missing() {
        use crate::embed::StubEmbedder;
        use crate::store::sqlite::VEC_DIMS;

        let (store, acct) = store_at_cursor(None);
        let base = "http://127.0.0.1:9";

        let eng =
            engine(store.clone(), acct, base).with_embedder(Arc::new(StubEmbedder::new(VEC_DIMS)));
        assert!(
            eng.vectors_dirty.load(Ordering::Relaxed),
            "a fresh engine owes the first drain"
        );
        eng.backfill_missing_vectors().await;
        assert!(
            !eng.vectors_dirty.load(Ordering::Relaxed),
            "a clean drain lowers the flag"
        );
        eng.embed_and_store(1, "hello there".to_string()).await;
        assert!(
            !eng.vectors_dirty.load(Ordering::Relaxed),
            "a successful ingest-time embed leaves it down"
        );

        // dims=0 is the stub's failure mode: `embed` returns Err.
        let failing =
            engine(store.clone(), acct, base).with_embedder(Arc::new(StubEmbedder::new(0)));
        failing.vectors_dirty.store(false, Ordering::Relaxed);
        failing.embed_and_store(2, "hello there".to_string()).await;
        assert!(
            failing.vectors_dirty.load(Ordering::Relaxed),
            "a failed embed raises it"
        );

        // No embedder anywhere: the store has none and the engine overrides none.
        let bare = engine(store, acct, base);
        bare.vectors_dirty.store(false, Ordering::Relaxed);
        bare.embed_and_store(3, "hello there".to_string()).await;
        assert!(
            bare.vectors_dirty.load(Ordering::Relaxed),
            "an embed skipped for want of an embedder raises it"
        );
        bare.backfill_missing_vectors().await;
        assert!(
            bare.vectors_dirty.load(Ordering::Relaxed),
            "a drain with no embedder leaves the debt standing"
        );

        // The flag is the store's, not one engine's: a second engine built
        // over the first one's handle (the daemon's sweeps engine) raises the
        // flag the first one's refine lane reads.
        let first = engine(store_at_cursor(None).0, acct, base)
            .with_embedder(Arc::new(StubEmbedder::new(VEC_DIMS)));
        first.backfill_missing_vectors().await;
        assert!(!first.vectors_dirty.load(Ordering::Relaxed));
        let second = engine(store_at_cursor(None).0, acct, base)
            .with_vectors_dirty(first.vectors_dirty_handle())
            .with_embedder(Arc::new(StubEmbedder::new(0)));
        second.embed_and_store(4, "hello there".to_string()).await;
        assert!(
            first.vectors_dirty.load(Ordering::Relaxed),
            "a raise on the sweeps engine lands on the sync engine's flag"
        );
    }

    /// An embedder that remembers every text it was handed: what the shared
    /// embedding service would have read.
    struct Recording(std::sync::Mutex<Vec<String>>);

    impl crate::embed::Embedder for Recording {
        fn embed(&self, text: &str) -> Result<Vec<f32>> {
            self.0.lock().unwrap().push(text.to_string());
            Ok(vec![0.25; crate::store::sqlite::VEC_DIMS])
        }
        fn dims(&self) -> usize {
            crate::store::sqlite::VEC_DIMS
        }
    }

    /// Commit a triage decision for `id`, `adjust`ed, the way the worker does.
    fn decide_now(
        store: &SqliteStore,
        acct: AccountId,
        id: i64,
        adjust: impl FnOnce(&mut crate::triage::decision::MessageDecision),
    ) {
        use crate::store::agent_triage::{AgentCommitOutcome, AgentTriageStore};
        let job = store
            .claim_agent_job(acct, "triage", Utc::now() + ChronoDuration::hours(1), 60)
            .unwrap()
            .expect("a triage job waits");
        assert_eq!(job.message_id, id);
        let context = store.load_agent_context(&job).unwrap();
        let mut decision = crate::triage::decision::MessageDecision {
            kinds: vec![crate::triage::decision::EmailKind::General],
            summary: "Mail".into(),
            reason: "Mail".into(),
            ..Default::default()
        };
        adjust(&mut decision);
        assert_eq!(
            store
                .commit_agent_decision(
                    &job,
                    &context,
                    &decision,
                    std::slice::from_ref(&context.message.source),
                )
                .unwrap(),
            AgentCommitOutcome::Applied
        );
    }

    /// THE SEALED GATE, END TO END (docs/SECURITY.md §4d): a login code is
    /// handed to no embedder, not at ingest (it is undecided), not by the
    /// drain before its decision, and not after the decision says auth, while
    /// the owner's own mail is embedded once its access check settles and
    /// ordinary inbound mail once it is decided.
    #[tokio::test]
    async fn no_embedder_ever_reads_a_login_code() {
        let (store, acct) = store_at_cursor(None);
        let recording = Arc::new(Recording(std::sync::Mutex::new(Vec::new())));
        let eng =
            engine(store.clone(), acct, "http://127.0.0.1:9").with_embedder(recording.clone());
        let now = Utc::now();
        let land = |msgid: &str, eml: &str, sent: bool| {
            let mut raw = fixture(acct, msgid, eml, sent);
            raw.gmail_thread_id = Some(format!("thread-{msgid}"));
            eng.ingest_one(&raw, &[], now, IngestOrigin::Incremental)
                .unwrap()
                .expect("a new row")
        };

        let otp = land(
            "otp",
            "From: noreply@bank.example\r\nSubject: Your sign-in code\r\n\r\nYour code is 482913",
            false,
        );
        assert!(otp.embed_text.is_none(), "undecided at ingest: no text out");
        let sent = land(
            "sent",
            "From: me@example.com\r\nTo: dan@example.com\r\nSubject: The lease\r\n\r\nSigned lease attached",
            true,
        );
        // Own mail is never triaged; until its access check answers it could
        // be a forwarded code, so it waits for that, not for a decision.
        assert!(
            sent.embed_text.is_none(),
            "own mail waits for its access check"
        );
        eng.backfill_missing_vectors().await;
        assert_eq!(vec_count(&store, sent.id), 0, "access check pending");
        {
            use crate::store::agent_triage::AgentTriageStore;
            let job = store
                .claim_agent_job(acct, "access", Utc::now() + ChronoDuration::hours(1), 60)
                .unwrap()
                .expect("own mail is assessed for access");
            assert_eq!(job.message_id, sent.id);
            let message = store.load_agent_access_message(&job).unwrap();
            store
                .commit_agent_access(
                    &job,
                    &message,
                    &crate::triage::access::AccessDecision {
                        restricted: false,
                        reason: "No credentials".into(),
                    },
                    &serde_json::json!({}),
                )
                .unwrap();
        }
        eng.backfill_missing_vectors().await;
        assert_eq!(vec_count(&store, sent.id), 1, "settled, then embedded");

        // The drain, before any decision: the login code is not selected.
        eng.backfill_missing_vectors().await;
        assert_eq!(vec_count(&store, otp.id), 0);
        // The decision says auth; the drain still does not select it.
        decide_now(&store, acct, otp.id, |d| {
            d.auth.kinds = vec![crate::triage::decision::AuthKind::Otp]
        });
        eng.backfill_missing_vectors().await;
        assert_eq!(vec_count(&store, otp.id), 0);

        // Ordinary inbound mail: nothing at ingest, a vector once decided.
        let lunch = land(
            "lunch",
            "From: alice@example.com\r\nSubject: Lunch\r\n\r\nLunch on Thursday?",
            false,
        );
        assert!(lunch.embed_text.is_none());
        eng.backfill_missing_vectors().await;
        assert_eq!(vec_count(&store, lunch.id), 0, "undecided");
        decide_now(&store, acct, lunch.id, |_| {});
        eng.backfill_missing_vectors().await;
        assert_eq!(vec_count(&store, lunch.id), 1, "decided, not sealed");

        let read = recording.0.lock().unwrap();
        assert!(
            !read.iter().any(|t| t.contains("482913")),
            "the login code reached an embedder: {read:?}"
        );
        assert!(read.iter().any(|t| t.contains("Signed lease")));
        assert!(read.iter().any(|t| t.contains("Lunch on Thursday")));
    }

    fn vec_count(store: &SqliteStore, id: i64) -> i64 {
        store
            .query_i64_for_test(&format!(
                "SELECT COUNT(*) FROM message_vecs WHERE message_id={id}"
            ))
            .unwrap()
    }

    /// docs/EMBED-SERVICE.md §3: a remote embedder answering 503 opens a
    /// backoff window, and inside it neither ingest nor the vector drain calls
    /// it again. Ingest still completes (the row is owed to the drain), which
    /// is the half that keeps mail sync from slowing down.
    #[tokio::test]
    async fn a_full_embedding_service_is_not_asked_again_inside_the_window() {
        use std::io::{Read, Write};
        use std::sync::atomic::AtomicUsize;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let asked = Arc::new(AtomicUsize::new(0));
        let count = asked.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                count.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    b"HTTP/1.1 503 X\r\nretry-after: 1\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                );
            }
        });
        let remote = tokio::task::spawn_blocking(move || {
            let settings = crate::config::EmbedConfig::default().settings();
            crate::embed::RemoteEmbedder::new(&url, None, &settings).unwrap()
        })
        .await
        .unwrap();

        let (store, acct) = store_at_cursor(None);
        let eng = engine(store, acct, "http://127.0.0.1:9").with_embedder(Arc::new(remote));
        eng.vectors_dirty.store(false, Ordering::Relaxed);

        eng.embed_and_store(1, "hello there".to_string()).await;
        assert_eq!(asked.load(Ordering::SeqCst), 1, "the first embed asks");
        assert!(eng.vectors_dirty.load(Ordering::Relaxed), "and is owed");
        assert!(!eng.embed_backoff.may_call(tokio::time::Instant::now()));

        eng.vectors_dirty.store(false, Ordering::Relaxed);
        eng.embed_and_store(2, "hello again".to_string()).await;
        assert!(
            eng.vectors_dirty.load(Ordering::Relaxed),
            "skipped inside the window, so still owed"
        );
        eng.backfill_missing_vectors().await;
        assert_eq!(
            asked.load(Ordering::SeqCst),
            1,
            "nothing asks again until the window closes"
        );
        assert!(
            eng.vectors_dirty.load(Ordering::Relaxed),
            "a drain that did not run leaves the debt standing"
        );
    }
}
