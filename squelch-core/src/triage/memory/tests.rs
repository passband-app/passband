//! The typed entries, the validator, and what a triage run is served.

use super::*;
use crate::store::SqliteStore;
use crate::store::memory::MemoryStore as _;

fn at_utc(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-04T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn record(id: &str, origin: Origin, kind: &str, line: &str, confidence: f64) -> MemoryRecord {
    let at = now() - Duration::days(1);
    MemoryRecord {
        id: id.into(),
        scope: "owner".into(),
        origin,
        kind: kind.into(),
        body: json!({"what": line}),
        line: line.into(),
        provenance: Provenance {
            message_ids: vec![],
            addrs: vec!["sam@acme.example".into()],
        },
        confidence,
        revision: 1,
        created_at: at,
        updated_at: at,
        affirmed_at: at,
        expires_at: (origin == Origin::Inferred).then(|| inferred_expiry(at)),
        retired_at: None,
        deleted_at: None,
    }
}

fn role(what: &str) -> RawEntry {
    RawEntry {
        scope: "owner".into(),
        kind: "role".into(),
        what: Some(what.into()),
        confidence: 0.7,
        message_ids: vec![7],
        ..Default::default()
    }
}

fn new_op(entry: RawEntry) -> RawOp {
    RawOp {
        op: "new".into(),
        id: None,
        entry: Some(entry),
    }
}

fn op(kind: &str, id: &str, entry: Option<RawEntry>) -> RawOp {
    RawOp {
        op: kind.into(),
        id: Some(id.into()),
        entry,
    }
}

struct World {
    existing: Vec<MemoryRecord>,
    known: BTreeSet<String>,
    owned: BTreeSet<i64>,
    exposed: BTreeSet<i64>,
}

impl World {
    fn new() -> Self {
        Self {
            existing: Vec::new(),
            known: [
                "sam@acme.example".to_string(),
                "news@vendor.example".to_string(),
            ]
            .into_iter()
            .collect(),
            owned: [7, 8, 9].into_iter().collect(),
            exposed: [7, 8].into_iter().collect(),
        }
    }
    fn validate(&self, ops: &[RawOp]) -> Validated {
        validate_ops(
            ops,
            &Facts {
                existing: &self.existing,
                known_addrs: &self.known,
                owned_ids: &self.owned,
                exposed_ids: &self.exposed,
                now: now(),
                today: now().date_naive(),
            },
        )
    }
    fn rejects(&self, ops: &[RawOp]) -> Rejection {
        let out = self.validate(ops);
        assert!(out.ops.is_empty(), "accepted: {:?}", out.ops);
        assert_eq!(out.rejected.len(), 1, "{:?}", out.rejected);
        out.rejected[0].1.clone()
    }
}

// ---- rendering ------------------------------------------------------------

#[test]
fn every_kind_renders_one_sentence_from_its_fields() {
    let today = now().date_naive();
    let line = |kind: &str, fields: Value| {
        MemoryBody::from_fields(kind, &fields, today)
            .unwrap()
            .render_line()
    };
    assert_eq!(
        line("role", json!({"what": "runs a small software company"})),
        "Runs a small software company."
    );
    assert_eq!(
        line(
            "project",
            json!({"name": "the seed round", "note": "investor and lawyer mail is live",
                   "until": "2026-11-30"})
        ),
        "Working on the seed round: investor and lawyer mail is live (through 2026-11-30)."
    );
    assert_eq!(
        line(
            "person",
            json!({"addr": "Sam <SAM@acme.example>", "relation": "cofounder",
                   "note": "answered within the hour on 31 of 33 threads"})
        ),
        "sam@acme.example is a cofounder; answered within the hour on 31 of 33 threads."
    );
    assert_eq!(
        line(
            "ignores",
            json!({"what": "product update mail from software vendors", "basis": "opened 2 of 38"})
        ),
        "Ignores product update mail from software vendors (opened 2 of 38)."
    );
    assert_eq!(
        line("prefers", json!({"what": "short replies over long ones"})),
        "Prefers short replies over long ones."
    );
}

#[test]
fn model_fields_lose_their_dashes_and_control_characters() {
    let body = MemoryBody::from_fields(
        "role",
        &json!({"what": "Runs a studio \u{2014} mostly\nclients\u{2013}hiring"}),
        now().date_naive(),
    )
    .unwrap();
    let line = body.render_line();
    assert_eq!(line, "Runs a studio, mostly clients-hiring.");
    assert!(!line.contains('\u{2014}') && !line.contains('\u{2013}'));
}

// ---- the validator ----------------------------------------------------------

#[test]
fn a_valid_new_entry_is_accepted_with_its_receipts() {
    let world = World::new();
    let out = world.validate(&[new_op(role("Runs a small design studio"))]);
    assert!(out.rejected.is_empty(), "{:?}", out.rejected);
    let ValidOp::New { entry } = &out.ops[0] else {
        panic!("{:?}", out.ops)
    };
    assert_eq!(entry.scope, "owner");
    assert_eq!(entry.provenance.message_ids, vec![7]);
}

#[test]
fn an_unknown_kind_is_rejected() {
    let mut entry = role("x");
    entry.kind = "secret".into();
    assert_eq!(
        World::new().rejects(&[new_op(entry)]),
        Rejection::UnknownKind
    );
}

#[test]
fn an_unknown_op_is_rejected() {
    let mut raw = new_op(role("Runs a studio"));
    raw.op = "promote".into();
    assert_eq!(World::new().rejects(&[raw]), Rejection::UnknownOp);
}

#[test]
fn an_over_cap_field_is_rejected() {
    let long = "a".repeat(MAX_WHAT_CHARS + 1);
    assert_eq!(
        World::new().rejects(&[new_op(role(&long))]),
        Rejection::FieldOverCap { field: "what" }
    );
    let mut person = RawEntry {
        scope: "sender:sam@acme.example".into(),
        kind: "person".into(),
        addr: Some("sam@acme.example".into()),
        relation: Some("cofounder".into()),
        note: Some("n".repeat(MAX_NOTE_CHARS + 1)),
        confidence: 0.9,
        message_ids: vec![7],
        ..Default::default()
    };
    assert_eq!(
        World::new().rejects(&[new_op(person.clone())]),
        Rejection::FieldOverCap { field: "note" }
    );
    person.note = None;
    person.relation = Some("lover".into());
    assert_eq!(
        World::new().rejects(&[new_op(person)]),
        Rejection::BadRelation
    );
}

#[test]
fn links_codes_and_credentials_are_rejected() {
    for what in [
        "Reads https://example.com daily",
        "Often visits www.example.com",
        "Was sent code 482913",
        "Keeps the password in mail",
        "Was sent 123 456",
        "Was sent 123-456",
        "Reads the OTP mail",
        "Says my pin is 123456",
        "Says the PIN is 4821",
        "Calls +1 (415) 555-0100",
        "Was sent 1234-5678",
        "Was sent 4821-0093",
        "Was sent token 48213",
        "Was sent a one-time code 3391",
        "Gave the security code 774 219",
        "Sends people to acme.example/reset",
        "Visits evil.example/reset often",
    ] {
        assert_eq!(
            World::new().rejects(&[new_op(role(what))]),
            Rejection::ForbiddenContent { field: "what" },
            "{what}"
        );
    }
}

#[test]
fn ordinary_prose_is_not_mistaken_for_a_secret() {
    for what in [
        "Runs a small studio with 3 to 5 people",
        "Spinning up a new team in 2026",
        "Answers within a day; prefers mornings",
        "Works with acme.example and its partners",
        "Splits time 50/50 between hiring and sales",
        "Pin this",
        "Closing the round by 2026-11-30",
        "Hiring for the 2025-26 season",
        "Runs the 2025-2026 budget",
        "Works in Node.js/React",
        "Works 9.30-17.30",
        "Open 10.00-18.00",
        "Raised 1.000.000 this year",
        "Raised 1 000 000 this year",
        "Holds ISO 27001 2022",
        "Works on token economics",
        "A one-time donor",
        "Gets a security code daily",
    ] {
        assert!(!forbidden_content(what), "{what}");
    }
}

fn project(until: &str) -> RawEntry {
    RawEntry {
        scope: "owner".into(),
        kind: "project".into(),
        name: Some("the launch".into()),
        until: Some(until.into()),
        confidence: 0.7,
        message_ids: vec![7],
        ..Default::default()
    }
}

#[test]
fn a_project_cannot_end_before_the_owners_today() {
    // `now()` is 12:00 UTC on 4 November.
    assert_eq!(
        World::new().rejects(&[new_op(project("2026-11-03"))]),
        Rejection::BadDate
    );
    assert!(
        World::new()
            .validate(&[new_op(project("2026-11-04"))])
            .rejected
            .is_empty()
    );
    // The OWNER'S today: at 02:00 UTC on the 5th it is still the 4th in
    // California, so a project through the 4th is accepted there.
    let world = World::new();
    let late = at_utc("2026-11-05T02:00:00Z");
    let facts = |today| Facts {
        existing: &world.existing,
        known_addrs: &world.known,
        owned_ids: &world.owned,
        exposed_ids: &world.exposed,
        now: late,
        today,
    };
    let ops = [new_op(project("2026-11-04"))];
    let pacific = late
        .with_timezone(&chrono_tz::America::Los_Angeles)
        .date_naive();
    assert!(validate_ops(&ops, &facts(pacific)).rejected.is_empty());
    assert_eq!(
        validate_ops(&ops, &facts(late.date_naive())).rejected,
        vec![(0, Rejection::BadDate)]
    );
}

#[test]
fn a_finished_project_is_not_live_and_frees_its_place_under_the_cap() {
    let mut done = record("01P", Origin::Inferred, "project", "Working on x.", 0.5);
    done.body = json!({"name": "x", "note": "", "until": "2026-11-03"});
    assert!(!done.is_live(now(), now().date_naive()));
    // Still live on the owner's 3 November.
    assert!(done.is_live(now(), "2026-11-03".parse().unwrap()));

    let mut world = World::new();
    for i in 0..MAX_LIVE_ENTRIES - 1 {
        world.existing.push(record(
            &format!("01E{i:02}"),
            Origin::Inferred,
            "role",
            "x.",
            0.5,
        ));
    }
    world.existing.push(done);
    let out = world.validate(&[new_op(role("One more"))]);
    assert!(out.rejected.is_empty(), "{:?}", out.rejected);
}

#[test]
fn a_foreign_unexposed_or_sealed_message_id_is_rejected() {
    let world = World::new();
    // 99: not this account's (or kept close): absent from `owned`.
    let mut entry = role("Runs a studio");
    entry.message_ids = vec![99];
    assert_eq!(world.rejects(&[new_op(entry)]), Rejection::ForeignMessage);
    // 9: owned, but this run never showed it to the model.
    let mut entry = role("Runs a studio");
    entry.message_ids = vec![9];
    assert_eq!(world.rejects(&[new_op(entry)]), Rejection::ForeignMessage);
}

#[test]
fn an_addr_not_in_the_ledger_is_rejected() {
    let world = World::new();
    let person = RawEntry {
        scope: "sender:stranger@else.example".into(),
        kind: "person".into(),
        addr: Some("stranger@else.example".into()),
        relation: Some("friend".into()),
        confidence: 0.6,
        message_ids: vec![7],
        ..Default::default()
    };
    assert_eq!(world.rejects(&[new_op(person)]), Rejection::UnknownAddr);
    let mut entry = role("Runs a studio");
    entry.addrs = vec!["stranger@else.example".into()];
    assert_eq!(world.rejects(&[new_op(entry)]), Rejection::UnknownAddr);
    let mut entry = role("Runs a studio");
    entry.scope = "domain:else.example".into();
    assert_eq!(world.rejects(&[new_op(entry)]), Rejection::UnknownAddr);
}

#[test]
fn a_person_must_be_scoped_to_their_own_address() {
    let person = RawEntry {
        scope: "owner".into(),
        kind: "person".into(),
        addr: Some("sam@acme.example".into()),
        relation: Some("cofounder".into()),
        confidence: 0.9,
        message_ids: vec![7],
        ..Default::default()
    };
    assert_eq!(
        World::new().rejects(&[new_op(person)]),
        Rejection::ScopeMismatch
    );
}

#[test]
fn an_entry_without_receipts_or_with_a_wild_confidence_is_rejected() {
    let mut entry = role("Runs a studio");
    entry.message_ids.clear();
    assert_eq!(
        World::new().rejects(&[new_op(entry)]),
        Rejection::NoProvenance
    );
    let mut entry = role("Runs a studio");
    entry.confidence = 1.5;
    assert_eq!(
        World::new().rejects(&[new_op(entry)]),
        Rejection::BadConfidence
    );
}

#[test]
fn the_pass_cannot_touch_a_stated_entry() {
    let mut world = World::new();
    world.existing.push(record(
        "01STATED",
        Origin::Stated,
        "role",
        "Runs a studio.",
        1.0,
    ));
    for raw in [
        op("keep", "01STATED", None),
        op("retire", "01STATED", None),
        op("revise", "01STATED", Some(role("Runs a bakery"))),
    ] {
        assert_eq!(world.rejects(&[raw]), Rejection::TouchesStated);
    }
}

#[test]
fn keep_revise_and_retire_need_a_live_inferred_entry_once() {
    let mut world = World::new();
    world.existing.push(record(
        "01A",
        Origin::Inferred,
        "role",
        "Runs a studio.",
        0.6,
    ));
    let mut retired = record("01B", Origin::Inferred, "role", "Ran a shop.", 0.6);
    retired.retired_at = Some(now() - Duration::days(3));
    world.existing.push(retired);

    assert_eq!(
        world.rejects(&[op("keep", "01NOPE", None)]),
        Rejection::UnknownEntry
    );
    assert_eq!(
        world.rejects(&[op("keep", "01B", None)]),
        Rejection::UnknownEntry
    );
    let out = world.validate(&[op("keep", "01A", None), op("retire", "01A", None)]);
    assert_eq!(out.ops, vec![ValidOp::Keep { id: "01A".into() }]);
    assert_eq!(out.rejected, vec![(1, Rejection::Duplicate)]);

    let mut prefers = role("short replies");
    prefers.kind = "prefers".into();
    assert_eq!(
        world.rejects(&[op("revise", "01A", Some(prefers))]),
        Rejection::KindChanged
    );
    assert_eq!(
        world.rejects(&[op("revise", "01A", None)]),
        Rejection::MissingEntry
    );
}

#[test]
fn a_failed_revise_does_not_spend_the_entrys_one_op() {
    let mut world = World::new();
    world.existing.push(record(
        "01A",
        Origin::Inferred,
        "role",
        "Runs a studio.",
        0.6,
    ));
    let bad = role(&"x".repeat(MAX_WHAT_CHARS + 1));
    let out = world.validate(&[op("revise", "01A", Some(bad)), op("keep", "01A", None)]);
    assert_eq!(out.ops, vec![ValidOp::Keep { id: "01A".into() }]);
    assert_eq!(
        out.rejected,
        vec![(0, Rejection::FieldOverCap { field: "what" })]
    );
}

#[test]
fn a_revise_may_recite_its_own_receipts_but_nothing_else_unseen() {
    let mut world = World::new();
    let mut existing = record("01A", Origin::Inferred, "role", "Runs a studio.", 0.6);
    existing.provenance.message_ids = vec![9];
    world.existing.push(existing);
    let mut entry = role("Runs a bigger studio");
    entry.message_ids = vec![9];
    let out = world.validate(&[op("revise", "01A", Some(entry))]);
    assert!(out.rejected.is_empty(), "{:?}", out.rejected);
    // A `new` citing the same id is refused: it was not shown this run.
    let mut entry = role("Hires designers");
    entry.message_ids = vec![9];
    assert_eq!(world.rejects(&[new_op(entry)]), Rejection::ForeignMessage);
}

#[test]
fn no_more_than_forty_entries_are_ever_live() {
    let mut world = World::new();
    for i in 0..MAX_LIVE_ENTRIES - 1 {
        world.existing.push(record(
            &format!("01E{i:02}"),
            Origin::Inferred,
            "role",
            "x.",
            0.5,
        ));
    }
    // An expired and a deleted entry do not count.
    let mut expired = record("01X1", Origin::Inferred, "role", "x.", 0.5);
    expired.expires_at = Some(now() - Duration::days(1));
    world.existing.push(expired);
    let mut deleted = record("01X2", Origin::Stated, "role", "x.", 0.5);
    deleted.deleted_at = Some(now() - Duration::days(1));
    world.existing.push(deleted);

    let out = world.validate(&[new_op(role("One more")), new_op(role("And another"))]);
    assert_eq!(out.ops.len(), 1);
    assert_eq!(out.rejected, vec![(1, Rejection::OverLiveCap)]);

    // A retire in the same document makes room.
    let out = world.validate(&[
        new_op(role("One more")),
        new_op(role("And another")),
        op("retire", "01E00", None),
    ]);
    assert!(out.rejected.is_empty(), "{:?}", out.rejected);
    assert_eq!(out.ops.len(), 3);
}

#[test]
fn one_bad_op_does_not_sink_the_document() {
    let mut world = World::new();
    world.existing.push(record(
        "01A",
        Origin::Inferred,
        "role",
        "Runs a studio.",
        0.6,
    ));
    let mut bad = role("x");
    bad.kind = "nope".into();
    let out = world.validate(&[new_op(bad), op("keep", "01A", None)]);
    assert_eq!(out.ops, vec![ValidOp::Keep { id: "01A".into() }]);
    assert_eq!(out.rejected, vec![(0, Rejection::UnknownKind)]);
}

#[test]
fn nothing_the_pass_writes_can_come_out_stated() {
    // A document cannot carry an origin at all: the raw entry has no field
    // for one, and a stray key is ignored by the parse.
    let parsed: ProfileOps = serde_json::from_value(json!({"ops": [{
        "op": "new", "id": null,
        "entry": {"scope": "owner", "kind": "role", "what": "Runs a studio", "origin": "stated",
                  "confidence": 0.5, "message_ids": [7], "addrs": []}
    }]}))
    .unwrap();
    let out = World::new().validate(&parsed.ops);
    assert!(matches!(&out.ops[..], [ValidOp::New { .. }]));
}

// ---- ids --------------------------------------------------------------------

#[test]
fn ulids_are_26_crockford_characters_and_sort_by_time() {
    let a = new_ulid(now());
    let b = new_ulid(now() + Duration::milliseconds(1));
    assert_eq!(a.len(), 26);
    assert!(
        a.chars()
            .all(|c| "0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(c))
    );
    assert!(a < b);
    assert_ne!(new_ulid(now()), new_ulid(now()));
    assert!(is_ulid(&a));
    for bad in [
        "",
        "01NOSUCHENTRY",
        &a.to_lowercase(),
        &format!("{a}X"),
        "0123456789ABCDEFGHJKMNPQRI",
    ] {
        assert!(!is_ulid(bad), "{bad}");
    }
}

// ---- serving ----------------------------------------------------------------

#[test]
fn stated_lines_come_first_then_by_confidence() {
    let rows = vec![
        (record("01A", Origin::Inferred, "role", "Low.", 0.3), None),
        (record("01B", Origin::Inferred, "role", "High.", 0.9), None),
        (record("01C", Origin::Stated, "role", "Mine.", 0.2), None),
    ];
    let served = select_for_context(rows, now(), now().date_naive());
    let lines: Vec<&str> = served.iter().map(|e| e.text.as_str()).collect();
    assert_eq!(lines, ["Mine.", "High.", "Low."]);
    assert_eq!(served[0].origin, "stated");
    assert_eq!(served[1].confidence, 0.9);
}

#[test]
fn retired_expired_deleted_and_finished_projects_are_not_served() {
    let mut retired = record("01A", Origin::Inferred, "role", "Retired.", 0.9);
    retired.retired_at = Some(now() - Duration::days(1));
    let mut expired = record("01B", Origin::Inferred, "role", "Expired.", 0.9);
    expired.expires_at = Some(now() - Duration::seconds(1));
    let mut deleted = record("01C", Origin::Stated, "role", "Deleted.", 0.9);
    deleted.deleted_at = Some(now() - Duration::days(1));
    let mut done = record("01D", Origin::Inferred, "project", "Done.", 0.9);
    done.body = json!({"name": "launch", "note": "", "until": "2026-11-01"});
    let live = record("01E", Origin::Inferred, "role", "Live.", 0.9);
    let served = select_for_context(
        [retired, expired, deleted, done, live]
            .into_iter()
            .map(|r| (r, None))
            .collect(),
        now(),
        now().date_naive(),
    );
    assert_eq!(
        served.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
        ["Live."]
    );
}

#[test]
fn inferred_lines_on_old_mail_are_discounted_and_say_so() {
    let mut old = record("01A", Origin::Inferred, "role", "Old guess.", 0.8);
    old.provenance.message_ids = vec![7];
    let mut fresh = record("01B", Origin::Inferred, "role", "Fresh guess.", 0.6);
    fresh.provenance.message_ids = vec![8];
    let mut stated = record("01C", Origin::Stated, "role", "My word.", 1.0);
    stated.provenance.message_ids = vec![9];
    let long_ago = Some(now() - Duration::days(OLD_PROVENANCE_DAYS + 1));
    let served = select_for_context(
        vec![
            (old, long_ago),
            (fresh, Some(now() - Duration::days(10))),
            (stated, long_ago),
        ],
        now(),
        now().date_naive(),
    );
    let by_id = |id: &str| served.iter().find(|e| e.id == id).unwrap().clone();
    assert_eq!(by_id("01A").text, "Old guess. (old)");
    assert_eq!(by_id("01A").confidence, 0.4);
    // Discounted below the fresh guess, so it now sorts after it.
    assert_eq!(served[1].id, "01B");
    // The owner's word is not aged.
    assert_eq!(by_id("01C").text, "My word.");
    assert_eq!(by_id("01C").confidence, 1.0);
}

#[test]
fn a_run_gets_at_most_twenty_lines_and_the_byte_budget() {
    let rows: Vec<ServedRow> = (0..30)
        .map(|i| {
            (
                record(
                    &format!("01R{i:02}"),
                    Origin::Inferred,
                    "role",
                    "Short.",
                    0.5,
                ),
                None,
            )
        })
        .collect();
    assert_eq!(
        select_for_context(rows, now(), now().date_naive()).len(),
        MAX_CONTEXT_LINES
    );

    let long = "x".repeat(MAX_CONTEXT_BYTES / 2 + 1);
    let rows = vec![
        (record("01L1", Origin::Inferred, "role", &long, 0.9), None),
        (record("01L2", Origin::Inferred, "role", &long, 0.8), None),
        (record("01S", Origin::Inferred, "role", "Short.", 0.1), None),
    ];
    let served = select_for_context(rows, now(), now().date_naive());
    // The second long line would overflow, so it is skipped and the short
    // one still fits.
    assert_eq!(
        served.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        ["01L1", "01S"]
    );
}

#[test]
fn the_scopes_of_a_run_are_owner_sender_and_domain() {
    assert_eq!(
        scopes_for("Sam <SAM@Acme.example>"),
        ["owner", "domain:acme.example", "sender:sam@acme.example"]
    );
    assert_eq!(scopes_for("not an address"), ["owner"]);
}

#[test]
fn the_store_provider_serves_nothing_while_disabled() {
    let store = SqliteStore::open_in_memory().unwrap();
    let acct = store.ensure_account("me@example.com").unwrap();
    let mut owner = record(
        &new_ulid(now()),
        Origin::Stated,
        "role",
        "Runs a studio.",
        1.0,
    );
    owner.expires_at = None;
    store.insert_memory_entry_unchecked(acct, &owner).unwrap();
    let mut other = record(
        &new_ulid(now()),
        Origin::Inferred,
        "role",
        "Elsewhere.",
        0.5,
    );
    other.scope = "sender:someone@else.example".into();
    store.insert_memory_entry_unchecked(acct, &other).unwrap();
    let mut kind = record(
        &new_ulid(now()),
        Origin::Inferred,
        "ignores",
        "Ignores promos.",
        0.5,
    );
    kind.scope = "kind:promos".into();
    store.insert_memory_entry_unchecked(acct, &kind).unwrap();
    let scope = ContextScope {
        account_id: acct,
        thread_id: "t".into(),
        sender: "sam@acme.example".into(),
    };

    let provider = StoreMemoryContext {
        store: &store,
        enabled: false,
        now: now(),
        today: now().date_naive(),
    };
    assert!(provider.relevant_memory(&scope).unwrap().is_empty());

    let provider = StoreMemoryContext {
        store: &store,
        enabled: true,
        now: now(),
        today: now().date_naive(),
    };
    let served = provider.relevant_memory(&scope).unwrap();
    // The owner line and the `kind:` line; another sender's line is not this
    // run's.
    assert_eq!(
        served.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
        ["Runs a studio.", "Ignores promos."]
    );
    // And the rows behind it were the store's own read.
    assert_eq!(
        store
            .memory_context_rows(acct, &scopes_for("sam@acme.example"), now())
            .unwrap()
            .len(),
        2
    );
}
