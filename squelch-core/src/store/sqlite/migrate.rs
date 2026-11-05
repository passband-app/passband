//! Additive schema migrations for pre-existing DBs.

use super::*;

/// Add `column` (`decl` = type + constraints) to `table` unless it is already
/// there. There is no schema-version counter, so presence is detected via
/// `PRAGMA table_info`, which keeps this idempotent across opens.
///
/// Returns `true` ONLY on the call that actually adds the column, which is what
/// lets a caller run a one-time backfill on the open that introduces it — never
/// on a fresh DB (schema.sql already carries it) and never again after.
fn add_column_if_missing(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<bool> {
    let cols = table_columns(conn, table)?;
    let any = !cols.is_empty();
    let present = cols.iter().any(|c| c == column);
    // An empty `table_info` means the table does not exist yet; skipping then
    // keeps this seam per-table independent for tests that build partial schemas.
    if any && !present {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
            [],
        )?;
        return Ok(true);
    }
    Ok(false)
}

/// A table's column names, EMPTY when the table does not exist (SQLite answers
/// an absent `PRAGMA table_info` with no rows rather than an error).
fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let cols = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(cols)
}

/// Does `table` carry ALL of `columns`? Guards the blocks that READ columns
/// rather than add them: the migration unit tests build partial schemas, and a
/// SELECT naming a column that install never had is a hard SQL error, not a
/// no-op.
fn has_columns(conn: &Connection, table: &str, columns: &[&str]) -> Result<bool> {
    let present = table_columns(conn, table)?;
    Ok(columns.iter().all(|c| present.iter().any(|p| p == c)))
}

/// Do ALL of `tables` exist? The migration unit tests build partial schemas, so
/// every block that touches more than the table it just altered guards on this
/// rather than assuming a full DB.
fn tables_exist(conn: &Connection, tables: &[&str]) -> Result<bool> {
    let placeholders = (1..=tables.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ({placeholders})"
    ))?;
    let binds: Vec<&dyn rusqlite::ToSql> =
        tables.iter().map(|t| t as &dyn rusqlite::ToSql).collect();
    let n: i64 = stmt.query_row(binds.as_slice(), |r| r.get(0))?;
    Ok(n as usize == tables.len())
}

/// Every stored `unknown` and `scanner` open is relabelled with TODAY'S
/// classifier, in BOTH directions: a scanner recorded before the label existed
/// stops counting, and a person's open hidden by a marker that has since been
/// removed comes back. `proxied` is left alone. Idempotent, one savepoint (one
/// commit, not one per row), and only rows whose label changes are written.
/// The table is small (capped per token, retired with its tracker), so this
/// runs on every open of the store rather than behind a version flag.
fn relabel_scanner_opens(conn: &Connection) -> Result<()> {
    if !has_columns(conn, "message_opens", &["user_agent", "classification"])? {
        return Ok(());
    }
    let rows: Vec<(i64, Option<String>, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, user_agent, classification FROM message_opens
             WHERE classification IN ('unknown', 'scanner')",
        )?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let changes: Vec<(i64, &str)> = rows
        .into_iter()
        .filter_map(|(id, ua, was)| {
            let now = crate::tracking::classify_user_agent(ua.as_deref());
            // `proxied` is never written here: only these two labels move.
            let now = if now == crate::store::OPEN_SCANNER {
                crate::store::OPEN_SCANNER
            } else {
                crate::store::OPEN_UNKNOWN
            };
            (now != was).then_some((id, now))
        })
        .collect();
    if changes.is_empty() {
        return Ok(());
    }
    conn.execute_batch("SAVEPOINT relabel_scanner_opens")?;
    let applied = changes.iter().try_for_each(|(id, label)| {
        conn.execute(
            "UPDATE message_opens SET classification = ?1 WHERE id = ?2",
            rusqlite::params![label, id],
        )
        .map(|_| ())
    });
    match applied {
        Ok(()) => conn.execute_batch("RELEASE relabel_scanner_opens")?,
        Err(e) => {
            let _ = conn
                .execute_batch("ROLLBACK TO relabel_scanner_opens; RELEASE relabel_scanner_opens");
            return Err(e.into());
        }
    }
    Ok(())
}

/// Additive, idempotent migrations for pre-existing DBs. New tables and most
/// indexes are handled by `CREATE ... IF NOT EXISTS` in `schema.sql`; this seam
/// carries new COLUMNS on an existing table, plus any index that NAMES such a
/// column (schema.sql runs first, so it cannot create one against an old table)
/// and the rare index that has to be dropped.
pub(super) fn migrate(conn: &Connection) -> Result<()> {
    retire_records_destination(conn)?;
    relabel_scanner_opens(conn)?;
    retire_preference_retriage(conn)?;
    rebuild_calendar_reminder_fires(conn)?;
    // The account's mail provider. 'gmail' on every pre-existing row, which is
    // the honest reading: Gmail was the only provider this daemon ever had.
    add_column_if_missing(
        conn,
        "accounts",
        "provider",
        "TEXT NOT NULL DEFAULT 'gmail'",
    )?;
    add_column_if_missing(conn, "messages", "list_unsubscribe", "TEXT")?;
    // First-load mail's FYE allowance (`FIRST_LOAD_FYE_CAP`). 0 on every
    // pre-existing row: an account that already has a board keeps it whole.
    add_column_if_missing(
        conn,
        "agent_thread_attention",
        "first_load",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(conn, "triage", "first_load", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(
        conn,
        "agent_thread_attention",
        "held",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(
        conn,
        "messages",
        "list_unsub_one_click",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    // Email-authentication verdict. NULL on every pre-existing row and NOT
    // backfilled: NULL is the safe reading (it grants nothing: no
    // tracking-pixel bypass, no ghost sender scope, no timezone observation),
    // and a re-sync refills it through the message upsert.
    add_column_if_missing(conn, "messages", "auth_pass", "INTEGER")?;
    // Where From and the real sender disagree (#639), as JSON. NULL on every
    // pre-existing row and NOT backfilled here: the raw mail is not stored,
    // so only a re-fetch can fill it, and the message upsert does that.
    add_column_if_missing(conn, "messages", "sender_signals", "TEXT")?;
    // Automatic-unsubscribe job state. NULL on every pre-existing row, which is
    // exactly a legacy browser unsubscribe: no job, and the detector armed.
    for col in ["status", "status_url", "status_reason", "status_at"] {
        add_column_if_missing(conn, "unsubscribes", col, "TEXT")?;
    }
    // The smart block proposed for a sender who ignored the request, as JSON.
    // NULL means not proposed yet, which is every pre-existing row.
    add_column_if_missing(conn, "unsubscribes", "smart_block", "TEXT")?;
    // Display recipients, one column per header. NULL on every pre-existing row,
    // which is exactly the recipients-backfill queue predicate (`cc_addrs IS
    // NULL`); the backfill runs from the sync engine, over the network, so it
    // cannot live in this synchronous seam.
    //
    // `cc_addrs` is the queue's predicate and not `to_addrs` deliberately:
    // `to_addrs` has been filling for sent mail since before the split, holding
    // To+Cc+Bcc mashed into one string, and those rows need re-reading too — a
    // predicate keyed on the old column would call them done and leave the Cc
    // list rendered as though everyone on it had been in To.
    add_column_if_missing(conn, "messages", "to_addrs", "TEXT")?;
    add_column_if_missing(conn, "messages", "cc_addrs", "TEXT")?;
    add_column_if_missing(conn, "messages", "bcc_addrs", "TEXT")?;
    // Gmail's own spam verdict. 0 on every pre-existing row, which is the honest
    // reading: this daemon never fetched the SPAM label before, so nothing
    // already stored came from it. The first sync after the upgrade backfills
    // the label's window like any other.
    add_column_if_missing(conn, "messages", "is_spam", "INTEGER NOT NULL DEFAULT 0")?;
    // The Date header's UTC offset, for the timezone inference. NULL on every
    // pre-existing row and NOT backfilled: the raw mail is not stored, so only
    // a Gmail fetch per message could fill it, and the inference learns
    // forward instead. A re-sync of a message fills its row.
    add_column_if_missing(conn, "messages", "date_offset_minutes", "INTEGER")?;
    // The archive tier's marker. 0 on every pre-existing row, which is the
    // honest reading: nothing stored before the walk existed came from it.
    add_column_if_missing(conn, "messages", "is_archive", "INTEGER NOT NULL DEFAULT 0")?;
    // The archive walk's forward edge. NULL on a cursor row written before it
    // existed, which the store reads as that row's `walked_to`.
    add_column_if_missing(conn, "archive_walk", "covered_to", "TEXT")?;
    // ITS INDEX, partial: only mail with a non-zero offset is an observation,
    // and most mail is stamped UTC, so the index holds a fraction of the table
    // and costs nothing on the bulk mail it leaves out. COVERING: it carries
    // every column `zones::OFFSETS_SQL` reads, so the recompute never visits a
    // message row (whose later columns sit past the body on overflow pages).
    // Keyed sender, then the flags the query pins (authenticated, inbound, not
    // spam), then time, so the read seeks straight to one sender's countable
    // observations, newest first, and stops at its cap. Every named column
    // guarded, as for the spam index below.
    //
    // `idx_messages_zone_obs` was this index without the flags; it never
    // shipped, but a development database may hold it, and it is dropped by
    // name so the two never coexist.
    conn.execute("DROP INDEX IF EXISTS idx_messages_zone_obs", [])?;
    if has_columns(
        conn,
        "messages",
        &[
            "account_id",
            "auth_pass",
            "date_offset_minutes",
            "from_addr",
            "is_sent",
            "is_spam",
            "received_at",
        ],
    )? {
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_zone_offsets
             ON messages(account_id, from_addr COLLATE NOCASE, auth_pass, is_sent, is_spam,
                         received_at, date_offset_minutes)
             WHERE date_offset_minutes != 0",
            [],
        )?;
    }
    // The composer grew Cc and Bcc fields; a draft has to be able to hold what
    // the composer can show, or closing one silently drops recipients.
    add_column_if_missing(conn, "drafts", "cc_addr", "TEXT NOT NULL DEFAULT ''")?;
    add_column_if_missing(conn, "drafts", "bcc_addr", "TEXT NOT NULL DEFAULT ''")?;

    // A staged file's SECOND claim, beside `draft_id`: the scheduled send that
    // holds it (docs/SCHEDULED-SEND.md). Every pre-existing row is unclaimed by
    // a schedule, which NULL says: no backfill. The index names the new column,
    // so it lives here, behind the column check, and never in schema.sql.
    add_column_if_missing(conn, "outbound_attachments", "scheduled_send_id", "INTEGER")?;
    if has_columns(
        conn,
        "outbound_attachments",
        &["account_id", "scheduled_send_id"],
    )? {
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_outbound_attachments_scheduled
                 ON outbound_attachments(account_id, scheduled_send_id)
                 WHERE scheduled_send_id IS NOT NULL",
            [],
        )?;
    }
    // The outbox's recovery columns (docs/SCHEDULED-SEND.md §4), for a DB
    // whose `scheduled_sends` predates them. 0 and NULL are the honest
    // readings: no earlier attempt is known to have reached Gmail, and no
    // recovery search is being waited out.
    add_column_if_missing(
        conn,
        "scheduled_sends",
        "maybe_sent",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(conn, "scheduled_sends", "recover_at", "TEXT")?;
    // The fire-time grace and the pre-fire search's give-up clock. NULL is the
    // honest reading for both: no attempt's end is known (the grace does not
    // hold a row back), and no search is being waited out.
    add_column_if_missing(conn, "scheduled_sends", "last_attempt_at", "TEXT")?;
    add_column_if_missing(conn, "scheduled_sends", "search_deferred_since", "TEXT")?;
    // MANY NEW-MESSAGE DRAFTS. The old single-slot index made a second
    // new-message draft a constraint violation; new mail is keyed by row id now.
    // No dedupe is owed: that index guaranteed at most one nil-keyed row.
    //
    // ROLLBACK GUARD, deliberate: the name is kept, on a NON-unique index. An
    // older binary's schema.sql runs `CREATE UNIQUE INDEX IF NOT EXISTS
    // idx_drafts_new` on every open, and `IF NOT EXISTS` checks only the name,
    // so with this index in place that create is a no-op. Without it, rolling
    // back after a user holds two keyless drafts fails every open with a
    // UNIQUE violation; with it, the old binary merely edits one of several
    // keyless rows. Dropped only while still UNIQUE, so a normal open is a
    // no-op rather than a drop and rebuild.
    //
    // The SECOND degradation under rollback: the older binary's key-less upsert
    // selects `reply_to_message_id IS NULL` and knows nothing of forwards, so
    // it can pick a forward row and rewrite its text while the forward key
    // survives: the forward's note is overwritten, and a later newer binary
    // shows the old app's new message as a forward of that message. Accepted,
    // not fixed: it needs a rollback AND a held forward draft, and no schema
    // trick here can change an old binary's WHERE clause.
    let unique_new_index: bool = conn
        .prepare("SELECT 1 FROM pragma_index_list('drafts') WHERE name = 'idx_drafts_new' AND \"unique\" = 1")?
        .exists([])?;
    if unique_new_index {
        conn.execute("DROP INDEX idx_drafts_new", [])?;
    }
    // NOT the same kind of guard as the forward index's below. On every real
    // open schema.sql has already created `drafts` with both of these columns,
    // so in production this is always true. It exists for the migrate unit
    // tests, which run `migrate` over partial schemas with no `drafts` table at
    // all; without it ~15 of them fail on a missing table. Nothing here adds
    // either column.
    if has_columns(conn, "drafts", &["account_id", "reply_to_message_id"])? {
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_drafts_new
             ON drafts(account_id) WHERE reply_to_message_id IS NULL",
            [],
        )?;
    }
    // Forwards are kept too, one per forwarded message like replies. The
    // index lives HERE, not in schema.sql, because it names the migrated column:
    // schema.sql runs before this seam, so on an old DB it would reference a
    // column that does not exist yet and fail every open.
    add_column_if_missing(conn, "drafts", "forward_of_message_id", "INTEGER")?;
    // Which drafts an agent wrote (issue #394). Every existing row is the
    // owner's own, which is what the default says.
    add_column_if_missing(conn, "drafts", "from_agent", "INTEGER NOT NULL DEFAULT 0")?;
    if has_columns(conn, "drafts", &["account_id", "forward_of_message_id"])? {
        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_drafts_forward
             ON drafts(account_id, forward_of_message_id)
             WHERE forward_of_message_id IS NOT NULL",
            [],
        )?;
    }
    // The push routing tag. NULL on every pre-existing row and NOT backfilled:
    // nothing here can know which account a device filed itself under, and the
    // client re-registers (tag included) on its next foreground.
    add_column_if_missing(conn, "devices", "tag", "TEXT")?;
    // The token's APNs environment (`production` | `sandbox`). NULL on every
    // pre-existing row and NOT backfilled: only the client knows which build
    // minted its token, and the pusher treats NULL as "use the configured
    // fallback", which is exactly what those rows got before the column existed.
    add_column_if_missing(conn, "devices", "environment", "TEXT")?;
    // Which app last presented each device token. NULL on every pre-existing
    // row and NOT backfilled: a client that predates `X-Passband-Client` never
    // said what it was, and the columns fill in on its first request after it
    // updates.
    add_column_if_missing(conn, "device_tokens", "client_platform", "TEXT")?;
    add_column_if_missing(conn, "device_tokens", "client_version", "TEXT")?;
    add_column_if_missing(conn, "device_tokens", "client_seen_at", "TEXT")?;
    // When that app last wrote something. NULL on every pre-existing row and
    // NOT backfilled: nothing recorded which app made a past write.
    add_column_if_missing(conn, "device_tokens", "client_action_at", "TEXT")?;
    // The device zone that app last reported (#526). NULL on every
    // pre-existing row and NOT backfilled: only the device knows where it is,
    // and it says so on its first request after it updates.
    add_column_if_missing(conn, "device_tokens", "client_timezone", "TEXT")?;
    // Per-property triage reasons (JSON object). NULL on pre-existing rows.
    add_column_if_missing(conn, "triage", "field_reasons", "TEXT")?;

    // WHICH AUTH SHAPE a notification is about (docs/NOTIFY.md §11.6). NULL on
    // every pre-existing row and NOT backfilled, which is both honest and the
    // only safe direction: no historical event came from a sealed message (the
    // old emission path required sensitivity='normal'), so there is no kind to
    // recover, and a client reads NULL as "an ordinary event" — exactly what
    // those rows are. Every SELECT here names the column, so it has to land
    // through this seam: schema.sql's `CREATE TABLE IF NOT EXISTS` is a no-op
    // against an `events` table that already exists.
    add_column_if_missing(conn, "events", "sealed_kind", "TEXT")?;
    add_column_if_missing(
        conn,
        "events",
        "is_auth",
        "INTEGER NOT NULL DEFAULT 0 CHECK(is_auth IN (0,1))",
    )?;

    // Markers from the retired two-stage classifier: `stage1_model_used` gated
    // the Stage-1 queue, `needs_stage2` was the escalation flag. Kept so old
    // rows still load.
    let added_stage1 = add_column_if_missing(conn, "triage", "stage1_model_used", "TEXT")?;
    add_column_if_missing(conn, "triage", "needs_stage2", "INTEGER NOT NULL DEFAULT 0")?;
    // Which arm of the retired escalation router set `needs_stage2`. NULL
    // means "did not escalate".
    add_column_if_missing(conn, "triage", "escalation_reason", "TEXT")?;
    // Re-evaluation counter from the retired revisit planner. Agent triage
    // counts its revisits in `agent_triage_jobs` instead.
    add_column_if_missing(
        conn,
        "triage",
        "revisit_count",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    // WHEN A HUMAN LAST ASKED for this row to be re-triaged. NULL on every
    // pre-existing row and NOT backfilled: nobody has asked, and a backfilled
    // stamp would force the whole mailbox through the LLM passes on the next
    // tick — the exact opposite of what the age-based stale skip is for.
    add_column_if_missing(conn, "triage", "retriage_at", "TEXT")?;
    if has_columns(conn, "triage", &["account_id", "message_id", "retriage_at"])? {
        // Partial, covering range lookup: untouched mail never enters the index.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_triage_retriage_window
             ON triage(account_id,retriage_at,message_id) WHERE retriage_at IS NOT NULL;",
        )?;
    }

    // MAY THIS MESSAGE EVER NOTIFY, and from when. NULL on every pre-existing
    // row and NOT backfilled, and the NULL is the whole safety property: a
    // stamp invented for mail already in the database would make the first tick
    // after an upgrade eligible to push a month of archived mail at a phone.
    // New mail earns its stamp at ingest like everything else; old mail simply
    // never notifies, which is exactly what it does today.
    add_column_if_missing(conn, "triage", "notify_eligible_at", "TEXT")?;

    // "REMIND ME LATER": the pending stamp and the fired one. NULL on every
    // pre-existing row and NOT backfilled — nobody has asked to be reminded of
    // anything, and a backfilled `reminded_at` would drag the whole mailbox into
    // the standing band (that column is one of its arms).
    add_column_if_missing(conn, "triage", "remind_at", "TEXT")?;
    add_column_if_missing(conn, "triage", "reminded_at", "TEXT")?;
    // WHEN THE USER OPENED THIS MAIL. NULL on every
    // pre-existing row and NOT backfilled, and the absence is honest rather
    // than convenient: nothing recorded opens before this column existed, and
    // inventing them would put a number in front of a user that their own
    // mailbox does not support. `share_open_rate` refuses to answer until the
    // column has been there long enough to mean something.
    if add_column_if_missing(conn, "triage", "opened_at", "TEXT")? {
        stamp_open_ledger_start(conn)?;
    }
    // HOW LONG IT WAS READ AND HOW MANY LINKS WERE FOLLOWED. Zero on every
    // pre-existing row, and that zero is not a measurement: no client reported
    // either before these columns existed. So the dwell ledger dates itself
    // exactly as the open ledger above does, keyed off `dwell_ms` alone so the
    // stamp is written once even if an open is interrupted between the two
    // columns. The floor is compared against `opened_at`, not `received_at`:
    // what matters is whether the OPEN happened where dwell could be reported.
    if add_column_if_missing(conn, "triage", "dwell_ms", "INTEGER NOT NULL DEFAULT 0")? {
        stamp_dwell_ledger_start(conn)?;
    }
    add_column_if_missing(conn, "triage", "link_clicks", "INTEGER NOT NULL DEFAULT 0")?;
    // WHO RESOLVED A ROW, when it was not a person. NULL on every pre-existing
    // row and NOT backfilled: nothing recorded it before, and guessing would
    // hand an undo rows it never took.
    add_column_if_missing(conn, "triage", "resolved_by", "TEXT")?;

    // AND THE SWEEP'S INDEX, HERE rather than in schema.sql, for the reason
    // spelled out at that file's `idx_triage_remind_at` note: it runs before this
    // function, so an index over a just-migrated column takes the open down. The
    // partial index is deliberate — the sweep asks only "what is due?", and every
    // row in a real mailbox has `remind_at IS NULL`, so a full index would be
    // almost entirely dead weight scanned on every poll tick.
    if has_columns(conn, "triage", &["remind_at"])? {
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_triage_remind_at
             ON triage(account_id, remind_at) WHERE remind_at IS NOT NULL",
            [],
        )?;
    }

    // AND THE FIRED REMINDERS' INDEX, the same shape for the same reason. A
    // fired reminder (`reminded_at` set, `remind_at` cleared) is a standing-
    // band arm, and `append_due_reminders` asks for them on EVERY Standing
    // read, which is the client's 10 s poll; without this the ask was a walk
    // of every triage row in the account, 233 ms at 50k messages, under the
    // mutex every request waits on. Partial because almost no row has ever
    // had a reminder fire. Both columns in the guard, as `idx_messages_spam`
    // below explains: an index names every column it covers.
    if has_columns(conn, "triage", &["account_id", "reminded_at"])? {
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_triage_reminded_at
             ON triage(account_id, reminded_at) WHERE reminded_at IS NOT NULL",
            [],
        )?;
    }

    // THE SPAM PAGE'S INDEX, partial for the same reason and in the same place.
    // Every other listing wants `is_spam = 0`, which is almost every row and so
    // is better served by a scan the planner was going to do anyway; only the
    // spam page asks for the handful with `is_spam = 1`, and it asks newest-first.
    // BOTH columns in the guard, not just the migrated one. `has_columns` is
    // there to keep an index off a table that has not grown its column yet, and
    // an index names every column it covers: checking only `is_spam` left
    // `received_at` assumed, which holds for any table `schema.sql` built and
    // not for a partial one.
    if has_columns(conn, "messages", &["is_spam", "received_at"])? {
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_spam
             ON messages(account_id, received_at) WHERE is_spam = 1",
            [],
        )?;
    }

    // Whether a relationship mark asks for a full recount. 0 on any row an
    // earlier build of the ledger wrote, which is the plain mark it was.
    add_column_if_missing(
        conn,
        "relationship_dirty",
        "full",
        "INTEGER NOT NULL DEFAULT 0",
    )?;

    // THE SEALED SET, one partial index per place a message is marked kept
    // close (`known_sealed_mail_sql!`'s four arms), each holding only the
    // sealed rows. The relationship ledger counts an address's sealed mail by
    // reading these few rows and joining each to its message by key
    // (`relationships::SEALED_COUNT_SQL`), instead of probing all four tables
    // for every message the address ever sent. Sealed mail is a small
    // minority, so the indexes are small. Each guarded by the columns it
    // names: the migration suite builds partial old schemas.
    //
    // THE DECISIONS PREDICATE IS GUARDED BY `json_valid`, in a CASE because
    // only a CASE is sure not to evaluate the JSON functions first. A partial
    // index's predicate runs on EVERY write to its table, so without the guard
    // one malformed `decision_json` would make the write itself fail. A
    // malformed decision is not sealed here; the macro's own read of it fails
    // as it always has.
    for (table, columns, sql) in [
        (
            "triage",
            &["account_id", "message_id", "sensitivity"][..],
            "CREATE INDEX IF NOT EXISTS idx_triage_sealed
             ON triage(account_id, message_id) WHERE sensitivity != 'normal'",
        ),
        (
            "agent_message_decisions",
            &["account_id", "message_id", "decision_json"][..],
            "CREATE INDEX IF NOT EXISTS idx_agent_decisions_sealed
             ON agent_message_decisions(account_id, message_id)
             WHERE (CASE WHEN json_valid(decision_json) THEN json_array_length(decision_json,'$.auth.kinds')>0 OR COALESCE(json_extract(decision_json,'$.external_access.restricted'),0) ELSE 0 END)",
        ),
        (
            "agent_message_state",
            &["account_id", "message_id", "access"][..],
            "CREATE INDEX IF NOT EXISTS idx_agent_state_restricted
             ON agent_message_state(account_id, message_id) WHERE access = 'restricted'",
        ),
        (
            "agent_triage_corrections",
            &["account_id", "message_id", "field", "value_json"][..],
            "CREATE INDEX IF NOT EXISTS idx_agent_corrections_sealed
             ON agent_triage_corrections(account_id, message_id)
             WHERE field = 'external_access' AND value_json = 'true'",
        ),
    ] {
        if tables_exist(conn, &[table])? && has_columns(conn, table, columns)? {
            conn.execute(sql, [])?;
        }
    }

    // THE RELATIONSHIP LEDGER'S PER-SENDER INDEX (`relationships`): one
    // sender's mail in arrival order, COVERING the three flags its inbound
    // count filters on. `idx_messages_from_nocase` orders a sender's rows by
    // rowid, which the newest-first backfill makes OLDEST-first, so "the
    // newest 500" off it was a sort of every row the sender ever sent, and
    // the count a table lookup per row: on a 40,000-message notifications
    // sender that was 70 ms a statement, every round it was marked. Here the
    // count is an index-only walk and the newest 500 are the index's own
    // tail. Here and not in schema.sql because it covers migrated columns
    // (see `idx_triage_remind_at`). The first open after this ships builds it
    // over the whole table, once.
    if has_columns(
        conn,
        "messages",
        &[
            "from_addr",
            "received_at",
            "is_sent",
            "is_spam",
            "is_archive",
        ],
    )? {
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_from_received
             ON messages(account_id, from_addr COLLATE NOCASE, received_at,
                         is_sent, is_spam, is_archive)",
            [],
        )?;
    }

    // stage2_usage grew `category` INSIDE ITS PRIMARY KEY. ALTER ADD COLUMN
    // cannot change a PK, and the bump upsert's ON CONFLICT(account_id, day,
    // category) needs that exact unique index, so an old table must be REBUILT —
    // otherwise every ledger bump fails and LLM spend goes unrecorded.
    let usage_has_category: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(stage2_usage)")?;
        let cols = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        cols.is_empty() || cols.iter().any(|c| c == "category")
    };
    if !usage_has_category {
        conn.execute_batch(
            "ALTER TABLE stage2_usage RENAME TO stage2_usage_old;
             CREATE TABLE stage2_usage (
                 account_id    INTEGER NOT NULL,
                 day           TEXT NOT NULL,
                 category      TEXT NOT NULL DEFAULT 'stage2',
                 calls         INTEGER NOT NULL DEFAULT 0,
                 input_tokens  INTEGER NOT NULL DEFAULT 0,
                 output_tokens INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY(account_id, day, category)
             );
             INSERT INTO stage2_usage(account_id, day, category, calls, input_tokens, output_tokens)
                 SELECT account_id, day, 'stage2', calls, input_tokens, output_tokens
                 FROM stage2_usage_old;
             DROP TABLE stage2_usage_old;",
        )?;
    }

    // Prompt-cache token columns, AFTER the category rebuild so they land on the
    // rebuilt table too. Zero is correct history: pre-existing rows never
    // recorded cache traffic, and zero cache tokens cost nothing at read time.
    add_column_if_missing(
        conn,
        "stage2_usage",
        "cache_creation_tokens",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(
        conn,
        "stage2_usage",
        "cache_read_tokens",
        "INTEGER NOT NULL DEFAULT 0",
    )?;

    // CATEGORIZE-THEN-EXTRACT markers. NULL is the correct resting value for
    // both (no category => never queued for extraction), so no backfill.
    add_column_if_missing(conn, "triage", "category", "TEXT")?;
    add_column_if_missing(conn, "triage", "extractor_model_used", "TEXT")?;

    // SHIPMENTS-EXTRACTOR trigger, plus the order reference the extractor writes
    // onto a shipment. Both rest at NULL on historical rows; the trigger gets a
    // bounded backfill at the bottom of this function (an unqueued column would
    // leave the whole existing mailbox invisible to the new extractor).
    let added_ship_extract = add_column_if_missing(conn, "triage", "ship_extract_model", "TEXT")?;
    add_column_if_missing(conn, "shipments", "order_ref", "TEXT")?;
    // AND ITS INDEX, HERE rather than in schema.sql. That file runs in full on
    // every open, BEFORE this function, so an index over a just-migrated column
    // would fail with "no such column: order_ref" on every pre-existing DB and
    // take the whole store's open down with it. `IF NOT EXISTS` keeps this
    // idempotent, and it covers fresh DBs too (schema.sql declares the column,
    // the ALTER above no-ops, this still runs).
    if tables_exist(conn, &["shipments"])? {
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_shipments_order_ref
             ON shipments(account_id, order_ref)",
            [],
        )?;
    }
    // `shipment_orders` and its index DO ride in on schema.sql: `init` runs that
    // whole file (all `CREATE ... IF NOT EXISTS`) on EVERY open, so a
    // pre-existing DB grows new TABLES there with no seam. Only new columns on
    // an existing table — and anything that references one — need this function.

    // SHIPMENT IDENTITY PROVENANCE. Three columns, each fixing a different way
    // mail-derived state used to be attributed to the wrong message.
    //
    // `created_by_message_id` — IMMUTABLE. `last_message_id` moves to whichever
    // mail most recently advanced the row, so the extractor's phantom reaping,
    // keyed on it, could delete a package established weeks earlier by another
    // email. Backfilled to `last_message_id`: for a historical row that is the
    // best available truth (often the only message the row ever saw).
    if add_column_if_missing(conn, "shipments", "created_by_message_id", "INTEGER")? {
        conn.execute(
            "UPDATE shipments SET created_by_message_id = last_message_id
             WHERE created_by_message_id IS NULL",
            [],
        )?;
    }
    // `item_name_msg` — which message's extraction supplied the CURRENT name.
    // Sealing scrubs by this, so a name donated onto a row another mail feeds
    // cannot outlive the seal of the mail it came from. Backfilled to
    // `last_message_id` wherever a name exists — again the best truth available,
    // and the pointer sealing already used.
    if add_column_if_missing(conn, "shipments", "item_name_msg", "INTEGER")? {
        conn.execute(
            "UPDATE shipments SET item_name_msg = last_message_id
             WHERE item_name_msg IS NULL AND COALESCE(item_name, '') != ''",
            [],
        )?;
    }
    // `item_name_source` — which MECHANISM supplied the name, the sibling of
    // `item_name_msg`'s which MESSAGE. 'regex' is correct history and needs no
    // backfill: every pre-existing name came from the ingest detector, so the
    // shipments extractor may replace any of them, and the detector's own
    // longer-wins heuristic still applies among them.
    add_column_if_missing(
        conn,
        "shipments",
        "item_name_source",
        "TEXT NOT NULL DEFAULT 'regex'",
    )?;
    // `order_merchant` — the namespace `order_ref` was always missing. Backfilled
    // in Rust below (it needs the registrable-domain rule, not a SQL expression);
    // a row left NULL simply matches no future order mail, which is the safe
    // direction — a missed name donation, never a cross-merchant one.
    let added_order_merchant = add_column_if_missing(conn, "shipments", "order_merchant", "TEXT")?;
    // `order_merchant_msg` — which MESSAGE named the current `order_merchant`,
    // for the agent door's access check. NULL on every pre-existing row: the
    // old extractor's domain merchant, judged by the row's own mail.
    add_column_if_missing(conn, "shipments", "order_merchant_msg", "INTEGER")?;
    if tables_exist(conn, &["shipments"])? {
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_shipments_order_merchant
             ON shipments(account_id, order_merchant, order_ref)",
            [],
        )?;
    }
    if added_order_merchant {
        backfill_order_merchant(conn)?;
    }

    // `shipment_orders` GREW ITS UNIQUE KEY: (account_id, order_ref) became
    // (account_id, order_merchant, order_ref). ALTER ADD COLUMN cannot change a
    // unique index, and the staging upsert's ON CONFLICT names the new triple, so
    // an old table must be REBUILT or every staging write fails.
    //
    // DROP + CREATE, discarding rows, rather than a copy: staging is write-only
    // scaffolding introduced in the same unreleased slice as this migration
    // (nothing has shipped that writes it), an un-namespaced row cannot be
    // assigned a merchant after the fact, and a staged order is pure
    // mail-derived content that the next order mail from that seller recreates.
    // Guarded on the column being absent, so it runs at most once per DB. The
    // DROP takes the table's indexes with it, hence the re-CREATE — schema.sql
    // already ran this open and will not run again.
    if tables_exist(conn, &["shipment_orders"])?
        && !has_columns(conn, "shipment_orders", &["order_merchant"])?
    {
        conn.execute_batch(
            "DROP TABLE shipment_orders;
             CREATE TABLE shipment_orders (
                 id INTEGER PRIMARY KEY,
                 account_id INTEGER NOT NULL,
                 order_ref TEXT NOT NULL,
                 order_merchant TEXT NOT NULL DEFAULT '',
                 item_name TEXT NOT NULL DEFAULT '',
                 thread_id TEXT NOT NULL DEFAULT '',
                 last_message_id INTEGER,
                 item_name_msg INTEGER,
                 first_seen TEXT NOT NULL,
                 last_update TEXT NOT NULL,
                 UNIQUE(account_id, order_merchant, order_ref)
             );
             CREATE INDEX IF NOT EXISTS idx_shipment_orders_msg
                 ON shipment_orders(account_id, last_message_id);",
        )?;
    }
    // A `shipment_orders` that already carries `order_merchant` but predates
    // `item_name_msg` still needs the column (and the same name-provenance
    // backfill as `shipments`).
    if add_column_if_missing(conn, "shipment_orders", "item_name_msg", "INTEGER")? {
        conn.execute(
            "UPDATE shipment_orders SET item_name_msg = last_message_id
             WHERE item_name_msg IS NULL AND COALESCE(item_name, '') != ''",
            [],
        )?;
    }

    // Recipient-autocomplete columns. NULL is fine on old rows: the Sent
    // harvest fills both for all of history, and ongoing seeding stamps
    // last_sent_at from then on.
    add_column_if_missing(conn, "contacts", "last_sent_at", "TEXT")?;
    add_column_if_missing(conn, "contacts", "display_name", "TEXT")?;

    // Carrier-polling columns. NULL/0 is correct history and needs no backfill:
    // a pre-existing row has never been polled, so nothing beyond what its mail
    // said is known about it, and it owes zero failures.
    add_column_if_missing(conn, "shipments", "carrier_status_raw", "TEXT")?;
    add_column_if_missing(conn, "shipments", "eta", "TEXT")?;
    add_column_if_missing(conn, "shipments", "delivered_at", "TEXT")?;
    add_column_if_missing(conn, "shipments", "last_polled_at", "TEXT")?;
    add_column_if_missing(
        conn,
        "shipments",
        "poll_failures",
        "INTEGER NOT NULL DEFAULT 0",
    )?;

    // USER CLEAR. NULL is correct history for every existing row — nobody has
    // cleared anything yet — and needs no backfill. Read-side only; see the
    // column comment in schema.sql.
    add_column_if_missing(conn, "shipments", "cleared_at", "TEXT")?;

    // THE CARRIER'S ANSWER CLOCK. Pre-existing rows never recorded which
    // attempt was answered, so the best available history is: a row holding a
    // carrier's words was answered no later than its last attempt. That is an
    // upper bound, which errs toward keeping a row listed for one more window
    // rather than hiding a live one; it is corrected by the next real answer.
    if add_column_if_missing(conn, "shipments", "last_answered_at", "TEXT")? {
        conn.execute(
            "UPDATE shipments SET last_answered_at = last_polled_at
              WHERE carrier_status_raw IS NOT NULL",
            [],
        )?;
    }

    // The cid an inline image part declared. NULL on every pre-existing row and
    // NOT backfillable from here — the Content-ID lives in the RFC822, which the
    // store never kept — so already-synced mail keeps painting its inline images
    // as tiles below the body until a re-sync re-ingests it.
    add_column_if_missing(conn, "attachments", "content_id", "TEXT")?;

    // Adding `stage1_model_used` leaves it NULL on every historical row — exactly
    // the Stage-1 queue predicate — so without this backfill the whole mailbox
    // re-classifies through the paid model. Rows already classified or already
    // seen are marked 'migrated' to stay out of the queue; the residual
    // (`status='new' AND model_used IS NULL`) correctly does re-enter Stage-1,
    // whose apply recomputes `needs_stage2` from model confidence.
    //
    // Guarded by `added_stage1` so it fires exactly once, at introduction: a
    // later run would wrongly 'migrate' rows legitimately queued for Stage-1 that
    // a read door had promoted past 'new'.
    if added_stage1 {
        conn.execute(
            "UPDATE triage SET stage1_model_used = 'migrated'
             WHERE stage1_model_used IS NULL
               AND (model_used IS NOT NULL OR status != 'new')",
            [],
        )?;
    }

    // ---- ONE-SHOTS AT THE `ship_extract_model` INTRODUCTION ----------------
    //
    // BOTH are gated on `added_ship_extract` rather than on an `app_settings`
    // done-flag. `app_settings` is keyed PER ACCOUNT and this seam runs per DB,
    // before any account is known, so a flag here would need an account loop that
    // buys nothing: "the column was absent" is already an exactly-once,
    // DB-scoped, self-recording trigger, and it is the idiom the `added_stage1`
    // backfill above established.
    if added_ship_extract {
        ship_extract_backfill(conn)?;
        clear_stale_no_extractor_markers(conn)?;
    }

    // A one-message re-triage requested while its job was leased waits as a
    // follow-up; this carries its foreground lane to the re-run. Older rows
    // were all background requests, which is the default: no backfill.
    add_column_if_missing(
        conn,
        "agent_triage_followups",
        "foreground",
        "INTEGER NOT NULL DEFAULT 0",
    )?;

    // Blind recipients on a local draft. Pre-existing rows had none, and '' is
    // the honest reading, which is also the column default: no backfill.
    add_column_if_missing(conn, "drafts", "bcc_addr", "TEXT NOT NULL DEFAULT ''")?;

    // Per-calendar failure state and the cap's mark, added after the calendar
    // tables first existed. The defaults read as "never failed, not cut",
    // which a store that had never recorded either is: no backfill.
    add_column_if_missing(
        conn,
        "calendar_calendars",
        "failures",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(conn, "calendar_calendars", "retry_at", "TEXT")?;
    add_column_if_missing(
        conn,
        "calendar_calendars",
        "truncated",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    // Colour and role per calendar, colour, organizer flag and description per
    // event (calendar wave 3). The calendar tables have not shipped, so this
    // only carries a development store forward: a calendar's role and colour
    // are filled by the next pass, an event's fields by its next change or
    // the daily full read. Until then a stored event reads as not the
    // user's own, which only hides an Edit button.
    add_column_if_missing(conn, "calendar_calendars", "color", "TEXT")?;
    add_column_if_missing(conn, "calendar_calendars", "access_role", "TEXT")?;
    add_column_if_missing(conn, "calendar_events", "color_id", "TEXT")?;
    add_column_if_missing(
        conn,
        "calendar_events",
        "organizer_self",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(conn, "calendar_events", "description", "TEXT")?;
    // Guests (calendar wave 5). An events table from before them was filled
    // by reads that asked Google for one attendee per event, and its sync
    // tokens were minted by those requests ("All other query parameters
    // should be the same as for the initial synchronization to avoid
    // undefined behavior"). So when the column is new, every calendar forgets
    // its token and its last full read: the next pass reads each in full with
    // the new parameters and fills the guest lists. Until it has, a stored
    // event reads as having no guests, and a guest edit still builds its list
    // from a fresh read of Google, never from these rows.
    if add_column_if_missing(
        conn,
        "calendar_events",
        "guests_truncated",
        "INTEGER NOT NULL DEFAULT 0",
    )? {
        conn.execute(
            "UPDATE calendar_calendars SET sync_token=NULL, full_synced_at=NULL",
            [],
        )?;
    }
    // The unreadable-organizer mark (calendar fix round 1). A stored row from
    // before it reads as "no organizer named", which the edit guard treats
    // as the user's own; so when the column is new every calendar forgets
    // its token and last full read, and the next pass rereads each in full
    // and sets the mark where Google sends such an organizer.
    if add_column_if_missing(
        conn,
        "calendar_events",
        "organizer_unreadable",
        "INTEGER NOT NULL DEFAULT 0",
    )? {
        conn.execute(
            "UPDATE calendar_calendars SET sync_token=NULL, full_synced_at=NULL",
            [],
        )?;
    }
    // Gmail's own events (2026-10-28). A row from before the mark reads as
    // an ordinary event, which offers an edit Google then refuses; so when
    // the column is new every calendar forgets its token and last full read,
    // and the next pass rereads each in full with `eventType` in the mask.
    if add_column_if_missing(
        conn,
        "calendar_events",
        "from_gmail",
        "INTEGER NOT NULL DEFAULT 0",
    )? {
        conn.execute(
            "UPDATE calendar_calendars SET sync_token=NULL, full_synced_at=NULL",
            [],
        )?;
    }

    // The card's place gained its street address (2026-10-29). A table made
    // before it holds names only, under the old prompt, so every row is
    // pending again anyway.
    add_column_if_missing(conn, "calendar_places", "address", "TEXT")?;

    // STRANDED FAN-OUTS. A group send settles its recipients from a background
    // task, and no task survives the process. So any recipient still 'pending'
    // when the store opens belongs to a batch that died — a restart, a crash, a
    // kill mid-send — and leaving it would make a months-old send read as one
    // still in progress.
    //
    // Failing it is the honest reading and the safe one: the mail either went or
    // did not, we cannot tell which from here, and a recipient recorded as
    // unreached is a prompt to check rather than a silent claim of delivery. Safe
    // to run unconditionally because nothing is in flight at open.
    if tables_exist(conn, &["group_send_recipients"])? {
        conn.execute(
            "UPDATE group_send_recipients
             SET status = 'failed', error = 'the daemon restarted mid-send'
             WHERE status = 'pending'",
            [],
        )?;
    }

    // ---- EVERY OPEN: the normalized sent-recipient index -------------------
    //
    // `message_recipients` arrives empty on an existing DB while `to_addrs` has
    // been filling for months, so without this a send group opens on a mailbox
    // that looks like it has never written to anyone. Derived history is the
    // half of the feature that can speak about the past, and this is what gives
    // it something to read.
    //
    // Self-triggering and idempotent: the candidate predicate is "sent, has
    // recipients, has no index rows", so a completed pass matches nothing. It
    // runs under one commit, so an interrupted one starts over on the next
    // open rather than resuming. No done-flag, no watermark and no `added_*`
    // gate, on purpose: a re-ingest whose raw does not parse clears a stored
    // message's index rows while its columns keep their text, and only a pass
    // over every sent row puts them back. The candidates come from two indexes,
    // so that pass is close to free (see the function).
    backfill_message_recipients(conn)?;

    // ---- THE FTS INDEX'S OPTIONS CHANGED ----------------------------------
    //
    // Stemming and a prefix index, neither of which fts5 can be altered into.
    // Best-effort by design: see the function for why a busy sibling process
    // must not turn this into a failed open.
    rebuild_fts_if_stale(conn)?;

    // ---- CALENDAR SEARCH'S INDEX OUT OF STEP ------------------------------
    //
    // Events written before the index existed, or by an older daemon since.
    reindex_calendar_search_if_stale(conn)?;

    // Re-launder every stored institution through the extractor's OWN sanitizer,
    // which grew a shape check after a model returned a valid-looking JSON object
    // whose institution ran from a correct bank name straight into this crate's
    // prompt scaffolding. The stored text was only ever CAPPED, so those rows are
    // on screen right now reading "Venmo=== TRUSTED".
    //
    // In Rust and through the real function, never a hand-written SQL predicate:
    // a second definition of "does this look like a bank name" would drift from
    // the one the extractor enforces, and then stored data and new data would
    // obey different rules.
    //
    // Self-triggering and idempotent, like the recipients backfill above: the
    // candidate predicate is "the sanitizer disagrees with what is stored", and
    // its output is a fixed point, so a completed pass matches nothing and a
    // re-run is free. No done-flag needed for a pass this cheap - one short
    // string per banking row, no bodies read.
    let relaundered = resanitize_institutions(conn)?;
    if relaundered > 0 {
        // A pass that can NULL a stored institution on a real database says so.
        // Counts only, never a value: both siblings report the same way.
        eprintln!("squelch: migration re-laundered {relaundered} institution name(s)");
    }

    // See the function: the ingest fix that shipped with the empty-body guard is
    // what makes these rows recoverable, and nothing else ever clears the stamp.
    let unstamped = clear_recoverable_no_body_markers(conn)?;
    if unstamped > 0 {
        eprintln!("squelch: migration re-queued {unstamped} row(s) whose body has since arrived");
    }

    create_shipment_order_links(conn)?;

    // See the function: ghosts built before `EventTime::resolve` lost every
    // zoned local time, so each is rebuilt once from its stored decisions.
    rebuild_ghosts_for_zoned_times(conn);

    // See the function: Amazon's Shipped/Delivered decisions were always
    // stored, and only lacked a key to project on.
    project_amazon_boxes(conn);
    // See the function: a drop-off receipt's box is going away, not coming.
    drop_sent_packages(conn);

    // `skipped` came after the table's first form; a database a pre-release
    // build opened has the table without it. Guarded, like the rest, on the
    // table existing.
    if tables_exist(conn, &["event_revisit_asked"])? {
        add_column_if_missing(
            conn,
            "event_revisit_asked",
            "skipped",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
    }

    // Cross-sender links between ghosts and the user's accepted changes
    // (`triage::event_identity`) came after the ghost tables.
    add_column_if_missing(conn, "ghost_events", "linked_to", "INTEGER")?;
    add_column_if_missing(conn, "ghost_events", "link_ack", "TEXT")?;
    add_column_if_missing(
        conn,
        "ghost_event_sources",
        "accepted",
        "INTEGER NOT NULL DEFAULT 0",
    )?;

    // Guarded on table existence — migration unit tests build partial schemas.
    if !tables_exist(conn, &["triage", "deadlines", "messages"])? {
        return Ok(());
    }
    // A model-sourced deadline more than 45 days BEFORE its own message's
    // receipt is a hallucinated year, so clear it (the mail stays surfaced, just
    // dateless), demote a stranded past_due tier, and drop the deadlines row.
    // Deterministic parses (source not stage1/stage2) are untouched — explicit
    // year text in an email is data. Idempotent: the predicate cannot re-match
    // once cleared.
    conn.execute(
        "UPDATE triage SET deadline = NULL,
                tier = CASE WHEN tier = 'past_due' THEN 'deadline' ELSE tier END
         WHERE deadline IS NOT NULL
           AND message_id IN (
             SELECT d.message_id FROM deadlines d
             JOIN messages m ON m.id = d.message_id
             WHERE d.source IN ('stage1', 'stage2')
               AND julianday(m.received_at) - julianday(d.due_at) > 45
           )",
        [],
    )?;
    conn.execute(
        "DELETE FROM deadlines
         WHERE source IN ('stage1', 'stage2')
           AND id IN (
             SELECT d.id FROM deadlines d
             JOIN messages m ON m.id = d.message_id
             WHERE julianday(m.received_at) - julianday(d.due_at) > 45
           )",
        [],
    )?;
    // THE SENDER DIRECTORY'S BACKFILL. `senders` is kept by the message upsert
    // from here on; an install that predates the table has a whole mailbox of
    // senders the upsert never saw. schema.sql has already created it (empty)
    // by the time this runs, so "empty directory beside a non-empty mailbox" is
    // the just-created signal: filled once, from every listing's own rule
    // (`is_sent = 0 AND is_spam = 0`), with each sender's NEWEST name. A fresh
    // install is both-empty and a no-op that ingest fills; the next open finds
    // rows and leaves them alone. Guarded like every other block here, so the
    // partial-schema unit tests keep passing.
    if tables_exist(conn, &["senders", "messages"])?
        && has_columns(
            conn,
            "messages",
            &[
                "from_addr",
                "from_name",
                "received_at",
                "is_sent",
                "is_spam",
            ],
        )?
    {
        let empty: bool =
            conn.query_row("SELECT NOT EXISTS(SELECT 1 FROM senders)", [], |r| r.get(0))?;
        if empty {
            conn.execute(
                "INSERT INTO senders(account_id, addr, display_name, msg_count, first_seen,
                                     last_received_at)
                 SELECT m.account_id, m.from_addr,
                        (SELECT NULLIF(m2.from_name, '') FROM messages m2
                          WHERE m2.account_id = m.account_id AND m2.from_addr = m.from_addr
                            AND m2.is_sent = 0 AND m2.is_spam = 0
                            AND NULLIF(m2.from_name, '') IS NOT NULL
                          ORDER BY m2.received_at DESC LIMIT 1),
                        COUNT(*), MIN(m.received_at), MAX(m.received_at)
                 FROM messages m
                 WHERE m.is_sent = 0 AND m.is_spam = 0 AND TRIM(m.from_addr) <> ''
                 GROUP BY m.account_id, m.from_addr",
                [],
            )?;
        }
    }

    // A repair, never a reason not to start: it fails as one transaction.
    if let Err(error) = heal_own_mail_read_as_inbound(conn) {
        eprintln!("squelch: own-mail re-read skipped: {error}");
    }

    Ok(())
}

/// Re-read the threads sitting in FYE that the model reasoned about while it
/// was still being shown the user's own mail as inbound (see `OWN_MAIL`). The
/// listings stop showing own mail on their own; what they cannot fix is a
/// verdict REASONED from the mistake, like "reply to <the user>" pinned on a
/// neighbour in the thread, or a thread with real inbound mail whose
/// representative is the user's reply.
///
/// Scoped to threads the model flagged that the user has not resolved, because
/// that is the only place the stale reading is still on screen and each one
/// costs model calls. Every other such row is put right the next time anything
/// enqueues it, by the same path.
///
/// Idempotent through the enqueue itself: authorship is part of a message's
/// content snapshot, so the first pass sees a changed snapshot, files the own
/// message as access-only and re-queues the thread's inbound target the way a
/// fresh sent reply would. Every later open finds the snapshot current, and
/// `heal` is a no-op on unchanged content.
pub(super) fn heal_own_mail_read_as_inbound(conn: &Connection) -> Result<()> {
    if !tables_exist(
        conn,
        &[
            "accounts",
            "messages",
            "triage",
            "agent_message_state",
            "agent_thread_attention",
            "agent_triage_jobs",
            "agent_job_lanes",
        ],
    )? || !has_columns(
        conn,
        "messages",
        &["from_addr", "is_sent", "is_spam", "auth_pass"],
    )? {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    // CROSS JOIN pins the order: this runs on every open, so it walks the
    // threads (one row each) into the thread index, never the whole mailbox.
    // `rep` is the row the FYE list reads a thread's done state from.
    let mut own_as_inbound = {
        let mut stmt = tx.prepare(&format!(
            "SELECT m.account_id,m.id FROM agent_thread_attention a
             LEFT JOIN triage rep ON rep.account_id=a.account_id AND rep.message_id=a.message_id
             CROSS JOIN messages m ON m.account_id=a.account_id AND m.thread_id=a.thread_id
             JOIN agent_message_state s ON s.account_id=m.account_id AND s.message_id=m.id
             WHERE a.show_in_fye=1 AND COALESCE(rep.status,'new')!='done'
               AND m.is_sent=0 AND m.is_spam=0 AND {}",
            super::OWN_MAIL
        ))?;
        stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    own_as_inbound.sort_unstable();
    for (account, message) in own_as_inbound {
        super::agent_triage::enqueue_agent_triage_conn(&tx, account, message, "heal", false)?;
    }
    tx.commit()?;
    Ok(())
}

/// Is `shipment_order_links` there? Readers guard on it because the migration
/// unit tests build partial schemas.
pub(super) fn order_links_exist(conn: &Connection) -> Result<bool> {
    tables_exist(conn, &["shipment_order_links"])
}

/// ORDER LINKS: which orders each package carries, many-to-many, so the
/// listing can fold one purchase's packages into one card.
///
/// NOT `shipment_orders`: that name is the old extractor's staging table for
/// orders with no tracking number yet, a different thing.
///
/// Created HERE rather than in schema.sql because its creation is the one-shot
/// backfill's trigger: schema.sql runs on every open before this function, so
/// a table it created would already exist and the backfill could never tell a
/// first open from the hundredth. On the open that creates it, every shipment
/// the old extractor gave both an order reference and a merchant gets a
/// 'legacy' link. Once, and never again: after this, `shipments.order_merchant`
/// is also written by the agent (a store name, not a domain), and re-running
/// the backfill would pair that with a stale extractor `order_ref`.
///
/// The AFTER DELETE trigger is how every path that deletes a shipment (the
/// agent projection's retirement, the extractor's phantom reaping, anything
/// added later) also deletes its links, without each remembering to.
///
/// ATOMIC, like [`rebuild_fts_if_stale`]: the table, its index, its trigger
/// and the backfill commit together under `BEGIN IMMEDIATE`, and existence is
/// re-checked INSIDE the transaction. Otherwise a backfill that failed halfway
/// would leave a table whose mere existence tells every later open "already
/// done", and the legacy links it never wrote would be lost for good; and two
/// daemons opening one file could both see "absent" and both backfill.
///
/// A legacy row it cannot read or sanitize is SKIPPED (see the body): junk in
/// one row must not cost every other row its link. On a real failure (a busy
/// sibling, an SQL error) it rolls back, says so, and returns Ok: the next open
/// tries again, and every reader and writer of the table guards on
/// [`order_links_exist`].
pub(super) fn create_shipment_order_links(conn: &Connection) -> Result<()> {
    if !tables_exist(conn, &["shipments"])? {
        return Ok(());
    }
    if !order_links_exist(conn)? {
        let created = (|| -> Result<()> {
            conn.execute_batch("BEGIN IMMEDIATE")?;
            // Another handle may have created it between the check above and
            // the write lock; its backfill already ran.
            if !order_links_exist(conn)? {
                create_and_backfill_order_links(conn)?;
            }
            conn.execute_batch("COMMIT")?;
            Ok(())
        })();
        if let Err(e) = created {
            eprintln!("squelch: could not create shipment order links ({e}); will retry next open");
            let _ = conn.execute_batch("ROLLBACK");
            return Ok(());
        }
    }
    // PROVENANCE, added after the table first shipped (a DB that ran the
    // first version of this migration has the table without them). NULL on
    // every pre-existing link, which is exactly "legacy, no message recorded".
    add_column_if_missing(conn, "shipment_order_links", "message_id", "INTEGER")?;
    add_column_if_missing(conn, "shipment_order_links", "merchant_msg", "INTEGER")?;
    Ok(())
}

/// The body of [`create_shipment_order_links`]'s transaction.
fn create_and_backfill_order_links(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE shipment_order_links (
             account_id   INTEGER NOT NULL,
             shipment_id  INTEGER NOT NULL,
             -- lowercase alphanumerics of `merchant`; '' = unknown seller,
             -- which never groups (see triage::order_link).
             merchant_key TEXT NOT NULL,
             -- uppercase alphanumerics of the sanitized `order_ref`.
             order_key    TEXT NOT NULL,
             merchant     TEXT NOT NULL DEFAULT '',
             order_ref    TEXT NOT NULL,
             -- 'agent': rebuilt by reconcile from the retained delivery
             -- proposals. 'legacy': the backfill; reconcile never touches it.
             source       TEXT NOT NULL CHECK (source IN ('agent', 'legacy')),
             -- PROVENANCE, for the agent door's access check: the message that
             -- printed `order_ref`, and the one that named `merchant`. NULL on
             -- a legacy link and (merchant_msg) on a merchant-less one.
             message_id   INTEGER,
             merchant_msg INTEGER,
             PRIMARY KEY (account_id, shipment_id, merchant_key, order_key)
         );
         CREATE INDEX IF NOT EXISTS idx_shipment_order_links_order
             ON shipment_order_links(account_id, merchant_key, order_key);
         CREATE TRIGGER IF NOT EXISTS shipment_order_links_follow_shipment
         AFTER DELETE ON shipments BEGIN
             DELETE FROM shipment_order_links
             WHERE account_id = OLD.account_id AND shipment_id = OLD.id;
         END;",
    )?;
    if !has_columns(conn, "shipments", &["order_ref", "order_merchant"])? {
        return Ok(());
    }
    // A ROW THE BACKFILL CANNOT READ IS SKIPPED, NOT FATAL. A merchant or
    // reference that is not valid text (a number, a blob, bytes that are not
    // UTF-8) is junk this link could not have been built from anyway, and
    // failing the whole backfill on it would fail it on every open, forever,
    // while the table's absence also turns off every agent link write. Only a
    // real SQL error (the `?`s below) rolls the transaction back.
    type LegacyRow = (i64, i64, Option<String>, Option<String>);
    let legacy: Vec<LegacyRow> = {
        let mut stmt = conn.prepare(
            "SELECT account_id, id, order_merchant, order_ref FROM shipments
             WHERE TRIM(COALESCE(order_ref, '')) <> ''
               AND TRIM(COALESCE(order_merchant, '')) <> ''",
        )?;
        let text = |r: &rusqlite::Row<'_>, i: usize| -> rusqlite::Result<Option<String>> {
            Ok(r.get_ref(i)?.as_str().ok().map(str::to_owned))
        };
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, text(r, 2)?, text(r, 3)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (account, id, merchant, raw_ref) in legacy {
        use crate::triage::order_link::{merchant_key, order_key};
        let (Some(merchant), Some(raw_ref)) = (merchant, raw_ref) else {
            continue;
        };
        let Some(order_ref) = crate::triage::extract::shipments::sanitize_order_ref(Some(&raw_ref))
        else {
            continue;
        };
        let (mkey, okey) = (merchant_key(&merchant), order_key(&order_ref));
        if mkey.is_empty() || okey.is_empty() {
            continue;
        }
        conn.execute(
            "INSERT OR IGNORE INTO shipment_order_links
                 (account_id, shipment_id, merchant_key, order_key, merchant, order_ref, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'legacy')",
            params![account, id, mkey, okey, merchant.trim(), order_ref],
        )?;
    }
    Ok(())
}

/// THE `messages_fts` DEFINITION, CUT OUT OF `schema.sql` ITSELF.
///
/// It is one string with two jobs — what a current index must look like, and
/// what the rebuild creates — and it is READ FROM THE SCHEMA rather than
/// written down a second time here, because the second copy is the bug. A
/// hand-maintained list of options to look for ("does it say porter") notices
/// only the options somebody remembered to list: the prefix index was added to
/// this line and the check that was supposed to guard it went on passing,
/// which is how a mailbox ends up serving search off an index nobody meant to
/// ship. There is nothing to keep in step now; `schema.sql` IS the check.
///
/// `None` only if the statement cannot be found at all, which the unit tests
/// make impossible to ship — the schema is compiled in, so this cannot depend
/// on anything a mailbox did.
pub(super) fn canonical_fts_create() -> Option<&'static str> {
    static PARSED: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    *PARSED.get_or_init(|| {
        let start = SCHEMA.find("CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts")?;
        // Through the closing paren of the fts5 argument list, without the
        // statement terminator: that is the shape SQLite stores.
        let end = start + SCHEMA[start..].find(");")? + 1;
        Some(SCHEMA[start..end].trim())
    })
}

/// Fold two `CREATE VIRTUAL TABLE` texts to something comparable: lowercased,
/// every run of whitespace one space, and `IF NOT EXISTS` gone.
///
/// Those three are exactly the differences SQLite itself introduces. It stores
/// the statement's own text in `sqlite_master`, verbatim apart from dropping
/// `IF NOT EXISTS` and the terminator, so a definition that differs from
/// `schema.sql`'s in ANY other way — a tokenizer, a prefix list, a column
/// order, a column name — is a different index and has to be rebuilt.
pub(super) fn normalize_fts_sql(sql: &str) -> String {
    let folded = sql.to_ascii_lowercase();
    let collapsed = folded.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.replace("if not exists ", "")
}

/// THE FTS INDEX'S OPTIONS ARE NOT ALTERABLE, so a change to them is a
/// DROP + CREATE + refill.
///
/// `messages_fts` was created with fts5's default tokenizer and no prefix
/// index, so the index held whichever form of a word the sender happened to
/// type (a reader searching "password" found nothing in the two messages that
/// say "passwords") and every as-you-type prefix walked the whole term list.
/// `schema.sql` now asks for `porter unicode61` and `prefix = '2 3'`, but its
/// `CREATE VIRTUAL TABLE IF NOT EXISTS` is a no-op against a table that already
/// exists, so the old definition would live forever on every mailbox that
/// predates those lines.
///
/// Refilling from `messages(subject, body)` plus the served translation is
/// exact, not approximate: every FTS writer goes through the same
/// `translations::FTS_ROW_SELECT`, so this reproduces the index rather than
/// reinterpreting it. The translation columns (`tsubject`, `tbody`) arrived
/// this way: a column list that differs from `schema.sql`'s is a rebuild.
///
/// THE TRIGGER IS ANY DIFFERENCE AT ALL between the CREATE text in
/// `sqlite_master` — what SQLite itself stored when the table was made — and
/// [`canonical_fts_create`], compared through [`normalize_fts_sql`]. Not a list
/// of options to look for: a list only ever notices the options somebody
/// remembered to add to it, and the next edit to that line in `schema.sql`
/// would ship silently unmigrated, which is a mailbox searching an index that
/// does not match its own schema and no way to tell from the outside.
///
/// Idempotent by construction: the rebuild creates the canonical definition, so
/// the next open compares it against itself and does nothing. Guarded on both
/// tables existing, because the migration unit tests build partial schemas.
///
/// TWO THINGS ABOUT THE TRANSACTION, both of them about the fact that this repo
/// really does open one DB from two processes (`squelchd token issue` and
/// friends run while `serve` holds the file):
///
/// - `BEGIN IMMEDIATE`, not a bare `BEGIN`. A deferred transaction takes its
///   write lock at the `DROP`, mid-batch, so a busy daemon means the batch
///   fails with the index already gone from this connection's view of the
///   world. Immediate takes the lock up front and fails before anything is
///   dropped.
/// - A FAILED REBUILD IS NOT A FAILED MIGRATION. Returning the error here would
///   propagate out of `init` and make a CLI command refuse to run because
///   another process happened to be writing. The old index still works, just
///   unstemmed, so the honest response is to say so and carry on; the next open
///   tries again.
///
/// COST: the rebuild holds the write lock for one full-mailbox `INSERT..SELECT`
/// (roughly half a second per 1,500 messages, so linear and measurable in
/// minutes on a very large mailbox). The first open after this ships is the
/// slow one and nothing else can write during it.
pub(super) fn rebuild_fts_if_stale(conn: &Connection) -> Result<()> {
    if !tables_exist(conn, &["messages_fts", "messages"])? {
        return Ok(());
    }
    let Some(canonical) = canonical_fts_create() else {
        // Unreachable in a shipped build (the schema is compiled in and a unit
        // test reads this statement out of it), and if it ever were reached,
        // rebuilding against a definition we could not find is the worse guess.
        eprintln!("squelch: no messages_fts definition in the schema; leaving the index alone");
        return Ok(());
    };
    let created: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'messages_fts'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    // A NULL/absent definition reads as "already current": there is nothing to
    // act on, and rebuilding an index we cannot describe is the riskier guess.
    let current = match created.as_deref() {
        Some(sql) => normalize_fts_sql(sql) == normalize_fts_sql(canonical),
        None => true,
    };
    if current {
        return Ok(());
    }
    // `IF NOT EXISTS` rides along in the canonical text and is a no-op one
    // statement after the DROP; stripping it would mean editing the string,
    // which is how the two copies started drifting in the first place.
    // The refill is the store's own FTS row SELECT, translations included.
    // A partial schema (the migration unit tests build them) may lack
    // `message_translations`; its rows then refill with no translation, which
    // is what such a mailbox has.
    let refill = if tables_exist(conn, &["message_translations"])? {
        super::translations::FTS_ROW_SELECT.to_string()
    } else {
        "SELECT id, subject, body, NULL, NULL FROM messages m".to_string()
    };
    let batch = format!(
        "BEGIN IMMEDIATE;
         DROP TABLE messages_fts;
         {canonical};
         INSERT INTO messages_fts(rowid, subject, body, tsubject, tbody)
             {refill};
         COMMIT;"
    );
    if let Err(e) = conn.execute_batch(&batch) {
        // The connection is dropped or rolled back by rusqlite, so the old
        // index is still there and still serves. Say which mailbox and why, so
        // the operator can tell this apart from search being broken.
        eprintln!("squelch: could not rebuild the search index ({e}); keeping the old one");
        let _ = conn.execute_batch("ROLLBACK");
    }
    Ok(())
}

/// ONE-SHOT at the `order_merchant` introduction: stamp each order-bearing
/// shipment with the registrable domain of the message that CREATED it, so
/// pre-existing rows stay reachable by a later order mail from the same shop.
///
/// Runs in Rust rather than SQL because the merchant key is the shared
/// registrable-domain rule ([`super::specialists::merchant_key`]) — a second,
/// drifting definition in SQL is exactly the bug the namespacing exists to stop.
/// Rows whose creator is unknown or has no derivable domain are LEFT NULL: they
/// then match no order mail at all, which loses a name donation but can never
/// bind one merchant's product to another's package.
/// Record, per existing account, the moment the open ledger started running.
///
/// WITHOUT THIS THE FIRST NUMBER IS A LIE, and it is a flattering one. An
/// established mailbox has years of mail and, the instant `opened_at` is added,
/// ZERO opens - so a rate computed over "the last 90 days of mail" divides a
/// real denominator by an empty numerator and reports that its owner opens
/// almost nothing. The sample floors in `sharing::share_stat` do not catch it:
/// they measure the age of the MAIL, which is ample, not the age of the LEDGER,
/// which is seconds.
///
/// So the ledger dates itself, and `Store::share_open_rate` refuses to look
/// back further than this. An account created AFTER this migration gets no row
/// and needs none: it has been recording since it existed, which is what its
/// own `created_at` says.
///
/// `INSERT OR IGNORE`, so a re-run cannot move a stamp that is already the
/// truth - which would restart the clock and hide the number for another month.
fn stamp_open_ledger_start(conn: &Connection) -> Result<()> {
    stamp_ledger_start(conn, OPEN_LEDGER_SINCE_KEY)
}

/// Record, per existing account, the moment the DWELL ledger started running.
///
/// The same lie the open ledger guards against, one column over. A mailbox
/// that has been recording opens for months has, the instant `dwell_ms` is
/// added, a pile of opened mail with zero dwell and zero clicks. Read naively
/// that says its owner glances at everything for no time at all and never
/// follows a link, which would rank every newsletter they actually read as a
/// purge candidate. So dwell and click statistics count only mail OPENED at or
/// after this stamp (`attention::dwell_ledger_since`); an account created
/// later has reported dwell since it existed and falls back to `created_at`.
fn stamp_dwell_ledger_start(conn: &Connection) -> Result<()> {
    stamp_ledger_start(conn, DWELL_LEDGER_SINCE_KEY)
}

/// The shared body of the two ledger stamps: one `app_settings` row per
/// existing account, written once.
fn stamp_ledger_start(conn: &Connection, key: &str) -> Result<()> {
    // BOTH TABLES OR NEITHER. In production this is never false: `schema.sql`
    // creates both and runs in full before this function does. It is false only
    // in the migration suite, which builds deliberately partial old schemas to
    // migrate FROM, and a stamp into a table that does not exist there would
    // fail a test about something else entirely.
    if !tables_exist(conn, &["app_settings", "accounts"])? {
        return Ok(());
    }
    conn.execute(
        "INSERT OR IGNORE INTO app_settings(account_id, key, value)
         SELECT id, ?1, ?2 FROM accounts",
        rusqlite::params![key, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

fn backfill_order_merchant(conn: &Connection) -> Result<()> {
    if !has_columns(
        conn,
        "shipments",
        &["order_ref", "order_merchant", "created_by_message_id"],
    )? || !has_columns(conn, "messages", &["from_addr"])?
    {
        return Ok(());
    }
    let rows: Vec<(i64, String)> = conn
        .prepare(
            "SELECT s.id, m.from_addr FROM shipments s
             JOIN messages m ON m.id = s.created_by_message_id AND m.account_id = s.account_id
             WHERE s.order_ref IS NOT NULL AND s.order_merchant IS NULL",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (id, from_addr) in rows {
        let merchant = super::specialists::merchant_key(&from_addr);
        if !merchant.is_empty() {
            conn.execute(
                "UPDATE shipments SET order_merchant = ?2 WHERE id = ?1",
                params![id, merchant],
            )?;
        }
    }
    Ok(())
}

/// How far back the shipments-trigger backfill looks. A package older than this
/// has either arrived or been written off; extracting it spends model budget to
/// resurrect a card nobody wants.
const SHIP_BACKFILL_DAYS: i64 = 30;

/// HARD CAP on rows the backfill may pend, applied to the candidate SELECT
/// itself and not just to the matches. The extractor is a paid per-row model
/// call, so the worst case a migration can commit the user to is this many —
/// a busy 30-day mailbox stays bounded, and anything the cap misses still gets
/// picked up by the ongoing ingest trigger the next time that seller writes.
const SHIP_BACKFILL_MAX: usize = 500;

/// ONE-SHOT at the `ship_extract_model` introduction: pend the recent mail that
/// carries a loose shipping signal, so the shipments extractor has history to
/// work from instead of only new arrivals.
///
/// The signal test is the SAME Rust predicate ingest uses
/// ([`has_loose_shipping_signal`](crate::triage::shipment::has_loose_shipping_signal)),
/// run row by row rather than reimplemented as SQL LIKEs — a second, drifting
/// definition of "shipping mail" is exactly the bug this column exists to avoid.
fn ship_extract_backfill(conn: &Connection) -> Result<()> {
    // Column-level guard, not just table-level: this SELECT names the message
    // surfaces the signal test reads, and a partial-schema DB that lacks one
    // would fail the whole open rather than skip a backfill.
    if !has_columns(
        conn,
        "messages",
        &["from_addr", "subject", "body", "received_at", "is_sent"],
    )? || !tables_exist(conn, &["triage"])?
    {
        return Ok(());
    }
    let cutoff = (Utc::now() - chrono::Duration::days(SHIP_BACKFILL_DAYS)).to_rfc3339();
    // Newest first so the cap keeps the mail most likely to still be in flight.
    let candidates: Vec<(i64, String, String, String)> = conn
        .prepare(
            "SELECT m.id, m.from_addr, m.subject, m.body
             FROM messages m
             JOIN triage t ON t.message_id = m.id
             WHERE m.account_id = t.account_id
               AND m.is_sent = 0
               AND m.received_at >= ?1
               AND COALESCE(t.sensitivity, 'normal') = 'normal'
             ORDER BY m.received_at DESC
             LIMIT ?2",
        )?
        .query_map(params![cutoff, SHIP_BACKFILL_MAX as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    for (id, from_addr, subject, body) in candidates {
        if crate::triage::shipment::has_loose_shipping_signal(&from_addr, &subject, &body) {
            conn.execute(
                "UPDATE triage SET ship_extract_model = 'pending' WHERE message_id = ?1",
                params![id],
            )?;
        }
    }
    Ok(())
}

/// ONE-SHOT, same trigger: un-stick the rows the DISPATCH BUG stranded.
///
/// While `extractable_categories` queued a category that `extractor_for_category`
/// did not handle, the extract pass stamped those rows `'skip-no-extractor'` —
/// a PROCESSED marker. The dispatch is fixed, but the marker is permanent, so
/// those rows (the whole marketing corpus of that era) would never re-queue.
/// NULLing it puts them back in the extract queue, and only for categories that
/// really do have an extractor now: a `'skip-no-extractor'` on a genuinely
/// unhandled category is an honest verdict and stays.
fn clear_stale_no_extractor_markers(conn: &Connection) -> Result<()> {
    if !tables_exist(conn, &["triage"])? {
        return Ok(());
    }
    let stranded: Vec<String> = conn
        .prepare(
            "SELECT DISTINCT COALESCE(category, '') FROM triage
             WHERE extractor_model_used = 'skip-no-extractor'",
        )?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let tx = conn.unchecked_transaction()?;
    for category in stranded {
        if crate::triage::extract::extractor_for_category(&category).is_some() {
            tx.execute(
                "UPDATE triage SET extractor_model_used = NULL
                 WHERE extractor_model_used = 'skip-no-extractor' AND category = ?1",
                params![category],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Sent mail by account, which [`backfill_message_recipients`] reads its
/// candidates from: the account and the row id are all it needs, so nothing
/// else is carried.
const SENT_INDEX_SQL: &str = "CREATE INDEX IF NOT EXISTS idx_messages_sent
     ON messages(account_id) WHERE is_sent = 1";

/// [`backfill_message_recipients`]' candidates: sent mail with recipients
/// stored and no index rows. The sent rows lacking index rows are found from
/// `idx_messages_sent` and `message_recipients`' key alone, and only they are
/// fetched from the table. Pinned both ways: without the `INDEXED BY` a store
/// with no `sqlite_stat1` is free to walk the table for `is_sent` again, and
/// the `CROSS JOIN` keeps the fetch a rowid lookup per candidate whatever the
/// statistics say.
pub(super) const RECIPIENTS_CANDIDATES_SQL: &str = "SELECT m.id, m.account_id,
        COALESCE(m.to_addrs, ''), COALESCE(m.cc_addrs, ''), COALESCE(m.bcc_addrs, '')
     FROM messages s INDEXED BY idx_messages_sent
     CROSS JOIN messages m ON m.id = s.id
     WHERE s.is_sent = 1
       AND NOT EXISTS (
           SELECT 1 FROM message_recipients r
           WHERE r.account_id = s.account_id AND r.message_id = s.id)
       AND (m.to_addrs IS NOT NULL OR m.cc_addrs IS NOT NULL)
       AND (COALESCE(m.to_addrs, '') != ''
            OR COALESCE(m.cc_addrs, '') != ''
            OR COALESCE(m.bcc_addrs, '') != '')";

/// EVERY OPEN: fill `message_recipients` from the recipient display strings
/// already on disk for any sent row that has none, under one commit, so an
/// open that dies halfway leaves nothing and the next one does it all again.
///
/// Local and parse-only — no network, unlike the recipients sweep in the sync
/// engine that produced those strings in the first place. Rows with no
/// recipients stored at all are not candidates here: they are that sweep's
/// queue, and it writes the index itself as it fills them.
///
/// ALL THREE HEADERS, because the index answers "did this message go to that
/// address" and a Cc'd or Bcc'd recipient received the mail exactly like a To'd
/// one. Pre-split rows carry all three in `to_addrs` already (it held To+Cc+Bcc
/// mashed together), and the concatenation below is a no-op on them.
///
/// EMPTY strings ("looked, and the headers named nobody") are not candidates.
/// A stored string that parses to no address yields no rows and so stays one,
/// read again on each open: a handful of rows at most.
///
/// THE CANDIDATES COME FROM TWO INDEXES, never the table. `is_sent` and the
/// recipient columns sit after the bodies, so a predicate on them reads past
/// every row's overflow chain: 38-50 ms of every open at 50k messages, and a
/// read of the whole table on a cold file, to find nothing. The sent mail
/// lacking index rows is read from `idx_messages_sent` and the recipient
/// index's own key, and only those rows are fetched.
///
/// THE INDEX IS BUILT HERE, beside its one reader, the first time an open
/// finds it missing. That one build reads the table, as every open used to,
/// and holds the write lock while it does. NEVER A REASON NOT TO START: a
/// build that fails (a sibling process holding the lock past the busy
/// timeout) skips the heal for this open, and the next open builds it.
fn backfill_message_recipients(conn: &Connection) -> Result<()> {
    if !tables_exist(conn, &["messages", "message_recipients"])?
        || !has_columns(
            conn,
            "messages",
            &["account_id", "to_addrs", "cc_addrs", "bcc_addrs", "is_sent"],
        )?
    {
        return Ok(());
    }
    if let Err(error) = conn.execute(SENT_INDEX_SQL, []) {
        eprintln!("squelch: sent index not built, recipients backfill skipped: {error}");
        return Ok(());
    }
    let rows: Vec<(i64, i64, String, String, String)> = conn
        .prepare(RECIPIENTS_CANDIDATES_SQL)?
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    // One commit for the whole backfill: a row per recipient, each its own
    // fsync in autocommit, over every sent message on a first open.
    let tx = conn.unchecked_transaction()?;
    for (message_id, account_id, to, cc, bcc) in rows {
        let joined = crate::sync::ingest::join_recipient_lists(&[&to, &cc, &bcc]);
        for addr in crate::sync::ingest::parse_stored_recipients(&joined) {
            tx.execute(
                "INSERT OR IGNORE INTO message_recipients(account_id, message_id, addr)
                 VALUES(?1,?2,?3)",
                params![account_id, message_id, addr],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// The `app_settings` key marking an account whose ghosts have been rebuilt
/// by [`rebuild_ghosts_for_zoned_times`].
pub(super) const GHOST_ZONED_TIMES_KEY: &str = "ghost_zoned_times_rebuilt";

/// ONE-TIME: rebuild every ghost so it reads the event times its stored
/// decisions already hold.
///
/// The model writes most event times as a local time and a zone
/// (`2026-10-19T21:30` in `Asia/Tokyo`), and the projection read only RFC3339
/// and bare dates, so it dropped those starts and then their ends: a flight
/// ghost with no times at all. The decisions kept the values, so
/// [`ghosts::rebuild`], which re-derives a ghost's facts from its assigned
/// records' stored decisions through the fixed `launder`, restores them. It
/// assigns nothing new, which is enough: a record that fed no ghost for want
/// of a start is matched the next time its message is decided.
///
/// GATED BY A MARKER per account in `app_settings`, since there is no column
/// to add and so no "the column was absent" signal. The marker is written for
/// every account in the same transaction as the rebuilds, under `BEGIN
/// IMMEDIATE` with the marker read inside it, so squelchd and squelch-mcp
/// opening one file do not both run it. An account created later has no
/// marker and has its ghosts rebuilt once on the next open, which is harmless:
/// a rebuild of a ghost built by the fixed code changes nothing that matters.
///
/// NEVER A REASON NOT TO START. One ghost that fails to rebuild is rolled back
/// to its own savepoint, logged by id and error kind (never its title or
/// place), and skipped for good; anything that fails the whole pass rolls it
/// back to retry on the next open. Ghosts number tens to hundreds an account,
/// so one pass is cheap.
fn rebuild_ghosts_for_zoned_times(conn: &Connection) {
    match rebuild_ghosts_once(conn) {
        Ok((0, _)) => {}
        Ok((rebuilt, skipped)) => eprintln!(
            "squelch: migration rebuilt {rebuilt} ghost(s) to read zoned event times ({skipped} skipped)"
        ),
        Err(error) => {
            eprintln!(
                "squelch: ghost time rebuild failed ({}); will retry next open",
                ghosts::error_kind(&error)
            );
            if !conn.is_autocommit() {
                let _ = conn.execute_batch("ROLLBACK");
            }
        }
    }
}

/// The body of [`rebuild_ghosts_for_zoned_times`]: (rebuilt, skipped).
fn rebuild_ghosts_once(conn: &Connection) -> Result<(usize, usize)> {
    // Every table `ghosts::rebuild` reads or writes. The migration suite
    // builds partial schemas on purpose; production always has them all.
    if !tables_exist(
        conn,
        &[
            "accounts",
            "app_settings",
            "ghost_events",
            "ghost_event_sources",
            "ghost_event_keys",
            "agent_message_decisions",
            "agent_message_state",
            "messages",
        ],
    )? {
        return Ok((0, 0));
    }
    let pending = |conn: &Connection| -> Result<Vec<i64>> {
        let mut stmt = conn.prepare(
            "SELECT id FROM accounts WHERE NOT EXISTS(
                 SELECT 1 FROM app_settings s WHERE s.account_id=accounts.id AND s.key=?1)
             ORDER BY id",
        )?;
        Ok(stmt
            .query_map([GHOST_ZONED_TIMES_KEY], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?)
    };
    // The converged state, on every open after the first: one indexed read,
    // no write lock.
    if pending(conn)?.is_empty() {
        return Ok((0, 0));
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let now = Utc::now().to_rfc3339();
    let (mut rebuilt, mut skipped) = (0, 0);
    // Re-read under the lock: another handle may have done it meanwhile.
    for account in pending(conn)? {
        let ids: Vec<i64> = {
            let mut stmt =
                conn.prepare("SELECT id FROM ghost_events WHERE account_id=?1 ORDER BY id")?;
            stmt.query_map([account], |r| r.get(0))?
                .collect::<std::result::Result<_, _>>()?
        };
        for id in ids {
            conn.execute_batch("SAVEPOINT ghost_time_rebuild")?;
            match ghosts::rebuild(conn, account, id, &now) {
                Ok(()) => {
                    conn.execute_batch("RELEASE ghost_time_rebuild")?;
                    rebuilt += 1;
                }
                // The transaction itself is gone (see `ghosts::guarded`):
                // nothing after this could commit as one, so stop.
                Err(error) if conn.is_autocommit() => return Err(error),
                Err(error) => {
                    eprintln!(
                        "squelch: ghost time rebuild skipped account {account} ghost {id} ({})",
                        ghosts::error_kind(&error)
                    );
                    conn.execute_batch(
                        "ROLLBACK TO ghost_time_rebuild; RELEASE ghost_time_rebuild",
                    )?;
                    skipped += 1;
                }
            }
        }
        conn.execute(
            "INSERT OR IGNORE INTO app_settings(account_id, key, value) VALUES(?1, ?2, ?3)",
            params![account, GHOST_ZONED_TIMES_KEY, now],
        )?;
    }
    conn.execute_batch("COMMIT")?;
    Ok((rebuilt, skipped))
}

/// The `app_settings` key marking an account whose stored Amazon decisions
/// have been projected by [`project_amazon_boxes`].
pub(super) const AMAZON_BOXES_KEY: &str = "amazon_boxes_projected";

/// ONE-TIME: put every Amazon box the stored decisions already describe on
/// the Shipments rail (`agent_deliveries::project_amazon_boxes`). Before box
/// keys existed, every Amazon Shipped/Delivered record had no tracking number
/// and projected nothing, so the rail had never shown an Amazon package.
///
/// Gated per account by a marker in `app_settings`, written in the same
/// `BEGIN IMMEDIATE` transaction as the projection and re-read inside it, the
/// way [`rebuild_ghosts_for_zoned_times`] is. Running it twice is harmless
/// (a rebuild is idempotent); the marker only spares the scan.
///
/// NEVER A REASON NOT TO START: a failure rolls the pass back, is logged with
/// no mail content, and retries on the next open.
fn project_amazon_boxes(conn: &Connection) {
    match project_amazon_boxes_once(conn) {
        Ok(0) => {}
        Ok(boxes) => eprintln!("squelch: migration projected {boxes} Amazon package(s)"),
        Err(error) => {
            eprintln!("squelch: Amazon package backfill failed ({error}); will retry next open");
            if !conn.is_autocommit() {
                let _ = conn.execute_batch("ROLLBACK");
            }
        }
    }
}

fn project_amazon_boxes_once(conn: &Connection) -> Result<usize> {
    once_per_account(
        conn,
        AMAZON_BOXES_KEY,
        agent_deliveries::project_amazon_boxes,
    )
}

/// The `app_settings` key marking an account whose stored drop-off receipts
/// have lost their delivery records ([`drop_sent_packages`]).
pub(super) const SENT_PACKAGES_KEY: &str = "sent_packages_dropped";

/// ONE-TIME: take a shipping store's drop-off receipt off the Shipments rail
/// (`agent_deliveries::retire_sent_packages`). Triage used to file the
/// receipt's tracking number as a package on its way, but the box is going
/// away from the owner and will never get another email. New receipts are
/// stripped as they are decided; this cleans the ones already stored.
///
/// Gated and failure-tolerant exactly as [`project_amazon_boxes`] is.
fn drop_sent_packages(conn: &Connection) {
    match once_per_account(
        conn,
        SENT_PACKAGES_KEY,
        agent_deliveries::retire_sent_packages,
    ) {
        Ok(0) => {}
        Ok(n) => eprintln!("squelch: migration retired {n} dropped-off package receipt(s)"),
        Err(error) => {
            eprintln!(
                "squelch: dropped-off package cleanup failed ({error}); will retry next open"
            );
            if !conn.is_autocommit() {
                let _ = conn.execute_batch("ROLLBACK");
            }
        }
    }
}

/// Run `pass` once for every account not yet marked with `key`, marking each
/// in the same `BEGIN IMMEDIATE` transaction (re-read inside it). Returns the
/// sum of what `pass` reported.
fn once_per_account(
    conn: &Connection,
    key: &str,
    pass: fn(&Connection, AccountId) -> Result<usize>,
) -> Result<usize> {
    if !tables_exist(
        conn,
        &[
            "accounts",
            "app_settings",
            "agent_message_decisions",
            "agent_delivery_projections",
            "messages",
            "shipments",
        ],
    )? {
        return Ok(0);
    }
    let pending = |conn: &Connection| -> Result<Vec<AccountId>> {
        let mut stmt = conn.prepare(
            "SELECT id FROM accounts WHERE NOT EXISTS(
                 SELECT 1 FROM app_settings s WHERE s.account_id=accounts.id AND s.key=?1)
             ORDER BY id",
        )?;
        Ok(stmt
            .query_map([key], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?)
    };
    if pending(conn)?.is_empty() {
        return Ok(0);
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let now = Utc::now().to_rfc3339();
    let mut total = 0;
    for account in pending(conn)? {
        total += pass(conn, account)?;
        conn.execute(
            "INSERT OR IGNORE INTO app_settings(account_id, key, value) VALUES(?1, ?2, ?3)",
            params![account, key, now],
        )?;
    }
    conn.execute_batch("COMMIT")?;
    Ok(total)
}

/// Re-run [`banking::sanitize_institution`] over every stored institution,
/// clearing or shortening the ones it now refuses. See the call site in
/// [`migrate`] for why this exists and why it is written in Rust.
///
/// A refusal writes NULL rather than deleting the banking row: the amount, the
/// kind and the card tail on that row are all still good, and the client falls
/// back to the sender's display name for the label, so the card keeps reading
/// "Venmo" without the paragraph after it.
fn resanitize_institutions(conn: &Connection) -> Result<usize> {
    use crate::triage::extract::banking::sanitize_institution;

    if !tables_exist(conn, &["banking"])? || !has_columns(conn, "banking", &["institution"])? {
        return Ok(0);
    }
    // ONE TRANSACTION, not one per row. These were bare autocommit `execute`s,
    // which on a real WAL file cost 1.02s for 20,000 rewrites against 43ms
    // inside a transaction — and left the table half-repaired on a crash
    // mid-loop.
    //
    // The two other one-shots in THIS file (`backfill_message_recipients` and
    // `clear_stale_no_extractor_markers`) take one too, as do the store-side
    // cleanups in `specialists.rs`.
    // `unchecked_transaction` rather than `transaction`, so this stays a
    // `&Connection` and the whole migrate() chain keeps its signature.
    let tx = conn.unchecked_transaction()?;
    let rows: Vec<(i64, String)> = {
        let mut stmt =
            tx.prepare("SELECT id, institution FROM banking WHERE institution IS NOT NULL")?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut changed = 0usize;
    for (id, stored) in rows {
        let clean = sanitize_institution(Some(&stored));
        if clean.as_deref() == Some(stored.as_str()) {
            continue;
        }
        changed += tx.execute(
            "UPDATE banking SET institution = ?2 WHERE id = ?1",
            rusqlite::params![id, clean],
        )?;
    }
    tx.commit()?;
    Ok(changed)
}

/// SELF-EXTINGUISHING: un-stamp every row an extractor refused for having no
/// body, whose message HAS a body now.
///
/// [`crate::triage::extract::RowAction::NoBody`] marks a row processed so an
/// empty body is never billed for a guess, and that stamp is terminal —
/// `extract_queue` selects on `extractor_model_used IS NULL`, and only a
/// hand-requested re-triage clears it. But the ingest fix that shipped
/// ALONGSIDE that guard is what makes those bodies recoverable: a re-fetch
/// heals `messages.body` and touches no triage column, so the row is left
/// permanently retired with the very content it was waiting for sitting in it.
///
/// That is not hypothetical. A sender shipping an empty text/plain part beside
/// a full HTML one stored `body = ''` for its whole history (39 of 39 Venmo
/// messages in one real mailbox), and every extract tick between the daemon
/// roll and the re-fetch stamps another one.
///
/// BLANKNESS IS DECIDED BY [`crate::triage::text::is_blank`], THE FUNCTION THE
/// EXTRACTOR ITSELF USES, and that is the whole reason this reads rows into
/// Rust instead of expressing the predicate in SQL. The first cut wrote
/// `TRIM(body) <> ''`, and SQLite's one-argument `TRIM` strips U+0020 and
/// nothing else — not a tab, not a newline, not the zero-width set `is_blank`
/// discounts. An "empty" text/plain part on the wire is `"\r\n"`, not `""`,
/// which is precisely the mail this pass exists for: every such row was
/// un-stamped here, refused again by the extractor, and un-stamped again on the
/// next open, forever. The rule was already written fifteen lines up for the
/// sibling pass and this function broke it — a second definition of a predicate
/// drifts from the one that matters, and this one drifted before it shipped.
///
/// CANDIDATES ARE THE STAMPED ROWS, NOT EVERY MESSAGE. Filtering in SQL meant
/// `message_id IN (SELECT id FROM messages WHERE ...)`, which plans as a full
/// scan of every body in the mailbox on every store open — 1.04s on a 50k
/// message database, in the converged state where it changes nothing, and the
/// store is opened by squelchd and squelch-mcp alike. Reading the marker first
/// bounds the work to rows that could possibly qualify.
fn clear_recoverable_no_body_markers(conn: &Connection) -> Result<usize> {
    // `messages.body` is as much a read here as the marker column, and the
    // migration suite builds partial schemas on purpose, so a SELECT naming a
    // column install never had is a hard SQL error that fails the whole open.
    // (`triage.extractor_model_used` needs no such check: it is added earlier in
    // this same function whenever the table exists at all.)
    if !tables_exist(conn, &["triage", "messages"])? || !has_columns(conn, "messages", &["body"])? {
        return Ok(0);
    }

    let candidates: Vec<(i64, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT t.message_id, m.body
             FROM triage t
             JOIN messages m ON m.id = t.message_id
             WHERE t.extractor_model_used = ?1",
        )?;
        stmt.query_map(rusqlite::params![crate::triage::NO_BODY_SKIP_MODEL], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
    };

    // A NULL body is still nothing to read, and reads as blank rather than as a
    // recovery.
    let recoverable: Vec<i64> = candidates
        .into_iter()
        .filter(|(_, body)| !crate::triage::text::is_blank(body.as_deref().unwrap_or("")))
        .map(|(id, _)| id)
        .collect();
    if recoverable.is_empty() {
        return Ok(0);
    }

    let tx = conn.unchecked_transaction()?;
    let mut cleared = 0usize;
    for id in recoverable {
        // The marker is re-checked in the UPDATE so a row that changed hands
        // between the read and the write is not stamped over.
        cleared += tx.execute(
            "UPDATE triage SET extractor_model_used = NULL
             WHERE message_id = ?1 AND extractor_model_used = ?2",
            rusqlite::params![id, crate::triage::NO_BODY_SKIP_MODEL],
        )?;
    }
    tx.commit()?;
    Ok(cleared)
}

/// Drop re-triage work queued by sender-rule changes. Rules now apply only to
/// mail that arrives after them (see `rules.rs`), and a backlog left by the old
/// fan-out would otherwise keep draining the background budget for days: one
/// `*` rule queued ~1,000 investigations. Every such job targets a message that
/// already has a decision, so closing it leaves that placement as it was.
/// Marked `completed` like a superseded revision, so lane and follow-up rows
/// keep a valid job. Idempotent: nothing enqueues this trigger any more.
pub(super) fn retire_preference_retriage(conn: &Connection) -> Result<()> {
    if !tables_exist(conn, &["agent_triage_jobs"])? {
        return Ok(());
    }
    conn.execute(
        "UPDATE agent_triage_jobs SET state='completed',lease_token=NULL,lease_until=NULL
         WHERE state='queued' AND trigger LIKE 'preference:%'",
        [],
    )?;
    Ok(())
}

/// `calendar_reminder_fires` GREW `covers` and became AUTOINCREMENT (a fire
/// id the phone fetches by must never be reused). Neither can be ALTERed in,
/// so a table without `covers` is DROPPED and recreated, discarding its rows:
/// it only ever existed on development databases of the unreleased branch
/// that made it, and a row is a week-long record of a banner already shown.
/// Guarded on the column, so it runs at most once per DB; schema.sql already
/// ran this open, hence the CREATE here.
pub(super) fn rebuild_calendar_reminder_fires(conn: &Connection) -> Result<()> {
    if !tables_exist(conn, &["calendar_reminder_fires"])?
        || has_columns(conn, "calendar_reminder_fires", &["covers"])?
    {
        return Ok(());
    }
    conn.execute_batch(
        "DROP TABLE calendar_reminder_fires;
         CREATE TABLE calendar_reminder_fires (
             id         INTEGER PRIMARY KEY AUTOINCREMENT,
             account_id INTEGER NOT NULL,
             item_key   TEXT NOT NULL,
             start_key  TEXT NOT NULL,
             fired_at   INTEGER NOT NULL,
             covers     TEXT NOT NULL DEFAULT '[]',
             frame      TEXT NOT NULL,
             UNIQUE(account_id, item_key, start_key)
         );",
    )?;
    Ok(())
}

/// Retire the old independent placement without dropping typed record facts.
/// Old generic placements are queued for investigation rather than relabeled
/// by guessing. The transaction makes cleanup and requeue atomic and idempotent.
pub(super) fn retire_records_destination(conn: &Connection) -> Result<()> {
    if !tables_exist(
        conn,
        &[
            "agent_message_decisions",
            "agent_message_destinations",
            "agent_triage_corrections",
            "agent_triage_jobs",
            "agent_message_state",
            "agent_job_lanes",
        ],
    )? {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    let mut orphaned = Vec::new();
    {
        let mut stmt = tx.prepare("SELECT account_id,message_id,decision_json FROM agent_message_decisions
            WHERE EXISTS(SELECT 1 FROM json_each(decision_json,'$.destinations') WHERE value='records')")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (account, message, encoded) in rows {
            let mut value: serde_json::Value =
                serde_json::from_str(&encoded).map_err(|e| CoreError::Other(e.into()))?;
            if let Some(destinations) = value["destinations"].as_array_mut() {
                destinations.retain(|v| v != "records");
            }
            if value["records"]
                .as_array()
                .is_none_or(|records| records.is_empty())
            {
                orphaned.push((account, message));
            }
            tx.execute("UPDATE agent_message_decisions SET decision_json=?3 WHERE account_id=?1 AND message_id=?2",
                params![account,message,value.to_string()])?;
        }
    }
    // Corrections can be a full list or an add/remove delta. Preserve Reading
    // preferences, but a legacy Records correction can no longer restore it.
    {
        let mut stmt = tx.prepare(
            "SELECT rowid,value_json FROM agent_triage_corrections WHERE field='destinations'",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (id, encoded) in rows {
            let mut value: serde_json::Value =
                serde_json::from_str(&encoded).map_err(|e| CoreError::Other(e.into()))?;
            if let Some(values) = value.as_array_mut() {
                values.retain(|v| v != "records");
            }
            for key in ["add", "remove"] {
                if let Some(values) = value.get_mut(key).and_then(serde_json::Value::as_array_mut) {
                    values.retain(|v| v != "records");
                }
            }
            let updated = value.to_string();
            if updated != encoded {
                tx.execute(
                    "UPDATE agent_triage_corrections SET value_json=?2 WHERE rowid=?1",
                    params![id, updated],
                )?;
            }
        }
    }
    tx.execute(
        "DELETE FROM agent_message_destinations WHERE destination='records'",
        [],
    )?;
    for (account, message) in orphaned {
        super::agent_triage::enqueue_agent_triage_conn(
            &tx,
            account,
            message,
            "records_cleanup",
            false,
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Rebuild calendar search's index (schema.sql, CALENDAR SEARCH) when it does
/// not hold exactly one row per stored event: events stored before it
/// existed, or written by an older daemon that does not keep it. Three counts
/// on an index-backed table each; a rebuild is one INSERT ... SELECT per
/// table, and the store holds at most a few thousand events per calendar.
/// `calendar_search_fts`'s CREATE statement as schema.sql has it.
fn canonical_calendar_search_create() -> Option<&'static str> {
    static PARSED: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    *PARSED.get_or_init(|| {
        let start = SCHEMA.find("CREATE VIRTUAL TABLE IF NOT EXISTS calendar_search_fts")?;
        let end = start + SCHEMA[start..].find(");")? + 1;
        Some(SCHEMA[start..end].trim())
    })
}

pub(super) fn reindex_calendar_search_if_stale(conn: &Connection) -> Result<()> {
    // A partial store (an old schema mid-upgrade, a test fixture) has nothing
    // to index, or nowhere to put it.
    if !tables_exist(
        conn,
        &[
            "calendar_events",
            "calendar_event_guests",
            "calendar_search_keys",
            "calendar_search_fts",
        ],
    )? {
        return Ok(());
    }
    let count = |sql: &str| -> Result<i64> { Ok(conn.query_row(sql, [], |r| r.get(0))?) };
    // A definition that differs from schema.sql's (the tokenizer changed) is
    // a different index: recreate it, and the counts below then force the
    // rebuild.
    let stored: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='calendar_search_fts'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let (Some(stored), Some(canonical)) = (stored, canonical_calendar_search_create())
        && normalize_fts_sql(&stored) != normalize_fts_sql(canonical)
    {
        conn.execute_batch(&format!("DROP TABLE calendar_search_fts; {canonical};"))?;
    }
    let events = count("SELECT COUNT(*) FROM calendar_events")?;
    let keys = count(
        "SELECT COUNT(*) FROM calendar_search_keys k JOIN calendar_events e
           ON e.account_id=k.account_id AND e.calendar_id=k.calendar_id AND e.event_id=k.event_id",
    )?;
    let all_keys = count("SELECT COUNT(*) FROM calendar_search_keys")?;
    let indexed = count("SELECT COUNT(*) FROM calendar_search_fts")?;
    if events == keys && keys == all_keys && all_keys == indexed {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    super::calendar::reindex_calendar_search(&tx)?;
    tx.commit()?;
    Ok(())
}
