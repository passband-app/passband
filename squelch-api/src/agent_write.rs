//! Agent writes (issue #394): the owner's setting for what an agent may write,
//! and the one implementation of [`AgentMailer`] the agent door is handed.
//!
//! WRAPPED, NOT HANDED OVER. The agent door never holds the Gmail write
//! credential and never builds a message. It passes a composition to
//! [`ApiAgentMailer`], which runs it through the SAME pipeline the owner's own
//! send takes (`validate_send`, the outbound secret guard, `perform_send`),
//! with three differences, all of them narrower:
//!
//! - the owner's [`AgentWriteAccess`] is read from the store on every call and
//!   decides whether anything happens at all;
//! - the guard cannot be overridden. A person may look at what the guard found
//!   and send anyway; an agent may not, because the override is exactly the
//!   decision a prompt-injected agent would be talked into making;
//! - every ledger row is written by actor `agent`, not `client-api`, so the
//!   audit page tells the owner which sends were not typed by them.
//!
//! An agent may only reply to mail it can read: the reply target goes through
//! the agent door's own access check, and a restricted message answers exactly
//! as an unknown one does.

use std::sync::Arc;

use async_trait::async_trait;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use squelch_core::agent_write::{
    AgentCompose, AgentDraftCreated, AgentMailer, AgentSent, AgentWriteAccess, AgentWriteError,
};
use squelch_core::error::CoreError;
use squelch_core::store::{DraftFields, NewAuditEntry, Store};

use crate::error::ApiError;
use crate::gmail_write::reply_subject;
use crate::handlers::{
    SendBody, SendGuard, SendOutcome, audit_action, composition_kinds, guard_verdict, no_store,
    perform_send, send_target, store_call, validate_send,
};
use crate::state::ApiState;

/// Actor written to the audit log for everything an agent writes through
/// [`ApiAgentMailer`]. The same word the agent door's `set_sender_rule` uses.
pub(crate) const AGENT_ACTOR: &str = "agent";

/// Audit action for an agent leaving a draft. The owner's own drafts are never
/// audited (an unsent composition is their own thinking); an agent's is a
/// write by somebody else, so it is. IDS ONLY, never the text.
const AUDIT_DRAFT: &str = "draft.create";
/// Audit action for the owner changing the setting.
const AUDIT_ACCESS: &str = "agent.write_access";

/// The longest body an agent may hand over, in bytes. Far past any real
/// email, and short of the point where one tool call could fill the store.
const MAX_BODY_BYTES: usize = 200_000;
/// The longest subject or recipient list, in bytes.
const MAX_HEADER_BYTES: usize = 4_000;

/// The human door's send pipeline, offered to the agent door as an
/// [`AgentMailer`]. Built by [`ApiState::agent_mailer`].
pub struct ApiAgentMailer {
    /// The daemon's state with its audit actor switched to [`AGENT_ACTOR`], so
    /// every row the shared pipeline writes is attributed to the agent.
    state: ApiState,
}

impl ApiAgentMailer {
    pub(crate) fn new(state: &ApiState) -> Self {
        Self {
            state: state.clone().with_audit_actor(AGENT_ACTOR),
        }
    }

    async fn access(&self) -> Result<AgentWriteAccess, AgentWriteError> {
        store_call(&self.state, |store, account_id| {
            AgentWriteAccess::load(store, account_id)
        })
        .await
        .map_err(|_| internal())
    }

    /// The reply target, IF AN AGENT MAY READ IT. `thread_id_for_message`
    /// applies the per-message access check and `external_thread_allowed` the
    /// whole-thread one, which is the pair `get_thread` resolves a message id
    /// with. Unknown, another account's, pending and restricted all answer
    /// [`AgentWriteError::NotFound`].
    async fn readable_parent(
        &self,
        message_id: i64,
    ) -> Result<squelch_core::store::ActionMessageRef, AgentWriteError> {
        store_call(&self.state, move |store, account_id| {
            let Some(thread_id) = Store::thread_id_for_message(store, account_id, message_id)?
            else {
                return Err(CoreError::NotFound);
            };
            if !store.external_thread_allowed(account_id, &thread_id)? {
                return Err(CoreError::NotFound);
            }
            store.action_message_ref(account_id, message_id)
        })
        .await
        .map_err(|e| {
            if e.status == StatusCode::NOT_FOUND {
                AgentWriteError::NotFound
            } else {
                internal()
            }
        })
    }
}

fn internal() -> AgentWriteError {
    AgentWriteError::Failed("Passband could not complete this. Try again later.".into())
}

fn blank(s: &Option<String>) -> bool {
    s.as_deref().is_none_or(|s| s.trim().is_empty())
}

/// The checks both writes share, before anything is read or spent.
fn validate_shape(compose: &AgentCompose) -> Result<(), AgentWriteError> {
    if compose.body.trim().is_empty() {
        return Err(AgentWriteError::Invalid("body must not be empty".into()));
    }
    if compose.body.len() > MAX_BODY_BYTES {
        return Err(AgentWriteError::Invalid(format!(
            "body is too long (limit {MAX_BODY_BYTES} bytes)"
        )));
    }
    for (name, value) in [
        ("to", &compose.to),
        ("cc", &compose.cc),
        ("bcc", &compose.bcc),
        ("subject", &compose.subject),
    ] {
        let Some(value) = value else { continue };
        if value.len() > MAX_HEADER_BYTES {
            return Err(AgentWriteError::Invalid(format!("{name} is too long")));
        }
        // A line break in a header field is how one header becomes two.
        if value.contains(['\r', '\n']) {
            return Err(AgentWriteError::Invalid(format!(
                "{name} must be a single line"
            )));
        }
    }
    Ok(())
}

#[async_trait]
impl AgentMailer for ApiAgentMailer {
    async fn create_draft(
        &self,
        compose: AgentCompose,
    ) -> Result<AgentDraftCreated, AgentWriteError> {
        let target = compose.reply_to_message_id.map(|id| id.to_string());
        if !self.access().await?.allows_drafts() {
            audit_action(
                &self.state,
                AUDIT_DRAFT,
                target,
                "rejected:agent_not_allowed",
            )
            .await;
            return Err(AgentWriteError::NotAllowed(AgentWriteAccess::Drafts));
        }
        validate_shape(&compose)?;

        // A reply draft is addressed and titled the way the composer would
        // have opened it, so the owner finds a draft ready to send rather than
        // one with nobody on it.
        let parent = match compose.reply_to_message_id {
            Some(id) => Some(self.readable_parent(id).await?),
            None => None,
        };
        let to = match (&parent, blank(&compose.to)) {
            (Some(p), true) => p.from_addr.clone(),
            _ => compose.to.clone().unwrap_or_default(),
        };
        let subject = match (&parent, blank(&compose.subject)) {
            (Some(p), true) => reply_subject(&p.subject),
            _ => compose.subject.clone().unwrap_or_default(),
        };

        let reply_to = compose.reply_to_message_id;
        let audit_target = target.clone();
        // FAIL-CLOSED, like `set_sender_rule`: a draft an agent wrote is never
        // left standing without its ledger row. If the row will not write, the
        // draft is taken back out and the tool reports a failure.
        let created = store_call(&self.state, move |store, account_id| {
            let Some(draft) = store.create_agent_draft(
                account_id,
                reply_to,
                DraftFields {
                    to_addr: to.trim(),
                    cc_addr: compose.cc.as_deref().unwrap_or("").trim(),
                    bcc_addr: compose.bcc.as_deref().unwrap_or("").trim(),
                    subject: subject.trim(),
                    body: &compose.body,
                },
                Utc::now(),
            )?
            else {
                return Ok(None);
            };
            let entry = NewAuditEntry {
                actor: AGENT_ACTOR.to_string(),
                action: AUDIT_DRAFT.to_string(),
                target: audit_target,
                detail: Some(format!("ok:{}", draft.id)),
            };
            if let Err(e) = store.append_audit(account_id, &entry) {
                let _ = store.delete_draft(account_id, draft.id);
                return Err(e);
            }
            Ok(Some(draft.id))
        })
        .await
        .map_err(|_| internal())?;

        match created {
            Some(draft_id) => Ok(AgentDraftCreated { draft_id }),
            None => {
                audit_action(&self.state, AUDIT_DRAFT, target, "rejected:draft_exists").await;
                Err(AgentWriteError::Blocked(
                    "The owner already has a draft reply to that message in Passband, and an \
                     agent may not overwrite it. Tell them, and let them decide what to do \
                     with their draft."
                        .into(),
                ))
            }
        }
    }

    async fn send(&self, compose: AgentCompose) -> Result<AgentSent, AgentWriteError> {
        let target = compose.reply_to_message_id.map(|id| id.to_string());
        if !self.access().await?.allows_send() {
            audit_action(&self.state, "send", target, "rejected:agent_not_allowed").await;
            return Err(AgentWriteError::NotAllowed(AgentWriteAccess::Send));
        }
        validate_shape(&compose)?;
        if let Some(id) = compose.reply_to_message_id {
            // Checked for its refusal only; the pipeline re-reads the target.
            self.readable_parent(id).await?;
        } else if blank(&compose.to) {
            return Err(AgentWriteError::Invalid(
                "a new message needs `to`; a reply needs `reply_to_message_id`".into(),
            ));
        }

        let keep = |s: Option<String>| s.filter(|s| !s.trim().is_empty());
        let body = SendBody {
            reply_to_message_id: compose.reply_to_message_id,
            forward_of_message_id: None,
            to: keep(compose.to),
            // Absent keeps a reply's derived copies; an agent never states an
            // empty list to strip them.
            cc: keep(compose.cc),
            reply_all: false,
            bcc: keep(compose.bcc),
            group_id: None,
            group_mode: None,
            subject: keep(compose.subject),
            body: compose.body,
            body_format: Some("markdown".into()),
            // The owner's standing consent is the setting checked above.
            confirm: true,
            // NEVER for an agent. See the module docs.
            override_guard: false,
            draft_id: None,
            include_tracker: None,
            attachment_ids: Vec::new(),
        };
        debug_assert_eq!(send_target(&body), target);

        if let Err(f) = validate_send(&body) {
            audit_action(&self.state, "send", target, f.reason).await;
            return Err(AgentWriteError::Invalid(f.error.message));
        }
        let kinds = composition_kinds(&body.body, &[]);
        if guard_verdict(&self.state, "send", &kinds, false, target)
            .await
            .is_some()
        {
            return Err(AgentWriteError::Blocked(format!(
                "Passband's outbound guard blocked this message: it looks like it contains a \
                 secret ({}). An agent cannot override the guard. Tell the owner; they can \
                 send it themselves from Passband if it is intended.",
                kinds.join(", ")
            )));
        }

        match perform_send(&self.state, &body, Vec::new(), SendGuard::Settled, None).await {
            Ok(SendOutcome::Sent { thread_id, .. }) => Ok(AgentSent { thread_id }),
            // Unreachable: no group is ever set on an agent's send.
            Ok(SendOutcome::Sending { .. }) => Err(internal()),
            Err(f) if f.error.status == StatusCode::FORBIDDEN => Err(AgentWriteError::Unavailable(
                "This Passband server is not set up to send mail (it has no Gmail write \
                 access). Tell the owner."
                    .into(),
            )),
            Err(f) if f.maybe_sent => Err(AgentWriteError::Failed(
                "Gmail did not confirm the send, and the message MAY have gone out. Do not \
                 retry on your own: tell the owner and let them check their Sent mail."
                    .into(),
            )),
            Err(f) if f.error.status.is_client_error() => {
                Err(AgentWriteError::Invalid(f.error.message))
            }
            Err(f) => Err(AgentWriteError::Failed(f.error.message)),
        }
    }
}

impl ApiState {
    /// The agent door's way to write mail: this daemon's send pipeline behind
    /// the [`AgentMailer`] seam. Hand it to `squelch_mcp::agent_door_router`.
    /// Holding it grants nothing by itself; every call re-reads the owner's
    /// [`AgentWriteAccess`].
    pub fn agent_mailer(&self) -> Arc<dyn AgentMailer> {
        Arc::new(ApiAgentMailer::new(self))
    }
}

// --- GET/PUT /client/settings/agent-access ----------------------------------

fn access_body(access: AgentWriteAccess) -> impl IntoResponse {
    (no_store(), Json(json!({ "write_access": access.as_str() })))
}

/// `GET /client/settings/agent-access`: what an agent may write to this mailbox.
pub async fn get_agent_access(
    State(state): State<ApiState>,
) -> Result<impl IntoResponse, ApiError> {
    let access = store_call(&state, |store, account_id| {
        AgentWriteAccess::load(store, account_id)
    })
    .await?;
    Ok(access_body(access))
}

#[derive(Debug, Deserialize)]
pub struct AgentAccessBody {
    write_access: String,
}

/// `PUT /client/settings/agent-access`: the owner sets it. Human door only, and
/// audited with the change itself (`none->send`), because this is the switch
/// that lets something other than the owner send as them.
pub async fn put_agent_access(
    State(state): State<ApiState>,
    Json(body): Json<AgentAccessBody>,
) -> Result<impl IntoResponse, ApiError> {
    let Some(wanted) = AgentWriteAccess::parse(body.write_access.trim()) else {
        return Err(ApiError::bad_request(
            "write_access must be one of: none, drafts, send",
        ));
    };
    let before = store_call(&state, move |store, account_id| {
        let before = AgentWriteAccess::load(store, account_id)?;
        wanted.save(store, account_id)?;
        Ok(before)
    })
    .await?;
    if before != wanted {
        audit_action(
            &state,
            AUDIT_ACCESS,
            None,
            &format!("{}->{}", before.as_str(), wanted.as_str()),
        )
        .await;
    }
    Ok(access_body(wanted))
}
