//! "As of T": the store's state at the moment a gold run completed, rebuilt
//! from timestamps on a snapshot copy. `export` uses it to build each case's
//! initial context, and the replay's evidence reader uses the SAME functions
//! to filter tool results, so the two cannot disagree (a different thread
//! revision between them fails the run with `context_changed_during_run`).
//!
//! What is knowable, and what is not:
//! - A message existed at T when its `triage.created_at` (local ingest) <= T.
//! - Only each message's CURRENT decision is stored. Thread attention as of T
//!   is replayed from those decisions' `decided_at`; a message decided more
//!   than once has lost its earlier decision (the case is marked suspect).
//! - User state: `opened_at` and `surfaced_at` are first-time stamps;
//!   `resolved_at` is set by done/defer and cleared by every reopen, so a
//!   stamp means the row is done now and has been since then.
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use rusqlite::Connection;
use serde_json::{Value, json};
use squelch_core::store::SqliteStore;
use squelch_core::store::agent_triage::{AgentContext, AgentMessage};
use squelch_core::sync::triage_worker::StoreEvidence;
use squelch_core::triage::context::{EvidenceReader, EvidenceRequest, EvidenceResult};
use squelch_core::triage::decision::{MessageDecision, ThreadAttentionDecision};
use squelch_core::types::AccountId;
use std::collections::HashMap;
use std::sync::Mutex;

/// RFC3339, or the bare `YYYY-MM-DD HH:MM:SS` / `YYYY-MM-DD` SQLite writes,
/// read as UTC. None for anything else.
pub fn parse_ts(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed.with_timezone(&Utc));
    }
    for format in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f"] {
        if let Ok(parsed) = NaiveDateTime::parse_from_str(value, format) {
            return Some(parsed.and_utc());
        }
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|date| date.and_utc())
}

/// True only when `value` parses and is at or before `t`. Unknown is "not
/// provably before", which is the direction that cannot leak the future.
pub fn at_or_before(value: Option<&str>, t: DateTime<Utc>) -> bool {
    value.and_then(parse_ts).is_some_and(|at| at <= t)
}

/// The user-state columns of one `triage` row.
#[derive(Debug, Clone, Default)]
pub struct UserStateRow {
    pub created_at: Option<String>,
    pub opened_at: Option<String>,
    pub surfaced_at: Option<String>,
    pub resolved_at: Option<String>,
    pub remind_at: Option<String>,
    pub reminded_at: Option<String>,
}

/// One change to a thread's attention: a stored decision's `attention` (for
/// its own thread) or one of its `related_updates` (for another thread).
#[derive(Debug, Clone)]
pub struct TimelineEntry {
    pub at: DateTime<Utc>,
    pub source_message_id: i64,
    pub attention: ThreadAttentionDecision,
}

/// A stored decision, located in time and thread.
#[derive(Debug, Clone)]
pub struct DecisionRow {
    pub message_id: i64,
    pub thread_id: String,
    pub decided_at: DateTime<Utc>,
    /// Sent, spam or own mail: commit never applies its attention.
    pub access_only: bool,
    pub decision: MessageDecision,
}

#[derive(Debug, Clone, Default)]
pub struct Timeline {
    threads: HashMap<String, Vec<TimelineEntry>>,
}

impl Timeline {
    /// `committed` is each thread's current `agent_thread_attention`, whose
    /// actions carry the ids production assigned at commit.
    pub fn from_decisions<'a>(
        rows: impl IntoIterator<Item = &'a DecisionRow>,
        committed: &HashMap<String, ThreadAttentionDecision>,
    ) -> Self {
        let mut timeline = Self::default();
        for row in rows {
            if row.access_only {
                continue;
            }
            let mut push = |thread: &str, attention: &ThreadAttentionDecision| {
                timeline
                    .threads
                    .entry(thread.to_owned())
                    .or_default()
                    .push(TimelineEntry {
                        at: row.decided_at,
                        source_message_id: row.message_id,
                        attention: attention.clone(),
                    });
            };
            push(&row.thread_id, &row.decision.attention);
            for update in &row.decision.related_updates {
                push(&update.thread_id, &update.attention);
            }
        }
        for (thread, entries) in timeline.threads.iter_mut() {
            entries.sort_by_key(|entry| (entry.at, entry.source_message_id));
            recover_action_ids(entries, committed.get(thread));
        }
        timeline
    }

    /// Entries for `thread` strictly before `t`, oldest first, never from the
    /// `target` message: its stored decision IS the answer being evaluated
    /// (and is stamped a few microseconds before its run completes).
    pub fn before(&self, thread: &str, t: DateTime<Utc>, target: i64) -> Vec<&TimelineEntry> {
        self.threads
            .get(thread)
            .into_iter()
            .flatten()
            .filter(|entry| entry.at < t && entry.source_message_id != target)
            .collect()
    }

    pub fn attention_as_of(
        &self,
        thread: &str,
        t: DateTime<Utc>,
        target: i64,
    ) -> Option<ThreadAttentionDecision> {
        self.before(thread, t, target)
            .last()
            .map(|entry| entry.attention.clone())
    }

    /// Production starts a thread's revision at 1 and adds 1 per update, with
    /// 0 for a thread that has none: the count of entries.
    pub fn revision_as_of(&self, thread: &str, t: DateTime<Utc>, target: i64) -> i64 {
        self.before(thread, t, target).len() as i64
    }
}

/// Give each new action (stored with a null id) the id production assigned
/// it at commit, which the next run was shown. Stored decisions keep null,
/// but the ids are recoverable:
/// - the thread's LATEST entry: the committed attention holds its actions in
///   order with their ids (commit assigns them positionally), when it is the
///   same list of actions;
/// - an EARLIER entry: ids the next entry references that no entry up to
///   this one already carried were assigned to this entry's new actions;
///   matched by (kind, description), then by order within a kind, then by
///   order. The next entry may be a case's own gold: only id strings are
///   taken from it, which production showed the model anyway.
///
/// Whatever stays unknown gets a stand-in, `act-<message>-<index>`.
fn recover_action_ids(entries: &mut [TimelineEntry], committed: Option<&ThreadAttentionDecision>) {
    use squelch_core::triage::decision::AttentionAction;
    let mut known: std::collections::HashSet<String> = std::collections::HashSet::new();
    let last = entries.len().saturating_sub(1);
    for k in 0..entries.len() {
        known.extend(
            entries[k]
                .attention
                .actions
                .iter()
                .filter_map(|a| a.id.clone()),
        );
        let slots: Vec<usize> = entries[k]
            .attention
            .actions
            .iter()
            .enumerate()
            .filter(|(_, a)| a.id.is_none())
            .map(|(index, _)| index)
            .collect();
        if slots.is_empty() {
            continue;
        }
        let same_list = |committed: &ThreadAttentionDecision| {
            committed.actions.len() == entries[k].attention.actions.len()
                && committed
                    .actions
                    .iter()
                    .zip(&entries[k].attention.actions)
                    .all(|(c, e)| {
                        c.kind == e.kind
                            && c.description == e.description
                            && (e.id.is_none() || e.id == c.id)
                    })
        };
        let mut found: Vec<(usize, String)> = Vec::new();
        if k == last {
            if let Some(committed) = committed.filter(|c| same_list(c)) {
                for &slot in &slots {
                    if let Some(id) = committed.actions[slot].id.clone() {
                        found.push((slot, id));
                    }
                }
            }
        } else {
            let candidates: Vec<&AttentionAction> = entries[k + 1]
                .attention
                .actions
                .iter()
                .filter(|a| a.id.as_ref().is_some_and(|id| !known.contains(id)))
                .collect();
            let mut used = vec![false; candidates.len()];
            let mut open: Vec<usize> = slots.clone();
            let actions = entries[k].attention.actions.clone();
            type Match = fn(&AttentionAction, &AttentionAction) -> bool;
            let passes: [Match; 3] = [
                |a, c| a.kind == c.kind && a.description == c.description,
                |a, c| a.kind == c.kind,
                |_, _| true,
            ];
            for pass in passes {
                open.retain(|&slot| {
                    let position = candidates
                        .iter()
                        .enumerate()
                        .position(|(i, c)| !used[i] && pass(&actions[slot], c));
                    match position {
                        Some(i) => {
                            used[i] = true;
                            found.push((slot, candidates[i].id.clone().expect("candidate id")));
                            false
                        }
                        None => true,
                    }
                });
            }
        }
        let message = entries[k].source_message_id;
        for (slot, id) in found {
            known.insert(id.clone());
            entries[k].attention.actions[slot].id = Some(id);
        }
        for (index, action) in entries[k].attention.actions.iter_mut().enumerate() {
            if action.id.is_none() {
                action.id = Some(format!("act-{message}-{index}"));
            }
        }
    }
}

/// Everything the as-of filters read, loaded once from the snapshot copy.
#[derive(Debug, Default)]
pub struct AsOfData {
    pub user_state: HashMap<i64, UserStateRow>,
    pub decisions: HashMap<i64, DecisionRow>,
    pub timeline: Timeline,
    /// ASCII-lowercased address (SQLite NOCASE) to its earliest `first_seen`.
    pub contacts: HashMap<String, Option<DateTime<Utc>>>,
    /// (message id, field) to the correction's `updated_at`.
    pub correction_times: HashMap<(i64, String), String>,
    /// Applied triage-kind runs per message.
    pub applied_runs: HashMap<i64, usize>,
    /// Stored decisions that no longer deserialize; left out of the timeline.
    pub undecodable_decisions: Vec<i64>,
    /// `messages.received_at`, the existence fallback for a message with no
    /// triage row.
    pub received_at: HashMap<i64, String>,
}

impl AsOfData {
    pub fn load(conn: &Connection) -> Result<Self> {
        let mut data = Self::default();
        let mut stmt = conn.prepare(
            "SELECT message_id,created_at,opened_at,surfaced_at,resolved_at,remind_at,reminded_at FROM triage",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                UserStateRow {
                    created_at: row.get(1)?,
                    opened_at: row.get(2)?,
                    surfaced_at: row.get(3)?,
                    resolved_at: row.get(4)?,
                    remind_at: row.get(5)?,
                    reminded_at: row.get(6)?,
                },
            ))
        })?;
        for row in rows {
            let (id, state) = row?;
            data.user_state.insert(id, state);
        }
        // `access_only` mirrors squelch-core's own_mail_sql!(): sent, or the
        // account's own address with no auth result, or spam.
        let mut stmt = conn.prepare(
            "SELECT d.message_id,m.thread_id,d.decided_at,d.decision_json,
                    (m.is_sent=1 OR COALESCE(m.auth_pass IS NULL AND trim(m.from_addr)!='' AND
                     lower(trim(m.from_addr))=(SELECT lower(trim(email)) FROM accounts WHERE id=m.account_id),0)
                     OR m.is_spam=1)
             FROM agent_message_decisions d JOIN messages m ON m.account_id=d.account_id AND m.id=d.message_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })?;
        for row in rows {
            let (message_id, thread_id, decided_at, json, access_only) = row?;
            let (Some(decided_at), Ok(decision)) = (
                parse_ts(&decided_at),
                serde_json::from_str::<MessageDecision>(&json),
            ) else {
                data.undecodable_decisions.push(message_id);
                continue;
            };
            data.decisions.insert(
                message_id,
                DecisionRow {
                    message_id,
                    thread_id,
                    decided_at,
                    access_only,
                    decision,
                },
            );
        }
        data.undecodable_decisions.sort_unstable();
        let mut committed = HashMap::new();
        let mut stmt =
            conn.prepare("SELECT thread_id,attention_json FROM agent_thread_attention")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (thread, json) = row?;
            if let Ok(attention) = serde_json::from_str::<ThreadAttentionDecision>(&json) {
                committed.insert(thread, attention);
            }
        }
        data.timeline = Timeline::from_decisions(data.decisions.values(), &committed);
        let mut stmt = conn.prepare("SELECT id,received_at FROM messages")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, received) = row?;
            data.received_at.insert(id, received);
        }
        let mut stmt = conn.prepare("SELECT addr,first_seen FROM contacts")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (addr, first_seen) = row?;
            let seen = parse_ts(&first_seen);
            let slot = data
                .contacts
                .entry(addr.trim().to_ascii_lowercase())
                .or_insert(seen);
            // The earliest provable sighting wins; unknown never beats known.
            *slot = match (*slot, seen) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
        let mut stmt =
            conn.prepare("SELECT message_id,field,updated_at FROM agent_triage_corrections")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (id, field, at) = row?;
            data.correction_times.insert((id, field), at);
        }
        let mut stmt = conn.prepare(
            "SELECT r.message_id,COUNT(*) FROM agent_triage_runs r JOIN agent_triage_jobs j ON j.id=r.job_id
             WHERE j.kind='triage' AND r.outcome='applied' GROUP BY r.message_id",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;
        for row in rows {
            let (id, count) = row?;
            data.applied_runs.insert(id, count.max(0) as usize);
        }
        Ok(data)
    }

    /// Did `message` exist (locally) at `t`? `triage.created_at`, falling back
    /// to the message's own date only when there is no triage row.
    pub fn existed_at(&self, message: &AgentMessage, t: DateTime<Utc>) -> bool {
        match self
            .user_state
            .get(&message.id)
            .and_then(|row| row.created_at.as_deref())
        {
            Some(created) => at_or_before(Some(created), t),
            None => at_or_before(Some(&message.received_at), t),
        }
    }

    /// [`Self::existed_at`] by id alone. An unknown id did not exist.
    pub fn existed_id(&self, id: i64, t: DateTime<Utc>) -> bool {
        match self
            .user_state
            .get(&id)
            .and_then(|row| row.created_at.as_deref())
        {
            Some(created) => at_or_before(Some(created), t),
            None => at_or_before(self.received_at.get(&id).map(String::as_str), t),
        }
    }

    /// [`Self::message_as_of`] on a serialized `AgentMessage` (a tool result),
    /// plus its thread's as-of attention revision.
    pub fn message_value_as_of(
        &self,
        value: &mut serde_json::Value,
        t: DateTime<Utc>,
        target: i64,
    ) {
        let Some(id) = value.get("id").and_then(|v| v.as_i64()) else {
            return;
        };
        let row = self.user_state.get(&id).cloned().unwrap_or_default();
        value["status"] = status_as_of(&row, t).into();
        if !at_or_before(value.get("opened_at").and_then(|v| v.as_str()), t) {
            value["opened_at"] = serde_json::Value::Null;
        }
        value["remind_at"] = remind_as_of(&row, t).into();
        if let Some(thread) = value
            .get("thread_id")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            && value.get("source").is_some_and(|v| v.is_object())
        {
            value["source"]["attention_revision"] =
                self.timeline.revision_as_of(&thread, t, target).into();
        }
    }

    /// Reset a message's explicit user state to what it was at `t`.
    pub fn message_as_of(&self, message: &mut AgentMessage, t: DateTime<Utc>) {
        let row = self
            .user_state
            .get(&message.id)
            .cloned()
            .unwrap_or_default();
        message.status = status_as_of(&row, t).into();
        if !at_or_before(message.opened_at.as_deref(), t) {
            message.opened_at = None;
        }
        message.remind_at = remind_as_of(&row, t);
    }

    /// The stored decision of `message` if it was made before `t`; never the
    /// target's own.
    pub fn decision_as_of(
        &self,
        message: i64,
        t: DateTime<Utc>,
        target: i64,
    ) -> Option<MessageDecision> {
        self.decisions
            .get(&message)
            .filter(|row| message != target && row.decided_at < t)
            .map(|row| row.decision.clone())
    }

    /// Was `addr` a contact at `t`?
    pub fn contact_at(&self, addr: &str, t: DateTime<Utc>) -> bool {
        self.contacts
            .get(&addr.trim().to_ascii_lowercase())
            .copied()
            .flatten()
            .is_some_and(|seen| seen <= t)
    }
}

/// `done` only if resolved by `t`; else `open` if it had surfaced (or a
/// reminder had fired) by `t`; else `new`. A done-then-reopened row has lost
/// its `resolved_at`, so an earlier resolution of it cannot be recovered.
pub fn status_as_of(row: &UserStateRow, t: DateTime<Utc>) -> &'static str {
    if at_or_before(row.resolved_at.as_deref(), t) {
        "done"
    } else if at_or_before(row.surfaced_at.as_deref(), t)
        || at_or_before(row.reminded_at.as_deref(), t)
    {
        "open"
    } else {
        "new"
    }
}

/// A pending reminder is visible at `t` only when it provably existed then:
/// setting one stamps `resolved_at` and later stamps only move it forward,
/// so `resolved_at <= t` dates the reminder before `t`. One already due at
/// `t` would have fired by then, so it is not pending.
pub fn remind_as_of(row: &UserStateRow, t: DateTime<Utc>) -> Option<String> {
    let remind = row.remind_at.as_deref()?;
    let pending = parse_ts(remind).is_some_and(|at| at > t);
    (pending && at_or_before(row.resolved_at.as_deref(), t)).then(|| remind.to_owned())
}

/// What the as-of filter changed, for fidelity and its notes.
#[derive(Debug, Clone, Default)]
pub struct AsOfReport {
    pub dropped_siblings: Vec<i64>,
    pub dropped_rules: Vec<i64>,
    pub contact_dropped: bool,
    pub dropped_corrections: Vec<String>,
    pub prior_attention_entries: usize,
    /// Messages left in the thread with more than one applied triage run.
    pub multi_run_messages: Vec<(i64, usize)>,
    /// Messages whose user state differs from the store's current state.
    pub user_state_changed: Vec<i64>,
}

impl AsOfReport {
    pub fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if !self.dropped_siblings.is_empty() {
            notes.push(format!(
                "dropped {} sibling(s) ingested after as_of: {:?}",
                self.dropped_siblings.len(),
                self.dropped_siblings
            ));
        }
        if !self.dropped_rules.is_empty() {
            notes.push(format!(
                "dropped matched rule(s) updated after as_of: {:?}",
                self.dropped_rules
            ));
        }
        if self.contact_dropped {
            notes.push("sender became a contact after as_of".into());
        }
        if !self.dropped_corrections.is_empty() {
            notes.push(format!(
                "dropped correction(s) made after as_of: {:?}",
                self.dropped_corrections
            ));
        }
        if self.prior_attention_entries > 0 {
            notes.push(format!(
                "thread attention replayed from {} earlier decision(s)",
                self.prior_attention_entries
            ));
        }
        for (id, runs) in &self.multi_run_messages {
            notes.push(format!(
                "message {id} has {runs} applied runs; its earlier decision is lost"
            ));
        }
        if !self.user_state_changed.is_empty() {
            notes.push(format!(
                "user state reset to as_of on {:?}",
                self.user_state_changed
            ));
        }
        notes
    }
}

/// Rewind a context loaded from the store (`load_agent_context` or
/// `agent_thread_context`) to `t`. `target` is the case's message, whose own
/// decision is never visible. The source fingerprints stay as the store
/// computed them, except `attention_revision`, which follows the timeline.
pub fn context_as_of(
    data: &AsOfData,
    context: &mut AgentContext,
    t: DateTime<Utc>,
    target: i64,
) -> AsOfReport {
    let mut report = AsOfReport::default();
    let anchor = context.message.id;
    context.thread.retain(|message| {
        let keep = message.id == anchor || data.existed_at(message, t);
        if !keep {
            report.dropped_siblings.push(message.id);
        }
        keep
    });
    let thread = context.message.thread_id.clone();
    let entries = data.timeline.before(&thread, t, target);
    report.prior_attention_entries = entries.len();
    context.attention = entries.last().map(|entry| entry.attention.clone());
    let revision = entries.len() as i64;
    context.revision.attention_revision = revision;
    context.previous_decision = data.decision_as_of(anchor, t, target);
    for message in std::iter::once(&mut context.message).chain(context.thread.iter_mut()) {
        let before = (
            message.status.clone(),
            message.opened_at.clone(),
            message.remind_at.clone(),
        );
        data.message_as_of(message, t);
        if before
            != (
                message.status.clone(),
                message.opened_at.clone(),
                message.remind_at.clone(),
            )
            && !report.user_state_changed.contains(&message.id)
        {
            report.user_state_changed.push(message.id);
        }
        if message.thread_id == thread {
            message.source.attention_revision = revision;
        }
    }
    let rule_known =
        |rule: &serde_json::Value| at_or_before(rule.get("updated_at").and_then(|v| v.as_str()), t);
    for rule in &context.matched_rules {
        if !rule_known(rule) {
            report
                .dropped_rules
                .push(rule.get("id").and_then(|v| v.as_i64()).unwrap_or(-1));
        }
    }
    context.matched_rules.retain(rule_known);
    context.rules.retain(rule_known);
    if context.sender_is_contact && !data.contact_at(&context.message.from_addr, t) {
        context.sender_is_contact = false;
        report.contact_dropped = true;
    }
    // THE RELATIONSHIP LEDGER HAS NO HISTORY: a row is today's recount, and
    // replaying it at `t` would hand the model replies the owner had not yet
    // written. Dropped whole, which is also what a context before the ledger
    // existed carried. A context load no longer fills it (only the worker
    // attaches it, under the flag), but this stays for contexts that already
    // carry one: an exported case deserialized with the field set.
    context.relationship = None;
    context.corrections.retain(|correction| {
        let field = correction
            .get("field")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned();
        let keep = at_or_before(
            data.correction_times
                .get(&(anchor, field.clone()))
                .map(String::as_str),
            t,
        );
        if !keep {
            report.dropped_corrections.push(field);
        }
        keep
    });
    let mut ids: Vec<i64> = context.thread.iter().map(|m| m.id).collect();
    ids.push(anchor);
    ids.sort_unstable();
    ids.dedup();
    for id in ids {
        let runs = data.applied_runs.get(&id).copied().unwrap_or(0);
        if runs > 1 {
            report.multi_run_messages.push((id, runs));
        }
    }
    report
}

/// Longest tool result kept in a trace, in characters.
pub const TRACE_RESULT_CHARS: usize = 4_000;

/// The replay's evidence reader: the production [`StoreEvidence`] over the
/// snapshot copy, with every result rewound to the case's T through the same
/// [`AsOfData`] `export` used.
///
/// The gold answer cannot come back through it: the target's own decision is
/// excluded from the timeline and from `decision_as_of`, `read_record` on the
/// target is refused, and every field that carries a decision (`attention`,
/// `previous_decision`, `records`) is REPLACED, never passed through.
pub struct AsOfReader<'a> {
    inner: StoreEvidence<'a, SqliteStore>,
    data: &'a AsOfData,
    target: i64,
    t: DateTime<Utc>,
    trace: Mutex<Vec<Value>>,
}

impl<'a> AsOfReader<'a> {
    pub fn new(
        store: &'a SqliteStore,
        data: &'a AsOfData,
        account: AccountId,
        target: i64,
        t: DateTime<Utc>,
        limit: usize,
    ) -> Self {
        Self {
            inner: StoreEvidence::new(store, account, limit),
            data,
            target,
            t,
            trace: Mutex::new(Vec::new()),
        }
    }

    /// Every request with its (bounded) result or error, in order.
    pub fn trace(&self) -> Vec<Value> {
        self.trace.lock().expect("reader trace").clone()
    }

    fn unavailable<T>() -> Result<T, String> {
        Err("evidence_unavailable".to_owned())
    }

    fn filtered(&self, request: &EvidenceRequest) -> Result<EvidenceResult, String> {
        let (t, target, data) = (self.t, self.target, self.data);
        match request {
            EvidenceRequest::ReadMessage { message_id, .. }
            | EvidenceRequest::ReadAttachment { message_id, .. }
                if !data.existed_id(*message_id, t) =>
            {
                return Self::unavailable();
            }
            EvidenceRequest::ReadRecord { message_id }
                if *message_id == target || !data.existed_id(*message_id, t) =>
            {
                return Self::unavailable();
            }
            _ => {}
        }
        let mut result = self.inner.read(request)?;
        match request {
            EvidenceRequest::ReadMessage { .. } => {
                data.message_value_as_of(&mut result.data["message"], t, target);
            }
            EvidenceRequest::ReadAttachment { .. } => {}
            EvidenceRequest::ReadRecord { message_id } => {
                result.data = json!({"records": data
                    .decision_as_of(*message_id, t, target)
                    .map(|decision| decision.records)});
            }
            EvidenceRequest::ReadThread { thread_id } => {
                let messages = self.existing_messages(&mut result);
                if messages.is_empty() {
                    return Self::unavailable();
                }
                // The store answered for its current representative; if
                // that message did not exist at T, its corrections are not
                // the as-of representative's.
                let current = representative(result.data["messages"].as_array());
                let as_of = representative(Some(&messages));
                let corrections = match (current, as_of) {
                    (Some(now), Some(then)) if now == then => result.data["user_corrections"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|correction| {
                            let field = correction["field"].as_str().unwrap_or_default();
                            at_or_before(
                                data.correction_times
                                    .get(&(then, field.to_owned()))
                                    .map(String::as_str),
                                t,
                            )
                        })
                        .cloned()
                        .collect(),
                    _ => Vec::new(),
                };
                let previous = as_of.and_then(|id| data.decision_as_of(id, t, target));
                result.data = json!({
                    "messages": messages,
                    "attention": data.timeline.attention_as_of(thread_id, t, target),
                    "user_corrections": corrections,
                    "previous_decision": previous,
                });
                result.thread_revisions = [(
                    thread_id.clone(),
                    data.timeline.revision_as_of(thread_id, t, target),
                )]
                .into_iter()
                .collect();
            }
            EvidenceRequest::SearchMail { .. } | EvidenceRequest::ReadSenderHistory { .. } => {
                let messages = self.existing_messages(&mut result);
                // Production reports the revision of the first message's thread.
                result.thread_revisions = messages
                    .first()
                    .and_then(|m| m["thread_id"].as_str())
                    .map(|thread| {
                        (
                            thread.to_owned(),
                            data.timeline.revision_as_of(thread, t, target),
                        )
                    })
                    .into_iter()
                    .collect();
                result.data = json!({"messages": messages});
            }
        }
        // Whatever the store reported, a revision is the timeline's as of T.
        for (thread, revision) in result.thread_revisions.iter_mut() {
            *revision = data.timeline.revision_as_of(thread, t, target);
        }
        Ok(result)
    }

    /// `result.data.messages` that existed at T, rewound; `source_message_ids`
    /// narrowed to match.
    fn existing_messages(&self, result: &mut EvidenceResult) -> Vec<Value> {
        let messages: Vec<Value> = result.data["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| {
                m["id"]
                    .as_i64()
                    .is_some_and(|id| self.data.existed_id(id, self.t))
            })
            .cloned()
            .map(|mut m| {
                self.data.message_value_as_of(&mut m, self.t, self.target);
                m
            })
            .collect();
        let kept: Vec<i64> = messages.iter().filter_map(|m| m["id"].as_i64()).collect();
        result.source_message_ids.retain(|id| kept.contains(id));
        messages
    }
}

/// `agent_thread_context`'s representative: inbound before own mail, then
/// not spam, then newest.
fn representative(messages: Option<&Vec<Value>>) -> Option<i64> {
    messages?
        .iter()
        .filter_map(|m| {
            Some((
                m["is_sent"].as_bool().unwrap_or(false),
                m["is_spam"].as_bool().unwrap_or(false),
                std::cmp::Reverse(m["received_at"].as_str().and_then(parse_ts)),
                std::cmp::Reverse(m["id"].as_i64()?),
            ))
        })
        .min()
        .map(|key| key.3.0)
}

impl EvidenceReader for AsOfReader<'_> {
    fn read(&self, request: &EvidenceRequest) -> Result<EvidenceResult, String> {
        let result = self.filtered(request);
        let logged = match &result {
            Ok(result) => {
                let text = serde_json::to_string(&result.data).unwrap_or_default();
                let bounded: String = text.chars().take(TRACE_RESULT_CHARS).collect();
                json!({"request": request, "result": bounded,
                    "truncated": text.chars().count() > TRACE_RESULT_CHARS,
                    "source_message_ids": result.source_message_ids})
            }
            Err(error) => json!({"request": request, "error": error}),
        };
        self.trace.lock().expect("reader trace").push(logged);
        result
    }
}

/// Open the snapshot copy for plain reads.
pub fn open_snapshot_sql(path: &std::path::Path) -> Result<Connection> {
    Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use squelch_core::triage::decision::RelatedAttentionUpdate;

    fn t(hours: i64) -> DateTime<Utc> {
        "2026-09-21T00:00:00Z".parse::<DateTime<Utc>>().unwrap() + chrono::Duration::hours(hours)
    }

    fn row(id: i64, thread: &str, at: i64, summary: &str) -> DecisionRow {
        let mut decision = MessageDecision::default();
        decision.attention.summary = summary.into();
        DecisionRow {
            message_id: id,
            thread_id: thread.into(),
            decided_at: t(at),
            access_only: false,
            decision,
        }
    }

    #[test]
    fn timeline_takes_latest_strictly_before_t_and_counts_updates() {
        let mut related = row(9, "other", 3, "own thread of 9");
        related
            .decision
            .related_updates
            .push(RelatedAttentionUpdate {
                thread_id: "x".into(),
                attention: ThreadAttentionDecision {
                    summary: "from 9 via related update".into(),
                    ..Default::default()
                },
                ..Default::default()
            });
        let mut spam = row(8, "x", 4, "spam never applies");
        spam.access_only = true;
        let rows = [
            row(2, "x", 5, "second"),
            row(1, "x", 1, "first"),
            related,
            spam,
        ];
        let timeline = Timeline::from_decisions(&rows, &HashMap::new());
        let summary = |at, target| {
            timeline
                .attention_as_of("x", t(at), target)
                .map(|a| a.summary)
        };
        assert_eq!(summary(1, 0), None, "strictly before");
        assert_eq!(summary(2, 0).as_deref(), Some("first"));
        assert_eq!(summary(4, 0).as_deref(), Some("from 9 via related update"));
        assert_eq!(timeline.revision_as_of("x", t(4), 0), 2);
        assert_eq!(summary(6, 0).as_deref(), Some("second"));
        assert_eq!(timeline.revision_as_of("x", t(6), 0), 3);
        // The target's own decision is never part of its past.
        assert_eq!(summary(6, 2).as_deref(), Some("from 9 via related update"));
        assert_eq!(timeline.revision_as_of("x", t(6), 2), 2);
        assert_eq!(timeline.revision_as_of("other", t(6), 0), 1);
    }

    /// Ids come from the next entry (by kind and description, then kind,
    /// then order) and, for the thread's last entry, from the committed
    /// attention; only what nothing reveals gets a stand-in.
    #[test]
    fn action_ids_are_recovered_not_invented() {
        use squelch_core::triage::decision::{ActionKind, AttentionAction};
        let action = |id: Option<&str>, kind, description: &str| AttentionAction {
            id: id.map(str::to_owned),
            kind,
            description: description.into(),
            ..Default::default()
        };
        let mut first = row(1, "x", 1, "first");
        first.decision.attention.actions = vec![
            action(None, ActionKind::Pay, "pay rent"),
            action(None, ActionKind::Pay, "pay water"),
            action(None, ActionKind::Other, "unreferenced"),
            action(None, ActionKind::Reply, "reply to Ann"),
        ];
        // Description decides between the two Pay actions; kind alone
        // decides the reworded Reply, even past an earlier unmatched slot.
        let mut second = row(2, "x", 2, "second");
        second.decision.attention.actions = vec![
            action(Some("real-reply"), ActionKind::Reply, "answer Ann"),
            action(Some("real-water"), ActionKind::Pay, "pay water"),
            action(Some("real-rent"), ActionKind::Pay, "pay rent"),
            action(None, ActionKind::Decide, "sign the lease"),
        ];
        // An id already known (real-rent) is never handed out again; the
        // re-kinded new id goes by order.
        let mut third = row(3, "x", 3, "third");
        third.decision.attention.actions = vec![
            action(Some("real-rent"), ActionKind::Pay, "pay rent"),
            action(Some("real-sign"), ActionKind::Other, "lease signature"),
            action(None, ActionKind::Attend, "viewing"),
        ];
        let mut committed_attention = third.decision.attention.clone();
        committed_attention.actions[2].id = Some("real-attend".into());
        let committed = HashMap::from([("x".to_owned(), committed_attention)]);
        let timeline = Timeline::from_decisions(&[first, second, third], &committed);
        let ids = |at| {
            timeline
                .attention_as_of("x", t(at), 0)
                .unwrap()
                .actions
                .into_iter()
                .map(|a| a.id.unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(2),
            vec!["real-rent", "real-water", "act-1-2", "real-reply"]
        );
        assert_eq!(
            ids(3),
            vec!["real-reply", "real-water", "real-rent", "real-sign"]
        );
        assert_eq!(ids(4), vec!["real-rent", "real-sign", "real-attend"]);
        // A committed row that is not this entry's action list is not used.
        let mut stale = committed["x"].clone();
        stale.actions[2].description = "something else".into();
        let rows = [row(9, "y", 1, "only")];
        let mut only = rows[0].clone();
        only.decision.attention = committed["x"].clone();
        only.decision.attention.actions[2].id = None;
        let other = Timeline::from_decisions(&[only], &HashMap::from([("y".to_owned(), stale)]));
        let attention = other.attention_as_of("y", t(2), 0).unwrap();
        assert_eq!(attention.actions[2].id.as_deref(), Some("act-9-2"));
    }

    #[test]
    fn user_state_rewinds_to_t() {
        let state = UserStateRow {
            surfaced_at: Some(t(2).to_rfc3339()),
            resolved_at: Some(t(4).to_rfc3339()),
            remind_at: Some(t(10).to_rfc3339()),
            ..Default::default()
        };
        assert_eq!(status_as_of(&state, t(1)), "new");
        assert_eq!(status_as_of(&state, t(3)), "open");
        assert_eq!(status_as_of(&state, t(5)), "done");
        // The reminder was set by the time it resolved, and pends until due.
        assert_eq!(remind_as_of(&state, t(3)), None);
        assert_eq!(remind_as_of(&state, t(5)), state.remind_at);
        assert_eq!(remind_as_of(&state, t(11)), None);
        let fired = UserStateRow {
            reminded_at: Some(t(2).to_rfc3339()),
            ..Default::default()
        };
        assert_eq!(status_as_of(&fired, t(3)), "open");
        assert!(!at_or_before(Some("not a time"), t(100)));
        assert_eq!(parse_ts("2026-09-21 01:00:00"), Some(t(1)));
    }
}
