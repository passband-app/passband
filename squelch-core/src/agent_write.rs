//! What an agent on the agent door may WRITE to the mailbox, and the seam it
//! writes through (issue #394).
//!
//! Two things live here because both doors need them and neither may own them:
//!
//! - [`AgentWriteAccess`], the owner's per-mailbox setting, changed in
//!   Settings > Agents. A mailbox that never touched it is at
//!   [`AgentWriteAccess::UNSET`]: drafts, nothing sent. It is read from the
//!   store on every call, so changing it takes effect on the next tool call
//!   rather than the next daemon restart.
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
///
/// NO `Default` IMPL, on purpose. There are two different "no stored answer"
/// cases and they must not share a spelling: a mailbox that never chose is
/// [`Self::UNSET`], and a value that cannot be read is [`Self::None`]. A
/// `default()` would invite `unwrap_or_default()` at exactly the call sites
/// where an error has to mean "nothing allowed".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AgentWriteAccess {
    /// The agent reads and nothing else.
    None,
    /// The agent may leave drafts in Passband for the owner to read, edit and
    /// send themselves. Nothing leaves the mailbox. What a mailbox that never
    /// opened the setting has ([`Self::UNSET`]).
    Drafts,
    /// The agent may send mail, having been told to get its human's approval
    /// for each message first.
    Send,
}

impl AgentWriteAccess {
    /// What a mailbox that has never set the level gets: drafts (the owner's
    /// decision, 2026-10-06). Nothing an agent does at this level leaves the
    /// mailbox; the only way a draft becomes mail is the owner pressing send.
    /// Sending is never a default, only ever a choice.
    pub const UNSET: Self = AgentWriteAccess::Drafts;

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

    /// The mailbox's current setting. Unset is [`Self::UNSET`]. A STORED
    /// VALUE THIS BUILD CANNOT READ FAILS CLOSED, to [`Self::None`] rather
    /// than to the unset default: a row written by a newer daemon, or a
    /// corrupted one, must never widen what this one allows, and "the owner
    /// chose something we do not understand" is not "the owner never chose".
    pub fn load(store: &dyn Store, account_id: AccountId) -> Result<Self> {
        Ok(
            match store.get_app_setting(account_id, AGENT_WRITE_ACCESS_KEY)? {
                None => Self::UNSET,
                Some(v) => Self::parse(&v).unwrap_or(AgentWriteAccess::None),
            },
        )
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
    /// Recipients, comma-joined. Optional on a reply, where it defaults to
    /// the parent's From address (the one the agent was shown, never a
    /// Reply-To it was not); required on a new message that is being sent.
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
    /// Who the mail was addressed to, as it went out. Handed back so the agent
    /// can tell its human exactly where the message went, including when it
    /// left `to` for the daemon to fill in on a reply.
    pub to: String,
    /// The visible copy list as it went out, `""` when there was none.
    pub cc: String,
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
    fn unset_is_drafts_and_an_unreadable_value_is_none() {
        let store = SqliteStore::open_in_memory().unwrap();
        let acct = store.ensure_account("me@localhost").unwrap();
        // Never chosen: drafts, and never more than drafts.
        assert_eq!(
            AgentWriteAccess::load(&store, acct).unwrap(),
            AgentWriteAccess::Drafts
        );
        assert!(!AgentWriteAccess::UNSET.allows_send());
        for junk in ["SEND", "everything", "", "send,drafts", "send ", " drafts"] {
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
            AgentWriteAccess::UNSET
        );
        // And an owner who chose read only keeps it.
        AgentWriteAccess::None.save(&store, b).unwrap();
        assert_eq!(
            AgentWriteAccess::load(&store, b).unwrap(),
            AgentWriteAccess::None
        );
    }
}
