//! `message_translations`: what ingest queues, what a claim takes, and how a
//! result, a failure and an archive open land.

use super::super::*;
use super::support::*;
use crate::config::Stage1Config;
use crate::embed::{DEFAULT_EMBED_MAX_CHARS, Embedder, message_embed_text};
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
        source_lang: None,
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
        source_lang: None,
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

// ---- step B: the index follows the row, sealed mail, the doors --------------

fn translate_row(store: &SqliteStore, id: i64, subject: &str, body: &str) {
    let t = Translated {
        subject: subject.into(),
        body: body.into(),
        body_html: Some(format!("<p>{body}</p>")),
        snippet: body.chars().take(200).collect(),
        missing_facts: vec![],
        source_lang: crate::translate::Lang::from_code("fr"),
    };
    store
        .record_translation(
            id,
            &TranslationWrite::Translated {
                translated: &t,
                model: "claude-sonnet-5",
                prompt_version: crate::translate::PROMPT_VERSION,
            },
            Utc::now(),
        )
        .unwrap();
}

fn hits(store: &SqliteStore, acct: AccountId, q: &str) -> Vec<SearchHit> {
    store.search(acct, q, 10, 0).unwrap()
}

#[test]
fn a_landed_translation_is_searchable_in_english_and_the_window_is_the_english() {
    let (store, acct) = store();
    let id = triaged(acct, "g1", "t1")
        .lang("fr")
        .subject("Votre facture")
        .body(FRENCH_BODY)
        .ingest(&store);
    assert!(hits(&store, acct, "invoice").is_empty(), "no English yet");
    assert_eq!(
        store
            .claim_translations(acct, Utc::now(), 10)
            .unwrap()
            .len(),
        1
    );
    translate_row(
        &store,
        id,
        "Your invoice",
        "Hello Marie, your October invoice is available in your customer area.",
    );
    let found = hits(&store, acct, "invoice");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, id);
    // The match is only in the English, so the window is the English one.
    assert!(
        found[0].snippet.contains("October invoice"),
        "{}",
        found[0].snippet
    );
    assert!(found[0].snippet_matches.iter().any(|m| m == "invoice"));
    // The original is still indexed, and its window still wins when it matches.
    let fr = hits(&store, acct, "facture");
    assert_eq!(fr.len(), 1);
    assert!(fr[0].snippet.contains("facture"), "{}", fr[0].snippet);
    // A re-sighting of the message keeps the English in the index.
    triaged(acct, "g1", "t1")
        .lang("fr")
        .subject("Votre facture")
        .body(FRENCH_BODY)
        .ingest(&store);
    assert_eq!(hits(&store, acct, "invoice").len(), 1);
}

#[test]
fn a_translation_judged_english_is_closed_and_indexed_nowhere() {
    let (store, acct) = store();
    let id = triaged(acct, "g1", "t1").lang("de").ingest(&store);
    store.claim_translations(acct, Utc::now(), 10).unwrap();
    store
        .record_translation(
            id,
            &TranslationWrite::NotNeeded {
                model: "claude-sonnet-5",
                prompt_version: crate::translate::PROMPT_VERSION,
            },
            Utc::now(),
        )
        .unwrap();
    assert_eq!(row(&store, id).unwrap().1, "not_needed");
    // Terminal: never claimed again, and not on either door.
    assert!(
        store
            .claim_translations(acct, Utc::now() + Duration::days(30), 10)
            .unwrap()
            .is_empty()
    );
    let view = store.thread_view_with_html(acct, "t1").unwrap();
    assert!(view.messages[0].translation.is_none());
}

#[test]
fn a_released_claim_gives_its_attempt_back() {
    let (store, acct) = store();
    let id = triaged(acct, "g1", "t1").lang("fr").ingest(&store);
    let now = Utc::now();
    assert_eq!(
        store.claim_translations(acct, now, 10).unwrap()[0].attempts,
        1
    );
    store
        .record_translation(id, &TranslationWrite::Released, now)
        .unwrap();
    assert_eq!(row(&store, id), Some(("fr".into(), "pending".into(), 0)));
    // Claimable again at once: nothing about the message was learned.
    assert_eq!(
        store.claim_translations(acct, now, 10).unwrap()[0].attempts,
        1
    );
}

#[test]
fn the_translator_names_the_language_it_read() {
    let (store, acct) = store();
    // whatlang read Spanish as Portuguese; the translator knows better.
    let id = triaged(acct, "g1", "t1").lang("pt").ingest(&store);
    store.claim_translations(acct, Utc::now(), 10).unwrap();
    let t = Translated {
        subject: "Thanks".into(),
        body: "Thanks for everything".into(),
        body_html: None,
        snippet: "Thanks for everything".into(),
        missing_facts: vec![],
        source_lang: crate::translate::Lang::from_code("es"),
    };
    store
        .record_translation(
            id,
            &TranslationWrite::Translated {
                translated: &t,
                model: "m",
                prompt_version: 2,
            },
            Utc::now(),
        )
        .unwrap();
    assert_eq!(row(&store, id).unwrap().0, "es");
    let wire = store.thread_view_with_html(acct, "t1").unwrap().messages[0]
        .translation
        .clone()
        .unwrap();
    assert_eq!(wire.source_lang, "es");
    assert_eq!(wire.source_language, "Spanish");
}

#[test]
fn mail_a_person_sealed_is_never_claimed() {
    let (store, acct) = store();
    let id = triaged(acct, "g1", "t1").lang("fr").ingest(&store);
    store
        .correct_triage(
            acct,
            id,
            crate::types::TriageAxis::Sensitivity,
            "sealed",
            None,
            Utc::now(),
        )
        .unwrap();
    assert!(
        store
            .claim_translations(acct, Utc::now(), 10)
            .unwrap()
            .is_empty()
    );
    // Unsealed by the person again: translatable again.
    store
        .correct_triage(
            acct,
            id,
            crate::types::TriageAxis::Sensitivity,
            "normal",
            None,
            Utc::now(),
        )
        .unwrap();
    assert_eq!(
        store
            .claim_translations(acct, Utc::now(), 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn the_reader_sees_pending_then_the_english_and_never_a_failure() {
    let (store, acct) = store();
    let id = triaged(acct, "g1", "t1").lang("fr").ingest(&store);
    let english = triaged(acct, "g2", "t1").ingest(&store);
    let tr = |store: &SqliteStore, id: i64| {
        store
            .thread_view_with_html(acct, "t1")
            .unwrap()
            .messages
            .into_iter()
            .find(|m| m.id == id)
            .unwrap()
            .translation
    };
    let pending = tr(&store, id).unwrap();
    assert_eq!(pending.status, "pending");
    assert_eq!(pending.source_lang, "fr");
    assert_eq!(pending.source_language, "French");
    assert!(pending.subject.is_empty() && pending.content.is_empty() && pending.html.is_none());
    assert!(tr(&store, english).is_none());
    // Serialized: no `translation` key at all on English mail.
    let json = serde_json::to_value(store.thread_view_with_html(acct, "t1").unwrap()).unwrap();
    let keys: Vec<bool> = json["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.get("translation").is_some())
        .collect();
    assert_eq!(keys.iter().filter(|k| **k).count(), 1);

    store.claim_translations(acct, Utc::now(), 10).unwrap();
    store
        .record_translation(id, &TranslationWrite::Failed { model: "m" }, Utc::now())
        .unwrap();
    assert!(
        tr(&store, id).is_none(),
        "a failure shows the original alone"
    );

    store
        .claim_translations(acct, Utc::now() + Duration::hours(1), 10)
        .unwrap();
    let t = Translated {
        subject: "Your invoice".into(),
        body: "Hello".into(),
        body_html: Some("<p>Hello</p>".into()),
        snippet: "Hello".into(),
        missing_facts: vec!["FR7612345".into()],
        source_lang: None,
    };
    store
        .record_translation(
            id,
            &TranslationWrite::Translated {
                translated: &t,
                model: "m",
                prompt_version: 2,
            },
            Utc::now(),
        )
        .unwrap();
    let done = tr(&store, id).unwrap();
    assert_eq!(done.status, "incomplete");
    assert_eq!(done.subject, "Your invoice");
    assert_eq!(done.content, "Hello");
    assert_eq!(done.html.as_deref(), Some("<p>Hello</p>"));
    assert_eq!(done.missing_facts, Some(vec!["FR7612345".to_string()]));
}

#[test]
fn the_agent_door_sees_the_translation_exactly_when_it_sees_the_body() {
    let (store, acct) = store();
    let id = triaged(acct, "g1", "t1").lang("fr").ingest(&store);
    store.claim_translations(acct, Utc::now(), 10).unwrap();
    translate_row(&store, id, "Your invoice", "Hello Marie");
    // Not yet assessed: the door serves nothing, translation included.
    assert!(store.thread_view(acct, "t1").is_err());
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_message_state SET access='allowed' WHERE message_id=?1",
            params![id],
        )
        .unwrap();
    let view = store.thread_view(acct, "t1").unwrap();
    let t = view.messages[0].translation.clone().unwrap();
    assert_eq!(t.source_language, "French");
    assert_eq!(t.subject, "Your invoice");
    assert_eq!(t.content, "Hello Marie");
    assert_eq!(t.status, "done");
    let json = serde_json::to_value(&view).unwrap();
    assert!(json["messages"][0]["translation"].get("html").is_none());
    // Restricted again: gone with the body.
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_message_state SET access='restricted' WHERE message_id=?1",
            params![id],
        )
        .unwrap();
    assert!(store.thread_view(acct, "t1").is_err());
}

#[test]
fn a_heal_drops_the_translation_its_vector_and_its_index_row() {
    let (store, acct) =
        store_with_embedder(std::sync::Arc::new(crate::embed::StubEmbedder::new(384)));
    let id = triaged(acct, "g1", "t1")
        .lang("fr")
        .body(FRENCH_BODY)
        .ingest(&store);
    store.claim_translations(acct, Utc::now(), 10).unwrap();
    translate_row(&store, id, "Your invoice", "Hello Marie, your invoice");
    let v = crate::embed::StubEmbedder::new(384)
        .embed("Your invoice Hello Marie")
        .unwrap();
    store.upsert_translation_vector(acct, id, &v).unwrap();
    let vecs = |store: &SqliteStore| -> i64 {
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM message_translation_vecs WHERE message_id=?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(vecs(&store), 1);
    assert_eq!(hits(&store, acct, "invoice").len(), 1);
    {
        let conn = store.lock().unwrap();
        super::super::translations::drop_translation_conn(&conn, id).unwrap();
    }
    assert_eq!(vecs(&store), 0, "vec0 has no cascade; the drop deletes it");
    assert!(hits(&store, acct, "invoice").is_empty());
    assert_eq!(hits(&store, acct, "facture").len(), 1);
    // And a vector for a translation that is not served is never written.
    store.upsert_translation_vector(acct, id, &v).unwrap();
    assert_eq!(vecs(&store), 0);
}

#[test]
fn the_vector_legs_take_the_nearer_of_the_two_vectors() {
    let embedder = std::sync::Arc::new(crate::embed::StubEmbedder::new(384));
    let (store, acct) = store_with_embedder(embedder.clone());
    let fr = triaged(acct, "g1", "t1")
        .lang("fr")
        .subject("Votre facture")
        .body(FRENCH_BODY)
        .ingest(&store);
    let decoy = triaged(acct, "g2", "t2")
        .subject("Weekend plans")
        .body("Hiking in the mountains this weekend with the dog")
        .ingest(&store);
    for (id, text) in [
        (
            fr,
            message_embed_text("Votre facture", FRENCH_BODY, DEFAULT_EMBED_MAX_CHARS),
        ),
        (
            decoy,
            message_embed_text(
                "Weekend plans",
                "Hiking in the mountains this weekend with the dog",
                DEFAULT_EMBED_MAX_CHARS,
            ),
        ),
    ] {
        store
            .upsert_message_vector(acct, id, &embedder.embed(&text).unwrap())
            .unwrap();
    }
    let query = "electricity bill payment due";
    let before = store.semantic_search(acct, query, 1).unwrap();
    assert_ne!(
        before[0].0, fr,
        "the French original shares no words with it"
    );

    store.claim_translations(acct, Utc::now(), 10).unwrap();
    let (s, b) = (
        "Your electricity bill",
        "Your electricity bill payment is due",
    );
    translate_row(&store, fr, s, b);
    assert_eq!(
        store.translations_missing_vectors(acct, 10).unwrap().len(),
        1,
        "the drain owes it one"
    );
    store
        .upsert_translation_vector(
            acct,
            fr,
            &embedder
                .embed(&message_embed_text(s, b, DEFAULT_EMBED_MAX_CHARS))
                .unwrap(),
        )
        .unwrap();
    assert!(
        store
            .translations_missing_vectors(acct, 10)
            .unwrap()
            .is_empty()
    );
    let after = store.semantic_search(acct, query, 2).unwrap();
    assert_eq!(after[0].0, fr, "the English vector is the nearer one");
    assert_eq!(after.len(), 2, "one entry per message, not per vector");

    // The external leg: the same answer once both rows are readable, and
    // nothing of the French message while it is not.
    let qvec = embedder
        .embed(&crate::embed::query_embed_text(query))
        .unwrap();
    let readable = |store: &SqliteStore| -> Vec<i64> {
        store
            .nearest_readable(acct, &qvec, 10)
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect()
    };
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_message_state SET access='allowed' WHERE message_id=?1",
            params![decoy],
        )
        .unwrap();
    assert_eq!(
        readable(&store),
        vec![decoy],
        "the unreadable row is never measured"
    );
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_message_state SET access='allowed' WHERE message_id=?1",
            params![fr],
        )
        .unwrap();
    assert_eq!(readable(&store), vec![fr, decoy]);
}
