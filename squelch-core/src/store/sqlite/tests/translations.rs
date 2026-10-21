//! `message_translations`: what ingest queues, what a claim takes, and how a
//! result, a failure and an archive open land.

use super::super::*;
use super::support::*;
use crate::config::Stage1Config;
use crate::store::{TranslationStatus, TranslationWrite};
use crate::sync::ingest::{RawFetched, ingest};
use crate::translate::Translated;
use chrono::Duration;

const FRENCH_BODY: &str = "Bonjour Marie, votre facture du mois d'octobre est disponible dans votre espace client. Merci de votre confiance.";

fn row(store: &SqliteStore, id: i64) -> Option<(String, String, i64)> {
    store
        .lock()
        .unwrap()
        .query_row(
            "SELECT source_lang, status, attempts FROM message_translations WHERE message_id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .unwrap()
}

fn raw(acct: AccountId, id: &str, subject: &str, body: &str, sent: bool, spam: bool) -> RawFetched {
    let eml = format!(
        "From: Marie <marie@example.fr>\r\nTo: me@example.com\r\nSubject: {subject}\r\n\
         Date: Mon, 19 Oct 2026 10:00:00 +0000\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
    );
    RawFetched {
        account_id: acct,
        gmail_msg_id: id.to_string(),
        gmail_thread_id: Some(format!("thr-{id}")),
        raw: eml.into_bytes(),
        internal_date: Some(Utc::now()),
        is_sent: sent,
        is_spam: spam,
        account_addr: "me@example.com".to_string(),
    }
}

#[test]
fn ingest_queues_french_and_nothing_for_english_spam_or_sent() {
    let (store, acct) = store();
    let land = |id: &str, subject: &str, body: &str, sent: bool, spam: bool| {
        let t = ingest(
            &raw(acct, id, subject, body, sent, spam),
            &Stage1Config::default(),
            Utc::now(),
            |_| false,
        );
        store.ingest_message(&t).unwrap()
    };
    let fr = land("fr", "Votre facture d'octobre", FRENCH_BODY, false, false);
    assert_eq!(row(&store, fr), Some(("fr".into(), "pending".into(), 0)));

    let en = land(
        "en",
        "Your October invoice",
        "Hi Marie, your invoice for October is available in your customer area. Thanks for your trust.",
        false,
        false,
    );
    assert_eq!(row(&store, en), None);
    let spam = land("spam", "Votre facture d'octobre", FRENCH_BODY, false, true);
    assert_eq!(row(&store, spam), None);
    let sent = land("sent", "Votre facture d'octobre", FRENCH_BODY, true, false);
    assert_eq!(row(&store, sent), None);
}

#[test]
fn the_store_refuses_spam_and_sent_whatever_the_caller_carried() {
    let (store, acct) = store();
    let spam = triaged(acct, "g1", "t1")
        .lang("fr")
        .is_spam(true)
        .ingest(&store);
    let sent = triaged(acct, "g2", "t2")
        .lang("fr")
        .is_sent(true)
        .ingest(&store);
    assert_eq!(row(&store, spam), None);
    assert_eq!(row(&store, sent), None);
}

#[test]
fn archive_rows_are_deferred_until_the_thread_is_opened() {
    let (store, acct) = store();
    let a = store
        .ingest_archive_message(&triaged(acct, "g1", "t1").lang("de").build())
        .unwrap()
        .unwrap();
    let b = store
        .ingest_archive_message(&triaged(acct, "g2", "t2").lang("de").build())
        .unwrap()
        .unwrap();
    assert_eq!(row(&store, a).unwrap().1, "deferred");
    let now = Utc::now();
    assert!(store.claim_translations(acct, now, 10).unwrap().is_empty());

    assert_eq!(
        store
            .promote_deferred_translations(acct, "t1", now)
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .promote_deferred_translations(acct, "t1", now)
            .unwrap(),
        0
    );
    assert_eq!(row(&store, a).unwrap().1, "pending");
    assert_eq!(row(&store, b).unwrap().1, "deferred");
    let jobs = store.claim_translations(acct, now, 10).unwrap();
    assert_eq!(
        jobs.iter().map(|j| j.message_id).collect::<Vec<_>>(),
        vec![a]
    );
}

#[test]
fn a_reingest_keeps_a_paid_translation() {
    let (store, acct) = store();
    let b = triaged(acct, "g1", "t1").lang("fr").body(FRENCH_BODY);
    let id = b.ingest(&store);
    let now = Utc::now();
    store.claim_translations(acct, now, 10).unwrap();
    let t = Translated {
        subject: "Lunch?".into(),
        body: "Hello".into(),
        body_html: None,
        snippet: "Hello".into(),
        missing_facts: vec![],
    };
    store
        .record_translation(
            id,
            &TranslationWrite::Translated {
                translated: &t,
                model: "claude-sonnet-5",
                prompt_version: 1,
            },
            now,
        )
        .unwrap();
    assert_eq!(b.ingest(&store), id);
    assert_eq!(row(&store, id).unwrap().1, "done");
}

#[test]
fn claim_write_and_read_round_trip() {
    let (store, acct) = store();
    let old = triaged(acct, "g1", "t1")
        .lang("fr")
        .subject("Votre facture")
        .body(FRENCH_BODY)
        .received_at(Utc::now() - Duration::days(1))
        .ingest(&store);
    let new = triaged(acct, "g2", "t2").lang("es").ingest(&store);
    let now = Utc::now();

    let jobs = store.claim_translations(acct, now, 10).unwrap();
    // Newest mail first, with the ORIGINAL text.
    assert_eq!(
        jobs.iter().map(|j| j.message_id).collect::<Vec<_>>(),
        vec![new, old]
    );
    assert_eq!(jobs[1].subject, "Votre facture");
    assert_eq!(jobs[1].body, FRENCH_BODY);
    assert_eq!(jobs[1].source_lang, "fr");
    assert_eq!(jobs[1].attempts, 1);
    // Leased: a second pass right now takes nothing.
    assert!(store.claim_translations(acct, now, 10).unwrap().is_empty());

    let done = Translated {
        subject: "Your invoice".into(),
        body: "Hello Marie".into(),
        body_html: Some("<p>Hello Marie</p>".into()),
        snippet: "Hello Marie".into(),
        missing_facts: vec![],
    };
    let partial = Translated {
        missing_facts: vec!["123456".into()],
        ..done.clone()
    };
    for (id, t) in [(old, &done), (new, &partial)] {
        store
            .record_translation(
                id,
                &TranslationWrite::Translated {
                    translated: t,
                    model: "claude-sonnet-5",
                    prompt_version: 1,
                },
                now,
            )
            .unwrap();
    }
    let rows = store.message_translations(acct, &[old, new, 999]).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].message_id, old);
    assert_eq!(rows[0].status, TranslationStatus::Done);
    assert_eq!(rows[0].subject.as_deref(), Some("Your invoice"));
    assert_eq!(rows[0].body_html.as_deref(), Some("<p>Hello Marie</p>"));
    assert_eq!(rows[0].model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(rows[0].prompt_version, Some(1));
    assert!(rows[0].missing_facts.is_empty());
    assert_eq!(rows[1].status, TranslationStatus::Incomplete);
    assert_eq!(rows[1].missing_facts, vec!["123456".to_string()]);
    // Another account sees none of it.
    let other = store.ensure_account("other@example.com").unwrap();
    assert!(
        store
            .message_translations(other, &[old, new])
            .unwrap()
            .is_empty()
    );
    // Finished rows are never claimed again.
    assert!(
        store
            .claim_translations(acct, now + Duration::days(30), 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn failures_back_off_and_run_out() {
    let (store, acct) = store();
    let id = triaged(acct, "g1", "t1").lang("ja").ingest(&store);
    let mut now = Utc::now();
    let mut claims = 0;
    // Five attempts, each failing; the backoff doubles from ten minutes.
    for attempt in 1..=5 {
        let jobs = store.claim_translations(acct, now, 10).unwrap();
        assert_eq!(jobs.len(), 1, "attempt {attempt}");
        assert_eq!(jobs[0].attempts, attempt);
        claims += 1;
        store
            .record_translation(id, &TranslationWrite::Failed { model: "m" }, now)
            .unwrap();
        assert_eq!(row(&store, id).unwrap().1, "failed");
        let wait = 600 * (1 << (attempt - 1));
        assert!(
            store
                .claim_translations(acct, now + Duration::seconds(wait - 1), 10)
                .unwrap()
                .is_empty(),
            "too early after attempt {attempt}"
        );
        now += Duration::seconds(wait);
    }
    assert_eq!(claims, 5);
    assert!(
        store
            .claim_translations(acct, now + Duration::days(30), 10)
            .unwrap()
            .is_empty(),
        "out of attempts"
    );
    assert_eq!(row(&store, id).unwrap().2, 5);
}

#[test]
fn a_claim_that_never_came_back_is_taken_again_after_the_lease() {
    let (store, acct) = store();
    triaged(acct, "g1", "t1").lang("fr").ingest(&store);
    let now = Utc::now();
    assert_eq!(store.claim_translations(acct, now, 10).unwrap().len(), 1);
    assert!(
        store
            .claim_translations(acct, now + Duration::minutes(14), 10)
            .unwrap()
            .is_empty()
    );
    let again = store
        .claim_translations(acct, now + Duration::minutes(15), 10)
        .unwrap();
    assert_eq!(again[0].attempts, 2);
}

#[test]
fn a_message_later_marked_spam_is_not_claimed() {
    let (store, acct) = store();
    let id = triaged(acct, "g1", "t1").lang("fr").ingest(&store);
    store.execute_batch_for_test(&format!("UPDATE messages SET is_spam = 1 WHERE id = {id}"));
    assert!(
        store
            .claim_translations(acct, Utc::now(), 10)
            .unwrap()
            .is_empty()
    );
}
