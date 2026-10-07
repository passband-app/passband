//! Model-owned semantic decisions. Defaults support construction; validation is
//! mandatory before a decision becomes authoritative.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

macro_rules! decision_enum {
    ($name:ident { $first:ident $(, $variant:ident)* $(,)? }) => {
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { #[default] $first, $($variant),* }
    };
}
decision_enum!(EmailKind {
    General,
    Correspondence,
    Editorial,
    Promotional,
    Bill,
    Receipt,
    FinancialUpdate,
    Delivery,
    EventReservation,
    AccountService,
    AuthenticationSecurity
});
decision_enum!(MessageDestination { Reading });
// What a Reading message is for the person reading it: a piece meant to be
// read like an article or post, or a promotion. Only meaningful when
// `destinations` holds Reading.
decision_enum!(ReadingKind { Content, Promotion });
decision_enum!(AttentionState {
    Informational,
    NeedsUser,
    WaitingOnOthers,
    Resolved
});
decision_enum!(ActionKind {
    Review,
    Reply,
    Pay,
    Decide,
    Attend,
    Other
});
decision_enum!(AuthKind {
    Otp,
    PasswordReset,
    SignInLink,
    Verification,
    LoginAlert,
    SecurityAlert
});

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub message_id: i64,
    /// A field or attachment location, never a quoted credential.
    pub location: String,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AuthAssessment {
    pub kinds: Vec<AuthKind>,
    pub evidence: Vec<EvidenceRef>,
}
impl AuthAssessment {
    pub fn is_auth(&self) -> bool {
        !self.kinds.is_empty()
    }
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AccessAssessment {
    pub restricted: bool,
    pub reason: String,
    pub evidence: Vec<EvidenceRef>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SupportedTime {
    /// ISO date or RFC3339 timestamp. Do not invent a time for a date-only fact.
    pub value: String,
    pub timezone: Option<String>,
    pub source_message_id: i64,
    pub interpreted_relative: bool,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AttentionFactors {
    pub urgency: f64,
    pub action_need: f64,
    pub personal_relevance: f64,
    pub importance: f64,
    pub attention_at: Option<SupportedTime>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AttentionAction {
    /// Existing action ID when updating; the executor assigns new IDs.
    pub id: Option<String>,
    pub kind: ActionKind,
    pub description: String,
    pub resolved: bool,
    pub due: Option<SupportedTime>,
    pub evidence: Vec<EvidenceRef>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThreadAttentionDecision {
    pub show_in_fye: bool,
    pub state: AttentionState,
    pub actions: Vec<AttentionAction>,
    pub summary: String,
    pub factors: AttentionFactors,
    pub relevant_message_ids: Vec<i64>,
    pub evidence: Vec<EvidenceRef>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NotificationAdvice {
    #[serde(default)]
    pub login_code: Option<super::login_code::LoginCode>,
    pub importance: u8,
    pub title: String,
    pub body: String,
    pub reason: String,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RuleException {
    pub rule_id: i64,
    pub reason: String,
    pub evidence: Vec<EvidenceRef>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RelatedAttentionUpdate {
    pub thread_id: String,
    pub expected_revision: i64,
    pub attention: ThreadAttentionDecision,
    pub evidence: Vec<EvidenceRef>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RevisitRequest {
    pub at: DateTime<Utc>,
    pub reason: String,
}

decision_enum!(EventRecordKind {
    Other,
    Ticket,
    Travel,
    Reservation,
    Invite,
    Proposal
});
// `Committed` means the user already booked, bought, registered or accepted.
decision_enum!(EventCommitment {
    Unknown,
    Committed,
    Invited,
    Proposed
});
// What THIS message does to the event. `Agreed` is evidence the user accepted.
decision_enum!(EventChange {
    Created,
    Updated,
    Cancelled,
    Agreed
});
decision_enum!(LikelihoodBand {
    Maybe,
    Likely,
    Unlikely
});
/// The model's guess at an event's length. Never a fact: it is only emitted
/// when the mail states no end. `minutes` is signed so a nonsense value
/// deserializes and is laundered (`triage::ghost`) instead of failing the step.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DurationEstimate {
    pub minutes: i64,
    pub basis: String,
}
/// A prior for a non-committed event becoming a commitment.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EventLikelihood {
    pub band: LikelihoodBand,
    pub reason: String,
}

/// Explicit facts only. No record proposal implicitly closes an obligation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordProposal {
    Bill {
        merchant: String,
        amount: Option<f64>,
        currency: Option<String>,
        due: Option<SupportedTime>,
        autopay: Option<bool>,
        evidence: Vec<EvidenceRef>,
    },
    Receipt {
        merchant: String,
        amount: Option<f64>,
        currency: Option<String>,
        evidence: Vec<EvidenceRef>,
    },
    /// `item_name`, `merchant` and `order_refs` arrived after decisions were
    /// already being stored, so each defaults: reconcile re-reads EVERY stored
    /// decision_json, and one that cannot deserialize fails the whole pass.
    /// Many-to-many on purpose: one box can carry several orders, and one order
    /// can ship in several boxes (several records, often several emails).
    Delivery {
        carrier: Option<String>,
        tracking_number: Option<String>,
        status: String,
        #[serde(default)]
        item_name: Option<String>,
        #[serde(default)]
        merchant: Option<String>,
        #[serde(default)]
        order_refs: Vec<String>,
        evidence: Vec<EvidenceRef>,
    },
    /// Every field after `location` arrived after decisions were already being
    /// stored, so each defaults, as on `Delivery`. `end` is a STATED end only;
    /// a guess lives in `duration_estimate` and is never promoted to `end`.
    Event {
        title: String,
        start: Option<SupportedTime>,
        location: Option<String>,
        #[serde(default)]
        end: Option<SupportedTime>,
        #[serde(default)]
        duration_estimate: Option<DurationEstimate>,
        #[serde(default)]
        event_kind: EventRecordKind,
        #[serde(default)]
        commitment: EventCommitment,
        #[serde(default)]
        change: EventChange,
        /// iCalendar UID, confirmation or booking numbers, event page URL.
        #[serde(default)]
        identity_refs: Vec<String>,
        #[serde(default)]
        likelihood: Option<EventLikelihood>,
        evidence: Vec<EvidenceRef>,
    },
    FinancialUpdate {
        institution: String,
        description: String,
        evidence: Vec<EvidenceRef>,
    },
}
impl RecordProposal {
    pub fn evidence(&self) -> &[EvidenceRef] {
        match self {
            Self::Bill { evidence, .. }
            | Self::Receipt { evidence, .. }
            | Self::Delivery { evidence, .. }
            | Self::Event { evidence, .. }
            | Self::FinancialUpdate { evidence, .. } => evidence,
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MessageDecision {
    pub kinds: Vec<EmailKind>,
    pub destinations: Vec<MessageDestination>,
    /// Arrived after decisions were already stored, so it defaults (see the
    /// note on `RecordProposal::Delivery`); older rows read as None and the
    /// client falls back to `kinds`.
    #[serde(default)]
    pub reading_kind: Option<ReadingKind>,
    pub summary: String,
    pub reason: String,
    pub auth: AuthAssessment,
    pub external_access: AccessAssessment,
    pub attention: ThreadAttentionDecision,
    pub records: Vec<RecordProposal>,
    pub related_updates: Vec<RelatedAttentionUpdate>,
    pub rule_exceptions: Vec<RuleException>,
    pub notification: NotificationAdvice,
    pub revisit: Option<RevisitRequest>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reconcile re-reads every stored decision_json, so an `event` record
    /// written before the calendar fields existed must still deserialize, with
    /// each new field at its default.
    #[test]
    fn an_event_record_without_the_calendar_fields_still_deserializes() {
        let old = serde_json::json!({
            "kind": "event",
            "title": "Dinner",
            "start": {"value": "2026-10-03T19:30:00-04:00", "timezone": null,
                      "source_message_id": 7, "interpreted_relative": false},
            "location": "Via Carota",
            "evidence": [{"message_id": 7, "location": "body"}],
        });
        let record: RecordProposal = serde_json::from_value(old).unwrap();
        let RecordProposal::Event {
            title,
            end,
            duration_estimate,
            event_kind,
            commitment,
            change,
            identity_refs,
            likelihood,
            evidence,
            ..
        } = record
        else {
            panic!("not an event");
        };
        assert_eq!(title, "Dinner");
        assert_eq!(end, None);
        assert_eq!(duration_estimate, None);
        assert_eq!(event_kind, EventRecordKind::Other);
        assert_eq!(commitment, EventCommitment::Unknown);
        assert_eq!(change, EventChange::Created);
        assert!(identity_refs.is_empty());
        assert_eq!(likelihood, None);
        assert_eq!(evidence.len(), 1);
    }

    #[test]
    fn an_event_record_with_the_calendar_fields_round_trips() {
        let record = RecordProposal::Event {
            title: "Show".into(),
            start: None,
            location: None,
            end: None,
            duration_estimate: Some(DurationEstimate {
                minutes: 150,
                basis: "doors 7, show 8".into(),
            }),
            event_kind: EventRecordKind::Ticket,
            commitment: EventCommitment::Committed,
            change: EventChange::Updated,
            identity_refs: vec!["ABC123".into()],
            likelihood: Some(EventLikelihood {
                band: LikelihoodBand::Likely,
                reason: "paid".into(),
            }),
            evidence: vec![],
        };
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["event_kind"], "ticket");
        assert_eq!(json["commitment"], "committed");
        assert_eq!(json["change"], "updated");
        assert_eq!(json["likelihood"]["band"], "likely");
        assert_eq!(
            serde_json::from_value::<RecordProposal>(json).unwrap(),
            record
        );
    }
}
