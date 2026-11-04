//! Atomic persistence for model-owned decisions and durable work.
use super::SqliteStore;
use crate::error::{CoreError, Result};
use crate::store::agent_triage::*;
use crate::triage::decision::{MessageDecision, MessageDestination, ThreadAttentionDecision};
use crate::triage::{agent_config::RankingConfig, ranking::rank};
use crate::types::AccountId;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

// New arrivals retain first priority. Explicit manual work comes before old
// import/migration work, without changing which budget pays for each job.
// `archive_access` shares that tier and nothing else of `manual:`: an agent
// is waiting on the answer, so it must not queue behind a cutover. ONLY WHILE
// IT HAS NEVER BEEN PUSHED BACK: `last_error` is written by every deferral and
// retry and cleared by neither a claim nor the passing of time, so a check
// parked on a spent budget comes back at midnight (or on a grant within its
// sub-cap's room) as ordinary work on the access lane instead of at the head
// of the queue, ahead of the owner's own sent mail. Within a tier and lane, a
// past-event re-check (`revisit:event_ended:%`) goes first: every `revisit:`
// job shares one daily cap (`__agent_revisit__`), and a landed flight's
// re-check must not wait days behind the agent's other revisits. Those jobs
// are few (the scheduler asks only about events the owner went to, at most
// three times a message), so going ahead of other background work costs it
// little.
const CLAIM_JOB_SQL: &str = "SELECT j.id,j.message_id,j.trigger,j.attempts,j.arrival_eligible,j.kind,COALESCE(lane.foreground,0),j.last_error
             FROM agent_triage_jobs j INDEXED BY idx_agent_jobs_pending_claim JOIN messages candidate ON candidate.account_id=j.account_id AND candidate.id=j.message_id
             LEFT JOIN agent_job_lanes lane ON lane.job_id=j.id
             WHERE j.account_id=?1 AND j.state IN ('queued','leased') AND (j.kind=?2 OR (?2 IN ('investigation','initial_investigation') AND j.kind IN ('triage','access')))
               AND (?2!='initial_investigation' OR j.trigger NOT LIKE 'revisit:%')
               AND j.available_at<=?3 AND (j.state='queued' OR (j.state='leased' AND j.lease_until<=?3))
               AND (j.kind NOT IN ('triage','access') OR NOT EXISTS(
                   SELECT 1 FROM message_translations tr WHERE tr.message_id=j.message_id
                     AND tr.status='pending' AND tr.triage_wait_until>?4))
               AND (j.kind NOT IN ('triage','access') OR NOT EXISTS(
                   SELECT 1 FROM agent_triage_jobs active JOIN messages other ON other.account_id=active.account_id AND other.id=active.message_id
                   WHERE active.account_id=j.account_id AND active.kind IN ('triage','access')
                     AND active.state='leased' AND active.lease_until>?3 AND active.id!=j.id
                     AND other.thread_id=candidate.thread_id))
             ORDER BY CASE
                 WHEN COALESCE(lane.foreground,0)=1 AND j.arrival_eligible=1 THEN 0
                 WHEN j.trigger LIKE 'manual:%' OR (j.trigger='archive_access' AND j.last_error IS NULL) THEN 1
                 ELSE 2 END,
                 COALESCE(lane.foreground,0) DESC,
                 j.trigger NOT LIKE 'revisit:event_ended:%',j.available_at,j.id LIMIT 1";

fn json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| CoreError::Other(e.into()))
}
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(|e| CoreError::Other(e.into()))
}
fn fingerprint<T: serde::Serialize>(value: &T) -> Result<String> {
    let digest = Sha256::digest(json(value)?.as_bytes());
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}
const MESSAGE_COLUMNS: &str = concat!(
    "m.id, m.thread_id, m.from_addr, m.subject, m.body, m.received_at,
    ",
    own_mail_sql!(),
    ", m.is_spam, COALESCE(t.status,'new'), t.opened_at, t.remind_at,
    t.notify_eligible_at, COALESCE((SELECT
             revision FROM agent_thread_attention a WHERE a.account_id=m.account_id AND
             a.thread_id=m.thread_id),0),COALESCE((SELECT group_concat(field || value_json || revision)
             FROM agent_triage_corrections c WHERE c.account_id=m.account_id AND c.message_id=m.id),''),
    m.is_sent,
    (SELECT tr.subject FROM message_translations tr WHERE tr.message_id=m.id
             AND tr.status IN ('done','incomplete')),
    (SELECT tr.body FROM message_translations tr WHERE tr.message_id=m.id
             AND tr.status IN ('done','incomplete')),
    m.list_unsubscribe IS NOT NULL, m.sender_signals"
);
fn message_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentMessage> {
    let mut message = AgentMessage {
        source: AgentSourceSnapshot {
            message_id: row.get(0)?,
            content: String::new(),
            user_state: String::new(),
            attention_revision: row.get(12)?,
        },
        id: row.get(0)?,
        thread_id: row.get(1)?,
        from_addr: row.get(2)?,
        subject: row.get(3)?,
        body: row.get(4)?,
        received_at: row.get(5)?,
        is_sent: row.get(6)?,
        is_spam: row.get(7)?,
        labelled_sent: row.get(14)?,
        is_bulk: row.get(17)?,
        status: row.get(8)?,
        opened_at: row.get(9)?,
        remind_at: row.get(10)?,
        notify_eligible_at: row.get(11)?,
        // The machine translation rides along for the model to READ, and it
        // IS in `content_snapshot` below (docs/TRANSLATION.md, Pipeline): the
        // access assessment must cover what the agent door will serve, so a
        // translation landing after triage re-triages the message once.
        translated_subject: row.get(15)?,
        translated_body: row.get(16)?,
        sender_signals: crate::types::SenderSignals::from_column(
            row.get::<_, Option<String>>(18)?.as_deref(),
        ),
    };
    // Only serde primitives are hashed, so serialization cannot fail here.
    message.source.content = content_snapshot(&message).expect("serializable message");
    message.source.user_state = fingerprint(&(
        message.id,
        message.status == "done",
        &message.opened_at,
        &message.remind_at,
        row.get::<_, String>(13)?,
    ))
    .expect("serializable user state");
    Ok(message)
}
/// ARCHIVE MAIL IS NOT EVIDENCE UNTIL SOMEBODY ASKED FOR IT. True of every live
/// row, and of an archive row only once an explicit surfacing has had it
/// assessed as allowed, or while work that somebody explicitly queued for it is
/// genuinely in flight: leased, or queued, due and never pushed back
/// (`last_error` NULL, see `CLAIM_JOB_SQL`). A PARKED JOB GRANTS NOTHING, or a
/// check waiting on tomorrow's budget would make the row evidence all day and
/// hide every live decision that then cited it. Every read the triage
/// agent's tools make carries this, and it is what keeps the tier free: a row
/// the agent is never shown is a row [`ensure_source_assessments`] is never
/// asked to assess. Written against the alias `m`.
///
/// A macro beside the `const` for the same reason `own_mail_sql!` is one:
/// static SQL can `concat!` it.
macro_rules! archive_evidence_sql {
    () => {
        "(m.is_archive=0 \
         OR EXISTS(SELECT 1 FROM agent_message_state s WHERE s.account_id=m.account_id \
             AND s.message_id=m.id AND s.access='allowed') \
         OR EXISTS(SELECT 1 FROM agent_triage_jobs j WHERE j.account_id=m.account_id \
             AND j.message_id=m.id AND j.kind IN ('triage','access') \
             AND (j.state='leased' OR (j.state='queued' AND j.last_error IS NULL \
                 AND julianday(j.available_at)<=julianday('now')))))"
    };
}
const ARCHIVE_EVIDENCE_SQL: &str = archive_evidence_sql!();

/// [`read_message`] for the triage agent's evidence tools: an archive row
/// nobody has asked about does not exist (see [`ARCHIVE_EVIDENCE_SQL`]).
fn read_evidence_message(conn: &Connection, account: AccountId, id: i64) -> Result<AgentMessage> {
    let readable: bool = conn
        .prepare_cached(&format!(
            "SELECT EXISTS(SELECT 1 FROM messages m
             WHERE m.account_id=?1 AND m.id=?2 AND {ARCHIVE_EVIDENCE_SQL})"
        ))?
        .query_row(params![account, id], |row| row.get(0))?;
    if !readable {
        return Err(CoreError::NotFound);
    }
    read_message(conn, account, id)
}
fn read_message(conn: &Connection, account: AccountId, id: i64) -> Result<AgentMessage> {
    conn.prepare_cached(&format!(
        "SELECT {MESSAGE_COLUMNS} FROM messages m LEFT JOIN triage t ON t.message_id=m.id AND
             t.account_id=m.account_id WHERE m.account_id=?1 AND m.id=?2"
    ))?
    .query_row(params![account, id], message_row)
    .optional()?
    .ok_or(CoreError::NotFound)
}
fn content_snapshot(message: &AgentMessage) -> Result<String> {
    let base = (
        message.id,
        &message.thread_id,
        &message.from_addr,
        &message.subject,
        &message.body,
        &message.received_at,
        message.is_sent,
        message.is_spam,
    );
    // THE SERVED TRANSLATION IS CONTENT (docs/TRANSLATION.md): the agent door
    // serves it beside the body, so the access assessment must have read it.
    // Hashed ONLY WHEN THERE IS ONE, so every message without a translation
    // keeps exactly the snapshot it always had and nothing re-triages
    // fleet-wide when this ships.
    // SENDER SIGNALS ARE DELIBERATELY NOT HASHED, an exception to the rule
    // above: the agent door serves them beside the body, but they are only
    // addresses and domains already visible in the headers the assessment
    // read (From, plus what the sender wrote), bounded at ingest, and hashing
    // them would re-triage every message whose row gains signals on a
    // re-ingest. Revisit if a signal ever carries free text.
    match (&message.translated_subject, &message.translated_body) {
        (None, None) => fingerprint(&base),
        (subject, body) => fingerprint(&(base, subject, body)),
    }
}
fn read_thread(
    conn: &Connection,
    account: AccountId,
    thread: &str,
    limit: usize,
) -> Result<Vec<AgentMessage>> {
    // The thread as the triage agent sees it: old archive siblings nobody has
    // asked about are absent, so a reply to a 2025 conversation does not buy
    // an assessment of the whole of it.
    let mut stmt = conn.prepare(&format!("SELECT {MESSAGE_COLUMNS} FROM messages m LEFT JOIN triage t ON t.message_id=m.id AND
             t.account_id=m.account_id WHERE m.account_id=?1 AND m.thread_id=?2 AND {ARCHIVE_EVIDENCE_SQL}
             ORDER BY m.received_at DESC,m.id DESC LIMIT ?3"))?;
    let rows = stmt
        .query_map(
            params![account, thread, limit.min(1000) as i64],
            message_row,
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}
fn rules(conn: &Connection, account: AccountId) -> Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(
        "SELECT id,match_pattern,want_text,disposition,updated_at FROM sender_rules WHERE
             account_id=?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map([account], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, i64>(0)?,
                "match_pattern": row.get::<_, String>(1)?,
                "want_text": row.get::<_, String>(2)?,
                "disposition": row.get::<_, String>(3)?,
                "updated_at": row.get::<_, String>(4)?,
            }))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}
fn context(conn: &Connection, account: AccountId, id: i64) -> Result<AgentContext> {
    let message = read_message(conn, account, id)?;
    let thread = read_thread(conn, account, &message.thread_id, 1000)?;
    let rules = rules(conn, account)?;
    let corrections = corrections(conn, account, id)?;
    let sender_is_contact = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM contacts WHERE account_id=?1 AND addr=?2 COLLATE NOCASE)",
        params![account, message.from_addr],
        |row| row.get(0),
    )?;
    let matched_rules: Vec<serde_json::Value> = rules
        .iter()
        .filter(|rule| {
            crate::triage::rules::glob_match(
                rule["match_pattern"].as_str().unwrap_or(""),
                &message.from_addr,
            )
        })
        .cloned()
        .collect();
    let previous:Option<String>=conn.query_row("SELECT decision_json FROM agent_message_decisions WHERE account_id=?1 AND message_id=?2",
        params![account,id],
        |r|r.get(0)).optional()?;
    let attention:Option<(String,i64)>=conn.query_row("SELECT attention_json,revision FROM agent_thread_attention WHERE account_id=?1 AND thread_id=?2",
        params![account,message.thread_id],
        |r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let content = thread
        .iter()
        .map(content_snapshot)
        .collect::<Result<Vec<_>>>()?;
    let state: Vec<_> = thread
        .iter()
        .map(|m| (m.id, m.status == "done", &m.opened_at, &m.remind_at))
        .collect();
    let revision = ContextRevision {
        content: fingerprint(&content)?,
        preferences: fingerprint(&(&matched_rules, sender_is_contact, &corrections))?,
        user_state: fingerprint(&state)?,
        attention_revision: attention.as_ref().map(|a| a.1).unwrap_or(0),
    };
    Ok(AgentContext {
        message,
        thread,
        previous_decision: previous.as_deref().map(decode).transpose()?,
        attention: attention.as_ref().map(|a| decode(&a.0)).transpose()?,
        rules,
        matched_rules,
        sender_is_contact,
        corrections,
        revision,
        memory: vec![],
        run_metadata: serde_json::json!({}),
    })
}

fn corrections(
    conn: &Connection,
    account: AccountId,
    message: i64,
) -> Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(
        "SELECT c.field,c.value_json,c.revision FROM agent_triage_corrections c
             WHERE c.account_id=?1 AND c.message_id=?2 ORDER BY c.field",
    )?;
    let rows = stmt
        .query_map(params![account, message], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(field, value, revision)| {
            Ok(serde_json::json!({
                "field": field,
                "value": decode::<serde_json::Value>(&value)?,
                "revision": revision,
            }))
        })
        .collect()
}
fn apply_list_delta<T: serde::de::DeserializeOwned + PartialEq>(
    current: &mut Vec<T>,
    value: &serde_json::Value,
) -> Result<()> {
    let read = |key: &str| -> Result<Vec<T>> {
        serde_json::from_value(value[key].clone())
            .map_err(|_| CoreError::InvalidInput(format!("invalid correction {key} values")))
    };
    let add = read("add")?;
    let remove = read("remove")?;
    if add.iter().any(|item| remove.contains(item)) {
        return Err(CoreError::InvalidInput(
            "correction cannot add and remove the same value".into(),
        ));
    }
    current.retain(|item| !remove.contains(item));
    for item in add {
        if !current.contains(&item) {
            current.push(item);
        }
    }
    Ok(())
}

fn apply_correction(
    decision: &mut MessageDecision,
    field: &str,
    value: &serde_json::Value,
) -> Result<()> {
    use crate::triage::decision::{EmailKind, MessageDestination};
    let reading_basis = |d: &MessageDecision| {
        (
            d.kinds.contains(&EmailKind::Editorial),
            d.kinds.contains(&EmailKind::Promotional),
            d.destinations.contains(&MessageDestination::Reading),
        )
    };
    let before = reading_basis(decision);
    match field {
        "kinds" if value.is_object() => apply_list_delta(&mut decision.kinds, value)?,
        "destinations" if value.is_object() => apply_list_delta(&mut decision.destinations, value)
            .map_err(|_| CoreError::InvalidInput("Reading is the only selectable destination; record groups come from typed facts".into()))?,
        "kinds" => {
            let kinds: Vec<crate::triage::decision::EmailKind> = decode(&value.to_string())?;
            if kinds.is_empty() {
                return Err(CoreError::InvalidInput("kinds cannot be empty".into()));
            }
            decision.kinds = kinds;
        }
        "destinations" => decision.destinations = decode(&value.to_string())
            .map_err(|_| CoreError::InvalidInput("Reading is the only selectable destination; record groups come from typed facts".into()))?,
        "show_in_fye" => {
            decision.attention.show_in_fye = value
                .as_bool()
                .ok_or_else(|| CoreError::InvalidInput("show_in_fye must be a boolean".into()))?
        }
        "external_access" => {
            decision.external_access.restricted = value.as_bool().ok_or_else(|| {
                CoreError::InvalidInput("external_access must be a restriction boolean".into())
            })?;
            decision.external_access.reason = "Explicit user correction".into();
            decision.external_access.evidence.clear();
        }
        _ => return Err(CoreError::InvalidInput("unknown correction field".into())),
    }
    // The model's content-vs-promotion call was made against the kinds and
    // destinations it chose. A correction that changes what it rests on
    // (editorial, promotional, or Reading membership) outranks it: clear it and
    // let clients read the corrected kinds. Unrelated edits leave it alone,
    // since the correction row is replayed over every later re-triage too.
    if reading_basis(decision) != before {
        decision.reading_kind = None;
    }
    Ok(())
}
pub(super) fn explicit_access_override(
    conn: &Connection,
    account: AccountId,
    message: i64,
) -> Result<Option<bool>> {
    let encoded: Option<String> = conn.query_row("SELECT value_json FROM agent_triage_corrections WHERE account_id=?1 AND message_id=?2 AND field='external_access'",
        params![account,message],|row|row.get(0)).optional()?;
    encoded.as_deref().map(decode).transpose()
}

fn thread_fye_override(
    conn: &Connection,
    account: AccountId,
    thread: &str,
) -> Result<Option<bool>> {
    let explicit: Option<bool> = conn
        .prepare_cached(
            "SELECT show_in_fye FROM agent_thread_preferences WHERE account_id=?1 AND thread_id=?2",
        )?
        .query_row(params![account, thread], |row| row.get(0))
        .optional()?;
    if explicit.is_some() {
        return Ok(explicit);
    }
    // Existing beta corrections predate the thread preference table.
    let encoded: Option<String> = conn.prepare_cached("SELECT c.value_json FROM agent_triage_corrections c JOIN messages m ON m.account_id=c.account_id AND m.id=c.message_id
        WHERE c.account_id=?1 AND m.thread_id=?2 AND c.field='show_in_fye' ORDER BY c.updated_at DESC,c.revision DESC,c.message_id DESC LIMIT 1")?
        .query_row(params![account,thread],|row|row.get(0)).optional()?;
    encoded.as_deref().map(decode).transpose()
}

fn apply_corrections(
    conn: &Connection,
    account: AccountId,
    message: i64,
    decision: &mut MessageDecision,
) -> Result<()> {
    for correction in corrections(conn, account, message)? {
        apply_correction(
            decision,
            correction["field"].as_str().unwrap_or(""),
            &correction["value"],
        )?;
    }
    let thread = read_message(conn, account, message)?.thread_id;
    if let Some(show) = thread_fye_override(conn, account, &thread)? {
        decision.attention.show_in_fye = show;
    }
    Ok(())
}
fn is_archive_conn(conn: &Connection, account: AccountId, message: i64) -> Result<bool> {
    Ok(conn
        .prepare_cached("SELECT is_archive FROM messages WHERE account_id=?1 AND id=?2")?
        .query_row(params![account, message], |row| row.get(0))
        .optional()?
        .unwrap_or(false))
}

/// PROMOTION: the one way `messages.is_archive` ever changes, and only from 1
/// to 0. The OWNER, through the human door, asked for this one archive message
/// to be triaged or corrected how it is filed, so from here on it is an
/// ordinary live row: full triage, revisits, counts, and the STRICT thread
/// guard (a promoted row whose access is still pending closes its thread at
/// the agent door like any live pending row).
///
/// EXACTLY TWO CALLERS, both inside the caller's own transaction:
/// `retriage_reset`'s single-message arm and [`correct_agent_triage_conn`] for
/// any field but `external_access`. Nothing at the agent door, in the walk, in
/// a live re-sighting or in a heal reaches either. It touches the flag and
/// nothing else. What happens to the row's access verdict afterwards is what
/// happens to any live row's: the triage the caller queues re-judges it, so a
/// model's 'restricted' may become 'allowed'; only the owner's own seal
/// (`external_access` corrected to true) outlives that.
///
/// NEVER THE OWNER'S OWN MAIL. Sent mail has no verdict to redo (see
/// `OWN_MAIL`), so there is nothing a promotion would buy; and a promoted
/// sent row is live and pending, which closes its thread at the agent door
/// over archive siblings that door could read a moment before. Returns
/// whether the row was promoted.
///
/// A caller that promotes then queues the row's triage and calls
/// [`requeue_promoted_conn`], in that order.
pub(super) fn promote_archive_conn(
    conn: &Connection,
    account: AccountId,
    message: i64,
) -> Result<bool> {
    let promoted = conn.execute(
        concat!(
            "UPDATE messages AS m SET is_archive=0
             WHERE m.account_id=?1 AND m.id=?2 AND m.is_archive=1 AND NOT ",
            own_mail_sql!()
        ),
        params![account, message],
    )? == 1;
    // Live from here on, so translated eagerly like live mail: a parked
    // translation is queued, and the triage the caller queues waits for it.
    if promoted {
        super::translations::promote_live_translation_conn(conn, message, Utc::now())?;
    }
    Ok(promoted)
}

/// A PROMOTED ROW IS LEFT WITH WORK THAT CAN RUN. Call after the promotion's
/// own enqueue. That enqueue merges into whatever job the row already has at
/// its current revision, and deliberately keeps a FAILED job failed (a
/// correction to a live row must not reset its retry state). For a row just
/// promoted that job is the surfacing check an agent's search once bought:
/// left failed, the row would be live, pending and closing its thread at the
/// agent door with nothing ever due to judge it. The same for a job that
/// completed and left the access pending. Requeued here from zero attempts;
/// nothing is touched while a job for the row is queued or running.
pub(super) fn requeue_promoted_conn(
    conn: &Connection,
    account: AccountId,
    message: i64,
    now: DateTime<Utc>,
) -> Result<()> {
    conn.execute(
        "UPDATE agent_triage_jobs SET state='queued',attempts=0,available_at=?3,last_error=NULL,
             lease_token=NULL,lease_until=NULL
         WHERE id=(SELECT j.id FROM agent_triage_jobs j
             JOIN agent_message_state a ON a.account_id=j.account_id AND a.message_id=j.message_id
             WHERE j.account_id=?1 AND j.message_id=?2 AND j.input_revision=a.revision
               AND j.kind IN ('triage','access') AND j.trigger NOT LIKE 'revisit:%'
               AND (j.state='failed' OR (j.state='completed' AND a.access='pending'))
             ORDER BY j.id DESC LIMIT 1)
           AND NOT EXISTS(SELECT 1 FROM agent_triage_jobs live
               WHERE live.account_id=?1 AND live.message_id=?2
                 AND live.kind IN ('triage','access') AND live.state IN ('queued','leased'))",
        params![account, message, now.to_rfc3339()],
    )?;
    Ok(())
}

pub(super) fn correct_agent_triage_conn(
    conn: &Connection,
    account: AccountId,
    message: i64,
    field: &str,
    value: &serde_json::Value,
    now: DateTime<Utc>,
) -> Result<()> {
    // Validate against the current effective classification before mutating jobs or intent.
    let encoded: Option<String> = conn.query_row(
        "SELECT decision_json FROM agent_message_decisions WHERE account_id=?1 AND message_id=?2",
        params![account,message], |row| row.get(0)).optional()?;
    let mut candidate: MessageDecision = encoded
        .as_deref()
        .map(decode)
        .transpose()?
        .unwrap_or_default();
    apply_correction(&mut candidate, field, value)?;
    if field == "kinds" && candidate.kinds.is_empty() {
        return Err(CoreError::InvalidInput("kinds cannot be empty".into()));
    }
    // A correction to how an ARCHIVE message is filed is the owner asking for
    // it to be treated as live mail: promote it, then queue the same triage
    // any corrected row gets. Sealing or unsealing one (`external_access`) is
    // not that. It is a statement about who may read the row, it is applied
    // below without a model, and the row stays in the archive with no job.
    //
    // An archive row that is NOT promoted (the owner's own sent mail) gets the
    // correction recorded and no job either: nothing may queue work on a row
    // still in the archive from here.
    let archived = is_archive_conn(conn, account, message)?;
    let promoted =
        archived && field != "external_access" && promote_archive_conn(conn, account, message)?;
    if !archived || promoted {
        let trigger = format!("correction:{}", now.to_rfc3339());
        enqueue_agent_triage_conn(conn, account, message, &trigger, false)?;
    }
    if promoted {
        requeue_promoted_conn(conn, account, message, now)?;
    }
    let revision: i64 = conn.query_row(
        "SELECT revision FROM agent_message_state WHERE account_id=?1 AND message_id=?2",
        params![account, message],
        |r| r.get(0),
    )?;
    conn.execute("INSERT INTO
             agent_triage_corrections(account_id,message_id,field,value_json,source_revision,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(account_id,message_id,field) DO UPDATE SET
             value_json=excluded.value_json,source_revision=excluded.source_revision,revision=agent_triage_corrections.revision+1,updated_at=excluded.updated_at",
        params![account,message,field,json(value)?,revision,now.to_rfc3339()])?;
    if field == "show_in_fye" {
        let thread = read_message(conn, account, message)?.thread_id;
        conn.execute("INSERT INTO agent_thread_preferences(account_id,thread_id,show_in_fye,updated_at)
             VALUES(?1,?2,?3,?4) ON CONFLICT(account_id,thread_id) DO UPDATE SET
             show_in_fye=excluded.show_in_fye,revision=agent_thread_preferences.revision+1,updated_at=excluded.updated_at",
            params![account,thread,value.as_bool().unwrap(),now.to_rfc3339()])?;
        // Visibility is a human preference, not new model evidence. Preserve the
        // representative, provenance, actions and activity even before the
        // representative has a classification or after it becomes spam.
        // The owner's say also ends a first-load hold: the thread is theirs now.
        conn.execute(
            "UPDATE agent_thread_attention SET show_in_fye=?3,held=0,
             attention_json=json_set(attention_json,'$.show_in_fye',json(?4)),
             revision=revision+1 WHERE account_id=?1 AND thread_id=?2",
            params![account, thread, value.as_bool().unwrap(), json(value)?],
        )?;
    }
    let previous:Option<String>=conn.query_row("SELECT decision_json FROM agent_message_decisions WHERE account_id=?1 AND message_id=?2",
        params![account,message],
        |r|r.get(0)).optional()?;
    if let Some(previous) = previous {
        let mut decision: MessageDecision = decode(&previous)?;
        apply_corrections(conn, account, message, &mut decision)?;
        conn.execute("UPDATE agent_message_decisions SET decision_json=?3 WHERE account_id=?1 AND message_id=?2",
        params![account,message,json(&decision)?])?;
        super::translations::close_if_untranslatable_conn(conn, account, message, now)?;
        if field == "destinations" {
            conn.execute(
                "DELETE FROM agent_message_destinations WHERE account_id=?1 AND message_id=?2",
                params![account, message],
            )?;
            for destination in decision.destinations {
                let name = match destination {
                    MessageDestination::Reading => "reading",
                };
                conn.execute("INSERT OR IGNORE INTO agent_message_destinations(account_id,message_id,destination) VALUES(?1,?2,?3)",
        params![account,message,name])?;
            }
        }
    }
    if field == "external_access" {
        conn.execute(
            "UPDATE agent_message_state SET access=?3 WHERE account_id=?1 AND message_id=?2",
            params![
                account,
                message,
                if value.as_bool() == Some(true) {
                    "restricted"
                } else {
                    "allowed"
                }
            ],
        )?;
        // The user's restriction (or lifting of it) applies to ghosts now,
        // from the corrected decision written above.
        super::ghosts::refresh_message_guarded(conn, account, message)?;
    }
    // A correction can SEAL (a restriction, an auth kind): the vectors go at
    // the event, in this transaction (`vector_forbidden_sql!`).
    super::search::purge_forbidden_vectors_conn(conn, Some(message))?;
    Ok(())
}

/// Merge immediate work without resetting its retry state. Autonomous timers
/// remain separate requests governed by the bounded revisit scheduler.
#[allow(clippy::too_many_arguments)] // One durable work request and its scheduling metadata.
fn queue_investigation(
    conn: &Connection,
    account: AccountId,
    message: i64,
    revision: i64,
    kind: &str,
    trigger: &str,
    eligible: bool,
    foreground: bool,
    requested_foreground: bool,
) -> Result<()> {
    let existing: Option<(i64,String,String,String)> = conn.query_row(
        "SELECT id,state,kind,trigger FROM agent_triage_jobs WHERE account_id=?1 AND message_id=?2 AND input_revision=?3
         AND kind IN ('triage','access') AND trigger NOT LIKE 'revisit:%'
         ORDER BY CASE state WHEN 'leased' THEN 0 WHEN 'queued' THEN 1 ELSE 2 END,id DESC LIMIT 1",
        params![account,message,revision],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
    ).optional()?;
    if let Some((id, state, existing_kind, existing_trigger)) = existing {
        let kind = if kind == "triage" || existing_kind == "triage" {
            "triage"
        } else {
            "access"
        };
        if state == "leased" {
            // A human's one-message request keeps its foreground lane through
            // the wait, whichever way the running job ends.
            let lane = requested_foreground && kind == "triage";
            conn.execute("INSERT INTO agent_triage_followups(job_id,kind,trigger,arrival_eligible,foreground) VALUES(?1,?2,?3,?4,?5)
                ON CONFLICT(job_id) DO UPDATE SET kind=CASE WHEN kind='triage' OR excluded.kind='triage' THEN 'triage' ELSE 'access' END,
                trigger=excluded.trigger,arrival_eligible=MAX(arrival_eligible,excluded.arrival_eligible),
                foreground=MAX(foreground,excluded.foreground)",params![id,kind,trigger,eligible,lane])?;
            return Ok(());
        }
        if ((state == "completed" && existing_trigger != trigger)
            || (state == "failed" && trigger.starts_with("manual:") && existing_trigger != trigger))
            && !matches!(
                trigger,
                "ingest"
                    | "arrival"
                    | "backfill"
                    | "heal"
                    | "translation"
                    | "source_access"
                    | "archive_access"
            )
        {
            conn.execute("UPDATE agent_triage_jobs SET state='queued',kind=?2,trigger=?3,attempts=0,available_at=?4,last_error=NULL WHERE id=?1",
                params![id,kind,trigger,Utc::now().to_rfc3339()])?;
            // A requeued job is refresh work, so background, unless a human
            // asked for this one message by hand.
            let lane = requested_foreground && kind == "triage";
            conn.execute("INSERT INTO agent_job_lanes(job_id,foreground) VALUES(?1,?2) ON CONFLICT(job_id) DO UPDATE SET foreground=excluded.foreground",params![id,lane])?;
            return Ok(());
        }
        if state == "completed" {
            return Ok(());
        }
        if state == "queued" || state == "failed" {
            // Keep backoff, attempts and terminal failure intact across trigger
            // churn. A real content revision creates a new input instead.
            conn.execute("UPDATE agent_triage_jobs SET kind=?2,trigger=?3,arrival_eligible=MAX(arrival_eligible,?4) WHERE id=?1",
                params![id,kind,trigger,eligible])?;
            // The one exception: a job parked on the daily budget is not
            // backoff. A human re-asking for this message moves it to the
            // foreground budget, which may well have room, so it may run now.
            // Other deferrals (provider cooldown, retries) keep their schedule.
            if requested_foreground && kind == "triage" && state == "queued" {
                conn.execute(
                    "UPDATE agent_triage_jobs SET available_at=?2,last_error=NULL
                    WHERE id=?1 AND last_error='daily_budget_exhausted'",
                    params![id, Utc::now().to_rfc3339()],
                )?;
            }
            conn.execute("INSERT INTO agent_job_lanes(job_id,foreground) VALUES(?1,?2) ON CONFLICT(job_id) DO UPDATE SET foreground=MAX(foreground,excluded.foreground)",params![id,foreground])?;
            return Ok(());
        }
    }
    conn.execute("INSERT INTO agent_triage_jobs(account_id,message_id,kind,trigger,input_revision,arrival_eligible,available_at)
        VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT DO NOTHING",
        params![account,message,kind,trigger,revision,eligible,Utc::now().to_rfc3339()])?;
    conn.execute("INSERT INTO agent_job_lanes(job_id,foreground)
        SELECT id,?6 FROM agent_triage_jobs WHERE account_id=?1 AND message_id=?2 AND kind=?3 AND trigger=?4 AND input_revision=?5
        ON CONFLICT(job_id) DO UPDATE SET foreground=MAX(foreground,excluded.foreground)",params![account,message,kind,trigger,revision,foreground])?;
    Ok(())
}

/// The `last_error` a job may carry: a bounded machine code, never provider
/// errors or body text.
fn job_error_code(error_code: &str) -> String {
    error_code
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(64)
        .collect()
}

/// The one way a leased job goes back to the queue (a retry, a deferral, a
/// hand-back): the lease cleared, `last_error` written when the caller has
/// a code and kept when it has none, this lease's attempt refunded when the
/// job never ran (a deferral) and kept when it did. Once, by the lease's
/// owner: a stale worker cannot change the attempt count or the schedule
/// after another worker has claimed the job.
fn requeue(
    conn: &Connection,
    job: &AgentJob,
    available_at: DateTime<Utc>,
    last_error: Option<&str>,
    refund_attempt: bool,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE agent_triage_jobs SET state='queued',available_at=?1,
             last_error=COALESCE(?2,last_error),
             attempts=CASE WHEN ?6 THEN MAX(attempts-1,0) ELSE attempts END,
             lease_token=NULL,lease_until=NULL
         WHERE id=?3 AND account_id=?4 AND state='leased' AND lease_token=?5",
        params![
            available_at.to_rfc3339(),
            last_error,
            job.id,
            job.account_id,
            job.lease_token,
            refund_attempt
        ],
    )? == 1;
    if changed {
        absorb_followup(conn, job)?;
    }
    Ok(changed)
}

fn absorb_followup(conn: &Connection, job: &AgentJob) -> Result<()> {
    // The job goes back to the queue carrying the follow-up. A human's
    // one-message request makes it foreground, and, exactly as a re-ask on a
    // queued job does, lifts a daily-budget park (never other backoff).
    let requested: bool = conn
        .query_row(
            "SELECT foreground FROM agent_triage_followups WHERE job_id=?1",
            [job.id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false);
    if requested {
        conn.execute("INSERT INTO agent_job_lanes(job_id,foreground) VALUES(?1,1) ON CONFLICT(job_id) DO UPDATE SET foreground=1",[job.id])?;
        conn.execute(
            "UPDATE agent_triage_jobs SET available_at=?2,last_error=NULL
             WHERE id=?1 AND state='queued' AND last_error='daily_budget_exhausted'",
            params![job.id, Utc::now().to_rfc3339()],
        )?;
    }
    conn.execute("UPDATE agent_triage_jobs SET
        kind=CASE WHEN kind='triage' OR (SELECT kind FROM agent_triage_followups WHERE job_id=?1)='triage' THEN 'triage' ELSE kind END,
        trigger=COALESCE((SELECT trigger FROM agent_triage_followups WHERE job_id=?1),trigger),
        arrival_eligible=MAX(arrival_eligible,COALESCE((SELECT arrival_eligible FROM agent_triage_followups WHERE job_id=?1),0))
        WHERE id=?1",[job.id])?;
    conn.execute(
        "DELETE FROM agent_triage_followups WHERE job_id=?1",
        [job.id],
    )?;
    Ok(())
}

fn release_followup(conn: &Connection, job: &AgentJob) -> Result<()> {
    let followup: Option<(String, String, bool, bool)> = conn
        .query_row(
            "SELECT kind,trigger,arrival_eligible,foreground FROM agent_triage_followups WHERE job_id=?1",
            [job.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    conn.execute(
        "DELETE FROM agent_triage_followups WHERE job_id=?1",
        [job.id],
    )?;
    if let Some((kind, trigger, eligible, requested)) = followup {
        let revision: i64 = conn.query_row(
            "SELECT input_revision FROM agent_triage_jobs WHERE id=?1",
            [job.id],
            |row| row.get(0),
        )?;
        queue_investigation(
            conn,
            job.account_id,
            job.message_id,
            revision,
            &kind,
            &trigger,
            eligible,
            requested,
            requested,
        )?;
    }
    Ok(())
}

/// The archive walk's half of [`enqueue_agent_triage_lane`]: the state row and
/// nothing behind it. Same snapshot function, revision 1, access pending, so
/// the row reads as unchanged to any later touch and stays closed to the agent
/// door until something explicit asks for its assessment. Insert-only: the
/// caller has just inserted the message, and a row that somehow has state keeps
/// it.
pub(crate) fn seed_archive_state_conn(
    conn: &Connection,
    account: AccountId,
    message: i64,
) -> Result<()> {
    let snapshot = content_snapshot(&read_message(conn, account, message)?)?;
    conn.execute(
        "INSERT INTO agent_message_state(account_id,message_id,content_snapshot,revision,access)
             VALUES(?1,?2,?3,1,'pending') ON CONFLICT(account_id,message_id) DO NOTHING",
        params![account, message, snapshot],
    )?;
    Ok(())
}

/// An archive row's stored snapshot brought up to the row as it now stands
/// (its served translation landed or left) WITHOUT queuing anything: a new
/// revision, access `pending`, so the agent door fails closed on it and the
/// first explicit surfacing assesses what is actually there. A no-op when
/// the snapshot has not moved, or the row has no state yet.
pub(crate) fn reseed_archive_snapshot_conn(
    conn: &Connection,
    account: AccountId,
    message: i64,
) -> Result<()> {
    let snapshot = content_snapshot(&read_message(conn, account, message)?)?;
    conn.execute(
        "UPDATE agent_message_state SET content_snapshot=?3,revision=revision+1,access='pending'
         WHERE account_id=?1 AND message_id=?2 AND content_snapshot!=?3",
        params![account, message, snapshot],
    )?;
    if let Some(restricted) = explicit_access_override(conn, account, message)? {
        conn.execute(
            "UPDATE agent_message_state SET access=?3 WHERE account_id=?1 AND message_id=?2",
            params![
                account,
                message,
                if restricted { "restricted" } else { "allowed" }
            ],
        )?;
    }
    // New content awaits its assessment: its vectors go until it has one.
    super::search::purge_forbidden_vectors_conn(conn, Some(message))?;
    Ok(())
}

/// Bring `message`'s stored content snapshot in line with the row as it now
/// stands, WITHOUT a new revision, an access reset or a job: for a change the
/// agent must not be asked about again. The one caller is a sealed message
/// losing its translation, where sealing is terminal.
pub(crate) fn refresh_snapshot_in_place_conn(
    conn: &Connection,
    account: AccountId,
    message: i64,
) -> Result<()> {
    let snapshot = content_snapshot(&read_message(conn, account, message)?)?;
    conn.execute(
        "UPDATE agent_message_state SET content_snapshot=?3
         WHERE account_id=?1 AND message_id=?2 AND content_snapshot!=?3",
        params![account, message, snapshot],
    )?;
    Ok(())
}

/// Call inside the message ingest transaction. Duplicate provider fetches leave
/// revisions and notification eligibility untouched.
pub(crate) fn enqueue_agent_triage_conn(
    conn: &Connection,
    account: AccountId,
    message: i64,
    trigger: &str,
    arrival_eligible: bool,
) -> Result<()> {
    enqueue_agent_triage_lane(conn, account, message, trigger, arrival_eligible, false)
}

/// [`enqueue_agent_triage_conn`] with an explicit foreground request: a human
/// asked for this ONE message, so its triage job is charged to the global
/// budget only and claimed ahead of background work, and a queued job parked
/// on `daily_budget_exhausted` is un-parked. Bulk work never passes `true`, so
/// one click cannot spend the arrival reserve on a whole window of mail.
pub(crate) fn enqueue_agent_triage_lane(
    conn: &Connection,
    account: AccountId,
    message: i64,
    trigger: &str,
    arrival_eligible: bool,
    requested_foreground: bool,
) -> Result<()> {
    let m = read_message(conn, account, message)?;
    let snapshot = content_snapshot(&m)?;
    let previous:Option<(String,i64)>=conn.query_row("SELECT content_snapshot,revision FROM agent_message_state WHERE account_id=?1 AND message_id=?2",
        params![account,message],
        |r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let changed = previous.as_ref().is_none_or(|p| p.0 != snapshot);
    // `heal` sits with the ingest family: a re-read whose stored content did
    // not actually change is a no-op, never a re-run.
    if !changed
        && matches!(
            trigger,
            "arrival" | "ingest" | "backfill" | "heal" | "translation"
        )
    {
        if trigger != "backfill"
            && previous.as_ref().is_some_and(|p| p.1 == 1)
            && !m.is_sent
            && !m.is_spam
        {
            conn.execute("INSERT INTO agent_job_lanes(job_id,foreground)
                SELECT j.id,1 FROM agent_triage_jobs j WHERE j.account_id=?1 AND j.message_id=?2
                  AND j.input_revision=1 AND j.kind='triage' AND j.state IN ('queued','leased')
                  AND NOT EXISTS(SELECT 1 FROM agent_message_decisions d WHERE d.account_id=?1 AND d.message_id=?2)
                ON CONFLICT(job_id) DO UPDATE SET foreground=1",params![account,message])?;
        }
        return Ok(());
    }
    let revision = previous
        .as_ref()
        .map(|p| p.1 + i64::from(changed))
        .unwrap_or(1);
    conn.execute(
        "INSERT INTO agent_message_state(account_id,message_id,content_snapshot,revision,access)
             VALUES(?1,?2,?3,?4,'pending') ON CONFLICT(account_id,message_id) DO UPDATE SET
             content_snapshot=excluded.content_snapshot,revision=excluded.revision,access=CASE WHEN
             agent_message_state.content_snapshot!=excluded.content_snapshot THEN 'pending' ELSE
             agent_message_state.access END",
        params![account, message, snapshot, revision],
    )?;
    if let Some(restricted) = explicit_access_override(conn, account, message)? {
        conn.execute(
            "UPDATE agent_message_state SET access=?3 WHERE account_id=?1 AND message_id=?2",
            params![
                account,
                message,
                if restricted { "restricted" } else { "allowed" }
            ],
        )?;
    }
    // CHANGED CONTENT IS UNDECIDED CONTENT: a vector computed before the change
    // is of text no decision has read, so it goes until one does
    // (`vector_forbidden_sql!`). A no-op for a new message, which has none.
    if changed {
        super::search::purge_forbidden_vectors_conn(conn, Some(message))?;
    }
    // Healing an unclassified arrival must not move its first classification
    // into the migration budget. Once any decision exists, later work is refresh.
    let unclassified_arrival: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_triage_jobs j JOIN agent_job_lanes l ON l.job_id=j.id
             WHERE j.account_id=?1 AND j.message_id=?2 AND l.foreground=1)
         AND NOT EXISTS(SELECT 1 FROM agent_message_decisions WHERE account_id=?1 AND message_id=?2)",
        params![account,message], |row| row.get(0),
    )?;
    let now = Utc::now().to_rfc3339();
    let pending_arrival: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_triage_jobs WHERE account_id=?1 AND message_id=?2
         AND arrival_eligible=1 AND state IN ('queued','leased'))",
        params![account, message],
        |r| r.get(0),
    )?;
    let pending_notification: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_triage_jobs WHERE account_id=?1 AND message_id=?2
         AND kind='notification' AND arrival_eligible=1 AND state IN ('queued','leased'))",
        params![account, message],
        |r| r.get(0),
    )?;
    if changed {
        conn.execute("UPDATE agent_triage_jobs SET state='completed',lease_token=NULL,lease_until=NULL WHERE
             account_id=?1 AND message_id=?2 AND input_revision!=?3 AND state IN ('queued','leased')",
        params![account,message,revision])?;
        conn.execute("DELETE FROM agent_triage_followups WHERE job_id IN (SELECT id FROM agent_triage_jobs WHERE account_id=?1 AND message_id=?2 AND input_revision!=?3)",params![account,message,revision])?;
    }
    // Both access triggers are a yes/no on one row somebody asked to read,
    // never a placement: an archive row gets no FYE verdict and no push.
    // WHATEVER THE TRIGGER, A ROW STILL IN THE ARCHIVE GETS AN ACCESS CHECK AND
    // NEVER A TRIAGE. The two paths that mean "triage this" promote the row
    // first (see `promote_archive_conn`), so by here it is no longer archive;
    // anything else that finds its way to one (`not_spam`, a refresh) must not
    // write destinations, records or revisits onto mail nobody asked about.
    let access_only = matches!(trigger, "source_access" | "archive_access")
        || is_archive_conn(conn, account, message)?;
    let kind = if m.is_sent || m.is_spam || access_only {
        "access"
    } else {
        "triage"
    };
    if m.is_sent && !m.is_spam {
        retire_inbound_verdict(conn, account, message)?;
    }
    let new_arrival = arrival_eligible && previous.is_none();
    let eligible = (new_arrival || (changed && pending_arrival)) && !m.is_sent && !m.is_spam;
    let foreground = kind == "triage"
        && (requested_foreground
            || unclassified_arrival
            || (revision == 1 && matches!(trigger, "ingest" | "arrival")));
    if trigger.starts_with("revisit:") {
        conn.execute("INSERT INTO agent_triage_jobs(account_id,message_id,kind,trigger,input_revision,arrival_eligible,available_at)
            VALUES(?1,?2,?3,?4,?5,0,?6) ON CONFLICT DO NOTHING",params![account,message,kind,trigger,revision,now])?;
    } else {
        queue_investigation(
            conn,
            account,
            message,
            revision,
            kind,
            trigger,
            eligible,
            foreground,
            requested_foreground,
        )?;
    }
    if eligible && (new_arrival || pending_notification) {
        conn.execute("INSERT INTO
             agent_triage_jobs(account_id,message_id,kind,trigger,input_revision,arrival_eligible,available_at)
             VALUES(?1,?2,'notification','arrival',?3,1,?4) ON CONFLICT DO NOTHING",
        params![account,message,revision,now])?;
    }
    // A sent reply is evidence that can resolve an existing obligation. It gets
    // no placement or arrival push of its own; re-evaluate the inbound target.
    if changed && m.is_sent && !m.is_spam && !access_only && !m.thread_id.is_empty() {
        let target: Option<i64> = conn
            .query_row(
                concat!(
                    "SELECT m.id FROM messages m WHERE m.account_id=?1 AND m.thread_id=?2 AND NOT ",
                    own_mail_sql!(),
                    // Never an archive row: a reply into an old conversation
                    // re-evaluates live mail or nothing.
                    " AND m.is_spam=0 AND m.is_archive=0
             ORDER BY m.received_at DESC,m.id DESC LIMIT 1"
                ),
                params![account, m.thread_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(target) = target {
            enqueue_agent_triage_conn(
                conn,
                account,
                target,
                &format!("thread_changed:{message}:{revision}"),
                false,
            )?;
        }
    }
    if changed {
        queue_dependent_refreshes(conn, account, message, revision)?;
    }
    Ok(())
}
/// The account's own mail is never placed, so whatever a triage run decided
/// about it while it was being read as inbound goes when it is filed
/// access-only. Nothing would ever refresh it otherwise: an access commit
/// writes `access` alone, so the decision's source rows would stay pinned to a
/// revision the message has left, and the external door reads a stale source
/// as "changed" and fails closed. That would take the message, its thread and
/// everything that cites it off the agent door for good.
///
/// A no-op for a stored sent copy, which never had a verdict to retire.
fn retire_inbound_verdict(conn: &Connection, account: AccountId, message: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM agent_decision_sources WHERE account_id=?1 AND message_id=?2",
        params![account, message],
    )?;
    conn.execute(
        "DELETE FROM agent_message_destinations WHERE account_id=?1 AND message_id=?2",
        params![account, message],
    )?;
    let previous: Option<String> = conn
        .query_row(
            "SELECT decision_json FROM agent_message_decisions WHERE account_id=?1 AND message_id=?2",
            params![account, message],
            |r| r.get(0),
        )
        .optional()?;
    let Some(previous) = previous else {
        return Ok(());
    };
    conn.execute(
        "DELETE FROM agent_message_decisions WHERE account_id=?1 AND message_id=?2",
        params![account, message],
    )?;
    // Shipments are derived from the decisions still standing. One that no
    // longer decodes proposed nothing a current reader could have used.
    //
    // BEST EFFORT, behind a savepoint. The rebuild re-reads every decision in
    // the account and fails on the first it cannot decode, and this runs inside
    // whatever carried the enqueue: an ingest transaction, or the store's own
    // open. A package card left standing is a far smaller cost than mail that
    // will not ingest or a daemon that will not start.
    if let Ok(previous) = decode::<MessageDecision>(&previous) {
        conn.execute_batch("SAVEPOINT retire_shipments")?;
        let rebuilt = super::agent_deliveries::reconcile(
            conn,
            account,
            message,
            Some(&previous),
            &MessageDecision::default(),
        );
        if let Err(error) = &rebuilt {
            conn.execute_batch("ROLLBACK TO retire_shipments")?;
            eprintln!("squelch: shipments not rebuilt after retiring a verdict: {error}");
        }
        conn.execute_batch("RELEASE retire_shipments")?;
    }
    Ok(())
}

/// What [`queue_dependent_refreshes`] refreshes when message `?2` of account
/// `?1` takes a new revision: every message whose decision cites it, the
/// message behind every thread attention that cites it, and theirs in turn,
/// since each refreshed consumer gets a new verdict of its own. Never the
/// source itself, and never archive mail, though the walk passes through
/// archive mail to what cites it.
///
/// SEEDED FROM THE SOURCE, one hop at a time down the reverse indexes. The
/// statement this replaces built the account's whole edge set (every decision
/// source and every attention source) and then walked it: 235 ms per changed
/// ingest at 50k messages, inside the ingest transaction, however few
/// dependents the message had. Walking from the source costs what its
/// dependents cost: 0.06 ms for a fresh arrival nothing cites yet.
///
/// PINNED, both the join order and the indexes. Without `sqlite_stat1` (see
/// `rules::SENDER_RULE_DIGEST_SQL`) the planner puts the recursive row INSIDE
/// each step's join; given the order, it still prefers the covering primary
/// keys on `account_id` alone. Either way every step walks every source row in
/// the account: 2.4 s for a message with 129 dependents, ten times what this
/// replaced. `CROSS JOIN` keeps the step's one row outermost, and `INDEXED BY`
/// keeps the seek and turns a missing index into a prepare error rather than
/// a walk. `tests::dependent_refreshes_walk_from_the_source` pins the plan.
const DEPENDENT_REFRESHES_SQL: &str = concat!(
    "WITH RECURSIVE affected(message_id) AS (
        SELECT ?2
        UNION SELECT d.message_id FROM affected a
            CROSS JOIN agent_decision_sources d INDEXED BY idx_agent_decision_source_reverse
                ON d.account_id=?1 AND d.source_message_id=a.message_id
        UNION SELECT t.message_id FROM affected a
            CROSS JOIN agent_attention_sources s INDEXED BY idx_agent_attention_source_reverse
                ON s.account_id=?1 AND s.source_message_id=a.message_id
            CROSS JOIN agent_thread_attention t ON t.account_id=s.account_id AND t.thread_id=s.thread_id
     ) SELECT m.id,st.revision,CASE WHEN ",
    own_mail_sql!(),
    " OR m.is_spam=1 THEN 'access' ELSE 'triage' END
       FROM affected dep CROSS JOIN messages m ON m.account_id=?1 AND m.id=dep.message_id
       JOIN agent_message_state st ON st.account_id=m.account_id AND st.message_id=m.id
       WHERE m.id!=?2 AND m.is_archive=0 ORDER BY m.id",
);

/// A source revision invalidates its consumers' model results, never their
/// human corrections. The existing per-message queue absorbs overlapping waves.
/// Never an archive dependent: a changed source must not buy a model call on
/// old mail that happens to cite it.
fn queue_dependent_refreshes(
    conn: &Connection,
    account: AccountId,
    source: i64,
    revision: i64,
) -> Result<()> {
    let targets = {
        let mut statement = conn.prepare_cached(DEPENDENT_REFRESHES_SQL)?;
        statement
            .query_map(params![account, source], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    let trigger = format!("source_changed:{source}:{revision}");
    for (message, input_revision, kind) in targets {
        queue_investigation(
            conn,
            account,
            message,
            input_revision,
            &kind,
            &trigger,
            false,
            false,
            false,
        )?;
    }
    Ok(())
}

fn replace_attention_sources(
    conn: &Connection,
    account: AccountId,
    thread: &str,
    sources: &[AgentSourceSnapshot],
) -> Result<()> {
    conn.execute(
        "DELETE FROM agent_attention_sources WHERE account_id=?1 AND thread_id=?2",
        params![account, thread],
    )?;
    for source in sources {
        let revision: i64 = conn
            .query_row(
                "SELECT revision FROM agent_message_state WHERE account_id=?1 AND message_id=?2",
                params![account, source.message_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        conn.execute("INSERT OR IGNORE INTO agent_attention_sources(account_id,thread_id,source_message_id,source_revision) VALUES(?1,?2,?3,?4)",
            params![account,thread,source.message_id,revision])?;
    }
    Ok(())
}

/// The messages a thread's attention was read from, or `None` when one of them
/// has moved past the revision the attention read: then the attention is
/// stale and nothing it says may go out the agent door.
fn current_attention_sources(
    conn: &Connection,
    account: AccountId,
    thread: &str,
) -> Result<Option<Vec<i64>>> {
    let mut statement=conn.prepare_cached("SELECT s.source_message_id,s.source_revision,a.revision FROM agent_attention_sources s
        LEFT JOIN agent_message_state a ON a.account_id=s.account_id AND a.message_id=s.source_message_id
        WHERE s.account_id=?1 AND s.thread_id=?2")?;
    let sources = statement
        .query_map(params![account, thread], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut ids = Vec::with_capacity(sources.len());
    for (id, expected, actual) in sources {
        if actual != Some(expected) {
            return Ok(None);
        }
        ids.push(id);
    }
    Ok(Some(ids))
}

/// Revisit limits apply to the message's entire history, not just its current
/// content revision. Manual requests never reset the autonomous lifetime cap.
fn schedule_revisit(
    conn: &Connection,
    job: &AgentJob,
    revision: i64,
    decision: &MessageDecision,
    policy: &crate::config::RevisitPassConfig,
    now: DateTime<Utc>,
) -> Result<()> {
    let Some(revisit) = decision.revisit.as_ref().filter(|_| policy.enabled) else {
        return Ok(());
    };
    let earliest = now + policy.min_lead();
    let latest = now + policy.max_horizon();
    let at = revisit.at.max(earliest);
    if at > latest || policy.max_per_message == 0 || policy.max_per_message_lifetime == 0 {
        return Ok(());
    }
    let (lifetime, pending): (i64,i64) = conn.query_row(
        "SELECT COUNT(*),COALESCE(SUM(CASE WHEN state IN ('queued','leased') AND id!=?3 THEN 1 ELSE 0 END),0)
         FROM agent_triage_jobs WHERE account_id=?1 AND message_id=?2 AND trigger LIKE 'revisit:%'",
        params![job.account_id,job.message_id,job.id],|r|Ok((r.get(0)?,r.get(1)?)),
    )?;
    if lifetime >= i64::from(policy.max_per_message_lifetime)
        || pending >= policy.max_per_message as i64
    {
        return Ok(());
    }
    let radius = policy.dedupe_window();
    let duplicate: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_triage_jobs WHERE account_id=?1 AND message_id=?2
         AND trigger LIKE 'revisit:%' AND available_at>=?3 AND available_at<=?4)",
        params![
            job.account_id,
            job.message_id,
            (at - radius).to_rfc3339(),
            (at + radius).to_rfc3339()
        ],
        |r| r.get(0),
    )?;
    if !duplicate {
        conn.execute("INSERT INTO agent_triage_jobs(account_id,message_id,kind,trigger,input_revision,arrival_eligible,available_at)
            VALUES(?1,?2,'triage',?3,?4,0,?5) ON CONFLICT DO NOTHING",
            params![job.account_id,job.message_id,format!("revisit:{}",at.to_rfc3339()),revision,at.to_rfc3339()])?;
    }
    Ok(())
}

/// The most of a model-authored done reason the `auto_done` ledger keeps.
pub(super) const AUTO_DONE_REASON_CHARS: usize = 300;

/// What became of a decision's `done`: applied, or the rail that refused it.
/// Codes only, so a log line can carry one: never the model's reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DoneOutcome {
    Applied,
    Refused(&'static str),
}

/// THE AGENT'S DONE, past the rails. The model judges whether a message
/// still needs anything from the owner; these are the few things it is never
/// trusted with, checked here in the decision's own transaction:
///
/// - only a scheduled re-check (`revisit:` trigger) may close mail: the model
///   was asked that question then, and no other run;
/// - the triage row is not already `done`. An `open` row (the owner undid
///   their own Done, or pulled it back from the done list) still closes: a
///   flight email kept handy before the flight is still unwanted after it.
///   The agent's own done, undone, stays undone by the `auto_done` rail;
/// - no reminder is pending or has fired: the owner set one, which is intent;
/// - the message is received mail, not spam and not the archive tier;
/// - neither it nor any message in its thread is the owner's own mail
///   ([`super::OWN_MAIL`]): that is correspondence (`''` is no thread);
/// - it was never auto-done before (its `auto_done` row), so an owner who
///   reopened it and an undo that put it back to `new` both keep it;
/// - every word in its thread is its own sender's ([`foreign_words`]): email
///   text is never trusted, so nobody else's reply can lead to a done.
///
/// APPLIED, IT IS THE OWNER'S DONE, to the letter: the same thread-wide
/// statement ([`super::attention::DONE_THREAD_SQL`]), because the bands show a
/// thread by its representative, which need not be the re-checked message, and
/// a done on one row would leave the thread in FYE. So the rails above hold for
/// EVERY message in the thread, not only the re-checked one, under the same
/// codes: any sibling that carries a reminder, is sent, spam or archive, or
/// was auto-done before refuses the whole done. A sibling already `done`
/// passes (the owner closed it; the owner's Done would close it again) and
/// gets no ledger row; every row the write moved from `new` or `open` gets
/// one, so
/// the rule digest and the never-twice rail see each of them. Refused,
/// nothing is written and the decision still commits.
fn apply_done(
    conn: &Connection,
    job: &AgentJob,
    decision: &MessageDecision,
    now: DateTime<Utc>,
) -> Result<Option<DoneOutcome>> {
    let Some(done) = decision.done.as_ref() else {
        return Ok(None);
    };
    let refuse = |code| Ok(Some(DoneOutcome::Refused(code)));
    if !job.trigger.starts_with("revisit:") {
        return refuse("not_revisit");
    }
    // The re-checked message and every message in its thread, with its triage
    // row if it has one (a row without one is never written, but its mail is
    // still the thread's and still read by the sent/spam/archive rail).
    let mut thread = conn.prepare_cached(
        "SELECT m.id,t.status,
                COALESCE(t.remind_at IS NOT NULL OR t.reminded_at IS NOT NULL,0),
                m.is_sent,m.is_spam,m.is_archive,
                EXISTS(SELECT 1 FROM auto_done l
                       WHERE l.account_id=m.account_id AND l.message_id=m.id)
         FROM messages me
         JOIN messages m ON m.account_id=me.account_id
                        AND (m.id=me.id OR (me.thread_id!='' AND m.thread_id=me.thread_id))
         LEFT JOIN triage t ON t.account_id=m.account_id AND t.message_id=m.id
         WHERE me.account_id=?1 AND me.id=?2
         ORDER BY m.id",
    )?;
    let rows = thread
        .query_map(params![job.account_id, job.message_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, bool>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, bool>(4)?,
                r.get::<_, bool>(5)?,
                r.get::<_, bool>(6)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    match rows.iter().find(|row| row.0 == job.message_id) {
        Some((_, Some(status), ..)) if status == "done" => return refuse("already_done"),
        Some((_, Some(_), ..)) => {}
        _ => return refuse("no_triage_row"),
    }
    if rows.iter().any(|row| row.2) {
        return refuse("reminder");
    }
    if rows.iter().any(|row| row.3 || row.4 || row.5) {
        return refuse("sent_spam_or_archive");
    }
    if rows.iter().any(|row| row.6) {
        return refuse("auto_done_before");
    }
    let own: bool = conn.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM messages me JOIN messages m ON m.account_id=me.account_id
                 AND (m.id=me.id OR (me.thread_id!='' AND m.thread_id=me.thread_id))
                 WHERE me.account_id=?1 AND me.id=?2 AND {})",
            super::OWN_MAIL
        ),
        params![job.account_id, job.message_id],
        |r| r.get(0),
    )?;
    if own {
        return refuse("own_mail_in_thread");
    }
    if let Some(code) = foreign_words(conn, job.account_id, job.message_id)? {
        return refuse(code);
    }
    let at = now.to_rfc3339();
    conn.execute(
        super::attention::AGENT_DONE_THREAD_SQL,
        params![at, job.account_id, job.message_id],
    )?;
    let reason = crate::text::truncate_chars(done.reason.trim(), AUTO_DONE_REASON_CHARS);
    // Only rows this write closed: the Done spares a sibling that holds FYE,
    // and a spared row given a ledger row would read as auto-done before.
    let mut ledger = conn.prepare_cached(
        "INSERT INTO auto_done(account_id,message_id,source,reason,done_at)
         SELECT account_id,message_id,'agent_revisit',?3,?4 FROM triage
         WHERE account_id=?1 AND message_id=?2 AND status='done' AND resolved_at=?4",
    )?;
    for (id, ..) in rows
        .iter()
        .filter(|row| matches!(row.1.as_deref(), Some("new") | Some("open")))
    {
        ledger.execute(params![job.account_id, id, reason, at])?;
    }
    Ok(Some(DoneOutcome::Applied))
}

/// Whether anyone but the message's own sender has words in its thread, as
/// the rail code that refuses an auto-done for it, or None when nobody does.
/// The owner's rule: email text is never trusted, so a done may rest only on
/// mail from the one sender the re-checked message speaks for. That is its
/// [`crate::triage::ghost::sender_scope`]: the authenticated organisation
/// domain, or the full address on a shared domain. A message with no scope
/// (not authenticated) speaks for nobody: `sender_unauthenticated`. A sibling
/// whose scope is any other, or none: `foreign_sender_in_thread`. Thread `''`
/// is no thread. The owner's own mail is its own rail (`own_mail_in_thread`).
pub(super) fn foreign_words(
    conn: &Connection,
    account: AccountId,
    message: i64,
) -> Result<Option<&'static str>> {
    use crate::triage::ghost::sender_scope;
    let Some((from, auth, thread)) = conn
        .query_row(
            "SELECT from_addr,auth_pass,thread_id FROM messages WHERE account_id=?1 AND id=?2",
            params![account, message],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<bool>>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
    else {
        return Ok(Some("sender_unauthenticated"));
    };
    let Some(scope) = sender_scope(&from, auth) else {
        return Ok(Some("sender_unauthenticated"));
    };
    if thread.is_empty() {
        return Ok(None);
    }
    let mut siblings = conn.prepare_cached(
        "SELECT from_addr,auth_pass FROM messages WHERE account_id=?1 AND thread_id=?2 AND id!=?3",
    )?;
    let senders = siblings
        .query_map(params![account, thread, message], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<bool>>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if senders
        .iter()
        .any(|(from, auth)| sender_scope(from, *auth).as_deref() != Some(scope.as_str()))
    {
        return Ok(Some("foreign_sender_in_thread"));
    }
    Ok(None)
}

/// Assess only legacy sources actually read as evidence. Their revision must
/// exist before the model consumes a snapshot or a derivative records provenance.
pub(super) fn ensure_agent_source_ids_conn(
    conn: &Connection,
    account: AccountId,
    ids: &[i64],
) -> Result<()> {
    let messages = ids
        .iter()
        .map(|id| read_message(conn, account, *id))
        .collect::<Result<Vec<_>>>()?;
    ensure_source_assessments(conn, account, &messages)
}

/// NEVER ASSESSES AN ARCHIVE ROW, whoever is asking. The triage agent's own
/// evidence reads and a human opening a message are unprompted, and unprompted
/// work on the archive is a model call per message over a year of mail. An
/// agent-door caller who names an archive row is prompted, but that request
/// has ceilings this function knows nothing about: it goes through
/// [`request_archive_access_conn`] and nowhere else.
fn ensure_source_assessments(
    conn: &Connection,
    account: AccountId,
    messages: &[AgentMessage],
) -> Result<()> {
    // Reads first, then every enqueue under ONE commit. An enqueue is three
    // or four writes, and a thread read that queued ten sources paid each one
    // as its own autocommit transaction: thirty fsyncs at `synchronous=FULL`,
    // under the store mutex, on a READ (measured at 97 ms for ten on a
    // file-backed store; 31 ms and one fsync with one commit). The common
    // case, nothing to queue, never takes the write lock.
    let mut wanted = Vec::new();
    for message in messages {
        if source_assessment_wanted(conn, account, message.id)? {
            wanted.push(message.id);
        }
    }
    if wanted.is_empty() {
        return Ok(());
    }
    in_write_transaction(conn, |tx| {
        for id in &wanted {
            // Asked again inside the transaction, because an enqueue can queue
            // work for a LATER source in the same batch (a dependent's refresh
            // through `queue_dependent_refreshes`), and that source is then
            // active and must be skipped, exactly as the one-at-a-time loop
            // skipped it. The answer can also have changed under a caller
            // that took its time between the two phases.
            if source_assessment_wanted(tx, account, *id)? {
                enqueue_agent_triage_conn(tx, account, *id, "source_access", false)?;
            }
        }
        Ok(())
    })
}

/// Does reading this source as evidence call for an access job: not an
/// archive row (see [`ensure_source_assessments`]), no assessment yet or one
/// still pending, and no job already queued, leased, or finished against the
/// current revision.
fn source_assessment_wanted(conn: &Connection, account: AccountId, message: i64) -> Result<bool> {
    if is_archive_conn(conn, account, message)? {
        return Ok(false);
    }
    let access: Option<String> = conn
        .prepare_cached(
            "SELECT access FROM agent_message_state WHERE account_id=?1 AND message_id=?2",
        )?
        .query_row(params![account, message], |row| row.get(0))
        .optional()?;
    if access.as_deref().is_some_and(|state| state != "pending") {
        return Ok(false);
    }
    let active: bool = conn
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM agent_triage_jobs WHERE account_id=?1 AND message_id=?2
         AND kind IN ('triage','access') AND state IN ('queued','leased'))",
        )?
        .query_row(params![account, message], |row| row.get(0))?;
    let terminal: bool = conn.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM agent_triage_jobs j JOIN agent_message_state a ON a.account_id=j.account_id AND a.message_id=j.message_id
         WHERE j.account_id=?1 AND j.message_id=?2 AND j.kind IN ('triage','access') AND j.state IN ('failed','completed') AND j.input_revision=a.revision)",
    )?.query_row(params![account,message],|row|row.get(0))?;
    Ok(!active && !terminal)
}

/// Run `f` under ONE write transaction: the caller's when one is open (the
/// `_conn` helpers compose, and SQLite refuses a nested BEGIN), otherwise an
/// IMMEDIATE one of its own, committed when `f` returns Ok and rolled back
/// when it does not.
///
/// IMMEDIATE rather than DEFERRED on purpose. A deferred transaction whose
/// first statement is a SELECT pins a read snapshot, and if another process
/// commits before this one's first write (`squelchd token`, `squelchd pair`,
/// which share the file with a serving daemon) that write fails at once with
/// `SQLITE_BUSY_SNAPSHOT`: the busy handler is never consulted for it. Taking
/// the write lock up front waits out `busy_timeout` instead, which is what
/// each autocommit statement did before the writes were batched.
pub(super) fn in_write_transaction<T>(
    conn: &Connection,
    f: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    if !conn.is_autocommit() {
        return f(conn);
    }
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let out = f(&tx)?;
    tx.commit()?;
    Ok(out)
}

fn leased(conn: &Connection, job: &AgentJob) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_triage_jobs WHERE id=?1 AND account_id=?2 AND
             message_id=?3 AND state='leased' AND lease_token=?4 AND lease_until>?5)",
        params![
            job.id,
            job.account_id,
            job.message_id,
            job.lease_token,
            Utc::now().to_rfc3339()
        ],
        |r| r.get(0),
    )?)
}
fn finish(
    conn: &Connection,
    job: &AgentJob,
    outcome: &str,
    metadata: &serde_json::Value,
) -> Result<()> {
    conn.execute("UPDATE agent_triage_jobs SET state='completed',lease_token=NULL,lease_until=NULL WHERE id=?1
             AND account_id=?2",
        params![job.id,job.account_id])?;
    conn.execute("INSERT INTO agent_triage_runs(job_id,account_id,message_id,outcome,completed_at,metadata_json) VALUES(?1,?2,?3,?4,?5,?6)",
        params![job.id,job.account_id,job.message_id,outcome,Utc::now().to_rfc3339(),json(metadata)?])?;
    release_followup(conn, job)?;
    Ok(())
}

/// [`SqliteStore::triage_waiting_counts`]. NOT `archive_access`: that is an
/// agent's search asking whether it may read old mail, not the owner's mail
/// waiting to be triaged, and counted here it would tell the human "triaging
/// N" about work they never asked for. Pinned to the account's ready index:
/// without ANALYZE the planner prefers the re-triage progress index on
/// `account_id` alone and walks every job the account ever had, under the
/// store mutex, on every `/client/stats`.
pub(crate) const TRIAGE_WAITING_SQL: &str = "SELECT
       COUNT(DISTINCT CASE WHEN state='queued' AND (available_at<=?2 OR last_error IS NOT NULL)
                           THEN message_id END),
       COUNT(DISTINCT CASE WHEN state='leased' THEN message_id END)
     FROM agent_triage_jobs INDEXED BY idx_agent_jobs_account_ready
     WHERE account_id=?1 AND state IN ('queued','leased') AND kind IN ('triage','access')
       AND trigger NOT LIKE 'revisit:%' AND trigger!='archive_access'";

/// [`SqliteStore::next_agent_jobs_due`], pinned to the same index: its
/// `(account_id, state, kind, available_at)` seeks straight to the pending
/// rows of the kinds asked for and serves `available_at` from the index, so
/// only a leased row is fetched, for its `lease_until`. The partial index the
/// claim itself seeks carries neither state nor kind and would walk every
/// pending job of the tenant, a first backfill's whole triage backlog among
/// them, to find the two notification kinds.
///
/// "Claimable" is [`CLAIM_JOB_SQL`]'s own notion, so that a job this read
/// counts is one the claim will hand out: queued and available, or leased
/// until its lease lapses (a leased row with no `lease_until`, which no claim
/// writes, is never claimable, and `MAX` of a NULL is NULL, which `MIN`
/// skips); and of a message that exists, the claim's own join. The per-thread
/// exclusion the claim adds applies to triage and access jobs only. Both
/// time columns are RFC 3339 text, which orders as text, as the claim's own
/// comparison relies on. The kinds arrive as one JSON array, so the statement
/// is one string whatever their number. A kind whose pending rows are all
/// unclaimable comes back with a NULL time, which the caller drops.
pub(crate) const NEXT_JOBS_DUE_SQL: &str = "SELECT j.kind,
       MIN(CASE WHEN j.state='leased' THEN MAX(j.available_at, j.lease_until)
                ELSE j.available_at END)
     FROM agent_triage_jobs j INDEXED BY idx_agent_jobs_account_ready
     JOIN messages m ON m.account_id=j.account_id AND m.id=j.message_id
     WHERE j.account_id=?1 AND j.state IN ('queued','leased')
       AND j.kind IN (SELECT value FROM json_each(?2))
     GROUP BY j.kind";

/// [`SqliteStore::triage_budget_parked`], pinned for the same reason.
pub(crate) const TRIAGE_BUDGET_PARKED_SQL: &str = "SELECT COUNT(*), MIN(available_at)
     FROM agent_triage_jobs INDEXED BY idx_agent_jobs_account_ready
     WHERE account_id=?1 AND state='queued' AND kind IN ('triage','access')
       AND trigger!='archive_access'
       AND last_error='daily_budget_exhausted' AND available_at>?2";

impl SqliteStore {
    /// Fixed-cardinality scrape counts, scoped to one account. Delayed retries
    /// remain queued; leases remain leased until reclaimed by a worker.
    pub fn triage_job_counts(&self, account_id: AccountId) -> Result<[u64; 2]> {
        let conn = self.lock()?;
        let mut counts = [0; 2];
        let mut query = conn.prepare(
            "SELECT state, COUNT(*) FROM agent_triage_jobs
             WHERE account_id=?1 AND kind IN ('triage','access')
               AND trigger!='archive_access'
               AND state IN ('queued','leased') GROUP BY state",
        )?;
        for row in query.query_map([account_id], |row| {
            Ok((row.get::<_, String>(0)?, super::count(row, 1)?))
        })? {
            let (state, count) = row?;
            counts[usize::from(state == "leased")] = count;
        }
        Ok(counts)
    }

    /// EMAILS WAITING FOR TRIAGE, for the client door: `[queued, running]` as
    /// DISTINCT messages, not jobs. [`Self::triage_job_counts`] is the scrape
    /// gauge and counts every queued job, which on a healthy idle mailbox is
    /// mostly scheduled revisits sitting up to 400 days out; a user-facing
    /// count built on it never reaches zero, so "You're all clear." never
    /// shows. Queued here means work that is owed now: due already, or pushed
    /// back by the worker itself (a retry, a provider cooldown, a spent daily
    /// budget, all of which set `last_error`). A revisit is never owed now; it
    /// is the model asking to look again later.
    pub fn triage_waiting_counts(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
    ) -> Result<[u64; 2]> {
        let conn = self.lock()?;
        let (queued, leased): (u64, u64) = conn.query_row(
            TRIAGE_WAITING_SQL,
            params![account_id, now.to_rfc3339()],
            |row| Ok((super::count(row, 0)?, super::count(row, 1)?)),
        )?;
        Ok([queued, leased])
    }

    /// Queued triage/access jobs the worker parked because a daily budget ran
    /// out, and the earliest time one becomes claimable again. Across ALL
    /// queued work, not only a re-triage: the stats door uses it to say "paused
    /// until tomorrow" rather than letting a parked queue read as stuck.
    pub fn triage_budget_parked(
        &self,
        account_id: AccountId,
    ) -> Result<(u64, Option<DateTime<Utc>>)> {
        let conn = self.lock()?;
        let (parked, resumes_at): (u64, Option<String>) = conn.query_row(
            TRIAGE_BUDGET_PARKED_SQL,
            params![account_id, Utc::now().to_rfc3339()],
            |row| Ok((super::count(row, 0)?, row.get(1)?)),
        )?;
        let resumes_at = resumes_at
            .as_deref()
            .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
            .map(|at| at.with_timezone(&Utc));
        Ok((parked, resumes_at))
    }
}

impl AgentTriageStore for SqliteStore {
    fn reserve_agent_budget(
        &self,
        account: AccountId,
        day: &str,
        limits: &[(String, u32)],
    ) -> Result<bool> {
        super::agent_budget::reserve(self, account, day, limits)
    }
    fn refund_agent_budget(
        &self,
        account: AccountId,
        day: &str,
        limits: &[(String, u32)],
    ) -> Result<()> {
        super::agent_budget::refund(self, account, day, limits)
    }

    fn agent_diagnostics(&self, account: AccountId, message: i64) -> Result<serde_json::Value> {
        let conn = self.lock()?;
        read_message(&conn, account, message)?;
        let jobs = {
            let mut stmt=conn.prepare(
            "SELECT id,kind,trigger,state,attempts,available_at,lease_until,last_error
             FROM agent_triage_jobs WHERE account_id=?1 AND message_id=?2 ORDER BY id DESC LIMIT 25"
        )?;
            stmt.query_map(params![account,message],|row|Ok(serde_json::json!({
            "id":row.get::<_,i64>(0)?,"kind":row.get::<_,String>(1)?,"trigger":row.get::<_,String>(2)?,
            "state":row.get::<_,String>(3)?,"attempts":row.get::<_,i64>(4)?,"available_at":row.get::<_,String>(5)?,
            "lease_until":row.get::<_,Option<String>>(6)?,"last_error":row.get::<_,Option<String>>(7)?,
        })))?.collect::<std::result::Result<Vec<_>,_>>()?
        };
        let runs = {
            let mut stmt = conn.prepare(MESSAGE_RUNS_SQL)?;
            let rows = stmt
                .query_map(params![account, message], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows.into_iter().map(|(id,job,outcome,completed,metadata)|Ok(serde_json::json!({"id":id,"job_id":job,"outcome":outcome,"completed_at":completed,"metadata":decode::<serde_json::Value>(&metadata)?}))).collect::<Result<Vec<_>>>()?
        };
        Ok(serde_json::json!({"jobs":jobs,"runs":runs}))
    }
    fn human_agent_updates(
        &self,
        account: AccountId,
        query: &AgentInventoryQuery,
    ) -> Result<Vec<crate::types::AttentionUpdate>> {
        human_inventory(&*self.lock()?, account, query)
    }
    fn external_agent_fye(
        &self,
        account: AccountId,
        limit: usize,
        config: &RankingConfig,
        now: DateTime<Utc>,
    ) -> Result<Vec<AgentListItem>> {
        let conn = self.lock()?;
        config.validate().map_err(CoreError::InvalidInput)?;
        let mut items = list_items(&conn, account, "fye", now)?;
        rank_items(&mut items, config, now);
        restrict_external_items(&conn, account, &mut items, limit)?;
        Ok(items)
    }
    fn external_agent_reading(
        &self,
        account: AccountId,
        limit: usize,
    ) -> Result<Vec<AgentListItem>> {
        self.external_agent_reading_with_query(account, limit, &AgentListQuery::default())
    }
    fn external_agent_reading_with_query(
        &self,
        account: AccountId,
        limit: usize,
        query: &AgentListQuery,
    ) -> Result<Vec<AgentListItem>> {
        let conn = self.lock()?;
        let mut items = list_items_with_query(&conn, account, "reading", Utc::now(), query)?;
        items.sort_by(|a, b| {
            b.received_at
                .cmp(&a.received_at)
                .then_with(|| b.message_id.cmp(&a.message_id))
        });
        restrict_external_items(&conn, account, &mut items, limit)?;
        Ok(items)
    }
    fn external_agent_decision(&self, account: AccountId, message: i64) -> Result<MessageDecision> {
        let conn = self.lock()?;
        let source = read_message(&conn, account, message)?;
        // The message itself by name: the thread guard omits an unassessed
        // archive row instead of failing on it, which is right for a sibling
        // and would be a leak for the row being asked about.
        if !super::messages::thread_allowed_with_conn(
            &conn,
            account,
            &source.thread_id,
            &[message],
        )? {
            return Err(CoreError::NotFound);
        }
        let encoded:Option<String>=conn.query_row("SELECT decision_json FROM agent_message_decisions WHERE account_id=?1 AND message_id=?2",params![account,message],|row|row.get(0)).optional()?;
        decode(&encoded.ok_or(CoreError::NotFound)?)
    }
    fn external_agent_records(
        &self,
        account: AccountId,
        limit: usize,
    ) -> Result<Vec<AgentListItem>> {
        self.external_agent_records_with_query(account, limit, &AgentListQuery::default())
    }
    fn external_agent_records_with_query(
        &self,
        account: AccountId,
        limit: usize,
        query: &AgentListQuery,
    ) -> Result<Vec<AgentListItem>> {
        let conn = self.lock()?;
        let mut items = list_items_with_query(&conn, account, "records", Utc::now(), query)?;
        items.sort_by(|a, b| {
            b.received_at
                .cmp(&a.received_at)
                .then_with(|| b.message_id.cmp(&a.message_id))
        });
        restrict_external_items(&conn, account, &mut items, limit)?;
        Ok(items)
    }
    fn agent_shipment_is_hidden(
        &self,
        account: AccountId,
        tracking_number: &str,
        silence: Option<crate::config::Silence>,
    ) -> Result<bool> {
        let conn = self.lock()?;
        let cleared = "s.cleared_at IS NOT NULL AND s.last_update<=s.cleared_at";
        Ok(
            match silence.as_ref().map(super::specialists::silence_binds) {
                Some((before, cap)) => conn.query_row(
                    &format!(
                        "SELECT EXISTS(SELECT 1 FROM shipments s WHERE s.account_id=?1
                     AND s.tracking_number=?4 AND (({cleared}) OR {}))",
                        super::specialists::SILENT_FOR_CERTAIN
                    ),
                    params![account, before, cap, tracking_number],
                    |row| row.get(0),
                )?,
                None => conn.query_row(
                    &format!(
                        "SELECT EXISTS(SELECT 1 FROM shipments s WHERE s.account_id=?1
                     AND s.tracking_number=?2 AND ({cleared}))"
                    ),
                    params![account, tracking_number],
                    |row| row.get(0),
                )?,
            },
        )
    }
    fn correct_agent_triage(
        &self,
        account: AccountId,
        message: i64,
        field: &str,
        value: &serde_json::Value,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        correct_agent_triage_conn(&tx, account, message, field, value, now)?;
        tx.commit()?;
        // A correction can UNSEAL (a restriction lifted, an auth kind
        // removed), and nothing else would tell the vector drain: no triage
        // job runs for an archive row, and one that does may wait on a budget.
        self.vectors_owed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
    fn correct_agent_triage_delta(
        &self,
        account: AccountId,
        message: i64,
        field: &str,
        add: &[String],
        remove: &[String],
        now: DateTime<Utc>,
    ) -> Result<()> {
        if !matches!(field, "kinds" | "destinations") {
            return Err(CoreError::InvalidInput(
                "list deltas require kinds or destinations".into(),
            ));
        }
        let incoming = serde_json::json!({"add":add,"remove":remove});
        apply_correction(&mut MessageDecision::default(), field, &incoming)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let prior = corrections(&tx, account, message)?
            .into_iter()
            .find(|entry| entry["field"] == field);
        let value = if let Some(prior) = prior.filter(|entry| entry["value"].is_array()) {
            // Preserve compatibility with a deliberate older whole-field correction.
            let mut values: Vec<String> = serde_json::from_value(prior["value"].clone())
                .map_err(|e| CoreError::Other(e.into()))?;
            apply_list_delta(&mut values, &incoming)?;
            serde_json::json!(values)
        } else {
            let prior = corrections(&tx, account, message)?
                .into_iter()
                .find(|entry| entry["field"] == field);
            let mut additions: Vec<String> = prior
                .as_ref()
                .and_then(|p| p["value"]["add"].as_array())
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
            let mut removals: Vec<String> = prior
                .as_ref()
                .and_then(|p| p["value"]["remove"].as_array())
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
            additions.retain(|v| !remove.contains(v));
            removals.retain(|v| !add.contains(v));
            for value in add {
                if !additions.contains(value) {
                    additions.push(value.clone());
                }
            }
            for value in remove {
                if !removals.contains(value) {
                    removals.push(value.clone());
                }
            }
            serde_json::json!({"add":additions,"remove":removals})
        };
        correct_agent_triage_conn(&tx, account, message, field, &value, now)?;
        tx.commit()?;
        self.vectors_owed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
    fn enqueue_agent_triage(
        &self,
        account: AccountId,
        message: i64,
        trigger: &str,
        arrival_eligible: bool,
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        enqueue_agent_triage_conn(&tx, account, message, trigger, arrival_eligible)?;
        tx.commit()?;
        Ok(())
    }
    fn claim_agent_job_sized(
        &self,
        account: AccountId,
        kind: &str,
        now: DateTime<Utc>,
        lease_seconds: i64,
        access_lease_seconds: i64,
    ) -> Result<Option<AgentJob>> {
        if lease_seconds <= 0 || access_lease_seconds <= 0 {
            return Err(CoreError::InvalidInput("lease must be positive".into()));
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let token: String = tx
            .prepare_cached("SELECT lower(hex(randomblob(16)))")?
            .query_row([], |r| r.get(0))?;
        let row = tx
            .prepare_cached(CLAIM_JOB_SQL)?
            .query_row(
                params![account, kind, now.to_rfc3339(), now.timestamp()],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, bool>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, bool>(6)?,
                        r.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            id,
            message_id,
            trigger,
            attempts,
            arrival_eligible,
            claimed_kind,
            foreground,
            last_error,
        )) = row
        else {
            return Ok(None);
        };
        let lease = if claimed_kind == "access" {
            access_lease_seconds
        } else {
            lease_seconds
        };
        let until = (now + Duration::seconds(lease)).to_rfc3339();
        tx.execute(
            "UPDATE agent_triage_jobs SET
             state='leased',lease_token=?1,lease_until=?2,attempts=attempts+1 WHERE id=?3",
            params![token, until, id],
        )?;
        tx.commit()?;
        Ok(Some(AgentJob {
            id,
            account_id: account,
            message_id,
            kind: claimed_kind,
            trigger,
            lease_token: token,
            attempts: attempts + 1,
            last_error,
            arrival_eligible,
            foreground,
        }))
    }
    fn next_agent_jobs_due(
        &self,
        account: AccountId,
        kinds: &[&str],
    ) -> Result<Vec<(String, DateTime<Utc>)>> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock()?;
        let mut statement = conn.prepare_cached(NEXT_JOBS_DUE_SQL)?;
        let rows = statement.query_map(params![account, json(&kinds)?], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        rows.filter_map(|row| match row {
            Ok((kind, Some(due))) => Some(
                DateTime::parse_from_rfc3339(&due)
                    .map(|due| (kind, due.with_timezone(&Utc)))
                    .map_err(|e| CoreError::Other(e.into())),
            ),
            Ok((_, None)) => None,
            Err(e) => Some(Err(e.into())),
        })
        .collect()
    }
    fn load_agent_context(&self, job: &AgentJob) -> Result<AgentContext> {
        context(&*self.lock()?, job.account_id, job.message_id)
    }
    fn load_agent_access_message(&self, job: &AgentJob) -> Result<AgentMessage> {
        let conn = self.lock()?;
        if job.kind != "access" || !leased(&conn, job)? {
            return Err(CoreError::NotFound);
        }
        read_message(&conn, job.account_id, job.message_id)
    }
    fn commit_agent_access(
        &self,
        job: &AgentJob,
        original: &AgentMessage,
        decision: &crate::triage::access::AccessDecision,
        metadata: &serde_json::Value,
    ) -> Result<AgentCommitOutcome> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        if job.kind != "access" || !leased(&tx, job)? {
            return Ok(AgentCommitOutcome::Stale);
        }
        let current = read_message(&tx, job.account_id, job.message_id)?;
        // Access depends on this message's content and explicit restriction,
        // not its siblings, opened stamp or attention representation.
        if current.source.content != original.source.content {
            return Ok(AgentCommitOutcome::Stale);
        }
        let restricted = explicit_access_override(&tx, job.account_id, job.message_id)?
            .unwrap_or(decision.restricted);
        tx.execute(
            "UPDATE agent_message_state SET access=?3 WHERE account_id=?1 AND message_id=?2",
            params![
                job.account_id,
                job.message_id,
                if restricted { "restricted" } else { "allowed" }
            ],
        )?;
        // Access changed outside the triage commit: the message's stored
        // decision is projected again now (matched afresh), not at its next
        // re-triage.
        super::ghosts::refresh_message_guarded(&tx, job.account_id, job.message_id)?;
        // A restriction seals: the owner's own mail, embedded at ingest, loses
        // its vectors at the event (`vector_forbidden_sql!`).
        super::search::purge_forbidden_vectors_conn(&tx, Some(job.message_id))?;
        finish(&tx, job, "access_applied", metadata)?;
        tx.commit()?;
        Ok(AgentCommitOutcome::Applied)
    }
    fn complete_agent_job(&self, job: &AgentJob) -> Result<bool> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        if !leased(&tx, job)? {
            return Ok(false);
        }
        finish(&tx, job, "completed", &serde_json::json!({}))?;
        tx.commit()?;
        Ok(true)
    }
    fn fail_agent_job(&self, job: &AgentJob, error_code: &str) -> Result<bool> {
        let code = job_error_code(error_code);
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute("UPDATE agent_triage_jobs SET state='failed',last_error=?1,lease_token=NULL,lease_until=NULL
             WHERE id=?2 AND account_id=?3 AND state='leased' AND lease_token=?4",
        params![code,job.id,job.account_id,job.lease_token])? == 1;
        if changed {
            tx.execute(
                "DELETE FROM agent_triage_followups WHERE job_id=?1",
                [job.id],
            )?;
        }
        tx.commit()?;
        Ok(changed)
    }
    fn retry_agent_job(
        &self,
        job: &AgentJob,
        retry_at: DateTime<Utc>,
        error_code: &str,
    ) -> Result<bool> {
        let code = job_error_code(error_code);
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = requeue(&tx, job, retry_at, Some(&code), false)?;
        tx.commit()?;
        Ok(changed)
    }
    fn defer_agent_job(
        &self,
        job: &AgentJob,
        available_at: DateTime<Utc>,
        error_code: &str,
    ) -> Result<bool> {
        let code = job_error_code(error_code);
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = requeue(&tx, job, available_at, Some(&code), true)?;
        tx.commit()?;
        Ok(changed)
    }
    fn release_agent_job(&self, job: &AgentJob, available_at: DateTime<Utc>) -> Result<bool> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = requeue(&tx, job, available_at, None, false)?;
        tx.commit()?;
        Ok(changed)
    }
    fn commit_agent_decision(
        &self,
        job: &AgentJob,
        original: &AgentContext,
        decision: &MessageDecision,
        sources: &[AgentSourceSnapshot],
    ) -> Result<AgentCommitOutcome> {
        self.commit_agent_decision_with_policy(
            job,
            original,
            decision,
            sources,
            &crate::config::RevisitPassConfig::default(),
        )
    }
    fn commit_agent_decision_with_policy(
        &self,
        job: &AgentJob,
        original: &AgentContext,
        decision: &MessageDecision,
        sources: &[AgentSourceSnapshot],
        policy: &crate::config::RevisitPassConfig,
    ) -> Result<AgentCommitOutcome> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if !leased(&tx, job)? {
            return Ok(AgentCommitOutcome::Stale);
        }
        let current = context(&tx, job.account_id, job.message_id)?;
        let mut stale = current.revision != original.revision;
        for source in sources {
            stale |= read_message(&tx, job.account_id, source.message_id)?.source != *source;
        }
        for update in &decision.related_updates {
            let revision:Option<i64>=tx.query_row("SELECT revision FROM agent_thread_attention WHERE account_id=?1 AND thread_id=?2",
        params![job.account_id,update.thread_id],
        |r|r.get(0)).optional()?;
            stale |= revision.unwrap_or(0) != update.expected_revision;
            if update.thread_id == current.message.thread_id {
                return Err(CoreError::InvalidInput(
                    "related update repeats current thread".into(),
                ));
            }
            let mut observed = false;
            for source in sources {
                if read_message(&tx, job.account_id, source.message_id)?.thread_id
                    == update.thread_id
                {
                    observed = true;
                }
            }
            if !observed {
                return Err(CoreError::InvalidInput(
                    "related attention was not read".into(),
                ));
            }
        }
        if stale {
            // Preserve this lease so the worker can apply its bounded retry/backoff
            // policy using the same token. Storage only records the rejected run.
            tx.execute(
                "INSERT INTO agent_triage_runs(job_id,account_id,message_id,outcome,completed_at,metadata_json)
             VALUES(?1,?2,?3,'stale',?4,?5)",
                params![
                    job.id,
                    job.account_id,
                    job.message_id,
                    Utc::now().to_rfc3339(),
                    json(&original.run_metadata)?
                ],
            )?;
            tx.commit()?;
            return Ok(AgentCommitOutcome::Stale);
        }
        let mut decision = decision.clone();
        apply_corrections(&tx, job.account_id, job.message_id, &mut decision)?;
        let has_kind_correction: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_triage_corrections WHERE account_id=?1 AND message_id=?2 AND field='kinds')",
            params![job.account_id,job.message_id], |row|row.get(0))?;
        if has_kind_correction && decision.kinds.is_empty() {
            return Err(CoreError::InvalidInput(
                "kinds cannot be empty after applying user corrections".into(),
            ));
        }
        let decision = &decision;
        let revision: i64 = tx.query_row(
            "SELECT revision FROM agent_message_state WHERE account_id=?1 AND message_id=?2",
            params![job.account_id, job.message_id],
            |r| r.get(0),
        )?;
        tx.execute("INSERT INTO agent_message_decisions(account_id,message_id,revision,decision_json,decided_at)
             VALUES(?1,?2,?3,?4,?5) ON CONFLICT(account_id,message_id) DO UPDATE SET
             revision=excluded.revision,decision_json=excluded.decision_json,decided_at=excluded.decided_at",
        params![job.account_id,job.message_id,revision,json(decision)?,Utc::now().to_rfc3339()])?;
        // A decision that SEALS the message closes its translation here, at
        // the event (docs/TRANSLATION.md, "Sealed mail").
        super::translations::close_if_untranslatable_conn(
            &tx,
            job.account_id,
            job.message_id,
            Utc::now(),
        )?;
        tx.execute(
            "UPDATE agent_message_state SET access=?1 WHERE account_id=?2 AND message_id=?3",
            params![
                if decision.external_access.restricted {
                    "restricted"
                } else {
                    "allowed"
                },
                job.account_id,
                job.message_id
            ],
        )?;
        // A decision that SEALS the message deletes its vectors here, at the
        // event, like its translation above (`vector_forbidden_sql!`).
        super::search::purge_forbidden_vectors_conn(&tx, Some(job.message_id))?;
        let done = apply_done(&tx, job, decision, Utc::now())?;
        // A RE-CHECK MOVES MAIL ONLY BY ITS DONE. For the owner, done and
        // leaving FYE are one act: a re-check writes no placement at all (no
        // FYE flag, no representative, no Reading destination, no related
        // thread), so a refused or absent done cannot hide mail by the side
        // door, and an applied done leaves placement as the owner's own Done
        // would, so their undo brings the thread straight back.
        let recheck = job.trigger.starts_with("revisit:");
        if !recheck {
            tx.execute(
                "DELETE FROM agent_message_destinations WHERE account_id=?1 AND message_id=?2",
                params![job.account_id, job.message_id],
            )?;
        }
        // Sent/spam access reviews cannot populate human placement surfaces.
        if !recheck && !current.message.is_sent && !current.message.is_spam {
            for destination in &decision.destinations {
                let value = match destination {
                    MessageDestination::Reading => "reading",
                };
                tx.execute("INSERT OR IGNORE INTO agent_message_destinations(account_id,message_id,destination) VALUES(?1,?2,?3)",
        params![job.account_id,job.message_id,value])?;
            }
            // What this job means for the first-load allowance: a person
            // asking again (`manual:`), or the backlog's first sorting.
            let here = FirstLoadJob {
                asked: job.trigger.starts_with("manual:"),
                sorting: current.previous_decision.is_none()
                    && tx.query_row(
                        "SELECT COALESCE((SELECT first_load FROM triage
                          WHERE account_id=?1 AND message_id=?2),0)",
                        params![job.account_id, job.message_id],
                        |r| r.get::<_, bool>(0),
                    )?,
            };
            apply_attention(
                &tx,
                job.account_id,
                &current.message.thread_id,
                job.message_id,
                &decision.attention,
                here,
            )?;
            replace_attention_sources(&tx, job.account_id, &current.message.thread_id, sources)?;
            for update in &decision.related_updates {
                apply_attention(
                    &tx,
                    job.account_id,
                    &update.thread_id,
                    job.message_id,
                    &update.attention,
                    FirstLoadJob::default(),
                )?;
                replace_attention_sources(&tx, job.account_id, &update.thread_id, sources)?;
            }
            cap_first_load_fye(&tx, job.account_id, here.sorting, Utc::now())?;
        }
        tx.execute(
            "DELETE FROM agent_decision_sources WHERE account_id=?1 AND message_id=?2",
            params![job.account_id, job.message_id],
        )?;
        for id in sources
            .iter()
            .map(|s| s.message_id)
            .chain(std::iter::once(job.message_id))
        {
            // Verify account ownership even if an upstream validator regresses.
            read_message(&tx, job.account_id, id)?;
            let source_revision:i64=tx.query_row("SELECT revision FROM agent_message_state WHERE account_id=?1 AND message_id=?2",
        params![job.account_id,id],
        |r|r.get(0)).optional()?.unwrap_or(0);
            tx.execute(
                "INSERT OR IGNORE INTO
             agent_decision_sources(account_id,message_id,source_message_id,source_revision)
             VALUES(?1,?2,?3,?4)",
                params![job.account_id, job.message_id, id, source_revision],
            )?;
        }
        // A message the done closed needs no further re-check, so a revisit
        // set beside it would be a billed run that can only refuse.
        if done != Some(DoneOutcome::Applied) {
            schedule_revisit(&tx, job, revision, decision, policy, Utc::now())?;
        }
        if job.arrival_eligible && !current.message.is_sent && !current.message.is_spam {
            tx.execute("INSERT INTO
             agent_triage_jobs(account_id,message_id,kind,trigger,input_revision,arrival_eligible,available_at)
             VALUES(?1,?2,'deliberate_notification','deliberate',?3,1,?4) ON CONFLICT DO NOTHING",
        params![job.account_id,job.message_id,revision,Utc::now().to_rfc3339()])?;
        }
        super::agent_deliveries::reconcile(
            &tx,
            job.account_id,
            job.message_id,
            current.previous_decision.as_ref(),
            decision,
        )?;
        // Ghosts never block triage: the projection runs under its own
        // savepoint and a failure costs only the ghosts. It errs only when
        // SQLite already rolled the whole transaction back, and then this
        // commit must fail rather than finish the job in autocommit. The
        // decision row written above is what its rebuild reads for this
        // message.
        super::ghosts::reconcile_guarded(
            &tx,
            job.account_id,
            job.message_id,
            !current.message.is_sent
                && !current.message.is_spam
                && super::ghosts::may_feed(decision),
            decision,
        )?;
        finish(&tx, job, "applied", &original.run_metadata)?;
        tx.commit()?;
        // Ids and rail codes only: the model's reason is never logged.
        match done {
            Some(DoneOutcome::Applied) => eprintln!(
                "squelch: auto-done applied to message {} (agent_revisit)",
                job.message_id
            ),
            Some(DoneOutcome::Refused(code)) => eprintln!(
                "squelch: auto-done not applied to message {} ({code})",
                job.message_id
            ),
            None => {}
        }
        Ok(AgentCommitOutcome::Applied)
    }
    fn acknowledge_agent_message(
        &self,
        account: AccountId,
        message: i64,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let conn = self.lock()?;
        read_message(&conn, account, message)?;
        conn.execute("UPDATE triage SET opened_at=COALESCE(opened_at,?1) WHERE account_id=?2 AND message_id=?3",
        params![now.to_rfc3339(),account,message])?;
        Ok(())
    }
    fn record_message_attention(
        &self,
        account: AccountId,
        message: i64,
        dwell_ms: u64,
        link_clicks: u32,
    ) -> Result<()> {
        let dwell = dwell_ms.min(MAX_DWELL_REPORT_MS);
        let clicks = link_clicks.min(MAX_LINK_CLICKS_REPORT);
        if dwell == 0 && clicks == 0 {
            return Ok(());
        }
        let conn = self.lock()?;
        // One statement: the add and both ceilings are evaluated against the
        // stored row, so two reports racing through the mutex cannot both read
        // the old total. The WHERE is every refusal at once: another
        // account's message, an id that does not exist, and mail that is kept
        // close each match nothing, and the call answers Ok for all of them
        // without having looked to see which it was.
        //
        // "KEPT CLOSE" IS `known_sealed_mail_sql!`: auth mail and restricted
        // mail by their stored decision, not only the retired `sensitivity`
        // column, which nothing arriving today sets.
        //
        // THE LOOSER OF THE TWO MACROS, and the only place it is used. Mail
        // whose assessment is still pending TAKES the report: it is being
        // read right now, before triage has got to it, and a refused report
        // cannot be had back. If it turns out to be a login code the count
        // sits on a row every reader skips (`sealed_mail_sql!` has the
        // argument, and the test that holds it).
        conn.execute(
            concat!(
                "UPDATE triage AS t
                 SET dwell_ms=min(?1,dwell_ms+?2), link_clicks=min(?3,link_clicks+?4)
                 WHERE t.account_id=?5 AND t.message_id=?6
                   AND EXISTS(SELECT 1 FROM messages m
                              WHERE m.account_id=t.account_id AND m.id=t.message_id
                                AND NOT ",
                known_sealed_mail_sql!(),
                ")"
            ),
            params![
                MAX_DWELL_TOTAL_MS as i64,
                dwell as i64,
                MAX_LINK_CLICKS_TOTAL,
                clicks,
                account,
                message
            ],
        )?;
        Ok(())
    }
    fn agent_access_allowed(&self, account: AccountId, message: i64) -> Result<bool> {
        super::messages::external_message_allowed_conn(&*self.lock()?, account, message)
    }
    fn snapshot_agent_sources(
        &self,
        account: AccountId,
        ids: &[i64],
    ) -> Result<Vec<AgentSourceSnapshot>> {
        let conn = self.lock()?;
        let messages = ids
            .iter()
            .map(|id| read_evidence_message(&conn, account, *id))
            .collect::<Result<Vec<_>>>()?;
        ensure_source_assessments(&conn, account, &messages)?;
        messages
            .into_iter()
            .map(|message| read_message(&conn, account, message.id).map(|m| m.source))
            .collect()
    }
    fn agent_thread_context(&self, account: AccountId, thread: &str) -> Result<AgentContext> {
        let conn = self.lock()?;
        let id: i64 = conn
            .query_row(
                concat!(
                    "SELECT m.id FROM messages m WHERE m.account_id=?1 AND m.thread_id=?2 AND ",
                    archive_evidence_sql!(),
                    " ORDER BY
             ",
                    own_mail_sql!(),
                    ",m.is_spam,m.received_at DESC,m.id DESC LIMIT 1"
                ),
                params![account, thread],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(CoreError::NotFound)?;
        context(&conn, account, id)
    }
    fn agent_read_message(&self, account: AccountId, message: i64) -> Result<AgentMessage> {
        let conn = self.lock()?;
        let message = read_evidence_message(&conn, account, message)?;
        ensure_source_assessments(&conn, account, std::slice::from_ref(&message))?;
        read_message(&conn, account, message.id)
    }
    fn human_read_message(&self, account: AccountId, message: i64) -> Result<AgentMessage> {
        let conn = self.lock()?;
        let message = read_message(&conn, account, message)?;
        ensure_source_assessments(&conn, account, std::slice::from_ref(&message))?;
        read_message(&conn, account, message.id)
    }
    fn agent_read_thread(
        &self,
        account: AccountId,
        thread: &str,
        limit: usize,
    ) -> Result<Vec<AgentMessage>> {
        let conn = self.lock()?;
        let messages = read_thread(&conn, account, thread, limit)?;
        ensure_source_assessments(&conn, account, &messages)?;
        read_thread(&conn, account, thread, limit)
    }
    fn agent_search_mail(
        &self,
        account: AccountId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<AgentMessage>> {
        let query = crate::store::FtsQuery::build(query, false);
        if query.terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.lock()?;
        let mut statement = conn.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages_fts JOIN messages m ON m.id=messages_fts.rowid
             LEFT JOIN triage t ON t.account_id=m.account_id AND t.message_id=m.id
             WHERE messages_fts MATCH ?2 AND m.account_id=?1 AND m.is_sent=0 AND m.is_spam=0
               AND {ARCHIVE_EVIDENCE_SQL}
             ORDER BY bm25(messages_fts),m.received_at DESC,m.id DESC LIMIT ?3"
        ))?;
        let limit = limit.min(100);
        let mut rows = statement
            .query_map(params![account, query.strict, limit as i64], message_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if rows.len() < limit && query.terms.len() > 1 {
            let partial = statement
                .query_map(params![account, query.any, limit as i64], message_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for message in partial {
                if rows.len() == limit {
                    break;
                }
                if !rows.iter().any(|existing| existing.id == message.id) {
                    rows.push(message);
                }
            }
        }
        ensure_source_assessments(&conn, account, &rows)?;
        rows.into_iter()
            .map(|message| read_message(&conn, account, message.id))
            .collect()
    }
    fn agent_sender_history(
        &self,
        account: AccountId,
        sender: &str,
        limit: usize,
    ) -> Result<Vec<AgentMessage>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages m LEFT JOIN triage t ON t.account_id=m.account_id AND t.message_id=m.id
             WHERE m.account_id=?1 AND m.from_addr=?2 COLLATE NOCASE AND m.is_sent=0 AND m.is_spam=0
               AND {ARCHIVE_EVIDENCE_SQL}
             ORDER BY m.received_at DESC,m.id DESC LIMIT ?3"
        ))?;
        let rows = statement
            .query_map(params![account, sender, limit.min(100) as i64], message_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ensure_source_assessments(&conn, account, &rows)?;
        rows.into_iter()
            .map(|message| read_message(&conn, account, message.id))
            .collect()
    }

    fn initialize_agent_cutover(&self, account: AccountId, since: DateTime<Utc>) -> Result<usize> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_triage_cutover WHERE account_id=?1)",
            [account],
            |r| r.get(0),
        )?;
        if exists {
            return Ok(0);
        }
        // An audited automatic closure is model evidence, not a user's Done.
        // A later explicit completion has a newer resolved_at and is preserved.
        tx.execute(
            "UPDATE triage SET status='open',resolved_at=NULL,resolved_by=NULL
             WHERE account_id=?1 AND status='done' AND EXISTS(
                 SELECT 1 FROM audit_log a
                 WHERE a.account_id=triage.account_id AND a.action='bill.auto_close'
                   AND a.target=CAST(triage.message_id AS TEXT)
                   AND a.ts>=triage.resolved_at
             )",
            [account],
        )?;
        let ids = {
            let mut stmt=tx.prepare("SELECT m.id FROM messages m LEFT JOIN triage t ON t.account_id=m.account_id AND
             t.message_id=m.id WHERE m.account_id=?1
             AND NOT EXISTS(SELECT 1 FROM agent_message_state a WHERE a.account_id=m.account_id AND a.message_id=m.id)
             AND (m.received_at>=?2 OR t.remind_at IS NOT NULL OR
             (t.status!='done' AND t.tier IN ('deadline','past_due'))) ORDER BY m.received_at DESC")?;
            stmt.query_map(params![account, since.to_rfc3339()], |r| r.get::<_, i64>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for id in &ids {
            enqueue_agent_triage_conn(&tx, account, *id, "migration", false)?;
        }
        tx.execute(
            "INSERT INTO agent_triage_cutover(account_id,completed_at) VALUES(?1,?2)",
            params![account, Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
        Ok(ids.len())
    }
    fn agent_fye(
        &self,
        account: AccountId,
        limit: usize,
        config: &RankingConfig,
        now: DateTime<Utc>,
    ) -> Result<Vec<AgentListItem>> {
        let conn = self.lock()?;
        config.validate().map_err(CoreError::InvalidInput)?;
        let mut items = list_items(&conn, account, "fye", now)?;
        rank_items(&mut items, config, now);
        items.truncate(limit);
        Ok(items)
    }
    fn agent_records(&self, account: AccountId, limit: usize) -> Result<Vec<AgentListItem>> {
        self.agent_records_with_query(account, limit, &AgentListQuery::default())
    }
    fn agent_records_with_query(
        &self,
        account: AccountId,
        limit: usize,
        query: &AgentListQuery,
    ) -> Result<Vec<AgentListItem>> {
        let conn = self.lock()?;
        let mut items = list_items_with_query(&conn, account, "records", Utc::now(), query)?;

        items.sort_by(|a, b| {
            b.received_at
                .cmp(&a.received_at)
                .then_with(|| b.message_id.cmp(&a.message_id))
        });
        items.truncate(limit);
        Ok(items)
    }
    fn agent_reading(&self, account: AccountId, limit: usize) -> Result<Vec<AgentListItem>> {
        self.agent_reading_with_query(account, limit, &AgentListQuery::default())
    }
    fn agent_reading_with_query(
        &self,
        account: AccountId,
        limit: usize,
        query: &AgentListQuery,
    ) -> Result<Vec<AgentListItem>> {
        reading_items(&*self.lock()?, account, limit, query, Utc::now())
    }
}

/// The Reading inventory, newest first: the ONE query for "what is in
/// Reading". `GET /client/v2/feed?destination=reading` reads it through
/// `agent_reading_with_query`, and smart clear and the per-sender purge read
/// (`reading_senders`) call it under the lock they already hold. Two shapes of
/// one answer: the 30-day inventory those cleaners work on, and, with
/// `query.reading_board`, the shorter Reading BOARD the owner sees (the
/// default feed), which is a newest-first prefix of it.
pub(super) fn reading_items(
    conn: &Connection,
    account: AccountId,
    limit: usize,
    query: &AgentListQuery,
    now: DateTime<Utc>,
) -> Result<Vec<AgentListItem>> {
    if query.reading_board {
        return reading_board(conn, account, limit, query, now);
    }
    let mut items = list_items_with_query(conn, account, "reading", now, query)?;
    sort_newest_first(&mut items);
    items.truncate(limit);
    Ok(items)
}

fn sort_newest_first(items: &mut [AgentListItem]) {
    items.sort_by(|a, b| {
        b.received_at
            .cmp(&a.received_at)
            .then_with(|| b.message_id.cmp(&a.message_id))
    });
}

/// The most days of mail the Reading board reaches back.
pub const READING_BOARD_DAYS: i64 = 7;
/// The most messages the Reading board shows. Reading is the low-stakes
/// pile (newsletters, announcements, promotions), and a month of it is a
/// guilt list, not a reading list.
pub const READING_BOARD_MAX: usize = 49;

/// THE READING BOARD: the newest [`READING_BOARD_MAX`] Reading messages of the
/// last [`READING_BOARD_DAYS`] days in the owner's zone, COUNTING MAIL ALREADY
/// DONE, less the done ones. A heavy week shrinks the window from its old end
/// by however much it takes; a quiet one shows the whole week.
///
/// - DONE MAIL HOLDS ITS PLACE, so clearing never refills: closing three cards
///   leaves three fewer, rather than letting older mail in behind them, and
///   the board can run out. It moves only when mail arrives or a day starts.
/// - NO WHOLE-DAY CUT. Dropping whole days from the old end emptied the board
///   whenever one day alone was over the cap (the morning after a sale day:
///   today nothing, yesterday eighty, so the board showed nothing).
/// - CALENDAR DAYS, not 24 hours back from now, so the old end does not slide
///   hour to hour.
///
/// Nothing is changed in the store: mail off the board stays open in All
/// mail and search, and smart clear still cleans it.
fn reading_board(
    conn: &Connection,
    account: AccountId,
    limit: usize,
    query: &AgentListQuery,
    now: DateTime<Utc>,
) -> Result<Vec<AgentListItem>> {
    // An owner zone that cannot be read is not a reason to show no Reading.
    let zone = super::device_tokens::owner_zone_conn(conn, account)
        .map(|owner| owner.zone)
        .unwrap_or(chrono_tz::Tz::UTC);
    let since = Some(reading_board_since(now, zone));
    let mut slots = list_items_with_query(
        conn,
        account,
        "reading",
        now,
        &AgentListQuery {
            since,
            include_done: true,
            reading_board: false,
        },
    )?;
    sort_newest_first(&mut slots);
    slots.truncate(READING_BOARD_MAX);
    if !query.include_done {
        let open: std::collections::HashSet<i64> = list_items_with_query(
            conn,
            account,
            "reading",
            now,
            &AgentListQuery {
                since,
                include_done: false,
                reading_board: false,
            },
        )?
        .into_iter()
        .map(|item| item.message_id)
        .collect();
        slots.retain(|item| open.contains(&item.message_id));
    }
    slots.truncate(limit);
    Ok(slots)
}

/// Where the board's oldest day begins: midnight in the owner's zone,
/// [`READING_BOARD_DAYS`] days back counting today. A midnight a clock change
/// skips is the first instant after it.
fn reading_board_since(now: DateTime<Utc>, zone: chrono_tz::Tz) -> DateTime<Utc> {
    use chrono::TimeZone;
    let day = now.with_timezone(&zone).date_naive() - Duration::days(READING_BOARD_DAYS - 1);
    let midnight = day.and_time(chrono::NaiveTime::MIN);
    zone.from_local_datetime(&midnight)
        .earliest()
        .or_else(|| {
            zone.from_local_datetime(&(midnight + Duration::hours(1)))
                .earliest()
        })
        .map(|at| at.with_timezone(&Utc))
        .unwrap_or_else(|| midnight.and_utc())
}

/// A blank display name is no display name.
fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|name| !name.trim().is_empty())
}

/// How many archive rows one agent search may send for an access check.
pub const ARCHIVE_SURFACE_CAP: usize = 10;
/// The most `archive_access` jobs one account may have queued or leased at
/// once. Past it a search queues nothing, so no caller can build a backlog
/// that outlives the day it was made in.
pub const ARCHIVE_ACCESS_OUTSTANDING_MAX: u32 = 20;
/// The daily sub-cap's key in `wake_budget`, charged by the worker for every
/// `archive_access` job on top of the global and access-lane budgets.
pub const ARCHIVE_ACCESS_BUDGET_KEY: &str = "__agent_archive_access__";
pub use crate::triage::agent_config::{ACCESS_BUDGET_KEY, BACKGROUND_BUDGET_KEY};
/// A failed access check on an archive row may be asked for again this long
/// after it was last scheduled, so a row is not pending for good.
const ARCHIVE_ACCESS_RETRY_HOURS: i64 = 24;

/// What a door may spend on surfacing archive mail, from `triage.agent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveAccessCaps {
    /// `triage.agent.archive_access_daily_cap`.
    pub daily: u32,
    /// The effective access-lane cap; once today's is spent nothing is
    /// queued, since every check would park.
    pub access_daily: u32,
    /// `triage.agent.daily_run_cap`, the same way.
    pub total_daily: u32,
}

impl From<&crate::triage::agent_config::AgentConfig> for ArchiveAccessCaps {
    fn from(config: &crate::triage::agent_config::AgentConfig) -> Self {
        Self {
            daily: config.archive_access_daily_cap,
            access_daily: config.effective_access_daily_run_cap(),
            total_daily: config.daily_run_cap,
        }
    }
}

impl Default for ArchiveAccessCaps {
    fn default() -> Self {
        (&crate::triage::agent_config::AgentConfig::default()).into()
    }
}

/// Where the checks one call queued have got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AccessProgress {
    /// Still able to finish soon: leased, or queued and due.
    pub waiting: usize,
    /// Of those, never yet picked up by a worker.
    pub untouched: usize,
}

/// Put a FAILED access job on an unassessed archive row back in the queue once
/// it is [`ARCHIVE_ACCESS_RETRY_HOURS`] old. `available_at` is the last time
/// the job was scheduled, which is never later than the failure, so this errs
/// toward a slightly early retry and never toward none. The row's LATEST
/// failure is the one that has to be that old: an older failed job beside a
/// fresh one is no reason to try again today. Returns whether a job was
/// revived.
fn revive_failed_archive_access(
    conn: &Connection,
    account: AccountId,
    message: i64,
    now: DateTime<Utc>,
) -> Result<bool> {
    let cutoff = (now - Duration::hours(ARCHIVE_ACCESS_RETRY_HOURS)).to_rfc3339();
    let job: Option<i64> = conn
        .query_row(
            concat!(
                "SELECT j.id FROM messages m
                 JOIN agent_message_state a ON a.account_id=m.account_id AND a.message_id=m.id
                 JOIN agent_triage_jobs j ON j.account_id=m.account_id AND j.message_id=m.id
                 WHERE m.account_id=?1 AND m.id=?2 AND m.is_spam=0 AND ",
                archive_unassessed_sql!(),
                " AND j.kind='access' AND j.trigger IN ('archive_access','source_access')
                   AND j.input_revision=a.revision AND j.state='failed'
                   AND julianday(j.available_at)<=julianday(?3)
                   AND NOT EXISTS(SELECT 1 FROM agent_triage_jobs fresh
                       WHERE fresh.account_id=m.account_id AND fresh.message_id=m.id
                         AND fresh.kind='access' AND fresh.input_revision=a.revision
                         AND fresh.state='failed'
                         AND julianday(fresh.available_at)>julianday(?3))
                   AND NOT EXISTS(SELECT 1 FROM agent_triage_jobs live
                       WHERE live.account_id=m.account_id AND live.message_id=m.id
                         AND live.kind IN ('triage','access') AND live.state IN ('queued','leased'))
                 ORDER BY j.trigger='archive_access' DESC,j.id DESC LIMIT 1"
            ),
            params![account, message, cutoff],
            |row| row.get(0),
        )
        .optional()?;
    let Some(job) = job else {
        return Ok(false);
    };
    // Revived AS `archive_access`, whatever queued it first: the trigger is
    // what the outstanding ceiling counts and what the worker charges the
    // daily sub-cap on, so a revived check is inside both.
    //
    // THE RENAME MUST NOT MEET THE JOBS' UNIQUE KEY (account, message, kind,
    // revision, trigger). The select above prefers a failed job that already
    // carries the trigger, but the row may hold one in another state beside
    // a failed `source_access` (completed, with the access still pending).
    // Then that job is the one revived, and the other is left as it is.
    let twin: Option<i64> = conn
        .query_row(
            "SELECT t.id FROM agent_triage_jobs j
             JOIN agent_triage_jobs t ON t.account_id=j.account_id AND t.message_id=j.message_id
               AND t.kind=j.kind AND t.input_revision=j.input_revision
             WHERE j.id=?1 AND t.id!=j.id AND t.trigger='archive_access'",
            params![job],
            |row| row.get(0),
        )
        .optional()?;
    let job = twin.unwrap_or(job);
    conn.execute(
        "UPDATE agent_triage_jobs SET state='queued',attempts=0,available_at=?2,last_error=NULL,
             trigger='archive_access'
         WHERE id=?1",
        params![job, now.to_rfc3339()],
    )?;
    Ok(true)
}

/// ASSESS ON SURFACING, in the caller's transaction: queue one `access` job
/// (trigger `archive_access`) for each of `ids` that is an unassessed archive
/// row of this account with NO job at its current revision, in any state. A
/// row whose check already ran or is parked on the daily budget is left
/// exactly as it is, so repeating a request never buys a second model call;
/// one whose check FAILED is asked for again once that failure is a day old.
/// Taken in the order given (the caller passes best-ranked first). Returns
/// the ids queued.
///
/// THE ONE PLACE AN AGENT-DOOR CALLER CAN QUEUE WORK ON AN ARCHIVE ROW, a
/// search that matched it and a thread open that named it alike, so both obey
/// the same ceilings and neither can queue what the worker would only park:
/// at most `limit` (never over [`ARCHIVE_SURFACE_CAP`]) per call; never more
/// than [`ARCHIVE_ACCESS_OUTSTANDING_MAX`] outstanding for the account; never
/// more than today's `caps.daily` can still pay for, counting what is already
/// outstanding. And nothing at all once today's access budget, or the total, is spent,
/// because every one of these would park.
pub(super) fn request_archive_access_conn(
    conn: &Connection,
    account: AccountId,
    ids: &[i64],
    limit: usize,
    caps: ArchiveAccessCaps,
    now: DateTime<Utc>,
) -> Result<Vec<i64>> {
    let day = now.format("%Y-%m-%d").to_string();
    // Nothing is queued once a cap every one of these checks would park on
    // is spent, the lane's or the total's, as the worker will reserve
    // against (the budget module's own rule, today's grant included): a
    // `squelchd triage grant` on either reopens surfacing too.
    let spent = |key: &str, configured: u32| -> Result<bool> {
        Ok(super::agent_budget::runs(conn, account, key, &day)?
            >= super::agent_budget::cap(conn, account, key, configured, &day)?)
    };
    if spent(ACCESS_BUDGET_KEY, caps.access_daily)?
        || spent(
            crate::triage::agent_config::TOTAL_BUDGET_KEY,
            caps.total_daily,
        )?
    {
        return Ok(Vec::new());
    }
    let outstanding: u32 = conn.query_row(
        "SELECT COUNT(*) FROM agent_triage_jobs WHERE account_id=?1 AND kind='access'
           AND trigger='archive_access' AND state IN ('queued','leased')",
        [account],
        |row| row.get(0),
    )?;
    let room = limit
        .min(ARCHIVE_SURFACE_CAP)
        .min(ARCHIVE_ACCESS_OUTSTANDING_MAX.saturating_sub(outstanding) as usize)
        .min(
            caps.daily
                .saturating_sub(super::agent_budget::runs(
                    conn,
                    account,
                    ARCHIVE_ACCESS_BUDGET_KEY,
                    &day,
                )?)
                .saturating_sub(outstanding) as usize,
        );
    let mut queued = Vec::new();
    for id in ids {
        if queued.len() >= room {
            break;
        }
        let wanted: bool = conn.query_row(
            concat!(
                "SELECT EXISTS(SELECT 1 FROM messages m
                 LEFT JOIN agent_message_state a
                   ON a.account_id=m.account_id AND a.message_id=m.id
                 WHERE m.account_id=?1 AND m.id=?2 AND m.is_spam=0 AND ",
                archive_unassessed_sql!(),
                " AND NOT EXISTS(SELECT 1 FROM agent_triage_jobs j
                     WHERE j.account_id=m.account_id AND j.message_id=m.id
                       AND j.kind IN ('triage','access')
                       AND j.input_revision=COALESCE(a.revision,1)))"
            ),
            params![account, id],
            |row| row.get(0),
        )?;
        if wanted {
            enqueue_agent_triage_conn(conn, account, *id, "archive_access", false)?;
            queued.push(*id);
        } else if revive_failed_archive_access(conn, account, *id, now)? {
            queued.push(*id);
        }
    }
    Ok(queued)
}

impl SqliteStore {
    /// [`request_archive_access_conn`] for a search's matches, in its own
    /// transaction: up to [`ARCHIVE_SURFACE_CAP`] of `ids`, inside every
    /// ceiling that function keeps.
    pub fn request_archive_access(
        &self,
        account: AccountId,
        ids: &[i64],
        caps: ArchiveAccessCaps,
        now: DateTime<Utc>,
    ) -> Result<Vec<i64>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let queued =
            request_archive_access_conn(&tx, account, ids, ARCHIVE_SURFACE_CAP, caps, now)?;
        tx.commit()?;
        Ok(queued)
    }

    /// Where the access checks on `ids` stand. `waiting` counts a job leased,
    /// or queued and due; a job parked for later (daily budget, provider
    /// backoff) is not counted, so a caller polling this stops waiting for
    /// work that will not run today. `untouched` is the part of `waiting` no
    /// worker has picked up even once, which is how a caller tells a busy
    /// lane from one that is not running.
    pub fn archive_access_progress(
        &self,
        account: AccountId,
        ids: &[i64],
        now: DateTime<Utc>,
    ) -> Result<AccessProgress> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT COALESCE(MAX(j.state='leased' OR j.available_at<=?3),0),
                    COALESCE(MIN(j.state='queued' AND j.attempts=0 AND j.last_error IS NULL),0)
             FROM agent_triage_jobs j
             WHERE j.account_id=?1 AND j.message_id=?2 AND j.kind IN ('triage','access')
               AND j.state IN ('queued','leased')",
        )?;
        let mut progress = AccessProgress::default();
        for id in ids {
            let (waiting, untouched): (bool, bool) = stmt
                .query_row(params![account, id, now.to_rfc3339()], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?;
            if waiting {
                progress.waiting += 1;
                progress.untouched += usize::from(untouched);
            }
        }
        Ok(progress)
    }

    /// An explicit thread open at the agent door, as its own step: queue the
    /// access checks its newest unassessed siblings need and return the ids
    /// queued, so the door can wait on them BEFORE it applies the guard. Live
    /// siblings as [`Self::thread_view`] would; ARCHIVE siblings only here,
    /// and only inside `caps` (see [`request_archive_access_conn`]): the read
    /// itself never queues work on an archive row. Idempotent.
    pub fn request_thread_access(
        &self,
        account: AccountId,
        thread: &str,
        caps: ArchiveAccessCaps,
    ) -> Result<Vec<i64>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let queued = super::messages::request_external_thread_access_conn(
            &tx,
            account,
            thread,
            Some(caps),
            Utc::now(),
        )?;
        tx.commit()?;
        Ok(queued)
    }
}

/// The external listings' restriction, in the order the listing will be
/// served, keeping the first `limit` rows that may go out the agent door.
/// A row passes when its thread's attention is current and drawn from allowed
/// mail, its thread passes the guard every external thread read does, and the
/// row's message and the message whose decision it shows are allowed
/// ([`super::messages::ListingJudge`]: one walk for a thread's only row, a
/// thread listed more than once judged once). Whether a row may go out does
/// not depend on the rows around it, so stopping at `limit` keeps exactly
/// what filtering everything and then truncating would.
fn restrict_external_items(
    conn: &Connection,
    account: AccountId,
    items: &mut Vec<AgentListItem>,
    limit: usize,
) -> Result<()> {
    let mut judge =
        super::messages::ListingJudge::new(items.iter().map(|item| item.thread_id.as_str()));
    let attention = |thread: &str| current_attention_sources(conn, account, thread);
    let mut allowed = Vec::with_capacity(items.len().min(limit));
    for mut item in items.drain(..) {
        if allowed.len() == limit {
            break;
        }
        if judge.allowed(
            conn,
            account,
            &item.thread_id,
            &[item.message_id, item.decision_source_message_id],
            attention,
        )? {
            // The display name is the human door's alone: untrusted header
            // text never rides out on an agent-facing listing.
            item.from_name = None;
            allowed.push(item);
        }
    }
    *items = allowed;
    Ok(())
}
fn rank_items(items: &mut [AgentListItem], config: &RankingConfig, now: DateTime<Utc>) {
    for item in items.iter_mut() {
        let at = DateTime::parse_from_rfc3339(&item.received_at)
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or(now);
        let unresolved = item
            .unresolved_since
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc));
        let breakdown = rank(&item.attention.factors, at, unresolved, now, config);
        item.score = breakdown.total;
        item.ranking = Some(breakdown);
    }
    items.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| b.received_at.cmp(&a.received_at))
            .then_with(|| b.message_id.cmp(&a.message_id))
    });
}

fn effective_activity(conn: &Connection, account: AccountId, message: i64) -> Result<String> {
    conn.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%fZ',min(julianday(m.received_at),COALESCE(julianday(t.created_at),julianday(?3)),julianday(?3)))
         FROM messages m LEFT JOIN triage t ON t.account_id=m.account_id AND t.message_id=m.id
         WHERE m.account_id=?1 AND m.id=?2",
        params![account,message,Utc::now().to_rfc3339()],|r|r.get(0),
    ).map_err(CoreError::from)
}

fn apply_attention(
    conn: &Connection,
    account: AccountId,
    thread: &str,
    fallback_message: i64,
    attention: &ThreadAttentionDecision,
    job: FirstLoadJob,
) -> Result<()> {
    let existing:Option<String>=conn.query_row("SELECT attention_json FROM agent_thread_attention WHERE account_id=?1 AND thread_id=?2",
        params![account,thread],
        |r|r.get(0)).optional()?;
    let existing: ThreadAttentionDecision = existing
        .as_deref()
        .map(decode)
        .transpose()?
        .unwrap_or_default();
    let existing_ids: std::collections::HashSet<_> = existing
        .actions
        .iter()
        .filter_map(|a| a.id.as_deref())
        .collect();
    let mut seen = std::collections::HashSet::new();
    for action in &attention.actions {
        if let Some(id) = action.id.as_deref()
            && (!existing_ids.contains(id) || !seen.insert(id))
        {
            return Err(CoreError::InvalidInput(
                "unknown or duplicate attention action id".into(),
            ));
        }
    }
    let mut attention = attention.clone();
    let pinned = thread_fye_override(conn, account, thread)?;
    if let Some(show) = pinned {
        attention.show_in_fye = show;
    }
    for action in &mut attention.actions {
        if action.id.is_none() {
            action.id =
                Some(conn.query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0))?);
        }
    }
    let mut latest: Option<(String, i64)> = None;
    for id in &attention.relevant_message_ids {
        let m = read_message(conn, account, *id)?;
        if m.thread_id != thread {
            return Err(CoreError::InvalidInput(
                "attention references another thread".into(),
            ));
        }
        // Nor an archive row, named or not: the model may cite old mail as
        // relevant, and it still never becomes the card.
        if !m.is_sent && !m.is_spam && !is_archive_conn(conn, account, m.id)? {
            let activity = (effective_activity(conn, account, m.id)?, m.id);
            if latest.as_ref().is_none_or(|previous| activity > *previous) {
                latest = Some(activity);
            }
        }
    }
    // Evidence in another thread may update attention, but cannot choose a new
    // representative for the target or replace its per-message classification.
    let cross_thread = read_message(conn, account, fallback_message)?.thread_id != thread;
    if cross_thread {
        let previous: Option<(String, i64)> = conn
            .query_row(
                concat!(
                    "SELECT a.relevant_activity,a.message_id FROM agent_thread_attention a
             JOIN messages m ON m.account_id=a.account_id AND m.id=a.message_id
             WHERE a.account_id=?1 AND a.thread_id=?2 AND m.thread_id=?2 AND NOT ",
                    own_mail_sql!(),
                    " AND m.is_spam=0"
                ),
                params![account, thread],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if previous.is_some() {
            latest = previous;
        }
    }
    let representative = match latest {
        Some(activity) => Some(activity),
        None => conn
            .query_row(
                concat!("SELECT strftime('%Y-%m-%dT%H:%M:%fZ',min(julianday(m.received_at),COALESCE(julianday(t.created_at),julianday(?3)),julianday(?3))) AS activity,m.id
             FROM messages m LEFT JOIN triage t ON t.account_id=m.account_id AND t.message_id=m.id
             WHERE m.account_id=?1 AND m.thread_id=?2 AND m.is_archive=0
             ORDER BY (NOT ",own_mail_sql!()," AND m.is_spam=0) DESC,activity DESC,m.id DESC LIMIT 1"),
                params![account, thread,Utc::now().to_rfc3339()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?,
    };
    // Sent-only/spam-only threads retain a local anchor and their provenance;
    // the view's sent/spam filters keep them off inbound surfaces. An absent
    // target has no projection to update, and must not abort unrelated work.
    let Some((activity, id)) = representative else {
        return Ok(());
    };
    // Classification belongs to the displayed representative. Evidence
    // provenance is tracked independently in agent_attention_sources.
    let decision_message = id;
    let unresolved_since = if attention.actions.iter().any(|action| !action.resolved)
        || attention.state == crate::triage::decision::AttentionState::NeedsUser
    {
        Some(Utc::now().to_rfc3339())
    } else {
        None
    };
    let (first_load, held) = first_load_flags(
        conn,
        account,
        thread,
        id,
        job,
        pinned.is_some(),
        &mut attention,
    )?;
    conn.execute(
        "INSERT INTO agent_thread_attention(
             account_id,thread_id,message_id,show_in_fye,attention_json,
             relevant_activity,unresolved_since,decision_message_id,first_load,held)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
         ON CONFLICT(account_id,thread_id) DO UPDATE SET
             first_load=excluded.first_load,
             held=excluded.held,
             message_id=excluded.message_id,
             decision_message_id=excluded.decision_message_id,
             revision=agent_thread_attention.revision+1,
             show_in_fye=excluded.show_in_fye,
             attention_json=excluded.attention_json,
             relevant_activity=excluded.relevant_activity,
             unresolved_since=CASE WHEN excluded.unresolved_since IS NULL THEN NULL
                 ELSE COALESCE(agent_thread_attention.unresolved_since,excluded.unresolved_since) END",
        params![account,thread,id,attention.show_in_fye,json(&attention)?,
            activity,unresolved_since,decision_message,first_load,held],
    )?;
    Ok(())
}

/// THE MOST FIRST-LOAD THREADS FYE SHOWS AT ONCE. A new account's first run
/// triages a month of mail in one sitting, and every thread the model would
/// have surfaced on the day it arrived lands on the board together: forty
/// rows of backlog is not a sitrep, it is a chore list on day one. So the
/// backfill's picks compete for this many places, by the board's own rank,
/// and the rest stay in All mail. Mail that arrives after the first run is
/// never counted against it.
pub(super) const FIRST_LOAD_FYE_CAP: usize = 15;

/// What the job writing a thread's attention says about the first-load
/// allowance. The default is a write on behalf of another thread (a related
/// update): neither asked about nor being sorted.
#[derive(Debug, Clone, Copy, Default)]
struct FirstLoadJob {
    /// A person asked for this message again: its thread leaves the
    /// allowance for good.
    asked: bool,
    /// The first decision on mail the first-run backfill brought in: the
    /// backlog being sorted, the only picks that compete for a place.
    sorting: bool,
}

/// Is every live message in the thread one the first-run backfill ingested
/// (`triage.first_load`)? Live mail in either direction, the owner's own
/// reply included, makes the thread current, and current threads are never
/// capped. `''` is no thread: the message alone answers.
fn thread_is_first_load(
    conn: &Connection,
    account: AccountId,
    thread: &str,
    message: i64,
) -> Result<bool> {
    let current: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages m
             LEFT JOIN triage t ON t.account_id=m.account_id AND t.message_id=m.id
             WHERE m.account_id=?1 AND m.is_archive=0 AND m.is_spam=0
               AND (CASE WHEN ?2='' THEN m.id=?3 ELSE m.thread_id=?2 END)
               AND COALESCE(t.first_load,0)=0)",
        params![account, thread, message],
        |r| r.get(0),
    )?;
    Ok(!current)
}

/// The first-load flags a write to a thread's attention leaves, and the FYE
/// pick it may keep (`attention.show_in_fye` is lowered in place when the
/// pick is held, so the JSON copy the sitrep reads agrees with the column).
///
/// - `first_load` is the thread's mail, not the job: true while every live
///   message in it came from the first-run backfill. Once false (live mail
///   arrived, the owner replied, or they asked for a re-triage: `asked`) it
///   stays false.
/// - A pick the owner made (`pinned`) is theirs: never held.
/// - A held thread stays held, and a thread showing keeps its place.
/// - A pick on a thread not showing lands on the board only when it is
///   `sorting` (the first decision on backfilled mail, which competes for a
///   place: [`cap_first_load_fye`] holds the excess) or pressing. Any other
///   pick is held: background re-runs never bring backlog back. Only
///   [`time_sensitive`] lets a held thread in.
fn first_load_flags(
    conn: &Connection,
    account: AccountId,
    thread: &str,
    message: i64,
    job: FirstLoadJob,
    pinned: bool,
    attention: &mut ThreadAttentionDecision,
) -> Result<(bool, bool)> {
    let existing: Option<(bool, bool, bool)> = conn
        .query_row(
            "SELECT first_load,show_in_fye,held FROM agent_thread_attention
             WHERE account_id=?1 AND thread_id=?2",
            params![account, thread],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let first_load = !job.asked
        && existing.is_none_or(|(was, _, _)| was)
        && thread_is_first_load(conn, account, thread, message)?;
    if !first_load || pinned || !attention.show_in_fye {
        return Ok((first_load, false));
    }
    let held = match existing {
        Some((_, _, true)) => true,
        Some((_, true, false)) => false,
        None | Some((_, false, false)) => !job.sorting && !time_sensitive(attention, Utc::now()),
    };
    if held {
        attention.show_in_fye = false;
    }
    Ok((first_load, held))
}

/// How far ahead a first-load thread's stated date makes it too pressing to
/// hold back: a bill due this week is on the board whatever the cap says.
const FIRST_LOAD_DUE_WITHIN_DAYS: i64 = 7;

/// The model's urgency at or above which a first-load thread is never held.
/// The prompt asks for "high urgency" on fraud checks and security
/// incidents; this is where that lands.
const FIRST_LOAD_URGENT: f64 = 0.8;

/// A first-load thread too pressing to hold: rated [`FIRST_LOAD_URGENT`],
/// or its stated date (`attention_at`) falls within
/// [`FIRST_LOAD_DUE_WITHIN_DAYS`], or has passed while the thread still
/// needs the owner (an overdue bill; a meeting that already happened is
/// informational and stays backlog). A date the parser cannot read is not a
/// reason to show mail early. Dates are compared by UTC day: a day of slack
/// on a week's window.
fn time_sensitive(attention: &ThreadAttentionDecision, now: DateTime<Utc>) -> bool {
    use crate::triage::decision::{AttentionState, StatedTime};
    let factors = &attention.factors;
    if factors.urgency.is_finite() && factors.urgency >= FIRST_LOAD_URGENT {
        return true;
    }
    let day = match factors.attention_at.as_ref().and_then(|at| at.stated()) {
        Some(StatedTime::Instant(at)) => at.date_naive(),
        Some(StatedTime::Local(at)) => at.date(),
        Some(StatedTime::Day(day)) => day,
        None => return false,
    };
    let today = now.date_naive();
    if day < today {
        return attention.state == AttentionState::NeedsUser;
    }
    day <= (now + Duration::days(FIRST_LOAD_DUE_WITHIN_DAYS)).date_naive()
}

/// Hold first-load mail to [`FIRST_LOAD_FYE_CAP`] places in FYE. Runs in every
/// placing triage commit, in two steps:
///
/// 1. LET IN what turned pressing: a held thread that is now
///    [`time_sensitive`] gets its FYE pick back. The check rides on triage
///    commits, so a held bill surfaces on the first commit after it comes
///    within the window, not at a tick of its own.
/// 2. HOLD BACK the excess, ONLY WHILE THE BACKLOG IS BEING SORTED (`sorting`:
///    this commit is the first decision on backfilled mail): rank the board
///    the way it is served, and every first-load thread past the cap that is
///    not pressing is held. Any other commit holds nothing, so once the
///    backlog is sorted a row the owner is looking at never vanishes on its
///    own.
///
/// IT NEVER REFILLS. Closing rows ahead of a held thread does not let it in:
/// a board that tops itself up after each Done never empties, and reaching
/// the end of it is the point.
///
/// The cap counts only threads it can move and that are on the board: not a
/// pinned or hidden thread (`agent_thread_preferences`), not done, snoozed or
/// reminded mail, and not a pressing one.
fn cap_first_load_fye(
    conn: &Connection,
    account: AccountId,
    sorting: bool,
    now: DateTime<Utc>,
) -> Result<()> {
    let candidates: Vec<(String, bool, String)> = conn
        .prepare_cached(
            "SELECT a.thread_id,a.held,a.attention_json FROM agent_thread_attention a
             JOIN triage t ON t.account_id=a.account_id AND t.message_id=a.message_id
             WHERE a.account_id=?1 AND a.first_load=1 AND (a.show_in_fye=1 OR a.held=1)
               AND t.status!='done' AND t.reminded_at IS NULL
               AND (t.remind_at IS NULL OR t.remind_at<=?2)
               AND NOT EXISTS(SELECT 1 FROM agent_thread_preferences p
                              WHERE p.account_id=a.account_id AND p.thread_id=a.thread_id)",
        )?
        .query_map(params![account, now.to_rfc3339()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<std::result::Result<_, _>>()?;
    // `movable`: on the board and not pressing, the only threads the cap counts.
    let mut release = Vec::new();
    let mut movable = std::collections::HashSet::new();
    for (thread, held, attention) in candidates {
        let attention: ThreadAttentionDecision = decode(&attention)?;
        match (held, time_sensitive(&attention, now)) {
            (true, true) => release.push(thread),
            (false, false) => {
                movable.insert(thread);
            }
            _ => {}
        }
    }
    // Column and JSON together: the sitrep's tiering reads the JSON copy.
    // No revision bump: this is not new evidence about the thread, and a
    // bump would turn every in-flight job that read it stale for nothing.
    let mut set = conn.prepare_cached(
        "UPDATE agent_thread_attention SET show_in_fye=?3,held=NOT ?3,
         attention_json=json_set(attention_json,'$.show_in_fye',json(CASE WHEN ?3 THEN 'true' ELSE 'false' END))
         WHERE account_id=?1 AND thread_id=?2",
    )?;
    for thread in &release {
        set.execute(params![account, thread, true])?;
    }
    let mut hold = Vec::new();
    if sorting && movable.len() > FIRST_LOAD_FYE_CAP {
        // The ranking config the board is served with lives in the daemon's
        // config, not the store; the default is what it ships with.
        let mut items = list_items(conn, account, "fye", now)?;
        rank_items(&mut items, &RankingConfig::default(), now);
        let mut seen = std::collections::HashSet::new();
        hold = items
            .into_iter()
            .filter(|item| movable.contains(&item.thread_id) && seen.insert(item.thread_id.clone()))
            .skip(FIRST_LOAD_FYE_CAP)
            .map(|item| item.thread_id)
            .collect();
    }
    for thread in &hold {
        set.execute(params![account, thread, false])?;
    }
    if !hold.is_empty() || !release.is_empty() {
        eprintln!(
            "squelch: first-load FYE cap held {} thread(s), let in {}",
            hold.len(),
            release.len()
        );
    }
    Ok(())
}

pub(super) fn list_items(
    conn: &Connection,
    account: AccountId,
    destination: &str,
    now: DateTime<Utc>,
) -> Result<Vec<AgentListItem>> {
    list_items_with_query(conn, account, destination, now, &AgentListQuery::default())
}

/// THE LISTS' BACKSTOP against the archive, over aliases `m` and `t`: FYE,
/// Reading and Records list live mail. Every path that could put an archive
/// row in one is closed where it starts (no decision, no attention, no
/// placement without promotion); this is the line that holds if one of them
/// is reopened. A reminder the owner set and that has fired is their own
/// instruction to see the row again, as in the human inventory.
macro_rules! archive_listable_sql {
    () => {
        "(m.is_archive=0 OR t.reminded_at IS NOT NULL)"
    };
}

fn list_items_with_query(
    conn: &Connection,
    account: AccountId,
    destination: &str,
    now: DateTime<Utc>,
    query: &AgentListQuery,
) -> Result<Vec<AgentListItem>> {
    // AUTH MAIL does not take the model's FYE pick: it lives on the Auth
    // page's decision rail, and listing every sign-in alert in both places
    // means ruling on it twice. A thread is in FYE for its auth mail only when
    // one of its not-done auth messages was ruled "Not me" or is a critical
    // account change still unruled (`auth_fye_threads_cte!`, shared with the
    // sitrep's placement and the stats), and that message is the row shown,
    // whichever message represents the thread. An explicit thread preference
    // (the human pinned it) and a fired reminder still win.
    //
    // THE USER'S OWN MAIL gets the same treatment for a plainer reason: they
    // wrote it. A thread whose representative is theirs (see `OWN_MAIL` for how
    // a sent message comes to sit in the inbound listing at all) is never the
    // model's to surface, and the same two human acts still win. The other
    // lists exclude it flatly, as they always have the stored sent copy: own
    // mail carries no decision to place, so a placement on it is a leftover
    // from being triaged as inbound and stays hidden until it is retired.
    let sql = if destination == "fye" {
        // `m` and `d` are the message the row SHOWS: the qualifying auth
        // message when the thread has one (`q`), else the representative.
        // `t` stays the REPRESENTATIVE's triage row, so the thread's own
        // reminder and activity time (and so its rank, and the agent door's
        // `since`) are what they were before any auth mail qualified. Done
        // and snooze are the shown message's: `q` is not done by
        // construction, and a snooze on the representative does not hide a
        // takeover-class change or a "Not me" (the sitrep's Standing band
        // judges the shown row the same way).
        concat!("WITH ",auth_fye_threads_cte!()," SELECT
             m.id,m.thread_id,m.from_addr,m.subject,
             strftime('%Y-%m-%dT%H:%M:%fZ',min(julianday(a.relevant_activity),COALESCE(julianday(t.created_at),julianday(?2)),julianday(?2))),d.decision_json,a.attention_json,a.unresolved_since,a.decision_message_id,m.from_name
             FROM agent_thread_attention a
             LEFT JOIN auth_fye_threads q ON q.account_id=a.account_id AND
               q.thread_key=(CASE WHEN a.thread_id='' THEN '#'||a.message_id ELSE a.thread_id END)
             JOIN messages m ON m.account_id=a.account_id AND m.id=COALESCE(q.id,a.message_id)
             LEFT JOIN agent_message_decisions d ON d.account_id=m.account_id AND
             d.message_id=m.id LEFT JOIN triage t ON t.account_id=a.account_id AND
             t.message_id=a.message_id LEFT JOIN agent_thread_preferences pref ON pref.account_id=a.account_id AND
             pref.thread_id=a.thread_id WHERE a.account_id=?1 AND ?3='fye' AND
             (COALESCE(pref.show_in_fye,CASE WHEN ",own_mail_sql!()," THEN 0
             WHEN q.id IS NOT NULL THEN 1
             WHEN ",auth_mail_sql!()," THEN 0 ELSE a.show_in_fye END)=1 OR t.reminded_at IS NOT NULL) AND m.is_sent=0 AND m.is_spam=0 AND ",archive_listable_sql!()," AND (q.id IS NOT NULL OR COALESCE(t.status,'new')!='done') AND
             (q.id IS NOT NULL OR t.remind_at IS NULL OR t.remind_at<=?2) AND ?4 IS ?4 AND ?5 IS ?5")
    } else if destination == "records" {
        concat!("SELECT m.id,m.thread_id,m.from_addr,m.subject,
             strftime('%Y-%m-%dT%H:%M:%fZ',min(julianday(m.received_at),COALESCE(julianday(t.created_at),julianday(?2)),julianday(?2))),d.decision_json,a.attention_json,a.unresolved_since,d.message_id,m.from_name
             FROM agent_message_decisions d JOIN messages m ON m.account_id=d.account_id AND m.id=d.message_id
             LEFT JOIN triage t ON t.account_id=m.account_id AND t.message_id=m.id
             LEFT JOIN agent_thread_attention a ON a.account_id=m.account_id AND a.thread_id=m.thread_id
             WHERE d.account_id=?1 AND ?3='records' AND NOT ",own_mail_sql!()," AND m.is_spam=0 AND ",archive_listable_sql!(),"
             AND EXISTS(SELECT 1 FROM json_each(d.decision_json,'$.records') r
                 WHERE json_extract(r.value,'$.kind') IN ('bill','receipt','delivery','event','financial_update'))
             AND (?4 OR COALESCE(t.status,'new')!='done')
             AND (?5 IS NULL OR min(julianday(m.received_at),COALESCE(julianday(t.created_at),julianday(?2)),julianday(?2))>=julianday(?5))")
    } else {
        concat!("SELECT
             m.id,m.thread_id,m.from_addr,m.subject,
             strftime('%Y-%m-%dT%H:%M:%fZ',min(julianday(m.received_at),COALESCE(julianday(t.created_at),julianday(?2)),julianday(?2))),d.decision_json,a.attention_json,a.unresolved_since,d.message_id,m.from_name
             FROM agent_message_destinations p JOIN messages m ON m.account_id=p.account_id AND
             m.id=p.message_id JOIN agent_message_decisions d ON d.account_id=m.account_id AND
             d.message_id=m.id LEFT JOIN triage t ON t.account_id=m.account_id AND t.message_id=m.id
             LEFT JOIN agent_thread_attention a ON a.account_id=m.account_id AND
             a.thread_id=m.thread_id WHERE p.account_id=?1 AND p.destination=?3 AND NOT ",own_mail_sql!()," AND
             m.is_spam=0 AND ",archive_listable_sql!()," AND (?4 OR COALESCE(t.status,'new')!='done') AND
             (?5 IS NULL OR min(julianday(m.received_at),COALESCE(julianday(t.created_at),julianday(?2)),julianday(?2))>=julianday(?5))")
    };
    let mut stmt = conn.prepare_cached(sql)?;
    let raw = stmt
        .query_map(
            params![
                account,
                now.to_rfc3339(),
                destination,
                query.include_done,
                query.since.map(|value| value.to_rfc3339())
            ],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, i64>(8)?,
                    r.get::<_, Option<String>>(9)?,
                ))
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut items: Vec<AgentListItem> = raw
        .into_iter()
        .map(
            |(
                message_id,
                thread_id,
                from_addr,
                subject,
                received_at,
                d,
                a,
                unresolved_since,
                decision_source_message_id,
                from_name,
            )| {
                // Pending classification must not hide valid thread attention,
                // nor borrow kinds/records from an unrelated update's source.
                let decision =
                    d.as_deref()
                        .map(decode)
                        .transpose()?
                        .unwrap_or_else(|| MessageDecision {
                            summary: subject.clone(),
                            reason: "Triage pending".into(),
                            ..Default::default()
                        });
                Ok(AgentListItem {
                    message_id,
                    decision_source_message_id,
                    thread_id,
                    from_addr,
                    from_name: non_blank(from_name),
                    subject,
                    received_at,
                    decision,
                    attention: a.as_deref().map(decode).transpose()?.unwrap_or_default(),
                    score: 0.0,
                    unresolved_since,
                    ranking: None,
                })
            },
        )
        .collect::<Result<_>>()?;
    if destination == "fye" {
        let mut visible = Vec::new();
        for item in items {
            if thread_fye_override(conn, account, &item.thread_id)? != Some(false) {
                visible.push(item);
            }
        }
        items = visible;
        append_due_reminders(conn, account, &mut items)?;
    }
    Ok(items)
}

/// The fired reminders, newest first: what [`append_due_reminders`] adds to
/// every FYE listing. A `const` so the plan test pins THIS statement to its
/// index (`idx_triage_reminded_at`, created in `migrate.rs`): it runs on every
/// Standing-band read, which is the client's 10 s poll, and without the index
/// it walked every triage row in the account.
pub(super) const DUE_REMINDERS_SQL: &str =
    "SELECT m.id,m.thread_id,m.from_addr,m.subject,m.received_at,t.reminded_at,d.decision_json,
            m.from_name
     FROM messages m JOIN triage t ON t.account_id=m.account_id AND t.message_id=m.id
     LEFT JOIN agent_message_decisions d ON d.account_id=m.account_id AND d.message_id=m.id
     WHERE m.account_id=?1 AND m.is_sent=0 AND m.is_spam=0 AND t.status!='done'
       AND t.reminded_at IS NOT NULL AND t.remind_at IS NULL
     ORDER BY t.reminded_at DESC,m.id DESC";

/// The newest runs for one message, for `agent_diagnostics`. A `const` so the
/// plan test pins it to `idx_agent_triage_runs_message`: without the index
/// this is a scan of every run in the retention window (`retention.rs`).
pub(super) const MESSAGE_RUNS_SQL: &str =
    "SELECT id,job_id,outcome,completed_at,metadata_json FROM agent_triage_runs
     WHERE account_id=?1 AND message_id=?2 ORDER BY id DESC LIMIT 25";

/// A user's scheduled reminder is sufficient authority to surface mail even
/// while model processing is unavailable. This is a view, never a stored verdict.
fn append_due_reminders(
    conn: &Connection,
    account: AccountId,
    items: &mut Vec<AgentListItem>,
) -> Result<()> {
    let mut statement = conn.prepare(DUE_REMINDERS_SQL)?;
    let rows = statement
        .query_map([account], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (id, thread, sender, subject, received, reminded, encoded, sender_name) in rows {
        if thread_fye_override(conn, account, &thread)? == Some(false) {
            continue;
        }
        if items.iter().any(|item| item.thread_id == thread) {
            continue;
        }
        let decision = encoded
            .as_deref()
            .map(decode)
            .transpose()?
            .unwrap_or_else(|| MessageDecision {
                summary: subject.clone(),
                reason: "Your reminder is due; triage is pending".into(),
                ..Default::default()
            });
        let attention = ThreadAttentionDecision {
            show_in_fye: true,
            state: crate::triage::decision::AttentionState::NeedsUser,
            summary: "Your reminder is due".into(),
            relevant_message_ids: vec![id],
            ..Default::default()
        };
        items.push(AgentListItem {
            message_id: id,
            decision_source_message_id: id,
            thread_id: thread,
            from_addr: sender,
            from_name: non_blank(sender_name),
            subject,
            received_at: received,
            decision,
            attention,
            score: 0.0,
            unresolved_since: Some(reminded),
            ranking: None,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triage::decision::{AccessAssessment, EmailKind};
    /// The thread half of an external listing row (current attention from
    /// allowed mail, and the thread's guard), from the production pieces.
    pub(super) fn thread_half_allowed(conn: &Connection, account: AccountId, thread: &str) -> bool {
        match current_attention_sources(conn, account, thread).unwrap() {
            None => false,
            Some(ids) => {
                super::super::messages::thread_allowed_with_conn(conn, account, thread, &ids)
                    .unwrap()
            }
        }
    }

    pub(super) fn fixture() -> SqliteStore {
        let store = SqliteStore::open_in_memory().unwrap();
        {
            let conn = store.lock().unwrap();
            conn.execute("INSERT INTO accounts(id,email,created_at) VALUES(1,'a@test','2026-01-01'),(2,'b@test','2026-01-01')",[]).unwrap();
            for (id, account, thread) in [(1, 1, "one"), (2, 1, "two"), (3, 2, "other")] {
                conn.execute("INSERT INTO
             messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body)
             VALUES(?1,?2,?3,?4,'sender@test','subject',?5,'snippet','body')",
        params![id,account,id.to_string(),thread,Utc::now().to_rfc3339()]).unwrap();
                conn.execute(
                    "INSERT INTO triage(account_id,message_id,created_at) VALUES(?1,?2,?3)",
                    params![account, id, Utc::now().to_rfc3339()],
                )
                .unwrap();
            }
        }
        store
    }
    /// THE LANE'S DUE READ: when the next claimable job of each notification
    /// kind is due, over the rows the claim seeks, taking nothing. Queued and
    /// available reads as its own time (in the past: due now); claimed, as
    /// the end of its lease, which is when the claim would hand it out again;
    /// completed, as nothing. A kind the lane does not claim, and another
    /// tenant's job, are not counted. And it is a read: the job it reports
    /// is still there for the claim.
    #[test]
    fn next_agent_jobs_due_read_the_queue_without_claiming() {
        let store = fixture();
        let kinds = ["notification", "deliberate_notification"];
        let due = |kinds: &[&str]| {
            let mut rows = store.next_agent_jobs_due(1, kinds).unwrap();
            rows.sort();
            rows
        };
        assert_eq!(due(&kinds), []);
        let past = Utc::now() - Duration::minutes(5);
        let ahead = Utc::now() + Duration::hours(1);
        store.lock().unwrap().execute(
            "INSERT INTO agent_triage_jobs(account_id,message_id,kind,trigger,input_revision,available_at)
             VALUES(1,1,'notification','arrival',1,?1),(1,2,'deliberate_notification','decision',1,?2),
                   (1,2,'triage','arrival',1,?1),(2,3,'notification','arrival',1,?1)",
            params![past.to_rfc3339(), ahead.to_rfc3339()],
        ).unwrap();
        assert_eq!(due(&[]), [], "no kinds, nothing");
        assert_eq!(
            due(&kinds),
            [
                ("deliberate_notification".to_string(), ahead),
                ("notification".to_string(), past)
            ],
            "each kind's own next; the triage row at the same time is not the lane's"
        );
        assert!(past <= Utc::now(), "and the queued one is due now");
        let job = store
            .claim_agent_job(1, "notification", Utc::now(), 60)
            .unwrap()
            .expect("the read took nothing");
        assert_eq!(job.message_id, 1);
        let leased_until = due(&["notification"])[0].1;
        assert!(
            leased_until > Utc::now() + Duration::seconds(55)
                && leased_until <= Utc::now() + Duration::seconds(60),
            "leased, it is due when the lease lapses: {leased_until}"
        );
        assert!(store.complete_agent_job(&job).unwrap());
        assert_eq!(
            due(&kinds),
            [("deliberate_notification".to_string(), ahead)],
            "completed, its kind has nothing pending"
        );
        assert_eq!(
            store.next_agent_jobs_due(2, &kinds).unwrap(),
            [("notification".to_string(), past)],
            "the other tenant's own queue"
        );
        // WHAT THE CLAIM WOULD REFUSE, THE READ DOES NOT COUNT: a job whose
        // message is gone, and a leased row no claim wrote (no lease end).
        store.lock().unwrap().execute(
            "INSERT INTO agent_triage_jobs(account_id,message_id,kind,trigger,input_revision,available_at,state)
             VALUES(1,99,'notification','arrival',1,?1,'queued'),(1,1,'notification','orphan-lease',1,?1,'leased')",
            params![past.to_rfc3339()],
        ).unwrap();
        assert_eq!(
            due(&kinds),
            [("deliberate_notification".to_string(), ahead)],
            "neither the orphan nor the endless lease is a job to wake for"
        );
        assert!(
            store
                .claim_agent_job(1, "notification", Utc::now(), 60)
                .unwrap()
                .is_none(),
            "and the claim agrees"
        );
    }

    /// The due read SEARCHES the account's ready index on account, state and
    /// kind, never scans the jobs table; and THE CANARY: with the index gone
    /// the pinned statement no longer prepares, so the assertion above was
    /// testing the index and not the planner's mood.
    #[test]
    fn next_jobs_due_seeks_the_account_ready_index() {
        let store = fixture();
        let conn = store.lock().unwrap();
        let plan = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {NEXT_JOBS_DUE_SQL}"))
            .unwrap()
            .query_map(
                params![1, r#"["notification","deliberate_notification"]"#],
                |r| r.get::<_, String>(3),
            )
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
            .join("\n");
        assert!(
            plan.contains(
                "INDEX idx_agent_jobs_account_ready (account_id=? AND state=? AND kind=?)"
            ),
            "{plan}"
        );
        assert!(!plan.contains("SCAN agent_triage_jobs"), "{plan}");
        assert!(
            plan.contains("SEARCH m USING INTEGER PRIMARY KEY"),
            "the message join is a rowid lookup per pending row:\n{plan}"
        );
        conn.execute_batch("DROP INDEX idx_agent_jobs_account_ready")
            .unwrap();
        assert!(
            conn.prepare(NEXT_JOBS_DUE_SQL).is_err(),
            "the statement is pinned to the index it was measured on"
        );
    }

    /// The dependent walk SEEKS each hop from the row it is on, on a store
    /// with no `sqlite_stat1`, which is the store the pin is for: the step's
    /// row is outermost and each edge table is reached by its reverse index on
    /// both columns, never walked on `account_id`. THE CANARY: with either
    /// index gone the statement no longer prepares.
    #[test]
    fn dependent_refreshes_walk_from_the_source() {
        let store = fixture();
        let plan = store
            .lock()
            .unwrap()
            .prepare(&format!("EXPLAIN QUERY PLAN {DEPENDENT_REFRESHES_SQL}"))
            .unwrap()
            .query_map(params![1, 1], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        for edge in [
            "SEARCH d USING INDEX idx_agent_decision_source_reverse (account_id=? AND source_message_id=?)",
            "SEARCH s USING INDEX idx_agent_attention_source_reverse (account_id=? AND source_message_id=?)",
        ] {
            let at = plan.iter().position(|line| line == edge);
            assert!(
                at.is_some_and(|at| at > 0 && plan[at - 1] == "SCAN a"),
                "{edge} is not a seek from the step's row:\n{plan:#?}"
            );
        }
        assert!(
            plan.iter().any(|line| line
                == "SEARCH t USING INDEX sqlite_autoindex_agent_thread_attention_1 (account_id=? AND thread_id=?)"),
            "{plan:#?}"
        );
        assert!(
            plan.iter()
                .any(|line| line == "SEARCH m USING INTEGER PRIMARY KEY (rowid=?)"),
            "each dependent is a rowid lookup:\n{plan:#?}"
        );
        // `a` is the step's row and `dep` the walk's result; every table is
        // reached by a SEARCH.
        assert!(
            plan.iter()
                .filter(|line| line.starts_with("SCAN"))
                .all(|line| ["SCAN CONSTANT ROW", "SCAN a", "SCAN dep"].contains(&line.as_str())),
            "only the walk itself is scanned:\n{plan:#?}"
        );
        for index in [
            "idx_agent_decision_source_reverse",
            "idx_agent_attention_source_reverse",
        ] {
            let store = fixture();
            let conn = store.lock().unwrap();
            conn.execute_batch(&format!("DROP INDEX {index}")).unwrap();
            assert!(
                conn.prepare(DEPENDENT_REFRESHES_SQL).is_err(),
                "the statement is pinned to {index}"
            );
        }
    }

    /// THE WALK QUEUES WHAT THE WHOLE-EDGE WALK QUEUED. The statement
    /// [`DEPENDENT_REFRESHES_SQL`] replaced is kept here as the oracle, and
    /// both are asked about every message of seeded random citation graphs:
    /// decision and attention edges, cycles and self-citations, archive mail
    /// partway along a chain, own and spam mail (refreshed as access reviews),
    /// messages with no agent state, and a second account whose edges name
    /// the first account's ids.
    #[test]
    fn dependent_refreshes_match_the_whole_edge_walk() {
        const WHOLE_EDGE_WALK_SQL: &str = concat!(
            "WITH RECURSIVE edges(dependent,source) AS (
                SELECT message_id,source_message_id FROM agent_decision_sources WHERE account_id=?1
                UNION SELECT a.message_id,s.source_message_id FROM agent_attention_sources s
                    JOIN agent_thread_attention a ON a.account_id=s.account_id AND a.thread_id=s.thread_id WHERE s.account_id=?1
             ), affected(message_id) AS (
                SELECT dependent FROM edges WHERE source=?2
                UNION SELECT e.dependent FROM edges e JOIN affected a ON e.source=a.message_id
             ) SELECT m.id,a.revision,CASE WHEN ",
            own_mail_sql!(),
            " OR m.is_spam=1 THEN 'access' ELSE 'triage' END
               FROM affected d JOIN messages m ON m.account_id=?1 AND m.id=d.message_id
               JOIN agent_message_state a ON a.account_id=m.account_id AND a.message_id=m.id
               WHERE m.id!=?2 AND m.is_archive=0 ORDER BY m.id",
        );
        // One hop and no filters: what cites the source directly.
        const CITES_DIRECTLY_SQL: &str = "SELECT message_id FROM agent_decision_sources
                 WHERE account_id=?1 AND source_message_id=?2
             UNION SELECT t.message_id FROM agent_attention_sources s
                 JOIN agent_thread_attention t ON t.account_id=s.account_id AND t.thread_id=s.thread_id
                 WHERE s.account_id=?1 AND s.source_message_id=?2";
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n) as i64
        };
        let store = SqliteStore::open_in_memory().unwrap();
        let conn = store.lock().unwrap();
        conn.execute(
            "INSERT INTO accounts(id,email,created_at) VALUES(1,'a@test','2026-01-01'),(2,'b@test','2026-01-01')",
            [],
        )
        .unwrap();
        let rows = |sql: &str, account: i64, source: i64| {
            conn.prepare_cached(sql)
                .unwrap()
                .query_map(params![account, source], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        };
        let direct = |account: i64, source: i64| {
            conn.prepare_cached(CITES_DIRECTLY_SQL)
                .unwrap()
                .query_map(params![account, source], |r| r.get::<_, i64>(0))
                .unwrap()
                .collect::<std::result::Result<std::collections::BTreeSet<_>, _>>()
                .unwrap()
        };
        let (mut compared, mut deeper, mut kinds) = (0, 0, std::collections::BTreeSet::new());
        for graph in 0..24 {
            conn.execute_batch(
                "DELETE FROM agent_attention_sources; DELETE FROM agent_thread_attention;
                 DELETE FROM agent_decision_sources; DELETE FROM agent_message_state;
                 DELETE FROM messages;",
            )
            .unwrap();
            for id in 1..=50 {
                let account = if id <= 40 { 1 } else { 2 };
                let from = ["a@test", "b@test", "sender@test", "sender@test"][next(4) as usize];
                conn.execute(
                    "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,
                         snippet,body,is_sent,is_spam,is_archive)
                     VALUES(?1,?2,?3,?4,?5,'subject','2026-01-01T00:00:00+00:00','','',?6,?7,?8)",
                    params![
                        id,
                        account,
                        id.to_string(),
                        format!("t{}", next(8)),
                        from,
                        next(10) == 0,
                        next(10) == 0,
                        next(5) == 0
                    ],
                )
                .unwrap();
                if next(8) != 0 {
                    conn.execute(
                        "INSERT INTO agent_message_state(account_id,message_id,content_snapshot,revision)
                         VALUES(?1,?2,'{}',?3)",
                        params![account, id, 1 + next(3)],
                    )
                    .unwrap();
                }
            }
            let account = |roll: i64| if roll == 0 { 2 } else { 1 };
            for _ in 0..60 {
                conn.execute(
                    "INSERT OR IGNORE INTO agent_decision_sources(account_id,message_id,source_message_id,source_revision)
                     VALUES(?1,?2,?3,1)",
                    params![account(next(5)), 1 + next(50), 1 + next(50)],
                )
                .unwrap();
            }
            for thread in 0..8 {
                if next(4) != 0 {
                    conn.execute(
                        "INSERT OR IGNORE INTO agent_thread_attention(decision_message_id,account_id,thread_id,
                             message_id,show_in_fye,attention_json,relevant_activity)
                         VALUES(?1,?2,?3,?1,0,'{}','2026-01-01T00:00:00+00:00')",
                        params![1 + next(50), account(next(5)), format!("t{thread}")],
                    )
                    .unwrap();
                }
            }
            for _ in 0..30 {
                conn.execute(
                    "INSERT OR IGNORE INTO agent_attention_sources(account_id,thread_id,source_message_id,source_revision)
                     VALUES(?1,?2,?3,1)",
                    params![account(next(5)), format!("t{}", next(8)), 1 + next(50)],
                )
                .unwrap();
            }
            for account in [1, 2] {
                for source in 1..=50 {
                    let walked = rows(DEPENDENT_REFRESHES_SQL, account, source);
                    assert_eq!(
                        walked,
                        rows(WHOLE_EDGE_WALK_SQL, account, source),
                        "graph {graph}, account {account}, source {source}"
                    );
                    let cites = direct(account, source);
                    compared += usize::from(!walked.is_empty());
                    deeper += usize::from(walked.iter().any(|(id, _, _)| !cites.contains(id)));
                    kinds.extend(walked.into_iter().map(|(_, _, kind)| kind));
                }
            }
        }
        // Not vacuous: plenty of sources had dependents, plenty of them past
        // the first hop, and both kinds of refresh were compared.
        assert!(compared > 400, "{compared}");
        assert!(deeper > 300, "{deeper}");
        assert_eq!(kinds.len(), 2, "{kinds:?}");
    }

    /// The set access check walks once and SEEKS every table, on a store with
    /// no `sqlite_stat1`: each hop and each stale-source probe is a primary
    /// key seek from the walk's row, and nothing but the walk and its seeds is
    /// scanned.
    #[test]
    fn set_access_check_seeks_every_table() {
        let store = fixture();
        let conn = store.lock().unwrap();
        let stats: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='sqlite_stat1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stats, 0, "the pin is for a store without statistics");
        let plan = conn
            .prepare(&format!(
                "EXPLAIN QUERY PLAN {}",
                super::super::messages::EXTERNAL_MESSAGES_ALLOWED_SQL
            ))
            .unwrap()
            .query_map(params![1, "[1,2]"], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            plan.iter()
                .filter(|line| line.starts_with("MATERIALIZE dependencies"))
                .count(),
            1,
            "the walk is shared by both checks:\n{plan:#?}"
        );
        let hops: Vec<usize> = plan
            .iter()
            .enumerate()
            .filter(|(_, line)| {
                line.starts_with(
                    "SEARCH d USING COVERING INDEX sqlite_autoindex_agent_decision_sources_1 (account_id=? AND message_id=?)",
                ) || line.starts_with(
                    "SEARCH d USING INDEX sqlite_autoindex_agent_decision_sources_1 (account_id=? AND message_id=?)",
                )
            })
            .map(|(at, _)| at)
            .collect();
        assert_eq!(
            hops.len(),
            2,
            "the hop and the stale-source probe:\n{plan:#?}"
        );
        assert!(
            hops.iter().all(|&at| at > 0 && plan[at - 1] == "SCAN p"),
            "each is a seek from the walk's row:\n{plan:#?}"
        );
        for seek in [
            "SEARCH m USING INTEGER PRIMARY KEY (rowid=?)",
            "SEARCH c USING INDEX sqlite_autoindex_agent_triage_corrections_1 (account_id=? AND message_id=? AND field=?)",
        ] {
            assert!(
                plan.iter().any(|line| line.starts_with(seek)),
                "{seek}:\n{plan:#?}"
            );
        }
        assert!(
            plan.iter()
                .filter(|line| line.starts_with("SCAN"))
                .all(|line| line == "SCAN p"
                    || line == "SCAN CONSTANT ROW"
                    || line.starts_with("SCAN json_each VIRTUAL TABLE")),
            "only the walk and its seeds are scanned:\n{plan:#?}"
        );
    }

    /// ONE WALK ANSWERS WHAT THE PER-MESSAGE CHECKS ANSWERED. The statement
    /// that checked one message at a time is kept here as the oracle, and
    /// every caller that now asks about a set at once is held to what the old
    /// code made of the per-message answers: a set (all of it allowed), a
    /// thread's guard (siblings an external reader counts, all allowed), a
    /// thread's attention (current, and from allowed mail), a listed item
    /// (its attention, its two messages, its thread) and the agent door's
    /// batched second look (the message and its thread). Over seeded random
    /// stores: every assessment state and a missing one, owner's seals and
    /// corrections that are not seals, sources read at a revision that has
    /// since moved, cycles, archive rows an external reader does not count,
    /// ids that do not exist and a second account's rows.
    #[test]
    fn set_access_checks_match_the_per_message_check() {
        use super::super::messages::{
            external_messages_allowed_conn, external_thread_ids_conn, thread_guard_and_subject,
        };
        const PER_MESSAGE_SQL: &str = "WITH RECURSIVE dependencies(message_id) AS (
             SELECT ?2
             UNION
             SELECT d.source_message_id FROM agent_decision_sources d
             JOIN dependencies p ON p.message_id=d.message_id
             WHERE d.account_id=?1
         )
         SELECT EXISTS(SELECT 1 FROM messages WHERE account_id=?1 AND id=?2)
           AND NOT EXISTS(
             SELECT 1 FROM dependencies d
             LEFT JOIN messages m ON m.id=d.message_id AND m.account_id=?1
             LEFT JOIN agent_message_state a ON a.message_id=d.message_id AND a.account_id=?1
             WHERE m.id IS NULL OR a.access IS NULL OR a.access!='allowed'
               OR EXISTS(SELECT 1 FROM agent_triage_corrections c WHERE c.account_id=?1
                   AND c.message_id=d.message_id AND c.field='external_access' AND c.value_json='true')
           )
           AND NOT EXISTS(
             SELECT 1 FROM agent_decision_sources d
             JOIN dependencies p ON p.message_id=d.message_id
             LEFT JOIN agent_message_state a ON a.message_id=d.source_message_id AND a.account_id=?1
             WHERE d.account_id=?1 AND (a.revision IS NULL OR a.revision!=d.source_revision)
           )";
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = move |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n) as i64
        };
        let threads: Vec<String> = (0..12).map(|t| format!("t{t}")).collect();
        let store = SqliteStore::open_in_memory().unwrap();
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO accounts(id,email,created_at) VALUES(1,'a@test','2026-01-01'),(2,'b@test','2026-01-01')",
                [],
            )
            .unwrap();
        // (allowed, refused) per kind of question, so none of them is vacuous.
        let mut seen = [[0usize; 2]; 6];
        let mut tally = |kind: usize, allowed: bool| seen[kind][usize::from(!allowed)] += 1;
        for graph in 0..16 {
            let mut door: Vec<(i64, i64, i64, String, bool)> = Vec::new();
            {
                let conn = store.lock().unwrap();
                conn.execute_batch(
                    "DELETE FROM agent_triage_corrections; DELETE FROM agent_attention_sources;
                     DELETE FROM agent_decision_sources; DELETE FROM agent_message_state;
                     DELETE FROM messages;",
                )
                .unwrap();
                let account_of = |id: i64| if id <= 40 { 1 } else { 2 };
                let mut revision = std::collections::HashMap::new();
                for id in 1..=50 {
                    conn.execute(
                        "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,
                             received_at,snippet,body,is_archive)
                         VALUES(?1,?2,?3,?4,'sender@test','subject','2026-01-01T00:00:00+00:00','','',?5)",
                        params![
                            id,
                            account_of(id),
                            id.to_string(),
                            threads[next(12) as usize],
                            next(5) == 0
                        ],
                    )
                    .unwrap();
                    let access = match next(20) {
                        0 => None,
                        1 => Some("pending"),
                        2 => Some("restricted"),
                        _ => Some("allowed"),
                    };
                    if let Some(access) = access {
                        let at = 1 + next(2);
                        revision.insert(id, at);
                        conn.execute(
                            "INSERT INTO agent_message_state(account_id,message_id,content_snapshot,revision,access)
                             VALUES(?1,?2,'{}',?3,?4)",
                            params![account_of(id), id, at, access],
                        )
                        .unwrap();
                    }
                }
                // Mostly read at the source's current revision; one in twelve
                // has since moved (or never had one).
                let read_at =
                    |source: i64, roll: i64, revision: &std::collections::HashMap<i64, i64>| {
                        if roll == 0 {
                            3
                        } else {
                            revision.get(&source).copied().unwrap_or(1)
                        }
                    };
                for _ in 0..40 {
                    let (message, source) = (1 + next(50), 1 + next(50));
                    let account = if next(6) == 0 { 2 } else { account_of(message) };
                    conn.execute(
                        "INSERT OR IGNORE INTO agent_decision_sources(account_id,message_id,source_message_id,source_revision)
                         VALUES(?1,?2,?3,?4)",
                        params![account, message, source, read_at(source, next(12), &revision)],
                    )
                    .unwrap();
                }
                for _ in 0..14 {
                    let source = 1 + next(50);
                    conn.execute(
                        "INSERT OR IGNORE INTO agent_attention_sources(account_id,thread_id,source_message_id,source_revision)
                         VALUES(?1,?2,?3,?4)",
                        params![
                            if next(6) == 0 { 2 } else { account_of(source) },
                            threads[next(12) as usize],
                            source,
                            read_at(source, next(12), &revision)
                        ],
                    )
                    .unwrap();
                }
                // A seal, and two corrections that are not one.
                for (field, value) in [
                    ("external_access", "true"),
                    ("external_access", "false"),
                    ("importance", "true"),
                ] {
                    let id = 1 + next(50);
                    conn.execute(
                        "INSERT OR IGNORE INTO agent_triage_corrections(account_id,message_id,field,value_json,source_revision,updated_at)
                         VALUES(?1,?2,?3,?4,1,'2026-01-01')",
                        params![account_of(id), id, field, value],
                    )
                    .unwrap();
                }
                let one = |account: i64, id: i64| -> bool {
                    conn.prepare_cached(PER_MESSAGE_SQL)
                        .unwrap()
                        .query_row(params![account, id], |r| r.get(0))
                        .unwrap()
                };
                for account in [1, 2] {
                    let ok = |id: i64| one(account, id);
                    // The siblings an external reader counts, spelled out here
                    // rather than read through the code under test: all but an
                    // archive row nobody has assessed (and the owner has not
                    // sealed).
                    let guard = |thread: &str| {
                        let ids: Vec<i64> = conn
                            .prepare(
                                "SELECT m.id FROM messages m
                                 LEFT JOIN agent_message_state a ON a.account_id=m.account_id AND a.message_id=m.id
                                 WHERE m.account_id=?1 AND m.thread_id=?2 AND NOT (m.is_archive=1
                                   AND (a.access IS NULL OR a.access='pending')
                                   AND NOT EXISTS(SELECT 1 FROM agent_triage_corrections c
                                       WHERE c.account_id=m.account_id AND c.message_id=m.id
                                         AND c.field='external_access' AND c.value_json='true'))",
                            )
                            .unwrap()
                            .query_map(params![account, thread], |r| r.get(0))
                            .unwrap()
                            .map(|id| id.unwrap())
                            .collect();
                        assert_eq!(
                            ids,
                            external_thread_ids_conn(&conn, account, thread).unwrap(),
                            "the siblings the guard counts"
                        );
                        !ids.is_empty() && ids.into_iter().all(ok)
                    };
                    let attention = |thread: &str| {
                        conn.prepare(
                            "SELECT s.source_message_id,s.source_revision,a.revision FROM agent_attention_sources s
                             LEFT JOIN agent_message_state a ON a.account_id=s.account_id AND a.message_id=s.source_message_id
                             WHERE s.account_id=?1 AND s.thread_id=?2",
                        )
                        .unwrap()
                        .query_map(params![account, thread], |r| {
                            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<i64>>(2)?))
                        })
                        .unwrap()
                        .map(|row| row.unwrap())
                        .all(|(id, expected, actual)| actual == Some(expected) && ok(id))
                    };
                    assert!(!external_messages_allowed_conn(&conn, account, &[]).unwrap());
                    for _ in 0..60 {
                        let set: Vec<i64> = (0..1 + next(5)).map(|_| 1 + next(55)).collect();
                        let want = set.iter().all(|&id| ok(id));
                        assert_eq!(
                            external_messages_allowed_conn(&conn, account, &set).unwrap(),
                            want,
                            "graph {graph}, account {account}, set {set:?}"
                        );
                        tally(0, want);
                    }
                    for thread in &threads {
                        let want = guard(thread);
                        assert_eq!(
                            thread_guard_and_subject(&conn, account, thread).is_ok(),
                            want,
                            "graph {graph}, account {account}, guard {thread}"
                        );
                        tally(1, want);
                        let want = attention(thread) && guard(thread);
                        assert_eq!(
                            thread_half_allowed(&conn, account, thread),
                            want,
                            "graph {graph}, account {account}, thread half {thread}"
                        );
                        tally(2, want);
                    }
                    let mut listed = Vec::new();
                    for _ in 0..30 {
                        let thread = &threads[next(12) as usize];
                        let (message, source) = (1 + next(50), 1 + next(50));
                        let want = attention(thread) && ok(message) && ok(source) && guard(thread);
                        // A thread's only row: one walk.
                        let only = super::super::messages::ListingJudge::new([thread.as_str()])
                            .allowed(&conn, account, thread, &[message, source], |thread| {
                                current_attention_sources(&conn, account, thread)
                            })
                            .unwrap();
                        assert_eq!(
                            only, want,
                            "graph {graph}, account {account}, item {message}/{source} in {thread}"
                        );
                        tally(3, want);
                        listed.push((thread.clone(), message, source, want));
                        door.push((
                            account,
                            message,
                            source,
                            thread.clone(),
                            ok(message) && ok(source) && guard(thread),
                        ));
                    }
                    // The listing as a whole, where 30 rows over 12 threads
                    // repeat threads and take the judged-once path.
                    let mut items: Vec<AgentListItem> = listed
                        .iter()
                        .map(|(thread, message, source, _)| AgentListItem {
                            message_id: *message,
                            decision_source_message_id: *source,
                            thread_id: thread.clone(),
                            from_addr: "sender@test".into(),
                            from_name: Some("Sender".into()),
                            subject: "subject".into(),
                            received_at: "2026-01-01T00:00:00+00:00".into(),
                            decision: MessageDecision::default(),
                            attention: ThreadAttentionDecision::default(),
                            score: 0.0,
                            unresolved_since: None,
                            ranking: None,
                        })
                        .collect();
                    let mut first = items.clone();
                    restrict_external_items(&conn, account, &mut items, usize::MAX).unwrap();
                    // Stopping at a limit keeps the same rows, cut.
                    restrict_external_items(&conn, account, &mut first, 5).unwrap();
                    assert_eq!(
                        first.iter().map(|item| item.message_id).collect::<Vec<_>>(),
                        items
                            .iter()
                            .take(5)
                            .map(|item| item.message_id)
                            .collect::<Vec<_>>(),
                        "graph {graph}, account {account}, listing cut at 5"
                    );
                    let kept: Vec<(String, i64, i64)> = items
                        .iter()
                        .map(|item| {
                            assert!(item.from_name.is_none());
                            (
                                item.thread_id.clone(),
                                item.message_id,
                                item.decision_source_message_id,
                            )
                        })
                        .collect();
                    let want: Vec<(String, i64, i64)> = listed
                        .iter()
                        .filter(|row| row.3)
                        .map(|(thread, message, source, _)| (thread.clone(), *message, *source))
                        .collect();
                    assert_eq!(kept, want, "graph {graph}, account {account}, listing");
                    for row in &listed {
                        tally(5, row.3);
                    }
                }
            }
            for account in [1, 2] {
                let rows: Vec<_> = door.iter().filter(|row| row.0 == account).collect();
                let asked: Vec<(i64, i64, &str)> = rows
                    .iter()
                    .map(|row| (row.1, row.2, row.3.as_str()))
                    .collect();
                let got = store.external_listing_allowed(account, &asked).unwrap();
                assert_eq!(got.len(), rows.len(), "one answer per row");
                for (row, got) in rows.iter().zip(got) {
                    assert_eq!(got, row.4, "graph {graph}, door {row:?}");
                    tally(4, row.4);
                }
            }
        }
        for (kind, [allowed, refused]) in seen.into_iter().enumerate() {
            assert!(
                allowed >= 10 && refused >= 10,
                "question {kind} was not tried both ways: {allowed} allowed, {refused} refused"
            );
        }
    }

    #[test]
    fn records_are_derived_from_facts_and_legacy_placements_are_retired() {
        let store = fixture();
        {
            let conn = store.lock().unwrap();
            conn.execute_batch("DROP TABLE agent_message_destinations;
                CREATE TABLE agent_message_destinations(account_id INTEGER,message_id INTEGER,destination TEXT,
                PRIMARY KEY(account_id,message_id,destination));
                INSERT INTO agent_message_destinations VALUES(1,1,'records'),(1,2,'records');").unwrap();
            for id in [1, 2] {
                let mut d = serde_json::to_value(decision(id)).unwrap();
                d["destinations"] = serde_json::json!(["reading", "records"]);
                if id == 1 {
                    d["records"] = serde_json::json!([]);
                }
                conn.execute(
                    "INSERT INTO agent_message_decisions VALUES(1,?1,1,?2,?3)",
                    params![id, d.to_string(), Utc::now().to_rfc3339()],
                )
                .unwrap();
            }
            conn.execute("INSERT INTO agent_triage_corrections(account_id,message_id,field,value_json,source_revision,revision,updated_at)
                VALUES(1,1,'destinations',?1,1,1,?2)",params![r#"{"add":["reading","records"],"remove":[]}"#,Utc::now().to_rfc3339()]).unwrap();
            super::super::migrate::retire_records_destination(&conn).unwrap();
            super::super::migrate::retire_records_destination(&conn).unwrap();
            let jobs: i64 = conn
                .query_row(
                    "SELECT count(*) FROM agent_triage_jobs WHERE trigger='records_cleanup'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                jobs, 1,
                "generic placements are queued once, typed records are retained"
            );
            let labels: i64 = conn
                .query_row(
                    "SELECT count(*) FROM agent_message_destinations WHERE destination='records'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(labels, 0);
            let correction: String = conn
                .query_row("SELECT value_json FROM agent_triage_corrections", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert!(!correction.contains("records"));
            assert!(correction.contains("reading"));
        }
        let records = store.agent_records(1, 10).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message_id, 2);
        assert_eq!(
            records[0].decision.destinations,
            vec![MessageDestination::Reading]
        );
        assert!(
            store
                .correct_agent_triage_delta(
                    1,
                    2,
                    "destinations",
                    &["records".into()],
                    &[],
                    Utc::now()
                )
                .is_err()
        );
    }

    #[test]
    fn manual_retriage_precedes_import_backlog_but_not_new_arrivals() {
        for arrival in [false, true] {
            let store = fixture();
            store.enqueue_agent_triage(1, 1, "ingest", arrival).unwrap();
            store
                .enqueue_agent_triage(1, 2, "manual:requested", false)
                .unwrap();
            let job = store
                .claim_agent_job(1, "investigation", Utc::now(), 60)
                .unwrap()
                .unwrap();
            assert_eq!(job.message_id, if arrival { 1 } else { 2 });
            if !arrival {
                assert!(
                    !job.foreground,
                    "manual priority must not bypass the background budget"
                );
            }
        }
    }

    #[test]
    fn triage_job_counts_include_retries_and_isolate_accounts_and_kinds() {
        let store = fixture();
        assert_eq!(store.triage_job_counts(1).unwrap(), [0, 0]);
        {
            let conn = store.lock().unwrap();
            for (account, message, kind, state, trigger) in [
                (1, 1, "triage", "queued", "retry"),
                (1, 2, "access", "leased", "access"),
                (1, 1, "notification", "queued", "notification"),
                (1, 1, "triage", "completed", "completed"),
                (1, 1, "triage", "failed", "failed"),
                (2, 3, "triage", "queued", "other"),
            ] {
                conn.execute(
                    "INSERT INTO agent_triage_jobs
                    (account_id,message_id,kind,trigger,input_revision,state,available_at)
                    VALUES(?1,?2,?3,?4,1,?5,'2099-01-01T00:00:00Z')",
                    params![account, message, kind, trigger, state],
                )
                .unwrap();
            }
        }
        assert_eq!(store.triage_job_counts(1).unwrap(), [1, 1]);
        assert_eq!(store.triage_job_counts(2).unwrap(), [1, 0]);
        assert_eq!(store.triage_job_counts(99).unwrap(), [0, 0]);
    }

    /// The client's "emails waiting" count: distinct messages with work owed
    /// NOW. Scheduled revisits (on by default, queued up to 400 days out) and
    /// never-tried future work must not count, or an idle healthy mailbox
    /// never reads as clear.
    #[test]
    fn triage_waiting_counts_only_work_owed_now_per_message() {
        let store = fixture();
        let now = Utc::now();
        assert_eq!(store.triage_waiting_counts(1, now).unwrap(), [0, 0]);
        {
            let conn = store.lock().unwrap();
            let past = "2000-01-01T00:00:00Z";
            let future = "2099-01-01T00:00:00Z";
            for (message, kind, state, trigger, at, error) in [
                // Due: counts once per message, however many jobs it has.
                (1, "triage", "queued", "arrival", past, None),
                (1, "access", "queued", "access", past, None),
                // Pushed back by the worker: still owed.
                (
                    2,
                    "triage",
                    "queued",
                    "retriage",
                    future,
                    Some("provider_cooldown"),
                ),
                // A scheduled revisit, due or not: never owed now.
                (
                    3,
                    "triage",
                    "queued",
                    "revisit:2099-01-01T00:00:00Z",
                    future,
                    None,
                ),
                (
                    4,
                    "triage",
                    "queued",
                    "revisit:2000-01-01T00:00:00Z",
                    past,
                    None,
                ),
                // Future work nobody has tried yet.
                (5, "triage", "queued", "arrival", future, None),
                // Running.
                (6, "triage", "leased", "arrival", past, None),
                // Other kinds and finished work.
                (7, "notification", "queued", "n", past, None),
                (8, "triage", "completed", "arrival", past, None),
            ] {
                conn.execute(
                    "INSERT INTO agent_triage_jobs
                    (account_id,message_id,kind,trigger,input_revision,state,available_at,last_error)
                    VALUES(1,?1,?2,?3,1,?4,?5,?6)",
                    params![message, kind, trigger, state, at, error],
                )
                .unwrap();
            }
        }
        assert_eq!(store.triage_waiting_counts(1, now).unwrap(), [2, 1]);
        assert_eq!(store.triage_waiting_counts(2, now).unwrap(), [0, 0]);
    }

    /// Both stats reads SEARCH the account's ready index, never scan the jobs
    /// table or settle for `account_id` alone. Unpinned, the planner (no
    /// ANALYZE here) picks the re-triage progress index on `account_id` only:
    /// the #266 review measured 20 ms at 300k rows per `/client/stats`.
    #[test]
    fn stats_queue_reads_use_the_account_ready_index() {
        let store = fixture();
        let conn = store.lock().unwrap();
        for sql in [TRIAGE_WAITING_SQL, TRIAGE_BUDGET_PARKED_SQL] {
            let plan = conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap()
                .query_map(params![1, Utc::now().to_rfc3339()], |r| {
                    r.get::<_, String>(3)
                })
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
                .join("\n");
            assert!(
                plan.contains("INDEX idx_agent_jobs_account_ready (account_id=? AND state=?"),
                "{plan}"
            );
            assert!(!plan.contains("SCAN"), "{plan}");
        }
    }

    #[test]
    fn triage_budget_parked_counts_only_future_budget_deferrals() {
        let store = fixture();
        assert_eq!(store.triage_budget_parked(1).unwrap(), (0, None));
        {
            let conn = store.lock().unwrap();
            for (account, message, kind, state, error, at) in [
                (
                    1,
                    1,
                    "triage",
                    "queued",
                    "daily_budget_exhausted",
                    "2099-01-02T00:00:00Z",
                ),
                (
                    1,
                    2,
                    "access",
                    "queued",
                    "daily_budget_exhausted",
                    "2099-01-01T00:00:00Z",
                ),
                // Already claimable again: not parked.
                (
                    1,
                    3,
                    "triage",
                    "queued",
                    "daily_budget_exhausted",
                    "2000-01-01T00:00:00Z",
                ),
                // Parked for a different reason.
                (
                    1,
                    4,
                    "triage",
                    "queued",
                    "provider_cooldown",
                    "2099-01-01T00:00:00Z",
                ),
                (
                    1,
                    5,
                    "notification",
                    "queued",
                    "daily_budget_exhausted",
                    "2099-01-01T00:00:00Z",
                ),
                (
                    2,
                    6,
                    "triage",
                    "queued",
                    "daily_budget_exhausted",
                    "2099-01-01T00:00:00Z",
                ),
            ] {
                conn.execute(
                    "INSERT INTO agent_triage_jobs
                    (account_id,message_id,kind,trigger,input_revision,state,available_at,last_error)
                    VALUES(?1,?2,?3,'t',1,?4,?5,?6)",
                    params![account, message, kind, state, at, error],
                )
                .unwrap();
            }
        }
        let (parked, resumes) = store.triage_budget_parked(1).unwrap();
        assert_eq!(parked, 2);
        assert_eq!(resumes.unwrap().to_rfc3339(), "2099-01-01T00:00:00+00:00");
        assert_eq!(store.triage_budget_parked(2).unwrap().0, 1);
    }

    pub(super) fn decision(id: i64) -> MessageDecision {
        MessageDecision {
            kinds: vec![EmailKind::Correspondence],
            summary: "Summary".into(),
            reason: "Reason".into(),
            destinations: vec![MessageDestination::Reading],
            records: vec![crate::triage::decision::RecordProposal::Receipt {
                merchant: "Shop".into(),
                amount: Some(12.0),
                currency: Some("USD".into()),
                evidence: vec![crate::triage::decision::EvidenceRef {
                    message_id: id,
                    location: "body".into(),
                }],
            }],
            external_access: AccessAssessment {
                reason: "No credential".into(),
                ..Default::default()
            },
            attention: ThreadAttentionDecision {
                show_in_fye: true,
                summary: "Attention".into(),
                relevant_message_ids: vec![id],
                ..Default::default()
            },
            ..Default::default()
        }
    }
    pub(super) fn claim(store: &SqliteStore, id: i64) -> (AgentJob, AgentContext) {
        store.enqueue_agent_triage(1, id, "arrival", true).unwrap();
        let job = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        let context = store.load_agent_context(&job).unwrap();
        (job, context)
    }
    #[test]
    fn restrictions_and_destination_corrections_survive_content_revision() {
        let store = fixture();
        let (job, _) = claim(&store, 1);
        store
            .correct_agent_triage(
                1,
                1,
                "external_access",
                &serde_json::json!(true),
                Utc::now(),
            )
            .unwrap();
        store
            .correct_agent_triage_delta(1, 1, "destinations", &[], &["reading".into()], Utc::now())
            .unwrap();
        // Simulate blank-body healing or provider re-fetch, both bump content.
        store
            .lock()
            .unwrap()
            .execute("UPDATE messages SET body='healed body' WHERE id=1", [])
            .unwrap();
        store.enqueue_agent_triage(1, 1, "ingest", false).unwrap();
        assert!(
            !store.agent_access_allowed(1, 1).unwrap(),
            "a human restriction must remain effective before the next model call"
        );
        let next = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert_ne!(job.id, next.id);
        let ctx = store.load_agent_context(&next).unwrap();
        assert_eq!(ctx.corrections.len(), 2);
        let sources = store.snapshot_agent_sources(1, &[1]).unwrap();
        assert_eq!(
            store
                .commit_agent_decision(&next, &ctx, &decision(1), &sources)
                .unwrap(),
            AgentCommitOutcome::Applied
        );
        assert!(
            !store.agent_access_allowed(1, 1).unwrap(),
            "model allowed cannot override human restricted"
        );
        assert!(store.agent_reading(1, 10).unwrap().is_empty());
        // Repair the pre-stabilization leak even before the next assessment.
        store.lock().unwrap().execute("UPDATE agent_message_state SET access='allowed' WHERE account_id=1 AND message_id=1",[]).unwrap();
        assert!(
            !store.agent_access_allowed(1, 1).unwrap(),
            "read guard independently enforces durable human restrictions"
        );
    }

    #[test]
    fn body_healing_retains_initial_arrival_capacity_but_later_refresh_does_not() {
        let store = fixture();
        let (original, _) = claim(&store, 1);
        assert!(original.foreground);
        store
            .lock()
            .unwrap()
            .execute("UPDATE messages SET body='healed body' WHERE id=1", [])
            .unwrap();
        store.enqueue_agent_triage(1, 1, "backfill", false).unwrap();
        let healed = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert!(healed.foreground);
        let ctx = store.load_agent_context(&healed).unwrap();
        store
            .commit_agent_decision(
                &healed,
                &ctx,
                &decision(1),
                std::slice::from_ref(&ctx.message.source),
            )
            .unwrap();
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE messages SET body='a later content correction' WHERE id=1",
                [],
            )
            .unwrap();
        store.enqueue_agent_triage(1, 1, "ingest", false).unwrap();
        let refresh = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert!(!refresh.foreground);
    }

    #[test]
    fn sibling_attention_cannot_undo_human_thread_exclusion() {
        let store = fixture();
        store
            .lock()
            .unwrap()
            .execute("UPDATE messages SET thread_id='one' WHERE id=2", [])
            .unwrap();
        let (job, ctx) = claim(&store, 1);
        store
            .commit_agent_decision(
                &job,
                &ctx,
                &decision(1),
                std::slice::from_ref(&ctx.message.source),
            )
            .unwrap();
        store
            .correct_agent_triage(1, 1, "show_in_fye", &serde_json::json!(false), Utc::now())
            .unwrap();
        let (sibling, ctx) = claim(&store, 2);
        store
            .commit_agent_decision(
                &sibling,
                &ctx,
                &decision(2),
                std::slice::from_ref(&ctx.message.source),
            )
            .unwrap();
        assert!(
            !store
                .agent_thread_context(1, "one")
                .unwrap()
                .attention
                .unwrap()
                .show_in_fye
        );
        assert!(
            store
                .agent_fye(1, 20, &RankingConfig::default(), Utc::now())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn trigger_bursts_coalesce_and_retries_do_not_reset_attempts() {
        let store = fixture();
        for i in 0..10 {
            store
                .enqueue_agent_triage(1, 1, &format!("thread_changed:{i}"), false)
                .unwrap();
        }
        let count = || {
            store
                .lock()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM agent_triage_jobs WHERE kind IN ('triage','access')",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
        };
        assert_eq!(count(), 1);
        let job = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        for i in 10..20 {
            store
                .enqueue_agent_triage(1, 1, &format!("thread_changed:{i}"), false)
                .unwrap();
        }
        assert_eq!(count(), 1);
        assert_eq!(
            store
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM agent_triage_followups", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        store
            .retry_agent_job(&job, Utc::now() + Duration::hours(1), "context_changed")
            .unwrap();
        assert!(
            store
                .claim_agent_job(1, "investigation", Utc::now(), 60)
                .unwrap()
                .is_none()
        );
        let retried = store
            .claim_agent_job(1, "investigation", Utc::now() + Duration::hours(2), 60)
            .unwrap()
            .unwrap();
        assert_eq!(retried.id, job.id);
        assert_eq!(retried.attempts, 2);
        assert_eq!(
            store
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM agent_triage_followups", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        store.fail_agent_job(&retried, "exhausted").unwrap();
        store
            .enqueue_agent_triage(1, 1, "rule_changed:99", false)
            .unwrap();
        assert!(
            store
                .claim_agent_job(1, "investigation", Utc::now() + Duration::days(2), 60)
                .unwrap()
                .is_none()
        );
        assert_eq!(count(), 1);
        store
            .enqueue_agent_triage(1, 1, "manual:new-human-request", false)
            .unwrap();
        let manual = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert_eq!(
            manual.attempts, 1,
            "only an explicit new manual request restarts failed work"
        );
        assert!(!manual.foreground);
        store.complete_agent_job(&manual).unwrap();
        store
            .enqueue_agent_triage(1, 1, "manual:new-human-request", false)
            .unwrap();
        assert!(
            store
                .claim_agent_job(1, "investigation", Utc::now(), 60)
                .unwrap()
                .is_none(),
            "repeating the same completed request is idempotent"
        );
    }

    #[test]
    fn leased_burst_releases_only_one_background_followup() {
        let store = fixture();
        let (job, _) = claim(&store, 1);
        for i in 0..10 {
            store
                .enqueue_agent_triage(1, 1, &format!("thread_changed:{i}"), false)
                .unwrap();
        }
        store.complete_agent_job(&job).unwrap();
        let next = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert!(!next.foreground);
        store.complete_agent_job(&next).unwrap();
        assert!(
            store
                .claim_agent_job(1, "investigation", Utc::now(), 60)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn access_commit_only_changes_access_and_honors_current_human_restriction() {
        let store = fixture();
        store
            .enqueue_agent_triage(1, 1, "source_access", false)
            .unwrap();
        let job = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        let original = store.load_agent_access_message(&job).unwrap();
        store
            .correct_agent_triage(
                1,
                1,
                "external_access",
                &serde_json::json!(true),
                Utc::now(),
            )
            .unwrap();
        let decision = crate::triage::access::AccessDecision {
            restricted: false,
            reason: "No actionable auth".into(),
        };
        assert_eq!(
            store
                .commit_agent_access(&job, &original, &decision, &serde_json::json!({}))
                .unwrap(),
            AgentCommitOutcome::Applied
        );
        assert!(!store.agent_access_allowed(1, 1).unwrap());
        let conn = store.lock().unwrap();
        for table in [
            "agent_message_decisions",
            "agent_thread_attention",
            "agent_message_destinations",
            "agent_decision_sources",
        ] {
            assert_eq!(
                conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0,
                "access executor cannot populate {table}"
            );
        }
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM agent_triage_jobs", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn related_evidence_preserves_target_representation_and_access_provenance() {
        use crate::triage::decision::RelatedAttentionUpdate;
        let store = fixture();
        let (first, initial) = claim(&store, 1);
        let mut bill = decision(1);
        bill.kinds = vec![EmailKind::Bill];
        bill.summary = "Electricity bill".into();
        store
            .commit_agent_decision(
                &first,
                &initial,
                &bill,
                std::slice::from_ref(&initial.message.source),
            )
            .unwrap();
        let (job, receipt_context) = claim(&store, 2);
        let target = store.agent_thread_context(1, "one").unwrap();
        let mut receipt = decision(2);
        receipt.kinds = vec![EmailKind::Receipt];
        receipt.summary = "Payment receipt".into();
        let mut attention = target.attention.unwrap();
        attention.summary = "Payment received".into();
        receipt.related_updates.push(RelatedAttentionUpdate {
            thread_id: "one".into(),
            expected_revision: target.revision.attention_revision,
            attention,
            evidence: vec![],
        });
        let sources = store.snapshot_agent_sources(1, &[1, 2]).unwrap();
        assert_eq!(
            store
                .commit_agent_decision(&job, &receipt_context, &receipt, &sources)
                .unwrap(),
            AgentCommitOutcome::Applied
        );
        let items = store
            .agent_fye(1, 10, &RankingConfig::default(), Utc::now())
            .unwrap();
        let target = items.iter().find(|item| item.thread_id == "one").unwrap();
        assert_eq!(target.message_id, 1);
        assert_eq!(target.decision.summary, "Electricity bill");
        assert_eq!(target.decision.kinds, vec![EmailKind::Bill]);
        assert_eq!(target.attention.summary, "Payment received");
        assert_eq!(target.decision_source_message_id, 1);
        assert_eq!(
            attention_projection(&store, "one")["sources"],
            serde_json::json!([[1, 1], [2, 1]])
        );
        store
            .correct_agent_triage(
                1,
                2,
                "external_access",
                &serde_json::json!(true),
                Utc::now(),
            )
            .unwrap();
        assert!(
            store
                .external_agent_fye(1, 10, &RankingConfig::default(), Utc::now())
                .unwrap()
                .is_empty()
        );
        assert!(store.external_agent_records(1, 10).unwrap().is_empty());
        assert!(store.external_agent_reading(1, 10).unwrap().is_empty());
        assert_eq!(
            store
                .agent_records(1, 10)
                .unwrap()
                .iter()
                .find(|item| item.message_id == 1)
                .unwrap()
                .decision,
            bill
        );
    }

    fn attention_projection(store: &SqliteStore, thread: &str) -> serde_json::Value {
        let encoded: String = store.lock().unwrap().query_row(
            "SELECT json_object('message_id',message_id,'decision_message_id',decision_message_id,
             'relevant_activity',relevant_activity,'unresolved_since',unresolved_since,
             'show_in_fye',json(CASE show_in_fye WHEN 1 THEN 'true' ELSE 'false' END),
             'attention',json(attention_json),'revision',revision)
             FROM agent_thread_attention WHERE account_id=1 AND thread_id=?1",
            [thread], |row| row.get(0),
        ).unwrap();
        let mut projection: serde_json::Value = decode(&encoded).unwrap();
        let conn = store.lock().unwrap();
        let mut statement = conn.prepare("SELECT source_message_id,source_revision FROM agent_attention_sources WHERE account_id=1 AND thread_id=?1 ORDER BY source_message_id").unwrap();
        let sources = statement
            .query_map([thread], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        projection["sources"] = serde_json::json!(sources);
        projection
    }

    /// A sign-in alert is auth the agent may still read: `restricted` is
    /// false and must stay false. It belongs on the Auth page's decision rail
    /// and NOT in FYE, even when the model asked for FYE. Regression: every
    /// unrestricted login alert used to land in FYE and never on the Auth page.
    #[test]
    fn unrestricted_auth_lists_on_auth_page_not_fye() {
        use crate::triage::decision::{AuthAssessment, AuthKind};
        let store = fixture();
        let (job, context) = claim(&store, 1);
        let mut alert = decision(1);
        alert.auth = AuthAssessment {
            kinds: vec![AuthKind::LoginAlert],
            evidence: vec![],
            critical: false,
        };
        assert!(!alert.external_access.restricted);
        assert!(alert.attention.show_in_fye);
        store
            .commit_agent_decision(
                &job,
                &context,
                &alert,
                std::slice::from_ref(&context.message.source),
            )
            .unwrap();
        let (job, context) = claim(&store, 2);
        store
            .commit_agent_decision(
                &job,
                &context,
                &decision(2),
                std::slice::from_ref(&context.message.source),
            )
            .unwrap();

        let fye_threads = |store: &SqliteStore| {
            store
                .agent_fye(1, 10, &RankingConfig::default(), Utc::now())
                .unwrap()
                .into_iter()
                .map(|item| item.thread_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(fye_threads(&store), vec!["two".to_string()]);
        // The legacy /client/updates Standing band (iOS, the assistant's
        // get_updates tool) and its tier label must agree with the v2 feed.
        let standing = |store: &SqliteStore| {
            let mut threads = store
                .human_agent_updates(
                    1,
                    &AgentInventoryQuery {
                        before: None,
                        limit: None,
                        since: Utc::now() - chrono::Duration::days(30),
                        min_importance: None,
                        status: None,
                        band: Some(crate::store::SitrepBand::Standing),
                        pending_reminders: false,
                        spam: crate::store::SpamScope::Exclude,
                        ranking: RankingConfig::default(),
                    },
                )
                .unwrap()
                .into_iter()
                .map(|item| item.update.thread_id)
                .collect::<Vec<_>>();
            threads.sort();
            threads
        };
        assert_eq!(standing(&store), vec!["two".to_string()]);
        let all = store
            .human_agent_updates(
                1,
                &AgentInventoryQuery {
                    before: None,
                    limit: None,
                    since: Utc::now() - chrono::Duration::days(30),
                    min_importance: None,
                    status: None,
                    band: None,
                    pending_reminders: false,
                    spam: crate::store::SpamScope::Exclude,
                    ranking: RankingConfig::default(),
                },
            )
            .unwrap();
        let tier_of = |id: i64| all.iter().find(|u| u.update.id == id).unwrap().update.tier;
        assert_eq!(tier_of(1), crate::types::Tier::Noise);
        assert_eq!(tier_of(2), crate::types::Tier::Signal);
        let sealed = store.sealed_messages(1).unwrap();
        assert_eq!(sealed.len(), 1);
        assert_eq!(sealed[0].id, 1);
        assert_eq!(sealed[0].sealed_kind.as_deref(), Some("login_alert"));
        assert!(store.sealed_body(1, 1).is_ok());
        assert!(matches!(
            store.sealed_body(1, 2).unwrap_err(),
            CoreError::NotFound
        ));

        // The human pinning the thread overrides the auth exclusion.
        store
            .correct_agent_triage(1, 1, "show_in_fye", &serde_json::json!(true), Utc::now())
            .unwrap();
        let mut pinned = fye_threads(&store);
        pinned.sort();
        assert_eq!(pinned, vec!["one".to_string(), "two".to_string()]);
        assert_eq!(standing(&store), pinned);

        // Archiving resolves the row to done, and done auth mail leaves the
        // Auth list (and so the badge), while an explicit reveal still works.
        store
            .lock()
            .unwrap()
            .execute("UPDATE triage SET status='done' WHERE message_id=1", [])
            .unwrap();
        assert!(store.sealed_messages(1).unwrap().is_empty());
        assert!(store.sealed_body(1, 1).is_ok());
    }

    /// Seeds one single-message thread per id in account 1, triaged with
    /// `sensitivity='normal'` (the agent pipeline never writes `sealed`, so
    /// nothing here may lean on it), and commits `auth` on it.
    fn seed_auth(
        store: &SqliteStore,
        id: i64,
        auth: crate::triage::decision::AuthAssessment,
        restricted: bool,
    ) {
        seed_auth_in(store, id, &format!("t{id}"), 0, auth, restricted);
    }

    /// [`seed_auth`] into `thread`, arriving `offset` seconds from now (so a
    /// thread's messages arrive in order and the last one represents it).
    fn seed_auth_in(
        store: &SqliteStore,
        id: i64,
        thread: &str,
        offset: i64,
        auth: crate::triage::decision::AuthAssessment,
        restricted: bool,
    ) {
        {
            let conn = store.lock().unwrap();
            let at = (Utc::now() + chrono::Duration::seconds(offset)).to_rfc3339();
            conn.execute(
                "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body)
                 VALUES(?1,1,?2,?3,'security@bank.test','subject',?4,'snippet','body')",
                params![id, id.to_string(), thread, at],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO triage(account_id,message_id,created_at,sensitivity) VALUES(1,?1,?2,'normal')",
                params![id, at],
            )
            .unwrap();
        }
        let (job, context) = claim(store, id);
        let mut decision = decision(id);
        decision.auth = auth;
        decision.external_access.restricted = restricted;
        // The model's own FYE pick, which auth mail must NOT take.
        decision.attention.show_in_fye = true;
        store
            .commit_agent_decision(
                &job,
                &context,
                &decision,
                std::slice::from_ref(&context.message.source),
            )
            .unwrap();
    }

    fn auth(
        kinds: &[crate::triage::decision::AuthKind],
        critical: bool,
    ) -> crate::triage::decision::AuthAssessment {
        crate::triage::decision::AuthAssessment {
            kinds: kinds.to_vec(),
            evidence: vec![],
            critical,
        }
    }

    fn inventory(band: Option<crate::store::SitrepBand>) -> AgentInventoryQuery {
        AgentInventoryQuery {
            before: None,
            limit: None,
            since: Utc::now() - chrono::Duration::days(30),
            min_importance: None,
            status: None,
            band,
            pending_reminders: false,
            spam: crate::store::SpamScope::Exclude,
            ranking: RankingConfig::default(),
        }
    }

    /// FIRST-LOAD AUTH MAIL ASKS NOTHING. A critical change the first-run
    /// backfill brought in is history: it holds no FYE place (list and sitrep
    /// placement alike) and the sealed list marks it `first_load`, which the
    /// client reads as "no question". A live one still holds; so does a
    /// first-load one the owner called "Not me".
    #[test]
    fn first_load_auth_mail_holds_no_fye_place() {
        use crate::triage::decision::AuthKind::*;
        use crate::types::AuthVerdict::*;
        let store = fixture();
        // (id, from the backfill, verdict, expected in FYE)
        let cases: &[(i64, bool, Option<crate::types::AuthVerdict>, bool)] = &[
            (40, false, None, true),
            (41, true, None, false),
            (42, true, Some(NotMine), true),
        ];
        for &(id, backfilled, verdict, _) in cases {
            seed_auth(&store, id, auth(&[AccountChange], true), false);
            if backfilled {
                store
                    .lock()
                    .unwrap()
                    .execute("UPDATE triage SET first_load=1 WHERE message_id=?1", [id])
                    .unwrap();
            }
            if verdict.is_some() {
                store.set_auth_verdict(1, id, verdict, Utc::now()).unwrap();
            }
        }
        let expected: Vec<i64> = cases.iter().filter(|c| c.3).map(|c| c.0).collect();
        let mut listed: Vec<i64> = store
            .agent_fye(1, 100, &RankingConfig::default(), Utc::now())
            .unwrap()
            .into_iter()
            .map(|item| item.message_id)
            .collect();
        listed.sort();
        assert_eq!(listed, expected, "the FYE list");
        let mut eyes: Vec<i64> = store
            .human_agent_updates(1, &inventory(None))
            .unwrap()
            .iter()
            .filter(|item| item.placement.as_deref() == Some("eyes"))
            .map(|item| item.update.id)
            .collect();
        eyes.sort();
        assert_eq!(eyes, expected, "the sitrep's placement agrees");
        let flags: Vec<(i64, bool)> = {
            let mut rows: Vec<(i64, bool)> = store
                .sealed_messages(1)
                .unwrap()
                .into_iter()
                .map(|m| (m.id, m.first_load))
                .collect();
            rows.sort();
            rows
        };
        assert_eq!(flags, [(40, false), (41, true), (42, true)]);
    }

    /// FYE MEMBERSHIP FOR AUTH MAIL, and the pin that the FYE list and the
    /// sitrep's placement read it the same way. Auth mail ignores the model's
    /// FYE pick; it is in FYE on a "Not me", or as a critical change nobody
    /// has ruled on; "That was me" takes it out; the owner's thread pin wins
    /// either way.
    #[test]
    fn auth_fye_list_and_placement_agree() {
        use crate::triage::decision::AuthKind::*;
        use crate::types::AuthVerdict::*;
        let store = fixture();
        // (id, kinds, critical, verdict, pin, expected in FYE)
        type Case = (
            i64,
            &'static [crate::triage::decision::AuthKind],
            bool,
            Option<crate::types::AuthVerdict>,
            Option<bool>,
            bool,
        );
        let cases: &[Case] = &[
            (10, &[LoginAlert], false, None, None, false),
            (11, &[LoginAlert], false, Some(NotMine), None, true),
            (12, &[AccountChange], true, None, None, true),
            (13, &[AccountChange], true, Some(Mine), None, false),
            (14, &[AccountChange], true, Some(Mine), Some(true), true),
            (15, &[AccountChange], false, None, None, false),
            // A stray flag beside another kind means nothing.
            (16, &[LoginAlert], true, None, None, false),
            (17, &[Otp], false, Some(NotMine), Some(false), false),
            (18, &[SigninReceipt], false, Some(Mine), None, false),
            (19, &[Otp, AccountChange], true, None, None, true),
        ];
        for &(id, kinds, critical, verdict, pin, _) in cases {
            seed_auth(&store, id, auth(kinds, critical), false);
            if verdict.is_some() {
                store.set_auth_verdict(1, id, verdict, Utc::now()).unwrap();
            }
            if let Some(pin) = pin {
                store
                    .correct_agent_triage(1, id, "show_in_fye", &serde_json::json!(pin), Utc::now())
                    .unwrap();
            }
        }
        // A control: ordinary mail the model picked is in FYE as ever.
        let (job, context) = claim(&store, 2);
        store
            .commit_agent_decision(
                &job,
                &context,
                &decision(2),
                std::slice::from_ref(&context.message.source),
            )
            .unwrap();
        // FYE IS THREAD-LEVEL. Gmail threads Google's "Security alert" mail,
        // so a critical change is often not its thread's newest message.
        // A critical change, then a sign-in alert after it: the thread is in
        // FYE, and the row shown is the change.
        seed_auth_in(&store, 30, "ta", -120, auth(&[AccountChange], true), false);
        seed_auth_in(&store, 31, "ta", -60, auth(&[LoginAlert], false), false);
        // "Not me" on the OLDER message holds the thread too.
        seed_auth_in(&store, 32, "tb", -120, auth(&[LoginAlert], false), false);
        seed_auth_in(&store, 33, "tb", -60, auth(&[LoginAlert], false), false);
        store
            .set_auth_verdict(1, 32, Some(NotMine), Utc::now())
            .unwrap();
        // A ruled change behind an alert: nothing holds the thread.
        seed_auth_in(&store, 34, "tc", -120, auth(&[AccountChange], true), false);
        seed_auth_in(&store, 35, "tc", -60, auth(&[LoginAlert], false), false);
        store
            .set_auth_verdict(1, 34, Some(Mine), Utc::now())
            .unwrap();

        // The row each qualifying thread shows, and every message placed in it.
        let mut expected: Vec<i64> = cases.iter().filter(|c| c.5).map(|c| c.0).collect();
        expected.extend([2, 30, 32]);
        expected.sort();
        let mut placed = expected.clone();
        placed.extend([31, 33]);
        placed.sort();
        let mut listed: Vec<i64> = store
            .agent_fye(1, 100, &RankingConfig::default(), Utc::now())
            .unwrap()
            .into_iter()
            .map(|item| item.message_id)
            .collect();
        listed.sort();
        assert_eq!(listed, expected, "the FYE list");
        let all = store.human_agent_updates(1, &inventory(None)).unwrap();
        let mut eyes: Vec<i64> = all
            .iter()
            .filter(|item| item.placement.as_deref() == Some("eyes"))
            .map(|item| item.update.id)
            .collect();
        eyes.sort();
        assert_eq!(eyes, placed, "the sitrep placement");
        // The stats tiers, message by message, off the CTE every count reads.
        let mut signal: Vec<i64> = {
            let conn = store.lock().unwrap();
            let mut statement = conn
                .prepare(&format!(
                    "{} SELECT id FROM classified WHERE account_id=1 AND display_tier='signal'",
                    super::super::attention::CANONICAL_MAIL_CTE
                ))
                .unwrap();
            statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap()
        };
        signal.sort();
        assert_eq!(signal, placed, "the stats tiers");
        let mut standing: Vec<i64> = store
            .human_agent_updates(1, &inventory(Some(crate::store::SitrepBand::Standing)))
            .unwrap()
            .into_iter()
            .map(|item| item.update.id)
            .collect();
        standing.sort();
        assert_eq!(standing, expected, "the standing band");
        // Auth mail out of FYE still sits on the Auth page.
        for &(id, ..) in cases.iter().filter(|c| !c.5 && c.4 != Some(false)) {
            let item = all.iter().find(|item| item.update.id == id).unwrap();
            assert_eq!(item.placement.as_deref(), Some("auth"), "message {id}");
        }
        // And the Auth page reads the same flags back.
        let sealed = store.sealed_messages(1).unwrap();
        for &(id, kinds, critical, verdict, ..) in cases {
            let row = sealed.iter().find(|row| row.id == id).unwrap();
            assert_eq!(
                row.critical,
                critical && kinds.contains(&AccountChange),
                "message {id}"
            );
            assert_eq!(row.verdict, verdict, "message {id}");
        }
        assert_eq!(
            sealed
                .iter()
                .find(|row| row.id == 19)
                .unwrap()
                .sealed_kind
                .as_deref(),
            Some("otp"),
            "the priority pick, not kinds[0]"
        );

        // Clearing a ruling puts a critical change back.
        store.set_auth_verdict(1, 13, None, Utc::now()).unwrap();
        assert!(
            store
                .agent_fye(1, 100, &RankingConfig::default(), Utc::now())
                .unwrap()
                .iter()
                .any(|item| item.message_id == 13)
        );
        // A verdict is only for auth mail, and only in its own account.
        assert!(matches!(
            store.set_auth_verdict(1, 2, Some(NotMine), Utc::now()),
            Err(CoreError::NotFound)
        ));
        assert!(matches!(
            store.set_auth_verdict(1, 3, Some(NotMine), Utc::now()),
            Err(CoreError::NotFound)
        ));
        assert!(matches!(
            store.set_auth_verdict(2, 12, Some(Mine), Utc::now()),
            Err(CoreError::NotFound)
        ));
    }

    /// The Auth page's `explained_by`, read off stored rows: a code the owner
    /// revealed as it arrived and then archived still explains the sign-in
    /// beside it, and is itself gone from the list. An unrevealed code, one
    /// revealed long after the sign-in, an unauthenticated one, a disowned
    /// one and one outside the window explain nothing; nor is a ruled alert
    /// explained.
    #[test]
    fn sealed_rows_carry_their_explanation() {
        use crate::triage::decision::AuthKind::*;
        struct Seed {
            id: i64,
            kind: crate::triage::decision::AuthKind,
            from: &'static str,
            at: i64,
            pass: bool,
            revealed_at: Option<i64>,
            done: bool,
        }
        let alert = |id, from, at| Seed {
            id,
            kind: LoginAlert,
            from,
            at,
            pass: true,
            revealed_at: None,
            done: false,
        };
        let code = |id, from, at, pass, revealed_at: Option<i64>, done| Seed {
            id,
            kind: Otp,
            from,
            at,
            pass,
            revealed_at,
            done,
        };
        let store = fixture();
        let base = Utc::now() - chrono::Duration::hours(3);
        let time = |offset: i64| (base + chrono::Duration::seconds(offset)).to_rfc3339();
        let seeds = [
            alert(10, "alerts@accounts.example.com", 30),
            code(11, "codes@example.com", 0, true, Some(5), true),
            alert(12, "alerts@elsewhere.test", 1000),
            code(13, "codes@elsewhere.test", 990, true, None, false),
            alert(14, "alerts@third.test", 2000),
            code(15, "codes@third.test", 1990, false, Some(1995), false),
            alert(16, "alerts@fourth.test", 3000),
            code(17, "codes@fourth.test", 2990, true, Some(2995), false),
            alert(18, "alerts@fifth.test", 4000),
            code(
                19,
                "codes@fifth.test",
                4000 - 301,
                true,
                Some(4000 - 300),
                true,
            ),
            // Opened ten minutes after the sign-in it would explain.
            alert(20, "alerts@sixth.test", 5000),
            code(21, "codes@sixth.test", 4990, true, Some(5600), false),
            // A ruled alert, beside a code that would explain it.
            alert(22, "alerts@seventh.test", 6000),
            code(23, "codes@seventh.test", 5990, true, Some(5995), false),
            // A code revealed only by tapping its notification banner.
            alert(24, "alerts@eighth.test", 7000),
            code(25, "codes@eighth.test", 6990, true, None, false),
        ];
        for seed in &seeds {
            seed_auth(&store, seed.id, auth(&[seed.kind], false), seed.kind == Otp);
            let conn = store.lock().unwrap();
            conn.execute(
                "UPDATE messages SET from_addr=?2, received_at=?3, auth_pass=?4 WHERE id=?1",
                params![seed.id, seed.from, time(seed.at), seed.pass],
            )
            .unwrap();
            if seed.done {
                conn.execute(
                    "UPDATE triage SET status='done' WHERE message_id=?1",
                    [seed.id],
                )
                .unwrap();
            }
            if let Some(revealed_at) = seed.revealed_at {
                // A later reveal too: the earliest is the one that counts.
                for at in [revealed_at, revealed_at + 3600] {
                    conn.execute(
                        "INSERT INTO audit_log(account_id,ts,actor,action,target)
                         VALUES(1,?1,'human','reveal_sealed',?2)",
                        params![time(at), seed.id.to_string()],
                    )
                    .unwrap();
                }
            }
        }
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO audit_log(account_id,ts,actor,action,target,detail)
                 VALUES(1,?1,'human','reveal_sealed','25','notification')",
                [time(6995)],
            )
            .unwrap();
        store
            .set_auth_verdict(1, 17, Some(crate::types::AuthVerdict::NotMine), Utc::now())
            .unwrap();
        store
            .set_auth_verdict(1, 22, Some(crate::types::AuthVerdict::Mine), Utc::now())
            .unwrap();
        let sealed = store.sealed_messages(1).unwrap();
        let explained = |id: i64| sealed.iter().find(|row| row.id == id).unwrap().explained_by;
        assert!(
            sealed.iter().all(|row| row.id != 11 && row.id != 19),
            "done rows leave the list"
        );
        assert_eq!(explained(10), Some(11), "a revealed, archived code");
        assert_eq!(explained(12), None, "an unrevealed code");
        assert_eq!(explained(14), None, "an unauthenticated code");
        assert_eq!(explained(16), None, "a disowned code");
        assert_eq!(explained(18), None, "a done code outside the window");
        assert_eq!(explained(20), None, "a code opened long after the sign-in");
        assert_eq!(explained(22), None, "a ruled alert");
        assert_eq!(explained(24), None, "a code revealed from its notification");
    }

    /// ARCHIVING ONE AUTH MESSAGE NEVER CLEARS AN UNRULED CRITICAL SIBLING.
    /// The Auth page archives the sign-in alert it used ("Used it, archive"),
    /// and Done is thread-wide; the recovery-phone change Gmail threaded with
    /// it must hold until the owner rules. Done on the change itself applies.
    #[test]
    fn done_on_a_sibling_spares_an_unruled_critical_change() {
        use crate::triage::decision::AuthKind::*;
        let fye = |store: &SqliteStore| {
            store
                .agent_fye(1, 100, &RankingConfig::default(), Utc::now())
                .unwrap()
                .into_iter()
                .map(|item| item.message_id)
                .collect::<Vec<_>>()
        };
        let sealed = |store: &SqliteStore| {
            store
                .sealed_messages(1)
                .unwrap()
                .into_iter()
                .map(|row| row.id)
                .collect::<Vec<_>>()
        };
        let store = fixture();
        seed_auth_in(&store, 40, "t", -120, auth(&[AccountChange], true), false);
        seed_auth_in(&store, 41, "t", -60, auth(&[LoginAlert], false), false);
        assert!(
            store
                .set_attention_status(1, 41, crate::types::AttentionStatus::Done)
                .unwrap()
        );
        assert_eq!(sealed(&store), vec![40]);
        assert_eq!(fye(&store), vec![40]);
        // A reminder on the sibling, or an unsubscribe from the sender, is
        // no ruling either.
        store
            .set_reminder(1, 41, Utc::now() + chrono::Duration::days(1))
            .unwrap();
        store.resolve_sender(1, "security@bank.test").unwrap();
        assert_eq!(sealed(&store), vec![40]);
        // The owner marking the change itself done is their own act.
        assert!(
            store
                .set_attention_status(1, 40, crate::types::AttentionStatus::Done)
                .unwrap()
        );
        assert!(sealed(&store).is_empty());
        assert!(fye(&store).is_empty());

        // Once ruled, the change goes with its thread like any message.
        let store = fixture();
        seed_auth_in(&store, 40, "t", -120, auth(&[AccountChange], true), false);
        seed_auth_in(&store, 41, "t", -60, auth(&[LoginAlert], false), false);
        store
            .set_auth_verdict(1, 40, Some(crate::types::AuthVerdict::Mine), Utc::now())
            .unwrap();
        assert!(
            store
                .set_attention_status(1, 41, crate::types::AttentionStatus::Done)
                .unwrap()
        );
        assert!(sealed(&store).is_empty());
    }

    /// The agent's done is the owner's Done, so it spares the same sibling:
    /// an unruled critical change in the thread stays where it is, and gets
    /// no `auto_done` row (it was not closed, so it must not read as closed
    /// before).
    #[test]
    fn an_auto_done_spares_an_unruled_critical_sibling_and_gives_it_no_ledger_row() {
        use crate::triage::decision::{AuthAssessment, AuthKind::*, DoneRequest, EvidenceRef};
        let store = fixture();
        seed_auth_in(&store, 40, "t", -120, auth(&[AccountChange], true), false);
        seed_auth_in(&store, 41, "t", -60, AuthAssessment::default(), false);
        {
            let conn = store.lock().unwrap();
            conn.execute("UPDATE messages SET auth_pass=1 WHERE id IN (40,41)", [])
                .unwrap();
            conn.execute(
                "INSERT INTO agent_triage_jobs(account_id,message_id,kind,trigger,input_revision,arrival_eligible,available_at)
                 SELECT 1,41,'triage','revisit:2026-10-20T00:00:00+00:00',revision,0,?1
                 FROM agent_message_state WHERE account_id=1 AND message_id=41",
                [(Utc::now() - chrono::Duration::seconds(1)).to_rfc3339()],
            )
            .unwrap();
        }
        let job = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert_eq!(
            (job.message_id, job.trigger.starts_with("revisit:")),
            (41, true)
        );
        let context = store.load_agent_context(&job).unwrap();
        let mut verdict = decision(41);
        verdict.done = Some(DoneRequest {
            reason: "over".into(),
            evidence: vec![EvidenceRef {
                message_id: 41,
                location: "body".into(),
            }],
        });
        store
            .commit_agent_decision(
                &job,
                &context,
                &verdict,
                std::slice::from_ref(&context.message.source),
            )
            .unwrap();
        let conn = store.lock().unwrap();
        let rows: Vec<(i64, String, bool)> = conn
            .prepare(
                "SELECT t.message_id,t.status,EXISTS(SELECT 1 FROM auto_done l
                     WHERE l.account_id=t.account_id AND l.message_id=t.message_id)
                 FROM triage t WHERE t.account_id=1 AND t.message_id IN (40,41) ORDER BY 1",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                (40, "new".to_string(), false),
                (41, "done".to_string(), true)
            ]
        );
    }

    /// "Not me" mail holds its place against the same sweeps: archiving the
    /// next alert in its thread, a reminder on it, an unsubscribe from the
    /// sender.
    #[test]
    fn done_on_a_sibling_spares_mail_ruled_not_mine() {
        use crate::triage::decision::AuthKind::*;
        let store = fixture();
        seed_auth_in(&store, 40, "t", -120, auth(&[LoginAlert], false), false);
        seed_auth_in(&store, 41, "t", -60, auth(&[LoginAlert], false), false);
        store
            .set_auth_verdict(1, 40, Some(crate::types::AuthVerdict::NotMine), Utc::now())
            .unwrap();
        assert!(
            store
                .set_attention_status(1, 41, crate::types::AttentionStatus::Done)
                .unwrap()
        );
        store
            .set_reminder(1, 41, Utc::now() + chrono::Duration::days(1))
            .unwrap();
        store.resolve_sender(1, "security@bank.test").unwrap();
        let ids: Vec<i64> = store
            .sealed_messages(1)
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, vec![40]);
    }

    /// A "critical" change from a sender that explicitly FAILED
    /// authentication is a spoof of the service's own address: no FYE slot,
    /// no Critical flag, no shield from sweeps or the shredder. Never
    /// evaluated (NULL, every fixture row here) still counts.
    #[test]
    fn a_spoofed_critical_change_holds_nothing() {
        use crate::triage::decision::AuthKind::*;
        let store = fixture();
        seed_auth_in(
            &store,
            40,
            "t",
            -120,
            auth(&[PasswordReset, AccountChange], true),
            true,
        );
        seed_auth_in(&store, 41, "t", -60, auth(&[LoginAlert], false), false);
        seed_auth(&store, 42, auth(&[AccountChange], true), false);
        store
            .lock()
            .unwrap()
            .execute("UPDATE messages SET auth_pass=0 WHERE id=40", [])
            .unwrap();
        let fye: Vec<i64> = store
            .agent_fye(1, 100, &RankingConfig::default(), Utc::now())
            .unwrap()
            .into_iter()
            .map(|item| item.message_id)
            .collect();
        assert_eq!(fye, vec![42], "only the unspoofed change");
        let sealed = store.sealed_messages(1).unwrap();
        let critical = |id: i64| sealed.iter().find(|row| row.id == id).unwrap().critical;
        assert!(!critical(40));
        assert!(critical(42));
        let cutoff = Utc::now() + chrono::Duration::hours(1);
        let shreddable: Vec<i64> = store
            .shred_candidates(1, cutoff, 10)
            .unwrap()
            .into_iter()
            .map(|c| c.message_id)
            .collect();
        assert_eq!(shreddable, vec![40]);
        assert!(
            store
                .set_attention_status(1, 41, crate::types::AttentionStatus::Done)
                .unwrap()
        );
        assert!(
            store
                .sealed_messages(1)
                .unwrap()
                .iter()
                .all(|row| row.id != 40)
        );
    }

    /// The FYE row SHOWS the qualifying auth message, but the thread keeps
    /// its own clock: rank time and the agent door's `since` read the
    /// representative's activity; a snooze on the representative does not
    /// hide the change.
    /// (A thread the owner unpinned stays out even when a reminder fires:
    /// `thread_fye_override` filters every FYE row, before and after this.)
    #[test]
    fn the_fye_row_keeps_its_threads_reminder_and_activity() {
        use crate::triage::decision::AuthKind::*;
        let store = fixture();
        seed_auth_in(&store, 40, "t", -3600, auth(&[AccountChange], true), false);
        seed_auth_in(&store, 41, "t", -60, auth(&[LoginAlert], false), false);
        let item = store
            .agent_fye(1, 100, &RankingConfig::default(), Utc::now())
            .unwrap()
            .into_iter()
            .find(|item| item.thread_id == "t")
            .unwrap();
        assert_eq!(item.message_id, 40, "the row shows the change");
        let shown_at = DateTime::parse_from_rfc3339(&item.received_at).unwrap();
        let representative: String = store
            .lock()
            .unwrap()
            .query_row("SELECT received_at FROM messages WHERE id=41", [], |r| {
                r.get(0)
            })
            .unwrap();
        let representative = DateTime::parse_from_rfc3339(&representative).unwrap();
        assert!(
            (shown_at - representative).num_seconds().abs() <= 1,
            "{shown_at} is the thread's activity, not the older change's"
        );
        let listed = |store: &SqliteStore| {
            store
                .agent_fye(1, 100, &RankingConfig::default(), Utc::now())
                .unwrap()
                .into_iter()
                .map(|item| item.message_id)
                .collect::<Vec<_>>()
        };
        // A snooze on the thread's representative does NOT hide the change:
        // the list agrees with the sitrep's Standing band, which judges the
        // shown row on its own.
        let snooze = |at: DateTime<Utc>| {
            store
                .lock()
                .unwrap()
                .execute(
                    "UPDATE triage SET remind_at=?1 WHERE message_id=41",
                    [at.to_rfc3339()],
                )
                .unwrap();
        };
        snooze(Utc::now() + chrono::Duration::days(1));
        assert_eq!(listed(&store), vec![40]);
        let standing: Vec<i64> = store
            .human_agent_updates(1, &inventory(Some(crate::store::SitrepBand::Standing)))
            .unwrap()
            .into_iter()
            .map(|item| item.update.id)
            .collect();
        assert_eq!(standing, vec![40], "the Standing band agrees");
        let agent: Vec<i64> = store
            .external_agent_fye(1, 100, &RankingConfig::default(), Utc::now())
            .unwrap()
            .into_iter()
            .map(|item| item.message_id)
            .collect();
        assert_eq!(agent, vec![40], "and so does the agent door");
        // Without a qualifying message, the snooze hides the thread as ever.
        store
            .set_auth_verdict(1, 40, Some(crate::types::AuthVerdict::Mine), Utc::now())
            .unwrap();
        assert!(listed(&store).is_empty());
    }

    /// `EXPLAIN QUERY PLAN` steps for `sql`, every placeholder bound to NULL.
    fn query_plan(conn: &Connection, sql: &str) -> Vec<String> {
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let nulls = vec![rusqlite::types::Value::Null; stmt.parameter_count()];
        stmt.query_map(rusqlite::params_from_iter(nulls), |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }

    /// The stats tier count, the widest reader of the auth thread CTE: it runs
    /// over the whole mailbox.
    fn stats_tier_sql() -> String {
        format!(
            "{} SELECT display_tier, COUNT(*) FROM classified GROUP BY display_tier",
            super::super::attention::CANONICAL_MAIL_CTE
        )
    }

    /// FYE MEMBERSHIP FOR AUTH MAIL NEVER PARSES THE MAILBOX: the thread CTE
    /// is driven from the partial index of critical decisions (and the small
    /// verdict table), never a scan of every decision. Pinned on the stats
    /// CTE, which runs over the whole mailbox.
    #[test]
    fn auth_fye_threads_read_the_critical_index() {
        let store = fixture();
        let conn = store.lock().unwrap();
        let plan = query_plan(&conn, &stats_tier_sql()).join("\n");
        assert!(plan.contains("idx_decisions_critical"), "{plan}");
        assert!(
            !plan
                .lines()
                .any(|line| line.trim_end().ends_with("SCAN agent_message_decisions")),
            "{plan}"
        );
    }

    /// The thread CTE is driven FROM the held messages, one rowid probe each,
    /// with and without statistics, on its own and inside the stats tier
    /// count. Its window function keeps it from being flattened into any outer
    /// query, so the order pinned here is the order in every reader (the FYE
    /// listing and the human inventory too).
    ///
    /// The canary is the same CTE unpinned under the statistics a real store
    /// has from its first pass, a minute after the daemon starts: a large
    /// `messages`, a few critical decisions, and no row at all for the empty
    /// `auth_verdicts` (ANALYZE writes none for an empty table, and the
    /// planner takes a table it has no row for as a large one). Unpinned, that
    /// store scans every message (300 ms at 50k, in every FYE, inventory and
    /// stats read).
    #[test]
    fn auth_fye_threads_drive_from_the_held_messages() {
        let store = fixture();
        let alone = format!(
            "WITH {} SELECT COUNT(*) FROM auth_fye_threads",
            auth_fye_threads_cte!()
        );
        let readers = [alone, stats_tier_sql()];
        let unpin = |sql: &str| {
            let unpinned = sql.replace("CROSS JOIN messages fm", "JOIN messages fm");
            assert_ne!(unpinned, sql, "the pin is in the CTE");
            unpinned
        };
        let driven = |steps: &[String]| {
            steps.iter().any(|s| s == "SCAN h")
                && steps
                    .iter()
                    .any(|s| s == "SEARCH fm USING INTEGER PRIMARY KEY (rowid=?)")
                && !steps.iter().any(|s| s.starts_with("SCAN fm"))
        };
        {
            let conn = store.lock().unwrap();
            for sql in &readers {
                let steps = query_plan(&conn, sql);
                assert!(driven(&steps), "no statistics: {steps:?}");
            }
        }

        // The production pass, which writes the fixture's own statistics
        // (stat1 only, under its analysis limit), then the 50k bench store's
        // in their place.
        store.refresh_statistics().unwrap();
        let conn = store.lock().unwrap();
        conn.execute_batch(
            "DELETE FROM sqlite_stat1;
             INSERT INTO sqlite_stat1(tbl,idx,stat) VALUES
               ('accounts','sqlite_autoindex_accounts_1','1 1'),
               ('agent_message_decisions','sqlite_autoindex_agent_message_decisions_1','50000 50000 1'),
               ('agent_message_decisions','idx_decisions_critical','50 1'),
               ('agent_message_state','sqlite_autoindex_agent_message_state_1','50000 50000 1'),
               ('agent_thread_attention','sqlite_autoindex_agent_thread_attention_1','15862 15862 1'),
               ('messages','idx_messages_from_nocase','50000 50000 10'),
               ('messages','idx_messages_from','50000 50000 10'),
               ('messages','idx_messages_received','50000 50000 1'),
               ('messages','idx_messages_thread_received','50000 50000 4 1'),
               ('messages','idx_messages_thread','50000 50000 4'),
               ('messages','sqlite_autoindex_messages_1','50000 50000 1'),
               ('triage','idx_triage_status','50000 50000 16667');
             ANALYZE sqlite_schema;",
        )
        .unwrap();
        for sql in &readers {
            let unpinned = query_plan(&conn, &unpin(sql));
            assert!(
                unpinned.iter().any(|s| s.starts_with("SCAN fm")),
                "canary: unpinned, a store with statistics scans every message: {unpinned:?}"
            );
            let steps = query_plan(&conn, sql);
            assert!(driven(&steps), "with statistics: {steps:?}");
        }
    }

    /// The shredder keeps evidence: never a message ruled "Not me", never a
    /// critical change still waiting for a ruling.
    #[test]
    fn the_shredder_keeps_disowned_mail_and_unruled_critical_changes() {
        use crate::triage::decision::AuthKind::*;
        let store = fixture();
        seed_auth(&store, 50, auth(&[Otp], false), true);
        seed_auth(
            &store,
            51,
            auth(&[PasswordReset, AccountChange], true),
            true,
        );
        seed_auth(&store, 52, auth(&[Otp], false), true);
        store
            .set_auth_verdict(1, 52, Some(crate::types::AuthVerdict::NotMine), Utc::now())
            .unwrap();
        let cutoff = Utc::now() + chrono::Duration::hours(1);
        let candidates = |store: &SqliteStore| {
            store
                .shred_candidates(1, cutoff, 10)
                .unwrap()
                .into_iter()
                .map(|c| (c.message_id, c.kind))
                .collect::<Vec<_>>()
        };
        assert_eq!(candidates(&store), vec![(50, Some("otp".to_string()))]);
        assert_eq!(store.shred_pending_count(1, cutoff).unwrap(), 1);
        // Ruled "That was me", the change is ordinary auth mail again.
        store
            .set_auth_verdict(1, 51, Some(crate::types::AuthVerdict::Mine), Utc::now())
            .unwrap();
        let ids: Vec<i64> = candidates(&store).into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&50) && ids.contains(&51));
        assert_eq!(store.shred_pending_count(1, cutoff).unwrap(), 2);
    }

    /// A body heal or re-fetch bumps the message's state revision before the
    /// re-triage lands. The critical change and the "Not me" stay in FYE
    /// through it.
    #[test]
    fn auth_fye_survives_a_content_revision() {
        use crate::triage::decision::AuthKind::*;
        let store = fixture();
        seed_auth(&store, 10, auth(&[AccountChange], true), false);
        seed_auth(&store, 11, auth(&[LoginAlert], false), false);
        store
            .set_auth_verdict(1, 11, Some(crate::types::AuthVerdict::NotMine), Utc::now())
            .unwrap();
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE agent_message_state SET revision=revision+1 WHERE message_id IN (10,11)",
                [],
            )
            .unwrap();
        let mut listed: Vec<i64> = store
            .agent_fye(1, 100, &RankingConfig::default(), Utc::now())
            .unwrap()
            .into_iter()
            .map(|item| item.message_id)
            .collect();
        listed.sort();
        assert_eq!(listed, vec![10, 11]);
    }

    /// FYE placement never opens the agent door: a restricted critical change
    /// with no ruling is in the owner's FYE and absent from the agent's.
    #[test]
    fn restricted_critical_auth_is_in_human_fye_but_not_the_agent_door() {
        use crate::triage::decision::AuthKind::*;
        let store = fixture();
        seed_auth(&store, 10, auth(&[AccountChange], true), true);
        seed_auth(&store, 11, auth(&[AccountChange], true), false);
        let human: Vec<i64> = store
            .agent_fye(1, 100, &RankingConfig::default(), Utc::now())
            .unwrap()
            .into_iter()
            .map(|item| item.message_id)
            .collect();
        assert!(human.contains(&10) && human.contains(&11));
        let agent: Vec<i64> = store
            .external_agent_fye(1, 100, &RankingConfig::default(), Utc::now())
            .unwrap()
            .into_iter()
            .map(|item| item.message_id)
            .collect();
        assert!(
            !agent.contains(&10),
            "restricted auth reached the agent door"
        );
        // The unrestricted one is mail the agent may already read, and its
        // placement is all that changed.
        assert!(agent.contains(&11));
        assert!(!store.agent_access_allowed(1, 10).unwrap());
    }

    #[test]
    fn fye_toggle_preserves_related_provenance_even_without_message_classification() {
        use crate::triage::decision::RelatedAttentionUpdate;
        for classified in [false, true] {
            let store = fixture();
            if classified {
                let (job, context) = claim(&store, 1);
                store
                    .commit_agent_decision(
                        &job,
                        &context,
                        &decision(1),
                        std::slice::from_ref(&context.message.source),
                    )
                    .unwrap();
            }
            let (job, context) = claim(&store, 2);
            let target = store.agent_thread_context(1, "one").unwrap();
            let mut verdict = decision(2);
            let mut attention = decision(1).attention;
            attention.summary = "Receipt-derived attention".into();
            attention
                .actions
                .push(crate::triage::decision::AttentionAction {
                    description: "Check payment".into(),
                    ..Default::default()
                });
            verdict.related_updates.push(RelatedAttentionUpdate {
                thread_id: "one".into(),
                expected_revision: target.revision.attention_revision,
                attention,
                evidence: vec![],
            });
            let sources = store.snapshot_agent_sources(1, &[1, 2]).unwrap();
            assert_eq!(
                store
                    .commit_agent_decision(&job, &context, &verdict, &sources)
                    .unwrap(),
                AgentCommitOutcome::Applied
            );
            store
                .correct_agent_triage(
                    1,
                    2,
                    "external_access",
                    &serde_json::json!(true),
                    Utc::now(),
                )
                .unwrap();
            for show in [false, true] {
                let mut expected = attention_projection(&store, "one");
                expected["show_in_fye"] = serde_json::json!(show);
                expected["attention"]["show_in_fye"] = serde_json::json!(show);
                expected["revision"] =
                    serde_json::json!(expected["revision"].as_i64().unwrap() + 1);
                store
                    .correct_agent_triage(1, 1, "show_in_fye", &serde_json::json!(show), Utc::now())
                    .unwrap();
                assert_eq!(
                    attention_projection(&store, "one"),
                    expected,
                    "classified={classified}"
                );
                assert!(
                    store
                        .external_agent_fye(1, 10, &RankingConfig::default(), Utc::now())
                        .unwrap()
                        .is_empty()
                );
                assert!(store.external_agent_reading(1, 10).unwrap().is_empty());
                assert!(store.external_agent_records(1, 10).unwrap().is_empty());
            }
        }
    }

    #[test]
    fn related_update_commits_with_sent_only_or_spam_only_target() {
        use crate::triage::decision::RelatedAttentionUpdate;
        for sent_only in [true, false] {
            let store = fixture();
            // The spam case represents a previously classified bill subsequently
            // marked spam. The sent-only case has never had a classification.
            if !sent_only {
                let (job, context) = claim(&store, 1);
                store
                    .commit_agent_decision(
                        &job,
                        &context,
                        &decision(1),
                        std::slice::from_ref(&context.message.source),
                    )
                    .unwrap();
            }
            store
                .lock()
                .unwrap()
                .execute(
                    "UPDATE messages SET is_sent=?1,is_spam=?2 WHERE id=1",
                    params![sent_only, !sent_only],
                )
                .unwrap();
            let (job, context) = claim(&store, 2);
            let target = store.agent_thread_context(1, "one").unwrap();
            let mut verdict = decision(2);
            verdict.related_updates.push(RelatedAttentionUpdate {
                thread_id: "one".into(),
                expected_revision: target.revision.attention_revision,
                attention: decision(1).attention,
                evidence: vec![],
            });
            let sources = store.snapshot_agent_sources(1, &[1, 2]).unwrap();
            assert_eq!(
                store
                    .commit_agent_decision(&job, &context, &verdict, &sources)
                    .unwrap(),
                AgentCommitOutcome::Applied
            );
            let projection = attention_projection(&store, "one");
            assert_eq!(projection["message_id"], 1);
            assert_eq!(projection["decision_message_id"], 1);
            let state: String = store
                .lock()
                .unwrap()
                .query_row(
                    "SELECT state FROM agent_triage_jobs WHERE id=?1",
                    [job.id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(state, "completed");
            for show in [false, true] {
                store
                    .correct_agent_triage(1, 1, "show_in_fye", &serde_json::json!(show), Utc::now())
                    .unwrap();
                assert_eq!(
                    attention_projection(&store, "one")["decision_message_id"],
                    1
                );
            }
            assert!(
                store
                    .agent_fye(1, 10, &RankingConfig::default(), Utc::now())
                    .unwrap()
                    .iter()
                    .all(|item| item.thread_id != "one")
            );
            assert_eq!(store.agent_records(1, 10).unwrap().len(), 1);
        }
    }

    #[test]
    fn an_archive_row_is_never_a_threads_fallback_representative() {
        use crate::triage::decision::RelatedAttentionUpdate;
        // Evidence in thread "two" updates attention on thread "one" without
        // naming a message there, so the representative is the fallback's
        // pick. With thread "one" live that is message 1; with it archived
        // there is nothing to anchor a card to, and none is made.
        for archived in [false, true] {
            let store = fixture();
            if archived {
                // Assessed as allowed, so the agent may read it as evidence;
                // it still may not become the card.
                let conn = store.lock().unwrap();
                conn.execute("UPDATE messages SET is_archive=1 WHERE id=1", [])
                    .unwrap();
                seed_archive_state_conn(&conn, 1, 1).unwrap();
                conn.execute(
                    "UPDATE agent_message_state SET access='allowed' WHERE message_id=1",
                    [],
                )
                .unwrap();
            }
            let (job, context) = claim(&store, 2);
            let sources = store.snapshot_agent_sources(1, &[1, 2]).unwrap();
            let mut verdict = decision(2);
            let mut attention = decision(1).attention;
            attention.relevant_message_ids.clear();
            verdict.related_updates.push(RelatedAttentionUpdate {
                thread_id: "one".into(),
                expected_revision: 0,
                attention,
                evidence: vec![],
            });
            assert_eq!(
                store
                    .commit_agent_decision(&job, &context, &verdict, &sources)
                    .unwrap(),
                AgentCommitOutcome::Applied
            );
            let anchored: Vec<i64> = store
                .lock()
                .unwrap()
                .prepare("SELECT message_id FROM agent_thread_attention WHERE thread_id='one'")
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            assert_eq!(anchored, if archived { vec![] } else { vec![1] });
        }
    }

    /// The named half of the same rule. The model may cite an old message as
    /// relevant to a thread's attention; cited or not, an archive row is never
    /// the card. Message 1 is the only message in thread "one", so with it
    /// archived there is nothing to anchor to and no card is made.
    #[test]
    fn an_archive_row_named_as_relevant_is_never_the_representative() {
        use crate::triage::decision::RelatedAttentionUpdate;
        for archived in [false, true] {
            let store = fixture();
            if archived {
                let conn = store.lock().unwrap();
                conn.execute("UPDATE messages SET is_archive=1 WHERE id=1", [])
                    .unwrap();
                seed_archive_state_conn(&conn, 1, 1).unwrap();
                conn.execute(
                    "UPDATE agent_message_state SET access='allowed' WHERE message_id=1",
                    [],
                )
                .unwrap();
            }
            let (job, context) = claim(&store, 2);
            let sources = store.snapshot_agent_sources(1, &[1, 2]).unwrap();
            let mut verdict = decision(2);
            let attention = decision(1).attention;
            assert_eq!(attention.relevant_message_ids, vec![1]);
            verdict.related_updates.push(RelatedAttentionUpdate {
                thread_id: "one".into(),
                expected_revision: 0,
                attention,
                evidence: vec![],
            });
            assert_eq!(
                store
                    .commit_agent_decision(&job, &context, &verdict, &sources)
                    .unwrap(),
                AgentCommitOutcome::Applied
            );
            let anchored: Vec<i64> = store
                .lock()
                .unwrap()
                .prepare("SELECT message_id FROM agent_thread_attention WHERE thread_id='one'")
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            assert_eq!(anchored, if archived { vec![] } else { vec![1] });
        }
    }

    #[test]
    fn unclassified_representative_stays_visible_without_borrowing_source_classification() {
        use crate::triage::decision::RelatedAttentionUpdate;
        for same_thread in [false, true] {
            let store = fixture();
            if same_thread {
                store
                    .lock()
                    .unwrap()
                    .execute("UPDATE messages SET thread_id='one' WHERE id=2", [])
                    .unwrap();
            }
            let (job, context) = claim(&store, 1);
            let mut verdict = decision(1);
            verdict.kinds = vec![EmailKind::Receipt];
            verdict.summary = "Unrelated receipt classification".into();
            let mut attention = decision(2).attention;
            attention.summary = "Obligation needs attention".into();
            if same_thread {
                verdict.attention = attention;
            } else {
                verdict.related_updates.push(RelatedAttentionUpdate {
                    thread_id: "two".into(),
                    expected_revision: 0,
                    attention,
                    evidence: vec![],
                });
            }
            let sources = store.snapshot_agent_sources(1, &[1, 2]).unwrap();
            assert_eq!(
                store
                    .commit_agent_decision(&job, &context, &verdict, &sources)
                    .unwrap(),
                AgentCommitOutcome::Applied
            );
            let fye = store
                .agent_fye(1, 10, &RankingConfig::default(), Utc::now())
                .unwrap();
            let item = fye
                .iter()
                .find(|item| item.message_id == 2)
                .expect("unclassified representative remains visible");
            assert_eq!(item.decision.summary, "subject");
            assert_eq!(item.decision.reason, "Triage pending");
            assert!(item.decision.kinds.is_empty());
            assert!(item.decision.records.is_empty());
            assert_eq!(item.attention.summary, "Obligation needs attention");
            assert_eq!(item.decision_source_message_id, 2);
            assert!(
                store
                    .external_agent_fye(1, 10, &RankingConfig::default(), Utc::now())
                    .unwrap()
                    .is_empty(),
                "pending representative access stays closed"
            );

            store
                .enqueue_agent_triage(1, 2, "manual:classify", false)
                .unwrap();
            let job = store
                .claim_agent_job(1, "investigation", Utc::now(), 60)
                .unwrap()
                .unwrap();
            assert_eq!(job.message_id, 2);
            let context = store.load_agent_context(&job).unwrap();
            let mut bill = decision(2);
            bill.kinds = vec![EmailKind::Bill];
            bill.summary = "Target bill classification".into();
            let sources = store.snapshot_agent_sources(1, &[1, 2]).unwrap();
            assert_eq!(
                store
                    .commit_agent_decision(&job, &context, &bill, &sources)
                    .unwrap(),
                AgentCommitOutcome::Applied
            );
            let fye = store
                .agent_fye(1, 10, &RankingConfig::default(), Utc::now())
                .unwrap();
            let item = fye.iter().find(|item| item.message_id == 2).unwrap();
            assert_eq!(item.decision.summary, "Target bill classification");
            assert_eq!(item.decision.kinds, vec![EmailKind::Bill]);
        }
    }

    #[test]
    fn first_related_attention_accepts_observed_revision_zero() {
        use crate::triage::decision::RelatedAttentionUpdate;
        let store = fixture();
        let (job, context) = claim(&store, 1);
        let source = store.agent_read_message(1, 2).unwrap();
        assert_eq!(source.source.attention_revision, 0);
        let mut verdict = decision(1);
        verdict.related_updates.push(RelatedAttentionUpdate {
            thread_id: "two".into(),
            expected_revision: 0,
            attention: decision(2).attention,
            evidence: vec![],
        });
        assert_eq!(
            store
                .commit_agent_decision(
                    &job,
                    &context,
                    &verdict,
                    &[context.message.source.clone(), source.source]
                )
                .unwrap(),
            AgentCommitOutcome::Applied
        );
        assert!(
            store
                .agent_thread_context(1, "two")
                .unwrap()
                .attention
                .is_some()
        );
    }

    /// A past-event re-check is claimed ahead of an older revisit of the
    /// agent's own (and of other background work): they share one daily cap,
    /// and a landed flight's re-check must not wait behind it.
    #[test]
    fn a_past_event_recheck_is_claimed_ahead_of_other_revisits() {
        let store = fixture();
        store
            .lock()
            .unwrap()
            .execute_batch(
                "INSERT INTO agent_triage_jobs(account_id,message_id,kind,trigger,input_revision,arrival_eligible,available_at)
                 VALUES(1,1,'triage','revisit:2026-01-01T00:00:00+00:00',1,0,'2026-01-01'),
                       (1,2,'triage','revisit:event_ended:2026-01-02T00:00:00+00:00',1,0,'2026-01-02')",
            )
            .unwrap();
        let first = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert_eq!(first.message_id, 2);
        let second = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert_eq!(second.message_id, 1);
    }

    #[test]
    fn manual_request_precedes_migration_then_migration_resumes() {
        let store = fixture();
        store
            .enqueue_agent_triage(1, 1, "migration", false)
            .unwrap();
        store
            .enqueue_agent_triage(1, 2, "manual:new", false)
            .unwrap();
        store.lock().unwrap().execute("UPDATE agent_triage_jobs SET available_at=CASE message_id WHEN 1 THEN '2026-01-01' ELSE '2026-01-02' END", []).unwrap();
        let job = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert_eq!(job.message_id, 2);
        let next = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert_eq!(next.message_id, 1);
    }

    #[test]
    fn claim_plan_excludes_two_hundred_thousand_completed_jobs() {
        let store = std::sync::Arc::new(fixture());
        store.lock().unwrap().execute_batch("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<200000)
            INSERT INTO agent_triage_jobs(account_id,message_id,kind,trigger,input_revision,state,available_at)
            SELECT 1,1,'triage','history',i,'completed','2026-01-01' FROM n;").unwrap();
        store.lock().unwrap().execute_batch("WITH RECURSIVE n(i) AS (SELECT 100 UNION ALL SELECT i+1 FROM n WHERE i<1599)
            INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body)
            SELECT i,1,'migration-'||i,'migration-'||i,'sender@test','subject','2026-01-01','snippet','body' FROM n;
            INSERT INTO agent_triage_jobs(account_id,message_id,kind,trigger,input_revision,available_at)
            SELECT 1,id,'triage','migration',1,'2026-01-01' FROM messages WHERE id>=100;").unwrap();
        store.enqueue_agent_triage(1, 2, "arrival", true).unwrap();
        let plan = {
            let conn = store.lock().unwrap();
            let mut query = conn
                .prepare(&format!("EXPLAIN QUERY PLAN {CLAIM_JOB_SQL}"))
                .unwrap();
            query
                .query_map(
                    params![
                        1,
                        "investigation",
                        Utc::now().to_rfc3339(),
                        Utc::now().timestamp()
                    ],
                    |r| r.get::<_, String>(3),
                )
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
                .join("\n")
        };
        assert!(plan.contains("idx_agent_jobs_pending_claim"), "{plan}");
        let reader_store = store.clone();
        let reader = std::thread::spawn(move || {
            for _ in 0..100 {
                reader_store.agent_read_message(1, 1).unwrap();
            }
        });
        let start = std::time::Instant::now();
        let cpu_start = thread_cpu();
        let arrival = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert_eq!(
            arrival.message_id, 2,
            "arrival precedes the migration backlog"
        );
        store.complete_agent_job(&arrival).unwrap();
        for _ in 0..99 {
            let job = store
                .claim_agent_job(1, "investigation", Utc::now(), 60)
                .unwrap()
                .unwrap();
            assert_eq!(job.trigger, "migration");
            store.complete_agent_job(&job).unwrap();
        }
        let cpu = thread_cpu() - cpu_start;
        let elapsed = start.elapsed();
        reader.join().unwrap();
        eprintln!(
            "200k completed jobs, 1,500 migrations, 100 claims + concurrent reads: {elapsed:?} wall, {cpu:?} cpu; mean {:?} cpu",
            cpu / 100
        );
        // EXPLAIN is the deterministic regression guard. This bound is the
        // backstop, and it is on the claiming thread's CPU rather than the wall
        // clock: the full suite on a small CI runner stretched the wall time of
        // an unchanged claim path to 6s, while the reported 74-92ms per-claim
        // history scan costs 7-9s of CPU for these 100 claims under any load.
        // Waiting on the store mutex behind the reader thread burns no CPU.
        assert!(cpu < std::time::Duration::from_secs(2), "{cpu:?}");
    }

    /// CPU consumed by the calling thread so far.
    fn thread_cpu() -> std::time::Duration {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `ts` is a valid, writable timespec for the call's duration.
        let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
        assert_eq!(rc, 0, "clock_gettime(CLOCK_THREAD_CPUTIME_ID)");
        std::time::Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
    }

    #[test]
    fn initial_investigation_selector_leaves_revisits_queued() {
        let store = fixture();
        store
            .enqueue_agent_triage(1, 1, "revisit:due", false)
            .unwrap();
        store
            .enqueue_agent_triage(1, 2, "migration", false)
            .unwrap();
        let fresh = store
            .claim_agent_job(1, "initial_investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert_eq!(fresh.message_id, 2);
        assert!(
            store
                .claim_agent_job(1, "initial_investigation", Utc::now(), 60)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .claim_agent_job(1, "investigation", Utc::now(), 60)
                .unwrap()
                .unwrap()
                .message_id,
            1
        );
    }

    #[test]
    fn only_a_capped_surfacing_request_assesses_an_archive_source() {
        let store = fixture();
        let conn = store.lock().unwrap();
        conn.execute("UPDATE messages SET is_archive=1 WHERE id=1", [])
            .unwrap();
        seed_archive_state_conn(&conn, 1, 1).unwrap();
        let queued = |conn: &Connection| -> Vec<(String, String)> {
            conn.prepare("SELECT kind,trigger FROM agent_triage_jobs WHERE message_id=1")
                .unwrap()
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap()
        };
        let message = read_message(&conn, 1, 1).unwrap();

        // The triage agent's own reads, a human's open, and the source repair
        // behind a thread read: nothing. None of them carries a ceiling.
        ensure_source_assessments(&conn, 1, std::slice::from_ref(&message)).unwrap();
        ensure_agent_source_ids_conn(&conn, 1, &[1]).unwrap();
        assert!(queued(&conn).is_empty());

        // A surfacing request with nothing left to spend: still nothing.
        let caps = ArchiveAccessCaps::default();
        let spent = ArchiveAccessCaps { daily: 0, ..caps };
        assert!(
            request_archive_access_conn(&conn, 1, &[1], 8, spent, Utc::now())
                .unwrap()
                .is_empty()
        );
        assert!(queued(&conn).is_empty());

        // One inside its caps: one access job, on the revision the archive
        // write recorded.
        assert_eq!(
            request_archive_access_conn(&conn, 1, &[1], 8, caps, Utc::now()).unwrap(),
            vec![1]
        );
        assert_eq!(
            queued(&conn),
            vec![("access".to_string(), "archive_access".to_string())]
        );
        let revision: i64 = conn
            .query_row(
                "SELECT revision FROM agent_message_state WHERE message_id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(revision, 1, "being asked about is not a change of content");
    }

    #[test]
    fn evidence_reads_do_not_resurrect_exhausted_access_jobs() {
        let store = fixture();
        store.agent_read_message(1, 1).unwrap();
        let job = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        store.fail_agent_job(&job, "attempts_exhausted").unwrap();
        store.agent_read_message(1, 1).unwrap();
        store.snapshot_agent_sources(1, &[1]).unwrap();
        assert!(
            store
                .claim_agent_job(1, "investigation", Utc::now(), 60)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn investigations_serialize_per_thread_without_blocking_notifications() {
        let store = fixture();
        store
            .lock()
            .unwrap()
            .execute("UPDATE messages SET thread_id='one' WHERE id=2", [])
            .unwrap();
        let (first, _) = claim(&store, 1);
        store.enqueue_agent_triage(1, 2, "manual", false).unwrap();
        assert!(
            store
                .claim_agent_job(1, "investigation", Utc::now(), 60)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .claim_agent_job(1, "notification", Utc::now(), 60)
                .unwrap()
                .is_some()
        );
        assert!(store.complete_agent_job(&first).unwrap());
        let next = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert_eq!(next.message_id, 2);
    }

    #[test]
    fn passive_listing_does_not_discard_a_paid_investigation() {
        let store = fixture();
        let (job, context) = claim(&store, 1);
        let sources = store.snapshot_agent_sources(1, &[1]).unwrap();
        crate::store::Store::mark_surfaced(&store, 1, &[1]).unwrap();
        assert_eq!(store.agent_read_message(1, 1).unwrap().status, "open");
        assert_eq!(
            store
                .commit_agent_decision(&job, &context, &decision(1), &sources)
                .unwrap(),
            AgentCommitOutcome::Applied
        );
    }

    #[test]
    fn real_ingest_arrivals_outrank_old_migration_and_cutover_does_not_duplicate_them() {
        let store = fixture();
        store
            .enqueue_agent_triage(1, 1, "migration", false)
            .unwrap();
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE agent_triage_jobs SET available_at='2000-01-01' WHERE message_id=1",
                [],
            )
            .unwrap();
        store.enqueue_agent_triage(1, 2, "ingest", true).unwrap();
        store.enqueue_agent_triage(1, 2, "ingest", true).unwrap();
        assert_eq!(
            store
                .initialize_agent_cutover(1, Utc::now() - Duration::days(30))
                .unwrap(),
            0
        );
        let jobs: i64 = store
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM agent_triage_jobs WHERE kind IN ('triage','access')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(jobs, 2);
        assert_eq!(
            store
                .claim_agent_job(1, "investigation", Utc::now(), 60)
                .unwrap()
                .unwrap()
                .message_id,
            2
        );
    }

    #[test]
    fn changed_content_transfers_unfinished_notification_eligibility() {
        let store = fixture();
        store.enqueue_agent_triage(1, 1, "ingest", true).unwrap();
        let old = store
            .claim_agent_job(1, "notification", Utc::now(), 60)
            .unwrap()
            .unwrap();
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE messages SET body='complete fetched body' WHERE id=1",
                [],
            )
            .unwrap();
        store.enqueue_agent_triage(1, 1, "ingest", true).unwrap();
        assert!(!store.complete_agent_job(&old).unwrap());
        let new = store
            .claim_agent_job(1, "notification", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert!(new.arrival_eligible);
        assert_ne!(new.id, old.id);
        assert!(
            store
                .claim_agent_job(1, "investigation", Utc::now(), 60)
                .unwrap()
                .unwrap()
                .arrival_eligible
        );
    }

    #[test]
    fn revisit_policy_bounds_time_pending_deduplication_and_lifetime_across_revisions() {
        use crate::triage::decision::RevisitRequest;
        let store = fixture();
        let (job, _) = claim(&store, 1);
        let now = Utc::now();
        let mut policy = crate::config::RevisitPassConfig {
            max_per_message: 1,
            max_per_message_lifetime: 2,
            max_horizon_days: 7,
            ..Default::default()
        };
        let mut verdict = decision(1);
        verdict.revisit = Some(RevisitRequest {
            at: now - Duration::days(1),
            reason: "check".into(),
        });
        let conn = store.lock().unwrap();
        policy.enabled = false;
        schedule_revisit(&conn, &job, 1, &verdict, &policy, now).unwrap();
        let count = || {
            conn.query_row(
                "SELECT COUNT(*) FROM agent_triage_jobs WHERE trigger LIKE 'revisit:%'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
        };
        assert_eq!(count(), 0);
        policy.enabled = true;
        schedule_revisit(&conn, &job, 1, &verdict, &policy, now).unwrap();
        let at: String = conn
            .query_row(
                "SELECT available_at FROM agent_triage_jobs WHERE trigger LIKE 'revisit:%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            DateTime::parse_from_rfc3339(&at).unwrap(),
            now + Duration::hours(1)
        );
        verdict.revisit.as_mut().unwrap().at = now + Duration::days(2);
        schedule_revisit(&conn, &job, 2, &verdict, &policy, now).unwrap();
        assert_eq!(count(), 1, "pending cap applies across revisions");
        conn.execute(
            "UPDATE agent_triage_jobs SET state='completed' WHERE trigger LIKE 'revisit:%'",
            [],
        )
        .unwrap();
        verdict.revisit.as_mut().unwrap().at = now + Duration::hours(2);
        schedule_revisit(&conn, &job, 2, &verdict, &policy, now).unwrap();
        assert_eq!(count(), 1, "nearby completed schedules deduplicate");
        verdict.revisit.as_mut().unwrap().at = now + Duration::days(8);
        schedule_revisit(&conn, &job, 2, &verdict, &policy, now).unwrap();
        assert_eq!(count(), 1, "far future requests do not escape horizon");
        verdict.revisit.as_mut().unwrap().at = now + Duration::days(2);
        schedule_revisit(&conn, &job, 2, &verdict, &policy, now).unwrap();
        conn.execute(
            "UPDATE agent_triage_jobs SET state='completed' WHERE trigger LIKE 'revisit:%'",
            [],
        )
        .unwrap();
        verdict.revisit.as_mut().unwrap().at = now + Duration::days(4);
        schedule_revisit(&conn, &job, 3, &verdict, &policy, now).unwrap();
        assert_eq!(
            count(),
            2,
            "manual passes and content revisions do not reset lifetime"
        );
    }

    #[test]
    fn reading_relevant_corrections_clear_the_models_reading_kind() {
        use crate::triage::decision::ReadingKind;
        use serde_json::json;
        let cases = [
            ("kinds", json!({"add": ["promotional"], "remove": []}), true),
            ("kinds", json!({"add": [], "remove": ["editorial"]}), true),
            ("kinds", json!(["promotional"]), true),
            (
                "destinations",
                json!({"add": [], "remove": ["reading"]}),
                true,
            ),
            // Unrelated or no-op edits keep the model's call.
            (
                "kinds",
                json!({"add": ["event_reservation"], "remove": []}),
                false,
            ),
            ("kinds", json!(["editorial", "general"]), false),
            ("destinations", json!(["reading"]), false),
            ("show_in_fye", json!(true), false),
        ];
        for (field, value, clears) in cases {
            let mut decision = MessageDecision {
                kinds: vec![EmailKind::Editorial],
                destinations: vec![MessageDestination::Reading],
                reading_kind: Some(ReadingKind::Content),
                ..Default::default()
            };
            apply_correction(&mut decision, field, &value).unwrap();
            let expected = if clears {
                None
            } else {
                Some(ReadingKind::Content)
            };
            assert_eq!(decision.reading_kind, expected, "{field} {value}");
        }
    }
    #[test]
    fn additive_corrections_preserve_other_choices_and_future_model_categories() {
        let store = fixture();
        let (job, context) = claim(&store, 1);
        let mut initial = decision(1);
        initial.kinds = vec![EmailKind::Correspondence, EmailKind::Bill];
        store
            .commit_agent_decision(
                &job,
                &context,
                &initial,
                std::slice::from_ref(&context.message.source),
            )
            .unwrap();
        store
            .correct_agent_triage_delta(
                1,
                1,
                "kinds",
                &["receipt".into()],
                &["bill".into()],
                Utc::now(),
            )
            .unwrap();
        store
            .correct_agent_triage_delta(
                1,
                1,
                "kinds",
                &["editorial".into()],
                &["correspondence".into()],
                Utc::now(),
            )
            .unwrap();
        let current = store
            .agent_thread_context(1, "one")
            .unwrap()
            .previous_decision
            .unwrap();
        assert_eq!(
            current.kinds,
            vec![EmailKind::Receipt, EmailKind::Editorial]
        );
        let job = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        let context = store.load_agent_context(&job).unwrap();
        let mut next = decision(1);
        next.kinds = vec![EmailKind::Bill, EmailKind::FinancialUpdate];
        store
            .commit_agent_decision(
                &job,
                &context,
                &next,
                std::slice::from_ref(&context.message.source),
            )
            .unwrap();
        assert_eq!(
            store
                .agent_thread_context(1, "one")
                .unwrap()
                .previous_decision
                .unwrap()
                .kinds,
            vec![
                EmailKind::FinancialUpdate,
                EmailKind::Receipt,
                EmailKind::Editorial
            ]
        );
    }

    #[test]
    fn evidence_reads_index_mail_and_queue_only_consumed_legacy_sources() {
        let store = fixture();
        store
            .lock()
            .unwrap()
            .execute(
                &format!(
                    "INSERT INTO messages_fts(rowid,subject,body,tsubject,tbody) {}",
                    crate::store::sqlite::translations::FTS_ROW_SELECT
                ),
                [],
            )
            .unwrap();
        store
            .lock()
            .unwrap()
            .execute("UPDATE messages SET is_sent=1 WHERE id=2", [])
            .unwrap();
        assert_eq!(
            store
                .agent_search_mail(1, "body", 10)
                .unwrap()
                .iter()
                .map(|m| m.id)
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(
            store
                .agent_sender_history(1, "sender@test", 10)
                .unwrap()
                .len(),
            1
        );
        let state_count: i64 = store
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM agent_message_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            state_count, 1,
            "unconsumed sent and other-account rows stay untouched"
        );
        let job = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert_eq!(job.kind, "access");
        assert_eq!(job.message_id, 1);
        assert!(!job.arrival_eligible);
    }
    #[test]
    fn legacy_sources_are_assessed_only_when_observed_and_eventually_unblock_derivatives() {
        let store = fixture();
        store
            .lock()
            .unwrap()
            .execute("UPDATE messages SET thread_id='one' WHERE id=2", [])
            .unwrap();
        let (job, context) = claim(&store, 1);
        let count_states = || {
            store
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM agent_message_state", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap()
        };
        assert_eq!(
            count_states(),
            1,
            "loading context must not enqueue every old sibling"
        );
        let sources = store.snapshot_agent_sources(1, &[1, 2]).unwrap();
        assert_eq!(
            count_states(),
            2,
            "only sources selected for the prompt initialize access work"
        );
        assert_eq!(
            store
                .commit_agent_decision(&job, &context, &decision(1), &sources)
                .unwrap(),
            AgentCommitOutcome::Applied
        );
        assert!(!store.agent_access_allowed(1, 1).unwrap());
        let sibling = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert_eq!(sibling.message_id, 2);
        assert_eq!(sibling.kind, "access");
        let context = store.load_agent_context(&sibling).unwrap();
        let sources = store.snapshot_agent_sources(1, &[1, 2]).unwrap();
        assert_eq!(
            store
                .commit_agent_decision(&sibling, &context, &decision(2), &sources)
                .unwrap(),
            AgentCommitOutcome::Applied
        );
        assert!(
            store.agent_access_allowed(1, 1).unwrap(),
            "source revision1 matches its later allowed assessment"
        );
        assert!(
            store
                .claim_agent_job(1, "investigation", Utc::now(), 60)
                .unwrap()
                .is_none(),
            "no self-requeue"
        );
    }

    #[test]
    fn unrelated_sender_rule_does_not_stale_an_investigation() {
        let store = fixture();
        let (job, original) = claim(&store, 1);
        crate::store::Store::set_sender_rule(
            &store,
            1,
            "elsewhere@other.test",
            "ignore promotions",
            crate::types::Disposition::Squelch,
        )
        .unwrap();
        let fresh = store.load_agent_context(&job).unwrap();
        assert_eq!(original.revision.preferences, fresh.revision.preferences);
        assert_eq!(
            store
                .commit_agent_decision(
                    &job,
                    &original,
                    &decision(1),
                    &store.snapshot_agent_sources(1, &[1]).unwrap()
                )
                .unwrap(),
            AgentCommitOutcome::Applied
        );
    }

    #[test]
    fn multiword_evidence_search_keeps_partial_matches_below_exact_matches() {
        let store = fixture();
        // Update both storage and index: one complete match and one partial.
        {
            let conn = store.lock().unwrap();
            conn.execute("UPDATE messages SET body=CASE id WHEN 1 THEN 'invoice zebra' ELSE 'zebra shipment' END",[]).unwrap();
            conn.execute("DELETE FROM messages_fts", []).unwrap();
            conn.execute(
                &format!(
                    "INSERT INTO messages_fts(rowid,subject,body,tsubject,tbody) {}",
                    crate::store::sqlite::translations::FTS_ROW_SELECT
                ),
                [],
            )
            .unwrap();
        }
        let results = store.agent_search_mail(1, "invoice zebra", 10).unwrap();
        assert_eq!(results.first().map(|message| message.id), Some(1));
        assert!(
            results.iter().any(|message| message.id == 2),
            "partial matches support investigation"
        );
        assert_eq!(
            store
                .agent_search_mail(1, "invoice zebra", 1)
                .unwrap()
                .len(),
            1
        );
        assert!(store.agent_search_mail(1, "", 10).unwrap().is_empty());
    }

    #[test]
    fn context_marks_actual_contacts_and_only_matching_sender_preferences() {
        let store = fixture();
        store.lock().unwrap().execute("INSERT INTO contacts(account_id,addr,first_seen) VALUES(1,'SENDER@test','2026-01-01')",[]).unwrap();
        for (id, pattern) in [(1, "*@test"), (2, "someone@else")] {
            store.lock().unwrap().execute("INSERT INTO sender_rules(id,account_id,match_pattern,want_text,disposition,updated_at) VALUES(?1,1,?2,'preference','squelch','2026-01-01')",params![id,pattern]).unwrap();
        }
        let (_, context) = claim(&store, 1);
        assert!(context.sender_is_contact);
        assert_eq!(context.rules.len(), 2);
        assert_eq!(context.matched_rules.len(), 1);
        assert_eq!(context.matched_rules[0]["id"], 1);
    }
    #[test]
    fn ingest_duplicates_are_idempotent_and_account_scoped() {
        let store = fixture();
        store.enqueue_agent_triage(1, 1, "arrival", true).unwrap();
        store.enqueue_agent_triage(1, 1, "arrival", true).unwrap();
        assert!(
            store
                .claim_agent_job(2, "triage", Utc::now(), 60)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .claim_agent_job(1, "triage", Utc::now(), 60)
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .claim_agent_job(1, "triage", Utc::now(), 60)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .claim_agent_job(1, "notification", Utc::now(), 60)
                .unwrap()
                .is_some()
        );
        assert!(!store.agent_access_allowed(1, 1).unwrap());
        assert!(store.agent_read_message(2, 1).is_err());
    }
    #[test]
    fn commit_publishes_overlapping_destinations_and_access() {
        let store = fixture();
        let (job, context) = claim(&store, 1);
        assert_eq!(
            store
                .commit_agent_decision(
                    &job,
                    &context,
                    &decision(1),
                    &store.snapshot_agent_sources(1, &[1]).unwrap()
                )
                .unwrap(),
            AgentCommitOutcome::Applied
        );
        assert!(store.agent_access_allowed(1, 1).unwrap());
        assert_eq!(store.agent_reading(1, 10).unwrap().len(), 1);
        assert_eq!(store.agent_records(1, 10).unwrap().len(), 1);
        assert_eq!(
            store
                .agent_fye(1, 10, &RankingConfig::default(), Utc::now())
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .commit_agent_decision(
                    &job,
                    &context,
                    &decision(1),
                    &store.snapshot_agent_sources(1, &[1]).unwrap()
                )
                .unwrap(),
            AgentCommitOutcome::Stale
        );
    }
    #[test]
    fn user_open_during_inference_invalidates_commit_and_requeues() {
        let store = fixture();
        let (job, context) = claim(&store, 1);
        store.acknowledge_agent_message(1, 1, Utc::now()).unwrap();
        assert_eq!(
            store
                .commit_agent_decision(
                    &job,
                    &context,
                    &decision(1),
                    &store.snapshot_agent_sources(1, &[1]).unwrap()
                )
                .unwrap(),
            AgentCommitOutcome::Stale
        );
        assert!(store.agent_reading(1, 10).unwrap().is_empty());
        assert!(
            store
                .retry_agent_job(&job, Utc::now(), "stale_snapshot")
                .unwrap(),
            "worker still owns the lease for bounded retry"
        );
        assert!(
            store
                .claim_agent_job(1, "triage", Utc::now(), 60)
                .unwrap()
                .is_some()
        );
    }
    #[test]
    fn changed_sender_preference_invalidates_inflight_commit() {
        let store = fixture();
        let (job, context) = claim(&store, 1);
        store.lock().unwrap().execute("INSERT INTO sender_rules(account_id,match_pattern,want_text,disposition,updated_at)
             VALUES(1,'sender@test','quiet please','suppress','2026-09-15')",[]).unwrap();
        assert_eq!(
            store
                .commit_agent_decision(
                    &job,
                    &context,
                    &decision(1),
                    &store.snapshot_agent_sources(1, &[1]).unwrap()
                )
                .unwrap(),
            AgentCommitOutcome::Stale
        );
    }
    #[test]
    fn changed_content_revokes_access_until_reassessed() {
        let store = fixture();
        let (job, context) = claim(&store, 1);
        store
            .commit_agent_decision(
                &job,
                &context,
                &decision(1),
                &store.snapshot_agent_sources(1, &[1]).unwrap(),
            )
            .unwrap();
        store
            .lock()
            .unwrap()
            .execute("UPDATE messages SET body='new code' WHERE id=1", [])
            .unwrap();
        store.enqueue_agent_triage(1, 1, "arrival", true).unwrap();
        assert!(!store.agent_access_allowed(1, 1).unwrap());
        let job = store
            .claim_agent_job(1, "triage", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert!(
            job.arrival_eligible,
            "unfinished arrival work keeps its notification eligibility"
        );
    }
    #[test]
    fn restricted_consumed_source_prevents_derivative_exposure() {
        let store = fixture();
        let (job, context) = claim(&store, 1);
        store
            .commit_agent_decision(
                &job,
                &context,
                &decision(1),
                &store.snapshot_agent_sources(1, &[1, 2]).unwrap(),
            )
            .unwrap();
        assert!(!store.agent_access_allowed(1, 1).unwrap());
    }
    #[test]
    fn lease_reclamation_rejects_previous_owner() {
        let store = fixture();
        let (job, context) = claim(&store, 1);
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE agent_triage_jobs SET lease_until='2000-01-01' WHERE id=?1",
                [job.id],
            )
            .unwrap();
        let newer = store
            .claim_agent_job(1, "triage", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert_ne!(job.lease_token, newer.lease_token);
        assert_eq!(
            store
                .commit_agent_decision(
                    &job,
                    &context,
                    &decision(1),
                    &store.snapshot_agent_sources(1, &[1]).unwrap()
                )
                .unwrap(),
            AgentCommitOutcome::Stale
        );
        assert!(!store.complete_agent_job(&job).unwrap());
        assert!(!store.retry_agent_job(&job, Utc::now(), "timeout").unwrap());
    }
    /// A release puts the job back queued with `last_error` untouched (NULL
    /// here, so the row still reads as never pushed back) and the attempt
    /// it took kept; a second claim gets the same job, and the old lease
    /// can no longer release it.
    #[test]
    fn a_release_hands_the_lease_back_untouched() {
        let store = fixture();
        let (job, _context) = claim(&store, 1);
        assert_eq!(job.attempts, 1);
        assert!(store.release_agent_job(&job, Utc::now()).unwrap());
        let (state, attempts, last_error): (String, i64, Option<String>) = store
            .lock()
            .unwrap()
            .query_row(
                "SELECT state,attempts,last_error FROM agent_triage_jobs WHERE id=?1",
                [job.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((state.as_str(), attempts, last_error), ("queued", 1, None));
        let again = store
            .claim_agent_job(1, "triage", Utc::now(), 60)
            .unwrap()
            .expect("claimable again at once");
        assert_eq!(again.id, job.id);
        assert_eq!(again.attempts, 2, "the interrupted attempt counted");
        assert_ne!(again.lease_token, job.lease_token);
        assert!(
            !store.release_agent_job(&job, Utc::now()).unwrap(),
            "a stale lease"
        );
        assert!(store.release_agent_job(&again, Utc::now()).unwrap());
    }
    /// An `access` job is one call under the short access deadline, so it
    /// gets the lease the claim was given for it, where an investigation's
    /// covers a whole run; the plain claim leases every kind alike.
    #[test]
    fn an_access_job_is_leased_for_its_own_deadline() {
        let store = fixture();
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO agent_triage_jobs
                (account_id,message_id,kind,trigger,input_revision,state,available_at)
                VALUES(1,1,'access','source_access',1,'queued','2000-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        store.enqueue_agent_triage(1, 2, "ingest", true).unwrap();
        let lease_of = |id: i64| -> i64 {
            let until: String = store
                .lock()
                .unwrap()
                .query_row(
                    "SELECT lease_until FROM agent_triage_jobs WHERE id=?1",
                    [id],
                    |row| row.get(0),
                )
                .unwrap();
            let until: DateTime<Utc> = until.parse().unwrap();
            (until - Utc::now()).num_seconds()
        };
        let now = Utc::now();
        let mut leases = std::collections::BTreeMap::new();
        for _ in 0..2 {
            let job = store
                .claim_agent_job_sized(1, "investigation", now, 480, 60)
                .unwrap()
                .expect("two due jobs on two threads");
            leases.insert(job.kind.clone(), lease_of(job.id));
        }
        assert!((58..=60).contains(&leases["access"]), "{leases:?}");
        assert!((478..=480).contains(&leases["triage"]), "{leases:?}");
        for job in leases.keys() {
            assert!(job == "access" || job == "triage", "{job}");
        }
        assert!(
            store
                .claim_agent_job_sized(1, "investigation", now, 480, 0)
                .is_err(),
            "both leases are validated"
        );
    }
    #[test]
    fn explicit_delivery_proposals_enter_carrier_polling_and_retract_atomically() {
        use crate::triage::decision::RecordProposal;
        let store = fixture();
        let (job, context) = claim(&store, 1);
        let mut verdict = decision(1);
        verdict.records.push(RecordProposal::Delivery {
            carrier: Some("ups".into()),
            tracking_number: Some("1Z999AA10123456784".into()),
            status: "shipped".into(),
            item_name: None,
            merchant: None,
            order_refs: vec![],
            evidence: vec![],
        });
        store
            .commit_agent_decision(
                &job,
                &context,
                &verdict,
                std::slice::from_ref(&context.message.source),
            )
            .unwrap();
        let rows = store
            .list_pollable_shipments(1, Utc::now() - Duration::days(1), 5)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].tracking_number, "1Z999AA10123456784",
            "explicit valid tracking enters carrier polling"
        );
        assert_eq!(store.external_shipments(1, true).unwrap().len(), 1);
        // The silence window is the human listing's alone: this feed is merged
        // with the agent's own delivery records, which have nothing to age by.
        let silent = (Utc::now() - Duration::days(30)).to_rfc3339();
        store
            .lock()
            .unwrap()
            .execute("UPDATE shipments SET last_update=?1", [&silent])
            .unwrap();
        let policy = crate::config::ShipmentListPolicy::default();
        assert!(store.list_shipments(1, true, policy).unwrap().is_empty());
        assert_eq!(store.external_shipments(1, true).unwrap().len(), 1);
        store
            .enqueue_agent_triage(1, 1, "record_retraction", false)
            .unwrap();
        let job = store
            .claim_agent_job(1, "triage", Utc::now(), 60)
            .unwrap()
            .unwrap();
        let context = store.load_agent_context(&job).unwrap();
        store
            .commit_agent_decision(
                &job,
                &context,
                &decision(1),
                std::slice::from_ref(&context.message.source),
            )
            .unwrap();
        assert!(
            store
                .list_pollable_shipments(1, Utc::now() - Duration::days(1), 5)
                .unwrap()
                .is_empty()
        );
        assert!(store.external_shipments(1, true).unwrap().is_empty());
    }

    #[test]
    fn shared_delivery_retraction_rebuilds_from_remaining_facts_and_keeps_carrier_observations() {
        use crate::triage::decision::RecordProposal;
        let store = fixture();
        // Guarantee message 2 is the newer explicit observation.
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE messages SET received_at=?1 WHERE id=2",
                [(Utc::now() + Duration::seconds(1)).to_rfc3339()],
            )
            .unwrap();
        let write = |id: i64, status: Option<&str>, trigger: &str| {
            store.enqueue_agent_triage(1, id, trigger, false).unwrap();
            let job = store
                .claim_agent_job(1, "triage", Utc::now(), 60)
                .unwrap()
                .unwrap();
            let context = store.load_agent_context(&job).unwrap();
            let mut verdict = decision(id);
            if let Some(status) = status {
                verdict.records.push(RecordProposal::Delivery {
                    carrier: Some("ups".into()),
                    tracking_number: Some("1Z999AA10123456784".into()),
                    status: status.into(),
                    item_name: None,
                    merchant: None,
                    order_refs: vec![],
                    evidence: vec![],
                });
            }
            store
                .commit_agent_decision(
                    &job,
                    &context,
                    &verdict,
                    std::slice::from_ref(&context.message.source),
                )
                .unwrap();
        };
        write(1, Some("shipped"), "first");
        write(2, Some("delivered"), "second");
        assert_eq!(
            store.external_shipments(1, true).unwrap()[0].status,
            "delivered"
        );
        write(2, None, "retract_newest");
        let rows = store.external_shipments(1, true).unwrap();
        assert_eq!(
            rows.len(),
            1,
            "remaining proposal keeps the polling projection"
        );
        assert_eq!(
            rows[0].status, "shipped",
            "withdrawn delivery status must not persist"
        );
        store
            .apply_carrier_track(
                1,
                rows[0].id,
                &crate::triage::CarrierTrack {
                    status: Some(crate::triage::ShipmentStatus::Delivered),
                    carrier_status_raw: "Delivered at porch".into(),
                    eta: None,
                    delivered_at: Some(Utc::now()),
                },
                Utc::now(),
            )
            .unwrap();
        write(1, Some("ordered"), "older_mail_fact");
        let observed = store.external_shipments(1, true).unwrap();
        assert_eq!(
            observed[0].status, "delivered",
            "mail proposals cannot erase carrier observations"
        );
        assert_eq!(
            observed[0].carrier_status_raw.as_deref(),
            Some("Delivered at porch")
        );
        write(1, None, "retract_last");
        assert!(store.external_shipments(1, true).unwrap().is_empty());
    }

    #[test]
    fn investigation_claims_sent_and_spam_access_work_with_actual_job_kind() {
        let store = fixture();
        store
            .lock()
            .unwrap()
            .execute("UPDATE messages SET is_sent=1 WHERE id=1", [])
            .unwrap();
        store
            .lock()
            .unwrap()
            .execute("UPDATE messages SET is_spam=1 WHERE id=2", [])
            .unwrap();
        store.enqueue_agent_triage(1, 1, "arrival", true).unwrap();
        store.enqueue_agent_triage(1, 2, "arrival", true).unwrap();
        assert!(
            store
                .claim_agent_job(1, "triage", Utc::now(), 60)
                .unwrap()
                .is_none()
        );
        for expected_id in [1, 2] {
            let job = store
                .claim_agent_job(1, "investigation", Utc::now(), 60)
                .unwrap()
                .unwrap();
            assert_eq!(job.message_id, expected_id);
            assert_eq!(job.kind, "access");
            assert!(
                !job.arrival_eligible,
                "sent and spam access checks never push"
            );
            let context = store.load_agent_context(&job).unwrap();
            assert_eq!(
                store
                    .commit_agent_decision(
                        &job,
                        &context,
                        &decision(expected_id),
                        std::slice::from_ref(&context.message.source)
                    )
                    .unwrap(),
                AgentCommitOutcome::Applied
            );
            assert!(store.agent_access_allowed(1, expected_id).unwrap());
        }
    }
    #[test]
    fn budget_deferrals_preserve_retry_capacity_and_refund_only_the_current_lease() {
        let store = fixture();
        let (mut job, _) = claim(&store, 1);
        let mut now = Utc::now();
        for _ in 0..5 {
            assert_eq!(job.attempts, 1, "budget pauses are not model failures");
            now += Duration::days(1);
            assert!(
                store
                    .defer_agent_job(&job, now, "daily_budget_exhausted")
                    .unwrap()
            );
            assert!(
                !store
                    .defer_agent_job(&job, now, "daily_budget_exhausted")
                    .unwrap()
            );
            assert!(
                store
                    .claim_agent_job(1, "triage", now - Duration::seconds(1), 60)
                    .unwrap()
                    .is_none()
            );
            let next = store
                .claim_agent_job(1, "triage", now, 60)
                .unwrap()
                .unwrap();
            assert!(!store.defer_agent_job(&job, now, "stale_worker").unwrap());
            job = next;
        }
        assert_eq!(job.attempts, 1);
        assert!(
            store
                .retry_agent_job(&job, now, "provider_timeout")
                .unwrap()
        );
        job = store
            .claim_agent_job(1, "triage", now, 60)
            .unwrap()
            .unwrap();
        assert_eq!(job.attempts, 2, "a real failure still consumes an attempt");
        assert!(
            store
                .defer_agent_job(&job, now, "daily_budget_exhausted")
                .unwrap()
        );
        job = store
            .claim_agent_job(1, "triage", now, 60)
            .unwrap()
            .unwrap();
        assert_eq!(
            job.attempts, 2,
            "deferral must preserve prior real failures"
        );
    }
    #[test]
    fn cutover_is_push_silent_and_idempotent() {
        let store = fixture();
        assert_eq!(
            store
                .initialize_agent_cutover(1, Utc::now() - Duration::days(30))
                .unwrap(),
            2
        );
        assert_eq!(
            store
                .initialize_agent_cutover(1, Utc::now() - Duration::days(30))
                .unwrap(),
            0
        );
        assert!(
            store
                .claim_agent_job(1, "notification", Utc::now(), 60)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn fetched_source_changes_invalidate_the_whole_transaction() {
        let store = fixture();
        let (job, context) = claim(&store, 1);
        let sources = store.snapshot_agent_sources(1, &[1, 2]).unwrap();
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE messages SET body='changed supporting evidence' WHERE id=2",
                [],
            )
            .unwrap();
        assert_eq!(
            store
                .commit_agent_decision(&job, &context, &decision(1), &sources)
                .unwrap(),
            AgentCommitOutcome::Stale
        );
        assert!(store.agent_records(1, 10).unwrap().is_empty());
    }
    #[test]
    fn successful_commit_durably_queues_the_deliberate_notification() {
        let store = fixture();
        let (job, context) = claim(&store, 1);
        let sources = store.snapshot_agent_sources(1, &[1]).unwrap();
        store
            .commit_agent_decision(&job, &context, &decision(1), &sources)
            .unwrap();
        let followup = store
            .claim_agent_job(1, "deliberate_notification", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert!(followup.arrival_eligible);
        assert!(
            store
                .load_agent_context(&followup)
                .unwrap()
                .previous_decision
                .is_some()
        );
    }
    #[test]
    fn category_correction_survives_without_freezing_attention_resolution() {
        let store = fixture();
        let (job, context) = claim(&store, 1);
        let sources = store.snapshot_agent_sources(1, &[1]).unwrap();
        store
            .commit_agent_decision(&job, &context, &decision(1), &sources)
            .unwrap();
        store
            .correct_agent_triage(1, 1, "kinds", &serde_json::json!(["receipt"]), Utc::now())
            .unwrap();
        let job = store
            .claim_agent_job(1, "triage", Utc::now(), 60)
            .unwrap()
            .unwrap();
        let context = store.load_agent_context(&job).unwrap();
        let mut next = decision(1);
        next.attention.show_in_fye = false;
        store
            .commit_agent_decision(
                &job,
                &context,
                &next,
                &store.snapshot_agent_sources(1, &[1]).unwrap(),
            )
            .unwrap();
        let items = store.agent_records(1, 10).unwrap();
        assert_eq!(items[0].decision.kinds, vec![EmailKind::Receipt]);
        assert!(
            store
                .agent_fye(1, 10, &RankingConfig::default(), Utc::now())
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn unknown_action_ids_roll_back_decision_and_access() {
        let store = fixture();
        let (job, context) = claim(&store, 1);
        let mut proposed = decision(1);
        proposed
            .attention
            .actions
            .push(crate::triage::decision::AttentionAction {
                id: Some("invented".into()),
                ..Default::default()
            });
        assert!(
            store
                .commit_agent_decision(
                    &job,
                    &context,
                    &proposed,
                    &store.snapshot_agent_sources(1, &[1]).unwrap()
                )
                .is_err()
        );
        assert!(!store.agent_access_allowed(1, 1).unwrap());
        assert!(store.agent_reading(1, 10).unwrap().is_empty());
    }
    #[test]
    fn related_resolution_is_atomic_and_preserves_explicit_user_state() {
        use crate::triage::decision::RelatedAttentionUpdate;
        let store = fixture();
        let (first, initial) = claim(&store, 1);
        store
            .commit_agent_decision(
                &first,
                &initial,
                &decision(1),
                &store.snapshot_agent_sources(1, &[1]).unwrap(),
            )
            .unwrap();
        let (job, context) = claim(&store, 2);
        let original = store.agent_thread_context(1, "one").unwrap();
        let mut proposal = decision(2);
        let mut resolved = original.attention.unwrap();
        resolved.show_in_fye = false;
        proposal.related_updates.push(RelatedAttentionUpdate {
            thread_id: "one".into(),
            expected_revision: original.revision.attention_revision,
            attention: resolved,
            evidence: vec![],
        });
        let sources = store.snapshot_agent_sources(1, &[1, 2]).unwrap();
        store.acknowledge_agent_message(1, 1, Utc::now()).unwrap();
        assert_eq!(
            store
                .commit_agent_decision(&job, &context, &proposal, &sources)
                .unwrap(),
            AgentCommitOutcome::Stale
        );
        assert_eq!(store.agent_records(1, 10).unwrap().len(), 1);
        assert!(
            store
                .retry_agent_job(&job, Utc::now(), "stale_snapshot")
                .unwrap()
        );
        let job = store
            .claim_agent_job(1, "triage", Utc::now(), 60)
            .unwrap()
            .unwrap();
        let context = store.load_agent_context(&job).unwrap();
        assert_eq!(
            store
                .commit_agent_decision(
                    &job,
                    &context,
                    &proposal,
                    &store.snapshot_agent_sources(1, &[1, 2]).unwrap()
                )
                .unwrap(),
            AgentCommitOutcome::Applied
        );
        assert_eq!(
            store
                .agent_fye(1, 10, &RankingConfig::default(), Utc::now())
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn preference_edit_keeps_placement_and_queues_nothing() {
        let store = fixture();
        let (job, context) = claim(&store, 1);
        store
            .commit_agent_decision(
                &job,
                &context,
                &decision(1),
                &store.snapshot_agent_sources(1, &[1]).unwrap(),
            )
            .unwrap();
        crate::store::Store::set_sender_rule(
            &store,
            1,
            "sender@test",
            "quiet",
            crate::types::Disposition::Squelch,
        )
        .unwrap();
        assert_eq!(
            store
                .agent_fye(1, 10, &RankingConfig::default(), Utc::now())
                .unwrap()
                .len(),
            1
        );
        let context = store.agent_thread_context(1, "one").unwrap();
        assert_eq!(
            context.rules.len(),
            1,
            "the next investigation reads the rule"
        );
        assert!(
            store
                .claim_agent_job(1, "triage", Utc::now(), 60)
                .unwrap()
                .is_none(),
            "a rule applies to later mail; stored mail is not re-run"
        );
    }
}

/// Human inventory uses current agent decisions, never legacy semantic columns.
/// Pending mail has envelope text and an explicit pending reason.
fn human_inventory(
    conn: &Connection,
    account: AccountId,
    query: &AgentInventoryQuery,
) -> Result<Vec<crate::types::AttentionUpdate>> {
    use crate::store::{SitrepBand, SpamScope};
    query.ranking.validate().map_err(CoreError::InvalidInput)?;
    use crate::types::{AttentionStatus, AttentionUpdate, Tier, Update};
    struct InventoryRow {
        id: i64,
        thread: String,
        sender: String,
        name: Option<String>,
        subject: String,
        snippet: String,
        received: DateTime<Utc>,
        status: String,
        surfaced: Option<DateTime<Utc>>,
        resolved: Option<DateTime<Utc>>,
        reminder: Option<DateTime<Utc>>,
        reminded: Option<DateTime<Utc>>,
        attachments: bool,
        decision: Option<String>,
        attention: Option<String>,
        representative: Option<i64>,
        own: bool,
        /// `auth_fye_threads_cte!` for this row's thread: one of its auth
        /// messages is the owner's to see in FYE (a "Not me", or an unruled
        /// critical change). The representative is then that message.
        auth_fye: bool,
    }
    let mut statement=conn.prepare_cached(concat!(
        "WITH ",auth_fye_threads_cte!(),"
         SELECT m.id,m.thread_id,m.from_addr,m.from_name,m.subject,m.snippet,m.received_at,
             COALESCE(t.status,'new'),t.surfaced_at,t.resolved_at,t.remind_at,t.reminded_at,
             EXISTS(SELECT 1 FROM attachments at WHERE at.account_id=m.account_id AND at.message_id=m.id),
             d.decision_json,a.attention_json,
             COALESCE(q.id,a.message_id),",own_mail_sql!(),",
             q.id IS NOT NULL
         FROM messages m
         LEFT JOIN triage t ON t.account_id=m.account_id AND t.message_id=m.id
         LEFT JOIN agent_message_decisions d ON d.account_id=m.account_id AND d.message_id=m.id
         LEFT JOIN agent_thread_attention a ON a.account_id=m.account_id AND a.thread_id=m.thread_id
         LEFT JOIN auth_fye_threads q ON q.account_id=m.account_id AND q.thread_key=",thread_key_sql!("m"),"
         WHERE m.account_id=?1 AND m.is_sent=0 AND m.is_spam=?2
           AND (m.is_archive=0 OR t.remind_at IS NOT NULL OR t.reminded_at IS NOT NULL)
           AND (m.received_at>=?3 OR t.remind_at IS NOT NULL OR t.reminded_at IS NOT NULL)
           AND (?4 IS NULL OR m.received_at<?4 OR (m.received_at=?4 AND m.id<?5))
         ORDER BY m.received_at DESC,m.id DESC"
    ))?;
    let rows = statement.query_map(
        params![
            account,
            query.spam == SpamScope::Only,
            query.since.to_rfc3339(),
            // The SAME text form ingest stores (`to_rfc3339`), so the
            // keyset compares exactly as the ORDER BY sorts, and the
            // received_at index serves both.
            query.before.map(|(at, _)| at.to_rfc3339()),
            query.before.map(|(_, id)| id).unwrap_or(i64::MIN)
        ],
        |row| {
            Ok(InventoryRow {
                id: row.get(0)?,
                thread: row.get(1)?,
                sender: row.get(2)?,
                name: row.get(3)?,
                subject: row.get(4)?,
                snippet: row.get(5)?,
                received: super::dt(row, 6)?,
                status: row.get(7)?,
                surfaced: super::dt_opt(row, 8)?,
                resolved: super::dt_opt(row, 9)?,
                reminder: super::dt_opt(row, 10)?,
                reminded: super::dt_opt(row, 11)?,
                attachments: row.get(12)?,
                decision: row.get(13)?,
                attention: row.get(14)?,
                representative: row.get(15)?,
                own: row.get(16)?,
                auth_fye: row.get(17)?,
            })
        },
    )?;
    // ITERATED, not collected: with an early limit the statement stops
    // stepping at the page, instead of decoding the whole window first.
    let mut output = Vec::new();
    let mut scores = std::collections::HashMap::new();
    if query.band == Some(SitrepBand::Standing) {
        let mut ranked = list_items(conn, account, "fye", Utc::now())?;
        rank_items(&mut ranked, &query.ranking, Utc::now());
        scores.extend(ranked.iter().map(|item| (item.message_id, item.score)));
    }
    // Only when nothing below re-sorts: then the rows come out in SQL order
    // and the first `limit` produced are the page.
    let early_limit = query
        .limit
        .filter(|_| !query.pending_reminders && query.band != Some(SitrepBand::Standing));
    for row in rows {
        if early_limit.is_some_and(|limit| output.len() >= limit) {
            break;
        }
        let row = row?;
        let decision: Option<MessageDecision> = row.decision.as_deref().map(decode).transpose()?;
        let attention: Option<ThreadAttentionDecision> =
            row.attention.as_deref().map(decode).transpose()?;
        let status = AttentionStatus::parse(&row.status).unwrap_or(AttentionStatus::New);
        if query.status.is_some_and(|expected| expected != status) {
            continue;
        }
        if query.pending_reminders && row.reminder.is_none() {
            continue;
        }
        // Same rule as the `fye` list in `list_items_with_query`: auth mail
        // lives on the Auth page, so the model's FYE pick does not count for
        // it. A thread is picked for its auth mail by `auth_fye_threads_cte!`
        // (a "Not me", or an unruled critical change, anywhere in it), and
        // the human's pin either way still wins. Nothing picks mail the user
        // wrote themselves.
        let wants_fye = attention.as_ref().is_some_and(|a| a.show_in_fye);
        let is_auth = decision.as_ref().is_some_and(|d| d.auth.is_auth());
        let pinned = thread_fye_override(conn, account, &row.thread)?;
        let models_pick = !row.own && (row.auth_fye || (!is_auth && wants_fye));
        let selected = models_pick || (wants_fye && pinned == Some(true));
        let is_standing = ((selected && row.representative == Some(row.id))
            || row.reminded.is_some())
            && pinned != Some(false);
        // The sitrep's own FYE membership, for the placement label: the
        // human's pin either way wins, then the model's pick (auth mail and
        // the user's own excepted), then a fired reminder.
        let in_fye = pinned != Some(false)
            && (pinned == Some(true) || models_pick || row.reminded.is_some());
        if let Some(band) = query.band {
            let visible = match band {
                SitrepBand::Standing => is_standing && status != AttentionStatus::Done,
                SitrepBand::New => status == AttentionStatus::New,
                SitrepBand::Open => status == AttentionStatus::Open,
            };
            if !visible {
                continue;
            }
        }
        let importance = decision
            .as_ref()
            .map(|d| (d.attention.factors.importance.clamp(0.0, 1.0) * 100.0).round() as u8)
            .unwrap_or(0);
        if query
            .min_importance
            .is_some_and(|minimum| importance < minimum)
        {
            continue;
        }
        // Tier is a compatibility label only. It never chooses placement.
        let tier = if selected { Tier::Signal } else { Tier::Noise };
        let summary = decision
            .as_ref()
            .map(|d| d.summary.clone())
            .unwrap_or_else(|| row.subject.clone());
        // Own mail is never triaged, so there is nothing for it to be pending.
        let reason = decision
            .as_ref()
            .map(|d| d.reason.clone())
            .unwrap_or_else(|| {
                if row.own {
                    "You sent this"
                } else {
                    "Triage pending"
                }
                .into()
            });
        let deadline = attention
            .as_ref()
            .and_then(|a| a.factors.attention_at.as_ref())
            .and_then(|time| DateTime::parse_from_rfc3339(&time.value).ok())
            .map(|time| time.with_timezone(&Utc));
        let deadline_date = attention
            .as_ref()
            .and_then(|a| a.factors.attention_at.as_ref())
            .filter(|time| chrono::NaiveDate::parse_from_str(&time.value, "%Y-%m-%d").is_ok())
            .map(|time| time.value.clone());
        let item = AttentionUpdate {
            deadline_date,
            update: Update {
                id: row.id,
                thread_id: row.thread,
                tier,
                importance,
                sender: row.sender,
                one_line: summary,
                reason,
                deadline,
                matched_rule: None,
                field_reasons: None,
                has_attachments: Some(row.attachments),
                from_name: row.name,
                subject: Some(row.subject),
                preview: Some(row.snippet),
            },
            status,
            received_at: Some(row.received),
            // Own mail sits in no zone: a decision left from when it was read
            // as inbound would label it with a page that will not list it.
            placement: placement(decision.as_ref().filter(|_| !row.own), in_fye, is_auth),
            surfaced_at: row.surfaced,
            resolved_at: row.resolved,
            remind_at: row.reminder,
            reminded_at: row.reminded,
        };
        output.push((item, row.received));
    }
    if query.pending_reminders {
        output.sort_by(|(a, ar), (b, br)| a.remind_at.cmp(&b.remind_at).then_with(|| br.cmp(ar)));
    } else if query.band == Some(SitrepBand::Standing) {
        output.sort_by(|(a, ar), (b, br)| {
            scores
                .get(&b.update.id)
                .copied()
                .unwrap_or(0.0)
                .total_cmp(&scores.get(&a.update.id).copied().unwrap_or(0.0))
                .then_with(|| br.cmp(ar))
        });
    }
    Ok(output.into_iter().map(|(item, _)| item).collect())
}

/// The sitrep zone a message lands in, in the order the sitrep itself claims
/// it: the attention band first (it outranks a verdict), then the Auth page,
/// then the record zones, then Reading. Anything else that was triaged is noise.
fn placement(decision: Option<&MessageDecision>, attention: bool, is_auth: bool) -> Option<String> {
    use crate::triage::decision::RecordProposal;
    if attention {
        return Some("eyes".into());
    }
    let decision = decision?;
    let zone = if is_auth {
        "auth"
    } else if let Some(record) = decision.records.first() {
        match record {
            RecordProposal::Event { .. } => "calendar",
            RecordProposal::Delivery { .. } => "shipments",
            RecordProposal::Receipt { .. } => "receipts",
            RecordProposal::Bill { .. } | RecordProposal::FinancialUpdate { .. } => "billing",
        }
    } else if decision.destinations.contains(&MessageDestination::Reading) {
        "reading"
    } else {
        "noise"
    };
    Some(zone.into())
}

#[cfg(test)]
mod inventory_tests {
    use super::*;
    fn query() -> AgentInventoryQuery {
        AgentInventoryQuery {
            before: None,
            limit: None,
            since: Utc::now() - Duration::days(30),
            min_importance: None,
            status: None,
            band: None,
            pending_reminders: false,
            spam: crate::store::SpamScope::Exclude,
            ranking: RankingConfig::default(),
        }
    }
    #[test]
    fn human_inventory_shows_pending_and_restricted_mail_without_legacy_verdicts() {
        let store = tests::fixture();
        store.lock().unwrap().execute("UPDATE triage SET importance=99,tier='deadline',one_line='obsolete' WHERE message_id=1",[]).unwrap();
        store
            .correct_agent_triage(
                1,
                1,
                "external_access",
                &serde_json::json!(true),
                Utc::now(),
            )
            .unwrap();
        let items = store.human_agent_updates(1, &query()).unwrap();
        assert_eq!(items.len(), 2);
        assert!(
            items
                .iter()
                .all(|item| item.update.reason == "Triage pending")
        );
        assert!(items.iter().all(|item| item.update.importance == 0));
        assert!(items.iter().any(|item| item.update.id == 1));
        assert!(store.external_agent_reading(1, 10).unwrap().is_empty());
    }
    #[test]
    fn placement_follows_the_sitreps_zones() {
        use crate::triage::decision::RecordProposal;
        let record = |kind: &str| -> RecordProposal {
            let fields = match kind {
                "event" => serde_json::json!({"kind": "event", "title": "t", "evidence": []}),
                "delivery" => {
                    serde_json::json!({"kind": "delivery", "status": "shipped", "evidence": []})
                }
                "financial_update" => serde_json::json!({
                    "kind": "financial_update", "institution": "i", "description": "d", "evidence": []
                }),
                other => serde_json::json!({"kind": other, "merchant": "m", "evidence": []}),
            };
            serde_json::from_value(fields).unwrap()
        };
        let with = |records: Vec<RecordProposal>, reading: bool| MessageDecision {
            records,
            destinations: if reading {
                vec![MessageDestination::Reading]
            } else {
                vec![]
            },
            ..Default::default()
        };
        let plain = with(vec![], false);
        assert_eq!(placement(None, false, false), None, "untriaged has no zone");
        assert_eq!(placement(None, true, false).as_deref(), Some("eyes"));
        assert_eq!(
            placement(Some(&plain), true, true).as_deref(),
            Some("eyes"),
            "a pin beats auth"
        );
        assert_eq!(
            placement(Some(&plain), false, true).as_deref(),
            Some("auth")
        );
        for (kind, zone) in [
            ("event", "calendar"),
            ("delivery", "shipments"),
            ("receipt", "receipts"),
            ("bill", "billing"),
            ("financial_update", "billing"),
        ] {
            let d = with(vec![record(kind)], true);
            assert_eq!(
                placement(Some(&d), false, false).as_deref(),
                Some(zone),
                "{kind}"
            );
        }
        assert_eq!(
            placement(Some(&with(vec![], true)), false, false).as_deref(),
            Some("reading")
        );
        assert_eq!(
            placement(Some(&plain), false, false).as_deref(),
            Some("noise")
        );
    }

    #[test]
    fn human_inventory_pages_by_keyset_without_gaps_or_repeats() {
        let store = tests::fixture();
        let all = store.human_agent_updates(1, &query()).unwrap();
        assert!(all.len() >= 2, "fixture has at least two messages");
        // One row per page, walking back by (received_at, id).
        let mut walked = Vec::new();
        let mut before = None;
        loop {
            let page = store
                .human_agent_updates(
                    1,
                    &AgentInventoryQuery {
                        before,
                        limit: Some(1),
                        since: DateTime::<Utc>::UNIX_EPOCH,
                        ..query()
                    },
                )
                .unwrap();
            assert!(page.len() <= 1, "the limit caps the page");
            let Some(row) = page.into_iter().next() else {
                break;
            };
            before = Some((row.received_at.unwrap(), row.update.id));
            walked.push(row.update.id);
            assert!(walked.len() <= all.len(), "paging never repeats");
        }
        assert_eq!(walked, all.iter().map(|u| u.update.id).collect::<Vec<_>>());
    }

    #[test]
    fn human_inventory_keeps_date_only_deadline_precision() {
        let store = tests::fixture();
        let (job, context) = tests::claim(&store, 1);
        let mut verdict = tests::decision(1);
        verdict.attention.factors.attention_at = Some(crate::triage::decision::SupportedTime {
            value: "2026-10-05".into(),
            source_message_id: 1,
            ..Default::default()
        });
        store
            .commit_agent_decision(
                &job,
                &context,
                &verdict,
                std::slice::from_ref(&context.message.source),
            )
            .unwrap();
        let item = store
            .human_agent_updates(1, &query())
            .unwrap()
            .into_iter()
            .find(|item| item.update.id == 1)
            .unwrap();
        assert!(item.update.deadline.is_none());
        assert_eq!(item.deadline_date.as_deref(), Some("2026-10-05"));
        // The mail list dates rows by arrival, so every inventory row carries it.
        assert!(item.received_at.is_some());
        // And names the sitrep zone it lands in: this fixture's verdict picks
        // it for attention.
        assert_eq!(item.placement.as_deref(), Some("eyes"));
    }

    #[test]
    fn future_dates_use_stable_ingest_time_for_reading_and_records() {
        let store = tests::fixture();
        let now = Utc::now();
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE messages SET received_at='2099-01-01T00:00:00Z' WHERE id=1",
                [],
            )
            .unwrap();
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE triage SET created_at=?1 WHERE message_id=1",
                [(now - Duration::days(5)).to_rfc3339()],
            )
            .unwrap();
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE triage SET created_at=?1 WHERE message_id=2",
                [now.to_rfc3339()],
            )
            .unwrap();
        for id in [1, 2] {
            let (job, context) = tests::claim(&store, id);
            store
                .commit_agent_decision(
                    &job,
                    &context,
                    &tests::decision(id),
                    std::slice::from_ref(&context.message.source),
                )
                .unwrap();
        }
        assert_eq!(store.agent_reading(1, 10).unwrap()[0].message_id, 2);
        assert_eq!(store.agent_records(1, 10).unwrap()[0].message_id, 2);
        let old = store
            .agent_reading(1, 10)
            .unwrap()
            .into_iter()
            .find(|item| item.message_id == 1)
            .unwrap();
        assert!(DateTime::parse_from_rfc3339(&old.received_at).unwrap() < now - Duration::days(4));
        assert_eq!(
            store.agent_read_message(1, 1).unwrap().received_at,
            "2099-01-01T00:00:00Z",
            "raw source metadata remains intact"
        );
    }

    #[test]
    fn compatibility_standing_uses_agent_membership_and_committed_summary() {
        let store = tests::fixture();
        let (job, context) = tests::claim(&store, 1);
        let decision = tests::decision(1);
        store
            .commit_agent_decision(
                &job,
                &context,
                &decision,
                &store.snapshot_agent_sources(1, &[1]).unwrap(),
            )
            .unwrap();
        store.lock().unwrap().execute("UPDATE triage SET importance=99,tier='deadline',one_line='obsolete' WHERE message_id=2",[]).unwrap();
        let mut query = query();
        query.band = Some(crate::store::SitrepBand::Standing);
        let items = store.human_agent_updates(1, &query).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].update.id, 1);
        assert_eq!(items[0].update.one_line, decision.summary);
    }
    #[test]
    fn explicit_due_reminder_survives_pending_triage_without_external_exposure() {
        let store = tests::fixture();
        let now = Utc::now();
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE triage SET reminded_at=?1 WHERE message_id=1",
                [now.to_rfc3339()],
            )
            .unwrap();
        let items = store
            .agent_fye(1, 10, &RankingConfig::default(), now)
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].message_id, 1);
        assert_eq!(items[0].attention.summary, "Your reminder is due");
        assert!(
            store
                .external_agent_fye(1, 10, &RankingConfig::default(), now)
                .unwrap()
                .is_empty()
        );
        let (_, context) = tests::claim(&store, 1);
        assert!(context.previous_decision.is_none());
    }
    #[test]
    fn external_canonical_records_recheck_all_consumed_sources() {
        let store = tests::fixture();
        let (job, context) = tests::claim(&store, 2);
        store
            .commit_agent_decision(
                &job,
                &context,
                &tests::decision(2),
                &store.snapshot_agent_sources(1, &[2]).unwrap(),
            )
            .unwrap();
        let (job, context) = tests::claim(&store, 1);
        store
            .commit_agent_decision(
                &job,
                &context,
                &tests::decision(1),
                &store.snapshot_agent_sources(1, &[1, 2]).unwrap(),
            )
            .unwrap();
        assert_eq!(store.external_agent_records(1, 10).unwrap().len(), 2);
        store
            .correct_agent_triage(
                1,
                2,
                "external_access",
                &serde_json::json!(true),
                Utc::now(),
            )
            .unwrap();
        assert!(store.external_agent_records(1, 10).unwrap().is_empty());
        assert_eq!(store.agent_records(1, 10).unwrap().len(), 2);
    }
}

#[cfg(test)]
mod dependency_tests {
    use super::tests::{claim, decision, fixture, thread_half_allowed};
    use super::*;
    use crate::triage::decision::{EmailKind, RelatedAttentionUpdate};

    fn commit(store: &SqliteStore, id: i64, sources: &[i64]) {
        let (job, context) = claim(store, id);
        assert_eq!(
            store
                .commit_agent_decision(
                    &job,
                    &context,
                    &decision(id),
                    &store.snapshot_agent_sources(1, sources).unwrap()
                )
                .unwrap(),
            AgentCommitOutcome::Applied
        );
    }

    #[test]
    fn related_attention_keeps_target_identity_and_guards_its_separate_sources() {
        let store = fixture();
        commit(&store, 2, &[2]);
        let (job, context) = claim(&store, 1);
        let target = store.agent_thread_context(1, "two").unwrap();
        let mut proposed = decision(1);
        proposed.summary = "Source classification must not replace target".into();
        proposed.kinds = vec![EmailKind::Bill];
        proposed.external_access.restricted = true;
        let mut attention = decision(2).attention;
        attention.summary = "Related evidence changed this attention".into();
        proposed.related_updates.push(RelatedAttentionUpdate {
            thread_id: "two".into(),
            expected_revision: target.revision.attention_revision,
            attention,
            evidence: vec![],
        });
        let sources = store.snapshot_agent_sources(1, &[1, 2]).unwrap();
        assert_eq!(
            store
                .commit_agent_decision(&job, &context, &proposed, &sources)
                .unwrap(),
            AgentCommitOutcome::Applied
        );
        let items = store
            .agent_fye(1, 10, &RankingConfig::default(), Utc::now())
            .unwrap();
        let target = items.iter().find(|item| item.thread_id == "two").unwrap();
        assert_eq!(target.message_id, 2);
        assert_eq!(target.decision_source_message_id, 2);
        assert_eq!(target.decision.summary, "Summary");
        assert_eq!(target.decision.kinds, vec![EmailKind::Correspondence]);
        assert_eq!(
            target.attention.summary,
            "Related evidence changed this attention"
        );
        assert!(store.agent_access_allowed(1, 2).unwrap());
        assert!(
            store
                .external_agent_fye(1, 10, &RankingConfig::default(), Utc::now())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn changed_evidence_refreshes_transitive_dependents_once_even_with_cycles() {
        let store = fixture();
        // Both decisions consume each other. Source changes must visit each once.
        commit(&store, 2, &[1, 2]);
        commit(&store, 1, &[1, 2]);
        store
            .correct_agent_triage(
                1,
                1,
                "external_access",
                &serde_json::json!(true),
                Utc::now(),
            )
            .unwrap();
        {
            let conn = store.lock().unwrap();
            conn.execute("UPDATE messages SET body='revised' WHERE id=2", [])
                .unwrap();
            enqueue_agent_triage_conn(&conn, 1, 2, "backfill", false).unwrap();
            enqueue_agent_triage_conn(&conn, 1, 2, "backfill", false).unwrap();
            let pending:i64=conn.query_row("SELECT COUNT(*) FROM agent_triage_jobs WHERE account_id=1 AND message_id=1 AND kind='triage' AND state IN('queued','leased')",[],|row|row.get(0)).unwrap();
            assert_eq!(pending, 1);
            let revision: i64 = conn
                .query_row(
                    "SELECT revision FROM agent_message_state WHERE account_id=1 AND message_id=1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                revision, 1,
                "evidence refresh does not pretend target content changed"
            );
        }
        assert!(
            !store.agent_access_allowed(1, 1).unwrap(),
            "human restriction survives evidence refresh"
        );
    }

    #[test]
    fn source_revision_refresh_reaches_indirect_consumers() {
        let store = fixture();
        {
            let conn = store.lock().unwrap();
            conn.execute("UPDATE messages SET account_id=1 WHERE id=3", [])
                .unwrap();
            conn.execute("UPDATE triage SET account_id=1 WHERE message_id=3", [])
                .unwrap();
        }
        commit(&store, 2, &[2]);
        commit(&store, 1, &[1, 2]);
        commit(&store, 3, &[1, 3]);
        assert!(store.agent_access_allowed(1, 3).unwrap());
        {
            let conn = store.lock().unwrap();
            conn.execute("UPDATE messages SET body='source changed' WHERE id=2", [])
                .unwrap();
            enqueue_agent_triage_conn(&conn, 1, 2, "backfill", false).unwrap();
            let queued=conn.prepare("SELECT message_id FROM agent_triage_jobs WHERE kind='triage' AND state='queued' ORDER BY message_id").unwrap().query_map([],|row|row.get::<_,i64>(0)).unwrap().collect::<std::result::Result<Vec<_>,_>>().unwrap();
            assert_eq!(queued, vec![1, 2, 3]);
        }
        assert!(
            !store.agent_access_allowed(1, 3).unwrap(),
            "indirect evidence must remain inaccessible until refreshed"
        );
    }

    #[test]
    fn opening_schema_repairs_old_cross_thread_identity_without_dropping_sources() {
        let store = fixture();
        commit(&store, 1, &[1]);
        commit(&store, 2, &[2]);
        let conn = store.lock().unwrap();
        conn.execute(
            "UPDATE agent_thread_attention SET decision_message_id=1 WHERE thread_id='two'",
            [],
        )
        .unwrap();
        conn.execute(
            "DELETE FROM agent_attention_sources WHERE thread_id='two'",
            [],
        )
        .unwrap();
        conn.execute_batch(include_str!("../schema.sql")).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT decision_message_id FROM agent_thread_attention WHERE thread_id='two'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM agent_attention_sources WHERE thread_id='two' AND source_message_id=1",[],|row|row.get::<_,i64>(0)).unwrap(),1);
        conn.execute_batch(include_str!("../schema.sql")).unwrap();
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM agent_attention_sources WHERE thread_id='two' AND source_message_id=1",[],|row|row.get::<_,i64>(0)).unwrap(),1);
    }

    #[test]
    fn deleted_related_source_does_not_remove_provenance_guard() {
        let store = fixture();
        commit(&store, 2, &[2]);
        let (job, context) = claim(&store, 1);
        let target = store.agent_thread_context(1, "two").unwrap();
        let mut proposed = decision(1);
        proposed.related_updates.push(RelatedAttentionUpdate {
            thread_id: "two".into(),
            expected_revision: target.revision.attention_revision,
            attention: decision(2).attention,
            evidence: vec![],
        });
        store
            .commit_agent_decision(
                &job,
                &context,
                &proposed,
                &store.snapshot_agent_sources(1, &[1, 2]).unwrap(),
            )
            .unwrap();
        assert_eq!(
            store
                .external_agent_fye(1, 10, &RankingConfig::default(), Utc::now())
                .unwrap()
                .len(),
            2
        );
        {
            let conn = store.lock().unwrap();
            conn.execute("DELETE FROM messages WHERE id=1", []).unwrap();
            conn.execute("DELETE FROM agent_message_state WHERE message_id=1", [])
                .unwrap();
            assert!(!thread_half_allowed(&conn, 1, "two"));
        }
        assert_eq!(
            store
                .agent_fye(1, 10, &RankingConfig::default(), Utc::now())
                .unwrap()
                .len(),
            1
        );
        assert!(
            store
                .external_agent_fye(1, 10, &RankingConfig::default(), Utc::now())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn inventories_default_to_recent_unfinished_with_explicit_history_options() {
        let store = fixture();
        commit(&store, 1, &[1]);
        commit(&store, 2, &[2]);
        {
            let conn = store.lock().unwrap();
            conn.execute("UPDATE triage SET status='done' WHERE message_id=1", [])
                .unwrap();
            conn.execute(
                "UPDATE messages SET received_at=?1 WHERE id=2",
                [(Utc::now() - Duration::days(40)).to_rfc3339()],
            )
            .unwrap();
        }
        assert!(store.agent_reading(1, 10).unwrap().is_empty());
        assert!(store.agent_records(1, 10).unwrap().is_empty());
        let all = AgentListQuery {
            since: None,
            include_done: true,
            reading_board: false,
        };
        assert_eq!(
            store.agent_reading_with_query(1, 10, &all).unwrap().len(),
            2
        );
        assert_eq!(
            store.agent_records_with_query(1, 10, &all).unwrap().len(),
            2
        );
        assert_eq!(
            store
                .agent_records_with_query(
                    1,
                    10,
                    &AgentListQuery {
                        since: None,
                        include_done: false,
                        reading_board: false,
                    }
                )
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .agent_reading_with_query(
                    1,
                    10,
                    &AgentListQuery {
                        since: Some(Utc::now() - Duration::days(30)),
                        include_done: true,
                        reading_board: false,
                    }
                )
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn later_model_proposal_cannot_be_emptied_by_persisted_kind_delta() {
        let store = fixture();
        let (job, context) = claim(&store, 1);
        let mut initial = decision(1);
        initial.kinds = vec![EmailKind::Correspondence, EmailKind::Bill];
        store
            .commit_agent_decision(
                &job,
                &context,
                &initial,
                &store.snapshot_agent_sources(1, &[1]).unwrap(),
            )
            .unwrap();
        store
            .correct_agent_triage_delta(1, 1, "kinds", &[], &["bill".into()], Utc::now())
            .unwrap();
        let job = store
            .claim_agent_job(1, "triage", Utc::now(), 60)
            .unwrap()
            .unwrap();
        let context = store.load_agent_context(&job).unwrap();
        let sources = store.snapshot_agent_sources(1, &[1]).unwrap();
        let mut incompatible = decision(1);
        incompatible.kinds = vec![EmailKind::Bill];
        incompatible.summary = "Must not be published".into();
        assert!(matches!(
            store.commit_agent_decision(&job, &context, &incompatible, &sources),
            Err(CoreError::InvalidInput(_))
        ));
        let existing = store.agent_records(1, 10).unwrap().remove(0);
        assert_eq!(existing.decision.kinds, vec![EmailKind::Correspondence]);
        assert_eq!(existing.decision.summary, "Summary");
        assert!(
            leased(&store.lock().unwrap(), &job).unwrap(),
            "rejection preserves the lease for bounded handling"
        );
        assert_eq!(
            store
                .commit_agent_decision(&job, &context, &decision(1), &sources)
                .unwrap(),
            AgentCommitOutcome::Applied
        );
    }

    #[test]
    fn kind_delta_cannot_remove_last_kind_and_rolls_back_intent_and_jobs() {
        let store = fixture();
        commit(&store, 1, &[1]);
        assert!(matches!(
            store.correct_agent_triage_delta(
                1,
                1,
                "kinds",
                &[],
                &["correspondence".into()],
                Utc::now()
            ),
            Err(CoreError::InvalidInput(_))
        ));
        {
            let conn = store.lock().unwrap();
            assert!(corrections(&conn, 1, 1).unwrap().is_empty());
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM agent_triage_jobs WHERE kind='triage' AND state='queued'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
        }
        store
            .correct_agent_triage_delta(
                1,
                1,
                "kinds",
                &["bill".into()],
                &["correspondence".into()],
                Utc::now(),
            )
            .unwrap();
        assert_eq!(
            store.agent_records(1, 10).unwrap()[0].decision.kinds,
            vec![EmailKind::Bill]
        );
    }
}

/// Mail the account itself wrote, sitting in the inbound listing with
/// `is_sent = 0` (see `OWN_MAIL`). The fixture account is `a@test`.
#[cfg(test)]
mod own_mail_tests {
    use super::tests::{claim, decision, fixture};
    use super::*;
    use crate::store::{SitrepBand, SpamScope};

    /// A message the account wrote that the INBOX walk claimed: visible row,
    /// own From. Spelled with stray case and padding because neither side of
    /// the address compare is normalized.
    fn own_inbox_copy(store: &SqliteStore, id: i64, thread: &str) {
        let conn = store.lock().unwrap();
        conn.execute(
            "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body)
             VALUES(?1,1,?2,?3,' A@Test ','subject',?4,'snippet','body')",
            params![id, id.to_string(), thread, (Utc::now() + Duration::seconds(5)).to_rfc3339()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO triage(account_id,message_id,created_at) VALUES(1,?1,?2)",
            params![id, Utc::now().to_rfc3339()],
        )
        .unwrap();
    }
    fn commit(store: &SqliteStore, id: i64, verdict: &MessageDecision) {
        let (job, context) = claim(store, id);
        assert_eq!(job.message_id, id);
        let sources = store
            .snapshot_agent_sources(1, &verdict.attention.relevant_message_ids)
            .unwrap();
        assert_eq!(
            store
                .commit_agent_decision(&job, &context, verdict, &sources)
                .unwrap(),
            AgentCommitOutcome::Applied
        );
    }
    fn fye(store: &SqliteStore) -> Vec<(String, i64)> {
        store
            .agent_fye(1, 10, &RankingConfig::default(), Utc::now())
            .unwrap()
            .into_iter()
            .map(|item| (item.thread_id, item.message_id))
            .collect()
    }
    fn inventory(
        store: &SqliteStore,
        band: Option<SitrepBand>,
    ) -> Vec<crate::types::AttentionUpdate> {
        store
            .human_agent_updates(
                1,
                &AgentInventoryQuery {
                    before: None,
                    limit: None,
                    since: Utc::now() - Duration::days(30),
                    min_importance: None,
                    status: None,
                    band,
                    pending_reminders: false,
                    spam: SpamScope::Exclude,
                    ranking: RankingConfig::default(),
                },
            )
            .unwrap()
    }
    fn jobs(store: &SqliteStore, message: i64) -> Vec<(String, String)> {
        let conn = store.lock().unwrap();
        let mut statement = conn
            .prepare(
                "SELECT kind,trigger FROM agent_triage_jobs
                 WHERE message_id=?1 AND kind IN ('triage','access') ORDER BY id",
            )
            .unwrap();
        statement
            .query_map([message], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    }

    fn count(store: &SqliteStore, sql: &str, message: i64) -> i64 {
        store
            .lock()
            .unwrap()
            .query_row(sql, [message], |row| row.get(0))
            .unwrap()
    }
    fn notification_jobs(store: &SqliteStore, message: i64) -> i64 {
        count(
            store,
            "SELECT count(*) FROM agent_triage_jobs WHERE message_id=?1 AND kind LIKE '%notification'",
            message,
        )
    }
    fn representative(store: &SqliteStore, thread: &str) -> i64 {
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT message_id FROM agent_thread_attention WHERE account_id=1 AND thread_id=?1",
                [thread],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn own_mail_in_the_inbox_is_evidence_not_an_arrival() {
        let store = fixture();
        own_inbox_copy(&store, 4, "one");
        store.enqueue_agent_triage(1, 4, "arrival", true).unwrap();
        // Access review only, with no arrival push, exactly like a row the SENT
        // walk stored. `triage` here is the bug: the model is asked what the
        // user owes on their own message.
        assert_eq!(
            jobs(&store, 4),
            vec![("access".to_string(), "arrival".to_string())]
        );
        assert_eq!(notification_jobs(&store, 4), 0);
        store.enqueue_agent_triage(1, 2, "arrival", true).unwrap();
        assert_eq!(
            notification_jobs(&store, 2),
            1,
            "the same call on inbound mail"
        );
        // The mail list has nothing pending to say about it either.
        let listed = inventory(&store, None);
        let own = listed.iter().find(|item| item.update.id == 4).unwrap();
        assert_eq!(own.update.reason, "You sent this");
        // And like a sent reply it re-opens the inbound mail it answers, never
        // itself: the newest `is_sent = 0` row in the thread IS the own copy.
        assert_eq!(
            jobs(&store, 1),
            vec![("triage".to_string(), "thread_changed:4:1".to_string())]
        );
        // The model is told who wrote it, on the target and in thread context.
        assert!(store.agent_read_message(1, 4).unwrap().is_sent);
        let context = store.agent_thread_context(1, "one").unwrap();
        assert_eq!(
            context.message.id, 1,
            "the inbound message represents the thread"
        );
        assert!(context.thread.iter().any(|m| m.id == 4 && m.is_sent));
        assert!(context.thread.iter().any(|m| m.id == 1 && !m.is_sent));
        // An access review writes no decision, so the tier count has to place
        // the row itself or it reads "pending triage" for as long as it exists.
        // Pending is the two inbound messages queued above, and only them.
        let stats = store.stats(1, Utc::now() - Duration::days(30)).unwrap();
        assert_eq!(stats.tier_counts.get("pending"), Some(&2));
        assert_eq!(stats.tier_counts.get("noise"), Some(&1));
    }

    #[test]
    fn a_thread_is_represented_by_inbound_mail_not_the_users_reply() {
        let store = fixture();
        own_inbox_copy(&store, 4, "one");
        let mut verdict = decision(1);
        verdict.attention.relevant_message_ids = vec![1, 4];
        commit(&store, 1, &verdict);
        // Message 4 is the newer relevant row and used to win on that alone,
        // which put a card in FYE with the user as its sender.
        assert_eq!(fye(&store), vec![("one".to_string(), 1)]);
    }

    #[test]
    fn the_models_pick_never_surfaces_own_mail_but_the_users_does() {
        let store = fixture();
        // A row triaged as inbound before authorship was asked: decision,
        // Reading placement, a receipt, and the thread's FYE seat.
        commit(&store, 1, &decision(1));
        assert_eq!(fye(&store), vec![("one".to_string(), 1)]);
        assert_eq!(store.agent_reading(1, 10).unwrap().len(), 1);
        assert_eq!(store.agent_records(1, 10).unwrap().len(), 1);
        store
            .lock()
            .unwrap()
            .execute("UPDATE messages SET from_addr='a@test' WHERE id=1", [])
            .unwrap();

        assert!(fye(&store).is_empty());
        assert!(store.agent_reading(1, 10).unwrap().is_empty());
        assert!(store.agent_records(1, 10).unwrap().is_empty());
        assert!(inventory(&store, Some(SitrepBand::Standing)).is_empty());
        // Still the user's mail to find in the mail list, just not owed to them.
        let listed = inventory(&store, None);
        let own = listed.iter().find(|item| item.update.id == 1).unwrap();
        assert_eq!(own.update.tier, crate::types::Tier::Noise);
        // Its leftover decision says Reading, a page that will not list it.
        assert_eq!(own.placement, None);

        // A reminder is the user asking to see it again, whoever wrote it.
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE triage SET reminded_at=?1 WHERE message_id=1",
                [Utc::now().to_rfc3339()],
            )
            .unwrap();
        assert_eq!(fye(&store), vec![("one".to_string(), 1)]);
        assert_eq!(inventory(&store, Some(SitrepBand::Standing)).len(), 1);
        store
            .lock()
            .unwrap()
            .execute("UPDATE triage SET reminded_at=NULL WHERE message_id=1", [])
            .unwrap();
        assert!(fye(&store).is_empty());

        // So is pinning the thread.
        store
            .correct_agent_triage(1, 1, "show_in_fye", &serde_json::json!(true), Utc::now())
            .unwrap();
        assert_eq!(fye(&store), vec![("one".to_string(), 1)]);
        assert_eq!(inventory(&store, Some(SitrepBand::Standing)).len(), 1);
    }

    #[test]
    fn own_mail_is_per_account_and_a_blank_address_never_matches() {
        let store = fixture();
        // Account 2 is `b@test`: mail from `a@test` is a stranger's there.
        store
            .lock()
            .unwrap()
            .execute_batch(
                "UPDATE messages SET from_addr='a@test' WHERE id=3;
                 UPDATE messages SET from_addr='' WHERE id=2;
                 UPDATE accounts SET email='' WHERE id=1;",
            )
            .unwrap();
        assert!(!store.agent_read_message(2, 3).unwrap().is_sent);
        assert!(!store.agent_read_message(1, 2).unwrap().is_sent);
    }

    #[test]
    fn upgrade_rereads_fye_threads_that_mistook_own_mail_for_inbound() {
        let store = fixture();
        // Before: message 4 arrives in thread "one", is triaged as inbound and
        // takes the thread's FYE seat. It was the user's all along.
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body)
                 VALUES(4,1,'4','one','sender@test','subject',?1,'snippet','body')",
                [(Utc::now() + Duration::seconds(5)).to_rfc3339()],
            )
            .unwrap();
        commit(&store, 4, &decision(4));
        assert_eq!(fye(&store), vec![("one".to_string(), 4)]);
        store
            .lock()
            .unwrap()
            .execute("UPDATE messages SET from_addr='a@test' WHERE id=4", [])
            .unwrap();
        assert!(fye(&store).is_empty(), "hidden at once, before any re-read");

        for _ in 0..2 {
            let conn = store.lock().unwrap();
            super::super::migrate::heal_own_mail_read_as_inbound(&conn).unwrap();
        }
        // Once, however many times the store is opened.
        assert_eq!(
            jobs(&store, 4),
            vec![
                ("triage".to_string(), "arrival".to_string()),
                ("access".to_string(), "heal".to_string())
            ]
        );
        assert_eq!(
            jobs(&store, 1),
            vec![("triage".to_string(), "thread_changed:4:2".to_string())]
        );
        // Nothing decided about it as inbound mail outlives the refiling.
        for table in [
            "agent_message_decisions",
            "agent_message_destinations",
            "agent_decision_sources",
        ] {
            assert_eq!(
                count(
                    &store,
                    &format!("SELECT count(*) FROM {table} WHERE message_id=?1"),
                    4
                ),
                0,
                "{table}"
            );
        }
        // The re-read lands on the inbound message, and the thread is back.
        let job = store
            .claim_agent_job(1, "triage", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert_eq!(job.message_id, 1);
        let context = store.load_agent_context(&job).unwrap();
        let mut verdict = decision(1);
        verdict.attention.relevant_message_ids = vec![1, 4];
        let sources = store.snapshot_agent_sources(1, &[1, 4]).unwrap();
        assert_eq!(
            store
                .commit_agent_decision(&job, &context, &verdict, &sources)
                .unwrap(),
            AgentCommitOutcome::Applied
        );
        assert_eq!(fye(&store), vec![("one".to_string(), 1)]);

        // On the agent door too, once the access review lands. A source row
        // left at the old revision reads as "changed" there and fails closed
        // for the message, its thread and everything citing it, for good.
        let job = store
            .claim_agent_job(1, "access", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert_eq!(job.message_id, 4);
        let own = store.agent_read_message(1, 4).unwrap();
        let verdict = crate::triage::access::AccessDecision {
            restricted: false,
            reason: "No credential".into(),
        };
        assert_eq!(
            store
                .commit_agent_access(&job, &own, &verdict, &serde_json::json!({}))
                .unwrap(),
            AgentCommitOutcome::Applied
        );
        assert!(store.agent_access_allowed(1, 4).unwrap());
        assert!(store.agent_access_allowed(1, 1).unwrap());
        assert!(store.external_thread_allowed(1, "one").unwrap());
        let external = store
            .external_agent_fye(1, 10, &RankingConfig::default(), Utc::now())
            .unwrap();
        assert_eq!(external.len(), 1);
        assert_eq!(external[0].message_id, 1);
    }

    #[test]
    fn mail_delivered_under_the_users_address_is_inbound_whatever_the_verdict() {
        // Anyone can type a From. A copy the mailbox wrote never crossed the
        // inbound edge and has no verdict; a delivered one has, pass or fail.
        for verdict in [0, 1] {
            let store = fixture();
            own_inbox_copy(&store, 4, "one");
            store
                .lock()
                .unwrap()
                .execute("UPDATE messages SET auth_pass=?1 WHERE id=4", [verdict])
                .unwrap();
            store.enqueue_agent_triage(1, 4, "arrival", true).unwrap();
            assert!(!store.agent_read_message(1, 4).unwrap().is_sent);
            assert_eq!(
                jobs(&store, 4),
                vec![("triage".to_string(), "arrival".to_string())]
            );
            assert_eq!(notification_jobs(&store, 4), 1);
            assert!(
                jobs(&store, 1).is_empty(),
                "it answers nothing on the user's behalf"
            );
            // And it can stand for the thread like any other inbound mail.
            let mut seen = decision(4);
            seen.attention.relevant_message_ids = vec![1, 4];
            let job = store
                .claim_agent_job(1, "investigation", Utc::now(), 60)
                .unwrap()
                .unwrap();
            let context = store.load_agent_context(&job).unwrap();
            let sources = store.snapshot_agent_sources(1, &[1, 4]).unwrap();
            store
                .commit_agent_decision(&job, &context, &seen, &sources)
                .unwrap();
            assert_eq!(fye(&store), vec![("one".to_string(), 4)]);
        }
    }

    #[test]
    fn a_thread_only_the_user_is_relevant_to_still_shows_its_inbound_mail() {
        let store = fixture();
        own_inbox_copy(&store, 4, "one");
        // The model cites the user's reply alone. No inbound message is
        // relevant, so the representative falls back to the thread, where the
        // newer own copy used to outrank the inbound one.
        let mut verdict = decision(1);
        verdict.attention.relevant_message_ids = vec![4];
        commit(&store, 1, &verdict);
        assert_eq!(representative(&store, "one"), 1);
    }

    #[test]
    fn evidence_from_another_thread_does_not_keep_an_own_representative() {
        use crate::triage::decision::RelatedAttentionUpdate;
        let store = fixture();
        // Thread "one" took message 4 as its representative while 4 was being
        // read as inbound.
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body)
                 VALUES(4,1,'4','one','sender@test','subject',?1,'snippet','body')",
                [(Utc::now() + Duration::seconds(5)).to_rfc3339()],
            )
            .unwrap();
        commit(&store, 4, &decision(4));
        assert_eq!(representative(&store, "one"), 4);
        store
            .lock()
            .unwrap()
            .execute("UPDATE messages SET from_addr='a@test' WHERE id=4", [])
            .unwrap();
        // A message in thread "two" updates it. A cross-thread update keeps the
        // target's representative, but only one that is inbound mail.
        let (job, context) = claim(&store, 2);
        assert_eq!(job.message_id, 2);
        let target = store.agent_thread_context(1, "one").unwrap();
        let mut attention = decision(1).attention;
        attention.relevant_message_ids = vec![1, 4];
        let mut verdict = decision(2);
        verdict.related_updates.push(RelatedAttentionUpdate {
            thread_id: "one".into(),
            expected_revision: target.revision.attention_revision,
            attention,
            evidence: vec![],
        });
        let sources = store.snapshot_agent_sources(1, &[1, 2, 4]).unwrap();
        assert_eq!(
            store
                .commit_agent_decision(&job, &context, &verdict, &sources)
                .unwrap(),
            AgentCommitOutcome::Applied
        );
        assert_eq!(representative(&store, "one"), 1);
    }

    #[test]
    fn a_changed_source_refreshes_own_mail_as_an_access_review() {
        let store = fixture();
        // A thread of the user's own, whose attention cites message 1.
        own_inbox_copy(&store, 4, "mine");
        store.enqueue_agent_triage(1, 4, "ingest", false).unwrap();
        store.enqueue_agent_triage(1, 1, "ingest", false).unwrap();
        {
            let conn = store.lock().unwrap();
            conn.execute(
                "INSERT INTO agent_thread_attention(decision_message_id,account_id,thread_id,message_id,
                     show_in_fye,attention_json,relevant_activity)
                 VALUES(4,1,'mine',4,0,'{}',?1)",
                [Utc::now().to_rfc3339()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO agent_attention_sources(account_id,thread_id,source_message_id,source_revision)
                 VALUES(1,'mine',1,1)",
                [],
            )
            .unwrap();
            conn.execute("UPDATE messages SET body='edited' WHERE id=1", [])
                .unwrap();
        }
        store.enqueue_agent_triage(1, 1, "ingest", false).unwrap();
        assert_eq!(
            jobs(&store, 4),
            vec![("access".to_string(), "source_changed:1:2".to_string())]
        );
    }

    #[test]
    fn retriage_redoes_inbound_mail_and_never_the_users_own() {
        let store = fixture();
        own_inbox_copy(&store, 4, "one");
        // A window is the inbound mail in it.
        assert_eq!(store.retriage_reset(1, None, 7).unwrap(), 2);
        assert!(jobs(&store, 4).is_empty());
        assert!(jobs(&store, 1)[0].1.starts_with("manual:"));
        assert!(jobs(&store, 2)[0].1.starts_with("manual:"));

        // Naming the own message asks about the conversation it is in.
        let store = fixture();
        own_inbox_copy(&store, 4, "one");
        assert_eq!(store.retriage_reset(1, Some(4), 7).unwrap(), 1);
        assert!(jobs(&store, 4).is_empty());
        let asked = jobs(&store, 1);
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].0, "triage");
        assert!(asked[0].1.starts_with("manual:"));
        assert_eq!(
            count(
                &store,
                "SELECT count(*) FROM triage WHERE message_id=?1 AND retriage_at IS NOT NULL",
                1
            ),
            1
        );

        // With no inbound mail in the thread there is only itself to review.
        let store = fixture();
        own_inbox_copy(&store, 4, "mine");
        assert_eq!(store.retriage_reset(1, Some(4), 7).unwrap(), 1);
        assert_eq!(jobs(&store, 4)[0].0, "access");
    }

    /// Message 4 arrives in thread "one" as `sender@test`, is triaged as
    /// inbound with `verdict`, and then turns out to be the user's.
    fn triaged_as_inbound_then_own(store: &SqliteStore, verdict: &MessageDecision) {
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body)
                 VALUES(4,1,'4','one','sender@test','subject',?1,'snippet','body')",
                [(Utc::now() + Duration::seconds(5)).to_rfc3339()],
            )
            .unwrap();
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO triage(account_id,message_id,created_at) VALUES(1,4,?1)",
                [Utc::now().to_rfc3339()],
            )
            .unwrap();
        commit(store, 4, verdict);
        store
            .lock()
            .unwrap()
            .execute("UPDATE messages SET from_addr='a@test' WHERE id=4", [])
            .unwrap();
    }

    #[test]
    fn the_upgrade_reread_leaves_resolved_threads_alone() {
        let store = fixture();
        triaged_as_inbound_then_own(&store, &decision(4));
        let heal = |store: &SqliteStore| {
            let conn = store.lock().unwrap();
            super::super::migrate::heal_own_mail_read_as_inbound(&conn).unwrap();
        };
        // Done is thread-wide and nothing done is on screen: a model call
        // here buys nothing.
        store
            .lock()
            .unwrap()
            .execute("UPDATE triage SET status='done' WHERE message_id=4", [])
            .unwrap();
        heal(&store);
        assert_eq!(
            jobs(&store, 4),
            vec![("triage".to_string(), "arrival".to_string())]
        );
        assert!(jobs(&store, 1).is_empty());
        store
            .lock()
            .unwrap()
            .execute("UPDATE triage SET status='open' WHERE message_id=4", [])
            .unwrap();
        heal(&store);
        assert_eq!(jobs(&store, 4).len(), 2);
        assert_eq!(jobs(&store, 1).len(), 1);
    }

    #[test]
    fn own_mail_with_no_thread_answers_nothing() {
        let store = fixture();
        // A blank thread id is no thread: every unthreaded message shares it.
        own_inbox_copy(&store, 4, "");
        store
            .lock()
            .unwrap()
            .execute("UPDATE messages SET thread_id='' WHERE id=2", [])
            .unwrap();
        store.enqueue_agent_triage(1, 4, "arrival", true).unwrap();
        assert_eq!(
            jobs(&store, 4),
            vec![("access".to_string(), "arrival".to_string())]
        );
        assert!(jobs(&store, 2).is_empty());
    }

    #[test]
    fn retiring_a_verdict_takes_its_package_and_survives_one_it_cannot_read() {
        use crate::triage::decision::RecordProposal;
        for unreadable_neighbour in [false, true] {
            let store = fixture();
            let mut shipped = decision(4);
            shipped.records.push(RecordProposal::Delivery {
                carrier: Some("ups".into()),
                tracking_number: Some("1Z999AA10123456784".into()),
                status: "shipped".into(),
                item_name: None,
                merchant: None,
                order_refs: vec![],
                evidence: vec![],
            });
            triaged_as_inbound_then_own(&store, &shipped);
            let packages = |store: &SqliteStore| {
                count(
                    store,
                    "SELECT count(*) FROM shipments WHERE account_id=?1",
                    1,
                )
            };
            assert_eq!(packages(&store), 1);
            if unreadable_neighbour {
                // The rebuild decodes every decision in the account.
                store
                    .lock()
                    .unwrap()
                    .execute(
                        "INSERT INTO agent_message_decisions VALUES(1,1,1,'{\"kinds\":[\"no such kind\"]}',?1)",
                        [Utc::now().to_rfc3339()],
                    )
                    .unwrap();
            }
            // The enqueue is an ingest transaction or the store's open, and
            // must come through either way.
            store.enqueue_agent_triage(1, 4, "backfill", false).unwrap();
            assert_eq!(
                count(
                    &store,
                    "SELECT count(*) FROM agent_message_decisions WHERE message_id=?1",
                    4
                ),
                0
            );
            assert_eq!(packages(&store), i64::from(unreadable_neighbour));
        }
    }
}

/// The agent's `done` (`apply_done`): applied only on a scheduled re-check,
/// only past every rail, as the owner's thread-wide Done, with a ledger row
/// per message it closed; refused, the
/// decision still commits and nothing else moves.
#[cfg(test)]
mod auto_done_tests {
    use super::tests::{claim, decision};
    use super::*;
    use crate::triage::decision::{DoneRequest, EvidenceRef, RevisitRequest};

    /// The shared fixture with every message from one AUTHENTICATED sender,
    /// so a done may rest on its words (`foreign_words`).
    fn fixture() -> SqliteStore {
        let store = super::tests::fixture();
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE messages SET from_addr='sender@venue.example',auth_pass=1",
                [],
            )
            .unwrap();
        store
    }

    fn done_verdict(id: i64, reason: &str) -> MessageDecision {
        MessageDecision {
            done: Some(DoneRequest {
                reason: reason.into(),
                evidence: vec![EvidenceRef {
                    message_id: id,
                    location: "body".into(),
                }],
            }),
            ..decision(id)
        }
    }
    /// The message's first decision (no done), then a queued `revisit:` job
    /// for it, the way `schedule_revisit` writes one, claimed.
    fn claim_revisit(store: &SqliteStore, id: i64) -> (AgentJob, AgentContext) {
        claim_revisit_after(store, id, "SELECT 1")
    }
    /// [`claim_revisit`], running `setup` between the first decision and the
    /// re-check's claim, so the claimed context already reads it.
    fn claim_revisit_after(store: &SqliteStore, id: i64, setup: &str) -> (AgentJob, AgentContext) {
        commit(store, claim(store, id), &decision(id));
        store.lock().unwrap().execute_batch(setup).unwrap();
        {
            let conn = store.lock().unwrap();
            conn.execute(
                "INSERT INTO agent_triage_jobs(account_id,message_id,kind,trigger,input_revision,arrival_eligible,available_at)
                 SELECT 1,?1,'triage','revisit:2026-10-20T00:00:00+00:00',revision,0,?2
                 FROM agent_message_state WHERE account_id=1 AND message_id=?1",
                params![id, (Utc::now() - Duration::seconds(1)).to_rfc3339()],
            )
            .unwrap();
        }
        let job = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert_eq!(
            (job.message_id, job.trigger.starts_with("revisit:")),
            (id, true)
        );
        let context = store.load_agent_context(&job).unwrap();
        (job, context)
    }
    fn commit(
        store: &SqliteStore,
        (job, context): (AgentJob, AgentContext),
        verdict: &MessageDecision,
    ) -> AgentJob {
        let sources = store.snapshot_agent_sources(1, &[job.message_id]).unwrap();
        assert_eq!(
            store
                .commit_agent_decision(&job, &context, verdict, &sources)
                .unwrap(),
            AgentCommitOutcome::Applied
        );
        job
    }
    fn status(store: &SqliteStore, id: i64) -> (String, Option<String>) {
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT status,resolved_at FROM triage WHERE account_id=1 AND message_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }
    fn ledger(store: &SqliteStore) -> Vec<(i64, i64, String, String, String)> {
        let conn = store.lock().unwrap();
        let mut statement = conn
            .prepare(
                "SELECT account_id,message_id,source,reason,done_at FROM auto_done
                 ORDER BY account_id,message_id",
            )
            .unwrap();
        statement
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }
    fn stored_done(store: &SqliteStore, id: i64) -> Option<DoneRequest> {
        let raw: String = store
            .lock()
            .unwrap()
            .query_row(
                "SELECT decision_json FROM agent_message_decisions WHERE account_id=1 AND message_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        serde_json::from_str::<MessageDecision>(&raw).unwrap().done
    }
    fn revisits_queued(store: &SqliteStore, id: i64) -> i64 {
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM agent_triage_jobs WHERE account_id=1 AND message_id=?1
                 AND trigger LIKE 'revisit:%' AND state='queued'",
                [id],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// A sibling in thread `one` from another address of the same
    /// authenticated organisation (which does not block), with a triage row
    /// in `status`.
    fn sibling(store: &SqliteStore, id: i64, status: &str) {
        let conn = store.lock().unwrap();
        conn.execute(
            "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body,auth_pass)
             VALUES(?1,1,?2,'one','desk@venue.example','subject',?3,'snippet','body',1)",
            params![id, id.to_string(), (Utc::now() + Duration::seconds(id)).to_rfc3339()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO triage(account_id,message_id,created_at,status,resolved_at)
             VALUES(1,?1,?2,?3,CASE WHEN ?3='done' THEN '2026-01-02T00:00:00+00:00' END)",
            params![id, Utc::now().to_rfc3339(), status],
        )
        .unwrap();
    }
    /// Every triage row of account 1, in id order: what a refused done must
    /// leave exactly as it was.
    fn all_status(store: &SqliteStore) -> Vec<(i64, String, Option<String>, Option<String>)> {
        let conn = store.lock().unwrap();
        let mut statement = conn
            .prepare(
                "SELECT message_id,status,resolved_at,reminded_at FROM triage
                 WHERE account_id=1 ORDER BY message_id",
            )
            .unwrap();
        statement
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }

    /// The whole path: a re-check's done is the owner's Done, so it closes
    /// the THREAD: the re-checked message and its `new` sibling, each with
    /// `resolved_at` equal to its ledger row's `done_at` and the model's
    /// reason capped. A sibling already closed is left exactly as it was:
    /// one the owner closed keeps the time they closed it, and one smart
    /// clear closed keeps its batch (`resolved_by`), so that batch's undo
    /// still reaches it; neither gets a ledger row. Another thread is
    /// untouched, and a revisit set beside the done
    /// is not queued: a closed message needs no further re-check.
    #[test]
    fn a_revisit_done_closes_the_thread_with_a_ledger_row_per_closed_message() {
        let store = fixture();
        sibling(&store, 4, "new");
        sibling(&store, 6, "done");
        sibling(&store, 7, "done");
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE triage SET resolved_by='smart-batch' WHERE message_id=7",
                [],
            )
            .unwrap();
        let before = |id: i64| -> (String, Option<String>, Option<String>) {
            store
                .lock()
                .unwrap()
                .query_row(
                    "SELECT status,resolved_at,resolved_by FROM triage WHERE account_id=1 AND message_id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap()
        };
        let closed_before = (before(6), before(7));
        let reason = format!("The concert was last night. {}", "x".repeat(500));
        let mut verdict = done_verdict(1, &reason);
        verdict.revisit = Some(RevisitRequest {
            at: Utc::now() + Duration::days(2),
            reason: "check again".into(),
        });
        let (job, context) = claim_revisit(&store, 1);
        commit(&store, (job, context), &verdict);
        let rows = ledger(&store);
        assert_eq!(
            rows.iter().map(|row| (row.0, row.1)).collect::<Vec<_>>(),
            [(1, 1), (1, 4)],
            "a ledger row for each message the done moved from new"
        );
        for (account, message, source, kept, done_at) in rows {
            assert_eq!((account, source.as_str()), (1, "agent_revisit"));
            assert_eq!(kept.chars().count(), AUTO_DONE_REASON_CHARS);
            assert!(reason.starts_with(&kept));
            assert_eq!(
                status(&store, message),
                ("done".into(), Some(done_at)),
                "{message}: resolved_at is the ledger's done_at"
            );
        }
        assert_eq!(
            (before(6), before(7)),
            closed_before,
            "closed siblings untouched"
        );
        assert_eq!(status(&store, 2), ("new".into(), None), "another thread");
        assert_eq!(revisits_queued(&store, 1), 0, "no revisit after a done");
        assert!(stored_done(&store, 1).is_some());
    }

    /// THE SAME WRITE AS THE OWNER'S DONE: the same thread closed once by the
    /// owner's Done and once by the agent's leaves every triage row of the
    /// account in the same state (status, resolved or not, fired reminder),
    /// the already-done sibling and the other thread included.
    #[test]
    fn an_auto_done_writes_what_the_owners_done_writes() {
        let closed = |by_agent: bool| {
            let store = fixture();
            sibling(&store, 4, "new");
            sibling(&store, 6, "done");
            if by_agent {
                let (job, context) = claim_revisit(&store, 1);
                commit(&store, (job, context), &done_verdict(1, "over"));
            } else {
                commit(&store, claim(&store, 1), &decision(1));
                store
                    .set_attention_status(1, 1, crate::types::AttentionStatus::Done)
                    .unwrap();
            }
            all_status(&store)
                .into_iter()
                .map(|(id, state, resolved, reminded)| (id, state, resolved.is_some(), reminded))
                .collect::<Vec<_>>()
        };
        let agent = closed(true);
        assert_eq!(agent, closed(false));
        assert_eq!(
            agent.iter().filter(|row| row.1 == "done").count(),
            3,
            "the thread"
        );
    }

    /// FYE READS THE THREAD BY ITS REPRESENTATIVE, here the newer sibling 4,
    /// not the re-checked message 1. After the agent's done the thread is out
    /// of FYE; the Mac's undo (`setStatus(id, .open)` for the ONE row it
    /// showed, message-scoped) brings it back; and a second re-check cannot
    /// close it again.
    #[test]
    fn an_auto_done_thread_leaves_fye_and_the_undo_brings_it_back() {
        let store = fixture();
        sibling(&store, 4, "new");
        let commit_thread = |(job, context): (AgentJob, AgentContext),
                             verdict: &MessageDecision| {
            let sources = store.snapshot_agent_sources(1, &[1, 4]).unwrap();
            assert_eq!(
                store
                    .commit_agent_decision(&job, &context, verdict, &sources)
                    .unwrap(),
                AgentCommitOutcome::Applied
            );
            job
        };
        let fye = || {
            store
                .agent_fye(1, 10, &RankingConfig::default(), Utc::now())
                .unwrap()
                .into_iter()
                .map(|item| (item.thread_id, item.message_id))
                .collect::<Vec<_>>()
        };
        let mut first = decision(1);
        first.attention.relevant_message_ids = vec![1, 4];
        commit_thread(claim(&store, 1), &first);
        assert!(
            fye().contains(&("one".to_string(), 4)),
            "the newer sibling represents the thread"
        );
        let insert_revisit = |at: &str| {
            store
                .lock()
                .unwrap()
                .execute(
                    "INSERT INTO agent_triage_jobs(account_id,message_id,kind,trigger,input_revision,arrival_eligible,available_at)
                     SELECT 1,1,'triage','revisit:'||?2,revision,0,?1
                     FROM agent_message_state WHERE account_id=1 AND message_id=1",
                    params![(Utc::now() - Duration::seconds(1)).to_rfc3339(), at],
                )
                .unwrap();
            let job = store
                .claim_agent_job(1, "investigation", Utc::now(), 60)
                .unwrap()
                .unwrap();
            assert_eq!(job.message_id, 1);
            let context = store.load_agent_context(&job).unwrap();
            (job, context)
        };
        // The model's natural answer: done, and nothing for FYE. The FYE flag
        // is not the done's to write, or the undo below would bring back a
        // thread that is still flagged hidden.
        let mut verdict = done_verdict(1, "over");
        verdict.attention.relevant_message_ids = vec![1, 4];
        verdict.attention.show_in_fye = false;
        commit_thread(insert_revisit("2026-10-20T00:00:00+00:00"), &verdict);
        assert!(
            !fye().iter().any(|(thread, _)| thread == "one"),
            "the thread is out of FYE"
        );
        assert!(
            store
                .set_attention_status(1, 4, crate::types::AttentionStatus::Open)
                .unwrap()
        );
        assert!(
            fye().contains(&("one".to_string(), 4)),
            "the undo brings it back"
        );
        let job = commit_thread(insert_revisit("2026-10-21T00:00:00+00:00"), &verdict);
        assert!(fye().contains(&("one".to_string(), 4)), "never twice");
        let conn = store.lock().unwrap();
        assert_eq!(
            apply_done(&conn, &job, &verdict, Utc::now()).unwrap(),
            Some(DoneOutcome::Refused("already_done"))
        );
        // The undo reopened 4, and an `open` row may close; the ledger is
        // what keeps the agent's own undone done undone.
        let reopened = AgentJob {
            message_id: 4,
            ..job.clone()
        };
        assert_eq!(
            apply_done(&conn, &reopened, &verdict, Utc::now()).unwrap(),
            Some(DoneOutcome::Refused("auto_done_before"))
        );
    }

    /// The rule suggester reads "cleared without opening" as the owner not
    /// wanting a sender's mail, so a done the daemon made reads as `new` in
    /// its digest, until the owner touches the row (its `resolved_at` is no
    /// longer the ledger's `done_at`).
    #[test]
    fn the_rule_digest_reads_an_agent_done_as_new() {
        let store = fixture();
        let (job, context) = claim_revisit(&store, 1);
        commit(&store, (job, context), &done_verdict(1, "over"));
        let digest = |store: &SqliteStore| {
            store
                .sender_rule_digest(1, "sender@venue.example", 10)
                .unwrap()
                .into_iter()
                .map(|row| row.status)
                .collect::<Vec<_>>()
        };
        assert_eq!(status(&store, 1).0, "done");
        assert_eq!(digest(&store), ["new", "new"]);
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE triage SET resolved_at='2026-01-02T00:00:00+00:00' WHERE message_id=1",
                [],
            )
            .unwrap();
        assert!(digest(&store).contains(&"done".to_string()));
    }

    /// The same answer on any other run is recorded in the decision and
    /// changes nothing else; its revisit still queues.
    #[test]
    fn a_done_outside_a_revisit_commits_but_closes_nothing() {
        let store = fixture();
        let mut verdict = done_verdict(1, "over");
        verdict.revisit = Some(RevisitRequest {
            at: Utc::now() + Duration::days(2),
            reason: "check again".into(),
        });
        let job = commit(&store, claim(&store, 1), &verdict);
        assert_eq!(job.trigger, "arrival");
        assert_eq!(status(&store, 1), ("new".into(), None));
        assert!(ledger(&store).is_empty());
        assert!(
            stored_done(&store, 1).is_some(),
            "the decision still commits"
        );
        assert_eq!(revisits_queued(&store, 1), 1);
        let conn = store.lock().unwrap();
        assert_eq!(
            apply_done(&conn, &job, &verdict, Utc::now()).unwrap(),
            Some(DoneOutcome::Refused("not_revisit"))
        );
    }

    /// Each rail ALONE keeps the message, on the message itself and on a
    /// sibling the thread-wide done would write: every case starts from the
    /// fixture that
    /// `a_revisit_done_closes_the_thread_with_a_ledger_row_per_closed_message`
    /// closes and changes one thing, set after the first decision (which
    /// would otherwise be the one to read it). The status, `resolved_at` and
    /// ledger are untouched, the decision still commits, and the refusal
    /// names that rail, so a rail that went missing would show as another
    /// rail's code or as a done.
    #[test]
    fn each_rail_alone_keeps_the_message() {
        // A sibling in the thread, from the same organisation, that would be
        // written by the thread-wide done: each rail holds for it too.
        const SIB: &str = "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body,auth_pass,is_spam,is_archive)
             VALUES(5,1,'5','one','desk@venue.example','re: subject','2026-01-02T00:00:00+00:00','snippet','body',1,
                    0,0);
             INSERT INTO triage(account_id,message_id,created_at) VALUES(1,5,'2026-01-02T00:00:00+00:00');";
        let sib = |then: &str| format!("{SIB}{then}");
        let sibling_cases = [
            (
                "a sibling's reminder pending",
                sib(
                    "UPDATE triage SET status='done',remind_at='2099-01-01T00:00:00+00:00' WHERE message_id=5",
                ),
                "reminder",
            ),
            (
                "a sibling's reminder fired",
                sib("UPDATE triage SET reminded_at='2026-01-02T00:00:00+00:00' WHERE message_id=5"),
                "reminder",
            ),
            (
                "a sibling in spam",
                sib("UPDATE messages SET is_spam=1 WHERE id=5"),
                "sent_spam_or_archive",
            ),
            (
                "a sibling in the archive",
                sib("UPDATE messages SET is_archive=1 WHERE id=5"),
                "sent_spam_or_archive",
            ),
            (
                "a sibling auto-done before",
                sib("UPDATE triage SET status='done' WHERE message_id=5;
                     INSERT INTO auto_done(account_id,message_id,source,reason,done_at)
                     VALUES(1,5,'agent_revisit','earlier','2026-01-02T00:00:00+00:00')"),
                "auto_done_before",
            ),
        ];
        let cases: [(&str, &str, &str); 12] = [
            (
                "already done",
                "UPDATE triage SET status='done',resolved_at='2026-01-02T00:00:00+00:00' WHERE message_id=1",
                "already_done",
            ),
            (
                "reminder pending",
                "UPDATE triage SET remind_at='2099-01-01T00:00:00+00:00' WHERE message_id=1",
                "reminder",
            ),
            (
                "reminder fired",
                "UPDATE triage SET reminded_at='2026-01-02T00:00:00+00:00' WHERE message_id=1",
                "reminder",
            ),
            ("sent", "UPDATE messages SET is_sent=1 WHERE id=1", "sent_spam_or_archive"),
            ("spam", "UPDATE messages SET is_spam=1 WHERE id=1", "sent_spam_or_archive"),
            ("archive", "UPDATE messages SET is_archive=1 WHERE id=1", "sent_spam_or_archive"),
            (
                "auto-done before, then undone",
                "INSERT INTO auto_done(account_id,message_id,source,reason,done_at)
                 VALUES(1,1,'agent_revisit','earlier','2026-01-02T00:00:00+00:00')",
                "auto_done_before",
            ),
            (
                "the owner wrote in the thread",
                "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body)
                 VALUES(5,1,'5','one','a@test','re: subject','2026-01-02T00:00:00+00:00','snippet','body')",
                "own_mail_in_thread",
            ),
            (
                "a stranger replied in the thread",
                "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body,auth_pass)
                 VALUES(5,1,'5','one','someone@elsewhere.example','re: subject','2026-01-02T00:00:00+00:00','snippet','body',1)",
                "foreign_sender_in_thread",
            ),
            (
                "an unauthenticated reply in the thread, from the sender's own address",
                "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body,auth_pass)
                 VALUES(5,1,'5','one','sender@venue.example','re: subject','2026-01-02T00:00:00+00:00','snippet','body',0)",
                "foreign_sender_in_thread",
            ),
            (
                "the message itself is not authenticated",
                "UPDATE messages SET auth_pass=0 WHERE id=1",
                "sender_unauthenticated",
            ),
            (
                "another address on a shared domain",
                "UPDATE messages SET from_addr='alice@gmail.com' WHERE id=1;
                 INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body,auth_pass)
                 VALUES(5,1,'5','one','bob@gmail.com','re: subject','2026-01-02T00:00:00+00:00','snippet','body',1)",
                "foreign_sender_in_thread",
            ),
        ];
        let cases = cases
            .into_iter()
            .map(|(name, setup, code)| (name, setup.to_string(), code))
            .chain(sibling_cases);
        for (name, setup, code) in cases {
            let store = fixture();
            let (job, context) = claim_revisit_after(&store, 1, &setup);
            let before = all_status(&store);
            let ledger_before = ledger(&store);
            let verdict = done_verdict(1, "over");
            commit(&store, (job.clone(), context), &verdict);
            assert_eq!(all_status(&store), before, "{name}: status moved");
            assert_eq!(ledger(&store), ledger_before, "{name}: ledger moved");
            assert!(stored_done(&store, 1).is_some(), "{name}: decision lost");
            let conn = store.lock().unwrap();
            assert_eq!(
                apply_done(&conn, &job, &verdict, Utc::now()).unwrap(),
                Some(DoneOutcome::Refused(code)),
                "{name}"
            );
        }
    }

    /// Reopened mail still closes. The owner may pull a flight email back
    /// out of done to keep it handy before the flight (`open`), and after the
    /// flight they still do not want it: the re-checked message and an
    /// `open` sibling both close, each with its ledger row.
    #[test]
    fn reopened_mail_still_closes_after_the_event() {
        let store = fixture();
        sibling(&store, 4, "open");
        let (job, context) = claim_revisit_after(
            &store,
            1,
            "UPDATE triage SET status='open' WHERE message_id=1",
        );
        assert_eq!(status(&store, 1).0, "open");
        commit(&store, (job, context), &done_verdict(1, "over"));
        assert_eq!(
            (status(&store, 1).0, status(&store, 4).0),
            ("done".into(), "done".into())
        );
        assert_eq!(
            ledger(&store).iter().map(|row| row.1).collect::<Vec<_>>(),
            vec![1, 4]
        );
    }

    /// A RE-CHECK MOVES MAIL ONLY BY ITS DONE. Every re-check answer here
    /// says hide: thread `one` out of FYE, no Reading destination, and a
    /// related update hiding thread `two`. On a re-check none of it is
    /// written, whether the done applies, a rail refuses it, or none is
    /// asked: done and leaving FYE are one act, and an applied done leaves
    /// the FYE flag as the owner's Done would. An ordinary run is not held:
    /// the model still places mail freely.
    #[test]
    fn a_recheck_writes_no_placement() {
        use crate::triage::decision::RelatedAttentionUpdate;
        let shown = |store: &SqliteStore, thread: &str| -> bool {
            store
                .lock()
                .unwrap()
                .query_row(
                    "SELECT json_extract(attention_json,'$.show_in_fye') FROM agent_thread_attention
                     WHERE account_id=1 AND thread_id=?1",
                    [thread],
                    |r| r.get(0),
                )
                .unwrap()
        };
        let reading = |store: &SqliteStore| -> bool {
            store
                .lock()
                .unwrap()
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM agent_message_destinations
                     WHERE account_id=1 AND message_id=1 AND destination='reading')",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        };
        let own_reply = "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body)
             VALUES(5,1,'5','one','a@test','re: subject','2026-01-02T00:00:00+00:00','snippet','body')";
        const RECHECK: &str = "revisit:2026-10-20T00:00:00+00:00";
        // (name, trigger, setup, asks for done, closes, placement kept)
        for (name, trigger, setup, asks_done, closes, kept) in [
            ("applied done", RECHECK, "SELECT 1", true, true, true),
            ("refused done", RECHECK, own_reply, true, false, true),
            ("re-check, no done", RECHECK, "SELECT 1", false, false, true),
            ("ordinary run", "retriage", "SELECT 1", false, false, false),
        ] {
            let store = fixture();
            let mut first = decision(1);
            first.destinations = vec![MessageDestination::Reading];
            commit(&store, claim(&store, 1), &first);
            commit(&store, claim(&store, 2), &decision(2));
            assert!(
                shown(&store, "one") && shown(&store, "two") && reading(&store),
                "{name}: starts placed"
            );
            {
                let conn = store.lock().unwrap();
                conn.execute_batch(setup).unwrap();
                conn.execute(
                    "INSERT INTO agent_triage_jobs(account_id,message_id,kind,trigger,input_revision,arrival_eligible,available_at)
                     SELECT 1,1,'triage',?1,revision,0,?2
                     FROM agent_message_state WHERE account_id=1 AND message_id=1",
                    params![trigger, (Utc::now() - Duration::seconds(1)).to_rfc3339()],
                )
                .unwrap();
            }
            let job = store
                .claim_agent_job(1, "investigation", Utc::now(), 60)
                .unwrap()
                .unwrap();
            assert_eq!((job.message_id, job.trigger.as_str()), (1, trigger));
            let context = store.load_agent_context(&job).unwrap();
            let mut verdict = if asks_done {
                done_verdict(1, "over")
            } else {
                decision(1)
            };
            verdict.attention.show_in_fye = false;
            verdict.destinations = vec![];
            let two = store.agent_thread_context(1, "two").unwrap();
            let mut hide_two = two.attention.unwrap();
            hide_two.show_in_fye = false;
            verdict.related_updates.push(RelatedAttentionUpdate {
                thread_id: "two".into(),
                expected_revision: two.revision.attention_revision,
                attention: hide_two,
                evidence: vec![],
            });
            let sources = store.snapshot_agent_sources(1, &[1, 2]).unwrap();
            assert_eq!(
                store
                    .commit_agent_decision(&job, &context, &verdict, &sources)
                    .unwrap(),
                AgentCommitOutcome::Applied,
                "{name}"
            );
            assert_eq!(status(&store, 1).0 == "done", closes, "{name}: closed");
            assert_eq!(shown(&store, "one"), kept, "{name}: own thread");
            assert_eq!(shown(&store, "two"), kept, "{name}: related thread");
            assert_eq!(reading(&store), kept, "{name}: reading");
        }
    }

    /// Own mail is read on the message too, not only its thread: a message
    /// from the owner's own address with no thread is kept. And `''` is no
    /// thread: other threadless own mail does not keep a threadless message.
    #[test]
    fn own_mail_reads_the_message_itself_and_no_thread_joins_nothing() {
        for (own_from, closed) in [(true, false), (false, true)] {
            let store = fixture();
            {
                let conn = store.lock().unwrap();
                conn.execute("UPDATE messages SET thread_id='' WHERE id IN (1,2)", [])
                    .unwrap();
                conn.execute(
                    "UPDATE messages SET from_addr='a@test',auth_pass=NULL WHERE id=?1",
                    [if own_from { 1 } else { 2 }],
                )
                .unwrap();
            }
            let (job, context) = claim_revisit(&store, 1);
            commit(&store, (job, context), &done_verdict(1, "over"));
            assert_eq!(status(&store, 1).0 == "done", closed, "own_from={own_from}");
            assert_eq!(ledger(&store).len(), usize::from(closed));
        }
    }

    /// Accounts never cross: another tenant's ledger row for the same
    /// message id, or its mail in a same-named thread, keeps nothing and is
    /// never written.
    #[test]
    fn rails_read_only_this_account() {
        let store = fixture();
        {
            let conn = store.lock().unwrap();
            conn.execute(
                "INSERT INTO auto_done(account_id,message_id,source,reason,done_at)
                 VALUES(2,1,'agent_revisit','theirs','2026-01-02T00:00:00+00:00')",
                [],
            )
            .unwrap();
            conn.execute(
                "UPDATE messages SET thread_id='one',from_addr='b@test' WHERE id=3",
                [],
            )
            .unwrap();
        }
        let (job, context) = claim_revisit(&store, 1);
        commit(&store, (job, context), &done_verdict(1, "over"));
        assert_eq!(status(&store, 1).0, "done");
        // Nor does the thread-wide write reach the other tenant's message in
        // the same-named thread, or its ledger.
        let rows = ledger(&store);
        assert_eq!(
            rows.iter().map(|row| (row.0, row.1)).collect::<Vec<_>>(),
            [(1, 1), (2, 1)]
        );
        let theirs: String = store
            .lock()
            .unwrap()
            .query_row(
                "SELECT status FROM triage WHERE account_id=2 AND message_id=3",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(theirs, "new");
    }
}

/// The Reading board's window starts at the owner's midnight
/// (`reading_board_since`); the rest of the board is pinned through the API
/// (`squelch-api/tests/client_triage_v2.rs`).
#[cfg(test)]
mod reading_board_tests {
    use super::*;

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    /// 05:00 UTC on the 3rd is the evening of the 2nd in Los Angeles, so the
    /// week counts back from the 2nd: the 27th, across the end of daylight
    /// time (1 November), so that midnight is 07:00 UTC, not 08:00.
    #[test]
    fn the_week_begins_at_the_owners_midnight() {
        let now = at("2026-11-03T05:00:00Z");
        assert_eq!(
            reading_board_since(now, chrono_tz::America::Los_Angeles),
            at("2026-10-27T07:00:00Z")
        );
        assert_eq!(
            reading_board_since(now, chrono_tz::Tz::UTC),
            at("2026-10-28T00:00:00Z")
        );
    }

    /// Havana skips its midnight when clocks go forward (8 March 2026): that
    /// day begins at the first instant there is, 01:00 local.
    #[test]
    fn a_skipped_midnight_starts_the_day_an_hour_late() {
        let now = at("2026-03-14T17:00:00Z");
        assert_eq!(
            reading_board_since(now, chrono_tz::America::Havana),
            at("2026-03-08T05:00:00Z")
        );
    }
}

/// The first-load allowance (`FIRST_LOAD_FYE_CAP`): a new account's backfill
/// fills at most that many FYE places, by rank; pressing mail skips it; the
/// rest is held and never refills the board; live mail is never counted.
#[cfg(test)]
mod first_load_cap_tests {
    use super::tests::decision;
    use super::*;
    use crate::triage::decision::{AttentionState, SupportedTime};

    const BACKLOG: i64 = 20;

    /// One account, `BACKLOG + 3` single-message threads `t1..`, all
    /// received now and all brought in by the first-run backfill.
    fn fixture() -> SqliteStore {
        let store = SqliteStore::open_in_memory().unwrap();
        {
            let conn = store.lock().unwrap();
            conn.execute(
                "INSERT INTO accounts(id,email,created_at) VALUES(1,'a@test','2026-01-01')",
                [],
            )
            .unwrap();
            for id in 1..=BACKLOG + 3 {
                conn.execute(
                    "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body)
                     VALUES(?1,1,?1,'t'||?1,'sender@test','subject',?2,'snippet','body')",
                    params![id, Utc::now().to_rfc3339()],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO triage(account_id,message_id,created_at,first_load) VALUES(1,?1,?2,1)",
                    params![id, Utc::now().to_rfc3339()],
                )
                .unwrap();
            }
        }
        store
    }

    fn exec(store: &SqliteStore, sql: &str) {
        store.lock().unwrap().execute(sql, []).unwrap();
    }

    /// Message `id` arrived live, not in the backfill.
    fn live(store: &SqliteStore, id: i64) {
        exec(
            store,
            &format!("UPDATE triage SET first_load=0 WHERE message_id={id}"),
        );
    }

    struct Verdict {
        urgency: f64,
        due_in_days: Option<i64>,
        state: AttentionState,
        show: bool,
    }

    fn pick(urgency: f64) -> Verdict {
        Verdict {
            urgency,
            due_in_days: None,
            state: AttentionState::default(),
            show: true,
        }
    }

    /// Triage message `id` under `trigger` with `verdict`.
    fn triage_with(store: &SqliteStore, id: i64, trigger: &str, verdict: Verdict) {
        store.enqueue_agent_triage(1, id, trigger, false).unwrap();
        let job = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert_eq!(job.message_id, id);
        let context = store.load_agent_context(&job).unwrap();
        let mut decided = decision(id);
        decided.attention.show_in_fye = verdict.show;
        decided.attention.state = verdict.state;
        decided.attention.factors.urgency = verdict.urgency;
        decided.attention.factors.attention_at = verdict.due_in_days.map(|days| SupportedTime {
            value: (Utc::now() + Duration::days(days))
                .date_naive()
                .format("%Y-%m-%d")
                .to_string(),
            timezone: None,
            source_message_id: id,
            interpreted_relative: false,
        });
        assert_eq!(
            store
                .commit_agent_decision(&job, &context, &decided, &[])
                .unwrap(),
            AgentCommitOutcome::Applied
        );
    }

    fn triage(store: &SqliteStore, id: i64, trigger: &str, urgency: f64) {
        triage_with(store, id, trigger, pick(urgency));
    }

    fn board(store: &SqliteStore) -> Vec<String> {
        let conn = store.lock().unwrap();
        let mut items = list_items(&conn, 1, "fye", Utc::now()).unwrap();
        rank_items(&mut items, &RankingConfig::default(), Utc::now());
        items.into_iter().map(|item| item.thread_id).collect()
    }

    fn held(store: &SqliteStore) -> Vec<String> {
        let conn = store.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT thread_id FROM agent_thread_attention WHERE held=1 ORDER BY thread_id")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }

    fn t(id: i64) -> String {
        format!("t{id}")
    }

    fn backlog(store: &SqliteStore) {
        // Urgency rises with the id, so the last FIRST_LOAD_FYE_CAP are the
        // board's best and arrive last: the cap must evict, not just refuse.
        // All of it stays under FIRST_LOAD_URGENT: none of it is pressing.
        for id in 1..=BACKLOG {
            triage(store, id, "backfill", 0.5 * id as f64 / BACKLOG as f64);
        }
    }

    #[test]
    fn the_backfill_keeps_only_its_best_ranked_threads() {
        let store = fixture();
        backlog(&store);
        let shown = board(&store);
        let best: Vec<String> = (BACKLOG - FIRST_LOAD_FYE_CAP as i64 + 1..=BACKLOG)
            .rev()
            .map(t)
            .collect();
        assert_eq!(shown, best);
        // Both copies of the pick moved: the sitrep tiers off the JSON one.
        let json_says_shown: i64 = store
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM agent_thread_attention
                 WHERE json_extract(attention_json,'$.show_in_fye')=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(json_says_shown, FIRST_LOAD_FYE_CAP as i64);
    }

    /// The trigger a job runs under is not what makes mail first-load: the
    /// SENT leg rewrites a queued backfill job's trigger to `thread_changed:`,
    /// and that thread is still backlog.
    #[test]
    fn a_rewritten_trigger_does_not_escape_the_cap() {
        let store = fixture();
        backlog(&store);
        // Backlog that competes like any other: the weak one is held...
        triage(&store, BACKLOG + 1, "thread_changed:99:1", 0.0);
        assert!(!board(&store).contains(&t(BACKLOG + 1)));
        assert!(held(&store).contains(&t(BACKLOG + 1)));
        // ...and a strong one wins a place from the weakest on the board.
        let weakest = t(BACKLOG - FIRST_LOAD_FYE_CAP as i64 + 1);
        assert!(board(&store).contains(&weakest));
        triage(&store, BACKLOG + 2, "thread_changed:99:2", 0.79);
        assert!(board(&store).contains(&t(BACKLOG + 2)));
        assert!(!board(&store).contains(&weakest));
        assert_eq!(board(&store).len(), FIRST_LOAD_FYE_CAP);
    }

    #[test]
    fn live_mail_is_never_counted_against_the_cap() {
        let store = fixture();
        backlog(&store);
        // A brand-new live thread, and a person asking about a held one.
        live(&store, BACKLOG + 1);
        triage(&store, BACKLOG + 1, "arrival", 0.0);
        triage(&store, 1, "manual:requested", 0.0);
        let shown = board(&store);
        assert_eq!(shown.len(), FIRST_LOAD_FYE_CAP + 2);
        assert!(shown.contains(&t(BACKLOG + 1)));
        assert!(shown.contains(&t(1)));
        // And a later backfill pick still competes only with the backlog.
        triage(&store, BACKLOG + 2, "backfill", 0.0);
        assert!(!board(&store).contains(&t(BACKLOG + 2)));
    }

    /// Live mail in a held thread, the owner's own reply included, makes it
    /// current: out of the allowance for good.
    #[test]
    fn a_reply_in_a_held_thread_brings_it_back() {
        let store = fixture();
        backlog(&store);
        assert!(held(&store).contains(&t(1)));
        exec(
            &store,
            "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,body,is_sent)
             VALUES(100,1,'100','t1','a@test','re',datetime('now'),'s','b',1)",
        );
        exec(
            &store,
            "INSERT INTO triage(account_id,message_id,created_at) VALUES(1,100,datetime('now'))",
        );
        triage(&store, 1, "thread_changed:100:1", 0.1);
        assert!(!held(&store).contains(&t(1)));
        assert!(board(&store).contains(&t(1)));
    }

    /// A related update the live arrival carries does not make ANOTHER
    /// thread current: that thread got no live mail.
    #[test]
    fn live_mail_does_not_release_threads_it_did_not_arrive_in() {
        let store = fixture();
        backlog(&store);
        assert!(held(&store).contains(&t(1)));
        live(&store, BACKLOG + 1);
        store
            .enqueue_agent_triage(1, BACKLOG + 1, "arrival", false)
            .unwrap();
        let job = store
            .claim_agent_job(1, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        let context = store.load_agent_context(&job).unwrap();
        let target = store.agent_thread_context(1, "t1").unwrap();
        let mut attention = target.attention.unwrap();
        attention.show_in_fye = true;
        let mut decided = decision(BACKLOG + 1);
        decided
            .related_updates
            .push(crate::triage::decision::RelatedAttentionUpdate {
                thread_id: "t1".into(),
                expected_revision: target.revision.attention_revision,
                attention,
                evidence: vec![],
            });
        let sources = store.snapshot_agent_sources(1, &[1, BACKLOG + 1]).unwrap();
        let outcome = store
            .commit_agent_decision(&job, &context, &decided, &sources)
            .unwrap();
        assert_eq!(outcome, AgentCommitOutcome::Applied);
        assert!(held(&store).contains(&t(1)));
        assert!(!board(&store).contains(&t(1)));
    }

    #[test]
    fn a_pinned_thread_neither_counts_nor_moves() {
        let store = fixture();
        triage(&store, 1, "backfill", 0.0);
        exec(
            &store,
            "INSERT INTO agent_thread_preferences(account_id,thread_id,show_in_fye,updated_at)
             VALUES(1,'t1',1,datetime('now'))",
        );
        for id in 2..=BACKLOG {
            triage(&store, id, "backfill", 0.5);
        }
        let shown = board(&store);
        assert_eq!(shown.len(), FIRST_LOAD_FYE_CAP + 1);
        assert!(shown.contains(&t(1)));
    }

    #[test]
    fn pinning_a_held_thread_ends_the_hold() {
        let store = fixture();
        backlog(&store);
        assert!(held(&store).contains(&t(1)));
        store
            .correct_agent_triage(1, 1, "show_in_fye", &serde_json::json!(true), Utc::now())
            .unwrap();
        assert!(!held(&store).contains(&t(1)));
        assert!(board(&store).contains(&t(1)));
    }

    #[test]
    fn pressing_mail_skips_the_cap() {
        let store = fixture();
        backlog(&store);
        // Urgent, due in three days, and overdue while still needing the
        // owner: all on the board, none costing the backlog a place.
        triage(&store, BACKLOG + 1, "backfill", 0.9);
        triage_with(
            &store,
            BACKLOG + 2,
            "backfill",
            Verdict {
                due_in_days: Some(3),
                ..pick(0.0)
            },
        );
        triage_with(
            &store,
            BACKLOG + 3,
            "backfill",
            Verdict {
                due_in_days: Some(-2),
                state: AttentionState::NeedsUser,
                ..pick(0.0)
            },
        );
        let shown = board(&store);
        assert_eq!(shown.len(), FIRST_LOAD_FYE_CAP + 3);
        for id in [BACKLOG + 1, BACKLOG + 2, BACKLOG + 3] {
            assert!(shown.contains(&t(id)), "t{id}");
        }
    }

    /// Backlog is full of dates that have come and gone. Only what still
    /// needs the owner is overdue; the rest, and anything a month out, is
    /// just backlog.
    #[test]
    fn past_and_distant_dates_are_backlog() {
        let store = fixture();
        backlog(&store);
        for (id, days) in [(BACKLOG + 1, -10), (BACKLOG + 2, 30)] {
            triage_with(
                &store,
                id,
                "backfill",
                Verdict {
                    due_in_days: Some(days),
                    state: AttentionState::Informational,
                    ..pick(0.0)
                },
            );
            assert!(held(&store).contains(&t(id)), "t{id}");
        }
    }

    #[test]
    fn a_held_thread_comes_back_when_it_turns_pressing() {
        let store = fixture();
        backlog(&store);
        assert!(held(&store).contains(&t(1)));
        // Its due date arrives (stood in for by the urgency it now carries).
        exec(
            &store,
            "UPDATE agent_thread_attention
             SET attention_json=json_set(attention_json,'$.factors.urgency',0.95)
             WHERE thread_id='t1'",
        );
        // The next commit, about anything, lets it in.
        live(&store, BACKLOG + 1);
        triage(&store, BACKLOG + 1, "arrival", 0.0);
        assert!(board(&store).contains(&t(1)));
        assert!(!held(&store).contains(&t(1)));
    }

    #[test]
    fn closing_rows_never_refills_the_board() {
        let store = fixture();
        backlog(&store);
        let before = held(&store);
        assert_eq!(before.len(), BACKLOG as usize - FIRST_LOAD_FYE_CAP);
        exec(
            &store,
            "UPDATE triage SET status='done' WHERE message_id IN (16,17,18,19,20)",
        );
        live(&store, BACKLOG + 1);
        triage(&store, BACKLOG + 1, "arrival", 0.0);
        assert_eq!(held(&store), before);
        assert_eq!(board(&store).len(), FIRST_LOAD_FYE_CAP - 5 + 1);
    }

    /// Re-runs of sorted backlog never bring it back, even when the model
    /// changes its mind both ways and there is room on the board.
    #[test]
    fn background_reruns_never_bring_backlog_back() {
        let store = fixture();
        backlog(&store);
        assert!(held(&store).contains(&t(1)));
        exec(
            &store,
            "UPDATE triage SET status='done' WHERE message_id IN (16,17,18,19,20)",
        );
        triage(&store, 1, "source_changed:2:1", 0.1);
        assert!(held(&store).contains(&t(1)));
        // Off and back on again.
        triage_with(
            &store,
            1,
            "source_changed:2:2",
            Verdict {
                show: false,
                ..pick(0.1)
            },
        );
        assert!(!held(&store).contains(&t(1)));
        triage(&store, 1, "source_changed:2:3", 0.1);
        assert!(held(&store).contains(&t(1)));
        assert!(!board(&store).contains(&t(1)));
        // Nor does a re-run of a row on the board cost another its place.
        let before = board(&store);
        triage(&store, 15, "source_changed:2:4", 0.49);
        assert_eq!(board(&store).len(), before.len());
        // A person asking again is not background: their answer stands.
        triage(&store, 1, "manual:requested", 0.1);
        assert!(board(&store).contains(&t(1)));
    }

    #[test]
    fn an_existing_board_is_left_whole() {
        let store = fixture();
        for id in 1..=BACKLOG {
            live(&store, id);
            triage(&store, id, "arrival", 0.5);
        }
        assert_eq!(board(&store).len(), BACKLOG as usize);
    }
}
