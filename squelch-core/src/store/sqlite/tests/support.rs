//! Fixtures shared by the store test modules.

use super::super::*;
use crate::embed::{DEFAULT_EMBED_MAX_CHARS, Embedder, message_embed_text};
use crate::triage::{CalendarInfo, CalendarKind, DeadlineHit, ReceiptInfo};
use crate::types::{FieldReasons, SealedKind, Sensitivity, Tier};
use chrono::TimeZone;
use std::sync::Arc;

/// A fresh in-memory store plus the standard test account.
pub(super) fn store() -> (SqliteStore, AccountId) {
    let store = SqliteStore::open_in_memory().unwrap();
    let acct = store.ensure_account("me@example.com").unwrap();
    (store, acct)
}

/// A FILE-backed store in a fresh directory of its own, plus the standard
/// test account, for a test that needs the journal, the fsyncs or a reopen
/// (`:memory:` has none of them). The directory is the caller's to remove.
pub(super) fn file_store(tag: &str) -> (SqliteStore, AccountId, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("squelch-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let store = SqliteStore::open(dir.join("store.sqlite3")).unwrap();
    let acct = store.ensure_account("me@example.com").unwrap();
    (store, acct, dir)
}

/// Same, with a semantic embedder attached at open (the vector-search tests).
pub(super) fn store_with_embedder(embedder: Arc<dyn Embedder>) -> (SqliteStore, AccountId) {
    let store = SqliteStore::open_in_memory()
        .unwrap()
        .with_embedder(embedder)
        .unwrap();
    let acct = store.ensure_account("me@example.com").unwrap();
    (store, acct)
}

// ---- the message/triage fixture builder --------------------------------

/// One chainable shape for every store fixture, with a terminal per way a test
/// lands a row: [`upsert`](TriagedBuilder::upsert) writes the message alone,
/// [`seed`](TriagedBuilder::seed) adds a hand-written triage row, and
/// [`ingest`](TriagedBuilder::ingest) runs the real ingest path.
#[derive(Clone)]
pub(super) struct TriagedBuilder {
    msg: NewMessage,
    sensitivity: Sensitivity,
    sealed_kind: Option<SealedKind>,
    importance: u8,
    tier: Tier,
    one_line: String,
    reason: String,
    field_reasons: FieldReasons,
    matched_rule: Option<i64>,
    deadline: Option<DeadlineHit>,
    receipt: Option<ReceiptInfo>,
    calendar: Option<CalendarInfo>,
    /// The loose shipping signal ingest computes — set directly here so the
    /// trigger/queue tests do not have to write shipping prose that happens to
    /// satisfy the detector.
    ship_extract: bool,
    confident: bool,
    /// Applied through the Stage-1 categorizer by `ingest`, after the row lands.
    category: Option<String>,
    /// The Date header's offset and the invitation, as ingest captures them.
    /// Only `ingest` carries them: `upsert` writes the message row alone.
    date_offset_minutes: Option<i32>,
    invite_zone: Option<crate::timezone::InviteZone>,
    /// The detected non-English language, as ingest carries it.
    translate_lang: Option<crate::translate::Lang>,
    /// Newly arrived (incremental) rather than backfilled.
    foreground: bool,
    over_budget: bool,
}

/// A plain inbound message with a neutral triage shape — the base every other
/// fixture chains off.
pub(super) fn triaged(account_id: AccountId, gmail: &str, thread: &str) -> TriagedBuilder {
    TriagedBuilder {
        msg: NewMessage {
            account_id,
            gmail_msg_id: gmail.to_string(),
            thread_id: thread.to_string(),
            from_addr: "alice@example.com".to_string(),
            from_name: Some("Alice".to_string()),
            subject: "Lunch?".to_string(),
            received_at: Utc::now(),
            snippet: "want to grab lunch".to_string(),
            body: "Hey, want to grab lunch tomorrow?".to_string(),
            body_html: None,
            is_sent: false,
            is_spam: false,
            to_addrs: None,
            cc_addrs: None,
            bcc_addrs: None,
            list_unsubscribe: None,
            list_unsub_one_click: false,
            auth_pass: None,
        },
        sensitivity: Sensitivity::Normal,
        sealed_kind: None,
        importance: 0,
        tier: Tier::Noise,
        one_line: String::new(),
        reason: String::new(),
        field_reasons: FieldReasons::default(),
        matched_rule: None,
        deadline: None,
        receipt: None,
        calendar: None,
        ship_extract: false,
        confident: true,
        category: None,
        date_offset_minutes: None,
        invite_zone: None,
        translate_lang: None,
        foreground: false,
        over_budget: false,
    }
}

// Setters are named for the columns they set (`from`, `is_sent`, `to_addrs`),
// which collides with the from_*/is_*/to_* self conventions.
#[allow(clippy::wrong_self_convention)]
impl TriagedBuilder {
    // -- message fields --
    pub(super) fn from(mut self, addr: &str) -> Self {
        self.msg.from_addr = addr.to_string();
        self
    }
    pub(super) fn from_name(mut self, name: Option<&str>) -> Self {
        self.msg.from_name = name.map(Into::into);
        self
    }
    pub(super) fn subject(mut self, subject: &str) -> Self {
        self.msg.subject = subject.to_string();
        self
    }
    pub(super) fn body(mut self, body: &str) -> Self {
        self.msg.body = body.to_string();
        self
    }
    pub(super) fn snippet(mut self, snippet: &str) -> Self {
        self.msg.snippet = snippet.to_string();
        self
    }
    pub(super) fn received_at(mut self, at: DateTime<Utc>) -> Self {
        self.msg.received_at = at;
        self
    }
    pub(super) fn is_sent(mut self, is_sent: bool) -> Self {
        self.msg.is_sent = is_sent;
        self
    }
    /// Mail the provider filed as spam — what the SPAM label walk ingests.
    pub(super) fn is_spam(mut self, is_spam: bool) -> Self {
        self.msg.is_spam = is_spam;
        self
    }
    /// Display recipients, as the ingest path renders them. One setter per
    /// header, because that is how they are stored and read back.
    pub(super) fn to_addrs(mut self, to: &str) -> Self {
        self.msg.to_addrs = Some(to.to_string());
        self
    }
    pub(super) fn cc_addrs(mut self, cc: &str) -> Self {
        self.msg.cc_addrs = Some(cc.to_string());
        self
    }
    /// Only ever kept on sent mail — the store drops it for anything else.
    pub(super) fn bcc_addrs(mut self, bcc: &str) -> Self {
        self.msg.bcc_addrs = Some(bcc.to_string());
        self
    }
    pub(super) fn list_unsubscribe(mut self, header: &str, one_click: bool) -> Self {
        self.msg.list_unsubscribe = Some(header.to_string());
        self.msg.list_unsub_one_click = one_click;
        self
    }
    pub(super) fn auth_pass(mut self, auth_pass: Option<bool>) -> Self {
        self.msg.auth_pass = auth_pass;
        self
    }

    // -- triage fields --
    pub(super) fn importance(mut self, importance: u8) -> Self {
        self.importance = importance;
        self
    }
    pub(super) fn tier(mut self, tier: Tier) -> Self {
        self.tier = tier;
        self
    }
    pub(super) fn one_line(mut self, one_line: &str) -> Self {
        self.one_line = one_line.to_string();
        self
    }
    pub(super) fn reason(mut self, reason: &str) -> Self {
        self.reason = reason.to_string();
        self
    }
    pub(super) fn field_reasons(mut self, field_reasons: FieldReasons) -> Self {
        self.field_reasons = field_reasons;
        self
    }
    /// Sensitivity WITHOUT a sealed_kind — the pre-kind row shape.
    pub(super) fn sensitivity(mut self, sensitivity: Sensitivity) -> Self {
        self.sensitivity = sensitivity;
        self
    }
    /// Sealed, carrying its kind (what a real seal writes).
    pub(super) fn sealed(mut self, kind: SealedKind) -> Self {
        self.sensitivity = Sensitivity::Sealed;
        self.sealed_kind = Some(kind);
        self
    }
    pub(super) fn matched_rule(mut self, rule_id: Option<i64>) -> Self {
        self.matched_rule = rule_id;
        self
    }
    pub(super) fn confident(mut self, confident: bool) -> Self {
        self.confident = confident;
        self
    }
    pub(super) fn deadline(mut self, deadline: DeadlineHit) -> Self {
        self.deadline = Some(deadline);
        self
    }
    pub(super) fn receipt(mut self, receipt: ReceiptInfo) -> Self {
        self.receipt = Some(receipt);
        self
    }
    pub(super) fn calendar(mut self, calendar: CalendarInfo) -> Self {
        self.calendar = Some(calendar);
        self
    }
    /// Carry the loose shipping signal, i.e. queue this row for the shipments
    /// extractor (`ship_extract_model='pending'`).
    pub(super) fn ship_extract(mut self, ship_extract: bool) -> Self {
        self.ship_extract = ship_extract;
        self
    }
    pub(super) fn category(mut self, category: &str) -> Self {
        self.category = Some(category.to_string());
        self
    }
    /// The Date header's UTC offset in minutes (ingest only).
    pub(super) fn date_offset(mut self, minutes: Option<i32>) -> Self {
        self.date_offset_minutes = minutes;
        self
    }
    /// Detected as written in `code` (ISO 639-1), as ingest would carry it.
    pub(super) fn lang(mut self, code: &str) -> Self {
        self.translate_lang = crate::translate::Lang::from_code(code);
        assert!(self.translate_lang.is_some(), "unknown language {code}");
        self
    }
    /// Newly arrived by incremental sync, as the engine marks it: its
    /// first triage may hold for its translation.
    pub(super) fn arrival(mut self) -> Self {
        self.foreground = true;
        self
    }
    /// Ingested with the day's translate budget spent.
    pub(super) fn over_budget(mut self) -> Self {
        self.over_budget = true;
        self
    }
    /// An invitation organized by `organizer` in `zone` (ingest only).
    pub(super) fn invite(mut self, organizer: &str, zone: &str) -> Self {
        self.invite_zone = Some(crate::timezone::InviteZone {
            organizer: organizer.to_string(),
            zone: zone.to_string(),
        });
        self
    }

    // -- terminals --
    pub(super) fn msg(&self) -> NewMessage {
        self.msg.clone()
    }

    pub(super) fn build(&self) -> TriagedMessage {
        TriagedMessage {
            foreground_triage: self.foreground,
            message: self.msg(),
            recipients: vec![],
            // DERIVED from `to_addrs` rather than set separately, so a test that
            // says who a sent message went to gets the normalized index for free
            // and cannot describe a message whose two recipient views disagree —
            // which is exactly the state the ingest path makes impossible.
            recipient_addrs: self
                .msg
                .to_addrs
                .as_deref()
                .map(crate::sync::ingest::parse_stored_recipients)
                .unwrap_or_default(),
            sensitivity: self.sensitivity,
            sealed_kind: self.sealed_kind,
            importance: self.importance,
            tier: self.tier,
            one_line: self.one_line.clone(),
            reason: self.reason.clone(),
            matched_rule: self.matched_rule,
            field_reasons: self.field_reasons.clone(),
            deadline: self.deadline.clone(),
            shipment: None,
            ship_extract: self.ship_extract,
            receipt: self.receipt.clone(),
            calendar: self.calendar.clone(),
            attachments: vec![],
            confident: self.confident,
            // The store fixtures are about storage, not about which sync path
            // an ingest was on; the engine's own tests own that decision. NULL
            // is also the shape every pre-existing row has after the migration.
            notify_eligible_at: None,
            date_offset_minutes: self.date_offset_minutes,
            invite_zone: self.invite_zone.clone(),
            translate_lang: self.translate_lang,
            translate_over_budget: self.over_budget,
        }
    }

    /// Write the message row only (no triage row).
    pub(super) fn upsert(&self, store: &SqliteStore) -> i64 {
        store.upsert_message(&self.msg()).unwrap()
    }

    /// Write the message plus a hand-written triage row (`set_triage`).
    pub(super) fn seed(&self, store: &SqliteStore) -> i64 {
        let id = self.upsert(store);
        store
            .set_triage(
                id,
                self.msg.account_id,
                self.importance,
                self.tier,
                self.sensitivity,
                self.sealed_kind,
                &self.one_line,
                &self.reason,
                self.deadline.as_ref().map(|d| d.due_at),
            )
            .unwrap();
        id
    }

    /// Run the real ingest path, then apply `.category()` — if one was set —
    /// through the Stage-1 categorizer.
    pub(super) fn ingest(&self, store: &SqliteStore) -> i64 {
        let id = store.ingest_message(&self.build()).unwrap();
        if let Some(category) = &self.category {
            apply_category(store, self.msg.account_id, id, category, false);
        }
        id
    }
}

// ---- thin wrappers over the builder ------------------------------------

/// A store-ready TriagedMessage carrying a receipt (noise tier), for the
/// auto-resolve tests.
pub(super) fn receipt_triaged(
    acct: AccountId,
    gmail: &str,
    thread: &str,
    amount: Option<f64>,
) -> TriagedBuilder {
    triaged(acct, gmail, thread)
        .from("no-reply@baywheels.com")
        .from_name(Some("Bay Wheels"))
        .subject("Your Bay Wheels ride receipt")
        .importance(15)
        .one_line("Your Bay Wheels ride receipt")
        .reason("receipt")
        .receipt(ReceiptInfo {
            amount,
            currency: amount.map(|_| "USD".into()),
        })
}

/// A plain, normal, non-receipt inbound (or sent) TriagedMessage with an
/// explicit sender + received_at, for the unsubscribe violation tests.
pub(super) fn inbound_triaged(
    acct: AccountId,
    gmail: &str,
    thread: &str,
    from: &str,
    received_at: DateTime<Utc>,
    is_sent: bool,
) -> TriagedBuilder {
    triaged(acct, gmail, thread)
        .from(from)
        .from_name(None)
        .received_at(received_at)
        .is_sent(is_sent)
        .importance(10)
}

/// Set a triage row's LLM category via the real Stage-1 apply path (the
/// categorizer). `needs_stage2` is irrelevant to extraction but honored.
pub(super) fn apply_category(
    store: &SqliteStore,
    acct: AccountId,
    message_id: i64,
    category: &str,
    needs_stage2: bool,
) {
    store
        .stage1_apply(&Stage1Applied {
            message_id,
            account_id: acct,
            importance: 40,
            tier: Tier::Noise,
            one_line: "categorized".into(),
            reason: "stage-1".into(),
            field_reasons: crate::types::FieldReasons::default(),
            stage1_model_used: "claude-opus-5".into(),
            needs_stage2,
            escalation_reason: needs_stage2.then_some("boundary"),
            deadline: None,
            category: Some(category.into()),
        })
        .unwrap();
}

pub(super) fn triage_extract_status(
    store: &SqliteStore,
    message_id: i64,
) -> (String, Option<String>, Option<String>) {
    let conn = store.lock().unwrap();
    conn.query_row(
        "SELECT status, resolved_at, extractor_model_used FROM triage WHERE message_id=?1",
        params![message_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .unwrap()
}

// ---- calendar updates --------------------------------------------------

/// Build a store-ready TriagedMessage carrying a calendar update (noise
/// tier), for the auto-resolve/listing tests.
pub(super) fn calendar_triaged(
    acct: AccountId,
    gmail: &str,
    kind: CalendarKind,
    received_at: DateTime<Utc>,
) -> TriagedBuilder {
    triaged(acct, gmail, &format!("t-{gmail}"))
        .from("sam@gmail.com")
        .from_name(Some("Sam Doe"))
        .subject("Invitation: Design review @ Wed Jul 22, 2026 10am")
        .received_at(received_at)
        .importance(15)
        .one_line("Invitation: Design review")
        .reason("calendar")
        .calendar(CalendarInfo {
            kind,
            event_title: Some("Design review".into()),
            starts_at: Some(Utc.with_ymd_and_hms(2026, 7, 22, 10, 0, 0).unwrap()),
            organizer: Some("Sam Doe".into()),
        })
}

// ---- receipt -> open-bill auto-close ----------------------------------

/// Build a store-ready TriagedMessage carrying a BILL (deadline tier) from
/// the given sender, for the receipt->bill auto-close tests.
pub(super) fn bill_triaged(
    acct: AccountId,
    gmail: &str,
    from_addr: &str,
    from_name: Option<&str>,
    amount: Option<f64>,
    received_at: DateTime<Utc>,
    due_at: DateTime<Utc>,
) -> TriagedBuilder {
    triaged(acct, gmail, &format!("t-{gmail}"))
        .from(from_addr)
        .from_name(from_name)
        .subject("Your statement is ready")
        .received_at(received_at)
        .importance(200)
        .tier(Tier::Deadline)
        .one_line("Payment due")
        .reason("bill")
        .deadline(DeadlineHit {
            kind: "payment_due".into(),
            amount,
            currency: amount.map(|_| "USD".into()),
            due_at,
            past_due: false,
            source: "test".into(),
        })
}

/// Build a store-ready receipt TriagedMessage from the given sender.
pub(super) fn receipt_from(
    acct: AccountId,
    gmail: &str,
    from_addr: &str,
    from_name: Option<&str>,
    amount: Option<f64>,
    received_at: DateTime<Utc>,
) -> TriagedBuilder {
    receipt_triaged(acct, gmail, &format!("t-{gmail}"), amount)
        .from(from_addr)
        .from_name(from_name)
        .received_at(received_at)
}

/// Read (status, resolved_at) straight off a triage row.
pub(super) fn triage_status(
    store: &SqliteStore,
    acct: AccountId,
    id: i64,
) -> (String, Option<String>) {
    let conn = store.lock().unwrap();
    conn.query_row(
        "SELECT status, resolved_at FROM triage WHERE account_id=?1 AND message_id=?2",
        params![acct, id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap()
}

pub(super) fn auto_close_audits(store: &SqliteStore, acct: AccountId) -> Vec<AuditEntry> {
    store
        .list_audit(acct, 50)
        .unwrap()
        .into_iter()
        .filter(|e| e.action == "bill.auto_close")
        .collect()
}

// --- sitrep seen-ledger --------------------------------------------------

/// Helper: a non-sealed triaged message with a chosen tier/importance.
pub(super) fn ingest_normal(
    store: &SqliteStore,
    acct: AccountId,
    gmail: &str,
    thread: &str,
    tier: Tier,
    importance: u8,
    received: DateTime<Utc>,
) -> i64 {
    triaged(acct, gmail, thread)
        .received_at(received)
        .importance(importance)
        .tier(tier)
        .seed(store)
}

// ---- Stage-1 LLM queue / markers -------------------------------------

/// Build a store-ready TriagedMessage with controllable rule/confidence so
/// the queue-marker logic in `ingest_message` is testable directly.
pub(super) fn triaged_row(
    acct: AccountId,
    gmail: &str,
    thread: &str,
    matched_rule: Option<i64>,
    confident: bool,
    sensitivity: Sensitivity,
) -> TriagedBuilder {
    let b = triaged(acct, gmail, thread)
        .importance(40)
        .one_line("seed")
        .reason("seed")
        .matched_rule(matched_rule)
        .confident(confident);
    if sensitivity == Sensitivity::Sealed {
        b.sealed(SealedKind::Otp)
    } else {
        b.sensitivity(sensitivity)
    }
}

// ---- Stage-2 store methods -------------------------------------------

/// Insert a message + a triage row with model_used NULL (queued) or set
/// (processed), controlling sensitivity so the sealed-exclusion is testable.
pub(super) fn seed_triage_row(
    store: &SqliteStore,
    acct: AccountId,
    gmail_id: &str,
    thread: &str,
    sensitivity: Sensitivity,
) -> i64 {
    triaged(acct, gmail_id, thread)
        .sensitivity(sensitivity)
        .importance(40)
        .one_line("ambiguous")
        .reason("no rule matched")
        .seed(store)
}

// ---- NOTIFICATION EVENTS ---------------------------------------------

/// A worthy `NewEvent` for `message_id`, distinct per id so ordering and
/// snapshotting are visible in assertions.
pub(super) fn new_event(store: &SqliteStore, acct: AccountId, message_id: i64) -> NewEvent {
    store.lock().unwrap().execute(
        "INSERT OR IGNORE INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet)
         VALUES(?1,?2,?3,?4,'alice@example.com','Notification fixture',?5,'')",
        params![message_id, acct, format!("event-{message_id}"), format!("t{message_id}"), Utc::now().to_rfc3339()],
    ).unwrap();
    NewEvent {
        account_id: acct,
        message_id,
        thread_id: format!("t{message_id}"),
        kind: EventKind::Surfaced,
        tier: Tier::Signal,
        importance: 70,
        sender: "alice@example.com".to_string(),
        one_line: format!("line {message_id}"),
        deadline: None,
        // An ordinary event. The sealed-routing tests set this explicitly.
        is_auth: false,
        sealed_kind: None,
    }
}

/// Embed a message's subject+body with `embedder` and write its vector, exactly
/// as the sync ingest/backfill path does. CALLER ensures the row is non-sealed
/// (mirrors the structural gate: sealed mail never reaches this).
pub(super) fn embed_and_store(
    store: &SqliteStore,
    embedder: &dyn Embedder,
    acct: AccountId,
    message_id: i64,
    subject: &str,
    body: &str,
) {
    let text = message_embed_text(subject, body, DEFAULT_EMBED_MAX_CHARS);
    let v = embedder.embed(&text).unwrap();
    store.upsert_message_vector(acct, message_id, &v).unwrap();
}

/// Count vectors present for a given message id (0 or 1). Used to assert a
/// sealed message is structurally absent from the vector space.
pub(super) fn vec_count_for(store: &SqliteStore, message_id: i64) -> i64 {
    let conn = store.lock().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM message_vecs WHERE message_id = ?1",
        params![message_id],
        |r| r.get(0),
    )
    .unwrap()
}

/// The listing policy for shipment tests that are NOT about the silence
/// window: an unreachable retirement cap and a disabled window, so nothing is
/// ever hidden and the filter cannot perturb a test asking about something
/// else. The silence and clear tests build their own.
pub(super) const KEEP_ALL_SHIPMENTS: crate::config::ShipmentListPolicy =
    crate::config::ShipmentListPolicy {
        retired_at_failures: u32::MAX,
        stale_after_days: 0,
    };

/// [`KEEP_ALL_SHIPMENTS`] with a specific retirement cap. The window stays off,
/// so this hides nothing by itself; it exists for tests that read the cap back
/// or pair it with [`stale_after`].
pub(super) fn retired_at(cap: u32) -> crate::config::ShipmentListPolicy {
    crate::config::ShipmentListPolicy {
        retired_at_failures: cap,
        ..KEEP_ALL_SHIPMENTS
    }
}

/// [`KEEP_ALL_SHIPMENTS`] with a silence window, for the tests that ARE about
/// the silence filter.
pub(super) fn stale_after(days: u32) -> crate::config::ShipmentListPolicy {
    crate::config::ShipmentListPolicy {
        stale_after_days: days,
        ..KEEP_ALL_SHIPMENTS
    }
}

// ---- query plans ---------------------------------------------------------

/// `EXPLAIN QUERY PLAN` for `sql`, one line per step, with every placeholder
/// bound to NULL: a plan does not depend on the values, but rusqlite still
/// insists each parameter is bound. For the tests that pin a statement to its
/// index rather than to a timing.
pub(super) fn plan(conn: &rusqlite::Connection, sql: &str) -> Vec<String> {
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let nulls = vec![rusqlite::types::Value::Null; stmt.parameter_count()];
    stmt.query_map(rusqlite::params_from_iter(nulls), |r| r.get::<_, String>(3))
        .unwrap()
        .map(|line| line.unwrap())
        .collect()
}

// ---- decisions, for the embedders' sealed gate ----------------------------

/// A triage decision for `id`, `adjust`ed, committed through the worker's
/// own path (claim, context, commit), so every hook a real decision runs
/// runs. The caller's store must hold no other claimable triage job.
pub(super) fn commit_decision(
    store: &SqliteStore,
    acct: AccountId,
    id: i64,
    adjust: impl FnOnce(&mut crate::triage::decision::MessageDecision),
) {
    use crate::store::agent_triage::{AgentCommitOutcome, AgentTriageStore};
    // A trigger of its own per call: a job is unique per trigger and revision,
    // and a re-triage of unchanged content is a second job, not the first.
    static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let run = RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    store
        .enqueue_agent_triage(acct, id, &format!("test:{run}"), false)
        .unwrap();
    let job = store
        .claim_agent_job(acct, "triage", Utc::now() + chrono::Duration::hours(1), 60)
        .unwrap()
        .expect("a triage job waits");
    assert_eq!(job.message_id, id, "another message's job was claimable");
    let context = store.load_agent_context(&job).unwrap();
    let mut decision = crate::triage::decision::MessageDecision {
        kinds: vec![crate::triage::decision::EmailKind::General],
        summary: "A note".into(),
        reason: "Ordinary mail".into(),
        ..Default::default()
    };
    adjust(&mut decision);
    assert_eq!(
        store
            .commit_agent_decision(
                &job,
                &context,
                &decision,
                std::slice::from_ref(&context.message.source)
            )
            .unwrap(),
        AgentCommitOutcome::Applied
    );
}

/// An ordinary, NOT sealed decision written straight into the tables, access
/// settled `allowed`, for fixtures whose subject is ranking or snippets and
/// that need several rows decided at once. The sealed-gate tests use
/// [`commit_decision`].
pub(super) fn settle_decided(store: &SqliteStore, acct: AccountId, ids: &[i64]) {
    let conn = store.lock().unwrap();
    for id in ids {
        conn.execute(
            "INSERT INTO agent_message_state(account_id,message_id,content_snapshot,revision,access)
             VALUES(?1,?2,'',1,'allowed') ON CONFLICT(account_id,message_id) DO UPDATE SET access='allowed'",
            params![acct, id],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO agent_message_decisions(account_id,message_id,revision,decision_json,decided_at)
             VALUES(?1,?2,(SELECT revision FROM agent_message_state WHERE account_id=?1 AND message_id=?2),
                    '{\"kinds\":[\"general\"]}',?3)",
            params![acct, id, Utc::now().to_rfc3339()],
        )
        .unwrap();
    }
}

/// Every non-spam message with no vector, as the drain selected them BEFORE
/// the sealed gate: for fixtures about ranking and snippets, which plant
/// vectors raw. Never the subject of a gate test.
pub(super) fn unvectored(
    store: &SqliteStore,
    acct: AccountId,
    limit: i64,
) -> Vec<crate::store::MissingVector> {
    let conn = store.lock().unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT m.id, m.subject, m.body FROM messages m
             JOIN triage t ON t.message_id = m.id
             WHERE m.account_id = ?1 AND m.is_spam = 0
               AND NOT EXISTS (SELECT 1 FROM message_vecs v WHERE v.message_id = m.id)
             ORDER BY m.received_at DESC LIMIT ?2",
        )
        .unwrap();
    stmt.query_map(params![acct, limit], |r| {
        Ok(crate::store::MissingVector {
            message_id: r.get(0)?,
            subject: r.get(1)?,
            body: r.get(2)?,
        })
    })
    .unwrap()
    .map(|r| r.unwrap())
    .collect()
}
