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
//! An agent may only reply to mail it can read, and "can read" is decided by
//! the agent door's own thread read, not by a second spelling of its rules: a
//! restricted, pending or spam message answers exactly as an unknown one does.
//!
//! WHAT THE HUMAN APPROVED IS WHAT GOES OUT. The owner's own reply honors the
//! parent's `Reply-To` and derives copies from its headers, because the owner
//! can see the result in the composer. An agent sees only a message's From,
//! and its human sees only what the agent shows them, so an agent's send goes
//! to exactly the addresses it stated, or to the parent's From, and the
//! resolved recipients are handed back in the result. Nothing is derived from
//! a header nobody in that conversation was shown.

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
    AgentCompose, AgentDraftCreated, AgentDraftOpened, AgentMailer, AgentSent, AgentWriteAccess,
    AgentWriteError, draft_link,
};
use squelch_core::error::CoreError;
use squelch_core::store::{AgentDraftOutcome, DraftFields, NewAuditEntry, Store};

use crate::error::ApiError;
use crate::gmail_write::{count_addrs, reply_subject};
use crate::handlers::{
    SendBody, SendGuard, SendOutcome, audit_action, composition_kinds, guard_verdict, no_store,
    perform_send, store_call, validate_send,
};
use crate::state::ApiState;

/// Actor written to the audit log for everything an agent writes through
/// [`ApiAgentMailer`]. The same word the agent door's `set_sender_rule` uses.
pub(crate) const AGENT_ACTOR: &str = "agent";

/// Audit action for an agent leaving a draft. The owner's own drafts are never
/// audited (an unsent composition is their own thinking); an agent's is a
/// write by somebody else, so it is. IDS ONLY, never the text.
const AUDIT_DRAFT: &str = "draft.create";
/// Audit action for an agent asking the Mac to show one of its drafts.
const AUDIT_OPEN: &str = "draft.open";
/// The least time between two "show this draft" requests. Each one may pull
/// the Mac app to the front, so an agent on a loop must not be able to hold
/// the owner's screen.
const OPEN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
/// Audit action for the owner changing the setting.
const AUDIT_ACCESS: &str = "agent.write_access";

/// The longest body an agent may hand over, in bytes. Far past any real
/// email, and short of the point where one tool call could fill the store.
const MAX_BODY_BYTES: usize = 200_000;
/// The longest subject or recipient list, in bytes.
const MAX_HEADER_BYTES: usize = 4_000;
/// The most addresses one agent composition may name across to, cc and bcc.
const MAX_RECIPIENTS: usize = 50;
/// The most unsent agent drafts a mailbox holds at once. Past it the agent is
/// told to have its human send or discard some.
const MAX_AGENT_DRAFTS: usize = 25;

/// The human door's send pipeline, offered to the agent door as an
/// [`AgentMailer`]. Built by [`ApiState::agent_mailer`].
pub struct ApiAgentMailer {
    /// The daemon's state with its audit actor switched to [`AGENT_ACTOR`], so
    /// every row the shared pipeline writes is attributed to the agent.
    state: ApiState,
    /// When the last "show this draft" request was let through. See
    /// [`OPEN_INTERVAL`]. One mailer serves the whole agent door, so this is
    /// one clock for every agent on the mailbox, which is the point: it is
    /// the owner's screen that is being rationed.
    last_open: std::sync::Mutex<Option<std::time::Instant>>,
}

impl ApiAgentMailer {
    pub(crate) fn new(state: &ApiState) -> Self {
        Self {
            state: state.clone().with_audit_actor(AGENT_ACTOR),
            last_open: std::sync::Mutex::new(None),
        }
    }

    async fn access(&self) -> Result<AgentWriteAccess, AgentWriteError> {
        store_call(&self.state, |store, account_id| {
            AgentWriteAccess::load(store, account_id)
        })
        .await
        .map_err(|_| internal())
    }

    /// Write the ledger row for a refusal and hand the error back, so no
    /// refusal can be returned without one. A probing agent (which message
    /// ids exist? what gets past validation?) leaves a trail.
    async fn refuse(
        &self,
        action: &'static str,
        target: &Option<String>,
        reason: &str,
        error: AgentWriteError,
    ) -> AgentWriteError {
        audit_action(&self.state, action, target.clone(), reason).await;
        error
    }

    /// The reply target, IF AN AGENT CAN READ IT, decided by performing the
    /// agent door's own read: the message must appear in the `thread_view`
    /// that `get_thread` would serve. Asking the read itself, rather than
    /// re-listing its conditions here, is the point: an earlier version of
    /// this function re-listed them and left out spam, so an agent could
    /// reply to mail the door would never show it. Unknown, another
    /// account's, pending, restricted and spam all answer
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
            let view = Store::thread_view(store, account_id, &thread_id)?;
            if !view.messages.iter().any(|m| m.id == message_id) {
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

/// Characters that take up no space on screen. A body made only of these and
/// whitespace is an empty body that passes `trim().is_empty()`.
fn invisible(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(
            c,
            '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}' | '\u{00AD}'
        )
}

/// The checks both writes share, before anything is read or spent.
fn validate_shape(compose: &AgentCompose) -> Result<(), AgentWriteError> {
    if compose.body.chars().all(invisible) {
        return Err(AgentWriteError::Invalid("body must not be empty".into()));
    }
    if compose.body.len() > MAX_BODY_BYTES {
        return Err(AgentWriteError::Invalid(format!(
            "body is too long (limit {MAX_BODY_BYTES} bytes)"
        )));
    }
    let mut recipients = 0;
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
        // A line break in a header field is how one header becomes two, and
        // CR/LF are not the only characters a mail system will break a line
        // on. No control character and no Unicode line separator belongs in
        // an address list or a subject.
        if value
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
        {
            return Err(AgentWriteError::Invalid(format!(
                "{name} must be a single line of plain text"
            )));
        }
        if name != "subject" {
            recipients += count_addrs(value);
        }
    }
    if recipients > MAX_RECIPIENTS {
        return Err(AgentWriteError::Invalid(format!(
            "too many recipients (limit {MAX_RECIPIENTS})"
        )));
    }
    Ok(())
}

/// Everything of an agent's composition the outbound guard reads.
///
/// MORE THAN THE BODY, which is all the owner's own send scans. The owner
/// typed their subject and chose their recipients; an agent's are as much
/// model output as its body, and for an agent the guard is the one check with
/// no override, so a key moved into the subject line or a display name must
/// not walk past it. The body is scanned twice: as written, and with markdown
/// emphasis marks removed, so `sk-abc**DEF**...` reads as the token it
/// renders to.
fn guard_text(to: &str, cc: &str, bcc: &str, subject: &str, body: &str) -> String {
    let plain: String = body
        .chars()
        .filter(|c| !matches!(c, '*' | '_' | '`' | '~'))
        .collect();
    [to, cc, bcc, subject, body, &plain].join("\n")
}

#[async_trait]
impl AgentMailer for ApiAgentMailer {
    async fn create_draft(
        &self,
        compose: AgentCompose,
    ) -> Result<AgentDraftCreated, AgentWriteError> {
        let target = compose.reply_to_message_id.map(|id| id.to_string());
        if !self.access().await?.allows_drafts() {
            return Err(self
                .refuse(
                    AUDIT_DRAFT,
                    &target,
                    "rejected:agent_not_allowed",
                    AgentWriteError::NotAllowed(AgentWriteAccess::Drafts),
                )
                .await);
        }
        if let Err(e) = validate_shape(&compose) {
            return Err(self
                .refuse(AUDIT_DRAFT, &target, "rejected:invalid", e)
                .await);
        }

        // A reply draft is addressed and titled the way the composer would
        // have opened it, so the owner finds a draft ready to send rather than
        // one with nobody on it. From the stored From, like an agent's send.
        let parent = match compose.reply_to_message_id {
            Some(id) => match self.readable_parent(id).await {
                Ok(p) => Some(p),
                Err(e) => return Err(self.refuse(AUDIT_DRAFT, &target, "failed:target", e).await),
            },
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
        let entry = NewAuditEntry {
            actor: AGENT_ACTOR.to_string(),
            action: AUDIT_DRAFT.to_string(),
            target: target.clone(),
            // Completed by the store with the new draft's id.
            detail: None,
        };
        // The draft and its ledger row are one transaction in the store.
        let outcome = store_call(&self.state, move |store, account_id| {
            store.create_agent_draft(
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
                MAX_AGENT_DRAFTS,
                &entry,
            )
        })
        .await
        .map_err(|_| internal())?;

        match outcome {
            AgentDraftOutcome::Created(draft_id) => {
                // The mailbox is only decoration on the link; a read that
                // fails costs the hint, never the draft that was just saved.
                let mailbox = store_call(&self.state, |store, account_id| {
                    store.account_email(account_id)
                })
                .await
                .unwrap_or_default();
                Ok(AgentDraftCreated {
                    draft_id,
                    link: draft_link(&mailbox, draft_id),
                })
            }
            AgentDraftOutcome::ReplyDraftExists => Err(self
                .refuse(
                    AUDIT_DRAFT,
                    &target,
                    "rejected:draft_exists",
                    AgentWriteError::Blocked(
                        "That message already has a draft reply in Passband, and an agent may \
                         not overwrite one. Tell the owner, and let them decide what to do \
                         with the draft that is there."
                            .into(),
                    ),
                )
                .await),
            AgentDraftOutcome::TooMany => Err(self
                .refuse(
                    AUDIT_DRAFT,
                    &target,
                    "rejected:too_many_drafts",
                    AgentWriteError::Blocked(format!(
                        "Passband already holds {MAX_AGENT_DRAFTS} unsent drafts from agents. \
                         Ask the owner to send or discard some before leaving more."
                    )),
                )
                .await),
        }
    }

    async fn open_draft(&self, draft_id: i64) -> Result<AgentDraftOpened, AgentWriteError> {
        let target = Some(draft_id.to_string());
        if !self.access().await?.allows_drafts() {
            return Err(self
                .refuse(
                    AUDIT_OPEN,
                    &target,
                    "rejected:agent_not_allowed",
                    AgentWriteError::NotAllowed(AgentWriteAccess::Drafts),
                )
                .await);
        }
        // ONLY A DRAFT AN AGENT WROTE. The owner's own drafts are theirs to
        // open; naming one here answers exactly as an id that does not exist,
        // so this cannot be used to count or find them either.
        let mailbox = store_call(&self.state, move |store, account_id| {
            match store.draft_by_id(account_id, draft_id)? {
                Some(draft) if draft.from_agent => store.account_email(account_id).map(Some),
                _ => Ok(None),
            }
        })
        .await
        .map_err(|_| internal())?;
        let Some(mailbox) = mailbox else {
            return Err(self
                .refuse(
                    AUDIT_OPEN,
                    &target,
                    "failed:target",
                    AgentWriteError::NotFound,
                )
                .await);
        };

        let too_soon = {
            let mut last = self.last_open.lock().unwrap_or_else(|e| e.into_inner());
            let now = std::time::Instant::now();
            let too_soon = last.is_some_and(|at| now.duration_since(at) < OPEN_INTERVAL);
            if !too_soon {
                *last = Some(now);
            }
            too_soon
        };
        if too_soon {
            return Err(self
                .refuse(
                    AUDIT_OPEN,
                    &target,
                    "rejected:too_soon",
                    AgentWriteError::Blocked(
                        "Passband was asked to show a draft a moment ago and will not take \
                         the owner's screen again so soon. The draft is saved; give them its \
                         link instead."
                            .into(),
                    ),
                )
                .await);
        }

        // Told to every Mac app listening for this mailbox. `send` errs only
        // when nothing is, which is the answer rather than a failure.
        let on_mac = self.state.draft_opens.send(draft_id).is_ok();
        audit_action(
            &self.state,
            AUDIT_OPEN,
            target,
            if on_mac { "ok:mac" } else { "ok:no_mac" },
        )
        .await;
        Ok(AgentDraftOpened {
            on_mac,
            link: draft_link(&mailbox, draft_id),
        })
    }

    async fn send(&self, compose: AgentCompose) -> Result<AgentSent, AgentWriteError> {
        let target = compose.reply_to_message_id.map(|id| id.to_string());
        if !self.access().await?.allows_send() {
            return Err(self
                .refuse(
                    "send",
                    &target,
                    "rejected:agent_not_allowed",
                    AgentWriteError::NotAllowed(AgentWriteAccess::Send),
                )
                .await);
        }
        if let Err(e) = validate_shape(&compose) {
            return Err(self.refuse("send", &target, "rejected:invalid", e).await);
        }

        // WHO IT GOES TO IS DECIDED HERE, never by the pipeline's derivation
        // (see the module docs): the stated `to`, else the parent's stored
        // From, and the stated `cc` or none.
        let to = match compose.reply_to_message_id {
            Some(id) => {
                let parent = match self.readable_parent(id).await {
                    Ok(p) => p,
                    Err(e) => return Err(self.refuse("send", &target, "failed:target", e).await),
                };
                if blank(&compose.to) {
                    parent.from_addr
                } else {
                    compose.to.clone().unwrap_or_default()
                }
            }
            None => {
                if blank(&compose.to) {
                    return Err(self
                        .refuse(
                            "send",
                            &target,
                            "rejected:no_recipient",
                            AgentWriteError::Invalid(
                                "a new message needs `to`; a reply needs `reply_to_message_id`"
                                    .into(),
                            ),
                        )
                        .await);
                }
                if blank(&compose.subject) {
                    return Err(self
                        .refuse(
                            "send",
                            &target,
                            "rejected:no_subject",
                            AgentWriteError::Invalid("a new message needs `subject`".into()),
                        )
                        .await);
                }
                compose.to.clone().unwrap_or_default()
            }
        };
        let to = to.trim().to_string();
        let cc = compose.cc.as_deref().unwrap_or("").trim().to_string();
        let bcc = compose.bcc.as_deref().unwrap_or("").trim().to_string();
        let subject = compose.subject.as_deref().unwrap_or("").trim().to_string();

        let body = SendBody {
            reply_to_message_id: compose.reply_to_message_id,
            forward_of_message_id: None,
            to: Some(to.clone()),
            // STATED, even when empty: a present `cc` replaces the pipeline's
            // derivation from the parent's headers.
            cc: Some(cc.clone()),
            reply_all: false,
            bcc: Some(bcc.clone()).filter(|s| !s.is_empty()),
            group_id: None,
            group_mode: None,
            // Absent on a reply reuses the parent's subject.
            subject: Some(subject.clone()).filter(|s| !s.is_empty()),
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

        if let Err(f) = validate_send(&body) {
            audit_action(&self.state, "send", target, f.reason).await;
            return Err(AgentWriteError::Invalid(f.error.message));
        }
        let kinds = composition_kinds(&guard_text(&to, &cc, &bcc, &subject, &body.body), &[]);
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
            Ok(SendOutcome::Sent { thread_id, .. }) => Ok(AgentSent { thread_id, to, cc }),
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
            Err(f) if f.error.status == StatusCode::NOT_FOUND => Err(AgentWriteError::NotFound),
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
    // LEDGER FIRST, and fail closed: the switch that lets something other
    // than the owner send as them is never thrown without its row. If the
    // row will not write, nothing changes.
    store_call(&state, move |store, account_id| {
        let before = AgentWriteAccess::load(store, account_id)?;
        if before == wanted {
            return Ok(());
        }
        store.append_audit(
            account_id,
            &NewAuditEntry {
                actor: crate::handlers::ACTION_ACTOR.to_string(),
                action: AUDIT_ACCESS.to_string(),
                target: None,
                detail: Some(format!("{}->{}", before.as_str(), wanted.as_str())),
            },
        )?;
        wanted.save(store, account_id)
    })
    .await?;
    Ok(access_body(wanted))
}
