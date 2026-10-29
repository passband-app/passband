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

/// The access check over `id`, committed through its real path, answering
/// `restricted`. Queues the check first if none is waiting. For mail filed
/// access-only (own mail), where every job is an access check.
fn commit_access(store: &SqliteStore, acct: AccountId, id: i64, restricted: bool) {
    let claim = || {
        store
            .claim_agent_job(acct, "access", Utc::now() + chrono::Duration::hours(1), 60)
            .unwrap()
    };
    let job = match claim() {
        Some(job) => job,
        None => {
            // A trigger of its own, so a settled check is asked again.
            static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let run = RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            store
                .enqueue_agent_triage(acct, id, &format!("test-access:{run}"), false)
                .unwrap();
            claim().expect("an access job waits")
        }
    };
    assert_eq!(job.message_id, id);
    let message = store.load_agent_access_message(&job).unwrap();
    store
        .commit_agent_access(
            &job,
            &message,
            &crate::triage::access::AccessDecision {
                restricted,
                reason: "the access check".into(),
            },
            &serde_json::json!({}),
        )
        .unwrap();
}

/// Own mail is never triaged, so its access check is the only thing that
/// can say it is not a forwarded login code. Until that check answers
/// `allowed`, no embedder reads it.
#[test]
fn the_owners_own_sent_mail_waits_for_its_access_check() {
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
    refused(&store, acct, id, "own mail, access check pending");

    commit_access(&store, acct, id, false);
    assert!(store.message_embeddable(acct, id).unwrap());
    assert!(owed(&store, acct, id));
    let v = vector();
    assert_eq!(store.write_embedded_vectors(acct, &[(id, &v)]).unwrap(), 1);

    // The access check restricting it (a forwarded code, say) seals it, and
    // its vector goes at that commit.
    commit_access(&store, acct, id, true);
    assert_eq!(vec_count_for(&store, id), 0, "restricted own mail loses it");
    refused(&store, acct, id, "restricted own mail");
}

/// A decision vouches for the text it read. Content that changed under a
/// person's explicit allow keeps `access='allowed'` and the old decision row,
/// at a revision that is no longer the message's: no embedder reads it until
/// the new content is decided, and a vector it had goes.
#[test]
fn a_decision_of_older_content_vouches_for_nothing() {
    let (store, acct) = store();
    let id = inbound(acct, "changed").ingest(&store);
    commit_decision(&store, acct, id, |_| {});
    store
        .correct_agent_triage(
            acct,
            id,
            "external_access",
            &serde_json::json!(false),
            Utc::now(),
        )
        .unwrap();
    assert!(store.message_embeddable(acct, id).unwrap());
    plant(&store, acct, id);

    // What a content change under that explicit allow leaves behind.
    {
        let conn = store.lock().unwrap();
        conn.execute(
            "UPDATE agent_message_state SET revision=revision+1, access='allowed' WHERE message_id=?1",
            [id],
        )
        .unwrap();
        search::purge_forbidden_vectors_conn(&conn, Some(id)).unwrap();
    }
    assert_eq!(vec_count_for(&store, id), 0, "the stale vector goes");
    refused(&store, acct, id, "decision of older content");
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
    commit_access(&store, acct, own, false);
    let own_pending = triaged(acct, "own-pending", "own-pending")
        .from("me@example.com")
        .is_sent(true)
        .ingest(&store);
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_triage_jobs SET state='completed' WHERE message_id=?1",
            [own_pending],
        )
        .unwrap();
    // A legacy row from before the agent pipeline: no state, the column normal.
    let vetted = triaged(acct, "vetted", "vetted")
        .from("bob@example.com")
        .seed(&store);
    // What a store looks like after the old ingest embedded everything.
    for id in [
        auth,
        restricted,
        legacy,
        undecided,
        own_pending,
        ordinary,
        own,
        vetted,
    ] {
        plant(&store, acct, id);
        assert_eq!(vec_count_for(&store, id), 1);
    }
    drop(store);

    let store = SqliteStore::open(dir.join("store.sqlite3")).unwrap();
    for id in [auth, restricted, legacy, undecided, own_pending] {
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
