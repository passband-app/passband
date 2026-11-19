//! Smart clear's store half, the keep record, and `triage.resolved_by`.

use super::super::smart_clear::SmartClearOutcome;
use super::super::*;
use super::reading_senders::{
    NEWS, into_reading, into_reading_with, mail, old_ledgers, sender, senders,
};
use super::support::*;
use crate::store::agent_triage::AgentListQuery;
use crate::triage::decision::{
    EmailKind, MessageDecision, ReadingKind, RecordProposal, SupportedTime,
};
use crate::triage::sender_fatigue::Reason;
use crate::triage::smart_clear::{ClearReason, SMART_CLEAR_MODEL_ROWS, Verdict};
use chrono::Duration;
use std::collections::HashMap;

pub(super) fn time(at: DateTime<Utc>) -> Option<SupportedTime> {
    Some(SupportedTime {
        value: at.format("%Y-%m-%d").to_string(),
        ..Default::default()
    })
}

pub(super) fn content(d: &mut MessageDecision) {
    d.kinds = vec![EmailKind::Editorial];
    d.reading_kind = Some(ReadingKind::Content);
}

/// Press the button with whatever the model said: scan, then apply, with
/// each verdict being about the summary the scan would have sent.
pub(super) fn apply(
    store: &SqliteStore,
    acct: AccountId,
    verdicts: &[(i64, ClearReason)],
) -> SmartClearOutcome {
    press(store, acct, verdicts, 200)
}

pub(super) fn press(
    store: &SqliteStore,
    acct: AccountId,
    verdicts: &[(i64, ClearReason)],
    feed_limit: usize,
) -> SmartClearOutcome {
    let inventory = AgentListQuery::default();
    let scan = store
        .smart_clear_scan(acct, Utc::now(), &inventory, feed_limit)
        .unwrap();
    let verdicts: HashMap<i64, Verdict> = verdicts
        .iter()
        .map(|(id, reason)| {
            let sent = scan
                .asks
                .iter()
                .find(|ask| ask.message_id == *id)
                .map_or("never asked", |ask| ask.summary.as_str());
            (*id, Verdict::about(*reason, sent))
        })
        .collect();
    store
        .smart_clear_apply(acct, Utc::now(), &inventory, feed_limit, &scan, &verdicts)
        .unwrap()
}

/// (status, resolved_at is set, resolved_by) for one row.
pub(super) fn row(store: &SqliteStore, id: i64) -> (String, bool, Option<String>) {
    store
        .lock()
        .unwrap()
        .query_row(
            "SELECT status, resolved_at IS NOT NULL, resolved_by FROM triage WHERE message_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
}

/// A Reading promotion from its own sender, `days_ago` old.
pub(super) fn promo(store: &SqliteStore, acct: AccountId, key: &str, days_ago: i64) -> i64 {
    let id = mail(store, acct, key, &format!("{key}@shop.example"), days_ago);
    into_reading(store, acct, id);
    id
}

// ---- what is cleared ----------------------------------------------------

#[test]
fn the_receipt_rows_are_the_rows_written_and_their_text_is_not_stored() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let now = Utc::now();
    let offer = mail(&store, acct, "offer", "b@offers.example", 8);
    into_reading_with(&store, acct, offer, |d| {
        d.ends = time(now - Duration::days(5))
    });
    let stale = promo(&store, acct, "stale", 20);
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE messages SET subject = 'RECEIPT-SUBJECT', from_name = 'RECEIPT-NAME'
             WHERE id = ?1",
            [stale],
        )
        .unwrap();
    // Left alone, so not on the receipt either.
    let young = promo(&store, acct, "young", 5);

    let outcome = apply(&store, acct, &[]);
    // The same messages, for the same reasons, in the same order.
    assert_eq!(
        outcome
            .rows
            .iter()
            .map(|r| (r.message_id, r.reason))
            .collect::<Vec<_>>(),
        outcome.cleared
    );
    assert_eq!(outcome.cleared.len(), 2);
    assert!(outcome.rows.iter().all(|r| r.message_id != young));
    let listed = outcome
        .rows
        .iter()
        .find(|r| r.message_id == stale)
        .expect("the stale promotion");
    assert_eq!(
        (
            listed.sender.as_str(),
            listed.sender_name.as_str(),
            listed.subject.as_str()
        ),
        ("stale@shop.example", "RECEIPT-NAME", "RECEIPT-SUBJECT")
    );
    assert!(DateTime::parse_from_rfc3339(&listed.received_at).is_ok());

    // MAIL TEXT STAYS OUT OF THE BATCH RECORD: ids, reasons, prior status.
    let detail: String = store
        .lock()
        .unwrap()
        .query_row("SELECT detail FROM smart_clear_batches", [], |r| r.get(0))
        .unwrap();
    for text in ["RECEIPT-SUBJECT", "RECEIPT-NAME", "shop.example"] {
        assert!(!detail.contains(text), "{text} in {detail}");
    }

    // An empty press has an empty receipt.
    assert!(apply(&store, acct, &[]).rows.is_empty());
}

#[test]
fn each_stored_reason_clears_its_own_row_with_provenance() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let now = Utc::now();
    let event = mail(&store, acct, "event", "a@events.example", 8);
    into_reading_with(&store, acct, event, |d| {
        d.records = vec![RecordProposal::Event {
            title: "Webinar".into(),
            start: time(now - Duration::days(5)),
            location: None,
            end: None,
            duration_estimate: None,
            event_kind: Default::default(),
            commitment: Default::default(),
            change: Default::default(),
            identity_refs: vec![],
            likelihood: None,
            evidence: vec![],
            identity: None,
        }];
        // An offer that has ended too: the event is first in the order.
        d.ends = time(now - Duration::days(5));
    });
    let offer = mail(&store, acct, "offer", "b@offers.example", 8);
    into_reading_with(&store, acct, offer, |d| {
        d.ends = time(now - Duration::days(5))
    });
    let stale = promo(&store, acct, "stale", 20);
    // Left alone: an offer still running, an old essay, a young promotion.
    let running = mail(&store, acct, "running", "c@offers.example", 20);
    into_reading_with(&store, acct, running, |d| {
        d.ends = time(now + Duration::days(5))
    });
    let essay = mail(&store, acct, "essay", "d@essays.example", 25);
    into_reading_with(&store, acct, essay, content);
    let young = promo(&store, acct, "young", 5);

    let outcome = apply(&store, acct, &[]);
    let mut cleared = outcome.cleared.clone();
    cleared.sort();
    assert_eq!(
        cleared,
        [
            (event, ClearReason::EventPassed),
            (offer, ClearReason::OfferEnded),
            (stale, ClearReason::StalePromo),
        ]
    );
    assert_eq!(outcome.senders, 3);
    let batch = outcome.batch_id.expect("a batch was recorded");
    for id in [event, offer, stale] {
        assert_eq!(
            row(&store, id),
            ("done".into(), true, Some(format!("smart_clear:{batch}")))
        );
    }
    for id in [running, essay, young] {
        assert_eq!(row(&store, id).0, "new", "{id} was not its to take");
        assert_eq!(row(&store, id).2, None);
    }
    let (count, detail): (i64, String) = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT cleared, detail FROM smart_clear_batches WHERE id = ?1 AND account_id = ?2
               AND undone_at IS NULL",
            params![batch, acct],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, 3);
    let detail: serde_json::Value = serde_json::from_str(&detail).unwrap();
    assert!(
        detail["rows"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!({"id": stale, "reason": "stale_promo", "was": "new"}))
    );

    // Nothing left to take: a second press records no batch.
    let again = apply(&store, acct, &[]);
    assert_eq!(again.batch_id, None);
    assert!(again.cleared.is_empty());
    let batches: i64 = store
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM smart_clear_batches", [], |r| r.get(0))
        .unwrap();
    assert_eq!(batches, 1);
}

/// A human done resolves the whole thread. Smart clear takes the one message
/// it judged, so the sibling it never looked at stays where it was.
#[test]
fn only_the_message_is_resolved_never_its_thread() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let stale = triaged(acct, "first", "one-thread")
        .from("a@shop.example")
        .received_at(Utc::now() - Duration::days(20))
        .seed(&store);
    into_reading(&store, acct, stale);
    let sibling = triaged(acct, "second", "one-thread")
        .from("a@shop.example")
        .received_at(Utc::now() - Duration::hours(2))
        .seed(&store);
    into_reading(&store, acct, sibling);

    let outcome = apply(&store, acct, &[]);
    assert_eq!(outcome.cleared, [(stale, ClearReason::StalePromo)]);
    assert_eq!(row(&store, sibling), ("new".into(), false, None));
}

/// The sender's score is the purge list's own score. Eleven ignored mails in
/// fifteen days, eight of them cleared unopened by hand, is 55 + 11 + 10.
#[test]
fn an_ignored_senders_open_mail_goes_and_the_sweep_does_not_feed_the_score() {
    let (store, acct) = store();
    // Fifteen days of ledger: enough for a score (fourteen is the floor).
    let ledger = (Utc::now() - Duration::days(15)).to_rfc3339();
    store
        .set_app_setting(acct, OPEN_LEDGER_SINCE_KEY, &ledger)
        .unwrap();
    store
        .set_app_setting(acct, DWELL_LEDGER_SINCE_KEY, &ledger)
        .unwrap();
    let mut open = Vec::new();
    for i in 0..10 {
        let id = mail(&store, acct, &format!("n{i}"), NEWS, 2 + i % 7);
        if i < 2 {
            // Content, so nothing but the sender's record can take it.
            into_reading_with(&store, acct, id, content);
            open.push(id);
        } else {
            store
                .set_attention_status(acct, id, AttentionStatus::Done)
                .unwrap();
        }
    }
    // One that arrived today keeps the sender on the list afterwards.
    let fresh = mail(&store, acct, "fresh", NEWS, 0);
    into_reading_with(&store, acct, fresh, content);
    let before = senders(&store, acct);
    let scored = sender(&before, NEWS);
    assert_eq!(scored.signals.cleared_unopened, 8);
    assert_eq!(scored.score, Some(76), "{:?}", scored.signals);

    let outcome = apply(&store, acct, &[]);
    let mut cleared = outcome.cleared.clone();
    cleared.sort();
    open.sort();
    assert_eq!(
        cleared,
        open.iter()
            .map(|id| (*id, ClearReason::IgnoredSender))
            .collect::<Vec<_>>()
    );
    assert_eq!(outcome.senders, 1);
    assert_eq!(row(&store, fresh).0, "new", "today's mail is never taken");

    // The two rows the sweep took are done and unopened, and are NOT counted
    // as the owner clearing them.
    let after = senders(&store, acct);
    assert_eq!(sender(&after, NEWS).signals.cleared_unopened, 8);
}

// ---- what is never cleared ----------------------------------------------

#[test]
fn nothing_the_owner_said_to_leave_alone_is_taken() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    // Every one of these is a three-week-old unopened promotion.
    let control = promo(&store, acct, "control", 20);
    let friend = promo(&store, acct, "friend", 20);
    store
        .merge_harvested_contacts(
            acct,
            &[crate::store::ContactEntry {
                addr: "friend@shop.example".into(),
                display_name: None,
                sent_count: 1,
                last_sent_at: Some(Utc::now()),
            }],
        )
        .unwrap();
    let wanted = promo(&store, acct, "wanted", 20);
    store
        .set_sender_rule(acct, "wanted@shop.example", "", Disposition::Surface)
        .unwrap();
    let kept = promo(&store, acct, "kept", 20);
    assert!(
        store
            .keep_reading_sender(acct, "Kept@Shop.Example ", Utc::now())
            .unwrap()
    );
    // Reminded: the owner asked to see it again, and the reminder has fired.
    let reminded = promo(&store, acct, "reminded", 20);
    store
        .set_reminder(acct, reminded, Utc::now() - Duration::minutes(5))
        .unwrap();
    assert_eq!(
        store.fire_due_reminders(acct, Utc::now()).unwrap(),
        [reminded]
    );
    assert_eq!(row(&store, reminded).0, "open");

    let outcome = apply(&store, acct, &[]);
    assert_eq!(outcome.cleared, [(control, ClearReason::StalePromo)]);
    for id in [friend, wanted, kept, reminded] {
        assert_ne!(row(&store, id).0, "done", "{id}");
    }
}

/// Only the hundred senders with the most open mail are scored. Who the owner
/// said to leave alone is asked of every sender, scored or not.
#[test]
fn a_sender_past_the_scoring_cap_is_still_protected() {
    use super::super::reading_senders::MAX_SENDERS;
    let (store, acct) = store();
    old_ledgers(&store, acct);
    // A hundred senders with two open mails each fill the cap.
    for i in 0..MAX_SENDERS {
        for n in 0..2 {
            let id = mail(
                &store,
                acct,
                &format!("m{i}-{n}"),
                &format!("s{i:03}@many.example"),
                0,
            );
            into_reading(&store, acct, id);
        }
    }
    // Two more with one stale promotion each: one kept, one not.
    let kept = promo(&store, acct, "kept", 20);
    store
        .keep_reading_sender(acct, "kept@shop.example", Utc::now())
        .unwrap();
    let plain = promo(&store, acct, "plain", 20);

    let outcome = press(&store, acct, &[], 1000);
    assert_eq!(outcome.cleared, [(plain, ClearReason::StalePromo)]);
    assert_eq!(row(&store, kept).0, "new");
}

#[test]
fn sealed_mail_is_never_taken() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let sealed = promo(&store, acct, "sealed", 20);
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE triage SET sensitivity = 'sealed' WHERE message_id = ?1",
            [sealed],
        )
        .unwrap();
    let outcome = apply(&store, acct, &[(sealed, ClearReason::Expired)]);
    assert!(outcome.cleared.is_empty(), "{:?}", outcome.cleared);
    assert_eq!(row(&store, sealed), ("new".into(), false, None));
}

// ---- the model's rows ---------------------------------------------------

#[test]
fn the_scan_offers_only_undecided_rows_that_have_a_summary() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let now = Utc::now();
    let ask = promo(&store, acct, "ask", 5);
    // Decided by a stored fact, protected by age, dated, and summary-less.
    promo(&store, acct, "stale", 20);
    promo(&store, acct, "today", 0);
    let dated = mail(&store, acct, "dated", "d@offers.example", 5);
    into_reading_with(&store, acct, dated, |d| {
        d.ends = time(now + Duration::days(9))
    });
    let blank = mail(&store, acct, "blank", "e@offers.example", 5);
    into_reading_with(&store, acct, blank, |d| d.summary = String::new());

    let asks = store
        .smart_clear_scan(acct, now, &AgentListQuery::default(), 200)
        .unwrap()
        .asks;
    assert_eq!(asks.len(), 1, "{asks:?}");
    assert_eq!(asks[0].message_id, ask);
    assert_eq!(asks[0].summary, "An issue");
    // The scan is a read.
    assert_eq!(row(&store, ask), ("new".into(), false, None));
}

#[test]
fn the_scan_is_capped_newest_first() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let mut ids = Vec::new();
    for i in 0..(SMART_CLEAR_MODEL_ROWS as i64 + 5) {
        let id = triaged(acct, &format!("m{i}"), &format!("m{i}"))
            .from(&format!("s{i}@many.example"))
            .received_at(Utc::now() - Duration::days(2) - Duration::minutes(i))
            .seed(&store);
        into_reading(&store, acct, id);
        ids.push(id);
    }
    let asks = store
        .smart_clear_scan(acct, Utc::now(), &AgentListQuery::default(), 200)
        .unwrap()
        .asks;
    assert_eq!(
        asks.iter().map(|a| a.message_id).collect::<Vec<_>>(),
        ids[..SMART_CLEAR_MODEL_ROWS]
    );
}

/// The owner keeps using the app while the model thinks. A verdict lands only
/// on a row that is still open and still undecided when the write happens.
#[test]
fn a_model_verdict_lands_only_on_a_row_that_is_still_askable() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let still = promo(&store, acct, "still", 5);
    let resolved = promo(&store, acct, "resolved", 5);
    let reminded = promo(&store, acct, "reminded", 5);
    let kept = promo(&store, acct, "kept", 5);
    let today = promo(&store, acct, "today", 0);
    // Between the scan and the write:
    store
        .set_attention_status(acct, resolved, AttentionStatus::Done)
        .unwrap();
    store
        .set_reminder(acct, reminded, Utc::now() + Duration::days(3))
        .unwrap();
    store
        .keep_reading_sender(acct, "kept@shop.example", Utc::now())
        .unwrap();

    let expired = ClearReason::Expired;
    let outcome = apply(
        &store,
        acct,
        &[
            (still, ClearReason::OfferEnded),
            (resolved, expired),
            (reminded, expired),
            (kept, expired),
            // Never offered: too young, and an id that is not mail at all.
            (today, expired),
            (9_999, expired),
        ],
    );
    assert_eq!(outcome.cleared, [(still, ClearReason::OfferEnded)]);
    // The row a person resolved is still theirs.
    assert_eq!(row(&store, resolved).2, None);
    assert_eq!(row(&store, kept).0, "new");
    assert_eq!(row(&store, today).0, "new");
}

// ---- provenance and undo ------------------------------------------------

#[test]
fn undo_restores_exactly_the_rows_nobody_has_touched_since() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let untouched_new = promo(&store, acct, "a", 20);
    let untouched_open = promo(&store, acct, "b", 20);
    // Surfaced before the sweep, so its status was `open`.
    store.mark_surfaced(acct, &[untouched_open]).unwrap();
    assert_eq!(row(&store, untouched_open).0, "open");
    let reopened_by_hand = promo(&store, acct, "c", 20);
    let redone_by_hand = promo(&store, acct, "d", 20);
    let reminded_by_hand = promo(&store, acct, "e", 20);
    // Done by a person BEFORE the sweep: never the batch's.
    let human = promo(&store, acct, "f", 20);
    store
        .set_attention_status(acct, human, AttentionStatus::Done)
        .unwrap();

    let batch = apply(&store, acct, &[]).batch_id.unwrap();
    let tag = Some(format!("smart_clear:{batch}"));
    for id in [
        untouched_new,
        untouched_open,
        reopened_by_hand,
        redone_by_hand,
        reminded_by_hand,
    ] {
        assert_eq!(row(&store, id), ("done".into(), true, tag.clone()));
    }
    assert_eq!(row(&store, human), ("done".into(), true, None));

    // A person takes three of them over, each by a different write.
    store
        .set_attention_status(acct, reopened_by_hand, AttentionStatus::Open)
        .unwrap();
    store
        .set_attention_status(acct, redone_by_hand, AttentionStatus::Done)
        .unwrap();
    store
        .set_reminder(acct, reminded_by_hand, Utc::now() + Duration::days(2))
        .unwrap();
    assert_eq!(row(&store, reopened_by_hand), ("open".into(), false, None));
    assert_eq!(row(&store, redone_by_hand), ("done".into(), true, None));
    assert_eq!(row(&store, reminded_by_hand), ("done".into(), true, None));

    // Somebody else's account cannot undo it, and an unknown batch is nothing.
    let other = store.ensure_account("other@example.com").unwrap();
    assert_eq!(
        store.smart_clear_undo(other, batch, Utc::now()).unwrap(),
        None
    );
    assert_eq!(
        store
            .smart_clear_undo(acct, batch + 99, Utc::now())
            .unwrap(),
        None
    );
    assert_eq!(
        row(&store, untouched_new).0,
        "done",
        "neither undid anything"
    );

    assert_eq!(
        store.smart_clear_undo(acct, batch, Utc::now()).unwrap(),
        Some(2)
    );
    // Each back to the status it had, with no trace of the sweep.
    assert_eq!(row(&store, untouched_new), ("new".into(), false, None));
    assert_eq!(row(&store, untouched_open), ("open".into(), false, None));
    // What a person did since is exactly as they left it.
    assert_eq!(row(&store, reopened_by_hand), ("open".into(), false, None));
    assert_eq!(row(&store, redone_by_hand), ("done".into(), true, None));
    assert_eq!(row(&store, reminded_by_hand), ("done".into(), true, None));
    assert_eq!(row(&store, human), ("done".into(), true, None));

    // A second undo is a no-op: re-done by hand in between, it stays done,
    // and the batch keeps the date it was first undone.
    let undone_at = |store: &SqliteStore| -> Option<String> {
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT undone_at FROM smart_clear_batches WHERE id = ?1",
                [batch],
                |r| r.get(0),
            )
            .unwrap()
    };
    let first = undone_at(&store).expect("stamped by the first undo");
    store
        .set_attention_status(acct, untouched_new, AttentionStatus::Done)
        .unwrap();
    assert_eq!(
        store
            .smart_clear_undo(acct, batch, Utc::now() + Duration::hours(1))
            .unwrap(),
        Some(0)
    );
    assert_eq!(row(&store, untouched_new).0, "done");
    assert_eq!(undone_at(&store), Some(first));
}

/// A fired reminder reopens a row whoever resolved it, and takes it over.
#[test]
fn a_fired_reminder_clears_the_provenance_of_the_row_it_reopens() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let id = promo(&store, acct, "a", 20);
    let batch = apply(&store, acct, &[]).batch_id.unwrap();
    // A pending reminder on a swept row, written underneath the store API so
    // that the sweep's stamp is still on it when the reminder comes due.
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE triage SET remind_at = ?1 WHERE message_id = ?2",
            params![(Utc::now() - Duration::minutes(1)).to_rfc3339(), id],
        )
        .unwrap();
    assert!(row(&store, id).2.is_some());
    assert_eq!(store.fire_due_reminders(acct, Utc::now()).unwrap(), [id]);
    assert_eq!(row(&store, id), ("open".into(), false, None));
    assert_eq!(
        store.smart_clear_undo(acct, batch, Utc::now()).unwrap(),
        Some(0)
    );
}

// ---- the keep record ----------------------------------------------------

#[test]
fn a_kept_sender_scores_zero_for_sixty_days_and_keeps_its_date_after() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    for i in 0..8 {
        let id = mail(&store, acct, &format!("n{i}"), NEWS, i + 1);
        if i == 0 {
            into_reading(&store, acct, id);
        }
    }
    let read = senders(&store, acct);
    assert_eq!(sender(&read, NEWS).kept_at, None);
    assert_eq!(sender(&read, NEWS).score, Some(55));

    let kept_at = Utc::now() - Duration::days(3);
    assert!(store.keep_reading_sender(acct, NEWS, kept_at).unwrap());
    assert!(!store.keep_reading_sender(acct, "  ", kept_at).unwrap());
    let read = senders(&store, acct);
    let s = sender(&read, NEWS);
    assert!(s.signals.kept_recently);
    assert_eq!(
        (s.score, &s.reasons),
        (Some(0), &vec![Reason::KeptRecently])
    );
    assert_eq!(
        s.kept_at.map(|at| at.timestamp()),
        Some(kept_at.timestamp())
    );

    // A second keep moves the date; it does not add a row.
    let older = Utc::now() - Duration::days(61);
    store.keep_reading_sender(acct, NEWS, older).unwrap();
    let rows: i64 = store
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM reading_sender_keeps", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1);
    // Sixty-one days on it no longer holds the score, and is still reported.
    let read = senders(&store, acct);
    let s = sender(&read, NEWS);
    assert!(!s.signals.kept_recently);
    assert_eq!(s.score, Some(55));
    assert_eq!(s.kept_at.map(|at| at.timestamp()), Some(older.timestamp()));
}

#[test]
fn a_later_unsubscribe_or_squelch_rule_outranks_a_keep() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let other = "deals@other.example";
    for from in [NEWS, other] {
        for i in 0..8 {
            let id = mail(&store, acct, &format!("{from}{i}"), from, i + 1);
            if i == 0 {
                into_reading(&store, acct, id);
            }
        }
        store
            .keep_reading_sender(acct, from, Utc::now() - Duration::days(2))
            .unwrap();
    }
    let read = senders(&store, acct);
    assert_eq!(sender(&read, NEWS).score, Some(0));
    assert_eq!(sender(&read, other).score, Some(0));

    // The owner changes their mind about each, a different way.
    store
        .upsert_unsubscribe(acct, NEWS, "browser", None, Utc::now())
        .unwrap();
    store
        .set_sender_rule(acct, "*@other.example", "", Disposition::Squelch)
        .unwrap();
    let read = senders(&store, acct);
    for from in [NEWS, other] {
        let s = sender(&read, from);
        assert!(!s.signals.kept_recently, "{from}");
        assert_eq!(s.score, Some(55), "{from}");
        assert!(s.kept_at.is_some(), "the row is not deleted");
    }

    // And keeping again after that is the newer decision once more.
    store.keep_reading_sender(acct, NEWS, Utc::now()).unwrap();
    assert_eq!(sender(&senders(&store, acct), NEWS).score, Some(0));
}

/// "Not spam" resets a row to `new`. Whatever resolved it before no longer
/// owns it.
#[test]
fn not_spam_clears_the_provenance_of_the_row_it_resets() {
    let (store, acct) = store();
    let id = triaged(acct, "junk", "junk").is_spam(true).seed(&store);
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE triage SET status = 'done', resolved_at = ?1, resolved_by = 'smart_clear:7'
             WHERE message_id = ?2",
            params![Utc::now().to_rfc3339(), id],
        )
        .unwrap();
    assert!(store.clear_spam(acct, id).unwrap());
    assert_eq!(row(&store, id), ("new".into(), false, None));
}
