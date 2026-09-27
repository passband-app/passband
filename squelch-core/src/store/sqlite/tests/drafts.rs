//! Local-draft CRUD tests.

use super::super::*;
use super::support::*;
use crate::types::SealedKind;

/// A fixed base instant, so `updated_at` movement is asserted, not raced.
fn t(minutes: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-07-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
        + chrono::Duration::minutes(minutes)
}

#[test]
fn upsert_draft_edits_the_same_reply_key_in_place() {
    let (store, acct) = store();
    let first = store
        .upsert_draft(
            acct,
            DraftKey::Reply(7),
            DraftFields {
                to_addr: "alice@example.com",
                subject: "Re: Lunch",
                body: "sure",
                ..Default::default()
            },
            t(0),
        )
        .unwrap()
        .unwrap();
    assert_eq!(first.created_at, t(0));
    assert_eq!(first.updated_at, t(0));

    let second = store
        .upsert_draft(
            acct,
            DraftKey::Reply(7),
            DraftFields {
                to_addr: "alice@example.com",
                subject: "Re: Lunch",
                body: "sure, 1pm",
                ..Default::default()
            },
            t(5),
        )
        .unwrap()
        .unwrap();
    // Same composition: same id and created_at, only the body and clock move.
    assert_eq!(second.id, first.id);
    assert_eq!(second.created_at, t(0));
    assert_eq!(second.updated_at, t(5));
    assert_eq!(second.body, "sure, 1pm");

    let all = store.list_drafts(acct).unwrap();
    assert_eq!(all.len(), 1, "an edit must not add a row");
    assert_eq!(all[0], second);
}

#[test]
fn a_draft_round_trips_both_copy_lists_and_an_edit_can_empty_them() {
    let (store, acct) = store();
    let saved = store
        .upsert_draft(
            acct,
            DraftKey::NewLatest,
            DraftFields {
                to_addr: "alice@example.com",
                cc_addr: "bob@example.com, carol@example.com",
                bcc_addr: "dave@example.com",
                subject: "Lunch",
                body: "1pm?",
            },
            t(0),
        )
        .unwrap()
        .unwrap();
    assert_eq!(saved.cc_addr, "bob@example.com, carol@example.com");
    assert_eq!(saved.bcc_addr, "dave@example.com");
    assert_eq!(store.list_drafts(acct).unwrap()[0], saved);

    // MOVING SOMEBODY OFF A LIST HAS TO STICK. An edit that empties `bcc_addr`
    // is the composer's "no longer blind-copying dave", and a store that kept
    // the old value would put him back on the next restore — silently, since a
    // Bcc is the one field nothing else on screen would contradict.
    let edited = store
        .upsert_draft(
            acct,
            DraftKey::NewLatest,
            DraftFields {
                to_addr: "alice@example.com",
                cc_addr: "",
                bcc_addr: "",
                subject: "Lunch",
                body: "1pm?",
            },
            t(5),
        )
        .unwrap()
        .unwrap();
    assert_eq!(edited.id, saved.id, "an edit is the same composition");
    assert_eq!(edited.cc_addr, "");
    assert_eq!(edited.bcc_addr, "");
}

fn fields<'a>(to: &'a str, subject: &'a str, body: &'a str) -> DraftFields<'a> {
    DraftFields {
        to_addr: to,
        subject,
        body,
        ..Default::default()
    }
}

#[test]
fn reply_forward_and_many_new_message_drafts_coexist() {
    let (store, acct) = store();
    let reply = store
        .upsert_draft(
            acct,
            DraftKey::Reply(7),
            fields("alice@example.com", "Re: Lunch", "sure"),
            t(0),
        )
        .unwrap()
        .unwrap();
    // TWO new-message drafts are two rows: a second composition no longer
    // overwrites the first.
    let a = store
        .upsert_draft(
            acct,
            DraftKey::NewCreate,
            fields("bob@example.com", "Hello", "hi"),
            t(1),
        )
        .unwrap()
        .unwrap();
    let b = store
        .upsert_draft(
            acct,
            DraftKey::NewCreate,
            fields("carol@example.com", "Other", "yo"),
            t(2),
        )
        .unwrap()
        .unwrap();
    assert_ne!(a.id, b.id);
    assert_ne!(a.id, reply.id);
    assert!(a.reply_to_message_id.is_none() && a.forward_of_message_id.is_none());
    let fwd = store
        .upsert_draft(
            acct,
            DraftKey::Forward(7),
            fields("dave@example.com", "Fwd: Lunch", "fyi"),
            t(3),
        )
        .unwrap()
        .unwrap();
    assert_eq!(fwd.forward_of_message_id, Some(7));
    assert_eq!(fwd.reply_to_message_id, None);
    assert_ne!(
        fwd.id, reply.id,
        "a reply and a forward of one message are two rows"
    );
    assert_eq!(store.list_drafts(acct).unwrap().len(), 4);

    // The forward key upserts in place, like the reply key.
    let fwd2 = store
        .upsert_draft(
            acct,
            DraftKey::Forward(7),
            fields("dave@example.com", "Fwd: Lunch", "fyi!"),
            t(4),
        )
        .unwrap()
        .unwrap();
    assert_eq!(fwd2.id, fwd.id);
    assert_eq!(fwd2.created_at, t(3));
    assert_eq!(store.list_drafts(acct).unwrap().len(), 4);

    // The one-per-key rule for replies and forwards is enforced by the SCHEMA,
    // not just by the upsert path; the NULL key is not unique at all any more.
    let conn = store.lock().unwrap();
    for (reply_to, forward_of) in [(Some(7), None), (None, Some(7))] {
        let e = conn.execute(
            "INSERT INTO drafts(account_id, reply_to_message_id, forward_of_message_id,
                 to_addr, subject, body, created_at, updated_at)
             VALUES(?1,?2,?3,'','','',?4,?4)",
            params![acct, reply_to, forward_of, t(5).to_rfc3339()],
        );
        let msg = e.expect_err("duplicate must be refused").to_string();
        assert!(
            msg.contains("UNIQUE"),
            "duplicate {reply_to:?}/{forward_of:?}: {msg}"
        );
    }
    conn.execute(
        "INSERT INTO drafts(account_id, to_addr, subject, body, created_at, updated_at)
         VALUES(?1,'','','',?2,?2)",
        params![acct, t(5).to_rfc3339()],
    )
    .expect("a third new-message draft is allowed");
}

#[test]
fn update_by_id_keeps_id_created_at_and_key_and_misses_other_accounts() {
    let (store, acct) = store();
    let other = store.ensure_account("other@example.com").unwrap();
    let a = store
        .upsert_draft(
            acct,
            DraftKey::NewCreate,
            fields("bob@example.com", "Hello", "hi"),
            t(0),
        )
        .unwrap()
        .unwrap();
    let b = store
        .upsert_draft(
            acct,
            DraftKey::NewCreate,
            fields("carol@example.com", "Other", "yo"),
            t(1),
        )
        .unwrap()
        .unwrap();
    let edited = store
        .upsert_draft(
            acct,
            DraftKey::NewById(a.id),
            fields("bob@example.com", "Hello", "hi there"),
            t(5),
        )
        .unwrap()
        .unwrap();
    assert_eq!(edited.id, a.id);
    assert_eq!(edited.created_at, t(0));
    assert_eq!(edited.updated_at, t(5));
    assert_eq!(edited.body, "hi there");
    // The other draft is untouched.
    assert_eq!(store.draft_by_id(acct, b.id).unwrap().unwrap(), b);

    // By id addresses a reply row too, and leaves its key alone.
    let reply = store
        .upsert_draft(
            acct,
            DraftKey::Reply(9),
            fields("x@example.com", "Re: X", "a"),
            t(6),
        )
        .unwrap()
        .unwrap();
    let reply2 = store
        .upsert_draft(
            acct,
            DraftKey::NewById(reply.id),
            fields("x@example.com", "Re: X", "b"),
            t(7),
        )
        .unwrap()
        .unwrap();
    assert_eq!(reply2.id, reply.id);
    assert_eq!(reply2.reply_to_message_id, Some(9));

    // Unknown id and another account's id are the same miss, and the miss
    // writes nothing.
    assert!(
        store
            .upsert_draft(acct, DraftKey::NewById(9999), fields("", "", "z"), t(8))
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .upsert_draft(
                other,
                DraftKey::NewById(a.id),
                fields("", "", "stolen"),
                t(8)
            )
            .unwrap()
            .is_none()
    );
    assert!(store.draft_by_id(other, a.id).unwrap().is_none());
    assert_eq!(
        store.draft_by_id(acct, a.id).unwrap().unwrap().body,
        "hi there"
    );
    assert_eq!(store.list_drafts(acct).unwrap().len(), 3);
    assert!(store.list_drafts(other).unwrap().is_empty());
}

#[test]
fn keyless_latest_edits_the_newest_new_message_draft_and_creates_when_none() {
    let (store, acct) = store();
    // No new-message draft yet: created. A reply row never counts.
    store
        .upsert_draft(acct, DraftKey::Reply(7), fields("", "Re", "r"), t(0))
        .unwrap()
        .unwrap();
    let first = store
        .upsert_draft(acct, DraftKey::NewLatest, fields("", "one", "1"), t(1))
        .unwrap()
        .unwrap();
    assert!(first.reply_to_message_id.is_none());
    let second = store
        .upsert_draft(acct, DraftKey::NewCreate, fields("", "two", "2"), t(2))
        .unwrap()
        .unwrap();
    // A forward NEWER than every plain row: its reply key is NULL too, so only
    // the forward predicate keeps an old app's save off it.
    let forward = store
        .upsert_draft(acct, DraftKey::Forward(9), fields("", "Fwd", "f"), t(3))
        .unwrap()
        .unwrap();
    // The old app's key-less save lands on the NEWEST PLAIN new-message row.
    let edited = store
        .upsert_draft(
            acct,
            DraftKey::NewLatest,
            fields("", "two", "2 again"),
            t(4),
        )
        .unwrap()
        .unwrap();
    assert_eq!(edited.id, second.id);
    assert_eq!(edited.created_at, t(2));
    assert!(edited.forward_of_message_id.is_none());
    assert_eq!(
        store.draft_by_id(acct, first.id).unwrap().unwrap().body,
        "1"
    );
    let fwd = store.draft_by_id(acct, forward.id).unwrap().unwrap();
    assert_eq!(
        fwd.body, "f",
        "the newer forward is never the keyless target"
    );
    assert_eq!(fwd.updated_at, t(3));
    assert_eq!(store.list_drafts(acct).unwrap().len(), 4);
}

#[test]
fn list_drafts_fills_parent_context_from_the_replied_or_forwarded_message() {
    let (store, acct) = store();
    let parent = triaged(acct, "g1", "t1").ingest(&store);
    let (thread, subject, from_addr, from_name): (String, String, String, Option<String>) = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT thread_id, subject, from_addr, from_name FROM messages WHERE id = ?1",
            params![parent],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    store
        .upsert_draft(acct, DraftKey::Reply(parent), fields("", "Re", "r"), t(0))
        .unwrap()
        .unwrap();
    store
        .upsert_draft(
            acct,
            DraftKey::Forward(parent),
            fields("", "Fwd", "f"),
            t(1),
        )
        .unwrap()
        .unwrap();
    store
        .upsert_draft(acct, DraftKey::NewCreate, fields("", "New", "n"), t(2))
        .unwrap()
        .unwrap();
    // A reply to a message that is not stored joins nothing.
    store
        .upsert_draft(
            acct,
            DraftKey::Reply(424242),
            fields("", "Orphan", "o"),
            t(3),
        )
        .unwrap()
        .unwrap();
    let all = store.list_drafts(acct).unwrap();
    let by = |s: &str| all.iter().find(|d| d.subject == s).unwrap().clone();
    for d in [by("Re"), by("Fwd")] {
        assert_eq!(d.thread_id.as_deref(), Some(thread.as_str()));
        assert_eq!(d.parent_subject.as_deref(), Some(subject.as_str()));
        assert_eq!(d.parent_from_addr.as_deref(), Some(from_addr.as_str()));
        assert_eq!(d.parent_from_name, from_name);
    }
    for d in [by("New"), by("Orphan")] {
        assert!(d.thread_id.is_none());
        assert!(d.parent_subject.is_none());
        assert!(d.parent_from_addr.is_none());
        assert!(d.parent_from_name.is_none());
    }
    // The single-row read carries the same context.
    let one = store.draft_by_id(acct, by("Fwd").id).unwrap().unwrap();
    assert_eq!(one.thread_id.as_deref(), Some(thread.as_str()));
}

#[test]
fn the_parent_join_never_reads_another_accounts_message() {
    let (store, acct) = store();
    let other = store.ensure_account("other@example.com").unwrap();
    let theirs = triaged(other, "g1", "t1").ingest(&store);
    // A draft keyed by an id that exists only on another account.
    store
        .upsert_draft(acct, DraftKey::Reply(theirs), fields("", "Re", "r"), t(0))
        .unwrap()
        .unwrap();
    let d = store.list_drafts(acct).unwrap().remove(0);
    assert!(d.thread_id.is_none() && d.parent_subject.is_none() && d.parent_from_addr.is_none());
}

#[test]
fn deleting_a_new_message_draft_by_id_takes_its_files_and_spares_the_rest() {
    let (store, acct) = store();
    let a = store
        .upsert_draft(acct, DraftKey::NewCreate, fields("", "a", "a"), t(0))
        .unwrap()
        .unwrap();
    let b = store
        .upsert_draft(acct, DraftKey::NewCreate, fields("", "b", "b"), t(1))
        .unwrap()
        .unwrap();
    let fa = store
        .stage_outbound_attachment(acct, "a.pdf", "application/pdf", "cid-a", b"A", t(0))
        .unwrap();
    let fb = store
        .stage_outbound_attachment(acct, "b.pdf", "application/pdf", "cid-b", b"B", t(0))
        .unwrap();
    store
        .claim_outbound_attachments(acct, a.id, &[fa.id], t(0))
        .unwrap();
    store
        .claim_outbound_attachments(acct, b.id, &[fb.id], t(0))
        .unwrap();
    assert!(store.delete_draft(acct, a.id).unwrap());
    assert!(store.outbound_attachment(acct, fa.id).unwrap().is_none());
    assert!(store.outbound_attachment(acct, fb.id).unwrap().is_some());
    assert_eq!(store.list_drafts(acct).unwrap().len(), 1);
}

#[test]
fn delete_draft_is_account_scoped_and_reports_a_miss() {
    let (store, acct) = store();
    let other = store.ensure_account("other@example.com").unwrap();
    let mine = store
        .upsert_draft(
            acct,
            DraftKey::Reply(7),
            DraftFields {
                to_addr: "alice@example.com",
                subject: "Re: Lunch",
                body: "sure",
                ..Default::default()
            },
            t(0),
        )
        .unwrap()
        .unwrap();

    // Unknown id => false (the handler turns this into 404).
    assert!(!store.delete_draft(acct, 9999).unwrap());
    // Another account's id is indistinguishable from unknown, and the row lives.
    assert!(!store.delete_draft(other, mine.id).unwrap());
    assert_eq!(store.list_drafts(acct).unwrap().len(), 1);

    assert!(store.delete_draft(acct, mine.id).unwrap());
    assert!(store.list_drafts(acct).unwrap().is_empty());
    // Deleting twice is a miss, not an error.
    assert!(!store.delete_draft(acct, mine.id).unwrap());
}

#[test]
fn list_drafts_orders_by_updated_at_desc_and_scopes_by_account() {
    let (store, acct) = store();
    let other = store.ensure_account("other@example.com").unwrap();
    store
        .upsert_draft(
            acct,
            DraftKey::Reply(1),
            DraftFields {
                to_addr: "a@example.com",
                subject: "one",
                body: "1",
                ..Default::default()
            },
            t(0),
        )
        .unwrap()
        .unwrap();
    store
        .upsert_draft(
            acct,
            DraftKey::Reply(2),
            DraftFields {
                to_addr: "b@example.com",
                subject: "two",
                body: "2",
                ..Default::default()
            },
            t(1),
        )
        .unwrap()
        .unwrap();
    store
        .upsert_draft(
            acct,
            DraftKey::Reply(3),
            DraftFields {
                to_addr: "c@example.com",
                subject: "three",
                body: "3",
                ..Default::default()
            },
            t(2),
        )
        .unwrap()
        .unwrap();
    store
        .upsert_draft(
            other,
            DraftKey::Reply(1),
            DraftFields {
                to_addr: "z@example.com",
                subject: "theirs",
                body: "z",
                ..Default::default()
            },
            t(9),
        )
        .unwrap()
        .unwrap();

    let subjects: Vec<String> = store
        .list_drafts(acct)
        .unwrap()
        .into_iter()
        .map(|d| d.subject)
        .collect();
    assert_eq!(subjects, ["three", "two", "one"]);

    // Touching the oldest draft moves it to the front.
    store
        .upsert_draft(
            acct,
            DraftKey::Reply(1),
            DraftFields {
                to_addr: "a@example.com",
                subject: "one",
                body: "1 again",
                ..Default::default()
            },
            t(3),
        )
        .unwrap()
        .unwrap();
    let subjects: Vec<String> = store
        .list_drafts(acct)
        .unwrap()
        .into_iter()
        .map(|d| d.subject)
        .collect();
    assert_eq!(subjects, ["one", "three", "two"]);

    // The other account sees only its own.
    let theirs = store.list_drafts(other).unwrap();
    assert_eq!(theirs.len(), 1);
    assert_eq!(theirs[0].subject, "theirs");
}

#[test]
fn list_drafts_preserves_human_composition_when_external_access_is_restricted() {
    // Restricting external readers does not hide the human's own composition.
    let (store, acct) = store();
    let parent = triaged(acct, "g1", "t1").seed(&store);
    store
        .upsert_draft(
            acct,
            DraftKey::Reply(parent),
            DraftFields {
                to_addr: "alice@example.com",
                subject: "Re: Lunch",
                body: "sure",
                ..Default::default()
            },
            t(0),
        )
        .unwrap()
        .unwrap();
    store
        .upsert_draft(
            acct,
            DraftKey::NewLatest,
            DraftFields {
                to_addr: "bob@example.com",
                subject: "Hello",
                body: "hi",
                ..Default::default()
            },
            t(1),
        )
        .unwrap()
        .unwrap();
    assert_eq!(store.list_drafts(acct).unwrap().len(), 2);

    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE triage SET sensitivity = 'sealed' WHERE message_id = ?1",
            params![parent],
        )
        .unwrap();

    let left = store.list_drafts(acct).unwrap();
    assert_eq!(left.len(), 2, "both human drafts remain available");
    assert!(
        left[0].reply_to_message_id.is_none(),
        "the NULL key compares against nothing and is never filtered"
    );
}

#[test]
fn reingest_never_deletes_a_human_draft() {
    // Re-ingestion cannot delete explicit user work, whatever the access assessment.
    let (store, acct) = store();
    let normal = triaged(acct, "g1", "t1");
    let parent = normal.ingest(&store);
    store
        .upsert_draft(
            acct,
            DraftKey::Reply(parent),
            DraftFields {
                to_addr: "alice@example.com",
                subject: "Re: Lunch",
                body: "sure",
                ..Default::default()
            },
            t(0),
        )
        .unwrap()
        .unwrap();

    let again = normal.clone().sealed(SealedKind::Otp).ingest(&store);
    assert_eq!(again, parent, "same Gmail id, same local row");
    assert_eq!(
        store.list_drafts(acct).unwrap().len(),
        1,
        "re-ingest preserves the draft"
    );
}

#[test]
fn access_restrictions_preserve_human_drafts_and_their_staged_files() {
    use crate::store::agent_triage::AgentTriageStore;
    // External access restrictions must not delete explicit human work.
    let (store, acct) = store();
    let normal = triaged(acct, "g1", "t1");
    let parent = normal.ingest(&store);
    let d = store
        .upsert_draft(acct, DraftKey::Reply(parent), DraftFields::default(), t(0))
        .unwrap()
        .unwrap();
    let file = store
        .stage_outbound_attachment(acct, "a.pdf", "application/pdf", "cid-a", b"A", t(0))
        .unwrap();
    store
        .claim_outbound_attachments(acct, d.id, &[file.id], t(0))
        .unwrap();
    normal.clone().sealed(SealedKind::Otp).ingest(&store);
    assert!(
        store.outbound_attachment(acct, file.id).unwrap().is_some(),
        "re-ingest preserves the draft attachment"
    );

    // The hand-correction path, on a second message.
    let other = triaged(acct, "g2", "t2").ingest(&store);
    let d2 = store
        .upsert_draft(acct, DraftKey::Reply(other), DraftFields::default(), t(1))
        .unwrap()
        .unwrap();
    let file2 = store
        .stage_outbound_attachment(acct, "b.pdf", "application/pdf", "cid-b", b"B", t(1))
        .unwrap();
    store
        .claim_outbound_attachments(acct, d2.id, &[file2.id], t(1))
        .unwrap();
    store
        .correct_agent_triage(
            acct,
            other,
            "external_access",
            &serde_json::json!(true),
            t(2),
        )
        .unwrap();
    assert_eq!(store.list_drafts(acct).unwrap().len(), 2);
    assert!(
        store.outbound_attachment(acct, file2.id).unwrap().is_some(),
        "restricting external agents preserves the human attachment"
    );
    assert!(store.delete_draft(acct, d2.id).unwrap());
    assert!(store.outbound_attachment(acct, file2.id).unwrap().is_none());
    assert!(store.outbound_attachment(acct, file.id).unwrap().is_some());
}

#[test]
fn account_email_reads_the_row_and_404s_unknown() {
    let (store, acct) = store();
    assert_eq!(store.account_email(acct).unwrap(), "me@example.com");
    assert!(matches!(
        store.account_email(9999).unwrap_err(),
        CoreError::NotFound
    ));
}
