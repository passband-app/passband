//! A small mailbox on a temp-file store, staged through the store's own APIs
//! and real decision commits, with timestamps then rewound to a fixed past.
//!
//! t1: A (ingest t0, decided t0+10m), B (t0+1h, gold t0+2h), C (t0+3h,
//! gold t0+3h30m). t2: D (t0+4h, gold t0+5h) with one rule and the contact
//! dated after its run. t3: E, triaged twice, and t4: S, moved to spam
//! after triage (both ineligible). t5: F (t0+4h) triaged twice, then G
//! (t0+4h30m, gold t0+5h30m): G's thread holds a message whose earlier
//! decision is lost, so G is suspect.
use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, params};
use squelch_core::store::agent_triage::{AgentCommitOutcome, AgentTriageStore};
use squelch_core::store::{ContactEntry, SqliteStore, Store};
use squelch_core::triage::decision::{
    AccessAssessment, AttentionAction, EmailKind, EvidenceRef, MessageDecision, MessageDestination,
    ThreadAttentionDecision,
};
use squelch_core::types::{Disposition, NewMessage};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Fixture {
    pub dir: PathBuf,
    pub db: PathBuf,
    pub t0: DateTime<Utc>,
    pub ids: BTreeMap<&'static str, i64>,
    pub kept_rule: i64,
    pub dropped_rule: i64,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "squelch-eval-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn ts(at: DateTime<Utc>) -> String {
    at.to_rfc3339()
}

/// Gold text long enough for the leak check, unique per message.
pub fn gold(id: i64, tag: &str, fye: bool) -> MessageDecision {
    let evidence = vec![EvidenceRef {
        message_id: id,
        location: "body".into(),
    }];
    MessageDecision {
        kinds: vec![EmailKind::General],
        destinations: if fye {
            vec![]
        } else {
            vec![MessageDestination::Reading]
        },
        summary: format!("Gold summary for {tag}, which must never leak"),
        reason: format!("Gold reason for {tag}, written by the incumbent model"),
        external_access: AccessAssessment {
            restricted: false,
            reason: "No credentials".into(),
            evidence: evidence.clone(),
        },
        attention: ThreadAttentionDecision {
            show_in_fye: fye,
            summary: format!("attention after {tag}"),
            actions: vec![AttentionAction {
                id: None,
                description: format!("follow up on {tag}"),
                evidence: evidence.clone(),
                ..Default::default()
            }],
            relevant_message_ids: vec![id],
            evidence,
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn build() -> Fixture {
    let dir = temp_dir("fixture");
    let db = dir.join("live.db");
    let t0: DateTime<Utc> = "2026-09-21T10:00:00Z".parse().unwrap();
    let h = Duration::hours;
    let m = Duration::minutes;
    let store = SqliteStore::open(&db).unwrap();
    let account = store.ensure_account("owner@example.com").unwrap();
    let conn = Connection::open(&db).unwrap();
    let mut ids = BTreeMap::new();
    for (name, thread, from, created) in [
        ("A", "t1", "alice@a.example", t0),
        ("B", "t1", "alice@a.example", t0 + h(1)),
        ("C", "t1", "alice@a.example", t0 + h(3)),
        ("D", "t2", "d@d.example", t0 + h(4)),
        ("E", "t3", "e@e.example", t0 + h(4)),
        ("S", "t4", "s@s.example", t0 + h(4)),
        ("F", "t5", "f@f.example", t0 + h(4)),
        ("G", "t5", "f@f.example", t0 + h(4) + m(30)),
    ] {
        let id = store
            .upsert_message(&NewMessage {
                account_id: account,
                gmail_msg_id: format!("gmail-{name}"),
                thread_id: thread.into(),
                from_addr: from.into(),
                from_name: None,
                subject: format!("Subject {name}"),
                received_at: created - m(5),
                snippet: String::new(),
                body: format!("Message {name}. {}", "lorem ipsum dolor ".repeat(1100)),
                body_html: Some(format!("<p>html {name}</p>")),
                is_sent: false,
                is_spam: false,
                to_addrs: None,
                cc_addrs: None,
                bcc_addrs: None,
                list_unsubscribe: None,
                list_unsub_one_click: false,
                auth_pass: None,
            })
            .unwrap();
        conn.execute(
            "INSERT INTO triage(account_id,message_id,created_at) VALUES(?1,?2,?3)",
            params![account, id, ts(created)],
        )
        .unwrap();
        ids.insert(name, id);
    }
    let kept_rule = store
        .set_sender_rule(account, "*@d.example", "domain mail", Disposition::Surface)
        .unwrap();
    let dropped_rule = store
        .set_sender_rule(account, "d@d.example", "this sender", Disposition::Surface)
        .unwrap();
    conn.execute(
        "UPDATE sender_rules SET updated_at=?1 WHERE id=?2",
        params![ts(t0), kept_rule],
    )
    .unwrap();
    conn.execute(
        "UPDATE sender_rules SET updated_at=?1 WHERE id=?2",
        params![ts(t0 + h(6)), dropped_rule],
    )
    .unwrap();
    let contact = |addr: &str| ContactEntry {
        addr: addr.into(),
        display_name: None,
        sent_count: 1,
        last_sent_at: None,
    };
    store
        .merge_harvested_contacts(
            account,
            &[contact("alice@a.example"), contact("d@d.example")],
        )
        .unwrap();
    conn.execute(
        "UPDATE contacts SET first_seen=?1 WHERE addr='alice@a.example'",
        [ts(t0 - Duration::days(1))],
    )
    .unwrap();
    conn.execute(
        "UPDATE contacts SET first_seen=?1 WHERE addr='d@d.example'",
        [ts(t0 + h(6))],
    )
    .unwrap();
    // Gold runs in production order. Tokens sized so every case's prompt is
    // about 4 bytes per token (bodies are ~20k bytes, siblings ride along).
    let stage = |name: &str, trigger: &str, at: DateTime<Utc>, tokens: u64, fye: bool| {
        let id = ids[name];
        store
            .enqueue_agent_triage(account, id, trigger, false)
            .unwrap();
        let job = store
            .claim_agent_job(account, "investigation", Utc::now(), 60)
            .unwrap()
            .unwrap();
        assert_eq!(job.message_id, id);
        let mut context = store.load_agent_context(&job).unwrap();
        context.run_metadata = serde_json::json!({
            "model": "claude-opus-5-5", "prompt_version": "agent-triage-v5",
            "model_turns": 1, "tool_calls": 0,
            "usage": [{"input_tokens": tokens, "output_tokens": 400,
                "cache_read_input_tokens": 6000, "cache_creation_input_tokens": 0}],
        });
        assert_eq!(
            store
                .commit_agent_decision(&job, &context, &gold(id, name, fye), &[])
                .unwrap(),
            AgentCommitOutcome::Applied
        );
        conn.execute(
            "UPDATE agent_triage_runs SET completed_at=?1 WHERE id=(SELECT MAX(id) FROM agent_triage_runs WHERE message_id=?2)",
            params![ts(at), id],
        )
        .unwrap();
        // Production stamps the decision just before the run completes.
        conn.execute(
            "UPDATE agent_message_decisions SET decided_at=?1 WHERE message_id=?2",
            params![ts(at - Duration::milliseconds(1)), id],
        )
        .unwrap();
    };
    stage("A", "arrival", t0 + m(10), 5_000, true);
    stage("B", "arrival", t0 + h(2), 10_000, true);
    stage("C", "arrival", t0 + h(3) + m(30), 15_000, true);
    stage("D", "arrival", t0 + h(5), 5_000, false);
    stage("E", "arrival", t0 + h(5), 5_000, false);
    stage("E", "manual:again", t0 + h(6), 5_000, false);
    stage("F", "arrival", t0 + h(4) + m(10), 5_000, false);
    stage("F", "manual:again", t0 + h(4) + m(20), 5_000, false);
    stage("G", "arrival", t0 + h(5) + m(30), 10_000, false);
    stage("S", "arrival", t0 + h(5), 5_000, false);
    // Moved to spam after it was triaged (spam at ingest only ever gets an
    // access job, never a triage run).
    conn.execute("UPDATE messages SET is_spam=1 WHERE id=?1", [ids["S"]])
        .unwrap();
    // User state as it stands now; each stamp lands after some case's T.
    conn.execute(
        "UPDATE triage SET status='done',resolved_at=?1 WHERE message_id=?2",
        params![ts(t0 + m(90)), ids["A"]],
    )
    .unwrap();
    conn.execute(
        "UPDATE triage SET status='open',surfaced_at=?1,opened_at=?2 WHERE message_id=?3",
        params![ts(t0 + h(3)), ts(t0 + m(150)), ids["B"]],
    )
    .unwrap();
    drop(conn);
    drop(store);
    Fixture {
        dir,
        db,
        t0,
        ids,
        kept_rule,
        dropped_rule,
    }
}
