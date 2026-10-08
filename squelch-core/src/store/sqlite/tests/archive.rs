//! The archive tier: mail older than the backfill window, stored to be found
//! and for nothing else. Every test here is about what an archive row must NOT
//! cause (a model job, a count, a listing) and the few places it must appear.

use super::super::*;
use super::support::*;
use crate::store::agent_triage::{AgentInventoryQuery, AgentTriageStore};
use crate::store::{SitrepBand, SpamScope};
use std::sync::Arc;

fn days_ago(days: i64) -> DateTime<Utc> {
    Utc::now() - chrono::Duration::days(days)
}

/// An archive row through the real archive write.
fn archive(store: &SqliteStore, builder: &TriagedBuilder) -> i64 {
    store
        .ingest_archive_message(&builder.build())
        .unwrap()
        .expect("a new id is stored")
}

fn count(store: &SqliteStore, sql: &str, message: i64) -> i64 {
    store
        .lock()
        .unwrap()
        .query_row(sql, params![message], |r| r.get(0))
        .unwrap()
}

/// Every job of every kind, in every state: the tier's whole promise.
fn jobs(store: &SqliteStore, message: i64) -> i64 {
    count(
        store,
        "SELECT COUNT(*) FROM agent_triage_jobs WHERE message_id=?1",
        message,
    )
}

fn state(store: &SqliteStore, message: i64) -> (i64, String) {
    store
        .lock()
        .unwrap()
        .query_row(
            "SELECT revision, access FROM agent_message_state WHERE message_id=?1",
            params![message],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

fn allow(store: &SqliteStore, message: i64) {
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_message_state SET access='allowed' WHERE message_id=?1",
            params![message],
        )
        .unwrap();
}

fn inventory(since: DateTime<Utc>) -> AgentInventoryQuery {
    AgentInventoryQuery {
        since,
        min_importance: None,
        status: None,
        band: None,
        pending_reminders: false,
        spam: SpamScope::Exclude,
        ranking: Default::default(),
        before: None,
        limit: None,
    }
}

#[test]
fn archive_ingest_stores_findable_mail_and_spends_nothing() {
    let (store, acct) = store();
    let old = days_ago(200);
    store
        .upsert_unsubscribe(acct, "news@old.example", "browser", None, days_ago(300))
        .unwrap();

    // Everything an arrival would act on, all at once: a shipping signal, a
    // notify stamp, a Date offset, an invitation, an unsubscribed sender.
    let mut inbound = triaged(acct, "a-in", "t-in")
        .from("news@old.example")
        .subject("quarterly heron survey")
        .body("the heron count is in")
        .received_at(old)
        .ship_extract(true)
        .auth_pass(Some(true))
        .date_offset(Some(-240))
        .invite("news@old.example", "America/New_York")
        .build();
    inbound.notify_eligible_at = Some(Utc::now());
    let inbound_id = store.ingest_archive_message(&inbound).unwrap().unwrap();

    let mut sent = triaged(acct, "a-out", "t-out")
        .from("me@example.com")
        .subject("re: heron survey")
        .body("thanks for the heron numbers")
        .received_at(old)
        .is_sent(true)
        .to_addrs("pat@old.example")
        .build();
    sent.recipients = vec!["pat@old.example".to_string()];
    let sent_id = store.ingest_archive_message(&sent).unwrap().unwrap();

    for id in [inbound_id, sent_id] {
        assert_eq!(jobs(&store, id), 0, "no job of any kind");
        assert_eq!(
            count(&store, "SELECT is_archive FROM messages WHERE id=?1", id),
            1
        );
        let (stage1, needs_stage2, ship, notify): (
            Option<String>,
            i64,
            Option<String>,
            Option<String>,
        ) = store
            .lock()
            .unwrap()
            .query_row(
                "SELECT stage1_model_used, needs_stage2, ship_extract_model, notify_eligible_at
                 FROM triage WHERE message_id=?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(stage1.as_deref(), Some("n/a"));
        assert_eq!(needs_stage2, 0);
        assert_eq!(ship, None);
        assert_eq!(notify, None, "an archive row can never notify");
        assert_eq!(state(&store, id), (1, "pending".to_string()));

        // The state row holds the snapshot the live writer would compute: a
        // later ordinary touch reads the row as unchanged and does nothing.
        store
            .enqueue_agent_triage(acct, id, "backfill", false)
            .unwrap();
        assert_eq!(state(&store, id), (1, "pending".to_string()));
        assert_eq!(jobs(&store, id), 0);
    }

    // Findable: search for the inbound row, the Sent page for the user's own.
    let hits = store.search(acct, "heron", 10, 0).unwrap();
    assert_eq!(
        hits.iter().map(|hit| hit.id).collect::<Vec<_>>(),
        [inbound_id]
    );
    let sent_page = store.sent_listing(acct, 10, 0).unwrap();
    assert_eq!(sent_page.len(), 1);

    // And none of an arrival's side effects.
    let conn = store.lock().unwrap();
    let n = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
    assert_eq!(
        n("SELECT COUNT(*) FROM contacts WHERE addr='pat@old.example'"),
        0,
        "no contact seeded"
    );
    assert_eq!(
        n("SELECT violation_count FROM unsubscribes"),
        0,
        "old mail is not a fresh violation"
    );
    assert_eq!(
        n("SELECT COUNT(*) FROM messages WHERE date_offset_minutes IS NOT NULL"),
        0,
        "the offset on the row is the timezone observation"
    );
    assert_eq!(n("SELECT COUNT(*) FROM sender_zone_invites"), 0);
    assert_eq!(n("SELECT COUNT(*) FROM sender_zones"), 0);
}

#[test]
fn an_archive_ingest_never_touches_a_message_already_stored() {
    let (store, acct) = store();
    let live = triaged(acct, "g-live", "t")
        .body("live body")
        .ingest(&store);
    let before = jobs(&store, live);

    let again = triaged(acct, "g-live", "t").body("archive body").build();
    assert_eq!(store.ingest_archive_message(&again).unwrap(), None);
    let (is_archive, body): (i64, String) = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT is_archive, body FROM messages WHERE id=?1",
            params![live],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(is_archive, 0, "a live row is never demoted");
    assert_eq!(body, "live body");
    assert_eq!(jobs(&store, live), before);

    // Nor an archive row it already wrote (a re-listed chunk).
    let first = archive(&store, &triaged(acct, "g-old", "t-old").body("first"));
    let second = triaged(acct, "g-old", "t-old").body("second").build();
    assert_eq!(store.ingest_archive_message(&second).unwrap(), None);
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM messages WHERE id=?1 AND body='first'",
            first
        ),
        1
    );
}

#[test]
fn a_live_sighting_of_an_archive_row_changes_nothing() {
    let (store, acct) = store();
    let id = archive(
        &store,
        &triaged(acct, "g-old", "t-old")
            .body("as archived")
            .received_at(days_ago(200)),
    );

    // The same id through the live ingest, with different content and every
    // reason to queue work.
    let mut live = triaged(acct, "g-old", "t-old")
        .body("as re-fetched")
        .received_at(days_ago(200))
        .build();
    live.foreground_triage = true;
    live.notify_eligible_at = Some(Utc::now());
    assert_eq!(store.ingest_message(&live).unwrap(), id);

    let (is_archive, body): (i64, String) = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT is_archive, body FROM messages WHERE id=?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(is_archive, 1, "is_archive never changes after insert");
    assert_eq!(body, "as archived");
    assert_eq!(state(&store, id), (1, "pending".to_string()));
    assert_eq!(jobs(&store, id), 0);

    // A heal refuses it outright.
    assert_eq!(
        store
            .ingest_message_fresh(&live, crate::store::HealScope::TextAndTriage)
            .unwrap(),
        None
    );
    assert_eq!(jobs(&store, id), 0);
}

#[test]
fn the_triage_agents_evidence_tools_neither_show_nor_assess_unasked_archive_mail() {
    let (store, acct) = store();
    let id = archive(
        &store,
        &triaged(acct, "g-old", "t-old")
            .from("pat@old.example")
            .subject("heron survey")
            .body("the heron count")
            .received_at(days_ago(200)),
    );
    // A live sibling in the same conversation, so thread reads have something
    // to return either way.
    let live = triaged(acct, "g-new", "t-old")
        .from("pat@old.example")
        .subject("re: heron survey")
        .body("a newer osprey note")
        .ingest(&store);

    let ids = |messages: Vec<crate::store::agent_triage::AgentMessage>| {
        let mut ids: Vec<i64> = messages.into_iter().map(|m| m.id).collect();
        ids.sort_unstable();
        ids
    };
    assert_eq!(
        ids(store.agent_search_mail(acct, "heron", 10).unwrap()),
        vec![live]
    );
    assert_eq!(
        ids(store
            .agent_sender_history(acct, "pat@old.example", 10)
            .unwrap()),
        vec![live]
    );
    assert_eq!(
        ids(store.agent_read_thread(acct, "t-old", 10).unwrap()),
        vec![live]
    );
    assert!(matches!(
        store.agent_read_message(acct, id),
        Err(CoreError::NotFound)
    ));
    assert!(matches!(
        store.snapshot_agent_sources(acct, &[id]),
        Err(CoreError::NotFound)
    ));
    let context = store.agent_thread_context(acct, "t-old").unwrap();
    assert_eq!(context.message.id, live);
    assert_eq!(ids(context.thread), vec![live]);
    assert_eq!(jobs(&store, id), 0, "and nothing was queued by looking");

    // A conversation that is archive from end to end is not there at all.
    let alone = archive(
        &store,
        &triaged(acct, "g-alone", "t-alone").received_at(days_ago(200)),
    );
    assert!(matches!(
        store.agent_thread_context(acct, "t-alone"),
        Err(CoreError::NotFound)
    ));
    assert_eq!(jobs(&store, alone), 0);

    // Once an explicit surfacing has had it assessed, it is ordinary evidence.
    allow(&store, id);
    assert_eq!(
        ids(store.agent_search_mail(acct, "heron", 10).unwrap()),
        vec![id, live]
    );
    assert_eq!(
        ids(store
            .agent_sender_history(acct, "pat@old.example", 10)
            .unwrap()),
        vec![id, live]
    );
    assert_eq!(
        ids(store.agent_read_thread(acct, "t-old", 10).unwrap()),
        vec![id, live]
    );
    assert_eq!(store.agent_read_message(acct, id).unwrap().id, id);
    assert_eq!(store.snapshot_agent_sources(acct, &[id]).unwrap().len(), 1);
    assert_eq!(
        ids(store.agent_thread_context(acct, "t-old").unwrap().thread),
        vec![id, live]
    );
    assert_eq!(jobs(&store, id), 0);
}

#[test]
fn nothing_unprompted_queues_work_for_an_archive_row() {
    let (store, acct) = store();
    let id = archive(
        &store,
        &triaged(acct, "g-old", "t-old")
            .from("pat@old.example")
            .body("\r\n")
            .received_at(days_ago(200)),
    );
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE messages SET body_html='<p>real</p>' WHERE id=?1",
            params![id],
        )
        .unwrap();
    // Control rows: the same shapes, live, so each assertion below is about
    // the archive flag and not about an empty store.
    let live = triaged(acct, "g-live", "t-live")
        .body("\r\n")
        .received_at(days_ago(200))
        .ingest(&store);
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE messages SET body_html='<p>real</p>' WHERE id=?1",
            params![live],
        )
        .unwrap();

    // A retriage window wide enough to cover it: one message, the live one.
    assert_eq!(store.retriage_reset(acct, None, 400).unwrap(), 1);
    assert_eq!(jobs(&store, id), 0);

    // The blank-body heal's scan.
    let scan = store.blank_body_messages(acct, i64::MAX, 100).unwrap();
    let healed: Vec<i64> = scan.candidates.iter().map(|c| c.message_id).collect();
    assert_eq!(healed, vec![live]);

    // The user's own reply landing in the old conversation.
    triaged(acct, "g-reply", "t-old")
        .from("me@example.com")
        .is_sent(true)
        .body("replying a year on")
        .ingest(&store);
    assert_eq!(jobs(&store, id), 0, "the reply re-evaluates live mail only");

    // A human opening it.
    assert_eq!(store.human_read_message(acct, id).unwrap().id, id);
    assert_eq!(jobs(&store, id), 0);
    assert_eq!(state(&store, id), (1, "pending".to_string()));
}

#[test]
fn archive_mail_is_in_search_and_the_reader_and_in_no_count_or_listing() {
    let (store, acct) = store();
    store
        .set_app_setting(acct, OPEN_LEDGER_SINCE_KEY, &days_ago(400).to_rfc3339())
        .unwrap();
    // One old, and one RECENT archive row: a window alone must not be what
    // keeps the tier out of a listing.
    let old = archive(
        &store,
        &triaged(acct, "a-old", "t-old")
            .from("pat@old.example")
            .subject("heron survey")
            .body("heron")
            .received_at(days_ago(200)),
    );
    let recent = archive(
        &store,
        &triaged(acct, "a-recent", "t-recent")
            .from("pat@old.example")
            .subject("heron again")
            .body("heron")
            .received_at(days_ago(2)),
    );
    let live = triaged(acct, "g-live", "t-live")
        .from("pat@old.example")
        .subject("heron today")
        .body("heron")
        .received_at(days_ago(1))
        .ingest(&store);
    let archived = [old, recent];

    // ---- where it must appear ------------------------------------------
    let mut found: Vec<i64> = store
        .search(acct, "heron", 10, 0)
        .unwrap()
        .iter()
        .map(|hit| hit.id)
        .collect();
    found.sort_unstable();
    assert_eq!(found, vec![old, recent, live]);
    let thread = store.thread_view_with_html(acct, "t-old").unwrap();
    assert_eq!(thread.messages.len(), 1);
    assert_eq!(thread.messages[0].id, old);

    // ---- the human inventory, every way it is asked ----------------------
    let listed = |query: &AgentInventoryQuery| -> Vec<i64> {
        store
            .human_agent_updates(acct, query)
            .unwrap()
            .iter()
            .map(|item| item.update.id)
            .collect()
    };
    assert_eq!(listed(&inventory(days_ago(30))), vec![live]);
    assert_eq!(listed(&inventory(days_ago(400))), vec![live]);
    let mut new_band = inventory(days_ago(400));
    new_band.band = Some(SitrepBand::New);
    assert_eq!(listed(&new_band), vec![live]);
    // Keyset paging, a row at a time, to the end.
    let mut paged = Vec::new();
    let mut page = inventory(days_ago(400));
    page.limit = Some(1);
    loop {
        let rows = store.human_agent_updates(acct, &page).unwrap();
        let Some(last) = rows.last() else { break };
        page.before = Some((last.received_at.unwrap(), last.update.id));
        paged.extend(rows.iter().map(|item| item.update.id));
    }
    assert_eq!(paged, vec![live]);

    // ---- counts ----------------------------------------------------------
    let stats = store.stats(acct, days_ago(400)).unwrap();
    assert_eq!(stats.total, 1);
    assert_eq!(stats.bands.new, 1);
    let activity = store
        .mail_activity(acct, days_ago(400), Utc::now())
        .unwrap();
    assert_eq!(activity.iter().map(|day| day.received).sum::<u64>(), 1);
    let rate = store.share_open_rate(acct, days_ago(400)).unwrap();
    assert_eq!(rate.received, 1);
    assert!(rate.oldest_received_at.unwrap() > days_ago(2));
    assert_eq!(store.count_inbound_since(acct, days_ago(400)).unwrap(), 1);
    assert_eq!(
        store
            .sender_rule_digest(acct, "pat@old.example", 10)
            .unwrap()
            .len(),
        1
    );

    // ---- the updates listings ---------------------------------------------
    let ranked: Vec<i64> = store
        .ranked_updates(acct, days_ago(400), None)
        .unwrap()
        .iter()
        .map(|update| update.id)
        .collect();
    assert_eq!(ranked, vec![live]);
    let attention: Vec<i64> = store
        .attention_updates(
            acct,
            days_ago(400),
            None,
            None,
            None,
            false,
            SpamScope::Exclude,
        )
        .unwrap()
        .iter()
        .map(|item| item.update.id)
        .collect();
    assert_eq!(attention, vec![live]);

    // ---- the lists ---------------------------------------------------------
    // Assessed as allowed AND given everything a committed triage writes to
    // put a row in FYE, Reading and Records (a decision with a record, a
    // Reading placement, the thread's attention card). The live row gets the
    // identical rows and is in all six lists; the archive rows are in none.
    // Without those rows this block passed with no archive code at all.
    for id in archived {
        allow(&store, id);
    }
    allow(&store, live);
    place_everywhere(&store, acct, old, "t-old");
    place_everywhere(&store, acct, recent, "t-recent");
    place_everywhere(&store, acct, live, "t-live");
    for (index, list) in six_lists(&store, acct).iter().enumerate() {
        assert_eq!(list, &vec![live], "list {index}");
    }
    assert!(
        store
            .deadlines(acct, None)
            .unwrap()
            .iter()
            .all(|d| !archived.contains(&d.message_id))
    );
    assert!(
        store
            .external_shipments_within(acct, None)
            .unwrap()
            .is_empty()
    );
    for id in archived {
        assert_eq!(jobs(&store, id), 0);
    }
}

#[test]
fn the_walk_cursor_round_trips_per_account() {
    let (store, acct) = store();
    let other = store.ensure_account("other@example.com").unwrap();
    assert_eq!(store.archive_walked_to(acct).unwrap(), None);

    let first = days_ago(30);
    store.set_archive_walked_to(acct, first).unwrap();
    assert_eq!(store.archive_walked_to(acct).unwrap(), Some(first));
    let further = days_ago(37);
    store.set_archive_walked_to(acct, further).unwrap();
    assert_eq!(store.archive_walked_to(acct).unwrap(), Some(further));
    assert_eq!(
        store.archive_walked_to(other).unwrap(),
        None,
        "one account's walk says nothing about another's"
    );
}

/// THE FORWARD EDGE is born with the cursor row, at the same instant, and the
/// backward cursor's later moves leave it where it is. A row from before the
/// column existed reads as its `walked_to`.
#[test]
fn the_forward_edge_starts_at_the_walks_start_and_moves_only_by_its_own_setter() {
    let (store, acct) = store();
    assert_eq!(store.archive_covered_to(acct).unwrap(), None);
    store.set_archive_covered_to(acct, days_ago(1)).unwrap();
    assert_eq!(
        store.archive_covered_to(acct).unwrap(),
        None,
        "no cursor row, nothing to move"
    );

    let start = days_ago(30);
    store.set_archive_walked_to(acct, start).unwrap();
    assert_eq!(store.archive_covered_to(acct).unwrap(), Some(start));
    let further = days_ago(37);
    store.set_archive_walked_to(acct, further).unwrap();
    assert_eq!(
        store.archive_covered_to(acct).unwrap(),
        Some(start),
        "walking backwards does not un-cover anything forwards"
    );
    let forward = days_ago(23);
    store.set_archive_covered_to(acct, forward).unwrap();
    assert_eq!(store.archive_covered_to(acct).unwrap(), Some(forward));
    assert_eq!(store.archive_walked_to(acct).unwrap(), Some(further));

    store
        .lock()
        .unwrap()
        .execute("UPDATE archive_walk SET covered_to=NULL", [])
        .unwrap();
    assert_eq!(
        store.archive_covered_to(acct).unwrap(),
        Some(further),
        "a cursor from before the column starts from its backward edge"
    );
}

/// THE DISK FLOOR'S NUMBER: a real figure for a database on disk, and "no
/// disk to fill" for one in memory.
#[test]
fn free_disk_bytes_reads_the_databases_own_filesystem() {
    let (memory, _) = store();
    assert_eq!(memory.free_disk_bytes().unwrap(), u64::MAX);

    let dir = std::env::temp_dir().join(format!("squelch-free-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let on_disk = SqliteStore::open(dir.join("squelch.db")).unwrap();
    let free = on_disk.free_disk_bytes().unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        free > 0 && free < u64::MAX,
        "a real number of bytes: {free}"
    );
}

#[test]
fn stored_ids_are_found_in_any_tier_across_a_list_longer_than_one_statement_can_bind() {
    let (store, acct) = store();
    let other = store.ensure_account("other@example.com").unwrap();
    triaged(acct, "g-live", "t-live").ingest(&store);
    archive(&store, &triaged(acct, "g-old", "t-old"));
    triaged(other, "g-theirs", "t-theirs").ingest(&store);

    // More ids than one SQLite statement can bind (32,766 on a current
    // build), with the stored ones in the first chunk and the last.
    let mut asked: Vec<String> = (0..40_000).map(|i| format!("missing-{i}")).collect();
    asked.insert(0, "g-live".to_string());
    asked.push("g-old".to_string());
    asked.push("g-theirs".to_string());

    let mut stored: Vec<String> = store
        .stored_gmail_msg_ids(acct, &asked)
        .unwrap()
        .into_iter()
        .collect();
    stored.sort();
    assert_eq!(stored, ["g-live", "g-old"]);
    assert!(store.stored_gmail_msg_ids(acct, &[]).unwrap().is_empty());
}

// ---- the agent door (step 3) ------------------------------------------------

fn access(store: &SqliteStore, message: i64, access: &str) {
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_message_state SET access=?2 WHERE message_id=?1",
            params![message, access],
        )
        .unwrap();
}

/// A live message the agent door may read.
fn live_allowed(store: &SqliteStore, builder: &TriagedBuilder) -> i64 {
    let id = builder.ingest(store);
    allow(store, id);
    id
}

fn agent_search(store: &SqliteStore, acct: AccountId, query: &str) -> crate::store::ExternalSearch {
    store
        .external_search_surfacing(acct, query, crate::store::SearchSort::Recent, 50)
        .unwrap()
}

fn hit_ids(found: &crate::store::ExternalSearch) -> Vec<i64> {
    found.hits.iter().map(|hit| hit.id).collect()
}

/// Every job for a message, as (kind, trigger, state).
fn job_rows(store: &SqliteStore, message: i64) -> Vec<(String, String, String)> {
    store
        .lock()
        .unwrap()
        .prepare("SELECT kind,trigger,state FROM agent_triage_jobs WHERE message_id=?1 ORDER BY id")
        .unwrap()
        .query_map(params![message], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap()
}

/// Run the one queued access check for `message` to a verdict, through the
/// same claim and commit the worker uses.
fn run_access_check(store: &SqliteStore, acct: AccountId, message: i64, restricted: bool) {
    let job = store
        .claim_agent_job(acct, "access", Utc::now(), 120)
        .unwrap()
        .expect("an access job is queued");
    assert_eq!(job.message_id, message);
    let original = store.load_agent_access_message(&job).unwrap();
    let decision = crate::triage::access::AccessDecision {
        restricted,
        reason: "test".into(),
    };
    assert_eq!(
        store
            .commit_agent_access(&job, &original, &decision, &serde_json::json!({}))
            .unwrap(),
        crate::store::agent_triage::AgentCommitOutcome::Applied
    );
}

/// THE THREAD GUARD. An old ancestor nobody has assessed neither closes a
/// live conversation nor rides out on it: not its body, not its sender, not
/// the subject it would have given the thread, not its existence.
#[test]
fn an_unassessed_archive_sibling_is_omitted_and_does_not_close_the_thread() {
    let (store, acct) = store();
    let old = archive(
        &store,
        &triaged(acct, "g-old", "t-mix")
            .from("zed@old.example")
            .subject("heron ledger")
            .body("the heron accounts")
            .received_at(days_ago(200)),
    );
    let live = live_allowed(
        &store,
        &triaged(acct, "g-new", "t-mix")
            .from("pat@new.example")
            .subject("re: osprey plans")
            .body("the osprey meeting"),
    );

    assert!(store.external_thread_allowed(acct, "t-mix").unwrap());
    let view = store.thread_view(acct, "t-mix").unwrap();
    assert_eq!(
        view.messages.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![live]
    );
    // The OLDEST message titles a thread; here that is one the agent may not see.
    assert_eq!(view.subject, "re: osprey plans");
    let wire = serde_json::to_string(&view).unwrap();
    for secret in ["heron", "zed@old.example", "ledger"] {
        assert!(!wire.contains(secret), "{secret} left by the thread read");
    }
    assert_eq!(store.thread_id_for_message(acct, old).unwrap(), None);
    // Even with a decision on file (there is none in practice), the old row's
    // own verdict is not served: the guard omits it, it does not vouch for it.
    store
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO agent_message_decisions(account_id,message_id,revision,decision_json,decided_at)
             VALUES(?1,?2,1,'{}',?3)",
            params![acct, old, Utc::now().to_rfc3339()],
        )
        .unwrap();
    assert!(matches!(
        store.external_agent_decision(acct, old),
        Err(CoreError::NotFound)
    ));

    let found = agent_search(&store, acct, "osprey");
    assert_eq!(hit_ids(&found), vec![live]);
    let wire = serde_json::to_string(&found.hits).unwrap();
    assert!(!wire.contains("heron") && !wire.contains("zed@old.example"));
    // The old row matches its own words and is reported by id, internally, only.
    let found = agent_search(&store, acct, "heron");
    assert!(found.hits.is_empty());
    assert_eq!(found.pending_archive, vec![old]);

    // The human reads all of it, and so may not cache it as agent-readable.
    assert_eq!(
        store
            .thread_view_with_html(acct, "t-mix")
            .unwrap()
            .messages
            .len(),
        2
    );
    assert!(!store.human_cache_allowed(acct, "t-mix").unwrap());

    // RESTRICTED is a verdict, not an absence: it closes the thread as ever.
    access(&store, old, "restricted");
    assert!(!store.external_thread_allowed(acct, "t-mix").unwrap());
    assert!(matches!(
        store.thread_view(acct, "t-mix"),
        Err(CoreError::NotFound)
    ));
    assert!(agent_search(&store, acct, "osprey").hits.is_empty());
    assert!(
        agent_search(&store, acct, "heron")
            .pending_archive
            .is_empty()
    );

    // Allowed, it is an ordinary member of the thread, and titles it.
    access(&store, old, "allowed");
    let view = store.thread_view(acct, "t-mix").unwrap();
    assert_eq!(view.messages.len(), 2);
    assert_eq!(view.subject, "heron ledger");
    assert!(store.human_cache_allowed(acct, "t-mix").unwrap());
}

/// The exemption is for ARCHIVE rows only. A live pending sibling is mail the
/// worker is about to judge, and it closes its thread until it has.
#[test]
fn a_pending_live_sibling_and_an_owner_sealed_archive_row_still_close_the_thread() {
    let (store, acct) = store();
    live_allowed(&store, &triaged(acct, "g-a", "t-live").body("osprey one"));
    triaged(acct, "g-b", "t-live")
        .body("osprey two")
        .ingest(&store);
    assert!(!store.external_thread_allowed(acct, "t-live").unwrap());
    assert!(matches!(
        store.thread_view(acct, "t-live"),
        Err(CoreError::NotFound)
    ));
    assert!(agent_search(&store, acct, "osprey").hits.is_empty());

    // An archive row the owner sealed by hand is not "unassessed".
    live_allowed(
        &store,
        &triaged(acct, "g-c", "t-sealed").body("kestrel one"),
    );
    let sealed = archive(
        &store,
        &triaged(acct, "g-d", "t-sealed")
            .body("kestrel two")
            .received_at(days_ago(200)),
    );
    assert!(store.external_thread_allowed(acct, "t-sealed").unwrap());
    store
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO agent_triage_corrections(account_id,message_id,field,value_json,source_revision,updated_at)
             VALUES(?1,?2,'external_access','true',1,?3)",
            params![acct, sealed, Utc::now().to_rfc3339()],
        )
        .unwrap();
    assert!(!store.external_thread_allowed(acct, "t-sealed").unwrap());
    assert!(
        agent_search(&store, acct, "kestrel")
            .pending_archive
            .is_empty()
    );
}

/// A conversation that is unassessed archive from end to end is, to an
/// external reader, a conversation that does not exist.
#[test]
fn a_thread_of_only_unassessed_archive_rows_is_not_found() {
    let (store, acct) = store();
    let old = archive(
        &store,
        &triaged(acct, "g-alone", "t-alone")
            .body("heron alone")
            .received_at(days_ago(200)),
    );
    assert!(!store.external_thread_allowed(acct, "t-alone").unwrap());
    assert!(matches!(
        store.thread_view(acct, "t-alone"),
        Err(CoreError::NotFound)
    ));
    assert!(matches!(
        store.thread_view(acct, "t-missing"),
        Err(CoreError::NotFound)
    ));
    assert!(agent_search(&store, acct, "heron").hits.is_empty());
    // Opening it by name is the explicit request that buys its check.
    assert_eq!(
        job_rows(&store, old),
        vec![("access".into(), "source_access".into(), "queued".into())]
    );

    // Once that check has run, a later surfacing request is not a reason to
    // run it again, whichever way the request arrives.
    run_access_check(&store, acct, old, false);
    store
        .enqueue_agent_triage(acct, old, "archive_access", false)
        .unwrap();
    assert_eq!(
        job_rows(&store, old),
        vec![("access".into(), "source_access".into(), "completed".into())]
    );
}

/// ASSESS ON SURFACING, the queueing half: one cheap `access` job per matched
/// row, capped, best-ranked first, and never a second one for the same row.
#[test]
fn surfacing_queues_one_background_access_job_per_archive_match_capped_and_once() {
    let (store, acct) = store();
    let other = store.ensure_account("other@example.com").unwrap();
    let mut old = Vec::new();
    for i in 0..12 {
        old.push(archive(
            &store,
            &triaged(acct, &format!("g-{i}"), &format!("t-{i}"))
                .subject("heron survey")
                .body("the heron count")
                .received_at(days_ago(100 + i)),
        ));
    }
    let live = triaged(acct, "g-live", "t-live")
        .body("heron today")
        .ingest(&store);
    let foreign = archive(
        &store,
        &triaged(other, "g-f", "t-f")
            .body("heron elsewhere")
            .received_at(days_ago(100)),
    );

    let found = agent_search(&store, acct, "heron");
    assert!(found.hits.is_empty());
    assert_eq!(
        found.pending_archive.len(),
        12,
        "the live pending row is not one"
    );
    assert!(!found.pending_archive.contains(&live));

    // Not this account's row, and not an archive row: neither is queued.
    assert!(
        store
            .request_archive_access(acct, &[foreign, live], Default::default(), Utc::now())
            .unwrap()
            .is_empty()
    );
    assert_eq!(jobs(&store, foreign), 0);
    assert!(
        job_rows(&store, live)
            .iter()
            .all(|job| job.1 != "archive_access")
    );

    // A live row with no job and no state at all, and an archive row already
    // allowed: neither is an unassessed archive row, so neither is queued.
    let bare = triaged(acct, "g-bare", "t-bare").upsert(&store);
    let settled = archive(
        &store,
        &triaged(acct, "g-settled", "t-settled").received_at(days_ago(300)),
    );
    allow(&store, settled);
    assert!(
        store
            .request_archive_access(acct, &[bare, settled], Default::default(), Utc::now())
            .unwrap()
            .is_empty()
    );
    assert_eq!(jobs(&store, bare) + jobs(&store, settled), 0);

    let queued = store
        .request_archive_access(acct, &found.pending_archive, Default::default(), Utc::now())
        .unwrap();
    assert_eq!(
        queued,
        found.pending_archive[..10].to_vec(),
        "cap, in rank order"
    );
    for id in &queued {
        assert_eq!(
            job_rows(&store, *id),
            vec![("access".into(), "archive_access".into(), "queued".into())]
        );
        assert_eq!(state(&store, *id), (1, "pending".into()));
    }
    for id in &found.pending_archive[10..] {
        assert_eq!(jobs(&store, *id), 0);
    }
    assert_eq!(
        store
            .archive_access_progress(acct, &queued, Utc::now())
            .unwrap()
            .waiting,
        10
    );

    // Asking again buys nothing for the ten already queued.
    assert!(
        store
            .request_archive_access(acct, &queued, Default::default(), Utc::now())
            .unwrap()
            .is_empty()
    );
    for id in &queued {
        assert_eq!(jobs(&store, *id), 1);
    }

    // Claimed ahead of the live row's older ingest job, on the background budget.
    let job = store
        .claim_agent_job(acct, "investigation", Utc::now(), 120)
        .unwrap()
        .unwrap();
    assert_eq!(job.trigger, "archive_access");
    assert_eq!(job.kind, "access");
    assert!(!job.foreground);

    // Parked on the daily budget: still pending, not waited on, never re-queued.
    let parked = job.message_id;
    assert!(
        store
            .defer_agent_job(
                &job,
                Utc::now() + chrono::Duration::hours(20),
                "daily_budget_exhausted"
            )
            .unwrap()
    );
    assert_eq!(
        store
            .archive_access_progress(acct, &[parked], Utc::now())
            .unwrap()
            .waiting,
        0
    );
    assert!(
        store
            .request_archive_access(acct, &[parked], Default::default(), Utc::now())
            .unwrap()
            .is_empty()
    );
    assert_eq!(jobs(&store, parked), 1);
    assert!(
        agent_search(&store, acct, "heron")
            .pending_archive
            .contains(&parked)
    );

    // Failed for good: the same.
    let job = store
        .claim_agent_job(acct, "access", Utc::now(), 120)
        .unwrap()
        .unwrap();
    let failed = job.message_id;
    assert_ne!(failed, parked);
    assert!(store.fail_agent_job(&job, "test").unwrap());
    assert!(
        store
            .request_archive_access(acct, &[failed], Default::default(), Utc::now())
            .unwrap()
            .is_empty()
    );
    assert_eq!(jobs(&store, failed), 1);
    assert_eq!(
        store
            .archive_access_progress(acct, &[failed], Utc::now())
            .unwrap()
            .waiting,
        0
    );
}

/// ASSESS ON SURFACING, the verdict half: allowed, the row is an ordinary
/// result; restricted, it never is, and it stops being counted as pending.
#[test]
fn a_surfaced_archive_row_is_returned_once_allowed_and_never_once_restricted() {
    let (store, acct) = store();
    let fine = archive(
        &store,
        &triaged(acct, "g-fine", "t-fine")
            .subject("heron survey results")
            .body("the heron count")
            .received_at(days_ago(200)),
    );
    let secret = archive(
        &store,
        &triaged(acct, "g-secret", "t-secret")
            .subject("heron account reset")
            .body("the heron code")
            .received_at(days_ago(210)),
    );
    let found = agent_search(&store, acct, "heron");
    assert!(found.hits.is_empty());
    assert_eq!(found.pending_archive, vec![fine, secret]);
    assert_eq!(
        store
            .request_archive_access(acct, &found.pending_archive, Default::default(), Utc::now())
            .unwrap(),
        vec![fine, secret]
    );

    run_access_check(&store, acct, fine, false);
    let found = agent_search(&store, acct, "heron");
    assert_eq!(hit_ids(&found), vec![fine]);
    assert_eq!(found.pending_archive, vec![secret]);
    assert_eq!(
        store
            .archive_access_progress(acct, &[fine, secret], Utc::now())
            .unwrap()
            .waiting,
        1
    );

    run_access_check(&store, acct, secret, true);
    let found = agent_search(&store, acct, "heron");
    assert_eq!(hit_ids(&found), vec![fine]);
    assert!(found.pending_archive.is_empty(), "a verdict is not pending");
    assert!(matches!(
        store.thread_view(acct, "t-secret"),
        Err(CoreError::NotFound)
    ));

    // The same search again spends nothing: one job each, ever.
    assert!(
        store
            .request_archive_access(acct, &[fine, secret], Default::default(), Utc::now())
            .unwrap()
            .is_empty()
    );
    for id in [fine, secret] {
        assert_eq!(
            job_rows(&store, id),
            vec![("access".into(), "archive_access".into(), "completed".into())]
        );
    }
}

/// THE REMINDER EXEMPTION. A reminder the user set on an archive message is
/// their instruction to see it again: the human inventory lists it like any
/// live row with a reminder, before and after it fires. The agent door does
/// not, until the row has passed an access check of its own.
#[test]
fn a_reminder_surfaces_an_archive_row_to_the_human_and_not_to_the_agent() {
    let (store, acct) = store();
    let plain = archive(
        &store,
        &triaged(acct, "g-plain", "t-plain").received_at(days_ago(200)),
    );
    let reminded = archive(
        &store,
        &triaged(acct, "g-rem", "t-rem")
            .subject("heron permit")
            .received_at(days_ago(200)),
    );
    let human = |query: &AgentInventoryQuery| -> Vec<i64> {
        store
            .human_agent_updates(acct, query)
            .unwrap()
            .iter()
            .map(|item| item.update.id)
            .collect()
    };
    let attention = || -> Vec<i64> {
        store
            .attention_updates(
                acct,
                days_ago(30),
                None,
                None,
                None,
                false,
                SpamScope::Exclude,
            )
            .unwrap()
            .iter()
            .map(|item| item.update.id)
            .collect()
    };
    assert!(human(&inventory(days_ago(30))).is_empty());
    assert!(attention().is_empty());

    assert!(
        store
            .set_reminder(acct, reminded, Utc::now() + chrono::Duration::hours(1))
            .unwrap()
    );
    let mut waiting = inventory(days_ago(30));
    waiting.pending_reminders = true;
    assert_eq!(human(&waiting), vec![reminded]);
    assert_eq!(attention(), vec![reminded]);
    assert!(!human(&inventory(days_ago(400))).contains(&plain));

    assert_eq!(
        store
            .fire_due_reminders(acct, Utc::now() + chrono::Duration::hours(2))
            .unwrap(),
        vec![reminded]
    );
    assert_eq!(human(&inventory(days_ago(30))), vec![reminded]);
    assert_eq!(attention(), vec![reminded]);
    assert_eq!(store.stats(acct, days_ago(30)).unwrap().bands.open, 1);

    let fye = || -> Vec<i64> {
        store
            .external_agent_fye(acct, 50, &Default::default(), Utc::now())
            .unwrap()
            .iter()
            .map(|item| item.message_id)
            .collect()
    };
    assert!(fye().is_empty(), "a fired reminder is not an access check");
    assert_eq!(jobs(&store, reminded), 0, "and it bought none");
    allow(&store, reminded);
    assert_eq!(fye(), vec![reminded]);
}

/// THE HORIZON an agent is told, and what it is NOT built from. With a walk it
/// is the walk's own cursor, as a date. Without one it is where stored mail
/// first becomes dense, so one stray `Date` header cannot claim years of
/// coverage and no single row's timestamp is ever handed out.
#[test]
fn the_search_horizon_is_a_date_no_single_message_can_set() {
    use crate::store::archive_surface::{OLDER_MAIL_POLICY, search_horizon};
    let (store, acct) = store();
    let now = Utc::now();
    assert_eq!(store.indexed_since(acct).unwrap(), None);
    let readable = |key: &str, at: DateTime<Utc>| {
        live_allowed(&store, &triaged(acct, key, key).received_at(at))
    };

    // One message claiming 2011, one from 1970, old spam, another account's
    // mail: none of them is how far back search reaches.
    let stray = DateTime::parse_from_rfc3339("2011-03-04T05:06:07Z")
        .unwrap()
        .with_timezone(&Utc);
    readable("g-stray", stray);
    let epoch = DateTime::parse_from_rfc3339("1970-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    for i in 0..3 {
        readable(&format!("g-junk{i}"), epoch + chrono::Duration::hours(i));
    }
    for i in 0..3 {
        let spam = triaged(acct, &format!("g-spam{i}"), "t-spam")
            .is_spam(true)
            .received_at(days_ago(300) + chrono::Duration::hours(i))
            .ingest(&store);
        allow(&store, spam);
    }
    let other = store.ensure_account("other@example.com").unwrap();
    for i in 0..3 {
        live_allowed(
            &store,
            &triaged(other, &format!("g-o{i}"), "t-o")
                .received_at(days_ago(350) + chrono::Duration::hours(i)),
        );
    }
    // Nor is mail the agent door may not read, however much of it there is:
    // pending, restricted, and unassessed archive rows, a dense week of each.
    for i in 0..3 {
        let at = days_ago(250) + chrono::Duration::hours(i);
        triaged(acct, &format!("g-pend{i}"), &format!("t-pend{i}"))
            .received_at(at)
            .ingest(&store);
        let sealed = triaged(acct, &format!("g-seal{i}"), &format!("t-seal{i}"))
            .received_at(at)
            .ingest(&store);
        access(&store, sealed, "restricted");
        archive(
            &store,
            &triaged(acct, &format!("g-arch{i}"), &format!("t-arch{i}")).received_at(at),
        );
    }
    // One readable message in the middle of that week does not make the
    // unreadable ones around it count toward its three.
    readable("g-lone", days_ago(250) + chrono::Duration::hours(1));
    assert_eq!(
        store.indexed_since(acct).unwrap(),
        None,
        "a single old row, and mail the door cannot read, are not coverage"
    );

    // Two in a week is still not three. And an unreadable message the day
    // before the readable week does not get to date the horizon.
    let dense = days_ago(40);
    triaged(acct, "g-eve", "t-eve")
        .received_at(dense - chrono::Duration::days(1))
        .ingest(&store);
    readable("g-d0", dense);
    readable("g-d1", dense + chrono::Duration::days(2));
    assert_eq!(store.indexed_since(acct).unwrap(), None);
    // A third just outside the week does not complete it; one inside does.
    readable("g-d2", dense + chrono::Duration::days(8));
    assert_eq!(store.indexed_since(acct).unwrap(), None);
    readable("g-d3", dense + chrono::Duration::days(5));
    assert_eq!(store.indexed_since(acct).unwrap(), Some(dense.date_naive()));

    let off = search_horizon(&store, acct, 0, now).unwrap();
    let date = off.indexed_since.clone().unwrap();
    assert_eq!(date, dense.format("%Y-%m-%d").to_string());
    assert_eq!(date.len(), 10, "a date, never an instant");
    assert_eq!(off.archive, None, "no archive, nothing to report");
    let wire = serde_json::to_value(&off).unwrap();
    assert!(wire.get("archive").is_none());
    assert_eq!(wire["indexed_since"], date);

    let walking = search_horizon(&store, acct, 365, now).unwrap();
    let archive_horizon = walking.archive.unwrap();
    assert_eq!(
        archive_horizon.target,
        (now - chrono::Duration::days(365))
            .format("%Y-%m-%d")
            .to_string()
    );
    assert!(!archive_horizon.walk_complete, "no cursor yet");
    assert_eq!(archive_horizon.older_mail, OLDER_MAIL_POLICY);
    assert_eq!(archive_horizon.older_mail, "checked_on_first_match");

    // WITH A WALK the cursor is the horizon, whatever the rows say: older
    // than the dense week, newer than the stray.
    let cursor = days_ago(100);
    store.set_archive_walked_to(acct, cursor).unwrap();
    let walked = search_horizon(&store, acct, 365, now).unwrap();
    assert_eq!(
        walked.indexed_since,
        Some(cursor.format("%Y-%m-%d").to_string())
    );
    assert!(!walked.archive.unwrap().walk_complete);
    // And with the archive since turned off, the cursor still answers.
    assert_eq!(
        search_horizon(&store, acct, 0, now).unwrap().indexed_since,
        Some(cursor.format("%Y-%m-%d").to_string())
    );
    store.set_archive_walked_to(acct, days_ago(366)).unwrap();
    assert!(
        search_horizon(&store, acct, 365, now)
            .unwrap()
            .archive
            .unwrap()
            .walk_complete
    );
    // An absurd depth is clamped, not overflowed.
    assert!(search_horizon(&store, acct, u32::MAX, now).is_ok());
}

// ---- fix round B -------------------------------------------------------------

/// [`store`], for a test that needs more than one.
fn fresh() -> (SqliteStore, AccountId) {
    store()
}

fn is_archive(store: &SqliteStore, message: i64) -> bool {
    count(
        store,
        "SELECT is_archive FROM messages WHERE id=?1",
        message,
    ) == 1
}

fn heron_archive(store: &SqliteStore, acct: AccountId, n: usize) -> Vec<i64> {
    (0..n)
        .map(|i| {
            archive(
                store,
                &triaged(acct, &format!("g-h{i}"), &format!("t-h{i}"))
                    .subject("heron survey")
                    .body("the heron count")
                    .received_at(days_ago(100 + i as i64)),
            )
        })
        .collect()
}

/// B2. THE VECTOR LEG NEVER BUYS A CHECK. Fifty archive rows that share no
/// word with the query are still its fifty nearest neighbours, because KNN has
/// no relevance floor; none of them is a match, and none is queued.
#[tokio::test]
async fn a_vector_only_neighbour_never_queues_an_access_check() {
    use crate::embed::StubEmbedder;
    use crate::store::archive_surface::{ArchiveSurfacing, search_with_surfacing};
    let embedder = Arc::new(StubEmbedder::new(VEC_DIMS));
    let (store, acct) = store_with_embedder(embedder.clone());
    let store = Arc::new(store);
    let mut old = Vec::new();
    for i in 0..50 {
        let (subject, body) = (format!("ledger {i}"), format!("quarterly ledger entry {i}"));
        let id = archive(
            &store,
            &triaged(acct, &format!("g-{i}"), &format!("t-{i}"))
                .subject(&subject)
                .body(&body)
                .received_at(days_ago(100 + i)),
        );
        embed_and_store(&store, embedder.as_ref(), acct, id, &subject, &body);
        old.push(id);
    }
    // The vector leg really does return them...
    let (legs, _): (Vec<crate::store::LeggedHit>, bool) = store
        .hybrid_search_legs(
            acct,
            "osprey migration",
            &Default::default(),
            crate::store::SearchSort::Recent,
            false,
            false,
            50,
        )
        .unwrap();
    assert_eq!(legs.len(), 50);
    assert!(legs.iter().all(|hit| hit.vector && !hit.keyword));
    // ...and the door sends none of them for a check.
    assert!(
        agent_search(&store, acct, "osprey migration")
            .pending_archive
            .is_empty()
    );
    let (hits, _) = search_with_surfacing(
        &store,
        acct,
        &ArchiveSurfacing {
            archive_days: 365,
            ..Default::default()
        },
        move |store: &SqliteStore| {
            store.external_search_surfacing(
                acct,
                "osprey migration",
                crate::store::SearchSort::Recent,
                10,
            )
        },
    )
    .await
    .unwrap();
    assert!(hits.is_empty());
    for id in &old {
        assert_eq!(jobs(&store, *id), 0);
    }
    // A keyword match among them is still surfaced: the leg, not the tier.
    assert_eq!(
        agent_search(&store, acct, "ledger").pending_archive.len(),
        50
    );
}

/// B3. THE THREE CEILINGS on what a search may queue.
#[test]
fn surfacing_stops_at_the_outstanding_ceiling_the_daily_cap_and_a_spent_background_budget() {
    use crate::store::{ARCHIVE_ACCESS_BUDGET_KEY, ArchiveAccessCaps, BACKGROUND_BUDGET_KEY};
    let now = Utc::now();
    let day = now.format("%Y-%m-%d").to_string();
    let caps = ArchiveAccessCaps::default();
    assert_eq!(caps.daily, 50);

    // Twenty outstanding and not one more, however many searches ask.
    let (store, acct) = fresh();
    let old = heron_archive(&store, acct, 30);
    assert_eq!(
        store
            .request_archive_access(acct, &old[..10], caps, now)
            .unwrap()
            .len(),
        10
    );
    assert_eq!(
        store
            .request_archive_access(acct, &old[10..], caps, now)
            .unwrap(),
        old[10..20].to_vec()
    );
    assert!(
        store
            .request_archive_access(acct, &old[20..], caps, now)
            .unwrap()
            .is_empty(),
        "twenty are outstanding"
    );
    // A finished check frees its place.
    run_access_check(&store, acct, old[0], false);
    assert_eq!(
        store
            .request_archive_access(acct, &old[20..], caps, now)
            .unwrap(),
        vec![old[20]]
    );

    // The daily sub-cap counts what is spent AND what is already queued.
    let (store, acct) = fresh();
    let old = heron_archive(&store, acct, 12);
    let small = ArchiveAccessCaps { daily: 5, ..caps };
    let limits = vec![(ARCHIVE_ACCESS_BUDGET_KEY.to_string(), 5)];
    for _ in 0..2 {
        assert!(store.reserve_agent_budget(acct, &day, &limits).unwrap());
    }
    assert_eq!(
        store
            .request_archive_access(acct, &old, small, now)
            .unwrap(),
        old[..3].to_vec(),
        "five a day, two spent"
    );
    assert!(
        store
            .request_archive_access(acct, &old, small, now)
            .unwrap()
            .is_empty(),
        "and three already queued"
    );
    // Tomorrow's budget is not today's, but what is still queued from today
    // will be paid for out of it.
    assert_eq!(
        store
            .request_archive_access(acct, &old, small, now + chrono::Duration::days(1))
            .unwrap(),
        old[3..5].to_vec(),
        "five tomorrow, three already owed"
    );
    let off = ArchiveAccessCaps { daily: 0, ..caps };
    let (store, acct) = fresh();
    let old = heron_archive(&store, acct, 2);
    assert!(
        store
            .request_archive_access(acct, &old, off, now)
            .unwrap()
            .is_empty(),
        "a zero cap turns surfacing off"
    );

    // A spent background budget: every check would park, so none is queued.
    let tight = ArchiveAccessCaps {
        background_daily: 2,
        ..caps
    };
    let background = vec![(BACKGROUND_BUDGET_KEY.to_string(), 2)];
    assert!(store.reserve_agent_budget(acct, &day, &background).unwrap());
    assert_eq!(
        store
            .request_archive_access(acct, &old[..1], tight, now)
            .unwrap()
            .len(),
        1
    );
    assert!(store.reserve_agent_budget(acct, &day, &background).unwrap());
    assert!(
        store
            .request_archive_access(acct, &old[1..], tight, now)
            .unwrap()
            .is_empty()
    );
    assert_eq!(jobs(&store, old[1]), 0);
}

/// B3. Tier 1 is for a check nobody has pushed back. Parked on the budget
/// and due again, it is ordinary background work behind what was already
/// waiting.
#[test]
fn a_deferred_archive_check_loses_its_place_at_the_head_of_the_queue() {
    let (store, acct) = store();
    let live = triaged(acct, "g-live", "t-live").ingest(&store);
    let old = heron_archive(&store, acct, 1)[0];
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_triage_jobs SET trigger='backfill' WHERE message_id=?1",
            params![live],
        )
        .unwrap();
    store
        .request_archive_access(acct, &[old], Default::default(), Utc::now())
        .unwrap();

    let first = store
        .claim_agent_job(acct, "investigation", Utc::now(), 120)
        .unwrap()
        .unwrap();
    assert_eq!(first.message_id, old, "fresh: ahead of the backlog");
    assert!(
        store
            .defer_agent_job(
                &first,
                Utc::now() + chrono::Duration::hours(20),
                "daily_budget_exhausted"
            )
            .unwrap()
    );
    // Tomorrow: both are due, and the backlog job has waited longer.
    let tomorrow = Utc::now() + chrono::Duration::hours(21);
    let next = store
        .claim_agent_job(acct, "investigation", tomorrow, 120)
        .unwrap()
        .unwrap();
    assert_eq!(next.message_id, live, "deferred once, tier 1 no more");
}

/// B7. A failed check is asked for again once it is a day old, by a search
/// and by a thread open, so a row is not pending for good.
#[test]
fn a_failed_archive_check_is_asked_for_again_after_a_day() {
    let (store, acct) = store();
    let old = heron_archive(&store, acct, 1)[0];
    let caps = Default::default();
    let now = Utc::now();
    store
        .request_archive_access(acct, &[old], caps, now)
        .unwrap();
    let job = store
        .claim_agent_job(acct, "access", Utc::now(), 120)
        .unwrap()
        .unwrap();
    assert!(store.fail_agent_job(&job, "attempts_exhausted").unwrap());

    for soon in [now, now + chrono::Duration::hours(23)] {
        assert!(
            store
                .request_archive_access(acct, &[old], caps, soon)
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(
        store
            .request_archive_access(acct, &[old], caps, now + chrono::Duration::hours(25))
            .unwrap(),
        vec![old]
    );
    assert_eq!(
        job_rows(&store, old),
        vec![("access".into(), "archive_access".into(), "queued".into())],
        "the same job, queued again"
    );
    assert_eq!(
        count(
            &store,
            "SELECT attempts FROM agent_triage_jobs WHERE message_id=?1",
            old
        ),
        0
    );
    // Not twice: it is in flight now.
    assert!(
        store
            .request_archive_access(acct, &[old], caps, now + chrono::Duration::hours(26))
            .unwrap()
            .is_empty()
    );

    // The thread open does the same for a sibling it queued itself.
    let sibling = archive(
        &store,
        &triaged(acct, "g-sib", "t-mix")
            .subject("osprey notes")
            .received_at(days_ago(150)),
    );
    live_allowed(&store, &triaged(acct, "g-live", "t-mix").subject("osprey"));
    assert_eq!(
        store.request_thread_access(acct, "t-mix").unwrap(),
        vec![sibling]
    );
    let job = store
        .claim_agent_job(acct, "access", Utc::now(), 120)
        .unwrap()
        .unwrap();
    assert_eq!(job.trigger, "source_access");
    assert!(store.fail_agent_job(&job, "attempts_exhausted").unwrap());
    assert!(
        store
            .request_thread_access(acct, "t-mix")
            .unwrap()
            .is_empty()
    );
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_triage_jobs SET available_at=?2 WHERE message_id=?1",
            params![sibling, days_ago(2).to_rfc3339()],
        )
        .unwrap();
    assert_eq!(
        store.request_thread_access(acct, "t-mix").unwrap(),
        vec![sibling]
    );
    assert_eq!(
        job_rows(&store, sibling),
        vec![("access".into(), "source_access".into(), "queued".into())]
    );
    // A restricted row is a verdict, not a failure: never asked again.
    access(&store, sibling, "restricted");
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_triage_jobs SET state='failed',available_at=?2 WHERE message_id=?1",
            params![sibling, days_ago(2).to_rfc3339()],
        )
        .unwrap();
    assert!(
        store
            .request_thread_access(acct, "t-mix")
            .unwrap()
            .is_empty()
    );
}

/// B8. A PARKED JOB GRANTS NOTHING. The triage agent may read an archive row
/// as evidence while a check on it is really in flight; a check waiting on
/// tomorrow's budget, or due again after a push-back, is not that.
#[test]
fn only_a_check_in_flight_makes_an_archive_row_evidence() {
    let (store, acct) = store();
    let old = heron_archive(&store, acct, 1)[0];
    let readable = || store.agent_read_message(acct, old).is_ok();
    let reschedule = |at: DateTime<Utc>, error: Option<&str>| {
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE agent_triage_jobs SET state='queued',available_at=?2,last_error=?3
                 WHERE message_id=?1",
                params![old, at.to_rfc3339(), error],
            )
            .unwrap();
    };
    assert!(!readable(), "nobody asked");

    store
        .request_archive_access(acct, &[old], Default::default(), Utc::now())
        .unwrap();
    assert!(readable(), "queued, due, never pushed back");

    reschedule(Utc::now() + chrono::Duration::hours(20), None);
    assert!(!readable(), "not due");
    reschedule(
        Utc::now() + chrono::Duration::hours(20),
        Some("daily_budget_exhausted"),
    );
    assert!(!readable(), "parked on the budget");
    reschedule(
        Utc::now() - chrono::Duration::minutes(1),
        Some("daily_budget_exhausted"),
    );
    assert!(!readable(), "due again, but it has been pushed back once");
    assert!(
        store
            .agent_search_mail(acct, "heron", 10)
            .unwrap()
            .is_empty(),
        "nor is it found by the agent's own search"
    );

    let job = store
        .claim_agent_job(acct, "access", Utc::now(), 120)
        .unwrap()
        .unwrap();
    assert!(readable(), "leased: the check is running");
    assert!(
        store
            .defer_agent_job(
                &job,
                Utc::now() + chrono::Duration::hours(20),
                "provider_cooldown"
            )
            .unwrap()
    );
    assert!(!readable());
    assert_eq!(jobs(&store, old), 1, "and none of this queued anything");
}

fn verdict(id: i64) -> crate::triage::decision::MessageDecision {
    use crate::triage::decision::*;
    MessageDecision {
        kinds: vec![EmailKind::Correspondence],
        summary: "Summary".into(),
        reason: "Reason".into(),
        destinations: vec![MessageDestination::Reading],
        records: vec![RecordProposal::Receipt {
            merchant: "Shop".into(),
            amount: Some(12.0),
            currency: Some("USD".into()),
            evidence: vec![EvidenceRef {
                message_id: id,
                location: "body".into(),
            }],
        }],
        attention: ThreadAttentionDecision {
            show_in_fye: true,
            summary: "Attention".into(),
            relevant_message_ids: vec![id],
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Everything a committed triage would have written to put a LIVE row in FYE,
/// Reading and Records, written by hand onto `id`.
fn place_everywhere(store: &SqliteStore, acct: AccountId, id: i64, thread: &str) {
    let decision = verdict(id);
    let conn = store.lock().unwrap();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO agent_message_decisions(account_id,message_id,revision,decision_json,decided_at)
         VALUES(?1,?2,1,?3,?4)",
        params![acct, id, serde_json::to_string(&decision).unwrap(), now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO agent_message_destinations(account_id,message_id,destination)
         VALUES(?1,?2,'reading')",
        params![acct, id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO agent_thread_attention(decision_message_id,account_id,thread_id,message_id,
             show_in_fye,attention_json,relevant_activity)
         VALUES(?2,?1,?3,?2,1,?4,?5)",
        params![
            acct,
            id,
            thread,
            serde_json::to_string(&decision.attention).unwrap(),
            now
        ],
    )
    .unwrap();
}

fn six_lists(store: &SqliteStore, acct: AccountId) -> [Vec<i64>; 6] {
    let ids = |items: Vec<crate::store::agent_triage::AgentListItem>| -> Vec<i64> {
        items.iter().map(|item| item.message_id).collect()
    };
    let now = Utc::now();
    [
        ids(store.agent_fye(acct, 50, &Default::default(), now).unwrap()),
        ids(store.agent_reading(acct, 50).unwrap()),
        ids(store.agent_records(acct, 50).unwrap()),
        ids(store
            .external_agent_fye(acct, 50, &Default::default(), now)
            .unwrap()),
        ids(store.external_agent_reading(acct, 50).unwrap()),
        ids(store.external_agent_records(acct, 50).unwrap()),
    ]
}

/// B9 / B14. THE LISTS' BACKSTOP. An archive row given every row a committed
/// triage writes (a decision with a record, a Reading placement, the thread's
/// attention card) is still in none of the six lists; the identical rows on a
/// live message put it in all six, which is what makes the first half a test.
#[test]
fn an_archive_row_with_a_full_decision_is_still_in_no_list() {
    let (store, acct) = store();
    let live = live_allowed(&store, &triaged(acct, "g-live", "t-live"));
    // Recent, as the walk's forward pass stores mail that skipped the inbox:
    // Reading and Records list a recent window by default, so an old row
    // would be absent for that reason and prove nothing about this one.
    let old = archive(
        &store,
        &triaged(acct, "g-old", "t-old").received_at(days_ago(2)),
    );
    allow(&store, old);
    place_everywhere(&store, acct, live, "t-live");
    place_everywhere(&store, acct, old, "t-old");

    for (index, list) in six_lists(&store, acct).iter().enumerate() {
        assert_eq!(list, &vec![live], "list {index}");
    }

    // The one exemption, as in the human inventory: a reminder the owner set
    // that has fired.
    assert!(
        store
            .set_reminder(acct, old, Utc::now() + chrono::Duration::hours(1))
            .unwrap()
    );
    assert_eq!(six_lists(&store, acct)[0], vec![live], "set, not yet fired");
    store
        .fire_due_reminders(acct, Utc::now() + chrono::Duration::hours(2))
        .unwrap();
    for (index, list) in six_lists(&store, acct).iter().enumerate() {
        let mut list = list.clone();
        list.sort();
        assert_eq!(list, vec![live, old], "list {index} after the reminder");
    }
}

/// B9. A changed source refreshes the live mail that cited it and never an
/// archive row that did.
#[test]
fn a_changed_source_never_queues_an_archive_dependent() {
    let (store, acct) = store();
    let source = live_allowed(&store, &triaged(acct, "g-src", "t-src").body("first"));
    let live = live_allowed(&store, &triaged(acct, "g-dep", "t-dep"));
    let old = archive(
        &store,
        &triaged(acct, "g-old", "t-old").received_at(days_ago(200)),
    );
    allow(&store, old);
    {
        let conn = store.lock().unwrap();
        for dependent in [live, old] {
            conn.execute(
                "INSERT INTO agent_decision_sources(account_id,message_id,source_message_id,source_revision)
                 VALUES(?1,?2,?3,1)",
                params![acct, dependent, source],
            )
            .unwrap();
        }
        conn.execute(
            "UPDATE messages SET body='second' WHERE id=?1",
            params![source],
        )
        .unwrap();
    }
    store
        .enqueue_agent_triage(acct, source, "ingest", false)
        .unwrap();
    assert!(
        job_rows(&store, live)
            .iter()
            .any(|job| job.1.starts_with("source_changed:")),
        "the live dependent is refreshed"
    );
    assert_eq!(jobs(&store, old), 0, "the archive one is not");
}

/// B10. PROMOTION, by the owner's single-message re-triage.
#[test]
fn the_owners_single_message_retriage_promotes_an_archive_row() {
    let (store, acct) = store();
    let old = archive(
        &store,
        &triaged(acct, "g-old", "t-old").received_at(days_ago(200)),
    );
    let bystander = archive(
        &store,
        &triaged(acct, "g-by", "t-by").received_at(days_ago(2)),
    );
    // A window never promotes, however far back it reaches.
    store.retriage_reset(acct, None, 400).unwrap();
    assert!(is_archive(&store, old) && is_archive(&store, bystander));
    assert_eq!(jobs(&store, old) + jobs(&store, bystander), 0);
    assert_eq!(store.count_inbound_since(acct, days_ago(400)).unwrap(), 0);

    assert_eq!(store.retriage_reset(acct, Some(old), 7).unwrap(), 1);
    assert!(!is_archive(&store, old), "promoted");
    assert!(is_archive(&store, bystander), "and only the one named");
    let rows = job_rows(&store, old);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "triage", "a live row now: full triage");
    assert!(rows[0].1.starts_with("manual:"));
    assert_eq!(store.count_inbound_since(acct, days_ago(400)).unwrap(), 1);

    // ONE WAY. Neither the walk meeting it again nor a live sighting nor a
    // second request changes it back.
    assert_eq!(
        store
            .ingest_archive_message(
                &triaged(acct, "g-old", "t-old")
                    .received_at(days_ago(200))
                    .build()
            )
            .unwrap(),
        None
    );
    assert!(!is_archive(&store, old));
    store.retriage_reset(acct, Some(old), 7).unwrap();
    assert!(!is_archive(&store, old));
}

/// B10. PROMOTION, by a correction to how the row is filed; and the one
/// correction that is not that.
#[test]
fn a_filing_correction_promotes_and_a_seal_does_not() {
    let (store, acct) = store();
    let mk = |key: &str| archive(&store, &triaged(acct, key, key).received_at(days_ago(200)));

    // Sealing an archive row: restricted at once, still archive, no job.
    let sealed = mk("sealed");
    store
        .correct_agent_triage(
            acct,
            sealed,
            "external_access",
            &serde_json::json!(true),
            Utc::now(),
        )
        .unwrap();
    assert!(is_archive(&store, sealed));
    assert_eq!(jobs(&store, sealed), 0, "a seal buys no model call");
    assert_eq!(state(&store, sealed).1, "restricted");
    // Unsealing it: allowed at once, the same.
    let opened = mk("opened");
    store
        .correct_agent_triage(
            acct,
            opened,
            "external_access",
            &serde_json::json!(false),
            Utc::now(),
        )
        .unwrap();
    assert!(is_archive(&store, opened));
    assert_eq!(jobs(&store, opened), 0);
    assert_eq!(state(&store, opened).1, "allowed");

    // Any other field is the owner filing it as live mail.
    let fye = mk("fye");
    store
        .correct_agent_triage(
            acct,
            fye,
            "show_in_fye",
            &serde_json::json!(true),
            Utc::now(),
        )
        .unwrap();
    assert!(!is_archive(&store, fye));
    let rows = job_rows(&store, fye);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "triage");
    assert!(rows[0].1.starts_with("correction:"));

    let reading = mk("reading");
    store
        .correct_agent_triage_delta(
            acct,
            reading,
            "destinations",
            &["reading".into()],
            &[],
            Utc::now(),
        )
        .unwrap();
    assert!(!is_archive(&store, reading));
    assert_eq!(job_rows(&store, reading)[0].0, "triage");

    // The legacy feedback endpoint is the same correction by another name.
    let tier = mk("tier");
    store
        .correct_triage(
            acct,
            tier,
            crate::types::TriageAxis::Tier,
            "deadline",
            None,
            Utc::now(),
        )
        .unwrap();
    assert!(!is_archive(&store, tier));
    let seal = mk("seal");
    store
        .correct_triage(
            acct,
            seal,
            crate::types::TriageAxis::Sensitivity,
            "sealed",
            None,
            Utc::now(),
        )
        .unwrap();
    assert!(is_archive(&store, seal));
    assert_eq!(jobs(&store, seal), 0);
}

/// B10. A PROMOTED ROW IS LIVE, SO THE STRICT GUARD APPLIES. While it was an
/// unassessed archive ancestor its thread stayed open with it omitted; once
/// promoted and still pending, it closes the thread like any live pending
/// row. And promotion never turns a 'restricted' into anything else.
#[test]
fn a_promoted_pending_row_closes_its_thread_and_a_restricted_one_stays_restricted() {
    let (store, acct) = store();
    let old = archive(
        &store,
        &triaged(acct, "g-old", "t-mix")
            .subject("heron ledger")
            .received_at(days_ago(200)),
    );
    let live = live_allowed(
        &store,
        &triaged(acct, "g-live", "t-mix").subject("osprey plans"),
    );
    assert_eq!(
        store.thread_view(acct, "t-mix").unwrap().messages.len(),
        1,
        "omitted, thread open"
    );
    assert_eq!(hit_ids(&agent_search(&store, acct, "osprey")), vec![live]);

    store.retriage_reset(acct, Some(old), 7).unwrap();
    assert_eq!(state(&store, old).1, "pending");
    assert!(matches!(
        store.thread_view(acct, "t-mix"),
        Err(CoreError::NotFound)
    ));
    assert!(!store.external_thread_allowed(acct, "t-mix").unwrap());
    let found = agent_search(&store, acct, "osprey");
    assert!(found.hits.is_empty());
    assert!(
        found.pending_archive.is_empty(),
        "it is not archive any more"
    );
    assert!(
        agent_search(&store, acct, "heron")
            .pending_archive
            .is_empty(),
        "nor is the promoted row itself surfaced as one"
    );

    // Restricted before, restricted after, by either promoting path.
    for by_correction in [false, true] {
        let key = format!("r{by_correction}");
        let secret = archive(
            &store,
            &triaged(acct, &key, &key).received_at(days_ago(200)),
        );
        access(&store, secret, "restricted");
        if by_correction {
            store
                .correct_agent_triage(
                    acct,
                    secret,
                    "show_in_fye",
                    &serde_json::json!(true),
                    Utc::now(),
                )
                .unwrap();
        } else {
            store.retriage_reset(acct, Some(secret), 7).unwrap();
        }
        assert!(!is_archive(&store, secret));
        assert_eq!(state(&store, secret), (1, "restricted".into()));
        assert!(matches!(
            store.thread_view(acct, &key),
            Err(CoreError::NotFound)
        ));
    }
}

/// B10. Whatever trigger reaches a row that is STILL in the archive, the job
/// is an access check. Only the two promoting paths can buy it a triage, and
/// they promote first.
#[test]
fn every_trigger_on_a_still_archive_row_is_an_access_check() {
    let (store, acct) = store();
    for trigger in [
        "manual:by-hand",
        "correction:by-hand",
        "not_spam",
        "thread_changed:1:2",
    ] {
        let key = trigger.replace(':', "-");
        let old = archive(
            &store,
            &triaged(acct, &key, &key).received_at(days_ago(200)),
        );
        store
            .enqueue_agent_triage(acct, old, trigger, false)
            .unwrap();
        assert!(is_archive(&store, old), "{trigger}");
        let rows = job_rows(&store, old);
        assert_eq!(rows.len(), 1, "{trigger}");
        assert_eq!(rows[0].0, "access", "{trigger}");
    }
}

/// B10. The own-mail redirect is the store's choice, not the owner's: asked to
/// re-triage their own reply, it hands the request to the newest inbound
/// message, and that must never be an archive row.
#[test]
fn the_own_mail_redirect_never_promotes_an_archive_row() {
    let (store, acct) = store();
    let old = archive(
        &store,
        &triaged(acct, "g-old", "t-conv")
            .from("pat@old.example")
            .received_at(days_ago(200)),
    );
    let mine = triaged(acct, "g-mine", "t-conv")
        .from("me@example.com")
        .is_sent(true)
        .ingest(&store);
    assert_eq!(store.retriage_reset(acct, Some(mine), 7).unwrap(), 1);
    assert!(is_archive(&store, old));
    assert_eq!(jobs(&store, old), 0);
}

/// B11. In the reader's search an archive row is DONE: for the grouping, for
/// the `done:` operator and on the hit itself, all three from one expression.
#[test]
fn human_search_treats_archive_mail_as_done_everywhere() {
    use crate::store::{SearchFilter, SearchSort};
    let (store, acct) = store();
    let old = archive(
        &store,
        &triaged(acct, "g-old", "t-old")
            .subject("heron archive")
            .body("heron")
            .received_at(days_ago(200)),
    );
    let finished = triaged(acct, "g-done", "t-done")
        .subject("heron finished")
        .body("heron")
        .received_at(days_ago(3))
        .ingest(&store);
    let open = triaged(acct, "g-open", "t-open")
        .subject("heron open")
        .body("heron")
        .received_at(days_ago(5))
        .ingest(&store);
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE triage SET status='done' WHERE message_id=?1",
            params![finished],
        )
        .unwrap();
    let ids = |hits: Vec<crate::store::SearchHit>| -> Vec<i64> {
        hits.iter().map(|hit| hit.id).collect()
    };
    let filter = |done: Option<bool>| SearchFilter {
        done,
        ..Default::default()
    };

    // Grouping: unfinished first, then the done group by recency. Taken at
    // its triage row's word ('new') the archive row would lead.
    let grouped = store
        .search_unfinished_first(
            acct,
            "heron",
            &filter(None),
            SearchSort::Recent,
            false,
            10,
            0,
        )
        .unwrap();
    assert_eq!(ids(grouped.clone()), vec![open, finished, old]);
    let flags: Vec<bool> = grouped.iter().map(|hit| hit.is_done).collect();
    assert_eq!(flags, vec![false, true, true]);
    // Paging across the seam agrees with the first page.
    let second = store
        .search_unfinished_first(
            acct,
            "heron",
            &filter(None),
            SearchSort::Recent,
            false,
            2,
            1,
        )
        .unwrap();
    assert_eq!(ids(second), vec![finished, old]);
    // Filter-only listing (no text): the same grouping.
    let from = SearchFilter {
        after: Some(days_ago(400)),
        ..Default::default()
    };
    let listing = store
        .search_unfinished_first(acct, "", &from, SearchSort::Recent, false, 10, 0)
        .unwrap();
    assert_eq!(ids(listing.clone()), vec![open, finished, old]);
    let flags: Vec<bool> = listing.iter().map(|hit| hit.is_done).collect();
    assert_eq!(flags, vec![false, true, true]);

    // The `done:` operator.
    let done = |flag: bool| {
        let mut found = ids(store
            .search_filtered(
                acct,
                "heron",
                &filter(Some(flag)),
                SearchSort::Recent,
                false,
                10,
                0,
            )
            .unwrap());
        found.sort();
        found
    };
    assert_eq!(done(true), {
        let mut both = vec![old, finished];
        both.sort();
        both
    });
    assert_eq!(done(false), vec![open]);

    // The hybrid path's hit, and its post-hoc filter.
    let (legs, _) = store
        .hybrid_search_legs(
            acct,
            "heron",
            &filter(None),
            SearchSort::Recent,
            false,
            false,
            10,
        )
        .unwrap();
    let archived = legs.iter().find(|hit| hit.hit.id == old).unwrap();
    assert!(archived.hit.is_done);
    let (unfinished, _) = store
        .hybrid_search(
            acct,
            "heron",
            &filter(Some(false)),
            SearchSort::Recent,
            false,
            10,
        )
        .unwrap();
    assert_eq!(ids(unfinished), vec![open]);

    // The agent door's hit says the same, once the row is readable there.
    allow(&store, old);
    let external = agent_search(&store, acct, "archive");
    assert_eq!(hit_ids(&external), vec![old]);
    assert!(external.hits[0].is_done);

    // Promoted, it is unfinished mail like any other.
    store.retriage_reset(acct, Some(old), 7).unwrap();
    assert_eq!(done(false).len(), 2);
}

/// B12. An agent's search asking about old mail is not the owner's mail
/// waiting for triage: it is in neither the "triaging N" count, the parked
/// count, nor the queue gauge.
#[test]
fn archive_access_jobs_are_not_counted_as_triage_waiting() {
    let (store, acct) = store();
    let old = heron_archive(&store, acct, 3);
    store
        .request_archive_access(acct, &old, Default::default(), Utc::now())
        .unwrap();
    assert_eq!(
        store.triage_waiting_counts(acct, Utc::now()).unwrap(),
        [0, 0]
    );
    assert_eq!(store.triage_job_counts(acct).unwrap(), [0, 0]);

    let job = store
        .claim_agent_job(acct, "access", Utc::now(), 120)
        .unwrap()
        .unwrap();
    assert_eq!(
        store.triage_waiting_counts(acct, Utc::now()).unwrap(),
        [0, 0]
    );
    assert_eq!(store.triage_job_counts(acct).unwrap(), [0, 0]);
    store
        .defer_agent_job(
            &job,
            Utc::now() + chrono::Duration::hours(20),
            "daily_budget_exhausted",
        )
        .unwrap();
    assert_eq!(
        store.triage_waiting_counts(acct, Utc::now()).unwrap(),
        [0, 0]
    );
    assert_eq!(store.triage_budget_parked(acct).unwrap().0, 0);

    // Live mail still counts, so the zeroes above are the exclusion's.
    triaged(acct, "g-live", "t-live").ingest(&store);
    assert_eq!(
        store.triage_waiting_counts(acct, Utc::now()).unwrap(),
        [1, 0]
    );
    assert_eq!(store.triage_job_counts(acct).unwrap(), [1, 0]);
}

/// B12. The walk runs newest to oldest, and the sender directory keeps "the
/// newest sighting's name". An older archive message must not put back the
/// name the sender used a year ago.
#[test]
fn archive_ingest_never_renames_a_sender() {
    let (store, acct) = store();
    let name = |addr: &str| -> Option<String> {
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT display_name FROM senders WHERE account_id=?1 AND addr=?2",
                params![acct, addr],
                |row| row.get(0),
            )
            .unwrap()
    };
    triaged(acct, "g-now", "t-now")
        .from("pat@old.example")
        .from_name(Some("Pat Current"))
        .ingest(&store);
    archive(
        &store,
        &triaged(acct, "g-then", "t-then")
            .from("pat@old.example")
            .from_name(Some("Patricia Formerly"))
            .received_at(days_ago(200)),
    );
    assert_eq!(name("pat@old.example").as_deref(), Some("Pat Current"));
    // The archive mail still counts toward the sender's mail.
    let count: i64 = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT msg_count FROM senders WHERE account_id=?1 AND addr='pat@old.example'",
            params![acct],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);

    // A sender known only from the archive takes the first name the walk
    // meets (its newest), and keeps it as the walk goes further back.
    archive(
        &store,
        &triaged(acct, "g-a1", "t-a1")
            .from("zed@old.example")
            .from_name(Some("Zed Recent"))
            .received_at(days_ago(100)),
    );
    archive(
        &store,
        &triaged(acct, "g-a2", "t-a2")
            .from("zed@old.example")
            .from_name(Some("Zed Earlier"))
            .received_at(days_ago(300)),
    );
    assert_eq!(name("zed@old.example").as_deref(), Some("Zed Recent"));
}

/// B5. The recall window stops at its ceiling: readable mail ranked below a
/// wall of hidden mail deeper than the ceiling is not dug for.
#[test]
fn the_external_recall_window_stops_at_its_ceiling() {
    assert_eq!(crate::store::EXTERNAL_RECALL_MAX, 4096);
    let (store, acct) = store();
    // Twenty unassessed archive matches, all newer than the one readable hit.
    for i in 0..20 {
        archive(
            &store,
            &triaged(acct, &format!("g-{i}"), &format!("t-{i}"))
                .subject("heron")
                .body("heron")
                .received_at(days_ago(50 + i)),
        );
    }
    let deep = archive(
        &store,
        &triaged(acct, "g-deep", "t-deep")
            .subject("heron")
            .body("heron")
            .received_at(days_ago(300)),
    );
    allow(&store, deep);
    let search = |max: usize| {
        let mut windows = Vec::new();
        let found = store
            .external_recall_within(acct, 2, max, |window| {
                windows.push(window);
                let hits = store.search_filtered(
                    acct,
                    "heron",
                    &Default::default(),
                    crate::store::SearchSort::Recent,
                    false,
                    window as u32,
                    0,
                )?;
                let full = hits.len() == window;
                Ok((
                    hits.into_iter()
                        .map(|hit| crate::store::LeggedHit {
                            hit,
                            keyword: true,
                            vector: false,
                        })
                        .collect(),
                    full,
                ))
            })
            .unwrap();
        (found, windows)
    };
    let (capped, windows) = search(8);
    assert!(capped.hits.is_empty(), "the ceiling was reached first");
    assert_eq!(capped.pending_archive.len(), 8);
    assert_eq!(windows, vec![2, 4, 8], "doubling, and no further");
    let (reached, windows) = search(64);
    assert_eq!(hit_ids(&reached), vec![deep]);
    assert_eq!(windows, vec![2, 4, 8, 16, 32]);
}
