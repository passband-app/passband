//! Bounded structured-output investigation. The model owns semantics; this
//! executor only limits resources and validates references and output shape.
use super::{
    agent_config::AgentTriageConfig,
    context::*,
    decision::*,
    llm::{self, LlmOutcome, LlmRequest, Usage},
};
use crate::config::Stage2Provider;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const PROMPT_VERSION: &str = "agent-triage-v8";
const SYSTEM: &str = include_str!("prompts/agent-v1.txt");

/// The one sentence of the prompt that offers the review step, and what it
/// says when there is no reviewer to offer. `system_prompt` swaps them, and a
/// test pins the first to the prompt file so an edit there cannot turn the
/// swap into a silent no-op.
const REVIEW_OFFER: &str = "Read evidence when needed, request configured stronger review \
                            for a specific question, or finish with a complete decision.";
const NO_REVIEW_OFFER: &str = "Read evidence when needed, or finish with a complete decision.";

/// The system prompt for a deployment with (`with_review`) or without a
/// configured, usable `review_model`: the same text, minus the offer of a
/// step the schema does not carry. See [`step_schema`] for why the step is
/// left out rather than refused. Composed once per process, like every other
/// prompt that feeds a cached prefix in this crate, so the bytes are
/// identical across calls.
fn system_prompt(with_review: bool) -> &'static str {
    static WITHOUT_REVIEW: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| SYSTEM.replacen(REVIEW_OFFER, NO_REVIEW_OFFER, 1));
    if with_review {
        SYSTEM
    } else {
        WITHOUT_REVIEW.as_str()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum AgentStep {
    ReadEvidence { requests: Vec<EvidenceRequest> },
    RequestReview { question: String },
    Finish { decision: Box<MessageDecision> },
}
pub struct AgentRun {
    pub decision: MessageDecision,
    pub source_message_ids: Vec<i64>,
    pub thread_revisions: BTreeMap<String, i64>,
    pub usage: Vec<Usage>,
    pub model_turns: usize,
    pub tool_calls: usize,
}
pub struct AgentConnection<'a> {
    pub http: &'a reqwest::Client,
    pub url: &'a str,
    pub api_key: &'a str,
    pub provider: Stage2Provider,
    pub model: &'a str,
    pub effort: Option<&'a str>,
}
/// Failed investigations still report usage for completed provider responses.
/// A timed-out in-flight request may be billed without returning provider usage.
#[derive(Debug)]
pub struct AgentFailure {
    pub kind: String,
    pub usage: Vec<Usage>,
    /// Number of attempted turns; a rejected first call may be refunded safely.
    pub model_calls: usize,
}
impl std::fmt::Display for AgentFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.kind)
    }
}
impl std::error::Error for AgentFailure {}

pub async fn run_agent(
    connection: AgentConnection<'_>,
    config: &AgentTriageConfig,
    mut context: ContextSnapshot,
    reader: &dyn EvidenceReader,
) -> Result<AgentRun, AgentFailure> {
    config.validate().map_err(|kind| AgentFailure {
        kind,
        usage: Vec::new(),
        model_calls: 0,
    })?;
    condense_initial_context(&mut context.initial, &config.context);
    // Reserve half the context window for investigation results. Truncation is
    // visible in the payload and never silently drops a message's identity.
    bound_evidence_text(&mut context.initial, config.context.max_context_bytes / 2);
    let mut usage = Vec::new();
    let mut model_calls = 0;
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(config.agent.timeout_secs),
        run_bounded(
            connection,
            config,
            context,
            reader,
            &mut usage,
            &mut model_calls,
        ),
    )
    .await;
    match outcome {
        Ok(Ok(run)) => Ok(run),
        Ok(Err(kind)) => Err(AgentFailure {
            kind,
            usage,
            model_calls,
        }),
        Err(_) => Err(AgentFailure {
            kind: "agent_timeout".into(),
            usage,
            model_calls,
        }),
    }
}

async fn run_bounded(
    connection: AgentConnection<'_>,
    config: &AgentTriageConfig,
    context: ContextSnapshot,
    reader: &dyn EvidenceReader,
    usage: &mut Vec<Usage>,
    model_calls: &mut usize,
) -> Result<AgentRun, String> {
    let mut sources: BTreeSet<i64> = context.source_message_ids.iter().copied().collect();
    let mut revisions = context.thread_revisions.clone();
    let mut observed = ObservedDecisionContext::default();
    observed.observe(&context.initial, Some(&context.thread_id));
    let mut history = Vec::<Value>::new();
    let mut tool_calls = 0;
    let mut reviews = 0;
    let mut model = config.agent.model.as_deref().unwrap_or(connection.model);
    // Offer the review step, in prompt and schema, only where a reviewer
    // exists to take it AND the budget lets it be taken: a `review_model`
    // with `max_review_calls = 0` is an offer whose acceptance fails the run.
    // Both are per-run constants, so the prompt and the schema are built once.
    let with_review = config.agent.review_model.is_some() && config.agent.max_review_calls > 0;
    let system = system_prompt(with_review);
    let schema = step_schema(with_review);
    for turn in 0..config.agent.max_model_turns {
        let final_turn = turn + 1 == config.agent.max_model_turns;
        let user = serde_json::to_string(
            &json!({"context": context, "investigation": history, "must_finish": final_turn}),
        )
        .map_err(|_| "context_encode")?;
        if user.len() > config.context.max_context_bytes {
            return Err("context_budget_exceeded".into());
        }
        let request = LlmRequest {
            model,
            system,
            user: &user,
            schema: schema.clone(),
            effort: connection.effort,
            max_tries: 1,
            max_tokens: None,
        };
        // Retain provider usage before decoding either JSON syntax or step
        // structure. A malformed paid answer still belongs in the spend ledger.
        *model_calls += 1;
        let response = llm::classify_agent_step(
            connection.http,
            connection.url,
            connection.api_key,
            connection.provider,
            &request,
        )
        .await
        .map_err(|error| error.kind)?;
        // A review request grants one stronger-model call. Follow-up tool
        // investigation returns to the base model unless another review is
        // explicitly requested within the remaining review budget.
        model = config.agent.model.as_deref().unwrap_or(connection.model);
        usage.extend(response.usage());
        let step = match response {
            LlmOutcome::Ok(value, _) => match serde_json::from_str::<WireStep>(&value) {
                Ok(wire) => wire.result,
                Err(_) => {
                    history.push(json!({"validation_error": "invalid_step_shape"}));
                    continue;
                }
            },
            LlmOutcome::Refused(_) => return Err("agent_refused".into()),
            LlmOutcome::Failed(kind, _) => {
                if llm::is_config_failure(&kind) {
                    return Err(kind);
                }
                history.push(json!({"validation_error": kind,
                    "instruction": "Return a valid structured step."}));
                continue;
            }
        };
        match step {
            AgentStep::Finish { decision } => {
                match validate_decision(&decision, &sources, &revisions, &context.rule_ids)
                    .and_then(|()| observed.validate(&decision, &context.thread_id))
                {
                    Ok(()) => {
                        let mut decision = *decision;
                        // reading_kind only means something on Reading mail; a
                        // stale one would colour the card if Reading is added
                        // by a correction later.
                        if !decision.destinations.contains(&MessageDestination::Reading) {
                            decision.reading_kind = None;
                        }
                        return Ok(AgentRun {
                            decision,
                            source_message_ids: sources.into_iter().collect(),
                            thread_revisions: revisions,
                            usage: usage.clone(),
                            model_turns: turn + 1,
                            tool_calls,
                        });
                    }
                    Err(error) => history.push(json!({"validation_error": error,
                        "instruction": "Repair your final decision using observed evidence."})),
                }
            }
            AgentStep::ReadEvidence { requests } => {
                if final_turn
                    || requests.is_empty()
                    || tool_calls + requests.len() > config.agent.max_tool_calls
                {
                    return Err("agent_tool_budget_exceeded".into());
                }
                for request in requests {
                    tool_calls += 1;
                    match reader.read(&request) {
                        Ok(mut result) => {
                            condense_evidence(&mut result.data);
                            bound_evidence_text(
                                &mut result.data,
                                config.context.max_tool_result_bytes,
                            );
                            let encoded =
                                serde_json::to_vec(&result.data).map_err(|_| "tool_encode")?;
                            if encoded.len() > config.context.max_tool_result_bytes {
                                history.push(json!({"request": request,
                                    "error": "result_too_large; narrow the request"}));
                                continue;
                            }
                            observed.observe(&result.data, None);
                            sources.extend(result.source_message_ids.iter().copied());
                            for (thread, revision) in &result.thread_revisions {
                                if revisions
                                    .get(thread)
                                    .is_some_and(|existing| existing != revision)
                                {
                                    return Err("context_changed_during_run".into());
                                }
                            }
                            revisions.extend(result.thread_revisions.clone());
                            history.push(json!({"request": request, "result": result.data}));
                        }
                        Err(_) => history.push(json!({"request": request,
                            "error": "evidence_unavailable"})),
                    }
                }
            }
            AgentStep::RequestReview { question } => {
                if final_turn || reviews >= config.agent.max_review_calls {
                    return Err("agent_review_budget_exceeded".into());
                }
                if let Some(review_model) = config.agent.review_model.as_deref() {
                    model = review_model;
                    reviews += 1;
                    history.push(json!({"review_question": question}));
                } else {
                    history.push(json!({"error":
                        "review_model_unavailable; investigate with available tools or finish"}));
                }
            }
        }
    }
    Err("agent_turn_budget_exhausted".into())
}

/// Condense the bodies of the initial snapshot: the target and the newest
/// `full_sibling_bodies` siblings lose quoted tails and long URLs
/// ([`super::condense::condense_body`]); older siblings shrink to an excerpt
/// and say so. Runs BEFORE [`bound_evidence_text`], so the byte budget is
/// spent on text that can still decide something.
fn condense_initial_context(initial: &mut Value, config: &super::agent_config::ContextConfig) {
    if let Some(Value::String(body)) = initial.pointer_mut("/message/body") {
        *body = super::condense::condense_body(body);
    }
    let Some(Value::Array(siblings)) = initial.get_mut("thread") else {
        return;
    };
    for (index, sibling) in siblings.iter_mut().enumerate() {
        let Some(fields) = sibling.as_object_mut() else {
            continue;
        };
        let Some(Value::String(body)) = fields.get_mut("body") else {
            continue;
        };
        if index < config.full_sibling_bodies {
            *body = super::condense::condense_body(body);
            continue;
        }
        let (excerpt, cut) = super::condense::excerpt(body, config.sibling_excerpt_chars);
        *body = excerpt;
        if cut {
            fields.insert("body_excerpted".into(), Value::Bool(true));
        }
    }
}

/// Condense the bodies in a tool result: every `messages[]` entry and a
/// top-level `message`. A `read_message` page (it carries `body_offset`) is
/// left raw, because its offsets index the stored text and the prompt promises
/// the model that pages are unmodified.
fn condense_evidence(data: &mut Value) {
    fn condense_message(value: &mut Value) {
        let Some(fields) = value.as_object_mut() else {
            return;
        };
        if fields.contains_key("body_offset") {
            return;
        }
        if let Some(Value::String(body)) = fields.get_mut("body") {
            *body = super::condense::condense_body(body);
        }
    }
    if let Some(message) = data.get_mut("message") {
        condense_message(message);
    }
    if let Some(Value::Array(messages)) = data.get_mut("messages") {
        messages.iter_mut().for_each(condense_message);
    }
}

/// Bound large textual evidence without truncating JSON, identifiers, user
/// preferences, or timestamps. Each shortened field gets a visible indicator.
fn bound_evidence_text(value: &mut Value, budget: usize) {
    if serde_json::to_vec(value).is_ok_and(|encoded| encoded.len() <= budget) {
        return;
    }
    fn trim(value: &mut Value, limit: usize) {
        match value {
            Value::Object(fields) => {
                let mut indicators = Vec::new();
                for (key, value) in fields.iter_mut() {
                    if key == "source" {
                        continue;
                    }
                    if matches!(key.as_str(), "body" | "content" | "text" | "snippet")
                        && let Value::String(text) = value
                    {
                        if text.len() > limit {
                            let mut end = limit;
                            while !text.is_char_boundary(end) {
                                end -= 1;
                            }
                            text.truncate(end);
                            indicators.push(format!("{key}_truncated"));
                        }
                        continue;
                    }
                    trim(value, limit);
                }
                for key in indicators {
                    // If a paged body is shortened again by the overall
                    // context bound, resume at the visible boundary, not the
                    // original page end (which would skip unseen evidence).
                    if key == "body_truncated"
                        && let (Some(offset), Some(body)) = (
                            fields.get("body_offset").and_then(Value::as_u64),
                            fields.get("body").and_then(Value::as_str),
                        )
                    {
                        let next = offset.saturating_add(body.chars().count() as u64);
                        fields.insert("next_offset".into(), json!(next));
                    }
                    fields.insert(key, Value::Bool(true));
                }
            }
            Value::Array(values) => {
                for value in values {
                    trim(value, limit);
                }
            }
            _ => {}
        }
    }
    // Measure the actual encoded JSON instead of assuming every character takes
    // eight bytes. Keep the target body first; distribute remaining bytes across
    // sibling/tool text with a common cap, letting short fields keep all of theirs.
    let original = value.clone();
    let target = original.pointer("/message/body").and_then(Value::as_str);
    let fit = |other_limit: usize, target_limit: usize| {
        let mut candidate = original.clone();
        let primary = candidate
            .get_mut("message")
            .and_then(Value::as_object_mut)
            .and_then(|message| message.remove("body"));
        trim(&mut candidate, other_limit);
        if let Some(body) = primary {
            let mut message = json!({"body":body});
            trim(&mut message, target_limit);
            let fields = candidate["message"]
                .as_object_mut()
                .expect("message object");
            fields.insert("body".into(), message["body"].clone());
            if message.get("body_truncated").is_some() {
                fields.insert("body_truncated".into(), true.into());
                if let Some(offset) = fields.get("body_offset").and_then(Value::as_u64) {
                    fields.insert(
                        "next_offset".into(),
                        json!(offset.saturating_add(
                            message["body"].as_str().unwrap().chars().count() as u64
                        )),
                    );
                }
            }
        }
        candidate
    };
    let size = |candidate: &Value| {
        serde_json::to_vec(candidate)
            .map(|encoded| encoded.len())
            .unwrap_or(usize::MAX)
    };
    let maximize = |mut lo: usize, mut hi: usize, allowed: &dyn Fn(usize) -> bool| {
        while lo < hi {
            let mid = lo + (hi - lo).div_ceil(2);
            if allowed(mid) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        lo
    };
    let target_limit = target
        .map(|body| maximize(0, body.len(), &|limit| size(&fit(0, limit)) <= budget))
        .unwrap_or(0);
    let other_limit = maximize(0, budget, &|limit| {
        size(&fit(limit, target_limit)) <= budget
    });
    *value = fit(other_limit, target_limit);
}

/// Structural validation only: no category clamps, auth detector, or score floor.
/// Structural facts captured from the evidence actually shown to the model.
/// Invalid action IDs, cross-thread relevance, and self-targeted updates get a
/// repair turn before commit instead of buying an entirely new investigation.
#[derive(Default)]
struct ObservedDecisionContext {
    message_threads: BTreeMap<i64, String>,
    action_ids: BTreeMap<String, BTreeSet<String>>,
    target_kind_correction: Option<Value>,
}
impl ObservedDecisionContext {
    fn observe(&mut self, data: &Value, current_thread: Option<&str>) {
        // Only the initial target's corrections apply to its classification.
        // A related-thread tool result must not replace this user intent.
        if current_thread.is_some() {
            self.target_kind_correction = data
                .get("user_corrections")
                .and_then(Value::as_array)
                .and_then(|corrections| {
                    corrections
                        .iter()
                        .find(|correction| correction["field"] == "kinds")
                })
                .map(|correction| correction["value"].clone());
        }
        let mut thread = current_thread.map(str::to_owned);
        let messages = data.get("message").into_iter().chain(
            ["messages", "thread"]
                .into_iter()
                .filter_map(|field| data.get(field).and_then(Value::as_array))
                .flatten(),
        );
        for message in messages {
            if let (Some(id), Some(name)) = (
                message.get("id").and_then(Value::as_i64),
                message.get("thread_id").and_then(Value::as_str),
            ) {
                self.message_threads.insert(id, name.into());
                thread.get_or_insert_with(|| name.into());
            }
        }
        if let Some(thread) = thread
            && let Some(attention) = data.get("attention")
        {
            self.action_ids.insert(
                thread,
                attention
                    .get("actions")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|action| {
                        action.get("id").and_then(Value::as_str).map(str::to_owned)
                    })
                    .collect(),
            );
        }
    }
    fn validate(&self, decision: &MessageDecision, thread: &str) -> Result<(), String> {
        if let Some(correction) = &self.target_kind_correction {
            let read = |value: &Value| {
                serde_json::from_value::<Vec<EmailKind>>(value.clone())
                    .map_err(|_| "invalid_user_kind_correction".to_owned())
            };
            let effective = if correction.is_object() {
                let remove = read(&correction["remove"])?;
                let mut kinds = decision.kinds.clone();
                kinds.retain(|kind| !remove.contains(kind));
                for kind in read(&correction["add"])? {
                    if !kinds.contains(&kind) {
                        kinds.push(kind);
                    }
                }
                kinds
            } else {
                read(correction)?
            };
            if effective.is_empty() {
                return Err("user_kind_correction_removes_all_proposed_kinds; propose an evidence-supported kind the user has not removed".into());
            }
        }
        let check = |attention: &ThreadAttentionDecision, target: &str| -> Result<(), String> {
            if attention
                .relevant_message_ids
                .iter()
                .any(|id| self.message_threads.get(id).map(String::as_str) != Some(target))
            {
                return Err("attention_relevance_must_belong_to_target_thread".into());
            }
            let mut seen = BTreeSet::new();
            for id in attention.actions.iter().filter_map(|a| a.id.as_ref()) {
                if !seen.insert(id)
                    || !self
                        .action_ids
                        .get(target)
                        .is_some_and(|ids| ids.contains(id))
                {
                    return Err("unknown_or_duplicate_attention_action_id".into());
                }
            }
            Ok(())
        };
        check(&decision.attention, thread)?;
        let mut targets = BTreeSet::new();
        for update in &decision.related_updates {
            if update.thread_id == thread || !targets.insert(&update.thread_id) {
                return Err("related_update_repeats_thread".into());
            }
            if !self.action_ids.contains_key(&update.thread_id) {
                return Err("related_attention_must_be_read".into());
            }
            check(&update.attention, &update.thread_id)?;
        }
        Ok(())
    }
}

pub fn validate_decision(
    decision: &MessageDecision,
    sources: &BTreeSet<i64>,
    revisions: &BTreeMap<String, i64>,
    rule_ids: &[i64],
) -> Result<(), String> {
    let fail = |kind: &str| Err(kind.to_owned());
    if decision.kinds.is_empty()
        || decision.summary.trim().is_empty()
        || decision.reason.trim().is_empty()
    {
        return fail("decision_missing_explanation");
    }
    if decision
        .kinds
        .iter()
        .enumerate()
        .any(|(i, k)| decision.kinds[..i].contains(k))
        || decision
            .destinations
            .iter()
            .enumerate()
            .any(|(i, d)| decision.destinations[..i].contains(d))
    {
        return fail("duplicate_classification");
    }
    if decision.notification.importance > 100 {
        return fail("notification_score_out_of_range");
    }
    let evidence = |items: &[EvidenceRef]| {
        !items.is_empty()
            && items.iter().all(|e| {
                sources.contains(&e.message_id)
                    && !e.location.trim().is_empty()
                    && e.location.len() <= 256
            })
    };
    if decision.external_access.reason.trim().is_empty()
        || !evidence(&decision.external_access.evidence)
    {
        return fail("access_requires_evidence");
    }
    if decision.auth.is_auth() && !evidence(&decision.auth.evidence) {
        return fail("auth_requires_evidence");
    }
    let supported_time = |time: &SupportedTime| {
        sources.contains(&time.source_message_id)
            && (chrono::DateTime::parse_from_rfc3339(&time.value).is_ok()
                || chrono::NaiveDate::parse_from_str(&time.value, "%Y-%m-%d").is_ok())
    };
    let attention = |a: &ThreadAttentionDecision| {
        [
            a.factors.urgency,
            a.factors.action_need,
            a.factors.personal_relevance,
            a.factors.importance,
        ]
        .iter()
        .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
            && a.factors.attention_at.as_ref().is_none_or(&supported_time)
            && !a.relevant_message_ids.is_empty()
            && a.relevant_message_ids.iter().all(|id| sources.contains(id))
            && evidence(&a.evidence)
            && a.actions.iter().all(|action| {
                !action.description.trim().is_empty()
                    && evidence(&action.evidence)
                    && action.due.as_ref().is_none_or(&supported_time)
            })
    };
    if !attention(&decision.attention) {
        return fail("invalid_attention_evidence_or_factors");
    }
    for update in &decision.related_updates {
        if revisions.get(&update.thread_id) != Some(&update.expected_revision)
            || !evidence(&update.evidence)
            || !attention(&update.attention)
        {
            return fail("related_update_not_observed");
        }
    }
    for exception in &decision.rule_exceptions {
        if !rule_ids.contains(&exception.rule_id)
            || exception.reason.trim().is_empty()
            || !evidence(&exception.evidence)
        {
            return fail("invalid_rule_exception");
        }
    }
    for record in &decision.records {
        if !evidence(record.evidence()) {
            return fail("record_requires_evidence");
        }
    }
    if decision
        .revisit
        .as_ref()
        .is_some_and(|r| r.reason.trim().is_empty())
    {
        return fail("revisit_requires_reason");
    }
    if decision
        .revisit
        .as_ref()
        .is_some_and(|r| r.at <= chrono::Utc::now())
    {
        return fail("revisit_must_be_in_the_future");
    }
    Ok(())
}

fn object(fields: Vec<(&str, Value)>) -> Value {
    let required: Vec<&str> = fields.iter().map(|(key, _)| *key).collect();
    let properties: serde_json::Map<String, Value> = fields
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect();
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn array(value: Value) -> Value {
    json!({"type":"array","items":value})
}
fn nullable(value: Value) -> Value {
    json!({"anyOf":[value,{"type":"null"}]})
}
fn enumeration(values: &[&str]) -> Value {
    json!({"type":"string","enum":values})
}
fn text() -> Value {
    json!({"type":"string"})
}
fn integer() -> Value {
    json!({"type":"integer"})
}
fn number() -> Value {
    json!({"type":"number"})
}
fn boolean() -> Value {
    json!({"type":"boolean"})
}
/// A `$ref` into [`schema_defs`]. The step schema used to inline every
/// repeated sub-schema — `attention` twice, `evidence` a dozen times, `time`
/// six — and the whole thing rode in the cached prefix of every call. Refs
/// keep the JSON the model sees a fraction of that size; both transports'
/// structured-output modes resolve local `$ref`s.
fn reference(name: &str) -> Value {
    json!({"$ref": format!("#/$defs/{name}")})
}
/// The shared definitions. Attached at the ROOT of whichever schema is sent
/// ([`step_schema`], or [`decision_schema`] on its own), because a `$ref` is
/// resolved against the document root, not the object that contains it.
fn schema_defs() -> Value {
    json!({
        "evidence": evidence_schema(),
        "time": time_schema(),
        "attention": attention_schema(),
    })
}
fn evidence_schema() -> Value {
    array(object(vec![
        ("message_id", integer()),
        ("location", text()),
    ]))
}
fn time_schema() -> Value {
    object(vec![
        ("value", text()),
        ("timezone", nullable(text())),
        ("source_message_id", integer()),
        ("interpreted_relative", boolean()),
    ])
}
fn attention_schema() -> Value {
    object(vec![
        ("show_in_fye", boolean()),
        (
            "state",
            enumeration(&[
                "informational",
                "needs_user",
                "waiting_on_others",
                "resolved",
            ]),
        ),
        (
            "actions",
            array(object(vec![
                ("id", nullable(text())),
                (
                    "kind",
                    enumeration(&["review", "reply", "pay", "decide", "attend", "other"]),
                ),
                ("description", text()),
                ("resolved", boolean()),
                ("due", nullable(reference("time"))),
                ("evidence", reference("evidence")),
            ])),
        ),
        ("summary", text()),
        (
            "factors",
            object(vec![
                ("urgency", number()),
                ("action_need", number()),
                ("personal_relevance", number()),
                ("importance", number()),
                ("attention_at", nullable(reference("time"))),
            ]),
        ),
        ("relevant_message_ids", array(integer())),
        ("evidence", reference("evidence")),
    ])
}
/// The decision schema as a self-contained document (with its `$defs`), for
/// callers that send or inspect it on its own.
pub fn decision_schema() -> Value {
    let mut schema = decision_schema_body();
    schema["$defs"] = schema_defs();
    schema
}
/// [`decision_schema`] minus the `$defs`, for embedding under a root that
/// carries them ([`step_schema`]).
fn decision_schema_body() -> Value {
    let records = vec![
        object(vec![
            ("kind", enumeration(&["bill"])),
            ("merchant", text()),
            ("amount", nullable(number())),
            ("currency", nullable(text())),
            ("due", nullable(reference("time"))),
            ("autopay", nullable(boolean())),
            ("evidence", reference("evidence")),
        ]),
        object(vec![
            ("kind", enumeration(&["receipt"])),
            ("merchant", text()),
            ("amount", nullable(number())),
            ("currency", nullable(text())),
            ("evidence", reference("evidence")),
        ]),
        object(vec![
            ("kind", enumeration(&["delivery"])),
            (
                "carrier",
                nullable(enumeration(&[
                    "ups", "usps", "fedex", "dhl", "amazon", "unknown",
                ])),
            ),
            ("tracking_number", nullable(text())),
            (
                "status",
                enumeration(&[
                    "ordered",
                    "shipped",
                    "out_for_delivery",
                    "delivered",
                    "exception",
                    "unknown",
                ]),
            ),
            ("item_name", nullable(text())),
            ("merchant", nullable(text())),
            ("order_refs", array(text())),
            ("evidence", reference("evidence")),
        ]),
        object(vec![
            ("kind", enumeration(&["event"])),
            ("title", text()),
            ("start", nullable(reference("time"))),
            ("location", nullable(text())),
            ("end", nullable(reference("time"))),
            (
                "duration_estimate",
                nullable(object(vec![("minutes", integer()), ("basis", text())])),
            ),
            (
                "event_kind",
                enumeration(&[
                    "ticket",
                    "travel",
                    "reservation",
                    "invite",
                    "proposal",
                    "other",
                ]),
            ),
            (
                "commitment",
                enumeration(&["committed", "invited", "proposed", "unknown"]),
            ),
            (
                "change",
                enumeration(&["created", "updated", "cancelled", "agreed"]),
            ),
            ("identity_refs", array(text())),
            (
                "likelihood",
                nullable(object(vec![
                    ("band", enumeration(&["likely", "maybe", "unlikely"])),
                    ("reason", text()),
                ])),
            ),
            ("evidence", reference("evidence")),
        ]),
        object(vec![
            ("kind", enumeration(&["financial_update"])),
            ("institution", text()),
            ("description", text()),
            ("evidence", reference("evidence")),
        ]),
    ];
    object(vec![
        (
            "kinds",
            array(enumeration(&[
                "general",
                "correspondence",
                "editorial",
                "promotional",
                "bill",
                "receipt",
                "financial_update",
                "delivery",
                "event_reservation",
                "account_service",
                "authentication_security",
            ])),
        ),
        ("destinations", array(enumeration(&["reading"]))),
        (
            "reading_kind",
            nullable(enumeration(&["content", "promotion"])),
        ),
        ("summary", text()),
        ("reason", text()),
        (
            "auth",
            object(vec![
                (
                    "kinds",
                    array(enumeration(&[
                        "otp",
                        "password_reset",
                        "sign_in_link",
                        "verification",
                        "login_alert",
                        "security_alert",
                    ])),
                ),
                ("evidence", reference("evidence")),
            ]),
        ),
        (
            "external_access",
            object(vec![
                ("restricted", boolean()),
                ("reason", text()),
                ("evidence", reference("evidence")),
            ]),
        ),
        ("attention", reference("attention")),
        ("records", array(json!({"anyOf":records}))),
        (
            "related_updates",
            array(object(vec![
                ("thread_id", text()),
                ("expected_revision", integer()),
                ("attention", reference("attention")),
                ("evidence", reference("evidence")),
            ])),
        ),
        (
            "rule_exceptions",
            array(object(vec![
                ("rule_id", integer()),
                ("reason", text()),
                ("evidence", reference("evidence")),
            ])),
        ),
        (
            "notification",
            object(vec![
                ("importance", integer()),
                ("title", text()),
                ("login_code", super::login_code::schema()),
                ("body", text()),
                ("reason", text()),
            ]),
        ),
        (
            "revisit",
            nullable(object(vec![("at", text()), ("reason", text())])),
        ),
    ])
}
/// The step schema the model answers with. `with_review` adds the
/// `request_review` step; without a configured, usable `review_model` the
/// step is LEFT OUT rather than refused at runtime, because an offer the
/// model can take up and have refused is a whole billed turn (cached prefix
/// read, the full context again, the thinking) spent learning that a tool is
/// absent, and one of its four turns gone.
///
/// How much the schema alone guarantees depends on the transport. On the
/// OpenAI wire it is a strict `json_schema` response format, so a step the
/// schema does not name cannot be produced. On the Anthropic wire (the hosted
/// gateway's) it is a non-strict tool `input_schema` with `tool_choice: auto`,
/// so a stray `request_review` can still arrive, and the `RequestReview` arm
/// in `run_bounded` that answers it with `review_model_unavailable` and
/// carries on is LOAD-BEARING there, not a backstop. The shape is a
/// per-deployment constant, so the cached prefix is unaffected.
fn step_schema(with_review: bool) -> Value {
    let requests = vec![
        object(vec![
            ("tool", enumeration(&["read_thread"])),
            ("thread_id", text()),
        ]),
        object(vec![
            ("tool", enumeration(&["read_message"])),
            ("message_id", integer()),
            ("offset", nullable(integer())),
        ]),
        object(vec![
            ("tool", enumeration(&["search_mail"])),
            ("query", text()),
        ]),
        object(vec![
            ("tool", enumeration(&["read_sender_history"])),
            ("sender", text()),
        ]),
        object(vec![
            ("tool", enumeration(&["read_record"])),
            ("message_id", integer()),
        ]),
        object(vec![
            ("tool", enumeration(&["read_attachment"])),
            ("message_id", integer()),
            ("attachment_id", text()),
        ]),
    ];
    let mut steps = vec![object(vec![
        ("step", enumeration(&["read_evidence"])),
        ("requests", array(json!({"anyOf": requests}))),
    ])];
    if with_review {
        steps.push(object(vec![
            ("step", enumeration(&["request_review"])),
            ("question", text()),
        ]));
    }
    steps.push(object(vec![
        ("step", enumeration(&["finish"])),
        ("decision", decision_schema_body()),
    ]));
    // Provider structured outputs require an object at the root; a nested
    // union is supported by both transports. WireStep unwraps that envelope.
    let mut schema = object(vec![("result", json!({"anyOf": steps}))]);
    schema["$defs"] = schema_defs();
    schema
}
#[derive(Deserialize)]
struct WireStep {
    result: AgentStep,
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The step schema is part of the cached prefix of EVERY triage call, so
    /// its encoded size is a cost line item. The bound is the `$defs` version's
    /// size with headroom; inlining the shared sub-schemas again roughly
    /// doubles it, and this is what notices. Raised from 9,000 when the
    /// calendar event fields (about 860 bytes) and `reading_kind` landed
    /// together at 9,027: a new field should fit under it, a second copy of
    /// the shared definitions should not.
    #[test]
    fn step_schema_stays_compact_and_its_refs_resolve() {
        // The larger shape is the one the size bound is about.
        let schema = step_schema(true);
        let encoded = serde_json::to_string(&schema).unwrap();
        eprintln!("step schema: {} bytes", encoded.len());
        assert!(
            encoded.len() < 9_500,
            "step schema grew to {} bytes",
            encoded.len()
        );
        let defs = schema["$defs"].as_object().unwrap();
        fn refs(value: &Value, out: &mut Vec<String>) {
            match value {
                Value::Object(fields) => {
                    if let Some(Value::String(target)) = fields.get("$ref") {
                        out.push(target.clone());
                    }
                    fields.values().for_each(|v| refs(v, out));
                }
                Value::Array(values) => values.iter().for_each(|v| refs(v, out)),
                _ => {}
            }
        }
        let mut found = Vec::new();
        refs(&schema, &mut found);
        assert!(
            found.len() >= 10,
            "expected the shared schemas to be referenced"
        );
        for target in found {
            let name = target.strip_prefix("#/$defs/").expect("local ref");
            assert!(defs.contains_key(name), "dangling $ref {target}");
        }
        // The standalone decision schema resolves on its own too.
        let alone = decision_schema();
        let mut found = Vec::new();
        refs(&alone, &mut found);
        for target in found {
            let name = target.strip_prefix("#/$defs/").unwrap();
            assert!(alone["$defs"].get(name).is_some(), "dangling {target}");
        }
    }
    /// The calendar fields are in the event record's schema and required (the
    /// strict structured-output mode needs every key listed), with the optional
    /// ones nullable.
    #[test]
    fn event_record_schema_carries_the_calendar_fields() {
        let schema = decision_schema();
        let event = schema["properties"]["records"]["items"]["anyOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["properties"]["kind"]["enum"][0] == "event")
            .expect("event record schema");
        let required: Vec<&str> = event["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_str().unwrap())
            .collect();
        for key in [
            "end",
            "duration_estimate",
            "event_kind",
            "commitment",
            "change",
            "identity_refs",
            "likelihood",
        ] {
            assert!(required.contains(&key), "{key} missing from {required:?}");
        }
        for key in ["end", "duration_estimate", "likelihood"] {
            assert!(
                event["properties"][key]["anyOf"][1]["type"] == "null",
                "{key} should be nullable"
            );
        }
        assert_eq!(
            event["properties"]["commitment"]["enum"],
            json!(["committed", "invited", "proposed", "unknown"])
        );
    }
    fn valid() -> MessageDecision {
        let evidence = vec![EvidenceRef {
            message_id: 7,
            location: "body".into(),
        }];
        MessageDecision {
            kinds: vec![EmailKind::General],
            summary: "A message".into(),
            reason: "Informational".into(),
            external_access: AccessAssessment {
                restricted: false,
                reason: "No credentials".into(),
                evidence: evidence.clone(),
            },
            attention: ThreadAttentionDecision {
                relevant_message_ids: vec![7],
                evidence,
                ..Default::default()
            },
            ..Default::default()
        }
    }
    /// The target and the two newest siblings keep their (condensed) bodies;
    /// older siblings become a flagged excerpt; quoted tails go everywhere.
    #[test]
    fn initial_context_condenses_bodies_and_excerpts_older_siblings() {
        let quote = "\n\nOn Mon, Sep 28, 2026 Alice <a@example.com> wrote:\n> earlier";
        let long = "x".repeat(600);
        let mut initial = json!({
            "message": {"id": 4, "body": format!("Reply four.{quote}")},
            "thread": [
                {"id": 3, "body": format!("Reply three.{quote}")},
                {"id": 2, "body": format!("Reply two.{quote}")},
                {"id": 1, "body": format!("{long}{quote}")},
                {"id": 0, "body": "short"},
            ],
        });
        let config = super::super::agent_config::ContextConfig::default();
        condense_initial_context(&mut initial, &config);
        let marker = super::super::condense::QUOTE_MARKER;
        assert_eq!(initial["message"]["body"], format!("Reply four.\n{marker}"));
        assert_eq!(
            initial["thread"][0]["body"],
            format!("Reply three.\n{marker}")
        );
        assert_eq!(
            initial["thread"][1]["body"],
            format!("Reply two.\n{marker}")
        );
        assert!(initial["thread"][0].get("body_excerpted").is_none());
        let third = initial["thread"][2]["body"].as_str().unwrap();
        assert_eq!(third.chars().count(), config.sibling_excerpt_chars);
        assert!(third.ends_with('…'));
        assert_eq!(initial["thread"][2]["body_excerpted"], true);
        // Already under the excerpt budget: unchanged and unflagged.
        assert_eq!(initial["thread"][3]["body"], "short");
        assert!(initial["thread"][3].get("body_excerpted").is_none());
    }

    /// Tool results condense too, except a raw page from read_message.
    #[test]
    fn tool_results_condense_except_raw_pages() {
        let quote = "\n\nOn Mon, Sep 28, 2026 Alice <a@example.com> wrote:\n> earlier";
        let mut thread = json!({"messages": [{"id": 1, "body": format!("Hi.{quote}")}]});
        condense_evidence(&mut thread);
        assert_eq!(
            thread["messages"][0]["body"],
            format!("Hi.\n{}", super::super::condense::QUOTE_MARKER)
        );
        let raw = format!("Hi.{quote}");
        let mut page = json!({"message": {"id": 1, "body": raw, "body_offset": 0}});
        condense_evidence(&mut page);
        assert_eq!(page["message"]["body"], raw);
    }

    #[test]
    fn long_thread_retains_decisive_subject_content_within_encoded_limit() {
        let body = format!("{}Payment is due tomorrow.", "x".repeat(23_900));
        let mut value = json!({
            "message": {"id": 1, "body": body},
            "thread": (2..10).map(|id| json!({"id": id, "body": "é".repeat(6000)})).collect::<Vec<_>>()
        });
        let before = serde_json::to_vec(&value).unwrap().len();
        bound_evidence_text(&mut value, 30_000);
        assert_eq!(value["message"]["body"], body);
        assert_eq!(value["thread"].as_array().unwrap().len(), 8);
        let after = serde_json::to_vec(&value).unwrap().len();
        assert!(after <= 30_000);
        assert!(after > 29_000);
        assert!(after < before / 3);
    }

    #[test]
    fn invitations_are_carved_out_of_auth_in_the_prompt() {
        // A Slack workspace invite ("X has invited you to work with them in Y",
        // one-click join link) was classified sign_in_link and sealed. The prompt
        // must say what an invitation is, not only list the auth kinds.
        assert!(SYSTEM.contains("An invitation is not authentication"));
        assert!(SYSTEM.contains("one-click join or accept link"));
        // A Passband invite (one-time signup code, 30-day expiry) was sealed as
        // verification: the code box looked like an OTP.
        assert!(SYSTEM.contains("invite, referral, promo, or waitlist code"));
        assert!(SYSTEM.contains("invite or referral or promo codes"));
        assert!(!SYSTEM.contains('\u{2014}'), "em dash in the agent prompt");
    }
    #[test]
    fn credentials_are_not_inferred_from_auth_category() {
        let mut decision = valid();
        decision.auth = AuthAssessment {
            kinds: vec![AuthKind::LoginAlert],
            evidence: decision.attention.evidence.clone(),
        };
        assert!(validate_decision(&decision, &BTreeSet::from([7]), &BTreeMap::new(), &[]).is_ok());
        assert!(!decision.external_access.restricted);
    }
    #[test]
    fn past_revisits_cannot_create_an_immediate_investigation_loop() {
        let mut decision = valid();
        decision.revisit = Some(RevisitRequest {
            at: chrono::Utc::now() - chrono::Duration::minutes(1),
            reason: "Check later".into(),
        });
        assert_eq!(
            validate_decision(&decision, &BTreeSet::from([7]), &BTreeMap::new(), &[]),
            Err("revisit_must_be_in_the_future".into())
        );
        decision.revisit.as_mut().unwrap().at = chrono::Utc::now() + chrono::Duration::hours(1);
        assert!(validate_decision(&decision, &BTreeSet::from([7]), &BTreeMap::new(), &[]).is_ok());
    }
    #[test]
    fn unknown_evidence_and_unfetched_updates_fail() {
        let mut decision = valid();
        decision.attention.relevant_message_ids = vec![8];
        assert!(validate_decision(&decision, &BTreeSet::from([7]), &BTreeMap::new(), &[]).is_err());
        decision = valid();
        decision.related_updates.push(RelatedAttentionUpdate {
            thread_id: "other".into(),
            expected_revision: 1,
            attention: decision.attention.clone(),
            evidence: decision.attention.evidence.clone(),
        });
        assert!(validate_decision(&decision, &BTreeSet::from([7]), &BTreeMap::new(), &[]).is_err());
    }
    /// A shipping email that never prints its order number: the model finds
    /// the number in the sender's history and cites THAT message. Any message a
    /// tool exposed this run is in `sources`, so the citation stands; one the
    /// run never saw does not.
    #[test]
    fn delivery_order_refs_may_cite_a_fetched_earlier_message() {
        let mut decision = valid();
        decision.records.push(RecordProposal::Delivery {
            carrier: Some("ups".into()),
            tracking_number: Some("1ZW061R3DG21045729".into()),
            status: "shipped".into(),
            item_name: None,
            merchant: Some("Bill's Exhausts".into()),
            order_refs: vec!["21470".into()],
            evidence: vec![
                EvidenceRef {
                    message_id: 7,
                    location: "body".into(),
                },
                EvidenceRef {
                    message_id: 3,
                    location: "subject".into(),
                },
            ],
        });
        let fetched = BTreeSet::from([7, 3]);
        assert!(validate_decision(&decision, &fetched, &BTreeMap::new(), &[]).is_ok());
        assert_eq!(
            validate_decision(&decision, &BTreeSet::from([7]), &BTreeMap::new(), &[]),
            Err("record_requires_evidence".into())
        );
        let delivery = &decision_schema()["properties"]["records"]["items"]["anyOf"][2];
        for field in ["item_name", "merchant", "order_refs"] {
            assert!(
                delivery["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|f| f == field),
                "{field}"
            );
        }
    }
    #[test]
    fn delivery_schema_statuses_are_compatible_with_carrier_projection() {
        let schema = decision_schema();
        let delivery = schema["properties"]["records"]["items"]["anyOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["properties"]["kind"]["enum"] == json!(["delivery"]))
            .unwrap();
        let statuses = delivery["properties"]["status"]["enum"].as_array().unwrap();
        assert_eq!(statuses.len(), 6);
        for status in statuses {
            let value = status.as_str().unwrap();
            assert!(
                value == "unknown" || crate::triage::ShipmentStatus::parse(value).is_some(),
                "schema status must be pollable or explicitly unknown: {value}"
            );
        }
    }
    #[test]
    fn reading_kind_is_offered_and_defaults_on_stored_rows() {
        // Product updates read like articles; the prompt must say they are not.
        assert!(SYSTEM.contains("A company writing about its own product is promotion"));
        let schema = decision_schema();
        assert_eq!(
            schema["properties"]["reading_kind"],
            nullable(enumeration(&["content", "promotion"]))
        );
        let mut decision = serde_json::to_value(valid()).unwrap();
        decision.as_object_mut().unwrap().remove("reading_kind");
        let stored: MessageDecision = serde_json::from_value(decision.clone()).unwrap();
        assert_eq!(stored.reading_kind, None);
        decision["reading_kind"] = json!("content");
        let fresh: MessageDecision = serde_json::from_value(decision).unwrap();
        assert_eq!(
            fresh.reading_kind,
            Some(crate::triage::decision::ReadingKind::Content)
        );
    }
    #[test]
    fn strict_schema_requires_all_object_fields() {
        fn check(value: &Value) {
            if value.get("type") == Some(&json!("object")) {
                assert_eq!(value["additionalProperties"], json!(false));
                assert_eq!(
                    value["required"].as_array().unwrap().len(),
                    value["properties"].as_object().unwrap().len()
                );
            }
            match value {
                Value::Object(map) => {
                    for v in map.values() {
                        check(v)
                    }
                }
                Value::Array(items) => {
                    for v in items {
                        check(v)
                    }
                }
                _ => {}
            }
        }
        check(&step_schema(true));
        check(&step_schema(false));
    }
    /// The review step and its sentence of prompt are offered only when a
    /// usable `review_model` exists to answer them. Pins the prompt file's wording
    /// too: if the sentence is reworded there, the swap would silently stop
    /// matching, and this is what notices.
    #[test]
    fn review_step_is_offered_only_with_a_review_model() {
        fn step_names(schema: &Value) -> Vec<String> {
            schema["properties"]["result"]["anyOf"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| {
                    s["properties"]["step"]["enum"][0]
                        .as_str()
                        .unwrap()
                        .to_string()
                })
                .collect()
        }
        assert_eq!(
            step_names(&step_schema(true)),
            ["read_evidence", "request_review", "finish"]
        );
        assert_eq!(step_names(&step_schema(false)), ["read_evidence", "finish"]);

        assert!(
            SYSTEM.contains(REVIEW_OFFER),
            "the prompt file no longer carries the review offer verbatim"
        );
        let with = system_prompt(true);
        let without = system_prompt(false);
        assert_eq!(with, SYSTEM);
        assert!(without.contains(NO_REVIEW_OFFER));
        assert!(
            !without.contains(REVIEW_OFFER) && !without.contains("stronger review"),
            "a prompt with no reviewer still offers one"
        );
        assert_eq!(
            with.len() - without.len(),
            REVIEW_OFFER.len() - NO_REVIEW_OFFER.len(),
            "only the one sentence should differ"
        );
    }

    /// On the Anthropic transport the schema is a non-strict tool, so a
    /// reviewer-less deployment can still receive a stray `request_review`;
    /// the runtime arm is what answers it there (see `step_schema`). A run
    /// that asks and is told no reviewer exists carries on and finishes on
    /// the next turn, instead of failing.
    #[tokio::test]
    async fn a_stray_review_request_without_a_reviewer_is_answered_not_fatal() {
        use axum::{Router, routing::post};
        use std::sync::{Arc, Mutex};
        let responses = Arc::new(Mutex::new(std::collections::VecDeque::from([
            json!({"result": {"step": "request_review", "question": "is this a bill?"}}),
            json!({"result": {"step": "finish", "decision": valid()}}),
        ])));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/", post(mock_provider))
            .with_state(responses);
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let http = reqwest::Client::new();
        let connection = AgentConnection {
            http: &http,
            url: &url,
            api_key: "test",
            provider: Stage2Provider::OpenAI,
            model: "test",
            effort: None,
        };
        let context = ContextSnapshot {
            thread_id: "thread".into(),
            initial: json!({"message":{"id":7,"thread_id":"thread"},"attention":null}),
            source_message_ids: vec![7],
            ..Default::default()
        };
        let config = AgentTriageConfig::default();
        assert!(
            config.agent.review_model.is_none(),
            "the default has no reviewer"
        );
        let run = run_agent(connection, &config, context, &NoEvidence)
            .await
            .expect("a stray review request is answered, not fatal");
        assert_eq!(
            run.model_turns, 2,
            "the refusal costs one turn, then it finishes"
        );
        server.abort();
    }

    struct NoEvidence;
    impl EvidenceReader for NoEvidence {
        fn read(&self, _: &EvidenceRequest) -> Result<EvidenceResult, String> {
            Err("unavailable".into())
        }
    }

    async fn mock_provider(
        axum::extract::State(responses): axum::extract::State<
            std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<Value>>>,
        >,
        axum::Json(request): axum::Json<Value>,
    ) -> axum::Json<Value> {
        assert_eq!(request["response_format"]["json_schema"]["strict"], true);
        let content = responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("bounded calls");
        axum::Json(json!({
            "choices": [{"message": {"content": content.as_str().map(str::to_owned).unwrap_or_else(|| content.to_string())}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 8}
        }))
    }

    #[tokio::test]
    async fn provider_step_repair_retains_usage_and_validates_evidence() {
        use axum::{Router, routing::post};
        use std::sync::{Arc, Mutex};
        let mut invalid = valid();
        invalid.attention.relevant_message_ids = vec![999];
        let responses = Arc::new(Mutex::new(std::collections::VecDeque::from([
            json!("{malformed-json"),
            json!({"result": {"step": "finish", "decision": invalid}}),
            json!({"result": {"step": "finish", "decision": valid()}}),
        ])));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/", post(mock_provider))
            .with_state(responses);
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let http = reqwest::Client::new();
        let connection = AgentConnection {
            http: &http,
            url: &url,
            api_key: "test",
            provider: Stage2Provider::OpenAI,
            model: "test",
            effort: None,
        };
        let context = ContextSnapshot {
            thread_id: "thread".into(),
            initial: json!({"message":{"id":7,"thread_id":"thread"},"attention":null}),
            source_message_ids: vec![7],
            ..Default::default()
        };
        let run = run_agent(
            connection,
            &AgentTriageConfig::default(),
            context,
            &NoEvidence,
        )
        .await
        .unwrap();
        assert_eq!(run.model_turns, 3);
        assert_eq!(run.usage.len(), 3);
        assert_eq!(run.usage.iter().map(|u| u.input_tokens).sum::<u64>(), 36);
        server.abort();
    }

    #[tokio::test]
    async fn commit_constraints_are_repaired_inside_the_paid_investigation() {
        use axum::{Router, routing::post};
        use std::sync::{Arc, Mutex};
        for invalid_kind in 0..4 {
            let mut invalid = valid();
            match invalid_kind {
                0 => invalid.attention.relevant_message_ids = vec![8],
                1 => invalid.attention.actions.push(AttentionAction {
                    id: Some("invented".into()),
                    description: "Pay".into(),
                    evidence: invalid.attention.evidence.clone(),
                    ..Default::default()
                }),
                2 => invalid.related_updates.push(RelatedAttentionUpdate {
                    thread_id: "thread".into(),
                    expected_revision: 1,
                    attention: invalid.attention.clone(),
                    evidence: invalid.attention.evidence.clone(),
                }),
                _ => invalid.kinds = vec![EmailKind::Receipt],
            }
            let responses = Arc::new(Mutex::new(std::collections::VecDeque::from([
                json!({"result":{"step":"finish","decision":invalid}}),
                json!({"result":{"step":"finish","decision":valid()}}),
            ])));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/", listener.local_addr().unwrap());
            let app = Router::new()
                .route("/", post(mock_provider))
                .with_state(responses);
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let http = reqwest::Client::new();
            let connection = AgentConnection {
                http: &http,
                url: &url,
                api_key: "test",
                provider: Stage2Provider::OpenAI,
                model: "test",
                effort: None,
            };
            let context = ContextSnapshot {
                thread_id: "thread".into(),
                source_message_ids: vec![7, 8],
                thread_revisions: BTreeMap::from([("thread".into(), 1)]),
                initial: json!({"message":{"id":7,"thread_id":"thread"},"thread":[{"id":8,"thread_id":"other"}],"attention":null,
                    "user_corrections":[{"field":"kinds","value":{"add":[],"remove":["receipt"]}}]}),
                ..Default::default()
            };
            let run = run_agent(
                connection,
                &AgentTriageConfig::default(),
                context,
                &NoEvidence,
            )
            .await
            .unwrap();
            assert_eq!(run.model_turns, 2, "{invalid_kind}");
            assert_eq!(run.usage.len(), 2);
            server.abort();
        }
    }

    #[test]
    fn long_thread_retains_target_body_and_uses_remaining_budget() {
        let body = format!(
            "{}Decisive instruction: reply by Friday.",
            "x".repeat(24_000)
        );
        let mut value = json!({"message":{"id":1,"body":body},
            "thread":(2..10).map(|id|json!({"id":id,"body":"y".repeat(16_000)})).collect::<Vec<_>>()});
        bound_evidence_text(&mut value, 40_000);
        assert!(
            value["message"]["body"]
                .as_str()
                .unwrap()
                .ends_with("reply by Friday.")
        );
        assert!(value["message"].get("body_truncated").is_none());
        let encoded = serde_json::to_vec(&value).unwrap().len();
        assert!(
            (39_900..=40_000).contains(&encoded),
            "actual encoded budget: {encoded}"
        );
        assert!(value["thread"][0]["body"].as_str().unwrap().len() > 1500);
        assert_eq!(value["thread"][0]["body_truncated"], true);
    }

    #[test]
    fn text_limits_are_visible_and_preserve_utf8_and_identity() {
        let mut value = json!({"message_id": 7, "body": "é".repeat(10000)});
        bound_evidence_text(&mut value, 1000);
        assert_eq!(value["message_id"], 7);
        assert_eq!(value["body_truncated"], true);
        let bytes = serde_json::to_vec(&value).unwrap().len();
        assert!(bytes <= 1000);
        assert!(bytes >= 990, "available context should be used: {bytes}");
    }
    struct ExtraEvidence;
    impl EvidenceReader for ExtraEvidence {
        fn read(&self, request: &EvidenceRequest) -> Result<EvidenceResult, String> {
            assert!(matches!(request, EvidenceRequest::SearchMail { .. }));
            Ok(EvidenceResult {
                data: json!({"messages": [{"id": 8, "snippet": "Additional evidence"}]}),
                source_message_ids: vec![8],
                ..Default::default()
            })
        }
    }

    #[tokio::test]
    async fn uncited_tool_sources_remain_in_derivative_provenance() {
        use axum::{Router, routing::post};
        use std::sync::{Arc, Mutex};
        let responses = Arc::new(Mutex::new(std::collections::VecDeque::from([
            json!({"result": {"step": "read_evidence", "requests": [{"tool": "search_mail", "query": "invoice"}]}}),
            json!({"result": {"step": "finish", "decision": valid()}}),
        ])));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/", post(mock_provider))
            .with_state(responses);
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let http = reqwest::Client::new();
        let connection = AgentConnection {
            http: &http,
            url: &url,
            api_key: "test",
            provider: Stage2Provider::OpenAI,
            model: "test",
            effort: None,
        };
        let context = ContextSnapshot {
            thread_id: "thread".into(),
            initial: json!({"message":{"id":7,"thread_id":"thread"},"attention":null}),
            source_message_ids: vec![7],
            ..Default::default()
        };
        let run = run_agent(
            connection,
            &AgentTriageConfig::default(),
            context,
            &ExtraEvidence,
        )
        .await
        .unwrap();
        assert_eq!(run.source_message_ids, vec![7, 8]);
        assert_eq!(run.tool_calls, 1);
        assert_eq!(run.decision.attention.evidence[0].message_id, 7);
        server.abort();
    }

    #[tokio::test]
    async fn failed_validation_still_reports_billed_usage() {
        use axum::{Router, routing::post};
        use std::sync::{Arc, Mutex};
        let responses = Arc::new(Mutex::new(std::collections::VecDeque::from([
            json!({"result": {"step": "finish", "decision": MessageDecision::default()}}),
        ])));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/", post(mock_provider))
            .with_state(responses);
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let http = reqwest::Client::new();
        let connection = AgentConnection {
            http: &http,
            url: &url,
            api_key: "test",
            provider: Stage2Provider::OpenAI,
            model: "test",
            effort: None,
        };
        let mut config = AgentTriageConfig::default();
        config.agent.max_model_turns = 1;
        let failure = run_agent(connection, &config, ContextSnapshot::default(), &NoEvidence)
            .await
            .err()
            .unwrap();
        assert_eq!(failure.kind, "agent_turn_budget_exhausted");
        assert_eq!(failure.usage.len(), 1);
        assert_eq!(failure.usage[0].input_tokens, 12);
        server.abort();
    }
    #[test]
    fn shortened_pages_resume_at_visible_unicode_boundary() {
        let mut value = json!({"body": "é".repeat(10000), "body_offset": 50, "next_offset": 10050});
        bound_evidence_text(&mut value, 1000);
        let visible = value["body"].as_str().unwrap().chars().count();
        assert_eq!(value["next_offset"], json!(50 + visible));
    }
}
