//! Reading attention (`dwell_ms`, `link_clicks`) and the per-sender purge read.

use super::super::reading_senders::{
    MAX_SENDERS, ReadingSender, ReadingSenders, SENDER_COUNTS_SQL, SENDER_OPENS_SQL,
};
use super::super::*;
use super::support::*;
use crate::store::agent_triage::{
    AgentCommitOutcome, AgentListQuery, AgentTriageStore, MAX_DWELL_REPORT_MS, MAX_DWELL_TOTAL_MS,
    MAX_LINK_CLICKS_REPORT, MAX_LINK_CLICKS_TOTAL,
};
use crate::triage::decision::{EmailKind, MessageDecision, MessageDestination};
use crate::types::{SealedKind, Sensitivity};
use chrono::Duration;

pub(super) const NEWS: &str = "news@letters.example";

/// Commit a model decision placing `id` in Reading, the way the worker does.
pub(super) fn into_reading(store: &SqliteStore, acct: AccountId, id: i64) {
    into_reading_with(store, acct, id, |_| {});
}

/// [`into_reading`] with the decision adjusted first: a promotion with a
/// summary by default, so a test names only what it is about.
pub(super) fn into_reading_with(
    store: &SqliteStore,
    acct: AccountId,
    id: i64,
    adjust: impl FnOnce(&mut MessageDecision),
) {
    store.enqueue_agent_triage(acct, id, "test", false).unwrap();
    let job = store
        .claim_agent_job(acct, "triage", Utc::now(), 60)
        .unwrap()
        .unwrap();
    assert_eq!(job.message_id, id);
    let context = store.load_agent_context(&job).unwrap();
    let mut decision = MessageDecision {
        kinds: vec![EmailKind::Promotional],
        destinations: vec![MessageDestination::Reading],
        summary: "An issue".into(),
        reason: "A newsletter".into(),
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

/// One triaged message from `from`, `days_ago` old. Its own thread, because
/// done resolves a whole thread.
pub(super) fn mail(
    store: &SqliteStore,
    acct: AccountId,
    key: &str,
    from: &str,
    days_ago: i64,
) -> i64 {
    triaged(acct, key, key)
        .from(from)
        .received_at(Utc::now() - Duration::days(days_ago))
        .seed(store)
}

fn attention(store: &SqliteStore, id: i64) -> (i64, i64) {
    store
        .lock()
        .unwrap()
        .query_row(
            "SELECT dwell_ms, link_clicks FROM triage WHERE message_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

/// The purge read with the Reading feed's defaults.
pub(super) fn senders(store: &SqliteStore, acct: AccountId) -> ReadingSenders {
    store
        .reading_senders(acct, Utc::now(), &AgentListQuery::default(), 200)
        .unwrap()
}

pub(super) fn sender<'a>(read: &'a ReadingSenders, addr: &str) -> &'a ReadingSender {
    read.senders
        .iter()
        .find(|s| s.sender == addr)
        .unwrap_or_else(|| panic!("{addr} missing from {:?}", read.senders))
}

/// Both ledgers running since before anything these tests seed, so a test
/// about one floor is not quietly decided by the other.
pub(super) fn old_ledgers(store: &SqliteStore, acct: AccountId) {
    let since = (Utc::now() - Duration::days(400)).to_rfc3339();
    store
        .set_app_setting(acct, OPEN_LEDGER_SINCE_KEY, &since)
        .unwrap();
    store
        .set_app_setting(acct, DWELL_LEDGER_SINCE_KEY, &since)
        .unwrap();
}

// ---- the write ---------------------------------------------------------

#[test]
fn attention_reports_add_to_the_stored_totals() {
    let (store, acct) = store();
    let id = mail(&store, acct, "a", NEWS, 1);
    assert_eq!(attention(&store, id), (0, 0), "both columns start at zero");
    store.record_message_attention(acct, id, 1_000, 1).unwrap();
    store.record_message_attention(acct, id, 2_500, 2).unwrap();
    assert_eq!(attention(&store, id), (3_500, 3));
}

#[test]
fn one_report_is_clamped() {
    let (store, acct) = store();
    let id = mail(&store, acct, "a", NEWS, 1);
    store
        .record_message_attention(acct, id, u64::MAX, u32::MAX)
        .unwrap();
    assert_eq!(
        attention(&store, id),
        (
            MAX_DWELL_REPORT_MS as i64,
            i64::from(MAX_LINK_CLICKS_REPORT)
        )
    );
}

#[test]
fn the_stored_totals_are_clamped() {
    let (store, acct) = store();
    let id = mail(&store, acct, "a", NEWS, 1);
    // Enough full reports to pass both ceilings several times over.
    for _ in 0..60 {
        store
            .record_message_attention(acct, id, MAX_DWELL_REPORT_MS, MAX_LINK_CLICKS_REPORT)
            .unwrap();
    }
    assert_eq!(
        attention(&store, id),
        (MAX_DWELL_TOTAL_MS as i64, i64::from(MAX_LINK_CLICKS_TOTAL))
    );
}

#[test]
fn sealed_mail_is_accepted_and_never_stamped() {
    let (store, acct) = store();
    let id = triaged(acct, "code", "code")
        .sealed(SealedKind::Otp)
        .seed(&store);
    store.record_message_attention(acct, id, 5_000, 2).unwrap();
    assert_eq!(attention(&store, id), (0, 0));
}

/// Unknown, another account's, and sealed: all accepted, none written, and
/// the answer does not say which an id was.
#[test]
fn a_report_about_a_message_that_is_not_ours_is_accepted_and_writes_nothing() {
    let (store, acct) = store();
    let other = store.ensure_account("other@example.com").unwrap();
    let theirs = mail(&store, other, "theirs", NEWS, 1);
    let before = store.lock().unwrap().total_changes();
    for id in [9_999, theirs] {
        store.record_message_attention(acct, id, 5_000, 2).unwrap();
        store.record_message_attention(acct, id, 0, 0).unwrap();
    }
    assert_eq!(store.lock().unwrap().total_changes(), before);
    assert_eq!(attention(&store, theirs), (0, 0));
}

/// A dwell report lands while mail is being read, which is when a triage pass
/// may be in flight. It must not make that pass's commit `Stale`.
#[test]
fn reporting_attention_leaves_the_agent_user_state_fingerprint_alone() {
    let (store, acct) = store();
    let id = mail(&store, acct, "a", NEWS, 1);
    store.enqueue_agent_triage(acct, id, "test", false).unwrap();
    let job = store
        .claim_agent_job(acct, "triage", Utc::now(), 60)
        .unwrap()
        .unwrap();
    // A RE-triage: the mail was assessed as readable before.
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_message_state SET access = 'allowed' WHERE message_id = ?1",
            [id],
        )
        .unwrap();
    let before = store.load_agent_context(&job).unwrap();

    store.record_message_attention(acct, id, 42_000, 3).unwrap();
    assert_eq!(attention(&store, id), (42_000, 3), "the report did land");

    let after = store.load_agent_context(&job).unwrap();
    assert_eq!(before.revision.user_state, after.revision.user_state);
    assert_eq!(
        before.message.source.user_state,
        after.message.source.user_state
    );
    // The proof that matters: the pass that was in flight still commits.
    let decision = MessageDecision {
        kinds: vec![EmailKind::Promotional],
        destinations: vec![MessageDestination::Reading],
        summary: "An issue".into(),
        reason: "A newsletter".into(),
        ..Default::default()
    };
    assert_eq!(
        store
            .commit_agent_decision(
                &job,
                &before,
                &decision,
                std::slice::from_ref(&before.message.source)
            )
            .unwrap(),
        AgentCommitOutcome::Applied
    );
}

// ---- the read ----------------------------------------------------------

#[test]
fn counts_cover_one_senders_unsealed_inbound_mail_only() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    // Five ordinary issues: one open in Reading, two opened, two cleared
    // without opening.
    let ids: Vec<i64> = (0..5)
        .map(|i| mail(&store, acct, &format!("n{i}"), NEWS, i + 1))
        .collect();
    into_reading(&store, acct, ids[0]);
    for id in &ids[1..3] {
        store
            .acknowledge_agent_message(acct, *id, Utc::now())
            .unwrap();
    }
    for id in &ids[3..5] {
        store
            .set_attention_status(acct, *id, AttentionStatus::Done)
            .unwrap();
    }
    // And everything that must not be counted, all from the same address.
    triaged(acct, "sealed", "sealed")
        .from(NEWS)
        .sealed(SealedKind::Otp)
        .seed(&store);
    triaged(acct, "spam", "spam")
        .from(NEWS)
        .is_spam(true)
        .seed(&store);
    triaged(acct, "own", "own")
        .from(NEWS)
        .is_sent(true)
        .seed(&store);
    // Another sender's mail is another sender's.
    mail(&store, acct, "else", "other@letters.example", 1);
    // Case is not identity.
    mail(&store, acct, "shout", "NEWS@Letters.Example", 2);

    let before = store.lock().unwrap().total_changes();
    let read = senders(&store, acct);
    assert_eq!(
        store.lock().unwrap().total_changes(),
        before,
        "the read writes nothing"
    );
    assert_eq!(read.total_senders, 1);
    let s = sender(&read, NEWS);
    assert_eq!(s.open_message_ids, vec![ids[0]]);
    assert_eq!(s.signals.received, 6, "five issues and the shouted one");
    assert_eq!(s.signals.opened, 2);
    assert_eq!(s.signals.cleared_unopened, 2);
    // And nothing was stamped by looking.
    let stamped: i64 = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM triage WHERE message_id = ?1
               AND (opened_at IS NOT NULL OR surfaced_at IS NOT NULL)",
            [ids[0]],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stamped, 0);
}

#[test]
fn mail_from_before_the_open_ledger_is_not_received() {
    let (store, acct) = store();
    store
        .set_app_setting(
            acct,
            OPEN_LEDGER_SINCE_KEY,
            &(Utc::now() - Duration::days(10)).to_rfc3339(),
        )
        .unwrap();
    for i in 0..3 {
        mail(&store, acct, &format!("old{i}"), NEWS, 20 + i);
    }
    let newest = mail(&store, acct, "new0", NEWS, 1);
    for i in 1..5 {
        mail(&store, acct, &format!("new{i}"), NEWS, 1 + i);
    }
    into_reading(&store, acct, newest);

    let read = senders(&store, acct);
    assert_eq!(read.window_days, 90);
    assert_eq!(read.ledger_days, 10, "the ledger, not the ninety asked for");
    let s = sender(&read, NEWS);
    assert_eq!(s.signals.received, 5, "the three older mails predate it");
    assert_eq!(
        s.signals.per_month,
        Some(15.0),
        "five in ten days, scaled by the days actually covered"
    );
}

#[test]
fn an_open_from_before_the_dwell_ledger_is_not_in_the_dwell_sample() {
    let (store, acct) = store();
    let now = Utc::now();
    store
        .set_app_setting(
            acct,
            OPEN_LEDGER_SINCE_KEY,
            &(now - Duration::days(400)).to_rfc3339(),
        )
        .unwrap();
    store
        .set_app_setting(
            acct,
            DWELL_LEDGER_SINCE_KEY,
            &(now - Duration::days(2)).to_rfc3339(),
        )
        .unwrap();
    let early = mail(&store, acct, "early", NEWS, 8);
    let late = mail(&store, acct, "late", NEWS, 7);
    let open = mail(&store, acct, "open", NEWS, 1);
    into_reading(&store, acct, open);
    // Opened before the client could report dwell: zero for no reason.
    // (Whatever a later re-read reported: the OPEN is what the floor dates.)
    store
        .acknowledge_agent_message(acct, early, now - Duration::days(5))
        .unwrap();
    store.record_message_attention(acct, early, 500, 4).unwrap();
    // Opened after: its dwell and click are evidence.
    store
        .acknowledge_agent_message(acct, late, now - Duration::days(1))
        .unwrap();
    store
        .record_message_attention(acct, late, 9_000, 1)
        .unwrap();

    let read = senders(&store, acct);
    let s = sender(&read, NEWS).signals;
    assert_eq!(s.opened, 2, "both count as opened");
    assert_eq!(s.dwell_sample, 1, "only one has dwell worth reading");
    assert_eq!(s.median_dwell_ms, Some(9_000));
    assert_eq!(s.clicks, 1);
}

/// `/opened` and `/attention` are separate calls, and a client that predates
/// the second keeps making the first. Those opens are not zero-second looks.
#[test]
fn an_open_nobody_reported_on_is_not_a_zero_second_look() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    for i in 0..6 {
        let id = mail(&store, acct, &format!("n{i}"), NEWS, i + 1);
        if i == 0 {
            into_reading(&store, acct, id);
        } else {
            store
                .acknowledge_agent_message(acct, id, Utc::now())
                .unwrap();
        }
    }
    let read = senders(&store, acct);
    let s = sender(&read, NEWS);
    assert_eq!(s.signals.opened, 5);
    assert_eq!(s.signals.dwell_sample, 0);
    assert_eq!(s.signals.median_dwell_ms, None);
    assert_eq!(
        s.reasons,
        vec![],
        "neither brief looks nor never clicked: {:?}",
        s.score
    );
}

/// The length a dwell is judged against is the flattened text triage reads,
/// never the HTML, and it is measured for the dwell sample alone.
#[test]
fn words_come_from_the_flattened_body_of_dwell_sample_rows_only() {
    let (store, acct) = store();
    let now = Utc::now();
    store
        .set_app_setting(
            acct,
            OPEN_LEDGER_SINCE_KEY,
            &(now - Duration::days(400)).to_rfc3339(),
        )
        .unwrap();
    store
        .set_app_setting(
            acct,
            DWELL_LEDGER_SINCE_KEY,
            &(now - Duration::days(2)).to_rfc3339(),
        )
        .unwrap();
    let words = |n: usize| vec!["word"; n].join(" ");
    let issue = |key: &str, days_ago: i64, body: &str| {
        let mut m = triaged(acct, key, key)
            .from(NEWS)
            .received_at(now - Duration::days(days_ago))
            .body(body)
            .msg();
        // Markup many times the length of the text it wraps.
        m.body_html = Some(format!("<p class=\"{}\">{body}</p>", words(5_000)));
        let id = store.upsert_message(&m).unwrap();
        store
            .set_triage(
                id,
                acct,
                0,
                crate::types::Tier::Noise,
                Sensitivity::Normal,
                None,
                "",
                "",
                None,
            )
            .unwrap();
        id
    };
    // Three issues of 476 words (two minutes of reading), each opened inside
    // the dwell ledger for thirty seconds: a quarter read.
    for i in 0..3 {
        let id = issue(&format!("read{i}"), 3 + i, &words(476));
        store
            .acknowledge_agent_message(acct, id, now - Duration::hours(1))
            .unwrap();
        store.record_message_attention(acct, id, 30_000, 0).unwrap();
    }
    // A much longer one opened BEFORE the dwell ledger, and one never opened:
    // neither is in the sample, so neither length is measured.
    let early = issue("early", 9, &words(9_000));
    store
        .acknowledge_agent_message(acct, early, now - Duration::days(5))
        .unwrap();
    store
        .record_message_attention(acct, early, 1_000, 0)
        .unwrap();
    let unopened = issue("unopened", 1, &words(9_000));
    into_reading(&store, acct, unopened);

    let read = senders(&store, acct);
    let s = sender(&read, NEWS).signals;
    assert_eq!(s.dwell_sample, 3);
    assert_eq!(s.read_sample, 3);
    assert_eq!(s.median_words, Some(476));
    assert_eq!(s.median_read_fraction, Some(0.25));
    assert_eq!(s.median_dwell_ms, Some(30_000));
}

#[test]
fn a_person_the_owner_writes_to_and_a_surface_rule_both_score_zero() {
    use crate::triage::sender_fatigue::Reason;
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let friend = "friend@people.example";
    let wanted = "digest@wanted.example";
    for (from, key) in [(friend, "f"), (wanted, "w"), (NEWS, "n")] {
        for i in 0..8 {
            let id = mail(&store, acct, &format!("{key}{i}"), from, i + 1);
            if i == 0 {
                into_reading(&store, acct, id);
            }
        }
    }
    store
        .merge_harvested_contacts(
            acct,
            &[crate::store::ContactEntry {
                addr: friend.into(),
                display_name: None,
                sent_count: 2,
                last_sent_at: Some(Utc::now()),
            }],
        )
        .unwrap();
    assert!(store.is_known_contact(acct, friend).unwrap());
    let rule = store
        .set_sender_rule(acct, "*@wanted.example", "", Disposition::Surface)
        .unwrap();

    let read = senders(&store, acct);
    let f = sender(&read, friend);
    assert!(f.signals.known_contact);
    assert_eq!(
        (f.score, &f.reasons),
        (Some(0), &vec![Reason::KnownContact])
    );
    let w = sender(&read, wanted);
    assert_eq!(w.rule.as_ref().map(|r| r.id), Some(rule));
    assert_eq!(
        (w.score, &w.reasons),
        (Some(0), &vec![Reason::WantedByRule])
    );
    let n = sender(&read, NEWS);
    assert!(!n.signals.known_contact && n.rule.is_none());
    assert_eq!(n.score, Some(55), "eight received, none opened");
    // Most purge-worthy first; the two zeros tie and fall back to the address.
    let order: Vec<&str> = read.senders.iter().map(|s| s.sender.as_str()).collect();
    assert_eq!(order, [NEWS, wanted, friend]);
}

#[test]
fn senders_sort_by_score_then_volume_then_address_with_unscored_last() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    // (sender, received, opened): c is ignored, a and b tie on score and
    // differ on volume, d has too little history to score at all.
    for (from, received, opened) in [
        ("d@x.example", 2, 0),
        ("a@x.example", 4, 4),
        ("b@x.example", 6, 6),
        ("c@x.example", 8, 0),
        ("e@x.example", 4, 4),
    ] {
        for i in 0..received {
            let id = mail(&store, acct, &format!("{from}{i}"), from, i + 1);
            if i == 0 {
                into_reading(&store, acct, id);
            }
            if i < opened {
                store
                    .acknowledge_agent_message(acct, id, Utc::now())
                    .unwrap();
            }
        }
    }
    let read = senders(&store, acct);
    let order: Vec<(&str, Option<u8>)> = read
        .senders
        .iter()
        .map(|s| (s.sender.as_str(), s.score))
        .collect();
    assert_eq!(
        order,
        [
            ("c@x.example", Some(55)),
            ("b@x.example", Some(0)),
            ("a@x.example", Some(0)),
            ("e@x.example", Some(0)),
            ("d@x.example", None),
        ]
    );
}

#[test]
fn only_the_senders_with_the_most_open_mail_are_measured() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    for i in 0..=MAX_SENDERS {
        let from = format!("s{i:03}@many.example");
        let id = mail(&store, acct, &format!("m{i}"), &from, 1);
        into_reading(&store, acct, id);
    }
    // One sender with two open mails must survive the cap ahead of the rest.
    for i in 0..2 {
        let id = mail(&store, acct, &format!("two{i}"), NEWS, 2);
        into_reading(&store, acct, id);
    }
    let read = store
        .reading_senders(acct, Utc::now(), &AgentListQuery::default(), 1000)
        .unwrap();
    assert_eq!(read.total_senders, MAX_SENDERS + 2);
    assert_eq!(read.senders.len(), MAX_SENDERS);
    assert_eq!(sender(&read, NEWS).open_message_ids.len(), 2);
}

/// Both per-sender statements SEEK the NOCASE sender index, pinned by
/// plan like `rule_digest_seeks_the_nocase_sender_index`, and each names the
/// index so that losing it is a loud prepare error and not a silent walk of
/// the account under the store mutex.
#[test]
fn the_per_sender_statements_seek_the_nocase_sender_index() {
    let (store, _acct) = store();
    let conn = store.lock().unwrap();
    let statements = [SENDER_COUNTS_SQL, SENDER_OPENS_SQL];
    for sql in statements {
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let nulls = vec![rusqlite::types::Value::Null; stmt.parameter_count()];
        let plan: Vec<String> = stmt
            .query_map(rusqlite::params_from_iter(nulls), |r| r.get::<_, String>(3))
            .unwrap()
            .map(|line| line.unwrap())
            .collect();
        assert!(
            plan.iter().any(
                |l| l.starts_with("SEARCH m USING INDEX idx_messages_from_nocase")
                    && l.contains("from_addr=?")
            ),
            "walked instead of seeking\n{sql}\n{plan:#?}"
        );
        assert!(
            plan.iter()
                .any(|l| l.starts_with("SEARCH t USING INTEGER PRIMARY KEY")),
            "triage is reached by key, not scanned\n{sql}\n{plan:#?}"
        );
    }
    conn.execute_batch("DROP INDEX idx_messages_from_nocase;")
        .unwrap();
    for sql in statements {
        assert!(
            conn.prepare(sql).is_err(),
            "INDEXED BY no longer names the index it depends on\n{sql}"
        );
    }
}
