//! THE EMBEDDERS' SEALED GATE (`embeddable_mail_sql!`, `vector_forbidden_sql!`):
//! sealed mail reaches neither the in-process embedder nor the shared
//! embedding service, and nothing inbound is embedded until a decision says
//! it is not sealed.
//!
//! Sealed today is the DECISION (auth, restricted), never the retired
//! `triage.sensitivity` column, which ingest always writes `normal`. Every
//! test that seals through the agent pipeline asserts the column still reads
//! `normal`, so none of them can pass on the column alone.

use super::super::*;
use super::support::*;
use crate::store::agent_triage::AgentTriageStore;
use crate::triage::decision::AuthKind;
use crate::types::SealedKind;

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

fn owed(store: &SqliteStore, acct: AccountId, id: i64) -> bool {
    store
        .messages_missing_vectors(acct, 100)
        .unwrap()
        .iter()
        .any(|m| m.message_id == id)
}

fn vector() -> Vec<f32> {
    vec![0.5; VEC_DIMS]
}

/// Plant a vector the way pre-gate code did: raw, no check.
fn plant(store: &SqliteStore, acct: AccountId, id: i64) {
    store.upsert_message_vector(acct, id, &vector()).unwrap();
}

/// Every way the gate can be asked, all answering "no": the predicate, the
/// drain's source, and the gated write (which writes nothing).
fn refused(store: &SqliteStore, acct: AccountId, id: i64, why: &str) {
    assert!(!store.message_embeddable(acct, id).unwrap(), "{why}");
    assert!(!owed(store, acct, id), "{why}: not in the drain");
    let v = vector();
    assert_eq!(
        store.write_embedded_vectors(acct, &[(id, &v)]).unwrap(),
        0,
        "{why}: the write refuses"
    );
    assert_eq!(vec_count_for(store, id), 0, "{why}: no vector");
}

fn fts_rows(store: &SqliteStore, id: i64) -> i64 {
    store
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM messages_fts WHERE rowid = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
}

fn inbound(acct: AccountId, key: &str) -> TriagedBuilder {
    triaged(acct, key, key)
        .from("noreply@service.example")
        .subject("Your sign-in code")
        .body("Your verification code is 482913")
}

#[test]
fn a_normal_row_with_an_auth_decision_is_never_embedded() {
    let (store, acct) = store();
    let id = inbound(acct, "otp").ingest(&store);
    commit_decision(&store, acct, id, |d| d.auth.kinds = vec![AuthKind::Otp]);
    assert_eq!(
        sensitivity(&store, id),
        "normal",
        "the legacy column is unused"
    );
    refused(&store, acct, id, "auth decision");
}

#[test]
fn a_restricted_row_is_never_embedded() {
    let (store, acct) = store();
    let id = inbound(acct, "restricted").ingest(&store);
    commit_decision(&store, acct, id, |d| d.external_access.restricted = true);
    assert_eq!(sensitivity(&store, id), "normal");
    refused(&store, acct, id, "restricted decision");
}

#[test]
fn a_legacy_sealed_row_is_never_embedded() {
    let (store, acct) = store();
    let id = inbound(acct, "legacy").sealed(SealedKind::Otp).seed(&store);
    assert_eq!(sensitivity(&store, id), "sealed");
    // Even with an ordinary decision and access settled beside it.
    settle_decided(&store, acct, &[id]);
    refused(&store, acct, id, "legacy column");
}

#[test]
fn undecided_inbound_mail_waits_for_a_decision_that_does_not_seal_it() {
    let (store, acct) = store();
    let id = triaged(acct, "note", "note")
        .from("alice@example.com")
        .subject("Lunch")
        .body("Lunch on Thursday?")
        .ingest(&store);
    // At ingest nothing has decided it: it could still be a login code.
    refused(&store, acct, id, "undecided");
    commit_decision(&store, acct, id, |_| {});
    assert!(store.message_embeddable(acct, id).unwrap());
    assert!(owed(&store, acct, id), "the drain owes it one now");
    let v = vector();
    assert_eq!(store.write_embedded_vectors(acct, &[(id, &v)]).unwrap(), 1);
    assert_eq!(vec_count_for(&store, id), 1);
}

#[test]
fn the_owners_own_sent_mail_is_embedded_without_a_decision() {
    let (store, acct) = store();
    let id = triaged(acct, "sent", "sent")
        .from("me@example.com")
        .is_sent(true)
        .subject("re: the lease")
        .body("I will send the signed lease on Friday.")
        .ingest(&store);
    let decided: bool = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_message_decisions WHERE message_id=?1)",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!decided, "own mail is never triaged");
    assert!(store.message_embeddable(acct, id).unwrap());
    assert!(owed(&store, acct, id));
    let v = vector();
    assert_eq!(store.write_embedded_vectors(acct, &[(id, &v)]).unwrap(), 1);

    // The access check restricting it (a forwarded code, say) seals it, and
    // its vector goes at that commit.
    let job = store
        .claim_agent_job(acct, "access", Utc::now() + chrono::Duration::hours(1), 60)
        .unwrap()
        .expect("own mail is assessed for access");
    assert_eq!(job.message_id, id);
    let message = store.load_agent_access_message(&job).unwrap();
    store
        .commit_agent_access(
            &job,
            &message,
            &crate::triage::access::AccessDecision {
                restricted: true,
                reason: "a forwarded code".into(),
            },
            &serde_json::json!({}),
        )
        .unwrap();
    assert_eq!(vec_count_for(&store, id), 0, "restricted own mail loses it");
    refused(&store, acct, id, "restricted own mail");
}

#[test]
fn a_decision_that_seals_deletes_the_vector_and_keeps_the_keyword_index() {
    let (store, acct) = store();
    let id = inbound(acct, "late").ingest(&store);
    commit_decision(&store, acct, id, |_| {});
    let v = vector();
    assert_eq!(store.write_embedded_vectors(acct, &[(id, &v)]).unwrap(), 1);
    assert_eq!(fts_rows(&store, id), 1);
    // A re-triage decides it was a login code after all.
    commit_decision(&store, acct, id, |d| d.auth.kinds = vec![AuthKind::Otp]);
    assert_eq!(sensitivity(&store, id), "normal");
    assert_eq!(vec_count_for(&store, id), 0, "the decision deleted it");
    assert_eq!(fts_rows(&store, id), 1, "keyword search is untouched");
}

#[test]
fn a_persons_restriction_deletes_the_vector() {
    let (store, acct) = store();
    let id = inbound(acct, "corrected").ingest(&store);
    commit_decision(&store, acct, id, |_| {});
    plant(&store, acct, id);
    store
        .correct_agent_triage(
            acct,
            id,
            "external_access",
            &serde_json::json!(true),
            Utc::now(),
        )
        .unwrap();
    assert_eq!(vec_count_for(&store, id), 0);
    refused(&store, acct, id, "restricted by a person");
}

#[test]
fn the_sweep_at_open_deletes_vectors_embedded_before_the_gate() {
    let (store, acct, dir) = file_store("embed-gate-sweep");
    let auth = inbound(acct, "auth").ingest(&store);
    commit_decision(&store, acct, auth, |d| d.auth.kinds = vec![AuthKind::Otp]);
    let restricted = inbound(acct, "restricted").ingest(&store);
    commit_decision(&store, acct, restricted, |d| {
        d.external_access.restricted = true
    });
    let legacy = inbound(acct, "legacy").sealed(SealedKind::Otp).seed(&store);
    let undecided = inbound(acct, "undecided").ingest(&store);
    let ordinary = triaged(acct, "ordinary", "ordinary")
        .from("alice@example.com")
        .ingest(&store);
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_triage_jobs SET state='completed' WHERE message_id=?1",
            [undecided],
        )
        .unwrap();
    commit_decision(&store, acct, ordinary, |_| {});
    let own = triaged(acct, "own", "own")
        .from("me@example.com")
        .is_sent(true)
        .ingest(&store);
    // A legacy row from before the agent pipeline: no state, the column normal.
    let vetted = triaged(acct, "vetted", "vetted")
        .from("bob@example.com")
        .seed(&store);
    // What a store looks like after the old ingest embedded everything.
    for id in [auth, restricted, legacy, undecided, ordinary, own, vetted] {
        plant(&store, acct, id);
        assert_eq!(vec_count_for(&store, id), 1);
    }
    drop(store);

    let store = SqliteStore::open(dir.join("store.sqlite3")).unwrap();
    for id in [auth, restricted, legacy, undecided] {
        assert_eq!(vec_count_for(&store, id), 0, "message {id} swept");
        assert_eq!(fts_rows(&store, id), 1, "its keyword row stays");
    }
    for id in [ordinary, own, vetted] {
        assert_eq!(vec_count_for(&store, id), 1, "message {id} kept");
    }
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn spam_stays_excluded() {
    let (store, acct) = store();
    let id = triaged(acct, "spam", "spam")
        .from("winner@lottery.example")
        .is_spam(true)
        .ingest(&store);
    settle_decided(&store, acct, &[id]);
    refused(&store, acct, id, "spam");
}

#[test]
fn the_sweep_reads_vector_keys_and_probes_messages_by_key() {
    // The backlog sweep's plan: the vector tables' keys, each joined to its
    // message by primary key, every gate probe a primary-key search. Never a
    // scan of `messages`.
    let (store, _acct) = store();
    let conn = store.lock().unwrap();
    let lines = plan(
        &conn,
        concat!(
            "SELECT m.id
             FROM (SELECT message_id AS id FROM message_vecs
                   UNION SELECT message_id FROM message_translation_vecs) vr
             CROSS JOIN messages m ON m.id = vr.id
             LEFT JOIN triage t ON t.message_id = m.id
             WHERE ",
            vector_forbidden_sql!()
        ),
    );
    assert!(
        !lines
            .iter()
            .any(|l| l == "SCAN m" || l.starts_with("SCAN m ")),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("SEARCH m USING INTEGER PRIMARY KEY")),
        "{lines:#?}"
    );
}
