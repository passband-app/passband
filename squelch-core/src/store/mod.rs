//! Storage abstraction.
//!
//! rusqlite is synchronous, so `Store` is a SYNC trait and `SqliteStore` wraps
//! its `Connection` in a `Mutex`; async callers wrap calls in
//! `tokio::task::spawn_blocking`.

pub mod agent_triage;
pub mod archive_surface;
pub mod recency;
pub mod search_query;
pub mod sqlite;

pub use search_query::{
    FtsQuery, SearchDiagnostics, SearchFilter, SearchSort, TermDf, parse_search_query,
};
pub use sqlite::{
    ACCESS_BUDGET_KEY, ARCHIVE_ACCESS_BUDGET_KEY, ARCHIVE_ACCESS_OUTSTANDING_MAX,
    ARCHIVE_SURFACE_CAP, AccessProgress, ArchiveAccessCaps, BACKGROUND_BUDGET_KEY,
    EXTERNAL_RECALL_MAX, EventMatch, ExternalSearchMode, SqliteStore,
};

use crate::error::Result;
// ONE closed vocabulary for the metric label and the stored ledger string; see
// the type comments in `metrics.rs` and docs/NOTIFY.md §11.4.
use crate::metrics::{NotifyDecision, NotifyLane};
use crate::triage::extract::shipments::ShipmentsApplied;
use crate::triage::rule_suggest::SmartBlock;
use crate::triage::{CalendarInfo, CarrierTrack, DeadlineHit, ReceiptInfo, ShipmentInfo};
use crate::types::{
    AccountId, AttachmentInfo, AttentionStatus, AuditEntry, Banking, CalendarUpdate, Deadline,
    Disposition, Event, EventKind, FieldReasons, NewMessage, OpenRate, Receipt, RetriageProgress,
    SealedKind, SearchHit, SenderRule, Sensitivity, ShredCandidate, StoreStats, ThreadView, Tier,
    TriageAxis, TriageFeedback, UnsubscribeRecord,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Server-side bucket for the sitrep chassis, selected by the `band` param on
/// `/client/updates`.
///
/// - `Standing` — mail owed the user's attention, immune to the surfacing clock
///   and never rotating out until resolved: a dated obligation (tier
///   `past_due`/`deadline`) OR live correspondence — a thread the user has
///   written in, or a sender the user has written to — with status != 'done'.
///   Participation only widens membership: it never unseals anything, and it
///   never surfaces the user's own sent rows, which no band lists.
/// - `New` — `surfaced_at IS NULL`: never surfaced through ANY door.
/// - `Open` — status = 'open', ordered by `age * importance` descending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SitrepBand {
    Standing,
    New,
    Open,
}

/// `app_settings` key holding when the on-demand spam sync last COMPLETED
/// (RFC3339 UTC), absent until one has.
///
/// It exists so the spam page can tell "we looked and the folder is empty" apart
/// from "nobody has looked yet", which are the same empty list on the wire and
/// opposite facts to a reader. The poll loop never walks the spam label at all,
/// so without this stamp a page that has not synced yet would tell somebody
/// their provider filtered nothing — a confident answer nobody has earned.
///
/// `pub` because the sync engine writes it and the store reads it into
/// [`crate::types::StoreStats`], and a key spelled in two places is a key that
/// eventually disagrees with itself.
pub const SPAM_SYNCED_AT_KEY: &str = "spam_synced_at";

/// WHICH SIDE of the provider's spam verdict a listing wants. A two-variant
/// enum rather than a `bool` because the boolean has no honest name: `spam:
/// true` reads as "include spam" at half the call sites and "only spam" at the
/// other half, and the two differ by the entire inbox.
///
/// There is no `Both`. Spam and ordinary mail interleaved in one list is the
/// state this feature exists to end, and offering it would make the default
/// listing one mistaken argument away from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpamScope {
    /// Everything the provider did NOT file as spam. The default, and what every
    /// band, queue, count and search asks for.
    #[default]
    Exclude,
    /// ONLY what the provider filed as spam — the human door's spam page, and
    /// the only caller in the codebase that passes it.
    Only,
}

impl SpamScope {
    /// The SQL predicate over `messages` (aliased `m`) this scope stands for.
    pub fn predicate(self) -> &'static str {
        match self {
            SpamScope::Exclude => "m.is_spam = 0",
            SpamScope::Only => "m.is_spam = 1",
        }
    }

    /// One accepted string, and `None` for anything else so the caller can 400
    /// rather than silently serve the wrong side of the verdict.
    pub fn parse(s: &str) -> Option<SpamScope> {
        match s {
            "only" => Some(SpamScope::Only),
            _ => None,
        }
    }
}

/// One attachment's `(filename, mime, data)`; `data` is `None` when the bytes
/// were not stored (the part was over the ingest cap).
pub type AttachmentBytes = (String, String, Option<Vec<u8>>);

impl SitrepBand {
    pub fn parse(s: &str) -> Option<SitrepBand> {
        match s {
            "standing" => Some(SitrepBand::Standing),
            "new" => Some(SitrepBand::New),
            "open" => Some(SitrepBand::Open),
            _ => None,
        }
    }
}

/// The Gmail sync cursor for one (account, mailbox key), persisted in
/// `sync_state`. The only row is keyed `mailbox = 'history'`: `uidvalidity` is
/// unused (0) and `last_uid` holds the account's monotonic `historyId`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncState {
    pub uidvalidity: u32,
    /// The account's `historyId` cursor.
    pub last_uid: u64,
}

/// Gmail's own unread counts for INBOX, as the sync loop last saw them.
///
/// This is the mailbox's truth, not ours: nothing local tracks reads (the read
/// scope cannot write them), and the ingest window covers only a slice of the
/// inbox, so these numbers can only come from Gmail. `fetched_at` says how stale
/// they are — a fetch failure keeps the previous row rather than clearing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InboxUnread {
    pub messages: i64,
    pub threads: i64,
    pub fetched_at: DateTime<Utc>,
}

/// Who the mailbox belongs to, as Google's People API last described them: the
/// part of [`account_profile`](Store::account_identity) small enough to ride on
/// every stats answer. The photo's bytes are fetched separately, by tag.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccountIdentity {
    /// The full display name ("Ada Lovelace").
    pub name: Option<String>,
    /// The given name alone, which is what a greeting wants.
    pub given_name: Option<String>,
    /// Present exactly when a photo is stored. A short hash of its bytes, so it
    /// changes when the picture does and not otherwise.
    pub photo_tag: Option<String>,
}

/// The account's profile photo, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountPhoto {
    pub bytes: Vec<u8>,
    /// Sniffed from the bytes at fetch time, never taken from a header.
    pub mime: String,
    pub tag: String,
}

/// One Sent-derived contact, as both the autocomplete hit shape and the
/// Sent-history harvest's merge input (same fields either way).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactEntry {
    pub addr: String,
    /// Mailbox display name from Sent To/Cc headers, when one was ever seen.
    pub display_name: Option<String>,
    pub sent_count: i64,
    pub last_sent_at: Option<DateTime<Utc>>,
}

/// How long a typed name is as [`Store::people_named`] will read it: the
/// letters and digits of its FIRST word, the punctuation around the word
/// dropped. `"s."`, `"(s)"`, `"s-"` and `". s"` are all one letter, whatever
/// their length in characters, and one letter matches a large part of
/// anybody's contacts: the calendar people route refuses a name shorter than
/// two by THIS count, not by counting characters (verify8 V8-13).
pub fn typed_name_len(name: &str) -> usize {
    name.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .find(|w| !w.is_empty())
        .map_or(0, |w| w.chars().filter(|c| c.is_alphanumeric()).count())
}

/// One person the user has written to whose name matches a typed given name:
/// the calendar event pane's guest SUGGESTION (`/client/calendar/people`).
/// HUMAN-DOOR ONLY, like [`ContactEntry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonNamed {
    pub addr: String,
    /// The name the match was made on: the one the user's own Sent mail used,
    /// else the newest one on that address's readable, authenticated mail.
    /// `None` when the match was made on the address itself.
    pub display_name: Option<String>,
}

/// One row of the sender directory (`senders`): an address that has written to
/// the account, as the search field's `from:` autocomplete hit shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderEntry {
    pub addr: String,
    /// The newest display name seen on this sender's mail, when any was.
    pub display_name: Option<String>,
    /// Inbound, non-spam messages from this address. Sealed ones are counted
    /// (sealing is decided after the count is taken); a sender with ONLY sealed
    /// mail is never returned at all.
    pub msg_count: i64,
    pub last_received_at: DateTime<Utc>,
}

/// The sender directory rolled up by host (`/client/sender-domains`): everyone
/// at one domain who has written to the account, as the rule editor's `*@`
/// suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderDomainEntry {
    /// The host exactly as the addresses carry it (`e.stripe.com` is not
    /// `stripe.com`), because a rule's glob matches the whole address and
    /// `*@stripe.com` would not cover the subdomain.
    pub domain: String,
    /// Directory rows at this host.
    pub sender_count: i64,
    /// The sum of those rows' `msg_count`, with the same sealed caveat.
    pub msg_count: i64,
    pub last_received_at: DateTime<Utc>,
}

/// A fully-triaged message committed in one transaction by
/// [`Store::ingest_message`]: message row and triage row together, so a sealed
/// message is never observable as normal mail (docs/SECURITY.md §4).
#[derive(Debug, Clone)]
pub struct TriagedMessage {
    /// First classification from incremental sync may use reserved arrival
    /// capacity. Historical backfill and repairs use background capacity.
    pub foreground_triage: bool,
    pub message: NewMessage,
    /// Sent mail only: To/Cc addresses seeding the contacts table (the account's
    /// own address is filtered out at ingest). Contacts come exclusively from
    /// recipients of sent mail, never from inbound senders.
    pub recipients: Vec<String>,
    /// Sent mail only: the FAITHFUL To/Cc address set, lowercased and deduped,
    /// which becomes the `message_recipients` index.
    ///
    /// Deliberately not `recipients`, which is filtered for contact seeding (the
    /// account's own address and robot addresses dropped). This one answers "who
    /// did this go to" — the question `messages.to_addrs` answers in display
    /// form — so send-group history can join against it and still see a mail
    /// that went to `support@` or to the user themselves. Empty on received mail.
    pub recipient_addrs: Vec<String>,
    pub sensitivity: Sensitivity,
    pub sealed_kind: Option<SealedKind>,
    pub importance: u8,
    pub tier: Tier,
    pub one_line: String,
    pub reason: String,
    /// Per-property Stage-1 justifications (importance / deadline / tier), stored
    /// as the triage row's `field_reasons` JSON and served HUMAN-DOOR ONLY. Empty
    /// for sealed / sent mail.
    pub field_reasons: FieldReasons,
    pub matched_rule: Option<i64>,
    /// The Stage-1 deadline hit. Only ever `Some` for non-sealed mail.
    pub deadline: Option<DeadlineHit>,
    /// A detected shipment/package. Runs independently of the triage tier, and
    /// only ever `Some` for non-sealed mail.
    pub shipment: Option<ShipmentInfo>,
    /// The LOOSE shipping signal
    /// ([`has_loose_shipping_signal`](crate::triage::shipment::has_loose_shipping_signal)):
    /// `true` queues the row for the shipments EXTRACTOR by stamping
    /// `triage.ship_extract_model='pending'`. Much wider than `shipment`, which
    /// needs a tracking number the regex could attribute — an order confirmation
    /// with neither sets this and leaves `shipment` `None`. Always `false` for
    /// sealed and sent mail, exactly like `shipment`.
    pub ship_extract: bool,
    /// A detected receipt (money already paid). Independent of the tier AND of
    /// shipment detection — one mail can be both. Only ever `Some` for non-sealed
    /// mail. Ingest AUTO-RESOLVES the triage row (`status='done'`) so a receipt
    /// lives only in the Receipts category instead of the attention bands.
    pub receipt: Option<ReceiptInfo>,
    /// A detected calendar update (invite / update / cancellation / RSVP). Same
    /// shape as receipts: tier-independent, non-sealed only, and ingest
    /// AUTO-RESOLVES the triage row so it lives only in the Calendar category.
    pub calendar: Option<CalendarInfo>,
    /// Attachments extracted from the RFC822 (real parts AND cid-inline), each
    /// already capped (over-cap parts carry `data: None`), written in the SAME
    /// ingest transaction. Present for sealed mail too — the byte-serving
    /// endpoint is what guards sealed parents.
    pub attachments: Vec<AttachmentInfo>,
    /// `false` when Stage-1 was not confident: the row keeps `model_used IS NULL`
    /// so the Stage-2 queue predicate picks it up.
    pub confident: bool,
    /// MAY THIS MESSAGE EVER NOTIFY, and from when. `Some(now)` for mail we are
    /// seeing for the first time on an incremental path that was still fresh
    /// when we saw it; `None` — the silent direction — for a backfill row, the
    /// user's own sent copy, and mail already stale at first sight.
    ///
    /// Set by [`crate::sync::SyncEngine`], NOT by ingest: the pure ingest
    /// pipeline does not know which sync path it is on, and that fact is the
    /// whole decision (docs/NOTIFY.md §11.3). `ingest_with_rules` therefore
    /// leaves it `None` and the engine fills it in before the store call.
    ///
    /// Written by [`Store::ingest_message`] on the triage row's FIRST INSERT
    /// ONLY and preserved verbatim on conflict, which is what makes a catch-up
    /// re-scan structurally unable to storm: a row it re-ingests already exists
    /// and keeps whatever it was stamped with, NULL included.
    pub notify_eligible_at: Option<DateTime<Utc>>,
    /// The `Date` header's UTC offset in minutes, east positive, written to
    /// `messages.date_offset_minutes`. `None` when no Date header parsed.
    /// Header metadata, not content, so it is captured for every message;
    /// what counts as an observation is the timezone inference's call.
    pub date_offset_minutes: Option<i32>,
    /// The first calendar invitation in the message that named a real zone,
    /// credited to its ORGANIZER (not the From). See [`crate::timezone`].
    pub invite_zone: Option<crate::timezone::InviteZone>,
    /// The language ingest detected, when it is NOT English
    /// ([`crate::translate::detect`]). `Some` queues a `message_translations`
    /// row in the ingest transaction: `pending` for live mail, `deferred` for
    /// the archive tier. Ingest leaves it `None` for spam and sent mail, and
    /// the store refuses both again, plus the account's own and sealed mail.
    pub translate_lang: Option<crate::translate::Lang>,
    /// The translator has no budget left today (`translate.daily_budget_usd`
    /// spent). Then live mail is queued without holding its triage, and a
    /// backfilled message is queued `deferred` (translated when the reader
    /// opens it) rather than `pending` for days.
    pub translate_over_budget: bool,
}

/// Where a message's translation stands (`message_translations.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslationStatus {
    /// Archive tier: nothing is spent until the reader displaying the thread
    /// asks (`POST /client/thread/{id}/translate`).
    Deferred,
    /// Waiting for (or claimed by) the translate pass.
    Pending,
    Done,
    /// Translated, but the fact check still found source facts missing.
    Incomplete,
    /// Nothing usable came back; retried with backoff, a bounded number of
    /// times.
    Failed,
    /// Terminal: the translator read the message as English. Served nowhere.
    NotNeeded,
    /// Terminal: the engine refused its own output (it could not verify the
    /// translation carries the original). Served nowhere, never retried.
    Rejected,
}

impl TranslationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deferred => "deferred",
            Self::Pending => "pending",
            Self::Done => "done",
            Self::Incomplete => "incomplete",
            Self::Failed => "failed",
            Self::NotNeeded => "not_needed",
            Self::Rejected => "rejected",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "deferred" => Self::Deferred,
            "pending" => Self::Pending,
            "done" => Self::Done,
            "incomplete" => Self::Incomplete,
            "failed" => Self::Failed,
            "not_needed" => Self::NotNeeded,
            "rejected" => Self::Rejected,
            _ => return None,
        })
    }
}

/// One claimed translation: the message's ORIGINAL fields, as stored.
#[derive(Debug, Clone)]
pub struct TranslationJob {
    pub message_id: i64,
    pub account_id: AccountId,
    /// ISO 639-1, as detection wrote it.
    pub source_lang: String,
    pub subject: String,
    pub body: String,
    pub body_html: Option<String>,
    /// Attempts including this one.
    pub attempts: i64,
    /// THE CLAIM TOKEN. Every write of this job's result matches on it, so a
    /// result for a row that a heal dropped and re-queued, or that another
    /// pass took after the lease expired, writes nothing.
    pub lease: String,
    /// The status and `updated_at` the row had before this claim, which a
    /// [`TranslationWrite::Released`] puts back, so a released `failed` row
    /// keeps its backoff.
    pub prev_status: TranslationStatus,
    pub prev_updated_at: i64,
    /// Consecutive releases before this claim. At
    /// `MAX_CONSECUTIVE_RELEASES` a [`TranslationWrite::Released`] is charged
    /// to the row instead, so the pass knows not to raise the alarm again.
    pub releases: i64,
}

/// What the translate pass writes back for a claimed row.
#[derive(Debug, Clone, Copy)]
pub enum TranslationWrite<'a> {
    /// `done`, or `incomplete` when `missing_facts` is not empty or only the
    /// head of the message was translated (`truncated`).
    Translated {
        translated: &'a crate::translate::Translated,
        model: &'a str,
        prompt_version: i64,
    },
    /// `failed`: retried after a backoff until the attempts run out.
    Failed { model: &'a str },
    /// `not_needed`: the translator read the message as English. Terminal.
    NotNeeded { model: &'a str, prompt_version: i64 },
    /// `rejected`: the engine refused its own output. Terminal, never served.
    Rejected { model: &'a str, prompt_version: i64 },
    /// Hand the claim back WITHOUT spending its attempt: the call never
    /// reached a verdict about this message (a rejected key, an exhausted
    /// budget, a model the gateway will not serve, a provider outage or
    /// timeout), so the row is exactly as translatable as it was, and goes
    /// back to the status and backoff it had before the claim. The pass that
    /// releases it stops and cools down. After `MAX_CONSECUTIVE_RELEASES`
    /// in a row it is charged instead (`failed`, the attempt spent), but
    /// only when `last_answer_ms` (when the gateway last answered any
    /// translation call for the account, unix ms) is later than the row's
    /// first release: a failure only this message keeps drawing while
    /// others get answers is about the message. `None` (a capacity failure,
    /// 429/5xx, or no answer seen) never charges, so a total outage keeps
    /// every row pending with its attempts untouched.
    Released { last_answer_ms: Option<i64> },
    /// Hand the claim back untouched because the pass never tried it (no
    /// room left in today's budget). Not counted as a release.
    Returned,
}

/// A stored translation row.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageTranslation {
    pub message_id: i64,
    pub source_lang: String,
    pub status: TranslationStatus,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub body_html: Option<String>,
    pub snippet: Option<String>,
    pub missing_facts: Vec<String>,
    pub model: Option<String>,
    pub prompt_version: Option<i64>,
    pub attempts: i64,
}

/// The full body of one sealed message. HUMAN-DOOR-ONLY: returned solely by
/// [`Store::sealed_body`] from the squelch-api reveal endpoint, which audits
/// before this value leaves the process (docs/SECURITY.md §4).
#[derive(Debug, Clone)]
pub struct SealedBody {
    pub id: i64,
    pub account_id: AccountId,
    pub thread_id: String,
    pub from_addr: String,
    pub from_name: Option<String>,
    pub subject: String,
    pub received_at: DateTime<Utc>,
    pub sealed_kind: Option<String>,
    pub body: String,
    /// The sanitized HTML body stored at ingest, when the mail had one; served
    /// through this one audited reveal door.
    pub body_html: Option<String>,
}

/// The Gmail ids + header fields an action endpoint needs to act on a message,
/// and no body. HUMAN-DOOR-ONLY: produced solely by
/// [`SqliteStore::action_message_ref`](sqlite::SqliteStore::action_message_ref),
/// which does NOT filter sealed or restricted rows (see its doc for where the
/// human-door and agent-door gates actually live).
#[derive(Debug, Clone)]
pub struct ActionMessageRef {
    /// Local message id.
    pub id: i64,
    pub account_id: AccountId,
    /// The Gmail-side message id (`users.messages.{id}`), used for modify/get.
    pub gmail_msg_id: String,
    /// The Gmail-side thread id, used as `threadId` when sending a reply.
    pub thread_id: String,
    /// Original sender — the default reply recipient.
    pub from_addr: String,
    pub from_name: Option<String>,
    pub subject: String,
}

/// The full triage state of one message, for the developer-mode inspector.
/// SERIALIZED SHAPE IS A WIRE CONTRACT — the client decodes this. Carries
/// verdicts/markers/reasons but never body content; human door only, and sealed
/// rows are never returned.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TriageDebug {
    pub message_id: i64,
    pub subject: String,
    pub importance: i64,
    pub tier: String,
    pub category: Option<String>,
    pub one_line: String,
    pub reason: String,
    pub field_reasons: Option<crate::types::FieldReasons>,
    pub deadline: Option<String>,
    pub matched_rule_id: Option<i64>,
    pub status: String,
    pub surfaced_at: Option<String>,
    pub resolved_at: Option<String>,
    pub stage1_model_used: Option<String>,
    pub model_used: Option<String>,
    pub needs_stage2: bool,
    pub extractor_model_used: Option<String>,
    pub created_at: String,
    /// The Gmail-side thread id, joined from `messages`. APPENDED LAST on
    /// purpose: an older client decoding this shape must keep working.
    pub thread_id: String,
    /// When a human last asked for this row to be re-triaged, `None` when
    /// nobody ever has. APPENDED LAST for the reason `thread_id` was.
    pub retriage_at: Option<String>,
}

/// The stored unsubscribe intent for one NON-SEALED message, resolved by
/// [`Store::message_unsub_fields`] for `POST /client/unsubscribe`. Sealed rows
/// are excluded in SQL, so missing and sealed are indistinguishable (`None`).
#[derive(Debug, Clone)]
pub struct MessageUnsub {
    /// The sender address as stored (the caller lowercases it for the wire).
    pub from_addr: String,
    /// Raw `List-Unsubscribe` header value, or `None`.
    pub list_unsubscribe: Option<String>,
    /// RFC 8058 one-click advertised.
    pub list_unsub_one_click: bool,
    /// The SANITIZED body, for the footer-link fallback when no header exists.
    /// Sanitized rather than raw on purpose: it is the markup the reader was
    /// actually shown, and scripts and handlers are already gone from it.
    pub body_html: Option<String>,
    /// The email-authentication verdict (`messages.auth_pass`): `None` when it
    /// was never recorded. The automatic path runs only on `Some(true)`.
    pub auth_pass: Option<bool>,
    /// Gmail filed it as spam. Never unsubscribed from automatically: fetching
    /// a spammer's link tells them the address is live.
    pub is_spam: bool,
    /// The stored `To:` line, for telling which of the user's addresses this
    /// mail was sent to (a plus address, usually) when a form asks.
    pub to_addrs: Option<String>,
}

/// A row to append to the human-door audit log.
#[derive(Debug, Clone)]
pub struct NewAuditEntry {
    pub actor: String,
    pub action: String,
    pub target: Option<String>,
    pub detail: Option<String>,
}

/// A notification-worthy event appended to the `events` log. Produced ONLY by
/// [`crate::triage::events`]; every field besides the ids is a denormalized
/// snapshot of the verdict at emission time.
///
/// The model supplies safe notification text. `is_auth` controls client
/// presentation independently from external-agent access restrictions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewEvent {
    pub account_id: AccountId,
    pub message_id: i64,
    pub thread_id: String,
    pub kind: EventKind,
    /// Model assessment, carried explicitly rather than inferred from urgency.
    #[serde(default)]
    pub is_auth: bool,
    pub tier: Tier,
    pub importance: u8,
    pub sender: String,
    pub one_line: String,
    /// RFC3339 deadline snapshot, or `None`.
    pub deadline: Option<String>,
    /// Which auth shape this event is about; `None` for every ordinary event.
    /// See [`crate::types::Event::sealed_kind`] — routing metadata, never a gate.
    pub sealed_kind: Option<SealedKind>,
}

/// One row for the NOTIFY DECISION LEDGER (docs/NOTIFY.md §11.4): what one lane
/// decided about notifying for one message, and nothing else.
///
/// CARRIES NO EMAIL-DERIVED TEXT — no one_line, no subject, no sender. Every
/// field is an id, a closed-vocabulary string, a score or a duration, which is
/// what lets a SEALED message be recorded here at all.
///
/// Written with INSERT OR IGNORE and never updated: see
/// [`Store::record_notify_decision`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewNotifyDecision {
    pub account_id: AccountId,
    pub message_id: i64,
    /// Which lane decided. The stored string is [`NotifyLane::as_str`].
    pub lane: NotifyLane,
    /// What it decided. The stored string is [`NotifyDecision::as_str`].
    pub decision: NotifyDecision,
    /// The notify model's 0-100 score, `None` when no model scored this row —
    /// a suppressed, expired or unavailable decision has no score, and a 0
    /// stood in for one would read as "the model said no".
    pub notify_importance: Option<u8>,
    /// Model id, `'sealed'`, `'heuristic'`, or `None`.
    pub model_used: Option<String>,
    /// First sight to this decision, in milliseconds. Fast lane only.
    pub latency_ms: Option<u32>,
}

/// One stored ledger row, as the eval read hands it back. `created_at` and `id`
/// are the store's, everything else is what the lane wrote.
///
/// The lane/decision strings are parsed back into the closed enums; a row whose
/// vocabulary this build does not know (written by a newer daemon) is SKIPPED by
/// the read rather than coerced into a neighbouring variant, because the whole
/// point of the ledger is that each row means exactly one thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyDecisionRow {
    pub id: i64,
    pub message_id: i64,
    pub lane: NotifyLane,
    pub decision: NotifyDecision,
    pub notify_importance: Option<u8>,
    pub model_used: Option<String>,
    pub latency_ms: Option<u32>,
    pub created_at: DateTime<Utc>,
}

/// Model reasoning for human diagnostics. Never exposed through the external agent door.
/// Contains bounded, nonsecret summaries, not raw message text or credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationAssessment {
    pub is_auth: bool,
    pub importance: u8,
    pub one_line: String,
    pub reason: String,
    pub model: String,
    pub prompt_version: String,
    pub assessed_at: DateTime<Utc>,
}

/// One registered APNs device.
///
/// PRIVACY: `token` is capability material — written by the human door, read by
/// the pusher, NEVER logged and never exposed on the agent door.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: i64,
    pub account_id: AccountId,
    pub token: String,
    /// Free-form platform tag; `ios` today.
    pub platform: String,
    /// Opaque client-minted label naming the account this device registered
    /// under, stamped onto every push sent here so the receiver can tell which
    /// mailbox an event id belongs to. `None` for a client that never sent one.
    pub tag: Option<String>,
    /// The APNs environment the client declared this token was minted in,
    /// `production` or `sandbox`. `None` for a client that did not say; the
    /// pusher then uses its configured environment, else the relay's default.
    pub environment: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Refreshed on every re-registration — iOS re-hands its token each launch,
    /// so this, not `created_at`, is the liveness signal.
    pub last_registered_at: DateTime<Utc>,
}

/// One issued human-door device token, described WITHOUT its secret.
///
/// This is the shape both `token list` and the auth middleware get back: there
/// is deliberately no field that could carry the plaintext or the stored hash,
/// so no caller can print one by accident. See
/// [`SqliteStore::verify_device_token`](sqlite::SqliteStore::verify_device_token).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceToken {
    pub id: i64,
    pub account_id: AccountId,
    /// Operator/device-supplied label; never secret.
    pub name: String,
    pub created_at: DateTime<Utc>,
    /// Last time this token authenticated a request, to within a minute — the
    /// verify path throttles the write. `None` until first use.
    pub last_used_at: Option<DateTime<Utc>>,
    /// Set once, forever. A tombstoned token can never authenticate again.
    pub revoked_at: Option<DateTime<Utc>>,
}

/// The request header a client names itself in: `<platform>/<version>`, e.g.
/// `macos/0.0.12`. Lowercase because that is how `http` spells header names.
///
/// ITS OWN HEADER rather than `User-Agent`, because the apps fetch remote
/// images an email chose from the same process: a header the app only ever
/// attaches to requests for its own daemon cannot leak to a sender's server
/// the way a session-wide User-Agent would.
pub const CLIENT_HEADER: &str = "x-passband-client";

/// Longest [`CLIENT_HEADER`] value worth looking at: the longest platform, the
/// slash, and four runs of five digits with their dots, with slack for
/// whitespace. The header is read BEFORE the bearer is known to be good, so
/// anything a stranger sends reaches the parser, and a megabyte of dots must
/// be refused by its length rather than split into a vector first.
const MAX_CLIENT_HEADER_LEN: usize = 48;

crate::types::str_enum! {
    /// Which app a client says it is. A CLOSED set, because it ends up as a
    /// metric label (`squelchd_client_last_seen_timestamp_seconds`) and a label
    /// is a series: an app this daemon has never heard of is not recorded at
    /// all rather than recorded under whatever it called itself.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum ClientPlatform {
        Macos => "macos",
        Ios => "ios",
    }
}

/// What a client said about itself in [`CLIENT_HEADER`], already validated.
///
/// The only way to build one is [`ClientTag::parse`], so a version that reaches
/// the store or a metric label is always dotted digits and nothing else. The
/// header is sent by whoever holds a token, which makes it the tenant's own
/// text, not a stranger's, but it is still text that becomes a label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientTag {
    pub platform: ClientPlatform,
    version: String,
    /// The device's zone, from [`crate::owner_clock::ZONE_HEADER`]. Not part of
    /// the tag's identity as a metric label; it rides along so the same write
    /// that stamps the client stamps where it is.
    zone: Option<chrono_tz::Tz>,
}

impl ClientTag {
    /// Parse `<platform>/<version>`. `None` for anything else: an unknown
    /// platform, a version that is not two to four dot-separated runs of one to
    /// five digits, or a missing half. A request with an unparseable header
    /// authenticates exactly as it would with none; it just is not recorded.
    pub fn parse(header: &str) -> Option<Self> {
        if header.len() > MAX_CLIENT_HEADER_LEN {
            return None;
        }
        let (platform, version) = header.trim().split_once('/')?;
        let platform = ClientPlatform::parse(platform)?;
        let parts = version.split('.').collect::<Vec<_>>();
        let numeric =
            |p: &&str| (1..=5).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit());
        if !(2..=4).contains(&parts.len()) || !parts.iter().all(numeric) {
            return None;
        }
        Some(Self {
            platform,
            version: version.to_string(),
            zone: None,
        })
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// Attach the device zone the request reported, if it is a real IANA zone
    /// ([`crate::owner_clock::device_zone`]). Anything else is dropped, which
    /// leaves the stored zone as it was.
    pub fn with_zone(mut self, raw: Option<&str>) -> Self {
        self.zone = raw.and_then(crate::owner_clock::device_zone);
        self
    }

    pub fn zone(&self) -> Option<chrono_tz::Tz> {
        self.zone
    }
}

/// One (platform, version) pair and the last time any of this account's
/// devices connected as it. See
/// [`SqliteStore::client_versions_seen`](sqlite::SqliteStore::client_versions_seen).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientSeen {
    pub platform: ClientPlatform,
    pub version: String,
    pub seen_at: DateTime<Utc>,
}

/// One platform and the last time any of this account's devices acted as it.
/// See
/// [`SqliteStore::client_actions_seen`](sqlite::SqliteStore::client_actions_seen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientActed {
    pub platform: ClientPlatform,
    pub acted_at: DateTime<Utc>,
}

/// A device token at the ONE moment its plaintext exists.
///
/// Returned by [`SqliteStore::issue_device_token`](sqlite::SqliteStore::issue_device_token)
/// and by a successful pairing claim, and by then the store holds only the hash
/// — so if the caller drops `token` without showing it, that credential is gone
/// for good. No `Serialize`: the wire shapes are the API's own structs, which
/// keeps a stray `.json()` on this type from being the leak.
#[derive(Clone)]
pub struct IssuedDeviceToken {
    pub id: i64,
    pub account_id: AccountId,
    pub name: String,
    /// PLAINTEXT `sqd_…`. Never stored, never logged, shown exactly once.
    pub token: String,
}

impl std::fmt::Debug for IssuedDeviceToken {
    /// The plaintext is REDACTED. `{:?}` on a struct is how secrets reach logs
    /// by accident, and this type exists precisely to carry one.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedDeviceToken")
            .field("id", &self.id)
            .field("account_id", &self.account_id)
            .field("name", &self.name)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// A freshly minted pairing code, plaintext included, for the operator to read
/// aloud or scan. Superseded by the next mint and by its own TTL.
#[derive(Clone)]
pub struct MintedPairingCode {
    pub id: i64,
    /// Display form, `XXXX-XXXX`. The claim normalizes, so the dashes are
    /// cosmetic and callers may hand this string straight to a QR or deep link.
    pub code: String,
    pub expires_at: DateTime<Utc>,
}

impl std::fmt::Debug for MintedPairingCode {
    /// Redacted for the same reason as [`IssuedDeviceToken`]: a code is a
    /// short-lived credential, and short-lived is not the same as harmless.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MintedPairingCode")
            .field("id", &self.id)
            .field("code", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// One observed open of a tracked outbound message.
///
/// SERIALIZED SHAPE IS A WIRE CONTRACT — this is the element type of
/// `GET /client/messages/{id}/opens`. `opened_at` is unix seconds.
///
/// `classification` describes the FETCHER, never a human: `proxied` means Gmail's
/// image proxy asked for the pixel (which may be a cache warm rather than a
/// read), `unknown` is everything else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageOpen {
    pub opened_at: i64,
    pub user_agent: Option<String>,
    pub classification: String,
}

/// Open classification for a fetch Gmail's image proxy made.
pub const OPEN_PROXIED: &str = "proxied";
/// Open classification for every other fetcher.
pub const OPEN_UNKNOWN: &str = "unknown";
/// Open classification for a fetch whose agent marks it as a machine: a
/// security scanner or link checker that fetches a mail's images on delivery.
/// Stored and listed, but never counted as an open and never notified. See
/// [`crate::tracking::classify_user_agent`].
pub const OPEN_SCANNER: &str = "scanner";

/// The message a tracking token points at, resolved in one hop from the token.
/// Only ever `Some` when the tracker exists, belongs to the account, and its
/// echo backfilled a local message id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedMessage {
    /// Local `messages.id` of the echoed copy of the sent mail.
    pub message_id: i64,
    pub thread_id: String,
    pub subject: String,
    /// The sent row's `From:` — the account's own address.
    pub from_addr: String,
}

/// One local draft: an unsent composition keyed by the message it replies to,
/// by the message it forwards, or (both keys `None`) a new message, of which an
/// account may hold any number, each addressed by its own `id`. A row carries
/// at most one of the two keys.
///
/// HUMAN-DOOR ONLY — produced by
/// [`SqliteStore::upsert_draft`](sqlite::SqliteStore::upsert_draft) and friends
/// for `/client/drafts`. Nothing here is synced to Gmail Drafts and no agent-door
/// read reaches the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub id: i64,
    pub account_id: AccountId,
    /// The message this replies to; `None` when it is not a reply.
    pub reply_to_message_id: Option<i64>,
    /// The message this forwards; `None` when it is not a forward. Never set
    /// together with `reply_to_message_id`.
    pub forward_of_message_id: Option<i64>,
    pub to_addr: String,
    /// The other two recipient lists, comma-joined exactly like `to_addr`. `""`
    /// when there are none, which is every draft written before the columns
    /// existed — and for `bcc_addr` empty is the ONLY state anything else can
    /// infer, since nothing derives a blind copy list.
    pub cc_addr: String,
    pub bcc_addr: String,
    pub subject: String,
    pub body: String,
    /// The files this draft has claimed, upload order. Metadata only — a
    /// restore needs names and sizes to draw the tray, and the send re-reads
    /// the bytes by id.
    pub attachments: Vec<OutboundAttachmentMeta>,
    /// First save of this draft; an edit keeps it.
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Context from the replied-to / forwarded message, for a drafts picker:
    /// its thread, subject and sender. All `None` for plain new mail, and for a
    /// draft whose parent is no longer stored.
    pub thread_id: Option<String>,
    pub parent_subject: Option<String>,
    pub parent_from_addr: Option<String>,
    pub parent_from_name: Option<String>,
    /// An agent on the agent door wrote this draft (issue #394). Set at insert
    /// by [`SqliteStore::create_agent_draft`](sqlite::SqliteStore::create_agent_draft)
    /// and kept through every later edit.
    pub from_agent: bool,
}

/// What [`SqliteStore::create_agent_draft`](sqlite::SqliteStore::create_agent_draft)
/// did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentDraftOutcome {
    /// Inserted, with its audit row, under this draft id.
    Created(i64),
    /// The message being replied to already has a draft. Nothing written.
    ReplyDraftExists,
    /// The account already holds the most unsent agent drafts it may.
    /// Nothing written.
    TooMany,
}

/// Which draft row a save addresses. Taken by
/// [`SqliteStore::upsert_draft`](sqlite::SqliteStore::upsert_draft).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftKey {
    /// The one draft replying to this message: edited in place when it exists.
    Reply(i64),
    /// The one draft forwarding this message: edited in place when it exists.
    Forward(i64),
    /// One existing draft by id, whatever it is keyed by (the key itself is
    /// left untouched). Missing, or another account's, is `None`.
    NewById(i64),
    /// A fresh new-message draft: always inserts.
    NewCreate,
    /// OLD-CLIENT COMPATIBILITY for a key-less `PUT /client/drafts`: the newest
    /// new-message draft (neither key set) is edited, created when there is
    /// none. From an app that predates many drafts, that is exactly the old
    /// single-slot behaviour. MIXED-VERSION COST: an older app edits whichever
    /// keyless plain draft is newest, possibly one a newer app has open
    /// elsewhere; last writer wins, rows never pile up. Kept short by shipping
    /// the app with the daemon. The same older app would READ a forward draft
    /// as its new message (it restores the first row with no reply key), so
    /// `GET /client/drafts` lists forward rows only to a client that sends
    /// `forwards=1`.
    NewLatest,
}

/// The COMPOSITION half of a draft: everything the composer holds, with the
/// keys (`account_id`, `reply_to_message_id`, `forward_of_message_id`, `id`)
/// left out. Taken by
/// [`SqliteStore::upsert_draft`](sqlite::SqliteStore::upsert_draft) as one
/// value rather than as five adjacent `&str` parameters, and that is the whole
/// reason it exists: five strings in a row is a transposition waiting to
/// happen, and two of them are recipient lists — swap `cc_addr` and `bcc_addr`
/// and a restored draft sends the blind list where everyone can read it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DraftFields<'a> {
    pub to_addr: &'a str,
    pub cc_addr: &'a str,
    pub bcc_addr: &'a str,
    pub subject: &'a str,
    pub body: &'a str,
}

/// One file staged for a send, WITHOUT its bytes: what a draft listing and an
/// upload's acknowledgement carry. The bytes are fetched only by the send
/// itself and by the byte endpoint, through [`OutboundAttachment`].
///
/// HUMAN-DOOR ONLY, like [`Draft`]: produced for `/client/compose/attachments`
/// and `/client/drafts`, and no agent-door read reaches the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundAttachmentMeta {
    pub id: i64,
    pub filename: String,
    pub mime: String,
    pub size_bytes: i64,
    /// The `cid:` token a body may reference this file by. Angle brackets are
    /// NOT part of it; the MIME builder writes them.
    pub content_id: String,
}

/// [`OutboundAttachmentMeta`] plus the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundAttachment {
    pub meta: OutboundAttachmentMeta,
    pub data: Vec<u8>,
}

/// The `CoreError::InvalidInput` message a scheduling answers when not every
/// named file could move onto the row (unknown, swept, another account's, or
/// held by another draft or scheduled send): the handler's
/// `400 rejected:attachment_missing`.
pub const ATTACHMENT_MISSING: &str = "attachment_missing";

/// The `CoreError::InvalidInput` message a scheduling answers when the account
/// already holds [`MAX_SCHEDULED_PER_ACCOUNT`] live rows: the handler's
/// `400 rejected:too_many_scheduled`.
pub const TOO_MANY_SCHEDULED: &str = "too_many_scheduled";

/// Live (`scheduled`, `sending`, `failed`) outbox rows one account may hold.
/// Each row pins its files' bytes for up to a year, and nobody schedules a
/// hundred mails by hand: past this it is a client loop, not a person.
pub const MAX_SCHEDULED_PER_ACCOUNT: usize = 100;

/// Claims a scheduled send gets before it is `failed` and waits for the user.
/// The claim counts, so a row fails on its third unsuccessful attempt. The
/// worker's claim refuses a row already at the limit (a belt: every path that
/// reaches it settles `failed` first).
pub const SCHEDULED_MAX_ATTEMPTS: u32 = 3;

/// Where a [`ScheduledSend`] is in its life. Stored as the lowercase TEXT
/// [`Self::as_str`] gives; see the `scheduled_sends` table comment in
/// schema.sql for the transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScheduledSendState {
    /// Waiting for `send_at`. The only state the worker claims from.
    Scheduled,
    /// Claimed by the worker under a lease: in Gmail's hands, NOT cancellable.
    Sending,
    Sent,
    /// Out of retries; waits for the user ("send now" or cancel).
    Failed,
    Cancelled,
}

impl ScheduledSendState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Sending => "sending",
            Self::Sent => "sent",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// `None` for text no build ever wrote.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "scheduled" => Self::Scheduled,
            "sending" => Self::Sending,
            "sent" => Self::Sent,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
}

/// One `scheduled_sends` row: a finished mail the daemon sends at `send_at`
/// (docs/SCHEDULED-SEND.md).
///
/// HUMAN-DOOR ONLY, like [`Draft`]: produced for `/client/scheduled` and the
/// scheduled-send worker, never synced, and no agent-door read reaches the
/// table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledSend {
    pub id: i64,
    pub account_id: AccountId,
    /// The send route's JSON body, frozen at schedule time (`confirm` forced
    /// true, `draft_id` stripped). CONTENT: never log or audit it.
    pub request: String,
    pub send_at: DateTime<Utc>,
    pub state: ScheduledSendState,
    /// Set while [`ScheduledSendState::Sending`], `None` otherwise.
    pub lease_until: Option<DateTime<Utc>>,
    /// Claims so far; the worker's claim increments it, a requeue does not.
    pub attempts: u32,
    /// Why the last attempt did not send: a REDACTED reason (a status, an
    /// error class), NEVER mail content, since it is shown and kept.
    pub last_error: Option<String>,
    /// The `Message-ID` minted at schedule time, angle brackets included. The
    /// crash-recovery key: searched for before any retry of a lapsed lease.
    pub message_id_header: String,
    /// Local `messages.id` of the sent copy's echo, once sent and echoed.
    pub sent_message_id: Option<i64>,
    pub sent_thread_id: Option<String>,
    /// STICKY: some earlier attempt may have reached Gmail (a transport error
    /// or a 5xx AFTER the send was posted, or a lapsed lease). Every later
    /// fire asks Gmail for `message_id_header` before sending again.
    pub maybe_sent: bool,
    /// When the last attempt ENDED: stamped by its retry or failed settle (a
    /// requeue stamps the lapsed lease's end). Not by the claim, which would
    /// overwrite the very value the fire needs and which, for a large upload,
    /// can precede the post by a whole lease. A `maybe_sent` row is not fired
    /// before this plus the recovery grace, so its search can see the mail.
    pub last_attempt_at: Option<DateTime<Utc>>,
    /// The first "could not search" deferral of a pre-fire search still
    /// unanswered, or `None`. The give-up clock for that search.
    pub search_deferred_since: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The files this row has claimed, upload order. Metadata only.
    pub attachments: Vec<OutboundAttachmentMeta>,
}

/// What an unclaim does to a row's pre-fire search clock
/// ([`ScheduledSend::search_deferred_since`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchClock {
    /// The search answered (or none was owed): the clock stops.
    Reset,
    /// The search could not be made: the clock starts, unless it is running.
    Start,
    /// The search was not tried (the fire-time grace): left as it is.
    Keep,
}

/// What [`SqliteStore::cancel_scheduled_send`](sqlite::SqliteStore::cancel_scheduled_send)
/// found (the row is boxed only to keep the enum small). Three answers because the handler gives three: the cancelled row (its
/// `request` reopens the composer), `409` for a row already in Gmail's hands,
/// and `404` for an unknown id, another account's, or one that is already
/// settled (`sent`/`cancelled`: there is nothing left to cancel).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelOutcome {
    Cancelled(Box<ScheduledSend>),
    Sending,
    NotFound,
}

/// The picker context of the stored message a composition replies to or
/// forwards: the same four columns a [`Draft`] carries flat, read by the same
/// account-pinned join. Every field is `None` for new mail and for a parent
/// that is no longer stored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParentContext {
    pub thread_id: Option<String>,
    pub subject: Option<String>,
    pub from_addr: Option<String>,
    pub from_name: Option<String>,
}

/// One recent message from a sender, as the RULE SUGGESTER sees it: the subject
/// plus what triage and the owner did with it. Non-sealed, inbound, non-spam
/// only; see [`Store::sender_rule_digest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderDigestRow {
    pub subject: String,
    /// Triage tier (`signal`, `noise`, ...).
    pub tier: String,
    /// Attention status: `new`, `open` or `done`. Mail the triage agent
    /// marked done on a re-check (the `auto_done` ledger), untouched since,
    /// is `new`: the owner did not clear it.
    pub status: String,
    /// The owner opened it in Passband.
    pub opened: bool,
    /// What triage called it (`marketing`, `bill`, ...), when it said.
    pub category: Option<String>,
}

/// Whether a smart block has been worked out for a sender, and what it was.
#[derive(Debug, Clone, PartialEq)]
pub enum SmartBlockMemo {
    NotAsked,
    /// Asked and answered; `None` is "this sender has no line to draw".
    Asked(Option<SmartBlock>),
}

impl SmartBlockMemo {
    /// Read the `unsubscribes.smart_block` column: NULL is never asked, JSON
    /// `null` is an answered no. Anything unreadable is asked again.
    pub(crate) fn from_column(raw: Option<String>) -> Self {
        raw.and_then(|raw| serde_json::from_str::<Option<SmartBlock>>(&raw).ok())
            .map_or(Self::NotAsked, Self::Asked)
    }
}

/// The store-facing outcome of applying a parsed Stage-1 LLM result onto a
/// triage row: stamps `stage1_model_used` (leaving the Stage-1 queue) and sets
/// `needs_stage2` (whether the row escalates).
#[derive(Debug, Clone)]
pub struct Stage1Applied {
    pub message_id: i64,
    pub account_id: AccountId,
    pub importance: u8,
    pub tier: Tier,
    pub one_line: String,
    pub reason: String,
    /// Per-property justifications describing the STORED values, replacing the
    /// heuristic seed reasons. Human-door only.
    pub field_reasons: FieldReasons,
    /// The Stage-1 model id to stamp `stage1_model_used` with.
    pub stage1_model_used: String,
    /// Stored as `needs_stage2`. Historical: the retired escalation router set
    /// it; nothing reads it to route mail any more.
    pub needs_stage2: bool,
    /// Stored as `escalation_reason`. Historical, like `needs_stage2`.
    pub escalation_reason: Option<&'static str>,
    /// A deadline to (re)write for this message, if the model extracted one.
    pub deadline: Option<DeadlineHit>,
    /// Routing category (`general` | `invoice` | `banking_statement` |
    /// `transaction_alert`); `None` leaves `triage.category` untouched.
    pub category: Option<String>,
}

/// One row queued for a specialist EXTRACTOR pass: a NON-SEALED triage row whose
/// LLM-assigned `category` has a registered extractor and that has not yet been
/// extracted (`extractor_model_used IS NULL`). The query gates on
/// `sensitivity='normal'` and on a real LLM category (sealed rows carry
/// `category=NULL`), so this never represents sealed mail.
#[derive(Debug, Clone)]
pub struct ExtractQueued {
    pub message_id: i64,
    pub account_id: AccountId,
    /// Carried for context/logging; the extract pass shares the Stage-1 GLOBAL
    /// budget scope.
    pub thread_id: String,
    pub from_addr: String,
    pub from_name: Option<String>,
    pub subject: String,
    pub body: String,
    /// The routing category that selected this row; decides which specialist
    /// extractor runs.
    pub category: String,
    /// Drives the stored `banking.received_at` and the stale-skip check.
    pub received_at: DateTime<Utc>,
    /// Always `'normal'` for queued rows; carried so the sealed guard can assert.
    pub sensitivity: Sensitivity,
    /// When a human last asked for this row to be re-triaged. Overrides the
    /// stale skip in
    /// [`crate::triage::extract::route_extract_row`] and in the shipments loop.
    pub retriage_at: Option<DateTime<Utc>>,
}

/// The store-facing outcome of running the marketing extractor on a row: a
/// `marketing` upsert plus the extractor marker.
#[derive(Debug, Clone)]
pub struct MarketingApplied {
    pub message_id: i64,
    pub account_id: AccountId,
    pub brand: Option<String>,
    pub offer: Option<String>,
    pub discount: Option<String>,
    /// Shape-validated promo code, or `None`. Never a sentence or a URL.
    pub code: Option<String>,
    /// `YYYY-MM-DD`, or `None` when absent/implausible.
    pub expires_at: Option<String>,
    pub received_at: DateTime<Utc>,
    /// Stamped onto `triage.extractor_model_used` so the queue stops selecting
    /// the row. Unlike banking, this write does NOT resolve the triage row.
    pub extractor_model_used: String,
}

/// One extracted promotion, for the client's marketing surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketingOffer {
    pub message_id: i64,
    pub thread_id: String,
    pub sender: String,
    pub subject: String,
    pub brand: Option<String>,
    pub offer: Option<String>,
    pub discount: Option<String>,
    pub code: Option<String>,
    pub expires_at: Option<String>,
    pub received_at: DateTime<Utc>,
}

/// The store-facing outcome of the banking specialist extractor: a `banking`
/// row upsert plus the extractor marker and (for records) an auto-resolve.
pub struct BankingApplied {
    pub message_id: i64,
    pub account_id: AccountId,
    /// `statement` | `transaction_alert` — derived from the row's category, not
    /// the model.
    pub kind: String,
    pub institution: Option<String>,
    pub amount: Option<f64>,
    pub currency: Option<String>,
    /// Masked last-4 tail (`…1234`) or `None` — never a full account number
    /// (post-validated in the extractor's apply).
    pub account_hint: Option<String>,
    pub received_at: DateTime<Utc>,
    /// Stamped onto `triage.extractor_model_used` so the queue stops selecting
    /// the row.
    pub extractor_model_used: String,
    /// `true` => also resolve the triage row to `status='done'`: banking
    /// statements/alerts are RECORDS and must leave the attention bands.
    pub auto_resolve: bool,
}

/// A day's Stage-2 API usage for one account. Cost is NOT stored — the human
/// door computes it from config-driven per-MTok prices at read time.
/// `input_tokens` is the UNCACHED prompt remainder; prompt-cache writes and
/// reads are separate columns because they price differently.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stage2Usage {
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
}

/// One day's Stage-2 usage row carrying its `day` key, for the human-door usage
/// history.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stage2UsageDay {
    /// UTC date key, `YYYY-MM-DD`.
    pub day: String,
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
}

/// One UTC day of the mailbox's own traffic, for the human door's activity
/// report. `received` includes auth mail but excludes provider spam. Tier fields
/// are compatibility projections; `pending` is unclassified mail and `sealed`
/// is an overlapping actionable-auth count, not a separate classification tier.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailActivityDay {
    /// UTC date key, `YYYY-MM-DD` — the usage ledger's key, so the two series
    /// line up day for day.
    pub day: String,
    pub received: u64,
    pub sent: u64,
    pub sealed: u64,
    pub past_due: u64,
    pub deadline: u64,
    pub signal: u64,
    pub noise: u64,
    /// Received mail awaiting a current agent classification.
    pub pending: u64,
}

/// One call's token counts, as the ledger records them: `input` is the UNCACHED
/// prompt remainder, with prompt-cache writes and reads in their own fields
/// because they price differently. A struct rather than four positional u64s so
/// a transposed input/output can't compile — which is also why there is no
/// positional constructor: build it as a field-named literal (production goes
/// through `From<Usage>`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageTokens {
    pub input: u64,
    pub output: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
}

/// Runtime daily-cap overrides from `app_settings`. `None` means no override
/// row, so the caller falls back to config/env then the built-in default; only
/// values parsing as an integer in `1..=100000` are surfaced, a malformed or
/// out-of-range value counts as absent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stage2CapOverrides {
    pub thread_daily_cap: Option<u32>,
    pub sender_daily_cap: Option<u32>,
    pub global_daily_cap: Option<u32>,
    /// The Stage-1 GLOBAL daily-cap override (Stage-1 has only a global cap).
    pub stage1_global_daily_cap: Option<u32>,
}

/// A NON-SEALED message still needing an embedding vector, for the startup
/// backfill pass. Carries only the text the embedder consumes.
#[derive(Debug, Clone)]
pub struct MissingVector {
    pub message_id: i64,
    pub subject: String,
    pub body: String,
}

/// A hybrid-search hit plus WHICH RECALL LEG produced it: the keyword list, the
/// vector list, or both. See
/// [`SqliteStore::hybrid_search_legs`](crate::store::SqliteStore::hybrid_search_legs).
///
/// The two facts are separate bools rather than an enum because a hit really
/// can be on both lists, and "both" is the interesting case: it is the one that
/// says the words AND the meaning agree.
#[derive(Debug, Clone)]
pub struct LeggedHit {
    pub hit: SearchHit,
    /// The FTS5 keyword leg returned this row.
    pub keyword: bool,
    /// The vector KNN returned this row.
    pub vector: bool,
}

/// A locally-stored sealed message, exposed ONLY to the TUI. This type never
/// crosses the MCP boundary.
#[derive(Debug, Clone)]
pub struct SealedMessage {
    pub id: i64,
    pub account_id: AccountId,
    pub thread_id: String,
    pub from_addr: String,
    pub subject: String,
    pub received_at: DateTime<Utc>,
    /// The one kind a client sees: [`AuthKind::priority_pick`] over the
    /// decision's kinds, in wire spelling (`magic_link`). Legacy rows keep
    /// their stored `triage.sealed_kind`.
    ///
    /// [`AuthKind::priority_pick`]: crate::triage::decision::AuthKind::priority_pick
    pub sealed_kind: Option<String>,
    /// A takeover-class account change that holds FYE: `AuthAssessment::
    /// is_critical`, unless the sender explicitly failed authentication
    /// (`auth_pass = 0`). The value FYE uses (`auth_critical_sql!`).
    pub critical: bool,
    /// The owner's server-side ruling, if they made one.
    pub verdict: Option<crate::types::AuthVerdict>,
    /// The auth message that accounts for this alert (a code, link or
    /// sign-in receipt from the same service within minutes), computed at
    /// read time by [`crate::triage::auth_pairing`].
    pub explained_by: Option<i64>,
}

/// One row of the user's OWN sent mail, for the human door's sent listing
/// ([`Store::sent_listing`]). HUMAN-DOOR ONLY: the agent door has no sent
/// surface, and every other listing filters `is_sent = 0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentMessage {
    pub id: i64,
    pub thread_id: String,
    /// Display recipients, comma-joined `Name <addr>`; `""` when the row predates
    /// the recipients backfill or its headers named nobody.
    pub to: String,
    pub subject: String,
    pub snippet: String,
    /// `messages.received_at` VERBATIM — the stored RFC3339 string, not a
    /// re-formatted parse of it.
    pub sent_at: String,
    /// Recorded read receipts for this message (`message_opens` through
    /// `send_trackers`). 0 for an untracked send, which is the default.
    pub opens: i64,
}

/// A message still missing its display recipients, for the one-shot recipients
/// backfill. Carries only what the Gmail metadata fetch needs.
#[derive(Debug, Clone)]
pub struct MessageMissingRecipients {
    pub message_id: i64,
    pub gmail_msg_id: String,
}

/// A stored message whose text body is BLANK TO A READER while a rendered HTML
/// body sits beside it: the population the one-shot blank-body heal re-fetches.
/// Carries only what the Gmail raw fetch and the re-ingest decision need. See
/// [`Store::blank_body_messages`].
#[derive(Debug, Clone)]
pub struct BlankBodyMessage {
    pub message_id: i64,
    pub gmail_msg_id: String,
    /// So the caller can decide, by the history window the agent cutover
    /// itself re-triaged, whether a re-read is worth an investigation or
    /// only the text.
    pub received_at: DateTime<Utc>,
}

/// One chunk of the blank-body scan: the blank rows found among the `limit`
/// newest rows below the cursor, and where the next chunk starts.
#[derive(Debug, Clone, Default)]
pub struct BlankBodyScan {
    pub candidates: Vec<BlankBodyMessage>,
    /// The id to pass back as `before_id` for the next chunk; `None` when the
    /// scan has walked past the oldest row and there is nothing left to read.
    pub next_before_id: Option<i64>,
}

/// How far [`Store::ingest_message_fresh`] hands a re-read row back to the
/// agent. Under either scope the message's CONTENT REVISION advances (the
/// stored body changed), which is what makes the external-agent access
/// assessment it carried go back to `pending` until re-read over the real
/// text; the two scopes differ only in which durable job the re-read queues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealScope {
    /// The text, and an ACCESS reassessment only (one small model call, the
    /// `source_access` job). For mail past the history window: the cutover
    /// did not investigate it either, and a background investigation of a
    /// years-old row is model spend that produces a placement nobody asked
    /// for. Its current decision stays whatever it was.
    Text,
    /// The text AND a fresh investigation: the row re-enters the agent's
    /// background lane at its new content revision, exactly as a changed
    /// message does, under the `heal` trigger.
    TextAndTriage,
}

/// What the archive walk's disk guards measure, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveDisk {
    /// Free to an unprivileged writer on the filesystem holding the database.
    pub free_bytes: u64,
    /// That whole filesystem's size. On a volume several tenants share it is
    /// everybody's, which is why a share of it is part of the floor.
    pub total_bytes: u64,
    /// This database's own file plus its write-ahead log.
    pub db_bytes: u64,
}

impl ArchiveDisk {
    /// No disk to fill: all the room there is, and nothing used.
    pub const UNBOUNDED: Self = Self {
        free_bytes: u64::MAX,
        total_bytes: 0,
        db_bytes: 0,
    };
}

/// The squelch local store. Implemented by [`SqliteStore`].
///
/// SECURITY: every method that can feed the MCP surface (`thread_view`,
/// `deadlines`, the agent listings) MUST exclude `sensitivity = 'sealed'` in the SQL
/// itself. `sealed_messages` is the sole local-only escape hatch (TUI).
pub trait Store: agent_triage::AgentTriageStore + Send + Sync {
    /// Insert or update a message (and its FTS body + derived contacts).
    /// Returns the local message id.
    fn upsert_message(&self, msg: &NewMessage) -> Result<i64>;

    /// MCP-facing thread view. Returns `NotFound` for a sealed thread so it is
    /// indistinguishable from a nonexistent one.
    ///
    /// SECURITY: the returned [`ThreadView`] carries text ONLY — never HTML.
    /// Two methods returning two types is the structural guarantee that html
    /// never crosses /mcp; the html-bearing variant is
    /// [`Store::thread_view_with_html`].
    fn thread_view(&self, account_id: AccountId, thread_id: &str) -> Result<ThreadView>;

    /// Resolve a LOCAL MESSAGE id to its `thread_id`, for the `get_thread`
    /// forgiveness path. Unknown and sealed both return `NotFound` so sealed rows
    /// never leak thread existence. The resolved thread may still contain sealed
    /// messages — the caller re-runs the full guard via `thread_view`.
    fn thread_id_for_message(
        &self,
        account_id: AccountId,
        message_id: i64,
    ) -> Result<Option<String>>;

    /// HUMAN-DOOR-ONLY thread view: same sealed/nonexistent -> `NotFound`
    /// behavior as [`Store::thread_view`], but each message also carries its
    /// sanitized `html` (`None` for plain-text-only mail). MUST NOT be called
    /// from MCP, sync, or triage.
    ///
    /// Its `is_sent` is AUTHORSHIP, not the stored column: the stored flag OR a
    /// case-insensitive `from_addr` == account-email match, because the column is
    /// a visibility flag that stays 0 on self-addressed mail the user did write.
    /// See [`crate::types::ClientMessage::is_sent`].
    fn thread_view_with_html(
        &self,
        account_id: AccountId,
        thread_id: &str,
    ) -> Result<crate::types::ClientThreadView>;

    /// HUMAN-DOOR-ONLY attachment byte fetch. `data` is `None` when the bytes
    /// were not stored (over the ingest cap — the endpoint answers 410).
    ///
    /// SECURITY: the query requires the PARENT message's `sensitivity='normal'`,
    /// so an attachment on sealed mail returns `Ok(None)`, indistinguishable
    /// from an unknown id.
    fn attachment_bytes(
        &self,
        account_id: AccountId,
        attachment_id: i64,
    ) -> Result<Option<AttachmentBytes>>;

    /// MCP-facing deadlines within `within_days` (None = all). Sealed excluded.
    fn deadlines(&self, account_id: AccountId, within_days: Option<u32>) -> Result<Vec<Deadline>>;

    /// Upsert a shipment keyed by `(account_id, tracking_number)`. A later mail
    /// about the same tracking number UPDATES the row through the no-regress
    /// status state machine (a delivered shipment is never walked back),
    /// refreshing `last_update`/`last_message_id` and adopting a longer
    /// `item_name`.
    ///
    /// An update the state machine ACCEPTS also resets `poll_failures` to 0 —
    /// mail is the second witness that un-retires a shipment the carrier has
    /// been rejecting, since the only other one (a successful poll) is
    /// unreachable once the row has left the poll queue.
    ///
    /// SECURITY: callers run this ONLY for non-sealed mail, so `shipments` holds
    /// no sealed rows by construction and reads need no sealed join.
    fn upsert_shipment(
        &self,
        account_id: AccountId,
        message_id: i64,
        shipment: &ShipmentInfo,
        seen_at: DateTime<Utc>,
    ) -> Result<i64>;

    /// List shipments for the account, most-recently-updated first;
    /// `include_delivered=false` restricts to en-route (status != 'delivered').
    /// Sealed rows are structurally absent, so no sealed filter is required.
    ///
    /// Two READ-SIDE hides apply, both of which leave the row live in the table
    /// and both of which reverse themselves:
    ///
    /// * SILENCE, the rule in
    ///   [`ShipmentListPolicy::hides`](crate::config::ShipmentListPolicy::hides),
    ///   which the agent door applies too so the two doors list the same
    ///   packages: `last_update` older than [`stale_after_days`] AND no carrier
    ///   vouching for the row. `last_update` moves on newer mail about the
    ///   package or a poll that changed something visible, never on a poll that
    ///   merely confirms. Newer mail or a poll moves it and the row is back.
    /// * `cleared_at` hides a row the user cleared, but ONLY while
    ///   `last_update <= cleared_at`. There is no un-clear: the comparison IS the
    ///   revival, so the first update to land after the clear brings the row back
    ///   with no second write anywhere.
    ///
    /// NONE of this touches [`Store::list_pollable_shipments`], deliberately: a
    /// hidden row keeps being polled, because a poll is the most likely source of
    /// the update that un-hides it.
    ///
    /// [`stale_after_days`]: crate::config::ShipmentListPolicy::stale_after_days
    fn list_shipments(
        &self,
        account_id: AccountId,
        include_delivered: bool,
        policy: crate::config::ShipmentListPolicy,
    ) -> Result<Vec<crate::types::Shipment>>;

    /// USER CLEAR: stamp `shipments.cleared_at = at`, taking the row out of
    /// [`Store::list_shipments`] until something advances its `last_update` past
    /// that stamp. Returns `false` for an unknown id (or another account's).
    ///
    /// IDEMPOTENT, and re-clearing RESTAMPS: a row that was cleared, revived by
    /// an update, and cleared again must hide against the LATER stamp, so the
    /// write is unconditional rather than "only if NULL".
    ///
    /// There is no `unclear_shipment`, by design. Un-hiding is the comparison in
    /// the listing, and the events that should un-hide a package (a poll that
    /// moved it, an email that advanced it) already write `last_update`.
    ///
    /// A CARD IS CLEARED WHOLE. When the row is one package of a grouped card
    /// (one order in several boxes, see [`crate::triage::order_link`]), every
    /// package on that card is stamped, the group being the one
    /// [`Store::list_shipments`] shows under the same `policy`. Each package
    /// still revives on its own when its own `last_update` moves.
    fn clear_shipment(
        &self,
        account_id: AccountId,
        shipment_id: i64,
        at: DateTime<Utc>,
        policy: crate::config::ShipmentListPolicy,
    ) -> Result<bool>;

    /// CALENDAR GHOSTS whose start falls in `[from, to)`, ordered by start,
    /// HUMAN-DOOR ONLY, at most 2000. A timed start compares as an instant; a
    /// date-only one is listed when its date is on or after `from`'s date in
    /// `from`'s offset and before `to`'s date in `to`'s own offset, and sorts
    /// at midnight in `from`'s offset. Each carries its eligible sources and
    /// the duration prior for its venue and kind. A ghost with no start, or
    /// whose facts came from a message that is not eligible right now (sent,
    /// spam or restricted), is never listed: that test is in the SQL, so it
    /// holds even when no rebuild has run since the message's status changed.
    fn list_ghost_events(
        &self,
        account_id: AccountId,
        from: DateTime<chrono::FixedOffset>,
        to: DateTime<chrono::FixedOffset>,
    ) -> Result<Vec<crate::types::GhostEvent>>;

    /// The user's PATCH of a ghost, applied in ONE transaction. Each field is
    /// `None` to leave it alone, `Some(None)` to clear it (a cleared state
    /// clears its stamp), `Some(Some(_))` to set it. Returns `false` for an
    /// unknown id or another account's, and
    /// [`CoreError::InvalidInput`](crate::error::CoreError::InvalidInput) for
    /// an end that is not after the start at the same precision, or a ghost
    /// with no start; either way NOTHING is written, the state included. The
    /// end is stored as a span from the current start, so mail that later
    /// moves the start moves it too. A timed end on a timed ghost with a venue
    /// also records the duration as the prior for that venue and kind;
    /// clearing the end keeps what was learned. The projection never
    /// overwrites either field.
    fn patch_ghost_user(
        &self,
        account_id: AccountId,
        ghost_id: i64,
        end: Option<Option<crate::triage::ghost::EventTime>>,
        state: Option<Option<crate::types::GhostUserState>>,
        at: DateTime<Utc>,
    ) -> Result<bool>;

    /// ONE-SHOT REPAIR: re-run shipment detection over every shipment row's
    /// feeder message and delete the rows the current detector no longer yields
    /// that number from, returning how many were deleted.
    ///
    /// EXACTLY ONCE PER ACCOUNT, and the store enforces it: the pass records its
    /// own done-flag in the SAME TRANSACTION as its deletions, and every later
    /// call returns 0 without judging anything. Callers do not gate it. The
    /// atomicity matters because the keep test is the REGEX detector — rows the
    /// shipments extractor wrote are precisely the ones it cannot reproduce, so a
    /// pass that ran but went unrecorded would reap them on the next start.
    ///
    /// Rows with no feeder message are left alone, as are rows carrying carrier
    /// evidence (a raw status or any poll attempt) or extractor evidence (an
    /// order reference): the one-shot exists to reap pre-tightening regex
    /// phantoms and nothing else.
    fn shipments_redetect_cleanup(&self, account_id: AccountId) -> Result<u64>;

    /// One-shot repair of receipt totals mis-parsed by the pre-fix amount
    /// patterns, which could match the FRACTIONAL TAIL of an unrounded float an
    /// email printed as its own total ("67.28999999999999 USD" stored as
    /// 28999999999999). Re-runs the fixed extraction over each row's message and
    /// corrects the stored amount; returns the number of rows changed.
    ///
    /// EXACTLY ONCE PER ACCOUNT, enforced by the store in the same transaction as
    /// the corrections. Callers do not gate it.
    fn receipts_reparse_cleanup(&self, account_id: AccountId) -> Result<u64>;

    /// Shipments worth a carrier-API poll: not yet delivered, on a carrier that
    /// HAS an API ("ups" | "usps" | "fedex" | "dhl" — Amazon and "unknown" have
    /// none), first seen at or after `min_first_seen`, and under `max_failures`
    /// permanent poll failures. Never-polled rows come first, then the
    /// least-recently-polled, so a caller taking a prefix spends its budget
    /// evenly. Sealed rows are structurally absent, as for every shipment read.
    ///
    /// DELIBERATELY BLIND TO THE LISTING FILTERS. A row the user cleared, and a
    /// row hidden as stale, are BOTH still polled — a poll is exactly what
    /// produces the `last_update` that brings them back, so filtering them here
    /// would make hiding permanent and silently break revival. Only
    /// [`Store::list_shipments`] filters. Do not "optimize" this.
    fn list_pollable_shipments(
        &self,
        account_id: AccountId,
        min_first_seen: DateTime<Utc>,
        max_failures: u32,
    ) -> Result<Vec<crate::types::Shipment>>;

    /// Apply one carrier-API result to a shipment, returning whether `status`
    /// changed (`false` for an unknown id). The carrier is ground truth, so the
    /// status is REPLACED through
    /// [`ShipmentStatus::reconcile_carrier`](crate::triage::ShipmentStatus::reconcile_carrier)
    /// rather than ratcheted — except when `track.status` is `None`, which
    /// leaves it untouched. `carrier_status_raw`/`eta` always take the carrier's
    /// values, `last_polled_at` always advances, and `poll_failures` resets;
    /// `delivered_at` fills once and is never overwritten. `last_update` moves
    /// ONLY when something user-visible changed, so polling does not churn the
    /// Sitrep sort order.
    ///
    /// `last_message_id` is NEVER touched — no message backs a poll, so the
    /// row's click target stays the last accepted email.
    fn apply_carrier_track(
        &self,
        account_id: AccountId,
        shipment_id: i64,
        track: &CarrierTrack,
        polled_at: DateTime<Utc>,
    ) -> Result<bool>;

    /// Record a poll attempt that produced no track. `last_polled_at` always
    /// advances (so the shipment rotates through the queue); `poll_failures`
    /// bumps only for a PERMANENT failure — an unknown or expired tracking
    /// number, not a transient network or rate-limit error.
    ///
    /// EVERY ANSWERED ATTEMPT IS RECORDED HERE, permanent or not. A row asked
    /// about and then left unstamped keeps the head of a queue that sorts
    /// never-polled first, so it is polled again, and again, ahead of everything
    /// behind it — one deterministically-failing number would own a carrier's
    /// entire throughput. Whether the failure counts is the caller's judgment
    /// (see [`crate::carriers::poller`]); that it happened is not.
    fn record_poll_outcome(
        &self,
        account_id: AccountId,
        shipment_id: i64,
        polled_at: DateTime<Utc>,
        permanent_failure: bool,
    ) -> Result<()>;

    /// Upsert a receipt keyed by `(account_id, message_id)` — a re-ingest of the
    /// same message updates in place (idempotent).
    ///
    /// SECURITY: callers run this ONLY for non-sealed mail, so `receipts` holds
    /// no sealed rows by construction.
    fn upsert_receipt(
        &self,
        account_id: AccountId,
        message_id: i64,
        from_addr: &str,
        from_name: Option<&str>,
        receipt: &ReceiptInfo,
        received_at: DateTime<Utc>,
    ) -> Result<i64>;

    /// Receipts received within the last `days`, newest first. Sealed rows are
    /// structurally absent, so no sealed filter is required.
    fn list_receipts(&self, account_id: AccountId, days: u32) -> Result<Vec<Receipt>>;

    /// Upsert a calendar update keyed by `(account_id, message_id)` — a re-ingest
    /// of the same message updates in place (idempotent).
    ///
    /// SECURITY: callers run this ONLY for non-sealed mail, so
    /// `calendar_updates` holds no sealed rows by construction.
    fn upsert_calendar_update(
        &self,
        account_id: AccountId,
        message_id: i64,
        calendar: &CalendarInfo,
        received_at: DateTime<Utc>,
    ) -> Result<i64>;

    /// Calendar updates RECEIVED within the last `hours` (mail arrival window,
    /// NOT event start time), newest first, each joined to its `thread_id` so the
    /// client can open the mail. Sealed rows are structurally absent.
    fn list_calendar_updates(
        &self,
        account_id: AccountId,
        hours: u32,
    ) -> Result<Vec<CalendarUpdate>>;

    // SPECIALIST EXTRACTORS (categorize-then-extract): the LLM assigns a
    // `category`; a category with a registered extractor queues the row for a
    // structured second pass. Sealed rows carry `category=NULL` and are
    // structurally excluded from every extractor queue.

    /// Up to `limit` rows queued for a specialist extractor: NON-SEALED,
    /// non-sent rows whose `category` is in `categories` and whose
    /// `extractor_model_used IS NULL`, newest first. Rows that already produced a
    /// RECEIPT are excluded so a receipt and a banking row never double-create.
    fn extract_queue(
        &self,
        account_id: AccountId,
        categories: &[&str],
        limit: usize,
    ) -> Result<Vec<ExtractQueued>>;

    /// Up to `limit` rows the SHIPMENTS extractor still owes a verdict:
    /// NON-SEALED, non-sent rows with `ship_extract_model='pending'`, newest
    /// first. Deliberately NOT [`Store::extract_queue`] — that one routes on
    /// `triage.category` (no shipping category exists) and excludes
    /// receipt-bearing messages, which most order confirmations are. The trigger
    /// is stamped at INGEST from a loose shipping signal, so a queued row may
    /// still carry a NULL category, surfaced here as `""`.
    fn ship_extract_queue(&self, account_id: AccountId, limit: usize)
    -> Result<Vec<ExtractQueued>>;

    /// Stamp `triage.ship_extract_model` with a PROCESSED marker (the extractor
    /// model id, or a `'stale-skip'` / `'apply-failed'` / `'extract-failed'`
    /// sentinel), taking the row out of [`Store::ship_extract_queue`]. Guarded by
    /// `sensitivity='normal'`, exactly like [`Store::extract_mark_processed`].
    fn ship_extract_mark(&self, account_id: AccountId, message_id: i64, marker: &str)
    -> Result<()>;

    /// Apply one SHIPMENTS-EXTRACTOR verdict IN ONE TRANSACTION: stamp
    /// `triage.ship_extract_model` (leaving [`Store::ship_extract_queue`]), then
    /// reconcile the package IDENTITY the model found against what the regex
    /// detector already wrote. Returns whether a tracked record was written or
    /// updated — a `shipments` row, or a staged `shipment_orders` row.
    ///
    /// The marker is stamped FIRST, guarded by `sensitivity='normal'`. If that
    /// guard matches nothing the message was SEALED mid-pass, and the call writes
    /// NOTHING derived from it and returns `Ok(false)` — the same TOCTOU rule as
    /// [`Store::stage1_apply`].
    ///
    /// DELETION RULE — a row is deleted ONLY on POSITIVE EVIDENCE that it is a
    /// phantom: the extractor named a DIFFERENT tracking number for this mail, or
    /// declared it not a shipment at all. An extraction that names NO number is
    /// silence, not a verdict, and deletes nothing — the detector that minted the
    /// row already passed a carrier-signal gate, a tracking-label context gate and
    /// an item/order hard negative, while the extractor drops a number it sees
    /// echoed in the order field.
    ///
    /// Every delete is then bounded three ways: to rows THIS message CREATED
    /// (`shipments.created_by_message_id`, immutable — `last_message_id` moves to
    /// the latest feeder and would put another mail's package in range), to
    /// AMBIGUOUS tracking SHAPES only (see
    /// [`is_ambiguous_tracking_shape`](crate::triage::is_ambiguous_tracking_shape)),
    /// so a model false negative can never destroy a real `1Z…` / `TBA…` / IMpb
    /// package, and NEVER to a row a carrier has answered about or been polled
    /// for. Then, by which identity the model found:
    ///
    /// * TRACKING NUMBER — upsert the shipment (status still flows through
    ///   [`ShipmentStatus::merge`](crate::triage::ShipmentStatus::merge), so a
    ///   delivered package never walks back), write `item_name` with
    ///   `item_name_source='llm'` (which beats any 'regex' name outright, since
    ///   the upsert's longer-name-wins heuristic otherwise keeps regex junk, and
    ///   yields to another 'llm' name only when that one is longer), record
    ///   `order_ref` with its merchant, and
    ///   PROMOTE any staged `shipment_orders` row under that reference — donating
    ///   its item name, and that name's PROVENANCE, if the shipment has none —
    ///   then delete it.
    /// * ORDER REFERENCE ONLY — adopt the item name onto the shipment already
    ///   carrying that reference, or STAGE the purchase in `shipment_orders`
    ///   until a ship notice arrives with a number.
    /// * NEITHER — name adoption only, and only when the thread holds exactly one
    ///   shipment and that row has no name. Status, `last_message_id` and
    ///   `last_update` are never touched: a mail with no identity has no claim on
    ///   a row's lifecycle.
    ///
    /// ORDER REFERENCES ARE MERCHANT-SCOPED. "Order #1042" is unique only within
    /// the shop that issued it, so both the `shipments` lookup and the staging key
    /// pair it with `order_merchant`, the registrable domain of the feeding
    /// message's sender. Where several rows still match (an order that split into
    /// two packages), the name is donated to NEITHER — ambiguous identity does not
    /// guess.
    ///
    /// `last_message_id` is only ever set by the tracking-number upsert, always to
    /// a real message id — the seal-time delete keys on it — and every item-name
    /// write stamps `item_name_msg` (which MESSAGE) and `item_name_source` (which
    /// MECHANISM), so sealing scrubs a donated name even from a row another
    /// message feeds, and resets the source with it.
    fn shipments_extract_apply(&self, applied: &ShipmentsApplied) -> Result<bool>;

    /// How far the live dev re-triage has got, for the progress modal that
    /// blocks the app while one runs. Read-only and cheap: one aggregate over
    /// the rows carrying a live `retriage_at` stamp — see [`RetriageProgress`].
    /// A zero `total` means no run is in flight.
    fn retriage_progress(&self, account_id: AccountId) -> Result<RetriageProgress>;

    /// DEV RE-TRIAGE: clear the LLM markers on non-sealed, non-sent inbound rows
    /// so they re-enter the Stage-1 queue, deleting their stale `banking`,
    /// `marketing` and `shipment_orders` rows (extraction recreates them) and
    /// re-pending any row that ever carried a shipping signal. `shipments` rows
    /// SURVIVE — they are identity-keyed by tracking number and carry
    /// carrier-poll state no re-run can recover — but the ITEM NAMES the reset
    /// messages contributed are cleared in both shipment tables, by
    /// `item_name_msg` provenance, exactly as sealing does. A name is a model
    /// verdict, and re-triage is redoing that verdict; leaving it in place means
    /// a re-extraction that finds no name (or says it was never a shipment)
    /// silently keeps the old one forever.
    /// Rule-decided rows (`stage1_model_used='rule'`)
    /// and sealed/sent rows (`'n/a'`) are NEVER touched — rules are authoritative
    /// and sealed mail re-enters no queue. `message_id=None` scopes to the
    /// trailing `days` of inbound mail; `Some(id)` to that one message.
    fn retriage_reset(
        &self,
        account_id: AccountId,
        message_id: Option<i64>,
        days: u32,
    ) -> Result<u64>;

    /// Restore owned spam to human inventory and atomically enqueue push-silent
    /// agent triage. Resets attention to new. Restricted auth can also be rescued.
    /// Returns false for unknown, foreign-account, or already non-spam messages.
    /// The caller restores the message in Gmail before changing local state.
    fn clear_spam(&self, account_id: AccountId, message_id: i64) -> Result<bool>;

    /// Mark an extract-queued row PROCESSED without writing a specialist row —
    /// stamp `extractor_model_used` only. Used on the skip / refusal / permanent-
    /// error paths so the row does not loop. Guarded by `sensitivity='normal'`.
    fn extract_mark_processed(
        &self,
        account_id: AccountId,
        message_id: i64,
        extractor_model_used: &str,
    ) -> Result<()>;

    /// Apply a parsed banking extraction IN ONE TRANSACTION: upsert the `banking`
    /// row, stamp `triage.extractor_model_used` (leaving the extract queue), and
    /// when `auto_resolve` is set resolve the triage row to `status='done'`.
    /// Guarded by `sensitivity='normal'`.
    fn banking_apply(&self, applied: &BankingApplied) -> Result<i64>;

    /// Banking records, newest-received first. Sealed rows are structurally
    /// absent, so no sealed filter is required.
    fn list_banking(&self, account_id: AccountId) -> Result<Vec<Banking>>;

    /// Upsert a sender rule. Returns the rule id.
    ///
    /// Rejects (`InvalidInput`) a `Filtered` rule with empty/whitespace
    /// `want_text`: the want text IS the rule's instruction, and without one the
    /// rule silently degrades. Same validation in
    /// [`Store::set_sender_rule_audited`] and [`Store::update_sender_rule`].
    fn set_sender_rule(
        &self,
        account_id: AccountId,
        match_pattern: &str,
        want_text: &str,
        disposition: Disposition,
    ) -> Result<i64>;

    /// AGENT-DOOR upsert: [`Store::set_sender_rule`] plus the audit row IN THE
    /// SAME TRANSACTION. FAIL-CLOSED — if the audit insert fails the rule write
    /// rolls back, because an agent write must never land untraced. Audit fields
    /// are written verbatim (the MCP door supplies actor="agent",
    /// action="rule.set").
    fn set_sender_rule_audited(
        &self,
        account_id: AccountId,
        match_pattern: &str,
        want_text: &str,
        disposition: Disposition,
        audit: &NewAuditEntry,
    ) -> Result<i64>;

    fn list_sender_rules(&self, account_id: AccountId) -> Result<Vec<SenderRule>>;

    /// The newest `limit` messages from `from_addr` with what became of each,
    /// for suggesting a rule about that sender. SEALED ROWS ARE EXCLUDED IN
    /// SQL: a sealed subject is as forbidden to a model as its body.
    fn sender_rule_digest(
        &self,
        account_id: AccountId,
        from_addr: &str,
        limit: usize,
    ) -> Result<Vec<SenderDigestRow>>;

    /// Overwrite an existing sender rule by id (scoped to `account_id`),
    /// restamping `updated_at`. `false` => unknown id => caller returns 404.
    fn update_sender_rule(
        &self,
        account_id: AccountId,
        id: i64,
        match_pattern: &str,
        want_text: &str,
        disposition: Disposition,
    ) -> Result<bool>;

    /// Store a message plus its triage (and any deadline) in ONE transaction.
    /// The ONLY ingest path the sync engine uses: a sealed classification commits
    /// with the row it seals, so there is no window in which a sealed message is
    /// queryable as normal mail (docs/SECURITY.md §4).
    fn ingest_message(&self, triaged: &TriagedMessage) -> Result<i64>;

    /// [`Store::ingest_message`] for a message the store ALREADY HOLDS, re-read
    /// with a body it was first stored without. Never inserts: a message the
    /// store does not hold, or holds as spam or sealed (a live re-check inside
    /// the transaction, so a verdict landing mid-sweep wins), is REFUSED with
    /// `Ok(None)` and nothing is written.
    ///
    /// The write is the ordinary ingest transaction — the message upsert
    /// replaces the body, and the agent bookkeeping at its end sees a changed
    /// content snapshot, advances the row's revision, retires any job still
    /// queued against the old content and puts external access back to
    /// `pending` — with three differences. The stale vector is dropped so the
    /// vector backfill re-embeds the row from its real text; the
    /// unsubscribe-violation ledger is NOT bumped again (the message was
    /// counted when it arrived, and the bump is a blind `+ 1`); and the job
    /// the new revision queues is chosen by `scope` rather than by sync
    /// origin: a `heal` investigation under [`HealScope::TextAndTriage`], an
    /// access reassessment alone under [`HealScope::Text`]. Neither is an
    /// arrival: the notify-eligibility stamp is preserved as stored, so a
    /// re-read of last year's mail cannot mint a notification.
    ///
    /// A PERSON'S DECISIONS SURVIVE: field corrections live in their own
    /// table keyed by message and are re-applied to whatever the next
    /// investigation commits, and the legacy `triage` row's verdict columns
    /// are kept by the upsert's own re-ingest guard. Nothing here touches
    /// either.
    fn ingest_message_fresh(
        &self,
        triaged: &TriagedMessage,
        scope: HealScope,
    ) -> Result<Option<i64>>;

    /// [`Store::ingest_message`] for the ARCHIVE WALK: mail older than the
    /// backfill window, stored to be found and for nothing else. INSERT-ONLY:
    /// a message the store already holds, live or archive, is left exactly as
    /// it is and the answer is `Ok(None)`.
    ///
    /// The row is marked `is_archive`, which is never changed afterwards (a
    /// later [`Store::ingest_message`] of the same id is a no-op that returns
    /// the stored id). It carries a neutral triage row with the same `n/a`
    /// stage markers sent mail has and no notify stamp, its text is indexed
    /// and its attachments stored, and its agent state is written at revision
    /// 1 with access pending and NO job: no model reads it until an agent-door
    /// caller explicitly surfaces it. Contacts, the unsubscribe ledger and
    /// timezone observations are not touched.
    fn ingest_archive_message(&self, triaged: &TriagedMessage) -> Result<Option<i64>>;

    /// The archive walk's cursor: the oldest instant the walk has fully
    /// covered, or `None` before its first chunk.
    fn archive_walked_to(&self, account_id: AccountId) -> Result<Option<DateTime<Utc>>>;

    /// Persist the archive walk's cursor after a fully ingested chunk. The
    /// call that creates the row also sets the forward edge to the same
    /// instant; no later call moves it.
    fn set_archive_walked_to(&self, account_id: AccountId, walked_to: DateTime<Utc>) -> Result<()>;

    /// The archive walk's FORWARD edge: the newest instant up to which mail
    /// carrying neither INBOX nor SENT has been stored, or `None` before the
    /// walk's first use.
    fn archive_covered_to(&self, account_id: AccountId) -> Result<Option<DateTime<Utc>>>;

    /// Move the forward edge after a fully ingested forward chunk. Writes
    /// nothing when the walk has no cursor row yet.
    fn set_archive_covered_to(
        &self,
        account_id: AccountId,
        covered_to: DateTime<Utc>,
    ) -> Result<()>;

    /// The three numbers the archive walk's disk guards read: see
    /// [`ArchiveDisk`]. A database that is not a file (in memory) has no disk
    /// to fill and answers [`ArchiveDisk::UNBOUNDED`]. An `Err` means the
    /// numbers could not be read, which the walk treats as "do not write".
    fn archive_disk(&self) -> Result<ArchiveDisk>;

    /// Whether the stored row `message_id` is an archive row. What a live
    /// ingest asks after [`Store::ingest_message`] hands back an id, to tell a
    /// row it wrote from an archive row it left alone.
    fn message_is_archive(&self, account_id: AccountId, message_id: i64) -> Result<bool>;

    /// Which of `gmail_msg_ids` this account already stores, in any tier. The
    /// walk lists a chunk and fetches only what this does not return, so a
    /// re-listed chunk costs no `messages.get`.
    fn stored_gmail_msg_ids(
        &self,
        account_id: AccountId,
        gmail_msg_ids: &[String],
    ) -> Result<std::collections::HashSet<String>>;

    /// True if `addr` appears in this account's Sent-derived contacts (the
    /// "people I know" signal the sync engine feeds to Stage-1).
    fn is_known_contact(&self, account_id: AccountId, addr: &str) -> Result<bool>;

    /// HUMAN-DOOR ONLY (`/client/contacts`): rank Sent-derived contacts for a
    /// typed fragment — recipient autocomplete. MUST NOT be reachable from MCP;
    /// the agent door never learns who the user writes to.
    fn search_contacts(
        &self,
        account_id: AccountId,
        q: &str,
        limit: u32,
    ) -> Result<Vec<ContactEntry>>;

    /// HUMAN-DOOR ONLY (`/client/calendar/people`): at most `limit` people
    /// THE USER HAS WRITTEN TO whose name starts with the typed words, the
    /// most recently exchanged-with first. The answer is offered as a
    /// possible invitee, so somebody who has only written TO the user is
    /// never in it, and neither is the account's own address. MUST NOT be
    /// reachable from MCP, the agent routes or the assistant relay: it is a
    /// lookup into who the user writes to.
    fn people_named(
        &self,
        account_id: AccountId,
        name: &str,
        limit: u32,
    ) -> Result<Vec<PersonNamed>>;

    /// Merge the Sent-history harvest's per-address aggregate (MAX semantics —
    /// idempotent across harvest re-runs and overlap with ingest seeding).
    fn merge_harvested_contacts(&self, account_id: AccountId, batch: &[ContactEntry])
    -> Result<()>;

    /// HUMAN-DOOR ONLY (`/client/senders`): rank the sender directory for a
    /// typed fragment — the search field's `from:` autocomplete. MUST NOT be
    /// reachable from MCP, for the reason `search_contacts` gives.
    fn search_senders(
        &self,
        account_id: AccountId,
        q: &str,
        limit: u32,
    ) -> Result<Vec<SenderEntry>>;

    /// HUMAN-DOOR ONLY (`/client/sender-domains`): the sender directory rolled
    /// up by host and ranked for a typed fragment of one, for the rule
    /// editor's `*@` suggestions. MUST NOT be reachable from MCP, like
    /// `search_senders`.
    fn search_sender_domains(
        &self,
        account_id: AccountId,
        q: &str,
        limit: u32,
    ) -> Result<Vec<SenderDomainEntry>>;

    /// Read the sync cursor for a mailbox key, if one has been persisted.
    fn sync_state(&self, account_id: AccountId, mailbox: &str) -> Result<Option<SyncState>>;

    /// Upsert the sync cursor for a mailbox key.
    fn set_sync_state(&self, account_id: AccountId, mailbox: &str, state: &SyncState)
    -> Result<()>;

    /// Which kind of mailbox the account is (`accounts.provider`).
    /// [`CoreError::NotFound`](crate::error::CoreError::NotFound) for an
    /// unknown account, and an error for a provider this build does not know:
    /// never a default, so an account a newer build set up is refused rather
    /// than synced as the wrong kind.
    fn account_provider(&self, account_id: AccountId) -> Result<crate::mail::MailProvider>;

    /// The sync engine's change-feed cursor for the account, opaque, or `None`
    /// before the first one is stored (a first run).
    ///
    /// FOR A GMAIL ACCOUNT THE LEGACY `sync_state` `'history'` ROW IS THE
    /// CURSOR, read exactly as the engine always read it, so an upgraded
    /// install resumes, a rolled-back build's progress is kept, and an
    /// operator's edit of it (the re-ingest drill in docs/SECURITY.md §1, a
    /// rewind, a zero) still works. See `store/sqlite/sync_cursor.rs`.
    fn sync_cursor(&self, account_id: AccountId) -> Result<Option<crate::mail::SyncCursor>>;

    /// Store the account's change-feed cursor. For a Gmail account the cursor
    /// must be a historyId (anything else is refused, nothing written), and
    /// the legacy `sync_state` `'history'` row is written in the same
    /// transaction, so a build from before `sync_cursor` resumes from the same
    /// point. [`CoreError::NotFound`](crate::error::CoreError::NotFound) for an
    /// unknown account.
    fn set_sync_cursor(
        &self,
        account_id: AccountId,
        cursor: &crate::mail::SyncCursor,
    ) -> Result<()>;

    /// Whether the account already stores the message the provider calls
    /// `provider_msg_id`, in any tier.
    ///
    /// UNWIRED ON PURPOSE. When a source wires it, it goes on the INCREMENTAL
    /// path only: the backfill and catch-up re-fetch mail already stored, and
    /// that re-fetch is how a sanitizer change reaches `body_html`, which is
    /// baked at ingest.
    fn has_message(&self, account_id: AccountId, provider_msg_id: &str) -> Result<bool>;

    /// The last Gmail INBOX unread counts the sync loop stored, or `None` when
    /// none were ever fetched (old DB, or every fetch so far has failed). `None`
    /// is NOT zero and callers must keep the distinction: zero unread is a real
    /// answer, "we do not know" is not.
    fn inbox_unread(&self, account_id: AccountId) -> Result<Option<InboxUnread>>;

    /// When this account was first created in the store, or `None` for an
    /// unknown id or an unparseable stamp. The triage agent's onboarding
    /// window counts from it.
    fn account_created_at(&self, account_id: AccountId) -> Result<Option<DateTime<Utc>>>;

    /// The account owner's zone: the most recently used app's device zone,
    /// else the primary calendar's, else UTC. Every agent reads "now" in it.
    /// See [`SqliteStore::owner_zone`](sqlite::SqliteStore::owner_zone).
    fn owner_zone(&self, account_id: AccountId) -> Result<crate::owner_clock::OwnerZone>;

    /// Overwrite this account's Gmail INBOX unread counts, stamping `fetched_at`
    /// now. Only ever called with numbers that came back from Gmail, so a failed
    /// fetch leaves the previous row in place.
    fn set_inbox_unread(&self, account_id: AccountId, messages: i64, threads: i64) -> Result<()>;

    /// The account's Google name and photo tag, or `None` when the profile was
    /// never fetched. `None` is "we do not know", not "this person has no
    /// name": clients fall back to what they showed before.
    fn account_identity(&self, account_id: AccountId) -> Result<Option<AccountIdentity>>;

    /// The stored profile photo, or `None` when there is none to serve.
    fn account_photo(&self, account_id: AccountId) -> Result<Option<AccountPhoto>>;

    /// Overwrite the account's profile whole. Only ever called with an answer
    /// that came back from Google, so a failed fetch leaves the previous row
    /// standing; a `None` photo CLEARS the stored one, which is how a removed
    /// picture stops being served.
    fn set_account_profile(
        &self,
        account_id: AccountId,
        name: Option<&str>,
        given_name: Option<&str>,
        photo: Option<&AccountPhoto>,
    ) -> Result<()>;

    /// LOCAL-ONLY (TUI): list sealed messages. This is the ONLY method that
    /// exposes sealed content and must never be reachable from MCP.
    fn sealed_messages(&self, account_id: AccountId) -> Result<Vec<SealedMessage>>;

    // HUMAN-DOOR additions (squelch-api /client/*). These MUST NOT be called
    // from MCP, sync, or triage. `search` still excludes sealed rows; the
    // sealed_* / audit methods are the human door's privileged surface.

    /// SEEN-LEDGER stamp, in ONE transaction: for each non-sealed message id set
    /// `surfaced_at=now` only if currently NULL, and promote `status`
    /// `new`->`open`. Sealed rows are guarded out in SQL. Returns the
    /// first-surface count (rows whose `surfaced_at` transitioned from NULL).
    fn mark_surfaced(&self, account_id: AccountId, message_ids: &[i64]) -> Result<usize>;

    /// THE OTHER STAMP: for each non-sealed message id set `opened_at=now` only
    /// if currently NULL. Called ONLY where a message BODY is served, which is
    /// what makes it mean "the user opened this" rather than "we showed this".
    /// Returns the first-open count.
    fn mark_opened(&self, account_id: AccountId, message_ids: &[i64]) -> Result<usize>;

    /// The same, for every message in one thread. What the human door's
    /// "I opened this" call goes through: the reader shows a whole thread, and
    /// the client should not have to hand back a list of ids the daemon gave
    /// it.
    fn mark_thread_opened(&self, account_id: AccountId, thread_id: &str) -> Result<usize>;

    /// Received vs opened over the window since `since`, for the one caller
    /// that puts a number in front of a human. See the implementation for what
    /// each side counts and, more importantly, what `opened` cannot see.
    fn share_open_rate(&self, account_id: AccountId, since: DateTime<Utc>) -> Result<OpenRate>;

    /// Set the attention status of one message's triage row. `Done` stamps
    /// `resolved_at=now`; `Open`/`New` clear it. Sealed rows are excluded in SQL,
    /// so missing and sealed both return `false`.
    fn set_attention_status(
        &self,
        account_id: AccountId,
        message_id: i64,
        status: AttentionStatus,
    ) -> Result<bool>;

    /// "Remind me about this at `remind_at`." Stamps the reminder on this
    /// message and marks the whole THREAD done, in one transaction — deferring
    /// mail is resolving it until the date arrives, and a thread that stayed
    /// half-open would keep showing the user what they just put off. Any
    /// previously fired reminder on the row is cleared. Sealed rows are excluded
    /// in SQL, so missing and sealed both return `false`.
    fn set_reminder(
        &self,
        account_id: AccountId,
        message_id: i64,
        remind_at: DateTime<Utc>,
    ) -> Result<bool>;

    /// Drop a pending reminder. Status, `resolved_at` and any already-fired
    /// `reminded_at` are left alone — this un-schedules, it does not undo. A row
    /// with no reminder is a successful no-op (idempotent), so `false` means only
    /// missing or sealed.
    fn clear_reminder(&self, account_id: AccountId, message_id: i64) -> Result<bool>;

    /// Fire every reminder due at `now`: status back to `open`, `resolved_at`
    /// cleared, and the stamp MOVED from `remind_at` to `reminded_at`. Returns
    /// the message ids moved. Idempotent by construction — a fired row no longer
    /// matches — and sealed rows are excluded in SQL. Called from the sync poll
    /// loop; nothing else should fire reminders.
    fn fire_due_reminders(&self, account_id: AccountId, now: DateTime<Utc>) -> Result<Vec<i64>>;

    /// THE PAST-EVENT RE-CHECK (docs/CALENDAR.md §3.8): queue a triage
    /// revisit (`revisit:event_ended:<ask time>`) for each message about a
    /// trip, ticket or booking whose ask time, a day after the latest date
    /// the model gave for it, has come, so the agent can decide whether it is
    /// done. Returns the message ids queued. Schedules only: whether mail is
    /// done is the model's answer, past the rails in the decision commit.
    /// Honours `policy` (off) and its own cap of three asks a message, and
    /// skips a message with a revisit already queued or running. Called
    /// from the sync poll loop.
    fn schedule_past_event_revisits(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
        policy: &crate::config::RevisitPassConfig,
    ) -> Result<Vec<i64>>;

    /// Resolve EVERY still-open triage row from one sender address, returning how
    /// many moved. Case-insensitive on `from_addr`; sealed rows excluded in SQL,
    /// exactly as `set_attention_status`.
    ///
    /// For the two actions that are verdicts on a SENDER rather than on a
    /// message — unsubscribing, and a squelch rule. Resolving only the thread
    /// the reader happened to be looking at leaves the rest of that sender's
    /// mail sitting in the bands, which is indistinguishable from the action not
    /// having worked. Never call this for a per-message action.
    fn resolve_sender(&self, account_id: AccountId, sender_addr: &str) -> Result<usize>;

    /// FTS5 keyword search over non-sealed messages. `limit`/`offset` paginate.
    ///
    /// Ranked by relevance TILTED TOWARD RECENT MAIL ([`recency`]) — the same
    /// curve the semantic and hybrid legs blend in, so one query does not mean
    /// two different things depending on which mode served it. Use
    /// [`Store::search_filtered`] to rank on relevance alone.
    ///
    /// SECURITY: sealed rows are excluded in SQL, exactly like `thread_view`.
    fn search(
        &self,
        account_id: AccountId,
        query: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SearchHit>>;

    /// [`Store::search`] with the operator half of the query applied ([`from:`,
    /// `after:`, `before:`](search_query)). `text` is the already-parsed search
    /// text — this method never sees a raw query string, so the operators are
    /// parsed exactly once, at the door.
    ///
    /// When `text` is empty and `filter` is not, this is a FILTER-ONLY LISTING:
    /// newest-first over `messages` with no FTS MATCH at all. Both shapes keep
    /// `search`'s guarantees — sealed rows excluded, sent mail excluded.
    ///
    /// `sort` decides whether the recency curve applies. It is the READER'S
    /// standing preference rather than part of the query, which is why it
    /// arrives beside `filter` instead of inside it: a filter says which mail
    /// counts, a sort says how to order whatever did.
    /// `partial` matches the LAST word as a prefix, which is what an
    /// as-you-type fetch wants and what a settled query must not have: an agent
    /// sending finished words would otherwise have `password` widened to
    /// `password*` and rank `passwordless` beside it.
    #[allow(clippy::too_many_arguments)] // the query, the operators, the order, the page
    fn search_filtered(
        &self,
        account_id: AccountId,
        text: &str,
        filter: &SearchFilter,
        sort: SearchSort,
        partial: bool,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SearchHit>>;

    /// Delete a sender rule by id (scoped to `account_id`). Returns whether a
    /// row was removed.
    fn delete_sender_rule(&self, account_id: AccountId, id: i64) -> Result<bool>;

    /// HUMAN-DOOR-ONLY: the full body of exactly one sealed message. Reachable
    /// only from the squelch-api reveal endpoint, which appends the audit row
    /// BEFORE calling this and answers `no-store`. `NotFound` when the message
    /// does not exist or is not sealed.
    fn sealed_body(&self, account_id: AccountId, message_id: i64) -> Result<SealedBody>;

    /// HUMAN-DOOR-ONLY (`PUT /client/sealed/{id}/verdict`): record the owner's
    /// ruling on one auth message, or clear it with `None`. `NotFound` when the
    /// message does not exist or is not auth mail (the membership
    /// [`Store::sealed_body`] uses). A legacy sealed row (no agent decision)
    /// takes a ruling but never moves FYE. MUST NOT be reachable from MCP.
    fn set_auth_verdict(
        &self,
        account_id: AccountId,
        message_id: i64,
        verdict: Option<crate::types::AuthVerdict>,
        now: DateTime<Utc>,
    ) -> Result<()>;

    /// HUMAN-DOOR-ONLY (`GET /client/sent`): the user's own sent mail, newest
    /// first (`received_at DESC, id DESC`), `limit`/`offset` paginating exactly
    /// as [`Store::search`] does. MUST NOT be reachable from MCP — the agent
    /// door has no sent surface at all.
    ///
    /// SECURITY: this is the one listing that reads `is_sent = 1`, so its sealed
    /// guard FAILS CLOSED — the triage join is an INNER join AND requires
    /// `sensitivity != 'sealed'`, which excludes a sent row missing its triage
    /// row rather than defaulting it to visible.
    fn sent_listing(
        &self,
        account_id: AccountId,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SentMessage>>;

    /// Up to `limit` messages whose headers have never been read for recipients,
    /// newest-first: the queue for the one-shot recipients backfill. Sent AND
    /// received, because the reader shows To and Cc on both.
    fn messages_missing_recipients(
        &self,
        account_id: AccountId,
        limit: u32,
    ) -> Result<Vec<MessageMissingRecipients>>;

    /// One chunk of the blank-body heal's queue: among the `limit` newest
    /// inbound, non-spam, non-sealed messages with `id < before_id` that carry
    /// a `body_html`, the ones whose stored text body is blank to a reader —
    /// mail ingested before the body selection learned to flatten the HTML
    /// alternative when the text/plain one is empty. Chunked so the store lock
    /// is held per chunk, not per mailbox, and cursor-driven so an interrupted
    /// sweep resumes where it stopped instead of re-reading everything.
    ///
    /// Blankness is decided by [`crate::triage::text::is_blank`] in Rust — the
    /// function ingest and the extractor use — never by a SQL re-expression of
    /// it: SQLite's one-argument `TRIM` strips U+0020 and nothing else, and an
    /// "empty" text/plain part on the wire is a bare CRLF. Sent mail is left
    /// out: squelch's own composer always writes a text part, and a sent row
    /// re-enters no queue, so a re-read would buy nothing but a raw fetch.
    fn blank_body_messages(
        &self,
        account_id: AccountId,
        before_id: i64,
        limit: u32,
    ) -> Result<BlankBodyScan>;

    /// Set one message's display recipients, per header; `false` when no such row
    /// exists. `""` is a legitimate value — it records that the header was read
    /// and named nobody, which is what takes the row out of the backfill queue.
    ///
    /// `bcc_addrs` is stored ONLY on sent mail; on a received row the
    /// implementation writes NULL whatever is passed, because a `Bcc:` header
    /// there is the sender's leak or a forgery rather than "who this went to".
    fn set_message_recipients(
        &self,
        account_id: AccountId,
        message_id: i64,
        to_addrs: &str,
        cc_addrs: &str,
        bcc_addrs: &str,
    ) -> Result<bool>;

    /// Append a row to the human-door audit log. Returns the new row id.
    fn append_audit(&self, account_id: AccountId, entry: &NewAuditEntry) -> Result<i64>;

    // NOTIFICATION EVENTS: the durable, monotonic log the delivery adapters read.
    // WRITTEN ONLY BY THE SYNC ENGINE via `triage::events` — never from a store
    // write method, because only the engine knows which sync path an ingest is
    // on, and that answer (not the sender's `Date:` header) is what decides
    // whether a message may ever notify. The engine writes it down once, as
    // `triage.notify_eligible_at`; see `TriagedMessage::notify_eligible_at`.
    // Every reader carries its OWN cursor; there is deliberately no global
    // 'delivered' flag.

    /// Append one event, at most once per message ever (INSERT OR IGNORE on
    /// `UNIQUE(message_id)`), which is what makes re-ingest and the refine passes
    /// idempotent. `None` when the message already has an event.
    ///
    /// A real insert also pokes the in-process broadcast (see
    /// [`SqliteStore::attach_event_notifier`]) so an SSE reader wakes without
    /// polling; that send is best-effort, no receivers is normal.
    fn append_event(&self, ev: &NewEvent) -> Result<Option<i64>>;

    /// Current explicit user state still permits delivering this logical event.
    /// Dispatchers recheck after queue waits/retries and advance suppressed events.
    fn notification_delivery_allowed(&self, account_id: AccountId, event_id: i64) -> Result<bool>;

    /// Whether this message already has an `events` row: "has the user already
    /// been notified about this one".
    ///
    /// EXISTS FOR THE EXPIRY COUNTER. `UNIQUE(message_id)` means a site that
    /// APPENDS can already tell a new buzz from a repeat one by the `Option` it
    /// gets back — but a site that REFUSES to append never reaches the store, so
    /// it cannot, and the one refusal we count (past the rescue window) is
    /// exactly the one that keeps coming back: a message notified from Stage-1
    /// is offered again by Stage-2 and again by every re-triage, hours later
    /// each time. Without this read, every one of those books a DELIVERED
    /// notification as a missed one, in the single number docs/NOTIFY.md §11.11
    /// says decides whether the window moves. See
    /// [`crate::sync::SyncEngine::emit_event`].
    fn message_has_event(&self, account_id: AccountId, message_id: i64) -> Result<bool>;

    /// Events with `id > after_id`, oldest first, capped at `limit`. The replay
    /// query behind `GET /client/events?after=<cursor>`.
    fn events_after(
        &self,
        account_id: AccountId,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<Event>>;

    /// One event by id, scoped to the account. `None` for an unknown id — this
    /// is what the iOS Notification Service Extension calls after an opaque
    /// push, so it must never be more informative than that.
    fn event_by_id(&self, account_id: AccountId, id: i64) -> Result<Option<Event>>;

    // THE NOTIFY DECISION LEDGER (docs/NOTIFY.md §11.4): one row per (message,
    // lane), APPEND-ONLY, carrying no email-derived text. It is the labeled
    // corpus that decides whether the threshold or the model moves; a row that
    // could be rewritten would be evidence of nothing.

    /// Append a bounded model assessment before recording its delivery outcome.
    /// The account must own the message; failures must leave the durable job retryable.
    fn record_notification_assessment(
        &self,
        account_id: AccountId,
        message_id: i64,
        lane: NotifyLane,
        assessment: &NotificationAssessment,
    ) -> Result<()>;

    /// Human diagnostics only; assessment text must never enter external agent responses.
    fn latest_notification_assessment(
        &self,
        account_id: AccountId,
        message_id: i64,
        lane: NotifyLane,
    ) -> Result<Option<NotificationAssessment>>;

    /// Record one lane's decision about one message. `false` when a row for that
    /// (message, lane) already existed and this call was IGNORED.
    ///
    /// INSERT OR IGNORE on `UNIQUE(message_id, lane)` is the append-only rule
    /// made structural: a Stage-2 pass over a row Stage-1 already recorded, or a
    /// human re-triage hours later, is a silent no-op rather than a rewrite of
    /// what that lane decided the first time it reached the message. Callers
    /// record only for messages carrying a `triage.notify_eligible_at` stamp, so
    /// backfill and already-stale mail produce no rows at all.
    fn record_notify_decision(&self, decision: &NewNotifyDecision) -> Result<bool>;

    /// Whether `lane` has ALREADY recorded a decision about this message.
    ///
    /// THE FAST LANE'S RE-ENTRY GATE, and it exists because `INSERT OR IGNORE`
    /// is not one. Re-ingest is routine (`catch_up` re-walks the whole window,
    /// and a failed SENT half holds the cursor so the next tick re-walks the
    /// same INBOX ids), and `notify_lane::candidate` re-fires on any re-ingested
    /// row whose stamp is still inside the freshness window. Without this read
    /// the second run makes a second PAID model call, charges a second
    /// `__notify_fast__` unit, and — if it scores above the line where the first
    /// run scored below it — appends the `events` row while the ignored ledger
    /// insert leaves the table still saying `declined_by_model`. A decision
    /// silently contradicted by the world is exactly what docs/NOTIFY.md
    /// §11.11's rescued/overturned joins cannot survive.
    ///
    /// HALF A GUARD, NEVER THE WHOLE ONE. This is a TOCTOU read: the row it
    /// looks for is written LAST, after the model call, so two runs that OVERLAP
    /// both see `false` here. `NotifyLane` pairs it with an in-process in-flight
    /// set that covers exactly that window; this read covers what the set cannot,
    /// which is a re-ingest on the other side of a restart. Neither is
    /// sufficient alone, and a change to one wants a look at the other.
    fn notify_decision_exists(
        &self,
        account_id: AccountId,
        message_id: i64,
        lane: NotifyLane,
    ) -> Result<bool>;

    /// This account's ledger rows created at or after `since`, oldest first,
    /// capped at `limit`. The eval read behind docs/NOTIFY.md §11.11.
    ///
    /// ORDER BY id, not `created_at`: two decisions inside the same clock tick
    /// are common (the fast lane records at ingest while a refine pass is
    /// recording another message), and the insert order is the only total one.
    /// A row whose stored lane or decision this build does not recognize is
    /// skipped rather than coerced.
    fn notify_decisions_since(
        &self,
        account_id: AccountId,
        since: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<NotifyDecisionRow>>;

    /// The newest event id for the account, or `0` when there are none — the
    /// starting cursor for a client that has never connected.
    fn latest_event_id(&self, account_id: AccountId) -> Result<i64>;

    // REGISTERED PUSH DEVICES. Written by the human door, read by the APNs
    // pusher, invisible to the agent door.

    /// Register a device token, or refresh an already-known one. IDEMPOTENT
    /// (UPSERT on `UNIQUE(token)`): iOS re-hands its token each launch, so a
    /// re-register updates `last_registered_at` rather than forking a row.
    ///
    /// A token registered to ANOTHER account is refused with
    /// [`CoreError::InvalidInput`] and nothing is written — re-registration must
    /// never silently repoint a device's pushes at a different account.
    ///
    /// `tag` is overwritten on every re-registration, including back to `None`:
    /// it describes where this device currently files the account, and a stale
    /// one would route a push at a mailbox the phone has since forgotten.
    ///
    /// `environment` is the OPPOSITE: `None` keeps whatever is stored. A token's
    /// APNs environment never changes, so an older build re-registering the same
    /// token without the field must not erase what a newer build declared. A
    /// `Some` replaces it.
    ///
    /// CAPPED at `MAX_DEVICES_PER_ACCOUNT` (16) rows per account, because each
    /// distinct (tag, environment) costs a relay request per event against a
    /// rate limit the hosted fleet shares. Going over EVICTS the account's
    /// least recently registered rows (never the one just upserted) in the
    /// same transaction; it does not refuse, since stale and rejected rows
    /// would otherwise lock a real phone out. Other accounts are untouched.
    fn upsert_device(
        &self,
        account_id: AccountId,
        token: &str,
        platform: &str,
        tag: Option<&str>,
        environment: Option<&str>,
    ) -> Result<Device>;

    /// Every registered device for the account, oldest first — a stable order, so
    /// a push fan-out and its response array line up reproducibly.
    fn list_devices(&self, account_id: AccountId) -> Result<Vec<Device>>;

    /// Drop one device by token, scoped to the account. Two callers: the human
    /// door's DELETE, and the pusher on APNs `410 Unregistered`.
    fn delete_device_by_token(&self, account_id: AccountId, token: &str) -> Result<bool>;

    // UNSUBSCRIBE (human door). All four are `/client/*`-only, and sealed mail is
    // invisible to them.

    /// The stored unsubscribe fields for a NON-SEALED message. `None` for a
    /// missing OR sealed message (indistinguishable), which the caller maps to
    /// 404.
    fn message_unsub_fields(
        &self,
        account_id: AccountId,
        message_id: i64,
    ) -> Result<Option<MessageUnsub>>;

    /// DEV DEBUG: the full triage row for one NON-SEALED message. `None` for
    /// missing OR sealed (indistinguishable). Human door only.
    fn triage_debug(&self, account_id: AccountId, message_id: i64) -> Result<Option<TriageDebug>>;

    /// Upsert the unsubscribe row for `(account_id, sender)`. On conflict this
    /// RESETS the violation ledger (`violation_count -> 0`, `last_violation_at`
    /// and `resolution -> NULL`): the user re-asked, so the 72h grace clock
    /// restarts. `sender` is stored verbatim — the caller lowercases it.
    fn upsert_unsubscribe(
        &self,
        account_id: AccountId,
        sender: &str,
        method: &str,
        source_message_id: Option<i64>,
        requested_at: DateTime<Utc>,
    ) -> Result<()>;

    /// List the account's unsubscribe records, newest `requested_at` first.
    fn list_unsubscribes(&self, account_id: AccountId) -> Result<Vec<UnsubscribeRecord>>;

    /// Start an AUTOMATIC unsubscribe for `sender`: the row becomes
    /// `method = 'auto'`, `status = 'running'`, with `url` kept as the page a
    /// `needs_you` outcome will send the user to. Resets the violation ledger
    /// exactly as [`Store::upsert_unsubscribe`] does.
    fn begin_unsubscribe_job(
        &self,
        account_id: AccountId,
        sender: &str,
        source_message_id: Option<i64>,
        url: &str,
        now: DateTime<Utc>,
    ) -> Result<()>;

    /// Settle a RUNNING job. `status` is `done` or `needs_you`; `method` says how
    /// it went out (`one_click` | `page`) and is kept as `auto` when it did not.
    /// `done` moves `requested_at` to `now`, so the violation grace runs from the
    /// moment the sender was actually asked. A row that is no longer running
    /// (the user re-asked through the browser meanwhile) is left alone.
    fn finish_unsubscribe_job(
        &self,
        account_id: AccountId,
        sender: &str,
        status: &str,
        method: &str,
        reason: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<()>;

    /// The user followed a `needs_you` row's link to finish on the sender's
    /// site: `status -> opened`, `requested_at -> now`, which arms the violation
    /// detector. `false` when the sender has no `needs_you` row.
    fn mark_unsubscribe_opened(
        &self,
        account_id: AccountId,
        sender: &str,
        now: DateTime<Utc>,
    ) -> Result<bool>;

    /// Turn every `running` row whose sender is NOT in `live` and whose
    /// `status_at` is before `stale_before` into `needs_you` with reason
    /// `unclear`: the job that owned it died with the process. Its `status_url`
    /// is still the original link, so the user can finish by hand. Returns how
    /// many rows were reaped.
    fn reap_unsubscribe_jobs(
        &self,
        account_id: AccountId,
        live: &[String],
        stale_before: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<usize>;

    /// Set the `resolution` (blocked|dismissed) on an existing unsubscribe row,
    /// disarming the violation detector for that sender. `false` when no row
    /// exists (=> caller returns 404).
    fn set_unsubscribe_resolution(
        &self,
        account_id: AccountId,
        sender: &str,
        resolution: &str,
    ) -> Result<bool>;

    /// What is remembered about a smart block for this sender's ignored
    /// unsubscribe, so the question is put to a model once. `None` when the
    /// sender has no unsubscribe row at all.
    fn unsubscribe_smart_block(
        &self,
        account_id: AccountId,
        sender: &str,
    ) -> Result<Option<SmartBlockMemo>>;

    /// Remember the answer on the sender's unsubscribe row: a proposal, or
    /// `None` for "nothing worth keeping apart". `false` when there is no row.
    /// A proposal is model text about sender-written subjects: it is stored to
    /// be SHOWN again and becomes a rule only through the owner's click. It
    /// outlives a resolution and a fresh request, being a reading of the
    /// sender; a remembered `None` is dropped by a fresh request, so asking
    /// again gets a second look at a sender whose mail may have changed.
    fn set_unsubscribe_smart_block(
        &self,
        account_id: AccountId,
        sender: &str,
        proposal: Option<&SmartBlock>,
    ) -> Result<bool>;

    /// Upsert one extracted promotion + stamp `triage.extractor_model_used`.
    /// Unlike [`Store::banking_apply`] this does NOT resolve the triage row.
    fn marketing_apply(&self, applied: &MarketingApplied) -> Result<()>;

    /// Extracted promotions, newest first, received within the last `days`,
    /// capped at `limit`. Structurally sealed-free.
    fn marketing_offers(
        &self,
        account_id: AccountId,
        days: u32,
        limit: u32,
    ) -> Result<Vec<MarketingOffer>>;

    /// Apply a human's triage correction and record it as feedback in ONE
    /// transaction — the training row and the state it describes must never
    /// disagree.
    ///
    /// Sets the triage row's `axis` column to `to_value` and stamps the row
    /// human-decided (`stage1_model_used`/`model_used` = 'human',
    /// `needs_stage2` = 0) so no later LLM pass can silently overwrite the human,
    /// then appends a `triage_feedback` row with the full prior snapshot.
    ///
    /// `None` when the message does not exist for this account. The caller
    /// validates `to_value` against [`TriageAxis::allowed`] first.
    fn correct_triage(
        &self,
        account_id: AccountId,
        message_id: i64,
        axis: TriageAxis,
        to_value: &str,
        note: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Option<TriageFeedback>>;

    /// Recorded corrections, newest first, capped at `limit` — the refinement
    /// dataset.
    fn list_triage_feedback(
        &self,
        account_id: AccountId,
        limit: u32,
    ) -> Result<Vec<TriageFeedback>>;

    // AUTH-MAIL SHREDDER (retention). Human-door-only; see the `shred_log` block
    // in schema.sql for the policy.

    /// Currently assessed actionable auth (restricted codes, resets, sign-in or
    /// verification links) received at or before `cutoff`, not yet in `shred_log`.
    /// Legacy sensitivity and login/security alerts do not qualify. Oldest first,
    /// capped at `limit`. Rows
    /// without a `gmail_msg_id` are skipped — the trash call has nothing to
    /// address.
    fn shred_candidates(
        &self,
        account_id: AccountId,
        cutoff: DateTime<Utc>,
        limit: u32,
    ) -> Result<Vec<ShredCandidate>>;

    /// How many candidates [`Store::shred_candidates`] would return, unbounded.
    /// Drives the "N waiting" figure without materializing the rows.
    fn shred_pending_count(&self, account_id: AccountId, cutoff: DateTime<Utc>) -> Result<i64>;

    /// Record one shredded message. Called ONLY after Gmail confirms the trash,
    /// so the ledger never claims a deletion that did not happen;
    /// `UNIQUE(account_id, message_id)` makes a re-run a no-op.
    fn record_shred(
        &self,
        account_id: AccountId,
        candidate: &ShredCandidate,
        shredded_at: DateTime<Utc>,
    ) -> Result<()>;

    /// Ledger totals for the Auth page: shreds since `recent_since`, the
    /// all-time count, and the most recent shred time.
    fn shred_counts(
        &self,
        account_id: AccountId,
        recent_since: DateTime<Utc>,
    ) -> Result<(i64, i64, Option<DateTime<Utc>>)>;

    /// The most recent audit rows, newest first, capped at `limit`. A row gains
    /// `target_sender`/`target_subject` when its `target` parses as a message id
    /// that exists for the account.
    fn list_audit(&self, account_id: AccountId, limit: u32) -> Result<Vec<AuditEntry>>;

    /// Per-tier / sealed / sync-cursor summary counts for the account. The
    /// band counts are windowed to `bands_since` so they agree with the list
    /// queries they head; the inventory counts are all-time.
    fn stats(&self, account_id: AccountId, bands_since: DateTime<Utc>) -> Result<StoreStats>;

    /// The mailbox's own traffic per UTC day over `[since, until)` on
    /// `received_at`: mail in, mail out, and how the incoming split by tier.
    /// Ascending and SPARSE — a day with no mail has no row. Spam is never
    /// counted: it is fetched on demand, so a day's spam total would report how
    /// recently the spam page was opened rather than what arrived.
    fn mail_activity(
        &self,
        account_id: AccountId,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Result<Vec<MailActivityDay>>;

    // STAGE-2 additions, supporting the LLM triage pass in the sync loop. The
    // queue predicate is `model_used IS NULL AND sensitivity='normal'`, so sealed
    // rows are structurally excluded.

    /// Apply a parsed Stage-1 LLM result onto a triage row IN ONE TRANSACTION:
    /// overwrite importance/tier/one_line/reason/field_reasons, stamp
    /// `stage1_model_used` (leaving the Stage-1 queue), set `needs_stage2`, and
    /// (re)write the `deadlines` row. Guarded by `sensitivity='normal'`; never
    /// touches `model_used` (the Stage-2 marker).
    ///
    /// `false` (the guarded UPDATE matched nothing) means the row was sealed
    /// between the queue SELECT and this apply: nothing was written, not even the
    /// deadline, and the caller must treat the verdict as NOT landed — in
    /// particular, no notification event.
    fn stage1_apply(&self, applied: &Stage1Applied) -> Result<bool>;

    /// `triage.notify_eligible_at` as the STORE holds it, for one message.
    /// `None` when the row is missing or was never stamped.
    ///
    /// NOT GATED ON `sensitivity`: a sealed row still has a stamp, and the fast
    /// lane's sealed path needs the stored
    /// stamp for exactly the reason its model path does. `ingest_message` writes
    /// the column on FIRST INSERT ONLY and preserves it on conflict, so the
    /// value ingest recomputed on a re-ingest is not the value that decides the
    /// rescue window (docs/NOTIFY.md §11.3).
    fn notify_eligible_at(
        &self,
        account_id: AccountId,
        message_id: i64,
    ) -> Result<Option<DateTime<Utc>>>;

    /// Bump the Stage-1 usage ledger for `(account_id, day)`: +1 call plus the
    /// response's token counts. Kept separate from Stage-2's ledger.
    fn stage1_bump_usage(
        &self,
        account_id: AccountId,
        day: &str,
        tokens: UsageTokens,
    ) -> Result<()>;

    /// Sum the Stage-1 usage ledger over every day `>= since_day`. Drives the
    /// trailing-window Stage-1 averages on `/client/triage-config`.
    fn stage1_usage_since(&self, account_id: AccountId, since_day: &str) -> Result<Stage2Usage>;

    /// Sum ONE usage-ledger category over every day `>= since_day`. The
    /// translate pass reads today's `translate` line to settle its dollar
    /// budget from what the provider actually billed.
    fn category_usage_since(
        &self,
        account_id: AccountId,
        since_day: &str,
        category: &str,
    ) -> Result<Stage2Usage>;

    /// Bump a SPECIALIST-EXTRACTOR usage line for `(account_id, day, category)`.
    /// `category` is the extractor's own ledger label (e.g. `extract_banking`),
    /// kept separate from `stage1`/`stage2` so per-specialist cost stays visible.
    #[allow(clippy::too_many_arguments)] // the parts of one usage ledger line
    fn extract_bump_usage(
        &self,
        account_id: AccountId,
        day: &str,
        category: &str,
        tokens: UsageTokens,
    ) -> Result<()>;

    /// Stage-1 usage history: the most recent `days` rows, newest-first (sparse).
    fn list_usage_stage1(&self, account_id: AccountId, days: u32) -> Result<Vec<Stage2UsageDay>>;

    /// EVERY ledger category with its history, sorted by category name.
    ///
    /// The ledger is open-ended — each extractor writes its own category — so a
    /// caller that names the categories it wants silently drops every one added
    /// after it was written. That is exactly how extractor spend went unreported
    /// while it accrued daily. Enumerate, don't enumerate-by-hand.
    fn list_usage_by_category(
        &self,
        account_id: AccountId,
        days: u32,
    ) -> Result<Vec<(String, Vec<Stage2UsageDay>)>>;

    /// Today's Stage-2 API-call count for a budget scope: `thread_id` is either a
    /// real Gmail thread id (per-thread cap) or the `'__global__'` sentinel
    /// (per-account cap). `day` is caller-provided so tests are deterministic.
    fn stage2_budget_used(&self, account_id: AccountId, thread_id: &str, day: &str) -> Result<u32>;

    /// Increment (and return the new value of) today's Stage-2 API-call count
    /// for a budget scope. Called BEFORE the API attempt so retries count and
    /// cannot exceed the cap. Upserts the `wake_budget` row.
    fn stage2_increment_budget(
        &self,
        account_id: AccountId,
        thread_id: &str,
        day: &str,
    ) -> Result<u32>;

    /// Give back one charge from [`Store::stage2_increment_budget`], for a call
    /// that was charged and then rejected at CONFIG level.
    ///
    /// Charging before the call is what stops a retry storm exceeding the cap,
    /// and that stays. But a config-level rejection (see
    /// [`crate::triage::llm::is_config_failure`]) is a 4xx in ~0ms that spends
    /// no tokens and no money — and because it is shared by every queued row,
    /// one broken config can charge the whole day's cap in minutes. That is not
    /// a hypothetical: on 2026-08-25 a hosted tenant's gateway misconfiguration
    /// burned 498 of a 500-call daily cap, and because the day key is UTC, the
    /// fleet stayed capped for 22 hours AFTER the gateway was fixed. Fixing an
    /// outage has to restore service, not schedule it for tomorrow.
    ///
    /// Row-level permanent failures (`json_parse`, `max_tokens_truncation`)
    /// deliberately keep their charge: the model ran and was paid for.
    fn stage2_refund_budget(&self, account_id: AccountId, thread_id: &str, day: &str)
    -> Result<()>;

    /// Bump the Stage-2 usage ledger for `(account_id, day)`: +1 call plus the
    /// response's token counts, after each classify that carried a usage block.
    fn stage2_bump_usage(
        &self,
        account_id: AccountId,
        day: &str,
        tokens: UsageTokens,
    ) -> Result<()>;

    /// Read the Stage-2 usage totals for `(account_id, day)`. Returns a zeroed
    /// [`Stage2Usage`] when no row exists for that day.
    fn stage2_usage_today(&self, account_id: AccountId, day: &str) -> Result<Stage2Usage>;

    /// Stage-2 usage history: the most recent `days` rows, newest-first and
    /// sparse (only days that have a row; no zero-filling).
    fn list_usage(&self, account_id: AccountId, days: u32) -> Result<Vec<Stage2UsageDay>>;

    /// Sum the Stage-2 usage ledger over every day `>= since_day` (a `YYYY-MM-DD`
    /// UTC key — lexical compare is correct for that format). Drives the
    /// trailing-window averages on `/client/triage-config`.
    fn stage2_usage_since(&self, account_id: AccountId, since_day: &str) -> Result<Stage2Usage>;

    // RUNTIME APP SETTINGS (human door): a per-account key/value store for knobs
    // a client can change without a restart. The Stage-2 pass re-reads the
    // overrides at the START of each cycle. Precedence: override > config/env >
    // default.

    /// Read one `app_settings` value for `(account_id, key)`, or `None` if unset.
    fn get_app_setting(&self, account_id: AccountId, key: &str) -> Result<Option<String>>;

    /// Upsert one `app_settings` value for `(account_id, key)`.
    fn set_app_setting(&self, account_id: AccountId, key: &str, value: &str) -> Result<()>;

    /// Read the daily-cap overrides in ONE query. A cap is `None` when no row
    /// exists OR the stored value does not parse as an integer in `1..=100000`.
    fn stage2_cap_overrides(&self, account_id: AccountId) -> Result<Stage2CapOverrides>;

    // OUTBOUND READ TRACKING. Written by the human door's send path and by the
    // two open sinks (the direct `/t/:token` route and the opens poller); read
    // only by the human door. The agent door never touches any of it.

    /// Record a minted tracking token. `message_id` is `None` until the send's
    /// echo lands; `created_at` is unix seconds.
    fn insert_send_tracker(
        &self,
        account_id: AccountId,
        token: &str,
        message_id: Option<i64>,
        created_at: i64,
    ) -> Result<()>;

    /// Backfill the local message id of a tracker once the echo has ingested.
    /// `false` for a token this account did not mint.
    fn set_send_tracker_message(
        &self,
        account_id: AccountId,
        token: &str,
        message_id: i64,
    ) -> Result<bool>;

    /// Append one open. `Ok(false)` — no row written — when `token` names no
    /// tracker for this account: both open sinks accept arbitrary tokens from
    /// the outside, so an unknown one must leave nothing behind.
    fn record_open(
        &self,
        account_id: AccountId,
        token: &str,
        opened_at: i64,
        user_agent: Option<&str>,
        classification: &str,
    ) -> Result<bool>;

    /// Every open of one local message, oldest first, joined through the
    /// trackers minted for it. Empty for an untracked or unknown message.
    fn message_opens(&self, account_id: AccountId, message_id: i64) -> Result<Vec<MessageOpen>>;

    /// Resolve a tracking token to the message it was minted for, or `None` when
    /// the token is unknown to this account or its echo never backfilled an id.
    fn tracked_message(&self, account_id: AccountId, token: &str)
    -> Result<Option<TrackedMessage>>;

    /// Count NON-SENT (`is_sent=0`) messages received at or after `since`. Feeds
    /// the `avg_inbound_per_day` figure on `/client/triage-config`.
    fn count_inbound_since(&self, account_id: AccountId, since: DateTime<Utc>) -> Result<u64>;

    // SEMANTIC RECALL vector-index writes. The embedder lives in the caller, so
    // these take a precomputed vector / return the text to embed and never touch
    // a model. Query-side methods are inherent on `SqliteStore` because they need
    // the attached embedder.
    //
    // SECURITY: SEALED MESSAGES ARE NEVER EMBEDDED. Sealed is the stored
    // decision (auth, restricted), a person's restriction or the retired
    // `triage.sensitivity` column, and NOTHING is embedded until it is known
    // not to be sealed: a stored decision with access settled, or the owner's
    // own mail (`embeddable_mail_sql!` in `store/sqlite/mod.rs`).
    // `message_embeddable` is asked before any text reaches an embedder,
    // `messages_missing_vectors` selects only such rows, and
    // `write_embedded_vectors` re-checks under the store lock.

    /// RAW insert (or replace) of the embedding vector for one message;
    /// idempotent. `embedding.len()` MUST equal the vec0 table width (384).
    /// NOT GATED: for fixtures. Production writes use
    /// [`Store::write_embedded_vectors`].
    fn upsert_message_vector(
        &self,
        account_id: AccountId,
        message_id: i64,
        embedding: &[f32],
    ) -> Result<()>;

    /// [`Store::upsert_message_vector`] for a batch, which a store may write
    /// under one commit (the SQLite store does; the backfill drain hands it
    /// eight at a time). The default is the one-at-a-time loop, so a store
    /// that has no cheaper shape is still correct. Same contract per item.
    fn upsert_message_vectors(&self, account_id: AccountId, items: &[(i64, &[f32])]) -> Result<()> {
        for (message_id, embedding) in items {
            self.upsert_message_vector(account_id, *message_id, embedding)?;
        }
        Ok(())
    }

    /// [`Store::upsert_message_vectors`] for the items whose message may still
    /// be embedded at the moment of the write (the sealed gate, re-checked
    /// under the store's lock); the rest are skipped. Returns how many were
    /// written. The one vector write the sync engine makes.
    fn write_embedded_vectors(
        &self,
        account_id: AccountId,
        items: &[(i64, &[f32])],
    ) -> Result<usize>;

    /// Whether an embedder may read `message_id` now: not spam, not sealed,
    /// and either the owner's own mail with its access check settled or
    /// decided on its current content with access settled. Ingest asks it
    /// before embedding a row's text.
    fn message_embeddable(&self, account_id: AccountId, message_id: i64) -> Result<bool>;

    /// Whether a write since the last call may have made a message newly
    /// embeddable without the sync engine seeing it (a person's correction
    /// lifting a restriction, through the API). Clears as it answers; the
    /// engine's vector drain raises its own flag on `true`.
    fn take_vectors_owed(&self) -> bool;

    /// Up to `limit` messages with no vector yet that the sealed gate lets an
    /// embedder read, newest-first, for the backfill drain.
    fn messages_missing_vectors(
        &self,
        account_id: AccountId,
        limit: usize,
    ) -> Result<Vec<MissingVector>>;

    /// The currently-attached embedder, letting the sync engine resolve a
    /// LATE-attached one (attached in the background after `squelchd serve` binds
    /// its port) without holding a second handle.
    fn embedder(&self) -> Option<std::sync::Arc<dyn crate::embed::Embedder>> {
        None
    }

    // ---- auto-translate (docs/TRANSLATION.md) ------------------------------

    /// Queue one message for translation from `source_lang` (ISO 639-1),
    /// `deferred` for the archive tier. A row that exists is left alone. A
    /// `pending` row holds its triage like newly arrived mail. Ingest queues
    /// its own rows inside its transaction; this is for everything else.
    fn queue_translation(
        &self,
        account_id: AccountId,
        message_id: i64,
        source_lang: &str,
        deferred: bool,
        now: DateTime<Utc>,
    ) -> Result<()>;

    /// Claim up to `limit` rows due for translation, newest mail first:
    /// `pending` ones, and `failed` ones whose backoff has passed. Each claim
    /// counts an attempt and stamps a lease token, and a row out of attempts is
    /// never claimed again. Spam, sealed and the account's own mail are never
    /// claimed. The claim first settles rows nothing will ever finish: a lease
    /// that expired on the last attempt is `failed`, and a row whose message
    /// became spam, sealed or own mail is closed (its served text, index
    /// columns and vector dropped).
    fn claim_translations(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<TranslationJob>>;

    /// Write a claimed row's result. Only over the job's own lease: a row a
    /// heal dropped and re-queued meanwhile, or one another pass re-claimed,
    /// is left as it is. `true` when it wrote, which is what lets the caller
    /// embed the translation (a stale one must not get a vector).
    fn record_translation(
        &self,
        job: &TranslationJob,
        write: &TranslationWrite<'_>,
        now: DateTime<Utc>,
    ) -> Result<bool>;

    /// Promote a thread's `deferred` rows to `pending`, which is what the
    /// reader DISPLAYING an archive thread asks for (`POST
    /// /client/thread/{id}/translate`; a plain read never does, because the
    /// client prefetches through it). Returns how many moved.
    fn promote_deferred_translations(
        &self,
        account_id: AccountId,
        thread_id: &str,
        now: DateTime<Utc>,
    ) -> Result<usize>;

    /// The owner's "Languages I read" (ISO 639-1, sorted): mail detected in
    /// one of them is shown as written and never translated.
    fn read_languages(&self, account_id: AccountId) -> Result<Vec<String>>;

    /// Replace the "Languages I read" set (unknown codes and English are
    /// dropped) and bring the queue in line: unserved rows in an added
    /// language close `not_needed`, rows closed for a removed one come back
    /// `deferred` (translated when opened). Served translations stay. Returns
    /// the set as stored.
    fn set_read_languages(
        &self,
        account_id: AccountId,
        codes: &[String],
        now: DateTime<Utc>,
    ) -> Result<Vec<String>>;

    /// The translation rows for `message_ids`, in any status, ordered by id.
    /// The caller decides what a status means to its door.
    fn message_translations(
        &self,
        account_id: AccountId,
        message_ids: &[i64],
    ) -> Result<Vec<MessageTranslation>>;

    /// Write the English translation's vector for one message
    /// (`message_translation_vecs`), replacing any. A no-op (beyond removing a
    /// stale vector) unless the translation is served (`done`/`incomplete`).
    fn upsert_translation_vector(
        &self,
        account_id: AccountId,
        message_id: i64,
        embedding: &[f32],
    ) -> Result<()>;

    /// Up to `limit` served translations still missing a vector, newest mail
    /// first, carrying the TRANSLATED subject and body.
    fn translations_missing_vectors(
        &self,
        account_id: AccountId,
        limit: usize,
    ) -> Result<Vec<MissingVector>>;
}
