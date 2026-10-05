//! Pending OAuth sessions: the server-side half of the flow, holding the two
//! values that must never ride in a cookie.
//!
//! - `state`, the CSRF token echoed by Google. Compared constant-time against
//!   what comes back on the callback.
//! - The PKCE verifier. This is what makes an intercepted authorization code
//!   worthless: whoever holds the code still cannot redeem it without the
//!   verifier, and the verifier lives here, in this process's memory, for ten
//!   minutes.
//!
//! SEVERAL KINDS OF SESSION share this table, because they are the same
//! ten-minute hop through Google and differ only in what they ask for and what
//! they do on the way back. A SIGNUP holds an invite reservation and provisions
//! a tenant. A CONSOLE login holds nothing, spends nothing, and asks Google for
//! identity alone; it ends in a pairing code for a tenant that already exists.
//! An APP login is the console login with its one input removed: it does not
//! know which tenant it is for, because it finds that out from the mailbox
//! Google names. An AGENT login is the app login on behalf of WorkOS AuthKit,
//! and it holds the one thing AuthKit gave it to finish with. The
//! [`SessionKind`] is what the callback branches on, and it is the AUTHORITY:
//! the cookie's copy of the same fact is checked against it, never trusted in
//! its place.
//!
//! In memory rather than in the store, deliberately. A restart drops pending
//! consents, and that is the correct behaviour: the recovery is to start the
//! signup again, and a verifier persisted to disk is a verifier that can be
//! read off disk.
//!
//! ONE-SHOT: [`SessionStore::take`] removes the session. A callback that is
//! replayed, or a code that is delivered twice, finds nothing.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Notify;

/// How long a session lives. The whole budget is one human reading one Google
/// consent screen. Matches [`crate::cookie::COOKIE_TTL_SECS`].
pub const SESSION_TTL: Duration = Duration::from_secs(10 * 60);

/// How long a CONFIRMED agent login waits for its person to press Continue.
/// Shorter than [`SESSION_TTL`] because nothing about it involves Google any
/// more: it is one page with two buttons, read by somebody who has just signed
/// in, and every minute it lives is a minute a verified identity sits waiting
/// for a click.
pub const AGENT_CONFIRM_TTL: Duration = Duration::from_secs(5 * 60);

/// How long a COMPLETED agent login remembers where it sent the browser, so a
/// double-clicked Continue gets the same answer instead of a refusal. Seconds,
/// not minutes: it exists only to cover a second press that was already in
/// flight. See [`SessionKind::AgentDone`].
pub const AGENT_DONE_TTL: Duration = Duration::from_secs(10);

/// How long the IN-FLIGHT placeholder lives: the WorkOS completion's own
/// budget plus five seconds. Long enough that a waiting second press always
/// sees the first one's answer land; short enough that a first request whose
/// task was dropped mid-call (its client went away) leaves a placeholder that
/// is gone moments after the call it stood for could have finished. See
/// [`SessionKind::AgentInFlight`].
pub const AGENT_IN_FLIGHT_TTL: Duration =
    Duration::from_secs(crate::workos::COMPLETE_TIMEOUT.as_secs() + 5);

/// The only form of a session id allowed to touch the control store: lowercase
/// hex SHA-256.
///
/// The invite reservation has to name the session holding it, and the store's
/// standing rule is that it keeps hashes rather than credentials. Equality is
/// all a reservation ever asks of this value, so the preimage never needs to
/// leave memory.
pub fn fingerprint(sid: &str) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    Sha256::digest(sid.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut out, b| {
            // Writing to a String cannot fail.
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// Hard ceiling on live sessions. Both flows are rate limited per client, but
/// the table must be bounded by something that does not depend on the limiter's
/// identity model being right: past this, new sessions are refused with a "try
/// again shortly" rather than the process growing without limit.
pub const MAX_SESSIONS: usize = 4_096;

/// Hard ceiling on live IDENTITY sessions, CARVED OUT OF [`MAX_SESSIONS`]
/// rather than added to it. Every hop a stranger can open spends it:
/// `GET /console/auth`, `GET /app/auth`, `GET /reconnect` and `GET /agent/auth`
/// (see [`SessionKind::is_identity`]).
///
/// THE ASYMMETRY IS THE POINT. Either login opens a session with no credential
/// and no invite behind it, so they are the cheap half of this table to flood; a
/// signup has to present a plausible invite code first. Without a per-kind
/// ceiling, a stranger looping a login hop fills all [`MAX_SESSIONS`] slots and
/// every real signup is refused. With it, login flooding can consume at most
/// this many entries and a signup always has
/// `MAX_SESSIONS - MAX_IDENTITY_SESSIONS` slots it cannot be crowded out of.
///
/// ONE CEILING FOR BOTH, not one each. They are the same errand through the same
/// consent, and two budgets would only mean a flooder gets to spend twice for
/// asking the same question two ways.
///
/// The reverse is deliberately NOT true: a table filled with real signups will
/// refuse a login too. Provisioning a mailbox is the flow that cannot be retried
/// cheaply, so it is the one that gets the whole table when it needs it.
pub const MAX_IDENTITY_SESSIONS: usize = 512;

/// Which flow a pending session belongs to, and therefore what the callback
/// does with it.
///
/// The invite id rides INSIDE the signup variant rather than beside the kind,
/// so "a console login has no invite" is a fact the type carries instead of a
/// sentinel row somebody has to remember to check. The agent login's
/// `external_auth_id` rides inside its variant for the same reason, which is
/// also why this type is `Clone` and not `Copy`.
///
/// `Debug` is HAND-WRITTEN, because a derived one would print the
/// `external_auth_id` into any line that ever formats a kind.
#[derive(Clone, PartialEq, Eq)]
pub enum SessionKind {
    /// A signup: this session holds a reservation on `invite_id` and will
    /// provision the label it names.
    Signup { invite_id: i64 },
    /// A console login for a tenant that already exists. Spends nothing, holds
    /// nothing, and asks Google only who is signed in.
    Console,
    /// An app login: the same identity hop with no tenant named going in. The
    /// label on a session of this kind is EMPTY and stays empty, because the
    /// mailbox Google names on the way back is what decides which tenant this
    /// was for.
    App,
    /// A RECONNECT: a fresh Gmail grant for a mailbox that already has a
    /// tenant, replacing a refresh token that stopped working.
    ///
    /// Like [`SessionKind::App`] its label is EMPTY on the way out and stays
    /// empty. Nobody types which mailbox they are reconnecting: Google says
    /// which account consented, and that is the only answer this flow will
    /// accept. Asking would turn the page into an oracle and, worse, would let
    /// somebody aim a credential they legitimately own at a tenant they do not.
    Reconnect,
    /// An AGENT login: the app login's identity hop, run because WorkOS AuthKit
    /// sent the browser here to log somebody in for an agent's OAuth flow.
    ///
    /// Its label is EMPTY and stays empty, exactly as an app login's: the
    /// tenant is found from the mailbox Google names. What it carries instead
    /// is `external_auth_id`, AuthKit's opaque handle for the authorization in
    /// progress, which the callback sends back to WorkOS and nowhere else.
    ///
    /// SERVER-SIDE ONLY. The id is not in the cookie, and that is a choice, not
    /// an omission. The cookie is a value the client holds and this table is
    /// the authority on what a session is for; if the id rode in both, the
    /// callback would have to pick one, and the only safe pick is this one, so
    /// a second copy would buy nothing but a place to get the comparison wrong.
    /// It would also put a live AuthKit handle in one more place a browser
    /// keeps (cookie jars are synced, exported and backed up), for no property
    /// gained. The cookie carries a bare marker instead, for refusal copy only:
    /// see [`crate::cookie::SessionClaim::agent`].
    Agent { external_auth_id: String },
    /// An agent login that has CLEARED GOOGLE and is waiting on its person.
    ///
    /// THE CONFIRMATION STEP exists because the agent hop's link can be handed
    /// to somebody: an attacker who registers an OAuth client under a name of
    /// their choosing ("Claude") can start an authorization aimed at a
    /// victim's tenant and mail the victim our `/agent/auth?external_auth_id=`
    /// link. Google's account chooser asks nothing of somebody who has signed
    /// in before, so without a page of ours in between, one "Allow" on
    /// AuthKit's screen handed the attacker a token for the victim's mail.
    /// This kind is what the callback parks INSTEAD of calling WorkOS, and only
    /// `POST /agent/confirm` spends it.
    ///
    /// UNDER A FRESH SESSION ID with a fresh cookie, never the pre-Google one:
    /// the id that went out before anybody authenticated is not the id that
    /// holds a verified identity. The entry's `state` is the form's one-shot
    /// CSRF token, its `pkce_verifier` is empty (the code was spent), and its
    /// `label` is the tenant the mailbox owns. It lives [`AGENT_CONFIRM_TTL`].
    ///
    /// Everything WorkOS will be told rides here, decided once on the callback
    /// from Google's verified answer and the tenant record:
    AgentConfirm {
        /// AuthKit's handle, carried over from the [`SessionKind::Agent`]
        /// session this replaced.
        external_auth_id: String,
        /// The mailbox as the tenant record stores it: `user.email`.
        stored_email: String,
        /// Google's verified address, trimmed and lowercased: `user.id`.
        user_id: String,
    },
    /// An agent login WorkOS has already COMPLETED, remembered for
    /// [`AGENT_DONE_TTL`] under the same session id the confirmation had.
    ///
    /// A DOUBLE CLICK is the reason. Two rapid presses of Continue send two
    /// POSTs; the first takes the confirmation, completes at WorkOS (spending
    /// AuthKit's handle) and answers a 303 the browser then abandons for the
    /// second, which used to find nothing and show a refusal. The person had
    /// to start over for having clicked twice. With this marker the second
    /// POST, carrying the same signed cookie and the same CSRF token, gets the
    /// same 303.
    ///
    /// WHAT IT CANNOT DO: reach WorkOS. Only [`SessionKind::AgentConfirm`]
    /// carries what a completion needs, and that was taken, once, before this
    /// was written; the marker holds a URL already validated and already given
    /// to this same browser. It is not charged to the stranger-openable pool
    /// (only a completed login writes one) and lives in this table, so the
    /// whole-table ceiling bounds it.
    AgentDone {
        /// The validated AuthKit URL the first press was sent to.
        redirect: String,
    },
    /// A confirmation whose WorkOS completion is IN FLIGHT right now, under
    /// the same session id and the same CSRF token.
    ///
    /// A real double click's second POST lands 100 to 300 ms after the first,
    /// which is usually while the first is still waiting on WorkOS: the
    /// confirmation is taken and no [`SessionKind::AgentDone`] exists yet. This
    /// placeholder is what that second press finds; it WAITS for the first to
    /// replace it with a done or failed marker and answers the same. It holds
    /// nothing a completion needs, so nothing that finds it can call WorkOS.
    /// Lives [`AGENT_IN_FLIGHT_TTL`].
    AgentInFlight,
    /// A completion that FAILED, remembered for [`AGENT_DONE_TTL`] so a press
    /// that waited on it (or lands just after) shows the same page class the
    /// first press got. Which page, and nothing else.
    AgentFailed {
        /// AuthKit refused this authorization (spent or expired), as opposed
        /// to WorkOS being unreachable or answering something unusable.
        expired: bool,
    },
}

impl std::fmt::Debug for SessionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionKind::Signup { invite_id } => f
                .debug_struct("Signup")
                .field("invite_id", invite_id)
                .finish(),
            SessionKind::Console => f.write_str("Console"),
            SessionKind::App => f.write_str("App"),
            SessionKind::Reconnect => f.write_str("Reconnect"),
            SessionKind::Agent { .. } => f
                .debug_struct("Agent")
                .field("external_auth_id", &"<redacted>")
                .finish(),
            // Every field is an identity or AuthKit's handle.
            SessionKind::AgentConfirm { .. } => f.write_str("AgentConfirm { <redacted> }"),
            // The URL carries AuthKit's state for this authorization.
            SessionKind::AgentDone { .. } => f.write_str("AgentDone { <redacted> }"),
            SessionKind::AgentInFlight => f.write_str("AgentInFlight"),
            SessionKind::AgentFailed { expired } => f
                .debug_struct("AgentFailed")
                .field("expired", expired)
                .finish(),
        }
    }
}

impl SessionKind {
    /// The invite row this session holds, if it holds one. `None` for either
    /// login, which is exactly what the release path wants: nothing to hand
    /// back.
    pub fn invite_id(&self) -> Option<i64> {
        match self {
            SessionKind::Signup { invite_id } => Some(*invite_id),
            // A reconnect spends nothing either: the tenant it is for was paid
            // for by an invite once already, and re-consenting must not cost a
            // second one. Nor does an agent login, which provisions nothing.
            SessionKind::Console
            | SessionKind::App
            | SessionKind::Reconnect
            | SessionKind::Agent { .. }
            | SessionKind::AgentConfirm { .. }
            | SessionKind::AgentDone { .. }
            | SessionKind::AgentInFlight
            | SessionKind::AgentFailed { .. } => None,
        }
    }

    /// How long a session of this kind lives. Every hop through Google gets
    /// [`SESSION_TTL`]; the confirmation page alone gets the shorter
    /// [`AGENT_CONFIRM_TTL`].
    pub fn ttl(&self) -> Duration {
        match self {
            SessionKind::AgentConfirm { .. } => AGENT_CONFIRM_TTL,
            SessionKind::AgentDone { .. } | SessionKind::AgentFailed { .. } => AGENT_DONE_TTL,
            SessionKind::AgentInFlight => AGENT_IN_FLIGHT_TTL,
            _ => SESSION_TTL,
        }
    }

    /// Whether this session shares [`MAX_IDENTITY_SESSIONS`], the budget for
    /// sessions a STRANGER can open. Asked as a question about the kind rather
    /// than spelled as a match at each call site, so a fourth kind cannot be
    /// added without deciding which budget it spends.
    ///
    /// RECONNECT counts here, despite being a grant flow rather than a login.
    /// The budget is not about what the session ends up holding, it is about
    /// who can open one: `/reconnect` is reachable without credentials, exactly
    /// like the two logins, so it has to draw from the pool sized for that.
    /// Charging it to the signup pool instead would let anyone exhaust the
    /// table an invite holder needs.
    ///
    /// AGENT counts here for the same reason, and more obviously: anybody can
    /// make up an `external_auth_id`, so `/agent/auth` is a session a stranger
    /// can open with nothing real in hand.
    ///
    /// AGENT CONFIRM AND ITS THREE MARKERS (in flight, done, failed) DO NOT,
    /// and that is the same rule read the other way. Only the callback writes a
    /// confirmation, and only for a mailbox Google has just verified that owns
    /// an ACTIVE tenant here; only a confirmation being completed writes the
    /// markers. A stranger cannot open
    /// either. Charging them to this pool would let anybody spraying
    /// `/agent/auth` refuse a person who had ALREADY consented at Google, at
    /// the one step after which they cannot cheaply retry. They are still
    /// bounded by [`MAX_SESSIONS`], and per person by what it takes to make
    /// one: a fresh `/agent/auth` session (the login bucket, ten a minute per
    /// client, and this pool), a full Google round trip, and a live tenant, for
    /// an entry that lives five minutes.
    pub fn is_identity(&self) -> bool {
        matches!(
            self,
            SessionKind::Console
                | SessionKind::App
                | SessionKind::Reconnect
                | SessionKind::Agent { .. }
        )
    }
}

/// What a pending session is holding while the user is at Google.
///
/// NO `Debug`, and that is deliberate: a derived one would print the verifier
/// and the state into any `tracing` call that ever formats a session.
pub struct PendingSession {
    /// Which flow this is, and the invite it holds if it is a signup.
    pub kind: SessionKind,
    /// CSRF token, echoed by Google as `state`.
    pub state: String,
    /// PKCE verifier. Never leaves this process except to Google, on the
    /// exchange.
    pub pkce_verifier: String,
    /// The validated label: the one being provisioned (signup) or the one being
    /// signed in to (console).
    pub label: String,
    created: Instant,
}

/// Why a session could not be started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertError {
    /// The table is at [`MAX_SESSIONS`].
    Full,
}

#[derive(Default)]
pub struct SessionStore {
    sessions: HashMap<String, PendingSession>,
    /// Rung by [`Self::mark`], so a press waiting on another press's WorkOS
    /// answer wakes when it lands instead of polling this table. One for the
    /// whole table: a wake is a cue to re-peek, never an answer, so a waiter
    /// woken by somebody else's settle looks once and goes back to waiting.
    settled: Arc<Notify>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Park a pending session under `sid`.
    ///
    /// Expired entries are purged first, so a deployment that has been running
    /// for a week is bounded by concurrent sessions rather than by total ones.
    ///
    /// TWO CEILINGS, and either login meets both: the whole-table
    /// [`MAX_SESSIONS`], and the [`MAX_IDENTITY_SESSIONS`] share of it.
    pub fn insert(
        &mut self,
        sid: String,
        kind: SessionKind,
        state: String,
        pkce_verifier: String,
        label: String,
        now: Instant,
    ) -> Result<(), InsertError> {
        self.sweep(now);
        if self.sessions.len() >= MAX_SESSIONS {
            return Err(InsertError::Full);
        }
        // THE CARVE-OUT. Counted rather than tracked in a counter: the sweep
        // above is already a walk of the same map, so this costs the insert
        // nothing it was not paying, and a count that cannot drift out of step
        // with the table is worth more here than a field.
        if kind.is_identity() && self.identity_len() >= MAX_IDENTITY_SESSIONS {
            return Err(InsertError::Full);
        }
        self.sessions.insert(
            sid,
            PendingSession {
                kind,
                state,
                pkce_verifier,
                label,
                created: now,
            },
        );
        Ok(())
    }

    /// Remove and return a session, if it exists and is live. ONE-SHOT: a
    /// second call with the same id gets `None`, which is what makes a replayed
    /// callback inert.
    pub fn take(&mut self, sid: &str, now: Instant) -> Option<PendingSession> {
        let session = self.sessions.remove(sid)?;
        // Expired is the same answer as absent. The entry is gone either way:
        // an expired session that stayed would be a slot an attacker could
        // keep probing.
        (now.saturating_duration_since(session.created) <= session.kind.ttl()).then_some(session)
    }

    /// Look at a live session WITHOUT taking it.
    ///
    /// For the two agent-flow reads that must be side-effect free: rendering
    /// the confirmation page (which a refresh will ask for again) and noticing,
    /// before anything takes it, that a session is one the asking route will
    /// not finish. Everything that ACTS on a session still goes through
    /// [`Self::take`].
    pub fn peek(&self, sid: &str, now: Instant) -> Option<&PendingSession> {
        self.sessions
            .get(sid)
            .filter(|s| now.saturating_duration_since(s.created) <= s.kind.ttl())
    }

    /// Turn the entry under `sid` into `kind`, in place, restarting its clock;
    /// `false` when there is no entry. For the agent completion's markers,
    /// which REPLACE the in-flight placeholder: an in-place change cannot grow
    /// the table, so unlike [`Self::insert`] it is not refused by a table that
    /// filled up while WorkOS was answering.
    ///
    /// Every call rings [`Self::notifier`], so anybody waiting on this sid
    /// re-peeks.
    pub fn mark(&mut self, sid: &str, kind: SessionKind, now: Instant) -> bool {
        let marked = match self.sessions.get_mut(sid) {
            Some(entry) => {
                entry.kind = kind;
                entry.created = now;
                true
            }
            None => false,
        };
        self.settled.notify_waiters();
        marked
    }

    /// The bell [`Self::mark`] rings. Cloned out under the lock so it can be
    /// awaited with the lock released.
    pub fn notifier(&self) -> Arc<Notify> {
        self.settled.clone()
    }

    /// TAKE a session and, under the SAME guard, put `next(&taken)` in its
    /// place: same id, same `state` (the CSRF token), same label, a fresh
    /// clock. ONE-SHOT exactly as [`Self::take`]: the entry is removed whatever
    /// `next` says, and an expired one is `None` (and gone).
    ///
    /// For the agent confirmation, whose take and in-flight placeholder must
    /// be one step: as two lock acquisitions, a second press arriving between
    /// them would see neither the confirmation nor the placeholder and be
    /// refused. `next` sees the taken entry so the caller can decide (kind,
    /// token, decision) before anything is put back; `None` puts nothing back.
    /// The replacement goes straight into the slot just vacated, so it cannot
    /// be refused for room.
    pub fn take_into(
        &mut self,
        sid: &str,
        now: Instant,
        next: impl FnOnce(&PendingSession) -> Option<SessionKind>,
    ) -> Option<PendingSession> {
        let taken = self.take(sid, now)?;
        if let Some(kind) = next(&taken) {
            self.sessions.insert(
                sid.to_string(),
                PendingSession {
                    kind,
                    state: taken.state.clone(),
                    pkce_verifier: String::new(),
                    label: taken.label.clone(),
                    created: now,
                },
            );
        }
        Some(taken)
    }

    /// Drop every expired session. Called on insert and by the background
    /// sweep, so an idle process does not hold a table of dead entries.
    pub fn sweep(&mut self, now: Instant) -> usize {
        let before = self.sessions.len();
        self.sessions
            .retain(|_, s| now.saturating_duration_since(s.created) <= s.kind.ttl());
        before - self.sessions.len()
    }

    /// Live sessions. A COUNT is the only thing about this table that may be
    /// logged.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Live LOGIN sessions of either kind, which is what
    /// [`MAX_IDENTITY_SESSIONS`] bounds.
    pub fn identity_len(&self) -> usize {
        self.sessions
            .values()
            .filter(|s| s.kind.is_identity())
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert(store: &mut SessionStore, sid: &str, now: Instant) -> Result<(), InsertError> {
        store.insert(
            sid.to_string(),
            SessionKind::Signup { invite_id: 1 },
            "state".into(),
            "verifier".into(),
            "ada".into(),
            now,
        )
    }

    #[test]
    fn a_session_is_taken_exactly_once() {
        let mut s = SessionStore::new();
        let now = Instant::now();
        insert(&mut s, "sid", now).unwrap();
        assert_eq!(s.len(), 1);

        let taken = s.take("sid", now).expect("live");
        assert_eq!(taken.label, "ada");
        assert_eq!(taken.pkce_verifier, "verifier");
        assert!(s.take("sid", now).is_none(), "replay finds nothing");
        assert!(s.is_empty());
    }

    /// The three kinds live in one table and come back as what they went in as.
    /// Neither login carries an invite, which is what makes the release path on
    /// the callback a no-op rather than a lookup for a row that never existed.
    #[test]
    fn a_login_session_holds_no_invite() {
        let mut s = SessionStore::new();
        let now = Instant::now();
        s.insert(
            "console".into(),
            SessionKind::Console,
            "state".into(),
            "verifier".into(),
            "ada".into(),
            now,
        )
        .unwrap();
        // An app login names no tenant going in: the label is empty until
        // Google says which mailbox this is.
        s.insert(
            "app".into(),
            SessionKind::App,
            "state".into(),
            "verifier".into(),
            String::new(),
            now,
        )
        .unwrap();
        insert(&mut s, "signup", now).unwrap();

        let console = s.take("console", now).expect("live");
        assert_eq!(console.kind, SessionKind::Console);
        assert_eq!(console.kind.invite_id(), None);
        assert_eq!(console.label, "ada");

        let app = s.take("app", now).expect("live");
        assert_eq!(app.kind, SessionKind::App);
        assert_eq!(app.kind.invite_id(), None);
        assert_eq!(app.label, "");

        let signup = s.take("signup", now).expect("live");
        assert_eq!(signup.kind, SessionKind::Signup { invite_id: 1 });
        assert_eq!(signup.kind.invite_id(), Some(1));
    }

    /// An agent login carries its `external_auth_id` in the TABLE, comes back
    /// with it intact, holds no invite, and never prints it.
    #[test]
    fn an_agent_session_holds_its_external_auth_id_and_hides_it() {
        let mut s = SessionStore::new();
        let now = Instant::now();
        let kind = SessionKind::Agent {
            external_auth_id: "01J3X4Y5Z6A7B8C9D0E1F2G3H4".into(),
        };
        assert!(kind.is_identity());
        assert_eq!(kind.invite_id(), None);
        let printed = format!("{kind:?}");
        assert!(!printed.contains("01J3X4Y5Z6"), "{printed}");

        s.insert(
            "agent".into(),
            kind.clone(),
            "state".into(),
            "verifier".into(),
            String::new(),
            now,
        )
        .unwrap();
        let taken = s.take("agent", now).expect("live");
        assert_eq!(taken.kind, kind);
        assert_eq!(taken.label, "");
    }

    /// A confirmation lives FIVE minutes, not ten, and an agent login still
    /// waiting on Google keeps its ten. Asserted through the clock this table
    /// already takes as an argument.
    #[test]
    fn a_confirmation_expires_sooner_than_a_google_hop() {
        let mut s = SessionStore::new();
        let now = Instant::now();
        let confirm = SessionKind::AgentConfirm {
            external_auth_id: "01J3X4Y5Z6A7B8C9D0E1F2G3H4".into(),
            stored_email: "ada@example.com".into(),
            user_id: "ada@example.com".into(),
        };
        assert!(
            !confirm.is_identity(),
            "a stranger cannot open one, so it is not charged to their pool"
        );
        let printed = format!("{confirm:?}");
        assert!(!printed.contains("ada@"), "{printed}");
        assert!(!printed.contains("01J3X4"), "{printed}");
        let agent = SessionKind::Agent {
            external_auth_id: "01J3X4Y5Z6A7B8C9D0E1F2G3H4".into(),
        };
        for (sid, kind) in [("c1", &confirm), ("c2", &confirm), ("a1", &agent)] {
            s.insert(
                sid.into(),
                kind.clone(),
                "csrf".into(),
                String::new(),
                "ada".into(),
                now,
            )
            .unwrap();
        }
        // Inside five minutes it is live.
        assert!(s.take("c1", now + AGENT_CONFIRM_TTL).is_some());
        // One second past, it is gone, while the Google hop is not.
        let later = now + AGENT_CONFIRM_TTL + Duration::from_secs(1);
        assert!(s.take("c2", later).is_none());
        assert!(s.take("a1", later).is_some());
    }

    fn confirm_kind() -> SessionKind {
        SessionKind::AgentConfirm {
            external_auth_id: "01J3X4Y5Z6A7B8C9D0E1F2G3H4".into(),
            stored_email: "ada@example.com".into(),
            user_id: "ada@example.com".into(),
        }
    }

    /// A STRANGER FLOODING `/agent/auth` CANNOT REFUSE A VERIFIED PERSON. With
    /// the stranger-openable pool full of pre-Google agent sessions, a
    /// confirmation (which only a Google-verified owner of a live tenant can
    /// reach) still parks, and so does a done marker.
    #[test]
    fn a_full_login_pool_does_not_refuse_a_confirmation() {
        let mut s = SessionStore::new();
        let now = Instant::now();
        for i in 0..MAX_IDENTITY_SESSIONS {
            insert_login(
                &mut s,
                SessionKind::Agent {
                    external_auth_id: "AAAA".into(),
                },
                &format!("spray{i}"),
                now,
            )
            .unwrap();
        }
        assert_eq!(
            insert_login(&mut s, SessionKind::App, "one-more", now),
            Err(InsertError::Full),
            "the pool is full"
        );
        assert_eq!(
            insert_login(&mut s, confirm_kind(), "verified", now),
            Ok(())
        );
        assert_eq!(
            insert_login(
                &mut s,
                SessionKind::AgentDone {
                    redirect: "https://foo.authkit.app/x".into()
                },
                "done",
                now
            ),
            Ok(())
        );
    }

    /// ...but the WHOLE TABLE still bounds them: a full table refuses a
    /// confirmation like anything else.
    #[test]
    fn a_full_table_still_refuses_a_confirmation() {
        let mut s = SessionStore::new();
        let now = Instant::now();
        for i in 0..MAX_SESSIONS {
            insert(&mut s, &format!("sid{i}"), now).unwrap();
        }
        assert_eq!(
            insert_login(&mut s, confirm_kind(), "verified", now),
            Err(InsertError::Full)
        );
    }

    /// `peek` reads without taking, and treats an expired entry as absent.
    #[test]
    fn a_peek_does_not_consume() {
        let mut s = SessionStore::new();
        let now = Instant::now();
        insert_login(&mut s, confirm_kind(), "c", now).unwrap();
        assert!(s.peek("c", now).is_some());
        assert!(s.peek("c", now).is_some(), "still there");
        assert_eq!(s.len(), 1);
        assert!(
            s.peek("c", now + AGENT_CONFIRM_TTL + Duration::from_secs(1))
                .is_none()
        );
        assert!(s.take("c", now).is_some(), "and still takeable once");
        // The done marker lives seconds.
        insert_login(
            &mut s,
            SessionKind::AgentDone {
                redirect: "https://foo.authkit.app/x".into(),
            },
            "d",
            now,
        )
        .unwrap();
        assert!(s.peek("d", now + AGENT_DONE_TTL).is_some());
        assert!(
            s.peek("d", now + AGENT_DONE_TTL + Duration::from_secs(1))
                .is_none()
        );
    }

    /// THE TAKE AND THE PLACEHOLDER ARE ONE STEP: the moment `take_into`
    /// returns the confirmation, the in-flight placeholder is already there
    /// under the same id with the same token. There is no instant at which a
    /// second press sees neither. When `next` declines, nothing is put back,
    /// and either way the confirmation itself is gone (one-shot).
    #[test]
    fn take_into_leaves_no_gap_between_take_and_placeholder() {
        let mut s = SessionStore::new();
        let now = Instant::now();
        s.insert(
            "c".into(),
            confirm_kind(),
            "csrf".into(),
            String::new(),
            "ada".into(),
            now,
        )
        .unwrap();
        let taken = s
            .take_into("c", now, |p| {
                matches!(p.kind, SessionKind::AgentConfirm { .. })
                    .then_some(SessionKind::AgentInFlight)
            })
            .expect("live");
        assert_eq!(taken.kind, confirm_kind());
        let placeholder = s.peek("c", now).expect("the placeholder is already there");
        assert_eq!(placeholder.kind, SessionKind::AgentInFlight);
        assert_eq!(placeholder.state, "csrf");
        assert_eq!(placeholder.label, "ada");
        assert_eq!(s.len(), 1);

        // Declined: taken, nothing put back.
        s.insert(
            "d".into(),
            confirm_kind(),
            "csrf".into(),
            String::new(),
            "ada".into(),
            now,
        )
        .unwrap();
        assert!(s.take_into("d", now, |_| None).is_some());
        assert!(s.peek("d", now).is_none());
        // Absent: nothing to take, nothing put.
        assert!(
            s.take_into("nope", now, |_| Some(SessionKind::AgentInFlight))
                .is_none()
        );
        assert!(s.peek("nope", now).is_none());
    }

    #[test]
    fn an_expired_session_is_gone_and_unreplayable() {
        let mut s = SessionStore::new();
        let now = Instant::now();
        insert(&mut s, "sid", now).unwrap();
        let later = now + SESSION_TTL + Duration::from_secs(1);
        assert!(s.take("sid", later).is_none());
        assert!(s.is_empty(), "the entry is removed, not merely refused");
    }

    #[test]
    fn expired_sessions_are_swept() {
        let mut s = SessionStore::new();
        let now = Instant::now();
        insert(&mut s, "a", now).unwrap();
        insert(&mut s, "b", now + Duration::from_secs(60)).unwrap();
        let later = now + SESSION_TTL + Duration::from_secs(1);
        assert_eq!(s.sweep(later), 1);
        assert_eq!(s.len(), 1);
    }

    /// The fingerprint is stable, distinct per session, and not the id itself:
    /// what the control store holds must not be usable as a session id.
    #[test]
    fn a_session_fingerprint_hides_its_id() {
        let a = fingerprint("a-session-id");
        assert_eq!(a, fingerprint("a-session-id"));
        assert_ne!(a, fingerprint("another-session-id"));
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!a.contains("a-session-id"));
    }

    fn insert_login(
        store: &mut SessionStore,
        kind: SessionKind,
        sid: &str,
        now: Instant,
    ) -> Result<(), InsertError> {
        store.insert(
            sid.to_string(),
            kind,
            "state".into(),
            "verifier".into(),
            "ada".into(),
            now,
        )
    }

    /// The logins' share of the table is CARVED OUT of it: a stranger looping
    /// `GET /console/auth` fills the ceiling and stops, and a signup posted at
    /// that moment still opens.
    ///
    /// ONE BUDGET FOR BOTH LOGINS, which is the half worth asserting: with a
    /// ceiling each, the same flooder gets to spend twice by alternating
    /// `/console/auth` and `/app/auth`.
    #[test]
    fn login_flooding_cannot_spend_signup_capacity() {
        let mut s = SessionStore::new();
        let now = Instant::now();
        let agent = || SessionKind::Agent {
            external_auth_id: "01J3X4Y5Z6A7B8C9D0E1F2G3H4".into(),
        };
        for i in 0..MAX_IDENTITY_SESSIONS {
            // Rotating, so the shared ceiling is what stops this and not one
            // kind's own.
            let kind = match i % 3 {
                0 => SessionKind::Console,
                1 => SessionKind::App,
                _ => agent(),
            };
            insert_login(&mut s, kind, &format!("login{i}"), now).unwrap();
        }
        assert_eq!(s.identity_len(), MAX_IDENTITY_SESSIONS);
        for kind in [SessionKind::Console, SessionKind::App, agent()] {
            assert_eq!(
                insert_login(&mut s, kind.clone(), "one-login-too-many", now),
                Err(InsertError::Full),
                "the shared login ceiling holds for {kind:?}"
            );
        }
        // ...and the flow that cannot be retried cheaply is untouched: every
        // remaining slot in the table is still a signup's to take.
        for i in 0..(MAX_SESSIONS - MAX_IDENTITY_SESSIONS) {
            insert(&mut s, &format!("signup{i}"), now).expect("signup capacity survives the flood");
        }
        assert_eq!(s.len(), MAX_SESSIONS);
    }

    #[test]
    fn the_table_is_bounded() {
        let mut s = SessionStore::new();
        let now = Instant::now();
        for i in 0..MAX_SESSIONS {
            insert(&mut s, &format!("sid{i}"), now).unwrap();
        }
        assert_eq!(insert(&mut s, "one-too-many", now), Err(InsertError::Full));
        // ...and the ceiling forgives once the sessions age out.
        let later = now + SESSION_TTL + Duration::from_secs(1);
        insert(&mut s, "later", later).unwrap();
        assert_eq!(s.len(), 1);
    }
}
