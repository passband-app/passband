//! The profile pass end to end over a real store and a scripted model: what it
//! is shown, what it writes, and every way it spends nothing.

use super::*;
use crate::store::SqliteStore;
use crate::store::agent_triage::AgentTriageStore as _;
use crate::store::memory::MemoryStore as _;
use crate::types::NewMessage;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

const SAM: &str = "sam@acme.example";
const INBOUND_CANARY: &str = "ospreyinboundwords";
const SEALED_CANARY: &str = "merlinsealedthread";

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn noon() -> DateTime<Utc> {
    at("2026-11-04T12:00:00Z")
}

/// What the scripted model answers.
#[derive(Clone)]
enum Answer {
    Ops(Value),
    Fail(&'static str),
}

struct Scripted {
    answer: Mutex<Answer>,
    calls: AtomicUsize,
    seen: Mutex<Vec<String>>,
}

impl Scripted {
    fn new(answer: Answer) -> Self {
        Self {
            answer: Mutex::new(answer),
            calls: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl ProfileModel for Scripted {
    async fn propose(&self, user: &str) -> ProfileCall {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(user.to_string());
        let usage = Some(Usage {
            input_tokens: 1_000,
            output_tokens: 200,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation: None,
        });
        let result = match self.answer.lock().unwrap().clone() {
            Answer::Ops(ops) => Ok(serde_json::from_value(ops).unwrap()),
            Answer::Fail(kind) => Err(kind.to_string()),
        };
        ProfileCall {
            result,
            usage: usage.into_iter().collect(),
            calls: 1,
        }
    }
}

fn message(acct: AccountId, gmail: &str, thread: &str, from: &str, body: &str) -> NewMessage {
    NewMessage {
        account_id: acct,
        gmail_msg_id: gmail.into(),
        thread_id: thread.into(),
        from_addr: from.into(),
        from_name: None,
        subject: format!("Plans {thread}"),
        received_at: noon() - Duration::days(2),
        snippet: String::new(),
        body: body.into(),
        body_html: None,
        is_sent: false,
        is_spam: false,
        to_addrs: None,
        cc_addrs: None,
        bcc_addrs: None,
        list_unsubscribe: None,
        list_unsub_one_click: false,
        auth_pass: None,
        sender_signals: None,
    }
}

struct Mailbox {
    store: SqliteStore,
    acct: AccountId,
    /// The owner's reply on the ordinary thread.
    reply: i64,
}

/// A mailbox with a twenty-day dwell ledger, one ordinary thread Sam started
/// and the owner answered, and one thread kept close (its inbound message
/// restricted by the owner) that the owner also answered.
fn mailbox() -> Mailbox {
    let store = SqliteStore::open_in_memory().unwrap();
    let acct = store.ensure_account("me@example.com").unwrap();
    store
        .set_app_setting(
            acct,
            crate::store::sqlite::DWELL_LEDGER_SINCE_KEY,
            &(noon() - Duration::days(20)).to_rfc3339(),
        )
        .unwrap();
    store
        .upsert_message(&message(
            acct,
            "in-1",
            "t1",
            SAM,
            &format!("Can we ship Friday? {INBOUND_CANARY}"),
        ))
        .unwrap();
    let reply = store
        .upsert_message(&NewMessage {
            is_sent: true,
            from_addr: "me@example.com".into(),
            to_addrs: Some(SAM.into()),
            received_at: noon() - Duration::days(1),
            ..message(
                acct,
                "out-1",
                "t1",
                "me@example.com",
                &format!(
                    "Yes, Friday works. Call me at 4155551234.\n\nOn Tue, Nov 3, 2026 at 8:00 AM Sam <sam@acme.example> wrote:\n> Can we ship Friday? {INBOUND_CANARY}"
                ),
            )
        })
        .unwrap();
    let sealed_in = store
        .upsert_message(&message(
            acct,
            "in-2",
            "t2",
            "bank@bank.example",
            "Your code",
        ))
        .unwrap();
    store
        .upsert_message(&NewMessage {
            is_sent: true,
            from_addr: "me@example.com".into(),
            received_at: noon() - Duration::days(1),
            ..message(
                acct,
                "out-2",
                "t2",
                "me@example.com",
                &format!("Thanks, {SEALED_CANARY}"),
            )
        })
        .unwrap();
    store
        .correct_agent_triage(acct, sealed_in, "external_access", &json!(true), noon())
        .unwrap();
    Mailbox { store, acct, reply }
}

fn new_role(reply: i64) -> Value {
    json!({"ops": [
        {"op": "new", "id": null, "entry": {
            "scope": "owner", "kind": "role", "what": "Ships software with a small team",
            "name": null, "note": null, "until": null, "addr": null, "relation": null,
            "basis": null, "confidence": 0.7, "message_ids": [reply], "addrs": []}},
        {"op": "new", "id": null, "entry": {
            "scope": format!("sender:{SAM}"), "kind": "person", "what": null, "name": null,
            "note": "answers within a day", "until": null, "addr": SAM,
            "relation": "cofounder", "basis": null, "confidence": 0.8,
            "message_ids": [reply], "addrs": [SAM]}},
        {"op": "new", "id": null, "entry": {
            "scope": "owner", "kind": "secrets", "what": "x", "name": null, "note": null,
            "until": null, "addr": null, "relation": null, "basis": null, "confidence": 0.5,
            "message_ids": [reply], "addrs": []}}
    ]})
}

fn on() -> Features {
    Features::pinned([features::UNDERSTANDING])
}

async fn run(mailbox: &Mailbox, now: DateTime<Utc>, model: &Scripted) -> PassOutcome {
    run_pass(
        &mailbox.store,
        mailbox.acct,
        now,
        OwnerZone::UTC,
        &on(),
        model,
    )
    .await
}

#[tokio::test]
async fn a_run_writes_valid_entries_and_bills_the_profile_line() {
    let mailbox = mailbox();
    let model = Scripted::new(Answer::Ops(new_role(mailbox.reply)));
    let outcome = run(&mailbox, noon(), &model).await;
    let PassOutcome::Applied(report) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(report.added, 2);
    assert_eq!(report.rejected, vec![(2, memory::Rejection::UnknownKind)]);

    let entries = mailbox.store.memory_entries(mailbox.acct).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|e| e.origin == Origin::Inferred));
    assert!(
        entries
            .iter()
            .all(|e| e.expires_at == Some(memory::inferred_expiry(noon())))
    );
    let person = entries.iter().find(|e| e.kind == "person").unwrap();
    assert_eq!(person.scope, format!("sender:{SAM}"));
    assert_eq!(
        person.line,
        "sam@acme.example is a cofounder; answers within a day."
    );

    let billed = mailbox
        .store
        .category_usage_since(
            mailbox.acct,
            "2026-11-04",
            crate::metrics::PROFILE_USAGE_CATEGORY,
        )
        .unwrap();
    assert_eq!((billed.calls, billed.input_tokens), (1, 1_000));
}

#[tokio::test]
async fn the_model_sees_the_owners_words_and_never_an_inbound_body_or_a_sealed_thread() {
    let mailbox = mailbox();
    let model = Scripted::new(Answer::Ops(json!({"ops": []})));
    run(&mailbox, noon(), &model).await;
    let user = model.seen.lock().unwrap()[0].clone();
    assert!(user.contains("Yes, Friday works."), "{user}");
    assert!(
        !user.contains(INBOUND_CANARY),
        "inbound body reached the pass: {user}"
    );
    assert!(
        !user.contains(SEALED_CANARY),
        "a sealed thread reached the pass: {user}"
    );
    // A long number in the owner's own words is blanked too.
    assert!(!user.contains("4155551234"), "{user}");
    let input: Value = serde_json::from_str(&user).unwrap();
    assert_eq!(input["replied_threads"].as_array().unwrap().len(), 1);
    assert_eq!(input["replied_threads"][0]["message_id"], mailbox.reply);
    assert_eq!(input["replied_threads"][0]["participants"], json!([SAM]));
    assert!(user.len() <= MAX_INPUT_BYTES);
}

/// THE BUDGET TEST (docs/UNDERSTANDING.md §7): a second run in one UTC day is
/// refused BEFORE any model call, even when the owner-local day stamp says
/// otherwise (a zone change, a cleared setting, a restarted daemon).
#[tokio::test]
async fn a_second_run_in_one_utc_day_is_refused_without_a_model_call() {
    let mailbox = mailbox();
    let model = Scripted::new(Answer::Ops(new_role(mailbox.reply)));
    assert!(matches!(
        run(&mailbox, noon(), &model).await,
        PassOutcome::Applied(_)
    ));
    assert_eq!(model.calls(), 1);

    // The same owner-local day: not even due.
    assert_eq!(
        run(&mailbox, noon() + Duration::hours(2), &model).await,
        PassOutcome::NotDue
    );
    // The local-day stamp gone, and something new to read: the UTC-day rule
    // alone still refuses, and the model is not called.
    mailbox
        .store
        .delete_app_setting(mailbox.acct, DAY_KEY)
        .unwrap();
    assert_eq!(
        run(&mailbox, noon() + Duration::hours(3), &model).await,
        PassOutcome::AlreadyRan
    );
    assert_eq!(model.calls(), 1);
}

#[tokio::test]
async fn failed_attempts_are_capped_per_utc_day_and_config_failures_refunded() {
    let mailbox = mailbox();
    let model = Scripted::new(Answer::Fail("transport"));
    for _ in 0..ATTEMPTS_PER_DAY {
        assert_eq!(
            run(&mailbox, noon(), &model).await,
            PassOutcome::Failed {
                kind: "transport".into()
            }
        );
    }
    assert_eq!(run(&mailbox, noon(), &model).await, PassOutcome::AlreadyRan);
    assert_eq!(model.calls() as u32, ATTEMPTS_PER_DAY);
    // Every attempt was billed as a call.
    let billed = mailbox
        .store
        .category_usage_since(
            mailbox.acct,
            "2026-11-04",
            crate::metrics::PROFILE_USAGE_CATEGORY,
        )
        .unwrap();
    assert_eq!(billed.calls as u32, ATTEMPTS_PER_DAY);

    // A rejected key spends nothing and gives its attempt back.
    let mailbox = self::mailbox();
    let model = Scripted::new(Answer::Fail("http_401"));
    for _ in 0..ATTEMPTS_PER_DAY + 1 {
        run(&mailbox, noon(), &model).await;
    }
    assert_eq!(model.calls() as u32, ATTEMPTS_PER_DAY + 1);
}

#[tokio::test]
async fn nothing_is_spent_off_early_young_or_unchanged() {
    let mailbox = mailbox();
    let model = Scripted::new(Answer::Ops(json!({"ops": []})));
    // Off.
    assert_eq!(
        run_pass(
            &mailbox.store,
            mailbox.acct,
            noon(),
            OwnerZone::UTC,
            &Features::none(),
            &model
        )
        .await,
        PassOutcome::Off
    );
    // Before the owner's 03:00.
    assert_eq!(
        run(&mailbox, at("2026-11-04T02:59:00Z"), &model).await,
        PassOutcome::NotDue
    );
    // A young ledger.
    let young = self::mailbox();
    young
        .store
        .set_app_setting(
            young.acct,
            crate::store::sqlite::DWELL_LEDGER_SINCE_KEY,
            &(noon() - Duration::days(3)).to_rfc3339(),
        )
        .unwrap();
    assert!(matches!(
        run(&young, noon(), &model).await,
        PassOutcome::NotEnoughCoverage { .. }
    ));
    assert_eq!(model.calls(), 0);

    // A run, then the next day with nothing new: unchanged, no call.
    assert!(matches!(
        run(&mailbox, noon(), &model).await,
        PassOutcome::Applied(_)
    ));
    assert_eq!(
        run(&mailbox, noon() + Duration::days(1), &model).await,
        PassOutcome::Unchanged
    );
    assert_eq!(model.calls(), 1);
}

#[tokio::test]
async fn a_stated_entry_survives_whatever_the_pass_says() {
    let mailbox = mailbox();
    let stated = MemoryRecord {
        id: memory::new_ulid(noon()),
        scope: "owner".into(),
        origin: Origin::Stated,
        kind: "role".into(),
        body: json!({"what": "Runs a studio"}),
        line: "Runs a studio.".into(),
        provenance: memory::Provenance::default(),
        confidence: 1.0,
        revision: 1,
        created_at: noon(),
        updated_at: noon(),
        affirmed_at: noon(),
        expires_at: None,
        retired_at: None,
        deleted_at: None,
    };
    mailbox
        .store
        .insert_memory_entry_unchecked(mailbox.acct, &stated)
        .unwrap();
    let model = Scripted::new(Answer::Ops(json!({"ops": [
        {"op": "retire", "id": stated.id, "entry": null}
    ]})));
    let PassOutcome::Applied(report) = run(&mailbox, noon(), &model).await else {
        panic!()
    };
    assert_eq!(report.rejected, vec![(0, memory::Rejection::TouchesStated)]);
    let entries = mailbox.store.memory_entries(mailbox.acct).unwrap();
    assert_eq!(entries[0].retired_at, None);
    assert_eq!(entries[0].origin, Origin::Stated);
}

#[tokio::test]
async fn an_entry_close_to_lapsing_is_a_change() {
    let mailbox = mailbox();
    let model = Scripted::new(Answer::Ops(new_role(mailbox.reply)));
    run(&mailbox, noon(), &model).await;
    // 80 days on, nothing new arrived, but every entry lapses within 14 days.
    let later = noon() + Duration::days(80);
    let keep = |id: &str| json!({"op": "keep", "id": id, "entry": null});
    let ids: Vec<Value> = mailbox
        .store
        .memory_entries(mailbox.acct)
        .unwrap()
        .iter()
        .map(|e| keep(&e.id))
        .collect();
    *model.answer.lock().unwrap() = Answer::Ops(json!({ "ops": ids }));
    let PassOutcome::Applied(report) = run(&mailbox, later, &model).await else {
        panic!()
    };
    assert_eq!(report.kept, 2);
    assert!(
        mailbox
            .store
            .memory_entries(mailbox.acct)
            .unwrap()
            .iter()
            .all(|e| e.expires_at == Some(memory::inferred_expiry(later)))
    );
}

#[test]
fn owner_words_cut_the_quote_and_skip_forwards() {
    assert_eq!(
        owner_words("Sure, see you then.\n\nOn Mon, Nov 2, 2026 at 9:14 AM Jo <jo@x.example> wrote:\n> are you free?").as_deref(),
        Some("Sure, see you then.")
    );
    assert_eq!(
        owner_words("FYI\n---------- Forwarded message ---------\nFrom: x"),
        None
    );
    assert_eq!(
        owner_words("Try https://evil.example/reset and code 123456").as_deref(),
        Some("Try [link] and code [number]")
    );
}

#[test]
fn the_prompt_states_the_cap_the_validator_enforces() {
    let prompt = system_prompt();
    assert!(prompt.contains(&format!("At most {} entries", memory::MAX_LIVE_ENTRIES)));
    assert!(!prompt.contains("{max_live_entries}"));
    assert!(prompt.contains("from their own behaviour"));
    assert!(prompt.contains("Never include a code, a link, a password"));
}
