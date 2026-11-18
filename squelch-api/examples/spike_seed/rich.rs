// `spike_seed --rich`: a realistic, ENTIRELY INVENTED inbox for designing and
// screenshotting the web client's sitrep. Every person, company, address and
// number here is made up (`.example` domains, invented brands, fake order
// numbers).
//
// Mail goes in through the real ingest, and every triage decision is committed
// through the worker's own path (claim the queued job, load its context,
// commit), so each zone is filled by the same projections a real mailbox uses.

use chrono::{Datelike, Duration, Utc};
use squelch_core::config::Stage1Config;
use squelch_core::store::agent_triage::{AgentCommitOutcome, AgentTriageStore};
use squelch_core::store::{SqliteStore, Store};
use squelch_core::sync::ingest::{RawFetched, ingest};
use squelch_core::triage::decision::{
    ActionKind, AttentionAction, AttentionFactors, AttentionState, EmailKind, EventChange,
    EventCommitment, EventRecordKind, EvidenceRef, MessageDecision, MessageDestination,
    ReadingKind, RecordProposal, SupportedTime, ThreadAttentionDecision,
};

/// What triage would have decided about one seeded message.
enum Verdict {
    /// For your eyes: needs the user, with an optional due day (days from today).
    NeedsYou {
        summary: &'static str,
        action: ActionKind,
        due_in_days: Option<i64>,
        importance: f64,
    },
    /// For your eyes, but informational (a reply, photos from family).
    Signal {
        summary: &'static str,
        importance: f64,
    },
    /// The Reading board.
    Reading {
        kind: ReadingKind,
        summary: &'static str,
    },
    /// A record (bill, receipt, delivery, event, statement), off the board.
    Record {
        kind: EmailKind,
        summary: &'static str,
        record: fn(i64) -> RecordProposal,
    },
}

struct Seed {
    from: &'static str,
    subject: &'static str,
    body: &'static str,
    hours_ago: i64,
    verdict: Verdict,
}

fn day(offset: i64) -> String {
    (Utc::now().date_naive() + Duration::days(offset))
        .format("%Y-%m-%d")
        .to_string()
}

fn days_to_friday() -> i64 {
    let wd = Utc::now().date_naive().weekday().num_days_from_monday() as i64; // Mon=0
    let d = (4 - wd).rem_euclid(7);
    if d == 0 { 7 } else { d }
}

fn time(value: String, id: i64) -> SupportedTime {
    SupportedTime {
        value,
        timezone: None,
        source_message_id: id,
        interpreted_relative: false,
    }
}

fn ev(id: i64) -> Vec<EvidenceRef> {
    vec![EvidenceRef {
        message_id: id,
        location: "body".into(),
    }]
}

fn seeds() -> Vec<Seed> {
    use Verdict::*;
    let mut v = vec![
        // ---- for your eyes
        Seed {
            from: "Harborview Leasing <leasing@harborview.example>",
            subject: "Lease renewal: signature needed",
            body: "Hi, your lease renewal is ready. Please sign before your current term ends so we can confirm the new rate.",
            hours_ago: 80,
            verdict: NeedsYou {
                summary: "Your lease renewal still needs a signature before the new term starts.",
                action: ActionKind::Other,
                due_in_days: Some(-3),
                importance: 0.95,
            },
        },
        Seed {
            from: "Lakeside Dental <frontdesk@lakesidedental.example>",
            subject: "Please confirm your Thursday appointment",
            body: "We have you down for Thursday at 2:30 PM. Reply to confirm or call us to reschedule.",
            hours_ago: 20,
            verdict: NeedsYou {
                summary: "Confirm Thursday's 2:30 appointment or call to reschedule.",
                action: ActionKind::Reply,
                due_in_days: Some(0),
                importance: 0.8,
            },
        },
        Seed {
            from: "Jamie <jamie@studio.example>",
            subject: "Two venue options for the offsite",
            body: "I found two places that can host us. Can you pick one by Friday so I can put down a hold?",
            hours_ago: 9,
            verdict: NeedsYou {
                summary: "Needs your pick between two offsite venues so they can hold one.",
                action: ActionKind::Decide,
                due_in_days: Some(days_to_friday()),
                importance: 0.75,
            },
        },
        Seed {
            from: "Alex <alex@studio.example>",
            subject: "Re: offsite dates",
            body: "Checked with everyone: the 14th works for the whole team.",
            hours_ago: 5,
            verdict: Signal {
                summary: "Replied to your offsite dates: the 14th works for everyone.",
                importance: 0.6,
            },
        },
        Seed {
            from: "Mom <mom@family.example>",
            subject: "Photos from the weekend",
            body: "Here are the pictures from Saturday. The garden looks wonderful this year.",
            hours_ago: 3,
            verdict: Signal {
                summary: "Sent photos from the weekend.",
                importance: 0.55,
            },
        },
        // ---- records
        Seed {
            from: "Keyline Keyboards <orders@keyline.example>",
            subject: "Your Q1 keyboard is out for delivery",
            body: "Good news: order KL-48211 is out for delivery today with Parcelway.",
            hours_ago: 2,
            verdict: Record {
                kind: EmailKind::Delivery,
                summary: "Your keyboard is out for delivery today.",
                record: |id| RecordProposal::Delivery {
                    carrier: Some("Parcelway".into()),
                    tracking_number: Some("PW4821100017".into()),
                    status: "out_for_delivery".into(),
                    item_name: Some("Q1 mechanical keyboard".into()),
                    merchant: Some("Keyline".into()),
                    order_refs: vec!["KL-48211".into()],
                    evidence: ev(id),
                },
            },
        },
        Seed {
            from: "Paper & Thread <hello@paperthread.example>",
            subject: "Your order has shipped",
            body: "Your linen notebooks (set of 3) have shipped and should arrive Saturday. 2 items in this package.",
            hours_ago: 30,
            verdict: Record {
                kind: EmailKind::Delivery,
                summary: "Your linen notebooks shipped and arrive Saturday.",
                record: |id| RecordProposal::Delivery {
                    carrier: Some("Parcelway".into()),
                    tracking_number: Some("PW2039155508".into()),
                    status: "shipped".into(),
                    item_name: Some("Linen notebooks, set of 3".into()),
                    merchant: Some("Paper & Thread".into()),
                    order_refs: vec!["PT-20391".into()],
                    evidence: ev(id),
                },
            },
        },
        Seed {
            from: "Metro Power & Light <billing@metropower.example>",
            subject: "Your bill is ready",
            body: "Your statement for this month is $142.08. It will be paid automatically on the 21st.",
            hours_ago: 26,
            verdict: Record {
                kind: EmailKind::Bill,
                summary: "This month's power bill is $142.08 on autopay.",
                record: |id| RecordProposal::Bill {
                    merchant: "Metro Power & Light".into(),
                    amount: Some(142.08),
                    currency: Some("USD".into()),
                    due: Some(time(day(9), id)),
                    autopay: Some(true),
                    evidence: ev(id),
                },
            },
        },
        Seed {
            from: "First Harbor Bank <alerts@firstharbor.example>",
            subject: "Your statement is ready",
            body: "The statement for your card ending 4417 is ready to view.",
            hours_ago: 40,
            verdict: Record {
                kind: EmailKind::FinancialUpdate,
                summary: "Your card statement (ending 4417) is ready.",
                record: |id| RecordProposal::FinancialUpdate {
                    institution: "First Harbor Bank".into(),
                    description: "Statement ready for card ending 4417".into(),
                    evidence: ev(id),
                },
            },
        },
        Seed {
            from: "Bluebird Coffee <receipts@bluebird.example>",
            subject: "Your receipt from Bluebird Coffee",
            body: "Thanks for stopping by. Total: $6.50.",
            hours_ago: 6,
            verdict: Record {
                kind: EmailKind::Receipt,
                summary: "Coffee, $6.50.",
                record: |id| RecordProposal::Receipt {
                    merchant: "Bluebird Coffee".into(),
                    amount: Some(6.50),
                    currency: Some("USD".into()),
                    evidence: ev(id),
                },
            },
        },
        Seed {
            from: "QuickBite <orders@quickbite.example>",
            subject: "Your order receipt",
            body: "Your delivery order total was $23.18.",
            hours_ago: 22,
            verdict: Record {
                kind: EmailKind::Receipt,
                summary: "Dinner delivery, $23.18.",
                record: |id| RecordProposal::Receipt {
                    merchant: "QuickBite".into(),
                    amount: Some(23.18),
                    currency: Some("USD".into()),
                    evidence: ev(id),
                },
            },
        },
        Seed {
            from: "Klubhaus Tickets <tickets@klubhaus.example>",
            subject: "Your ticket purchase",
            body: "Thanks for your purchase. Total: 25,00 EUR.",
            hours_ago: 50,
            verdict: Record {
                kind: EmailKind::Receipt,
                summary: "Club entry, 25.00 EUR.",
                record: |id| RecordProposal::Receipt {
                    merchant: "Klubhaus".into(),
                    amount: Some(25.0),
                    currency: Some("EUR".into()),
                    evidence: ev(id),
                },
            },
        },
        Seed {
            from: "Lakeside Dental <calendar@lakesidedental.example>",
            subject: "Appointment: checkup with Dr. Ortiz",
            body: "Your checkup is booked for Thursday at 2:30 PM.",
            hours_ago: 48,
            verdict: Record {
                kind: EmailKind::EventReservation,
                summary: "Checkup booked for Thursday at 2:30 PM.",
                record: |id| RecordProposal::Event {
                    title: "Dr. Ortiz · checkup".into(),
                    start: Some(time(format!("{}T14:30", day(2)), id)),
                    location: Some("Lakeside Dental".into()),
                    end: None,
                    duration_estimate: None,
                    event_kind: EventRecordKind::Reservation,
                    commitment: EventCommitment::Committed,
                    change: EventChange::Created,
                    identity_refs: vec![],
                    likelihood: None,
                    evidence: ev(id),
                    identity: None,
                },
            },
        },
        Seed {
            from: "Jamie <jamie@studio.example>",
            subject: "Offsite moved to the 14th",
            body: "Updated invite: the team offsite is now on the 14th.",
            hours_ago: 7,
            verdict: Record {
                kind: EmailKind::EventReservation,
                summary: "The team offsite moved to the 14th.",
                record: |id| RecordProposal::Event {
                    title: "Team offsite".into(),
                    start: Some(time(day(12), id)),
                    location: None,
                    end: None,
                    duration_estimate: None,
                    event_kind: EventRecordKind::Invite,
                    commitment: EventCommitment::Committed,
                    change: EventChange::Updated,
                    identity_refs: vec![],
                    likelihood: None,
                    evidence: ev(id),
                    identity: None,
                },
            },
        },
    ];

    // ---- reading: six senders, two to nine issues each
    let reading: [(&str, ReadingKind, &[&str]); 6] = [
        (
            "The Weekly Whisker <hello@whisker.example>",
            ReadingKind::Content,
            &[
                "Why your cat stares at the wall, and nine other mysteries",
                "The great cardboard box debate",
                "Ten quiet toys for loud cats",
                "Reader mailbag: the midnight zoomies",
            ],
        ),
        (
            "Field Notes Daily <news@fieldnotes.example>",
            ReadingKind::Content,
            &[
                "Markets react calmly to everything. Plus the week ahead",
                "The morning brief: what changed overnight",
                "Weekend edition: long reads for slow mornings",
            ],
        ),
        (
            "The Long Read <editor@longread.example>",
            ReadingKind::Content,
            &[
                "This month's programme: three essays on attention",
                "Letters: on keeping a notebook",
            ],
        ),
        (
            "Brightly <deals@brightly.example>",
            ReadingKind::Promotion,
            &[
                "48 hours only: 40% off everything you saved",
                "Your cart misses you",
                "New arrivals for the season",
                "Members get early access",
                "Last chance: free shipping ends tonight",
                "A gift guide for everyone on your list",
                "Restocked: the bestsellers are back",
                "Your weekly picks",
                "Thanks for being a member",
            ],
        ),
        (
            "Cedar & Pine <shop@cedarpine.example>",
            ReadingKind::Promotion,
            &[
                "Deals picked for you, based on one search",
                "Back in stock: the walnut desk tray",
                "Your recently viewed items",
                "Weekend sale: up to 30% off",
                "Something new from the workshop",
            ],
        ),
        (
            "Northwind Air <miles@northwind.example>",
            ReadingKind::Promotion,
            &[
                "1,204 miles expire at the end of the month",
                "Fares from 89 to the coast",
            ],
        ),
    ];
    for (from, kind, subjects) in reading {
        for (i, subject) in subjects.iter().enumerate() {
            v.push(Seed {
                from,
                subject,
                body: "A synthetic newsletter issue for the spike daemon.",
                hours_ago: 4 + i as i64 * 19,
                verdict: Reading {
                    kind,
                    summary: if kind == ReadingKind::Promotion {
                        "A promotional offer."
                    } else {
                        "A newsletter issue."
                    },
                },
            });
        }
    }
    v
}

fn decision_for(verdict: &Verdict, id: i64) -> MessageDecision {
    let base = MessageDecision {
        kinds: vec![EmailKind::General],
        reason: "Synthetic triage for the spike daemon".into(),
        ..Default::default()
    };
    match verdict {
        Verdict::NeedsYou {
            summary,
            action,
            due_in_days,
            importance,
        } => MessageDecision {
            kinds: vec![EmailKind::Correspondence],
            summary: (*summary).into(),
            attention: ThreadAttentionDecision {
                show_in_fye: true,
                state: AttentionState::NeedsUser,
                actions: vec![AttentionAction {
                    id: None,
                    kind: *action,
                    description: (*summary).into(),
                    resolved: false,
                    due: due_in_days.map(|d| time(day(d), id)),
                    evidence: ev(id),
                }],
                summary: (*summary).into(),
                factors: AttentionFactors {
                    urgency: *importance,
                    action_need: 0.9,
                    personal_relevance: 0.9,
                    importance: *importance,
                    attention_at: None,
                },
                relevant_message_ids: vec![id],
                evidence: ev(id),
            },
            ..base
        },
        Verdict::Signal {
            summary,
            importance,
        } => MessageDecision {
            kinds: vec![EmailKind::Correspondence],
            summary: (*summary).into(),
            attention: ThreadAttentionDecision {
                show_in_fye: true,
                state: AttentionState::Informational,
                summary: (*summary).into(),
                factors: AttentionFactors {
                    urgency: 0.3,
                    action_need: 0.2,
                    personal_relevance: 0.9,
                    importance: *importance,
                    attention_at: None,
                },
                relevant_message_ids: vec![id],
                evidence: ev(id),
                ..Default::default()
            },
            ..base
        },
        Verdict::Reading { kind, summary } => MessageDecision {
            kinds: vec![if *kind == ReadingKind::Promotion {
                EmailKind::Promotional
            } else {
                EmailKind::Editorial
            }],
            destinations: vec![MessageDestination::Reading],
            reading_kind: Some(*kind),
            summary: (*summary).into(),
            ..base
        },
        Verdict::Record {
            kind,
            summary,
            record,
        } => MessageDecision {
            kinds: vec![*kind],
            summary: (*summary).into(),
            records: vec![record(id)],
            ..base
        },
    }
}

pub fn seed_rich(store: &SqliteStore, acct: i64) -> anyhow::Result<()> {
    // The owner's name, for "Good morning, <name>".
    Store::set_account_profile(store, acct, Some("Sam Example"), Some("Sam"), None)?;

    let mut verdicts = std::collections::HashMap::new();
    for (i, s) in seeds().into_iter().enumerate() {
        let raw = format!(
            "From: {}\r\nTo: me@example.com\r\nSubject: {}\r\nMessage-ID: <rich-{i}@example.com>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{}\r\n",
            s.from, s.subject, s.body
        );
        let fetched = RawFetched {
            account_id: acct,
            gmail_msg_id: format!("rich-{i}"),
            gmail_thread_id: Some(format!("rich-t{i}")),
            raw: raw.into_bytes(),
            internal_date: Some(Utc::now() - Duration::hours(s.hours_ago)),
            is_sent: false,
            is_spam: false,
            account_addr: "me@example.com".to_string(),
        };
        let triaged = ingest(&fetched, &Stage1Config::default(), Utc::now(), |_| false);
        let id = store.ingest_message(&triaged)?;
        verdicts.insert(id, s.verdict);
    }

    // Decide every queued job the ingest enqueued, through the worker's path.
    let (mut applied, mut other) = (0, Vec::new());
    while let Some(job) = store.claim_agent_job(acct, "triage", Utc::now(), 600)? {
        let Some(verdict) = verdicts.get(&job.message_id) else {
            other.push(job.kind.clone());
            store.complete_agent_job(&job)?;
            continue;
        };
        let context = store.load_agent_context(&job)?;
        let decision = decision_for(verdict, job.message_id);
        let outcome = store.commit_agent_decision(
            &job,
            &context,
            &decision,
            std::slice::from_ref(&context.message.source),
        )?;
        if outcome == AgentCommitOutcome::Applied {
            applied += 1;
        } else {
            eprintln!("message {}: {outcome:?}", job.message_id);
        }
    }
    // Shipments are their own table, written by the mail extractor beside
    // triage (`upsert_shipment`), so the delivery mail is recorded there too.
    use squelch_core::triage::shipment::{ShipmentInfo, ShipmentStatus};
    let mut shipments = 0;
    for (id, verdict) in &verdicts {
        let Verdict::Record { record, .. } = verdict else {
            continue;
        };
        let RecordProposal::Delivery {
            carrier,
            tracking_number,
            status,
            item_name,
            ..
        } = record(*id)
        else {
            continue;
        };
        let status = match status.as_str() {
            "out_for_delivery" => ShipmentStatus::OutForDelivery,
            _ => ShipmentStatus::Shipped,
        };
        let info = ShipmentInfo {
            carrier: carrier.unwrap_or_default(),
            tracking_number: tracking_number.unwrap_or_default(),
            item_name: item_name.unwrap_or_default(),
            status,
            tracking_url: None,
        };
        Store::upsert_shipment(store, acct, *id, &info, Utc::now())?;
        shipments += 1;
    }
    println!(
        "rich: {} messages, {applied} decisions applied, {shipments} shipments",
        verdicts.len()
    );
    if !other.is_empty() {
        println!("rich: completed {} unrelated jobs: {other:?}", other.len());
    }
    Ok(())
}
