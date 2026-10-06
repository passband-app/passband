//! What an agent on the agent door may WRITE to the mailbox, and the seam it
//! writes through (issue #394).
//!
//! Two things live here because both doors need them and neither may own them:
//!
//! - [`AgentWriteAccess`], the owner's per-mailbox setting. Off unless the
//!   owner turned it on in Settings > Agents, and read from the store on every
//!   call, so flipping it back off takes effect on the next tool call rather
//!   than the next daemon restart.
//! - [`AgentMailer`], the trait the agent door calls to create a draft or send
//!   mail. The agent door holds a `dyn AgentMailer` and nothing else: the
//!   implementation lives beside the human door's send pipeline (squelch-api),
//!   which is the only code that ever touches the Gmail write credential. The
//!   agent proposes a composition; the pipeline (validation, outbound secret
//!   guard, audit log) decides whether it leaves.

use async_trait::async_trait;

use crate::error::Result;
use crate::store::Store;
use crate::types::AccountId;

/// `app_settings.key` holding the mailbox's [`AgentWriteAccess`].
pub const AGENT_WRITE_ACCESS_KEY: &str = "agent.write_access";

/// How much an agent may write to the mailbox. ORDERED: each level includes
/// the ones before it, so an agent allowed to send may also leave a draft.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum AgentWriteAccess {
    /// The agent reads and nothing else. The default, and what every mailbox
    /// that never opened the setting has.
    #[default]
    None,
    /// The agent may leave drafts in Passband for the owner to read, edit and
    /// send themselves. Nothing leaves the mailbox.
    Drafts,
    /// The agent may send mail, having been told to get its human's approval
    /// for each message first.
    Send,
}

impl AgentWriteAccess {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentWriteAccess::None => "none",
            AgentWriteAccess::Drafts => "drafts",
            AgentWriteAccess::Send => "send",
        }
    }

    /// Exact wire values only. An unreadable value is `None` here so each
    /// caller decides what that means: the settings route answers 400, the
    /// stored-value read falls closed.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(AgentWriteAccess::None),
            "drafts" => Some(AgentWriteAccess::Drafts),
            "send" => Some(AgentWriteAccess::Send),
            _ => None,
        }
    }

    pub fn allows_drafts(self) -> bool {
        self >= AgentWriteAccess::Drafts
    }

    pub fn allows_send(self) -> bool {
        self >= AgentWriteAccess::Send
    }

    /// The mailbox's current setting. FAILS CLOSED: unset, and any value this
    /// build does not recognize, both read as [`AgentWriteAccess::None`], so a
    /// row written by a newer daemon can never widen what an older one allows.
    pub fn load(store: &dyn Store, account_id: AccountId) -> Result<Self> {
        Ok(store
            .get_app_setting(account_id, AGENT_WRITE_ACCESS_KEY)?
            .and_then(|v| Self::parse(v.trim()))
            .unwrap_or_default())
    }

    pub fn save(self, store: &dyn Store, account_id: AccountId) -> Result<()> {
        store.set_app_setting(account_id, AGENT_WRITE_ACCESS_KEY, self.as_str())
    }
}

/// A composition an agent proposes, as a draft or as a send. Deliberately
/// narrow next to the human composer: no attachments, no forward, no send
/// group, no read tracking, and no way to override the outbound guard.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentCompose {
    /// The stored message this replies to. Must be one the agent can read.
    pub reply_to_message_id: Option<i64>,
    /// Recipients, comma-joined. Optional on a reply (derived from the
    /// parent), required on a new message that is being sent.
    pub to: Option<String>,
    pub cc: Option<String>,
    pub bcc: Option<String>,
    /// Optional on a reply (derived from the parent's subject).
    pub subject: Option<String>,
    /// The message text, as markdown.
    pub body: String,
}

/// A draft the agent left for the owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDraftCreated {
    pub draft_id: i64,
}

/// Mail the agent sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSent {
    /// The conversation the mail is in: the parent's, or the one just created.
    pub thread_id: Option<String>,
}

/// Why an agent's write did not happen. Every message here is written to be
/// read BY THE AGENT and relayed to its human, so each says what to do next
/// and none carries mail content or upstream detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentWriteError {
    /// The owner's setting does not allow this. Carries the level it needs.
    NotAllowed(AgentWriteAccess),
    /// The composition is malformed (no body, no recipient, ...).
    Invalid(String),
    /// The message being replied to is unknown or not readable by an agent.
    /// One answer for both, so a restricted message's existence is not
    /// confirmed by trying to reply to it.
    NotFound,
    /// Refused by a safeguard (the outbound secret guard, an existing draft).
    Blocked(String),
    /// This daemon cannot write mail at all (no write credential).
    Unavailable(String),
    /// The write was attempted and failed.
    Failed(String),
}

impl std::fmt::Display for AgentWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentWriteError::NotAllowed(needed) => {
                let what = match needed {
                    AgentWriteAccess::Send => "send email",
                    _ => "create drafts",
                };
                write!(
                    f,
                    "The mailbox owner has not allowed agents to {what}. They can change this \
                     in Passband under Settings > Agents. Do not retry; tell them."
                )
            }
            AgentWriteError::Invalid(m)
            | AgentWriteError::Blocked(m)
            | AgentWriteError::Unavailable(m)
            | AgentWriteError::Failed(m) => f.write_str(m),
            AgentWriteError::NotFound => f.write_str("not found"),
        }
    }
}

impl std::error::Error for AgentWriteError {}

/// The agent door's only way to write mail. Implemented over the human door's
/// send pipeline; see the module docs.
///
/// EVERY IMPLEMENTATION RE-CHECKS [`AgentWriteAccess`] ITSELF. The agent door
/// checks it too, to decide which tools to offer, but the check that counts
/// sits beside the write, where no caller can forget it.
#[async_trait]
pub trait AgentMailer: Send + Sync {
    /// Leave a draft for the owner. Needs [`AgentWriteAccess::Drafts`].
    async fn create_draft(
        &self,
        compose: AgentCompose,
    ) -> std::result::Result<AgentDraftCreated, AgentWriteError>;

    /// Send mail now. Needs [`AgentWriteAccess::Send`].
    async fn send(&self, compose: AgentCompose) -> std::result::Result<AgentSent, AgentWriteError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SqliteStore;

    #[test]
    fn levels_are_ordered_and_round_trip() {
        for level in [
            AgentWriteAccess::None,
            AgentWriteAccess::Drafts,
            AgentWriteAccess::Send,
        ] {
            assert_eq!(AgentWriteAccess::parse(level.as_str()), Some(level));
        }
        assert!(!AgentWriteAccess::None.allows_drafts());
        assert!(AgentWriteAccess::Drafts.allows_drafts());
        assert!(!AgentWriteAccess::Drafts.allows_send());
        assert!(AgentWriteAccess::Send.allows_drafts());
        assert!(AgentWriteAccess::Send.allows_send());
    }

    #[test]
    fn unset_and_unrecognized_both_read_as_none() {
        let store = SqliteStore::open_in_memory().unwrap();
        let acct = store.ensure_account("me@localhost").unwrap();
        assert_eq!(
            AgentWriteAccess::load(&store, acct).unwrap(),
            AgentWriteAccess::None
        );
        for junk in ["SEND", "everything", "", "send,drafts"] {
            Store::set_app_setting(&store, acct, AGENT_WRITE_ACCESS_KEY, junk).unwrap();
            assert_eq!(
                AgentWriteAccess::load(&store, acct).unwrap(),
                AgentWriteAccess::None,
                "{junk:?} must not grant anything"
            );
        }
        AgentWriteAccess::Send.save(&store, acct).unwrap();
        assert_eq!(
            AgentWriteAccess::load(&store, acct).unwrap(),
            AgentWriteAccess::Send
        );
    }

    #[test]
    fn the_setting_is_per_account() {
        let store = SqliteStore::open_in_memory().unwrap();
        let a = store.ensure_account("a@localhost").unwrap();
        let b = store.ensure_account("b@localhost").unwrap();
        AgentWriteAccess::Send.save(&store, a).unwrap();
        assert_eq!(
            AgentWriteAccess::load(&store, b).unwrap(),
            AgentWriteAccess::None
        );
    }
}
