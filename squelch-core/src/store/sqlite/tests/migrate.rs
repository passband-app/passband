//! Migration + init upgrade-path tests.

use super::super::migrate::{
    canonical_fts_create, create_shipment_order_links, migrate, normalize_fts_sql,
    rebuild_fts_if_stale, reindex_calendar_search_if_stale,
};
use super::super::*;
use super::support::*;
use crate::types::Sensitivity;

#[test]
fn migrate_adds_unsub_columns_to_a_preexisting_messages_table() {
    // Simulate an existing install whose `messages` predates the unsubscribe
    // columns. The migration must add them, and be idempotent on re-open.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE messages(
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             gmail_msg_id TEXT NOT NULL, body TEXT);",
    )
    .unwrap();
    migrate(&conn).unwrap();
    migrate(&conn).unwrap(); // idempotent
    let mut stmt = conn.prepare("PRAGMA table_info(messages)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert!(cols.iter().any(|c| c == "list_unsubscribe"));
    assert!(cols.iter().any(|c| c == "list_unsub_one_click"));
}

#[test]
fn migrate_adds_auth_pass_null_to_a_preexisting_messages_table() {
    // An install predating the email-authentication column. The migration adds
    // it, and every historical row rests at NULL — no backfill, which is what
    // makes pre-existing mail read as "no tracking-pixel bypass".
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE messages(
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             gmail_msg_id TEXT NOT NULL, body TEXT);
         INSERT INTO messages(id, account_id, gmail_msg_id, body) VALUES (1, 1, 'g1', 'old');",
    )
    .unwrap();
    migrate(&conn).unwrap();
    migrate(&conn).unwrap(); // idempotent
    let mut stmt = conn.prepare("PRAGMA table_info(messages)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert!(cols.iter().any(|c| c == "auth_pass"));
    let existing: Option<i64> = conn
        .query_row("SELECT auth_pass FROM messages WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(existing, None, "pre-existing mail is never backfilled");
}

#[test]
fn migrate_adds_to_addrs_null_so_old_sent_mail_enters_the_backfill_queue() {
    // An install predating the display-recipients column. The migration adds it,
    // every historical row rests at NULL — which IS the backfill queue predicate
    // (`is_sent=1 AND to_addrs IS NULL`), since only a network pass can fill it.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE messages(
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             gmail_msg_id TEXT NOT NULL, body TEXT, is_sent INTEGER NOT NULL DEFAULT 0);
         INSERT INTO messages(id, account_id, gmail_msg_id, body, is_sent)
             VALUES (1, 1, 'g1', 'old', 1);",
    )
    .unwrap();
    migrate(&conn).unwrap();
    migrate(&conn).unwrap(); // idempotent
    let mut stmt = conn.prepare("PRAGMA table_info(messages)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert!(cols.iter().any(|c| c == "to_addrs"));
    let existing: Option<String> = conn
        .query_row("SELECT to_addrs FROM messages WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(existing, None, "old sent mail is queued, never invented");
}

#[test]
fn migrate_adds_is_archive_zero_to_a_preexisting_messages_table() {
    // An install predating the archive tier. Nothing it holds came from the
    // walk, so every old row reads 0, and the column is NOT NULL so no query
    // has to think about a third state.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE messages(
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             gmail_msg_id TEXT NOT NULL, body TEXT, is_sent INTEGER NOT NULL DEFAULT 0);
         INSERT INTO messages(id, account_id, gmail_msg_id, body) VALUES (1, 1, 'g1', 'old');",
    )
    .unwrap();
    migrate(&conn).unwrap();
    migrate(&conn).unwrap(); // idempotent
    let existing: i64 = conn
        .query_row("SELECT is_archive FROM messages WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(existing, 0, "pre-existing mail is live mail");
    assert!(
        conn.execute(
            "INSERT INTO messages(id, account_id, gmail_msg_id, is_archive) VALUES (2, 1, 'g2', NULL)",
            [],
        )
        .is_err(),
        "the flag has no unknown state"
    );
}

#[test]
fn migrate_adds_the_forward_edge_null_to_a_preexisting_walk_cursor() {
    // A database that walked before the forward pass existed: the cursor row
    // keeps its backward edge and gains a NULL forward one.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE archive_walk(
             account_id INTEGER PRIMARY KEY, walked_to TEXT NOT NULL, updated_at TEXT NOT NULL);
         INSERT INTO archive_walk VALUES (1, '2026-01-01T00:00:00+00:00', '2026-01-02T00:00:00+00:00');",
    )
    .unwrap();
    migrate(&conn).unwrap();
    migrate(&conn).unwrap(); // idempotent
    let (walked_to, covered_to): (String, Option<String>) = conn
        .query_row(
            "SELECT walked_to, covered_to FROM archive_walk WHERE account_id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(walked_to, "2026-01-01T00:00:00+00:00");
    assert_eq!(covered_to, None);
}

#[test]
fn migrate_adds_date_offset_null_and_its_partial_index_to_a_preexisting_messages_table() {
    // An install predating the timezone capture. The column arrives NULL on
    // every old row (the raw mail is gone, so nothing can fill it locally),
    // and the observation index arrives with it, on the second open as on
    // the first. A development database may hold the index's earlier,
    // non-covering shape under its old name; that one goes.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE messages(
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             gmail_msg_id TEXT NOT NULL, from_addr TEXT NOT NULL,
             received_at TEXT NOT NULL, body TEXT,
             is_sent INTEGER NOT NULL DEFAULT 0);
         INSERT INTO messages(id, account_id, gmail_msg_id, from_addr, received_at, body)
             VALUES (1, 1, 'g1', 'dan@example.com', '2026-01-01T00:00:00+00:00', 'old');
         CREATE INDEX idx_messages_zone_obs ON messages(account_id, from_addr, received_at);",
    )
    .unwrap();
    migrate(&conn).unwrap();
    migrate(&conn).unwrap(); // idempotent
    let existing: Option<i64> = conn
        .query_row(
            "SELECT date_offset_minutes FROM messages WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(existing, None, "pre-existing mail is never backfilled");
    let index: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'idx_messages_zone_offsets'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(index.contains("WHERE date_offset_minutes != 0"), "{index}");
    let old: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'idx_messages_zone_obs'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(old, 0, "the superseded index is dropped");
}

#[test]
fn migrate_adds_field_reasons_to_a_preexisting_triage_table() {
    // An install whose `triage` has no field_reasons. `model_used`/`status`
    // are present because the two-stage backfill in migrate() reads them.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE triage(
             message_id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             importance INTEGER NOT NULL DEFAULT 0, reason TEXT,
             model_used TEXT, status TEXT NOT NULL DEFAULT 'new');",
    )
    .unwrap();
    migrate(&conn).unwrap();
    migrate(&conn).unwrap(); // idempotent
    let mut stmt = conn.prepare("PRAGMA table_info(triage)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert!(cols.iter().any(|c| c == "field_reasons"));
}

#[test]
fn migrate_adds_two_stage_columns_so_ingest_works() {
    // A `triage` table with no stage1_model_used / needs_stage2 /
    // field_reasons. The migration must add them so ingest, which NAMES those
    // columns, doesn't fail with "no such column".
    register_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE triage(
             message_id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             importance INTEGER NOT NULL DEFAULT 0, tier TEXT NOT NULL DEFAULT 'noise',
             sensitivity TEXT NOT NULL DEFAULT 'normal', sealed_kind TEXT,
             one_line TEXT NOT NULL DEFAULT '', reason TEXT NOT NULL DEFAULT '',
             deadline TEXT, matched_rule_id INTEGER, model_used TEXT,
             status TEXT NOT NULL DEFAULT 'new', surfaced_at TEXT, resolved_at TEXT,
             created_at TEXT NOT NULL);",
    )
    .unwrap();
    // init applies SCHEMA (IF NOT EXISTS preserves the existing triage
    // shape) then migrate(), which adds the two columns + backfill.
    let store = SqliteStore::init(conn).unwrap();
    let acct = store.ensure_account("me@example.com").unwrap();

    let id = triaged_row(acct, "g-x", "t-x", None, false, Sensitivity::Normal).ingest(&store);
    // The upgraded table carries the columns, and ingest wrote through them.
    let conn = store.lock().unwrap();
    let (stage1, needs_stage2): (Option<String>, i64) = conn
        .query_row(
            "SELECT stage1_model_used, needs_stage2 FROM triage WHERE message_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(stage1.is_none(), "a fresh normal row is unmodelled");
    assert_eq!(
        needs_stage2, 1,
        "an unconfident seed is written through the new column"
    );
}

#[test]
fn migrate_rebuilds_stage2_usage_with_category_pk() {
    // A pre-category stage2_usage table (PK account_id+day) must be rebuilt
    // — ALTER ADD COLUMN can't extend a PK, and the bump upsert's
    // ON CONFLICT(account_id, day, category) needs the new unique index.
    // Existing rows land under category='stage2'; bumps work after.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE stage2_usage (
             account_id INTEGER NOT NULL, day TEXT NOT NULL,
             calls INTEGER NOT NULL DEFAULT 0,
             input_tokens INTEGER NOT NULL DEFAULT 0,
             output_tokens INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY(account_id, day));
         INSERT INTO stage2_usage VALUES (1, '2026-07-01', 3, 100, 50);
         CREATE TABLE triage(message_id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             model_used TEXT, status TEXT NOT NULL DEFAULT 'new',
             tier TEXT NOT NULL DEFAULT 'noise', deadline TEXT);
         CREATE TABLE deadlines(id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             message_id INTEGER NOT NULL, kind TEXT NOT NULL, due_at TEXT NOT NULL,
             past_due INTEGER NOT NULL DEFAULT 0, source TEXT NOT NULL);
         CREATE TABLE messages(id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             received_at TEXT NOT NULL);",
    )
    .unwrap();
    migrate(&conn).unwrap();
    // Old row preserved under the default category.
    let (cat, calls): (String, i64) = conn
        .query_row(
            "SELECT category, calls FROM stage2_usage WHERE account_id=1 AND day='2026-07-01'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(cat, "stage2");
    assert_eq!(calls, 3);
    // The category-keyed upsert now works (this was the failing bump).
    conn.execute(
        "INSERT INTO stage2_usage(account_id, day, category, calls, input_tokens, output_tokens)
         VALUES (1, '2026-07-01', 'stage1', 1, 10, 5)
         ON CONFLICT(account_id, day, category) DO UPDATE SET calls = calls + 1",
        [],
    )
    .unwrap();
    // Idempotent: migrate again is a no-op.
    migrate(&conn).unwrap();
}

#[test]
fn migrate_adds_cache_token_columns_to_a_preexisting_usage_table() {
    // A category-keyed stage2_usage that predates the cache columns gains them
    // additively — old rows read 0, and the cache-carrying bump upsert works.
    // Also covers the rebuild path: the pre-category shape from the test above
    // is rebuilt WITHOUT the cache columns, so they must be added after.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE stage2_usage (
             account_id INTEGER NOT NULL, day TEXT NOT NULL,
             category TEXT NOT NULL DEFAULT 'stage2',
             calls INTEGER NOT NULL DEFAULT 0,
             input_tokens INTEGER NOT NULL DEFAULT 0,
             output_tokens INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY(account_id, day, category));
         INSERT INTO stage2_usage VALUES (1, '2026-08-01', 'stage2', 2, 100, 50);",
    )
    .unwrap();
    migrate(&conn).unwrap();
    let (cache_w, cache_r): (i64, i64) = conn
        .query_row(
            "SELECT cache_creation_tokens, cache_read_tokens FROM stage2_usage
             WHERE account_id=1 AND day='2026-08-01' AND category='stage2'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((cache_w, cache_r), (0, 0), "history reads as zero cache");
    // The widened bump upsert (this was the failing INSERT pre-migration).
    conn.execute(
        "INSERT INTO stage2_usage(account_id, day, category, calls, input_tokens, output_tokens,
                                  cache_creation_tokens, cache_read_tokens)
         VALUES (1, '2026-08-01', 'stage2', 1, 10, 5, 700, 9000)
         ON CONFLICT(account_id, day, category) DO UPDATE SET
             cache_creation_tokens = cache_creation_tokens + excluded.cache_creation_tokens,
             cache_read_tokens = cache_read_tokens + excluded.cache_read_tokens",
        [],
    )
    .unwrap();
    // Idempotent: migrate again is a no-op.
    migrate(&conn).unwrap();
}

#[test]
fn migrate_cleans_year_slipped_stage_deadlines() {
    // A stage-sourced deadline far before its message's receipt is nulled
    // (tier demoted from past_due) and its deadlines row deleted;
    // deterministic-source rows are untouched.
    let store = SqliteStore::open_in_memory().unwrap();
    let conn = store.lock().unwrap();
    conn.execute_batch(
        "INSERT INTO accounts(id, email, created_at) VALUES (1, 'a@b', '2026-01-01T00:00:00Z');
         INSERT INTO messages(id, account_id, gmail_msg_id, thread_id, from_addr, subject,
                              received_at, snippet)
             VALUES (1, 1, 'g1', 't1', 'x@y', 's', '2026-07-23T00:00:00Z', ''),
                    (2, 1, 'g2', 't2', 'x@y', 's', '2026-07-23T00:00:00Z', '');
         INSERT INTO triage(message_id, account_id, tier, deadline, created_at, status)
             VALUES (1, 1, 'past_due', '2025-07-24T00:00:00Z', '2026-07-23T00:00:00Z', 'new'),
                    (2, 1, 'past_due', '2025-07-24T00:00:00Z', '2026-07-23T00:00:00Z', 'new');
         INSERT INTO deadlines(account_id, message_id, kind, due_at, past_due, source)
             VALUES (1, 1, 'bill', '2025-07-24T00:00:00Z', 1, 'stage1'),
                    (1, 2, 'bill', '2025-07-24T00:00:00Z', 1, 'subject');",
    )
    .unwrap();
    migrate(&conn).unwrap();
    // Stage-sourced year-slip: deadline nulled, tier demoted, row gone.
    let (dl1, tier1): (Option<String>, String) = conn
        .query_row(
            "SELECT deadline, tier FROM triage WHERE message_id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(dl1, None);
    assert_eq!(tier1, "deadline");
    let n1: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM deadlines WHERE message_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n1, 0);
    // Deterministic source ('subject'): untouched (explicit text is data).
    let dl2: Option<String> = conn
        .query_row("SELECT deadline FROM triage WHERE message_id=2", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(dl2.is_some(), "deterministic-source deadline must survive");
}

#[test]
fn migrate_backfill_keeps_only_processed_history_out_of_stage1() {
    // Old-semantics rows on a pre-two-stage `triage` table; the backfill must
    // mark ONLY genuinely-processed history 'migrated' (out of the Stage-1
    // queue) and leave genuinely-unprocessed recent rows to re-queue.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE triage(
             message_id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             model_used TEXT, status TEXT NOT NULL DEFAULT 'new');
         INSERT INTO triage(message_id, account_id, model_used, status) VALUES
             (1, 1, NULL, 'open'),       -- finalized/seen (past 'new')
             (2, 1, 'claude-x', 'new'),  -- old stage-2 processed
             (3, 1, NULL, 'new');        -- genuinely new & unprocessed",
    )
    .unwrap();
    migrate(&conn).unwrap();
    migrate(&conn).unwrap(); // idempotent: backfill fires ONCE at column add

    let get = |mid: i64| -> (Option<String>, i64) {
        conn.query_row(
            "SELECT stage1_model_used, needs_stage2 FROM triage WHERE message_id=?1",
            [mid],
            |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?)),
        )
        .unwrap()
    };
    assert_eq!(
        get(1).0.as_deref(),
        Some("migrated"),
        "seen/finalized row out of Stage-1"
    );
    assert_eq!(
        get(2).0.as_deref(),
        Some("migrated"),
        "old stage-2 processed row out"
    );
    assert_eq!(
        get(3).0,
        None,
        "genuinely-new unprocessed row re-enters Stage-1"
    );
    assert_eq!(
        get(3).1,
        0,
        "residual row's needs_stage2 rests at 0 (Stage-1 recomputes)"
    );
}

// ---- categorize-then-extract: schema migration + extractor queue -------

#[test]
fn migrate_adds_category_and_extractor_columns_to_preexisting_triage() {
    // A `triage` table predating the categorize-then-extract feature. The
    // additive migration must add both columns so the queue/apply SQL that
    // NAMES them doesn't fail with "no such column". Idempotent on re-open.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE triage(
             message_id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             model_used TEXT, status TEXT NOT NULL DEFAULT 'new');",
    )
    .unwrap();
    migrate(&conn).unwrap();
    migrate(&conn).unwrap(); // idempotent
    let mut stmt = conn.prepare("PRAGMA table_info(triage)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert!(
        cols.iter().any(|c| c == "category"),
        "category column added"
    );
    assert!(
        cols.iter().any(|c| c == "extractor_model_used"),
        "extractor_model_used column added"
    );
}

#[test]
fn init_upgrades_old_db_with_banking_table_and_extract_flow() {
    // OLD-shape triage (no category / extractor_model_used) and NO banking
    // table. init() applies SCHEMA (creates the banking table IF NOT EXISTS)
    // then migrate() (adds the two triage columns), so the whole extract flow
    // works on the upgraded DB.
    register_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE triage(
             message_id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             importance INTEGER NOT NULL DEFAULT 0, tier TEXT NOT NULL DEFAULT 'noise',
             sensitivity TEXT NOT NULL DEFAULT 'normal', sealed_kind TEXT,
             one_line TEXT NOT NULL DEFAULT '', reason TEXT NOT NULL DEFAULT '',
             deadline TEXT, matched_rule_id INTEGER, model_used TEXT,
             status TEXT NOT NULL DEFAULT 'new', surfaced_at TEXT, resolved_at TEXT,
             created_at TEXT NOT NULL);",
    )
    .unwrap();
    let store = SqliteStore::init(conn).unwrap();
    let acct = store.ensure_account("me@example.com").unwrap();

    let id = triaged_row(acct, "g-b", "t-b", None, false, Sensitivity::Normal)
        .category("banking_statement")
        .ingest(&store);

    // The banking table exists and the extract queue + apply work end to end.
    let cats = ["banking_statement", "transaction_alert"];
    let q = store.extract_queue(acct, &cats, 10).unwrap();
    assert_eq!(q.len(), 1);
    let applied = BankingApplied {
        message_id: id,
        account_id: acct,
        kind: "statement".into(),
        institution: Some("Chase".into()),
        amount: Some(1234.56),
        currency: Some("USD".into()),
        account_hint: Some("…1234".into()),
        received_at: Utc::now(),
        extractor_model_used: "claude-haiku-4-5".into(),
        auto_resolve: true,
    };
    store.banking_apply(&applied).unwrap();
    assert_eq!(store.list_banking(acct).unwrap().len(), 1);
}

// ---- the shipments trigger: column, backfill, and the stale-marker sweep ----

/// A DB shaped like an install that predates `ship_extract_model`: the two
/// tables the one-shots read, with no shipping columns on either.
fn preship_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE messages(
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             gmail_msg_id TEXT NOT NULL, from_addr TEXT NOT NULL DEFAULT '',
             subject TEXT NOT NULL DEFAULT '', body TEXT NOT NULL DEFAULT '',
             received_at TEXT NOT NULL, is_sent INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE triage(
             message_id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             sensitivity TEXT NOT NULL DEFAULT 'normal',
             category TEXT, extractor_model_used TEXT,
             model_used TEXT, status TEXT NOT NULL DEFAULT 'new');",
    )
    .unwrap();
    conn
}

/// Insert a message + its triage row, `days_ago` old.
fn preship_msg(conn: &Connection, id: i64, subject: &str, body: &str, days_ago: i64, sent: bool) {
    let at = (Utc::now() - chrono::Duration::days(days_ago)).to_rfc3339();
    conn.execute(
        "INSERT INTO messages(id, account_id, gmail_msg_id, from_addr, subject, body,
                              received_at, is_sent)
         VALUES(?1, 1, ?2, 'orders@shop.com', ?3, ?4, ?5, ?6)",
        params![id, format!("g{id}"), subject, body, at, sent as i64],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO triage(message_id, account_id) VALUES(?1, 1)",
        params![id],
    )
    .unwrap();
}

fn ship_marker(conn: &Connection, id: i64) -> Option<String> {
    conn.query_row(
        "SELECT ship_extract_model FROM triage WHERE message_id=?1",
        params![id],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn migrate_backfills_ship_extract_only_for_recent_signal_bearing_mail() {
    let conn = preship_db();
    // Recent + shipping signal -> pending.
    preship_msg(
        &conn,
        1,
        "Your order has shipped",
        "Tracking number 1Z999AA10123456784",
        2,
        false,
    );
    // Recent, no shipping signal at all -> untouched.
    preship_msg(&conn, 2, "Lunch tomorrow?", "See you at noon", 2, false);
    // Recent, but a RETURN -> excluded by the same precedence detection uses.
    preship_msg(
        &conn,
        3,
        "Your return label",
        "Print the return label and drop off your package",
        2,
        false,
    );
    // Signal-bearing but OUTSIDE the 30-day window -> untouched.
    preship_msg(
        &conn,
        4,
        "Your package was delivered",
        "Delivered to your porch",
        90,
        false,
    );
    // Signal-bearing SENT mail -> the user's own outbox is never tracked.
    preship_msg(
        &conn,
        5,
        "Re: your package shipped",
        "thanks, tracking looks right",
        2,
        true,
    );

    migrate(&conn).unwrap();
    assert_eq!(ship_marker(&conn, 1).as_deref(), Some("pending"));
    assert_eq!(ship_marker(&conn, 2), None, "no shipping signal");
    assert_eq!(ship_marker(&conn, 3), None, "a return is not a shipment");
    assert_eq!(ship_marker(&conn, 4), None, "outside the 30-day window");
    assert_eq!(ship_marker(&conn, 5), None, "sent mail is never queued");

    // Idempotent: the one-shot fires only on the open that ADDS the column, so
    // a second migrate must not re-pend a row an extractor has since ruled on.
    conn.execute(
        "UPDATE triage SET ship_extract_model='claude-x' WHERE message_id=1",
        [],
    )
    .unwrap();
    migrate(&conn).unwrap();
    assert_eq!(
        ship_marker(&conn, 1).as_deref(),
        Some("claude-x"),
        "the backfill must not fire twice"
    );
}

#[test]
fn migrate_ship_extract_backfill_honors_its_hard_row_cap() {
    // 600 signal-bearing messages, newest first by id. The cap is on the
    // CANDIDATE select, so exactly 500 rows can ever be pended, and they are the
    // newest ones (the mail most likely to still be in flight).
    let conn = preship_db();
    for i in 1..=600i64 {
        // id 600 is newest (0 days old), id 1 is oldest (~20 days old).
        preship_msg(
            &conn,
            i,
            "Your order has shipped",
            "on its way",
            (600 - i) / 30,
            false,
        );
    }
    migrate(&conn).unwrap();
    let pended: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM triage WHERE ship_extract_model='pending'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        pended, 500,
        "the hard cap bounds what a migration can spend"
    );
    assert_eq!(
        ship_marker(&conn, 600).as_deref(),
        Some("pending"),
        "newest mail is kept"
    );
    assert_eq!(ship_marker(&conn, 1), None, "oldest mail falls off the cap");
}

#[test]
fn migrate_clears_stale_skip_no_extractor_only_where_an_extractor_exists() {
    // Rows stamped 'skip-no-extractor' while the dispatch bug hid the marketing
    // extractor are permanently processed and would never re-queue. The sweep
    // frees exactly those; a category that genuinely has no extractor keeps its
    // honest verdict, and a real model marker is never touched.
    let conn = preship_db();
    conn.execute_batch(
        "INSERT INTO triage(message_id, account_id, category, extractor_model_used) VALUES
             (1, 1, 'marketing', 'skip-no-extractor'),
             (2, 1, 'invoice', 'skip-no-extractor'),
             (3, 1, 'banking_statement', 'claude-haiku-4-5'),
             (4, 1, NULL, 'skip-no-extractor');",
    )
    .unwrap();
    migrate(&conn).unwrap();

    let ext = |mid: i64| -> Option<String> {
        conn.query_row(
            "SELECT extractor_model_used FROM triage WHERE message_id=?1",
            params![mid],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(ext(1), None, "marketing has an extractor now: re-queue it");
    assert_eq!(
        ext(2).as_deref(),
        Some("skip-no-extractor"),
        "invoice really has no extractor"
    );
    assert_eq!(
        ext(3).as_deref(),
        Some("claude-haiku-4-5"),
        "a real model marker is untouched"
    );
    assert_eq!(
        ext(4).as_deref(),
        Some("skip-no-extractor"),
        "a NULL category has no extractor"
    );
}

#[test]
fn migrate_adds_order_ref_and_init_creates_the_orders_staging_table() {
    // The column seam covers `shipments.order_ref`; `shipment_orders` and its
    // indexes ride in on schema.sql, which `init` runs on EVERY open — the
    // reason migrate needs no CREATE TABLE of its own.
    register_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE shipments (
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             tracking_number TEXT NOT NULL, carrier TEXT NOT NULL,
             item_name TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'shipped',
             tracking_url TEXT, last_message_id INTEGER,
             first_seen TEXT NOT NULL, last_update TEXT NOT NULL,
             UNIQUE(account_id, tracking_number));",
    )
    .unwrap();
    let store = SqliteStore::init(conn).unwrap();
    let conn = store.lock().unwrap();
    let cols: Vec<String> = conn
        .prepare("PRAGMA table_info(shipments)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert!(cols.iter().any(|c| c == "order_ref"), "order_ref added");
    // The user-clear column lands on the same seam, NULL on every historical row
    // (nobody has cleared anything yet), so no backfill and nothing hidden.
    assert!(cols.iter().any(|c| c == "cleared_at"), "cleared_at added");
    let staged: i64 = conn
        .query_row("SELECT COUNT(*) FROM shipment_orders", [], |r| r.get(0))
        .unwrap();
    assert_eq!(staged, 0, "the staging table exists on an upgraded DB");
}

#[test]
fn migrate_adds_shipment_provenance_and_backfills_it_from_the_pointer() {
    // An install predating the provenance columns. `created_by_message_id` and
    // `item_name_msg` are both backfilled to `last_message_id` — for a
    // historical row that is the best available truth, and it is the pointer
    // sealing and reaping already (wrongly) relied on. `order_merchant` is
    // backfilled from the creating message's sender domain.
    register_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE messages(
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             gmail_msg_id TEXT NOT NULL, thread_id TEXT NOT NULL DEFAULT '',
             from_addr TEXT NOT NULL DEFAULT '', subject TEXT NOT NULL DEFAULT '',
             body TEXT NOT NULL DEFAULT '', received_at TEXT NOT NULL DEFAULT '',
             is_sent INTEGER NOT NULL DEFAULT 0);
         INSERT INTO messages(id, account_id, gmail_msg_id, from_addr)
             VALUES(7, 1, 'g7', 'orders@mail.shopa.com');
         CREATE TABLE shipments (
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             tracking_number TEXT NOT NULL, carrier TEXT NOT NULL,
             item_name TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'shipped',
             tracking_url TEXT, last_message_id INTEGER, order_ref TEXT,
             first_seen TEXT NOT NULL, last_update TEXT NOT NULL,
             UNIQUE(account_id, tracking_number));
         INSERT INTO shipments(id, account_id, tracking_number, carrier, item_name,
                               last_message_id, order_ref, first_seen, last_update)
             VALUES(1, 1, '1Z999AA10123456784', 'ups', 'Standing desk', 7, '1042', '', ''),
                   (2, 1, '9400111899223817428490', 'usps', '', 7, NULL, '', '');",
    )
    .unwrap();

    let store = SqliteStore::init(conn).unwrap();
    let conn = store.lock().unwrap();
    let row = |id: i64| -> (Option<i64>, Option<i64>, Option<String>) {
        conn.query_row(
            "SELECT created_by_message_id, item_name_msg, order_merchant
             FROM shipments WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };
    // `item_name_source` lands on the same seam with NO backfill: every
    // historical name came from the ingest detector, so 'regex' is the truth and
    // the extractor is free to replace any of them.
    let source = |id: i64| -> String {
        conn.query_row(
            "SELECT item_name_source FROM shipments WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(source(1), "regex");
    assert_eq!(source(2), "regex");
    assert_eq!(
        row(1),
        (Some(7), Some(7), Some("shopa.com".to_string())),
        "provenance backfills from the pointer, merchant from the sender"
    );
    assert_eq!(
        row(2),
        (Some(7), None, None),
        "no name and no order reference means nothing to attribute"
    );

    // ORDER LINKS: the open that creates the table backfills a 'legacy' link
    // for every row with both an order reference and a merchant, and no other.
    let links = |conn: &Connection| -> Vec<(i64, String, String, String)> {
        conn.prepare(
            "SELECT shipment_id, merchant_key, order_key, source FROM shipment_order_links",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap()
    };
    let want = vec![(1, "shopacom".into(), "1042".into(), "legacy".into())];
    assert_eq!(links(&conn), want);
    // ONE-SHOT: a later open must not pair an agent-written merchant with the
    // extractor's stale order_ref.
    conn.execute(
        "UPDATE shipments SET order_merchant = 'Shop A', order_ref = '1042' WHERE id = 2",
        [],
    )
    .unwrap();
    migrate(&conn).unwrap();
    assert_eq!(links(&conn), want, "the backfill never runs again");
    // Deleting a shipment deletes its links, whichever path deletes it.
    conn.execute("DELETE FROM shipments WHERE id = 1", [])
        .unwrap();
    assert!(links(&conn).is_empty(), "links follow their shipment out");
}

#[test]
fn migrate_rebuilds_the_staging_table_onto_the_merchant_scoped_key() {
    // `shipment_orders` grew its UNIQUE key from (account, order_ref) to
    // (account, merchant, order_ref). ALTER cannot change a unique index, and the
    // staging upsert's ON CONFLICT names the new triple, so an un-namespaced
    // table must be rebuilt or every staging write fails outright.
    register_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE shipment_orders (
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             order_ref TEXT NOT NULL, item_name TEXT NOT NULL DEFAULT '',
             thread_id TEXT NOT NULL DEFAULT '', last_message_id INTEGER,
             first_seen TEXT NOT NULL, last_update TEXT NOT NULL,
             UNIQUE(account_id, order_ref));
         INSERT INTO shipment_orders(account_id, order_ref, item_name, first_seen, last_update)
             VALUES(1, '1042', 'Cat bed', '', '');",
    )
    .unwrap();

    let store = SqliteStore::init(conn).unwrap();
    let conn = store.lock().unwrap();
    let cols: Vec<String> = conn
        .prepare("PRAGMA table_info(shipment_orders)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert!(cols.iter().any(|c| c == "order_merchant"));
    assert!(cols.iter().any(|c| c == "item_name_msg"));

    // Two shops CAN now stage the same reference — the point of the rebuild —
    // which the old unique key made impossible.
    for merchant in ["shopa.com", "shopb.com"] {
        conn.execute(
            "INSERT INTO shipment_orders(account_id, order_ref, order_merchant,
                                         first_seen, last_update)
             VALUES(1, '1042', ?1, '', '')
             ON CONFLICT(account_id, order_merchant, order_ref) DO NOTHING",
            params![merchant],
        )
        .unwrap();
    }
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM shipment_orders WHERE order_ref = '1042'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 2, "the rebuilt key namespaces by merchant");

    // The index the DROP took with it is back.
    let idx: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type='index' AND name='idx_shipment_orders_msg'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(idx, 1, "the rebuild re-creates the dropped index");
}

/// The carrier's answer clock lands on a pre-existing `shipments` table, and
/// its history is backfilled ONCE from the best available bound: a row holding
/// a carrier's words was answered no later than its last attempt. A row never
/// answered stays NULL, and a re-open does not re-backfill over a real answer
/// that has since been written.
#[test]
fn migrate_adds_last_answered_at_and_backfills_it_from_answered_rows_once() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE shipments(
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             tracking_number TEXT NOT NULL, carrier TEXT NOT NULL,
             item_name TEXT NOT NULL DEFAULT '', last_message_id INTEGER,
             carrier_status_raw TEXT, last_polled_at TEXT);
         INSERT INTO shipments(id,account_id,tracking_number,carrier,carrier_status_raw,last_polled_at)
         VALUES (1,1,'a','ups','In Transit','2026-09-01T00:00:00+00:00'),
                (2,1,'b','ups',NULL,'2026-09-01T00:00:00+00:00'),
                (3,1,'c','ups',NULL,NULL);",
    )
    .unwrap();
    migrate(&conn).unwrap();
    let answered = |id: i64| -> Option<String> {
        conn.query_row(
            "SELECT last_answered_at FROM shipments WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        answered(1).as_deref(),
        Some("2026-09-01T00:00:00+00:00"),
        "answered rows backfill from the attempt clock"
    );
    assert_eq!(
        answered(2),
        None,
        "an attempt with no answer is not an answer"
    );
    assert_eq!(answered(3), None);

    conn.execute(
        "UPDATE shipments SET last_answered_at='2026-09-20T00:00:00+00:00', last_polled_at='2026-09-21T00:00:00+00:00' WHERE id=1",
        [],
    )
    .unwrap();
    migrate(&conn).unwrap();
    assert_eq!(
        answered(1).as_deref(),
        Some("2026-09-20T00:00:00+00:00"),
        "a re-open never overwrites a real answer"
    );
}

#[test]
fn migrate_adds_reminder_columns_and_the_due_index_to_preexisting_triage() {
    // A `triage` table predating "remind me later". Both columns must land (the
    // reminder SQL NAMES them, so a missing one is a hard error rather than a
    // dark feature), and so must the partial index the sync sweep reads — it
    // cannot live in schema.sql, which runs BEFORE this seam and would fail on
    // "no such column: remind_at". Idempotent on re-open, index included.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE triage(
             message_id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             model_used TEXT, status TEXT NOT NULL DEFAULT 'new');
         INSERT INTO triage(message_id, account_id) VALUES (1, 1);",
    )
    .unwrap();
    migrate(&conn).unwrap();
    migrate(&conn).unwrap(); // idempotent
    let mut stmt = conn.prepare("PRAGMA table_info(triage)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert!(cols.iter().any(|c| c == "remind_at"), "remind_at added");
    assert!(cols.iter().any(|c| c == "reminded_at"), "reminded_at added");

    for index in ["idx_triage_remind_at", "idx_triage_reminded_at"] {
        let indexed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                [index],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(indexed, 1, "{index} is created here, not in schema.sql");
    }

    // NOT backfilled: `reminded_at` is a standing-band arm, so a stamped
    // historical row would drag the whole mailbox into the band.
    let (remind, reminded): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT remind_at, reminded_at FROM triage WHERE message_id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(remind, None);
    assert_eq!(reminded, None);
}

#[test]
fn migrate_adds_attention_columns_and_dates_the_dwell_ledger_once() {
    // A `triage` table that records opens but predates dwell and clicks, in a
    // mailbox with two accounts. Both columns land at zero, and because that
    // zero is not a measurement the dwell ledger is dated for EVERY existing
    // account on this open, and never moved by a later one.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE accounts(id INTEGER PRIMARY KEY, email TEXT NOT NULL,
             created_at TEXT NOT NULL);
         CREATE TABLE app_settings(account_id INTEGER NOT NULL, key TEXT NOT NULL,
             value TEXT NOT NULL, PRIMARY KEY(account_id, key));
         CREATE TABLE triage(
             message_id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             model_used TEXT, status TEXT NOT NULL DEFAULT 'new', opened_at TEXT);
         INSERT INTO accounts VALUES (1, 'a@example.com', '2020-01-01T00:00:00+00:00'),
                                     (2, 'b@example.com', '2020-01-01T00:00:00+00:00');
         INSERT INTO triage(message_id, account_id, opened_at)
             VALUES (1, 1, '2026-01-01T00:00:00+00:00');",
    )
    .unwrap();
    let before = Utc::now();
    migrate(&conn).unwrap();

    let (dwell, clicks): (i64, i64) = conn
        .query_row(
            "SELECT dwell_ms, link_clicks FROM triage WHERE message_id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((dwell, clicks), (0, 0), "NOT NULL DEFAULT 0 on old rows");

    let stamps = |conn: &Connection| -> Vec<(i64, String)> {
        conn.prepare("SELECT account_id, value FROM app_settings WHERE key=?1 ORDER BY account_id")
            .unwrap()
            .query_map([DWELL_LEDGER_SINCE_KEY], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    };
    let first = stamps(&conn);
    assert_eq!(
        first.iter().map(|s| s.0).collect::<Vec<_>>(),
        [1, 2],
        "one stamp per existing account"
    );
    for (_, value) in &first {
        let at = DateTime::parse_from_rfc3339(value).unwrap();
        assert!(at >= before && at <= Utc::now(), "stamped at this open");
    }
    // `opened_at` was already there, so the OPEN ledger is not restarted by
    // an open that only adds dwell.
    let open_stamps: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM app_settings WHERE key=?1",
            [OPEN_LEDGER_SINCE_KEY],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(open_stamps, 0);

    // Idempotent: a second open adds nothing and moves nothing.
    std::thread::sleep(std::time::Duration::from_millis(5));
    migrate(&conn).unwrap();
    assert_eq!(stamps(&conn), first);
}

#[test]
fn migrate_adds_resolved_by_null_to_a_preexisting_triage_table() {
    // A `triage` table from before provenance, holding a row somebody (or
    // something) resolved. The column lands NULL and is NOT backfilled: NULL
    // means "a person, or a path older than this column", which is the only
    // true thing to say about that row, and a guessed stamp would hand some
    // batch's undo a row it never took.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE triage(
             message_id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             model_used TEXT, status TEXT NOT NULL DEFAULT 'new', resolved_at TEXT);
         INSERT INTO triage(message_id, account_id, status, resolved_at)
             VALUES (1, 1, 'done', '2026-01-01T00:00:00+00:00');",
    )
    .unwrap();
    migrate(&conn).unwrap();
    migrate(&conn).unwrap(); // idempotent
    let (status, by): (String, Option<String>) = conn
        .query_row(
            "SELECT status, resolved_by FROM triage WHERE message_id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((status.as_str(), by), ("done", None));
}

#[test]
fn migrate_adds_notify_eligible_at_null_to_a_preexisting_triage_table() {
    // A `triage` table predating the notify-eligibility stamp. The column has
    // to land through the migration seam rather than schema.sql, because
    // schema.sql's `CREATE TABLE IF NOT EXISTS` is a no-op against a table that
    // already exists — and `ingest_message` NAMES the column in both halves of
    // its upsert, so a missing one is a hard "no such column" on the very first
    // message rather than a quietly dark feature.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE triage(
             message_id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             model_used TEXT, status TEXT NOT NULL DEFAULT 'new');
         INSERT INTO triage(message_id, account_id) VALUES (1, 1);",
    )
    .unwrap();
    migrate(&conn).unwrap();
    migrate(&conn).unwrap(); // idempotent
    let mut stmt = conn.prepare("PRAGMA table_info(triage)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert!(
        cols.iter().any(|c| c == "notify_eligible_at"),
        "notify_eligible_at added"
    );

    // NOT BACKFILLED, and the NULL is the whole safety property. A stamp
    // invented for mail already in the database would make the first tick after
    // an upgrade eligible to push a month of archived mail at a phone; historical
    // rows simply never notify, which is exactly what they do today.
    let stamp: Option<String> = conn
        .query_row(
            "SELECT notify_eligible_at FROM triage WHERE message_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stamp, None, "every pre-existing row rests at NULL");
}

#[test]
fn migrate_adds_sealed_kind_null_to_a_preexisting_events_table() {
    // An `events` table predating the sealed routing tag. The column has to land
    // through the migration seam rather than schema.sql, because schema.sql's
    // `CREATE TABLE IF NOT EXISTS` is a no-op against a table that already
    // exists — and both event SELECTs plus `append_event` NAME the column, so a
    // missing one is a hard "no such column" on the very first notification.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE events(
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             message_id INTEGER NOT NULL UNIQUE, thread_id TEXT NOT NULL,
             kind TEXT NOT NULL, tier TEXT NOT NULL, importance INTEGER NOT NULL,
             sender TEXT NOT NULL, one_line TEXT NOT NULL, deadline TEXT,
             created_at TEXT NOT NULL);
         INSERT INTO events(id, account_id, message_id, thread_id, kind, tier, importance,
                            sender, one_line, created_at)
             VALUES (1, 1, 1, 't1', 'surfaced', 'signal', 70, 'a@x.com', 'old',
                     '2026-08-01T00:00:00Z');",
    )
    .unwrap();
    migrate(&conn).unwrap();
    migrate(&conn).unwrap(); // idempotent
    let mut stmt = conn.prepare("PRAGMA table_info(events)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert!(cols.iter().any(|c| c == "sealed_kind"), "sealed_kind added");

    // NOT BACKFILLED, and NULL is the honest reading rather than merely the
    // convenient one: no historical event came from a sealed message, because
    // the old emission path required sensitivity='normal'. There is no kind to
    // recover, and a client reads NULL as "an ordinary event" — which is what
    // every one of these rows is.
    let kind: Option<String> = conn
        .query_row("SELECT sealed_kind FROM events WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(kind, None, "every pre-existing event rests at NULL");
}

/// The LEDGER needs no migration seam at all — it is a new TABLE, and
/// `schema.sql` is all `CREATE ... IF NOT EXISTS` and runs in full on every
/// open. This asserts the thing that claim depends on: an install that predates
/// the table gets it, with its UNIQUE and its index, from `init` alone.
#[test]
fn init_creates_notify_decisions_on_a_db_that_predates_it() {
    let (store, acct) = store();
    let conn = store.lock().unwrap();

    let mut stmt = conn.prepare("PRAGMA table_info(notify_decisions)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    for expected in [
        "id",
        "account_id",
        "message_id",
        "lane",
        "decision",
        "notify_importance",
        "model_used",
        "latency_ms",
        "created_at",
    ] {
        assert!(cols.iter().any(|c| c == expected), "missing {expected}");
    }

    // The UNIQUE is the append-only rule made structural, so it is worth an
    // assertion of its own: a second row for the same (message, lane) is what
    // `INSERT OR IGNORE` relies on to be a no-op instead of a duplicate.
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO notify_decisions(account_id, message_id, lane, decision, created_at)
         VALUES(?1, 1, 'fast', 'sent', ?2)",
        params![acct, now],
    )
    .unwrap();
    let dup = conn.execute(
        "INSERT INTO notify_decisions(account_id, message_id, lane, decision, created_at)
         VALUES(?1, 1, 'fast', 'expired', ?2)",
        params![acct, now],
    );
    assert!(dup.is_err(), "UNIQUE(message_id, lane) is enforced");

    let index: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type='index' AND name='idx_notify_decisions_account_created'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(index, 1, "the eval read's index ships with the table");
}

#[test]
fn migrate_adds_content_id_null_to_a_preexisting_attachments_table() {
    // An install predating the cid column. The migration adds it, and every
    // already-synced attachment rests at NULL: the Content-ID only exists in the
    // RFC822, which the store never kept, so there is nothing to backfill from
    // and those inline images stay tiles until a re-sync re-ingests the mail.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE attachments(
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             message_id INTEGER NOT NULL, filename TEXT NOT NULL,
             mime TEXT NOT NULL, size_bytes INTEGER NOT NULL, data BLOB,
             UNIQUE(account_id, message_id, filename, size_bytes));
         INSERT INTO attachments(id, account_id, message_id, filename, mime, size_bytes, data)
             VALUES (1, 1, 1, 'logo.png', 'image/png', 3, x'616263');",
    )
    .unwrap();
    migrate(&conn).unwrap();
    migrate(&conn).unwrap(); // idempotent
    let mut stmt = conn.prepare("PRAGMA table_info(attachments)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert!(cols.iter().any(|c| c == "content_id"));
    let existing: Option<String> = conn
        .query_row("SELECT content_id FROM attachments WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        existing, None,
        "pre-existing attachments are not backfilled"
    );
}

// ---- one-shot repairs of already-stored specialist rows ----------------

#[test]
fn migrate_relaunders_a_prompt_echo_institution_out_of_banking() {
    // THE ROW THAT SHIPPED. The extractor only ever CAPPED this field, so a
    // model that emitted a correct name and then kept generating left 80
    // characters of prompt scaffolding on a card that renders one line
    // ("Venmo=== TRUSTED"). The cap is now a shape check, and the rows already
    // written have to be re-judged by it — nothing else can reach them.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE banking(
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             message_id INTEGER NOT NULL, kind TEXT NOT NULL,
             institution TEXT, amount REAL, currency TEXT, account_hint TEXT);
         INSERT INTO banking(id, account_id, message_id, kind, institution, amount, currency)
           VALUES
             (1, 1, 981, 'transaction_alert',
              'Venmo=== TRUSTED CONTEXT (from the account owner; authoritative) ===
owner refin', NULL, NULL),
             (2, 1, 1046, 'transaction_alert',
              'Visa=== END NOTE ===Continue extraction.ionation. Return JSON.999,', 39.99, 'USD'),
             (3, 1, 1200, 'statement', 'American Express', 4138.89, 'USD'),
             (4, 1, 1291, 'autopay', NULL, 91.43, 'USD');",
    )
    .unwrap();

    migrate(&conn).unwrap();
    migrate(&conn).unwrap(); // idempotent

    let read = |id: i64| -> Option<String> {
        conn.query_row(
            "SELECT institution FROM banking WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(read(1), None, "prompt echo cleared");
    assert_eq!(read(2), None, "degenerated answer cleared");
    assert_eq!(
        read(3).as_deref(),
        Some("American Express"),
        "a real institution is left alone"
    );
    assert_eq!(read(4), None, "an already-NULL institution stays NULL");

    // THE REST OF THE ROW SURVIVES. Clearing a bad label must not cost the
    // amount, the kind, or the card tail — the row is still a real transaction.
    let (kind, amount): (String, Option<f64>) = conn
        .query_row("SELECT kind, amount FROM banking WHERE id = 2", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(kind, "transaction_alert");
    assert_eq!(amount, Some(39.99));
}

#[test]
fn institution_relaundering_survives_a_db_without_a_banking_table() {
    // The migration suite builds deliberately partial old schemas, and an
    // install predating the banking table must not fault on a pass that reads
    // one.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE messages(id INTEGER PRIMARY KEY, account_id INTEGER);")
        .unwrap();
    migrate(&conn).unwrap();
}

#[test]
fn migrate_requeues_a_no_body_row_once_its_body_arrives() {
    // THE STAMP IS TERMINAL AND THE BODY IS RECOVERABLE, which is the whole
    // problem: `extract_queue` selects on `extractor_model_used IS NULL`, a
    // re-fetch heals `messages.body` without touching any triage column, and
    // nothing else ever clears the marker. Between a daemon roll and that
    // re-fetch, every extract tick permanently retires another row the ingest
    // fix was written to rescue.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE messages(
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL, body TEXT);
         CREATE TABLE triage(
             message_id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             model_used TEXT, status TEXT NOT NULL DEFAULT 'new',
             extractor_model_used TEXT);
         INSERT INTO messages(id, account_id, body) VALUES
             (1, 1, 'the body finally arrived'),
             (2, 1, ''),
             -- CRLF, a tab, an NBSP and a BOM. An 'empty' text/plain part on
             -- the wire is a bare CRLF, not a zero-length string, and the first
             -- cut of this sweep used SQL TRIM, which strips U+0020 and nothing
             -- else: every one of these was re-queued on each open, refused
             -- again by the extractor, and re-queued again, forever. They are
             -- the exact shape this pass exists for, and the old fixture's
             -- three spaces were the single blank value TRIM did handle.
             (3, 1, char(13) || char(10)),
             (6, 1, char(9)),
             (7, 1, char(160)),
             (8, 1, char(65279)),
             (4, 1, 'plenty here'),
             (5, 1, 'and here');
         INSERT INTO triage(message_id, account_id, extractor_model_used) VALUES
             (1, 1, 'skip-no-body'),
             (2, 1, 'skip-no-body'),
             (3, 1, 'skip-no-body'),
             (6, 1, 'skip-no-body'),
             (7, 1, 'skip-no-body'),
             (8, 1, 'skip-no-body'),
             (4, 1, 'stale-skip'),
             (5, 1, 'claude-opus-5');",
    )
    .unwrap();

    migrate(&conn).unwrap();

    let stamp = |id: i64| -> Option<String> {
        conn.query_row(
            "SELECT extractor_model_used FROM triage WHERE message_id = ?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        stamp(1),
        None,
        "a body arrived, so the row goes back in the queue"
    );
    assert_eq!(
        stamp(2).as_deref(),
        Some("skip-no-body"),
        "still empty: leave it refused, it will be refused again correctly"
    );
    for (id, what) in [
        (3, "CRLF"),
        (6, "a tab"),
        (7, "a non-breaking space"),
        (8, "a byte-order mark"),
    ] {
        assert_eq!(
            stamp(id).as_deref(),
            Some("skip-no-body"),
            "{what} is still nothing to read, whatever SQL TRIM thinks"
        );
    }
    // THE OTHER STAMPS ARE NOT OURS TO CLEAR. A stale skip and a real
    // extraction are both finished business; re-queuing them would re-bill an
    // LLM call for a verdict that already exists.
    assert_eq!(stamp(4).as_deref(), Some("stale-skip"));
    assert_eq!(stamp(5).as_deref(), Some("claude-opus-5"));

    // SELF-EXTINGUISHING: the predicate cannot re-match what it cleared, so a
    // second open is free and no done-flag is needed.
    migrate(&conn).unwrap();
    assert_eq!(stamp(1), None);
    assert_eq!(stamp(2).as_deref(), Some("skip-no-body"));
}

#[test]
fn the_no_body_sweep_survives_a_messages_table_with_no_body_column() {
    // THE GUARD THIS ACTUALLY REACHES. The first version of this test built only
    // `messages`, so `tables_exist(["triage","messages"])` short-circuited and
    // neither column check was ever evaluated — it passed with every guard
    // deleted and proved nothing but that migrate() tolerates a triage-less DB,
    // which thirty other tests already prove.
    //
    // Both tables present, `messages` predating `body`: now the SELECT would
    // name a column install never had, which is a hard SQL error that fails the
    // whole store open rather than a no-op.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE messages(
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL, gmail_msg_id TEXT);
         CREATE TABLE triage(
             message_id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             model_used TEXT, status TEXT NOT NULL DEFAULT 'new',
             extractor_model_used TEXT);
         INSERT INTO messages(id, account_id, gmail_msg_id) VALUES (1, 1, 'g1');
         INSERT INTO triage(message_id, account_id, extractor_model_used)
             VALUES (1, 1, 'skip-no-body');",
    )
    .unwrap();

    migrate(&conn).unwrap();
    migrate(&conn).unwrap();

    // The stamp is untouched: with no body column there is nothing to judge
    // recoverable, and guessing would re-queue a row on no evidence.
    let stamp: Option<String> = conn
        .query_row(
            "SELECT extractor_model_used FROM triage WHERE message_id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stamp.as_deref(), Some("skip-no-body"));
}

#[test]
fn migrate_backfills_the_sender_directory_from_existing_mail_exactly_once() {
    // An install predating `senders`: schema.sql creates the table empty while
    // `messages` is full. The migration fills it under every listing's rule
    // (sent and spam excluded), with each sender's newest name, and does NOT
    // touch a directory that already has rows.
    use chrono::TimeZone;
    let at = |day: u32| Utc.with_ymd_and_hms(2026, 8, day, 12, 0, 0).unwrap();
    let (store, acct) = store();
    triaged(acct, "g1", "t")
        .from("dan@example.com")
        .from_name(Some("Dan Smith"))
        .received_at(at(1))
        .upsert(&store);
    triaged(acct, "g2", "t")
        .from("dan@example.com")
        .from_name(Some("Dan"))
        .received_at(at(3))
        .upsert(&store);
    triaged(acct, "g3", "t")
        .from("me@example.com")
        .is_sent(true)
        .upsert(&store);
    triaged(acct, "g4", "t")
        .from("spam@x.example")
        .is_spam(true)
        .upsert(&store);
    {
        let conn = store.lock().unwrap();
        // Simulate the pre-table install: the rows ingest wrote never existed.
        conn.execute("DELETE FROM senders", []).unwrap();
        migrate(&conn).unwrap();
        let rows: Vec<(String, Option<String>, i64, String, String)> = conn
            .prepare(
                "SELECT addr, display_name, msg_count, first_seen, last_received_at
                 FROM senders ORDER BY addr",
            )
            .unwrap()
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "sent and spam mail register nobody: {rows:?}"
        );
        assert_eq!(rows[0].0, "dan@example.com");
        assert_eq!(rows[0].1.as_deref(), Some("Dan"), "the newest name wins");
        assert_eq!(rows[0].2, 2);
        assert_eq!(rows[0].3, at(1).to_rfc3339());
        assert_eq!(rows[0].4, at(3).to_rfc3339());
        // A populated directory is left exactly as it is.
        conn.execute("UPDATE senders SET msg_count = 99", [])
            .unwrap();
        migrate(&conn).unwrap();
        let count: i64 = conn
            .query_row("SELECT msg_count FROM senders", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 99, "a second open must not rebuild the directory");
    }
    // And the reader sees what the backfill wrote.
    let hits = store.search_senders(acct, "dan", 8).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].last_received_at, at(3));
}

#[test]
fn migrate_rebuilds_the_fts_index_with_the_porter_tokenizer() {
    // An install whose `messages_fts` predates the stemmer. The migration has
    // to notice from the stored CREATE text, rebuild the table, and refill it
    // from `messages` — losing a row here would make mail silently unfindable,
    // which is the one failure a search index can have that nobody reports.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE messages(
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             gmail_msg_id TEXT NOT NULL, subject TEXT NOT NULL DEFAULT '',
             body TEXT NOT NULL DEFAULT '');
         CREATE VIRTUAL TABLE messages_fts USING fts5(subject, body);
         INSERT INTO messages(id, account_id, gmail_msg_id, subject, body)
             VALUES (1, 1, 'g1', 'venue details', 'the wifi passwords are on the agenda page'),
                    (2, 1, 'g2', 'shipping update', 'your parcel shipped this morning');
         INSERT INTO messages_fts(rowid, subject, body)
             SELECT id, subject, body FROM messages;",
    )
    .unwrap();

    // Before: the exact word only.
    let matches = |conn: &Connection, q: &str| -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH ?1",
            params![q],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(matches(&conn, "\"password\""), 0, "no stemming yet");

    migrate(&conn).unwrap();
    migrate(&conn).unwrap(); // idempotent: the second open rebuilds nothing

    let created: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'messages_fts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(created.contains("porter"), "tokenizer replaced: {created}");
    assert!(created.contains("prefix"), "prefix index added: {created}");

    assert_eq!(
        matches(&conn, "\"password\""),
        1,
        "singular now finds the plural"
    );
    assert_eq!(matches(&conn, "\"shipping\""), 1, "and shipped/shipping");
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages_fts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 2, "every message survived the rebuild");

    // The window still reads back the ORIGINAL text, not a stem: `snippet()`
    // cuts the stored column, and the stemmer only ever touched the index.
    let window: String = conn
        .query_row(
            "SELECT snippet(messages_fts, 1, '', '', '…', 24)
             FROM messages_fts WHERE messages_fts MATCH ?1",
            params!["\"password\""],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        window.contains("passwords"),
        "snippet returns the sender's own word: {window:?}"
    );
}

#[test]
fn migrate_rebuilds_an_index_that_stems_but_has_no_prefix_index() {
    // The in-between state: a mailbox opened by a build that had the stemmer
    // and not the prefix index. Checking only for "porter" would have called
    // that current and left the as-you-type expansion walking the whole term
    // list forever, which is the failure mode of a one-marker drift check.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE messages(
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             gmail_msg_id TEXT NOT NULL, subject TEXT NOT NULL DEFAULT '',
             body TEXT NOT NULL DEFAULT '');
         CREATE VIRTUAL TABLE messages_fts USING fts5(
             subject, body, tokenize = 'porter unicode61');
         INSERT INTO messages(id, account_id, gmail_msg_id, subject, body)
             VALUES (1, 1, 'g1', 'venue details', 'the wifi passwords are on the agenda page');
         INSERT INTO messages_fts(rowid, subject, body)
             SELECT id, subject, body FROM messages;",
    )
    .unwrap();

    migrate(&conn).unwrap();
    migrate(&conn).unwrap(); // idempotent: the second open rebuilds nothing

    let created: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'messages_fts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(created.contains("prefix"), "prefix index added: {created}");
    assert!(created.contains("porter"), "stemmer kept: {created}");
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages_fts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1, "the message survived the rebuild");
}

/// The fixture the FTS drift tests share: a `messages` table and whatever
/// `messages_fts` definition the caller wants to have been created by an older
/// build, filled from `messages` the way the upsert would have.
fn fts_fixture(fts_create: &str) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE messages(
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             gmail_msg_id TEXT NOT NULL, subject TEXT NOT NULL DEFAULT '',
             body TEXT NOT NULL DEFAULT '');
         {fts_create};
         INSERT INTO messages(id, account_id, gmail_msg_id, subject, body)
             VALUES (1, 1, 'g1', 'venue details', 'the wifi passwords are on the agenda page');
         INSERT INTO messages_fts(rowid, subject, body)
             SELECT id, subject, body FROM messages;"
    ))
    .unwrap();
    conn
}

fn stored_fts_sql(conn: &Connection) -> String {
    conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'messages_fts'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

fn fts_rows(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM messages_fts", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn an_fts_definition_that_differs_at_all_is_rebuilt() {
    // Not "does it say porter" — ANY difference from `schema.sql`'s own line.
    // A marker list only notices the options somebody remembered to list, so
    // the next edit to that line (the prefix index was one) would have shipped
    // silently unmigrated: a mailbox searching an index that does not match its
    // own schema, with nothing on the outside to say so.
    //
    // This definition differs in every way at once: no stemmer, no prefix
    // index, columns the other way round.
    let conn = fts_fixture("CREATE VIRTUAL TABLE messages_fts USING fts5(body, subject)");

    migrate(&conn).unwrap();

    assert_eq!(
        normalize_fts_sql(&stored_fts_sql(&conn)),
        normalize_fts_sql(canonical_fts_create().expect("schema.sql carries the definition")),
        "the rebuilt index is the schema's own definition, exactly"
    );
    assert_eq!(fts_rows(&conn), 1, "the message survived the rebuild");
}

#[test]
fn the_schemas_own_definition_is_never_rebuilt() {
    // The other half of the same claim, and the one that keeps this cheap: a
    // mailbox created by a current build must open without a full reindex. The
    // proof is a row in `messages` that no FTS row was ever written for — a
    // rebuild refills from `messages`, so it would pick that row up.
    let canonical = canonical_fts_create().expect("schema.sql carries the definition");
    let conn = fts_fixture(canonical);
    conn.execute_batch(
        "DELETE FROM messages_fts;
         INSERT INTO messages(id, account_id, gmail_msg_id, subject, body)
             VALUES (2, 1, 'g2', 'shipping update', 'your parcel shipped this morning');",
    )
    .unwrap();

    // What SQLite stored when it created the canonical statement differs from
    // the statement itself only in the ways `normalize_fts_sql` folds away
    // (`IF NOT EXISTS`, the terminator, whitespace). If that ever stopped being
    // true, every open would rebuild the whole index.
    assert_eq!(
        normalize_fts_sql(&stored_fts_sql(&conn)),
        normalize_fts_sql(canonical),
    );

    migrate(&conn).unwrap();
    migrate(&conn).unwrap();
    assert_eq!(fts_rows(&conn), 0, "nothing was refilled, so nothing ran");
}

#[test]
fn two_opens_in_a_row_rebuild_the_index_once() {
    // Idempotence, measured rather than assumed. The first open rebuilds; the
    // second compares the definition it just wrote against the schema's, finds
    // them equal, and does nothing. A row added to `messages` alone between the
    // two is the tell: only a second rebuild could put it in the index.
    let conn = fts_fixture("CREATE VIRTUAL TABLE messages_fts USING fts5(subject, body)");

    migrate(&conn).unwrap();
    assert_eq!(fts_rows(&conn), 1, "the first open rebuilds");
    assert_eq!(
        normalize_fts_sql(&stored_fts_sql(&conn)),
        normalize_fts_sql(canonical_fts_create().unwrap()),
    );

    conn.execute_batch(
        "INSERT INTO messages(id, account_id, gmail_msg_id, subject, body)
             VALUES (2, 1, 'g2', 'shipping update', 'your parcel shipped this morning');",
    )
    .unwrap();
    migrate(&conn).unwrap();
    assert_eq!(
        fts_rows(&conn),
        1,
        "the second open rebuilt nothing, so the un-indexed row stayed out"
    );
}

#[test]
fn a_rebuild_that_cannot_run_leaves_the_old_index_serving() {
    // Two processes really do open one mailbox here (`squelchd token issue`
    // runs while `serve` holds the file). A rebuild that loses that race must
    // not take the whole open down with it: the old index still finds mail,
    // just unstemmed, and the next open tries again. A second connection holds
    // a write lock to stand in for the busy daemon.
    //
    // The rebuild is called directly rather than through `migrate`, because
    // under an exclusive lock EVERY write in the chain fails and the assertion
    // would stop being about this one.
    let dir = std::env::temp_dir().join(format!("squelch-fts-busy-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("busy.db");
    // A leftover file from a previous run would test the wrong thing.
    let _ = std::fs::remove_file(&path);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE messages(
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             gmail_msg_id TEXT NOT NULL, subject TEXT NOT NULL DEFAULT '',
             body TEXT NOT NULL DEFAULT '');
         CREATE VIRTUAL TABLE messages_fts USING fts5(subject, body);
         INSERT INTO messages(id, account_id, gmail_msg_id, subject, body)
             VALUES (1, 1, 'g1', 'venue details', 'the wifi passwords are here');
         INSERT INTO messages_fts(rowid, subject, body)
             SELECT id, subject, body FROM messages;",
    )
    .unwrap();

    let blocker = Connection::open(&path).unwrap();
    blocker
        .busy_timeout(std::time::Duration::from_millis(1))
        .unwrap();
    // RESERVED, not EXCLUSIVE: a writer is blocked and a reader is not, which
    // is what one daemon mid-write actually looks like to a second handle.
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    conn.busy_timeout(std::time::Duration::from_millis(1))
        .unwrap();

    rebuild_fts_if_stale(&conn).expect("a busy sibling is not a failed migration");

    // Still the old definition, still holding every row: nothing was dropped.
    let created: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'messages_fts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!created.contains("porter"), "not rebuilt: {created}");
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages_fts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1, "the index the reader still searches is intact");

    // And the next open, once the sibling has let go, does the work.
    blocker.execute_batch("ROLLBACK").unwrap();
    rebuild_fts_if_stale(&conn).unwrap();
    let created: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'messages_fts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(created.contains("porter"), "rebuilt on retry: {created}");
}

#[test]
fn the_translation_columns_arrive_by_rebuild_and_carry_served_translations() {
    // The definition every mailbox had before auto-translate: stemmed, prefix
    // indexed, two columns. Adding `tsubject`/`tbody` to `schema.sql` is a
    // column change, so the open rebuilds, and the refill must pick up the
    // English of every SERVED translation (done, incomplete) and of no other.
    let conn = fts_fixture(
        "CREATE VIRTUAL TABLE messages_fts USING fts5(
             subject, body, tokenize = 'porter unicode61', prefix = '2 3')",
    );
    conn.execute_batch(
        "CREATE TABLE message_translations(
             message_id INTEGER PRIMARY KEY, status TEXT NOT NULL,
             subject TEXT, body TEXT);
         INSERT INTO messages(id, account_id, gmail_msg_id, subject, body)
             VALUES (2, 1, 'g2', 'Votre facture', 'votre facture est prête'),
                    (3, 1, 'g3', 'Ihre Rechnung', 'Ihre Rechnung ist da'),
                    (4, 1, 'g4', 'Tu factura', 'tu factura está lista');
         INSERT INTO messages_fts(rowid, subject, body)
             SELECT id, subject, body FROM messages WHERE id > 1;
         INSERT INTO message_translations VALUES
             (2, 'done', 'Your invoice', 'your invoice is ready'),
             (3, 'incomplete', 'Your bill', 'your invoice is here'),
             (4, 'failed', 'Your invoice', 'your invoice is ready');",
    )
    .unwrap();
    let matching = |conn: &Connection, q: &str| -> Vec<i64> {
        let mut stmt = conn
            .prepare("SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?1 ORDER BY rowid")
            .unwrap();
        stmt.query_map(params![q], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert!(matching(&conn, "invoice").is_empty());

    migrate(&conn).unwrap();
    migrate(&conn).unwrap();

    let created = stored_fts_sql(&conn);
    assert!(
        created.contains("tsubject") && created.contains("tbody"),
        "{created}"
    );
    assert_eq!(fts_rows(&conn), 4, "every message survived the rebuild");
    assert_eq!(
        matching(&conn, "invoice"),
        vec![2, 3],
        "failed rows serve nothing"
    );
    assert_eq!(matching(&conn, "tbody : invoice"), vec![2, 3]);
    assert_eq!(
        matching(&conn, "facture"),
        vec![2],
        "the original is still indexed"
    );
    // Positional readers keep their meaning: column 1 is still the body.
    let body: String = conn
        .query_row(
            "SELECT snippet(messages_fts, 1, '', '', '…', 24) FROM messages_fts
             WHERE messages_fts MATCH 'facture'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(body.contains("prête"), "{body}");
}

#[test]
fn event_auth_flag_migration_defaults_legacy_rows_without_inferring_urgency() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE events(id INTEGER PRIMARY KEY,kind TEXT); INSERT INTO events VALUES(1,'urgent');").unwrap();
    migrate(&conn).unwrap();
    migrate(&conn).unwrap();
    let is_auth: bool = conn
        .query_row("SELECT is_auth FROM events WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert!(
        !is_auth,
        "legacy urgency alone does not prove authentication"
    );
}

/// THE ORDER-LINKS BACKFILL SKIPS JUNK AND IS ALL-OR-NOTHING ON A REAL ERROR.
///
/// A legacy row it cannot read (a merchant stored as a number, a reference
/// stored as a blob or as bytes that are not UTF-8) is skipped, and every other
/// row still gets its link: one bad row must not fail the backfill on every
/// open forever.
///
/// A real SQL failure must leave NO table behind: the table's existence is
/// what tells every later open the backfill already ran, so a half-done one
/// would lose the legacy links for good. Here a temp view shadowing
/// `shipments` fails the migration after the table was created; the next
/// open, with the fault gone, does the whole job.
#[test]
fn order_links_migration_is_all_or_nothing() {
    let exists = |conn: &Connection| -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'shipment_order_links'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
            == 1
    };
    let links = |conn: &Connection| -> Vec<(i64, String)> {
        conn.prepare("SELECT shipment_id, order_ref FROM shipment_order_links ORDER BY 1")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    };

    // JUNK IS SKIPPED: rows 2-4 cannot be read as text, rows 1 and 5 backfill.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE shipments (
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             order_ref, order_merchant);
         INSERT INTO shipments VALUES
             (1, 1, '1042', 'Shop A'),
             (2, 1, '77', 42),
             (3, 1, x'0102', 'Shop C'),
             (4, 1, '88', CAST(x'ff' AS TEXT)),
             (5, 1, '1043', 'Shop E');",
    )
    .unwrap();
    create_shipment_order_links(&conn).unwrap();
    assert!(exists(&conn), "junk rows do not fail the backfill");
    assert_eq!(
        links(&conn),
        vec![(1, "1042".to_string()), (5, "1043".to_string())],
        "every readable row is linked, the junk ones skipped"
    );

    // A REAL SQL ERROR ROLLS EVERYTHING BACK. A temp view shadowing
    // `shipments` makes the migration's AFTER DELETE trigger fail to create,
    // after the table and its index already have, inside the transaction.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE shipments (
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             order_ref TEXT, order_merchant TEXT);
         INSERT INTO shipments VALUES (1, 1, '1042', 'Shop A'), (2, 1, '77', 'Shop B');
         CREATE TEMP VIEW shipments AS SELECT * FROM main.shipments;",
    )
    .unwrap();
    create_shipment_order_links(&conn).expect("a failed backfill is not a failed open");
    assert!(
        !exists(&conn),
        "rolled back: no table claims the backfill ran"
    );
    assert!(
        conn.is_autocommit(),
        "the transaction is closed, not left open"
    );
    conn.execute_batch("DROP VIEW temp.shipments").unwrap();
    create_shipment_order_links(&conn).unwrap();
    assert!(exists(&conn));
    assert_eq!(
        links(&conn).len(),
        2,
        "the retry backfills every legacy row"
    );
}

/// A DB that already ran the first version of the order-links migration (the
/// user's live one) has the table WITHOUT the provenance columns, and shipments
/// without `order_merchant_msg`. The next open adds them, and reconcile then
/// writes through them.
#[test]
fn order_link_provenance_columns_reach_a_db_that_already_has_the_table() {
    let store = SqliteStore::open_in_memory().unwrap();
    {
        let conn = store.lock().unwrap();
        conn.execute_batch(
            "DROP TABLE shipment_order_links;
             CREATE TABLE shipment_order_links (
                 account_id INTEGER NOT NULL, shipment_id INTEGER NOT NULL,
                 merchant_key TEXT NOT NULL, order_key TEXT NOT NULL,
                 merchant TEXT NOT NULL DEFAULT '', order_ref TEXT NOT NULL,
                 source TEXT NOT NULL CHECK (source IN ('agent', 'legacy')),
                 PRIMARY KEY (account_id, shipment_id, merchant_key, order_key));
             ALTER TABLE shipments DROP COLUMN order_merchant_msg;",
        )
        .unwrap();
        migrate(&conn).unwrap();
        let columns = |table: &str| -> Vec<String> {
            conn.prepare(&format!("PRAGMA table_info({table})"))
                .unwrap()
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap()
        };
        let links = columns("shipment_order_links");
        assert!(links.contains(&"message_id".to_string()), "{links:?}");
        assert!(links.contains(&"merchant_msg".to_string()), "{links:?}");
        assert!(columns("shipments").contains(&"order_merchant_msg".to_string()));
    }
}

#[test]
fn migrate_adds_a_background_foreground_flag_to_preexisting_followups() {
    // An install whose follow-ups predate the lane flag. Every such row was a
    // background request, so the column arrives as 0, and a re-open is a no-op.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE agent_triage_followups(
             job_id INTEGER PRIMARY KEY, kind TEXT NOT NULL, trigger TEXT NOT NULL,
             arrival_eligible INTEGER NOT NULL DEFAULT 0);
         INSERT INTO agent_triage_followups VALUES(7,'triage','manual:x',0);",
    )
    .unwrap();
    migrate(&conn).unwrap();
    migrate(&conn).unwrap();
    let lane: i64 = conn
        .query_row(
            "SELECT foreground FROM agent_triage_followups WHERE job_id=7",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(lane, 0);
}

#[test]
fn init_upgrades_a_single_slot_drafts_table_to_many_drafts_and_forwards() {
    // An install from before many drafts: no `forward_of_message_id`, and the
    // single-slot `idx_drafts_new`, with its one new-message draft saved. Going
    // through `init` (schema.sql THEN migrate) is the point: an index on the
    // new column in schema.sql would fail right here, before migrate ran.
    //
    // A FILE, not memory: the second `init` below has to reopen the same DB.
    register_vec_extension();
    let dir = std::env::temp_dir().join(format!("squelch-drafts-many-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("drafts.db");
    let _ = std::fs::remove_file(&path);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE drafts(
             id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL,
             reply_to_message_id INTEGER,
             to_addr TEXT NOT NULL DEFAULT '', cc_addr TEXT NOT NULL DEFAULT '',
             bcc_addr TEXT NOT NULL DEFAULT '', subject TEXT NOT NULL DEFAULT '',
             body TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL);
         CREATE UNIQUE INDEX idx_drafts_reply
             ON drafts(account_id, reply_to_message_id) WHERE reply_to_message_id IS NOT NULL;
         CREATE UNIQUE INDEX idx_drafts_new
             ON drafts(account_id) WHERE reply_to_message_id IS NULL;
         INSERT INTO drafts(account_id, subject, body, created_at, updated_at)
             VALUES (1, 'old', 'kept', '2026-07-01T00:00:00+00:00', '2026-07-01T00:00:00+00:00');",
    )
    .unwrap();
    let store = SqliteStore::init(conn).unwrap();
    let acct = store.ensure_account("me@example.com").unwrap();
    assert_eq!(acct, 1, "the fixture's row belongs to the first account");
    {
        let conn = store.lock().unwrap();
        assert_eq!(
            idx_drafts_new_unique(&conn),
            Some(false),
            "the single-slot index is replaced by a NON-unique one of the same name"
        );
        // Two MORE nil-keyed rows now insert beside the pre-existing one.
        for _ in 0..2 {
            conn.execute(
                "INSERT INTO drafts(account_id, created_at, updated_at)
                 VALUES (1, '2026-07-02T00:00:00+00:00', '2026-07-02T00:00:00+00:00')",
                [],
            )
            .expect("many new-message drafts");
        }
        // The forward key exists and is unique per target.
        conn.execute(
            "INSERT INTO drafts(account_id, forward_of_message_id, created_at, updated_at)
             VALUES (1, 5, '2026-07-03T00:00:00+00:00', '2026-07-03T00:00:00+00:00')",
            [],
        )
        .unwrap();
        let dup = conn
            .execute(
                "INSERT INTO drafts(account_id, forward_of_message_id, created_at, updated_at)
                 VALUES (1, 5, '2026-07-03T00:00:00+00:00', '2026-07-03T00:00:00+00:00')",
                [],
            )
            .expect_err("a second forward draft of one message is refused")
            .to_string();
        assert!(dup.contains("UNIQUE"), "{dup}");
    }
    // The pre-existing draft reads back through the store, key columns NULL.
    let all = store.list_drafts(acct).unwrap();
    let old = all.iter().find(|d| d.subject == "old").unwrap();
    assert_eq!(old.body, "kept");
    assert!(old.reply_to_message_id.is_none() && old.forward_of_message_id.is_none());
    // Migrating again is a no-op.
    migrate(&store.lock().unwrap()).unwrap();
    // And so is a whole second open of the same file, with three keyless rows.
    drop(store);
    let store = SqliteStore::init(Connection::open(&path).unwrap()).unwrap();
    assert_eq!(
        idx_drafts_new_unique(&store.lock().unwrap()),
        Some(false),
        "a re-open leaves the guard index non-unique"
    );
    assert_eq!(store.list_drafts(acct).unwrap().len(), 4);
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `Some(unique)` for `idx_drafts_new`, `None` when there is no such index.
fn idx_drafts_new_unique(conn: &Connection) -> Option<bool> {
    conn.query_row(
        "SELECT \"unique\" FROM pragma_index_list('drafts') WHERE name = 'idx_drafts_new'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .optional()
    .unwrap()
    .map(|u| u == 1)
}

#[test]
fn an_older_binarys_unique_drafts_index_create_is_a_no_op_after_many_drafts() {
    // ROLLBACK: an older daemon's schema.sql runs this exact statement on every
    // open. Once a user holds two keyless drafts it must not fail, or the old
    // binary cannot open the DB at all. It is a no-op only because the new
    // schema keeps an index of the same NAME (IF NOT EXISTS checks nothing else).
    let store = SqliteStore::open_in_memory().unwrap();
    let acct = store.ensure_account("me@example.com").unwrap();
    store
        .upsert_draft(
            acct,
            DraftKey::NewCreate,
            DraftFields::default(),
            Utc::now(),
        )
        .unwrap();
    store
        .upsert_draft(
            acct,
            DraftKey::NewCreate,
            DraftFields::default(),
            Utc::now(),
        )
        .unwrap();
    let conn = store.lock().unwrap();
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_drafts_new
             ON drafts(account_id) WHERE reply_to_message_id IS NULL",
        [],
    )
    .expect("the old binary's create is a no-op, not a UNIQUE violation");
    assert_eq!(idx_drafts_new_unique(&conn), Some(false));
}

/// Re-triage queued by the retired rule-change fan-out is closed on open, so a
/// backlog cannot keep draining the budget. In-flight and other work is kept.
#[test]
fn migrate_retires_queued_rule_change_retriage() {
    let (store, acct) = store();
    let conn = store.lock().unwrap();
    for (message, trigger, state) in [
        (1, "preference:abc", "queued"),
        (2, "preference:abc", "leased"),
        (3, "ingest", "queued"),
    ] {
        conn.execute(
            "INSERT INTO agent_triage_jobs(account_id,message_id,kind,trigger,input_revision,state,available_at)
             VALUES(?1,?2,'triage',?3,1,?4,'2026-09-29T00:00:00Z')",
            params![acct, message, trigger, state],
        )
        .unwrap();
    }
    migrate(&conn).unwrap();
    migrate(&conn).unwrap(); // idempotent
    let states: Vec<(i64, String)> = conn
        .prepare("SELECT message_id,state FROM agent_triage_jobs ORDER BY message_id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(
        states,
        vec![
            (1, "completed".into()),
            (2, "leased".into()),
            (3, "queued".into())
        ]
    );
}

/// The calendar ghost tables are new and need no backfill, so `schema.sql`
/// alone brings them: on a fresh store, and on an existing mailbox from
/// before they existed (simulated by dropping them), whose mail survives.
/// A second open of a DB that already has them is a no-op.
#[test]
fn init_creates_the_ghost_tables_on_fresh_and_existing_dbs() {
    const TABLES: [&str; 4] = [
        "ghost_events",
        "ghost_event_keys",
        "ghost_event_sources",
        "event_duration_priors",
    ];
    const TRIGGERS: [&str; 2] = [
        "ghost_event_rows_follow_ghost",
        "ghost_event_sources_follow_message",
    ];
    fn present(conn: &Connection) -> (usize, usize, Vec<String>) {
        let named = |kind: &str, names: &[&str]| -> usize {
            names
                .iter()
                .filter(|name| {
                    conn.query_row(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type=?1 AND name=?2",
                        params![kind, name],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap()
                        == 1
                })
                .count()
        };
        let mut stmt = conn.prepare("PRAGMA table_info(ghost_events)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        (named("table", &TABLES), named("trigger", &TRIGGERS), cols)
    }
    let (fresh, _) = store();
    let (tables, triggers, cols) = present(&fresh.lock().unwrap());
    assert_eq!((tables, triggers), (4, 2));
    for expected in [
        "id",
        "account_id",
        "kind",
        "title",
        "starts_at",
        "start_sort",
        "start_timezone",
        "ends_at",
        "est_minutes",
        "est_basis",
        "location",
        "venue_key",
        "commitment",
        "owner_scope",
        "owner_addr",
        "mail_state",
        "likelihood_band",
        "likelihood_reason",
        "supplier_message_id",
        "last_message_id",
        "created_at",
        "updated_at",
        "user_state",
        "user_state_at",
        "user_end_minutes",
        "user_end_days",
    ] {
        assert!(cols.iter().any(|c| c == expected), "missing {expected}");
    }

    let dir = std::env::temp_dir().join(format!("squelch-ghost-tables-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mail.db");
    let _ = std::fs::remove_file(&path);
    let acct;
    {
        let old = SqliteStore::open(&path).unwrap();
        acct = old.ensure_account("me@example.com").unwrap();
        triaged_row(acct, "g-1", "t-1", None, false, Sensitivity::Normal).ingest(&old);
        old.lock()
            .unwrap()
            .execute_batch(
                "DROP TRIGGER ghost_event_rows_follow_ghost;
                 DROP TRIGGER ghost_event_sources_follow_message;
                 DROP TABLE ghost_events; DROP TABLE ghost_event_keys;
                 DROP TABLE ghost_event_sources; DROP TABLE event_duration_priors;",
            )
            .unwrap();
        assert_eq!(present(&old.lock().unwrap()).0, 0);
    }
    {
        let upgraded = SqliteStore::open(&path).unwrap();
        let conn = upgraded.lock().unwrap();
        assert_eq!(present(&conn).0, 4);
        assert_eq!(present(&conn).1, 2);
        let mail: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mail, 1, "the mailbox survives the upgrade");
        conn.execute(
            "INSERT INTO ghost_events(account_id,kind,title,commitment,created_at,updated_at)
             VALUES(?1,'ticket','Show','committed','2026-09-30T00:00:00Z','2026-09-30T00:00:00Z')",
            [acct],
        )
        .unwrap();
    }
    let reopened = SqliteStore::open(&path).unwrap();
    let ghosts: i64 = reopened
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM ghost_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ghosts, 1, "a later open keeps what is there");
    drop(reopened);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The Google Calendar tables are new and need no backfill either: present on
/// a fresh store with the columns the sync and the timeline use, created on an
/// existing mailbox from before them (dropped here), whose mail survives, and
/// left alone by a later open.
#[test]
fn init_creates_the_calendar_tables_on_fresh_and_existing_dbs() {
    fn columns(conn: &Connection, table: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }
    fn index_exists(conn: &Connection) -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_calendar_events_start'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
            == 1
    }
    let (fresh, _) = store();
    {
        let conn = fresh.lock().unwrap();
        assert_eq!(
            columns(&conn, "calendar_calendars"),
            [
                "account_id",
                "calendar_id",
                "summary",
                "is_primary",
                "selected",
                "time_zone",
                "sync_token",
                "synced_at",
                "full_synced_at",
                "last_error_kind",
                "failures",
                "retry_at",
                "truncated",
                "color",
                "access_role",
            ]
        );
        assert_eq!(
            columns(&conn, "calendar_events"),
            [
                "account_id",
                "calendar_id",
                "event_id",
                "ical_uid",
                "status",
                "summary",
                "location",
                "starts_at",
                "start_sort",
                "ends_at",
                "all_day",
                "start_timezone",
                "organizer_email",
                "self_response",
                "transparent",
                "html_link",
                "updated_at",
                "color_id",
                "organizer_self",
                "description",
                "guests_truncated",
                "organizer_unreadable",
            ]
        );
        assert_eq!(
            columns(&conn, "calendar_event_guests"),
            [
                "account_id",
                "calendar_id",
                "event_id",
                "position",
                "email",
                "name",
                "response",
                "is_organizer",
                "is_self",
            ]
        );
        assert!(index_exists(&conn));
    }

    let dir = std::env::temp_dir().join(format!("squelch-calendar-tables-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mail.db");
    let _ = std::fs::remove_file(&path);
    let acct;
    {
        let old = SqliteStore::open(&path).unwrap();
        acct = old.ensure_account("me@example.com").unwrap();
        triaged_row(acct, "g-1", "t-1", None, false, Sensitivity::Normal).ingest(&old);
        old.lock()
            .unwrap()
            .execute_batch(
                "DROP TABLE calendar_event_guests; DROP TABLE calendar_events;
                 DROP TABLE calendar_calendars;",
            )
            .unwrap();
    }
    {
        let upgraded = SqliteStore::open(&path).unwrap();
        let conn = upgraded.lock().unwrap();
        assert_eq!(columns(&conn, "calendar_events").len(), 22);
        assert_eq!(columns(&conn, "calendar_event_guests").len(), 9);
        assert!(index_exists(&conn));
        let mail: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mail, 1, "the mailbox survives the upgrade");
        conn.execute(
            "INSERT INTO calendar_calendars(account_id,calendar_id,is_primary) VALUES(?1,'primary',1)",
            [acct],
        )
        .unwrap();
    }
    let reopened = SqliteStore::open(&path).unwrap();
    let kept: i64 = reopened
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM calendar_calendars", [], |r| r.get(0))
        .unwrap();
    assert_eq!(kept, 1, "a later open keeps what is there");
    // A calendar table from before the per-calendar failure state, the
    // colour and the role (and an events table from before the colour, the
    // organizer flag and the description) gains them, keeping its rows, with
    // the defaults that mean "never failed, not cut".
    reopened
        .lock()
        .unwrap()
        .execute_batch(
            "ALTER TABLE calendar_calendars DROP COLUMN failures;
             ALTER TABLE calendar_calendars DROP COLUMN retry_at;
             ALTER TABLE calendar_calendars DROP COLUMN truncated;
             ALTER TABLE calendar_calendars DROP COLUMN color;
             ALTER TABLE calendar_calendars DROP COLUMN access_role;
             ALTER TABLE calendar_events DROP COLUMN color_id;
             ALTER TABLE calendar_events DROP COLUMN organizer_self;
             ALTER TABLE calendar_events DROP COLUMN description;
             ALTER TABLE calendar_events DROP COLUMN guests_truncated;
             ALTER TABLE calendar_events DROP COLUMN organizer_unreadable;
             UPDATE calendar_calendars SET sync_token='minted-with-one-attendee',
                    full_synced_at='2026-10-01T00:00:00+00:00', synced_at='2026-10-01T00:00:00+00:00';",
        )
        .unwrap();
    drop(reopened);
    let reopened = SqliteStore::open(&path).unwrap();
    {
        let conn = reopened.lock().unwrap();
        assert_eq!(columns(&conn, "calendar_calendars").len(), 15);
        assert_eq!(columns(&conn, "calendar_events").len(), 22);
        // A store from before guests was synced one attendee at a time: its
        // tokens are forgotten, so the next pass reads every calendar in full
        // with the new parameters.
        let sync: (Option<String>, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT sync_token, full_synced_at, synced_at FROM calendar_calendars",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((sync.0, sync.1), (None, None));
        assert!(sync.2.is_some(), "when it was last read is kept");
        conn.execute("UPDATE calendar_calendars SET sync_token='fresh'", [])
            .unwrap();
        let row: (i64, Option<String>, i64) = conn
            .query_row(
                "SELECT failures, retry_at, truncated FROM calendar_calendars",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, (0, None, 0));
    }
    drop(reopened);
    // A later open, with the column there, leaves the token alone.
    let reopened = SqliteStore::open(&path).unwrap();
    let token: Option<String> = reopened
        .lock()
        .unwrap()
        .query_row("SELECT sync_token FROM calendar_calendars", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(token.as_deref(), Some("fresh"));
    // A store from AFTER guests but before the unreadable-organizer mark
    // (verify2 V9): only that column is new, and it alone must make every
    // calendar forget its token and last full read, so the next pass rereads
    // in full and sets the mark.
    reopened
        .lock()
        .unwrap()
        .execute_batch(
            "ALTER TABLE calendar_events DROP COLUMN organizer_unreadable;
             UPDATE calendar_calendars SET sync_token='minted-before-the-mark',
                    full_synced_at='2026-10-02T00:00:00+00:00';",
        )
        .unwrap();
    drop(reopened);
    let reopened = SqliteStore::open(&path).unwrap();
    {
        let conn = reopened.lock().unwrap();
        assert_eq!(columns(&conn, "calendar_events").len(), 22);
        let sync: (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT sync_token, full_synced_at FROM calendar_calendars",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            sync,
            (None, None),
            "the organizer mark alone forgets the tokens"
        );
    }
    drop(reopened);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A guest list is stored with its event, replaced whole when the event is
/// written again, read back in Google's order, scoped by account, and GONE
/// with the event whichever path deletes it (the trigger).
#[test]
fn an_events_guests_are_stored_with_it_and_go_with_it() {
    use crate::gcal::{
        CalendarMeta, EventItem, EventRow, Guest, GuestResponse, MAX_GUESTS, PRIMARY,
    };
    use crate::triage::ghost::EventTime;
    let (store, acct) = store();
    let other = store.ensure_account("other@example.com").unwrap();
    let now: DateTime<Utc> = "2026-10-05T12:00:00Z".parse().unwrap();
    let guest = |email: &str, response: GuestResponse, is_self: bool| Guest {
        email: email.into(),
        name: (!is_self).then(|| "A Guest".to_string()),
        response,
        organizer: is_self,
        is_self,
    };
    let event = |id: &str, guests: Vec<Guest>, truncated| EventRow {
        event_id: id.into(),
        ical_uid: None,
        tentative: false,
        summary: Some("Party".into()),
        location: None,
        start: EventTime::parse("2026-10-06T19:00:00-04:00").unwrap(),
        end: None,
        start_timezone: None,
        organizer_email: None,
        organizer_unreadable: false,
        organizer_self: true,
        self_response: None,
        transparent: false,
        html_link: None,
        updated_at: None,
        color_id: None,
        description: None,
        guests,
        guests_truncated: truncated,
    };
    let count = |id: &str| -> i64 {
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM calendar_event_guests WHERE event_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap()
    };
    for account in [acct, other] {
        store
            .calendar_set_selection(account, &[CalendarMeta::primary_only()])
            .unwrap();
    }
    let three = vec![
        guest("me@example.com", GuestResponse::Accepted, true),
        guest("zed@example.com", GuestResponse::Declined, false),
        guest("amy@example.com", GuestResponse::Pending, false),
    ];
    assert!(
        store
            .calendar_put_event(acct, PRIMARY, &event("a", three.clone(), false))
            .unwrap()
    );
    store
        .calendar_put_event(acct, PRIMARY, &event("b", three[..2].to_vec(), true))
        .unwrap();
    // Read back whole, in the order given (not by address), one event or a
    // listing.
    let one = store.calendar_event(acct, PRIMARY, "a").unwrap().unwrap();
    assert_eq!(one.row.guests, three);
    assert!(!one.row.guests_truncated);
    assert_eq!(one.row.other_guests(), 2);
    let from = (now - chrono::Duration::days(1)).fixed_offset();
    let to = (now + chrono::Duration::days(30)).fixed_offset();
    let (listed, _) = store.list_calendar_events(acct, from, to, now).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].row.guests, three);
    assert_eq!(listed[1].row.guests, three[..2]);
    assert!(listed[1].row.guests_truncated);
    // Another account holds none of it.
    assert!(store.calendar_event(other, PRIMARY, "a").unwrap().is_none());
    assert!(
        store
            .list_calendar_events(other, from, to, now)
            .unwrap()
            .0
            .is_empty()
    );
    // The same event id under another account keeps its own list.
    store
        .calendar_put_event(other, PRIMARY, &event("a", three[..1].to_vec(), false))
        .unwrap();
    assert_eq!(
        store
            .calendar_event(acct, PRIMARY, "a")
            .unwrap()
            .unwrap()
            .row
            .guests
            .len(),
        3
    );
    // Written again with fewer: replaced, not added to.
    store
        .calendar_put_event(acct, PRIMARY, &event("a", three[..2].to_vec(), false))
        .unwrap();
    assert_eq!(
        store
            .calendar_event(acct, PRIMARY, "a")
            .unwrap()
            .unwrap()
            .row
            .guests,
        three[..2]
    );
    // Never more than the cap, whatever it is handed.
    let crowd: Vec<Guest> = (0..MAX_GUESTS + 20)
        .map(|i| {
            guest(
                &format!("g{i}@example.com"),
                GuestResponse::Tentative,
                false,
            )
        })
        .collect();
    store
        .calendar_put_event(acct, PRIMARY, &event("c", crowd, true))
        .unwrap();
    assert_eq!(count("c"), MAX_GUESTS as i64);

    // GONE WITH THE EVENT, by each path that deletes one.
    assert_eq!(count("a"), 3, "two here, one under the other account");
    assert!(store.calendar_remove_event(acct, PRIMARY, "a").unwrap());
    assert_eq!(
        count("a"),
        1,
        "the user's delete; the other account's stays"
    );
    store
        .calendar_apply_page(acct, PRIMARY, &[EventItem::Remove("b".into())], now)
        .unwrap();
    assert_eq!(count("b"), 0, "a cancelled event in a synced page");
    store
        .calendar_finish_sync(
            acct,
            PRIMARY,
            Some("t"),
            now,
            Some((&Default::default(), false)),
        )
        .unwrap();
    assert_eq!(count("c"), 0, "a full read's sweep");
    store.calendar_set_selection(other, &[]).unwrap();
    assert_eq!(count("a"), 0, "a calendar that stops being read");
    let left: i64 = store
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM calendar_event_guests", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(left, 0);
}

/// The per-connection tuning `set_pragmas` applies on every open, asserted on a
/// FILE-backed store because the journal limit means nothing to `:memory:`.
/// Each value is read back through the same pragma, so a typo in a name (which
/// SQLite ignores silently) fails here instead of in production; and the two
/// settings deliberately left at their defaults are pinned at those defaults,
/// so adding one back has to argue with the doc on `set_pragmas` first.
#[test]
fn open_tunes_the_connection_pragmas() {
    let dir = std::env::temp_dir().join(format!("squelch-pragmas-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pragmas.db");

    {
        let store = SqliteStore::open(&path).unwrap();
        let conn = store.lock().unwrap();
        let read = |name: &str| -> i64 {
            conn.query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(read("cache_size"), CACHE_SIZE_KIB_NEGATED);
        assert_eq!(read("journal_size_limit"), JOURNAL_SIZE_LIMIT_BYTES);
        // Left alone on purpose: FULL (2) and FILE (0 or 1); see `set_pragmas`.
        assert_eq!(read("synchronous"), 2, "synchronous must stay FULL");
        assert_ne!(read("temp_store"), 2, "temp_store must not be MEMORY");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the three indexes the 50k-row bench asked for -----------------------

/// Three listings that walked the whole account on the planner bench, pinned
/// by PLAN rather than by timing (a timing test is flaky on CI and a plan is
/// exact), and pinned on the SHIPPED statements (the `const`s the store runs)
/// so an edit to a query that loses its seek fails here. The second half drops
/// each index and asserts the plan really degrades, so a rename that silently
/// detaches one cannot pass either:
///
/// - `append_due_reminders` runs on EVERY Standing-band read (the client's
///   10 s poll) and asked for `reminded_at IS NOT NULL` with no index over it:
///   a walk of every triage row in the account, 233 ms at 50k messages;
/// - `message_opens` (and the sent and group listings' per-row open counts)
///   reached `send_trackers` through `token`, its only key, so each was a SCAN
///   of the whole tracker table;
/// - `agent_diagnostics` reads `agent_triage_runs ORDER BY id DESC LIMIT 25`,
///   and the table had no index at all over a column that grows forever.
#[test]
fn reminder_tracker_and_run_lookups_seek_their_indexes() {
    use super::super::agent_triage::{DUE_REMINDERS_SQL, MESSAGE_RUNS_SQL};
    use super::super::tracking::MESSAGE_OPENS_SQL;

    let (store, _acct) = store();
    let conn = store.lock().unwrap();

    /// Whether `table` (the alias, when there is one) is reached through
    /// `index`.
    fn seeks(plan: &[String], table: &str, index: &str) -> bool {
        plan.iter()
            .any(|line| line.starts_with(&format!("SEARCH {table} USING")) && line.contains(index))
    }
    /// Whether `table` is walked whole.
    fn scans(plan: &[String], table: &str) -> bool {
        plan.iter()
            .any(|line| line.starts_with(&format!("SCAN {table}")))
    }

    let p = plan(&conn, DUE_REMINDERS_SQL);
    assert!(
        seeks(&p, "t", "idx_triage_reminded_at"),
        "due reminders walk the account's triage rows:\n{p:#?}"
    );
    let p = plan(&conn, MESSAGE_OPENS_SQL);
    assert!(
        seeks(&p, "t", "idx_send_trackers_message"),
        "message opens scan send_trackers:\n{p:#?}"
    );
    let p = plan(&conn, MESSAGE_RUNS_SQL);
    assert!(
        seeks(&p, "agent_triage_runs", "idx_agent_triage_runs_message"),
        "run diagnostics scan agent_triage_runs:\n{p:#?}"
    );

    // THE CANARY: with the indexes gone the same statements must degrade to
    // what they did before, or the assertions above were never testing the
    // index. Each is asserted on the shape the planner actually falls back
    // to, not on the absence of a name that cannot appear once dropped: the
    // reminders are driven from `messages` with `t` reached by primary key
    // instead of through an index, the opens walk `message_opens` whole and
    // reach each tracker by token, and the runs become a table scan.
    conn.execute_batch(
        "DROP INDEX idx_triage_reminded_at;
         DROP INDEX idx_send_trackers_message;
         DROP INDEX idx_agent_triage_runs_message;",
    )
    .unwrap();
    let p = plan(&conn, DUE_REMINDERS_SQL);
    assert!(
        !p.iter()
            .any(|line| line.starts_with("SEARCH t USING INDEX")),
        "canary: reminders still reach triage through an index:\n{p:#?}"
    );
    let p = plan(&conn, MESSAGE_OPENS_SQL);
    assert!(
        scans(&p, "o") && !seeks(&p, "t", "idx_send_trackers_message"),
        "canary: opens do not fall back to walking message_opens:\n{p:#?}"
    );
    let p = plan(&conn, MESSAGE_RUNS_SQL);
    assert!(
        scans(&p, "agent_triage_runs"),
        "canary: runs do not fall back to a table scan:\n{p:#?}"
    );
}

/// The planner has statistics to plan with, from the pass the daemon runs on a
/// schedule: the real tables are analyzed, the bulky FTS and vector shadow
/// tables (the largest b-trees in the file, which nothing plans against) are
/// not, and a later pass notices a table that grew.
#[test]
fn refresh_statistics_analyzes_the_real_tables_and_optimize_keeps_up() {
    let (store, acct) = store();
    for i in 0..5 {
        let _ = triaged(acct, &format!("g{i}"), &format!("t{i}")).upsert(&store);
    }
    // Opening analyzed nothing: that work is the scheduled pass's, not the
    // open path's (a CLI open must not take the write lock for it).
    let stat_table_exists = |store: &SqliteStore| -> bool {
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='sqlite_stat1')",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert!(!stat_table_exists(&store), "open must not analyze");

    store.refresh_statistics().unwrap();
    let stat_for = |store: &SqliteStore, tbl: &str| -> Vec<(String, String)> {
        store
            .lock()
            .unwrap()
            .prepare("SELECT idx, stat FROM sqlite_stat1 WHERE tbl = ?1 ORDER BY idx")
            .unwrap()
            .query_map([tbl], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    };
    let first = stat_for(&store, "messages");
    assert!(!first.is_empty(), "messages has statistics after the pass");
    // The pass's own ANALYZE loop never names a shadow table. `PRAGMA
    // optimize` then takes statistics for the few small keyed ones whose
    // index the connection consulted (the FTS term index and the two config
    // tables), which is SQLite's call and a few rows each; the bulky ones,
    // the body blobs, docsizes and vector chunks, have no index to plan with
    // and are never walked.
    let shadow_rows: Vec<String> = store
        .lock()
        .unwrap()
        .prepare(
            "SELECT DISTINCT tbl FROM sqlite_stat1
             WHERE tbl LIKE 'messages\\_fts%' ESCAPE '\\'
                OR tbl LIKE 'message\\_vecs%' ESCAPE '\\'
             ORDER BY tbl",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    let keyed_and_small = [
        "message_vecs_info",
        "messages_fts_config",
        "messages_fts_idx",
    ];
    assert!(
        shadow_rows
            .iter()
            .all(|t| keyed_and_small.contains(&t.as_str())),
        "only the small keyed shadow tables may carry statistics: {shadow_rows:?}"
    );
    for bulky in [
        "messages_fts_data",
        "messages_fts_docsize",
        "messages_fts_content",
        "message_vecs_chunks",
        "message_vecs_rowids",
        "message_vecs_vector_chunks00",
    ] {
        assert!(
            !shadow_rows.iter().any(|t| t == bulky),
            "{bulky} holds the bodies or the vectors and is never walked"
        );
    }

    // Fifty times the rows (optimize's growth gate is a b-tree page estimate,
    // too coarse to trust at ten), read through an index so the connection
    // has planned against the table, and the incremental pass re-analyzes
    // it, at the full limit: the second line of statistics is the honest
    // one, not SQLite's 2000-row sample.
    for i in 5..255 {
        let _ = triaged(acct, &format!("g{i}"), &format!("t{i}")).upsert(&store);
    }
    let _: i64 = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE account_id = ?1 AND received_at > ''",
            [acct],
            |r| r.get(0),
        )
        .unwrap();
    store.refresh_statistics().unwrap();
    let second = stat_for(&store, "messages");
    assert_ne!(first, second, "a table that grew 50x is re-analyzed");
    let received = second
        .iter()
        .find(|(idx, _)| idx == "idx_messages_received")
        .map(|(_, stat)| stat.as_str())
        .expect("the (account_id, received_at) index has statistics");
    assert_eq!(
        received, "255 255 1",
        "every row was counted, so `account_id = ?` is known to match them all"
    );
}

#[test]
fn migrate_adds_skipped_to_a_prerelease_event_revisit_ledger() {
    // A database a pre-release build opened has the past-event ledger without
    // `skipped`; without it every scheduler tick would fail its query.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE event_revisit_asked(
             account_id INTEGER NOT NULL, message_id INTEGER NOT NULL,
             ask_at TEXT NOT NULL, asked_at TEXT NOT NULL,
             PRIMARY KEY(account_id, message_id));
         INSERT INTO event_revisit_asked VALUES(1,1,'a','b');",
    )
    .unwrap();
    migrate(&conn).unwrap();
    migrate(&conn).unwrap(); // idempotent
    let skipped: i64 = conn
        .query_row("SELECT skipped FROM event_revisit_asked", [], |r| r.get(0))
        .unwrap();
    assert_eq!(skipped, 0);
}

/// A sent row stored straight into `messages`, with no index rows: what the
/// recipients backfill exists to find.
fn raw_sent(conn: &Connection, id: i64, account: i64, to: &str) {
    conn.execute(
        "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,
             snippet,body,is_sent,to_addrs)
         VALUES(?1,?2,?3,?3,'me@x.com','subject','2026-01-01T00:00:00+00:00','','',1,?4)",
        params![id, account, id.to_string(), to],
    )
    .unwrap();
}

fn indexed(conn: &Connection, message: i64) -> Vec<String> {
    conn.prepare("SELECT addr FROM message_recipients WHERE message_id=?1 ORDER BY addr")
        .unwrap()
        .query_map([message], |r| r.get(0))
        .unwrap()
        .map(|addr| addr.unwrap())
        .collect()
}

/// EVERY OPEN HEALS EVERY ROW WITH NONE. A sent row with recipients stored and
/// no index rows is indexed whenever it turns up: on the first open, and again
/// after its rows are lost (a re-ingest whose raw does not parse clears them
/// while the stored columns keep their text). Bcc alone is enough. Received
/// mail is never indexed, nor is sent mail whose recipients were never stored
/// (the recipients sweep's queue) or were stored empty. And the candidates come
/// from the indexes, the table fetched by rowid only, with statistics and
/// without.
#[test]
fn recipients_backfill_heals_on_every_open_from_the_indexes() {
    use super::super::migrate::RECIPIENTS_CANDIDATES_SQL;
    let store = SqliteStore::open_in_memory().unwrap();
    let conn = store.lock().unwrap();
    conn.execute(
        "INSERT INTO accounts(id,email,created_at) VALUES(1,'me@x.com','2026-01-01')",
        [],
    )
    .unwrap();
    raw_sent(&conn, 1, 1, "Ann <ann@x.com>, Bo <bo@x.com>");
    conn.execute(
        "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,
             snippet,body,is_sent,to_addrs)
         VALUES(2,1,'2','2','ann@x.com','subject','2026-01-01T00:00:00+00:00','','',0,'me@x.com')",
        [],
    )
    .unwrap();
    // The sweep's queue (nothing stored), stored empty, and Bcc alone.
    conn.execute_batch(
        "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,
             snippet,body,is_sent,to_addrs,cc_addrs,bcc_addrs)
         VALUES(3,1,'3','3','me@x.com','s','2026-01-01T00:00:00+00:00','','',1,NULL,NULL,NULL),
               (4,1,'4','4','me@x.com','s','2026-01-01T00:00:00+00:00','','',1,'','',''),
               (5,1,'5','5','me@x.com','s','2026-01-01T00:00:00+00:00','','',1,'','','Cy <cy@x.com>');",
    )
    .unwrap();
    migrate(&conn).unwrap();
    assert_eq!(indexed(&conn, 1), ["ann@x.com", "bo@x.com"]);
    assert!(indexed(&conn, 2).is_empty());
    assert!(indexed(&conn, 3).is_empty());
    assert!(indexed(&conn, 4).is_empty());
    assert_eq!(indexed(&conn, 5), ["cy@x.com"]);
    let candidates: Vec<i64> = conn
        .prepare(RECIPIENTS_CANDIDATES_SQL)
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|id| id.unwrap())
        .collect();
    assert!(candidates.is_empty(), "{candidates:?}");

    conn.execute("DELETE FROM message_recipients WHERE message_id=1", [])
        .unwrap();
    migrate(&conn).unwrap();
    assert_eq!(indexed(&conn, 1), ["ann@x.com", "bo@x.com"]);

    let mut plans = vec![plan(&conn, RECIPIENTS_CANDIDATES_SQL)];
    conn.execute_batch("ANALYZE").unwrap();
    plans.push(plan(&conn, RECIPIENTS_CANDIDATES_SQL));
    for plan in plans {
        assert!(
            plan.iter()
                .any(|line| line.starts_with("SCAN s USING COVERING INDEX idx_messages_sent")),
            "{plan:#?}"
        );
        assert!(
            plan.iter()
                .any(|line| line.starts_with("SEARCH r USING COVERING INDEX")),
            "{plan:#?}"
        );
        let table: Vec<&String> = plan.iter().filter(|line| line.contains(" m ")).collect();
        assert!(
            !table.is_empty()
                && table
                    .iter()
                    .all(|line| line.starts_with("SEARCH m USING INTEGER PRIMARY KEY")),
            "the table is only fetched by rowid:\n{plan:#?}"
        );
    }
}

/// The open-time attention repairs at the end of schema.sql probe a thread's
/// messages from `idx_messages_thread` alone, never by reading the decision
/// message's row.
#[test]
fn schema_attention_repairs_probe_the_thread_index() {
    let store = SqliteStore::open_in_memory().unwrap();
    let conn = store.lock().unwrap();
    let start = SCHEMA
        .find("-- Repair beta related-thread projections")
        .unwrap();
    let mut repairs = Vec::new();
    for opening in [
        "INSERT OR IGNORE INTO agent_attention_sources(account_id,thread_id,source_message_id,source_revision)\nSELECT a.account_id,a.thread_id,d.source_message_id",
        "INSERT OR IGNORE INTO agent_attention_sources(account_id,thread_id,source_message_id,source_revision)\nSELECT a.account_id,a.thread_id,a.decision_message_id",
        "UPDATE agent_thread_attention SET decision_message_id=message_id",
    ] {
        let at = start + SCHEMA[start..].find(opening).unwrap();
        let end = at + SCHEMA[at..].find(';').unwrap();
        repairs.push(&SCHEMA[at..end]);
    }
    for statement in repairs {
        let plan = plan(&conn, statement);
        assert!(
            plan.iter().any(|line| line.starts_with(
                "SEARCH m USING COVERING INDEX idx_messages_thread (account_id=? AND thread_id=? AND rowid=?)"
            )),
            "{statement}\n{plan:#?}"
        );
    }
}

#[test]
fn migrate_gives_every_preexisting_account_the_gmail_provider() {
    // An `accounts` table from before providers: every row in it is a Gmail
    // mailbox, because Gmail was the only kind there was.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE accounts(id INTEGER PRIMARY KEY, email TEXT UNIQUE NOT NULL,
             created_at TEXT NOT NULL);
         INSERT INTO accounts VALUES (1, 'a@example.com', '2020-01-01T00:00:00+00:00');",
    )
    .unwrap();
    migrate(&conn).unwrap();
    migrate(&conn).unwrap(); // idempotent
    let provider: String = conn
        .query_row("SELECT provider FROM accounts WHERE id = 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(provider, crate::mail::MailProvider::Gmail.as_str());
    // A row written afterwards without naming a provider gets the same.
    conn.execute(
        "INSERT INTO accounts(id, email, created_at) VALUES (2, 'b@example.com', 'x')",
        [],
    )
    .unwrap();
    let provider: String = conn
        .query_row("SELECT provider FROM accounts WHERE id = 2", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(provider, "gmail");
}

/// Calendar search's index (schema.sql, CALENDAR SEARCH) holds exactly one
/// row per stored event, whichever path writes or deletes it, and a store
/// whose index is out of step (events written before it existed, or by an
/// older daemon) is rebuilt at open.
#[test]
fn calendar_search_index_follows_every_write_and_is_rebuilt_when_stale() {
    use crate::gcal::search::{Audience, CalendarQuery, search};
    use crate::gcal::{CalendarMeta, EventItem, EventRow, PRIMARY};
    use crate::triage::ghost::EventTime;
    let (store, acct) = store();
    let now = Utc::now();
    store
        .calendar_set_selection(acct, &[CalendarMeta::primary_only()])
        .unwrap();
    let event = |id: &str, title: &str| EventRow {
        event_id: id.into(),
        ical_uid: None,
        tentative: false,
        summary: Some(title.into()),
        location: None,
        start: EventTime::At((now + chrono::Duration::days(2)).fixed_offset()),
        end: None,
        start_timezone: None,
        organizer_email: None,
        organizer_unreadable: false,
        organizer_self: true,
        self_response: None,
        transparent: false,
        html_link: None,
        updated_at: None,
        color_id: None,
        description: None,
        guests: Vec::new(),
        guests_truncated: false,
    };
    let counts = || -> (i64, i64, i64) {
        let conn = store.lock().unwrap();
        let n = |sql: &str| conn.query_row(sql, [], |r| r.get(0)).unwrap();
        (
            n("SELECT COUNT(*) FROM calendar_events"),
            n("SELECT COUNT(*) FROM calendar_search_keys"),
            n("SELECT COUNT(*) FROM calendar_search_fts"),
        )
    };
    let titles = |q: &str| -> Vec<String> {
        search(
            &store,
            acct,
            &CalendarQuery::parse(q).unwrap(),
            Audience::Owner,
            true,
            now,
            50,
        )
        .unwrap()
        .hits
        .into_iter()
        .map(|h| match h {
            crate::gcal::search::CalendarHit::Event(e) => e.row.summary.unwrap_or_default(),
            crate::gcal::search::CalendarHit::Ghost(g) => g.title,
        })
        .collect()
    };
    let page = |items: Vec<EventItem>| {
        store
            .calendar_apply_page(acct, PRIMARY, &items, now)
            .unwrap();
    };
    page(vec![
        EventItem::Upsert(Box::new(event("a", "Pottery class"))),
        EventItem::Upsert(Box::new(event("b", "Dentist"))),
    ]);
    assert_eq!(counts(), (2, 2, 2));
    assert_eq!(titles("pottery"), ["Pottery class"]);

    // Rewritten: the old words go, the new ones come, no second row.
    page(vec![EventItem::Upsert(Box::new(event(
        "a",
        "Ceramics class",
    )))]);
    assert_eq!(counts(), (2, 2, 2));
    assert!(titles("pottery").is_empty());
    assert_eq!(titles("ceramics"), ["Ceramics class"]);

    // Every delete path: a removal in a page, the user's own delete, a
    // full sync's sweep.
    page(vec![EventItem::Remove("a".into())]);
    assert_eq!(counts(), (1, 1, 1));
    assert!(titles("ceramics").is_empty());
    store
        .calendar_put_event(acct, PRIMARY, &event("c", "Flight"))
        .unwrap();
    assert_eq!(counts(), (2, 2, 2));
    store.calendar_remove_event(acct, PRIMARY, "c").unwrap();
    assert_eq!(counts(), (1, 1, 1));
    let seen = std::collections::HashSet::new();
    store
        .calendar_finish_sync(acct, PRIMARY, Some("tok"), now, Some((&seen, false)))
        .unwrap();
    assert_eq!(counts(), (0, 0, 0));

    // Stale: an older daemon wrote events and kept no index.
    page(vec![EventItem::Upsert(Box::new(event(
        "d",
        "Yoga retreat",
    )))]);
    {
        let conn = store.lock().unwrap();
        conn.execute_batch("DELETE FROM calendar_search_fts; DELETE FROM calendar_search_keys;")
            .unwrap();
    }
    assert!(titles("yoga").is_empty());
    reindex_calendar_search_if_stale(&store.lock().unwrap()).unwrap();
    assert_eq!(counts(), (1, 1, 1));
    assert_eq!(titles("yoga"), ["Yoga retreat"]);
}
