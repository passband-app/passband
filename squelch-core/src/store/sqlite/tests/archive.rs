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
            .request_archive_access(acct, &[foreign, live])
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
            .request_archive_access(acct, &[bare, settled])
            .unwrap()
            .is_empty()
    );
    assert_eq!(jobs(&store, bare) + jobs(&store, settled), 0);

    let queued = store
        .request_archive_access(acct, &found.pending_archive)
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
            .archive_access_waiting(acct, &queued, Utc::now())
            .unwrap(),
        10
    );

    // Asking again buys nothing for the ten already queued.
    assert!(
        store
            .request_archive_access(acct, &queued)
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
            .archive_access_waiting(acct, &[parked], Utc::now())
            .unwrap(),
        0
    );
    assert!(
        store
            .request_archive_access(acct, &[parked])
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
            .request_archive_access(acct, &[failed])
            .unwrap()
            .is_empty()
    );
    assert_eq!(jobs(&store, failed), 1);
    assert_eq!(
        store
            .archive_access_waiting(acct, &[failed], Utc::now())
            .unwrap(),
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
            .request_archive_access(acct, &found.pending_archive)
            .unwrap(),
        vec![fine, secret]
    );

    run_access_check(&store, acct, fine, false);
    let found = agent_search(&store, acct, "heron");
    assert_eq!(hit_ids(&found), vec![fine]);
    assert_eq!(found.pending_archive, vec![secret]);
    assert_eq!(
        store
            .archive_access_waiting(acct, &[fine, secret], Utc::now())
            .unwrap(),
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
            .request_archive_access(acct, &[fine, secret])
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

/// THE HORIZON an agent is told: the oldest real date in the index, how far
/// back the walk is going, and whether it has got there.
#[test]
fn the_search_horizon_reports_the_oldest_real_date_and_the_walks_progress() {
    use crate::store::archive_surface::search_horizon;
    let (store, acct) = store();
    let now = Utc::now();
    assert_eq!(store.indexed_since(acct).unwrap(), None);

    let oldest = days_ago(200);
    archive(&store, &triaged(acct, "g-old", "t-old").received_at(oldest));
    triaged(acct, "g-new", "t-new").ingest(&store);
    // A broken Date header and a spam row are not how far back search reaches.
    let epoch = DateTime::parse_from_rfc3339("1970-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    archive(
        &store,
        &triaged(acct, "g-junk", "t-junk").received_at(epoch),
    );
    triaged(acct, "g-spam", "t-spam")
        .is_spam(true)
        .received_at(days_ago(300))
        .ingest(&store);
    let other = store.ensure_account("other@example.com").unwrap();
    archive(
        &store,
        &triaged(other, "g-o", "t-o").received_at(days_ago(350)),
    );

    let indexed = store.indexed_since(acct).unwrap().unwrap();
    assert_eq!(indexed.timestamp(), oldest.timestamp());

    let off = search_horizon(&store, acct, 0, now).unwrap();
    assert_eq!(off.indexed_since, Some(indexed));
    assert_eq!(off.archive, None, "no archive, nothing to report");
    assert!(serde_json::to_value(&off).unwrap().get("archive").is_none());

    let walking = search_horizon(&store, acct, 365, now).unwrap();
    let archive_horizon = walking.archive.unwrap();
    assert_eq!(
        archive_horizon.target,
        (now - chrono::Duration::days(365))
            .format("%Y-%m-%d")
            .to_string()
    );
    assert!(!archive_horizon.walk_complete, "no cursor yet");
    store.set_archive_walked_to(acct, days_ago(100)).unwrap();
    assert!(
        !search_horizon(&store, acct, 365, now)
            .unwrap()
            .archive
            .unwrap()
            .walk_complete
    );
    store.set_archive_walked_to(acct, days_ago(366)).unwrap();
    assert!(
        search_horizon(&store, acct, 365, now)
            .unwrap()
            .archive
            .unwrap()
            .walk_complete
    );
}
