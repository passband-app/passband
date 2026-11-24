//! What smart clear must never count, never take, and never mistake for the
//! owner. Each test here reproduces a review finding.

use super::super::*;
use super::reading_senders::{
    NEWS, into_reading, into_reading_with, mail, old_ledgers, sender, senders,
};
use super::smart_clear::{apply, content, promo, row, time};
use super::support::*;
use crate::store::agent_triage::AgentTriageStore;
use crate::triage::decision::{
    AuthKind, EmailKind, EvidenceRef, MessageDecision, RecordProposal, ThreadAttentionDecision,
};
use crate::triage::smart_clear::ClearReason;
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
}

#[test]
fn archive_mail_is_not_counted_for_its_sender() {
    // Old mail the archive tier stored to be found was never in front of the
    // owner, so it never went unopened: counting it would make every sender
    // with a long history look ignored.
    let (store, acct) = store();
    old_ledgers(&store, acct);
    for i in 0..4 {
        let id = mail(&store, acct, &format!("n{i}"), NEWS, i + 1);
        if i == 0 {
            into_reading(&store, acct, id);
        }
    }
    for i in 0..5 {
        let id = mail(&store, acct, &format!("arch{i}"), NEWS, i + 2);
        store
            .set_attention_status(acct, id, AttentionStatus::Done)
            .unwrap();
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE messages SET is_archive = 1 WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap();
    }
    let read = senders(&store, acct);
    let s = sender(&read, NEWS).signals;
    assert_eq!(
        s.received, 4,
        "archive rows are not the sender's recent mail"
    );
    assert_eq!(s.cleared_unopened, 0);
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
fn an_owner_sweep_by_sender_takes_over_rows_smart_clear_took() {
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
        store.smart_clear_undo(acct, batch, Utc::now()).unwrap(),
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
                format!("smart_clear:{batch}"),
                stray
            ],
        )
        .unwrap();
    assert_eq!(
        store.smart_clear_undo(acct, batch, Utc::now()).unwrap(),
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
        .smart_clear_scan(acct, Utc::now(), &inventory, 200)
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
    let verdicts: std::collections::HashMap<i64, crate::triage::smart_clear::Verdict> = scan
        .asks
        .iter()
        .map(|a| {
            (
                a.message_id,
                crate::triage::smart_clear::Verdict::about(ClearReason::Expired, &a.summary),
            )
        })
        .collect();
    let outcome = store
        .smart_clear_apply(acct, Utc::now(), &inventory, 200, &scan, &verdicts)
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
            .smart_clear_scan(acct, Utc::now(), &inventory, 200)
            .unwrap();
        assert_eq!(
            scan.sender_scores.get(NEWS).and_then(|mark| mark.score),
            Some(77)
        );
        store
            .acknowledge_agent_message(acct, first, Utc::now())
            .unwrap();
        // An open since the scan means the scan's score is out of date, and
        // opening mail is exactly what lowers it. The sender is treated as
        // unscored for this press: neither of the two goes.
        let outcome = store
            .smart_clear_apply(acct, Utc::now(), &inventory, 200, &scan, &none)
            .unwrap();
        assert!(outcome.cleared.is_empty(), "{:?}", outcome.cleared);
        assert_eq!(row(&store, second).0, "new");
        assert_eq!(row(&store, first).0, "new");
    }
    // Kept the sender while the model was thinking.
    {
        let (store, acct) = store();
        let [first, second] = ignored_sender(&store, acct);
        let scan = store
            .smart_clear_scan(acct, Utc::now(), &inventory, 200)
            .unwrap();
        store.keep_reading_sender(acct, NEWS, Utc::now()).unwrap();
        let outcome = store
            .smart_clear_apply(acct, Utc::now(), &inventory, 200, &scan, &none)
            .unwrap();
        assert!(outcome.cleared.is_empty(), "{:?}", outcome.cleared);
        assert_eq!(row(&store, first).0, "new");
        assert_eq!(row(&store, second).0, "new");
    }
    // And a sender the scan never scored is not swept on no score at all.
    let (store, acct) = store();
    let [first, _] = ignored_sender(&store, acct);
    let outcome = store
        .smart_clear_apply(
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

// ---- final review ---------------------------------------------------------

use crate::triage::decision::{AttentionAction, SupportedTime};

fn evidence(id: i64) -> Vec<EvidenceRef> {
    vec![EvidenceRef {
        message_id: id,
        location: "body".into(),
    }]
}

fn stated(value: impl Into<String>, timezone: Option<&str>) -> Option<SupportedTime> {
    Some(SupportedTime {
        value: value.into(),
        timezone: timezone.map(str::to_string),
        ..Default::default()
    })
}

fn gala(start: impl Into<String>, timezone: Option<&str>) -> RecordProposal {
    RecordProposal::Event {
        title: "Gala".into(),
        start: stated(start, timezone),
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
    }
}

/// The triage agent writes a timed event as a LOCAL time with a zone name
/// beside it (`2026-10-10T19:00:00`, no offset): three in four of the event
/// starts in a real store. An event still ahead in that shape must protect
/// the row exactly as an RFC3339 one does, and a start nobody can parse must
/// leave the row alone rather than count as no date at all.
#[test]
fn an_upcoming_event_protects_however_its_time_is_written() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let ahead = Utc::now() + Duration::days(5);
    let control = promo(&store, acct, "control", 20);
    let mut kept = Vec::new();
    let shapes = [
        ("seconds", ahead.format("%Y-%m-%dT%H:%M:%S").to_string()),
        ("minutes", ahead.format("%Y-%m-%dT%H:%M").to_string()),
        ("offset", ahead.to_rfc3339()),
        ("words", "next Friday evening".to_string()),
    ];
    for (name, start) in shapes {
        // A three-week-old unopened promotion for it...
        let id = mail(&store, acct, name, &format!("{name}@shop.example"), 20);
        let value = start.clone();
        into_reading_with(&store, acct, id, move |d| {
            d.records = vec![gala(value, Some("America/Los_Angeles"))];
        });
        kept.push(id);
        // ...and an essay about it, which would otherwise go to the model.
        let essay = mail(
            &store,
            acct,
            &format!("{name}-essay"),
            "essays@x.example",
            5,
        );
        into_reading_with(&store, acct, essay, move |d| {
            content(d);
            d.summary = "Join us for the gala on the 10th".into();
            d.records = vec![gala(start, Some("America/Los_Angeles"))];
        });
        kept.push(essay);
    }
    let scan = store
        .smart_clear_scan(
            acct,
            Utc::now(),
            &crate::store::agent_triage::AgentListQuery::default(),
            200,
        )
        .unwrap();
    assert!(
        scan.asks.is_empty(),
        "none is the model's to judge: {:?}",
        scan.asks
    );
    let verdicts: Vec<_> = kept
        .iter()
        .map(|id| (*id, ClearReason::EventPassed))
        .collect();
    let outcome = apply(&store, acct, &verdicts);
    assert_eq!(outcome.cleared, [(control, ClearReason::StalePromo)]);
    // And the event is still on the Calendar side of the house.
    let records = store.agent_records(acct, 100).unwrap();
    assert_eq!(
        records.len(),
        kept.len(),
        "every one still listed as a record"
    );
}

/// A naive timestamp that HAS passed is evidence too.
#[test]
fn a_timed_event_that_has_passed_clears_in_the_agents_own_time_format() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let id = mail(&store, acct, "gala", "gala@events.example", 9);
    let start = (Utc::now() - Duration::days(4))
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string();
    into_reading_with(&store, acct, id, move |d| {
        content(d);
        d.records = vec![gala(start, Some("Europe/Berlin"))];
    });
    assert_eq!(
        apply(&store, acct, &[]).cleared,
        [(id, ClearReason::EventPassed)]
    );
}

/// An open action with a date ahead, or a revisit the agent asked for, is the
/// mail saying it is not finished. Neither is ever a reason to clear.
#[test]
fn an_open_action_or_a_pending_revisit_keeps_a_row() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let control = promo(&store, acct, "control", 20);
    let due = mail(&store, acct, "rsvp", "rsvp@shop.example", 20);
    into_reading_with(&store, acct, due, move |d| {
        d.attention = ThreadAttentionDecision {
            relevant_message_ids: vec![due],
            evidence: evidence(due),
            actions: vec![AttentionAction {
                description: "Register before the deadline".into(),
                due: stated(
                    (Utc::now() + Duration::days(6))
                        .format("%Y-%m-%d")
                        .to_string(),
                    None,
                ),
                evidence: evidence(due),
                ..Default::default()
            }],
            ..Default::default()
        };
    });
    let revisit = mail(&store, acct, "revisit", "later@shop.example", 20);
    into_reading_with(&store, acct, revisit, |d| {
        d.revisit = Some(crate::triage::decision::RevisitRequest {
            at: Utc::now() + Duration::days(3),
            reason: "See whether the sale was extended".into(),
        });
    });
    // A RESOLVED action, and one whose date has gone by, protect nothing.
    let settled = mail(&store, acct, "settled", "settled@shop.example", 20);
    into_reading_with(&store, acct, settled, move |d| {
        d.attention = ThreadAttentionDecision {
            relevant_message_ids: vec![settled],
            evidence: evidence(settled),
            actions: vec![
                AttentionAction {
                    description: "Done already".into(),
                    resolved: true,
                    due: stated(
                        (Utc::now() + Duration::days(6))
                            .format("%Y-%m-%d")
                            .to_string(),
                        None,
                    ),
                    evidence: evidence(settled),
                    ..Default::default()
                },
                AttentionAction {
                    description: "Missed".into(),
                    due: stated(
                        (Utc::now() - Duration::days(6))
                            .format("%Y-%m-%d")
                            .to_string(),
                        None,
                    ),
                    evidence: evidence(settled),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
    });
    let mut cleared = apply(&store, acct, &[]).cleared;
    cleared.sort();
    assert_eq!(
        cleared,
        [
            (control, ClearReason::StalePromo),
            (settled, ClearReason::StalePromo)
        ]
    );
    assert_eq!(row(&store, due).0, "new");
    assert_eq!(row(&store, revisit).0, "new");
}

/// An undo is the owner saying "not those". The next press must not take the
/// same rows again.
#[test]
fn rows_an_undo_gave_back_are_not_taken_again() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let a = promo(&store, acct, "a", 20);
    let b = promo(&store, acct, "b", 20);
    let batch = apply(&store, acct, &[]).batch_id.unwrap();
    // The owner re-does one by hand before undoing: that one was not given
    // back, so nothing is remembered about it.
    store
        .set_attention_status(acct, b, AttentionStatus::Done)
        .unwrap();
    assert_eq!(
        store.smart_clear_undo(acct, batch, Utc::now()).unwrap(),
        Some(1)
    );
    let later = promo(&store, acct, "later", 20);
    let again = apply(&store, acct, &[(a, ClearReason::Expired)]);
    assert_eq!(again.cleared, [(later, ClearReason::StalePromo)]);
    assert_eq!(row(&store, a).0, "new");
}

/// A batch whose record cannot be read is not thrown away by trying.
#[test]
fn a_batch_whose_detail_cannot_be_read_is_not_marked_undone() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let id = promo(&store, acct, "a", 20);
    let batch = apply(&store, acct, &[]).batch_id.unwrap();
    let tag = Some(format!("smart_clear:{batch}"));
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
    for broken in ["{\"by_reason\":{}}", "not json", "{\"rows\":\"none\"}"] {
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE smart_clear_batches SET detail = ?1 WHERE id = ?2",
                params![broken, batch],
            )
            .unwrap();
        assert!(
            store.smart_clear_undo(acct, batch, Utc::now()).is_err(),
            "{broken}"
        );
        assert_eq!(undone_at(&store), None, "{broken}");
        assert_eq!(row(&store, id), ("done".into(), true, tag.clone()));
    }
    // Repaired, it undoes as it always would have.
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE smart_clear_batches SET detail = ?1 WHERE id = ?2",
            params![
                serde_json::json!({"rows": [{"id": id, "reason": "stale_promo", "was": "new"}]})
                    .to_string(),
                batch
            ],
        )
        .unwrap();
    assert_eq!(
        store.smart_clear_undo(acct, batch, Utc::now()).unwrap(),
        Some(1)
    );
}

/// A promotion a week old is stale whether or not opens were being recorded
/// when it arrived: it has had its week, wherever it was read.
#[test]
fn a_promotion_from_before_the_open_ledger_still_goes_stale() {
    let (store, acct) = store();
    // The ledger began three weeks ago.
    let ledger = (Utc::now() - Duration::days(21)).to_rfc3339();
    store
        .set_app_setting(acct, OPEN_LEDGER_SINCE_KEY, &ledger)
        .unwrap();
    store
        .set_app_setting(acct, DWELL_LEDGER_SINCE_KEY, &ledger)
        .unwrap();
    let backfilled = promo(&store, acct, "backfilled", 25);
    let since = promo(&store, acct, "since", 20);
    let outcome = apply(&store, acct, &[]);
    let mut cleared = outcome.cleared.clone();
    cleared.sort();
    let mut want = vec![
        (backfilled, ClearReason::StalePromo),
        (since, ClearReason::StalePromo),
    ];
    want.sort();
    assert_eq!(cleared, want);
}

/// THE POINT OF `skimmed_sender`: on a ledger two days old, with no score for
/// anyone, a sender whose mail the owner opens and shuts in a second loses
/// its promotions, unopened ones included. A sender read properly does not.
#[test]
fn a_skimmed_senders_promotions_clear_on_a_young_ledger() {
    let (store, acct) = store();
    let now = Utc::now();
    let ledger = (now - Duration::days(2)).to_rfc3339();
    store
        .set_app_setting(acct, OPEN_LEDGER_SINCE_KEY, &ledger)
        .unwrap();
    store
        .set_app_setting(acct, DWELL_LEDGER_SINCE_KEY, &ledger)
        .unwrap();
    let seed = |key: &str, from: &str, dwell: Option<u64>| {
        let id = mail(&store, acct, key, from, 2);
        into_reading(&store, acct, id);
        if let Some(ms) = dwell {
            store
                .acknowledge_agent_message(acct, id, now - Duration::hours(30))
                .unwrap();
            store.record_message_attention(acct, id, ms, 0).unwrap();
        }
        id
    };
    let glanced: Vec<i64> = (0..3)
        .map(|i| seed(&format!("glance{i}"), "deals@skim.example", Some(1_000)))
        .collect();
    let unopened = seed("unopened", "deals@skim.example", None);
    let read: Vec<i64> = (0..3)
        .map(|i| seed(&format!("read{i}"), "deals@loved.example", Some(60_000)))
        .collect();
    let loved_unopened = seed("loved", "deals@loved.example", None);

    let outcome = apply(&store, acct, &[]);
    let mut cleared = outcome.cleared.clone();
    cleared.sort();
    let mut want: Vec<(i64, ClearReason)> = glanced
        .iter()
        .chain([&unopened])
        .map(|id| (*id, ClearReason::SkimmedSender))
        .collect();
    want.sort();
    assert_eq!(cleared, want);
    for id in read.iter().chain([&loved_unopened]) {
        assert_eq!(row(&store, *id).0, "new", "a sender read properly stays");
    }
}

/// Seed a skimmed sender on a two-day ledger: three promos glanced at for a
/// second each, and one unopened. Returns (the glanced ids, the unopened id).
fn skimmed_sender(store: &SqliteStore, acct: AccountId) -> (Vec<i64>, i64) {
    let now = Utc::now();
    let ledger = (now - Duration::days(2)).to_rfc3339();
    store
        .set_app_setting(acct, OPEN_LEDGER_SINCE_KEY, &ledger)
        .unwrap();
    store
        .set_app_setting(acct, DWELL_LEDGER_SINCE_KEY, &ledger)
        .unwrap();
    let seed = |key: &str, glance: bool| {
        let id = mail(store, acct, key, "deals@skim.example", 2);
        into_reading(store, acct, id);
        if glance {
            store
                .acknowledge_agent_message(acct, id, now - Duration::hours(30))
                .unwrap();
            store.record_message_attention(acct, id, 1_000, 0).unwrap();
        }
        id
    };
    let glanced = (0..3).map(|i| seed(&format!("g{i}"), true)).collect();
    (glanced, seed("unopened", false))
}

/// THE SKIM MARK IS DROPPED WHEN THE OWNER LOOKS AGAIN during the model call,
/// even when the look is a REOPEN, which stamps no new `opened_at`.
#[test]
fn a_skim_mark_from_the_scan_does_not_outlive_a_read_since() {
    let inventory = crate::store::agent_triage::AgentListQuery::default();
    let apply_after = |store: &SqliteStore, acct, scan: &_| {
        store
            .smart_clear_apply(acct, Utc::now(), &inventory, 200, scan, &Default::default())
            .unwrap()
    };
    // Untouched: the mark holds and the sender's promos go.
    {
        let (store, acct) = store();
        let (_, unopened) = skimmed_sender(&store, acct);
        let scan = store
            .smart_clear_scan(acct, Utc::now(), &inventory, 200)
            .unwrap();
        assert!(scan.sender_scores["deals@skim.example"].skimmed);
        let outcome = apply_after(&store, acct, &scan);
        assert!(
            outcome
                .cleared
                .contains(&(unopened, ClearReason::SkimmedSender))
        );
    }
    // A glanced-at promo REOPENED and read for a minute while the model
    // thinks: no new open stamp, but the dwell moved, so nothing goes.
    {
        let (store, acct) = store();
        let (glanced, unopened) = skimmed_sender(&store, acct);
        let scan = store
            .smart_clear_scan(acct, Utc::now(), &inventory, 200)
            .unwrap();
        store
            .record_message_attention(acct, glanced[0], 60_000, 0)
            .unwrap();
        let outcome = apply_after(&store, acct, &scan);
        assert!(outcome.cleared.is_empty(), "{:?}", outcome.cleared);
        assert_eq!(row(&store, unopened).0, "new");
    }
}

/// A shop's receipts glanced at are complete reads of receipts. They do not
/// make its promotions "skimmed".
#[test]
fn glances_at_a_senders_receipts_do_not_mark_its_promotions_skimmed() {
    let (store, acct) = store();
    let now = Utc::now();
    let ledger = (now - Duration::days(2)).to_rfc3339();
    store
        .set_app_setting(acct, OPEN_LEDGER_SINCE_KEY, &ledger)
        .unwrap();
    store
        .set_app_setting(acct, DWELL_LEDGER_SINCE_KEY, &ledger)
        .unwrap();
    for i in 0..3 {
        let id = mail(
            &store,
            acct,
            &format!("receipt{i}"),
            "orders@shop.example",
            2,
        );
        into_reading_with(&store, acct, id, |d| {
            d.kinds = vec![EmailKind::Receipt];
            d.reading_kind = None;
        });
        store
            .acknowledge_agent_message(acct, id, now - Duration::hours(30))
            .unwrap();
        store.record_message_attention(acct, id, 1_000, 0).unwrap();
    }
    let sale = mail(&store, acct, "sale", "orders@shop.example", 2);
    into_reading(&store, acct, sale);
    let outcome = apply(&store, acct, &[]);
    assert!(
        !outcome
            .cleared
            .iter()
            .any(|(_, r)| *r == ClearReason::SkimmedSender),
        "{:?}",
        outcome.cleared
    );
    assert_eq!(row(&store, sale).0, "new");
}

/// The rule suggester's digest hands SUBJECTS to a model. Auth and restricted
/// mail must not be in it, by the decision and not the retired column.
#[test]
fn the_rule_digest_leaves_out_auth_and_restricted_mail() {
    let (store, acct) = store();
    let plain = triaged(acct, "plain", "plain")
        .from(NEWS)
        .subject("Autumn issue")
        .seed(&store);
    decide(&store, acct, plain, |_| {});
    for (name, seal) in sealings() {
        let id = triaged(acct, name, name)
            .from(NEWS)
            .subject(&format!("Your code is 123456 ({name})"))
            .seed(&store);
        decide(&store, acct, id, seal);
        assert_eq!(sensitivity(&store, id), "normal");
    }
    let subjects: Vec<String> = store
        .sender_rule_digest(acct, NEWS, 10)
        .unwrap()
        .into_iter()
        .map(|r| r.subject)
        .collect();
    assert_eq!(subjects, ["Autumn issue"]);
}

// ---- each sealed clause, on its own ---------------------------------------

/// A decision that restricts access, on a row whose access STATE says
/// allowed: the decision clause alone has to catch it.
#[test]
fn a_restricting_decision_seals_even_when_the_state_says_allowed() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let open = mail(&store, acct, "open", NEWS, 1);
    into_reading(&store, acct, open);
    let id = mail(&store, acct, "restricted", NEWS, 2);
    decide(&store, acct, id, |d| d.external_access.restricted = true);
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_message_state SET access = 'allowed' WHERE message_id = ?1",
            [id],
        )
        .unwrap();
    store.record_message_attention(acct, id, 4_000, 1).unwrap();
    assert_eq!(stored_attention(&store, id), (0, 0));
    assert_eq!(sender(&senders(&store, acct), NEWS).signals.received, 1);
}

/// A person's "restrict this" correction, with the decision and the state
/// left saying otherwise: the correction clause alone has to catch it.
#[test]
fn a_restricting_correction_seals_on_its_own() {
    let (store, acct) = store();
    old_ledgers(&store, acct);
    let open = mail(&store, acct, "open", NEWS, 1);
    into_reading(&store, acct, open);
    let id = mail(&store, acct, "corrected", NEWS, 2);
    decide(&store, acct, id, |_| {});
    store
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO agent_triage_corrections
                 (account_id, message_id, field, value_json, source_revision, revision, updated_at)
             VALUES (?1, ?2, 'external_access', 'true', 1, 1, ?3)",
            params![acct, id, Utc::now().to_rfc3339()],
        )
        .unwrap();
    store.record_message_attention(acct, id, 4_000, 1).unwrap();
    assert_eq!(stored_attention(&store, id), (0, 0));
    assert_eq!(sender(&senders(&store, acct), NEWS).signals.received, 1);
}

/// The write statement refuses kept-close mail by itself, whatever the scan
/// in front of it concluded.
#[test]
fn the_done_statement_alone_refuses_kept_close_mail() {
    use super::super::smart_clear::MARK_DONE_SQL;
    let (store, acct) = store();
    let plain = mail(&store, acct, "plain", NEWS, 20);
    into_reading(&store, acct, plain);
    let code = mail(&store, acct, "code", NEWS, 20);
    into_reading_with(&store, acct, code, |d| d.auth.kinds = vec![AuthKind::Otp]);
    let conn = store.lock().unwrap();
    let mark = |id: i64| {
        conn.execute(
            MARK_DONE_SQL,
            params![Utc::now().to_rfc3339(), "smart_clear:1", acct, id],
        )
        .unwrap()
    };
    assert_eq!(mark(code), 0);
    assert_eq!(mark(plain), 1);
    assert_eq!(mark(plain), 0, "and never a row that is already done");
}
