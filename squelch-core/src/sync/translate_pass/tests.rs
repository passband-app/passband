//! The translate pass end to end, against a scripted translator: what a round
//! claims, bills and records, what the index and the doors then hold, and the
//! three ways a round spends nothing.

use super::*;
use crate::store::SqliteStore;
use crate::sync::ingest::RawFetched;
use crate::triage::llm::{ClassifyError, LlmOutcome, Usage};
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

const SUBJECT: &str = "Votre facture d'électricité";
const BODY: &str = "Bonjour Marie, votre facture d'électricité du mois d'octobre est disponible. Le paiement est dû avant le vendredi prochain. Merci de votre confiance.";

/// What the scripted model answers.
#[derive(Clone, Copy)]
enum Answer {
    /// Every segment through a small dictionary, facts untouched.
    French,
    /// "This is English", as for a detection false positive.
    English,
    /// A gateway that rejects the key or the model.
    Rejected,
}

struct Scripted {
    answer: Answer,
    calls: AtomicUsize,
    sent: Mutex<Vec<String>>,
}

impl Scripted {
    fn new(answer: Answer) -> Arc<Self> {
        Arc::new(Self {
            answer,
            calls: AtomicUsize::new(0),
            sent: Mutex::new(Vec::new()),
        })
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

fn usage() -> Option<Usage> {
    Some(Usage {
        input_tokens: 120,
        output_tokens: 80,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation: None,
    })
}

fn english(segment: &str) -> String {
    let dict = [
        ("Votre facture d'électricité", "Your electricity bill"),
        ("Bonjour", "Hello"),
        (
            "votre facture d'électricité du mois d'octobre est disponible",
            "your electricity bill for October is available",
        ),
        (
            "Le paiement est dû avant le vendredi prochain",
            "Payment is due before next Friday",
        ),
        ("Merci de votre confiance", "Thank you for your trust"),
    ];
    let mut out = segment.to_string();
    for (fr, en) in dict {
        out = out.replace(fr, en);
    }
    out
}

#[async_trait::async_trait]
impl crate::translate::TranslateBackend for Scripted {
    async fn call(
        &self,
        user: &str,
        _max_tokens: u32,
    ) -> std::result::Result<LlmOutcome<String>, ClassifyError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.sent.lock().unwrap().push(user.to_string());
        let start = user
            .find("-----BEGIN UNTRUSTED EMAIL SEGMENTS-----\n")
            .unwrap()
            + 41;
        let end = user
            .find("\n-----END UNTRUSTED EMAIL SEGMENTS-----")
            .unwrap();
        let segments: Vec<String> = serde_json::from_str(&user[start..end]).unwrap();
        Ok(match self.answer {
            Answer::French => LlmOutcome::Ok(
                serde_json::json!({
                    "source_language": "fr",
                    "translations": segments.iter().map(|s| english(s)).collect::<Vec<_>>(),
                })
                .to_string(),
                usage(),
            ),
            Answer::English => LlmOutcome::Ok(
                serde_json::json!({"source_language": "en", "translations": []}).to_string(),
                usage(),
            ),
            Answer::Rejected => LlmOutcome::Failed("http_402:payment_required".into(), usage()),
        })
    }
    fn model(&self) -> &str {
        "claude-sonnet-5"
    }
}

type Engine = SyncEngine<SqliteStore, crate::credentials::EnvCredentialStore>;

/// An engine that may spend (a poll has succeeded, an LLM is configured),
/// with `translator` standing in for the real client and a deterministic
/// embedder on the store.
fn engine(translator: Option<Arc<Scripted>>) -> (Engine, Arc<SqliteStore>, AccountId) {
    let store = Arc::new(
        SqliteStore::open_in_memory()
            .unwrap()
            .with_embedder(Arc::new(crate::embed::StubEmbedder::new(384)))
            .unwrap(),
    );
    let acct = store.ensure_account("me@example.com").unwrap();
    let mut eng = SyncEngine::new(
        store.clone(),
        Arc::new(crate::credentials::EnvCredentialStore),
        acct,
        "me@example.com".to_string(),
        Config::default(),
    );
    eng.stage2_llm = Some(ResolvedLlm {
        api_key: "test".into(),
        provider: Stage2Provider::Anthropic,
        url: "http://127.0.0.1:1/".into(),
    });
    eng.poll_healthy.store(true, Ordering::Relaxed);
    eng.translator = translator.map(|t| t as Arc<dyn crate::translate::TranslateBackend>);
    (eng, store, acct)
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

/// Ingest through the engine's own path, the one that drops the queue
/// when there is no translator.
fn land(eng: &Engine, acct: AccountId, id: &str, sent: bool, spam: bool) -> i64 {
    let fetched = raw(acct, id, SUBJECT, BODY, sent, spam);
    match eng.ingest_one(&fetched, &[], Utc::now(), IngestOrigin::Incremental) {
        Ok(Some(ingested)) => ingested.id,
        other => panic!("ingest: {:?}", other.map(|i| i.map(|i| i.id))),
    }
}

fn status(store: &SqliteStore, id: i64) -> Option<(String, i64)> {
    let acct = store
        .query_i64_for_test(&format!("SELECT account_id FROM messages WHERE id={id}"))
        .unwrap();
    store
        .message_translations(acct, &[id])
        .unwrap()
        .into_iter()
        .next()
        .map(|t| (t.status.as_str().to_string(), t.attempts))
}

fn count(store: &SqliteStore, sql: &str) -> i64 {
    store.query_i64_for_test(sql).unwrap_or(0)
}

fn allow(store: &SqliteStore, id: i64) {
    store.execute_batch_for_test(&format!(
        "UPDATE agent_message_state SET access='allowed' WHERE message_id={id}"
    ));
}

fn billed(store: &SqliteStore, acct: AccountId) -> (u64, u64) {
    store
        .list_usage_by_category(acct, 2)
        .unwrap()
        .into_iter()
        .find(|(c, _)| c == crate::translate::USAGE_CATEGORY)
        .map(|(_, days)| {
            days.iter()
                .fold((0, 0), |(c, i), d| (c + d.calls, i + d.input_tokens))
        })
        .unwrap_or((0, 0))
}

#[tokio::test]
async fn french_mail_is_translated_indexed_and_served_to_both_doors() {
    let backend = Scripted::new(Answer::French);
    let (eng, store, acct) = engine(Some(backend.clone()));
    let id = land(&eng, acct, "fr1", false, false);
    assert_eq!(status(&store, id), Some(("pending".into(), 0)));
    assert!(store.search(acct, "electricity", 10, 0).unwrap().is_empty());

    assert!(eng.translate_pass().await, "the round claimed work");
    assert_eq!(backend.calls(), 1, "subject and body in one batch");
    assert_eq!(status(&store, id).unwrap().0, "done");
    assert_eq!(billed(&store, acct), (1, 120), "billed under translate");
    assert!(!eng.translate_pass().await, "nothing left to claim");

    // KEYWORD: an English word only the translation has.
    let hits = store.search(acct, "electricity", 10, 0).unwrap();
    assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), vec![id]);
    assert!(hits[0].snippet.contains("Payment is due") || hits[0].snippet.contains("electricity"));

    // SEMANTIC: the translation's vector was written by the pass itself.
    let vecs = count(
        &store,
        &format!("SELECT COUNT(*) FROM message_translation_vecs WHERE message_id={id}"),
    );
    assert_eq!(vecs, 1);
    let near = store
        .semantic_search(acct, "electricity bill payment due Friday", 1)
        .unwrap();
    assert_eq!(near[0].0, id);

    // THE READER: the English beside the original.
    let thread = "thr-fr1".to_string();
    let view = store.thread_view_with_html(acct, &thread).unwrap();
    let t = view.messages[0].translation.clone().unwrap();
    assert_eq!(t.status, "done");
    assert_eq!(t.source_language, "French");
    assert_eq!(t.subject, "Your electricity bill");
    assert!(
        t.content.contains("Payment is due before next Friday"),
        "{}",
        t.content
    );
    assert_eq!(
        view.messages[0].subject, SUBJECT,
        "the original is never rewritten"
    );
    assert_eq!(view.messages[0].content.trim(), BODY);

    // THE AGENT DOOR: only once the body itself is readable there.
    assert!(store.thread_view(acct, &thread).is_err());
    allow(&store, id);
    let mcp = store.thread_view(acct, &thread).unwrap();
    assert_eq!(
        mcp.messages[0].translation.as_ref().unwrap().subject,
        "Your electricity bill"
    );

    // TRIAGE CONTEXT: the English rides along, outside the content snapshot.
    let before = count(
        &store,
        &format!("SELECT COUNT(*) FROM agent_triage_jobs WHERE message_id={id}"),
    );
    let job = crate::store::agent_triage::AgentJob {
        id: 0,
        account_id: acct,
        message_id: id,
        kind: "triage".into(),
        trigger: "context".into(),
        lease_token: String::new(),
        attempts: 0,
        arrival_eligible: false,
        foreground: false,
    };
    let context =
        crate::store::agent_triage::AgentTriageStore::load_agent_context(store.as_ref(), &job)
            .unwrap();
    assert_eq!(
        context.message.translated_subject.as_deref(),
        Some("Your electricity bill")
    );
    assert!(context.message.translated_body.is_some());
    let json = serde_json::to_value(&context.message).unwrap();
    assert!(json.get("translated_body").is_some());
    let after = count(
        &store,
        &format!("SELECT COUNT(*) FROM agent_triage_jobs WHERE message_id={id}"),
    );
    assert_eq!(before, after, "a translation landing queued no re-triage");
}

#[tokio::test]
async fn a_message_the_model_reads_as_english_is_not_needed() {
    let backend = Scripted::new(Answer::English);
    let (eng, store, acct) = engine(Some(backend.clone()));
    let id = land(&eng, acct, "fr1", false, false);
    assert!(eng.translate_pass().await);
    assert_eq!(status(&store, id).unwrap().0, "not_needed");
    assert_eq!(billed(&store, acct).0, 1, "the call is billed all the same");
    let view = store.thread_view_with_html(acct, "thr-fr1").unwrap();
    assert!(view.messages[0].translation.is_none());
    assert!(!eng.translate_pass().await, "terminal");
    assert_eq!(backend.calls(), 1);
}

#[tokio::test]
async fn a_rejected_request_stops_the_pass_and_spends_no_attempt() {
    let backend = Scripted::new(Answer::Rejected);
    let (eng, store, acct) = engine(Some(backend.clone()));
    let a = land(&eng, acct, "fr1", false, false);
    let b = land(&eng, acct, "fr2", false, false);
    assert!(eng.translate_pass().await);
    // Both rows of the round came back untouched, attempt and all.
    assert_eq!(status(&store, a), Some(("pending".into(), 0)));
    assert_eq!(status(&store, b), Some(("pending".into(), 0)));
    assert_eq!(
        billed(&store, acct).0,
        2,
        "what the rejections cost is billed"
    );
    assert!(eng.translate_retry_after.load(Ordering::Relaxed) > Utc::now().timestamp());
    // Cooling down: the next round asks nobody.
    let calls = backend.calls();
    assert!(!eng.translate_pass().await);
    assert_eq!(backend.calls(), calls);
    // Triage's own circuit is not this pass's to trip.
    assert_eq!(eng.agent_retry_after.load(Ordering::Relaxed), 0);
    assert!(eng.refine_can_spend());
    // Once the cooldown is over the rows are there to try again.
    eng.translate_retry_after.store(0, Ordering::Relaxed);
    assert!(eng.translate_pass().await);
    assert_eq!(backend.calls(), calls + 2);
}

#[tokio::test]
async fn spam_and_sent_mail_are_never_sent_to_the_translator() {
    let backend = Scripted::new(Answer::French);
    let (eng, store, acct) = engine(Some(backend.clone()));
    let spam = land(&eng, acct, "spam", false, true);
    let sent = land(&eng, acct, "sent", true, false);
    assert_eq!(status(&store, spam), None);
    assert_eq!(status(&store, sent), None);
    // A row that predates the spam verdict is not claimed either.
    let late = land(&eng, acct, "late", false, false);
    store.execute_batch_for_test(&format!("UPDATE messages SET is_spam=1 WHERE id={late}"));
    assert!(!eng.translate_pass().await);
    assert_eq!(backend.calls(), 0);
}

#[tokio::test]
async fn mail_a_person_sealed_is_skipped() {
    let backend = Scripted::new(Answer::French);
    let (eng, store, acct) = engine(Some(backend.clone()));
    let id = land(&eng, acct, "fr1", false, false);
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
    assert!(!eng.translate_pass().await);
    assert_eq!(backend.calls(), 0);
    assert_eq!(status(&store, id), Some(("pending".into(), 0)));
}

#[tokio::test]
async fn archive_mail_waits_for_the_thread_to_be_opened() {
    let backend = Scripted::new(Answer::French);
    let (eng, store, acct) = engine(Some(backend.clone()));
    let fetched = raw(acct, "old", SUBJECT, BODY, false, false);
    let id = eng
        .ingest_one(&fetched, &[], Utc::now(), IngestOrigin::Archive)
        .unwrap()
        .unwrap()
        .id;
    assert_eq!(status(&store, id), Some(("deferred".into(), 0)));
    assert!(
        !eng.translate_pass().await,
        "the archive spends nothing unopened"
    );
    assert_eq!(backend.calls(), 0);
    // Not even on the reader before it is opened.
    assert!(
        store
            .thread_view_with_html(acct, "thr-old")
            .unwrap()
            .messages[0]
            .translation
            .is_none()
    );
    // Opening it (what GET /client/thread does) promotes, then the pass runs.
    assert_eq!(
        store
            .promote_deferred_translations(acct, "thr-old", Utc::now())
            .unwrap(),
        1
    );
    let view = store.thread_view_with_html(acct, "thr-old").unwrap();
    assert_eq!(
        view.messages[0].translation.as_ref().unwrap().status,
        "pending"
    );
    assert!(eng.translate_pass().await);
    assert_eq!(status(&store, id).unwrap().0, "done");
}

#[tokio::test]
async fn no_translator_queues_nothing_and_spends_nothing() {
    let (eng, store, acct) = engine(None);
    let id = land(&eng, acct, "fr1", false, false);
    assert_eq!(status(&store, id), None, "no row promising a translation");
    assert!(!eng.translate_pass().await);
}

#[tokio::test]
async fn the_drain_embeds_a_translation_that_landed_without_a_vector() {
    let backend = Scripted::new(Answer::French);
    let (eng, store, acct) = engine(Some(backend.clone()));
    let id = land(&eng, acct, "fr1", false, false);
    assert!(eng.translate_pass().await);
    store.execute_batch_for_test(&format!(
        "DELETE FROM message_translation_vecs WHERE message_id={id}"
    ));
    assert_eq!(
        store.translations_missing_vectors(acct, 10).unwrap().len(),
        1
    );
    eng.backfill_missing_vectors().await;
    assert!(
        store
            .translations_missing_vectors(acct, 10)
            .unwrap()
            .is_empty()
    );
    assert!(!eng.vectors_dirty.load(Ordering::Relaxed));
}
