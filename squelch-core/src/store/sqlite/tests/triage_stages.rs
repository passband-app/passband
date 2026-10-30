//! Stage-1 / Stage-2 / extract queue, budget and usage-ledger tests.

use super::super::*;
use super::support::*;
// The REAL marker the heuristic-only fallback stamps, not a local mirror of it.
use crate::types::{Sensitivity, Tier};

#[test]
fn extract_queue_selects_banking_only_excludes_others_sealed_and_receipts() {
    let (store, acct) = store();

    // banking_statement + transaction_alert -> queued.
    let stmt = triaged_row(acct, "g-stmt", "t1", None, false, Sensitivity::Normal).ingest(&store);
    apply_category(&store, acct, stmt, "banking_statement", false);
    let alert = triaged_row(acct, "g-alert", "t2", None, false, Sensitivity::Normal).ingest(&store);
    apply_category(&store, acct, alert, "transaction_alert", false);

    // invoice + general -> NOT queued (no extractor).
    let inv = triaged_row(acct, "g-inv", "t3", None, false, Sensitivity::Normal)
        .category("invoice")
        .ingest(&store);
    let genr = triaged_row(acct, "g-gen", "t4", None, false, Sensitivity::Normal).ingest(&store);
    apply_category(&store, acct, genr, "general", false);

    // A sealed row: category stays NULL (stage1_apply is guarded), excluded.
    let sealed = triaged_row(acct, "g-seal", "t5", None, false, Sensitivity::Sealed).ingest(&store);
    apply_category(&store, acct, sealed, "banking_statement", false); // no-op on sealed

    // A banking-categorized message that ALSO produced a receipt: excluded
    // (a receipt and a banking row must never double-create).
    let dual = triaged_row(acct, "g-dual", "t6", None, false, Sensitivity::Normal).ingest(&store);
    apply_category(&store, acct, dual, "banking_statement", false);
    store
        .upsert_receipt(
            acct,
            dual,
            "orders@shop.com",
            None,
            &crate::triage::ReceiptInfo {
                amount: Some(5.0),
                currency: Some("USD".into()),
            },
            Utc::now(),
        )
        .unwrap();

    let cats = ["banking_statement", "transaction_alert"];
    let q = store.extract_queue(acct, &cats, 20).unwrap();
    let ids: Vec<i64> = q.iter().map(|r| r.message_id).collect();
    assert_eq!(
        q.len(),
        2,
        "only the two banking rows without a receipt: {ids:?}"
    );
    assert!(ids.contains(&stmt));
    assert!(ids.contains(&alert));
    assert!(!ids.contains(&inv), "invoice has no extractor");
    assert!(!ids.contains(&genr), "general has no extractor");
    assert!(!ids.contains(&sealed), "sealed row carries a NULL category");
    assert!(!ids.contains(&dual), "receipt-bearing row is excluded");
}

#[test]
fn extract_bump_usage_records_its_own_ledger_category() {
    let (store, acct) = store();
    store
        .extract_bump_usage(
            acct,
            "2026-07-23",
            "extract_banking",
            UsageTokens {
                input: 500,
                output: 20,
                cache_creation: 700,
                cache_read: 3000,
            },
        )
        .unwrap();
    store
        .extract_bump_usage(
            acct,
            "2026-07-23",
            "extract_banking",
            UsageTokens {
                input: 300,
                output: 10,
                cache_creation: 300,
                cache_read: 1000,
            },
        )
        .unwrap();
    let conn = store.lock().unwrap();
    let (calls, in_tok, out_tok, cache_w, cache_r): (i64, i64, i64, i64, i64) = conn
        .query_row(
            "SELECT calls, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens
             FROM stage2_usage
             WHERE account_id=?1 AND day=?2 AND category='extract_banking'",
            params![acct, "2026-07-23"],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(in_tok, 800);
    assert_eq!(out_tok, 30);
    assert_eq!(cache_w, 1000);
    assert_eq!(cache_r, 4000);
}

// ---- the SHIPMENTS extractor's own queue --------------------------------

#[test]
fn ship_extract_queue_keeps_receipt_bearing_rows_and_drops_sealed_and_sent() {
    let (store, acct) = store();

    // A plain order confirmation with the loose signal -> pending.
    let plain = triaged_row(acct, "g-ship", "t1", None, false, Sensitivity::Normal)
        .ship_extract(true)
        .ingest(&store);

    // THE WHOLE REASON THIS QUEUE EXISTS: an order confirmation that also
    // produced a RECEIPT. `extract_queue` drops those, and most order
    // confirmations are exactly that shape, so it could never serve shipments.
    let with_receipt = triaged_row(acct, "g-both", "t2", None, false, Sensitivity::Normal)
        .ship_extract(true)
        .ingest(&store);
    store
        .upsert_receipt(
            acct,
            with_receipt,
            "orders@shop.com",
            None,
            &crate::triage::ReceiptInfo {
                amount: Some(42.0),
                currency: Some("USD".into()),
            },
            Utc::now(),
        )
        .unwrap();

    // No shipping signal at ingest -> NULL, never queued.
    let quiet = triaged_row(acct, "g-quiet", "t3", None, false, Sensitivity::Normal).ingest(&store);
    // Sealed and sent carry the signal flag but must never queue.
    let sealed = triaged_row(acct, "g-seal", "t4", None, false, Sensitivity::Sealed)
        .ship_extract(true)
        .ingest(&store);
    let sent = triaged_row(acct, "g-sent", "t5", None, false, Sensitivity::Normal)
        .is_sent(true)
        .ship_extract(true)
        .ingest(&store);

    let q = store.ship_extract_queue(acct, 20).unwrap();
    let ids: Vec<i64> = q.iter().map(|r| r.message_id).collect();
    assert_eq!(q.len(), 2, "the two signal-bearing normal rows: {ids:?}");
    assert!(ids.contains(&plain));
    assert!(
        ids.contains(&with_receipt),
        "a receipt-bearing order confirmation MUST still queue"
    );
    assert!(!ids.contains(&quiet), "no shipping signal, no queue");
    assert!(!ids.contains(&sealed), "sealed mail never reaches an LLM");
    assert!(!ids.contains(&sent), "the user's own outbox is not tracked");
    // Queued before Stage-1 ever ran, so the category is still NULL -> "".
    assert_eq!(q[0].category, "");
    assert_eq!(q[0].sensitivity, Sensitivity::Normal);
}

#[test]
fn ship_extract_mark_removes_the_row_from_the_queue() {
    let (store, acct) = store();
    let id = triaged_row(acct, "g-ship", "t1", None, false, Sensitivity::Normal)
        .ship_extract(true)
        .ingest(&store);
    assert_eq!(store.ship_extract_queue(acct, 10).unwrap().len(), 1);

    store
        .ship_extract_mark(acct, id, "claude-haiku-4-5")
        .unwrap();
    assert!(
        store.ship_extract_queue(acct, 10).unwrap().is_empty(),
        "a stamped marker takes the row out of the queue"
    );

    // A sealed row is guarded: the marker never lands on it.
    let sealed = triaged_row(acct, "g-seal", "t2", None, false, Sensitivity::Sealed).ingest(&store);
    store.ship_extract_mark(acct, sealed, "stale-skip").unwrap();
    let conn = store.lock().unwrap();
    let marker: Option<String> = conn
        .query_row(
            "SELECT ship_extract_model FROM triage WHERE message_id=?1",
            params![sealed],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(marker, None, "sensitivity guard holds on the mark");
}

#[test]
fn manual_retriage_preserves_decisions_records_and_user_state() {
    use crate::store::agent_triage::AgentTriageStore;
    let (store, account) = store();
    let id = triaged_row(
        account,
        "message",
        "thread",
        None,
        false,
        Sensitivity::Normal,
    )
    .ingest(&store);
    store
        .marketing_apply(&crate::store::MarketingApplied {
            message_id: id,
            account_id: account,
            brand: Some("Shop".into()),
            offer: Some("Offer".into()),
            discount: None,
            code: None,
            expires_at: None,
            received_at: Utc::now(),
            extractor_model_used: "old".into(),
        })
        .unwrap();
    store
        .set_attention_status(account, id, AttentionStatus::Done)
        .unwrap();
    let before: (String, Option<String>, Option<String>) = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT status,stage1_model_used,extractor_model_used FROM triage WHERE message_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(store.retriage_reset(account, Some(id), 7).unwrap(), 1);
    let after: (String, Option<String>, Option<String>) = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT status,stage1_model_used,extractor_model_used FROM triage WHERE message_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        before, after,
        "re-triage cannot erase user state or old facts"
    );
    assert_eq!(store.marketing_offers(account, 30, 10).unwrap().len(), 1);
    assert_eq!(store.retriage_progress(account).unwrap().done, 0);
    let mut found_manual = false;
    while let Some(job) = store
        .claim_agent_job(account, "triage", Utc::now(), 60)
        .unwrap()
    {
        if job.trigger.starts_with("manual:") {
            assert!(!job.arrival_eligible);
            found_manual = true;
        }
        store.complete_agent_job(&job).unwrap();
    }
    assert!(found_manual);
    assert_eq!(store.retriage_progress(account).unwrap().done, 1);
}

#[test]
fn repeated_manual_requests_create_new_durable_work() {
    use crate::store::agent_triage::AgentTriageStore;
    let (store, account) = store();
    let id = triaged_row(
        account,
        "message",
        "thread",
        None,
        false,
        Sensitivity::Normal,
    )
    .ingest(&store);
    store.retriage_reset(account, Some(id), 7).unwrap();
    while let Some(job) = store
        .claim_agent_job(account, "triage", Utc::now(), 60)
        .unwrap()
    {
        store.complete_agent_job(&job).unwrap();
    }
    assert_eq!(store.retriage_progress(account).unwrap().done, 1);
    store.retriage_reset(account, Some(id), 7).unwrap();
    assert_eq!(store.retriage_progress(account).unwrap().done, 0);
    assert!(
        store
            .claim_agent_job(account, "triage", Utc::now(), 60)
            .unwrap()
            .is_some()
    );
}

#[test]
fn manual_retriage_includes_restricted_mail_and_preserves_field_corrections() {
    use crate::store::agent_triage::AgentTriageStore;
    let (store, account) = store();
    let id =
        triaged_row(account, "auth", "thread", None, false, Sensitivity::Sealed).ingest(&store);
    store
        .correct_agent_triage(
            account,
            id,
            "external_access",
            &serde_json::json!(true),
            Utc::now(),
        )
        .unwrap();
    assert_eq!(store.retriage_reset(account, Some(id), 7).unwrap(), 1);
    assert!(!store.agent_access_allowed(account, id).unwrap());
    assert_eq!(
        store
            .agent_thread_context(account, "thread")
            .unwrap()
            .corrections
            .len(),
        1
    );
    assert_eq!(store.retriage_progress(account).unwrap().total, 1);
    assert_eq!(store.retriage_reset(account, Some(999999), 7).unwrap(), 0);
}

#[test]
fn manual_retriage_windows_bulk_requests_but_accepts_an_explicit_old_message() {
    let (store, account) = store();
    let old = triaged_row(account, "old", "old", None, false, Sensitivity::Normal)
        .received_at(Utc::now() - chrono::Duration::days(120))
        .ingest(&store);
    triaged_row(
        account,
        "recent",
        "recent",
        None,
        false,
        Sensitivity::Normal,
    )
    .ingest(&store);
    assert_eq!(store.retriage_reset(account, None, 7).unwrap(), 1);
    assert_eq!(store.retriage_reset(account, Some(old), 7).unwrap(), 1);
    assert_eq!(store.retriage_progress(account).unwrap().total, 2);
}

/// One settled message whose ingest work is done, so the only job left to
/// claim is whatever the test asks for next.
fn settled_message(store: &SqliteStore, account: AccountId) -> i64 {
    use crate::store::agent_triage::AgentTriageStore;
    let id = triaged_row(
        account,
        "message",
        "thread",
        None,
        false,
        Sensitivity::Normal,
    )
    .ingest(store);
    while let Some(job) = store
        .claim_agent_job(account, "investigation", Utc::now(), 60)
        .unwrap()
    {
        store.complete_agent_job(&job).unwrap();
    }
    id
}

fn manual_job_row(store: &SqliteStore, id: i64) -> (String, Option<String>, i64, i64) {
    store
        .lock()
        .unwrap()
        .query_row(
            "SELECT j.available_at,j.last_error,j.attempts,COALESCE(l.foreground,0)
             FROM agent_triage_jobs j LEFT JOIN agent_job_lanes l ON l.job_id=j.id
             WHERE j.message_id=?1 AND j.trigger LIKE 'manual:%'",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
}

#[test]
fn single_message_retriage_is_foreground_and_a_window_is_not() {
    use crate::store::agent_triage::AgentTriageStore;
    for single in [true, false] {
        let (store, account) = store();
        let id = settled_message(&store, account);
        store
            .retriage_reset(account, single.then_some(id), 7)
            .unwrap();
        let job = store
            .claim_agent_job(account, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert!(job.trigger.starts_with("manual:"));
        assert_eq!(
            job.foreground, single,
            "only a one-message request skips the background budget"
        );
    }
}

#[test]
fn reasking_for_one_message_unparks_a_budget_deferral_only() {
    use crate::store::agent_triage::AgentTriageStore;
    let later = Utc::now() + chrono::Duration::hours(6);
    for (error, unparks) in [
        ("daily_budget_exhausted", true),
        ("provider_cooldown", false),
    ] {
        let (store, account) = store();
        let id = settled_message(&store, account);
        // A window request runs in the background and gets parked.
        store.retriage_reset(account, None, 7).unwrap();
        let job = store
            .claim_agent_job(account, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        store.defer_agent_job(&job, later, error).unwrap();
        let parked = manual_job_row(&store, id);
        assert_eq!(parked.1.as_deref(), Some(error));
        store.retriage_reset(account, Some(id), 7).unwrap();
        let after = manual_job_row(&store, id);
        assert_eq!(after.2, parked.2, "{error}: attempts are never reset");
        assert_eq!(after.3, 1, "{error}: the re-ask is foreground work");
        let claimed = store
            .claim_agent_job(account, "investigation", Utc::now(), 60)
            .unwrap();
        if unparks {
            assert_eq!(after.1, None, "the budget deferral is cleared");
            assert!(after.0 < parked.0, "and it is claimable now");
            assert!(claimed.unwrap().foreground);
        } else {
            assert_eq!(after.0, parked.0, "{error}: genuine backoff is kept");
            assert_eq!(after.1.as_deref(), Some(error));
            assert!(claimed.is_none());
        }
    }
}

/// A click that lands while the message's job is LEASED is stored as a
/// follow-up. Whichever way the running job ends, the re-run it leaves behind
/// is the human's one-message request, so it must be foreground; a window
/// request's follow-up stays background.
#[test]
fn a_one_message_request_during_a_lease_keeps_its_foreground_lane() {
    use crate::store::agent_triage::AgentTriageStore;
    let later = Utc::now() + chrono::Duration::hours(6);
    for (single, ending) in [
        (true, "complete"),
        (false, "complete"),
        (true, "budget"),
        (false, "budget"),
        (true, "cooldown"),
    ] {
        let (store, account) = store();
        let id = settled_message(&store, account);
        // A window request runs in the background and is mid-flight.
        store.retriage_reset(account, None, 7).unwrap();
        let running = store
            .claim_agent_job(account, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert!(!running.foreground);
        // Distinct manual stamps: the request identity is the timestamp.
        std::thread::sleep(std::time::Duration::from_millis(2));
        store
            .retriage_reset(account, single.then_some(id), 7)
            .unwrap();
        match ending {
            "complete" => {
                store.complete_agent_job(&running).unwrap();
            }
            "budget" => {
                store
                    .defer_agent_job(&running, later, "daily_budget_exhausted")
                    .unwrap();
            }
            _ => {
                store
                    .defer_agent_job(&running, later, "provider_cooldown")
                    .unwrap();
            }
        }
        let row = manual_job_row(&store, id);
        assert_eq!(
            row.3 == 1,
            single,
            "{single}/{ending}: the re-run's lane follows the request"
        );
        let claimed = store
            .claim_agent_job(account, "investigation", Utc::now(), 60)
            .unwrap();
        match (single, ending) {
            (_, "complete") | (true, "budget") => {
                let job = claimed.expect("the re-run is claimable now");
                assert_eq!(job.foreground, single, "{single}/{ending}");
                assert!(job.trigger.starts_with("manual:"));
            }
            _ => assert!(
                claimed.is_none(),
                "{single}/{ending}: a window stays parked and genuine backoff is kept"
            ),
        }
    }
}

#[test]
fn retriage_progress_reports_budget_parked_jobs_and_when_they_resume() {
    use crate::store::agent_triage::AgentTriageStore;
    let (store, account) = store();
    let first = settled_message(&store, account);
    let second =
        triaged_row(account, "second", "other", None, false, Sensitivity::Normal).ingest(&store);
    while let Some(job) = store
        .claim_agent_job(account, "investigation", Utc::now(), 60)
        .unwrap()
    {
        store.complete_agent_job(&job).unwrap();
    }
    store.retriage_reset(account, None, 7).unwrap();
    let progress = store.retriage_progress(account).unwrap();
    assert_eq!((progress.total, progress.budget_parked), (2, 0));
    assert_eq!(progress.budget_resumes_at, None);

    let soon = Utc::now() + chrono::Duration::hours(2);
    let later = Utc::now() + chrono::Duration::hours(5);
    for when in [later, soon] {
        let job = store
            .claim_agent_job(account, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert!([first, second].contains(&job.message_id));
        store
            .defer_agent_job(&job, when, "daily_budget_exhausted")
            .unwrap();
    }
    let progress = store.retriage_progress(account).unwrap();
    assert_eq!(progress.budget_parked, 2);
    assert_eq!(
        progress.budget_resumes_at.map(|t| t.timestamp()),
        Some(soon.timestamp()),
        "the earliest resume time is the one worth showing"
    );
    let wire = serde_json::to_value(&progress).unwrap();
    assert_eq!(wire["budget_parked"], 2);
    assert!(wire["budget_resumes_at"].is_string());

    // A cooldown is not a budget pause, and a deferral that has come due is
    // just queued work again.
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_triage_jobs SET available_at=?2 WHERE message_id=?1",
            params![
                first,
                (Utc::now() - chrono::Duration::minutes(1)).to_rfc3339()
            ],
        )
        .unwrap();
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_triage_jobs SET last_error='provider_cooldown' WHERE message_id=?1",
            [second],
        )
        .unwrap();
    let progress = store.retriage_progress(account).unwrap();
    assert_eq!(progress.budget_parked, 0);
    assert_eq!(progress.budget_resumes_at, None);
}

#[test]
fn retriage_progress_expires_old_requests_without_touching_their_jobs() {
    let (store, account) = store();
    let id = triaged_row(
        account,
        "message",
        "thread",
        None,
        false,
        Sensitivity::Normal,
    )
    .ingest(&store);
    store.retriage_reset(account, Some(id), 7).unwrap();
    assert_eq!(store.retriage_progress(account).unwrap().total, 1);
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE triage SET retriage_at=?2 WHERE message_id=?1",
            params![id, (Utc::now() - chrono::Duration::hours(25)).to_rfc3339()],
        )
        .unwrap();
    assert_eq!(store.retriage_progress(account).unwrap().total, 0);
    let count:i64=store.lock().unwrap().query_row("SELECT COUNT(*) FROM agent_triage_jobs WHERE account_id=?1 AND trigger LIKE 'manual:%'",[account],|r|r.get(0)).unwrap();
    assert_eq!(count, 1, "progress visibility does not delete durable work");
}

#[test]
fn stage1_usage_ledger_is_a_separate_category() {
    let (store, acct) = store();
    store
        .stage1_bump_usage(
            acct,
            "2026-07-09",
            UsageTokens {
                input: 100,
                output: 20,
                cache_creation: 40,
                cache_read: 900,
            },
        )
        .unwrap();
    store
        .stage2_bump_usage(
            acct,
            "2026-07-09",
            UsageTokens {
                input: 500,
                output: 90,
                ..Default::default()
            },
        )
        .unwrap();

    let s1 = store.stage1_usage_since(acct, "2026-07-01").unwrap();
    assert_eq!(s1.calls, 1);
    assert_eq!(s1.input_tokens, 100);
    assert_eq!(s1.output_tokens, 20);
    assert_eq!(s1.cache_creation_tokens, 40);
    assert_eq!(s1.cache_read_tokens, 900);
    let s2 = store.stage2_usage_since(acct, "2026-07-01").unwrap();
    assert_eq!(s2.calls, 1);
    assert_eq!(s2.input_tokens, 500);

    let rows1 = store.list_usage_stage1(acct, 30).unwrap();
    assert_eq!(rows1.len(), 1);
    assert_eq!(rows1[0].input_tokens, 100);
    assert_eq!(rows1[0].cache_creation_tokens, 40);
    assert_eq!(rows1[0].cache_read_tokens, 900);
    // The stage-2 list is unaffected by the stage-1 row.
    let rows2 = store.list_usage(acct, 30).unwrap();
    assert_eq!(rows2.len(), 1);
    assert_eq!(rows2[0].input_tokens, 500);
}

#[test]
fn list_usage_by_category_surfaces_extractors_nobody_named() {
    let (store, acct) = store();
    store
        .stage1_bump_usage(
            acct,
            "2026-07-09",
            UsageTokens {
                input: 100,
                output: 20,
                ..Default::default()
            },
        )
        .unwrap();
    store
        .stage2_bump_usage(
            acct,
            "2026-07-09",
            UsageTokens {
                input: 500,
                output: 90,
                ..Default::default()
            },
        )
        .unwrap();
    // An extractor category, and a category invented right here: the point of
    // enumerating is that a ledger writer added LATER still reports, without
    // anyone editing the reader.
    store
        .extract_bump_usage(
            acct,
            "2026-07-09",
            "extract_banking",
            UsageTokens {
                input: 40,
                output: 8,
                ..Default::default()
            },
        )
        .unwrap();
    store
        .extract_bump_usage(
            acct,
            "2026-07-10",
            "extract_something_new",
            UsageTokens {
                input: 7,
                output: 3,
                ..Default::default()
            },
        )
        .unwrap();

    let all = store.list_usage_by_category(acct, 30).unwrap();
    let names: Vec<&str> = all.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "extract_banking",
            "extract_something_new",
            "stage1",
            "stage2"
        ],
        "every ledger category, sorted by name"
    );

    let banking = &all.iter().find(|(c, _)| c == "extract_banking").unwrap().1;
    assert_eq!(banking.len(), 1);
    assert_eq!(banking[0].input_tokens, 40);
    assert_eq!(banking[0].output_tokens, 8);
    // Categories stay isolated — the extractor rows do not leak into stage-1.
    let s1 = &all.iter().find(|(c, _)| c == "stage1").unwrap().1;
    assert_eq!(s1[0].input_tokens, 100);
}

#[test]
fn stage2_budget_increment_and_exhaustion() {
    let (store, acct) = store();
    let day = "2026-07-09";

    assert_eq!(store.stage2_budget_used(acct, "t-abc", day).unwrap(), 0);
    assert_eq!(
        store.stage2_increment_budget(acct, "t-abc", day).unwrap(),
        1
    );
    assert_eq!(
        store.stage2_increment_budget(acct, "t-abc", day).unwrap(),
        2
    );
    assert_eq!(store.stage2_budget_used(acct, "t-abc", day).unwrap(), 2);

    // A different thread and a different day are independent counters.
    assert_eq!(store.stage2_budget_used(acct, "t-other", day).unwrap(), 0);
    assert_eq!(
        store
            .stage2_budget_used(acct, "t-abc", "2026-07-10")
            .unwrap(),
        0
    );

    // The global sentinel is a separate scope in the same table.
    assert_eq!(
        store
            .stage2_increment_budget(acct, "__global__", day)
            .unwrap(),
        1
    );
    assert_eq!(
        store.stage2_budget_used(acct, "__global__", day).unwrap(),
        1
    );
    // The per-thread counter is unaffected by the global increment.
    assert_eq!(store.stage2_budget_used(acct, "t-abc", day).unwrap(), 2);
}

#[test]
fn mailing_list_storm_capped_at_thread_daily_cap() {
    // A mailing-list storm — 30 messages in ONE thread — must cost AT MOST
    // `thread_daily_cap` API calls. Models the check-BEFORE-increment
    // discipline stage2_pass runs per row, with the global cap set high so
    // the per-thread cap binds.
    let (store, acct) = store();
    let day = "2026-07-09";
    let thread = "t-listserv";
    let thread_daily_cap: u32 = 3; // matches Stage2Config default

    let mut calls = 0u32;
    for _ in 0..30 {
        let used = store.stage2_budget_used(acct, thread, day).unwrap();
        if used >= thread_daily_cap {
            continue; // capped: row stays queued, no call
        }
        // "Make the call": increment BEFORE the attempt.
        store.stage2_increment_budget(acct, thread, day).unwrap();
        calls += 1;
    }

    assert_eq!(
        calls, thread_daily_cap,
        "30-message storm on one thread must cost at most thread_daily_cap calls"
    );
    assert_eq!(
        store.stage2_budget_used(acct, thread, day).unwrap(),
        thread_daily_cap,
        "counter must not exceed the cap"
    );
}

#[test]
fn one_sender_across_many_threads_capped_at_sender_daily_cap() {
    // A chatty sender fanning 10 messages across 10 DIFFERENT threads must
    // cost AT MOST `sender_daily_cap` calls. Models the per-sender
    // check-BEFORE-increment the pass runs (keyed by sender:<addr>), with the
    // per-thread and global caps set high so the per-sender cap binds.
    let (store, acct) = store();
    let day = "2026-07-09";
    let sender_key = "sender:chatty@example.com";
    let sender_daily_cap: u32 = 5; // matches Stage2Config default

    let mut calls = 0u32;
    for i in 0..10 {
        // Each message is in its OWN thread — the per-thread cap never binds.
        let _thread = format!("t-{i}");
        let used = store.stage2_budget_used(acct, sender_key, day).unwrap();
        if used >= sender_daily_cap {
            continue; // sender capped: row stays queued, no call
        }
        store
            .stage2_increment_budget(acct, sender_key, day)
            .unwrap();
        calls += 1;
    }

    assert_eq!(
        calls, sender_daily_cap,
        "10 messages from one sender across 10 threads cost at most sender_daily_cap"
    );
    assert_eq!(
        store.stage2_budget_used(acct, sender_key, day).unwrap(),
        sender_daily_cap
    );
}

#[test]
fn stage2_usage_ledger_bumps_and_reads() {
    // Bumping the ledger accumulates calls + tokens per day, and reading
    // returns the running totals (zeroed for an untouched day).
    let (store, acct) = store();
    let day = "2026-07-09";

    // Untouched day reads as zeros.
    let z = store.stage2_usage_today(acct, day).unwrap();
    assert_eq!(z, Stage2Usage::default());

    store
        .stage2_bump_usage(
            acct,
            day,
            UsageTokens {
                input: 1200,
                output: 60,
                cache_creation: 500,
                cache_read: 4000,
            },
        )
        .unwrap();
    store
        .stage2_bump_usage(
            acct,
            day,
            UsageTokens {
                input: 800,
                output: 40,
                cache_creation: 100,
                cache_read: 2000,
            },
        )
        .unwrap();
    let u = store.stage2_usage_today(acct, day).unwrap();
    assert_eq!(u.calls, 2);
    assert_eq!(u.input_tokens, 2000);
    assert_eq!(u.output_tokens, 100);
    assert_eq!(u.cache_creation_tokens, 600);
    assert_eq!(u.cache_read_tokens, 6000);

    // A different day is an independent row.
    assert_eq!(
        store.stage2_usage_today(acct, "2026-07-10").unwrap(),
        Stage2Usage::default()
    );
}

#[test]
fn list_usage_returns_recent_days_newest_first() {
    let (store, acct) = store();

    // Empty ledger => no rows.
    assert!(store.list_usage(acct, 30).unwrap().is_empty());

    store
        .stage2_bump_usage(
            acct,
            "2026-07-07",
            UsageTokens {
                input: 100,
                output: 10,
                ..Default::default()
            },
        )
        .unwrap();
    store
        .stage2_bump_usage(
            acct,
            "2026-07-08",
            UsageTokens {
                input: 200,
                output: 20,
                ..Default::default()
            },
        )
        .unwrap();
    store
        .stage2_bump_usage(
            acct,
            "2026-07-09",
            UsageTokens {
                input: 300,
                output: 30,
                ..Default::default()
            },
        )
        .unwrap();
    store
        .stage2_bump_usage(
            acct,
            "2026-07-09",
            UsageTokens {
                input: 100,
                output: 10,
                ..Default::default()
            },
        )
        .unwrap();

    // Newest-first, sparse (only days with a row).
    let rows = store.list_usage(acct, 30).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].day, "2026-07-09");
    assert_eq!(rows[0].calls, 2);
    assert_eq!(rows[0].input_tokens, 400);
    assert_eq!(rows[0].output_tokens, 40);
    assert_eq!(rows[2].day, "2026-07-07");

    // `days` caps the row count (still newest-first).
    let capped = store.list_usage(acct, 2).unwrap();
    assert_eq!(capped.len(), 2);
    assert_eq!(capped[0].day, "2026-07-09");
    assert_eq!(capped[1].day, "2026-07-08");
}

#[test]
fn app_settings_get_set_roundtrip_and_scoping() {
    let store = SqliteStore::open_in_memory().unwrap();
    let a = store.ensure_account("a@example.com").unwrap();
    let b = store.ensure_account("b@example.com").unwrap();

    // Unset key reads None.
    assert!(store.get_app_setting(a, "k").unwrap().is_none());

    // Set, read back, and overwrite (upsert).
    store.set_app_setting(a, "k", "v1").unwrap();
    assert_eq!(
        store.get_app_setting(a, "k").unwrap().as_deref(),
        Some("v1")
    );
    store.set_app_setting(a, "k", "v2").unwrap();
    assert_eq!(
        store.get_app_setting(a, "k").unwrap().as_deref(),
        Some("v2")
    );

    // Per-account scoped: b's key is independent.
    assert!(store.get_app_setting(b, "k").unwrap().is_none());
}

#[test]
fn stage2_cap_overrides_reads_and_precedence() {
    use crate::config::{
        APP_SETTING_GLOBAL_DAILY_CAP, APP_SETTING_SENDER_DAILY_CAP, APP_SETTING_THREAD_DAILY_CAP,
    };
    let (store, acct) = store();

    // No rows => all None (caller falls back to config/env then default).
    assert_eq!(
        store.stage2_cap_overrides(acct).unwrap(),
        Default::default()
    );

    // A set thread cap surfaces; the others stay None (so the effective cap
    // is the override where present, config/default elsewhere — precedence).
    store
        .set_app_setting(acct, APP_SETTING_THREAD_DAILY_CAP, "5")
        .unwrap();
    let o = store.stage2_cap_overrides(acct).unwrap();
    assert_eq!(o.thread_daily_cap, Some(5));
    assert_eq!(o.sender_daily_cap, None);
    assert_eq!(o.global_daily_cap, None);

    // Set the remaining two.
    store
        .set_app_setting(acct, APP_SETTING_SENDER_DAILY_CAP, "9")
        .unwrap();
    store
        .set_app_setting(acct, APP_SETTING_GLOBAL_DAILY_CAP, "300")
        .unwrap();
    let o = store.stage2_cap_overrides(acct).unwrap();
    assert_eq!(o.thread_daily_cap, Some(5));
    assert_eq!(o.sender_daily_cap, Some(9));
    assert_eq!(o.global_daily_cap, Some(300));

    // A malformed OR out-of-range stored value is ignored (treated as absent),
    // so a corrupt row can never remove the cap entirely.
    store
        .set_app_setting(acct, APP_SETTING_THREAD_DAILY_CAP, "not-a-number")
        .unwrap();
    assert_eq!(
        store.stage2_cap_overrides(acct).unwrap().thread_daily_cap,
        None
    );
    store
        .set_app_setting(acct, APP_SETTING_THREAD_DAILY_CAP, "0")
        .unwrap();
    assert_eq!(
        store.stage2_cap_overrides(acct).unwrap().thread_daily_cap,
        None
    );
    store
        .set_app_setting(acct, APP_SETTING_THREAD_DAILY_CAP, "100001")
        .unwrap();
    assert_eq!(
        store.stage2_cap_overrides(acct).unwrap().thread_daily_cap,
        None
    );
}

#[test]
fn override_cap_binds_below_config_default() {
    // Precedence is override > config/env > default: a runtime override of 1
    // caps a thread the config default (3) would have allowed 3 calls on.
    use crate::config::APP_SETTING_THREAD_DAILY_CAP;
    let (store, acct) = store();
    let day = "2026-07-09";
    let thread = "t-override";
    let config_default_cap: u32 = 3; // Stage2Config default

    // Client lowers the per-thread cap to 1 at runtime.
    store
        .set_app_setting(acct, APP_SETTING_THREAD_DAILY_CAP, "1")
        .unwrap();

    // Effective cap = override (1), NOT the config default (3) — precedence.
    let overrides = store.stage2_cap_overrides(acct).unwrap();
    let effective = overrides.thread_daily_cap.unwrap_or(config_default_cap);
    assert_eq!(effective, 1);

    // Same check-before-increment loop the pass runs, using the effective cap.
    let mut calls = 0u32;
    for _ in 0..10 {
        let used = store.stage2_budget_used(acct, thread, day).unwrap();
        if used >= effective {
            continue;
        }
        store.stage2_increment_budget(acct, thread, day).unwrap();
        calls += 1;
    }
    assert_eq!(
        calls, 1,
        "override cap of 1 must bind below the config default of 3"
    );
}

#[test]
fn stage2_usage_since_sums_window_inclusively() {
    let (store, acct) = store();

    // Empty ledger => zeros.
    assert_eq!(
        store.stage2_usage_since(acct, "2026-07-01").unwrap(),
        Stage2Usage::default()
    );

    store
        .stage2_bump_usage(
            acct,
            "2026-07-05",
            UsageTokens {
                input: 100,
                output: 10,
                ..Default::default()
            },
        )
        .unwrap();
    store
        .stage2_bump_usage(
            acct,
            "2026-07-08",
            UsageTokens {
                input: 200,
                output: 20,
                ..Default::default()
            },
        )
        .unwrap();
    store
        .stage2_bump_usage(
            acct,
            "2026-07-08",
            UsageTokens {
                input: 300,
                output: 30,
                ..Default::default()
            },
        )
        .unwrap();

    // since_day <= earliest => everything summed (2 days, 3 calls).
    let all = store.stage2_usage_since(acct, "2026-07-05").unwrap();
    assert_eq!(all.calls, 3);
    assert_eq!(all.input_tokens, 600);
    assert_eq!(all.output_tokens, 60);

    // Window boundary is inclusive on since_day and excludes older rows.
    let recent = store.stage2_usage_since(acct, "2026-07-08").unwrap();
    assert_eq!(recent.calls, 2);
    assert_eq!(recent.input_tokens, 500);
    assert_eq!(recent.output_tokens, 50);
}

#[test]
fn count_inbound_since_counts_only_received_in_window() {
    let (store, acct) = store();
    let now = Utc::now();

    let inbound = |gmail: &str, sent: bool, received: DateTime<Utc>| {
        let m = NewMessage {
            account_id: acct,
            gmail_msg_id: gmail.to_string(),
            thread_id: gmail.to_string(),
            from_addr: "x@y.com".to_string(),
            from_name: None,
            subject: "s".to_string(),
            received_at: received,
            snippet: String::new(),
            body: String::new(),
            body_html: None,
            is_sent: sent,
            is_spam: false,
            to_addrs: None,
            cc_addrs: None,
            bcc_addrs: None,
            list_unsubscribe: None,
            list_unsub_one_click: false,
            auth_pass: None,
            sender_signals: None,
        };
        store.upsert_message(&m).unwrap();
    };

    // Two recent inbound, one old inbound, one recent SENT (excluded).
    inbound("m1", false, now - chrono::Duration::days(1));
    inbound("m2", false, now - chrono::Duration::days(10));
    inbound("m3", false, now - chrono::Duration::days(30));
    inbound("m4", true, now - chrono::Duration::days(1));

    let since = now - chrono::Duration::days(14);
    assert_eq!(store.count_inbound_since(acct, since).unwrap(), 2);
}

#[test]
fn stage1_apply_reports_false_when_the_row_was_sealed_mid_pass() {
    use crate::triage::DeadlineHit;
    // TOCTOU: a human seals a row between the Stage-1 queue SELECT and its
    // apply. The guarded UPDATE matches nothing, and the apply must say so or
    // the engine emits a notification event for a now-sealed message.
    let (store, acct) = store();
    let id = seed_triage_row(&store, acct, "g-race", "t1", Sensitivity::Normal);
    let applied = Stage1Applied {
        message_id: id,
        account_id: acct,
        importance: 90,
        tier: Tier::PastDue,
        one_line: "loud verdict".into(),
        reason: "stage-1".into(),
        field_reasons: crate::types::FieldReasons::default(),
        stage1_model_used: "claude-haiku-4-5".into(),
        needs_stage2: false,
        escalation_reason: None,
        deadline: Some(DeadlineHit {
            kind: "bill".into(),
            amount: Some(10.0),
            currency: Some("USD".into()),
            due_at: Utc::now() - chrono::Duration::days(1),
            past_due: true,
            source: "stage1".into(),
        }),
        category: None,
    };

    // Sealed between queue and apply.
    store
        .correct_triage(
            acct,
            id,
            TriageAxis::Sensitivity,
            "sealed",
            None,
            Utc::now(),
        )
        .unwrap()
        .unwrap();

    assert!(
        !store.stage1_apply(&applied).unwrap(),
        "sealed mid-pass: apply reports false"
    );
    assert!(
        store.deadlines(acct, Some(365)).unwrap().is_empty(),
        "no deadline row"
    );

    // Control: the same apply on a live row reports true.
    let live = seed_triage_row(&store, acct, "g-live", "t2", Sensitivity::Normal);
    let mut ok = applied.clone();
    ok.message_id = live;
    assert!(
        store.stage1_apply(&ok).unwrap(),
        "normal row: apply reports true"
    );
}

/// A refund gives back exactly one charge, and never digs below zero.
///
/// The floor is the interesting half. The budget key is the UTC day, so a call
/// charged at 23:59:59 and refunded two seconds later refunds against the NEXT
/// day's row, which holds nothing. Without the floor that row would go
/// negative and hand the new day a free call on top of its own cap.
#[test]
fn a_config_failure_refund_gives_back_one_charge_and_floors_at_zero() {
    let (store, acct) = store();
    let day = "2026-08-25";

    store
        .stage2_increment_budget(acct, "__stage1_global__", day)
        .unwrap();
    store
        .stage2_increment_budget(acct, "__stage1_global__", day)
        .unwrap();
    assert_eq!(
        store
            .stage2_budget_used(acct, "__stage1_global__", day)
            .unwrap(),
        2
    );

    store
        .stage2_refund_budget(acct, "__stage1_global__", day)
        .unwrap();
    assert_eq!(
        store
            .stage2_budget_used(acct, "__stage1_global__", day)
            .unwrap(),
        1
    );

    // Refunding past zero is a no-op, not a negative balance.
    store
        .stage2_refund_budget(acct, "__stage1_global__", day)
        .unwrap();
    store
        .stage2_refund_budget(acct, "__stage1_global__", day)
        .unwrap();
    assert_eq!(
        store
            .stage2_budget_used(acct, "__stage1_global__", day)
            .unwrap(),
        0
    );

    // A refund against a day that was never charged creates nothing.
    store
        .stage2_refund_budget(acct, "__stage1_global__", "2026-08-26")
        .unwrap();
    assert_eq!(
        store
            .stage2_budget_used(acct, "__stage1_global__", "2026-08-26")
            .unwrap(),
        0
    );

    // Scopes stay independent: refunding one key leaves the others alone.
    store.stage2_increment_budget(acct, "t-abc", day).unwrap();
    store
        .stage2_refund_budget(acct, "__stage1_global__", day)
        .unwrap();
    assert_eq!(store.stage2_budget_used(acct, "t-abc", day).unwrap(), 1);
}

/// THE OUTAGE THIS EXISTS FOR, in miniature: a run of config-level failures
/// must not leave the cap spent once the config is fixed.
///
/// On 2026-08-25 a gateway misconfiguration charged 498 of a 500-call daily cap
/// in about half an hour. Because the budget key is the UTC day and the charge
/// stood, the tenant stayed capped for 22 hours after the gateway was repaired.
#[test]
fn a_run_of_config_failures_leaves_the_cap_intact() {
    let (store, acct) = store();
    let day = "2026-08-25";
    let cap = 500u32;

    // Every attempt charges before the call (the retry-storm guard), and every
    // one comes back a config-level 4xx, so every one is refunded.
    for _ in 0..498 {
        store
            .stage2_increment_budget(acct, "__stage1_global__", day)
            .unwrap();
        store
            .stage2_refund_budget(acct, "__stage1_global__", day)
            .unwrap();
    }

    let used = store
        .stage2_budget_used(acct, "__stage1_global__", day)
        .unwrap();
    assert_eq!(used, 0, "config failures must not spend the day's cap");
    assert!(
        used < cap,
        "the fleet can still triage once the config is fixed"
    );
}

#[test]
fn retriage_progress_uses_covering_range_and_trigger_lookups() {
    let (store, account) = store();
    let conn = store.lock().unwrap();
    let sql = format!(
        "EXPLAIN QUERY PLAN {}",
        super::super::triage_stages::RETRIAGE_PROGRESS_SQL
    );
    let plan: Vec<String> = conn
        .prepare(&sql)
        .unwrap()
        .query_map(
            params![account, Utc::now().to_rfc3339(), Utc::now().to_rfc3339()],
            |row| row.get(3),
        )
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    let plan = plan.join("\n");
    assert!(
        plan.contains("SEARCH t USING COVERING INDEX idx_triage_retriage_window"),
        "{plan}"
    );
    assert!(plan.contains("retriage_at>?"), "{plan}");
    assert!(
        plan.contains("SEARCH j USING COVERING INDEX idx_agent_jobs_retriage_progress"),
        "{plan}"
    );
    assert!(plan.contains("trigger=?"), "{plan}");
    assert!(!plan.contains("SCAN"), "{plan}");
}
