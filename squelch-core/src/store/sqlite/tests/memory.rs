//! `memory_entries` at the SQL level: what an owner delete leaves behind.

use super::super::*;
use super::support::*;
use crate::triage::memory::{MemoryRecord, Origin, Provenance, new_ulid};
use serde_json::json;

#[test]
fn an_owner_delete_scrubs_the_words_and_the_receipts() {
    let (store, acct) = store();
    let now = Utc::now();
    let id = new_ulid(now);
    store
        .insert_memory_entry_unchecked(
            acct,
            &MemoryRecord {
                id: id.clone(),
                scope: "sender:sam@acme.example".into(),
                origin: Origin::Inferred,
                kind: "person".into(),
                body: json!({"addr": "sam@acme.example", "relation": "friend", "note": "close"}),
                line: "sam@acme.example is a friend; close.".into(),
                provenance: Provenance {
                    message_ids: vec![7],
                    addrs: vec!["sam@acme.example".into()],
                },
                confidence: 0.8,
                revision: 1,
                created_at: now,
                updated_at: now,
                affirmed_at: now,
                expires_at: None,
                retired_at: None,
                deleted_at: None,
            },
        )
        .unwrap();
    assert!(store.delete_memory_entry(acct, &id, now).unwrap());
    let (scope, line, body, provenance, deleted_at): (
        String,
        String,
        String,
        String,
        Option<String>,
    ) = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT scope, line, body, provenance, deleted_at FROM memory_entries
             WHERE account_id = ?1 AND id = ?2",
            params![acct, id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    // The address lived in the scope too.
    assert_eq!(
        (
            scope.as_str(),
            line.as_str(),
            body.as_str(),
            provenance.as_str()
        ),
        ("", "", "{}", "{}")
    );
    assert!(deleted_at.is_some());
    // Gone to every reader, and a second delete finds nothing.
    assert!(store.list_memory_entries(acct).unwrap().is_empty());
    assert!(!store.delete_memory_entry(acct, &id, now).unwrap());
}
