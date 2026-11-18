//! Quick clear's store half: the backlog it prices, and its record in the
//! re-notify tables (docs/RENOTIFY.md, "Quick clear").

use super::super::*;
use super::reading_senders::{into_reading_with, mail, old_ledgers};
use super::support::*;
use crate::quick_clear::{Basis, ItemKind, Planned};
use crate::renotify::{BannerKind, Trigger};
use crate::store::agent_triage::AgentListQuery;
use crate::triage::decision::{EmailKind, EvidenceRef, ThreadAttentionDecision};
use chrono::Duration;

/// A card on the For your eyes board from `from`, committed the way the
/// worker commits a decision.
fn fye(store: &SqliteStore, acct: AccountId, key: &str, from: &str) -> i64 {
    let id = mail(store, acct, key, from, 2);
    into_reading_with(store, acct, id, |d| {
        d.destinations.clear();
        d.reading_kind = None;
        d.kinds = vec![EmailKind::Receipt];
        d.summary = format!("Receipt {key}");
        d.attention = ThreadAttentionDecision {
            show_in_fye: true,
            relevant_message_ids: vec![id],
            evidence: vec![EvidenceRef {
                message_id: id,
                location: "body".into(),
            }],
            summary: "A receipt".into(),
            ..Default::default()
        };
    });
    id
}

fn backlog(store: &SqliteStore, acct: AccountId) -> Vec<crate::quick_clear::OpenItem> {
    store
        .quick_clear_backlog(acct, Utc::now(), &AgentListQuery::default(), 200)
        .unwrap()
}

fn set(store: &SqliteStore, id: i64, set: &str) {
    store
        .lock()
        .unwrap()
        .execute(
            &format!("UPDATE triage SET {set} WHERE message_id = ?1"),
            [id],
        )
        .unwrap();
}

#[test]
fn the_backlog_is_the_fye_board_plus_the_reading_rows_smart_clear_would_take() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let a = fye(&store, acct, "a", "shop@a.example");
    let b = fye(&store, acct, "b", "shop@b.example");
    // A promotion three weeks old: smart clear takes it on stored facts.
    let stale = mail(&store, acct, "stale", "deals@c.example", 20);
    into_reading_with(&store, acct, stale, |_| {});
    // A promotion two days old: smart clear leaves it, so it is not counted.
    let young = mail(&store, acct, "young", "deals@d.example", 2);
    into_reading_with(&store, acct, young, |_| {});

    let items = backlog(&store, acct);
    let mut ids: Vec<i64> = items.iter().map(|i| i.message_id).collect();
    ids.sort();
    let mut want = vec![a, b, stale];
    want.sort();
    assert_eq!(ids, want, "the young promotion is not backlog");
    let by_id = |id| items.iter().find(|i| i.message_id == id).unwrap();
    assert!(by_id(stale).smart_clear);
    assert!(!by_id(a).smart_clear);
    assert_eq!(by_id(a).one_line, "Receipt a");
    assert_eq!(by_id(a).sender, "shop@a.example");
    assert_eq!(by_id(a).decision.kinds, vec![EmailKind::Receipt]);
    assert!(by_id(a).attention.show_in_fye);
    assert!(!by_id(a).sealed && !by_id(a).reminder);
    // And the planner prices them: two receipts and one press.
    let e = crate::quick_clear::estimate(by_id(a)).unwrap();
    assert_eq!((e.basis, e.kind), (Basis::Heuristic, ItemKind::Receipt));
    let e = crate::quick_clear::estimate(by_id(stale)).unwrap();
    assert_eq!(e.basis, Basis::SmartClear);
}

#[test]
fn a_reminder_on_a_card_is_carried_to_the_planner() {
    let (store, acct) = store();
    let a = fye(&store, acct, "a", "shop@a.example");
    set(
        &store,
        a,
        &format!("reminded_at = '{}'", Utc::now().to_rfc3339()),
    );
    let items = backlog(&store, acct);
    let item = items.iter().find(|i| i.message_id == a).unwrap();
    assert!(item.reminder);
    assert_eq!(crate::quick_clear::estimate(item), None);
}

#[test]
fn a_senders_dwell_history_counts_only_reported_looks_since_the_ledger_began() {
    let (store, acct) = store();
    let floor = Utc::now() - Duration::days(10);
    store
        .set_app_setting(acct, DWELL_LEDGER_SINCE_KEY, &floor.to_rfc3339())
        .unwrap();
    let card = fye(&store, acct, "card", "shop@a.example");
    // Three reported looks since the floor, one before it, one unreported.
    let looks = [(1, 2_000), (2, 4_000), (3, 9_000), (20, 60_000), (1, 0)];
    for (n, (days_ago, dwell)) in looks.iter().enumerate() {
        let id = mail(&store, acct, &format!("old{n}"), "shop@a.example", 30);
        set(
            &store,
            id,
            &format!(
                "opened_at = '{}', dwell_ms = {dwell}",
                (Utc::now() - Duration::days(*days_ago)).to_rfc3339()
            ),
        );
    }
    let items = backlog(&store, acct);
    let item = items.iter().find(|i| i.message_id == card).unwrap();
    let history = item.sender_dwell.expect("a history");
    assert_eq!(history.samples, 3);
    assert_eq!(history.median_ms, 4_000);
    let e = crate::quick_clear::estimate(item).unwrap();
    assert_eq!((e.secs, e.basis), (4, Basis::Dwell));
}

fn planned(id: i64, secs: u32, basis: Basis) -> Planned {
    Planned {
        message_id: id,
        thread_id: format!("t{id}"),
        est_secs: secs,
        basis,
        kind: ItemKind::Glance,
        sender: "a@example.com".into(),
        one_line: "x".into(),
    }
}

#[test]
fn a_quick_clear_is_recorded_as_its_own_kind_and_spends_no_renotify() {
    let (store, acct) = store();
    store
        .append_event(&new_event(&store, acct, 1))
        .unwrap()
        .unwrap();
    let fired = Utc::now() - Duration::hours(1);
    assert_eq!(store.last_quick_clear_at(acct).unwrap(), None);
    let batch = store
        .record_quick_clear(
            acct,
            Trigger::ClientOnline,
            fired,
            1,
            &[planned(1, 8, Basis::Heuristic)],
        )
        .unwrap();
    assert_eq!(store.last_quick_clear_at(acct).unwrap(), Some(fired));

    // Being counted in a sprint is not a re-notify of that message.
    let c = store.renotify_candidates(acct).unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].renotified, 0);
    assert_eq!(c[0].last_renotified_at, None);
    // But it is a banner: the day's budget counts it, and its tap lands.
    assert_eq!(
        store
            .renotify_fired_since(acct, fired - Duration::minutes(1))
            .unwrap(),
        1
    );
    assert!(
        store
            .mark_renotify_opened(acct, batch, None, Utc::now())
            .unwrap()
    );
    let outcomes = store
        .renotify_outcomes(acct, fired - Duration::days(1))
        .unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].kind, BannerKind::QuickClear);
    assert!(outcomes[0].opened);

    // A re-notify is still told apart.
    store
        .record_renotify(acct, Trigger::ClientOnline, Utc::now(), 1, &[(1, None)])
        .unwrap();
    let outcomes = store
        .renotify_outcomes(acct, fired - Duration::days(1))
        .unwrap();
    assert_eq!(outcomes[0].kind, BannerKind::Renotify);
    assert_eq!(store.renotify_candidates(acct).unwrap()[0].renotified, 1);
}

#[test]
fn calibration_reads_predicted_against_what_each_item_really_took() {
    let (store, acct) = store();
    for id in [1, 2, 3, 4, 5] {
        store
            .append_event(&new_event(&store, acct, id))
            .unwrap()
            .unwrap();
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO triage(message_id, account_id, created_at) VALUES(?1, ?2, ?3)",
                params![id, acct, Utc::now().to_rfc3339()],
            )
            .unwrap();
    }
    let fired = Utc::now() - Duration::hours(1);
    store
        .record_quick_clear(
            acct,
            Trigger::ClientOnline,
            fired,
            1,
            &[
                planned(1, 0, Basis::SmartClear),
                planned(2, 8, Basis::Heuristic),
                planned(3, 12, Basis::Model),
                planned(4, 5, Basis::Dwell),
                planned(5, 0, Basis::SmartClear),
            ],
        )
        .unwrap();
    let after = |m: i64| (fired + Duration::minutes(m)).to_rfc3339();
    // 1 was taken by a smart-clear press after the banner.
    set(
        &store,
        1,
        &format!(
            "status = 'done', resolved_at = '{}', resolved_by = 'smart_clear:7'",
            after(2)
        ),
    );
    // 2 was opened and read for 6.2 seconds.
    set(
        &store,
        2,
        &format!("opened_at = '{}', dwell_ms = 6200", after(1)),
    );
    // 3 was opened with nothing reported, and done 20 seconds later.
    set(
        &store,
        3,
        &format!(
            "opened_at = '{}', status = 'done', resolved_at = '{}'",
            after(1),
            (fired + Duration::minutes(1) + Duration::seconds(20)).to_rfc3339()
        ),
    );
    // 5 was a smart-clear row the owner marked done by hand: the press
    // did not take it, so it says nothing about the promise.
    set(
        &store,
        5,
        &format!("status = 'done', resolved_at = '{}'", after(3)),
    );
    // 4 was opened BEFORE the banner: says nothing about the sprint.
    set(
        &store,
        4,
        &format!("opened_at = '{}', dwell_ms = 3000", after(-5)),
    );

    let mut samples = store
        .quick_clear_calibration(acct, fired - Duration::days(1))
        .unwrap();
    samples.sort_by_key(|s| (s.basis, std::cmp::Reverse(s.handled)));
    let got: Vec<(Basis, u32, Option<u32>, bool)> = samples
        .iter()
        .map(|s| (s.basis, s.predicted_secs, s.actual_secs, s.handled))
        .collect();
    assert_eq!(
        got,
        vec![
            (Basis::SmartClear, 0, Some(0), true),
            (Basis::SmartClear, 0, None, false),
            (Basis::Dwell, 5, None, false),
            (Basis::Model, 12, Some(20), true),
            (Basis::Heuristic, 8, Some(7), true),
        ]
    );
    // Outside the window: nothing.
    assert!(
        store
            .quick_clear_calibration(acct, Utc::now())
            .unwrap()
            .is_empty()
    );
}
