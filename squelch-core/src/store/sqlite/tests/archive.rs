//! The archive tier: mail older than the backfill window, stored to be found
//! and for nothing else. Every test here is about what an archive row must NOT
//! cause (a model job, a count, a listing) and the few places it must appear.

use super::super::*;
use super::support::*;
use crate::store::agent_triage::{AgentInventoryQuery, AgentTriageStore};
use crate::store::{SitrepBand, SpamScope};

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

    // ---- the agent door's lists -------------------------------------------
    // Even assessed as allowed, an archive row has no decision, so it is in
    // none of the decision-backed lists.
    for id in archived {
        allow(&store, id);
    }
    let now = Utc::now();
    let fye = store
        .external_agent_fye(acct, 50, &Default::default(), now)
        .unwrap();
    let reading = store.external_agent_reading(acct, 50).unwrap();
    let records = store.external_agent_records(acct, 50).unwrap();
    for item in fye.iter().chain(&reading).chain(&records) {
        assert!(!archived.contains(&item.message_id));
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
