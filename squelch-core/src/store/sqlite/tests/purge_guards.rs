//! What the Reading purge and smart done must never count, never take, and
//! never mistake for the owner. Each test here reproduces a review finding.

use super::super::*;
use super::reading_senders::{
    NEWS, into_reading, into_reading_with, mail, old_ledgers, sender, senders,
};
use super::smart_done::{apply, content, promo, row, time};
use super::support::*;
use crate::store::agent_triage::AgentTriageStore;
use crate::triage::decision::{
    AuthKind, EvidenceRef, MessageDecision, RecordProposal, ThreadAttentionDecision,
};
use crate::triage::smart_done::ClearReason;
use chrono::Duration;

/// The two ways the agent pipeline marks mail as kept close. Neither touches
/// the retired `triage.sensitivity` column, which stays `normal`.
type Sealing = (&'static str, fn(&mut MessageDecision));

fn sealings() -> [Sealing; 2] {
    [
        ("auth", |d| d.auth.kinds = vec![AuthKind::Otp]),
        ("restricted", |d| d.external_access.restricted = true),
    ]
}

fn sensitivity(store: &SqliteStore, id: i64) -> String {
    store
        .lock()
        .unwrap()
        .query_row(
            "SELECT sensitivity FROM triage WHERE message_id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
}

/// A decision that is NOT a Reading placement, committed the way the worker
/// commits one.
fn decide(store: &SqliteStore, acct: AccountId, id: i64, adjust: fn(&mut MessageDecision)) {
    into_reading_with(store, acct, id, |d| {
        d.destinations.clear();
        adjust(d);
    });
}

// ---- 1. sealed is the decision, not the retired column -------------------

#[test]
fn auth_and_restricted_mail_is_never_swept() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let control = promo(&store, acct, "control", 20);
    let mut kept = Vec::new();
    for (name, seal) in sealings() {
        let id = mail(&store, acct, name, &format!("{name}@shop.example"), 20);
        into_reading_with(&store, acct, id, seal);
        assert_eq!(
            sensitivity(&store, id),
            "normal",
            "the legacy column is unused"
        );
        kept.push(id);
    }
    // Even with the model saying so.
    let verdicts: Vec<_> = kept.iter().map(|id| (*id, ClearReason::Expired)).collect();
    let outcome = apply(&store, acct, &verdicts);
    assert_eq!(outcome.cleared, [(control, ClearReason::StalePromo)]);
    for id in kept {
        assert_eq!(row(&store, id), ("new".into(), false, None));
    }
}

#[test]
fn auth_and_restricted_mail_is_not_counted_for_its_sender() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    for i in 0..4 {
        let id = mail(&store, acct, &format!("n{i}"), NEWS, i + 1);
        if i == 0 {
            into_reading(&store, acct, id);
        }
    }
    // Login codes and restricted mail from the same address: never opened,
    // and two of them cleared without opening.
    for (name, seal) in sealings() {
        for i in 0..2 {
            let id = mail(&store, acct, &format!("{name}{i}"), NEWS, i + 2);
            decide(&store, acct, id, seal);
            if i == 0 {
                store
                    .set_attention_status(acct, id, AttentionStatus::Done)
                    .unwrap();
            }
        }
    }
    let read = senders(&store, acct);
    let s = sender(&read, NEWS).signals;
    assert_eq!(s.received, 4, "the shop's codes are not its newsletter");
    assert_eq!(s.cleared_unopened, 0);
}

#[test]
fn auth_and_restricted_mail_is_never_the_unsubscribe_representative() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let issue = triaged(acct, "issue", "issue")
        .from(NEWS)
        .received_at(Utc::now() - Duration::days(9))
        .list_unsubscribe("<https://letters.example/u>", false)
        .seed(&store);
    into_reading(&store, acct, issue);
    for (i, (name, seal)) in sealings().into_iter().enumerate() {
        let id = triaged(acct, name, name)
            .from(NEWS)
            .received_at(Utc::now() - Duration::days(1 + i as i64))
            .list_unsubscribe("<https://letters.example/code>", false)
            .seed(&store);
        decide(&store, acct, id, seal);
    }
    let read = senders(&store, acct);
    assert_eq!(
        sender(&read, NEWS)
            .unsub_candidates
            .iter()
            .map(|c| c.message_id)
            .collect::<Vec<_>>(),
        [issue]
    );
}

#[test]
fn attention_is_not_stored_on_auth_or_restricted_mail() {
    let (store, acct) = store();
    let plain = mail(&store, acct, "plain", NEWS, 1);
    decide(&store, acct, plain, |_| {});
    store
        .record_message_attention(acct, plain, 4_000, 1)
        .unwrap();
    let stored = |id: i64| -> (i64, i64) {
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT dwell_ms, link_clicks FROM triage WHERE message_id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    };
    assert_eq!(stored(plain), (4_000, 1), "ordinary mail still records");
    for (name, seal) in sealings() {
        let id = mail(&store, acct, name, NEWS, 1);
        decide(&store, acct, id, seal);
        // Accepted, as every report is, and never written.
        store.record_message_attention(acct, id, 4_000, 1).unwrap();
        assert_eq!(stored(id), (0, 0), "{name}");
    }
}

// ---- 2. never clear what is also somewhere else --------------------------

#[test]
fn a_row_whose_thread_shows_in_fye_is_never_swept() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let control = promo(&store, acct, "control", 20);
    let on_board = mail(&store, acct, "board", "board@shop.example", 20);
    into_reading_with(&store, acct, on_board, |d| {
        let evidence = vec![EvidenceRef {
            message_id: on_board,
            location: "body".into(),
        }];
        d.attention = ThreadAttentionDecision {
            show_in_fye: true,
            relevant_message_ids: vec![on_board],
            evidence,
            summary: "Worth a look".into(),
            ..Default::default()
        };
    });
    let fye = |store: &SqliteStore| -> Vec<i64> {
        store
            .agent_fye(acct, 50, &Default::default(), Utc::now())
            .unwrap()
            .iter()
            .map(|i| i.message_id)
            .collect()
    };
    assert_eq!(fye(&store), [on_board], "it is on the board");

    let outcome = apply(&store, acct, &[(on_board, ClearReason::Expired)]);
    assert_eq!(outcome.cleared, [(control, ClearReason::StalePromo)]);
    assert_eq!(fye(&store), [on_board], "and it still is");
}

#[test]
fn a_row_carrying_a_bill_receipt_or_delivery_is_never_swept() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let control = promo(&store, acct, "control", 20);
    let receipt = mail(&store, acct, "receipt", "orders@shop.example", 20);
    into_reading_with(&store, acct, receipt, |d| {
        d.records = vec![RecordProposal::Receipt {
            merchant: "Shop".into(),
            amount: Some(10.0),
            currency: Some("USD".into()),
            evidence: vec![],
        }];
    });
    let outcome = apply(&store, acct, &[(receipt, ClearReason::Expired)]);
    assert_eq!(outcome.cleared, [(control, ClearReason::StalePromo)]);
    assert_eq!(row(&store, receipt).0, "new");
}

// ---- 5. a young ledger knows nothing -------------------------------------

#[test]
fn a_ledger_under_two_weeks_old_scores_nobody_and_sweeps_no_sender() {
    let (store, acct) = store();
    let ledger = (Utc::now() - Duration::hours(30)).to_rfc3339();
    store
        .set_app_setting(acct, OPEN_LEDGER_SINCE_KEY, &ledger)
        .unwrap();
    store
        .set_app_setting(acct, DWELL_LEDGER_SINCE_KEY, &ledger)
        .unwrap();
    // Eight mails in a day, six cleared unopened by hand: on thirty hours of
    // evidence that used to be a score of 76 and "192 a month".
    let mut open = Vec::new();
    for i in 0..8 {
        let id = triaged(acct, &format!("n{i}"), &format!("n{i}"))
            .from(NEWS)
            .received_at(Utc::now() - Duration::hours(25) - Duration::minutes(i))
            .seed(&store);
        if i < 2 {
            into_reading_with(&store, acct, id, content);
            open.push(id);
        } else {
            store
                .set_attention_status(acct, id, AttentionStatus::Done)
                .unwrap();
        }
    }
    // And an essay from before the ledger began.
    let backlog = mail(&store, acct, "backlog", NEWS, 10);
    into_reading_with(&store, acct, backlog, content);

    let read = senders(&store, acct);
    let s = sender(&read, NEWS);
    assert_eq!(s.signals.received, 8);
    assert_eq!(s.score, None, "{:?}", s.reasons);
    assert_eq!(
        s.reasons,
        vec![crate::triage::sender_fatigue::Reason::NotEnoughHistory]
    );
    let outcome = apply(&store, acct, &[]);
    assert!(outcome.cleared.is_empty(), "{:?}", outcome.cleared);
    for id in open.into_iter().chain([backlog]) {
        assert_eq!(row(&store, id).0, "new");
    }
}

// ---- 6. spoofed mail is not the sender -----------------------------------

#[test]
fn mail_that_failed_authentication_is_not_the_senders() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    // The real newsletter: four issues, one unchecked (older than the column).
    let mut real = Vec::new();
    for i in 0..4 {
        let id = triaged(acct, &format!("real{i}"), &format!("real{i}"))
            .from(NEWS)
            .received_at(Utc::now() - Duration::days(10 + i))
            .list_unsubscribe(&format!("<https://letters.example/u/{i}>"), false)
            .auth_pass(if i == 0 { None } else { Some(true) })
            .seed(&store);
        real.push(id);
    }
    into_reading(&store, acct, real[0]);
    // A flood forged in its name, newer than all of it, cleared unopened.
    for i in 0..6 {
        let id = triaged(acct, &format!("forged{i}"), &format!("forged{i}"))
            .from(NEWS)
            .received_at(Utc::now() - Duration::days(1) - Duration::minutes(i))
            .list_unsubscribe("<https://attacker.example/u>", false)
            .auth_pass(Some(false))
            .seed(&store);
        store
            .set_attention_status(acct, id, AttentionStatus::Done)
            .unwrap();
    }
    let read = senders(&store, acct);
    let s = sender(&read, NEWS);
    assert_eq!(
        s.signals.received, 4,
        "unchecked mail counts, forged does not"
    );
    assert_eq!(s.signals.cleared_unopened, 0);
    // The representative is an authenticated issue, ahead of the newer
    // unchecked one, and never a forged one.
    assert_eq!(
        s.unsub_candidates
            .iter()
            .map(|c| (c.message_id, c.auth_pass))
            .collect::<Vec<_>>(),
        [
            (real[1], Some(true)),
            (real[2], Some(true)),
            (real[3], Some(true))
        ]
    );
}

// ---- 8. a sweep is not the owner -----------------------------------------

#[test]
fn the_rule_digest_does_not_call_a_sweep_a_dismissal() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let swept = promo(&store, acct, "swept", 20);
    let by_hand = mail(&store, acct, "hand", "swept@shop.example", 19);
    store
        .set_attention_status(acct, by_hand, AttentionStatus::Done)
        .unwrap();
    assert_eq!(apply(&store, acct, &[]).cleared.len(), 1);
    assert_eq!(row(&store, swept).0, "done");

    let digest = store
        .sender_rule_digest(acct, "swept@shop.example", 10)
        .unwrap();
    let cleared_by_owner = digest
        .iter()
        .filter(|r| r.status == "done" && !r.opened)
        .count();
    assert_eq!(digest.len(), 2);
    assert_eq!(cleared_by_owner, 1, "only the one a person cleared");
}

#[test]
fn an_owner_sweep_by_sender_takes_over_rows_smart_done_took() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let taken = promo(&store, acct, "taken", 20);
    let other = promo(&store, acct, "other", 20);
    let batch = apply(&store, acct, &[]).batch_id.unwrap();
    // The owner unsubscribes from one of the senders: all their mail is done,
    // by the owner now.
    store.resolve_sender(acct, "taken@shop.example").unwrap();
    assert_eq!(row(&store, taken), ("done".into(), true, None));
    assert_eq!(
        store.smart_done_undo(acct, batch, Utc::now()).unwrap(),
        Some(1)
    );
    assert_eq!(row(&store, taken).0, "done", "the owner's sweep stands");
    assert_eq!(row(&store, other).0, "new");
}

#[test]
fn a_snoozed_thread_is_not_cleared_unopened() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    for i in 0..6 {
        let id = mail(&store, acct, &format!("n{i}"), NEWS, i + 1);
        match i {
            0 => into_reading(&store, acct, id),
            // Snoozed: done for now, because the owner wants it back.
            1 | 2 => {
                store
                    .set_reminder(acct, id, Utc::now() + Duration::days(2))
                    .unwrap();
            }
            3 => {
                store
                    .set_attention_status(acct, id, AttentionStatus::Done)
                    .unwrap();
            }
            _ => {}
        }
    }
    let read = senders(&store, acct);
    assert_eq!(sender(&read, NEWS).signals.cleared_unopened, 1);
}

// ---- 10. undo reads the batch's own list ----------------------------------

#[test]
fn undo_reaches_only_the_rows_the_batch_recorded() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let taken = promo(&store, acct, "taken", 20);
    let batch = apply(&store, acct, &[]).batch_id.unwrap();
    // A row that wears the batch's tag and was never in its list.
    let stray = mail(&store, acct, "stray", NEWS, 3);
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE triage SET status = 'done', resolved_at = ?1, resolved_by = ?2
             WHERE message_id = ?3",
            params![
                Utc::now().to_rfc3339(),
                format!("smart_done:{batch}"),
                stray
            ],
        )
        .unwrap();
    assert_eq!(
        store.smart_done_undo(acct, batch, Utc::now()).unwrap(),
        Some(1)
    );
    assert_eq!(row(&store, taken).0, "new");
    assert_eq!(row(&store, stray).0, "done", "the list is the membership");
    let _ = time;
}

// ---- 7. a verdict is about the summary that was sent ----------------------

/// A message re-triaged while the model was thinking has the same id and a
/// different summary. The verdict was about the old one.
#[test]
fn a_verdict_for_a_summary_that_has_since_changed_is_dropped() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let same = promo(&store, acct, "same", 5);
    let changed = promo(&store, acct, "changed", 5);
    let inventory = crate::store::agent_triage::AgentListQuery::default();
    let scan = store
        .smart_done_scan(acct, Utc::now(), &inventory, 200)
        .unwrap();
    assert_eq!(scan.asks.len(), 2);
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_message_decisions
             SET decision_json = json_set(decision_json, '$.summary', 'Sale extended to the 30th')
             WHERE message_id = ?1",
            [changed],
        )
        .unwrap();
    // The model answers about what it was sent.
    let verdicts: std::collections::HashMap<i64, crate::triage::smart_done::Verdict> = scan
        .asks
        .iter()
        .map(|a| {
            (
                a.message_id,
                crate::triage::smart_done::Verdict::about(ClearReason::Expired, &a.summary),
            )
        })
        .collect();
    let outcome = store
        .smart_done_apply(acct, Utc::now(), &inventory, 200, &scan, &verdicts)
        .unwrap();
    assert_eq!(outcome.cleared, [(same, ClearReason::Expired)]);
    assert_eq!(row(&store, changed).0, "new");
}

// ---- 10. the scan's scores are reused, and nothing else is ----------------

/// An ignored sender with two open essays, scored on fifteen days of ledger.
fn ignored_sender(store: &SqliteStore, acct: AccountId) -> [i64; 2] {
    let ledger = (Utc::now() - Duration::days(15)).to_rfc3339();
    store
        .set_app_setting(acct, OPEN_LEDGER_SINCE_KEY, &ledger)
        .unwrap();
    store
        .set_app_setting(acct, DWELL_LEDGER_SINCE_KEY, &ledger)
        .unwrap();
    let mut open = Vec::new();
    for i in 0..11 {
        let id = mail(store, acct, &format!("n{i}"), NEWS, 2 + i % 7);
        if i < 2 {
            into_reading_with(store, acct, id, content);
            open.push(id);
        } else {
            store
                .set_attention_status(acct, id, AttentionStatus::Done)
                .unwrap();
        }
    }
    [open[0], open[1]]
}

/// The write reuses the sender scores the first read measured. What the owner
/// did between the two still decides: the score only ever proposes.
#[test]
fn a_score_from_the_scan_does_not_outrank_what_the_owner_did_since() {
    let inventory = crate::store::agent_triage::AgentListQuery::default();
    let none = std::collections::HashMap::new();

    // Opened one of the two while the model was thinking.
    {
        let (store, acct) = store();
        let [first, second] = ignored_sender(&store, acct);
        let scan = store
            .smart_done_scan(acct, Utc::now(), &inventory, 200)
            .unwrap();
        assert_eq!(scan.sender_scores.get(NEWS), Some(&Some(77)));
        store
            .acknowledge_agent_message(acct, first, Utc::now())
            .unwrap();
        let outcome = store
            .smart_done_apply(acct, Utc::now(), &inventory, 200, &scan, &none)
            .unwrap();
        assert_eq!(outcome.cleared, [(second, ClearReason::IgnoredSender)]);
        assert_eq!(row(&store, first).0, "new");
    }
    // Kept the sender while the model was thinking.
    {
        let (store, acct) = store();
        let [first, second] = ignored_sender(&store, acct);
        let scan = store
            .smart_done_scan(acct, Utc::now(), &inventory, 200)
            .unwrap();
        store.keep_reading_sender(acct, NEWS, Utc::now()).unwrap();
        let outcome = store
            .smart_done_apply(acct, Utc::now(), &inventory, 200, &scan, &none)
            .unwrap();
        assert!(outcome.cleared.is_empty(), "{:?}", outcome.cleared);
        assert_eq!(row(&store, first).0, "new");
        assert_eq!(row(&store, second).0, "new");
    }
    // And a sender the scan never scored is not swept on no score at all.
    let (store, acct) = store();
    let [first, _] = ignored_sender(&store, acct);
    let outcome = store
        .smart_done_apply(
            acct,
            Utc::now(),
            &inventory,
            200,
            &Default::default(),
            &none,
        )
        .unwrap();
    assert!(outcome.cleared.is_empty());
    assert_eq!(row(&store, first).0, "new");
}

// ---- pending access: a report is kept, a statistic waits -------------------

fn stored_attention(store: &SqliteStore, id: i64) -> (i64, i64) {
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

/// Mail opened in the first moments after it arrives is read before triage
/// has assessed it. That is exactly when dwell is real, and a refused report
/// is lost for good, so the write takes it. Only mail KNOWN to be kept close
/// is refused.
#[test]
fn a_report_on_mail_still_waiting_on_its_assessment_is_stored() {
    let (store, acct) = store();
    let pending = mail(&store, acct, "pending", NEWS, 1);
    store
        .enqueue_agent_triage(acct, pending, "test", false)
        .unwrap();
    store
        .record_message_attention(acct, pending, 4_000, 1)
        .unwrap();
    assert_eq!(stored_attention(&store, pending), (4_000, 1));

    // Assessed and restricted, with no decision saying so: still refused.
    let restricted = mail(&store, acct, "restricted", NEWS, 1);
    store
        .enqueue_agent_triage(acct, restricted, "test", false)
        .unwrap();
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_message_state SET access = 'restricted' WHERE message_id = ?1",
            [restricted],
        )
        .unwrap();
    store
        .record_message_attention(acct, restricted, 4_000, 1)
        .unwrap();
    assert_eq!(stored_attention(&store, restricted), (0, 0));
}

/// The other half of that bargain: a count that landed on a pending row is
/// harmless if the row turns out to be a login code, because every READER
/// leaves kept-close mail out at read time.
#[test]
fn dwell_reported_before_an_auth_decision_reaches_no_statistic() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    for i in 0..4 {
        let id = mail(&store, acct, &format!("n{i}"), NEWS, i + 2);
        if i == 0 {
            into_reading(&store, acct, id);
        }
    }
    // Arrives, is opened and read at once, and only then is triaged: a code.
    let code = mail(&store, acct, "code", NEWS, 1);
    store
        .enqueue_agent_triage(acct, code, "test", false)
        .unwrap();
    store
        .acknowledge_agent_message(acct, code, Utc::now())
        .unwrap();
    store
        .record_message_attention(acct, code, 9_000, 3)
        .unwrap();
    assert_eq!(stored_attention(&store, code), (9_000, 3), "it was taken");

    // While it is pending it does not count for its sender at all.
    let waiting = senders(&store, acct);
    let s = sender(&waiting, NEWS).signals;
    assert_eq!((s.received, s.opened, s.dwell_sample), (4, 0, 0));

    // The decision lands: auth.
    let job = store
        .claim_agent_job(acct, "triage", Utc::now(), 60)
        .unwrap()
        .unwrap();
    assert_eq!(job.message_id, code);
    let context = store.load_agent_context(&job).unwrap();
    let mut decision = MessageDecision {
        kinds: vec![crate::triage::decision::EmailKind::AuthenticationSecurity],
        summary: "A sign-in code".into(),
        reason: "A code".into(),
        ..Default::default()
    };
    decision.auth.kinds = vec![AuthKind::Otp];
    assert_eq!(
        store
            .commit_agent_decision(
                &job,
                &context,
                &decision,
                std::slice::from_ref(&context.message.source)
            )
            .unwrap(),
        crate::store::agent_triage::AgentCommitOutcome::Applied
    );
    assert_eq!(
        stored_attention(&store, code),
        (9_000, 3),
        "still on the row"
    );

    let read = senders(&store, acct);
    let s = sender(&read, NEWS).signals;
    assert_eq!(s.received, 4, "the code is not the newsletter's mail");
    assert_eq!(s.opened, 0);
    assert_eq!(s.dwell_sample, 0);
    assert_eq!(s.median_dwell_ms, None);
    assert_eq!(s.clicks, 0);
    // And no other sender picked it up.
    assert_eq!(read.senders.len(), 1);
}
